# Channel OutputSink 抽象与 qq_split 合并 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 CLI/QQ channel 重复的"事件消费 + 中断 + agent task 调度"逻辑上提到 `agent::sink` 模块，通过 `OutputSink` trait 让 channel 只负责"如何输出"；同时把 `qq_split.rs` 合并到 `qq.rs`。

**Architecture:** 新增 `agent::sink` 模块定义 `OutputSink` trait 和 `run_turn` 函数。`run_turn` 内部 spawn agent task 并消费 `TurnEvent` 流，按事件调用 sink 回调；中断通过 `Arc<Notify>` 传入，中断时 `drop(rx)` 让 agent 检测 tx closed 优雅退出。`CliSink`/`QqSink` 各自实现 trait，channel 主 loop 只负责输入读取和 sink 构造。

**Tech Stack:** Rust 2021, tokio (mpsc + Notify + Mutex), async-trait, anyhow

**Spec:** [docs/specs/2026-08-01-channel-sink-abstraction-design.md](../specs/2026-08-01-channel-sink-abstraction-design.md)

---

## 文件结构

| 文件 | 责任 | 改动 |
|---|---|---|
| `src/agent/sink.rs` | `OutputSink` trait + `run_turn` 函数 + mock sink 测试 | 新建 |
| `src/agent/mod.rs` | 模块声明 | 加 `pub mod sink;` |
| `src/channels/cli.rs` | CLI channel + `CliSink` | 删事件循环，加 sink |
| `src/channels/qq.rs` | QQ channel + `QqSink` + `split_reply` | 删事件循环，加 sink，并入 split_reply |
| `src/channels/qq_split.rs` | （已合并到 qq.rs） | 删除 |
| `src/channels/mod.rs` | channel 模块声明 | 删 `pub mod qq_split;` |
| `tests/qq_split.rs` | split_reply 集成测试 | 改 import 路径 |

---

## Task 1: 创建 `agent::sink` 模块骨架与 `OutputSink` trait

**Files:**
- Create: `src/agent/sink.rs`
- Modify: `src/agent/mod.rs:1-3`

- [ ] **Step 1: 在 `src/agent/mod.rs` 加模块声明**

修改 [src/agent/mod.rs](../../src/agent/mod.rs) 第 1-3 行，在 `pub mod runner;` 后加一行：

```rust
pub mod context;
pub mod registry;
pub mod runner;
pub mod sink;

pub use crate::agent::registry::AgentRegistry;
```

- [ ] **Step 2: 创建 `src/agent/sink.rs`，定义 `OutputSink` trait**

```rust
use crate::agent::MediaKind;
use async_trait::async_trait;

/// channel 输出抽象：`run_turn` 按 `TurnEvent` 回调 sink 的方法。
/// channel 只实现"如何输出"，不关心 agent task 调度和中断。
#[async_trait]
pub trait OutputSink: Send {
    /// 文本增量
    async fn on_chunk(&mut self, delta: &str);
    /// 工具调用开始
    async fn on_tool_start(&mut self, name: &str);
    /// 工具执行结果（默认忽略，CLI override 打印预览）
    async fn on_tool_result(&mut self, _output: &str) {}
    /// Agent 请求发送媒体给用户
    async fn on_media(&mut self, path: &str, kind: MediaKind);
    /// 整轮正常结束
    async fn on_done(&mut self);
    /// 错误（已生成的文本保留，错误追加）
    async fn on_error(&mut self, message: &str);
    /// 被 /stop 或 Ctrl+C 中断
    async fn on_interrupted(&mut self);
}
```

- [ ] **Step 3: 验证编译**

Run: `cargo build`
Expected: 编译通过（trait 还没被使用，只有未使用警告）

- [ ] **Step 4: Commit**

```bash
git add src/agent/sink.rs src/agent/mod.rs
git commit -m "feat(agent): add OutputSink trait for channel output abstraction"
```

---

## Task 2: 实现 `run_turn` 函数并测试事件分发

**Files:**
- Modify: `src/agent/sink.rs`
- Test: `src/agent/sink.rs` (内联 `#[cfg(test)] mod tests`)

- [ ] **Step 1: 在 `src/agent/sink.rs` 加 `run_turn` 函数**

在 trait 定义下方追加：

