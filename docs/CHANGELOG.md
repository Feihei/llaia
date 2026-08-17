# LLAIA Changelog

> This file is the **delivery archive** for each phase (the ✅-marked history formerly in `plan.md`).
> The forward-looking roadmap and next steps live in [`plan.md`](plan.md).
> Per-phase implementation plans are in [`plans/`](plans/), design specs in [`specs/`](specs/), and architecture decisions in [`adr/`](adr/).

---

## P5 — 进行中（v0.2.2 候选）

> P5 各条目按 `plan.md` 推荐顺序逐条交付，每完成一条在此累加勾选清单。

### ✅ P5-1 Provider Compat 层（[ADR-0026](adr/0026-provider-compat.md)）

针对 Ollama / Llama.cpp 等 OpenAI 兼容端点的格式与行为差异，做**非破坏性**专项适配。

**新增**
- `src/provider/compat.rs`：`Compat` 结构体（6 个归一化开关）+ `MaxTokensField` 枚举（`none`/`max_tokens`/`max_completion_tokens`）。
  - `Compat::default()` = bare 行为（与改造前完全一致）。
  - `Compat::detect(base_url)` 按 host 子串自动探测：`ollama` → 全开归一化；`llamacpp` → 同上但 `requires_assistant_after_tool=false`；其余（含 LMStudio `1234`）保持 bare。
  - `ollama()` / `llamacpp()` 预设构造函数；`apply_override(CompatConfig)` 让 `[provider.<id>].compat.*` 显式覆盖探测结果。
- `OpenAiCompatibleProvider` 消费 `Compat`：
  - **reasoning 归一化**：`reasoning_content` / `thinking` delta 折回 `content`。
  - **max_tokens 字段切换**：随 `max_tokens_field` 发 `max_tokens` 或 `max_completion_tokens`；配合 `[provider.<id>].model.<alias>.max_tokens` 上限值。
  - **streaming usage**：`stream_options.include_usage=true` + 解析末帧 `usage`，落 `ChatResponse.usage`。
  - **finish_reason 推断**：缺失但有 tool_calls 时推断为 `tool_calls`。
  - **assistant 占位**：`requires_assistant_after_tool` 时在 tool 消息后补空 assistant。
- `ChatResponse` 新增 `usage: Option<Usage>` 与 `finish_reason: Option<String>`；`StreamEvent` 新增 `Usage(Usage)` 变体（带 `usage` 的流式末帧）。
- `[provider.<id>].compat` 配置子表（`CompatConfig`，全字段 `Option`，可单独覆盖）。

**验证**
- 新增 11 个单元测试（mockito 模拟 Ollama/Llama.cpp SSE：reasoning 折叠、usage 解析、finish_reason 推断、字段选择、assistant 占位、detect 探测、override 覆盖、default 回归）。
- 既有 `provider_http` / `provider_stream` 集成测试通过；`cargo clippy --all-targets -D warnings` 与 `cargo fmt --all --check` 全绿。

**影响面**：所有 `ChatResponse` / `StreamEvent::Usage` 构造点同步更新（anthropic / gemini / fallback / context / mod mock）。

---

### ✅ P5-2 系统提示词 MEMORY 上限（[ADR-0025](adr/0025-system-prompt-memory-budget.md)）

MEMORY.md **全量加载**进 system prompt（不懒加载），但受可配置 token 预算约束；超限时最旧溢出段经 `compact_provider` 摘要压缩（无则硬截断保留近期），SOUL/USER 永留全量、不计入预算。

**新增**
- `src/memory/trim.rs`：`trim_memory_to_budget(memory, budget, compact_provider)` — 复用 `chars()/4` token 启发式；MEMORY 按空行分段为条目，从末尾贪心累加近期条目，旧段经 `compact_provider` 摘要成前缀（摘要失败降级为丢弃），无 `compact_provider` 时硬截断保留末尾预算内条目；单条巨长条目自行截断。结果按 `(内容 hash, 预算, 是否含 provider)` 缓存，避免每轮重摘要导致 system prompt 抖动。
- `src/config.rs`：`[agent.<alias>].memory_token_budget`（默认 4000，单位复用 `chars()/4` 启发式），`AgentConfig` 新增字段。
- `src/channels/cli.rs` `build_single_agent`：拼 `system_prompt_base` 前对 MEMORY 套用预算裁剪；裁剪结果经 `init_system_meta` 缓存，全频道共享且 skills 热重载稳定。
- `src/commands/slash.rs`：新增 `/memory-compact` 斜杠命令，复用 `src/memory/markdown.rs::compress_memory` 把 MEMORY.md **持久化**压缩（写前备份到 `workspace/backups/`，不丢原文于 sqlite）。

