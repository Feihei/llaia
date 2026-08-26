# LLAIA 项目 Roadmap

> 本文档是 LLAIA 的**前瞻路线图**：顶部是已交付阶段一览（索引），主体是下一步计划（P6）。
> 各阶段的**完整交付清单**见 [`CHANGELOG.md`](CHANGELOG.md)；详细实现计划见 [`plans/`](plans/)，设计规格见 [`specs/`](specs/)，架构决策见 [`adr/`](adr/)。

**整体目标**：一个单用户、本地优先的私人 AI 助理，跨 CLI/QQ/Web 等多 channel 接入，主 Agent + 可委派子 Agent 协作，持久化记忆与会话。

---

## 状态图例

- ✅ 已完成
- 🚧 进行中
- ⏳ 计划中（未开始）

---

## 已交付阶段一览

| 阶段 | 状态 | 一句话目标 | 交付清单 |
|---|---|---|---|
| P1 | ✅ | MVP：CLI 单 channel，REPL + 基础工具 + 持久化 | [CHANGELOG.md](CHANGELOG.md)（§P1） |
| P1.5 | ✅ | QQ channel + 全 channel 流式输出 + 稳定性补丁 | [CHANGELOG.md](CHANGELOG.md)（§P1.5） |
| P2 | ✅ | 子 Agent 委派 + 交互增强 + Web channel | [CHANGELOG.md](CHANGELOG.md)（§P2） |
| P3 | ✅ | 能力扩展与生态接入（边界/init/cron/MCP/Skill） | [CHANGELOG.md](CHANGELOG.md)（§P3） |
| P3+ | ✅ | 交互增强与生态扩展（快赢/Anthropic/Telegram/钉钉/微信） | [CHANGELOG.md](CHANGELOG.md)（§P3+） |
| P4 | ✅ | 基础能力增强（时区/做梦/压缩/权限/shutdown/Gemini/飞书…） | [CHANGELOG.md](CHANGELOG.md)（§P4） |
| P5 | ✅ | Provider Compat / 记忆预算 / 统一搜索 / todo / ask_user / skill 自管 / goal / 剩余项 | [CHANGELOG.md](CHANGELOG.md)（§P5） |

---

## P6 — 下一步计划

**状态**：🚧 进行中（起点 2026-08-21，源自 [docs/issues/issues.md](issues/issues.md) 的 9 个问题与设想评估）

> 候选池来自 `docs/issues/issues.md`。评估结论（2026-08-21）：3 个 bug/修复类（其中 2 个可直接实施、1 个需先验证根因），6 个需求/设计类需 grill 明确边界后立项。条目按状态分组，标注**必要性**（高/中/低，不做会持续踩坑或已影响正确性→高；明显改善体验→中；锦上添花→低）与**难度**（★☆☆ 半天内单点 / ★★☆ 一到数天跨模块 / ★★★ 结构性改造，动手前先出 ADR），便于排期。

### ✅ 稳定性修复（已全部交付）

- [x] `/provider N` 切换后 fallback 规则失效（必要性：**高** / 难度：★☆☆）— ✅ 已交付
  - **根因**：`switch_provider`（`src/commands/slash.rs:593`）拿到 `provider_from_ref` 后直接 `reload_provider(Some(provider))` 裸替换，`FallbackProvider` 链被丢弃。
  - **修法**：切换时若 `[agent.<alias>].fallback` 非空，重建 `FallbackProvider{主=选中模型, 备=fallback 链}`；复用 `fallback.rs` 既有 mock 测试基建补回归。
  - > ✅ 已交付（`switch_provider` 改走 `build_provider_chain` + `Provider::kind()` 标识 + 3 个回归测试）
- [x] `/stats` 返回的上下文长度数据有误（必要性：**中** / 难度：★☆☆）— ✅ 已交付
  - **根因**：`estimate_tokens`（`src/agent/context.rs:80`）只统计 `system + summary + history` 的 `chars/4`，漏掉实际发送给 provider 的 tool definitions JSON schema、`env_state` 注入、goal/todo runtime context 与每条消息 role 开销，显示值显著低于真实发送量。
  - **修法**：把这些注入源纳入估算口径（与 compact 判定共用同一函数，顺带修正自动压缩触发时机）。
  - > ✅ 已交付（`estimate_tokens` 基于 `to_messages` 全量文本 + 8 token/条结构开销；`/stats` 另加 tool defs 序列化估算）