```rust
use crate::agent::{Agent, TurnEvent};
use crate::provider::ChatMessage;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

/// 跑一轮 agent turn：spawn agent task → 消费 TurnEvent → 按 sink 回调输出。
///
/// - `stop`: 中断信号。notify 后 `run_turn` 会 `drop(rx)` 让 agent task 检测
///   tx closed 优雅退出（保存部分输出到 sqlite/context）。
/// - agent task 的 `Result<String>` 在此处消费，仅 log 错误；
///   channel 已通过 sink 收到全部事件，不需要再拿返回值。
pub async fn run_turn(
    agent: Arc<Mutex<Agent>>,
    user_msg: ChatMessage,
    channel: String,
    mut sink: Box<dyn OutputSink + Send>,
    stop: Arc<Notify>,
) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let agent_clone = agent.clone();
    let channel_clone = channel.clone();
    let join = tokio::spawn(async move {
        let mut a = agent_clone.lock().await;
        a.handle_message_streaming(user_msg, &channel_clone, tx).await
    });

    let mut interrupted = false;
    let mut agent_err: Option<String> = None;
    loop {
        tokio::select! {
            _ = stop.notified() => {
                interrupted = true;
                break;
            }
            ev = rx.recv() => {
                match ev {
                    Some(TurnEvent::Chunk { delta }) => sink.on_chunk(&delta).await,
                    Some(TurnEvent::ToolStart { name, .. }) => sink.on_tool_start(&name).await,
                    Some(TurnEvent::ToolResult { output, .. }) => sink.on_tool_result(&output).await,
                    Some(TurnEvent::MediaOutput { path, kind }) => sink.on_media(&path, kind).await,
                    Some(TurnEvent::Done) => break,
                    Some(TurnEvent::Error { message }) => {
                        agent_err = Some(message);
                        break;
                    }
                    None => break,
                }
            }
        }
    }

    // drop rx 让 agent task 检测 tx closed 优雅退出（保存部分输出）
    drop(rx);
    let task_result = join.await;

    if interrupted {
        sink.on_interrupted().await;
        // agent task 已通过 tx closed 路径保存部分输出，这里不重复处理
        return Ok(());
    }

    if let Err(e) = task_result {
        tracing::error!(error = %e, "agent task panicked");
        sink.on_error(&format!("agent task panicked: {}", e)).await;
        return Ok(());
    }
    let inner_result = task_result.unwrap();
    if let Some(msg) = agent_err {
        sink.on_error(&msg).await;
        return Ok(());
    }
    if let Err(e) = inner_result {
        tracing::error!(error = %e, "handle_message_streaming failed");
        sink.on_error(&format!("{}", e)).await;
        return Ok(());
    }
    sink.on_done().await;
    Ok(())
}
```

- [ ] **Step 2: 加 mock sink 测试模块**

