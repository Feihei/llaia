# 流式输出设计 (P1.5)

> 日期：2026-07-22
> 状态：Approved
> 关联：[ADR 0009](../adr/0009-qq-channel.md)、[QQ Channel Spec](2026-07-21-qq-channel-design.md)

## 背景

P1.5 QQ 频道完成后，所有频道（CLI/QQ/未来 Web）的回复都是一次性返回，长回复需要等 LLM 生成完毕才能看到第一个字符。问题：

1. **CLI 体验差**：长回复要等几秒甚至十几秒才一次性打印
2. **QQ 响应延迟高**：虽然内部已流式生成，但用户要等全部生成完才收到
3. **Web UI 前置依赖**：未来 Web 频道必须流式，否则前端体验差到要重做

## 目标

- CLI 实现打字机效果（边生成边打印）
- QQ 保持现有行为（等生成完后分片发送），但 provider 层用流式避免长时间阻塞
- 为未来 Web 频道打基础（WebSocket 直接转发 TurnEvent）
- 保持向后兼容：`chat()` 和 `handle_input()` 非流式接口保留

## 非目标

- 用户主动中止生成（Ctrl+C 直接退出进程，未来 Web 再加）
- 工具执行进度流式（工具一次性返回结果，不做工具内流式）
- Web 频道实现（P2）

## 架构

参考 zeroclaw 验证过的三层模式：**Provider Stream + Agent mpsc + Channel 消费**。

```
Provider::chat_stream()  ─Stream(TextDelta/ToolCall/Done)─▶  Agent::handle_input_streaming()
                                                                   │
                                                                   ▼ mpsc::Sender<TurnEvent>
                                                            ┌──────────────┐
                                                            │  TurnEvent   │
                                                            │  Chunk       │
                                                            │  ToolStart   │
                                                            │  ToolResult  │
                                                            │  Done        │
                                                            │  Error       │
                                                            └──────────────┘
                                                                   │
                                    ┌──────────────────────────────┼─────────────────────────┐
                                    ▼                              ▼                         ▼
                              CliChannel                     QqChannel                  WebChannel (未来)
                              print! delta                   累积 buffer                WS frame 转发
                              flush                          Done 后 split_reply        (直接映射 TurnEvent)
```

## 组件设计

### 1. Provider 层：StreamEvent + chat_stream

新增 `StreamEvent` 枚举和 `chat_stream` 方法。

```rust
// src/provider/mod.rs

/// 流式事件
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// 文本增量
    TextDelta(String),
    /// 工具调用（native 模式下完整 ToolCall；标签模式不产生此事件，由 Agent 状态机解析）
    ToolCall(ToolCall),
    /// 本轮流式结束
    Done,
    /// 错误
    Error(String),
}

#[async_trait]
pub trait Provider: Send + Sync {
    // 保留：非流式接口
    async fn chat(&self, req: &ChatRequest<'_>) -> Result<ChatResponse>;
    // 新增：流式接口
    async fn chat_stream(
        &self,
        req: &ChatRequest<'_>,
    ) -> BoxStream<'_, Result<StreamEvent>>;
    fn native_tool_calling(&self) -> bool;
}
```

**`chat()` 的实现**：内部调 `chat_stream()`，收集所有 `TextDelta` 拼成 `text`，收集所有 `ToolCall` 拼成 `tool_calls`，遇到 `Done` 返回。保证向后兼容。

**OpenAI 兼容 provider 的 `chat_stream` 实现**：
- 请求体加 `"stream": true`
- 用 `reqwest::Response::bytes_stream()` 拿到字节流
- 按 SSE 格式解析（`data: {...}\n\n`，`data: [DONE]`）
- 每个 SSE chunk 的 `choices[0].delta.content` → `TextDelta`
- 每个 `choices[0].delta.tool_calls[i]` → 累积成完整 `ToolCall`
- `[DONE]` → `Done`

**标签降级模式（`native_tool_calling=false`）**：
- `chat_stream` 只产生 `TextDelta` 和 `Done`，不产生 `ToolCall`
- LLM 输出里的 `<tool_call>...</tool_call>` 标签作为普通文本返回
- Agent 层的状态机负责过滤和解析

### 2. Agent 层：TurnEvent + handle_input_streaming

新增 `TurnEvent` 枚举和 `handle_input_streaming` 方法。

