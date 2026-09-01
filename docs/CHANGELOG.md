# LLAIA Changelog

> This file is the **delivery archive** for each phase (the ✅-marked history formerly in `plan.md`).
> The forward-looking roadmap and next steps live in [`plan.md`](plan.md).
> Per-phase implementation plans are in [`plans/`](plans/), design specs in [`specs/`](specs/), and architecture decisions in [`adr/`](adr/).

---

## v0.3.2 (unreleased)

**Bug fixes / 稳定性**
- **provider**：llama.cpp / Ollama 思考模型会把大段思考内容原样吐给用户（实测于 QQ 频道）——`Compat::ollama()` / `Compat::llamacpp()` 预设的 `reasoning_to_content` 默认改为 `false`：这两个端点的 OpenAI 兼容层在思考模型下 `content` 照常返回正式回答，`reasoning_content` 只是额外思考流，折回只会把思考混进可见文本、context 与 sqlite。此前行为还自相矛盾——内联带 `<think>` 标签的思考被 `ToolCallStreamParser` 剥掉，拆到 `reasoning_content` 字段的反而被折回显示。确实需要折回的端点仍可显式 `[provider.<id>.compat] reasoning_to_content = true`（[ADR-0026](adr/0026-provider-compat.md) 修订记录）
- **provider**：一并删除 per-model 表 `model_folds_reasoning`——它对 `deepseek-reasoner` / `deepseek-r1` / `deepseek-reasoning` / `kimi-k` 强制开启同一字段，导致线上推理模型同样吐思考。该规则移植自 nanobot，但原意是「R1 走 `reasoning_content` 字段名而非 `reasoning`」，而 llaia 流解析本就同时读 `reasoning_content` 与 `thinking`、从不读 `reasoning`，故其在 llaia 内无字段选择作用，只剩强制折回的副作用。per-model 表现在只剩 `max_tokens_field`
- **provider**：SSE 事件分隔符只认 `\n\n`，CRLF（`\r\n\r\n`）服务端/反代的响应永不分割 → 整条回复静默丢失；三个 provider 统一归一化分隔符
- **provider**：IP 型 llama.cpp 端点（如 `http://10.0.11.187:8080/v1`）未命中 compat 预设 → 流式 usage 不上报、`/stats` 抓不到 token 数据
- **agent**：`cheap_normalize` 丢弃空文本 `assistant(tool_calls)` 消息会产生孤儿 tool 消息（违反 OpenAI 协议，严格端点直接 400）；`StreamEvent::Error` 路径丢失错误前已生成的部分输出
- **qq**：`get_ws_url` 识别 11244 / 中文过期消息体，修复 token 失效后的重连死循环
- **cron**：agent 模式加交付门——白跑的一轮（无实质产出）不再当作成功推送；`cron_task update` 改为局部 patch，改时间不再牵连 prompt
- **memory**：`memory_write` 追加前补换行并折叠多行 entry，避免写出不可解析的 MEMORY.md 行
- **web**：新增 provider 填 api key 后立即生效，不再误报 `environment variable referenced but not set`（WebUI 保存时序问题，key 曾被压成空串、热加载失效）
- **webui**：`checkUpdate` 缺闭合花括号导致 app.js 语法错误、整个 UI 无法启动
- **webui**：恢复 pane 高度链，sessions / config 左侧栏不再随内容滚走；会话详情区右下角加「跳顶部/跳底部」悬浮按钮（plan.md W1）
- **terminal / path-guard**：字面量 `\n` 等含反斜杠片段不再被误判为越界路径；裸 `/` 等命令路径元字符同样不再误判
- **approval**：verbatim 前缀导致 moved 目录内操作被误审；`/move` 批准后不再注入消息触发模型续跑
- **stats**：`/stats` 迁移 `context_size_now()`，窗口展示跟随当前模型（切换 provider 后不再显示旧模型的阈值与占比）
- **skill_edit**：patch 兼容模型二次序列化（字符串含 `{find,replace}` 对象时按替换处理）；成功消息明示操作类型（追加/替换/整篇覆盖），无需模型读回文件分辨

