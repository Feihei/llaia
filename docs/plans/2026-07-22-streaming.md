# 流式输出实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** LLAIA 所有 channel 实现 LLM 流式输出，CLI 打字机效果，QQ 内部流式生成保持原行为，为未来 Web channel 打基础。

**Architecture:** 三层流式管道：Provider `chat_stream()` 返回 async Stream 产出 `StreamEvent` → Agent `handle_input_streaming()` 消费 stream 并通过 `tokio::sync::mpsc::Sender<TurnEvent>` 推送高层事件 → Channel 在 `run()` 内部消费 mpsc receiver 按各自协议输出（CLI 实时打印 / QQ 累积后分片）。`chat()` 和 `handle_input()` 非流式接口保留，内部调流式版本 collect。

**Tech Stack:** Rust, tokio (mpsc + Stream), reqwest (bytes_stream), futures-util (StreamExt), async-trait, mockito (SSE 测试)

**Spec:** [docs/specs/2026-07-22-streaming-design.md](../specs/2026-07-22-streaming-design.md)

---

## File Structure

| 文件 | 责任 | 动作 |
|---|---|---|
| `src/provider/mod.rs` | `StreamEvent` 枚举，`Provider` trait 加 `chat_stream` | 修改 |
| `src/provider/openai_compat.rs` | SSE 解析 + `chat_stream` 实现，`chat` 改 collect | 修改 |
| `src/tool_call/mod.rs` | 导出 `stream_parser` 模块 | 修改 |
| `src/tool_call/stream_parser.rs` | `ToolCallStreamParser` 状态机 | 新建 |
| `src/agent/mod.rs` | `TurnEvent` 枚举，`handle_input_streaming`，`handle_input` 改 collect | 修改 |
| `src/channels/cli.rs` | `run()` 改 mpsc 消费 + 打字机 | 修改 |
| `src/channels/qq.rs` | `handle_user_message` 改 mpsc 累积 | 修改 |
| `tests/stream_parser.rs` | 状态机集成测试 | 新建 |
| `tests/provider_stream.rs` | Provider SSE 集成测试 | 新建 |

---

## Task 0: 添加依赖 async-stream

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: 添加 async-stream 依赖**

修改 [Cargo.toml](Cargo.toml)，在 `[dependencies]` 节加一行：

```toml
async-stream = "0.3"
```

最终 `[dependencies]` 节应为：

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["json"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
rusqlite = { version = "0.31", features = ["bundled"] }
anyhow = "1"
thiserror = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
clap = { version = "4", features = ["derive"] }
regex = "1"
dirs = "5"
async-trait = "0.1"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4"] }
shellexpand = "3"
tokio-tungstenite = { version = "0.23", features = ["native-tls"] }
futures-util = "0.3"
async-stream = "0.3"
```

- [ ] **Step 2: 验证依赖可解析**

Run: `cargo check`
Expected: 编译通过，无错误

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add async-stream dependency for SSE parsing"
```

---

## Task 1: 定义 StreamEvent 和 TurnEvent 枚举

**Files:**
- Modify: `src/provider/mod.rs:83-99`
- Modify: `src/agent/mod.rs:1-12`

- [ ] **Step 1: 在 `src/provider/mod.rs` 加 StreamEvent 枚举**

打开 [src/provider/mod.rs](src/provider/mod.rs)，在 `ChatResponse` 结构体后（第 93 行之后）、`Provider` trait 之前（第 95 行之前）插入：

```rust
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
```

- [ ] **Step 2: 修改 `Provider` trait 加 `chat_stream` 方法**

把 [src/provider/mod.rs:95-99](src/provider/mod.rs) 的 Provider trait：

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, req: &ChatRequest<'_>) -> Result<ChatResponse>;
    fn native_tool_calling(&self) -> bool;
}
```

改为：

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, req: &ChatRequest<'_>) -> Result<ChatResponse>;
    async fn chat_stream(
        &self,
        req: &ChatRequest<'_>,
    ) -> BoxStream<'_, Result<StreamEvent>>;
    fn native_tool_calling(&self) -> bool;
}
```

在文件顶部 `use` 区加（紧接第 3 行 `use serde::{Deserialize, Serialize};` 之后）：

```rust
use futures_util::Stream;
```

注意：`BoxStream<'a, T>` 是 `Pin<Box<dyn Stream<Item = T> + Send + 'a>>`，等价于 `Pin<Box<dyn Stream<Item = T> + Send + 'a>>`。需要在文件顶部加：

```rust
use futures_util::stream::BoxStream;
```

最终 use 区应为：

```rust
use anyhow::Result;
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
```

- [ ] **Step 3: 在 `src/agent/mod.rs` 加 TurnEvent 枚举**

打开 [src/agent/mod.rs](src/agent/mod.rs)，在 `use` 区后（第 12 行 `use std::sync::Arc;` 之后）、`pub struct Agent` 之前（第 13 行之前）插入：

```rust
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
```

在 `src/agent/mod.rs` 顶部 use 区（第 4 行 `use crate::agent::context::Context;` 后）加：

```rust
use crate::provider::StreamEvent;
```

并在 use 区（第 10 行 `use anyhow::Result;` 之后）加：

```rust
use tokio::sync::mpsc;
```

- [ ] **Step 4: 验证编译**

Run: `cargo check`
Expected: 编译失败，因为 `OpenAiCompatibleProvider` 尚未实现 `chat_stream`。错误应类似 `not all trait members implemented: missing chat_stream`。这一步只验证 StreamEvent/TurnEvent 本身没有语法错。

- [ ] **Step 5: Commit**

```bash
git add src/provider/mod.rs src/agent/mod.rs
git commit -m "feat: define StreamEvent and TurnEvent enums"
```

---

## Task 2: 实现 ToolCallStreamParser 状态机

