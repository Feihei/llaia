use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct FileRead {
    workspace: PathBuf,
}
pub struct FileWrite {
    workspace: PathBuf,
}
pub struct FileEdit {
    workspace: PathBuf,
}

impl FileRead {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}
impl FileWrite {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}
impl FileEdit {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

/// 将用户提供的路径解析为绝对路径，并确保落在 workspace 内。
/// - 相对路径以 workspace 为基准
/// - 绝对路径原样使用，但必须位于 workspace 子树内
/// - `..` 逃逸报错
///
/// 注：使用词法规范化（非 canonicalize）做边界检查，能拦截 `..` 逃逸。
/// 符号链接逃逸不检测——单用户私人助理场景下威胁低，且 terminal 在 QQ channel 下已禁用。
pub(crate) fn resolve_within(workspace: &Path, p: &str) -> Result<PathBuf> {
    let path = PathBuf::from(p);
    let joined = if path.is_absolute() {
        path
    } else {
        workspace.join(p)
    };
    let norm_joined = normalize_lexical(&joined);
    let norm_ws = normalize_lexical(workspace);
    if !norm_joined.starts_with(&norm_ws) {
        anyhow::bail!(
            "path {:?} is outside workspace {:?}",
            joined,
            workspace
        );
    }
    Ok(norm_joined)
}

/// 词法规范化：处理 `.` 和 `..`，不依赖文件系统存在。
fn normalize_lexical(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(c) => out.push(c),
        }
    }
    out
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
        let resolved = resolve_within(&self.workspace, path)?;
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
    /// false: workspace 边界检查（resolve_within）已保证路径安全
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
        let resolved = resolve_within(&self.workspace, path)?;
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        tokio::fs::write(&resolved, content)
            .await
            .map_err(|e| anyhow!("write {:?}: {}", resolved, e))?;
        Ok(format!("wrote {} bytes to {}", content.len(), resolved.display()))
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
    /// false: workspace 边界检查（resolve_within）已保证路径安全
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
        let resolved = resolve_within(&self.workspace, path)?;
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
    use std::io::Write;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_file_read_write() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        let rel = "test.txt";
        std::fs::write(ws_path.join(rel), "hello world").unwrap();

        let tool = FileRead::new(ws_path);
        let result = tool.execute(&json!({"path": rel}), "cli").await.unwrap();
        assert!(result.contains("hello world"));
    }

    #[tokio::test]
    async fn test_file_write_and_read() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        let rel = "out.txt";

        let write_tool = FileWrite::new(ws_path.clone());
        write_tool
            .execute(&json!({"path": rel, "content": "new content"}), "cli")
            .await
            .unwrap();

        let read_tool = FileRead::new(ws_path);
        let result = read_tool.execute(&json!({"path": rel}), "cli").await.unwrap();
        assert_eq!(result, "new content");
    }

    #[tokio::test]
    async fn test_file_edit() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        let rel = "edit.txt";
        std::fs::write(ws_path.join(rel), "line1\nline2\nline3").unwrap();

        let tool = FileEdit::new(ws_path.clone());
        tool.execute(&json!({"path": rel, "old_string": "line2", "new_string": "LINE TWO"}), "cli")
            .await
            .unwrap();

        let read = FileRead::new(ws_path);
        let result = read.execute(&json!({"path": rel}), "cli").await.unwrap();
        assert!(result.contains("LINE TWO"));
        assert!(!result.contains("line2"));
    }

    #[tokio::test]
    async fn test_file_edit_no_match() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        let rel = "nomatch.txt";
        std::fs::write(ws_path.join(rel), "hello").unwrap();

        let tool = FileEdit::new(ws_path);
        let result = tool
            .execute(&json!({"path": rel, "old_string": "missing", "new_string": "x"}), "cli")
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_relative_path_resolves_to_workspace() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();

        // 写相对路径，预期落到 workspace 下
        let write_tool = FileWrite::new(ws_path.clone());
        write_tool
            .execute(&json!({"path": "sub/test.txt", "content": "hello"}), "cli")
            .await
            .unwrap();

        // 文件应当存在于 workspace/sub/test.txt
        let expected = ws_path.join("sub/test.txt");
        assert!(expected.exists(), "expected file at {:?}", expected);

        // 读回来也用相对路径
        let read_tool = FileRead::new(ws_path);
        let result = read_tool
            .execute(&json!({"path": "sub/test.txt"}), "cli")
            .await
            .unwrap();
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_requires_confirm_flags() {
        let ws = PathBuf::from(".");
        assert!(!FileRead::new(ws.clone()).requires_confirm());
        // file_write/file_edit 不再需要确认：workspace 边界检查已保证安全
        assert!(!FileWrite::new(ws.clone()).requires_confirm());
        assert!(!FileEdit::new(ws).requires_confirm());
    }

    #[tokio::test]
    async fn test_workspace_boundary_blocks_parent_traversal() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        // 在 workspace 里放一个文件
        let write_tool = FileWrite::new(ws_path.clone());
        write_tool
            .execute(&json!({"path": "inside.txt", "content": "ok"}), "cli")
            .await
            .unwrap();

        // `..` 逃逸应当被拒绝
        let read_tool = FileRead::new(ws_path.clone());
        let escaped = read_tool
            .execute(&json!({"path": "../outside.txt"}), "cli")
            .await;
        assert!(escaped.is_err(), "parent traversal should be blocked");

        // 绝对路径指向 workspace 外应当被拒绝
        let outside = ws_path.parent().unwrap().join("outside.txt");
        let abs_read = FileRead::new(ws_path);
        let result = abs_read
            .execute(
                &json!({"path": outside.to_str().unwrap()}),
                "cli",
            )
            .await;
        assert!(result.is_err(), "absolute path outside workspace should be blocked");
    }

    #[tokio::test]
    async fn test_workspace_boundary_allows_subdir() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        // 子目录写文件应当成功
        let write_tool = FileWrite::new(ws_path.clone());
        let r = write_tool
            .execute(
                &json!({"path": "deep/nested/file.txt", "content": "x"}),
                "cli",
            )
            .await;
        assert!(r.is_ok(), "writing to workspace subdir should succeed: {:?}", r);
    }
}
