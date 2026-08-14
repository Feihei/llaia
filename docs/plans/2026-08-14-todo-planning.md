# P5: 规划后执行（todo） 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: 使用 superpowers:executing-plans 按任务顺序实现。步骤用 checkbox (`- [ ]`) 标记追踪。

**Goal:** 提供内置轻量 `todo` 工具，让 agent 对复杂任务先列步骤清单、再逐步勾选推进；清单每轮注入 Runtime Context，WebUI 可看。`plan mode`（列完先确认再执行）首版不做。

**Architecture:** `todo` 工具维护 session 级清单（in-memory + 落盘 `todos.json`），提供 `todo_add` / `todo_list` / `todo_update` / `todo_done`。清单经 Runtime Context 注入每轮；系统提示要求 agent 对非平凡任务先规划。

**Tech Stack:** Rust + serde + tokio::fs（落盘）

**参考设计:** [ADR-0024](../adr/0024-planning-todo.md)

---

## 文件结构

**新建：**
- `src/tools/todo.rs` — `TodoTool` + `TodoStore`（清单结构与读写）
- `tests/todo_store.rs` — 清单增删改 + 落盘单测

**修改：**
- `src/channels/cli.rs` — 注册 `TodoTool`
- `src/agent/mod.rs` — 每轮注入 todo 清单到 Runtime Context
- `src/agent/mod.rs` — 系统提示加"非平凡任务先规划"约定
- `src/agent/mod.rs` 压缩逻辑 — todo 清单压缩后存活
- `src/web/static/app.js` / `index.html` — todo 面板（首版只读）

---

## Task 1: TodoStore 结构与落盘

**Files:** Create `src/tools/todo.rs`, Create `tests/todo_store.rs`

- [ ] 结构：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus { Pending, InProgress, Done }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub task: String,
    pub status: TodoStatus,
    pub created_at: i64,
    pub updated_at: i64,
}

pub struct TodoStore {
    items: Vec<TodoItem>,
    path: PathBuf, // workspace/todos.json
}
```

- [ ] 方法：`add(task)` / `list()` / `update(id, status)` / `done(id)`；每次变更后 `persist()` 写 `todos.json`（若 path 不可写则仅内存，记 warn）。
- [ ] 单测：add 后 list 含该条；done 后 status=Done；`/new` 调用 `clear()` 后为空。

---

## Task 2: todo 工具实现

**Files:** Create `src/tools/todo.rs`, Modify `src/channels/cli.rs`

- [ ] 实现 `TodoTool`（单一工具，用 `action` 字段分发，或按 ADR 拆 4 个工具名；建议单工具 + `action` 以精简工具列表）：

```rust
pub struct TodoTool { store: Arc<Mutex<TodoStore>> }

// args: {"action":"add"|"list"|"update"|"done", "task?"?, "id?"?, "status"?}
```

- [ ] 注册进主 Agent 工具集（默认注册，无副作用/只读为主，`add`/`done` 有写但不危险，可不走审批或按 `requires_confirm()=false`）。
- [ ] 单测：工具调用 `add`→`list`→`done` 链路正确。

---

## Task 3: 运行时注入 Runtime Context

**Files:** Modify `src/agent/mod.rs`

- [ ] 每轮 turn 前，若 todo 清单非空，把清单渲染进 Runtime Context（仿 goal）：

```
Todo list:
[ ] 1. 抓取需求文档
[x] 2. 设计 schema
[ ] 3. 写迁移脚本
```

- [ ] 压缩时 todo 清单按"关键消息"保留（不被摘要丢弃）。

---

## Task 4: prompt 约定

**Files:** Modify `src/agent/mod.rs` 系统提示拼接

- [ ] 在系统提示加约定（中文，面向 agent）："面对多步骤的复杂任务，先调用 `todo_add` 列出步骤清单，再逐步 `todo_update`/`todo_done` 推进；单步可完成的简单任务无需清单。"

---

## Task 5: WebUI 展示

**Files:** Modify `src/web/static/app.js`, `index.html`

- [ ] 会话面板侧栏/底部显示当前 todo 清单（[ ]/[x] 复选样式），首版只读；后续可点击勾选回传 `todo_done`。

---

## Task 6: 集成验证 + plan 状态

- [ ] `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test`
- [ ] 手动：`llaia chat` → "帮我部署项目" 观察 agent 先列 todo 再逐步执行；`/new` 后清单清空。
- [ ] 更新 `docs/plan.md` 本条目状态。

---

## 自查

- 内置工具（非 todoist/MCP）✅；session 级落盘 `todos.json` ✅；Runtime Context 注入 ✅
- prompt 约定驱动"先规划"✅；plan mode 明确首版不做 ✅
- 类型一致性：TodoItem/TodoStatus/TodoStore 在 todo.rs + tests + mod 一致 ✅