**Features**
- **approval**：/move 受信目录模型（plan.md #B）——批准过的目录登记为会话级受信集合，与 workspace_root 同等参与「是否在 workspace 内」判定：`/move home` 切走再回来、或切换期间触碰受信目录内的路径均免审批，逃出全部范围的仍需审批。受信目录仅存内存（重启清空），只能经 `validate_move_target`（canonicalize + 黑名单）进入
- **session**：压缩时自动生成会话标题，落 `sessions.title` 并在 WebUI 列表展示——仅标题为空时生成一次，失败降级为首条用户消息截断（plan.md「会话主题自动总结」）
- **memory**：新增 `memory_research` 工具——基于 FTS5 的跨会话历史消息全文搜索，只读无需审批，结果上限 20 条（plan.md #5）
- **token**：token 用量统计链路打通（plan.md W3）——新增 `turn_usage` 表按回合累计输入/输出 token 并区分 sidecar 调用（compact / vision / reminder）；新增 `GET /api/stats/tokens` 聚合 API 与 WebUI 顶层 Stats tab（范围选择 + 汇总卡 + 每日柱状图 + per-model / per-session 排名，纯 SVG/CSS 手绘、零图表依赖）。已知限制：LMStudio / bare 端点默认不上报 usage，该 provider 无数据是预期行为
- **webui**：About 页更新检查按钮 + `GET /api/update/check`（比对 GitHub Releases latest，结果分钟级缓存）（plan.md W2）
- **provider**：`/provider <n|id.alias>` 默认持久化到 `config.toml`（`toml_edit` 定点改 `[agent.<alias>].model`，保住注释），`--temp` 保持纯内存临时切换；`context_size` 与 Agent 解耦为懒解析 + 缓存，reload 后失效（plan.md #E / #F）
- **provider**：`native_tool_calling` 并入 `Compat` 自动探测，配置改 `Option<bool>`（`None=auto` 跟随探测），用户无需手设（plan.md #10）
- **move**：`/move` 到外部目录时把该目录的 `AGENTS.md` 加载进系统提示词
- **tmp**：serve / chat 启动时清理 `workspace/tmp` 下 3 天前的文件，防止工具图片无界增长
- **skill**：skill 目录只读边界——`file_read` 放行全部配套文件、terminal 读/执行放行（[ADR-0028](adr/0028-skill-dir-read-boundary.md)）
- **channel**：微信 / 钉钉 / 飞书 / Telegram 的工具调用通知收敛为 QQ 紧凑模式（每回合一条「🔧 正在调用工具...」，结束后把去重工具名拼进回复开头）
- **slash / i18n**：斜杠命令大小写不敏感；审批提示支持裸 `/ok`；运行时提示统一为英文
- **docs**：`AGENTS.md` 与 `guide/tools.md` 工具清单补齐 `skill_create` / `skill_edit`（ADR-0027 落地时遗漏）

**Performance**
- **provider**：`detect_context_size` 按 host 门控——仅本地后端（localhost / `.local` / 回环 / 私网 / 链路本地）才打 `/props`、`/api/tags`，云 provider 直接跳过（plan.md #4 残余）
- **startup**：`build_agent` 四阶段独立计时打点（mcp connect / skills / sub agents / main agent）；`McpRegistry::connect_all` 改 `join_all` 并发握手，多 server 时不再串行累加 30s 超时（plan.md 启动优化②③）

**Refactor**
- **skill_edit**：patch union 分派改为对齐 `file_edit` 的扁平三模式

**Breaking / 架构简化**
- **memory**：整体移除「做梦」（dream）记忆自动整理机制（[ADR-0030](adr/0030-remove-dream.md)，取代 [ADR-0016](adr/0016-dream.md)）：harness 层不再有无人值守自动改写 `MEMORY.md` 的路径；`MEMORY.md` 变更只剩 `memory_write` / `/remember` 确定性写入与 `/memory-compact` 手动压缩。同步删除 `/dream`、`/dream-rollback` 斜杠命令、`CronTask` 的 `kind` / `idle_minutes` 字段与内置任务播种、sqlite dream 游标、agent 侧 `run_isolated_turn(_with)` 专用机制；`write_memory_atomic` 迁至 `src/memory/mod.rs` 供 `memory_write` 继续使用。替代做法：自建普通 cron 任务（prompt + `memory_research` 检索历史、产物推送人工审阅）。存量数据无需迁移（`dream_draft.md` / `MEMORY.backups/` 可手动清理；cron.toml 中如有 `kind = "dream"` 任务段请手删）。
- **goal**：整体撤销 `/goal` 长期目标系统（[ADR-0021](adr/0021-goal-system.md) 已撤销，−676 行）：移除 `/goal` 斜杠命令、goal 存储与注入逻辑，以及系统提示词中的目标段落。v0.3.0 引入的目标跟踪未达预期，记忆整理改由 memory 侧确定性写入与手动压缩承担

---

## v0.3.1 (2026-08-24)

**Milestone**: P6 稳定性修复 + 快赢项。源自 `docs/issues/issues.md` 的 9 个问题评估（详见 [plan.md](plan.md) P6 节）。