**验证**
- 8 个单元测试覆盖：不超限原样返回、超限无 provider 硬截断、超限有 provider 摘要、单条巨长条目截断、缓存稳定性。
- `cargo clippy --lib --all-targets` 与 `cargo test --lib`（trim/config 相关）通过。
- 注：`provider/openai_compat` 的 mockito 测试在沙箱内无法绑定本地端口，属环境问题，与本次改动无关。

**影响面**：仅 system prompt 拼装路径；SOUL/USER 永留全量（人格/画像不削减）。

---

## v0.2.1 (2026-08-14)

**Patch release** — stability and WebUI consistency fixes accumulated since v0.2.0. No breaking changes.

**Bug fixes**
- **QQ channel**: text/media send paths now self-heal on token expiry. When QQ returns code `11244` ("token not exist or expire"), the send loop refreshes the app access token once and retries, instead of reusing the stale token for all attempts (previously only the WS reconnect path refreshed). Fixes repeated failures, especially under a dual-instance setup sharing one `app_id`/`app_secret` (e.g. podman + native).
- **QQ media**: reject 0-byte files early (`send_image`/`send_file` tools and the upload path) so an empty TTS/audio output fails with a clear error instead of QQ's opaque `"file data empty"` (code 10000).

**WebUI**
- Removed the deprecated `workspace` / `soul` / `user` / `memory` inputs from the agent form — these fields are ignored at runtime (the agent home dir is derived from `--config-dir`); they were misleading dead controls.
- The config save now surfaces the backend's real hot-reload note (provider / runtime / skills / MCP / cron / sub-agents are reloaded in-process) instead of a hardcoded "restart to take effect" alert.
- **Hot-reload for cron/mcp raw editors**: editing `cron.toml` / `mcp.toml` via the WebUI raw TOML editors now hot-reloads immediately (was write-only, required a restart). Both the structured config save and the raw editors now share the same reload logic.

---

## v0.2.0 (2026-08-13)

**Milestone**: First minor release after v0.1.0. The headline change is the breaking workspace directory migration, plus accumulated capability expansions from P1.5–P4+ and several stability fixes.

**⚠️ Breaking change — workspace directory migration**
- Agent home moved from `~/.llaia/` root to `~/.llaia/workspace/`: SOUL.md / USER.md / MEMORY.md / sessions.db / uploads / subagent/ now all live under `workspace/`.
- On first launch, old data is migrated automatically and a `.migrated_v0.2` marker is written; `workspace_root` can now be switched via `/move` (the agent home stays fixed — see AGENTS.md for the distinction).

**Delivered in this release (accumulated since v0.1.0)**
- P1.5: QQ channel + streaming output across all channels + stability patches
- P2: main Agent delegates to sub-agents (`delegate` tool) + Web channel (WebUI)
- P3: capability boundaries / `llaia init` / cron schedules / MCP client / Skill system
- P3+: Anthropic provider, Telegram / DingTalk / WeChat channels, interaction quick-wins
- P4: timezone awareness, dreaming, smarter context compaction, permission tiers (read-only / default / yolo), shutdown, Gemini provider, Feishu channel, and other baseline enhancements
- Stability: `rustls` crypto provider pinned to `ring` (fixes the dual-provider panic, see commit `1010bb7`)
- i18n: user-facing output, init templates, and built-in example skills unified to English (USER template keeps `language: Chinese` preference)

See the P1–P4+ sections below for the detailed per-phase delivery list.

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
- [x] `llaia config` / `llaia doctor` / `llaia remember` 子命令

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

**状态**：✅ 已完成

**目标**：引入主从 Agent 协作模式，补齐流式交互与运维能力，最后接入 Web channel。

**P2 子阶段执行顺序**：P2-a → P2-d → P2-c → P2-b

