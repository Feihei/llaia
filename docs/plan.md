# LLAIA 项目 Roadmap

> 本文档是 LLAIA 的整体阶段路线图，标注各阶段状态与关键交付物。
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

**状态**：✅ 已完成（基础委派 + 循环保护 + 重复工具检测 + workspace 边界）

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

**后续优化（推迟到 P3+ 视需求评估）**：

- [ ] 异步委派：子 Agent 完成后通过唤醒机制通知主 Agent，主 Agent 期间可继续对话（参考 AstrBot 的 `background_task` + CronMessageEvent 方案，需先引入事件/通知子系统）
- [ ] 每子 Agent 独立工具形态：`transfer_to_{name}` 替代单一 `delegate` + enum，对 native tool calling 模式更友好（标签降级模式会增加 system prompt 体积，需权衡）

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

**P3 子阶段执行顺序**：P3-a → P3-b → P3-c → P3-d

- P3-a 先做：QQ 能力边界是当前最大痛点（QQ 只能聊天，有副作用的工具全被拒）
- P3-b 紧随：init 引导是新用户入门必需，轻量快赢
- P3-c 中段：cron 是主动能力的基础，依赖 P3-a 的 workspace 模型
- P3-d 后期：MCP client 接入扩展工具生态
- P3-e 最后：Skill 框架建立在 MCP 之上，封装提示词 + 工具集

### P3-a：Agent 能力边界重塑

**状态**：✅ 已完成

**目标**：把所有 channel 从"主 agent 只能聊天 + 子 agent 全放开"升级为"按 agent 隔离 workspace + 命令拦截"，channel 不再决定工具权限。

**核心思路**：参考 AstrBot 的 workspace 隔离 + 命令拦截路线（详见 ADR-0011），不引入 OS 沙箱（单用户私人助理场景过重）。

- [x] 目录结构重构：`~/.llaia/` 根只放配置 + 敏感信息，主 agent 工作区移到 `~/.llaia/workspace/`，子 agent 在 `~/.llaia/workspace/subagent/<name>/`
- [x] workspace 按 agent 隔离：file/terminal 工具只能在自己 workspace 内操作（类似容器挂载目录）；主 agent `file_read` 可读 `subagent/`（整合子 agent 产出），`file_write`/`file_edit` 不可写 `subagent/`
- [x] 跨 workspace 协作：① 主→子用 delegate 的 `file_paths` 参数复制到子 agent `.inbox/`（每次委派先清空再复制）② 子→主用 delegate 返回值 `{text, output_files}`（output_files 从子 agent 工具调用记录提取 file_write/file_edit 路径）③ USER.md 启动时从主 agent 同步覆盖到子 agent（子 agent memory_write 拒写 USER.md），SOUL.md 各自独立
- [x] terminal cwd 固定为当前 agent workspace 根
- [x] terminal 命令拦截（全局，不区分 channel）：`command_policy = blacklist`（默认）/ `whitelist` / `none`；内置黑名单（rm -rf /、sudo、shutdown、reboot、kill -9、dd、mkfs、curl|sh 等）+ 可配白名单
- [x] terminal 路径防御三层（防 LLM 误操作，不防恶意用户）：① shell 包装拒绝（拦截 `bash -c`/`eval`/`$()`/反引号等任意代码执行）② 路径白名单（canonicalize `starts_with` workspace，含不存在路径回溯祖先）③ 路径黑名单兜底（跨平台危险目录前缀：Linux `/root`/`/usr`/`/etc`、macOS `/System`/`/Library`、Windows `C:\Windows`/`C:\Program Files` 等）
- [x] file 工具路径校验复用 terminal 的第二三层（canonicalize `starts_with` workspace + 黑名单兜底），不需要 shell 词法解析
- [x] confirm_mode 重定义为全局开关（不再 per-channel）：`none`（新默认）/ `always` / `session`；`whitelist` 废弃，加载时 warn + fallback 到 `none`
- [x] 危险动作审计：`~/.llaia/logs/audit.log` 记录所有 `requires_confirm == true` 工具调用（timestamp / agent / channel / tool / args / result）
- [x] 目录迁移：启动时检测旧结构（SOUL.md/USER.md/MEMORY.md/sessions.db 在 `~/.llaia/` 根），自动迁移到 `workspace/`，写 `.migrated_v0.2` 标记
- [x] `AgentConfig.workspace` / `soul` / `user` / `memory` 字段废弃（自动推导），加载时 warn

**参考**：[ADR-0011](adr/0011-qq-capability-boundary.md)

### P3-b：llaia init 引导命令

**状态**：✅ 已完成

