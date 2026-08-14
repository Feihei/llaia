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