- P2-d 先于 P2-c：进程模型决策影响中止生成实现，token 插值是独立安全痛点
- P2-b 最后：当前 CLI + QQ 够用，WebUI 非紧迫（最终定位为配置面板为主）

### P2-a：子 Agent 委派模式

**状态**：✅ 已完成（基础委派 + 循环保护 + 重复工具检测 + workspace 边界 + 异步委派；「每子 Agent 独立工具形态」经评估明确不做，P2-a 收敛完成）

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

**后续优化（已并入主线）**：

- [x] 异步委派（见 [spec](specs/2026-08-12-async-delegation-design.md)）：`delegate` 工具新增 `async:bool` 参数（默认 false，零回归）；`async:true` 时 `tokio::spawn` 后台跑子 agent 并立即返回 ack，结果经 channel `pusher()` 推回原会话（仅最终结果，前缀 `[子Agent {name} 完成]`）；`/delegate-list` + `/delegate-cancel <id>` 取消；每会话并发上限 3（硬编码）。CLI 走 stdout，未实现 `ProactivePusher` 的 channel（飞书/Telegram/微信/钉钉）异步委派返回友好错误。
- [x] 每子 Agent 独立工具形态（`transfer_to_{name}`）**明确不做**：当前单一 `delegate` + `agent_name` enum 在 native 与标签降级两种模式均通吃。改为 N 个 `transfer_to_{name}` 仅对 native 模式略有好处，但在标签降级模式下会令 system prompt 多出 N-1 块工具说明，且动态生成 / 热重载更复杂。净收益边际甚至为负。

**详细计划**：[plans/2026-07-23-sub-agent-delegation.md](plans/2026-07-23-sub-agent-delegation.md)
**设计规格**：[specs/2026-07-23-sub-agent-delegation-design.md](specs/2026-07-23-sub-agent-delegation-design.md)
**参考**：[ADR-0002](adr/0002-agent-architecture.md)（委派模式设计）

### P2-d：进程模型与运维

**状态**：✅ 已完成

- [x] token / api_key 环境变量插值（`${VAR}` 语法，未定义变量报错 fail fast）
- [x] sqlite WAL 模式（已存在，确认生效）
- [x] PID 文件检测（`<config_dir>/llaia.pid`，重复实例警告不阻止，RAII 自动清理）

### P2-c：流式交互增强

**状态**：✅ 已完成

- [x] 用户主动中止生成（CLI Ctrl+C 优雅退出，Agent 检测 tx closed 保存部分输出）
- [x] 工具调用状态通知（QQ channel 收到 ToolStart 发送 `🔧 {tool}...` 提示）
- [x] delegate 工具进度流式（子 Agent Chunk 转发给主 channel，用户可见委派进度）

### P2-b：Web Channel

**状态**：✅ 已完成（实际定位为配置面板为主，聊天为辅）

- [x] axum HTTP server + WebSocket（`WebChannel` 实现 `Channel` trait）
- [x] `WebSink` 实现 `OutputSink`，通过 mpsc 与 WS 写 task 解耦
- [x] `TurnEvent` → `WebEvent` 扁平 JSON 转发到浏览器（真流式）
- [x] 浏览器端中止生成（`stop` 消息 → `Notify`）
- [x] 鉴权：Bearer / cookie / query token 三路提取，token 留空时启动随机生成
- [x] 配置 API：`GET/PUT /api/config`（结构化，敏感字段掩码）、`GET/PUT /api/config/raw`（TOML 文本）、`POST /api/config/validate`、`GET /api/status`
- [x] 多媒体上传/读取：`POST /upload`、`GET /file`（路径安全校验 `resolve_within`）
- [x] 前端单页（Alpine.js + marked + highlight.js + CodeMirror 5），零 node 构建
- [x] `WebConfig` 拆分 `host` + `port` 字段（不使用 `bind`）
- [x] 系统级路由独立到 `src/web/mod.rs`，WS/WebChannel 留在 `src/channels/web.rs`
- [x] WS 心跳保活 + TOML 编辑器初始化时机修复
- [x] CI/CD（GitHub Actions + Docker）

**详细计划**：[plans/2026-08-03-webui-channel.md](plans/2026-08-03-webui-channel.md)
**设计规格**：[specs/2026-08-03-webui-channel-design.md](specs/2026-08-03-webui-channel-design.md)

### P2-e：能力扩展（部分完成）

