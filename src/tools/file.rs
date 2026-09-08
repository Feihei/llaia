use crate::path_guard;
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct FileRead {
    workspace: Arc<RwLock<PathBuf>>,
    /// 会话级受信目录（plan.md #B，与 Agent 共享同一 Arc）：执行层边界 = workspace ∪ 受信
    trusted: Arc<RwLock<Vec<PathBuf>>>,
    is_main: bool,
    /// skills 目录（<config_dir>/skills）：SKILL.md 特殊放行用，None 时不放行
    skills_dir: Option<PathBuf>,
}
pub struct FileWrite {
    workspace: Arc<RwLock<PathBuf>>,
    trusted: Arc<RwLock<Vec<PathBuf>>>,
    is_main: bool,
}
pub struct FileEdit {
    workspace: Arc<RwLock<PathBuf>>,
    trusted: Arc<RwLock<Vec<PathBuf>>>,
    is_main: bool,
}

impl FileRead {
    pub fn new(
        workspace: Arc<RwLock<PathBuf>>,
        trusted: Arc<RwLock<Vec<PathBuf>>>,
        is_main: bool,
        skills_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            workspace,
            trusted,
            is_main,
            skills_dir,
        }
    }
}
impl FileWrite {
    pub fn new(
        workspace: Arc<RwLock<PathBuf>>,
        trusted: Arc<RwLock<Vec<PathBuf>>>,
        is_main: bool,
    ) -> Self {
        Self {
            workspace,
            trusted,
            is_main,
        }
    }
}
impl FileEdit {
    pub fn new(
        workspace: Arc<RwLock<PathBuf>>,
        trusted: Arc<RwLock<Vec<PathBuf>>>,
        is_main: bool,
    ) -> Self {
        Self {
            workspace,
            trusted,
            is_main,
        }
    }
}

/// 保留旧函数签名供 cli.rs 的 @path 图片解析复用
pub(crate) fn resolve_within(workspace: &Path, p: &str) -> Result<PathBuf> {
    path_guard::validate_path(workspace, p, None)
}

/// 行尾宽容匹配（Windows CRLF 陷阱）：文件与 old_string 行尾不一致导致逐字节
/// 匹配必失手——即使模型逐字复制 file_read 的内容，`\n` 也匹配不上 `\r\n`。
/// 首选完全一致；未命中且仅行尾不同（单侧 CRLF）时换算行尾重试。
/// 返回 (生效 old, 生效 new, 命中次数)；都不命中时按原文返回 0 次让上层报错。
fn match_with_eol_tolerance(content: &str, old: &str, new: &str) -> (String, String, usize) {
    let exact = content.matches(old).count();
    if exact > 0 {
        return (old.to_string(), new.to_string(), exact);
    }
    let content_crlf = content.contains("\r\n");
    let old_crlf = old.contains("\r\n");
    // 只处理纯单侧行尾，混合行尾（同一 old 里 \n 与 \r\n 并存）不在宽容范围
    if content_crlf && !old_crlf && !old.contains('\r') && old.contains('\n') {
        let old2 = old.replace('\n', "\r\n");
        let n = content.matches(&old2).count();
        if n > 0 {
            return (old2, new.replace('\n', "\r\n"), n);
        }
    } else if !content_crlf && old_crlf && !content.contains('\r') {
        let old2 = old.replace("\r\n", "\n");
        let n = content.matches(&old2).count();
        if n > 0 {
            return (old2, new.replace("\r\n", "\n"), n);
        }
    }
    (old.to_string(), new.to_string(), 0)
}

