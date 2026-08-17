//! 规划后执行（ADR-0024）：每会话一份的 todo 清单。
//!
//! 设计要点：
//! - 单一 `todo` 工具（`action` = add/list/update/done），复用 P5-3 `search` 工具的
//!   "单工具 + action 分发" 模式，而非 ADR 草稿里列的 4 个独立工具名。
//! - 状态按 `session_uuid` 分桶，in-memory + 落盘 `workspace/todos/<uuid>.json`；
//!   跨会话天然隔离（不串味），`/new` 后新会话空清单、旧会话文件仍保留。
//! - `TodoStore` 挂在共享的 `ToolRegistry` 上：agent 每轮把"当前 session_uuid"
//!   写入 `current_session`，todo 工具执行时按它路由；同时 agent 把当前清单文本注入
//!   Runtime Context（每轮可见"还差哪几步"）。
//! - 不引入新依赖；落盘点无 `workspace` 时（测试/降级）自动禁用。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::Tool;

/// todo 条目状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

impl TodoStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Done => "done",
        }
    }
    /// 清单展示用的勾选标记。
    pub fn mark(&self) -> &'static str {
        match self {
            TodoStatus::Pending => "[ ]",
            TodoStatus::InProgress => "[~]",
            TodoStatus::Done => "[x]",
        }
    }
}

/// 单条 todo。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: usize,
    pub task: String,
    pub status: TodoStatus,
    pub created_at: String,
    pub updated_at: String,
}

struct Inner {
    /// 当前 turn 对应的 session_uuid（由 agent 每轮写入）。
    current_session: Option<String>,
    /// 各 session 的清单缓存（首次访问时从磁盘懒加载）。
    lists: HashMap<String, Vec<TodoItem>>,
    /// 已尝试加载过的 session（避免每轮重复 stat 磁盘）。
    loaded: HashSet<String>,
}

/// 跨工具/跨 agent 共享的 todo 状态存储。
pub struct TodoStore {
    inner: RwLock<Inner>,
    /// agent 家目录（固定）；为 None 时禁用落盘（测试/降级）。
    workspace: Option<PathBuf>,
}