**目标**：新用户运行 `llaia init` 后，生成 `~/.llaia/` 目录骨架 + 基础模板，提示进入 WebUI 完成配置。支持"init → serve → WebUI 配置"流程，无 provider 也能启动 serve。

- [x] `llaia init [--config-dir <path>] [--force]` 子命令：创建 `~/.llaia/`、`logs/`、`workspace/`（含 `uploads/`、`subagent/` 空目录）
- [x] 生成 `config.toml` 默认模板（CLI enabled、QQ/Web disabled、provider/agent 注释占位）
- [x] 生成 `~/.llaia/workspace/SOUL.md` / `USER.md` / `MEMORY.md` 默认模板（内嵌常量）
- [x] 终端输出引导：提示运行 `llaia serve` 后浏览器访问 WebUI 完成 provider/agent/channel 配置
- [x] 幂等：已存在的文件不覆盖，只创建缺失项
- [x] **无 provider 启动支持**：`llaia serve` 无 provider 时 warn 但正常启动，WebUI 配置功能可用，聊天功能降级（返回提示而非报错）；`llaia chat` 无 provider 报错退出并引导
- [x] **provider 热加载**：WebUI `PUT /api/config` 保存后触发 `Agent::reload_provider()`，无需重启 serve；正在进行的 turn 用旧 provider 完成后切换；失败回滚
- [x] **doctor 检查项**：provider 配置检查（无则 warn）+ sessions.db 存在性检查（无则 warn）

**参考**：[ADR-0012](adr/0012-llaia-init.md)

### P3-c：cron 定时任务

**状态**：✅ 已完成

**目标**：用户配置定时任务，到点后自动执行。双模式：直接跑工具链 / 唤醒 agent 跑一轮对话。

- [x] cron 配置：`~/.llaia/cron.toml` 或 `[cron.<id>]` section，含 `schedule`（5 字段 cron 表达式）、`mode`（`tools` / `agent`）、`task`（工具链 JSON / agent 提示词）、`channel`（结果推送目标）
- [x] cron 调度器：`tokio_cron_scheduler` 或自实现，进程启动时加载所有任务
- [x] tools 模式：到点后直接按预定义工具链顺序执行（如 `tavily_search` → `memory_write` → `send_message`），不消耗 LLM token
- [x] agent 模式：到点后唤醒主 agent，注入系统消息（"8:00 到了，按计划执行 X"），agent 自主调工具完成任务并回复用户
- [x] 结果推送：通过指定 channel（QQ/CLI/Web）回推结果
- [x] 持久化：cron 任务定义在 config 文件，进程重启后自动恢复
- [x] WebUI 管理：在配置面板加 cron tab，可视化增删改查

**参考**：[ADR-0013](adr/0013-cron-scheduling.md)

### P3-d：MCP Client 接入

**状态**：✅ 已完成

**目标**：作为 MCP client 消费外部 MCP server 提供的工具，扩展 LLAIA 的工具生态（不作为 MCP server 暴露自身能力）。

- [x] MCP client 实现：协议层自实现（JSON-RPC 2.0），支持 stdio / streamable HTTP / SSE 三种 transport
- [x] 配置：`~/.llaia/mcp.toml` 独立文件，`[[server]]` section，含 `command` / `args` / `env`（stdio）或 `url` / `headers`（HTTP/SSE），支持 `${VAR}` 环境变量插值
- [x] 工具适配：MCP `tools/list` 返回的工具，通过 McpTool adapter 包装成 LLAIA `Tool` trait 实现，以 `<server_id>__<tool_name>` 双下划线命名注册到主 agent
- [x] 工具调用：MCP `tools/call` 协议 + isError envelope 处理（secret scrubbing + 500 字符截断）+ bounded reconnect
- [x] 启动时连接：进程启动时初始化所有配置的 MCP server，失败的不阻塞启动（log + 跳过）
- [x] WebUI 配置：配置面板加 MCP tab，状态列表 + raw TOML 编辑 + 测试连接
- [x] 安全：MCP 工具默认 requires_confirm，`safe_tools` 白名单免确认；受 agent 边界（denied_tools / confirm_mode / audit）约束

**详细计划**：[plans/2026-08-07-mcp-client.md](plans/2026-08-07-mcp-client.md)
**参考**：[ADR-0014](adr/0014-mcp-client.md)

### P3-e：Skill 技能框架

**状态**：✅ 已完成

**目标**：在 MCP 工具之上封装"提示词 + 工具集"的技能包，让用户可以快速给 agent 加能力。对齐 OpenAI Codex CLI / Anthropic Claude Skills / AstrBot 的业界标准 SKILL.md 格式。