在 `src/agent/sink.rs` 末尾追加：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::config::Config;
    use crate::memory::sqlite::SessionStore;
    use crate::provider::{Provider, StreamEvent, ToolCall};
    use async_stream::try_stream;
    use async_trait::async_trait;
    use futures_util::stream::BoxStream;
    use serde_json::json;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::mpsc;

    /// 记录所有 sink 调用，用于断言
    #[derive(Default)]
    struct MockSink {
        events: Arc<StdMutex<Vec<String>>>,
    }

    #[async_trait]
    impl OutputSink for MockSink {
        async fn on_chunk(&mut self, delta: &str) {
            self.events.lock().unwrap().push(format!("chunk:{}", delta));
        }
        async fn on_tool_start(&mut self, name: &str) {
            self.events.lock().unwrap().push(format!("tool_start:{}", name));
        }
        async fn on_tool_result(&mut self, output: &str) {
            self.events.lock().unwrap().push(format!("tool_result:{}", output));
        }
        async fn on_media(&mut self, path: &str, kind: MediaKind) {
            self.events.lock().unwrap().push(format!("media:{:?}:{}", kind, path));
        }
        async fn on_done(&mut self) {
            self.events.lock().unwrap().push("done".into());
        }
        async fn on_error(&mut self, message: &str) {
            self.events.lock().unwrap().push(format!("error:{}", message));
        }
        async fn on_interrupted(&mut self) {
            self.events.lock().unwrap().push("interrupted".into());
        }
    }

    /// Mock provider：按预设 rounds 依次返回事件流
    struct MockProvider {
        rounds: Arc<StdMutex<std::collections::VecDeque<Vec<StreamEvent>>>>,
    }

    impl MockProvider {
        fn new(rounds: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                rounds: Arc::new(StdMutex::new(rounds.into())),
            }
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat(&self, _req: &crate::provider::ChatRequest<'_>) -> anyhow::Result<crate::provider::ChatResponse> {
            unreachable!()
        }
        async fn chat_stream(&self, _req: &crate::provider::ChatRequest<'_>) -> BoxStream<'_, anyhow::Result<StreamEvent>> {
            let events = self.rounds.lock().unwrap().pop_front().unwrap_or_default();
            let s = try_stream! {
                for ev in events { yield ev; }
            };
            Box::pin(s)
        }
        fn native_tool_calling(&self) -> bool { true }
    }

    async fn make_agent(rounds: Vec<Vec<StreamEvent>>) -> Arc<Mutex<Agent>> {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("test", "test").unwrap();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(rounds));
        let tools = Arc::new(crate::agent::runner::ToolRegistry::new());
        let config = Config::default_for_workspace("/tmp/llaia-test");
        let agent = Agent::new(
            &config, provider, tools, Arc::new(store), sid,
            "test".into(), 8192, std::path::PathBuf::from("/tmp/llaia-test"),
        ).await;
        Arc::new(Mutex::new(agent))
    }

    #[tokio::test]
    async fn test_run_turn_plain_text_dispatches_done() {
        let agent = make_agent(vec![vec![
            StreamEvent::TextDelta("hello".into()),
            StreamEvent::Done,
        ]]).await;
        let events = Arc::new(StdMutex::new(vec![]));
        let sink = Box::new(MockSink { events: events.clone() });
        let stop = Arc::new(Notify::new());
        run_turn(agent, crate::provider::ChatMessage::user("hi"), "cli".into(), sink, stop).await.unwrap();
        let evs = events.lock().unwrap().clone();
        assert!(evs.iter().any(|s| s == "chunk:hello"));
        assert!(evs.iter().any(|s| s == "done"));
    }

    #[tokio::test]
    async fn test_run_turn_stop_notifies_interrupted() {
        // provider 返回一个慢流：先不结束，等 stop 信号
        // 用 mpsc 构造可控流
        let agent = make_agent(vec![vec![
            StreamEvent::TextDelta("partial".into()),
            // 不发 Done，模拟长任务
        ]]).await;
        let events = Arc::new(StdMutex::new(vec![]));
        let sink = Box::new(MockSink { events: events.clone() });
        let stop = Arc::new(Notify::new());

        // 先 notify 再 await，确保 select! 能收到
        let stop_clone = stop.clone();
        let handle = tokio::spawn(async move {
            run_turn(agent, crate::provider::ChatMessage::user("hi"), "cli".into(), sink, stop_clone).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        stop.notify_one();
        handle.await.unwrap().unwrap();

        let evs = events.lock().unwrap().clone();
        assert!(evs.iter().any(|s| s == "chunk:partial"));
        assert!(evs.iter().any(|s| s == "interrupted"));
        assert!(!evs.iter().any(|s| s == "done"));
    }

    #[tokio::test]
    async fn test_run_turn_tool_events_dispatched() {
        let tc = ToolCall { id: "1".into(), name: "echo".into(), arguments: json!({}) };
        let agent = make_agent(vec![
            vec![StreamEvent::ToolCall(tc), StreamEvent::Done],
            vec![StreamEvent::TextDelta("ok".into()), StreamEvent::Done],
        ]).await;
        let events = Arc::new(StdMutex::new(vec![]));
        let sink = Box::new(MockSink { events: events.clone() });
        let stop = Arc::new(Notify::new());
        run_turn(agent, crate::provider::ChatMessage::user("do"), "cli".into(), sink, stop).await.unwrap();
        let evs = events.lock().unwrap().clone();
        assert!(evs.iter().any(|s| s == "tool_start:echo"));
        assert!(evs.iter().any(|s| s == "chunk:ok"));
        assert!(evs.iter().any(|s| s == "done"));
    }
}
```

- [ ] **Step 3: 运行测试，验证通过**

Run: `cargo test --lib agent::sink`
Expected: 3 个测试全部 PASS

- [ ] **Step 4: Commit**

```bash
git add src/agent/sink.rs
git commit -m "feat(agent): implement run_turn with event dispatch and interruption"
```

---

## Task 3: 合并 `qq_split.rs` 到 `qq.rs`

**Files:**
- Modify: `src/channels/qq.rs` (末尾追加 `split_reply` + tests)
- Delete: `src/channels/qq_split.rs`
- Modify: `src/channels/mod.rs`

- [ ] **Step 1: 在 `src/channels/qq.rs` 末尾追加 `split_reply` 函数和 tests**

把 [src/channels/qq_split.rs](../../src/channels/qq_split.rs) 的全部内容（`split_reply` 函数 + `#[cfg(test)] mod tests`）追加到 [src/channels/qq.rs](../../src/channels/qq.rs) 末尾。函数签名和实现完全不变，只是换位置：

```rust
/// 将长文本按 QQ 单条消息上限分片。
///
/// 规则：
/// 1. 优先按段落（`\n\n`）切
/// 2. 单段超 max 时按行（`\n`）切
/// 3. 单行超 max 时按字符硬切
/// 4. 代码块跨片时闭合后再开，下一片以 ``` 同语言标记开始
pub fn split_reply(text: &str, max: usize) -> Vec<String> {
    // ... 完整实现从 qq_split.rs 原样搬过来 ...
}

#[cfg(test)]
mod split_reply_tests {
    use super::*;
    // ... 6 个测试原样搬过来 ...
}
```

注意：原 `qq_split.rs` 内的 tests 模块名是 `tests`，为避免与 qq.rs 内已有的 tests 模块冲突，重命名为 `split_reply_tests`。

- [ ] **Step 2: 删除 `src/channels/qq_split.rs`**

```bash
git rm src/channels/qq_split.rs
```

- [ ] **Step 3: 修改 `src/channels/mod.rs`，移除 `pub mod qq_split;`**

[src/channels/mod.rs](../../src/channels/mod.rs) 改为：

```rust
pub mod cli;
pub mod qq;

// 重新导出，方便外部使用
pub use cli::CliChannel;
pub use qq::QqChannel;

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

/// 抽象一个用户接入通道（CLI / QQ / 未来邮箱、web 等）。
/// 每个实现负责自己的 I/O 循环（读用户输入、写回复），
/// 共享同一个 AgentRegistry（main + sub_agents，通过 Arc<Mutex> 串行化访问）。
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    /// 启动 channel，阻塞运行直到退出。
    async fn run(self: Arc<Self>, registry: Arc<crate::agent::AgentRegistry>) -> Result<()>;
}
```

- [ ] **Step 4: 修改 `qq.rs` 内 `split_reply` 的引用**

[src/channels/qq.rs](../../src/channels/qq.rs) 顶部 `use crate::channels::qq_split::split_reply;` 删除（现在同文件内直接调用）。搜索文件内所有 `split_reply(` 调用，确认无需 import。

- [ ] **Step 5: 修改 `tests/qq_split.rs` 的 import 路径**

[tests/qq_split.rs](../../tests/qq_split.rs) 第 1 行改为：

```rust
use llaia::channels::qq::split_reply;
```

- [ ] **Step 6: 运行测试验证**

Run: `cargo test --lib channels::qq::split_reply_tests && cargo test --test qq_split`
Expected: 全部 PASS

- [ ] **Step 7: Commit**

```bash
git add src/channels/qq.rs src/channels/mod.rs tests/qq_split.rs
git commit -m "refactor(channels): merge qq_split.rs into qq.rs"
```

---

## Task 4: 实现 `CliSink` 并改造 CLI channel

**Files:**
- Modify: `src/channels/cli.rs`

- [ ] **Step 1: 在 `src/channels/cli.rs` 加 `CliSink` 结构和 impl**

在文件顶部 imports 调整后（加 `use crate::agent::sink::{OutputSink, run_turn};`、`use std::io::Write as _;`、`use tokio::sync::Notify;`），在 `CliChannel` 结构定义前加：

```rust
/// CLI 输出 sink：即时打印到 stdout
struct CliSink;

#[async_trait::async_trait]
impl OutputSink for CliSink {
    async fn on_chunk(&mut self, delta: &str) {
        print!("{}", delta);
        let _ = std::io::stdout().flush();
    }
    async fn on_tool_start(&mut self, name: &str) {
        println!("\n[tool: {}]", name);
    }
    async fn on_tool_result(&mut self, output: &str) {
        // 200 字符边界安全截断（与原 cli.rs 行为一致）
        let preview = if output.len() > 200 {
            let mut end = 200;
            while end > 0 && !output.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}...(truncated)", &output[..end])
        } else {
            output.to_string()
        };
        println!("[result: {}]", preview);
    }
    async fn on_media(&mut self, path: &str, kind: crate::agent::MediaKind) {
        let label = match kind {
            crate::agent::MediaKind::Image => "image",
            crate::agent::MediaKind::File => "file",
        };
        println!("[sent {}: {}]", label, path);
    }
    async fn on_done(&mut self) {
        println!("\n");
    }
    async fn on_error(&mut self, message: &str) {
        println!("\n[error: {}]\n", message);
    }
    async fn on_interrupted(&mut self) {
        println!("\n[stopped]");
    }
}
```

- [ ] **Step 2: 改造 `cli.rs` 生成态事件循环**

定位 [src/channels/cli.rs](../../src/channels/cli.rs) 中 `// spawn agent task，进入生成态` 处（约 187-278 行），把原来的"spawn + select! 消费 rx"替换为调用 `run_turn`。

