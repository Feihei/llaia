# LLAIA 项目 Roadmap

> 本文档是 LLAIA 的**前瞻路线图**：顶部是已交付阶段一览（索引），主体是下一步计划（P5）。
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

---

## P5 — 未来计划（下一步）

**状态**：✅ 全部已交付（P5-1 ~ P5-7 + 剩余项 E1/W1/W2/S1/T1/M1 均完成，详见 `docs/CHANGELOG.md`）

> 来自 `docs/issues/` 反馈与扩展评估的候选池。下方条目按主题分组，并标注**必要性**（高/中/低，不做会持续踩坑或已影响正确性→高；明显改善体验→中；锦上添花→低）与**难度**（★☆☆ 半天内单点 / ★★☆ 一到数天跨模块 / ★★★ 结构性改造，动手前先出 ADR），便于排期。

### 推荐实现顺序（风险分层，非主题分组）

按「是否动 agent 主循环 / 是否改 sqlite schema」分层：先打低风险、自包含的特性，把最难回滚的留到最后。

1. **provider 接入优化**（[ADR-0026](adr/0026-provider-compat.md)）— ✅ 已交付（Compat 层 + 自动探测 + 配置覆盖 + 单测）
2. **系统提示词优化 / MEMORY 预算**（[ADR-0025](adr/0025-system-prompt-memory-budget.md)）— ✅ 已交付（trim + 配置预算 + 缓存 + /memory-compact）
3. **统一搜索 search**（[ADR-0023](adr/0023-unified-search.md)）— ✅ 已交付（统一 `search` 工具 + tavily/baidu/brave 内置 provider，单一 provider 路由，doubao 暂未实现）
4. **规划后执行 todo**（[ADR-0024](adr/0024-planning-todo.md)）— ✅ 已交付（单一 `todo` 工具 + 每会话落盘 + Runtime Context 注入 + WebUI 只读面板）
5. **ask_user**（[ADR-0022](adr/0022-ask-user-suspend-resume.md)）— ✅ 已交付（ask_user 工具 + ApprovalGate 复用 + 单 pending 续答 + /answer /cancel + WebUI 只读面板）
6. **skill 自管**（[ADR-0027](adr/0027-skill-authoring.md)）— ✅ 已交付（skill_create/skill_edit 工具 + 内置 skill-authoring 元 skill + 路径安全 + frontmatter 长度约束）
7. **/goal 长期目标**（[ADR-0021](adr/0021-goal-system.md)）— ✅ 已交付（goal.md 文件方案 + 每轮 Runtime Context 注入 + `goal` 工具 + 四 slash 命令 + WebUI GOAL 面板）

8. **环境探测 env**（[plans/2026-08-17-p5-remaining.md](plans/2026-08-17-p5-remaining.md) §E1）— ✅ 已交付（启动探测 + Runtime Context 注入 + `/env`）
9. **WebUI 会话历史**（§W1）— ✅ 已交付（Sessions tab：列表/详情/删除/导出）
10. **WebUI 模型探测**（§W2）— ✅ 已交付（Probe models + 勾选添加）
11. **敏感信息 .env**（§S1）— ✅ 已交付（保存自动转存 + 脱敏 + /migrate-secrets + 启动 warn）
12. **TTS**（§T1）— ✅ 已交付（tts 工具 + WebUI 音频播放；edge-tts 降级 v2）
13. **自然对话 MCP**（§M1）— ✅ 现状核实：ADR-0014 已把 MCP 工具接入主 agent 工具集

> 注：剩余项的评估、细化设计与实施记录见 [plans/2026-08-17-p5-remaining.md](plans/2026-08-17-p5-remaining.md)。

### 模型与 Provider