/// old_string 未命中时的报错附录：用 old_string 里首个能在文件中找到的行做锚点，
/// 摘出文件对应片段（带 1 起始行号、限量），让模型对照差异自我纠正——
/// 凭印象改写 / 引用过期版本内容是主要失败模式，只报 "not found" 会让模型盲试。
/// 返回值以 `\n` 开头（无锚点时给行动指引）。
fn not_found_hint(content: &str, old: &str) -> String {
    const MAX_LINES: usize = 24;
    const MAX_CHARS: usize = 1000;
    // 锚点行要求非空且 ≥8 字符，避免拿空行/常见短行匹配到无关位置
    let anchor = old
        .lines()
        .map(str::trim)
        .filter(|l| l.len() >= 8)
        .find_map(|needle| {
            content
                .lines()
                .position(|line| line.trim() == needle)
                .map(|pos| (needle, pos))
        });
    let Some((needle, pos)) = anchor else {
        return "\n[hint] no line of old_string exists in this file - the content may be stale or from an older version; re-read the file with file_read and copy old_string verbatim.".to_string();
    };
    let span = old.lines().count().min(MAX_LINES);
    let mut excerpt = String::new();
    let mut chars = 0;
    for (i, line) in content
        .lines()
        .skip(pos)
        .take(span)
        .take_while(|l| {
            chars += l.len() + 1;
            chars <= MAX_CHARS
        })
        .enumerate()
    {
        excerpt.push_str(&format!("{:>5} | {}\n", pos + i + 1, line));
    }
    format!(
        "\n[hint] closest match at line {} (anchored by {:?}); actual file content there:\n{}compare your old_string against the excerpt and retry with content copied verbatim from file_read.",
        pos + 1,
        needle,
        excerpt
    )
}

/// 主 agent 可读 subagent/ 子目录的额外路径
fn extra_readable_for_main(workspace: &Path) -> Option<PathBuf> {
    Some(workspace.join("subagent"))
}

#[async_trait]
impl Tool for FileRead {
    fn name(&self) -> &str {
        "file_read"
    }
    fn description(&self) -> &str {
        "Read the content of a file at the given path. Relative paths resolve to the agent workspace."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute or relative file path (relative to agent workspace)" }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        self.run(args, false).await
    }

    /// 批准豁免（ADR-0020）：`/ok` 后跳过 workspace 白名单，危险前缀黑名单保留。
    async fn execute_approved(
        &self,
        args: &Value,
        _channel: &str,
        _event_tx: Option<&tokio::sync::mpsc::Sender<crate::agent::TurnEvent>>,
    ) -> Result<String> {
        self.run(args, true).await
    }
}

impl FileRead {
    async fn run(&self, args: &Value, approved: bool) -> Result<String> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path' argument"))?;
        let ws = self.workspace.read().await;
        // 特殊放行：skills 目录内的 SKILL.md（位于 agent workspace 之外，ADR-0015）
        if let Some(skills_dir) = &self.skills_dir {
            if let Some(skill_path) =
                crate::skill::loader::resolve_skill_path(skills_dir, &ws, path)
            {
                let content = tokio::fs::read_to_string(&skill_path)
                    .await
                    .map_err(|e| anyhow!("read {:?}: {}", skill_path, e))?;
                return Ok(content);
            }
        }
        let resolved = if approved {
            path_guard::resolve_approved_path(&ws, path)?
        } else {
            let extra = if self.is_main {
                extra_readable_for_main(&ws)
            } else {
                None
            };
            let trusted = self.trusted.read().await.clone();
            path_guard::validate_path_in_scope(&ws, &trusted, path, extra.as_deref())?
        };
        let content = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| anyhow!("read {:?}: {}", resolved, e))?;
        Ok(content)
    }
}

#[async_trait]
impl Tool for FileWrite {
    fn name(&self) -> &str {
        "file_write"
    }
    fn description(&self) -> &str {
        "Write content to a file (overwrites). Relative paths resolve to the agent workspace."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute or relative file path (relative to agent workspace)" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        })
    }
    fn requires_confirm(&self) -> bool {
        true
    }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        self.run(args, false).await
    }

    /// 批准豁免（ADR-0020）：`/ok` 后跳过 workspace 白名单，危险前缀黑名单保留。
    async fn execute_approved(
        &self,
        args: &Value,
        _channel: &str,
        _event_tx: Option<&tokio::sync::mpsc::Sender<crate::agent::TurnEvent>>,
    ) -> Result<String> {
        self.run(args, true).await
    }
}

