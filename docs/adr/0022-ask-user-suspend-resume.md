# ADR-0022: ask_user 阻塞式澄清（suspend/resume 复用审批）

- 状态：提议（P5 实施）
- 日期：2026-08-14
- 关联：P5 任务编排与交互；ADR-0020（权限档位与交互式审批）

## 背景

Agent 执行复杂任务时常需向用户澄清（选方案、补参数、确认范围），目前只能"一次问完"或"猜着做"，容易跑偏。`ask_user` 是要让 agent 在执行中**主动抛一个问题、阻塞等待回答、再继续**。

llaia 已有 ADR-0020 的 `ApprovalGate` 挂起-回传机制（`/ok` `/deny` 审批：注册 pending → 本轮暂停 → 用户回复 → continuation turn 续跑）。`ask_user` 的"挂起等待用户"语义与审批高度同构，应复用而非另造。

## 决策

### 1. 复用 ApprovalGate 的软暂停骨架（架构适配 zeroclaw UX）

`ask_user` 执行时注册 `PendingQuestion { id, question, channel, turn_id, choices?, timeout_secs? }`，本轮 turn **软暂停**（返回占位结果 `⏳ 等待用户回答`），不继续调用模型；用户回答后触发 continuation turn，把答案写回上下文。这与 zeroclaw 原生 `ask_user` 的"send + listen 阻塞等下一条消息"达到相同 UX，但适配 llaia 的异步 turn 制（不在工具 `execute` 内阻塞）：**单 pending 时，用户的下一条普通消息即答案**；多 pending 时由显式 `/answer <id>` 消歧。

### 2. 与审批的语义/UI 区分

| 维度 | 审批（`/ok` `/deny`） | ask_user |
|---|---|---|
| 对象 | 待执行的**操作**（有副作用） | 对**问题**的回答 |
| 返回值 | 操作真实结果 / 已拒绝 | 用户自由文本答案 |
| UI | "是否允许此操作" | "请回答这个问题" |

二者共用 pending 注册表与续跑机制，但 `PendingKind` 分 `Approval` / `Question` 两型，路由与展示区分。

### 3. 频道路由（统一软暂停，含 CLI）

所有频道走同一套"软暂停 + continuation turn"路径（CLI 也不例外，不单独做 execute 内阻塞）：
- **交互频道**（cli / qq / telegram / dingtalk / wechat / web，**新增 feishu**）：`ask_user` 把问题推送给用户并注册 pending；单 pending 时用户下一条普通消息即答案，多 pending 时需 `/answer <id>`。复用 ADR-0020 的续跑路径（`SlashOutcome::Resume`）。
- **非交互频道**（mail 等）：`ask_user` 自动以"无法询问用户，按最合理假设继续并说明"返回（参考审批在 cron 下自动拒绝）。
- **feishu 修复**：feishu 当前不在 `is_interactive_channel` 白名单（历史遗漏），本期一并加入，使 ask_user / 审批在其上正常工作。

### 4. 超时 / 放弃 / 排队 / 多问题

- 超时：配置 `ask_user.timeout_secs`，**默认 300**（对齐 zeroclaw）；超时返回"用户未在规定时间内回答，已按最合理假设继续"。
- 放弃：用户可 `/cancel <id>`（或回复"跳过"），工具返回"用户放弃回答"。
- 多问题：**严格一次一个、多 turn 串行**——同一 turn 内第二个 `ask_user` 要等第一个的答案续跑后才执行，不并行阻塞；`/answer <id>` 用于多 pending 时消歧。

### 5. 结构化单选（可选 `choices` 参数）

`ask_user(question, choices?)` 支持可选 `choices: Vec<String>`：有 `choices` 时走"结构化单选"（对齐 zeroclaw 的 `request_choice`），WebUI / IM 渲染为选项卡片/按钮，用户点选或回复序号即答案；无 `choices` 时为自由文本。结构化频道（WS 审批/ACP 类）一律走此路径，不回退到自由文本监听。

## 后果

- 需在 runner/agent loop 支持"工具返回值是异步等待的用户输入"——但 ADR-0020 已验证 suspend/resume 可行，本 ADR 是同一机制的语义扩展，风险可控。
- 新增：`PendingQuestion` 结构、slash `/answer` 与 `/cancel`、CLI stdin 直问分支、WebUI 问题卡片。
- 属结构性改动（agent 循环 + pending 注册表扩展），故立此 ADR。

## 实现补记（P5-5 交付）

- **复用 ApprovalGate**：`PendingKind::{Approval, Question}` 两型共用注册表与续跑机制。审批走 `/ok` `/deny`，提问走 `单 pending 时用户下一条普通消息即答案` 或 `/answer <id> <text>` 显式消歧，`/cancel <id>` 取消任一 pending。
- **runner 拦截**：`execute_tool_calls` 在审批判定前按工具名 `ask_user` 拦截——交互频道注册 pending question + 占位结果 + `deferred`（turn 软暂停）；非交互频道（mail/cron）直接返回"按最合理假设继续"。
- **续答集中点**：`Agent::handle_input_streaming` 检测单 pending question，把下一条普通消息包装为答案跑 continuation turn，所有频道自动受益（无需逐频道改）。同 SLASH 层先过 `try_handle`，故 `/answer` `/cancel` 不会被误判为答案。
- **feishu** 已补入 `is_interactive_channel` 白名单。
- **WebUI**：`GET /api/questions` 只读展示 pending，聊天页底部只读面板（5s 轮询）。
- **已知限制**：纯静默超时自动续跑（用户始终不回复）未做后台定时巡检；超时字段与判定函数已就绪，且仅在"用户下一条消息到达"时一并判定（超时则丢弃 pending 并注入超时说明，走普通流程）。后续可加轻量后台巡检实现全自动续跑。