- [x] Skill 定义：`~/.llaia/skills/<name>/SKILL.md`（markdown + YAML frontmatter，对齐业界标准），frontmatter 含 `name` / `description` / `duration`（turn / session，默认 turn）/ `tools`（可选，提示 LLM 推荐用的工具列表，不实际控制挂载）
- [x] Progressive Disclosure：启动时扫描 `~/.llaia/skills/*/SKILL.md`，解析 frontmatter 拿 `name` + `description`，在 system prompt 追加"## Skills"段列出所有 active skill 的 name + description + SKILL.md 路径；规则提示 LLM "用 skill 前必须先 file_read 它的 SKILL.md"
- [x] 触发机制：agent 判断为主（LLM 看 name+description 自行决定），用户显式提到 skill 名也算触发（LLM 自然语言理解，无特殊语法）。不做关键词匹配
- [x] 工具挂载：方案 C — skill 的 `tools` 字段只是 prompt 提示，不实际控制工具挂载。内置工具始终全挂载，MCP 工具按 server 挂载（与 skill 解耦）
- [x] active 开关：`~/.llaia/skills.json` 控制每个 skill 是否激活（类似 AstrBot）
- [x] WebUI 管理：配置面板加 Skill tab，可视化增删改查 + SKILL.md 编辑器
- [x] 内置示例 Skill：todoist（提醒）、news-digest（新闻摘要）、code-review（代码审查）
- [x] 路径安全：skill name / path 注入到 prompt 时过滤危险字符（借鉴 AstrBot `_SAFE_PATH_RE`），防 prompt injection

**详细计划**：[plans/2026-08-07-skill-framework.md](plans/2026-08-07-skill-framework.md)
**参考**：[ADR-0015](adr/0015-skill-framework.md)

---

## P3+ — 交互增强与生态扩展（已完成）

**状态**：✅ 已完成（2026-08-07）

> 完整评估见 [specs/2026-08-07-provider-channel-expansion.md](specs/2026-08-07-provider-channel-expansion.md)；实施计划见 [plans/2026-08-07-quickwins.md](plans/2026-08-07-quickwins.md)。

### 快赢项

- [x] `/provider` 斜杠命令：`/provider` 列出可用 provider/模型（当前标记 `*`）、`/provider <序号>` 或 `/provider <id.alias>` 运行时切换（不写 config.toml）
- [x] model fallback：主模型不可用时自动降级备用模型（`[agent.main].fallback` + `FallbackProvider`）
- [x] WebUI 重启按钮：Config > About 页 Restart Service 按钮，serve 自重启

### Provider 直连（参考 zeroclaw-providers 移植，不引依赖）

- [x] Anthropic Messages API：system 顶层 + tool_use/tool_result blocks + SSE（`src/provider/anthropic.rs`，`[provider.<id>].type = "anthropic"` 分发；ModelConfig 新增 `max_tokens`）

### Channel 扩展（好实现的）

- [x] Telegram：官方 Bot API + long polling，免公网回调（`src/channels/telegram.rs`；`allow_chat_id` 单用户安全锁；媒体 sendPhoto/sendDocument）
- [x] 钉钉：Stream Mode WS 免公网（`src/channels/dingtalk.rs`，参考 zeroclaw 554 行移植；sessionWebhook markdown 回复；`allow_staff_id` 安全锁）
- [x] 微信 ClawBot：腾讯官方 `openclaw-weixin`（ilink bot）接口（`src/channels/wechat.rs`；扫码登录 + getupdates 长轮询；登录态存 `wechat_state.json`；CDN AES-128-ECB 媒体上传；v1 媒体接收仅文本占位）

---

## P4 — 基础能力增强

**状态**：⏳ 计划中

> 来源：[docs/issues/](issues/) 收集的反馈与扩展评估，已实现的见各阶段完成清单。
> 已取消：cron.toml 移入 agent workspace（10#）—— CronTool 已让 agent 动态管理任务，无需文件层编辑。
> 2026-08-10 重整：新增「时间感知」「做梦」两条，全部条目按主题重新分组并标注必要性/难度，末尾汇总为 P4-a ~ P4-f 阶段计划。

**评估口径**：

- 必要性 **高** = 不做会持续踩坑或已影响正确性；**中** = 明显改善体验，可择机；**低** = 锦上添花或已有替代路径
- 难度 ★☆☆ = 半天内、单点改动；★★☆ = 一到数天、跨多个模块；★★★ = 结构性改造，动手前先出 ADR

---

### P4 / 时间感知与运行时事实注入

