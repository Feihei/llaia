# LLAIA 项目 Roadmap

> 本文档是 LLAIA 的**前瞻路线图**：顶部是已交付阶段一览（索引），主体是下一步计划（P6）。
> 各阶段的**完整交付清单**见 [`CHANGELOG.md`](CHANGELOG.md)；详细实现计划见 [`plans/`](plans/)，设计规格见 [`specs/`](specs/)，架构决策见 [`adr/`](adr/)。

**整体目标**：一个单用户、本地优先的私人 AI 助理，跨 CLI/QQ/Web 等多 channel 接入，主 Agent + 可委派子 Agent 协作，持久化记忆与会话。

---

## 状态图例

- ✅ 已完成
- 🚧 进行中
- ⏳ 计划中（未开始）

> 条目勾选框语义：**`[x]` = 代码已落地**，`[ ]` = 尚有未完部分（含「已定案未实现」「部分交付」）。
> 「已定案/已立项」只是决策完成，不算 `[x]`——须在实现后勾上，并在条目标注交付日期与代码位置。

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
| P6 | 🚧 | 首批交付（稳定性修复 + 快赢）已随 v0.3.1 发布；WebUI 改进等后续批次进行中 | [CHANGELOG.md](CHANGELOG.md)（§v0.3.1） |

---

## P6 — 下一步计划

**状态**：🚧 进行中（起点 2026-08-21，源自 [docs/issues/issues.md](issues/issues.md) 的 9 个问题与设想评估）

> 候选池来自 `docs/issues/issues.md`。评估结论（2026-08-21）：3 个 bug/修复类（其中 2 个可直接实施、1 个需先验证根因），6 个需求/设计类需 grill 明确边界后立项。条目按状态分组，标注**必要性**（高/中/低，不做会持续踩坑或已影响正确性→高；明显改善体验→中；锦上添花→低）与**难度**（★☆☆ 半天内单点 / ★★☆ 一到数天跨模块 / ★★★ 结构性改造，动手前先出 ADR），便于排期。
> 已交付部分（4 项稳定性修复 + 6 项快赢）随 v0.3.1 发布，完整勾选清单已归档至 [CHANGELOG.md](CHANGELOG.md) §v0.3.1，此处不再重复；主干体检直接修项将随下一版本归档。本节只保留剩余计划与进行中条目。

### 🌐 WebUI 改进批次（2026-08-27 评估立项）

> 三项来自用户需求评估（2026-08-27）。W1/W2 是快赢可直接做；W3 有数据采集缺口需先补链路，动手前先定表结构与统计边界。

- [x] **W1** sessions / config 页左侧栏固定不随内容滚走 + 会话详情区右下角「跳顶部/跳底部」悬浮按钮（必要性：**中** / 难度：★☆☆）— 已交付（2026-08-28）
  - 根因已定位（两个页面同源）：`index.html` 里 `<template x-if="authed">` 的匿名包裹 `<div>` 无任何样式，打断了 flex 高度链——`main { flex:1; overflow:hidden }` 因父级不是 flex 容器而失效，pane 高度随内容无限增长、变成整页（body）滚动；sessions 左侧会话列表与 config 左侧分区导航都因此被滚出视口。
  - 方案：给该包裹 div 加 class 并声明 `display:flex; flex-direction:column; flex:1; min-height:0`，恢复高度约束后 `.session-list` 与 `#config-pane .sidebar`（均已自带 `overflow-y:auto`）各自固定在视口内、右侧内容独立滚动。纯 CSS 改动，一次修两处。
  - 悬浮按钮：`.session-detail` 设 `position:relative`，右下角两个 absolute 定位按钮（↑/↓），分别 `scrollTo({top:0})` / `scrollTo({top:scrollHeight})`；纯前端改动，无新 API。
- [x] **W2** About 页更新检查按钮（必要性：**低** / 难度：★☆☆）— 已交付（2026-08-28）
  - 后端：新增 `GET /api/update/check` —— reqwest 请求 GitHub Releases latest API（`https://api.github.com/repos/Feihei/llaia/releases/latest`，该 API 必须带 User-Agent 头否则 403），`tag_name` 去 `v` 前缀后与 `CARGO_PKG_VERSION` 三段数值比较；返回 `{current, latest, update_available, url}`；失败（离线/限流）透传原因，结果加分钟级内存缓存防连点。无鉴权请求限 60 次/h，单用户足够。
  - 前端：About section 加 Check Updates 按钮 + 结果行（已最新 / 新版本号 + Release 页下载链接），复用现有 cron-msg 消息展示模式。
