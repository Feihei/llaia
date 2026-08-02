# Channel OutputSink 抽象与 qq_split 合并

- 状态：Accepted
- 日期：2026-08-01

## 背景

当前 CLI 和 QQ 两个 channel 在事件消费逻辑上存在明显重复：

- [cli.rs:188-278](../../src/channels/cli.rs) 和 [qq.rs:576-712](../../src/channels/qq.rs) 都做"spawn agent task + 消费 `rx` 上的 `TurnEvent` + 监听中断信号"
- 差异只在"如何输出"：CLI 即时 `print!`，QQ 累积到 buffer 后分片发送
- 差异在"如何中断"：CLI `drop(rx)`，QQ 用 `Notify`

此外 [qq_split.rs](../../src/channels/qq_split.rs) 只服务 [qq.rs](../../src/channels/qq.rs) 一个调用方，独立成模块增加跳转成本但无复用价值。

## 目标

- 把重复的"事件消费 + 中断 + agent task 调度"逻辑上提到 `agent` 模块
- channel 只负责实现"如何输出"
- 合并 qq_split.rs 到 qq.rs

## 非目标

- 不重构多模态消息构造（CLI 的 `@path` 解析 vs QQ 的附件下载），差异大留在各自 channel
- 不重构斜杠命令分发（CLI 接 `/exit`，QQ 不接），留在各自 channel
- 不调整 `build_agent` 工厂函数位置（独立议题）

## 设计

### 新增 `agent::sink` 模块

定义 `OutputSink` trait，把 channel 的"如何输出"抽象成回调接口：

```rust
#[async_trait]
pub trait OutputSink: Send {
    async fn on_chunk(&mut self, delta: &str);
    async fn on_tool_start(&mut self, name: &str);
    async fn on_tool_result(&mut self, output: &str) {} // 默认忽略
    async fn on_media(&mut self, path: &str, kind: MediaKind);
    async fn on_done(&mut self);
    async fn on_error(&mut self, message: &str);
    async fn on_interrupted(&mut self);
}
```

- `on_tool_result` 给默认实现（CLI 打印预览、QQ 忽略），其余必须实现
- `MediaKind` 复用 `agent::MediaKind`，不再在 trait 内重定义

### 新增 `agent::run_turn` 函数

承接现在两个 channel 重复的"spawn agent task + 消费 rx + 中断"逻辑：

```rust
pub async fn run_turn(
    agent: Arc<Mutex<Agent>>,
    user_msg: ChatMessage,
    channel: String,
    sink: Box<dyn OutputSink + Send>,
    stop: Arc<Notify>,
) -> Result<()>
```

内部流程：

1. `tokio::spawn` 起一个 agent task：lock agent → `handle_message_streaming(user_msg, channel, tx)` → 返回 `Result<String>`
2. 主任务 `select!` 监听：
   - `stop.notified()` → `interrupted = true; break`
   - `rx.recv()` → 按 `TurnEvent` 分发到 sink 方法
3. 中断时 `drop(rx)` 让 agent task 检测 tx closed 优雅退出（保存部分输出到 sqlite/context，与现有行为一致）
4. `join.await` 等 agent task 结束
5. 中断 → `sink.on_interrupted()`；正常 → `sink.on_done()`；agent 返回 `Err` → `sink.on_error(msg)`

**关键不变量**：

- `run_turn` 持有 `JoinHandle` 直到结束，agent task 的 `Result<String>` 在 `run_turn` 内消费（仅用 log 记录错误，不再返回给 channel —— channel 已通过 sink 收到全部事件）
- sink 的回调在 `run_turn` 的上下文里同步 `await`，sink 实现内部可持有 `Arc<QqChannel>` 等状态

### CliSink 实现

```rust
struct CliSink;

#[async_trait]
impl OutputSink for CliSink {
    async fn on_chunk(&mut self, delta: &str) { print!("{}", delta); let _ = io::stdout().flush(); }
    async fn on_tool_start(&mut self, name: &str) { println!("\n[tool: {}]", name); }
    async fn on_tool_result(&mut self, output: &str) {
        // 复用现有 200 字符边界安全的预览逻辑
        let preview = if output.len() > 200 { /* 截断 */ } else { output.to_string() };
        println!("[result: {}]", preview);
    }
    async fn on_media(&mut self, path: &str, kind: MediaKind) {
        let label = match kind { MediaKind::Image => "image", MediaKind::File => "file" };
        println!("[sent {}: {}]", label, path);
    }
    async fn on_done(&mut self) { println!("\n"); }
    async fn on_error(&mut self, message: &str) { println!("\n[error: {}]\n", message); }
    async fn on_interrupted(&mut self) { println!("\n[stopped]"); }
}
```

### QqSink 实现

