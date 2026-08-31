# ADR-0026: Provider Compat 层（Ollama / Llama.cpp）

- 状态：Accepted
- 日期：2026-08-14
- 关联：plan.md §P5「provider 接入优化」；参考 pi `packages/ai`；`src/provider/mod.rs`、`src/provider/openai_compat.rs`

## 背景 / Context

`Provider` trait（`chat` / `chat_stream` / `native_tool_calling` / `detect_context_size` / `label`，`src/provider/mod.rs`）已干净。`OpenAiCompatibleProvider`（`src/provider/openai_compat.rs`）是 **bare 实现**：仅做 role 序列化 + tool calls 透传，**无**以下适配：

- developer role 处理（OpenAI 新规范把 system 拆到 developer）
- reasoning / thinking 内容归一（`reasoning_content` 未折回 `content`）
- `max_completion_tokens` 字段切换（部分端点只认 `max_tokens`）
- streaming usage 兜底（流式时不解析 usage）
- finish_reason 推断（tool call 后未正确标 `tool_calls`）

本地端点尤其明显：Ollama 对 developer role / reasoning 处理与 OpenAI 不同；Llama.cpp 需 `--jinja` 才支持 tool calling，且 usage / finish_reason 字段名可能偏差。pi 用 ~25 个 `compat` 开关 + `detectCompat()` 按 base_url 启发式探测解决，但 llaia 只跑少量本地端点，不需要全集。

## 决策 / Decision

1. 给 `OpenAiCompatibleProvider` 加**精简 `Compat` 结构**（子集，非 pi 25 开关全集）：
   - `supports_developer_role: bool` — `false` 时把 developer 内容并入 system
   - `reasoning_to_content: bool` — `true` 时把 `reasoning_content` / `thinking` 折回 `content`（避免某些端点丢思考）
   - `max_tokens_field: MaxTokensField` — `MaxTokens` | `MaxCompletionTokens`
   - `streaming_usage: bool` — `true` 时流式也解析 usage；否则回退估算
   - `infer_finish_reason: bool` — `true` 时从 tool_calls 是否存在推断 `finish_reason = tool_calls`
   - `requires_assistant_after_tool: bool` — `true` 时多轮 tool 结果后补一条空 assistant 占位（Ollama 某些版本需要）
2. **自动探测**：`detect_compat(base_url)` 按 host 子串——含 `ollama` → ollama 预设、含 `llama` / `llamacpp` → llamacpp 预设；其余默认 `Compat::default()`（= 当前 bare 行为，**零回归**）。
3. **显式覆盖**：`[provider.<id>].compat.*` 字段可手动覆盖任一开关（优先级高于探测）。
4. **非破坏性**：默认 `Compat::default()` 等同现状，现有 Ollama / LMStudio 用户无感。
5. 首版覆盖 **Ollama + Llama.cpp** 高频差异；后续端点（vLLM / KoboldCpp …）按需加预设，不预建 25 开关。

## 备选 / Alternatives

- **直接 fork 25 开关全集（pi）**：否决——llaia 只跑少量本地端点，维护成本高、AI 误配风险大。
- **每 provider 一个独立 struct 实现**：否决——OpenAI 兼容度极高，一个 `Compat` 位集合 + 探测足够，避免代码膨胀。

## 后果 / Consequences

- 正向：本地端点 tool calling / reasoning / usage 正确；零配置（base_url 探测）。
- 负向：新增一个 `Compat` 结构与探测函数，需测试覆盖（Ollama / Llama.cpp mock 响应）。

## 待办（实现计划）

见 [`plans/2026-08-14-provider-compat.md`](../plans/2026-08-14-provider-compat.md)。

## 修订记录

### 2026-09-01：`reasoning_to_content` 默认值改为 `false`

**现象**：llama.cpp 端点跑 Qwen3 思考模型时，QQ 频道会原样吐出大段思考内容；显式写
`[provider.llamacpp.compat] reasoning_to_content = false` 才恢复正常。

**根因**：本 ADR 决策 1 中「折回 content（避免某些端点丢思考）」的前提对 llama.cpp / Ollama
并不成立——二者的 OpenAI 兼容层在思考模型下 `content` **照常返回正式回答**，`reasoning_content`
只是**额外**的思考流。折回它的唯一效果是把思考混进可见文本、context 与 sqlite 会话历史
（进而污染压缩素材）。

更糟的是两层行为相反：模型把思考内联成带 `<think>` 标签的文本时，本就被 `ToolCallStreamParser`
剥掉（`src/tool_call/stream_parser.rs`）；而端点把思考拆到 `reasoning_content` 字段时反而被折回显示。
即「带标签的隐藏、分字段的显示」。

**决策**：`Compat::ollama()` / `Compat::llamacpp()` 预设的 `reasoning_to_content` 由 `true` 改为
`false`。`Compat::default()` 本就是 `false`，故未命中预设的端点（bare / LMStudio / 线上）零回归。
确实需要折回的端点仍可用 `[provider.<id>.compat] reasoning_to_content = true` 显式开启，
覆盖优先级（决策 3）不变。

**连带收敛**：per-model 表 `model_folds_reasoning` 一并删除。它对 `deepseek-reasoner` /
`deepseek-r1` / `deepseek-reasoning` / `kimi-k` 强制开启 `reasoning_to_content`，属同一类问题
（这些端点同样是 `content` 带正式回答）。该规则源自 nanobot `_MODEL_THINKING_STYLES`，而 nanobot
原意是「R1 走 `reasoning_content` 字段名而非 `reasoning`」（**字段选择**），并非「折回可见文本」；
llaia 的流解析（`openai_compat.rs`）本就同时读 `reasoning_content` 与 `thinking`、从不读
`reasoning`，故这条规则在 llaia 里没有字段选择可言，唯一效果就是强制把思考折回可见文本
——纯 bug 放大器。删除后 per-model 表只剩 `max_tokens_field` 一项。