- [x] **W3** token 用量 dashboard，参考 AstrBot WebUI（必要性：**中** / 难度：★★☆）— 已交付（2026-08-28）
  - 参考实现已探明（`.ref/AstrBot`）：两张表（小时预聚合消息计数 + 逐请求 provider_stats 含 input/cached/output tokens 与 timing）；API 把单次范围查询在 Python 聚合成预成形 `[ts,value]` series；UI 为时间范围选择（1/3/7 天）+ 汇总卡（总 tokens/调用数/avg TTFT/成功率）+ per-provider 堆叠柱图 + per-session Top10。
  - LLAIA 数据链路现状（2026-08-27 核实）：
    - Provider 层已备好：`ChatResponse.usage: Option<Usage>`、流式 `StreamEvent::Usage(Usage)`（openai_compat 仅 `compat.streaming_usage=true` 时发送；ollama/llamacpp 自动探测预设默认开启）
    - 缺口①：agent loop 对 `StreamEvent::Usage` 直接丢弃（`agent/mod.rs:825`），未累计未落库
    - 缺口②：sqlite 无逐回合用量表；`sessions.token_count` 列存在但零调用者（死列），可回收利用做会话级累计
    - 缺口③：anthropic/gemini 流式 usage 未解析（anthropic 收集流事件时丢弃 / gemini 恒 None），云端 provider 要计入统计需先补齐——列为本项前置子任务
  - 最小边界方案：
    - 采集：回合内累计 usage（工具循环多次迭代合并一条），turn 结束写 `turn_usage(id, session_id, ts, model_ref, prompt_tokens, completion_tokens, kind)`；compact/vision/reminder 等 sidecar LLM 调用用 `kind` 区分，UI 默认只看主对话
    - API：`GET /api/stats/tokens?days=N` 单查询 SQL 聚合出天/小时 bucket series + 总计 + per-model/per-session 分组；单用户数据量小，不需要 AstrBot 式预聚合表
    - UI：顶层 tab「Stats」：范围选择 + 汇总卡（tokens/请求数）+ 每日柱状图 + per-model/per-session 排名表
    - 明确不做：TTFT/TPM/成功率（AstrBot 有是因为记了 timing 埋点，LLAIA Provider 层无此埋点，单独补不值）、引入图表库（纯 SVG/CSS 手绘条形图起步，契合终端风零依赖；不够用再 vendored Chart.js）
  - 已知限制须在 UI 标注：LMStudio/bare 端点默认不上报 usage（streaming_usage 未开且端点未必支持）→ 该 provider 无数据是预期行为而非 bug。


### 🧩 待 grill 明确后立项（需求/设计类）

- [x] 会话主题自动总结（必要性：**中** / 难度：★★☆）— 已交付（2026-08-29）
  - 定案（grill 2026-08-24）：**压缩时顺带**用 compact provider 生成标题，落 `sessions.title`，WebUI 会话列表展示；失败降级为默认标题。存储即随会话。
  - 实现：`sessions` 表加 `title` 列（`memory/sqlite.rs`，存量库幂等 `ALTER TABLE` 补列）；`Agent::ensure_session_title`（`agent/mod.rs`）在自动压缩（`maybe_auto_compact`）与 `/compact` 实际发生 LLM 压缩后调用——仅当标题为空时生成一次，素材为前 6 条 user/assistant 消息，失败/空回复降级为首条用户消息截断（40 字符），标题清洗（剥引号/书名号、取首行、60 字符上限）；`list_sessions` 带出 `title`，WebUI 会话列表优先显示标题（无标题回退 channel）。
