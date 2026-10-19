use crate::path_guard;
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use rusqlite::OpenFlags;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// 内置只读 SQL 查询（2026-10-19）：消掉「查 sessions.db 等本地 sqlite → Windows
/// 无 sqlite3 CLI → `python -c` 内联被 T3 拦死」的实证摩擦（cron dream 任务首跑
/// 即命中）。rusqlite（bundled）本就是直接依赖，零新增。安全边界是
/// `SQLITE_OPEN_READ_ONLY` 连接标志（SQLite 层硬约束，写/DDL 一律失败），
/// SELECT/WITH 前缀检查只是 UX 提示层，不是边界。范围口径同 FileRead：
/// workspace ∪ 受信 ∪ main 的 subagent/ 额外可读。requires_confirm 缺省 false。
pub struct SqliteQuery {
    workspace: Arc<RwLock<PathBuf>>,
    trusted: Arc<RwLock<Vec<PathBuf>>>,
    is_main: bool,
}

/// 输出行数上限
const SQLITE_MAX_ROWS: usize = 200;
/// 单元格文本截断
const SQLITE_MAX_CELL_CHARS: usize = 200;
/// 整体输出字符上限（防宽表刷爆上下文）
const SQLITE_MAX_OUTPUT_CHARS: usize = 28000;

impl SqliteQuery {
    pub fn new(
        workspace: Arc<RwLock<PathBuf>>,
        trusted: Arc<RwLock<Vec<PathBuf>>>,
        is_main: bool,
    ) -> Self {
        Self {
            workspace,
            trusted,
            is_main,
        }
    }

    async fn run(&self, args: &Value) -> Result<String> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path' argument"))?;
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("missing 'query' argument"))?;

        // UX 层检查：只放行 SELECT / WITH 开头（真正的边界是 READ_ONLY 连接）
        let first_word = query
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if first_word != "select" && first_word != "with" {
            anyhow::bail!("only SELECT / WITH queries are allowed (read-only tool)");
        }

        let ws = self.workspace.read().await;
        let extra = if self.is_main {
            Some(ws.join("subagent"))
        } else {
            None
        };
        let trusted = self.trusted.read().await.clone();
        let resolved = path_guard::validate_path_in_scope(&ws, &trusted, path, extra.as_deref())?;
        drop(ws);
        if !resolved.is_file() {
            anyhow::bail!("not a file: {}", resolved.display());
        }

        // 查询在线程池执行（rusqlite 是同步 IO），阻塞不超过 busy_timeout
        let db_path = resolved.clone();
        let owned_query = query.to_string();
        let (rows_out, note) =
            tokio::task::spawn_blocking(move || query_sqlite(&db_path, &owned_query))
                .await
                .map_err(|e| anyhow!("query task failed: {}", e))??;

        if rows_out.is_empty() {
            return Ok(format!("(0 rows){note}"));
        }

        // 管道分隔输出：首行表头 + 数据行
        let mut out = String::new();
        out.push_str(&rows_out[0].join(" | "));
        out.push('\n');
        for row in &rows_out[1..] {
            if out.len() > SQLITE_MAX_OUTPUT_CHARS {
                out.push_str(&format!(
                    "(output truncated at {} chars; refine the query with LIMIT)\n",
                    SQLITE_MAX_OUTPUT_CHARS
                ));
                break;
            }
            out.push_str(&row.join(" | "));
            out.push('\n');
        }
        out.push_str(&format!("({} rows){}", rows_out.len() - 1, note));
        Ok(out)
    }
}

/// 同步执行查询：返回 (表头+数据行, 截断附注)。行内单元格超长截断，
/// blob 显示字节数，超过行数上限提前停。
fn query_sqlite(db_path: &std::path::Path, query: &str) -> Result<(Vec<Vec<String>>, String)> {
    let conn = rusqlite::Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| anyhow!("open {:?}: {}", db_path, e))?;
    conn.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|e| anyhow!("busy_timeout: {}", e))?;

    let mut stmt = conn.prepare(query).map_err(|e| anyhow!("prepare: {}", e))?;
    let col_count = stmt.column_count();
    let header: Vec<String> = (0..col_count)
        .filter_map(|i| stmt.column_name(i).ok().map(str::to_string))
        .collect();

    let mut rows_out = vec![header];
    let mut truncated_rows = false;
    let mut rows = stmt.query([]).map_err(|e| anyhow!("execute: {}", e))?;
    while let Some(row) = rows.next().map_err(|e| anyhow!("fetch: {}", e))? {
        if rows_out.len() > SQLITE_MAX_ROWS {
            truncated_rows = true;
            break;
        }
        let mut cells = Vec::with_capacity(col_count);
        for i in 0..col_count {
            let cell = match row.get_ref(i).map_err(|e| anyhow!("column {i}: {}", e))? {
                rusqlite::types::ValueRef::Null => "NULL".to_string(),
                rusqlite::types::ValueRef::Integer(v) => v.to_string(),
                rusqlite::types::ValueRef::Real(v) => v.to_string(),
                rusqlite::types::ValueRef::Text(t) => {
                    clip_chars(&String::from_utf8_lossy(t), SQLITE_MAX_CELL_CHARS)
                }
                rusqlite::types::ValueRef::Blob(b) => format!("<BLOB {} B>", b.len()),
            };
            cells.push(cell);
        }
        rows_out.push(cells);
    }
    let note = if truncated_rows {
        format!(" (truncated at {SQLITE_MAX_ROWS} rows)")
    } else {
        String::new()
    };
    Ok((rows_out, note))
}