- [x] QQ 频道发图失败 `40093006 请求参数错误`（必要性：**高** / 难度：★☆☆）— ✅ 已交付
  - **现象**：upload 阶段 `POST /v2/users/{openid}/files` 返回 40093006（`qq.rs:476`），日志 `2026-08-21T07:17:19Z`，文件 `cover_aly.png`。
  - **根因（2026-08-21 实测确认）**：腾讯 v2 富媒体接口已改为 **JSON body**（`file_type` + `file_data`(base64)/`url` + `srv_send_msg`），**不再支持 multipart 文件上传**；旧实现发 multipart `file` 字段被拒（复现时 multipart 一律 40093007 "富媒体文件下载失败"，与日志 40093006 同属接口协议不匹配）。见官方文档 [富媒体消息](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/message/send-receive/rich-media.html)。
  - **修法（已实现）**：`qq.rs` 上传段改为 JSON + base64（`file_data`，`srv_send_msg=false`，随后仍走 msg_type=7 发送）；用真实 `cover_aly.png`（2.7MB）实测 HTTP 200 返回 file_info 验证通过。
  - > ✅ 已交付并实测验证
- [x] context_size 探测失败，恒为默认 8192（必要性：**高** / 难度：★☆☆）— ✅ 已交付
  - **根因**：llama.cpp `/props`、Ollama `/api/*` 管理端点挂在服务根路径，而 OpenAI 兼容 `base_url` 以 `/v1` 结尾，探测请求打到了 `/v1/props` → 404 → `detect_context_size` 恒失败 → 落默认 8192（128k 模型显示 127% used）。
  - **修法**：`probe_base()` 剥掉 `/v1` 后缀再拼管理端点（`openai_compat.rs`）；补 mockito 回归测试（`/v1` base_url 命中根路径 `/props` 返回 131072 等 3 例）。
  - > ✅ 已交付，2026-08-24 实测校验通过

### 🧩 待 grill 明确后立项（需求/设计类）

### 🧩 快赢交付（2026-08-24，随 v0.3.1 发布）

- [x] WebUI 加入 doctor 功能（必要性：**中** / 难度：★☆☆~★★☆）— ✅ 已交付
  - 落点：Config 页 Doctor section；`GET /api/doctor` → `commands::doctor_checks`（provider 连通性 5s 超时、主模型链、context_size 探测、.env 存在性+权限、sessions.db、cron/mcp 解析、skills 计数），结构化 check 列表 ok/warn/error 分色展示。
- [x] 模型 reasoning 设置：对话临时修改参数（必要性：待定 / 难度：★☆☆）— ✅ 已交付
  - 最小边界：会话级（不持久化）、仅 thinking 开关（`/reasoning on|off`）；`Agent.thinking_off` 与隔离 turn 临时位 OR 关系；对支持 `chat_template_kwargs` 的 provider 生效（llama.cpp/Ollama/vLLM 等），其余忽略无害。
- [x] 环境发现在 WebUI 中可视（必要性：**低** / 难度：★☆☆）— ✅ 已交付
  - 落点：聊天页 ENV 只读面板 + Refresh 按钮；`GET /api/env` 返回缓存、`POST /api/env/refresh` 重探（同 `/env`）。
- [x] `/stats` 工具列表改分组计数（必要性：**低** / 难度：★☆☆）— ✅ 已交付
  - 原 `tools: {:?}` 全量名单冗长；改为 `N builtin + M mcp (server: n, ...)`（按 MCP `<server_id>__<tool>` 前缀归类）。
- [x] QQ 工具通知收敛单条 + 回复开头拼工具清单（必要性：**中** / 难度：★☆☆）— ✅ 已交付
  - 原每个 ToolStart 即发一条「🔧 xxx...」，连续工具调用刷屏。改为：首工具发一条「🔧 正在调用工具...」后续静默（QQ 消息不可编辑，更新=新消息）；`on_done` 把去重后的工具名拼进回复开头（「🔧 已调用: a、b、c」），零额外消息。
- [x] 自动 Tail Reminder：LLM 提炼抗长会话漂移要点（必要性：**中** / 难度：★★☆）— ✅ 已交付
  - **背景**：SOUL/USER 每轮在 system 头部完整重发（与 AstrBot 同构，压缩不动 system），10K 上下文即出现风格漂移的根因是 LLM 自我模仿 + 中段注意力稀释，非 prompt 丢失。
  - **机制**：回合起点对 SOUL+USER 求 md5，与 `workspace/reminder.md` 记录失配（或缺失）时后台隔离 turn 让 LLM 从 SOUL+USER 提炼 ≤120 token 的行为指令清单（走 compact_provider 回退主模型），写盘后下一轮作为最后一条消息注入（离生成点最近）。MEMORY/skills 不参与 hash（避免 memory_write 频繁触发）；文件头注释声明勿手改；生成失败静默降级。