- [x] deepseek / glm / kimi 等 provider 针对性优化（必要性：**高** / 难度：★★☆）— 已交付（2026-08-28 核实）
  - 现状：`compat.rs::detect` 仅覆盖 ollama/llamacpp 预设，线上 provider（deepseek/glm/kimi/moonshot…）走 bare；用户实测**非本地 provider 的 probe/探测常失败**，疑似线上 API 规则不同。
  - .ref 现成实现（已探明，可照抄）：
    - **nanobot** `openai_compat_provider.py`：`_MODEL_THINKING_STYLES` 按模型 slug 映射 thinking 线上参数（`thinking_type`/`enable_thinking`/`reasoning_split`）；`reasoning_content` 加入放行字段并保证 deepseek-R1 走 `reasoning_content` 而非 `reasoning`；`_requires_max_completion_tokens` 对 o 系/kimi-k3 用 `max_completion_tokens`。
    - **goose** `crates/goose-providers/src/openai.rs`：`PROVIDERS_NEEDING_MAX_TOKENS_REMAP`（cerebras/custom_deepseek/groq/kimi/mistral/moonshot…→ 传统 `max_tokens`）、`PROVIDERS_NEEDING_REASONING_EFFORT_MAPPING`、Meta effort 折叠。
  - 定案方向（grill 2026-08-24）：`Compat::detect` 扩展——detect 不只看 base_url host，还要能按其 provider 预设（deepseek/glm/kimi/moonshot）设定 `max_tokens_field` / `reasoning_to_content` / thinking 参数，模式对齐 nanobot/goose 的 per-model 表；把 `native_tool_calling` 并入同套探测（见 #10）。
  - 已实现（`provider/compat.rs:104-165`）：`Compat::detect(base_url, model)` 在 ollama/llamacpp 之外新增 deepseek / zhipu·bigmodel·glm / moonshot·kimi host 预设（开 `streaming_usage`）；per-model 规则对齐 nanobot/goose——o 系与 `kimi-k3` 切 `max_completion_tokens`（`max_tokens_field`）、`deepseek-reasoner`/`deepseek-r1`/`kimi-k` 开 `reasoning_to_content`；`native_tool_calling` 并入同套探测（见 #10）。全部带回归测试。
  - 残余已收（2026-08-29）：`detect_context_size` 按 host 门控——仅本地后端（localhost / `.local` / 回环 / 私网 / 链路本地）才打 `/props`、`/api/tags`，云 provider 直接跳过（`openai_compat.rs::probe_host_is_local`，含分类回归测试）。`probe 失败`的其余根因（若有）待后续实测。
- [x] `memory_research` 工具：跨 session 搜索历史记忆（必要性：**中** / 难度：★★☆）— 已定案 → 已实现
  - 现状：`sessions/messages/tool_calls` 已在 sqlite（`memory/sqlite.rs`）。
  - 定案（grill 2026-08-24）：**仅搜索 messages 文本**，FTS5，返回 N 条 + 所属 session + 时间，**暴露为模型可调工具**。结果上限与隐私边界实现时定（先给硬上限 N=20）。
  - 已实现：`message_fts` FTS5 虚拟表 + INSERT/DELETE 触发器 + 存量回填；`SessionStore::search_messages`（`memory/sqlite.rs`）；`MemoryResearch` 工具封装（`tools/memory.rs`，只读无需审批，limit 1..=20，snippet 截断，非法查询降级提示）；CLI 工具集注册。
- [x] 检查清理基本架构 / agent loop（必要性：**中** / 难度：★★☆~★★★）— 已定案 → **立项为定期例行检查**
  - 注：本项是**例行项而非一次性交付**，`[x]` 表示已定案立项；各轮实际产出见下方「主干代码体检记录」小节。
  - 本意（用户澄清 2026-08-25）：**定期体检主干代码**——架构合理性、逻辑反模式、死代码/未接线路径、与 ADR/编码约定（AGENTS.md：无 `#[allow(dead_code)]`、无占位 config、生产路径无 unwrap）的偏差。**非问题驱动**，不是等 loop 卡死/上下文爆炸才查，而是周期性主动检查。
  - 定案（修正 grill 2026-08-24 的"不立项"结论）：开放发散型例行项，产出为检查记录（发现项 → 直接修 / 单独立项 / 搁置留档），不要求一次清完；主干模块（agent loop / provider / memory / web）逐次过一遍。
  - 节奏：**不固定，需要时手动触发**（用户定，2026-08-25）；任何时刻想体检直接提即可。
