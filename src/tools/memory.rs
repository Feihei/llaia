use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct MemoryWrite {
    pub memory_path: PathBuf,
    pub lock: Arc<Mutex<()>>,
}

impl MemoryWrite {
    pub fn new(memory_path: PathBuf) -> Self {
        Self {
            memory_path,
            lock: Arc::new(Mutex::new(())),
        }
    }
}

#[async_trait]
impl Tool for MemoryWrite {
    fn name(&self) -> &str {
        "memory_write"
    }
    fn description(&self) -> &str {
        "Write a short factual entry to long-term memory. Use for things the user said to remember."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "entry": { "type": "string", "description": "Short factual entry to remember" }
            },
            "required": ["entry"]
        })
    }
    fn requires_confirm(&self) -> bool {
        true
    }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let entry = args
            .get("entry")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'entry'"))?;
        let _g = self.lock.lock().await;

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let line = format!("- [{}] {}\n", today, entry);

        let mut content = tokio::fs::read_to_string(&self.memory_path)
            .await
            .unwrap_or_default();
        content.push_str(&line);
        tokio::fs::write(&self.memory_path, &content)
            .await
            .map_err(|e| anyhow!("write memory: {}", e))?;
        Ok(format!("remembered: {}", entry))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_memory_write() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("MEMORY.md");
        let tool = MemoryWrite::new(path.clone());
        tool.execute(&serde_json::json!({"entry": "user likes rust"}), "cli")
            .await
            .unwrap();

        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(content.contains("user likes rust"));
        assert!(content.contains("[2026-") || content.contains("[2025-") || content.contains("[2027-"));
    }
}
