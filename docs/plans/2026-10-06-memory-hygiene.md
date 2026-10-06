# P9 记忆卫生：第 0 档（写入防重）+ 第 1 档（结构化压缩）+ 自动压缩触发

- 日期：2026-10-06
- 关联：[ADR-0033](../adr/0033-security-model.md)「A0 特许写入例外」、[plan.md](../plan.md) P9 记忆卫生三档、[ADR-0025](adr/0025-system-prompt-memory-budget.md)（预算）
- 范围：第 0 档 + 第 1 档 + 触发机制。SOUL/USER 压缩维持留观（人格文件不经 sidecar 改写）。

## 第 0 档 · memory_write 写入时防重

- `src/memory/hygiene.rs::normalize_entry`：小写 + 标点/符号/空白折叠为单空格（CJK `is_alphanumeric` 天然覆盖）。多数膨胀是字面重复，纯函数比对，无需 LLM。
- 两条写通路（`MemoryWrite::execute` 主 agent 共享工具 + `write_memory_entry` 实例路由）在追加前把新 entry 归一化后与现有条目逐一比对；命中 → 返回 `already remembered: <entry>`（Ok，不落盘、不报错）。

## 第 1 档 · compress_memory 结构化升级

`memory/markdown.rs::compress_memory` 从「裸 LLM 单发 + 盲写」升级为三段管线（新模块 `src/memory/hygiene.rs` 承载纯函数）：

1. **确定性预检**：`parse_memory` 按行解析契约（`- [YYYY-MM-DD] text`）——头部行（`# MEMORY` / 注释）与不合规行原样保留（不丢数据、不进 LLM）；`dedupe_entries` 归一化去重（保留最早条目）。去重后若已达预算（自动触发传入 budget 时）→ **直接写回、跳过 LLM**（tier-0 哲学：字面重复占大头）。
2. **LLM 只做语义合并**：输入输出均为条目列表（头部不进 prompt）；提示词明令「禁止发明新条目、复用输入措辞」。
3. **输出结构校验**（`validate_compact_output`）：每个非空行必须匹配契约，且**可溯源**到至少一条输入——归一化互含或字符二元组 Jaccard ≥ 0.3（语义合并复用源词 → 高重叠；凭空行近乎零重叠）。任一违规 → **整体拒绝**（不部分采用），带原因重试一次，仍败 → 原文件保留 + Err（被骗压缩器加不进任何新内容）。

签名变化：`compress_memory(..., budget: Option<usize>) -> Result<String>`（返回报告；手动 `/memory-compact` 传 None 恒走 LLM，写前备份与快照行为不变）。

## 自动压缩触发

- 信号：启动构建 main agent 时 `estimate_tokens(raw_memory) > memory_token_budget`（ADR-0025 同款 chars/4 启发式，即「trim 实际开始丢弃内容」），且 MEMORY 非模板（bootstrap 未完成不压）。
- 动作：`tokio::spawn` 后台任务——`GuardCtx::snapshot_target` 写前快照（判据 1 可恢复性 → 免交互审批）→ `compress_memory(budget=Some)` → 成功/失败均 tracing 留痕；无 provider（降级模式）跳过 + warn。每进程至多一次（随启动评估）；压缩结果与 `/memory-compact` 同语义——重启后才进 system prompt（既有缓存性质）。仅 main MEMORY；实例私有 MEMORY 不自动压。

## 任务清单

1. `src/memory/hygiene.rs`：normalize / parse / dedupe / traceable / validate + 单测
2. `tools/memory.rs` 两通路接入第 0 档 + 单测
3. `markdown.rs::compress_memory` 三段管线重写（budget 参数、重试、报告）+ mock provider 单测（伪造行拒绝、合规合并写回、达标跳 LLM）
4. cli.rs 自动触发 + slash.rs `/memory-compact` 适配新签名
5. 文档回填：plan.md 三档勾选、ADR-0033 §1 注、AGENTS.md、本文件交付注记

## 验收

- fmt / clippy / 全量 test 绿。
- 手工路径：`/remember` 重复条目 → "already remembered"；`/memory-compact` 在有字面重复时先确定性去重；伪造 LLM 输出 → 压缩失败、原文件保留。

## 交付注记（2026-10-06）

全量交付。实现与计划的偏差：

- `validate_compact_output` 返回 `anyhow::Result<Vec<String>>`（Err 文本即拒绝原因，供重试提示与最终报错）。
- 不合规行按「`#` 开头 / `<!--` 开头」分桶为头部（保留）、其余为 malformed（同样保留但不进 LLM）——实测唯一现存 malformed 形态是用户手改的杂行，原样保留比丢弃诚实。
- 确定性路径的「已达预算」判定 = 去重重建后 `estimate_tokens <= budget`；重建文本与原文逐字节相同（零移除）时不写盘。
- 自动触发挂在 `build_single_agent`（main），须在 `Agent::new` 消费 provider 之前 clone Arc——放在 guard_ctx/web_fetch_gate 构建之后。
- runner 侧零改动；`/remember` 经共享 memory_write 工具自动获得第 0 档防重。

P9 进度：Phase 0/1/2 + 记忆卫生第 0/1 档全部落地。剩余：Phase 3（T2 受限进程，部署级）、Phase 4（闸门复审减法）、SOUL/USER 压缩（留观）。