**Bug fixes / 稳定性**
- **provider**：`/provider N` 切换后 fallback 降级链被裸替换丢弃——改走 `build_provider_chain` 重建链，新增 `Provider::kind()` 标识 + 3 个回归测试
- **agent**：`estimate_tokens` 漏算 Runtime Context（todo/goal/env）与每条消息结构开销，`/stats` 显示显著低于真实发送量——改为基于 `to_messages` 全量文本 + tool defs 序列化估算
- **qq**：发图失败 `40093006/40093007`——腾讯 v2 富媒体接口已改为 JSON body（`file_type`+`file_data`(base64)+`srv_send_msg`），不再支持 multipart 上传；已用真实凭据实测验证
- **provider**：context_size 探测恒失败落默认 8192（128k 模型显示 127% used）——llama.cpp `/props`、Ollama `/api/*` 挂在服务根路径而 `base_url` 以 `/v1` 结尾，探测 URL 打到 `/v1/props` 404；`probe_base()` 剥掉 `/v1` 后缀，mockito 回归测试覆盖
- **web**：WebUI Config 页模型探测按钮首次探测后全部失灵（需刷新页面）——`probeModels` 同时是 data 属性（探测结果 map）与方法名，方法体内 `this.probeModels = {...}` 把方法覆盖成普通对象，后续点击抛 `probeModels is not a function`；探测方法改名 `runProbe` 消除同名冲突，新增 app.js 顶层键去重回归测试兜住此类问题

**Features**
- **agent**：自动 Tail Reminder（抗长会话风格漂移）：回合起点对 SOUL+USER 求 md5，与 `workspace/reminder.md` 失配时后台隔离 turn 让 LLM 提炼 ≤120 token 行为指令，下一轮作为请求最后一条消息注入（离生成点最近）；MEMORY/skills 不参与 hash；失败静默降级
- **slash**：`/reasoning on|off` 会话级深度思考开关（对支持 `chat_template_kwargs` 的端点生效）
- **slash**：`/stats` 工具列表改为分组计数（`N builtin + M mcp (server: n, ...)`）
- **web**：聊天页 ENV 只读面板（`GET /api/env` / `POST /api/env/refresh`）
- **web**：Config 页 Doctor section（`GET /api/doctor` → `commands::doctor_checks`：provider 连通性 / 主模型链 / context_size 探测 / `.env` 权限 / sessions.db / cron/mcp / skills，ok/warn/error 分色）
- **qq**：工具通知收敛为每回合一条「🔧 正在调用工具...」，结束后把去重工具名拼进回复开头（原每个 ToolStart 刷一条消息）

---

## v0.3.0 (2026-08-21)

**Milestone**: P5 全部交付（P5-1~P5-7 + 剩余项 E1/W1/W2/S1/T1/M1），叠加 v0.2.1 后的稳定性修复。workspace 版本号由 0.2.2 直升 0.3.0（P5 为里程碑级发布，跳过一个 patch）。

**P5 交付总览**（逐条详情见下方 P5 分节）
- Provider Compat 层（ADR-0026）、system prompt MEMORY 预算（ADR-0025）、统一 search（ADR-0023）、规划后执行 todo（ADR-0024）、ask_user 阻塞澄清（ADR-0022）、Skill 自管（ADR-0027）、长期目标 /goal（ADR-0021）
- 剩余项：envprobe 环境探测、WebUI 会话历史 + 模型探测、敏感信息 .env 自动化、TTS、MCP 现状收尾

**Bug fixes / 稳定性**
- **terminal (Windows)**：改走 Git Bash `bash -s` stdin 执行（白名单路径 + PATH 探测、排除 WSL 假 bash、`$MSYSTEM` 校验），绕开 MSVCRT argv 转义层——修复双引号被破坏成 `\"` 字面量、bash 风格 `;` 链 / `$VAR` 不展开、中文 GBK 乱码；无 Git Bash 时回退 `cmd` + `raw_arg`（`304469d`）
- **agent**：工具返回 base64 图片（如 blender-mcp 截图）剥离为 `[图片]` 占位 + 缩放重编码（1024/JPEG85）落盘回显 + 多模态桥接 / vision 描述，超大非图片结果按 `tool_result_cap` 截断（`b3a791e`）
- **agent**：工具循环内紧凑上下文，避免单回合工具链把上下文撑爆（`c6da3fd`）
- **cron**：会话复用 / 全局锁冻结 / 时区漂移修复（`4bbdaa6`）；禁深度思考 + 隔离 turn 修复超时（`e3102d3`）；dream 合并会话、纯化分发（`6aa0a09`）
- **cron 模板**：morning_news 示例 prompt 加"≤3 来源 / 不重复抓同一 URL / 抓完立即总结"约束，并提示调小 web_fetch max_chars（`2488225`）
- **web**：tool-result 折叠、MCP/channel 卡片化、config 保存保注释、模型下拉实时反映（`bdda4fa` `630b4df` `8a251f3` `2f1eb8d` 等）
- **skill**：递归扫描子目录（`833117f`）；**ci**：修复存量 fmt/clippy（`0b7b1a9` `91028cf`）