**Files:**
- Create: `src/tool_call/stream_parser.rs`
- Modify: `src/tool_call/mod.rs:1-4`
- Create: `tests/stream_parser.rs`

- [ ] **Step 1: 写失败的集成测试**

创建 [tests/stream_parser.rs](tests/stream_parser.rs)：

```rust
use llaia::tool_call::stream_parser::ToolCallStreamParser;
use llaia::provider::ToolCall;

#[test]
fn test_plain_text_passthrough() {
    let mut p = ToolCallStreamParser::new();
    let out = p.feed("hello world");
    assert_eq!(out, "hello world");
    let out = p.feed(" more text");
    assert_eq!(out, " more text");
    let calls = p.take_tool_calls();
    assert!(calls.is_empty());
    assert_eq!(p.finish(), "");
}

#[test]
fn test_single_tag_in_one_chunk() {
    let mut p = ToolCallStreamParser::new();
    let out = p.feed(r#"before <tool_call>{"name":"file_read","arguments":{"path":"/tmp/x"}}</tool_call> after"#);
    assert_eq!(out, "before  after");
    let calls = p.take_tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "file_read");
}

#[test]
fn test_tag_split_across_chunks() {
    let mut p = ToolCallStreamParser::new();
    // 标签开始跨 chunk
    let out1 = p.feed("before <tool_");
    assert_eq!(out1, "before ");
    let out2 = p.feed(r#"call>{"name":"x","arguments":{}}</tool_call> after"#);
    assert_eq!(out2, " after");
    let calls = p.take_tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "x");
}

#[test]
fn test_multiple_tags_consecutive() {
    let mut p = ToolCallStreamParser::new();
    let out = p.feed(r#"<tool_call>{"name":"a","arguments":{}}</tool_call><tool_call>{"name":"b","arguments":{}}</tool_call>"#);
    assert_eq!(out, "");
    let calls = p.take_tool_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "a");
    assert_eq!(calls[1].name, "b");
}

#[test]
fn test_lt_char_not_tag() {
    let mut p = ToolCallStreamParser::new();
    let out = p.feed("a < b and c > d");
    assert_eq!(out, "a < b and c > d");
    let calls = p.take_tool_calls();
    assert!(calls.is_empty());
    assert_eq!(p.finish(), "");
}

#[test]
fn test_unclosed_tag_finish_returns_buffer_as_text() {
    let mut p = ToolCallStreamParser::new();
    let _ = p.feed("text <tool_call>not closed");
    let calls = p.take_tool_calls();
    assert!(calls.is_empty());
    let rest = p.finish();
    // 未闭合，残留按普通文本返回（不含 <tool_call> 标签本身，因为已进入 InToolCall 状态）
    assert!(rest.contains("not closed"));
}

#[test]
fn test_partial_tag_at_chunk_end_finish_returns_as_text() {
    let mut p = ToolCallStreamParser::new();
    let _ = p.feed("text <tool");
    let calls = p.take_tool_calls();
    assert!(calls.is_empty());
    let rest = p.finish();
    // 残留的 "<tool" 应作为普通文本返回
    assert!(rest.contains("<tool"));
}

#[test]
fn test_malformed_json_kept_as_text() {
    let mut p = ToolCallStreamParser::new();
    let out = p.feed("<tool_call>not json</tool_call>");
    // 解析失败，整段当普通文本返回
    assert!(out.contains("not json"));
    let calls = p.take_tool_calls();
    assert!(calls.is_empty());
}
```

- [ ] **Step 2: 跑测试验证失败**

Run: `cargo test --test stream_parser`
Expected: 编译失败，`unresolved module stream_parser`

- [ ] **Step 3: 在 `src/tool_call/mod.rs` 注册模块**

打开 [src/tool_call/mod.rs](src/tool_call/mod.rs)，整体替换为：

```rust
pub mod prompt;
pub mod stream_parser;
pub mod tag_parser;

pub use prompt::build_tool_instructions;
pub use stream_parser::ToolCallStreamParser;
pub use tag_parser::parse_tool_calls;
```

- [ ] **Step 4: 实现 ToolCallStreamParser**

创建 [src/tool_call/stream_parser.rs](src/tool_call/stream_parser.rs)：