### 🧩 待 grill 明确后立项（需求/设计类）

- [x] 会话主题自动总结（必要性：**中** / 难度：★★☆）— 已定案 → 立项
  - 现状：无代码基础；`sessions` 表加 `title` 字段。
  - 定案（grill 2026-08-24）：**压缩时顺带**用 compact provider 生成标题，落 `sessions.title`，WebUI 会话列表展示；失败降级为默认标题。存储即随会话。
  - 待实现：压缩流程内加一步标题生成 + 更新行 + WebUI 读列。
- [ ] deepseek / glm / kimi 等 provider 针对性优化（必要性：**高** / 难度：★★☆）— 已定案，有 .ref 现成实现
  - 现状：`compat.rs::detect` 仅覆盖 ollama/llamacpp 预设，线上 provider（deepseek/glm/kimi/moonshot…）走 bare；用户实测**非本地 provider 的 probe/探测常失败**，疑似线上 API 规则不同。
  - .ref 现成实现（已探明，可照抄）：
    - **nanobot** `openai_compat_provider.py`：`_MODEL_THINKING_STYLES` 按模型 slug 映射 thinking 线上参数（`thinking_type`/`enable_thinking`/`reasoning_split`）；`reasoning_content` 加入放行字段并保证 deepseek-R1 走 `reasoning_content` 而非 `reasoning`；`_requires_max_completion_tokens` 对 o 系/kimi-k3 用 `max_completion_tokens`。
    - **goose** `crates/goose-providers/src/openai.rs`：`PROVIDERS_NEEDING_MAX_TOKENS_REMAP`（cerebras/custom_deepseek/groq/kimi/mistral/moonshot…→ 传统 `max_tokens`）、`PROVIDERS_NEEDING_REASONING_EFFORT_MAPPING`、Meta effort 折叠。
  - 定案方向（grill 2026-08-24）：`Compat::detect` 扩展——detect 不只看 base_url host，还要能按其 provider 预设（deepseek/glm/kimi/moonshot）设定 `max_tokens_field` / `reasoning_to_content` / thinking 参数，模式对齐 nanobot/goose 的 per-model 表；把 `native_tool_calling` 并入同套探测（见 #10）。
  - 待明确：`probe 失败`的具体根因（context_size 探测走非 OpenAI 管理端点？`/models` 响应结构差异？）需一次实测抓包/trace 定位后再定实现。
- [ ] `memory_research` 工具：跨 session 搜索历史记忆（必要性：**中** / 难度：★★☆）— 已定案 → 立项
  - 现状：`sessions/messages/tool_calls` 已在 sqlite（`memory/sqlite.rs`）。
  - 定案（grill 2026-08-24）：**仅搜索 messages 文本**，FTS5（`rusqlite` 加 `fts5` feature），返回 N 条 + 所属 session + 时间，**暴露为模型可调工具**。结果上限与隐私边界实现时定（先给硬上限 N=20）。
  - 待实现：建 FTS 虚拟表 + 增量同步 + 工具封装 + 注册。
- [x] 检查清理基本架构 / agent loop（必要性：**中** / 难度：★★☆~★★★）— 已定案 → **立项为定期例行检查**
  - 本意（用户澄清 2026-08-25）：**定期体检主干代码**——架构合理性、逻辑反模式、死代码/未接线路径、与 ADR/编码约定（AGENTS.md：无 `#[allow(dead_code)]`、无占位 config、生产路径无 unwrap）的偏差。**非问题驱动**，不是等 loop 卡死/上下文爆炸才查，而是周期性主动检查。
  - 定案（修正 grill 2026-08-24 的"不立项"结论）：开放发散型例行项，产出为检查记录（发现项 → 直接修 / 单独立项 / 搁置留档），不要求一次清完；主干模块（agent loop / provider / memory / web）逐次过一遍。
  - 节奏：**不固定，需要时手动触发**（用户定，2026-08-25）；任何时刻想体检直接提即可。