- [x] **时区从 USER.md 剥离，改由 config 注入 + 热更新**（必要性：**高** / 难度：★☆☆）
  - 现状问题：
    1. **agent 实际上没有当前时间感知**。system prompt 只在进程启动时拼一次（`channels/cli.rs:441`），USER.md 里的「时区：Asia/Shanghai」（`memory/markdown.rs:46`）只是一行静态文本——模型知道时区名，但不知道「现在几点、今天星期几」，跨天长驻进程里这行还会越来越误导。
    2. **散落的本地时间调用**。`chrono::Local::now()` 在 cron/runner、audit、memory_write、MEMORY 备份、`llaia remember` 等 6 处各写各的，全依赖宿主机 TZ；Docker 镜像默认 UTC，导致记忆条目日期、审计时间戳、cron 触发时刻整体偏 8 小时，且与 USER.md 里声明的时区自相矛盾。
    3. **热更新缺口（关键）**：`Agent` 持有的是 `config: Arc<Config>` **启动快照**（`agent/mod.rs:79,123`），而 `hot_reload_providers`（`web/mod.rs:391`）只换 provider、**不更新** 这个快照。即便加了 `[runtime].timezone`，WebUI 改了也不会被 agent 读到——必须先把 live config 通道打通。
  - **细化方案（详见 [ADR-0017](docs/adr/0017-timezone-injection.md)）**：
    - **① Config schema**：`RuntimeConfig` 加 `pub timezone: Option<String>`（IANA 名，`default_timezone()` → `None` = 跟随系统，零配置无回归）；`Config::load` 加校验——非法 IANA 名（参考 `compact_model` 校验写法，`config.rs:440`）warn + 置 `None`。`Cargo.toml` 加 `chrono-tz`（与现有 `chrono 0.4` 兼容，用 `0.10`）。
    - **② 统一时间源 `src/time.rs`**（新建）：
      - `pub struct Now { naive: NaiveDateTime, zone_label: String }`；`pub fn now(tz: &Option<String>) -> Now`（set → `chrono_tz::Tz` 解析 + `Utc::now().with_timezone`；unset → `Local::now()`）；`pub fn unix_now() -> i64`（`Utc::now().timestamp()`，**时区无关**，供做梦空闲门算 elapsed）；`pub fn resolve_tz(tz: &Option<String>) -> Option<Tz>`（`OnceLock<Mutex<HashMap>>` 缓存解析结果，非法/未设 → `None`）。
      - `pub fn status_bar(tz: &Option<String>) -> String`：产出 §2.6.3「Agent 状态栏」文本 = 当前本地时间(含星期) + 时区标签 + **§2.6.5 操作提示**（如「距上次对话已过 N 分钟，空闲中 / 当前非工作时间，可执行整理类后台任务」）。
    - **③ live config 通道（热更新核心）**：`Agent` 新增 `live_config: Arc<RwLock<Config>>`，`new()` 接收它；`build_agent` 在 WebUI 下传 `state.config.clone()`、CLI 下设 `Arc::new(RwLock::new(config.clone()))`（永不更新，与 CLI 无热加载现状一致）。在 `hot_reload_providers`（`web/mod.rs:391`）末尾补：`*state.registry.main.lock().await.live_config.write().await = new_config.clone();` —— 下次 `to_messages` 即用新时区，进行中的 turn 不受影响（snapshot 语义）。保留原 `config: Arc<Config>` 快照给 `/provider` 等 provider 引用读取（slash.rs 不需要改）。
    - **④ 状态栏注入点**：`Context::to_messages(&self) -> Vec<ChatMessage>`（`agent/context.rs:24`）改为 `to_messages(&self, tz: &Option<String>)`，在末尾 `msgs.push(ChatMessage::user(crate::time::status_bar(tz)))`。调用方 `agent/mod.rs:293` 改为传 `&self.live_config.read().await.runtime.timezone`；`context.rs:133/143` 两个测试改为 `to_messages(&None)`。**不**写入 `context.history`——状态栏每轮 `to_messages` 现算、仅挂尾，整段 system（SOUL/USER/MEMORY）作为稳定前缀被缓存，符合 §2.6.3。QQ/Telegram/Web 全部经 `handle_message_streaming → to_messages`，自动覆盖。
    - **⑤ 收敛零散 `Local::now()`**：
      - **改走 tz**：`tools/memory.rs:61`（/remember 落盘日期）、`memory/markdown.rs:75`（MEMORY 备份文件名）、`commands/mod.rs:682`（remember CLI 日期）→ `time::now(tz).naive.format("%Y-%m-%d")`。其中 memory 工具需拿到 tz：v1 先传 `&None`（仍系统本地，行为与今一致，无回归），列入后续「把 tz 透传进工具」的小改进；**模型感知的时间（状态栏）已是正确的配置时区**，这正是本项的核心目标。
      - **维持 UTC（运维日志，不动）**：`audit.rs:41`、`memory/sqlite.rs:106/128/156`、`cron/runner.rs:54` 本就用 `Utc::now()` 或仅打日志，保持 UTC，与用户可见时间解耦。
    - **⑥ 配套清理**：`USER_TEMPLATE`（`memory/markdown.rs:46`）去掉「时区：Asia/Shanghai」行；旧 USER.md 不强制迁移（纯静态文本无害，下次编辑自然淘汰）；`llaia doctor` 加时区解析检查；WebUI 配置面板暴露 `[runtime].timezone`。
  - **验证要点**：① 改 timezone 后不重启，`/dream` 触发或下一轮对话的状态栏即时反映新时区；② 多轮对话下 system 前缀 token 稳定（cache 命中）；③ `resolve_tz("Asia/Shanghai")`→`Some`、`resolve_tz("Mars/Phobos")`→`None`；④ Docker(UTC) 下记忆落盘日期与配置时区一致而非宿主机 UTC。