**注意（破坏性）**
- cron 任务引用旧工具名 `tavily_search` 需改为 `search`（P5-3）
- 废弃字段 `agent.workspace` 等从 schema 移除（`6a19a1e`）

---

## P5 — 已完成

> P5 各条目按 `plan.md` 推荐顺序逐条交付，每完成一条在此累加勾选清单。P5-1 ~ P5-7 + 剩余项（E1/W1/W2/S1/T1/M1）全部交付。

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

### ✅ P5-3 统一搜索 search（[ADR-0023](adr/0023-unified-search.md)）

原 `tavily_search` 单一 provider 工具收敛为统一的 `search` 工具，内部按 `[tools.search].provider` 选定**单一** provider（tavily / baidu / brave）执行，不串试、不聚合（经 zeroclaw / nanobot 复核后的决策）。各搜索源内置实现、归一化为 `SearchResult`，零额外进程。

**新增**
- `src/tools/search/mod.rs`：`SearchProvider` trait（`search(query, top_k)`）+ `SearchResult { title, url, snippet }` 归一化结构；`UnifiedSearch` 工具（对 agent 只暴露 `search(query, top_k?)`，名称 `search`）；`UnifiedSearch::build(&ToolsConfig)` 按 `provider` 选定、key 缺失/未知则不注册该工具（条件注册，与老 tavily 行为一致）。
- `src/tools/search/tavily.rs`：从原 `src/tools/tavily.rs` 迁移为 `TavilyProvider`（老配置 `[tools.tavily].api_key` 仍生效）。
- `src/tools/search/baidu.rs`：`BaiduProvider` — 百度千帆 AI Search（`POST /v2/ai_search/web_search`，Bearer token，响应 `references[]`）。
- `src/tools/search/brave.rs`：`BraveProvider` — Brave Search API（`GET /res/v1/web/search`，`X-Subscription-Token` 头，响应 `web.results[]`）。
- `src/tools/search/mod.rs` 单元测试：结果格式化、top_k 覆盖、空结果占位、schema 结构。

**修改**
- `src/config.rs`：新增 `SearchConfig { provider, top_k }` + `BaiduConfig` / `BraveConfig`；`expand()` 处理各家 key 的 `${VAR}`。
- `src/channels/cli.rs`：用 `UnifiedSearch::build` 替换原 `TavilySearch` 注册（全频道统一生效）；删除 `src/tools/tavily.rs`。
- `src/web/mod.rs`：`mask_sensitive` / `merge_masked` 同步掩码 baidu / brave key + 测试。
- `src/commands/mod.rs`：`init` 模板加 `[tools.search]` 段 + baidu/brave key 段与 `.env` 模板；cron 示例 `tavily_search` → `search`。
- `src/skill/loader.rs` 示例 skill、`src/tools/cron.rs` schema/测试：`tavily_search` → `search`。
- 文档：`guide/tools.md` / `glossary.md` / `configuration.md` / `adr/0006` / `adr/0011` / `adr/0013` / `adr/0015` / `AGENTS.md` 工具清单与配置段同步更新。

**未做（已知范围）**
- **doubao（豆包）provider 暂未实现**：其公开接入只有 MCP/Skill 或 Volcengine SigV4 SDK（access_key+secret_key），没有干净的"单 api_key REST"端点，且 ADR 明确"内置而非 MCP"；手搓 SigV4 不可测、风险高。选定 `provider = "doubao"` 时给清晰报错。后续需要可单独补（届时补 `DoubaoConfig`）。
- 破坏性变更：cron 任务里若引用旧工具名 `tavily_search`，需改为 `search`。

**验证**
- `cargo clippy --lib --all-targets` 与 `cargo test --lib`（search/cron/web/config 相关 55 项）通过。
- 注：`provider/openai_compat` 的 mockito 测试在沙箱内无法绑定本地端口，属环境问题，与本次改动无关。

---

### ✅ P5-4 规划后执行 todo（[ADR-0024](adr/0024-planning-todo.md)）

内置轻量 `todo` 工具（非外包 todoist/MCP），让 agent 对**非平凡任务**先拆步骤清单、逐步推进，清单每轮注入 Runtime Context，复杂任务执行更可靠。