impl TodoStore {
    /// 启用落盘：清单写入 `workspace/todos/<uuid>.json`。
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            inner: RwLock::new(Inner {
                current_session: None,
                lists: HashMap::new(),
                loaded: HashSet::new(),
            }),
            workspace: Some(workspace),
        }
    }

    /// 禁用落盘（测试 / 无 workspace 场景）。
    pub fn disabled() -> Self {
        Self {
            inner: RwLock::new(Inner {
                current_session: None,
                lists: HashMap::new(),
                loaded: HashSet::new(),
            }),
            workspace: None,
        }
    }

    /// agent 每轮调用：标记当前 session_uuid，后续 todo 操作都路由到它。
    pub fn set_current_session(&self, uuid: &str) {
        self.inner.write().unwrap().current_session = Some(uuid.to_string());
    }

    fn current_uuid(&self) -> Result<String> {
        self.inner
            .read()
            .unwrap()
            .current_session
            .clone()
            .ok_or_else(|| anyhow!("no active session for todo (agent never set current_session)"))
    }

    fn file_path(&self, uuid: &str) -> Option<PathBuf> {
        self.workspace
            .as_ref()
            .map(|w| w.join("todos").join(format!("{uuid}.json")))
    }

    /// 首次访问某 session 时从磁盘懒加载；workspace 为 None 时跳过。
    fn load_if_needed(&self, uuid: &str) {
        if self.workspace.is_none() {
            return;
        }
        {
            let g = self.inner.read().unwrap();
            if g.loaded.contains(uuid) {
                return;
            }
        }
        let list: Vec<TodoItem> = match self.file_path(uuid) {
            Some(p) if p.exists() => std::fs::read_to_string(&p)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let mut g = self.inner.write().unwrap();
        g.lists.insert(uuid.to_string(), list);
        g.loaded.insert(uuid.to_string());
    }

    /// 把当前 session 的清单写回磁盘（workspace 为 None 时静默跳过）。
    fn persist(&self, uuid: &str) {
        let (ws, list) = {
            let g = self.inner.read().unwrap();
            (self.workspace.clone(), g.lists.get(uuid).cloned())
        };
        if let (Some(ws), Some(list)) = (ws, list) {
            let path = ws.join("todos").join(format!("{uuid}.json"));
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(s) = serde_json::to_string_pretty(&list) {
                let _ = std::fs::write(&path, s);
            }
        }
    }

    /// 新增一条待办，返回其 id。
    pub fn add(&self, task: &str) -> Result<usize> {
        let uuid = self.current_uuid()?;
        self.load_if_needed(&uuid);
        let mut g = self.inner.write().unwrap();
        let list = g.lists.entry(uuid.clone()).or_default();
        let id = list.iter().map(|t| t.id).max().map(|m| m + 1).unwrap_or(1);
        let now = chrono::Utc::now().to_rfc3339();
        list.push(TodoItem {
            id,
            task: task.to_string(),
            status: TodoStatus::Pending,
            created_at: now.clone(),
            updated_at: now,
        });
        drop(g);
        self.persist(&uuid);
        Ok(id)
    }

    /// 当前 session 的清单（按 id 升序）。
    pub fn list(&self) -> Result<Vec<TodoItem>> {
        let uuid = self.current_uuid()?;
        self.load_if_needed(&uuid);
        Ok(self
            .inner
            .read()
            .unwrap()
            .lists
            .get(&uuid)
            .cloned()
            .unwrap_or_default())
    }

    /// 更新某条状态。
    pub fn update(&self, id: usize, status: TodoStatus) -> Result<()> {
        let uuid = self.current_uuid()?;
        self.load_if_needed(&uuid);
        let mut g = self.inner.write().unwrap();
        let list = g.lists.entry(uuid.clone()).or_default();
        match list.iter_mut().find(|t| t.id == id) {
            Some(item) => {
                item.status = status;
                item.updated_at = chrono::Utc::now().to_rfc3339();
            }
            None => return Err(anyhow!("todo #{} not found", id)),
        }
        drop(g);
        self.persist(&uuid);
        Ok(())
    }

    /// 标记完成（等价于 update(id, Done)）。
    pub fn done(&self, id: usize) -> Result<()> {
        self.update(id, TodoStatus::Done)
    }

    /// 当前清单的展示文本（供 Runtime Context 注入）。无 session 或空清单返回空串。
    pub fn current_list_text(&self) -> String {
        let uuid = {
            let g = self.inner.read().unwrap();
            match &g.current_session {
                Some(u) => u.clone(),
                None => return String::new(),
            }
        };
        self.load_if_needed(&uuid);
        let list = self
            .inner
            .read()
            .unwrap()
            .lists
            .get(&uuid)
            .cloned()
            .unwrap_or_default();
        if list.is_empty() {
            return String::new();
        }
        let mut out = String::from("[Current todo list]\n");
        for item in &list {
            out.push_str(&format!(
                "{} #{} {}\n",
                item.status.mark(),
                item.id,
                item.task
            ));
        }
        out
    }
}

/// 统一 `todo` 工具：agent 自管当前会话的子步骤清单。
pub struct TodoTool {
    store: Arc<TodoStore>,
}

