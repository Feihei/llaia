# ADR-0024: 规划后执行（planning todo）

- 状态：提议（P5 实施）
- 日期：2026-08-14
- 关联：P5 任务编排与交互

## 背景

复杂任务下 agent 容易"想到哪做到哪"，缺一个可检视、可勾选的推进清单。用户提出"复杂任务先列 todo 再执行"，并提到有 todoist skill，问"怎么更好"。

候选实现：
- **① 内置轻量 `todo` 工具**：agent 自管 in-memory + 落盘 `todos.json`，WebUI 可看。
- **② 外部任务管理 skill / task-MCP（todoist）**：把任务追踪外包给专业工具。

## 决策

### 1. 内置，不外包（否决 todoist/MCP 方案）

子步骤跟踪本质是 **agent 的工作记忆**，而非人类的项目管理：
- 它应紧耦合当前任务/session，廉价写入、随上下文注入；
- 外部工具（todoist）schema 面向"人类待办"，与"agent 子步骤"语义错位，且引入账号/网络/延迟；
- 内置实现零依赖、全可控、与 WebUI 直接打通。

故采用方案 ①。

### 2. 工具接口

```
todo_add(task: String) -> id            # 新增一条待办
todo_list() -> Vec<TodoItem>            # 列出当前清单
todo_update(id, status)                 # 标记 in_progress / pending
todo_done(id)                           # 标记完成
```

状态：`TodoItem { id, task, status: Pending|InProgress|Done, created_at, updated_at }`。

### 3. 持久化（每会话一份）

**每会话一份**：in-memory + 落盘 `workspace/todos/<session_uuid>.json`。`/new` 或切会话清空当前清单；首版不做跨会话持久（如需，后续加 `persistent` 开关）。**不**使用全局 `kv` 表（`kv` 无 session 隔离，会串味）。

### 4. 运行时注入

todo 列表每轮注入 Runtime Context（类似 goal_state），让模型始终知道"还差哪几步"；压缩后存活（属工作记忆，按关键消息保留）。

### 5. prompt 约定（核心价值点）

系统提示要求 agent：**非平凡任务先拆 checklist（调 `todo_add` 列清单），再逐步 `todo_update`/`todo_done` 推进**。这把"规划能力"做成默认行为，而非依赖外部 skill。

### 6. plan mode（可选，首版不做）

进阶形态：列完清单先等用户 `/ok` 确认再执行（复用 ADR-0020 审批续跑）。首版不启用，避免增加交互摩擦；后续按需加。

### 7. WebUI

展示当前 todo 清单 + 勾选状态；首版只读展示，后续可点击勾选回传 `todo_done`。

## 后果

- 一个小工具 + 一条 prompt 约定，实现成本低（★☆☆~★★☆），直接提升复杂任务执行可靠性。
- 与 `goal`（长期目标）、`cron`（定时）、`delegate`（子任务）正交互补：todo 是"当前任务的步骤清单"。
- 不引入新依赖。

## 实现补记（P5-4 交付时）

- **工具形态采用单一 `todo` 工具 + `action` 分发**（add/list/update/done），而非 §2 草稿里列的 4 个独立工具名（`todo_add`/`todo_list`/`todo_update`/`todo_done`）。理由：复用 P5-3 `search` 工具的"单工具 + action"模式，与本项目 `cron` 工具约定一致，减少 prompt 内工具条目数。
- **共享状态挂载点**：`TodoStore` 挂在共享的 `ToolRegistry` 上（`todo_store` 字段）。agent 每轮在 `handle_message_streaming` 起点把当前 `session_uuid` 写入 `current_session`，todo 工具据此路由；同时把当前清单文本写入 `Context.todo_state`，在 `to_messages` 尾部（Runtime Context 区，与 status_bar 同区）注入，每轮可见"还差哪几步"。未挂真实 workspace（`ToolRegistry::new()` 默认）时为禁用态（测试/降级用）。
- **持久化**：每会话一份，落盘 `workspace/todos/<session_uuid>.json`，首次访问懒加载；`/new` 后新会话天然空清单、旧会话文件保留。
- **WebUI**：`GET /api/todos` 只读返回当前清单；聊天页底部加了只读 todo 面板（5s 轮询）。点击勾选回传（plan mode / 可交互）留待后续。