- [x] provider native 模式默认简化（必要性：**中** / 难度：★★☆）— 已交付（随 #4 同一套探测，2026-08-28 核实）
  - 现状：`native_tool_calling` 是每个 model 的布尔字段（`config.rs` `ModelConfig`，缺省默认 `true`），两种模式协议本质不同——native 发 `tools` 参数并期待结构化 `tool_calls`；标签降级不发 tools、靠注入 `<tool_call>` 协议指令 + prompt 约束。P4-b 已让 `ToolCallStreamParser` 始终清洗文本流（native 也剥标签），但请求载荷差异仍在，无法完全二合一。
  - 定案（grill 2026-08-24）：**并入 Compat 自动探测**——`compat.rs::detect` 按 provider 预设（ollama/llamacpp/以及 #4 新增的 deepseek/glm/kimi）推断 `native_tool_calling`，字段允许 `None=auto`（缺省跟随探测），用户不再手设；**不做**单次请求内动态降级（复杂度不值）。无实际踩坑，属预防性简化。
  - 已实现：`Compat` 新增 `native_tool_calling` 字段（bare/ollama/llamacpp 预设均 `true`，`compat.rs:49-52`）；`ModelConfig.native_tool_calling` 改 `Option<bool>`（`None=auto` 跟随探测，`config.rs`）；`[provider.<id>.compat]` 可逐字段覆盖。与 #4 合并为同一套探测框架，无两套逻辑。
- [x] 启动速度优化（必要性：**中** / 难度：★★☆）— 已交付（2026-08-29：A/B 修复 + ②耗时打点 + ③MCP 并行）
  - 现状：`build_agent`（`channels/cli.rs:625`）启动主路径串行执行：① `McpRegistry::connect_all`（`mcp/client.rs:249` 串行 for，每 server `transport.connect` + `handshake`(30s 超时) + `tools/list`）→ ② `load_skills` → ③ 逐个 build 子 Agent + main Agent。
  - 实测（2026-08-24 用户 trace）：启动首日志→registry built ≈ **840ms**，其中 MCP 握手+注册 ~1ms（单 server）、skills 扫描(37) ~7ms 均非瓶颈；**大头是 `detect_context_size` 的 llama.cpp `/props` 探测**——coder 一次 ~206ms、main 探两次 ~225ms，合计 400–600ms 独占近 2/3。
  - 原定案（grill）：MCP `join_all` 并行连接。**实测表明对当前单 server 配置几乎无收益，方向转向**：
    1. **`/props` 探测收敛/去重**：`final=0` 却覆盖 `configured=128000`（疑似 bug，探测盖掉显式配置）；子 Agent 与 main 重复探测、main 探两次。先查根因（为何 `/props` 返 0、为何多次探、为何覆盖配置），再谈缓存/并行。
    2. MCP `join_all` 并行仍值得做，但对多 server 才有效，且不是当前用户瓶颈——降至次要。
    3. 给 build_agent 加阶段耗时打点（`elapsed_ms`），后续 trace 不用再靠猜。
  - 待办：~~① 定位 `context_size final=0 且覆盖 configured` 的根因与重复探测~~（已修，见下）；~~② 加阶段耗时打点~~；~~③ MCP 并行连接~~（②③ 已交付，见下）。
  - **根因定位（2026-08-24 用户 trace 深挖）**：
    - **A（bug：`final=0` 覆盖显式配置）**：后端为 llama.cpp，`/props` 返回 `n_ctx: 0`（服务器以 auto/0 启动，0 表示用模型默认，非真实窗口）；`try_llamacpp_props`（`openai_compat.rs:483-495`）对 `n==0` 仍返 `Some(0)`；`cli.rs:564-574` 走 `configured.min(detected)` → `128000.min(0)=0`。显式配置被无意义 0 覆盖。→ 修：`n==0` 视为 `None`（Ollama `.context_length==0` 同理），探测失败时 `final` 保持 `configured`。
      - ✅ 已修（2026-08-28 核实）：`try_llamacpp_props` 末尾 `n_ctx.filter(|&n| n > 0)`（`openai_compat.rs:499-501`），Ollama `.context_length==0` 同处理；含 mockito 回归测试。
    - **B（性能：#11 大头）**：`FallbackProvider::detect_context_size`（`fallback.rs:82-91`）逐探测 fallback 链上每个 provider；main 带 2 模型链 → main 探 2 次 + coder 1 次 = 3 次 GET /props@~200ms ≈ 600ms，占启动 2/3。同一后端多模型探测必同值，冗余。→ 修：同后端去重 / 只探 `main()`。
      - ✅ 已修（2026-08-28 核实）：`fallback.rs:86` 只探 `self.main()`，注释声明「避免同一后端多模型重复探测拖慢启动」，含回归测试。
    - **C（次要）**：main与子 agent 同后端各自探测同值 → 可缓存。已被 #F 的懒解析 + 缓存间接覆盖（`context_size_now` 结果进 `Agent.resolved_context_size`，fork 子 agent 共享该缓存）。
  - **②③ 已实现（2026-08-29）**：② `build_agent`（`channels/cli.rs`）四阶段独立计时——mcp connect / skills / sub agents / main agent 各带 `elapsed_ms`，末行 `AgentRegistry built` 带 `total_elapsed_ms`；③ `connect_all`（`mcp/client.rs`）改 `join_all` 并发握手，结果仍按原配置顺序注册（`tool_index` 稳定），单 server 超时（30s）不再串行累加。