把这段：

```rust
// spawn agent task，进入生成态
let (tx, mut rx) = tokio::sync::mpsc::channel(64);
let agent_clone = agent.clone();
tokio::spawn(async move {
    let mut a = agent_clone.lock().await;
    if let Err(e) = a
        .handle_message_streaming(user_msg, "cli", tx)
        .await
    {
        tracing::error!(error = %e, "handle_message_streaming failed");
    }
});
println!(); // 换行，分隔 prompt 和回复

print!(">> "); // 生成态提示符
std::io::stdout().flush()?;

// 生成态：select 监听 agent 事件 / stdin 输入 / Ctrl+C
loop {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => { println!("\n[stopped]"); break; }
        input = stdin_rx.recv() => { /* ... /stop 或排队 ... */ }
        ev = rx.recv() => { /* ... 按 TurnEvent 打印 ... */ }
    }
}
drop(rx);
```

替换为：

```rust
// 用 run_turn 跑这一轮，sink 即时打印
let stop = Arc::new(Notify::new());
let sink = Box::new(CliSink);
let agent_clone = agent.clone();
let turn_handle = tokio::spawn(run_turn(
    agent_clone,
    user_msg,
    "cli".into(),
    sink,
    stop.clone(),
));

println!(); // 换行，分隔 prompt 和回复
print!(">> "); // 生成态提示符
std::io::stdout().flush()?;

// 生成态：select 监听 turn 结束 / stdin 输入 / Ctrl+C
loop {
    tokio::select! {
        // Ctrl+C：触发中断，等 run_turn 自己结束
        _ = tokio::signal::ctrl_c() => {
            stop.notify_one();
        }
        input = stdin_rx.recv() => {
            match input {
                Some(Some(l)) if l == "/stop" => {
                    stop.notify_one();
                }
                Some(Some(l)) => {
                    println!("[queued: {}]", l);
                    queued_inputs.push(l);
                }
                Some(None) | None => {
                    // stdin EOF：等当前 turn 结束
                }
            }
        }
        res = &mut turn_handle => {
            // run_turn 结束（正常/中断/错误都走 sink 回调已打印）
            match res {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::error!(error = %e, "run_turn failed"),
                Err(e) => tracing::error!(error = %e, "run_turn task panicked"),
            }
            break;
        }
    }
}
```

