use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::provider::Role;

pub struct SessionStore {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionRow {
    pub session_uuid: String,
    pub channel: String,
    pub created_at: String,
    pub last_activity: String,
    pub token_count: i64,
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: i64,
    pub session_id: i64,
    pub role: String,
    pub content: String,
    pub reasoning_content: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct ToolCallRow {
    pub id: i64,
    pub message_id: i64,
    pub tool_call_id: String,
    pub tool_name: String,
    pub payload: String,
    pub outcome: Option<String>,
    pub created_at: String,
}

impl SessionStore {
    pub fn open(db_path: &PathBuf) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn =
            Connection::open(db_path).with_context(|| format!("open sqlite {:?}", db_path))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
CREATE TABLE IF NOT EXISTS sessions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    session_uuid  TEXT NOT NULL UNIQUE,
    channel       TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    last_activity TEXT NOT NULL,
    token_count   INTEGER NOT NULL DEFAULT 0,
    state         TEXT NOT NULL DEFAULT 'idle'
);

CREATE TABLE IF NOT EXISTS messages (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id        INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role              TEXT NOT NULL,
    content           TEXT NOT NULL,
    reasoning_content TEXT,
    created_at        TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tool_calls (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id   INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    tool_call_id TEXT NOT NULL,
    tool_name    TEXT NOT NULL,
    payload      TEXT NOT NULL,
    outcome      TEXT,
    created_at   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS kv (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
CREATE INDEX IF NOT EXISTS idx_tool_calls_message ON tool_calls(message_id);
"#,
        )?;
        Ok(())
    }

    pub fn create_session(&self, session_uuid: &str, channel: &str) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (session_uuid, channel, created_at, last_activity, state) VALUES (?1, ?2, ?3, ?3, 'idle')",
            rusqlite::params![session_uuid, channel, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn latest_session(&self) -> Result<Option<(i64, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, session_uuid FROM sessions ORDER BY last_activity DESC LIMIT 1")?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            Ok(Some((row.get(0)?, row.get(1)?)))
        } else {
            Ok(None)
        }
    }

    pub fn append_message(&self, session_id: i64, role: &Role, content: &str) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        let role_str = match role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO messages (session_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, role_str, content, now],
        )?;
        let msg_id = conn.last_insert_rowid();
        conn.execute(
            "UPDATE sessions SET last_activity = ?1 WHERE id = ?2",
            rusqlite::params![now, session_id],
        )?;
        Ok(msg_id)
    }

    pub fn append_tool_call(
        &self,
        message_id: i64,
        tool_call_id: &str,
        tool_name: &str,
        payload: &str,
        outcome: Option<&str>,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO tool_calls (message_id, tool_call_id, tool_name, payload, outcome, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![message_id, tool_call_id, tool_name, payload, outcome, now],
        )?;
        Ok(())
    }

    pub fn recent_messages(&self, session_id: i64, limit: i64) -> Result<Vec<MessageRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, reasoning_content, created_at
             FROM messages WHERE session_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows: Result<Vec<MessageRow>, _> = stmt
            .query_map(rusqlite::params![session_id, limit], |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    reasoning_content: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect();
        let mut msgs = rows?;
        msgs.reverse();
        Ok(msgs)
    }

