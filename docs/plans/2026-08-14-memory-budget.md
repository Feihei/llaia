# 实现计划：系统提示词 MEMORY 上限（hermes 式）

> 关联 ADR：[0025-system-prompt-memory-budget.md](../adr/0025-system-prompt-memory-budget.md)
> 日期：2026-08-14

## Goal

MEMORY.md **全量加载**进 system prompt，但受可配置 token 预算约束；超限时最旧溢出段经 `compact_provider` 摘要压缩（无则硬截断保留近期），SOUL/USER 永留全量。削减插在 `system_prompt_base` 拼装处（`cli.rs:474`），全频道生效。

## Architecture

- 复用 `src/agent/context.rs::estimate_tokens`（`chars()/4`）做 token 估算。
- 新增 `trim_memory_to_budget(memory: &str, budget: usize) -> String`：
  - 若 `estimate_tokens(memory) <= budget` → 原样返回。
  - 否则：按 `# ` 标题 / 空行分条目，最旧（前）溢出段交给 `compact_provider` 摘要成一段压缩文本，拼回近期原文；无 `compact_provider` → 硬截断保留末尾 `budget` token。
  - 结果按 MEMORY.md 内容 hash 缓存，避免每 turn 重摘要导致 system prompt 抖动。
- `init_system_meta`（`agent/mod.rs:242`）缓存的 base 用削减后的 memory，skill 热重载重建时复用。

## Tech Stack

Rust（llaia 单 crate）。复用 `compact_provider`、`context.rs::estimate_tokens`。

## 文件结构

- `src/agent/mod.rs`（或新 `src/memory/trim.rs`）：`trim_memory_to_budget`。
- `src/channels/cli.rs:474`：拼 `system_prompt_base` 时套用 trim。
- `src/config.rs`：`[agent.<alias>].memory_token_budget`（默认 4000）或 `[runtime]`。
- 可选 `src/commands/slash.rs`：`/memory-compact` 显式持久化压缩（复用 `src/memory/markdown.rs`）。

## 分步 Task

1. [ ] 加配置 `memory_token_budget`（默认 4000），CLI / serve 读取并传入 agent。
2. [ ] 实现 `trim_memory_to_budget`；单测：不超限原样返回；超限走摘要 / 截断两条路径正确（mock `compact_provider`）。
3. [ ] `cli.rs:474` 拼 `system_prompt_base` 时套用 trim；`init_system_meta` 用削减后 base。
4. [ ] 加缓存（MEMORY.md 内容 hash → 削减结果）。
5. [ ] `/memory-compact` 斜杠命令（可选，复用 `markdown.rs` 写回 MEMORY.md）。
6. [ ] 集成测试：长 MEMORY 下 system prompt 不超预算；SOUL/USER 内容完整。
7. [ ] 文档：AGENTS.md 记 MEMORY 上限约定；CHANGELOG 更新。

## 自查

- [ ] `cargo test` + `cargo clippy` 绿
- [ ] 本地 Ollama 小上下文跑通，MEMORY 不撑爆
- [ ] SOUL/USER 内容在 system prompt 中完整出现
- [ ] 超限时削减结果稳定（同内容不产生抖动）
