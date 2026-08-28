use crate::memory::sqlite::{short_uuid, SessionStore};
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// 工具侧硬上限：单次最多返回 20 条命中（plan.md）。
const MAX_RESULTS: i64 = 20;
/// 单条命中正文最多返回的字符数（含截断标记）。
const SNIPPET_MAX: usize = 200;

pub struct MemoryWrite {
    pub memory_path: PathBuf,
    /// USER.md 路径（子 agent 拒绝写此文件）
    pub user_path: PathBuf,
    pub is_main: bool,
    pub lock: Arc<Mutex<()>>,
    /// `[runtime].timezone` 启动快照：决定条目日期用哪个时区。
    /// Docker 镜像默认 UTC，不带这个值时北京用户的记忆会整体早一天落盘。
    /// 构造期快照即可——改时区属于低频操作，重启生效可接受。
    timezone: Option<String>,
}

impl MemoryWrite {
    pub fn new(memory_path: PathBuf, user_path: PathBuf, is_main: bool) -> Self {
        Self {
            memory_path,
            user_path,
            is_main,
            lock: Arc::new(Mutex::new(())),
            timezone: None,
        }
    }

    pub fn with_timezone(mut self, tz: Option<String>) -> Self {
        self.timezone = tz;
        self
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
            anyhow::bail!("sub-agent cannot write long-term memory; identity binding is managed by the main agent");
        }

        let _g = self.lock.lock().await;
        let today = crate::time::now(&self.timezone).ymd();
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

/// 跨会话全文搜索历史消息（plan.md memory_research）。只读、无副作用，无需审批。
pub struct MemoryResearch {
    store: Arc<SessionStore>,
}

impl MemoryResearch {
    pub fn new(store: Arc<SessionStore>) -> Self {
        Self { store }
    }
}

/// 截断命中正文为单行片段（压缩空白 + 限长）。
fn snippet(content: &str) -> String {
    let collapsed: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= SNIPPET_MAX {
        collapsed
    } else {
        let cut: String = collapsed.chars().take(SNIPPET_MAX).collect();
        format!("{}…", cut)
    }
}

#[async_trait]
impl Tool for MemoryResearch {
    fn name(&self) -> &str {
        "memory_research"
    }
    fn description(&self) -> &str {
        "Search across all past conversation history (all sessions) by full-text query. Returns matching messages with their session and time. Use when you need to recall something discussed earlier in any session. Only indexes user and assistant text (system prompts and tool outputs are not searchable)."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Full-text search query (SQLite FTS5 syntax). Plain keywords are implicitly AND; use double quotes around whole phrases like `\"borrow checker\"` to match the exact phrase sequence; supports standard syntax like `AND`/`OR`/`-exclude`/`prefix:*`."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results (1..=20). Default 10."
                }
            },
            "required": ["query"]
        })
    }
    fn requires_confirm(&self) -> bool {
        false
    }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("missing 'query'"))?;
        if query.chars().count() > 200 {
            return Err(anyhow!("query too long (max 200 chars)"));
        }
        let limit = args
            .get("limit")
            .and_then(|v| v.as_i64())
            .unwrap_or(10)
            .clamp(1, MAX_RESULTS);

        let hits = match self.store.search_messages(query, limit) {
            Ok(h) => h,
            // FTS5 查询语法非法（如裸特殊字符）→ 提示而非崩溃
            Err(e) if e.to_string().contains("fts5") || e.to_string().contains("syntax error") => {
                return Ok(format!(
                    "no results (invalid query for full-text search: {})",
                    e
                ));
            }
            Err(e) => return Err(anyhow!("search failed: {}", e)),
        };

        if hits.is_empty() {
            return Ok("no matching history found".to_string());
        }
        let mut out = String::new();
        for (i, h) in hits.iter().enumerate() {
            let short = short_uuid(&h.session_uuid);
            out.push_str(&format!(
                "{}. [{} | {} | {}] {}\n",
                i + 1,
                short,
                h.channel,
                h.created_at,
                snippet(&h.content)
            ));
        }
        out.push_str(&format!(
            "({} result(s), session ids truncated)",
            hits.len()
        ));
        Ok(out)
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
    async fn test_entry_date_uses_configured_timezone() {
        let dir = tempdir().unwrap();
        let mem_path = dir.path().join("MEMORY.md");
        let user_path = dir.path().join("USER.md");
        let tool = MemoryWrite::new(mem_path.clone(), user_path, true)
            .with_timezone(Some("Asia/Shanghai".into()));
        tool.execute(&serde_json::json!({"entry": "tz check"}), "cli")
            .await
            .unwrap();

        let content = tokio::fs::read_to_string(&mem_path).await.unwrap();
        let expected = crate::time::now(&Some("Asia/Shanghai".into())).ymd();
        assert!(content.contains(&format!("- [{}] tz check", expected)));
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
        assert!(result.unwrap_err().to_string().contains("sub-agent"));
    }

    // ---- memory_research ----
    fn research_store() -> Arc<SessionStore> {
        let store = SessionStore::open_in_memory().unwrap();
        let s1 = store.create_session("uuid-aaaaaaaa-0001", "web").unwrap();
        store
            .append_message(
                s1,
                &crate::provider::Role::User,
                "how to fix borrow checker",
            )
            .unwrap();
        store
            .append_message(
                s1,
                &crate::provider::Role::Assistant,
                "try splitting borrows",
            )
            .unwrap();
        let s2 = store.create_session("uuid-bbbbbbbb-0002", "cli").unwrap();
        store
            .append_message(s2, &crate::provider::Role::User, "rust is fun")
            .unwrap();
        store
            .append_message(s2, &crate::provider::Role::Tool, "tool noise borrow")
            .unwrap();
        Arc::new(store)
    }

    #[tokio::test]
    async fn test_memory_research_finds_cross_session() {
        let tool = MemoryResearch::new(research_store());
        // FTS 短语需整句加双引号（裸 "borrow checker" 会被解析为两列）
        let out = tool
            .execute(&serde_json::json!({"query": "\"borrow checker\""}), "cli")
            .await
            .unwrap();
        // 命中来自 s1 的 user 消息；不索引 tool 消息（tool noise 不应命中）
        assert!(out.contains("fix borrow checker"), "got: {}", out);
        // 命中携带所属 session 短 id（动态计算 expected，避免硬编码错位）
        let expected_short = short_uuid("uuid-aaaaaaaa-0001");
        assert!(out.contains(&expected_short), "got: {}", out);
        assert!(
            !out.contains("tool noise"),
            "tool messages should not be indexed: {}",
            out
        );
    }

    #[tokio::test]
    async fn test_memory_research_no_match() {
        let tool = MemoryResearch::new(research_store());
        let out = tool
            .execute(&serde_json::json!({"query": "zebra"}), "cli")
            .await
            .unwrap();
        assert!(out.contains("no matching history"));
    }

    #[tokio::test]
    async fn test_memory_research_requires_query() {
        let tool = MemoryResearch::new(research_store());
        let res = tool.execute(&serde_json::json!({"limit": 5}), "cli").await;
        assert!(res.is_err());
    }
}