```rust
// src/agent/mod.rs

/// Agent turn 事件（推给 channel 消费）
#[derive(Debug, Clone)]
pub enum TurnEvent {
    /// 文本增量（已过滤掉 tool_call 标签）
    Chunk { delta: String },
    /// 工具调用开始
    ToolStart { id: String, name: String },
    /// 工具执行结果
    ToolResult { id: String, output: String },
    /// 整轮结束（所有文本和工具调用完成）
    Done,
    /// 错误（已生成的文本保留，错误追加）
    Error { message: String },
}

impl Agent {
    /// 流式版本：通过 event_tx 推送 TurnEvent
    pub async fn handle_input_streaming(
        &mut self,
        user_input: &str,
        channel: &str,
        event_tx: mpsc::Sender<TurnEvent>,
    ) -> Result<String>;

    /// 非流式版本（保留）：内部调 handle_input_streaming + 收集
    pub async fn handle_input(&mut self, user_input: &str, channel: &str) -> Result<String>;
}
```

**`handle_input_streaming` 的逻辑**：

```
1. push user message 到 context 和 session_store
2. 检查是否需要 compaction
3. 循环（最多 max_iterations 次）:
   a. provider.chat_stream(req) → 拿到 stream
   b. while let Some(ev) = stream.next().await:
      - TextDelta(delta):
        - native 模式：直接 event_tx.send(Chunk{delta})
        - 标签模式：喂给状态机
          - 状态机输出"标签外文本" → event_tx.send(Chunk{delta})
          - 状态机累积"标签内文本" → 不发送
          - 标签闭合 → 解析为 ToolCall
      - ToolCall(tc): 累积到 calls 列表
      - Done: 跳出内层循环
   c. 如果没有 ToolCall → event_tx.send(Done) 并返回
   d. 有 ToolCall：
      - push assistant message 到 context 和 session_store
      - for each call:
        - event_tx.send(ToolStart{id, name})
        - execute_tool_calls（QQ confirm 检查不变）
        - event_tx.send(ToolResult{id, output})
        - push tool message 到 context 和 session_store
      - 继续下一轮循环
4. 达到 max_iterations → event_tx.send(Chunk{"[reached max iterations]"}) + Done
```

**`handle_input` 非流式版本**：内部建临时 mpsc，调 `handle_input_streaming`，在另一个 task 里收集所有 `Chunk` 拼成完整字符串返回。保留给 slash 命令等不需要流式的场景。

### 3. 标签降级模式状态机

纯函数模块，输入是 chunk，输出是 (发给用户的文本, 解析出的 ToolCall 列表)。

```rust
// src/tool_call/stream_parser.rs

pub struct ToolCallStreamParser {
    state: State,           // Outside / InToolCall / MaybeTag
    buffer: String,         // 标签内累积
    pending: String,        // 可能是标签开头的 '<' 缓冲
    completed: Vec<ToolCall>, // 闭合的 tool_call
}

impl ToolCallStreamParser {
    pub fn new() -> Self;
    /// 喂一个 chunk，返回应该发给用户的文本增量
    pub fn feed(&mut self, chunk: &str) -> String;
    /// 取出已解析的 ToolCall（清空内部列表）
    pub fn take_tool_calls(&mut self) -> Vec<ToolCall>;
    /// 流结束时调用，返回残留的 pending/buffer 作为普通文本
    pub fn finish(self) -> String;
}
```

**状态机**：

- **Outside**：正常文本，遇到 `<` → 切到 MaybeTag，缓冲 `<`
- **MaybeTag**：继续匹配 `tool_call>`，匹配成功 → 切到 InToolCall；匹配失败 → 把缓冲作为普通文本输出，切回 Outside
- **InToolCall**：累积文本，遇到 `</tool_call>` → 解析累积内容为 ToolCall，切回 Outside
- **finish**：流结束时，如果还在 MaybeTag 或 InToolCall，把缓冲当普通文本返回（容错）

### 4. Channel 层

**Channel trait 不变**。各 channel 在 `run()` 内部自己建 mpsc + spawn 消费。

#### CliChannel

```rust
// 收到用户输入后
let (tx, mut rx) = mpsc::channel(64);
let agent_clone = agent.clone();
let input_clone = input.to_string();
tokio::spawn(async move {
    let _ = agent_clone.lock().await
        .handle_input_streaming(&input_clone, "cli", tx).await;
});
while let Some(ev) = rx.recv().await {
    match ev {
        TurnEvent::Chunk { delta } => {
            print!("{}", delta);
            io::stdout().flush();
        }
        TurnEvent::ToolStart { name, .. } => {
            println!("\n[tool: {}]", name);
        }
        TurnEvent::ToolResult { output, .. } => {
            // 长结果折叠显示
            let preview = if output.len() > 200 {
                format!("{}...(truncated)", &output[..200])
            } else {
                output
            };
            println!("[result: {}]", preview);
        }
        TurnEvent::Done => println!(),
        TurnEvent::Error { message } => println!("\n[error: {}]", message),
    }
}
```

#### QqChannel

QQ 不做真流式，等 `Done` 后用 `split_reply` 分片发送：