- [x] provider native 模式默认简化（必要性：**中** / 难度：★★☆）— 已定案 → 并入 #4 的 Compat 探测
  - 现状：`native_tool_calling` 是每个 model 的布尔字段（`config.rs` `ModelConfig`，缺省默认 `true`），两种模式协议本质不同——native 发 `tools` 参数并期待结构化 `tool_calls`；标签降级不发 tools、靠注入 `<tool_call>` 协议指令 + prompt 约束。P4-b 已让 `ToolCallStreamParser` 始终清洗文本流（native 也剥标签），但请求载荷差异仍在，无法完全二合一。
  - 定案（grill 2026-08-24）：**并入 Compat 自动探测**——`compat.rs::detect` 按 provider 预设（ollama/llamacpp/以及 #4 新增的 deepseek/glm/kimi）推断 `native_tool_calling`，字段允许 `None=auto`（缺省跟随探测），用户不再手设；**不做**单次请求内动态降级（复杂度不值）。无实际踩坑，属预防性简化。
  - 待实现：给 `Compat` 增加推断 `native_tool_calling` 的能力 + `ModelConfig` 支持 `None` + detect 扩展。与 #4 同一套探测框架合并做，避免两套逻辑。
- [x] 启动速度优化（必要性：**中** / 难度：★★☆）— 已定案，**实测定点后方向转向**
  - 现状：`build_agent`（`channels/cli.rs:625`）启动主路径串行执行：① `McpRegistry::connect_all`（`mcp/client.rs:249` 串行 for，每 server `transport.connect` + `handshake`(30s 超时) + `tools/list`）→ ② `load_skills` → ③ 逐个 build 子 Agent + main Agent。
  - 实测（2026-08-24 用户 trace）：启动首日志→registry built ≈ **840ms**，其中 MCP 握手+注册 ~1ms（单 server）、skills 扫描(37) ~7ms 均非瓶颈；**大头是 `detect_context_size` 的 llama.cpp `/props` 探测**——coder 一次 ~206ms、main 探两次 ~225ms，合计 400–600ms 独占近 2/3。
  - 原定案（grill）：MCP `join_all` 并行连接。**实测表明对当前单 server 配置几乎无收益，方向转向**：
    1. **`/props` 探测收敛/去重**：`final=0` 却覆盖 `configured=128000`（疑似 bug，探测盖掉显式配置）；子 Agent 与 main 重复探测、main 探两次。先查根因（为何 `/props` 返 0、为何多次探、为何覆盖配置），再谈缓存/并行。
    2. MCP `join_all` 并行仍值得做，但对多 server 才有效，且不是当前用户瓶颈——降至次要。
    3. 给 build_agent 加阶段耗时打点（`elapsed_ms`），后续 trace 不用再靠猜。
  - 待办：① 定位 `context_size final=0 且覆盖 configured` 的根因与重复探测；② 视情况加阶段耗时打点；③ MCP 并行连接保留为次要优化。
  - **根因定位（2026-08-24 用户 trace 深挖）**：
    - **A（bug：`final=0` 覆盖显式配置）**：后端为 llama.cpp，`/props` 返回 `n_ctx: 0`（服务器以 auto/0 启动，0 表示用模型默认，非真实窗口）；`try_llamacpp_props`（`openai_compat.rs:483-495`）对 `n==0` 仍返 `Some(0)`；`cli.rs:564-574` 走 `configured.min(detected)` → `128000.min(0)=0`。显式配置被无意义 0 覆盖。→ 修：`n==0` 视为 `None`（Ollama `.context_length==0` 同理），探测失败时 `final` 保持 `configured`。
    - **B（性能：#11 大头）**：`FallbackProvider::detect_context_size`（`fallback.rs:82-91`）逐探测 fallback 链上每个 provider；main 带 2 模型链 → main 探 2 次 + coder 1 次 = 3 次 GET /props@~200ms ≈ 600ms，占启动 2/3。同一后端多模型探测必同值，冗余。→ 修：同后端去重 / 只探 `main()`。
    - **C（次要）**：main与子 agent 同后端各自探测同值 → 可缓存。

### 🧩 主干代码体检记录（2026-08-26，例行第 1 轮）

范围：agent loop（mod/sink/context/runner）+ provider 层（openai_compat/fallback/compat）+ memory 层（sqlite/trim）+ 约定扫描。整体评价：主干质量高（锁设计、KV cache 友好注入、compat 零回归约束均有注释与回归测试）。

**直接修（本轮已交付）**：

