use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;

pub struct FileRead;
pub struct FileWrite;
pub struct FileEdit;

#[async_trait]
impl Tool for FileRead {
    fn name(&self) -> &str {
        "file_read"
    }
    fn description(&self) -> &str {
        "Read the content of a file at the given path."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute or relative file path" }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: &Value) -> Result<String> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path' argument"))?;
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| anyhow!("read {:?}: {}", path, e))?;
        Ok(content)
    }
}

#[async_trait]
impl Tool for FileWrite {
    fn name(&self) -> &str {
        "file_write"
    }
    fn description(&self) -> &str {
        "Write content to a file (overwrites)."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        })
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
        tokio::fs::write(path, content)
            .await
            .map_err(|e| anyhow!("write {:?}: {}", path, e))?;
        Ok(format!("wrote {} bytes to {}", content.len(), path))
    }
}

#[async_trait]
impl Tool for FileEdit {
    fn name(&self) -> &str {
        "file_edit"
    }
    fn description(&self) -> &str {
        "Replace old_string with new_string in a file."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" }
            },
            "required": ["path", "old_string", "new_string"]
        })
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
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| anyhow!("read {:?}: {}", path, e))?;
        let new_content = if old.is_empty() {
            new.to_string()
        } else {
            let count = content.matches(old).count();
            if count == 0 {
                return Err(anyhow!("old_string not found in {}", path));
            }
            if count > 1 {
                return Err(anyhow!(
                    "old_string appears {} times in {}, need unique match",
                    count,
                    path
                ));
            }
            content.replacen(old, new, 1)
        };
        tokio::fs::write(path, &new_content)
            .await
            .map_err(|e| anyhow!("write {:?}: {}", path, e))?;
        Ok(format!("edited {}", path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_file_read_write() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "hello world").unwrap();
        let path = tmp.path().to_str().unwrap();

        let tool = FileRead;
        let result = tool.execute(&json!({"path": path})).await.unwrap();
        assert!(result.contains("hello world"));
    }

    #[tokio::test]
    async fn test_file_write_and_read() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let write_tool = FileWrite;
        write_tool
            .execute(&json!({"path": &path, "content": "new content"}))
            .await
            .unwrap();

        let read_tool = FileRead;
        let result = read_tool.execute(&json!({"path": &path})).await.unwrap();
        assert_eq!(result, "new content");
    }

    #[tokio::test]
    async fn test_file_edit() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "line1\nline2\nline3").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let tool = FileEdit;
        tool.execute(&json!({"path": &path, "old_string": "line2", "new_string": "LINE TWO"}))
            .await
            .unwrap();

        let read = FileRead;
        let result = read.execute(&json!({"path": &path})).await.unwrap();
        assert!(result.contains("LINE TWO"));
        assert!(!result.contains("line2"));
    }

    #[tokio::test]
    async fn test_file_edit_no_match() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "hello").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let tool = FileEdit;
        let result = tool
            .execute(&json!({"path": &path, "old_string": "missing", "new_string": "x"}))
            .await;
        assert!(result.is_err());
    }
}
