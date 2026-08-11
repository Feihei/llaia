# ADR-0019: 更聪明的上下文压缩

- 状态：Accepted
- 日期：2026-08-11

## 背景

P1 的 `Context::compact`（`src/agent/context.rs`）是「把旧消息整段丢给 LLM 摘要 +
保留最近 N 条」的简单策略。落地后暴露三个问题，正好对应 plan.md P4-c 第二条
「防止重要信息丢失、提高缓存命中、减少对 LLM 压缩的依赖」：

1. **违反 ADR-0004「首条用户消息留」**：首条用户消息一旦落入 `to_compress` 区就会被
   摘要掉，会话初始指令/上下文可能无声丢失。
2. **工具消息撑爆上下文**：`Role::Tool` 消息（base64 / 大段输出）整条留在 `history`，
   且原样进摘要 dump——既吃 token 又白费 LLM 调用。ADR-0004 明言「工具调用结果可丢」，
   完整内容已在 sqlite 留底。
3. **LLM 依赖过重、损害缓存**：只要超阈值就必调一次 LLM 摘要，没有更便宜的本地手段先兜底；
   且每次摘要都改写 `summary` 前缀，降低 KV cache 命中率。

参考：李博杰《深入理解 AI Agent》v1.2 §2.7.2（抽取式 / 重要性驱动的上下文管理——
先本地裁剪、再摘要）。

## 决策

`Context::compact` 升级为「**廉价抽取式先行 + 重要性锚点 + LLM 摘要兜底**」：

1. **廉价归一化 `cheap_normalize`（不调 LLM，每次必跑）**
   - 丢弃空消息（`Role::Tool` 除外，保留工具配对）。
   - 多模态图片降级为 `[图片]` 文本占位（原只在 `to_keep` 做，现提前到整段）。
   - 工具消息内容截断到 `TOOL_TRIM_CAP`（默认 500 字符），标注
     `…[已截断，完整结果见会话记录]`。
   - 连续重复的用户消息去重（只留一条）。
2. **预算门控（cheap-first）**：归一化后若 `history.len() <= keep_recent` 或
   `estimate_tokens() <= token_budget`，直接返回、不调 LLM——保留 `summary` 前缀稳定
   （KV cache 友好），也减少一次 LLM 调用。
3. **重要性锚点（ADR-0004 落地）**：首条用户消息若落在 `to_compress` 区，提出来前置到
   `to_keep`，且不进摘要 dump——永不被摘要掉。
4. **摘要 dump 裁剪**：`to_compress` 中的工具消息只给一行
   `[tool] (结果已归档) <前 80 字>`，不让大段工具输出进摘要 LLM（进一步缩小 LLM 输入、
   降低依赖）。
5. `summary` 增量合并沿用原 `[Later]` 拼接。

签名改为 `compact(&mut self, provider, keep_recent, token_budget) -> Result<bool>`
（`bool` = 是否真的调了 LLM）。调用方：
- agent 主循环 `self.context.compact(p, 6, self.context_size)`
- slash `/compact` `agent.context.compact(p, 6, agent.context_size)`

## 影响

- `src/agent/context.rs`：`compact` 重写 + 新增 `cheap_normalize` + 单测。
- `src/agent/mod.rs:382` / `src/commands/slash.rs:40`：补 `token_budget` 实参（= `context_size`）。
- **不新增 config key**：行为作为默认升级，无「先占」feature flag；`keep_recent` 仍为参数
  （调用点硬编码 6）。
- 不破坏 tool-call 配对：工具结果仍保留为短注，`assistant` 的 `tool_calls` 请求原样保留。

## 参考

- ADR-0004 §上下文压缩（首条用户消息留、工具结果可丢）
- 李博杰《深入理解 AI Agent》v1.2 §2.7.2
- ADR-0016（做梦：与压缩共用「抽取 → 合并」思路，但压缩是实时在线、做梦是闲时离线）