```rust
let (tx, mut rx) = mpsc::channel(64);
let agent_clone = agent.clone();
let input_clone = input.to_string();
tokio::spawn(async move {
    let _ = agent_clone.lock().await
        .handle_input_streaming(&input_clone, "qq", tx).await;
});
let mut buffer = String::new();
while let Some(ev) = rx.recv().await {
    match ev {
        TurnEvent::Chunk { delta } => buffer.push_str(&delta),
        TurnEvent::Done => {
            let chunks = split_reply(&buffer, 1800);
            for (i, chunk) in chunks.iter().enumerate() {
                let id = if i == 0 { Some(&msg_id) } else { None };
                if let Err(e) = qq.send_c2c_message(&user, chunk, id).await {
                    tracing::error!(error = %e, "send failed");
                }
            }
        }
        TurnEvent::Error { message } => {
            let _ = qq.send_c2c_message(&user, &format!("[错误: {}]", message), Some(&msg_id)).await;
        }
        _ => {}  // ToolStart/ToolResult 在 QQ 下不发送
    }
}
```

## 文件结构

| 文件 | 责任 |
|---|---|
| `src/provider/mod.rs` | `StreamEvent` 枚举，`Provider` trait 加 `chat_stream` |
| `src/provider/openai_compat.rs` | SSE 解析 + `chat_stream` 实现，`chat` 改为 collect 包装 |
| `src/agent/mod.rs` | `TurnEvent` 枚举，`handle_input_streaming`，`handle_input` 改为包装 |
| `src/tool_call/stream_parser.rs` | `ToolCallStreamParser` 状态机（新文件） |
| `src/channels/cli.rs` | `run()` 改用 mpsc 消费 |
| `src/channels/qq.rs` | `handle_user_message` 改用 mpsc 累积 |

## 测试策略

### 单元测试

1. **`stream_parser` 状态机**（纯函数，最易测）：
   - 普通 chunk 全输出
   - `<tool_call>` 标签完整在一个 chunk → 输出空，take_tool_calls 拿到 1 个
   - 标签跨 2 个 chunk（`<tool_` + `call>`）→ 正确识别
   - 多个标签连续
   - 标签未闭合 → finish 返回残留作为普通文本
   - 标签外的 `<` 字符（如 `a < b`）正确输出

2. **Provider SSE 解析**：
   - mockito 模拟 SSE 响应（多个 `data:` 行 + `[DONE]`），验证 `chat_stream` 产生的 `StreamEvent` 序列
   - native 模式：SSE 里有 `tool_calls` delta → 验证累积成完整 `ToolCall`
   - 非 native 模式：SSE 里只有 `content` delta → 只产生 `TextDelta`

3. **Agent 流式**：
   - mock provider 返回预设 `StreamEvent` 序列，验证 `event_rx` 收到的 `TurnEvent` 序列
   - native 模式：TextDelta → Chunk，ToolCall → ToolStart+执行+ToolResult
   - 标签模式：TextDelta 混合 `<tool_call>` → 验证 Chunk 只含标签外文本，ToolCall 正确解析
   - 多轮工具调用：第一轮返回 ToolCall → 第二轮返回纯文本 → Done

4. **`chat()` 向后兼容**：原 `chat()` 测试保持通过

### 集成测试

- 现有 `provider_http.rs` mockito 测试保持通过
- 新增 `provider_http.rs` 流式测试：mock SSE 响应

## 范围边界

**做**：
- Provider SSE 流式解析
- Agent runner 流式改造（`handle_input_streaming`）
- `TurnEvent` + mpsc 推送
- 标签降级模式状态机
- CliChannel 打字机效果
- QqChannel 用流式 provider（行为不变，等 Done 后分片）
- `chat()` / `handle_input()` 向后兼容

**不做**：
- 用户主动中止（Ctrl+C 退出进程）
- 工具执行进度流式
- WebChannel 实现
- QQ 边生成边发送（仍等 Done 后一次性分片）

## 风险

1. **SSE 解析复杂度**：OpenAI SSE 格式有边界情况（`data:` 前缀空格、多个 `\n`、心跳 `:ping` 等）。用 `eventsource-stream` 或 `async-sse` crate 可以减少手写解析，但增加依赖。倾向手写最小解析器，约 50 行代码。

2. **工具调用 delta 累积**：OpenAI 流式 tool_calls 是按 `index` 分片返回 arguments 字符串，需要按 index 累积。状态比文本 delta 复杂。

3. **标签状态机边界情况**：`<tool_call>` 跨 chunk、`<` 后立即 chunk 结束、嵌套引号包含 `</tool_call>` 等。需要充分测试。

## 依赖变更

- `futures-util`：已有（QQ channel 引入），用 `StreamExt`
- `async-stream`：可选，用 `try_stream!` 宏简化 SSE 解析。或手写 `impl Stream`。倾向手写，避免新依赖。