### P4 / 记忆与上下文

- [x] **「做梦」：闲时自动整理记忆**（必要性：**中高** / 难度：★★☆）
  - 动机：MEMORY.md 目前只在超限时被动压缩（`memory/markdown.rs:65 compress_memory`），长期使用必然堆积重复、过期、互相矛盾的条目；而会话里产生的有效信息，只有用户手动 `/remember` 才能沉淀下来。
  - 形态：cron 触发的 agent 模式任务，在独立 cron session 上跑一轮，通过 `memory_write` 整理记忆（主会话历史不污染）。
  - 可直接复用的现成件：cron 调度器 + `run_agent_mode`（P3-c，独立 session）、`compress_memory`、sqlite 会话记录、audit 日志、pusher。
  - **设计已决议（见 [ADR-0016](docs/adr/0016-dream.md)）**，要点：
    - **架构**：做梦 = 一个 Agent 模式的 cron 任务，复用 `run_agent_mode`（`src/cron/runner.rs`），**不引入新 agent 架构**（否决「影子 agent / 独立闭环」——用户指出它会凭空造第三类东西，与现有架构不对齐）。
    - **两阶段管线（2026-08-11 增补，参考 nanobot）**：stage1 临时 dream 会话蒸馏增量历史 → 写 `dream_draft.md`（中间缓冲，不进上下文）；stage2 基于 draft 手术式全编辑 MEMORY.md（进上下文）。两阶段都跑在独立 cron 会话，复用同一套 machinery。
    - **游标增量（2026-08-11 增补）**：sqlite 记 `last_dream_message_id`，stage1 每轮只消化 `id > 游标` 的新消息、成功后推进；首启游标置当前最大 id（不重放老历史）。增量、可续跑、崩溃安全。
    - **触发**：cron 定时调度 + 运行时空闲门控（距最后一条用户消息 N 分钟无交互才跑），不做独立 idle 检测器（参考 openclaw）。空闲门依赖 P4-a 时区，但为「配套」非「阻塞」。
    - **模型**：主模型 + fallback，v1 不起独立 `compact_provider` 实例（idle 门已保证不抢活跃会话）。
    - **写入边界**：MEMORY.md 放开全编辑（增 / 改 / 删，去重压缩价值成立之必须，由扩展后 `memory_write` 承载）；USER.md v1 不碰（最多在 diff 摘要里「提议」）。
    - **安全兜底**：保留手写时间戳 `.bak`（**不引 git**，本地单用户够用）→ 事后算 diff 推送通知用户（绝不静默）→ `/dream` 手动触发 → `/dream-rollback` 回滚 → **默认开**（idle 门 + diff + 回滚三道防线使风险可控）。