- [x] provider 接入优化：针对 Ollama / Llama.cpp 等 OpenAI 兼容端点的格式与行为做专项适配（必要性：**中** / 难度：★★☆）— ✅ 已交付
  - **现状**：`Provider` trait（`chat`/`chat_stream`/`native_tool_calling`/`detect_context_size`/`label`，`src/provider/mod.rs`）已干净；`openai_compat` 是 bare 实现（仅 role 序列化 + tool calls），**无** developer role / reasoning 归一化 / `max_completion_tokens` 字段切换 / finish_reason 推断 / streaming usage 兜底。llama.cpp 需 `--jinja` 才支持 tool calling。
  - **借鉴（pi `packages/ai`）**：`compat` 标志集 + `detectCompat()` 按 base_url 启发式探测 + 显式覆盖优先；llama.cpp 作为 extension provider 完整示例（context window 从 `/models` 探测、vision 探测、`maxTokens` 字段、`compat` 固定）。
  - **方向已确认**：给 `OpenAiCompatibleProvider` 加精简 `Compat` 结构（**不做** pi 的 25 开关全集，只覆盖 llaia 实际跑的本地端点子集）；**按 base_url 自动探测**（含 `ollama`→ollama 适配、`llama`→llamacpp 适配）+ `[provider.<id>].compat.*` 显式覆盖；优先覆盖 Ollama/Llama.cpp 高频差异（tool-call 格式、`reasoning→text` 降级、`max_completion_tokens`、streaming usage 落位、finish_reason 推断）。非破坏性（默认 `Compat::default()` 即当前 bare 行为）。
  - 详见 [plans/2026-08-14-provider-compat.md](plans/2026-08-14-provider-compat.md) / [ADR-0026](adr/0026-provider-compat.md)（必要性：**中** / 难度：★★☆）
- [x] 系统提示词优化：MEMORY 限定 token 预算并全量加载（hermes 式），SOUL/USER 永留全量（必要性：**中** / 难度：★☆☆）— ✅ 已交付
  - **现状（基座 `src/channels/cli.rs:474`）**：`# SOUL` + `# USER` + `# MEMORY` + `# WORKSPACE` 全量塞入 system prompt；仅此处拼装一次，存入 `agent.context.system`，所有频道共享；`init_system_meta` 缓存 `system_prompt_base` 供 skill 热重载重建。token 估算统一用 `chars()/4`（`src/agent/context.rs:48`）。
  - **决策（对齐 hermes，区别于 pi）**：MEMORY.md **全量加载**（不懒加载——llaia 是个人助理记忆，非 coding agent 的项目开发史），但设**可配置 token 预算**（默认 ~4000，复用 `chars()/4`）；超限时把最旧溢出段用 `compact_provider` **摘要压缩**、保留近期条目原文（无 compact_provider 时降级为硬截断保留近期）；SOUL/USER 仍永留全量（人格/画像，体积极小）。削减逻辑插在 `system_prompt_base` 拼装处，全频道自动生效；不引入 pi 式 tools 懒加载（llaia 工具集已精简）。
  - 详见 [plans/2026-08-14-memory-budget.md](plans/2026-08-14-memory-budget.md) / [ADR-0025](adr/0025-system-prompt-memory-budget.md)（必要性：**中** / 难度：★☆☆）

### WebUI 增强

- [x] session.db 会话历史在 WebUI 中可查询/修改，参考 AstrBot（必要性：**中** / 难度：★★☆）— ✅ 已交付
  - Sessions tab：列表（含消息数）+ 详情（消息 + tool_calls 折叠）+ 删除（cascade）+ 导出 JSON；只读 v1（编辑 v2 仅落 sqlite 不同步内存 Context，见 [plans/2026-08-17-p5-remaining.md](plans/2026-08-17-p5-remaining.md) §W1）
- [x] WebUI provider API 探测可用模型，点击添加到 models；添加按钮检查可用性，参考 AstrBot（必要性：**中** / 难度：★★☆）— ✅ 已交付
  - Config 页 "Probe models"：POST `/api/providers/:id/models` 探测 OpenAI 兼容 `GET /models`，勾选生成 model 条目走既有 `PUT /api/config`（见 [plans/2026-08-17-p5-remaining.md](plans/2026-08-17-p5-remaining.md) §W2）

### 安全