```rust
use crate::provider::ToolCall;
use serde_json::Value;

/// 流式 tool_call 标签解析器（状态机）。
///
/// 喂入文本 chunk，输出应发给用户的纯文本增量；
/// 完整的 `<tool_call>...</tool_call>` 标签被解析为 ToolCall，不输出。
/// 支持标签跨 chunk 边界。
pub struct ToolCallStreamParser {
    state: State,
    /// InToolCall 状态下累积的标签内容
    buffer: String,
    /// MaybeTag 状态下缓冲的可能是标签开头的内容（如 "<tool"）
    pending: String,
    /// 已解析的 ToolCall 列表
    completed: Vec<ToolCall>,
}

#[derive(PartialEq)]
enum State {
    Outside,
    MaybeTag,
    InToolCall,
}

const OPEN_TAG: &str = "<tool_call>";
const CLOSE_TAG: &str = "</tool_call>";

impl ToolCallStreamParser {
    pub fn new() -> Self {
        Self {
            state: State::Outside,
            buffer: String::new(),
            pending: String::new(),
            completed: Vec::new(),
        }
    }

    /// 喂一个 chunk，返回应发给用户的文本增量
    pub fn feed(&mut self, chunk: &str) -> String {
        let mut out = String::new();
        for ch in chunk.chars() {
            match self.state {
                State::Outside => {
                    if ch == '<' {
                        self.state = State::MaybeTag;
                        self.pending.push(ch);
                    } else {
                        out.push(ch);
                    }
                }
                State::MaybeTag => {
                    self.pending.push(ch);
                    // 检查是否匹配 <tool_call>
                    if OPEN_TAG.starts_with(&self.pending) {
                        // 还在匹配中
                        if self.pending == OPEN_TAG {
                            // 完整匹配，进入 InToolCall
                            self.state = State::InToolCall;
                            self.pending.clear();
                            self.buffer.clear();
                        }
                        // 否则继续等下一个字符
                    } else {
                        // 不匹配，把 pending 全部输出，回到 Outside
                        out.push_str(&self.pending);
                        self.pending.clear();
                        self.state = State::Outside;
                    }
                }
                State::InToolCall => {
                    self.buffer.push(ch);
                    // 检查 buffer 末尾是否匹配 </tool_call>
                    if self.buffer.ends_with(CLOSE_TAG) {
                        // 去掉闭合标签
                        let body = &self.buffer[..self.buffer.len() - CLOSE_TAG.len()];
                        let body_trimmed = body.trim();
                        // 尝试解析 JSON
                        if let Ok(value) = serde_json::from_str::<Value>(body_trimmed) {
                            if let Some(call) = value_to_tool_call(&value) {
                                self.completed.push(call);
                            } else {
                                // JSON 但不是 tool_call 结构，当普通文本
                                out.push_str(&self.buffer);
                            }
                        } else {
                            // 解析失败，当普通文本
                            out.push_str(&self.buffer);
                        }
                        self.buffer.clear();
                        self.state = State::Outside;
                    }
                }
            }
        }
        out
    }

    /// 取出已解析的 ToolCall（清空内部列表）
    pub fn take_tool_calls(&mut self) -> Vec<ToolCall> {
        std::mem::take(&mut self.completed)
    }

    /// 流结束时调用，返回残留的 pending/buffer 作为普通文本
    pub fn finish(self) -> String {
        let mut out = String::new();
        // MaybeTag 状态下残留的 pending 全部当文本
        out.push_str(&self.pending);
        // InToolCall 状态下未闭合的 buffer，整段当文本（包含标签内内容）
        if self.state == State::InToolCall {
            // 还原 <tool_call> 前缀 + buffer
            out.push_str(OPEN_TAG);
            out.push_str(&self.buffer);
        }
        out
    }
}

fn value_to_tool_call(value: &Value) -> Option<ToolCall> {
    let name = value.get("name")?.as_str()?.to_string();
    let arguments = match value.get("arguments") {
        Some(Value::Object(_)) => value.get("arguments").cloned().unwrap(),
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        _ => Value::Null,
    };
    let id = format!("tag_{}", uuid::Uuid::new_v4().simple());
    Some(ToolCall { id, name, arguments })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smoke() {
        let mut p = ToolCallStreamParser::new();
        assert_eq!(p.feed("hi"), "hi");
    }
}
```

- [ ] **Step 5: 跑测试验证通过**

Run: `cargo test --test stream_parser`
Expected: 8 个测试全部 PASS

Run: `cargo test --lib stream_parser`
Expected: 1 个单元测试 PASS

- [ ] **Step 6: Commit**

```bash
git add src/tool_call/stream_parser.rs src/tool_call/mod.rs tests/stream_parser.rs
git commit -m "feat: ToolCallStreamParser state machine for streaming tag detection"
```

---

## Task 3: 实现 Provider::chat_stream（SSE 解析）

**Files:**
- Modify: `src/provider/openai_compat.rs:1-30, 108-217`
- Create: `tests/provider_stream.rs`

- [ ] **Step 1: 写失败的集成测试**

创建 [tests/provider_stream.rs](tests/provider_stream.rs)：

```rust
use futures_util::StreamExt;
use llaia::provider::openai_compat::OpenAiCompatibleProvider;
use llaia::provider::{ChatMessage, ChatRequest, Provider, StreamEvent};
use serde_json::json;

#[tokio::test]
async fn test_stream_text_deltas() {
    let mut server = mockito::Server::new_async().await;
    // 模拟 OpenAI SSE 响应：3 个文本 delta + [DONE]
    let sse_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let m = server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(sse_body)
        .create_async()
        .await;

    let provider = OpenAiCompatibleProvider::new(server.url(), "", "test-model", true).unwrap();
    let msgs = vec![ChatMessage::user("hi")];
    let req = ChatRequest { messages: &msgs, tools: None };
    let mut stream = provider.chat_stream(&req).await;

    let mut deltas = Vec::new();
    let mut done = false;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            StreamEvent::TextDelta(d) => deltas.push(d),
            StreamEvent::Done => done = true,
            _ => {}
        }
    }
    m.assert_async().await;
    assert_eq!(deltas.concat(), "hello world");
    assert!(done);
}

#[tokio::test]
async fn test_stream_tool_calls_accumulated() {
    let mut server = mockito::Server::new_async().await;
    // 模拟 OpenAI 流式 tool_calls：index 0 的 arguments 分两次返回
    let sse_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"file_read\",\"arguments\":\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\""}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\":\\\"/tmp\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let m = server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(sse_body)
        .create_async()
        .await;

    let provider = OpenAiCompatibleProvider::new(server.url(), "", "test-model", true).unwrap();
    let msgs = vec![ChatMessage::user("read /tmp")];
    let req = ChatRequest { messages: &msgs, tools: None };
    let mut stream = provider.chat_stream(&req).await;

    let mut tool_calls = Vec::new();
    let mut done = false;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            StreamEvent::ToolCall(tc) => tool_calls.push(tc),
            StreamEvent::Done => done = true,
            _ => {}
        }
    }
    m.assert_async().await;
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "call_1");
    assert_eq!(tool_calls[0].name, "file_read");
    assert_eq!(tool_calls[0].arguments, json!({"path": "/tmp"}));
    assert!(done);
}

#[tokio::test]
async fn test_stream_error_status() {
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/chat/completions")
        .with_status(500)
        .with_body("internal error")
        .create_async()
        .await;

    let provider = OpenAiCompatibleProvider::new(server.url(), "", "test-model", true).unwrap();
    let msgs = vec![ChatMessage::user("hi")];
    let req = ChatRequest { messages: &msgs, tools: None };
    let mut stream = provider.chat_stream(&req).await;

    let ev = stream.next().await.unwrap();
    match ev {
        StreamEvent::Error(msg) => assert!(msg.contains("500") || msg.contains("internal")),
        other => panic!("expected Error, got {:?}", other),
    }
    m.assert_async().await;
}
```