- [x] 更聪明的上下文压缩：防止重要信息丢失、提高缓存命中、减少对 LLM 压缩的依赖（参考《深入理解 AI Agent》李博杰 v1.2 §2.7.2）（必要性：**高** / 难度：★★★）
  - **设计已决议（见 [ADR-0019](docs/adr/0019-smart-compaction.md)）**，要点：
    - **廉价抽取式先行（cheap-first，不调 LLM）**：`compact` 每轮先跑 `cheap_normalize`——丢弃空消息、多模态图片降级为 `[图片]`、工具消息截断到 500 字符、连续重复用户消息去重。归一化后若仍在预算内（`estimate_tokens() <= token_budget`）直接返回，跳过 LLM 摘要：既省一次调用，又保留 `summary` 前缀稳定（KV cache 友好）。
    - **重要性锚点（落地 ADR-0004「首条用户消息留」）**：首条用户消息若落入 to_compress 区，提出来前置到 to_keep 且不进摘要 dump——永不被摘要掉（原实现会把它一起摘要丢）。
    - **工具消息裁剪**：ADR-0004「工具调用结果可丢」——工具消息在上下文里只留截断短注（完整内容已在 sqlite 留底），且进摘要 dump 时只给一行 `[tool] (结果已归档) <前80字>`，不让大段工具输出消耗 LLM 输入。
    - 签名 `compact(provider, keep_recent, token_budget) -> Result<bool>`（bool=是否真调 LLM）；调用方补 `token_budget = context_size`（主循环 + `/compact`）。不新增 config key，作为默认升级。
- [x] 上下文注入策略文档化：明确每次启动注入哪些记忆（SOUL/USER/MEMORY + 上一轮未完成会话历史 + 近期摘要），供用户理解记忆边界（必要性：**中** / 难度：★☆☆，纯文档）
- [x] 上下文注入策略文档化：明确每次启动注入哪些记忆（SOUL/USER/MEMORY + 上一轮未完成会话历史 + 近期摘要），供用户理解记忆边界（必要性：**中** / 难度：★☆☆，纯文档）

### P4 / 模型与工具调用

- [x] 工具调用格式优化：解决 think 内容 / `<tool_call>` 标签泄露到用户回复的问题（agen 系模型偶发），研究 jinja 模板调用格式，参考 AstrBot `core/agent/tool.py` 与 zeroclaw `zeroclaw-tool-call-parser`（必要性：**高**，属可见的正确性缺陷 / 难度：★★☆）
  - **实现**：统一走 `ToolCallStreamParser`（去掉 native/标签降级分支），native 模式下 think/tool_call 标签泄露也被清洗；补充 markdown fence 格式（`` ```tool_call `` / `` ```invoke ``）；`iter_text` 改存清洗后文本（不污染 context/sqlite）。详见 [specs/2026-08-11-p4b-tool-call-cleanup-design.md](specs/2026-08-11-p4b-tool-call-cleanup-design.md)
- [x] image 描述模型单独设置：主模型无多模态时，用独立模型描述图片，避免能力缺失（必要性：**中** / 难度：★★☆）
  - **实现**：`RuntimeConfig.vision_model` 配置（照搬 `compact_model` 模式）；Agent 持有 `vision_provider`（支持热替换）；`handle_message_streaming` 入口拦截多模态消息，用 vision_provider 逐张描述图片，描述文本替换图片注入主模型上下文

### P4 / 进程生命周期与重启机制

> 背景（2026-08-10 评估）：WebUI 重启按钮走 spawn 替代进程路线（zeroclaw 的 respawn 层），子进程故意脱离终端——终端启动的用户重启后失去 Ctrl+C 控制，只能任务管理器杀。调研 zeroclaw（`.ref/zeroclaw`）三层机制后结论：全套 daemon/supervisor 太重不借鉴，但两项便宜改进与一项正解记入本节。当前阶段决定保持轻量不动，痛点可用现有机制缓解（provider 改动已热加载，多数场景无需重启）。