**新增**
- `src/tools/todo.rs`：
  - `TodoStore`：按 `session_uuid` 分桶的共享状态（agent 与工具共享同一实例）；in-memory + 落盘 `workspace/todos/<session_uuid>.json`（首次访问懒加载）；`set_current_session(uuid)` 由 agent 每轮写入，后续 todo 操作据此路由；`current_list_text()` 供 Runtime Context 注入。
  - `TodoTool`：单一 `todo` 工具，`action` ∈ `add` / `list` / `update` / `done`（`update` 带 `status: pending|in_progress|done`）；`requires_confirm = false`（廉价工作记忆，低副作用）；无条件注册（无需 api_key）。
  - 单元测试：id 自增、done/update 状态、session 隔离、无 session 报错、清单渲染、`TodoTool` 工具调用 roundtrip。

**修改**
- `src/agent/runner.rs` `ToolRegistry`：新增 `todo_store: Arc<TodoStore>` 字段（`new()` 默认禁用态），作为 agent 与工具的共享挂载点（不改动 `Agent::new` 签名）。
- `src/channels/cli.rs` `build_single_agent`：创建真实（带 workspace 落盘）`TodoStore`，构造 `TodoTool` 注册，并替换 registry 默认禁用态。
- `src/agent/mod.rs` `handle_message_streaming`：每轮起点用 `session_store.session_uuid(session_id)` 解析当前 uuid，写入 `todo_store.current_session`，并把 `todo_store.current_list_text()` 写入 `Context.todo_state`。
- `src/agent/context.rs` `Context`：新增 `todo_state: Option<String>`，`to_messages` 在尾部（与 status_bar 同 Runtime Context 区）追加，system 前缀保持稳定（KV 缓存友好）。
- `src/memory/sqlite.rs` `SessionStore`：新增 `session_uuid(session_id)` 反查。
- `src/web/mod.rs`：新增 `GET /api/todos`（只读返回当前会话清单）；`src/web/static/{app.js,index.html,theme.css}`：聊天页底部只读 todo 面板（5s 轮询）。
- 文档：`AGENTS.md` / `guide/tools.md` / `glossary.md` / `adr/0006` 工具清单同步；`ADR-0024` 补"实现补记"。

**设计偏离（已在 ADR-0024 记录）**
- 工具形态采用**单一 `todo` 工具 + `action` 分发**，而非 ADR 草稿里列的 4 个独立工具名（`todo_add` 等）——复用 P5-3 `search` 的"单工具 + action"模式，与 `cron` 工具约定一致。

**验证**
- `cargo clippy --lib --all-targets` 与 `cargo test --lib`（todo 相关 + 全量回归）通过。
- 注：`provider/openai_compat` 的 mockito 测试在沙箱内无法绑定本地端口，属环境问题，与本次改动无关。

---

### ✅ P5-5 ask_user 阻塞式澄清（[ADR-0022](adr/0022-ask-user-suspend-resume.md)）

复用 ADR-0020 的 ApprovalGate 挂起-回传机制，新增 `ask_user` 工具：agent 在执行中主动向用户抛问题、**阻塞等待回答、再继续**，复杂/歧义任务不再"猜着做"。

**新增**
- `src/tools/ask_user.rs`：
  - `AskUserTool`（`name="ask_user"`，schema：`question` 必填 + 可选 `choices` 结构化单选）；`ASK_USER_TOOL_NAME` 常量；`parse_ask_user_args` 解析 helper（含单测）。工具无条件注册（无需 api_key）。
  - 实际挂起由 agent 循环处理，`execute` 仅为完整接口占位（正常路径不触发）。
- `src/agent/approval.rs`：
  - `PendingKind::{Approval, Question}` 两型；`PendingApproval` 增加 `kind/question/choices/created_at/timeout_secs`。
  - 新增 `register_question` / `take_question` / `questions` / `single_question` / `is_question_expired`；`feishu` 补入 `is_interactive_channel` 白名单。

**修改**
- `src/agent/runner.rs` `execute_tool_calls`：在审批判定前按工具名拦截 `ask_user`——交互频道注册 pending question + 占位结果 + `deferred`（turn 软暂停）；非交互频道（mail/cron）直接返回"按最合理假设继续"。`ApprovalContext` 增加 `ask_user_timeout_secs`（取自 `[runtime].ask_user_timeout_secs`）。
- `src/agent/mod.rs` `handle_input_streaming`：集中检测单 pending question，把用户下一条普通消息包装为答案跑 continuation turn（所有频道自动受益）；`map_ask_user_choice` 把 `choices` 的序号/原文映射回选项；超时则丢弃 pending 并注入超时说明。
- `src/commands/slash.rs`：新增 `/answer <id> <text>`（显式消歧+回答，走 Resume）与 `/cancel <id>`（取消 question 或 approval）；help 文本同步。
- `src/config.rs` `RuntimeConfig`：新增 `ask_user_timeout_secs`（默认 300，对齐 zeroclaw）。
- `src/web/mod.rs`：新增 `GET /api/questions`；`src/web/static/{app.js,index.html}`：聊天页底部只读 pending 问题面板（5s 轮询）。

