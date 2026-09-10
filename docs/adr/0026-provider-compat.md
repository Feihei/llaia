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

### 2026-09-10：思考能力模型与 `disable_thinking` → `ThinkingIntent`（P1/P2）

随 [thinking-capability plan](../plans/2026-09-10-thinking-capability-model.md) 对五家在用
端点的 wire 实测，本 ADR 的 compat 面有两点演进：

**`disable_thinking: bool` 升级为 `ChatRequest.thinking: Option<ThinkingIntent>`。**
单一 bool 隐含「所有端点共享一种关方言」的前提被实测推翻：本地靠嵌套
`chat_template_kwargs`、DeepSeek 靠 `thinking:{type:"disabled"}`、Ollama/agnes 只认
`reasoning_effort:"none"`、GLM 部分网关直接 400。请求体组装改由
`resolve_thinking()` 在 provider 层唯一收口：显式声明（`[provider.<id>.<model>.thinking]`
的 `level_wire`/`off_wire`，值必须来自 wire 实测）走对应方言，声明未写的段（unknown）
沿 `compat.disable_thinking_template` 门走 legacy 兜底——旧 `disable_thinking` 的全部
语义由 `ThinkingIntent::None` + legacy 兜底承接，`auto` 请求体与旧版逐字节一致。

**「折回 content」与「独立 `reasoning_content` 字段」的二选一前提被服务端演进打破。**
llama.cpp 新 build（10867+）的 `--reasoning-format deepseek-legacy` 同时填 `content`（带
`<think>` 标签）与 `reasoning_content`，说明二者在服务端可以共存。框架侧仍维持二选一：
`reasoning_to_content=true` 折回通路不产出留存（避免同一段文本双份吃预算），独立字段通路
（P0 留存 + P2 `preserve` 回传）不折回。这也修正本 ADR 决策 1 的叙事：折回与否在当时
是「显示偏好」，实测后确认对某些部署形态它是功能必需——但正确解法是按部署声明方言，
而不是按模型家族猜。`guard_thinking_cap` 的长期方向是映射为 llama.cpp 的请求级
`--reasoning-budget`（token 级连续预算），只在端点不支持时退化成 abort + 重试
（登记为后续项，未在 P1/P2 分期内实现）。