impl TodoTool {
    pub fn new(store: Arc<TodoStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &str {
        "todo"
    }
    fn description(&self) -> &str {
        "Manage a per-session task checklist for the current conversation. For any non-trivial task, first break it into steps (action=\"add\", one call per step), then track progress with action=\"update\" (status: pending|in_progress|done) or action=\"done\". Use action=\"list\" to review. The checklist is re-injected into the prompt each turn so you always know what remains — keep it lightweight and add items as you discover sub-steps."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "list", "update", "done"],
                    "description": "add: create a todo (needs `task`). list: show current checklist. update: change status (needs `id`+`status`). done: mark completed (needs `id`)."
                },
                "task": { "type": "string", "description": "The step text (action=add)." },
                "id": { "type": "integer", "description": "Todo item id (action=update/done)." },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "done"],
                    "description": "New status (action=update)."
                }
            },
            "required": ["action"]
        })
    }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'action'"))?;
        match action {
            "add" => {
                let task = args
                    .get("task")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("missing 'task' for action=add"))?;
                let id = self.store.add(task)?;
                Ok(format!("Added todo #{}: {}", id, task))
            }
            "list" => {
                let items = self.store.list()?;
                if items.is_empty() {
                    return Ok("(no todos)".to_string());
                }
                let mut out = String::new();
                for it in &items {
                    out.push_str(&format!("{} #{} {}\n", it.status.mark(), it.id, it.task));
                }
                Ok(out)
            }
            "update" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| anyhow!("missing 'id' for action=update"))?
                    as usize;
                let status = args
                    .get("status")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("missing 'status' for action=update"))?;
                let status = match status {
                    "pending" => TodoStatus::Pending,
                    "in_progress" => TodoStatus::InProgress,
                    "done" => TodoStatus::Done,
                    other => return Err(anyhow!("invalid status '{}'", other)),
                };
                self.store.update(id, status)?;
                Ok(format!("Updated todo #{} -> {}", id, status.as_str()))
            }
            "done" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| anyhow!("missing 'id' for action=done"))?
                    as usize;
                self.store.done(id)?;
                Ok(format!("Completed todo #{}", id))
            }
            other => Err(anyhow!("unknown action '{}'", other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> TodoStore {
        TodoStore::disabled()
    }

    #[test]
    fn add_returns_sequential_ids() {
        let s = store();
        s.set_current_session("sess1");
        assert_eq!(s.add("step one").unwrap(), 1);
        assert_eq!(s.add("step two").unwrap(), 2);
        let items = s.list().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].task, "step one");
        assert_eq!(items[0].status, TodoStatus::Pending);
    }

    #[test]
    fn done_marks_completed() {
        let s = store();
        s.set_current_session("sess1");
        let id = s.add("write code").unwrap();
        s.done(id).unwrap();
        let items = s.list().unwrap();
        assert_eq!(items[0].status, TodoStatus::Done);
    }

    #[test]
    fn update_to_in_progress() {
        let s = store();
        s.set_current_session("sess1");
        let id = s.add("research").unwrap();
        s.update(id, TodoStatus::InProgress).unwrap();
        let items = s.list().unwrap();
        assert_eq!(items[0].status, TodoStatus::InProgress);
    }

    #[test]
    fn list_scoped_per_session() {
        let s = store();
        s.set_current_session("a");
        s.add("only in a").unwrap();
        s.set_current_session("b");
        s.add("only in b").unwrap();
        s.set_current_session("a");
        let items = s.list().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].task, "only in a");
    }

    #[test]
    fn missing_session_errors() {
        let s = store();
        assert!(s.add("x").is_err());
        assert!(s.list().is_err());
    }

    #[test]
    fn current_list_text_empty_when_no_session() {
        let s = store();
        assert_eq!(s.current_list_text(), "");
    }

    #[test]
    fn current_list_text_renders_marks() {
        let s = store();
        s.set_current_session("sess1");
        let id = s.add("task a").unwrap();
        s.done(id).unwrap();
        s.add("task b").unwrap();
        let txt = s.current_list_text();
        assert!(txt.contains("[x] #1 task a"));
        assert!(txt.contains("[ ] #2 task b"));
    }

    #[tokio::test]
    async fn tool_add_and_list_roundtrip() {
        let s = Arc::new(store());
        s.set_current_session("sess1");
        let tool = TodoTool::new(s);
        let out = tool
            .execute(
                &serde_json::json!({ "action": "add", "task": "hello" }),
                "cli",
            )
            .await
            .unwrap();
        assert!(out.contains("Added todo #1"));
        let out = tool
            .execute(&serde_json::json!({ "action": "list" }), "cli")
            .await
            .unwrap();
        assert!(out.contains("[ ] #1 hello"));
        // done via tool
        let out = tool
            .execute(&serde_json::json!({ "action": "done", "id": 1 }), "cli")
            .await
            .unwrap();
        assert!(out.contains("Completed todo #1"));
    }

    #[tokio::test]
    async fn tool_rejects_unknown_action() {
        let s = Arc::new(store());
        s.set_current_session("sess1");
        let tool = TodoTool::new(s);
        let err = tool
            .execute(&serde_json::json!({ "action": "frobnicate" }), "cli")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown action"));
    }
}