注意：`turn_handle` 是 `JoinHandle<Result<()>>`，`&mut turn_handle` 在 select! 里可被 await。CLI 原来的"Ctrl+C 紧急中断"语义保留 —— 触发 notify 后继续等 JoinHandle，agent task 通过 tx closed 优雅退出。

- [ ] **Step 3: 删除 `cli.rs` 顶部不再需要的 imports**

搜索 `cli.rs` 顶部 use 块，删除以下不再直接使用的：

- `use crate::agent::TurnEvent;`（现在由 `run_turn` 内部消费）

确认仍需保留：`use crate::agent::Agent;`、`use crate::agent::AgentRegistry;`、`use crate::channels::Channel;` 等。

- [ ] **Step 4: 编译验证**

Run: `cargo build`
Expected: 编译通过

- [ ] **Step 5: 运行 CLI 集成测试**

Run: `cargo test --lib channels::cli`
Expected: 现有 CLI 测试（如有）PASS

- [ ] **Step 6: 手动冒烟测试（可选但推荐）**

Run: `cargo run -- chat`
测试场景：
1. 普通对话：输入 `hello`，看到回复即时打印
2. 工具调用：让 agent 调一个工具，看到 `[tool: xxx]` 和 `[result: ...]`
3. /stop：生成中输入 `/stop`，看到 `[stopped]`
4. Ctrl+C：生成中按 Ctrl+C，看到 `[stopped]`

