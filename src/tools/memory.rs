use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct MemoryWrite {
    pub memory_path: PathBuf,
    /// USER.md 路径（子 agent 拒绝写此文件）
    pub user_path: PathBuf,
    pub is_main: bool,
    pub lock: Arc<Mutex<()>>,
}

impl MemoryWrite {
    pub fn new(memory_path: PathBuf, user_path: PathBuf, is_main: bool) -> Self {
        Self {
            memory_path,
            user_path,
            is_main,
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

        // 子 agent 不允许写 USER.md（身份绑定统一在主 agent 管理）
        // memory_write 本身写 MEMORY.md，但检查 is_main 防止子 agent 误用
        if !self.is_main {
            anyhow::bail!("子 agent 不可写长期记忆，身份绑定统一在主 agent 管理");
        }

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
    async fn test_main_agent_can_write_memory() {
        let dir = tempdir().unwrap();
        let mem_path = dir.path().join("MEMORY.md");
        let user_path = dir.path().join("USER.md");
        let tool = MemoryWrite::new(mem_path.clone(), user_path, true);
        tool.execute(&serde_json::json!({"entry": "user likes rust"}), "cli")
            .await
            .unwrap();

        let content = tokio::fs::read_to_string(&mem_path).await.unwrap();
        assert!(content.contains("user likes rust"));
    }

    #[tokio::test]
    async fn test_sub_agent_cannot_write_memory() {
        let dir = tempdir().unwrap();
        let mem_path = dir.path().join("MEMORY.md");
        let user_path = dir.path().join("USER.md");
        let tool = MemoryWrite::new(mem_path.clone(), user_path, false);
        let result = tool
            .execute(&serde_json::json!({"entry": "test"}), "cli")
            .await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("子 agent"));
    }
}