**设计说明（详见 ADR-0022「实现补记」）**
- 复用 ApprovalGate 而非另造；审批 `/ok` `/deny` 与提问续答共用注册表与 resume 路径。
- 纯静默超时自动续跑（用户始终不回复）为已知限制：超时字段与判定就绪，仅在下一条消息到达时一并判定；后续可加轻量后台巡检实现全自动续跑。

**验证**
- `cargo fmt --all --check` / `cargo clippy --lib --all-targets` / `cargo test --lib`（ask_user 解析 + 全量回归）通过。

---

### ✅ P5-6 Skill 自管（[ADR-0027](adr/0027-skill-authoring.md)）

agent 通过 `skill_create` / `skill_edit` 工具直接写/改 SKILL.md（skill 目录落在 workspace 之外，file_write 够不到），路径安全校验；加内置 `skill-authoring` 元 skill 引导方法论。不做 npx 式搜索 / 自动安装（用户保留甄选权）。

**新增**
- `src/tools/skill_create.rs`：`SkillCreateTool`（`name="skill_create"`），参数 `name` / `description` / `content` / `scope?`。
  - `name` 经 `is_valid_skill_name` 校验（kebab-case，无 `/` 与 `..`）；`scope` 仅允许 `user`（默认，`<config_dir>/skills`）/ `project`（`<workspace>/.workbuddy/skills`）。
  - `content` 为 skill body（markdown）：自带 `---` frontmatter 则原样采用，否则自动生成 `name`/`description`/`duration: turn` frontmatter。
  - 写盘前经 `ensure_within_skills_dir` 词法防穿越兜底，`validate_skill_md` 校验；已存在则拒绝覆盖（改请用 `skill_edit`）。
  - `requires_confirm = true`（写 skills 目录属有副作用操作，走审批门）。
- `src/tools/skill_edit.rs`：`SkillEditTool`（`name="skill_edit"`），参数 `name` / `content | patch` / `scope?`。
  - `content`：整文件替换；`patch`：字符串（追加到 body，保留 frontmatter）或对象 `{find, replace}`（单次精确替换，找不到报错）。
  - 已存在校验 + 原子写（临时文件 + rename，避免半写损坏）+ `validate_skill_md`。`requires_confirm = true`。
- `resolve_skills_dir(config_dir, workspace, scope)` 共享的 scope → skills 目录解析（两工具复用）。
- `src/skill/loader.rs`：`validate_skill_md` 补长度约束（name ≤ 64、description ≤ 1024，对齐 pi）；新增 `META_SKILL_AUTHORING` 内置元 skill 常量 + `ensure_builtin_meta_skills()`，在 `load_skills` 中幂等确保（不覆盖用户改动），老用户首次启动即拿到元 skill。
- 内置元 skill `skill-authoring`：引导 agent 何时该建 skill（可复用、跨会话、非平凡）vs 直接做；`skill_create`/`skill_edit` 用法；frontmatter 约束（name kebab-case、description ≤1024 必填、progressive disclosure）；路径安全（用专用工具而非 file_write）；`validate_skill_md` 规则；审查/整理已有 skill。

**修改**
- `src/tools/mod.rs`：注册 `skill_create` / `skill_edit` 模块。
- `src/channels/cli.rs` `build_single_agent`：两工具**仅 main agent** 注册（scope 解析需要 `config_dir` + `workspace`）；受 `denied_tools` 过滤。
- 文档：`docs/guide/skills.md` 补「agent 自管（skill_create / skill_edit）」段；`plan.md` 标记 P5-6 ✅；本文件补本节。

**已知范围（非缺陷）**
- 项目级 skill（`scope="project"`）写出后**不**自动注入 system prompt——当前仅用户级 skills 目录参与 Progressive Disclosure 扫描（`cli.rs` 只 `load_skills(config_dir/skills)`）；需重启 + 不在 prompt 注入范围内。用户级 skill 创建后即被扫描加载。
- 不引入引擎级 skill import / npx 式自动安装（ADR-0027 决策 #1、#5）。

**验证**
- `cargo fmt --all --check` / `cargo clippy --lib --all-targets` / `cargo test --lib`（367 项，含两工具单测：路径越权被拒、name 校验、scope 两值、user/project 落盘、覆盖拒绝、patch 追加/查找替换、校验失败不改坏原文件）通过。