- [ ] **Step 2: 跑测试验证失败**

Run: `cargo test --test provider_stream`
Expected: 编译失败，`chat_stream` 未实现

- [ ] **Step 3: 在 `src/provider/openai_compat.rs` 实现 chat_stream**

打开 [src/provider/openai_compat.rs](src/provider/openai_compat.rs)。

先在文件顶部加 use（第 1 行后）：

```rust
use crate::provider::{ChatRequest, ChatResponse, Provider, Role, StreamEvent, ToolCall};
use anyhow::{anyhow, Result};
use async_stream::try_stream;
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
```

注意第一行原本是 `use crate::provider::{ChatRequest, ChatResponse, Provider, Role, ToolCall};`，改为上面的版本（加 `StreamEvent`）。同时新增 `async_stream::try_stream`、`futures_util::StreamExt` 两个 use。

然后在 `impl Provider for OpenAiCompatibleProvider` 块内（[第 108-217 行](src/provider/openai_compat.rs)），在 `async fn chat` 方法之后、`fn native_tool_calling` 之前，插入 `chat_stream` 方法。

为了保持 `chat()` 的代码不变，把原来的 `impl Provider` 块替换为：

```rust
#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    async fn chat(&self, req: &ChatRequest<'_>) -> Result<ChatResponse> {
        let mut stream = self.chat_stream(req).await;
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        while let Some(ev) = stream.next().await {
            match ev? {
                StreamEvent::TextDelta(d) => text.push_str(&d),
                StreamEvent::ToolCall(tc) => tool_calls.push(tc),
                StreamEvent::Done => break,
                StreamEvent::Error(msg) => return Err(anyhow!("stream error: {}", msg)),
            }
        }
        Ok(ChatResponse {
            text: if text.is_empty() { None } else { Some(text) },
            tool_calls,
        })
    }

    async fn chat_stream(
        &self,
        req: &ChatRequest<'_>,
    ) -> BoxStream<'_, Result<StreamEvent>> {
        let url = format!("{}/chat/completions", self.base_url);

        let messages: Vec<OpenAiMessage> = req
            .messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                };
                OpenAiMessage {
                    role,
                    content: &m.content,
                    tool_calls: m.tool_calls.as_ref().map(|tcs| {
                        tcs.iter()
                            .map(|tc| OpenAiToolCallSer {
                                id: &tc.id,
                                tool_type: "function",
                                function: OpenAiFunctionSer {
                                    name: &tc.name,
                                    arguments: tc.arguments.to_string(),
                                },
                            })
                            .collect()
                    }),
                    tool_call_id: m.tool_call_id.as_deref(),
                }
            })
            .collect();

        let tools: Option<Vec<OpenAiTool>> = if self.native_tool_calling {
            req.tools.map(|ts| {
                ts.iter()
                    .map(|t| OpenAiTool {
                        tool_type: "function",
                        function: OpenAiFunctionSpec {
                            name: t.name.clone(),
                            description: t.description.clone(),
                            parameters: t.parameters.clone(),
                        },
                    })
                    .collect()
            })
        } else {
            None
        };

        let tool_choice = if tools.is_some() {
            Some("auto".to_string())
        } else {
            None
        };

        let body = ChatCompletionsStreamRequest {
            model: &self.model,
            messages,
            tools,
            tool_choice,
            stream: true,
        };

        let mut request = self.client.post(&url).json(&body);
        if !self.api_key.is_empty() {
            request = request.bearer_auth(&self.api_key);
        }

        let resp = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                return Box::pin(try_stream! {
                    yield StreamEvent::Error(format!("request failed: {}", e));
                });
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Box::pin(try_stream! {
                yield StreamEvent::Error(format!("provider returned {}: {}", status, text));
            });
        }

        // 流式 SSE 解析
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        // tool_calls 按 index 累积
        let mut tc_accum: std::collections::HashMap<u32, ToolCallAccum> = std::collections::HashMap::new();
        // 按 index 排序后的最终列表
        let mut tc_order: Vec<u32> = Vec::new();

        let s = try_stream! {
            while let Some(chunk_res) = stream.next().await {
                let chunk = match chunk_res {
                    Ok(c) => c,
                    Err(e) => {
                        yield StreamEvent::Error(format!("stream chunk error: {}", e));
                        return;
                    }
                };
                buf.push_str(std::str::from_utf8(&chunk).unwrap_or(""));
                // 按双换行分割 SSE event
                while let Some(pos) = buf.find("\n\n") {
                    let event_str = buf[..pos].to_string();
                    buf = buf[pos + 2..].to_string();
                    // 解析 event_str 中的 data: 行
                    for line in event_str.lines() {
                        let line = line.trim();
                        if line.is_empty() || line.starts_with(':') {
                            continue;
                        }
                        if let Some(data) = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:")) {
                            let data = data.trim();
                            if data == "[DONE]" {
                                // 发送累积的 tool_calls
                                let mut indices: Vec<u32> = tc_order.clone();
                                indices.sort();
                                for idx in indices {
                                    if let Some(acc) = tc_accum.remove(&idx) {
                                        let args: serde_json::Value = serde_json::from_str(&acc.arguments)
                                            .unwrap_or(serde_json::Value::Null);
                                        yield StreamEvent::ToolCall(ToolCall {
                                            id: acc.id,
                                            name: acc.name,
                                            arguments: args,
                                        });
                                    }
                                }
                                yield StreamEvent::Done;
                                return;
                            }
                            // 解析 JSON
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                                if let Some(delta) = v.get("choices").and_then(|c| c.get(0)).and_then(|c| c.get("delta")) {
                                    // 文本 delta
                                    if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                                        if !content.is_empty() {
                                            yield StreamEvent::TextDelta(content.to_string());
                                        }
                                    }
                                    // tool_calls delta
                                    if let Some(tcs) = delta.get("tool_calls") {
                                        if let Some(tcs_arr) = tcs.as_array() {
                                            for tc in tcs_arr {
                                                let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
                                                if !tc_order.contains(&idx) {
                                                    tc_order.push(idx);
                                                }
                                                let acc = tc_accum.entry(idx).or_insert_with(|| ToolCallAccum::default());
                                                if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                                                    acc.id = id.to_string();
                                                }
                                                if let Some(func) = tc.get("function") {
                                                    if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                                                        acc.name = name.to_string();
                                                    }
                                                    if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
                                                        acc.arguments.push_str(args);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // 流自然结束没收到 [DONE]，也发 Done
            let mut indices: Vec<u32> = tc_order.clone();
            indices.sort();
            for idx in indices {
                if let Some(acc) = tc_accum.remove(&idx) {
                    let args: serde_json::Value = serde_json::from_str(&acc.arguments)
                        .unwrap_or(serde_json::Value::Null);
                    yield StreamEvent::ToolCall(ToolCall {
                        id: acc.id,
                        name: acc.name,
                        arguments: args,
                    });
                }
            }
            yield StreamEvent::Done;
        };
        Box::pin(s)
    }

    fn native_tool_calling(&self) -> bool {
        self.native_tool_calling
    }
}

#[derive(Default)]
struct ToolCallAccum {
    id: String,
    name: String,
    arguments: String,
}

/// 流式请求体（比 ChatCompletionsRequest 多 stream: true）
#[derive(Serialize)]
struct ChatCompletionsStreamRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAiMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    stream: bool,
}
```