- [x] `cheap_normalize` 丢弃空文本 assistant(tool_calls) 消息 → 孤儿 tool 消息违反 OpenAI 协议（`context.rs`；原生工具调用「零文本 + tool_calls」是常态形态，压缩后严格端点直接 400。修：丢弃条件加 `m.tool_calls.is_none()` + 回归测试）
- [x] `StreamEvent::Error` 路径丢失错误前已生成的部分输出（`agent/mod.rs`；用户看到半截回复但模型下一轮不知道自己说过什么。修：镜像 tx-closed 中止路径保存 `iter_text`）
- [x] 4 处 `#[allow(dead_code)]` 清理：`sqlite.rs::all_messages`（零调用者）、`feishu.rs::event_id`、`tavily.rs` 两个 DTO 字段

**待修（下轮候选）**：

- [ ] SSE 解析只认 `\n\n` 事件分隔符（`openai_compat.rs`）：CRLF（`\r\n\r\n`）服务端/反代事件永不分割 → 整回复静默丢失。触碰流解析核心，单独修 + mockito 双分隔符测试；anthropic/gemini 流解析一并检查
- [ ] `workspace/tmp/` 工具图片落盘后无任何清理 → 磁盘无界增长（`agent/mod.rs::persist_tool_image`）。需小设计：启动时清理 N 天前文件

**搁置留档（影响小，暂不动）**：

- 生产路径 `lock().unwrap()`：`slash.rs:476/504`（background_tasks）与 `sqlite.rs` 全文件（conn，约 20 处）。锁内均同步调用、无 await，正确性无问题，仅 poisoning-panic 与编码约定冲突。机械替换 `unwrap_or_else(|e| e.into_inner())` 可解，diff 大
- `TRIM_CACHE` 无上限增长（`memory/trim.rs`）：单用户 MEMORY 变更频率低，实际影响极小
- 图片逐张串行 vision 描述（`agent/mod.rs::maybe_describe_images`）：可 `join_all`，但通常单图
- tools schema 每次请求重建序列化（`openai_compat.rs`）：~20 工具 × 每迭代，微小
- 常量正则 `unwrap()`（`secrets.rs:52` / `config.rs:957` / `approval.rs:163`）：逻辑上不可 panic，按约定补注释即可
- 云 provider 启动也打 `/props`、`/api/tags` 探测：已由上方 #4（provider 针对性优化）覆盖，不重复立项

### ⭐ 新增发现（2026-08-26·2）

**#A 新增 provider 填 api key 后报「environment variable referenced but not set」（已修复）**

- 现象：WebUI 添加新 provider（如 modelscope）填 key 保存，日志立刻 WARN `var="LLAIA_PROVIDER_MODELSCOPE_API_KEY" ... replacing with empty string`，且该 provider 内存态 key 被压成空串、热加载失效（重启才恢复）。
- 根因：**不是 .env 没写**（.env / config.toml 均已正确落盘并引用）。真因在 `web/mod.rs::put_config` 时序：`apply_refs` 把 `merged` 里新 secret 替换成 `${VAR}` **之后**才 `merged.clone()` 出 `runtime_config`，随后 `expand_config_secrets` 对刚写入磁盘、**尚未进入当前进程 env** 的变量做 `std::env::var` → 命中「未设置」降级（运行态从不重载 .env，仅 main.rs 启动加载一次）。
- 修复：`runtime_config` **先于 `apply_refs` 克隆**（保留明文），磁盘仍写 `${VAR}` 引用；`expand_config_secrets` 对非引用明文透传、不查询 env → 无警告、key 立即生效。
- 说明：这是布局级最小改动（移动一行克隆 + 注释），符合「成功才应用引用 / 失败保留明文」既有降级语义。

**#B /move 后的目录信任模型（分析·待决策，未实现）**