- [ ] **Step 7: Commit**

```bash
git add src/channels/cli.rs
git commit -m "refactor(channels): CLI uses run_turn + CliSink, remove inline event loop"
```

---

## Task 5: 实现 `QqSink` 并改造 QQ channel

**Files:**
- Modify: `src/channels/qq.rs`

- [ ] **Step 1: 在 `src/channels/qq.rs` 加 imports**

在文件顶部 use 块加：

```rust
use crate::agent::sink::{OutputSink, run_turn};
use crate::agent::MediaKind;
```

删除不再直接需要的（如 `use tokio::sync::mpsc;`，如果只有 `handle_user_message` 用到且现在移除的话——保留 `Notify` 和 `Mutex`）。

- [ ] **Step 2: 在 `src/channels/qq.rs` 加 `QqSink` 结构和 impl**

放在 `QqChannel` impl 块之后、`Channel` impl 之前：

```rust
/// QQ 输出 sink：累积 chunk 后分片发送，工具调用即时通知
struct QqSink {
    qq: Arc<QqChannel>,
    user_openid: String,
    msg_id: String,
    buffer: String,
}

#[async_trait]
impl OutputSink for QqSink {
    async fn on_chunk(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }
    async fn on_tool_start(&mut self, name: &str) {
        let notice = format!("🔧 {}...", name);
        let _ = self.qq
            .send_c2c_message(&self.user_openid, &notice, Some(&self.msg_id))
            .await;
    }
    async fn on_media(&mut self, path: &str, kind: MediaKind) {
        if let Err(e) = self.qq
            .send_media_to_user(&self.user_openid, path, kind, Some(&self.msg_id))
            .await
        {
            tracing::error!(error = %e, path = path, "failed to send media");
            let _ = self.qq
                .send_c2c_message(
                    &self.user_openid,
                    &format!("[发送媒体失败: {}]", e),
                    Some(&self.msg_id),
                )
                .await;
        }
    }
    async fn on_done(&mut self) {
        // agent 可能只调工具无文本输出，buffer 为空时给占位回复
        // 否则 QQ 会因 content="" 返回 304061 invalid content
        let reply = if self.buffer.trim().is_empty() {
            tracing::warn!(total_len = self.buffer.len(), "agent reply empty, sending placeholder");
            "[已完成（无文本输出）]"
        } else {
            self.buffer.as_str()
        };
        let chunks = split_reply(reply, 1800);
        tracing::info!(chunks = chunks.len(), total_len = reply.len(), "sending reply");
        for (i, chunk) in chunks.iter().enumerate() {
            if chunk.trim().is_empty() {
                continue;
            }
            // 只有第一片带 msg_id（被动回复），后续片用主动消息
            let id = if i == 0 { Some(self.msg_id.as_str()) } else { None };
            if let Err(e) = self.qq.send_c2c_message(&self.user_openid, chunk, id).await {
                tracing::error!(error = %e, chunk = i, "failed to send chunk after retries");
            }
        }
    }
    async fn on_error(&mut self, message: &str) {
        let err_msg = if self.buffer.is_empty() {
            format!("[内部错误: {}]", message)
        } else {
            // 保留已生成文本，错误追加
            self.buffer.clone()
        };
        let chunks = split_reply(&err_msg, 1800);
        for (i, chunk) in chunks.iter().enumerate() {
            let id = if i == 0 { Some(self.msg_id.as_str()) } else { None };
            if let Err(e) = self.qq.send_c2c_message(&self.user_openid, chunk, id).await {
                tracing::error!(error = %e, chunk = i, "failed to send chunk after retries");
            }
        }
    }
    async fn on_interrupted(&mut self) {
        // /stop 的回复文本由中断触发方（QQ /stop handler）发送，这里只 log
        tracing::info!(user = %self.user_openid, "turn interrupted by /stop");
    }
}
```

- [ ] **Step 3: 改造 `handle_user_message` 的"普通消息"分支**

定位 [src/channels/qq.rs](../../src/channels/qq.rs) `handle_user_message` 方法中 `// 普通消息：spawn 子任务调 agent` 处（约 575-712 行），把原来的"spawn + select! 消费 rx + 中断 + 分片发送"替换为调用 `run_turn`。