### 🧩 主干代码体检记录（2026-08-26，例行第 1 轮）

范围：agent loop（mod/sink/context/runner）+ provider 层（openai_compat/fallback/compat）+ memory 层（sqlite/trim）+ 约定扫描。整体评价：主干质量高（锁设计、KV cache 友好注入、compat 零回归约束均有注释与回归测试）。

**直接修（本轮已交付）**：

- [x] `cheap_normalize` 丢弃空文本 assistant(tool_calls) 消息 → 孤儿 tool 消息违反 OpenAI 协议（`context.rs`；原生工具调用「零文本 + tool_calls」是常态形态，压缩后严格端点直接 400。修：丢弃条件加 `m.tool_calls.is_none()` + 回归测试）
- [x] `StreamEvent::Error` 路径丢失错误前已生成的部分输出（`agent/mod.rs`；用户看到半截回复但模型下一轮不知道自己说过什么。修：镜像 tx-closed 中止路径保存 `iter_text`）
- [x] 4 处 `#[allow(dead_code)]` 清理：`sqlite.rs::all_messages`（零调用者）、`feishu.rs::event_id`、`tavily.rs` 两个 DTO 字段

**待修（下轮候选）**：

- [x] SSE 解析只认 `\n\n` 事件分隔符（`openai_compat.rs`）：CRLF（`\r\n\r\n`）服务端/反代事件永不分割 → 整回复静默丢失。触碰流解析核心，单独修 + mockito 双分隔符测试；anthropic/gemini 流解析一并检查 — 已交付（2026-08-28）
- [x] `workspace/tmp/` 工具图片落盘后无任何清理 → 磁盘无界增长（`agent/mod.rs::persist_tool_image`）。需小设计：启动时清理 N 天前文件 — 已交付（2026-08-28，serve/chat 启动时清理 3 天前文件）

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

**#C terminal 命令安全扫描把字面量 `\n` 当路径（已交付）**

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

### ⭐ 新增设计与待实施（2026-08-27），两项一起落地

**#E `/provider` 切换默认持久化 + `--temp` 临时切换（已交付，2026-08-28 核实）**

- 现状：`switch_provider`（`slash.rs:697`）只 `reload_provider` 内存替换，**不写 config.toml** → 进程内生效、重启/WebUI 保存配置即回落到 config 值；`context_size` 冻结在 Agent 构建期，切换后压缩阈值不跟随新模型。
- 定案（用户倾向，2026-08-27）：
  - `/provider <n|id.alias>` → **默认持久化**：把新模型写进 `[agent.<alias>].model`，同步更新内存 `live_config.agent.<alias>.model`，再 `reload_provider`。
  - `/provider --temp <n|id.alias>` → 维持纯内存临时切换（试跑不落地）。
  - 写盘用 `toml_edit` 定点改 `[agent.<alias>].model` 单 key（保住注释，不 `toml::to_string_pretty` 全量覆盖）；`switch` 从 live_config 分离 `persist` 标志。
