use crate::path_guard;
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct FileRead {
    workspace: PathBuf,
    is_main: bool,
    /// skills 目录（<config_dir>/skills）：SKILL.md 特殊放行用，None 时不放行
    skills_dir: Option<PathBuf>,
}
pub struct FileWrite {
    workspace: PathBuf,
    is_main: bool,
}
pub struct FileEdit {
    workspace: PathBuf,
    is_main: bool,
}

impl FileRead {
    pub fn new(workspace: PathBuf, is_main: bool, skills_dir: Option<PathBuf>) -> Self {
        Self {
            workspace,
            is_main,
            skills_dir,
        }
    }
}
impl FileWrite {
    pub fn new(workspace: PathBuf, is_main: bool) -> Self {
        Self { workspace, is_main }
    }
}
impl FileEdit {
    pub fn new(workspace: PathBuf, is_main: bool) -> Self {
        Self { workspace, is_main }
    }
}

/// 保留旧函数签名供 cli.rs 的 @path 图片解析复用
pub(crate) fn resolve_within(workspace: &Path, p: &str) -> Result<PathBuf> {
    path_guard::validate_path(workspace, p, None)
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
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path' argument"))?;
        // 特殊放行：skills 目录内的 SKILL.md（位于 agent workspace 之外，ADR-0015）
        if let Some(skills_dir) = &self.skills_dir {
            if let Some(skill_path) =
                crate::skill::loader::resolve_skill_path(skills_dir, &self.workspace, path)
            {
                let content = tokio::fs::read_to_string(&skill_path)
                    .await
                    .map_err(|e| anyhow!("read {:?}: {}", skill_path, e))?;
                return Ok(content);
            }
        }
        let extra = if self.is_main {
            extra_readable_for_main(&self.workspace)
        } else {
            None
        };
        let resolved = path_guard::validate_path(&self.workspace, path, extra.as_deref())?;
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
        false
    }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path'"))?;
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'content'"))?;

        // 主 agent 写 subagent/ 路径时拒绝（.inbox/ 例外由 delegate 系统层处理，不经 file 工具）
        let resolved = path_guard::validate_path(&self.workspace, path, None)?;
        if self.is_main {
            let subagent_dir = self.workspace.join("subagent");
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
        false
    }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
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

        let resolved = path_guard::validate_path(&self.workspace, path, None)?;
        if self.is_main {
            let subagent_dir = self.workspace.join("subagent");
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
            let count = content.matches(old).count();
            if count == 0 {
                return Err(anyhow!("old_string not found in {}", resolved.display()));
            }
            if count > 1 {
                return Err(anyhow!(
                    "old_string appears {} times in {}, need unique match",
                    count,
                    resolved.display()
                ));
            }
            content.replacen(old, new, 1)
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
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_file_read_write() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        std::fs::write(ws_path.join("test.txt"), "hello world").unwrap();
        let tool = FileRead::new(ws_path, true, None);
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
        let write_tool = FileWrite::new(ws_path.clone(), true);
        write_tool
            .execute(&json!({"path": "inside.txt", "content": "ok"}), "cli")
            .await
            .unwrap();

        let read_tool = FileRead::new(ws_path, true, None);
        let escaped = read_tool
            .execute(&json!({"path": "../outside.txt"}), "cli")
            .await;
        assert!(escaped.is_err());
    }

    #[tokio::test]
    async fn test_main_agent_can_read_subagent() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        let subagent_dir = ws_path.join("subagent").join("coder");
        std::fs::create_dir_all(&subagent_dir).unwrap();
        std::fs::write(subagent_dir.join("result.md"), "sub output").unwrap();

        let tool = FileRead::new(ws_path, true, None);
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

        let tool = FileWrite::new(ws_path, true);
        let result = tool
            .execute(
                &json!({"path": "subagent/coder/evil.txt", "content": "hack"}),
                "cli",
            )
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot write to sub-agent"));
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

        let tool = FileRead::new(coder_ws, false, None);
        let result = tool
            .execute(&json!({"path": "../searcher/secret.txt"}), "cli")
            .await;
        assert!(result.is_err());
    }

    /// SKILL.md 特殊放行：workspace 外的 skills 目录内 SKILL.md 可读，其他文件仍拒绝
    #[tokio::test]
    async fn test_file_read_skill_md_special_allow() {
        let root = tempdir().unwrap();
        let ws_path = root.path().join("workspace");
        let skills_dir = root.path().join("skills");
        std::fs::create_dir_all(&ws_path).unwrap();
        let skill_dir = skills_dir.join("demo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "---\nname: demo\n---\nskill body").unwrap();
        std::fs::write(skill_dir.join("secret.txt"), "secret").unwrap();

        let tool = FileRead::new(ws_path, true, Some(skills_dir.clone()));
        // SKILL.md 可读（绝对路径）
        let result = tool
            .execute(&json!({"path": skill_dir.join("SKILL.md").to_str().unwrap()}), "cli")
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("skill body"));
        // 同目录非 SKILL.md 文件仍拒绝
        let result = tool
            .execute(&json!({"path": skill_dir.join("secret.txt").to_str().unwrap()}), "cli")
            .await;
        assert!(result.is_err());
        // 未配 skills_dir 时 SKILL.md 也拒绝
        let tool_no_skills = FileRead::new(
            root.path().join("workspace"),
            true,
            None,
        );
        let result = tool_no_skills
            .execute(&json!({"path": skill_dir.join("SKILL.md").to_str().unwrap()}), "cli")
            .await;
        assert!(result.is_err());
    }
}