- [ ] **Step 4: 跑 provider_stream 测试**

Run: `cargo test --test provider_stream`
Expected: 3 个测试全部 PASS

- [ ] **Step 5: 跑现有 provider_http 测试确认未回归**

Run: `cargo test --test provider_http`
Expected: 3 个测试全部 PASS（chat() 内部走 chat_stream + collect）

- [ ] **Step 6: Commit**

```bash
git add src/provider/openai_compat.rs tests/provider_stream.rs
git commit -m "feat: implement Provider::chat_stream with SSE parsing"
```

---

## Task 4: 实现 Agent::handle_input_streaming

**Files:**
- Modify: `src/agent/mod.rs:13-139`

- [ ] **Step 1: 写失败的单元测试**

打开 [src/agent/mod.rs](src/agent/mod.rs)，在文件末尾加测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatMessage, ChatRequest, ChatResponse, Provider, Role, StreamEvent, ToolCall};
    use async_stream::try_stream;
    use async_trait::async_trait;
    use futures_util::stream::BoxStream;
    use futures_util::StreamExt;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::mpsc;

    /// Mock provider：每次 chat_stream 调用返回下一组预设事件
    struct MockProvider {
        native: bool,
        /// 每次调用的回合事件列表，按调用顺序消费
        rounds: Arc<StdMutex<std::collections::VecDeque<Vec<StreamEvent>>>>,
    }

    impl MockProvider {
        fn new(native: bool, rounds: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                native,
                rounds: Arc::new(StdMutex::new(rounds.into())),
            }
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat(&self, _req: &ChatRequest<'_>) -> Result<ChatResponse> {
            unreachable!()
        }
        async fn chat_stream(
            &self,
            _req: &ChatRequest<'_>,
        ) -> BoxStream<'_, Result<StreamEvent>> {
            let events = self.rounds.lock().unwrap().pop_front().unwrap_or_default();
            let s = async_stream::try_stream! {
                for ev in events {
                    yield ev;
                }
            };
            Box::pin(s)
        }
        fn native_tool_calling(&self) -> bool {
            self.native
        }
    }

    fn make_agent_with_rounds(
        native: bool,
        rounds: Vec<Vec<StreamEvent>>,
    ) -> Agent {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("test", "test").unwrap();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(native, rounds));
        let tools = Arc::new(ToolRegistry::new());
        Agent::new(
            &Config::default_for_workspace("/tmp/llaia-test"),
            provider,
            tools,
            Arc::new(store),
            sid,
            "test system".into(),
            8192,
        )
    }

    #[tokio::test]
    async fn test_streaming_plain_text() {
        let rounds = vec![vec![
            StreamEvent::TextDelta("hello ".into()),
            StreamEvent::TextDelta("world".into()),
            StreamEvent::Done,
        ]];
        let mut agent = make_agent_with_rounds(true, rounds);
        let (tx, mut rx) = mpsc::channel(64);
        let result = agent.handle_input_streaming("hi", "cli", tx).await.unwrap();
        assert_eq!(result, "hello world");

        let mut chunks = Vec::new();
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                TurnEvent::Chunk { delta } => chunks.push(delta),
                TurnEvent::Done => done = true,
                _ => {}
            }
        }
        assert_eq!(chunks.concat(), "hello world");
        assert!(done);
    }

    #[tokio::test]
    async fn test_streaming_native_tool_call() {
        // 第一轮返回 ToolCall + Done，第二轮返回 "done" 文本 + Done
        let tc = ToolCall {
            id: "call_1".into(),
            name: "echo".into(),
            arguments: json!({}),
        };
        let rounds = vec![
            vec![StreamEvent::ToolCall(tc), StreamEvent::Done],
            vec![StreamEvent::TextDelta("done".into()), StreamEvent::Done],
        ];
        let mut agent = make_agent_with_rounds(true, rounds);
        let (tx, mut rx) = mpsc::channel(64);
        let _ = agent.handle_input_streaming("read", "cli", tx).await;

        let mut tool_starts = Vec::new();
        let mut chunks = Vec::new();
        let mut done_count = 0;
        while let Some(ev) = rx.recv().await {
            match ev {
                TurnEvent::ToolStart { name, .. } => tool_starts.push(name),
                TurnEvent::Chunk { delta } => chunks.push(delta),
                TurnEvent::Done => done_count += 1,
                _ => {}
            }
        }
        // 工具被识别并触发 ToolStart（虽然未注册，execute 会返回 unknown tool 错误消息）
        assert_eq!(tool_starts, vec!["echo"]);
        assert_eq!(chunks.concat(), "done");
        assert_eq!(done_count, 1);
    }

    #[tokio::test]
    async fn test_streaming_tag_mode_filters_tags() {
        // native=false，LLM 输出里混有 <tool_call> 标签
        let rounds = vec![vec![
            StreamEvent::TextDelta("before ".into()),
            StreamEvent::TextDelta(r#"<tool_call>{"name":"x","arguments":{}}</tool_call>"#),
            StreamEvent::TextDelta(" after".into()),
            StreamEvent::Done,
        ]];
        let mut agent = make_agent_with_rounds(false, rounds);
        let (tx, mut rx) = mpsc::channel(64);
        let _ = agent.handle_input_streaming("hi", "cli", tx).await;

        let mut chunks = Vec::new();
        let mut tool_starts = Vec::new();
        while let Some(ev) = rx.recv().await {
            match ev {
                TurnEvent::Chunk { delta } => chunks.push(delta),
                TurnEvent::ToolStart { name, .. } => tool_starts.push(name),
                _ => {}
            }
        }
        // 标签被过滤，只输出 "before  after"
        assert_eq!(chunks.concat(), "before  after");
        // 工具被解析（虽然执行会失败，因为没注册）
        assert_eq!(tool_starts, vec!["x"]);
    }
}
```

- [ ] **Step 2: 跑测试验证失败**

Run: `cargo test --lib agent::tests`
Expected: 编译失败，`handle_input_streaming` 未定义，可能 `SessionStore::open_in_memory` 未定义

- [ ] **Step 3: 在 `src/memory/sqlite.rs` 加 open_in_memory**

`SessionStore::open` 在 [src/memory/sqlite.rs:44-55](src/memory/sqlite.rs)，用 `Connection::open` + `init_schema`。在 `open` 方法之后加一个 `open_in_memory` 方法（同一个 `impl SessionStore` 块内，第 55 行后）：

```rust
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
```

注意：方法名是 `init_schema`（不是 `init_db`）。

- [ ] **Step 4: 实现 handle_input_streaming**

打开 [src/agent/mod.rs](src/agent/mod.rs)，把 `handle_input` 方法（[第 48-138 行](src/agent/mod.rs)）整段替换为下面两个方法（保留 `handle_input` 作为非流式包装）：

```rust
/// 非流式版本（保留向后兼容）：内部调 handle_input_streaming + 收集
pub async fn handle_input(&mut self, user_input: &str, channel: &str) -> Result<String> {
    let (tx, mut rx) = mpsc::channel(64);
    let result = self.handle_input_streaming(user_input, channel, tx).await;
    let mut text = String::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            TurnEvent::Chunk { delta } => text.push_str(&delta),
            TurnEvent::Error { message } => {
                return Err(anyhow::anyhow!(message));
            }
            _ => {}
        }
    }
    result?;
    Ok(text)
}