impl FileWrite {
    async fn run(&self, args: &Value, approved: bool) -> Result<String> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path'"))?;
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'content'"))?;

        let ws = self.workspace.read().await;
        // 主 agent 写 subagent/ 路径时拒绝（.inbox/ 例外由 delegate 系统层处理，不经 file 工具）
        let resolved = if approved {
            path_guard::resolve_approved_path(&ws, path)?
        } else {
            let trusted = self.trusted.read().await.clone();
            path_guard::validate_path_in_scope(&ws, &trusted, path, None)?
        };
        if self.is_main {
            let subagent_dir = ws.join("subagent");
            if resolved.starts_with(&subagent_dir) {
                anyhow::bail!("main agent cannot write to sub-agent workspace: {}", path);
            }
        }

        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        tokio::fs::write(&resolved, content)
            .await
            .map_err(|e| anyhow!("write {:?}: {}", resolved, e))?;
        Ok(format!(
            "wrote {} bytes to {}",
            content.len(),
            resolved.display()
        ))
    }
}

#[async_trait]
impl Tool for FileEdit {
    fn name(&self) -> &str {
        "file_edit"
    }
    fn description(&self) -> &str {
        "Replace old_string with new_string in a file. Relative paths resolve to the agent workspace."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute or relative file path (relative to agent workspace)" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    fn requires_confirm(&self) -> bool {
        true
    }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        self.run(args, false).await
    }

    /// 批准豁免（ADR-0020）：`/ok` 后跳过 workspace 白名单，危险前缀黑名单保留。
    async fn execute_approved(
        &self,
        args: &Value,
        _channel: &str,
        _event_tx: Option<&tokio::sync::mpsc::Sender<crate::agent::TurnEvent>>,
    ) -> Result<String> {
        self.run(args, true).await
    }
}

impl FileEdit {
    async fn run(&self, args: &Value, approved: bool) -> Result<String> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path'"))?;
        let old = args
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'old_string'"))?;
        let new = args
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'new_string'"))?;

        let ws = self.workspace.read().await;
        let resolved = if approved {
            path_guard::resolve_approved_path(&ws, path)?
        } else {
            let trusted = self.trusted.read().await.clone();
            path_guard::validate_path_in_scope(&ws, &trusted, path, None)?
        };
        if self.is_main {
            let subagent_dir = ws.join("subagent");
            if resolved.starts_with(&subagent_dir) {
                anyhow::bail!("main agent cannot write to sub-agent workspace: {}", path);
            }
        }