```rust
struct QqSink {
    qq: Arc<QqChannel>,
    user_openid: String,
    msg_id: String,
    buffer: String,
}

#[async_trait]
impl OutputSink for QqSink {
    async fn on_chunk(&mut self, delta: &str) { self.buffer.push_str(delta); }
    async fn on_tool_start(&mut self, name: &str) {
        let notice = format!("🔧 {}...", name);
        let _ = self.qq.send_c2c_message(&self.user_openid, &notice, Some(&self.msg_id)).await;
    }
    async fn on_media(&mut self, path: &str, kind: MediaKind) {
        if let Err(e) = self.qq.send_media_to_user(&self.user_openid, path, kind, Some(&self.msg_id)).await {
            tracing::error!(error = %e, path = path, "failed to send media");
            let _ = self.qq.send_c2c_message(&self.user_openid, &format!("[发送媒体失败: {}]", e), Some(&self.msg_id)).await;
        }
    }
    async fn on_done(&mut self) {
        // 复用现有空 buffer 占位逻辑 + split_reply 分片发送
        // 第一片带 msg_id（被动回复），后续片主动消息
    }
    async fn on_error(&mut self, message: &str) {
        let _ = self.qq.send_c2c_message(&self.user_openid, &format!("[错误: {}]", message), Some(&self.msg_id)).await;
    }
    async fn on_interrupted(&mut self) {
        // /stop 的回复文本由中断触发方（QQ /stop handler）发送，这里只 log
        tracing::info!(user = %self.user_openid, "turn interrupted by /stop");
    }
}
```

### CLI run 改造

[cli.rs](../../src/channels/cli.rs) 主 loop 的生成态分支改为：

```rust
// 构造 user_msg（@path 图片解析逻辑保留不动）...
let stop = Arc::new(Notify::new());
let sink = Box::new(CliSink);
let agent_clone = agent.clone();
let turn_handle = tokio::spawn(run_turn(agent_clone, user_msg, "cli".into(), sink, stop.clone()));

// 生成态 select!：
// - stdin 有输入：/stop → stop.notify_one()；其他 → 入队
// - JoinHandle 完成 → 退出生成态
// - Ctrl+C → stop.notify_one()（等 JoinHandle 完成后再退出）
```

注意：CLI 现有的"Ctrl+C 紧急中断"语义保留 —— 触发 notify 后仍等 JoinHandle，agent task 通过 tx closed 优雅退出。

### QQ handle_user_message 改造

[qq.rs](../../src/channels/qq.rs) `handle_user_message` 的"普通消息"分支改为：

```rust
// 构造 user_msg（附件下载逻辑保留不动）...
let stop = Arc::new(Notify::new());
{
    let mut stops = self.running_stops.lock().await;
    stops.insert(user_openid.to_string(), stop.clone());
}
let sink = Box::new(QqSink { qq: self.clone(), user_openid, msg_id, buffer: String::new() });
run_turn(agent.clone(), user_msg, "qq".into(), sink, stop).await?;
{
    let mut stops = self.running_stops.lock().await;
    stops.remove(user_openid);
}
```

QQ 的 /stop handler 保持现有逻辑：从 `running_stops` 取出 `Notify` 并 `notify_one()`。

### qq_split 并入 qq.rs

- [qq_split.rs](../../src/channels/qq_split.rs) 整个文件（`split_reply` 函数 + tests）移到 [qq.rs](../../src/channels/qq.rs) 末尾
- 删除 `qq_split.rs`
- [channels/mod.rs](../../src/channels/mod.rs) 移除 `pub mod qq_split;`
- `QqSink::on_done` 内直接调本文件的 `split_reply`，不再跨模块

## 影响范围

| 文件 | 改动 |
|---|---|
| 新增 `src/agent/sink.rs` | `OutputSink` trait + `run_turn` 函数 |
| `src/agent/mod.rs` | 新增 `pub mod sink;` 声明 |
| `src/channels/cli.rs` | 删事件循环，加 `CliSink` + spawn `run_turn` |
| `src/channels/qq.rs` | 删事件循环，加 `QqSink` + 调 `run_turn` + 并入 `split_reply` 及其 tests |
| `src/channels/qq_split.rs` | 删除 |
| `src/channels/mod.rs` | 删 `pub mod qq_split;` |
| `tests/qq_split.rs` | 路径调整或合并到 qq 集成测试 |

## 测试策略

- `OutputSink` 是 trait，可用 mock sink 单测 `run_turn` 的事件分发与中断行为（不依赖具体 channel）
- `CliSink` / `QqSink` 各自的输出行为保持现有集成测试覆盖（`tests/qq_split.rs` 验证 `split_reply`，`tests/qq_http.rs` 验证 send 路径）
- `run_turn` 的中断语义（drop rx → agent 优雅退出）通过 mock provider + mock sink 验证：notify 后 sink 收到 `on_interrupted`，agent task 的部分输出已持久化

## 不变量

- agent task 的持久化行为（sqlite/context 保存部分输出）不变
- QQ 的 msg_seq 递增、被动回复 msg_id 关联、分片发送策略不变
- CLI 的输入排队、Ctrl+C 语义不变
- TurnEvent 枚举本身不变（只是消费方从 channel 移到 `run_turn`）
