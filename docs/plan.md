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

### 🔧 稳定性修复（可直接实施）

- [ ] `/provider N` 切换后 fallback 规则失效（必要性：**高** / 难度：★☆☆）
  - **根因**：`switch_provider`（`src/commands/slash.rs:593`）拿到 `provider_from_ref` 后直接 `reload_provider(Some(provider))` 裸替换，`FallbackProvider` 链被丢弃。
  - **修法**：切换时若 `[agent.<alias>].fallback` 非空，重建 `FallbackProvider{主=选中模型, 备=fallback 链}`；复用 `fallback.rs` 既有 mock 测试基建补回归。
  - > 🚧 2026-08-21 已实现（`switch_provider` 改走 `build_provider_chain` + `Provider::kind()` 标识 + 3 个回归测试），待本地 `cargo test --lib` 验证。
- [ ] `/stats` 返回的上下文长度数据有误（必要性：**中** / 难度：★☆☆）
  - **根因**：`estimate_tokens`（`src/agent/context.rs:80`）只统计 `system + summary + history` 的 `chars/4`，漏掉实际发送给 provider 的 tool definitions JSON schema、`env_state` 注入、goal/todo runtime context 与每条消息 role 开销，显示值显著低于真实发送量。
  - **修法**：把这些注入源纳入估算口径（与 compact 判定共用同一函数，顺带修正自动压缩触发时机）。
  - > 🚧 2026-08-21 已实现（`estimate_tokens` 基于 `to_messages` 全量文本 + 8 token/条结构开销；`/stats` 另加 tool defs 序列化估算），待本地 `cargo test --lib` 验证。

### 🧪 待验证根因后修复

- [ ] QQ 频道发图失败 `40093006 请求参数错误`（必要性：**高** / 难度：★☆☆）
  - **现象**：upload 阶段 `POST /v2/users/{openid}/files` 返回 40093006（`qq.rs:476`），日志 `2026-08-21T07:17:19Z`，文件 `cover_aly.png`。
  - **根因（2026-08-21 实测确认）**：腾讯 v2 富媒体接口已改为 **JSON body**（`file_type` + `file_data`(base64)/`url` + `srv_send_msg`），**不再支持 multipart 文件上传**；旧实现发 multipart `file` 字段被拒（复现时 multipart 一律 40093007 "富媒体文件下载失败"，与日志 40093006 同属接口协议不匹配）。见官方文档 [富媒体消息](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/message/send-receive/rich-media.html)。
  - **修法（已实现）**：`qq.rs` 上传段改为 JSON + base64（`file_data`，`srv_send_msg=false`，随后仍走 msg_type=7 发送）；用真实 `cover_aly.png`（2.7MB）实测 HTTP 200 返回 file_info 验证通过。
  - > 🚧 2026-08-21 已实现（`qq.rs` upload 改 JSON+base64），已用真实凭据实测上传成功；待本地 `cargo test --lib` 编译验证。

### 🧩 待 grill 明确后立项（需求/设计类）

- [ ] 模型 reasoning 设置：对话临时修改参数（必要性：待定 / 难度：★☆☆）
  - 现状：`disable_thinking` 仅内部 isolated turn 使用（`agent/mod.rs:363`），用户不可控。
  - 待明确：命令形态（`/reasoning on/off`？）、参数集合（thinking/temperature/max_tokens？）、生效范围（当前会话 or 持久）、对哪些 provider 生效。
- [ ] 环境发现在 WebUI 中可视（必要性：**低** / 难度：★☆☆）
  - 现状：`envprobe` 已注入 Runtime Context（`/env` 可刷新），但 `web/mod.rs` 未暴露给前端。
  - 待明确：展示位置（设置页/侧栏/会话头部）、只读 or 带刷新按钮、是否按 agent 分开展示。
- [ ] 会话主题自动总结（必要性：**中** / 难度：★★☆）
  - 现状：无代码基础；落点可选 `sessions` 表加 title 字段。
  - 待明确：触发时机（压缩时顺带？固定 N 条？）、存储位置、展示处（WebUI 会话列表？CLI `/stats`？）、用 compact provider or 主 provider、失败降级。
- [ ] deepseek / glm / kimi 等 provider 针对性优化，探讨必要性（必要性：待定 / 难度：★☆☆~★★☆）
  - 现状：`compat.rs::detect` 仅覆盖 ollama/llamacpp 预设（`reasoning_to_content` / `streaming_usage` 等），deepseek / glm / kimi 走 bare 行为。
  - 待明确：是遇到具体报错（哪个 provider 什么现象）还是预防性优化？目标清单（deepseek `reasoning_content` 处理、glm `thinking` 字段等）？
- [ ] `memory_research` 工具：跨 session 搜索历史记忆（必要性：**中** / 难度：★★☆）
  - 现状：`sessions/messages/tool_calls` 已在 sqlite（`memory/sqlite.rs`），FTS5 技术路线可行（rusqlite 加 `fts5` feature）。
  - 待明确：搜索范围（仅 messages？含 tool_calls 结果？含 MEMORY.md？）、返回形态（N 条 + 所属 session？）、暴露为模型可调工具 or slash 命令、结果上限与隐私边界。
- [ ] 检查清理基本架构 / agent loop（必要性：待定 / 难度：★★☆~★★★）
  - 现状：无动机描述，范围完全模糊。
  - 待明确：触发点（loop 卡死？上下文爆炸？性能？）、清理范围（`agent/mod.rs` 主循环？`runner`？）、期望产出（重构 or 文档梳理）。

---

## 工程约定

- 每个 Task 完成后跑 `cargo test` + `cargo clippy`
- 提交节奏：一个完整功能/修复链路验证通过后提交一次，不要每个 Task 都提交
- 遇到编译错误立即修，不要积累
- 详细实现计划放 `docs/plans/YYYY-MM-DD-<feature>.md`，设计规格放 `docs/specs/YYYY-MM-DD-<feature>-design.md`，架构决策放 `docs/adr/NNNN-<topic>.md`
- 阶段交付后，其完整勾选清单迁入 `docs/CHANGELOG.md`，本文件只保留「已交付阶段一览」索引 + 下一步计划