- [x] 图片 / 文件消息收发（QQ 收发图/文件、CLI `@path`、`send_image`/`send_file` 工具）—— 见 [specs/2026-07-24-multimedia-design.md](specs/2026-07-24-multimedia-design.md)
- [x] 主动消息推送（与 P3 cron 一并实现）
- [x] 邮箱 channel（IMAP 轮询 + SMTP，`lettre` + `async-imap` 生态成熟）—— 见 [specs/2026-08-07-provider-channel-expansion.md](specs/2026-08-07-provider-channel-expansion.md)

---

## P3 — 能力扩展与生态接入

**状态**：✅ 已完成

**目标**：在 P2 已完成的 channel/provider/agent 基础设施上，补齐"能让 agent 真正干活"的能力：QQ 工具边界、初始化引导、定时任务、MCP 工具生态、Skill 技能框架。

**P3 子阶段执行顺序**：P3-a → P3-b → P3-c → P3-d（P3-e 最后）

- P3-a 先做：QQ 能力边界是当前最大痛点
- P3-b 紧随：init 引导是新用户入门必需，轻量快赢
- P3-c 中段：cron 是主动能力的基础，依赖 P3-a 的 workspace 模型
- P3-d 后期：MCP client 接入扩展工具生态
- P3-e 最后：Skill 框架建立在 MCP 之上

### P3-a：Agent 能力边界重塑

**状态**：✅ 已完成

**目标**：把所有 channel 从"主 agent 只能聊天 + 子 agent 全放开"升级为"按 agent 隔离 workspace + 命令拦截"，channel 不再决定工具权限。

**核心思路**：参考 AstrBot 的 workspace 隔离 + 命令拦截路线（详见 ADR-0011），不引入 OS 沙箱（单用户私人助理场景过重）。

- [x] 目录结构重构：`~/.llaia/` 根只放配置 + 敏感信息，主 agent 工作区移到 `~/.llaia/workspace/`，子 agent 在 `~/.llaia/workspace/subagent/<name>/`
- [x] workspace 按 agent 隔离：file/terminal 工具只能在自己 workspace 内操作；主 agent `file_read` 可读 `subagent/`，`file_write`/`file_edit` 不可写 `subagent/`
- [x] 跨 workspace 协作：① 主→子用 delegate 的 `file_paths` 参数复制到子 agent `.inbox/` ② 子→主用 delegate 返回值 `{text, output_files}` ③ USER.md 启动时从主 agent 同步覆盖到子 agent
- [x] terminal cwd 固定为当前 agent workspace 根
- [x] terminal 命令拦截（全局）：`command_policy = blacklist`（默认）/ `whitelist` / `none`；内置黑名单 + 可配白名单
- [x] terminal 路径防御三层（防 LLM 误操作）：① shell 包装拒绝 ② 路径白名单（canonicalize `starts_with` workspace）③ 路径黑名单兜底（跨平台危险目录）
- [x] file 工具路径校验复用 terminal 的第二三层
- [x] confirm_mode 重定义为全局开关（不再 per-channel）：`none`（新默认）/ `always` / `session`；`whitelist` 废弃，加载时 warn + fallback 到 `none`
- [x] 危险动作审计：`~/.llaia/logs/audit.log` 记录所有 `requires_confirm == true` 工具调用
- [x] 目录迁移：启动时检测旧结构，自动迁移到 `workspace/`，写 `.migrated_v0.2` 标记
- [x] `AgentConfig.workspace` / `soul` / `user` / `memory` 字段废弃（自动推导），加载时 warn

**参考**：[ADR-0011](adr/0011-qq-capability-boundary.md)

### P3-b：llaia init 引导命令

**状态**：✅ 已完成

**目标**：新用户运行 `llaia init` 后，生成 `~/.llaia/` 目录骨架 + 基础模板，提示进入 WebUI 完成配置。支持"init → serve → WebUI 配置"流程，无 provider 也能启动。