- [x] `/api/shutdown` + WebUI 停止按钮：优雅退出 serve，解决脱管进程只能任务管理器杀的痛点（必要性：**高** / 难度：★☆☆）
  - **现状**：WebUI 已有 `/api/restart`（`web/mod.rs:466`，spawn 替代进程后 `exit(0)`），但**没有** `/api/shutdown`；`serve_cmd`（`commands/mod.rs:463`）只靠 `tokio::select! { ctrl_c / 所有 channel task 结束 }` 退出，且退出路径**无任何清理**（cron 调度器不显式 shutdown、spawn 出的 channel task 直接随进程退出丢弃）。终端启动的用户点 restart 后子进程脱离终端、失去 Ctrl+C，只能任务管理器杀——这正是本项的痛点来源。
  - **细化方案（详见 [ADR-0018](docs/adr/0018-shutdown.md)）**：
    - **① 共享 shutdown 信号**：`WebChannel` 新增 `pub shutdown_signal: Arc<tokio::sync::Notify>`（`web.rs:301` 结构体；在 `new()` 里 `Arc::new(Notify::new())`）。`build_router`（`web.rs:361`）把它 clone 进 `AppState`（新字段 `shutdown_signal`）。`serve_cmd` 在 spawn 前 `let shutdown_signal = web.shutdown_signal.clone();` 持有同一 Arc。
    - **② 新 handler `shutdown_service`（`web/mod.rs`，仿 `restart_service`）**：`authorize` 通过后 `state.shutdown_signal.notify_one()`。与 restart 不同，**容器内允许**（stop 正是用户想要的，停止 PID1=停容器，合理）；`build_system_routes()`（`web/mod.rs:1138` 附近）加 `.route("/api/shutdown", axum::routing::post(shutdown_service))`。
    - **③ `serve_cmd` 收尾（`commands/mod.rs:463`）**：`tokio::select!` 增加分支 `_ = shutdown_signal.notified() => { … }`，与 ctrl_c 共用后续清理：
      ```rust
      // 退出路径共用：先停 cron，再 abort 各 channel task 并 await
      if let Some(cron) = &_cron { let _ = cron.shutdown().await; }   // cron/mod.rs:299
      for t in &tasks { t.abort(); }
      for t in tasks { let _ = t.await; }
      println!("\n{}", crate::banner::GOODBYE);
      ```
      `tasks` 不再被 `for t in tasks { t.await }` 消费（改为 `for t in &mut tasks { … }` 借用），保证 shutdown 分支能 abort 它们。
    - **④ 响应时序**：handler 先 `return Json({shutting_down:true})`，再用 `tokio::spawn` 延迟 ~100ms 后 `notify_one()`，确保浏览器收到响应、WebUI 切到「已停止」态，再触发进程收尾。
    - **⑤ WebUI 前端**：`index.html:377` 的 `Restart Service` 按钮旁加 `Stop Service` 按钮，`app.js:499 restartService()` 旁加 `shutdownService()`（POST `/api/shutdown`，成功后显示「Service stopped」而非轮询重启）。
  - **验证要点**：① 点停止按钮 → 进程在 ~1s 内干净退出，终端打印 GOODBYE，无残留子进程（含 MCP child，transport 有 `kill_on_drop`）；② cron 任务不再触发（shutdown 已调）；③ Ctrl+C 路径行为不变；④ 容器内点停止能停掉容器（restart 仍拒）。
- [ ] spawn-after-teardown 顺序（zeroclaw `restart.rs` 精华）：启动时 record_launch 记录原始 argv，先优雅 teardown（端口已释放）再拉子进程，替换 ping/sleep 延迟土法并保留启动参数（必要性：**中** / 难度：★★☆）
- [ ] 同 PID 原地 reload（正解，zeroclaw 主力路线）：WebUI 触发进程内信号，原地拆除/重建全部 channel 子系统，PID 不变，终端归属与 Ctrl+C 保留；需要给 QQ WS/Web/cron/MCP 各 channel 做 cancellation token 化改造（必要性：**中** / 难度：★★★）

### P4 / 交互增强

- [ ] `/move` 或 `/cd` 斜杠命令：允许把 CWD 从默认 workspace 移动到用户指定位置，提醒风险并要求确认（扩展 P3-a 的 workspace 边界模型）（必要性：**中** / 难度：★☆☆）

### P4 / 权限管理系统

- [ ] 三档权限 profile：`read-only` / `default` / `yolo`，对齐 opencode 的 plan/build 双模式思路（必要性：**中** / 难度：★★☆）
  - 双维度判定：① 操作是否风险 ② 是否在 workspace 内
  - `read-only`：所有写操作都审批
  - `default`：仅 workspace 外的风险操作审批
  - `yolo`：全放行
  - 审批流程：发消息提示操作内容 + 目录，加 `/ok` `/deny` 斜杠命令，所有频道一致
  - 在 P3-a 的 `confirm_mode`（none/always/session）之上演进，不破坏现有边界
  - 注：与 `/move` 是同一套边界模型的两端（一个放宽范围、一个收紧授权），宜同期设计

### P4 / Provider / Channel 继续扩展

- [x] Google Gemini REST provider（generateContent + functionDeclarations）（必要性：**中** / 难度：★★☆）
- [x] 邮箱 channel：IMAP 轮询 + SMTP（还 P2-e 欠账）（必要性：**中** / 难度：★★☆）
- [x] 飞书 / Lark：事件订阅长连接模式（必要性：**中低** / 难度：★★☆）
- [ ] OpenAI Responses API（缓做，聚合网关可用 OpenAI 兼容协议绕过）（必要性：**低** / 难度：★★☆）
- [ ] Slack Socket Mode / Discord / LINE（必要性：**低**，单用户助理已有 5 个 channel / 难度：★★★）
- [ ] 明确不做：WhatsApp 自实现、微信个人号非官方协议（封号风险，与 ClawBot 官方路线是两回事）

### P4 / 生态复用

