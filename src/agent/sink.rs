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
        a.handle_message_streaming(user_msg, &channel_clone, tx)
            .await
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
            provider,
            tools,
            Arc::new(store),
            sid,
            "test".into(),
            8192,
            std::path::PathBuf::from("/tmp/llaia-test/workspace"),
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