        let content = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| anyhow!("read {:?}: {}", resolved, e))?;
        let new_content = if old.is_empty() {
            new.to_string()
        } else {
            let (old_eff, new_eff, count) = match_with_eol_tolerance(&content, old, new);
            if count == 0 {
                return Err(anyhow!(
                    "old_string not found in {}.{}",
                    resolved.display(),
                    not_found_hint(&content, old)
                ));
            }
            if count > 1 {
                return Err(anyhow!(
                    "old_string appears {} times in {}, need unique match",
                    count,
                    resolved.display()
                ));
            }
            content.replacen(&old_eff, &new_eff, 1)
        };
        tokio::fs::write(&resolved, &new_content)
            .await
            .map_err(|e| anyhow!("write {:?}: {}", resolved, e))?;
        Ok(format!("edited {}", resolved.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::tempdir;
    use tokio::sync::RwLock;

    #[tokio::test]
    async fn test_file_read_write() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        std::fs::write(ws_path.join("test.txt"), "hello world").unwrap();
        let tool = FileRead::new(
            Arc::new(RwLock::new(ws_path)),
            Arc::new(RwLock::new(Vec::new())),
            true,
            None,
        );
        let result = tool
            .execute(&json!({"path": "test.txt"}), "cli")
            .await
            .unwrap();
        assert!(result.contains("hello world"));
    }

    #[tokio::test]
    async fn test_workspace_boundary_blocks_parent_traversal() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        let write_tool = FileWrite::new(
            Arc::new(RwLock::new(ws_path.clone())),
            Arc::new(RwLock::new(Vec::new())),
            true,
        );
        write_tool
            .execute(&json!({"path": "inside.txt", "content": "ok"}), "cli")
            .await
            .unwrap();

        let read_tool = FileRead::new(
            Arc::new(RwLock::new(ws_path)),
            Arc::new(RwLock::new(Vec::new())),
            true,
            None,
        );
        let escaped = read_tool
            .execute(&json!({"path": "../outside.txt"}), "cli")
            .await;
        assert!(escaped.is_err());
    }

    /// 回归（ADR-0020 审批豁免）：`/ok` 批准的 workspace 外写入必须在执行层放行，
    /// 不再被白名单二次拒绝；但灾难前缀黑名单（C:\Windows 等）仍保留。
    #[tokio::test]
    async fn test_approved_out_of_workspace_write_executes() {
        let ws = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let target = outside.path().join("out.txt");
        let tool = FileWrite::new(
            Arc::new(RwLock::new(ws.path().to_path_buf())),
            Arc::new(RwLock::new(Vec::new())),
            true,
        );

        // 未批准：越界拒绝
        let result = tool
            .execute(
                &json!({"path": target.display().to_string(), "content": "x"}),
                "cli",
            )
            .await;
        assert!(result.is_err(), "未批准的越界写入应拒绝");

        // 批准豁免：放行并真实写入
        tool.execute_approved(
            &json!({"path": target.display().to_string(), "content": "ok"}),
            "cli",
            None,
        )
        .await
        .expect("approved out-of-workspace write should run");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "ok");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_approved_write_still_blocks_dangerous_prefix() {
        let ws = tempdir().unwrap();
        let tool = FileWrite::new(
            Arc::new(RwLock::new(ws.path().to_path_buf())),
            Arc::new(RwLock::new(Vec::new())),
            true,
        );
        let result = tool
            .execute_approved(
                &json!({"path": r"C:\Windows\evil.txt", "content": "x"}),
                "cli",
                None,
            )
            .await;
        assert!(result.is_err(), "approved mode must keep blacklist prefix");
    }

    /// 回归（plan.md #B 执行层）：受信目录内的操作在执行层必须放行——
    /// 审批层按 workspace ∪ 受信放行后，执行层若仍只认 workspace_root，
    /// 会出现「批准了却执行失败」的割裂。信任列表与工具共享同一 Arc，
    /// /move 批准登记后（无需 /move 切走）目标目录内读写即时生效。
    #[tokio::test]
    async fn test_trusted_dir_operations_execute_after_move_away() {
        let home = tempdir().unwrap();
        let moved = tempdir().unwrap();
        let home_ws = home.path().join("workspace");
        std::fs::create_dir_all(&home_ws).unwrap();
        let moved_dir = moved.path().to_path_buf();

        let trusted: Arc<RwLock<Vec<PathBuf>>> = Arc::new(RwLock::new(Vec::new()));
        // workspace_root 已 /move 到 moved_dir，home 的旧 workspace 进受信集合
        *trusted.write().await = vec![home_ws.clone()];

        let write_tool = FileWrite::new(
            Arc::new(RwLock::new(moved_dir.clone())),
            trusted.clone(),
            true,
        );
        write_tool
            .execute(
                &json!({"path": home_ws.join("note.md").to_str().unwrap(), "content": "hi"}),
                "cli",
            )
            .await
            .expect("受信目录内写入应放行");

        let read_tool = FileRead::new(Arc::new(RwLock::new(moved_dir)), trusted, true, None);
        let out = read_tool
            .execute(
                &json!({"path": home_ws.join("note.md").to_str().unwrap()}),
                "cli",
            )
            .await
            .expect("受信目录内读取应放行");
        assert!(out.contains("hi"));

        // 未受信的第三方目录仍拒绝
        let other = tempdir().unwrap();
        let result = read_tool
            .execute(
                &json!({"path": other.path().join("x.txt").to_str().unwrap()}),
                "cli",
            )
            .await;
        assert!(result.is_err(), "逃出 workspace ∪ 受信的路径应拒绝");
    }

    /// CRLF 行尾宽容：文件是 CRLF、old_string 是 LF（模型从 file_read 复制后
    /// 串行化丢失 \r 的常见形态）也应命中，且写回不整篇改行尾。
    #[tokio::test]
    async fn test_file_edit_tolerates_crlf_file_with_lf_old_string() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        std::fs::write(ws_path.join("crlf.txt"), "alpha\r\nbeta\r\ngamma\r\n").unwrap();
        let tool = FileEdit::new(
            Arc::new(RwLock::new(ws_path.clone())),
            Arc::new(RwLock::new(Vec::new())),
            true,
        );
        tool.execute(
            &json!({"path": "crlf.txt", "old_string": "beta\r\ngamma", "new_string": "BETA"}),
            "cli",
        )
        .await
        .expect("CRLF 文件 + CRLF old_string 应命中");

        // 同一文件再试 LF old_string
        tool.execute(
            &json!({"path": "crlf.txt", "old_string": "alpha\nBETA", "new_string": "ALPHA"}),
            "cli",
        )
        .await
        .expect("CRLF 文件 + LF old_string 应宽容命中");
        let out = std::fs::read_to_string(ws_path.join("crlf.txt")).unwrap();
        assert_eq!(out, "ALPHA\r\n"); // old_string 命中片段被整体替换，未触碰片段保持 CRLF

        // 反向：LF 文件 + CRLF old_string
        std::fs::write(ws_path.join("lf.txt"), "one\ntwo\n").unwrap();
        tool.execute(
            &json!({"path": "lf.txt", "old_string": "one\r\ntwo", "new_string": "1\n2"}),
            "cli",
        )
        .await
        .expect("LF 文件 + CRLF old_string 应宽容命中");
        assert_eq!(
            std::fs::read_to_string(ws_path.join("lf.txt")).unwrap(),
            "1\n2\n"
        );
    }

    /// not found 报错应附上最接近片段（带行号），而非裸报错让模型盲试。
    #[tokio::test]
    async fn test_file_edit_not_found_error_includes_excerpt_hint() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        std::fs::write(
            ws_path.join("code.txt"),
            "fn a() {\n    // real comment\n    work();\n}\n",
        )
        .unwrap();
        let tool = FileEdit::new(
            Arc::new(RwLock::new(ws_path)),
            Arc::new(RwLock::new(Vec::new())),
            true,
        );
        let err = tool
            .execute(
                &json!({
                    "path": "code.txt",
                    "old_string": "fn a() {\n    // hallucinated comment\n    work();\n}",
                    "new_string": "x"
                }),
                "cli",
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("old_string not found"));
        assert!(err.contains("[hint]"));
        assert!(
            err.contains("// real comment"),
            "excerpt 应含文件真实行: {err}"
        );
        assert!(err.contains("2 |"), "excerpt 应带行号: {err}");
    }

    /// old_string 里没有任何一行存在于文件（过期内容）时，报错应指向重新 file_read。
    #[tokio::test]
    async fn test_file_edit_not_found_without_anchor_suggests_reread() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        std::fs::write(ws_path.join("f.txt"), "current content\n").unwrap();
        let tool = FileEdit::new(
            Arc::new(RwLock::new(ws_path)),
            Arc::new(RwLock::new(Vec::new())),
            true,
        );
        let err = tool
            .execute(
                &json!({
                    "path": "f.txt",
                    "old_string": "stale paragraph from an older version of this file",
                    "new_string": "x"
                }),
                "cli",
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("stale"), "应提示内容可能过期: {err}");
        assert!(err.contains("file_read"));
    }

    #[tokio::test]
    async fn test_main_agent_can_read_subagent() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        let subagent_dir = ws_path.join("subagent").join("coder");
        std::fs::create_dir_all(&subagent_dir).unwrap();
        std::fs::write(subagent_dir.join("result.md"), "sub output").unwrap();

        let tool = FileRead::new(
            Arc::new(RwLock::new(ws_path)),
            Arc::new(RwLock::new(Vec::new())),
            true,
            None,
        );
        let result = tool
            .execute(&json!({"path": "subagent/coder/result.md"}), "cli")
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("sub output"));
    }

    #[tokio::test]
    async fn test_main_agent_cannot_write_subagent() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        std::fs::create_dir_all(ws_path.join("subagent").join("coder")).unwrap();

        let tool = FileWrite::new(
            Arc::new(RwLock::new(ws_path)),
            Arc::new(RwLock::new(Vec::new())),
            true,
        );
        let result = tool
            .execute(
                &json!({"path": "subagent/coder/evil.txt", "content": "hack"}),
                "cli",
            )
            .await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("cannot write to sub-agent"));
    }

    #[tokio::test]
    async fn test_sub_agent_cannot_read_subagent_sibling() {
        let ws = tempdir().unwrap();
        // 子 agent workspace 是 subagent/coder/
        let coder_ws = ws.path().join("subagent").join("coder");
        std::fs::create_dir_all(&coder_ws).unwrap();
        // 兄弟子 agent searcher 的文件
        let searcher_ws = ws.path().join("subagent").join("searcher");
        std::fs::create_dir_all(&searcher_ws).unwrap();
        std::fs::write(searcher_ws.join("secret.txt"), "secret").unwrap();

        let tool = FileRead::new(
            Arc::new(RwLock::new(coder_ws)),
            Arc::new(RwLock::new(Vec::new())),
            false,
            None,
        );
        let result = tool
            .execute(&json!({"path": "../searcher/secret.txt"}), "cli")
            .await;
        assert!(result.is_err());
    }

    /// 特殊放行：workspace 外的 skills 目录内任意文件可读（SKILL.md、脚本、资产），
    /// 但路径穿越到 skills 目录外仍拒绝。
    #[tokio::test]
    async fn test_file_read_skill_md_special_allow() {
        let root = tempdir().unwrap();
        let ws_path = root.path().join("workspace");
        let skills_dir = root.path().join("skills");
        std::fs::create_dir_all(&ws_path).unwrap();
        let skill_dir = skills_dir.join("demo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo\n---\nskill body",
        )
        .unwrap();
        std::fs::write(skill_dir.join("secret.txt"), "secret").unwrap();

        let tool = FileRead::new(
            Arc::new(RwLock::new(ws_path)),
            Arc::new(RwLock::new(Vec::new())),
            true,
            Some(skills_dir.clone()),
        );
        // SKILL.md 可读（绝对路径）
        let result = tool
            .execute(
                &json!({"path": skill_dir.join("SKILL.md").to_str().unwrap()}),
                "cli",
            )
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("skill body"));
        // 同目录非 SKILL.md 配套文件（脚本/配置/资产）也可读
        let result = tool
            .execute(
                &json!({"path": skill_dir.join("secret.txt").to_str().unwrap()}),
                "cli",
            )
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("secret"));
        // 路径穿越到 skills 目录外仍拒绝
        let escape = "demo/../../secret.txt";
        let result = tool.execute(&json!({"path": escape}), "cli").await;
        assert!(
            result.is_err(),
            "path escape out of skills dir must be rejected"
        );
        // 未配 skills_dir 时 SKILL.md 也拒绝
        let tool_no_skills = FileRead::new(
            Arc::new(RwLock::new(root.path().join("workspace"))),
            Arc::new(RwLock::new(Vec::new())),
            true,
            None,
        );
        let result = tool_no_skills
            .execute(
                &json!({"path": skill_dir.join("SKILL.md").to_str().unwrap()}),
                "cli",
            )
            .await;
        assert!(result.is_err());
    }
}