- [x] 敏感信息存储：api-key 等自动写入 `.env`，config 只保留环境引用；探讨二进制（如 db）存储避免明文（必要性：**高** / 难度：★★☆）— ✅ 已交付
  - 保存配置时自动转存 `.env`（成功才替换为 `${VAR}`，失败保留明文降级）+ `GET /api/config` 掩码 + `/migrate-secrets` 存量迁移（toml_edit 保注释）+ 启动扫描 warn；二进制存储决策为不做（key 管理无解），见 [plans/2026-08-17-p5-remaining.md](plans/2026-08-17-p5-remaining.md)（§S1）

### 生态与工具

- [x] 环境探测：本地 shell / python / node / rust / go 等环境探测，据情况提示 agent 优化行为（必要性：**中** / 难度：★☆☆）— ✅ 已交付
  - 启动探测一次（main agent，2s/命令 timeout）以 Runtime Context 尾部注入（复用 todo/goal 机制）+ `/env` 手动刷新，见 [plans/2026-08-17-p5-remaining.md](plans/2026-08-17-p5-remaining.md)（§E1）
- [x] skill 增强：让 agent 自管 skill（`skill_create`/`skill_edit` 工具 + 内置元 skill 引导），对齐 deepseek 元技能思路（必要性：**中** / 难度：★★☆）
  - **现状（已扎实）**：`src/skill/loader.rs` + `prompt.rs` 已实现 progressive disclosure（name+desc+path 进 prompt，全文 `file_read`）；`skills.json` 管 active 开关；目录名=标识；frontmatter name/description 校验；`resolve_skill_path` 防越权；WebUI 可创建（`default_skill_template`）。
  - **决策**：①**不做** npx-skills 的"搜索 + 自动安装"（用户更愿自己甄选 skill，避免 hermes 式繁杂 skill 集）；②加 `skill_create`/`skill_edit` **工具**——直接写/改 SKILL.md，因 skill 目录（`~/.workbuddy/skills/` 用户级、`{workspace}/.workbuddy/skills/` 项目级）在主 agent 文件作用域外，文件工具够不到；默认落**用户级**，可选 `scope:"project"` 切项目级；路径经 `resolve_skill_path` 校验防越权；③加一个**内置元 skill**（如 `skill-authoring`）引导 agent 如何按 llaia 约定（frontmatter 约束、progressive disclosure、路径安全）创建/审查/整理 skill；④补 frontmatter 长度/字符约束（对齐 pi）。
  - 详见 [plans/2026-08-14-skill-authoring.md](plans/2026-08-14-skill-authoring.md) / [ADR-0027](adr/0027-skill-authoring.md)（必要性：**中** / 难度：★★☆）
- [x] 自然对话给主 agent 添加 MCP 工具（必要性：**中** / 难度：★★☆）
  - **现状核实（2026-08-17）**：ADR-0014 交付时已把 MCP 工具接入主 agent 工具集（`cli.rs:482` `all_tools.extend(mcp_tools)` + WebUI `replace_mcp_tools` 热加载），「配置好 MCP server → 自然对话直接调用」已成立；agent 自主配置 MCP server 与描述增强两选项经评审不做（见 [plans/2026-08-17-p5-remaining.md](plans/2026-08-17-p5-remaining.md) §M1）

### 语音

- [x] TTS 服务接入、发语音（必要性：**低** / 难度：★★☆）— ✅ 已交付
  - `tts` 工具（OpenAI 兼容 `/audio/speech`，合成到 workspace/tts/，发送复用 `send_file`）+ WebUI 按扩展名渲染 `<audio>`；原拟 edge-tts（WS+Sec-MS-GEC 签名、不可测）降级 v2，QQ silk 转码不做，见 [plans/2026-08-17-p5-remaining.md](plans/2026-08-17-p5-remaining.md)（§T1）

### 目标系统