- [x] 评估借用 zeroclaw 代码：结论——**值得借鉴、不值得依赖**。许可（Apache-2.0/MIT）允许复制，但引 crate 会拖进 zeroclaw-api/config 依赖树且实现过于全功能（单用户场景可砍 70%）；正确姿势是单文件 vendor + 裁剪适配（dingtalk.rs 仅 554 行可直接移植）。详见 [specs/2026-08-07-provider-channel-expansion.md](specs/2026-08-07-provider-channel-expansion.md)

---

### P4 阶段计划

**执行顺序**：P4-a → P4-b → P4-c → P4-d → P4-e →（P4-f 按需触发）

| 阶段 | 主题 | 包含条目 | 难度 | 排序理由 |
|---|---|---|---|---|
| **P4-a** | 地基与快赢 | 时区 config 化 + 热更新（[ADR-0017](docs/adr/0017-timezone-injection.md) 已决议）；`/api/shutdown` + 停止按钮（[ADR-0018](docs/adr/0018-shutdown.md) 已决议）；上下文注入策略文档化 | ★☆☆ | 全是单点改动，且时区是 P4-c「做梦」的前置依赖——idle 判定和「最近 N 天」语义都要先有可信时间 |
| **P4-b** | 输出正确性 | ✅ 工具调用格式优化（think / `<tool_call>` 泄露）；image 描述模型单独设置 | ★★☆ | 唯一影响「用户直接看到的东西是否正确」的一组，优先级高于任何新功能 |
| **P4-c** | 记忆系统进化 | 「做梦」（[ADR-0016](docs/adr/0016-dream.md) 已决议）；更聪明的上下文压缩 | ★★☆~★★★ | 两者共用同一条「抽取 → 合并 → 压缩」管线，分开做会返工；本阶段是 P4 的核心价值点 |
| **P4-d** | 边界与授权 | 三档权限 profile + `/ok` `/deny`；`/move` `/cd` | ★★☆ | 同一套 workspace 边界模型的一放一收，同期设计才自洽；依赖 P4-a 已落地的稳定运行时 |
| **P4-e** | 生态扩展 | Gemini provider → 邮箱 channel → 飞书 | ★★☆ | 纯增量、互不阻塞，可穿插进任何空档；按实际使用需求拉取，不必一次做完 |
| **P4-f** | 待触发 | 同 PID 原地 reload；spawn-after-teardown；Responses API；Slack / Discord / LINE | ★★☆~★★★ | 当前无强痛点（provider 已热加载，重启需求低频）。等 P4-a 的 `/api/shutdown` 上线后复评：若仍频繁被重启问题咬到，再启动 cancellation token 化改造 |

**阶段间依赖**：

- P4-c「做梦」← P4-a 时区（idle 窗口、日期语义）
- P4-c「做梦」← 已有 cron 调度器（P3-c）、`compress_memory`、`compact_provider`，无需新造轮子
- P4-d 权限 profile ← P3-a 的 `confirm_mode` 与 audit 日志，在其上演进而非重写
- P4-f 同 PID reload ← 各 channel 的 cancellation token 化，是全 P4 最大的一块结构性改造，不进主线

---

## P5 - 未来计划

**状态**：⏳ 计划中

- 环境探测：本地shell、python、node、rust、go等环境探测，根据情况对agent进行提示，优化行为
- skill增强，在现有skill工具基础上针对llaia进行优化，npx skills工具的rust实现，claude的创建skill、hermes的curator等管理skill的元skill的llaia化
- provider接入优化，参考zeroclaw、goose等项目的provider接入，openai兼容格式针对ollama、llamacpp和其它供应商的优化探讨
- 系统提示词优化，言简意赅，占用更少token，参考pi等项目
- session.db会话历史在webUI中可查询和修改，参考astrbot
- webUI中provider api探测可用模型，点击可添加到models中，添加按钮检查可用性，参考astrbot
- 配置中api-key等敏感信息自动写入.env文件中，config只保留环境引用，加强安全，探讨是否使用别的手段避免明文存储敏感信息，比如存储如db二进制？
- 给主agent配置mcp的工具，通过自然对话添加mcp

---

## 工程约定

- 每个 Task 完成后跑 `cargo test` + `cargo clippy`
- 提交节奏：一个完整功能/修复链路验证通过后提交一次，不要每个 Task 都提交
- 遇到编译错误立即修，不要积累
- 详细实现计划放 `docs/plans/YYYY-MM-DD-<feature>.md`，设计规格放 `docs/specs/YYYY-MM-DD-<feature>-design.md`，架构决策放 `docs/adr/NNNN-<topic>.md`
