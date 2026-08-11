# 上下文注入策略

**日期：** 2026-08-11
**状态：** 已落地（P4-a），本文为策略文档化
**关联：** `ADR-0004-session-and-context.md`、`ADR-0017-timezone-injection.md`；实现见 `src/agent/context.rs`、`src/agent/mod.rs`、`src/time.rs`

---

## 1. 背景与目标

LLM 是无状态、无时钟的：它看不到"现在几点"、不知道上一轮对话被压缩成了什么、也不会自动记住系统设定。LLAIA 必须在每轮请求时，把"当前应当知道的上下文"重新组装成一条消息序列送给 provider。

本文把这套**上下文注入策略**从代码里提炼成书面约定，明确：

- 消息序列的**组装顺序**与各段语义
- 为什么把"状态栏"挂在尾部而非写入历史（KV cache 友好）
- 时区 / 运行时状态（ADR-0017）如何注入
- 压缩（compaction）触发与保留策略

目标读者：任何要改 `Context`、`to_messages`、system prompt 或压缩逻辑的人，先读本文，避免破坏前缀稳定性。

---

## 2. 消息序列组装顺序

`Context::to_messages(&self, tz)`（`src/agent/context.rs`）产出的顺序如下，**严格自上而下**：

| # | 段 | 角色 | 来源 | 是否持久化 |
|---|----|------|------|-----------|
| 1 | System prompt 前缀 | `system` | `Context.system`（`SOUL.md` + `USER.md` + `MEMORY.md` + 注入的 Skills，在 `agent` 构造时拼接） | 是（构建一次，整轮不变） |
| 2 | 历史摘要 | `system` | `Context.summary`（压缩产物，见 §5） | 是 |
| 3 | 对话历史 | `user` / `assistant` / `tool` | `Context.history`（`push` 累积的逐轮消息） | 是（落 sqlite） |
| 4 | 运行时状态栏 | `user` | `time::status_bar(tz)`（ADR-0017，每轮现算） | **否**（仅本轮注入，不写入 history） |

要点：

- **第 1 段是唯一真正由 `agent` 在启动时拼好的 system 前缀**，包含人格、用户画像、长期记忆、技能。它的内容在一轮对话内**不变**——这是 KV cache 命中的关键。
- 第 4 段状态栏用 `user` 角色挂上（而非 `system`），因为模型更习惯在 user 消息里接收"当前指令/上下文"类信息；且它必须尾随在 history 之后，作为权威时钟。
- 状态栏**不写入 `history`**：它每轮重新计算，若落库会让旧状态栏永久污染后续对话。

---

## 3. KV-cache 友好的前缀稳定性

长驻 daemon（serve 模式）跨多轮对话复用同一份 system 前缀时，provider 端的 KV cache 能否命中，取决于**前缀字节是否逐轮一致**。

策略：

- 静态内容（SOUL/USER/MEMORY/Skills + 摘要 + 历史）在 `to_messages` 里**逐字节不变**，只有第 4 段状态栏随当前时间变化。
- 因此 `a[..a.len()-1]` 与 `b[..b.len()-1]`（去掉各自尾部状态栏）必须完全相等——`context.rs` 的 `test_system_prefix_stable_across_turns` 专门卡这条回归。
- 任何想"在 system 前缀里塞动态值（日期、随机种子、当前用户消息摘要）"的改动，都会破坏前缀稳定性，应当改为挂在尾部状态栏或独立段。

---

## 4. 运行时状态栏与时区（ADR-0017）

`time::status_bar(tz)`（`src/time.rs`）产出形如：

```
[Runtime status] Current time: 2026-08-11 10:44 Tuesday (Asia/Shanghai). Afternoon working hours.
This line is regenerated every turn and is the authoritative clock — prefer it over dates mentioned earlier in the conversation or in your training data.
```

设计要点：

- **模型无时钟**：system prompt 在进程启动时拼好一次，daemon 跑久了会一直"说昨天的日期"。状态栏每轮重写，给出权威当前时间。
- **时区来源 `[runtime].timezone`**（IANA 名，如 `Asia/Shanghai`）：`None` 跟随宿主机本地时区（与旧行为一致，无回归）；非法值在 `Config::load` 里 warn 并降级为 `None`。
- 状态栏同时承担"运营提示"（`day_hint`：深夜提醒勿扰、工作时段正常语气等），是 §2 第 4 段的实现。
- `tz` 在 `handle_message_streaming` 每轮开头整轮快照一次（`let tz = self.timezone().await;`），读取来自 `agent.live_config`（serve 模式下与 WebUI 共享，支持热更新，见 ADR-0017）。

---

## 5. 压缩（Compaction）策略

当 `estimate_tokens() / context_size > context_threshold`（默认 0.7）时触发压缩：

- 保留最近 `keep_recent` 条消息（默认 3），其余送入 summarizer。
- 旧摘要与新摘要拼接（`old\n\n[Later]\nnew`），保留在 `Context.summary`，作为 §2 第 2 段。
- 保留的 `keep_recent` 条里若含图片（Multimodal），降级为 `[图片]` 文本占位，省 token——图片信息已进摘要。
- 压缩用的模型：`runtime.compact_model` 指定更便宜的模型；未设则复用主 agent 的 provider。
- 压缩结果与历史都落 sqlite（`sessions.db`），是会话的 source of truth；上下文超阈值时旧消息从内存移除但 sqlite 留底。

---

## 6. 实现约束速查

- 改 `to_messages` → 必须保持 §2 顺序，且**不能**把动态内容塞进第 1 段前缀。
- 改 `time::status_bar` / `time::now` → 保持 `None` 降级到宿主机时区，不破坏无回归约定。
- 新增"每轮都变"的注入内容 → 走 §2 第 4 段（尾部状态栏）或独立的尾随段，不要进 `Context.system`。
- 回归保护：`context.rs` 的 `test_status_bar_is_last_and_not_persisted` 与 `test_system_prefix_stable_across_turns` 卡住"状态栏在尾、不污染 history、前缀稳定"三条不变量。