---

### ⚠️ P5-7 长期目标 /goal（[ADR-0021](adr/0021-goal-system.md) 文件方案修订）— **已于 2026-08-31 撤销**

> **撤销（2026-08-31，随 v0.3.2 移除）**：交付后从未产生真实使用，且结构性问题大于收益——单活跃目标存放在 agent 家目录一份文件、跨 session 跨全部频道生效，而收尾完全依赖用户手敲 `/goal-done` 或 agent 自觉调工具，无过期机制，目标一旦作废即永久污染每轮上下文；长期意图本属 MEMORY.md（system prompt 常驻），会话内拆解本属 `todo`，属第四套平行机制。已删除：`src/goal/`、`goal` 工具、`/goal` `/goal-list` `/goal-done` `/goal-cancel`、`Context.goal_state` 注入、`GET /api/goal` 与 WebUI GOAL 面板。理由与后续重立项条件见 [ADR-0021 撤销节](adr/0021-goal-system.md)。**下方保留为原交付记录。**

把跨 session 持续推进的「长期目标」持久化为 `goal.md` 文件（**不进 `sessions` schema**，零迁移），每轮从文件重新注入 Runtime Context。

**设计取舍（ADR-0021 修订 2026-08-17）**
- 原方案「单活跃 goal 存 session metadata」被推翻：goal 是跨会话语义，绑单场会话既名实不符又需改 schema。改文件方案后，**零 schema、零迁移、零回滚风险**，省 token（不进会话历史）、人可读可手改可 git 跟踪。
- 落盘位置：`<config_dir>/workspace/goal.md`（默认 `~/.llaia/workspace/goal.md`），与 SOUL/USER/MEMORY 同处 agent 家目录；对 `file_write` 等工具不可见（家目录不进工具作用域），由专用 `/goal` 命令 + `goal` 工具管理。
- 仅 v1 workspace（用户级）范围；用户级全局 goal 留作以后扩展。

**新增**
- `src/goal/mod.rs`：`GoalStatus`(active/done/cancelled) / `GoalState`，`goal.md` 读写与解析（frontmatter + `# Goal` / `## Progress` 正文拆分，`status` 不可识别则整体视为无目标）。函数：`read_goal` / `read_active_goal_line` / `set_goal` / `update_status` / `update_progress` / `goal_runtime_lines`（仅 active 返回 `Goal (active): <objective> / Summary: <progress>`）。原子写（临时文件 + rename）。
- `src/tools/goal.rs`：`GoalTool`（`name="goal"`），动作 `done` / `cancel` / `progress <text>` / `set <text>`；供 agent 在执行中回写进度、判定达成后标记 done（ADR-0021 决策 #6 路径①）。`requires_confirm = false`。
- `src/commands/slash.rs`：`/goal <text>`（设定/重置，置 active）、`/goal-list`（只读展示状态+进度）、`/goal-done`、`/goal-cancel`；`/help` 同步。
- `src/web/mod.rs` + 前端：`GET /api/goal`（只读展示 goal.md 状态，无目标返回 `{goal:null}`）+ WebUI `GOAL` 只读面板（5s 轮询，复用 todo 面板样式，active 蓝）。

**修改**
- `src/agent/context.rs`：`Context` 新增 `goal_state: Option<String>`，`to_messages` 尾部（status_bar 之后、todo 之后）注入 active goal。
- `src/agent/mod.rs` `handle_message_streaming`：每轮 turn 起点从 `self.workspace`（家目录）`read_active_goal_line` 现读现注。
- `src/channels/cli.rs` `build_single_agent`：仅 main agent 注册 `GoalTool`（落盘需家目录）。
- `src/lib.rs`：新增 `pub mod goal;`。
- 文档：`docs/plan.md` 标记 P5-7 ✅；`docs/adr/0021`、`docs/plans/2026-08-14-goal.md`、`docs/guide/faq.md` 已在设计阶段同步修订（文件方案）。

**验证**
- `cargo fmt --all --check` / `cargo clippy --lib --all-targets` / `cargo test --lib`（380 项，含 goal 解析/读写/状态切换/roundtrip 单测、Context 注入单测、工具 action 单测）全绿；`cargo build` 通过。

---

### ✅ P5 剩余项（E1 环境探测 / W1-W2 WebUI 增强 / S1 敏感信息 / T1 TTS / M1 收尾）

对 `plan.md` P5 中未勾选的 6 个条目做现状核实、设计评审后全部交付；评估与设计见 [plans/2026-08-17-p5-remaining.md](plans/2026-08-17-p5-remaining.md)。

