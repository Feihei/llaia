# 会话单条 / 区间消息删除

日期：2026-09-02
状态：Approved（已与用户确认）

## 目标

在 WebUI 会话详情页支持删除**单条**历史消息或**两点之间**的一段连续消息，用于清理无限循环等堆积的长消息，而非只能删除整个会话。仅清理历史存储与显示，**不改动**会话的 token 预算 / live context。

## 非目标

- 不重新计算或扣减 `sessions.token_count` / Agent 内存上下文。
- 不实现"删除到会话末尾"（用户选定为"两点之间"）。

## 后端

### sqlite 层（`src/memory/sqlite.rs`）

新增 `pub fn delete_messages(&self, session_id: i64, from_id: i64, to_id: i64) -> Result<u64>`：

```sql
DELETE FROM messages WHERE session_id = ?1 AND id BETWEEN ?2 AND ?3
```

- 返回受影响行数。
- `tool_calls` 由 `messages` 外键 `ON DELETE CASCADE` 级联删除；FTS 由 `trg_message_fts_delete` 触发器清理，无需手写。

### API 层（`src/web/mod.rs`）

新路由 `DELETE /api/sessions/:uuid/messages`，body 二选一：

- `{ "id": 12 }` → 单条（from=to=12）
- `{ "from_id": 12, "to_id": 40 }` → 区间

校验：
- session 不存在 → 404。
- `id`/区间消息不在该 session 内，或参数非法（`from_id > to_id`）→ 400。
- 复用现有鉴权中间件逻辑（`authorize`）。
- 成功 → `{ "deleted": N }`（`deleted` 为实际删除条数）。

## 前端

### `src/web/static/app.js`

新增状态：
- `delMode: false` —— 删除模式开关。
- `delPick: []` —— 已选消息 id 数组（最多 2 条，按 id 判定选中）。

新增方法：
- `toggleDelMode()` —— 反转 `delMode`，并清空 `delPick`。
- `pickMessage(m)` —— 删除模式下：若已选则取消；否则加入，最多保留 2 条；达到 2 条即调用 `confirmDeleteMessages()`。
- `confirmDeleteMessages()` —— 单条传 `{ id }`，两条传 `{ from_id: min, to_id: max }`（取 id 较小为 from）；`confirm` 文案沿用"不影响 live context"；成功后 `delMode=false`、`delPick=[]`、`await this.openSession(this.selectedSession)`（复用现有渲染）。

### `src/web/static/index.html`

- 头部（SESSION 行）加 `Toggle delete` 按钮，绑定 `toggleDelMode`。
- 每条消息 `div.msg`：`delMode` 下绑定 `@click="pickMessage(m)"`，选中项（`delPick.includes(m.id)`）追加高亮 class。
- 消息区顶部（删除模式激活时）显示提示："点 1 条=单条，点 2 条=删除两点之间的一段"。

选中按 `id` 而非下标判定，删除后重新拉取详情完成重渲染，避免下标错位。

## 测试

- sqlite 单测：构造 session + messages + tool_calls，验证 `delete_messages` 区间删除、级联清 tool_calls、FTS 命中数变化。沿用现有 `test_*` 风格。
- `cargo build` 通过。