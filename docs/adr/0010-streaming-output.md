# ADR 0010: 流式输出

日期：2026-07-22
状态：Accepted

## 背景

v1.5 QQ channel 完成后，所有 channel 的 LLM 回复都是一次性返回。CLI 体验差（长回复要等十几秒），QQ 虽然内部流式但用户要等全部生成完。未来 Web channel 必须流式。

## 决策

采用三层流式管道：

1. **Provider 层**：`chat_stream() -> BoxStream<Result<StreamEvent>>` 返回 async Stream，产出 `TextDelta` / `ToolCall` / `Done` / `Error`。`chat()` 保留，内部 collect。
2. **Agent 层**：`handle_input_streaming(input, channel, mpsc::Sender<TurnEvent>)` 消费 provider stream，转成高层 `TurnEvent`（`Chunk` / `ToolStart` / `ToolResult` / `Done` / `Error`）推给 mpsc。标签降级模式用 `ToolCallStreamParser` 状态机过滤 `<tool_call>` 标签。
3. **Channel 层**：在 `run()` 内部建 mpsc + spawn agent 调用 task，消费 receiver 按协议输出。
   - **CliChannel**：实时打印 Chunk，打字机效果
   - **QqChannel**：累积 Chunk 到 buffer，Done 后用 split_reply 分片发送（行为不变）
   - **WebChannel（未来）**：直接把 TurnEvent 转 WS frame 转发

## 标签降级模式

`native_tool_calling=false` 时，LLM 输出里混有 `<tool_call>...</tool_call>` 标签。`ToolCallStreamParser` 状态机维护三个状态：
- **Outside**：正常文本输出，遇 `<` 切 MaybeTag
- **MaybeTag**：匹配 `<tool_call>` 前缀，匹配成功切 InToolCall，失败把 pending 输出回 Outside
- **InToolCall**：累积内容直到 `</tool_call>`，解析为 ToolCall

支持标签跨 chunk 边界。流结束时未闭合的标签按容错处理（当普通文本）。

## 向后兼容

- `Provider::chat()` 保留，内部 `chat_stream().collect()`
- `Agent::handle_input()` 保留，内部 `handle_input_streaming() + collect Chunk`
- 现有调用方（slash 命令等）不受影响

## 不做

- 用户主动中止（Ctrl+C 退出进程）
- 工具执行进度流式（工具一次性返回）
- WebChannel 实现（v2）
