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

- [ ] 群聊支持（@ 机器人、群消息事件）
- [x] 图片 / 文件消息收发（QQ 收发图/文件、CLI `@path`、`send_image`/`send_file` 工具）—— 见 [specs/2026-07-24-multimedia-design.md](specs/2026-07-24-multimedia-design.md)
- [ ] 主动消息推送（与 P3 cron 一并实现）
- [ ] 邮箱 channel

---

## P3 — 能力扩展与生态接入

**状态**：⏳ 计划中

**目标**：在 P2 已完成的 channel/provider/agent 基础设施上，补齐"能让 agent 真正干活"的能力：QQ 工具边界、初始化引导、定时任务、MCP 工具生态、Skill 技能框架。

**P3 子阶段执行顺序**：P3-a → P3-b → P3-c → P3-d

- P3-a 先做：QQ 能力边界是当前最大痛点（QQ 只能聊天，有副作用的工具全被拒）
- P3-b 紧随：init 引导是新用户入门必需，轻量快赢
- P3-c 中段：cron 是主动能力的基础，依赖 P3-a 的 workspace 模型
- P3-d 后期：MCP client 接入扩展工具生态
- P3-e 最后：Skill 框架建立在 MCP 之上，封装提示词 + 工具集

### P3-a：Agent 能力边界重塑

**状态**：⏳ 计划中

**目标**：把所有 channel 从"主 agent 只能聊天 + 子 agent 全放开"升级为"按 agent 隔离 workspace + 命令拦截"，channel 不再决定工具权限。

**核心思路**：参考 AstrBot 的 workspace 隔离 + 命令拦截路线（详见 ADR-0011），不引入 OS 沙箱（单用户私人助理场景过重）。

- [ ] 目录结构重构：`~/.llaia/` 根只放配置 + 敏感信息，主 agent 工作区移到 `~/.llaia/workspace/`，子 agent 在 `~/.llaia/workspace/subagent/<name>/`
- [ ] workspace 按 agent 隔离：file/terminal 工具只能在自己 workspace 内操作（类似容器挂载目录）；主 agent `file_read` 可读 `subagent/`（整合子 agent 产出），`file_write`/`file_edit` 不可写 `subagent/`
- [ ] 跨 workspace 协作：① 主→子用 delegate 的 `file_paths` 参数复制到子 agent `.inbox/`（每次委派先清空再复制）② 子→主用 delegate 返回值 `{text, output_files}`（output_files 从子 agent 工具调用记录提取 file_write/file_edit 路径）③ USER.md 启动时从主 agent 同步覆盖到子 agent（子 agent memory_write 拒写 USER.md），SOUL.md 各自独立
- [ ] terminal cwd 固定为当前 agent workspace 根
- [ ] terminal 命令拦截（全局，不区分 channel）：`command_policy = blacklist`（默认）/ `whitelist` / `none`；内置黑名单（rm -rf /、sudo、shutdown、reboot、kill -9、dd、mkfs、curl|sh 等）+ 可配白名单
- [ ] terminal 路径防御三层（防 LLM 误操作，不防恶意用户）：① shell 包装拒绝（拦截 `bash -c`/`eval`/`$()`/反引号等任意代码执行）② 路径白名单（canonicalize `starts_with` workspace，含不存在路径回溯祖先）③ 路径黑名单兜底（跨平台危险目录前缀：Linux `/root`/`/usr`/`/etc`、macOS `/System`/`/Library`、Windows `C:\Windows`/`C:\Program Files` 等）
- [ ] file 工具路径校验复用 terminal 的第二三层（canonicalize `starts_with` workspace + 黑名单兜底），不需要 shell 词法解析
- [ ] confirm_mode 重定义为全局开关（不再 per-channel）：`none`（新默认）/ `always` / `session`；`whitelist` 废弃，加载时 warn + fallback 到 `none`
- [ ] 危险动作审计：`~/.llaia/logs/audit.log` 记录所有 `requires_confirm == true` 工具调用（timestamp / agent / channel / tool / args / result）
- [ ] 目录迁移：启动时检测旧结构（SOUL.md/USER.md/MEMORY.md/sessions.db 在 `~/.llaia/` 根），自动迁移到 `workspace/`，写 `.migrated_v0.2` 标记
- [ ] `AgentConfig.workspace` / `soul` / `user` / `memory` 字段废弃（自动推导），加载时 warn

**参考**：[ADR-0011](adr/0011-qq-capability-boundary.md)

### P3-b：llaia init 引导命令

**状态**：⏳ 计划中

**目标**：新用户运行 `llaia init` 后，生成 `~/.llaia/` 目录骨架 + 基础模板，提示进入 WebUI 完成配置。支持"init → serve → WebUI 配置"流程，无 provider 也能启动 serve。