- [x] `llaia init [--config-dir <path>] [--force]` 子命令：创建 `~/.llaia/`、`logs/`、`workspace/`（含 `uploads/`、`subagent/` 空目录）
- [x] 生成 `config.toml` 默认模板（CLI enabled、QQ/Web disabled、provider/agent 注释占位）
- [x] 生成 `~/.llaia/workspace/SOUL.md` / `USER.md` / `MEMORY.md` 默认模板（内嵌常量）
- [x] 终端输出引导：提示运行 `llaia serve` 后浏览器访问 WebUI 完成配置
- [x] 幂等：已存在的文件不覆盖，只创建缺失项
- [x] 无 provider 启动支持：`llaia serve` 无 provider 时 warn 但正常启动，聊天降级；`llaia chat` 无 provider 报错退出并引导
- [x] provider 热加载：WebUI `PUT /api/config` 保存后触发 `Agent::reload_provider()`，无需重启 serve
- [x] doctor 检查项：provider 配置检查（无则 warn）+ sessions.db 存在性检查（无则 warn）

**参考**：[ADR-0012](adr/0012-llaia-init.md)

### P3-c：cron 定时任务

**状态**：✅ 已完成

**目标**：用户配置定时任务，到点后自动执行。双模式：直接跑工具链 / 唤醒 agent 跑一轮对话。

- [x] cron 配置：`~/.llaia/cron.toml` 或 `[cron.<id>]` section，含 `schedule`（5 字段 cron）、`mode`（`tools` / `agent`）、`task`、`channel`
- [x] cron 调度器：进程启动时加载所有任务
- [x] tools 模式：到点后直接按预定义工具链顺序执行，不消耗 LLM token
- [x] agent 模式：到点后唤醒主 agent，注入系统消息，agent 自主调工具完成任务并回复
- [x] 结果推送：通过指定 channel（QQ/CLI/Web）回推结果
- [x] 持久化：cron 任务定义在 config 文件，进程重启后自动恢复
- [x] WebUI 管理：配置面板加 cron tab，可视化增删改查

**参考**：[ADR-0013](adr/0013-cron-scheduling.md)

### P3-d：MCP Client 接入

**状态**：✅ 已完成

**目标**：作为 MCP client 消费外部 MCP server 提供的工具，扩展 LLAIA 的工具生态（不作为 MCP server 暴露自身能力）。

- [x] MCP client 实现：协议层自实现（JSON-RPC 2.0），支持 stdio / streamable HTTP / SSE 三种 transport
- [x] 配置：`~/.llaia/mcp.toml` 独立文件，`[[server]]` section，支持 `${VAR}` 环境变量插值
- [x] 工具适配：MCP `tools/list` 返回的工具，通过 McpTool adapter 包装成 LLAIA `Tool` trait，以 `<server_id>__<tool_name>` 双下划线命名注册
- [x] 工具调用：MCP `tools/call` 协议 + isError envelope 处理（secret scrubbing + 500 字符截断）+ bounded reconnect
- [x] 启动时连接：进程启动时初始化所有配置的 MCP server，失败的不阻塞启动
- [x] WebUI 配置：配置面板加 MCP tab，状态列表 + raw TOML 编辑 + 测试连接
- [x] 安全：MCP 工具默认 requires_confirm，`safe_tools` 白名单免确认；受 agent 边界约束

**详细计划**：[plans/2026-08-07-mcp-client.md](plans/2026-08-07-mcp-client.md)
**参考**：[ADR-0014](adr/0014-mcp-client.md)

### P3-e：Skill 技能框架

**状态**：✅ 已完成

**目标**：在 MCP 工具之上封装"提示词 + 工具集"的技能包，对齐 OpenAI Codex CLI / Anthropic Claude Skills / AstrBot 的业界标准 SKILL.md 格式。

- [x] Skill 定义：`~/.llaia/skills/<name>/SKILL.md`（markdown + YAML frontmatter），frontmatter 含 `name` / `description` / `duration` / `tools`
- [x] Progressive Disclosure：启动时扫描 `~/.llaia/skills/*/SKILL.md`，追加"## Skills"段列出 active skill 的 name + description + 路径
- [x] 触发机制：agent 判断为主（LLM 看 name+description 自行决定），用户显式提到 skill 名也算触发，不做关键词匹配
- [x] 工具挂载：方案 C — skill 的 `tools` 字段只是 prompt 提示，不实际控制工具挂载
- [x] active 开关：`~/.llaia/skills.json` 控制每个 skill 是否激活
- [x] WebUI 管理：配置面板加 Skill tab，可视化增删改查 + SKILL.md 编辑器
- [x] 内置示例 Skill：todoist（提醒）、news-digest（新闻摘要）、code-review（代码审查）
- [x] 路径安全：skill name / path 注入到 prompt 时过滤危险字符

