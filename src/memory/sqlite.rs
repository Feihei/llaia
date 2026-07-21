use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::provider::Role;

pub struct SessionStore {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone)]
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
        let conn = Connection::open(db_path)
            .with_context(|| format!("open sqlite {:?}", db_path))?;
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
        let mut stmt = conn.prepare(
            "SELECT id, session_uuid FROM sessions ORDER BY last_activity DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            Ok(Some((row.get(0)?, row.get(1)?)))
        } else {
            Ok(None)
        }
    }

    pub fn append_message(
        &self,
        session_id: i64,
        role: &Role,
        content: &str,
    ) -> Result<i64> {
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
}