- [x] `/goal` 长期目标（跨多轮持续推进的同一目标，区别于 cron 的"定时触发一次"），参考 zeroclaw、hermes、**nanobot**
  - nanobot 做法：`/goal` 把目标写进 session metadata（`{status:"active", objective, ui_summary}`），激活后在 Runtime Context 注入目标文本；WebUI 通过 WS `goal_state` 事件展示进度（其"长目标 turn 豁免超时"经验证对 llaia 不适用，已作废，见 ADR-0021 决策 #4）
  - **方向已确认（2026-08-17 修订）**：单活跃 goal，但**持久化改为文件** `<config_dir>/workspace/goal.md`（默认 `~/.llaia/workspace/goal.md`，与 SOUL/USER/MEMORY 同处 agent 家目录），**不进 session schema**。理由：长期目标本就跨 session、不该绑单场会话；文件方案零迁移、goal 不进消息历史故天然无需压缩保留
  - 落地要点：frontmatter（status/created_at/updated_at）+ 正文（`# Goal` + `## Progress`），每轮注入 Runtime Context；配套 `/goal` 设目标、`/goal-list` 查看、`/goal-done` 收尾、`/goal-cancel` 取消；专用 `goal` 工具供 agent 自维护 `## Progress` / 内部标记完成；进度可视化（WebUI 只读面板 + CLI 状态行）
  - 详见 [plans/2026-08-14-goal.md](plans/2026-08-14-goal.md) / [ADR-0021](adr/0021-goal-system.md)（必要性：**中** / 难度：★☆☆）— ✅ 已交付（goal.md 文件方案 + 每轮 Runtime Context 注入 + `goal` 工具 + 四 slash 命令 + WebUI GOAL 面板）

### 任务编排与交互

- [x] `ask_user` 工具：agent 在执行中主动向用户抛澄清问题并**阻塞等待**回答，再继续（ADR-0022）— ✅ 已交付（复用 ApprovalGate 挂起-回传、单 pending 续答、/answer /cancel、feishu 入白名单、超时默认 300）
  - **方向已确认**：复用现有消息式审批流（`/ok` `/deny`）的"挂起—回传"机制；无 stdin 的频道（QQ/Telegram/微信/飞书）走消息回传，CLI 走 stdin 直问；一个 pending question 绑定当前 turn，支持超时/放弃与多问题排队
  - 详见 [plans/2026-08-14-ask-user.md](plans/2026-08-14-ask-user.md) / [ADR-0022](adr/0022-ask-user-suspend-resume.md)（必要性：**中** / 难度：★★☆）
- [x] 规划后执行工具：复杂任务先产出 todo 清单再逐步执行，执行中可勾选/增删
  - **方向已确认**：内置轻量 `todo` 工具（agent 自管 in-memory + 落盘 `todos.json`，可经 WebUI 查看），不外包给 todoist/MCP；并加"非平凡任务先规划"的 prompt 约定（可选 plan mode：列完先等用户确认再执行）
  - 详见 [plans/2026-08-14-todo-planning.md](plans/2026-08-14-todo-planning.md) / [ADR-0024](adr/0024-planning-todo.md)（必要性：**中** / 难度：★☆☆~★★☆）— ✅ 已交付（单一 `todo` 工具 + 每会话 `workspace/todos/<uuid>.json` 落盘 + Runtime Context 注入 + WebUI 只读面板）

### 搜索提供方扩展

- [x] 在当前 `tavily` 之外，增加更多搜索 API：豆包（Doubao）、百度（Baidu）、Brave
  - **方向已确认**：统一 `search` 抽象（一个 `search` 工具按配置选定**单一** provider，不串试不聚合；经 zeroclaw/nanobot 复核后的决策），tavily/百度/Brave 均**内置**实现，不走纯 MCP；豆包因仅 MCP/SigV4 接入暂未实现
  - 详见 [plans/2026-08-14-unified-search.md](plans/2026-08-14-unified-search.md) / [ADR-0023](adr/0023-unified-search.md)（必要性：**中** / 难度：★☆☆~★★☆）

---

## 工程约定

- 每个 Task 完成后跑 `cargo test` + `cargo clippy`
- 提交节奏：一个完整功能/修复链路验证通过后提交一次，不要每个 Task 都提交
- 遇到编译错误立即修，不要积累
- 详细实现计划放 `docs/plans/YYYY-MM-DD-<feature>.md`，设计规格放 `docs/specs/YYYY-MM-DD-<feature>-design.md`，架构决策放 `docs/adr/NNNN-<topic>.md`
- 阶段交付后，其完整勾选清单迁入 `docs/CHANGELOG.md`，本文件只保留「已交付阶段一览」索引 + 下一步计划