把从 `// 普通消息：spawn 子任务调 agent` 到 `Ok(())` 结束的整段替换为：

```rust
// 普通消息：用 run_turn 跑这一轮，QqSink 负责输出
let stop = Arc::new(Notify::new());
{
    let mut stops = self.running_stops.lock().await;
    stops.insert(user_openid.to_string(), stop.clone());
}

let sink = Box::new(QqSink {
    qq: self.clone(),
    user_openid: user_openid.to_string(),
    msg_id: msg_id.to_string(),
    buffer: String::new(),
});

let turn_result = run_turn(agent.clone(), user_msg, "qq".into(), sink, stop).await;

// 清理中断信号注册
{
    let mut stops = self.running_stops.lock().await;
    stops.remove(user_openid);
}

turn_result?;
Ok(())
```

注意：原 `handle_user_message` 是 `self: Arc<Self>`，`self.clone()` 得到 `Arc<QqChannel>` 正好用于 `QqSink::qq` 字段。

- [ ] **Step 4: 删除 `qq.rs` 中不再直接使用的 imports**

搜索顶部 use 块，删除：

- `use tokio::sync::mpsc;`（如果只有原事件循环用到）
- `use crate::agent::TurnEvent;`（现在由 run_turn 消费）

保留：`use tokio::sync::{Mutex, Notify};`、`use crate::agent::Agent;` 等。

- [ ] **Step 5: 编译验证**

Run: `cargo build`
Expected: 编译通过

- [ ] **Step 6: 运行 QQ 相关测试**

Run: `cargo test --lib channels::qq && cargo test --test qq_http`
Expected: 全部 PASS（split_reply_tests、qq_http 集成测试）

- [ ] **Step 7: Commit**

```bash
git add src/channels/qq.rs
git commit -m "refactor(channels): QQ uses run_turn + QqSink, remove inline event loop"
```

---

## Task 6: 最终验证与清理

**Files:**
- 无新改动，仅全量验证

- [ ] **Step 1: 全量编译 + 测试**

Run: `cargo test`
Expected: 全部 PASS，无编译警告（或仅原有未相关警告）

- [ ] **Step 2: 检查无遗留引用**

Run: `grep -r "qq_split" src/ tests/`
Expected: 无匹配（已全部迁移）

Run: `grep -r "handle_message_streaming" src/channels/`
Expected: 无匹配（channel 不再直接调用，由 run_turn 内部调用）

- [ ] **Step 3: 检查 agent 模块导出**

确认 `src/agent/mod.rs` 的 `pub mod sink;` 存在，且 `sink` 模块的 `OutputSink` 和 `run_turn` 为 `pub`。

Run: `cargo doc --no-deps --document-private-items 2>&1 | grep -i sink`
Expected: 无错误

- [ ] **Step 4: 最终 commit（如有清理）**

```bash
git add -A
git commit -m "chore: finalize channel sink abstraction refactor"
```

或若无改动则跳过。

---

## Self-Review

**1. Spec coverage:**
- OutputSink trait + run_turn → Task 1, 2 ✓
- CliSink → Task 4 ✓
- QqSink → Task 5 ✓
- CLI run 改造 → Task 4 ✓
- QQ handle_user_message 改造 → Task 5 ✓
- qq_split 并入 qq.rs → Task 3 ✓
- 影响范围表所有文件 → Task 1-5 覆盖 ✓
- 测试策略（mock sink 单测、集成测试保持）→ Task 2 (mock sink), Task 3 (split_reply), Task 5 (qq_http) ✓
- 不变量（持久化、msg_seq、Ctrl+C、TurnEvent 不变）→ run_turn 内部 drop(rx) 保留 agent 优雅退出路径；QqSink 保留 msg_id 分片；CliSink 保留 Ctrl+C + 等 JoinHandle；TurnEvent 枚举未触碰 ✓

**2. Placeholder scan:** 无 TBD/TODO；所有代码块完整；测试代码完整可运行。

**3. Type一致性:** `OutputSink` 方法签名在 Task 1/2/4/5 一致；`run_turn` 签名在 Task 2/4/5 一致；`MediaKind` 复用 `crate::agent::MediaKind` 一致；`QqSink` 字段 `qq: Arc<QqChannel>` 在 Task 5 与 `handle_user_message` 的 `self: Arc<Self>` 一致。
