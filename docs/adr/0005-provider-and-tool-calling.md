# ADR-0005: Provider 抽象与工具调用协议

- 状态：Accepted
- 日期：2026-07-21

## 背景

LLAIA 优先支持本地 OpenAI 兼容端点（Ollama/Llama.cpp/LMStudio），
但这些端点对 function calling 的支持参差，需要决定工具调用协议。

参考 zeroclaw 的混合策略（原生优先 + 标签降级），LLAIA 需要决定 P1 裁剪到什么程度。

## 决策

### Provider 抽象

- P1 只实现 `OpenAiCompatible` 一种 provider
- Provider trait 含 `native_tool_calling: bool` 能力声明
- 配置 schema 命名式 `[provider.<id>]`，P1 只认 `default`
- 模型配置放 toml，P2 进 Web 面板可视化修改
- P1 **不做流式输出**（SSE），P2 再加

### 工具调用协议

**混合策略，分两层**（比 zeroclaw 简化）：

1. **原生优先**：`native_tool_calling = true` 时走 OpenAI function calling 协议
2. **标签降级**：`native_tool_calling = false` 时，system prompt 注入协议说明，
   模型用 `<tool_call>{"name":"...","arguments":{...}}</tool_call>` 包裹调用，
   回复文本由解析器抽取

### P1 砍掉的 zeroclaw 能力

- `StreamTextGuard`（流式标签抑制）：P1 不做流式，无需
- 流后文本兜底解析：P1 不做流式，无需
- 多 provider 并发路由：P1 单 provider，无需
- `reasoning_content` 透传：P1 schema 预留字段，但不强制实现

### 配置示例

```toml
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"
api_key = ""
model = "qwen2.5:7b"
native_tool_calling = true    # false 则走 <tool_call> 标签降级
```

## 影响

- Provider trait 设计要考虑 P2 扩展（多 provider、流式），但 P1 只实现最小集
- 工具调用解析器需要独立模块（`src/agent/tool_call.rs`），支持原生 JSON 和标签两种输入
- 配置项 `native_tool_calling` 是用户调试本地模型的关键开关

## 参考

- grilling 第四轮 Q21–Q22
- zeroclaw `crates/zeroclaw-api/src/model_provider.rs`
- zeroclaw `crates/zeroclaw-tool-call-parser/src/lib.rs`
