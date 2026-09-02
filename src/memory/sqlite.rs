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

/// memory_research（plan.md）：FTS5 全文搜索命中，含所属 session 信息。
#[derive(Debug, Clone)]
pub struct MessageFtsHit {
    pub message_id: i64,
    pub session_uuid: String,
    pub channel: String,
    pub role: String,
    pub content: String,
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

/// 未归档任务线（ADR-0031 /tasks 列表）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskSessionRow {
    pub session_id: i64,
    pub session_uuid: String,
    pub title: String,
    pub bound_path: Option<String>,
    pub channel: String,
    pub last_activity: String,
}

/// /btw 侧问记录（plan.md #H，独立表不进 FTS）。
#[derive(Debug, Clone)]
pub struct SideMessageRow {
    pub question: String,
    pub answer: String,
    pub created_at: String,
}

/// 会话类型信息（ADR-0031）：kind（main/task）+ title + bound_path。
#[derive(Debug, Clone)]
pub struct SessionKindInfo {
    pub kind: String,
    pub title: Option<String>,
    pub bound_path: Option<String>,
}

// ---- plan.md W3 token 用量 dashboard ----

/// 一次模型调用的 token 用量（plan.md W3）：逐请求记录，供 Stats dashboard 聚合。
#[derive(Debug, Clone)]
pub struct TurnUsage {
    pub session_id: i64,
    pub model_ref: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    /// 'chat'（默认）/ 'compact' / 'vision' / 'reminder' 等 sidecar 调用区分
    pub kind: String,
}

/// 单天 token 用量 bucket（plan.md W3-④ Stats dashboard）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TokenDayBucket {
    pub date: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub requests: i64,
}

/// 分组统计行（per-model / per-session 排名）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TokenGroupRow {
    pub name: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub requests: i64,
}

/// `token_stats()` 返回的完整聚合结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TokenStats {
    pub days: u32,
    pub total_prompt: i64,
    pub total_completion: i64,
    pub total_tokens: i64,
    pub total_requests: i64,
    pub series: Vec<TokenDayBucket>,
    pub by_model: Vec<TokenGroupRow>,
    pub by_session: Vec<TokenGroupRow>,
}