    pub fn update_token_count(&self, session_id: i64, delta: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET token_count = token_count + ?1 WHERE id = ?2",
            rusqlite::params![delta, session_id],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn all_messages(&self, session_id: i64) -> Result<Vec<MessageRow>> {
        self.recent_messages(session_id, i64::MAX)
    }

    // ---- kv 存储（做梦游标等小元数据） ----

    /// 读取 kv 值；key 不存在返回 None。
    pub fn get_kv(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT value FROM kv WHERE key = ?1")?;
        let mut rows = stmt.query(rusqlite::params![key])?;
        Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
    }

    /// 写入 kv 值（upsert）。
    pub fn set_kv(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    // ---- 做梦（Dream）相关 ----

    /// 做梦游标：已处理到的 messages.id 上限。
    /// 首读自动迁移为「当前最大 id」，即做梦不会重放整段老历史。
    pub fn get_last_dream_message_id(&self) -> Result<i64> {
        if let Some(v) = self.get_kv("last_dream_message_id")? {
            if let Ok(n) = v.parse::<i64>() {
                return Ok(n);
            }
        }
        // 迁移：置为当前最大 id
        let max_id: i64 = {
            let conn = self.conn.lock().unwrap();
            conn.query_row("SELECT COALESCE(MAX(id), 0) FROM messages", [], |r| {
                r.get(0)
            })?
        };
        self.set_kv("last_dream_message_id", &max_id.to_string())?;
        Ok(max_id)
    }

    /// 推进做梦游标。
    pub fn set_last_dream_message_id(&self, id: i64) -> Result<()> {
        self.set_kv("last_dream_message_id", &id.to_string())
    }

    /// 读取 id > `after_id` 的增量消息（排除 cron: 会话自身，防做梦轮被自己消化），
    /// 按 id 升序、最多 limit 条。返回内容串好的文本，供做梦蒸馏。
    pub fn messages_after(&self, after_id: i64, limit: i64) -> Result<Vec<MessageRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.role, m.content, m.reasoning_content, m.created_at
             FROM messages m
             JOIN sessions s ON s.id = m.session_id
             WHERE m.id > ?1 AND s.channel NOT LIKE 'cron:%'
             ORDER BY m.id ASC LIMIT ?2",
        )?;
        let rows: Result<Vec<MessageRow>, _> = stmt
            .query_map(rusqlite::params![after_id, limit], |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    reasoning_content: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect();
        Ok(rows?)
    }

    /// 最近一条用户消息（role='user'）的创建时间（RFC3339 字符串）。
    /// 用于做梦的空闲门控：距上次对话多久。
    pub fn last_user_message_time(&self) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT m.created_at
             FROM messages m
             JOIN sessions s ON s.id = m.session_id
             WHERE m.role = 'user' AND s.channel NOT LIKE 'cron:%'
             ORDER BY m.id DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
    }

