# LAIA 项目 Roadmap

> 本文档是 LAIA 的整体阶段路线图，标注各阶段状态与关键交付物。
> 每个阶段的详细实现计划见 [docs/plans/](plans/)，设计规格见 [docs/specs/](specs/)，架构决策见 [docs/adr/](adr/)。

**整体目标**：一个单用户、本地优先的私人 AI 助理，跨 CLI/QQ/Web 等多 channel 接入，主 Agent + 可委派子 Agent 协作，持久化记忆与会话。

---

## 状态图例

- ✅ 已完成
- 🚧 进行中
- ⏳ 计划中（未开始）

---

## P1 — MVP（CLI 单 channel）

**状态**：✅ 已完成

**目标**：能 `cargo run -- chat` 进 REPL 多轮对话，调本地 Ollama/LMStudio，用基础工具，自动压缩上下文，SOUL/USER/MEMORY 持久化。

**关键交付物**：

- [x] 项目骨架（cargo init + 依赖 + tracing 日志）
- [x] TOML 配置加载（`[provider.<id>]` / `[agent.<alias>]` / `[channels.cli]`）
- [x] Provider 抽象 + OpenAI 兼容实现（覆盖 Ollama / LMStudio / llama.cpp）
- [x] 工具调用协议：原生优先 + 标签降级（`<tool_call>` 标签解析）
- [x] 工具集：`file_read` / `file_write` / `file_edit` / `terminal` / `web_fetch` / `tavily_search` / `memory_write`
- [x] 持久化：SOUL.md / USER.md / MEMORY.md + sqlite 会话历史
- [x] 上下文管理：token 估算 + 自动压缩（LLM 摘要 + 关键消息保留）
- [x] Agent 主循环（工具调用迭代）
- [x] CLI REPL + 斜杠命令（`/new` `/compact` `/remember` `/config` `/help` `/exit`）
- [x] `laia config` / `laia doctor` / `laia remember` 子命令

**参考**：[ADR-0001](adr/0001-product-positioning.md) 到 [ADR-0008](adr/0008-config-schema-v1.1.md)

---

## P1.5 — QQ Channel + 流式输出

**状态**：✅ 已完成

**目标**：接入腾讯官方 QQ 开放平台机器人，实现跨 channel 会话接续；所有 channel 改造为流式输出。

### P1.5-a：QQ Channel 接入

- [x] Channel trait 抽象（`run(self: Arc<Self>, agent: Arc<Mutex<Agent>>)`）
- [x] QqConfig + 配置扩展（`app_id` + `app_secret` + `confirm_mode`）
- [x] `Tool::requires_confirm()` 副作用标记
- [x] QQ confirm 策略（`always` / `whitelist` / `none`）
- [x] 长回复分片（`split_reply` 纯函数，按段落/行/字符三级切分）
- [x] QqChannel 实现（WebSocket 接收 C2C 消息 + HTTPS API 发送回复）
- [x] 多 channel 启动（`tokio::spawn` 多 task）
- [x] 跨 channel 会话共享（同一 SessionStore）

**详细计划**：[plans/2026-07-21-qq-channel.md](plans/2026-07-21-qq-channel.md)
**设计规格**：[specs/2026-07-21-qq-channel-design.md](specs/2026-07-21-qq-channel-design.md)
**参考 ADR**：[ADR-0009](adr/0009-qq-channel.md)

### P1.5-b：流式输出

- [x] 三层流式管道：Provider `chat_stream` → Agent mpsc `TurnEvent` → Channel 消费
- [x] `StreamEvent` 枚举（`TextDelta` / `ToolCall` / `Done` / `Error`）
- [x] `TurnEvent` 枚举（`Chunk` / `ToolStart` / `ToolResult` / `Done` / `Error`）
- [x] `ToolCallStreamParser` 状态机（标签降级模式下流式过滤 `<tool_call>` 标签）
- [x] Provider SSE 解析（OpenAI 兼容流式响应）
- [x] `Agent::handle_input_streaming` + `chat()` / `handle_input()` 向后兼容
- [x] CliChannel 打字机效果
- [x] QqChannel 流式 provider + 累积后分片发送（行为不变）

**详细计划**：[plans/2026-07-22-streaming.md](plans/2026-07-22-streaming.md)
**设计规格**：[specs/2026-07-22-streaming-design.md](specs/2026-07-22-streaming-design.md)
**参考 ADR**：[ADR-0010](adr/0010-streaming-output.md)

### P1.5 稳定性修复（上线后补丁）

- [x] CLI / QQ channel 死锁修复（MutexGuard 提前释放 / Agent lock 先释放再消费 event channel）
- [x] QQ channel 空闲断连自动重连（外层 `run` 包无限重连循环）
- [x] QQ `/new` 等斜杠命令跨 channel 复用（`SlashOutcome::Handled(String)` 返回输出文本）
- [x] QQ token 过期自动恢复（`invalidate_token` + 重试，处理错误码 11244）
- [x] `context_size` 自动探测（llama.cpp `/props` + Ollama `/api/show`，取 `min(配置值, 探测值)`）

