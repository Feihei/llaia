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

/// 会话列表项：SessionRow + 消息数（WebUI 会话历史，P5 W1）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionListItem {
    pub session_uuid: String,
    pub channel: String,
    pub created_at: String,
    pub last_activity: String,
    pub token_count: i64,
    pub state: String,
    pub message_count: i64,
}

/// 消息 + 关联工具调用（WebUI 会话详情，P5 W1）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct MessageDetail {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub reasoning_content: Option<String>,
    pub created_at: String,
    pub tool_calls: Vec<ToolCallDetail>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolCallDetail {
    pub id: i64,
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
        // 排除 cron/dream 自动会话：主对话 session 的 source 为 main/web/cli 等，
        // 而复活的 cron 会话会把 last_activity 刷到最新，若不加过滤会把主对话路由进
        // cron 会话（ADR-0013 会话隔离）。详见 cron 任务诊断。
        let mut stmt = conn.prepare(
            "SELECT id, session_uuid FROM sessions WHERE channel NOT LIKE 'cron:%' ORDER BY last_activity DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            Ok(Some((row.get(0)?, row.get(1)?)))
        } else {
            Ok(None)
        }
    }

    /// 按 channel 精确查找最近一次（last_activity 最大）的会话 id。
    /// cron 复用同一任务会话时用：同一 `cron:<id>` 只应有一个活跃会话，
    /// 历史重复的孤儿会话取最新者复用，避免每次触发都新建会话。
    pub fn session_by_channel(&self, channel: &str) -> Result<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id FROM sessions WHERE channel = ?1 ORDER BY last_activity DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![channel])?;
        Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
    }

    /// 反查某会话的 channel（/new 新建会话时沿用当前会话的 channel）。
    pub fn channel_of(&self, session_id: i64) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT channel FROM sessions WHERE id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![session_id])?;
        Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
    }

    /// 由 session_id 反查 session_uuid（ADR-0024 todo 按 session_uuid 分桶落盘用）。
    pub fn session_uuid(&self, session_id: i64) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT session_uuid FROM sessions WHERE id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![session_id])?;
        Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
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

    /// 会话列表（含消息数），按 last_activity 降序，分页（P5 W1 WebUI 会话历史）。
    pub fn list_sessions(&self, limit: i64, offset: i64) -> Result<Vec<SessionListItem>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT s.session_uuid, s.channel, s.created_at, s.last_activity, s.token_count, s.state,
                    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) AS message_count
             FROM sessions s
             ORDER BY s.last_activity DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit, offset], |row| {
            Ok(SessionListItem {
                session_uuid: row.get(0)?,
                channel: row.get(1)?,
                created_at: row.get(2)?,
                last_activity: row.get(3)?,
                token_count: row.get(4)?,
                state: row.get(5)?,
                message_count: row.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 按 session_uuid 查会话，返回 (内部 id, 行)；不存在返回 None（P5 W1）。
    pub fn session_by_uuid(&self, uuid: &str) -> Result<Option<(i64, SessionRow)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_uuid, channel, created_at, last_activity, token_count, state
             FROM sessions WHERE session_uuid = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![uuid])?;
        if let Some(row) = rows.next()? {
            Ok(Some((
                row.get(0)?,
                SessionRow {
                    session_uuid: row.get(1)?,
                    channel: row.get(2)?,
                    created_at: row.get(3)?,
                    last_activity: row.get(4)?,
                    token_count: row.get(5)?,
                    state: row.get(6)?,
                },
            )))
        } else {
            Ok(None)
        }
    }

    /// 单会话完整消息（含 tool_calls），按 id 升序（P5 W1 会话详情）。
    /// 单个 Mutex 作用域内完成，避免嵌套 lock 死锁。
    pub fn messages_with_tool_calls(&self, session_id: i64) -> Result<Vec<MessageDetail>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, role, content, reasoning_content, created_at
             FROM messages WHERE session_id = ?1 ORDER BY id ASC",
        )?;
        let msgs: Vec<(i64, String, String, Option<String>, String)> = stmt
            .query_map(rusqlite::params![session_id], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<Result<_, _>>()?;

        let mut out = Vec::with_capacity(msgs.len());
        for (msg_id, role, content, reasoning, created) in msgs {
            let mut tc_stmt = conn.prepare(
                "SELECT id, tool_call_id, tool_name, payload, outcome, created_at
                 FROM tool_calls WHERE message_id = ?1 ORDER BY id ASC",
            )?;
            let tool_calls = tc_stmt
                .query_map(rusqlite::params![msg_id], |r| {
                    Ok(ToolCallDetail {
                        id: r.get(0)?,
                        tool_call_id: r.get(1)?,
                        tool_name: r.get(2)?,
                        payload: r.get(3)?,
                        outcome: r.get(4)?,
                        created_at: r.get(5)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            out.push(MessageDetail {
                id: msg_id,
                role,
                content,
                reasoning_content: reasoning,
                created_at: created,
                tool_calls,
            });
        }
        Ok(out)
    }

    /// 删除会话（cascade 删 messages/tool_calls）。返回是否真的删了（P5 W1）。
    pub fn delete_session(&self, uuid: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM sessions WHERE session_uuid = ?1",
            rusqlite::params![uuid],
        )?;
        Ok(n > 0)
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
    fn test_session_by_channel_returns_latest() {
        let store = SessionStore::open_in_memory().unwrap();
        let s1 = store.create_session("uuid1", "cron:morning_news").unwrap();
        // 推进 last_activity，使 s2 成为最新
        std::thread::sleep(std::time::Duration::from_millis(5));
        let s2 = store.create_session("uuid2", "cron:morning_news").unwrap();
        // 非该 channel 的会话不应被命中
        store.create_session("uuid3", "cli").unwrap();

        let got = store.session_by_channel("cron:morning_news").unwrap();
        assert_eq!(got, Some(s2));
        assert_ne!(got, Some(s1));
        assert_eq!(store.session_by_channel("cron:nonexistent").unwrap(), None);
    }

    #[test]
    fn test_channel_of() {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("uuid1", "web").unwrap();
        assert_eq!(store.channel_of(sid).unwrap(), Some("web".to_string()));
        assert_eq!(store.channel_of(99999).unwrap(), None);
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

    // ---- P5 W1 WebUI 会话历史 ----

    #[test]
    fn test_list_sessions_with_message_count() {
        let store = SessionStore::open_in_memory().unwrap();
        let sid1 = store.create_session("uuid-1", "cli").unwrap();
        let sid2 = store.create_session("uuid-2", "web").unwrap();
        store.append_message(sid1, &Role::User, "a").unwrap();
        store.append_message(sid1, &Role::Assistant, "b").unwrap();
        store.append_message(sid2, &Role::User, "c").unwrap();

        let items = store.list_sessions(10, 0).unwrap();
        // 按 last_activity 降序：uuid-2 后创建在前
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].session_uuid, "uuid-2");
        assert_eq!(items[0].message_count, 1);
        assert_eq!(items[1].session_uuid, "uuid-1");
        assert_eq!(items[1].message_count, 2);
        assert_eq!(items[1].channel, "cli");
    }

    #[test]
    fn test_list_sessions_pagination() {
        let store = SessionStore::open_in_memory().unwrap();
        for i in 0..5 {
            store.create_session(&format!("uuid-{}", i), "cli").unwrap();
        }
        let page = store.list_sessions(2, 1).unwrap();
        assert_eq!(page.len(), 2);
        // offset=1 跳过最新一条
        assert_eq!(page[0].session_uuid, "uuid-3");
        assert_eq!(page[1].session_uuid, "uuid-2");
    }

    #[test]
    fn test_session_by_uuid_found_and_missing() {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("uuid-x", "cli").unwrap();
        let (found_id, row) = store.session_by_uuid("uuid-x").unwrap().unwrap();
        assert_eq!(found_id, sid);
        assert_eq!(row.channel, "cli");
        assert!(store.session_by_uuid("nope").unwrap().is_none());
    }

    #[test]
    fn test_messages_with_tool_calls() {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("uuid-y", "cli").unwrap();
        let m1 = store.append_message(sid, &Role::User, "hi").unwrap();
        let m2 = store
            .append_message(sid, &Role::Assistant, "let me check")
            .unwrap();
        store
            .append_tool_call(
                m2,
                "call_1",
                "file_read",
                r#"{"path":"x"}"#,
                Some("content"),
            )
            .unwrap();

        let details = store.messages_with_tool_calls(sid).unwrap();
        assert_eq!(details.len(), 2);
        assert_eq!(details[0].role, "user");
        assert_eq!(details[0].tool_calls.len(), 0);
        assert_eq!(details[1].role, "assistant");
        assert_eq!(details[1].tool_calls.len(), 1);
        assert_eq!(details[1].tool_calls[0].tool_name, "file_read");
        assert_eq!(details[1].tool_calls[0].outcome.as_deref(), Some("content"));
        assert_eq!(details[0].id, m1);
    }

    #[test]
    fn test_delete_session_cascades() {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("uuid-z", "cli").unwrap();
        let m = store.append_message(sid, &Role::User, "hello").unwrap();
        store
            .append_tool_call(m, "call_1", "terminal", "{}", Some("out"))
            .unwrap();

        assert!(store.delete_session("uuid-z").unwrap());
        // cascade：messages/tool_calls 一并删除
        let conn = store.conn.lock().unwrap();
        let msg_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        let tc_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(msg_count, 0);
        assert_eq!(tc_count, 0);
        drop(conn);
        // 二次删除返回 false
        assert!(!store.delete_session("uuid-z").unwrap());
    }
}