**详细计划**：[plans/2026-08-07-skill-framework.md](plans/2026-08-07-skill-framework.md)
**参考**：[ADR-0015](adr/0015-skill-framework.md)

---

## P3+ — 交互增强与生态扩展

**状态**：✅ 已完成（2026-08-07）

> 完整评估见 [specs/2026-08-07-provider-channel-expansion.md](specs/2026-08-07-provider-channel-expansion.md)；实施计划见 [plans/2026-08-07-quickwins.md](plans/2026-08-07-quickwins.md)。

### 快赢项

- [x] `/provider` 斜杠命令：列出可用 provider/模型、`/provider <序号>` 或 `/provider <id.alias>` 运行时切换（不写 config.toml）
- [x] model fallback：主模型不可用时自动降级备用模型（`[agent.main].fallback` + `FallbackProvider`）
- [x] WebUI 重启按钮：Config > About 页 Restart Service 按钮，serve 自重启

### Provider 直连

- [x] Anthropic Messages API：system 顶层 + tool_use/tool_result blocks + SSE（`src/provider/anthropic.rs`，`[provider.<id>].type = "anthropic"`；ModelConfig 新增 `max_tokens`）

### Channel 扩展（好实现的）

- [x] Telegram：官方 Bot API + long polling，免公网回调（`src/channels/telegram.rs`；`allow_chat_id` 单用户安全锁；媒体 sendPhoto/sendDocument）
- [x] 钉钉：Stream Mode WS 免公网（`src/channels/dingtalk.rs`；sessionWebhook markdown 回复；`allow_staff_id` 安全锁）
- [x] 微信 ClawBot：腾讯官方 `openclaw-weixin`（ilink bot）接口（`src/channels/wechat.rs`；扫码登录 + getupdates 长轮询；登录态存 `wechat_state.json`；CDN AES-128-ECB 媒体上传；v1 媒体接收仅文本占位）

---

## P4 — 基础能力增强

**状态**：✅ 主体完成（P4-a~P4-e 全交付；P4-f 经复评收敛为空——原待触发项均已明确本阶段不做）

**评估口径**：

- 必要性 **高** = 不做会持续踩坑或已影响正确性；**中** = 明显改善体验，可择机；**低** = 锦上添花或已有替代路径
- 难度 ★☆☆ = 半天内、单点改动；★★☆ = 一到数天、跨多个模块；★★★ = 结构性改造，动手前先出 ADR

### P4 / 时间感知与运行时事实注入

- [x] 时区从 USER.md 剥离，改由 config 注入 + 热更新（必要性：高 / 难度：★☆☆）
  - 见 [ADR-0017](adr/0017-timezone-injection.md)：统一时间源 `src/time.rs` + `RuntimeConfig.timezone`（IANA，None=跟随系统）+ live config 通道，收敛 6 处零散 `Local::now()`；状态栏经 `Context::to_messages` 注入。

### P4 / 记忆与上下文

- [x] 「做梦」：闲时自动整理记忆（必要性：中高 / 难度：★★☆）
  - 见 [ADR-0016](adr/0016-dream.md)：cron 触发的 Agent 模式任务；两阶段管线（draft 蒸馏 → 手术编辑 MEMORY.md）+ 游标增量 + 空闲门控 + 三道防线（`.bak`/diff 推送/`/dream-rollback`），默认开。
- [x] 更聪明的上下文压缩（必要性：高 / 难度：★★★）
  - 见 [ADR-0019](adr/0019-smart-compaction.md)：cheap-first 抽取式归一化先行 + 重要性锚点 + 工具消息裁剪；`compact(provider, keep_recent, token_budget) -> Result<bool>`，无新 config key。
- [x] 上下文注入策略文档化（必要性：中 / 难度：★☆☆，纯文档）

### P4 / 模型与工具调用

- [x] 工具调用格式优化（必要性：高 / 难度：★★☆）
  - 统一 `ToolCallStreamParser` 清洗 think/`<tool_call>` 泄露（native/标签降级通吃），补 markdown fence 格式；见 [spec](specs/2026-08-11-p4b-tool-call-cleanup-design.md)。