**E1 环境探测**（★☆☆）
- `src/envprobe.rs`：启动时对 main agent 探测本机工具链（shell/python/node/npm/rustc/cargo/go/git/docker，2s/命令 timeout，只列存在且版本可解析的项），以 Runtime Context 尾部注入（`Context.env_state`，与 todo/goal 同区、KV 缓存友好）；`/env` 斜杠命令手动刷新；子 agent 不探测（避免启动开销）。

**W1 WebUI 会话历史**（★★★）
- `SessionStore` 新增 `list_sessions`（含消息数）/ `session_by_uuid` / `messages_with_tool_calls`（含 tool_calls 明细）/ `delete_session`（cascade）。
- API：`GET /api/sessions`、`GET|DELETE /api/sessions/:uuid`、`GET /api/sessions/:uuid/export`；`authorize` 泛型化为 `TokenProvider` trait（复用 SessionListQuery）。
- WebUI 新增 Sessions tab：会话列表 + 消息详情（role 徽标、tool_calls 折叠、时间戳）+ 删除（二次确认）+ 导出 JSON。只读 v1；编辑 v2（仅落 sqlite、不同步内存 Context）留待后续。

**W2 WebUI 模型探测**（★★☆）
- `src/provider/probe.rs`：OpenAI 兼容端点 `GET /models` 探测（5s connect / 10s total timeout，纯解析函数可单测）。
- API：`POST /api/providers/:id/models`（可覆盖 base_url/api_key，失败返回 `{ok:false,error}`）。
- WebUI Config 页 "Probe models" 按钮：列表勾选 → 生成 `[provider.<id>].model.<alias>` 条目 → 走既有 `PUT /api/config` 保存。v1 仅 OpenAI 兼容；Anthropic 无 models 端点、Gemini 留 v2。

**S1 敏感信息 .env 自动化**（★★☆，必要性高）
- `src/config/secrets.rs`：`collect_plaintext_secrets`（provider api_key / 频道 token/secret / 搜索 key / TTS key / webui token，跳过空与 `${VAR}` 引用）+ `upsert_env`（幂等、保注释、Unix 0600）+ `apply_refs` + `expand_config_secrets` + `migrate_config_secrets`（toml_edit 定点替换保注释）+ `count_plaintext_secrets`。
- `PUT /api/config` 保存时自动转存：**先写 .env 成功才替换为 `${VAR}` 引用**，失败保留明文 + warn 降级；写盘用 `${VAR}`、内存态展开回明文供热加载（`build_provider_from_config` 不认 `${VAR}`）。
- `/migrate-secrets` 斜杠命令迁移存量 config.toml；启动时扫描明文敏感字段 log warn。
- `mask_sensitive` / `merge_masked` 补全 telegram/dingtalk/mail/feishu/tts 字段（掩码 `••••`，保存时空输入 = 保留原值）。
- 二进制存储决策：**不做**（key 管理无解，单用户本地场景 .env + 0600 已够）。

**T1 TTS**（★★☆，必要性低）
- `src/tools/tts.rs`：`tts` 工具（OpenAI 兼容 `POST /audio/speech`，`[tools.tts]` 配置 enabled/base_url/api_key/model/voice，合成到 `workspace/tts/<uuid>.mp3`，发送复用 `send_file`）；`tts.api_key` 纳入 .env 敏感管线。
- WebUI 聊天页按扩展名渲染媒体：`.mp3/.wav/.ogg/.m4a` → `<audio>` 播放器（顺带修复 File 类媒体此前不显示的问题）。
- **决策修订**：原拟 edge-tts，实为 **WebSocket + Sec-MS-GEC 签名协议**（非 HTTP），不可测且接口脆弱，**降级 v2**；v1 用 OpenAI 兼容端点（mock 可测、稳定）。QQ silk 转码不做。

**M1 自然对话 MCP**（收尾，无代码）
- 现状核实：ADR-0014 已把 MCP 工具接入主 agent 工具集（`cli.rs` `all_tools.extend(mcp_tools)` + WebUI `replace_mcp_tools` 热加载），"配置好 MCP server → 自然对话直接调用"已成立；agent 自主配置 MCP server 与描述增强两选项经评审不做。

**验证**
- `cargo fmt --all --check` / `cargo clippy --lib --all-targets` 全绿；`cargo test --lib` **408 项全部通过**（新增 envprobe 4、context env 注入 2、sqlite 会话历史 6、probe 解析 3、secrets 11、tts 5）。
- 提交：`730d875`（E1）、`752acea`（W1）、`109f683`（W2）、`ca67fcc`（S1）、`40b6d0a`（T1）。

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