- 已实现：`slash.rs:697-765` 解析 `--temp` 前缀（`parse_provider_arg`）→ `switch_provider(persist)` 双分支；默认分支用 `toml_edit` 定点写 `[agent.<alias>].model`（保注释）+ 同步内存 `live_config` → `reload_provider`；临时分支仅内存替换，输出追加 `(temporary)`。含两个回归测试（`test_provider_switch_temp_does_not_persist` / 持久化用例写真实 `config.toml`）。

**#F `context_size` 与 Agent 解耦：懒解析 + 缓存 + reload 失效（已交付，2026-08-28 核实；残留一处见末条）**

- 现状：`Agent.context_size: usize` 字段在 `Agent::new`（`cli.rs:562`）同步解析并冻结；`reload_provider`（`mod.rs:244`）与 WebUI `hot_reload_providers` 只换 provider **不重算 context_size** → 进程内切到大/小窗模型后压缩阈值仍是旧值（`needs_compaction` 与 `compact` 的 token 预算都读它，`mod.rs:615/625`）。且构建期同步 `detect_context_size()` 是启动阻塞大头（见 #11 实测 ~200ms/次、main 探多次）。
- 定案：`context_size` 不再冻结，改为**按活动 provider 懒解析 + 缓存**，窗口随模型跟随：
  - 新增 `Agent.resolved_context_size: RwLock<Option<usize>>` 作为结果缓存；
  - 新增方法 `context_size_now()`：缓存命中直接返回；未命中 → 从 live_config 取当前 alias 的 `model_cfg.context_size`（`configured`）+ 活动 provider `detect_context_size()`，`min(configured,detected)`（探测不到用 configured、都没有 8192）→ 缓存；
  - `reload_provider` / `reload_compact_provider` 命中时清空缓存 → `/provider` 切换、WebUI 热保存后窗口立即跟随新模型；
  - 使用点全部改 `context_size_now()`：`maybe_auto_compact`（`mod.rs:615`）、`spawn_continuation` 克隆（`mod.rs:450`）、`/config` 展示与 `/compact`（`slash.rs:204/337`）。
  - 顺带：`cli.rs::build_agent` **不再同步探测**（去掉对 `provider.detect_context_size()` 与 `provider_ref` 的依赖），把探测挪到首个 turn 懒执行 → 启动不再被 /props 阻塞；构建期仅按 `configured.unwrap_or(8192)` 传入基线。
- 已实现（2026-08-28 核实）：`Agent.resolved_context_size` 缓存 + `context_size_now()`（`mod.rs:286`，`min(configured,detected)` / 全无 8192）；`reload_provider` 清缓存（`mod.rs:277`）；`fork_for_isolated` 共享父级缓存（`mod.rs:528`）；构建期不再同步探测（`cli.rs:567` 仅传 `configured.unwrap_or(8192)` 作降级基线）；使用点已迁移 `maybe_auto_compact`（`mod.rs:677`）、`/compact`（`slash.rs:205`）、`/config`（`slash.rs:336`）；含回归测试 `test_context_size_now_follows_provider_switch`。
- **残留已清（2026-08-29）**：`/stats`（`slash.rs`）原直读冻结基线 `agent.context_size`，已迁移 `context_size_now()`（与 `/config` 一致），展示的 context_size/阈值/占比跟随当前模型。
- 待决（本项不实现）：探测结果磁盘缓存（按 base_url+model，存 sqlite）——与 #11 的「探测收敛/缓存」合并，重启免重复探测；先靠懒解析 + reload 失效解决正确性与启动阻塞。
- 附带影响：`context_size_now` 是 async，`/config`/`/compact` 路径随之 async（本就 async 上下文，无碍）；`--temp` 切换时 live_config 模型未更新，`configured` 取到旧模型值作为 min 上限，属可接受的临时实验边界。

---

## 工程约定

- 每个 Task 完成后跑 `cargo test` + `cargo clippy`
- 提交节奏：一个完整功能/修复链路验证通过后提交一次，不要每个 Task 都提交
- 遇到编译错误立即修，不要积累
- 详细实现计划放 `docs/plans/YYYY-MM-DD-<feature>.md`，设计规格放 `docs/specs/YYYY-MM-DD-<feature>-design.md`，架构决策放 `docs/adr/NNNN-<topic>.md`
- 阶段交付后，其完整勾选清单迁入 `docs/CHANGELOG.md`，本文件只保留「已交付阶段一览」索引 + 下一步计划