fn clip_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let clipped: String = s.chars().take(max).collect();
        format!("{clipped}…")
    }
}

#[async_trait]
impl Tool for SqliteQuery {
    fn name(&self) -> &str {
        "sqlite_query"
    }
    fn description(&self) -> &str {
        "Run a read-only SELECT query against a local SQLite database file (.db). The connection is opened read-only — writes and DDL always fail. Use this to inspect sessions.db or other sqlite files instead of terminal sqlite3/python. Example: path=\"sessions.db\", query=\"SELECT role, substr(content,1,100) FROM messages ORDER BY rowid DESC LIMIT 20\"."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "SQLite database file path (relative paths resolve to the agent workspace)" },
                "query": { "type": "string", "description": "A single SELECT or WITH query" }
            },
            "required": ["path", "query"]
        })
    }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        self.run(args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_sqlite_query_select() {
        let ws = tempdir().unwrap();
        let db = ws.path().join("test.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE t (id INTEGER, name TEXT);
             INSERT INTO t VALUES (1, 'alpha'), (2, 'beta');",
        )
        .unwrap();

        let tool = SqliteQuery::new(
            Arc::new(RwLock::new(ws.path().to_path_buf())),
            Arc::new(RwLock::new(Vec::new())),
            true,
        );
        let out = tool
            .execute(
                &json!({"path": "test.db", "query": "SELECT id, name FROM t ORDER BY id"}),
                "cli",
            )
            .await
            .unwrap();
        assert!(out.contains("id | name"), "{out}");
        assert!(out.contains("1 | alpha"), "{out}");
        assert!(out.contains("2 | beta"), "{out}");
        assert!(out.contains("(2 rows)"), "{out}");
    }

    #[tokio::test]
    async fn test_sqlite_query_rejects_writes() {
        let ws = tempdir().unwrap();
        let db = ws.path().join("test.db");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE t (id INTEGER); INSERT INTO t VALUES (1);")
            .unwrap();
        let tool = SqliteQuery::new(
            Arc::new(RwLock::new(ws.path().to_path_buf())),
            Arc::new(RwLock::new(Vec::new())),
            true,
        );
        // UX 层：非 SELECT 前缀拒绝
        assert!(tool
            .execute(
                &json!({"path": "test.db", "query": "INSERT INTO t VALUES (2)"}),
                "cli"
            )
            .await
            .is_err());
        assert!(tool
            .execute(&json!({"path": "test.db", "query": "DELETE FROM t"}), "cli")
            .await
            .is_err());
        assert!(tool
            .execute(&json!({"path": "test.db", "query": "DROP TABLE t"}), "cli")
            .await
            .is_err());
        // 硬边界层：CTE 包装的写入同样被 READ_ONLY 连接拒绝
        let result = tool
            .execute(
                &json!({"path": "test.db", "query": "WITH x AS (SELECT 1) INSERT INTO t SELECT * FROM x"}),
                "cli",
            )
            .await;
        assert!(result.is_err(), "READ_ONLY 连接必须拒绝写入: {result:?}");
        // 数据未被修改
        let n: i64 = rusqlite::Connection::open(&db)
            .unwrap()
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn test_sqlite_query_out_of_scope_rejected() {
        let ws = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let db = outside.path().join("secret.db");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE t (x);")
            .unwrap();
        let tool = SqliteQuery::new(
            Arc::new(RwLock::new(ws.path().to_path_buf())),
            Arc::new(RwLock::new(Vec::new())),
            true,
        );
        let result = tool
            .execute(
                &json!({"path": db.display().to_string(), "query": "SELECT * FROM t"}),
                "cli",
            )
            .await;
        assert!(result.is_err(), "界外数据库必须拒绝");
    }

    #[tokio::test]
    async fn test_sqlite_query_row_and_cell_caps() {
        let ws = tempdir().unwrap();
        let db = ws.path().join("test.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        let mut batch = String::from("CREATE TABLE t (id INTEGER, big TEXT);");
        for i in 0..250 {
            batch.push_str(&format!(
                "INSERT INTO t VALUES ({i}, '{}');\n",
                "x".repeat(300)
            ));
        }
        conn.execute_batch(&batch).unwrap();
        let tool = SqliteQuery::new(
            Arc::new(RwLock::new(ws.path().to_path_buf())),
            Arc::new(RwLock::new(Vec::new())),
            true,
        );
        let out = tool
            .execute(
                &json!({"path": "test.db", "query": "SELECT * FROM t"}),
                "cli",
            )
            .await
            .unwrap();
        assert!(out.contains("truncated at 200 rows"), "{out}");
        assert!(out.contains('…'), "单元格超长应截断: {out}");
        assert!(
            !out.contains(&"x".repeat(300)),
            "不应输出完整 300 字符单元格"
        );
    }

    #[tokio::test]
    async fn test_sqlite_query_null_and_blob() {
        let ws = tempdir().unwrap();
        let db = ws.path().join("test.db");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE t (a, b); INSERT INTO t VALUES (NULL, x'0102');")
            .unwrap();
        let tool = SqliteQuery::new(
            Arc::new(RwLock::new(ws.path().to_path_buf())),
            Arc::new(RwLock::new(Vec::new())),
            true,
        );
        let out = tool
            .execute(
                &json!({"path": "test.db", "query": "SELECT a, b FROM t"}),
                "cli",
            )
            .await
            .unwrap();
        assert!(out.contains("NULL"), "{out}");
        assert!(out.contains("<BLOB 2 B>"), "{out}");
    }
}
