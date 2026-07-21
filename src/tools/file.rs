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

/// 将用户提供的路径解析为绝对路径：绝对路径原样使用，相对路径以 workspace 为基准
fn resolve_within(workspace: &Path, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_absolute() {
        path
    } else {
        workspace.join(p)
    }
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
    async fn execute(&self, args: &Value) -> Result<String> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path' argument"))?;
        let resolved = resolve_within(&self.workspace, path);
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
    async fn execute(&self, args: &Value) -> Result<String> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path'"))?;
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'content'"))?;
        let resolved = resolve_within(&self.workspace, path);
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
    fn requires_confirm(&self) -> bool {
        true
    }
    async fn execute(&self, args: &Value) -> Result<String> {
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
        let resolved = resolve_within(&self.workspace, path);
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
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_file_read_write() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "hello world").unwrap();
        let path = tmp.path().to_str().unwrap();

        let tool = FileRead::new(PathBuf::from("."));
        let result = tool.execute(&json!({"path": path})).await.unwrap();
        assert!(result.contains("hello world"));
    }

    #[tokio::test]
    async fn test_file_write_and_read() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let write_tool = FileWrite::new(PathBuf::from("."));
        write_tool
            .execute(&json!({"path": &path, "content": "new content"}))
            .await
            .unwrap();

        let read_tool = FileRead::new(PathBuf::from("."));
        let result = read_tool.execute(&json!({"path": &path})).await.unwrap();
        assert_eq!(result, "new content");
    }

    #[tokio::test]
    async fn test_file_edit() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "line1\nline2\nline3").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let tool = FileEdit::new(PathBuf::from("."));
        tool.execute(&json!({"path": &path, "old_string": "line2", "new_string": "LINE TWO"}))
            .await
            .unwrap();

        let read = FileRead::new(PathBuf::from("."));
        let result = read.execute(&json!({"path": &path})).await.unwrap();
        assert!(result.contains("LINE TWO"));
        assert!(!result.contains("line2"));
    }

    #[tokio::test]
    async fn test_file_edit_no_match() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "hello").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let tool = FileEdit::new(PathBuf::from("."));
        let result = tool
            .execute(&json!({"path": &path, "old_string": "missing", "new_string": "x"}))
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
            .execute(&json!({"path": "sub/test.txt", "content": "hello"}))
            .await
            .unwrap();

        // 文件应当存在于 workspace/sub/test.txt
        let expected = ws_path.join("sub/test.txt");
        assert!(expected.exists(), "expected file at {:?}", expected);

        // 读回来也用相对路径
        let read_tool = FileRead::new(ws_path);
        let result = read_tool
            .execute(&json!({"path": "sub/test.txt"}))
            .await
            .unwrap();
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_requires_confirm_flags() {
        let ws = PathBuf::from(".");
        assert!(!FileRead::new(ws.clone()).requires_confirm());
        assert!(FileWrite::new(ws.clone()).requires_confirm());
        assert!(FileEdit::new(ws).requires_confirm());
    }
}