- [ ] `llaia init [--config-dir <path>] [--force]` 子命令：创建 `~/.llaia/`、`logs/`、`workspace/`（含 `uploads/`、`subagent/` 空目录）
- [ ] 生成 `config.toml` 默认模板（CLI enabled、QQ/Web disabled、provider/agent 注释占位）
- [ ] 生成 `~/.llaia/workspace/SOUL.md` / `USER.md` / `MEMORY.md` 默认模板（内嵌常量）
- [ ] 终端输出引导：提示运行 `llaia serve` 后浏览器访问 WebUI 完成 provider/agent/channel 配置
- [ ] 幂等：已存在的文件不覆盖，只创建缺失项
- [ ] **无 provider 启动支持**：`llaia serve` 无 provider 时 warn 但正常启动，WebUI 配置功能可用，聊天功能降级（返回提示而非报错）；`llaia chat` 无 provider 报错退出并引导
- [ ] **provider 热加载**：WebUI `PUT /api/config` 保存后触发 `Agent::reload_provider()`，无需重启 serve；正在进行的 turn 用旧 provider 完成后切换；失败回滚
- [ ] **doctor 检查项**：provider 配置检查（无则 warn）+ sessions.db 存在性检查（无则 warn）

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

**状态**：⏳ 计划中

**目标**：作为 MCP client 消费外部 MCP server 提供的工具，扩展 LLAIA 的工具生态（不作为 MCP server 暴露自身能力）。

- [ ] MCP client 实现：支持 stdio transport（启动子进程）和 HTTP transport（远程 MCP server）
- [ ] 配置：`[mcp.<id>]` section，含 `command` / `args` / `env`（stdio）或 `url` / `headers`（HTTP）
- [ ] 工具适配：MCP `tools/list` 返回的工具，通过 adapter 包装成 LLAIA `Tool` trait 实现，注册到主 agent
- [ ] 工具调用：MCP `tools/call` 协议，结果转成 LLAIA 工具返回格式
- [ ] 启动时连接：进程启动时初始化所有配置的 MCP server，失败的不阻塞启动（log + 跳过）
- [ ] WebUI 配置：配置面板加 MCP tab，可视化增删 MCP server
- [ ] 安全：MCP 工具默认走 QQ confirm_mode 策略（受 P3-a 边界约束）

**参考**：[ADR-0014](adr/0014-mcp-client.md)

### P3-e：Skill 技能框架

**状态**：⏳ 计划中（依赖 P3-d 完成）

**目标**：在 MCP 工具之上封装"提示词 + 工具集"的技能包，让用户可以快速给 agent 加能力。对齐 OpenAI Codex CLI / Anthropic Claude Skills / AstrBot 的业界标准 SKILL.md 格式。

- [ ] Skill 定义：`~/.llaia/skills/<name>/SKILL.md`（markdown + YAML frontmatter，对齐业界标准），frontmatter 含 `name` / `description` / `duration`（turn / session，默认 turn）/ `tools`（可选，提示 LLM 推荐用的工具列表，不实际控制挂载）
- [ ] Progressive Disclosure：启动时扫描 `~/.llaia/skills/*/SKILL.md`，解析 frontmatter 拿 `name` + `description`，在 system prompt 追加"## Skills"段列出所有 active skill 的 name + description + SKILL.md 路径；规则提示 LLM "用 skill 前必须先 file_read 它的 SKILL.md"
- [ ] 触发机制：agent 判断为主（LLM 看 name+description 自行决定），用户显式提到 skill 名也算触发（LLM 自然语言理解，无特殊语法）。不做关键词匹配
- [ ] 工具挂载：方案 C — skill 的 `tools` 字段只是 prompt 提示，不实际控制工具挂载。内置工具始终全挂载，MCP 工具按 server 挂载（与 skill 解耦）
- [ ] active 开关：`~/.llaia/skills.json` 控制每个 skill 是否激活（类似 AstrBot）
- [ ] WebUI 管理：配置面板加 Skill tab，可视化增删改查 + SKILL.md 编辑器
- [ ] 内置示例 Skill：todoist（提醒）、news-digest（新闻摘要）、code-review（代码审查）
- [ ] 路径安全：skill name / path 注入到 prompt 时过滤危险字符（借鉴 AstrBot `_SAFE_PATH_RE`），防 prompt injection

**参考**：[ADR-0015](adr/0015-skill-framework.md)

---

## P3+ — 交互增强（待规划）

**状态**：⏳ 计划中（P3 主线完成后的增量项）

- [ ] `/provider` 斜杠命令：`/provider` 列出可用 provider/模型（当前标记 `*`）、`/provider <序号>` 运行时切换、`/provider <id>.<alias>` 按全名切换（参考 AstrBot，不写 config.toml）
- [ ] 主动消息推送（与 P3-c cron 一并实现）

---

## 工程约定

- 每个 Task 完成后跑 `cargo test` + `cargo clippy`
- 提交节奏：一个完整功能/修复链路验证通过后提交一次，不要每个 Task 都提交
- 遇到编译错误立即修，不要积累
- 详细实现计划放 `docs/plans/YYYY-MM-DD-<feature>.md`，设计规格放 `docs/specs/YYYY-MM-DD-<feature>-design.md`，架构决策放 `docs/adr/NNNN-<topic>.md`
