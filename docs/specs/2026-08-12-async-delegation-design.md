# 异步委派设计（Async Delegation）

**状态**：spec 已出，待实现（P2-a 后续优化）
**日期**：2026-08-12
**关联**：[ADR-0002](adr/0002-agent-architecture.md)（委派模式）、P2-a 子 Agent 委派

## 1. 动机

当前 `delegate` 为同步委派：`execute_with_events` 用 `tokio::time::timeout` 包裹子 agent 的
`handle_input_streaming`（`src/tools/delegate.rs`），主 agent 这一轮被阻塞到子 agent 完成。
虽有 P2-d 的 Chunk 流式进度，但长任务期间用户无法发新消息、无法并行推进别的任务。

异步化的价值：让主 agent 在后台跑长任务（如 coder 重构、长研究）时，用户能继续对话或
发起别的委派。此前评估误判"需先建事件/通知子系统、成本:高"——实际 LLAIA 已有
`ProactivePusher`（cron 同款）可承载"后台任务完成推回结果"，无需新造子系统。

## 2. 设计决策（已与 Feihei 确认）

- **工具形态**：单一 `delegate` 工具新增 `async: bool` 参数，**默认 `false`** = 当前同步行为，零回归；模型按需设 `true`。
- **结果回传**：**仅最终结果**——后台任务结束后推一条消息，不流式刷屏。
- **取消能力**：v1 含 `/delegate-list` + `/delegate-cancel <id>`（`JoinHandle::abort()`）。
- **并发上限**：硬编码每会话 **3** 个并发后台委派，不新增 config key。
- **结果归属**：推回消息带前缀 `[子Agent {name} 完成]`，区分主/子 agent 产出。

## 3. 架构

### 3.1 注册表 `BackgroundTaskRegistry`

- 位置：`AgentRegistry` 内新增 `background_tasks: Arc<Mutex<HashMap<String, BackgroundTask>>>`
  （CLI 与 serve 共用，因二者都有 `AgentRegistry`）。
- `BackgroundTask { id: String, agent_name: String, started: Instant, handle: JoinHandle<()> }`。
- spawn 前若 `len() >= 3` → 工具返回错误"后台委派已达上限(3)"。
- `/delegate-list`：列出 id / 子 agent 名 / 已运行时长。
- `/delegate-cancel <id>`：从表取出 `abort()`；不在表中提示。

### 3.2 投递目标 `DeliveryTarget`

- 后台任务需把最终结果推回"发起这次委派的 channel"。
- 扩展工具执行上下文，携带 `delivery: DeliveryTarget`（spawn 时克隆进后台任务）：
  - serve channel：`DeliveryTarget::Pusher(Box<dyn ProactivePusher>)` —— 取该 channel 的 `pusher()`（复用 cron 的 `ProactivePusher` 机制）。
  - CLI：`DeliveryTarget::Stdout` —— 直接打印（无 pusher）。
- **`Channel` trait 新增 `fn pusher(&self) -> Option<Box<dyn ProactivePusher>>`**：
  Web/QQ/Telegram/飞书/邮箱返回各自 pusher；CLI 返回 `None`。

### 3.3 执行流程（`async=true`）

1. 主 agent 调 `delegate`，`async: true`。
2. 工具从 registry 取子 agent 定义，构建子 agent（同现有逻辑）。
3. 校验并发上限；登记 `BackgroundTask`（uuid）。
4. `tokio::spawn` 后台任务：
   - 跑子 agent `handle_input_streaming`（持自身 agent 快照，不受主 agent 阻塞）。
   - 完成后：成功 → `delivery.push("[子Agent {name} 完成]\n{result}")`；失败/超时 → `delivery.push("[子Agent {name} 失败] {err}")`。
   - 无论成败，从 registry 移除该 task。
5. 工具立即返回主 agent 一句："已后台启动子 Agent {name}（任务 {id}），完成会主动通知你。可用 /delegate-list 查看、/delegate-cancel 取消。"
6. 主 agent 本轮结束，用户可继续对话。

### 3.4 取消

- 强杀（`JoinHandle::abort()`）v1 足够；子 agent 可能停在工具调用中途，本地单用户可接受。
- 后续可演进为协作取消（传 `Arc<Notify>`，子 agent 循环检查）。

## 4. 边界与兼容性

- **零回归**：`async` 默认 `false`，同步路径完全不变（含现有单测）。
- **热重载**：后台任务持有自身 agent 快照，`reload_all` 重建 agents 不影响进行中的后台任务（snapshot 语义，同 in-flight turn）。
- **错误文案**：子 agent 报错/超时，推回错误消息而非静默丢失。
- **id 命名**：uuid 作 id，避免与现有命令参数混淆。

## 5. 改动文件（预估 ~200–300 行，难度 ★★☆）

- `src/tools/delegate.rs`：`DelegateTool` 加 `async` 参数 + 异步分支 + 注册表交互 + 返回 ack。
- `src/channels/mod.rs`：`Channel` trait 加 `pusher()`（默认 `None`，各 channel 重写）。
- `src/agent/registry.rs`：`AgentRegistry` 加 `background_tasks` 注册表。
- `src/commands/slash.rs`：加 `/delegate-list` + `/delegate-cancel`。
- 工具执行上下文：携带 `DeliveryTarget`（各 channel 在 `run_turn` 时注入自身 pusher / CLI 注入 Stdout）。
- `docs/plan.md` P2-a：标注 spec 已出、待实现。

## 6. 验证

- 同步 `delegate`（默认）行为不变（现有测试通过）。
- 异步：发起长任务后立即发新消息，主 agent 正常响应；后台完成后收到带前缀的结果消息。
- `/delegate-list` 显示运行中的任务；`/delegate-cancel <id>` 后任务终止、列表清空。
- 并发第 4 个委派被拒（"已达上限(3)"）。
- CLI 异步委派：结果打到 stdout。
- `cargo fmt --all && cargo build`（0 warning）&& `cargo test` 全绿。