- 现状：`approval_decision` 的 `workspace` 参数取自 `workspace_root`（`mod.rs:927`），/move 后**目录内**的 file/terminal 操作在 default 档本就自动放行（`within→Approved`）。因此「每次都批准」仅出现在**逃出被移动目录**的操作：绝对路径指向别处、`..` 上退、terminal 触碰 moved 目录之外（含 agent 自家 home workspace 的 goal.md/MEMORY 等记账文件）→ 每次 /ok。
- 用户诉求：一次 /move 批准后应信任该目录，且 move 审批信息里提醒信任范围。
- 已落最小改动：`format_move_prompt` 提示补充「切换后该目录内的文件读写/终端命令默认放行，仅目录外的路径仍需审批」（`approval.rs`）。
- 待决设计（需选型，暂不实现）：
  - **选 1（推荐）**：会话级持久「受信目录」——把 /move 目标 canonical 目录加入受信集合（随 workspace_root 持久化），`tool_within_workspace` 以「落在任一受信目录内」判定，令 moved 目录内操作自动放行；逃出集合仍审批。负担低、语义清晰。
  - **选 2**：恢复 home workspace 也应视为受信（等价于选 1 默认含 home）。
  - **选 3（谨慎）**：仅保留现有「workspace_root 内放行」+ 提示澄清，不引入受信集合（省改动，但 home 记账文件反复审批的摩擦仍在）。
  - 安全权衡：/move 到过宽的目录（如 `C:\`）会把几乎全部文件操作划为「内」，需与黑名单校验配合评估；受信目录需 canonicalize + 黑名单复核。

### ⭐ 新增发现与技术探讨（2026-08-26·3）

**#C terminal 命令安全扫描把字面量 `\n` 当路径（定位完成·修复待确认）**

- 现象：用 terminal 跑 officecli 时，命令里的 `\n`（想表达的换行文本）被视为路径 → 越界拒绝 → 多次失败，被迫转 python-pptx（sessions.db sess17 msg 1163-1205 记录完整试错链）。
- 根因（非 officecli，是安全扫描层）：`path_guard.rs::extract_path_tokens` 用 `split_whitespace()`（把空格/`\t`/**换行**全拆），且 `looks_like_path` 含 `token.contains(std::path::MAIN_SEPARATOR)`——**Windows 上单个反斜杠即命中**。于是字面量 `\n`（反斜杠+n）所在 token 被判为路径，`validate_path` 越界失败。
- 附带真实约束（msg1172）：绝对路径 `E:/...` 出 workspace 触发工作区校验 = 正常安全行为，agent 误以为是「换写法绕开」，反而绕进 JSON heredoc 撞上 `\n` 误报。
- 修复建议（安全层，需谨慎）：`looks_like_path` 收紧——去掉裸 `contains('\')`，改为「含 `\` 且含盘符 `X:\` / 前导 `\`(UNC)」，或「有 `/` `./` `../` `~`」之一。一行改动 + 回归测试。
- 修复（已交付）：`looks_like_path` 收紧为「前导 `/` `~` `./` `../` `\` || 盘符 `X:` || 含 `/`」，不再把含单个反斜杠的转义片段当路径；新增字面量 `\n`、Windows 盘符/UNC 两个回归测试。fmt/clippy/test 全绿。

**#D session 管理模型：通用线 + 任务线（方向已定·设计探索）**

- 背景：通用 agent 沿用 coding agent 的 session 模型不自然。coding 以「任务」为 session 边界；通用助手日常杂活不该每条都开任务。
- 定案方向（用户选）：**一条常驻通用 session + 按需显式开启的任务 session**。任务完成/用户关闭即归档，独享完整上下文，不污染通用线。
- **与 /move 耦合（用户补充**：移动到 workspace 外目录往往意味着「在该目录执行主线以外的任务」→ 可作为**自动触发任务 session 的信号**：/move 到外部目录时，提示把该目录绑定为一个新任务 session（绑定目录 + 独立上下文）。这与 #B 的「受信目录」天然成一套：**任务 session = 受信目录 + 独立上下文边界**。
- 待决问题（勿提前实现）：
  - 触发边界：显式 `/task <名>` +/move 自动建议为主，规则猜测不可靠。
  - 可发现性：需任务列表/入口；评估能否合并现有 goal/todo，避免三套平行「有边界的事」。
  - 任务 session 持久化：独立上下文如何落 sqlite（session 类型字段 + 关联目录？）。
  - 跨频道进出任务：换频道能否回到某任务线。

---

## 工程约定

- 每个 Task 完成后跑 `cargo test` + `cargo clippy`
- 提交节奏：一个完整功能/修复链路验证通过后提交一次，不要每个 Task 都提交
- 遇到编译错误立即修，不要积累
- 详细实现计划放 `docs/plans/YYYY-MM-DD-<feature>.md`，设计规格放 `docs/specs/YYYY-MM-DD-<feature>-design.md`，架构决策放 `docs/adr/NNNN-<topic>.md`
- 阶段交付后，其完整勾选清单迁入 `docs/CHANGELOG.md`，本文件只保留「已交付阶段一览」索引 + 下一步计划
