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
    /// 长任务心跳：按墙钟每 KEEPALIVE_INTERVAL 回调一次，`elapsed` 为自本轮开始
    /// 的累计时长（默认忽略；交互聊天频道 override 发送 "still working" 提示，
    /// 避免用户误以为卡死）。与事件是否密集无关，保证长循环也会周期提示。
    async fn on_keepalive(&mut self, _elapsed: std::time::Duration) {}
    /// 单轮超过最大时长被自动中断时回调（默认忽略；聊天频道 override 说明原因），
    /// 防止模型陷入无限循环却对用户保持沉默。
    async fn on_auto_stopped(&mut self, _reason: &str) {}
    /// Agent 请求发送媒体给用户
    async fn on_media(&mut self, path: &str, kind: MediaKind);
    /// 整轮正常结束
    async fn on_done(&mut self);
    /// 错误（已生成的文本保留，错误追加）
    async fn on_error(&mut self, message: &str);
    /// 被 /stop 或 Ctrl+C 中断
    async fn on_interrupted(&mut self);
}

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
///
/// 长任务心跳：按墙钟每 `runtime.keepalive_interval_secs` 触发一次
/// `sink.on_keepalive(elapsed)`，让聊天频道发 "still working"，避免用户以为卡死。
/// 与事件是否密集无关——即便模型在连续工具调用循环中也会周期提示。仅聊天频道生效。
/// 防死循环：单轮运行超过 `runtime.max_turn_duration_secs` 自动中断
/// （`sink.on_auto_stopped`），防止模型陷入无限循环却对用户保持沉默。
pub async fn run_turn(
    agent: Arc<Mutex<Agent>>,
    user_msg: ChatMessage,
    channel: String,
    mut sink: Box<dyn OutputSink + Send>,
    stop: Arc<Notify>,
) -> Result<()> {
    // 心跳周期与单轮上限全部来自 [runtime] 配置（秒），便于 WebUI/CLI 调整
    let (keepalive_interval, max_turn_duration) = {
        let a = agent.lock().await;
        let live = a.live_config();
        let cfg = live.read().await;
        (
            std::time::Duration::from_secs(cfg.runtime.keepalive_interval_secs),
            std::time::Duration::from_secs(cfg.runtime.max_turn_duration_secs),
        )
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let agent_clone = agent.clone();
    let channel_clone = channel.clone();
    let join = tokio::spawn(async move {
        let mut a = agent_clone.lock().await;
        a.handle_message_streaming(user_msg, &channel_clone, tx)
            .await
    });

    let mut interrupted = false;
    let mut agent_err: Option<String> = None;
    let mut auto_stopped = false;
    // 墙钟基线：从 turn_start 计算心跳和超时
    let turn_start = tokio::time::Instant::now();
    // 已触发的心跳次数，用于计算下一次心跳阈值
    let mut keepalive_count: u32 = 0;
    loop {
        let turn_elapsed = tokio::time::Instant::now().saturating_duration_since(turn_start);

        // 单轮超过最大时长 → 自动中断，防止模型无限循环
        if turn_elapsed >= max_turn_duration {
            auto_stopped = true;
            sink.on_auto_stopped(&format!(
                "任务运行超过 {} 分钟仍无结果，已自动停止（可重发消息继续）",
                max_turn_duration.as_secs() / 60
            ))
            .await;
            break;
        }

        // 下次心跳距离 turn_start 多少秒
        let next_heartbeat = keepalive_interval
            .checked_mul(keepalive_count + 1)
            .unwrap_or(max_turn_duration);
        let keepalive_wait = if turn_elapsed >= next_heartbeat {
            tokio::time::sleep(std::time::Duration::ZERO)
        } else {
            tokio::time::sleep(next_heartbeat - turn_elapsed)
        };

        tokio::select! {
            _ = stop.notified() => {
                interrupted = true;
                break;
            }
            ev = rx.recv() => {
                match ev {
                    Some(ev) => {
                        match ev {
                            TurnEvent::Chunk { delta } => sink.on_chunk(&delta).await,
                            TurnEvent::ToolStart { name, .. } => sink.on_tool_start(&name).await,
                            TurnEvent::ToolResult { output, .. } => sink.on_tool_result(&output).await,
                            TurnEvent::MediaOutput { path, kind } => sink.on_media(&path, kind).await,
                            TurnEvent::Done => break,
                            TurnEvent::Error { message } => {
                                agent_err = Some(message);
                                break;
                            }
                        }
                    }
                    None => break,
                }
            }
            _ = keepalive_wait => {
                // 墙钟心跳：不管有没有事件，每 KEEPALIVE_INTERVAL 发一次
                // 上报累计时长，方便聊天频道提示 "still working for N minutes"
                let elapsed = tokio::time::Instant::now().saturating_duration_since(turn_start);
                keepalive_count += 1;
                sink.on_keepalive(elapsed).await;
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
    // 自动中断（单轮超时）：已通过 on_auto_stopped 说明原因，不再追加 on_done
    if auto_stopped {
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
            self.events
                .lock()
                .unwrap()
                .push(format!("tool_start:{}", name));
        }
        async fn on_tool_result(&mut self, output: &str) {
            self.events
                .lock()
                .unwrap()
                .push(format!("tool_result:{}", output));
        }
        async fn on_media(&mut self, path: &str, kind: MediaKind) {
            self.events
                .lock()
                .unwrap()
                .push(format!("media:{:?}:{}", kind, path));
        }
        async fn on_done(&mut self) {
            self.events.lock().unwrap().push("done".into());
        }
        async fn on_error(&mut self, message: &str) {
            self.events
                .lock()
                .unwrap()
                .push(format!("error:{}", message));
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
        async fn chat(
            &self,
            _req: &crate::provider::ChatRequest<'_>,
        ) -> anyhow::Result<crate::provider::ChatResponse> {
            unreachable!()
        }
        async fn chat_stream(
            &self,
            _req: &crate::provider::ChatRequest<'_>,
        ) -> BoxStream<'_, anyhow::Result<StreamEvent>> {
            let events = self.rounds.lock().unwrap().pop_front().unwrap_or_default();
            let s = try_stream! {
                for ev in events { yield ev; }
                // 事件 yield 完后短暂挂起，模拟未立即结束的流，让中断测试能在流结束前触发 stop。
                // 含 Done 的测试用例会在 Done 时 break，不会走到这里。
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            };
            Box::pin(s)
        }
        fn native_tool_calling(&self) -> bool {
            true
        }
    }

    async fn make_agent(rounds: Vec<Vec<StreamEvent>>) -> Arc<Mutex<Agent>> {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("test", "test").unwrap();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(rounds));
        let tools = Arc::new(crate::agent::runner::ToolRegistry::new());
        let config = Config::default_for_workspace("/tmp/llaia-test");
        let agent = Agent::new(
            &config,
            Some(provider),
            None,
            None,
            tools,
            Arc::new(store),
            sid,
            "test".into(),
            8192,
            std::path::PathBuf::from("/tmp/llaia-test/workspace"),
            Arc::new(tokio::sync::RwLock::new(std::path::PathBuf::from(
                "/tmp/llaia-test/workspace",
            ))),
            std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new())),
            std::path::PathBuf::from("/tmp/llaia-test"),
            true,
            "main".into(),
            None,
        )
        .await;
        Arc::new(Mutex::new(agent))
    }

    #[tokio::test]
    async fn test_run_turn_plain_text_dispatches_done() {
        let agent = make_agent(vec![vec![
            StreamEvent::TextDelta("hello".into()),
            StreamEvent::Done,
        ]])
        .await;
        let events = Arc::new(StdMutex::new(vec![]));
        let sink = Box::new(MockSink {
            events: events.clone(),
        });
        let stop = Arc::new(Notify::new());
        run_turn(
            agent,
            crate::provider::ChatMessage::user("hi"),
            "cli".into(),
            sink,
            stop,
        )
        .await
        .unwrap();
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
        ]])
        .await;
        let events = Arc::new(StdMutex::new(vec![]));
        let sink = Box::new(MockSink {
            events: events.clone(),
        });
        let stop = Arc::new(Notify::new());

        // 先 notify 再 await，确保 select! 能收到
        let stop_clone = stop.clone();
        let handle = tokio::spawn(async move {
            run_turn(
                agent,
                crate::provider::ChatMessage::user("hi"),
                "cli".into(),
                sink,
                stop_clone,
            )
            .await
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
        let tc = ToolCall {
            id: "1".into(),
            name: "echo".into(),
            arguments: json!({}),
        };
        let agent = make_agent(vec![
            vec![StreamEvent::ToolCall(tc), StreamEvent::Done],
            vec![StreamEvent::TextDelta("ok".into()), StreamEvent::Done],
        ])
        .await;
        let events = Arc::new(StdMutex::new(vec![]));
        let sink = Box::new(MockSink {
            events: events.clone(),
        });
        let stop = Arc::new(Notify::new());
        run_turn(
            agent,
            crate::provider::ChatMessage::user("do"),
            "cli".into(),
            sink,
            stop,
        )
        .await
        .unwrap();
        let evs = events.lock().unwrap().clone();
        assert!(evs.iter().any(|s| s == "tool_start:echo"));
        assert!(evs.iter().any(|s| s == "chunk:ok"));
        assert!(evs.iter().any(|s| s == "done"));
    }
}