- [x] image 描述模型单独设置（必要性：中 / 难度：★★☆）
  - `RuntimeConfig.vision_model` 配置；Agent 持有 `vision_provider`（支持热替换）；`handle_message_streaming` 入口拦截多模态消息，用 vision_provider 逐张描述图片。

### P4 / 进程生命周期与重启机制

- [x] `/api/shutdown` + WebUI 停止按钮（必要性：高 / 难度：★☆☆）
  - 见 [ADR-0018](adr/0018-shutdown.md)：共享 `shutdown_signal: Arc<Notify>`；优雅退出 serve。
- [x] WebUI config 热加载（reload_all，即 P4-f 轻量方案 A）（必要性：中 / 难度：★★☆）
  - 保存 `/api/config` / `/api/config/raw` 后进程内就地重载 agent 定义 / skills / MCP 工具 / cron 任务 / 非连接型 channel 参数，无需重启。
- [x] spawn-after-teardown 顺序 **明确本阶段不做**：被 `reload_all` 覆盖，重启低频无强痛点。
- [x] 同 PID 原地 reload **明确本阶段不做**：同上，被 `reload_all` 与低频重启需求覆盖。

### P4 / 交互增强

- [x] `/move` 或 `/cd` 斜杠命令（必要性：中 / 难度：★☆☆）
  - 已交付（commit `13d275d`）：`/move`/`/cd` 同一 handler；家目录与工具作用域解耦；风险确认走权限档位审批门。明确不做：git-bash `/x/...` 跨盘路径。

### P4 / 权限管理系统

- [x] 三档权限 profile：`read-only` / `default` / `yolo`（必要性：中 / 难度：★★☆）
  - 已交付（commit `147544f`）：`RuntimeConfig.permission` + `ApprovalGate` + `/permission` + `/ok` `/deny` 跨频道一致；WebUI Runtime 表单 permission 下拉。

### P4 / Provider / Channel 继续扩展

- [x] Google Gemini REST provider（generateContent + functionDeclarations）（必要性：中 / 难度：★★☆）
- [x] 邮箱 channel：IMAP 轮询 + SMTP（必要性：中 / 难度：★★☆）
- [x] 飞书 / Lark：事件订阅长连接模式（必要性：中低 / 难度：★★☆）
- [x] OpenAI Responses API **明确本阶段不做**：聚合网关已用 OpenAI 兼容协议绕过。
- [x] Slack Socket Mode / Discord / LINE **明确本阶段不做**：已有 5 个 channel，三者均纯手写未引 SDK，未来若做优先 Slack。
- [x] 明确不做：WhatsApp 自实现、微信个人号非官方协议（封号风险）。

### P4 / 生态复用

- [x] 评估借用 zeroclaw 代码：结论——值得借鉴、不值得依赖。正确姿势是单文件 vendor + 裁剪适配。

### P4 阶段计划（回顾）

**执行顺序**：P4-a → P4-b → P4-c → P4-d → P4-e →（P4-f 按需触发）

| 阶段 | 主题 | 难度 | 排序理由 |
|---|---|---|---|
| **P4-a** | 地基与快赢 | ★☆☆ | 全是单点改动，时区是 P4-c「做梦」前置依赖 |
| **P4-b** | 输出正确性 | ★★☆ | 唯一影响「用户直接看到的东西是否正确」的一组 |
| **P4-c** | 记忆系统进化 | ★★☆~★★★ | 两者共用同一条「抽取 → 合并 → 压缩」管线 |
| **P4-d** | 边界与授权 | ★★☆ | 同一套 workspace 边界模型的一放一收 |
| **P4-e** | 生态扩展 | ★★☆ | 纯增量、互不阻塞 |
| **P4-f** | 已收敛 | — | 结构性改造经复评明确不做，诉求已由 reload_all 覆盖 |

**阶段间依赖**：

- P4-c「做梦」← P4-a 时区（idle 窗口、日期语义）
- P4-c「做梦」← 已有 cron 调度器（P3-c）、`compress_memory`、`compact_provider`
- P4-d 权限 profile ← P3-a 的 `confirm_mode` 与 audit 日志，在其上演进而非重写
- P4-f 同 PID reload ← 各 channel 的 cancellation token 化，是全 P4 最大的结构性改造，不进主线