---

## P2 — 子 Agent 委派 + 交互增强 + Web Channel

**状态**：⏳ 计划中（P2-a 进行中）

**目标**：引入主从 Agent 协作模式，补齐流式交互与运维能力，最后接入 Web channel。

**P2 子阶段执行顺序**：P2-a（进行中）→ P2-d → P2-c → P2-b

- P2-d 先于 P2-c：进程模型决策影响中止生成实现，token 插值是独立安全痛点
- P2-b 最后：当前 CLI + QQ 够用，WebUI 非紧迫

### P2-a：子 Agent 委派模式

**状态**：🚧 进行中（基础委派链路已通，长测中）

- [x] 主 Agent 委派机制（`delegate` 工具 + `AgentRegistry` 预加载子 Agent）
- [x] 专用子 Agent 定义与注册（`[agent.<alias>]` 配置 + `denied_tools` 黑名单）
- [x] 子 Agent 结果回传（同步委派 + `tokio::time::timeout` + 部分输出保留）
- [x] 子 Agent SOUL/workspace 隔离（独立 workspace + sessions.db）
- [x] 防递归委派（子 Agent 不挂 delegate 工具）
- [x] QQ channel 下子 agent 不受 confirm_mode 拦截（channel 固定为 `"delegate"`）
- [x] 标签降级模式下 delegate enum 延迟填充（`set_registry` 后重生成 tool instructions）
- [x] 循环保护：max_iterations 达上限后强制总结（拔工具 + 注入提示词）
- [x] 重复工具检测（三级渐进式警告，防止子 Agent 卡在重复调用循环）
- [x] file 工具 workspace 边界限制（`..` 逃逸拦截，QQ channel 下 file_write/file_edit 放开）

**后续优化（待评估）**：

- [ ] 异步委派：子 Agent 完成后通过唤醒机制通知主 Agent，主 Agent 期间可继续对话（参考 AstrBot 的 `background_task` + CronMessageEvent 方案，需先引入事件/通知子系统）
- [ ] 每子 Agent 独立工具形态：`transfer_to_{name}` 替代单一 `delegate` + enum，对 native tool calling 模式更友好（标签降级模式会增加 system prompt 体积，需权衡）

**详细计划**：[plans/2026-07-23-sub-agent-delegation.md](plans/2026-07-23-sub-agent-delegation.md)
**设计规格**：[specs/2026-07-23-sub-agent-delegation-design.md](specs/2026-07-23-sub-agent-delegation-design.md)
**参考**：[ADR-0002](adr/0002-agent-architecture.md)（委派模式设计）

### P2-d：进程模型与运维

**状态**：✅ 已完成

- [x] token / api_key 环境变量插值（`${VAR}` 语法，未定义变量报错 fail fast）
- [x] sqlite WAL 模式（已存在，确认生效）
- [x] PID 文件检测（`<config_dir>/laia.pid`，重复实例警告不阻止，RAII 自动清理）

### P2-c：流式交互增强

**状态**：✅ 已完成

- [x] 用户主动中止生成（CLI Ctrl+C 优雅退出，Agent 检测 tx closed 保存部分输出）
- [x] 工具调用状态通知（QQ channel 收到 ToolStart 发送 `🔧 {tool}...` 提示）
- [x] delegate 工具进度流式（子 Agent Chunk 转发给主 channel，用户可见委派进度）

### P2-b：Web Channel

**状态**：⏳ 计划中（推迟到最后）

- [ ] WebSocket server 实现
- [ ] `TurnEvent` 直接映射为 WS frame 转发（真流式到浏览器）
- [ ] Web UI 前端（聊天界面 + 工具调用展示）
- [ ] 浏览器端中止生成（用户主动取消）

### P2-e：能力扩展（待评估）

- [ ] 群聊支持（@ 机器人、群消息事件）
- [ ] 图片 / 语音 / 文件消息收发
- [ ] 主动消息推送
- [ ] 邮箱 channel

---

## 发布版本

- [ ] **0.1.0**：P1 + P1.5 整体稳定后首个 release（当前长测中）

后续版本号按语义化版本管理，P2 各子阶段视完成情况拆分为 0.2.0 / 0.3.0 ...

---

## 工程约定

- 每个 Task 完成后跑 `cargo test` + `cargo clippy`
- 提交节奏：一个完整功能/修复链路验证通过后提交一次，不要每个 Task 都提交
- 遇到编译错误立即修，不要积累
- 详细实现计划放 `docs/plans/YYYY-MM-DD-<feature>.md`，设计规格放 `docs/specs/YYYY-MM-DD-<feature>-design.md`，架构决策放 `docs/adr/NNNN-<topic>.md`