    /// 按 channel 前缀查询会话（用于 cron 历史过滤，channel LIKE 'cron:%'）。
    /// 按 last_activity 降序，最多 200 条。
    pub fn list_sessions_by_channel_prefix(&self, prefix: &str) -> Result<Vec<SessionRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT session_uuid, channel, created_at, last_activity, token_count, state
             FROM sessions WHERE channel LIKE ?1 ORDER BY last_activity DESC LIMIT 200",
        )?;
        let pattern = format!("{}%", prefix);
        let rows = stmt.query_map(rusqlite::params![pattern], |row| {
            Ok(SessionRow {
                session_uuid: row.get(0)?,
                channel: row.get(1)?,
                created_at: row.get(2)?,
                last_activity: row.get(3)?,
                token_count: row.get(4)?,
                state: row.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Role;
    use tempfile::tempdir;

    fn open_temp() -> SessionStore {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.db");
        SessionStore::open(&path).unwrap()
    }

    #[test]
    fn test_create_and_latest_session() {
        let store = open_temp();
        let id1 = store.create_session("uuid-1", "cli").unwrap();
        let id2 = store.create_session("uuid-2", "cli").unwrap();
        let latest = store.latest_session().unwrap().unwrap();
        assert_eq!(latest.0, id2);
        assert_eq!(latest.1, "uuid-2");
        store.append_message(id1, &Role::User, "hi").unwrap();
        let latest = store.latest_session().unwrap().unwrap();
        assert_eq!(latest.0, id1);
    }

    #[test]
    fn test_append_and_read_messages() {
        let store = open_temp();
        let sid = store.create_session("uuid", "cli").unwrap();
        store.append_message(sid, &Role::User, "hello").unwrap();
        store
            .append_message(sid, &Role::Assistant, "hi back")
            .unwrap();
        let msgs = store.recent_messages(sid, 10).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "hello");
        assert_eq!(msgs[1].content, "hi back");
    }

    #[test]
    fn test_tool_call_persistence() {
        let store = open_temp();
        let sid = store.create_session("uuid", "cli").unwrap();
        let msg_id = store
            .append_message(sid, &Role::Assistant, "calling tool")
            .unwrap();
        store
            .append_tool_call(
                msg_id,
                "call_1",
                "file_read",
                "{\"path\":\"/tmp\"}",
                Some("content"),
            )
            .unwrap();
        let conn = store.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tool_calls WHERE message_id = ?1",
                rusqlite::params![msg_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_list_sessions_by_channel_prefix() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("uuid1", "qq").unwrap();
        store.create_session("uuid2", "cron:morning_news").unwrap();
        store.create_session("uuid3", "web").unwrap();
        store.create_session("uuid4", "cron:health_check").unwrap();

        let rows = store.list_sessions_by_channel_prefix("cron:").unwrap();
        assert_eq!(rows.len(), 2);
        for r in &rows {
            assert!(
                r.channel.starts_with("cron:"),
                "expected channel starting with 'cron:', got {}",
                r.channel
            );
        }
        // 按 last_activity 降序：uuid4 后创建应排在前
        assert_eq!(rows[0].session_uuid, "uuid4");
        assert_eq!(rows[1].session_uuid, "uuid2");
        // 验证字段完整
        assert_eq!(rows[0].channel, "cron:health_check");
        assert_eq!(rows[0].state, "idle");
        assert_eq!(rows[0].token_count, 0);
    }

    #[test]
    fn test_list_sessions_by_channel_prefix_no_match() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("uuid1", "qq").unwrap();
        let rows = store.list_sessions_by_channel_prefix("cron:").unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn test_list_sessions_by_channel_prefix_qq() {
        let store = SessionStore::open_in_memory().unwrap();
        store.create_session("u1", "qq").unwrap();
        store.create_session("u2", "qq").unwrap();
        store.create_session("u3", "web").unwrap();
        let rows = store.list_sessions_by_channel_prefix("qq").unwrap();
        assert_eq!(rows.len(), 2);
        for r in &rows {
            assert_eq!(r.channel, "qq");
        }
    }

    #[test]
    fn test_kv_store_and_read() {
        let store = SessionStore::open_in_memory().unwrap();
        assert_eq!(store.get_kv("missing").unwrap(), None);
        store.set_kv("k", "v").unwrap();
        assert_eq!(store.get_kv("k").unwrap(), Some("v".to_string()));
        store.set_kv("k", "v2").unwrap();
        assert_eq!(store.get_kv("k").unwrap(), Some("v2".to_string()));
    }

    #[test]
    fn test_dream_cursor_migration_and_advance() {
        let store = SessionStore::open_in_memory().unwrap();
        let sid1 = store.create_session("s1", "cli").unwrap();
        let sid2 = store.create_session("s2", "cli").unwrap();
        let _m1 = store.append_message(sid1, &Role::User, "a").unwrap();
        let _m2 = store.append_message(sid2, &Role::Assistant, "b").unwrap();
        let _m3 = store.append_message(sid1, &Role::User, "c").unwrap();
        // 首读：迁移为当前最大 id（3），不重放历史
        assert_eq!(store.get_last_dream_message_id().unwrap(), 3);
        store.set_last_dream_message_id(3).unwrap();
        assert_eq!(store.get_last_dream_message_id().unwrap(), 3);
    }

    #[test]
    fn test_messages_after_excludes_cron() {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("s1", "cli").unwrap();
        let m1 = store.append_message(sid, &Role::User, "u1").unwrap();
        let _m2 = store.append_message(sid, &Role::Assistant, "a1").unwrap();
        let _cron_sid = store.create_session("dream1", "cron:dream").unwrap();
        let after = store.messages_after(0, 100).unwrap();
        assert_eq!(after.len(), 2);
        assert!(after.iter().all(|m| m.id <= m1 + 1));
        // 推进游标后只取新消息
        let later = store.messages_after(m1, 100).unwrap();
        assert_eq!(later.len(), 1);
        assert_eq!(later[0].content, "a1");
    }

    #[test]
    fn test_last_user_message_time() {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("s1", "cli").unwrap();
        store.append_message(sid, &Role::Assistant, "ai").unwrap();
        store.append_message(sid, &Role::User, "human").unwrap();
        let t = store.last_user_message_time().unwrap();
        assert!(t.is_some());
        assert!(t.unwrap().contains('T'));
    }
}
