# ADR-0025: 系统提示词 MEMORY 上限（hermes 式）

- 状态：Accepted
- 日期：2026-08-14
- 关联：plan.md §P5「系统提示词优化」；参考 pi `system-prompt.ts`、hermes memory 机制；`src/channels/cli.rs:474`、`src/agent/context.rs:48`、`src/agent/mod.rs:242`

## 背景 / Context

llaia 当前把 `# SOUL` + `# USER` + `# MEMORY` + `# WORKSPACE` **全量塞入** system prompt（`src/channels/cli.rs:474`），每次请求重发（KV cache 靠字节一致命中）。MEMORY.md 是 llaia 作为个人助理的**长期人格化记忆**（用户偏好、长期事实），会随使用持续变长。

pi 的 coding agent 把 memory 当作「项目开发史」做懒加载（首轮只索引，全文按需 `file_read`）——但 llaia 不同：MEMORY 是跨会话的人格记忆，应**全量加载**而非懒加载（懒加载反而增加 agent 取数摩擦，还需额外提示词引导）。hermes 的做法是「限定 token 数量 + 全量加载」：超限时**压缩**而非懒加载。

token 估算在 llaia 已统一为 `chars().count() / 4` 启发式（`src/agent/context.rs:48`），无真实 tokenizer——本 ADR 直接复用该启发式。

## 决策 / Decision

1. **MEMORY.md 全量加载**进 system prompt（不懒加载、不按需 `file_read`）。
2. 设**可配置 token 预算** `memory_token_budget`（默认 4000，单位复用 `chars()/4` 启发式），挂在 `[agent.<alias>]` 或 `[runtime]`。
3. **超限削减**：MEMORY 内容超过预算时，把最旧的溢出段用 `compact_provider` **摘要压缩**（保留近期条目原文），拼成「压缩前缀 + 近期原文」；**无 `compact_provider` 时降级为硬截断、保留末尾 N token**。
4. **SOUL/USER 永留全量**（人格/画像，体积极小，不计入 MEMORY 预算，不削减）。
5. 削减逻辑插在 `system_prompt_base` 拼装处（`cli.rs:474`），并同步更新 `init_system_meta`（`agent/mod.rs:242`）缓存的 base——skill 热重载重建时复用，避免每次重建重新摘要导致 system prompt 抖动。所有频道共享同一 `Arc<Mutex<Agent>>`，削减自动全频道生效。
6. **不引入** pi 式 tools 懒加载 / guidelines 去重——llaia 工具集已精简，收益低、复杂度高。

## 备选 / Alternatives

- **pi 式懒加载**（首轮只索引，全文按需 `file_read`）：否决——llaia MEMORY 是长期人格记忆而非项目开发史，懒加载增加取数摩擦且需额外提示词引导。
- **永远不削减**（无限增长）：否决——本地小上下文模型（Ollama）会被吃满。
- **每次超限都把压缩结果写回 MEMORY.md 文件**：否决为主路径（有隐式副作用、需谨慎）；改为提供显式 `/memory-compact` 斜杠命令复用 `src/memory/markdown.rs` 机制，让 agent **主动**持久化压缩结果。in-context 削减只影响当次运行副本，不改文件。

## 后果 / Consequences

- 正向：本地小模型也能稳定加载记忆；MEMORY 不丢长期事实（摘要保留关键信息）。
- 成本：超限时需一次 `compact_provider` 调用——结果可缓存（按 MEMORY.md 内容 hash），非每 turn 都调。
- 风险：摘要可能丢细节——靠「近期条目原文」兜底 + `/memory-compact` 手动整理缓解。

## 待办（实现计划）

见 [`plans/2026-08-14-memory-budget.md`](../plans/2026-08-14-memory-budget.md)。
