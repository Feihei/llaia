# ADR-0030: 移除「做梦」记忆自动整理机制

- 状态：Accepted
- 日期：2026-08-31
- 关联：取代 [ADR-0016](0016-dream.md)（做梦整体设计）；终结 [ADR-0029](0029-dream-stage2-memory-patch.md)（其 Proposed 方案不再实现）

## 背景 / Context

「做梦」最初是对 openclaw / nanobot 的模仿：闲时两阶段整理 `MEMORY.md`（蒸馏增量历史 → 合并去重改写记忆）。运行一个多月后的复盘结论：

1. **效用与复杂度不成比例**。为「记忆自动更新」这一个特性，额外引入了：两阶段管线与专用编排入口（`run_dream`）、sqlite 游标（`last_dream_message_id` / `messages_after`）、空闲门控、`CronTask` 的 `kind` / `idle_minutes` 两个仅它使用的字段、MEMORY 备份/回滚/形状校验/diff 推送整套安全兜底、agent 侧 `run_isolated_turn(_with)` 专用变体（含 `disable_tools` 临时位）。安全兜底的规模已经接近功能本体——这是机制性风险过高的信号。
2. **实跑已证伪「整体重写」的安全性**（ADR-0029 记录的事故）：LLM 一次输出的作用域是整份长期记忆，形状校验挡得住塌方、挡不住语义损坏；且坏记忆会随 `morning_news` 等下游 cron 任务扩散。
3. **价值错位**。记忆系统的价值主要在写入侧写了什么——`memory_write` 在主会话的实时写入已覆盖最有价值的部分；dream 额外买到的只有去重与去陈化，而单用户场景下人工一个月看一次的成本近零。
4. **违背极简 harness 原则**。harness 层不应内置「无人值守时自动改写用户长期记忆」这种高风险副作用路径；需要自动捕获时，一个普通 cron agent 任务即可达成，无需 harness 提供专用机制。

## 决策 / Decision

**整体移除 dream 机制**，不保留任何自动改写 `MEMORY.md` 的内置路径：

- 删除 `src/cron/dream.rs`（两阶段管线）与 `src/memory/dream.rs`（备份/回滚/校验/diff）；`write_memory_atomic` 迁至 `src/memory/mod.rs`（`memory_write` 工具继续使用）。
- 删除 `/dream`、`/dream-rollback` 斜杠命令。
- `CronTask` 删除 `kind` / `idle_minutes` 字段与内置任务播种逻辑；cron 任务只剩 agent / tools 两种普通模式。
- sqlite 删除 dream 游标与 `messages_after` / `last_user_message_time`（仅 dream 使用）。
- agent 删除 `run_isolated_turn` / `run_isolated_turn_with` 与 `disable_tools` 临时位（仅 dream 使用）；`fork_for_isolated`（普通 cron agent 模式在用）与 `disable_thinking` 保留。

**替代方案**（用户自建，harness 零参与）：

- 需要自动捕获时：建一个普通 agent 模式 cron 任务，prompt 让 agent 用 `memory_research` 检索近期历史、把值得长期保留的事实**推送给人审阅**——是否入记忆由人（或人批准的 `memory_write`）决定。
- 记忆膨胀时：`/memory-compact` 手动压缩（写前备份）。

## 后果 / Consequences

- (+) harness 层不再有无人值守改写长期记忆的路径；`MEMORY.md` 的变更只剩确定性写入（`memory_write` / `/remember`）与用户显式命令（`/memory-compact`）。
- (+) 删除约 500 行管线 + 兜底代码，cron 配置面收窄（无 `kind` / `idle_minutes`）。
- (−) 失去自动去重/去陈化：长期记忆的整理变为手动（`/memory-compact` 或直接编辑文件）。对单用户可接受。
- 存量数据无需迁移：`sessions.db` 的 `last_dream_message_id` kv 行成为无害残留；`workspace/dream_draft.md`、`workspace/MEMORY.backups/` 可手动清理；cron.toml 里如有 `kind = "dream"` 任务段，该 key 会被 serde 忽略、任务退化为普通 agent 任务（prompt 为空会在校验时被拒，需手删该段）。

## 备选 / Alternatives

- **按 ADR-0029 改结构化 patch**：把破坏面缩到条目级，但管线复杂度不减反增（新增合并器），且治标不治本——harness 仍持有自动改写记忆的能力。
- **保留管线、仅去掉自动触发**（手动 `/dream`）：事故链主要来自夜间无人值守，手动触发确实可控；但意味着整套管线与兜底代码为零使用频率保留，违背不留死代码的约定。
- **追加-only 蒸馏**（Mem0 式：dream 只追加不改写）：风险可控，但 harness 仍需维护专用管线，且去重完全推给人工——与普通 cron 任务方案相比无增量收益。