/// 流式版本：通过 event_tx 推送 TurnEvent
pub async fn handle_input_streaming(
    &mut self,
    user_input: &str,
    channel: &str,
    event_tx: mpsc::Sender<TurnEvent>,
) -> Result<String> {
    self.session_store
        .append_message(self.session_id, &Role::User, user_input)?;
    self.context.push(ChatMessage::user(user_input));

    if self
        .context
        .needs_compaction(self.max_tokens, self.context_threshold)
    {
        if let Err(e) = self.context.compact(self.provider.as_ref(), 6).await {
            tracing::warn!(error = %e, "auto-compact failed");
        }
    }

    let max_iters = self.max_iterations;
    let mut final_text = String::new();

    for i in 0..max_iters {
        let messages = self.context.to_messages();
        let tools = self.tools.specs();
        let tools_ref = if tools.is_empty() {
            None
        } else {
            Some(tools.as_slice())
        };
        let req = ChatRequest {
            messages: &messages,
            tools: tools_ref,
        };

        let mut stream = self.provider.chat_stream(&req).await?;
        let mut iter_text = String::new();
        let mut calls: Vec<crate::provider::ToolCall> = Vec::new();
        let mut parser = crate::tool_call::ToolCallStreamParser::new();

        while let Some(ev) = stream.next().await {
            match ev? {
                StreamEvent::TextDelta(d) => {
                    if self.provider.native_tool_calling() {
                        let _ = event_tx.send(TurnEvent::Chunk { delta: d.clone() }).await;
                        iter_text.push_str(&d);
                    } else {
                        let user_text = parser.feed(&d);
                        if !user_text.is_empty() {
                            let _ = event_tx.send(TurnEvent::Chunk { delta: user_text }).await;
                        }
                        iter_text.push_str(&d);
                        let new_calls = parser.take_tool_calls();
                        calls.extend(new_calls);
                    }
                }
                StreamEvent::ToolCall(tc) => {
                    calls.push(tc);
                }
                StreamEvent::Done => break,
                StreamEvent::Error(msg) => {
                    let _ = event_tx
                        .send(TurnEvent::Error { message: msg.clone() })
                        .await;
                    return Err(anyhow::anyhow!(msg));
                }
            }
        }

        if !self.provider.native_tool_calling() {
            let rest = parser.finish();
            if !rest.is_empty() {
                let _ = event_tx.send(TurnEvent::Chunk { delta: rest.clone() }).await;
                iter_text.push_str(&rest);
            }
        }

        final_text = iter_text.clone();

        if calls.is_empty() {
            self.session_store
                .append_message(self.session_id, &Role::Assistant, &iter_text)?;
            self.context.push(ChatMessage::assistant(&iter_text));
            let _ = event_tx.send(TurnEvent::Done).await;
            return Ok(iter_text);
        }

        let assistant_msg = ChatMessage::assistant_with_tools(iter_text.clone(), calls.clone());
        let assistant_msg_id = self.session_store.append_message(
            self.session_id,
            &Role::Assistant,
            &iter_text,
        )?;
        self.context.push(assistant_msg);

        for tc in &calls {
            self.session_store
                .append_tool_call(
                    assistant_msg_id,
                    &tc.id,
                    &tc.name,
                    &tc.arguments.to_string(),
                    None,
                )
                .ok();
        }

        // 工具调用开始事件
        for tc in &calls {
            let _ = event_tx
                .send(TurnEvent::ToolStart {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                })
                .await;
        }

        let tool_msgs = execute_tool_calls(
            &self.tools,
            &calls,
            channel,
            &self.qq_confirm_mode,
        )
        .await?;
        for msg in tool_msgs.iter() {
            let _ = event_tx
                .send(TurnEvent::ToolResult {
                    id: msg.tool_call_id.clone().unwrap_or_default(),
                    output: msg.content.clone(),
                })
                .await;
            self.session_store
                .append_message(self.session_id, &Role::Tool, &msg.content)?;
            self.context.push(msg.clone());
        }

        tracing::info!(iter = i, "tool iteration done");
    }

    let fallback = "[reached max tool iterations]";
    self.session_store
        .append_message(self.session_id, &Role::Assistant, fallback)?;
    self.context.push(ChatMessage::assistant(fallback));
    let _ = event_tx
        .send(TurnEvent::Chunk {
            delta: fallback.into(),
        })
        .await;
    let _ = event_tx.send(TurnEvent::Done).await;
    Ok(fallback.into())
}
```

**注意**：`execute_tool_calls` 当前已处理 unknown tool（返回 `[error: unknown tool ...]` 消息而非 Err），所以测试 `test_streaming_native_tool_call` 中未注册的 echo 工具会触发 ToolStart 事件 + 一条 ToolResult（含错误消息），不影响后续第二轮调用。

- [ ] **Step 5: 在 use 区加 mpsc**

[src/agent/mod.rs](src/agent/mod.rs) 顶部 use 区已经加了 `use tokio::sync::mpsc;`（Task 1 Step 3）。确认存在。

- [ ] **Step 6: 跑测试验证通过**

Run: `cargo test --lib agent::tests`
Expected: 3 个测试全部 PASS

- [ ] **Step 7: 跑全部测试确认无回归**

Run: `cargo test`
Expected: 所有测试通过

- [ ] **Step 8: Commit**

```bash
git add src/agent/mod.rs src/memory/sqlite.rs
git commit -m "feat: implement Agent::handle_input_streaming with mpsc TurnEvent"
```

---

## Task 5: 改造 CliChannel 为打字机模式

**Files:**
- Modify: `src/channels/cli.rs:30-62`

- [ ] **Step 1: 修改 CliChannel::run**

打开 [src/channels/cli.rs](src/channels/cli.rs)，把 `impl Channel for CliChannel` 块（[第 31-61 行](src/channels/cli.rs)）替换为：

```rust
#[async_trait]
impl Channel for CliChannel {
    async fn run(self: Arc<Self>, agent: Arc<Mutex<Agent>>) -> Result<()> {
        println!("llaia v0.1.5 - type /help for commands, /exit to quit\n");
        let stdin = std::io::stdin();
        loop {
            print!("> ");
            std::io::stdout().flush()?;
            let mut line = String::new();
            if stdin.lock().read_line(&mut line)? == 0 {
                break;
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            match try_handle(line, &mut *agent.lock().await).await? {
                SlashOutcome::Exit => break,
                SlashOutcome::Handled => continue,
                SlashOutcome::NotSlash => {
                    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
                    let agent_clone = agent.clone();
                    let input_clone = line.to_string();
                    tokio::spawn(async move {
                        let mut a = agent_clone.lock().await;
                        if let Err(e) = a
                            .handle_input_streaming(&input_clone, "cli", tx)
                            .await
                        {
                            tracing::error!(error = %e, "handle_input_streaming failed");
                        }
                    });
                    println!();  // 换行，分隔 prompt 和回复
                    while let Some(ev) = rx.recv().await {
                        match ev {
                            TurnEvent::Chunk { delta } => {
                                print!("{}", delta);
                                std::io::stdout().flush().ok();
                            }
                            TurnEvent::ToolStart { name, .. } => {
                                println!("\n[tool: {}]", name);
                            }
                            TurnEvent::ToolResult { output, .. } => {
                                let preview = if output.len() > 200 {
                                    format!("{}...(truncated)", &output[..200])
                                } else {
                                    output
                                };
                                println!("[result: {}]", preview);
                            }
                            TurnEvent::Done => {
                                println!("\n");
                            }
                            TurnEvent::Error { message } => {
                                println!("\n[error: {}]\n", message);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
```

- [ ] **Step 2: 在 use 区加 TurnEvent**

[src/channels/cli.rs](src/channels/cli.rs) 顶部 use 区，在 `use crate::agent::Agent;` 后加：

```rust
use crate::agent::TurnEvent;
```

- [ ] **Step 3: 验证编译**

Run: `cargo check`
Expected: 编译通过

- [ ] **Step 4: 跑全部测试**

Run: `cargo test`
Expected: 所有测试通过

- [ ] **Step 5: Commit**

```bash
git add src/channels/cli.rs
git commit -m "feat: CliChannel typewriter streaming via mpsc consumer"
```

---

## Task 6: 改造 QqChannel handle_user_message

**Files:**
- Modify: `src/channels/qq.rs:202-236`

- [ ] **Step 1: 修改 handle_user_message**

打开 [src/channels/qq.rs](src/channels/qq.rs)，把 `handle_user_message` 方法（[第 202-236 行](src/channels/qq.rs)）替换为：

```rust
    /// 处理一条用户消息：流式调 agent，等 Done 后分片发送
    async fn handle_user_message(
        self: Arc<Self>,
        agent: &Arc<Mutex<Agent>>,
        user_openid: &str,
        text: &str,
        msg_id: &str,
    ) -> Result<()> {
        tracing::info!(user = %user_openid, text = %text, "qq received message");

        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let reply = {
            let mut a = agent.lock().await;
            a.handle_input_streaming(text, "qq", tx).await
        };

        if let Err(e) = reply {
            tracing::error!(error = %e, "agent handle_input_streaming failed");
            let err_msg = format!("[内部错误: {}]", e);
            let _ = self
                .send_c2c_message(user_openid, &err_msg, Some(msg_id))
                .await;
            return Ok(());
        }

        // 累积所有 Chunk，等 Done 后分片发送
        let mut buffer = String::new();
        while let Some(ev) = rx.recv().await {
            match ev {
                crate::agent::TurnEvent::Chunk { delta } => buffer.push_str(&delta),
                crate::agent::TurnEvent::Done => {
                    let chunks = split_reply(&buffer, 1800);
                    tracing::info!(
                        chunks = chunks.len(),
                        total_len = buffer.len(),
                        "sending reply"
                    );
                    for (i, chunk) in chunks.iter().enumerate() {
                        // 只有第一片带 msg_id 用于被动回复，后续片用主动消息
                        let id = if i == 0 { Some(msg_id) } else { None };
                        if let Err(e) = self
                            .send_c2c_message(user_openid, chunk, id)
                            .await
                        {
                            tracing::error!(
                                error = %e,
                                chunk = i,
                                "failed to send chunk after retries"
                            );
                        }
                    }
                    break;
                }
                crate::agent::TurnEvent::Error { message } => {
                    let err_msg = format!("[错误: {}]", message);
                    let _ = self
                        .send_c2c_message(user_openid, &err_msg, Some(msg_id))
                        .await;
                    break;
                }
                _ => {} // ToolStart / ToolResult 在 QQ 下不转发
            }
        }
        Ok(())
    }
```

- [ ] **Step 2: 验证编译**

Run: `cargo check`
Expected: 编译通过

- [ ] **Step 3: 跑 QQ 相关测试**

Run: `cargo test --test qq_http`
Expected: 所有测试通过（HTTP 集成测试不涉及 handle_user_message 改动）

Run: `cargo test --test qq_split`
Expected: 6 个测试通过

- [ ] **Step 4: 跑全部测试**

Run: `cargo test`
Expected: 所有测试通过

- [ ] **Step 5: Commit**

```bash
git add src/channels/qq.rs
git commit -m "feat: QqChannel uses streaming agent with buffered reply"
```

---

## Task 7: 端到端验证

**Files:**
- 无代码改动，手动验证

- [ ] **Step 1: cargo build release**

Run: `cargo build`
Expected: 编译成功

- [ ] **Step 2: 启动 CLI 验证打字机**

Run: `cargo run -- chat`
Expected:
- 看到 `llaia v0.1.5 - type /help for commands, /exit to quit` 提示
- 输入 "你好"，回复应**逐字打印**（不是一次性出现）
- 输入 "列出当前目录文件"（触发 terminal 工具），应看到 `[tool: terminal]` 后跟 `[result: ...]`，然后继续文本输出

- [ ] **Step 3: 启动 serve 验证 QQ**

Run: `cargo run -- serve`
Expected:
- 日志显示 `QqChannel starting` 和 `qq ws IDENTIFY success (READY)`
- 从 QQ 发消息给 bot，应收到回复
- 回复仍是分片发送（行为不变），但 agent 内部是流式生成

- [ ] **Step 4: 验证 CLI 工具调用显示**

在 CLI 输入 "在工作目录下创建一个文件 test_streaming.md，内容是 hello"

Expected:
- 看到 `[tool: file_write]`
- 看到 `[result: ...]`
- 然后看到 agent 的文本回复（如 "已创建文件..."）

- [ ] **Step 5: Commit（如有修复）**

如果端到端测试发现问题，修复后 commit。否则跳过。

---

## Task 8: 文档更新

**Files:**
- Create: `docs/adr/0010-streaming-output.md`

- [ ] **Step 1: 创建 ADR 0010**

创建 [docs/adr/0010-streaming-output.md](docs/adr/0010-streaming-output.md)：

```markdown
# ADR 0010: 流式输出

日期：2026-07-22
状态：Accepted

## 背景

P1.5 QQ channel 完成后，所有 channel 的 LLM 回复都是一次性返回。CLI 体验差（长回复要等十几秒），QQ 虽然内部流式但用户要等全部生成完。未来 Web channel 必须流式。

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
- WebChannel 实现（P2）
```

- [ ] **Step 2: 更新 AGENTS.md 索引**

打开 [AGENTS.md](AGENTS.md)，在底部"设计文档索引"节，把 ADR 索引行从：

```markdown
- [docs/adr/](docs/adr/) — 架构决策记录（ADR-0001 到 ADR-0009）
```

改为：

```markdown
- [docs/adr/](docs/adr/) — 架构决策记录（ADR-0001 到 ADR-0010）
```

- [ ] **Step 3: Commit**

```bash
git add docs/adr/0010-streaming-output.md AGENTS.md
git commit -m "docs: ADR 0010 streaming output"
```