/// 取 session_uuid 的前 8 字符做短名。
pub fn short_uuid(uuid: &str) -> String {
    uuid.chars().take(8).collect()
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
    /// 会话主题标题（压缩时由 compact provider 生成，plan.md 会话主题自动总结）；
    /// 未生成过为 None，前端回退显示 channel。
    pub title: Option<String>,
    /// 会话类型（ADR-0031）：'main'（通用线，默认）/ 'task'（任务线）。
    pub kind: String,
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
    state         TEXT NOT NULL DEFAULT 'idle',
    title         TEXT,
    kind          TEXT NOT NULL DEFAULT 'main',
    bound_path    TEXT
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

-- /btw 侧问问答（plan.md #H）：独立表落库，不进 messages/FTS——上下文零污染，
-- 留作可回查记录；是否扩进 memory_research 检索待有真实需求再议。
CREATE TABLE IF NOT EXISTS side_messages (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    question   TEXT NOT NULL,
    answer     TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_side_messages_session ON side_messages(session_id);

-- 逐请求 token 用量（plan.md W3 token dashboard）：一行 = 一次模型调用
CREATE TABLE IF NOT EXISTS turn_usage (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id        INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    ts                TEXT NOT NULL,
    model_ref         TEXT NOT NULL,
    prompt_tokens     INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    kind              TEXT NOT NULL DEFAULT 'chat'
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
CREATE INDEX IF NOT EXISTS idx_tool_calls_message ON tool_calls(message_id);
CREATE INDEX IF NOT EXISTS idx_turn_usage_ts ON turn_usage(ts);
CREATE INDEX IF NOT EXISTS idx_turn_usage_session ON turn_usage(session_id);

-- memory_research（plan.md）：跨会话全文索引。只索引 user/assistant 正文
-- （system 提示词与 tool 输出是噪音，不参与历史记忆检索）。
CREATE VIRTUAL TABLE IF NOT EXISTS message_fts USING fts5(content, tokenize='unicode61');
CREATE TRIGGER IF NOT EXISTS trg_message_fts_insert AFTER INSERT ON messages
BEGIN
    INSERT INTO message_fts(rowid, content)
        SELECT NEW.id, NEW.content WHERE NEW.role IN ('user','assistant');
END;
CREATE TRIGGER IF NOT EXISTS trg_message_fts_delete AFTER DELETE ON messages
BEGIN
    DELETE FROM message_fts WHERE rowid = OLD.id;
END;
"#,
        )?;
        // 存量回填：仅补 user/assistant 里尚未入索引的行（升级既有库时一次性补齐），幂等。
        conn.execute_batch(
            "INSERT INTO message_fts(rowid, content)
             SELECT m.id, m.content FROM messages m
             WHERE m.role IN ('user','assistant')
               AND m.id NOT IN (SELECT rowid FROM message_fts);",
        )?;
        // 存量库幂等补列（sqlite 的 ALTER TABLE ADD COLUMN 没有 IF NOT EXISTS，
        // 先查 table_info 再补）：title（会话主题）/ kind、bound_path（ADR-0031 任务线）
        for (col, ddl) in [
            ("title", "ALTER TABLE sessions ADD COLUMN title TEXT;"),
            (
                "kind",
                "ALTER TABLE sessions ADD COLUMN kind TEXT NOT NULL DEFAULT 'main';",
            ),
            (
                "bound_path",
                "ALTER TABLE sessions ADD COLUMN bound_path TEXT;",
            ),
        ] {
            let has = conn
                .prepare("PRAGMA table_info(sessions)")?
                .query_map([], |r| r.get::<_, String>(1))?
                .filter_map(|r| r.ok())
                .any(|name| name == col);
            if !has {
                conn.execute_batch(ddl)?;
            }
        }
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

    /// 读会话标题（未生成过为 None；plan.md 会话主题自动总结）。
    pub fn session_title(&self, session_id: i64) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT title FROM sessions WHERE id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![session_id])?;
        match rows.next()? {
            Some(row) => Ok(row.get(0)?),
            None => Ok(None),
        }
    }

    /// 写会话标题（压缩时由 compact provider 生成；幂等覆盖）。
    pub fn set_session_title(&self, session_id: i64, title: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET title = ?2 WHERE id = ?1",
            rusqlite::params![session_id, title],
        )?;
        Ok(())
    }

    pub fn latest_session(&self) -> Result<Option<(i64, String)>> {
        let conn = self.conn.lock().unwrap();
        // 排除 cron 自动会话：主对话 session 的 source 为 main/web/cli 等，
        // 而复活的 cron 会话会把 last_activity 刷到最新，若不加过滤会把主对话路由进
        // cron 会话（ADR-0013 会话隔离）。详见 cron 任务诊断。
        // 归档任务线（ADR-0031 state='archived'）也不续接——归档即不可续写。
        let mut stmt = conn.prepare(
            "SELECT id, session_uuid FROM sessions
             WHERE channel NOT LIKE 'cron:%' AND state != 'archived'
             ORDER BY last_activity DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            Ok(Some((row.get(0)?, row.get(1)?)))
        } else {
            Ok(None)
        }
    }

    // ---- ADR-0031 任务线 ----

    /// 创建任务线 session：kind='task'，title 即任务名（查找键），bound_path 为
    /// 创建时的工作目录（!= home 时才绑定；纯元数据，不参与审批/执行判定）。
    pub fn create_task_session(
        &self,
        session_uuid: &str,
        channel: &str,
        title: &str,
        bound_path: Option<&str>,
    ) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (session_uuid, channel, created_at, last_activity, state, title, kind, bound_path)
             VALUES (?1, ?2, ?3, ?3, 'idle', ?4, 'task', ?5)",
            rusqlite::params![session_uuid, channel, now, title, bound_path],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 按任务名查找未归档任务线（`/task <名>` 的切换键；同名取最近活跃）。
    pub fn find_open_task(&self, title: &str) -> Result<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id FROM sessions
             WHERE kind = 'task' AND state != 'archived' AND title = ?1
             ORDER BY last_activity DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![title])?;
        Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
    }

    /// 列出所有未归档任务线（/tasks）。
    pub fn list_open_tasks(&self) -> Result<Vec<TaskSessionRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_uuid, COALESCE(title, ''), bound_path, channel, last_activity
             FROM sessions WHERE kind = 'task' AND state != 'archived'
             ORDER BY last_activity DESC LIMIT 200",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(TaskSessionRow {
                session_id: r.get(0)?,
                session_uuid: r.get(1)?,
                title: r.get(2)?,
                bound_path: r.get(3)?,
                channel: r.get(4)?,
                last_activity: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 归档会话（`/task close`）：state='archived' 后不可续写，消息仍可被检索。
    pub fn archive_session(&self, session_id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET state = 'archived' WHERE id = ?1",
            rusqlite::params![session_id],
        )?;
        Ok(())
    }

    /// 读会话类型信息（ADR-0031）；不存在返回 None。
    pub fn session_kind(&self, session_id: i64) -> Result<Option<SessionKindInfo>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT kind, title, bound_path FROM sessions WHERE id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![session_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(SessionKindInfo {
                kind: row.get(0)?,
                title: row.get(1)?,
                bound_path: row.get(2)?,
            })),
            None => Ok(None),
        }
    }

    /// 最近的通用线（kind='main'，排除 cron 与归档）——`/task` 无参 / close 的回归目标。
    pub fn latest_main_session(&self) -> Result<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id FROM sessions
             WHERE kind = 'main' AND channel NOT LIKE 'cron:%' AND state != 'archived'
             ORDER BY last_activity DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
    }

    /// 会话尾部消息回灌（ADR-0031 切线）：从最新往回取，按字符预算封顶、
    /// 不截断半条消息（首条超预算的单条消息也保留，保证至少回灌一条）。
    /// 供 slash 切线路径把目标线 sqlite 尾部装回内存 context。
    pub fn recent_messages_within_budget(
        &self,
        session_id: i64,
        char_budget: usize,
    ) -> Result<Vec<MessageRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, reasoning_content, created_at
             FROM messages WHERE session_id = ?1 ORDER BY id DESC LIMIT 500",
        )?;
        let desc: Vec<MessageRow> = stmt
            .query_map(rusqlite::params![session_id], |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    reasoning_content: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        let mut picked: Vec<MessageRow> = Vec::new();
        let mut used = 0usize;
        for m in desc {
            let len = m.content.chars().count();
            if !picked.is_empty() && used + len > char_budget {
                break;
            }
            used += len;
            picked.push(m);
            if used >= char_budget {
                break;
            }
        }
        picked.reverse();
        Ok(picked)
    }

    // ---- /btw 侧问（plan.md #H）----

    /// 落一条侧问问答（独立表，不进 messages/FTS）。
    pub fn add_side_message(&self, session_id: i64, question: &str, answer: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO side_messages (session_id, question, answer, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, question, answer, now],
        )?;
        Ok(())
    }

    /// 最近的侧问问答（自连上下文用，倒序取最新 N 条）。
    pub fn recent_side_messages(&self, session_id: i64, limit: i64) -> Result<Vec<SideMessageRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT question, answer, created_at FROM side_messages
             WHERE session_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id, limit], |r| {
            Ok(SideMessageRow {
                question: r.get(0)?,
                answer: r.get(1)?,
                created_at: r.get(2)?,
            })
        })?;
        let mut out = rows.collect::<Result<Vec<_>, _>>()?;
        out.reverse(); // 时间正序
        Ok(out)
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

    /// 跨会话全文搜索历史消息（plan.md memory_research，FTS5）。
    /// `limit` 由调用方 clamp（工具侧硬上限 20）。返回按相关性排序的命中，
    /// 含所属 session 短 id、channel 与时间；content 由调用方截断展示。
    pub fn search_messages(&self, query: &str, limit: i64) -> Result<Vec<MessageFtsHit>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT m.id, s.session_uuid, s.channel, m.role, m.content, m.created_at
             FROM message_fts f
             JOIN messages m ON m.id = f.rowid
             JOIN sessions  s ON s.id = m.session_id
             WHERE f.content MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![query, limit], |r| {
            Ok(MessageFtsHit {
                message_id: r.get(0)?,
                session_uuid: r.get(1)?,
                channel: r.get(2)?,
                role: r.get(3)?,
                content: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?;
        let hits: rusqlite::Result<Vec<MessageFtsHit>> = rows.collect();
        let mut hits = hits?;
        hits.sort_by(|a, b| b.created_at.cmp(&a.created_at)); // 同相关度按新优先
        Ok(hits)
    }

    pub fn update_token_count(&self, session_id: i64, delta: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET token_count = token_count + ?1 WHERE id = ?2",
            rusqlite::params![delta, session_id],
        )?;
        Ok(())
    }

    /// 记录一次模型调用（一次 chat_stream）的 token 用量（plan.md W3）。
    pub fn add_turn_usage(&self, u: &TurnUsage) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO turn_usage (session_id, ts, model_ref, prompt_tokens, completion_tokens, kind) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                u.session_id,
                now,
                u.model_ref,
                u.prompt_tokens,
                u.completion_tokens,
                u.kind
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 逐模型 token 用量聚合（plan.md W3-④ Stats dashboard）。
    /// 仅统计主对话（kind='chat'），供 `GET /api/stats/tokens?days=N` 使用。
    pub fn token_stats(&self, days: u32) -> Result<TokenStats> {
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(days as i64)).to_rfc3339();
        let conn = self.conn.lock().unwrap();

        // 总计
        let (total_prompt, total_completion, total_requests) = conn.query_row(
            "SELECT COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0), COUNT(*)
             FROM turn_usage WHERE kind='chat' AND ts >= ?1",
            rusqlite::params![cutoff],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )?;

        // 天级 bucket（ts 为 RFC3339 UTC，取前 10 字符得 YYYY-MM-DD）
        let mut by_day: std::collections::HashMap<String, (i64, i64, i64)> =
            std::collections::HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT substr(ts,1,10), SUM(prompt_tokens), SUM(completion_tokens), COUNT(*)
                 FROM turn_usage WHERE kind='chat' AND ts >= ?1 GROUP BY substr(ts,1,10)",
            )?;
            let rows = stmt.query_map(rusqlite::params![cutoff], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                let (day, p, c, n) = row?;
                by_day.insert(day, (p, c, n));
            }
        }
        // 补全 last-days 天标签（含无数据的天，前端柱状图对齐 X 轴）
        let mut series = Vec::with_capacity(days as usize);
        for i in (0..days).rev() {
            let day = (chrono::Utc::now() - chrono::Duration::days(i as i64))
                .format("%Y-%m-%d")
                .to_string();
            let (p, c, n) = by_day.get(&day).copied().unwrap_or((0, 0, 0));
            series.push(TokenDayBucket {
                date: day,
                prompt_tokens: p,
                completion_tokens: c,
                requests: n,
            });
        }

        // per-model 排名
        let mut by_model = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT model_ref, SUM(prompt_tokens), SUM(completion_tokens), COUNT(*)
                 FROM turn_usage WHERE kind='chat' AND ts >= ?1
                 GROUP BY model_ref ORDER BY SUM(prompt_tokens)+SUM(completion_tokens) DESC",
            )?;
            let rows = stmt.query_map(rusqlite::params![cutoff], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                let (m, p, c, n) = row?;
                by_model.push(TokenGroupRow {
                    name: m,
                    prompt_tokens: p,
                    completion_tokens: c,
                    requests: n,
                });
            }
        }

        // per-session 排名（Top10，join sessions 取 uuid）
        let mut by_session = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT s.session_uuid, SUM(u.prompt_tokens), SUM(u.completion_tokens), COUNT(*)
                 FROM turn_usage u JOIN sessions s ON s.id = u.session_id
                 WHERE u.kind='chat' AND u.ts >= ?1
                 GROUP BY u.session_id
                 ORDER BY SUM(u.prompt_tokens)+SUM(u.completion_tokens) DESC LIMIT 10",
            )?;
            let rows = stmt.query_map(rusqlite::params![cutoff], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                let (u, p, c, n) = row?;
                by_session.push(TokenGroupRow {
                    name: short_uuid(&u),
                    prompt_tokens: p,
                    completion_tokens: c,
                    requests: n,
                });
            }
        }

        Ok(TokenStats {
            days,
            total_prompt,
            total_completion,
            total_tokens: total_prompt + total_completion,
            total_requests,
            series,
            by_model,
            by_session,
        })
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

    // ---- cron 会话过滤 ----

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
                    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) AS message_count,
                    s.title, s.kind
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
                title: row.get(7)?,
                kind: row.get(8)?,
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

    /// 删除某会话内指定 ids 的消息（可多条）。tool_calls 由外键 ON DELETE CASCADE 级联清理，
    /// FTS 由触发器清理。返回实际删除条数。
    pub fn delete_messages(&self, session_id: i64, ids: &[i64]) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let conn = self.conn.lock().unwrap();
        let placeholders: Vec<_> = std::iter::repeat("?").take(ids.len()).collect();
        let sql = format!(
            "DELETE FROM messages WHERE session_id = ?1 AND id IN ({})",
            placeholders.join(",")
        );
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(ids.len() + 1);
        params.push(&session_id);
        for id in ids {
            params.push(id);
        }
        let n = conn.execute(&sql, rusqlite::params_from_iter(params))?;
        Ok(n as u64)
    }

    /// 该 message id 是否属于指定会话（用于删除前校验）。
    pub fn message_in_session(&self, session_id: i64, msg_id: i64) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND id = ?2",
            rusqlite::params![session_id, msg_id],
            |r| r.get(0),
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

    #[test]
    fn test_delete_messages_ids() {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("uuid-del", "cli").unwrap();
        let m1 = store.append_message(sid, &Role::User, "keep me").unwrap();
        let m2 = store
            .append_message(sid, &Role::Assistant, "loop marker X7F")
            .unwrap();
        let m3 = store
            .append_message(sid, &Role::Tool, "bloat X7F")
            .unwrap();
        store
            .append_tool_call(m2, "call_loop", "terminal", "{}", Some("out"))
            .unwrap();

        // 归属校验
        assert!(store.message_in_session(sid, m2).unwrap());
        assert!(!store.message_in_session(sid, 999_999).unwrap());
        assert!(store.delete_messages(sid, &[]).unwrap() == 0);

        // 删除 m2 与 m3，留下 m1
        assert_eq!(store.delete_messages(sid, &[m2, m3]).unwrap(), 2);
        let left = store.recent_messages(sid, 10).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, m1);

        // 级联：m2 的工具调用一并删除
        let conn = store.conn.lock().unwrap();
        let tc_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tc_count, 0);
        drop(conn);

        // FTS：删除的行不再命中
        assert!(store.search_messages("X7F", 10).unwrap().is_empty());
    }

    #[test]
    fn test_token_stats_aggregation() {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("uuid-stats", "cli").unwrap();
        store
            .add_turn_usage(&TurnUsage {
                session_id: sid,
                model_ref: "model-a".into(),
                prompt_tokens: 100,
                completion_tokens: 50,
                kind: "chat".into(),
            })
            .unwrap();
        // sidecar（kind != chat）计入 kind 字段但默认统计被排除
        store
            .add_turn_usage(&TurnUsage {
                session_id: sid,
                model_ref: "model-a".into(),
                prompt_tokens: 999,
                completion_tokens: 1,
                kind: "compact".into(),
            })
            .unwrap();

        let stats = store.token_stats(7).unwrap();
        assert_eq!(stats.total_prompt, 100);
        assert_eq!(stats.total_completion, 50);
        assert_eq!(stats.total_tokens, 150);
        assert_eq!(stats.total_requests, 1);
        assert_eq!(stats.by_model.len(), 1);
        assert_eq!(stats.by_model[0].name, "model-a");
        assert_eq!(stats.by_model[0].prompt_tokens, 100);
        assert_eq!(stats.by_session.len(), 1);
        assert_eq!(stats.by_session[0].prompt_tokens, 100);
        // 天级序列补齐为 7 个 bucket，其中一个含数据
        assert_eq!(stats.series.len(), 7);
        let total_in_series: i64 = stats
            .series
            .iter()
            .map(|d| d.prompt_tokens + d.completion_tokens)
            .sum();
        assert_eq!(total_in_series, 150);
        assert_eq!(stats.series.iter().map(|d| d.requests).sum::<i64>(), 1);
    }

    #[test]
    fn test_session_title_round_trip() {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("uuid-title", "web").unwrap();
        // 新会话无标题
        assert_eq!(store.session_title(sid).unwrap(), None);
        assert_eq!(store.list_sessions(10, 0).unwrap()[0].title, None);

        store.set_session_title(sid, "配置迁移讨论").unwrap();
        assert_eq!(
            store.session_title(sid).unwrap().as_deref(),
            Some("配置迁移讨论")
        );
        // list_sessions 一并带出（WebUI 会话列表展示）
        let items = store.list_sessions(10, 0).unwrap();
        let item = items
            .iter()
            .find(|i| i.session_uuid == "uuid-title")
            .unwrap();
        assert_eq!(item.title.as_deref(), Some("配置迁移讨论"));

        // 幂等覆盖
        store.set_session_title(sid, "新标题").unwrap();
        assert_eq!(store.session_title(sid).unwrap().as_deref(), Some("新标题"));

        // 不存在的会话：读返回 None，写不报错（影响 0 行）
        assert_eq!(store.session_title(99999).unwrap(), None);
        store.set_session_title(99999, "x").unwrap();
    }

    #[test]
    fn test_title_column_added_to_legacy_db() {
        // 存量库（旧 schema 无 title 列）经 init_schema 幂等补列
        let db_path = std::env::temp_dir().join(format!(
            "llaia-test-title-migrate-{}-{}.db",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_file(&db_path);
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_uuid TEXT NOT NULL UNIQUE,
                    channel TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    last_activity TEXT NOT NULL,
                    token_count INTEGER NOT NULL DEFAULT 0,
                    state TEXT NOT NULL DEFAULT 'idle'
                );
                INSERT INTO sessions (session_uuid, channel, created_at, last_activity)
                    VALUES ('old-1', 'cli', 't', 't');",
            )
            .unwrap();
        }
        let store = SessionStore::open(&db_path).unwrap();
        let sid = store.session_by_uuid("old-1").unwrap().unwrap().0;
        assert_eq!(store.session_title(sid).unwrap(), None);
        store.set_session_title(sid, "回填测试").unwrap();
        assert_eq!(
            store.session_title(sid).unwrap().as_deref(),
            Some("回填测试")
        );
        // 再次打开不报错（幂等），旧行数据保留
        drop(store);
        let store2 = SessionStore::open(&db_path).unwrap();
        assert_eq!(
            store2.session_title(sid).unwrap().as_deref(),
            Some("回填测试")
        );
        std::fs::remove_file(&db_path).ok();
    }

    #[test]
    fn test_task_session_lifecycle() {
        let store = SessionStore::open_in_memory().unwrap();
        let main = store.create_session("main-uuid", "web").unwrap();
        store.append_message(main, &Role::User, "主线消息").unwrap();

        // 创建任务线（带绑定目录）
        let t1 = store
            .create_task_session("task-uuid", "web", "目录整理", Some("/data/docs"))
            .unwrap();
        let info = store.session_kind(t1).unwrap().unwrap();
        assert_eq!(info.kind, "task");
        assert_eq!(info.title.as_deref(), Some("目录整理"));
        assert_eq!(info.bound_path.as_deref(), Some("/data/docs"));

        // 按名查找命中；通用线 kind=main
        assert_eq!(store.find_open_task("目录整理").unwrap(), Some(t1));
        assert_eq!(store.find_open_task("不存在").unwrap(), None);
        assert_eq!(
            store.session_kind(main).unwrap().unwrap().kind,
            "main".to_string()
        );

        // /tasks 列表带出
        let tasks = store.list_open_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "目录整理");
        assert_eq!(tasks[0].bound_path.as_deref(), Some("/data/docs"));

        // 归档后：不可被 find/list/latest 命中
        store.archive_session(t1).unwrap();
        assert_eq!(store.find_open_task("目录整理").unwrap(), None);
        assert!(store.list_open_tasks().unwrap().is_empty());
        assert_eq!(store.latest_session().unwrap().unwrap().0, main);
        // latest_main_session 排除归档任务线
        assert_eq!(store.latest_main_session().unwrap(), Some(main));
    }

    #[test]
    fn test_latest_main_session_prefers_recent() {
        let store = SessionStore::open_in_memory().unwrap();
        let m1 = store.create_session("m1", "cli").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.create_session("cron:x", "cron:x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let m2 = store.create_session("m2", "web").unwrap();
        // cron 会话 last_activity 更新也不能抢走 main 归属
        assert_eq!(store.latest_main_session().unwrap(), Some(m2));
        assert_ne!(store.latest_main_session().unwrap(), Some(m1));
    }

    #[test]
    fn test_recent_messages_within_budget() {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("uuid-bf", "cli").unwrap();
        for i in 0..6 {
            store
                .append_message(sid, &Role::User, &format!("msg{}", i))
                .unwrap();
        }
        // 预算只够 2 条（每条 4 字符）
        let msgs = store.recent_messages_within_budget(sid, 8).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "msg4");
        assert_eq!(msgs[1].content, "msg5");
        // 不截断半条：预算 5 仍取 2 条（msg5 单条已超也不丢首条）
        let msgs = store.recent_messages_within_budget(sid, 5).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "msg5");
        // 大预算全量、时间正序
        let msgs = store.recent_messages_within_budget(sid, 10_000).unwrap();
        assert_eq!(msgs.len(), 6);
        assert_eq!(msgs[0].content, "msg0");
    }

    #[test]
    fn test_side_messages_round_trip() {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("uuid-side", "web").unwrap();
        assert!(store.recent_side_messages(sid, 5).unwrap().is_empty());
        store.add_side_message(sid, "q1", "a1").unwrap();
        store.add_side_message(sid, "q2", "a2").unwrap();
        let rows = store.recent_side_messages(sid, 1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].question, "q2");
        let rows = store.recent_side_messages(sid, 5).unwrap();
        assert_eq!(rows.len(), 2);
        // 时间正序：q1 在前
        assert_eq!(rows[0].question, "q1");
        assert_eq!(rows[1].answer, "a2");
        // 不进 messages / FTS
        assert!(store.recent_messages(sid, 100).unwrap().is_empty());
        assert!(store.search_messages("q1", 10).unwrap().is_empty());
    }

    #[test]
    fn test_task_columns_added_to_legacy_db() {
        // 存量库（旧 schema 无 kind/bound_path）经 init_schema 幂等补列
        let db_path = std::env::temp_dir().join(format!(
            "llaia-test-task-migrate-{}-{}.db",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_file(&db_path);
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_uuid TEXT NOT NULL UNIQUE,
                    channel TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    last_activity TEXT NOT NULL,
                    token_count INTEGER NOT NULL DEFAULT 0,
                    state TEXT NOT NULL DEFAULT 'idle',
                    title TEXT
                );
                INSERT INTO sessions (session_uuid, channel, created_at, last_activity)
                    VALUES ('old-1', 'cli', 't', 't');",
            )
            .unwrap();
        }
        let store = SessionStore::open(&db_path).unwrap();
        let sid = store.session_by_uuid("old-1").unwrap().unwrap().0;
        // 旧会话默认 main；list_sessions 带出 kind 供 WebUI 徽标
        assert_eq!(
            store.session_kind(sid).unwrap().unwrap().kind,
            "main".to_string()
        );
        assert_eq!(store.list_sessions(10, 0).unwrap()[0].kind, "main");
        // 新任务线照常创建（kind 列已补）
        let t = store
            .create_task_session("t-new", "cli", "新任务", None)
            .unwrap();
        assert_eq!(store.find_open_task("新任务").unwrap(), Some(t));
        // 幂等重开
        drop(store);
        let store2 = SessionStore::open(&db_path).unwrap();
        assert_eq!(store2.find_open_task("新任务").unwrap(), Some(t));
        std::fs::remove_file(&db_path).ok();
    }
}
