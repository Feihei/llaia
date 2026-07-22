pub mod context;
pub mod runner;

use crate::agent::context::Context;
use crate::agent::runner::{execute_tool_calls, ToolRegistry};
use crate::config::Config;
use crate::memory::sqlite::SessionStore;
use crate::provider::{ChatMessage, ChatRequest, Provider, Role, StreamEvent};
use anyhow::Result;
use futures_util::StreamExt;
use std::sync::Arc;
use tokio::sync::mpsc;

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

pub struct Agent {
    pub provider: Arc<dyn Provider>,
    pub tools: Arc<ToolRegistry>,
    pub context: Context,
    pub session_store: Arc<SessionStore>,
    pub session_id: i64,
    pub max_tokens: usize,
    pub context_threshold: f64,
    pub max_iterations: u32,
    pub qq_confirm_mode: String,
}

impl Agent {
    pub async fn new(
        config: &Config,
        provider: Arc<dyn Provider>,
        tools: Arc<ToolRegistry>,
        session_store: Arc<SessionStore>,
        session_id: i64,
        system_prompt: String,
        max_tokens: usize,
    ) -> Self {
        Self {
            provider,
            tools,
            context: Context::new(system_prompt),
            session_store,
            session_id,
            max_tokens,
            context_threshold: config.runtime.context_threshold,
            max_iterations: config.runtime.max_iterations,
            qq_confirm_mode: config.channels.qq.confirm_mode.clone(),
        }
    }

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

            let mut stream = self.provider.chat_stream(&req).await;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatRequest, ChatResponse, Provider, StreamEvent, ToolCall};
    use async_stream::try_stream;
    use async_trait::async_trait;
    use futures_util::stream::BoxStream;
    use serde_json::json;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::mpsc;

    /// Mock provider：每次 chat_stream 调用返回下一组预设事件
    struct MockProvider {
        native: bool,
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
            let s = try_stream! {
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

    async fn make_agent_with_rounds(
        native: bool,
        rounds: Vec<Vec<StreamEvent>>,
    ) -> Agent {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("test", "test").unwrap();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(native, rounds));
        let tools = Arc::new(ToolRegistry::new());
        let config = Config::default_for_workspace("/tmp/laia-test");
        Agent::new(
            &config,
            provider,
            tools,
            Arc::new(store),
            sid,
            "test system".into(),
            8192,
        )
        .await
    }

    #[tokio::test]
    async fn test_streaming_plain_text() {
        let rounds = vec![vec![
            StreamEvent::TextDelta("hello ".into()),
            StreamEvent::TextDelta("world".into()),
            StreamEvent::Done,
        ]];
        let mut agent = make_agent_with_rounds(true, rounds).await;
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
        let tc = ToolCall {
            id: "call_1".into(),
            name: "echo".into(),
            arguments: json!({}),
        };
        let rounds = vec![
            vec![StreamEvent::ToolCall(tc), StreamEvent::Done],
            vec![StreamEvent::TextDelta("done".into()), StreamEvent::Done],
        ];
        let mut agent = make_agent_with_rounds(true, rounds).await;
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
        assert_eq!(tool_starts, vec!["echo"]);
        assert_eq!(chunks.concat(), "done");
        assert_eq!(done_count, 1);
    }

    #[tokio::test]
    async fn test_streaming_tag_mode_filters_tags() {
        let tag = "\u{3c}tool_call\u{3e}{\"name\":\"x\",\"arguments\":{}}\u{3c}/tool_call\u{3e}";
        let rounds = vec![vec![
            StreamEvent::TextDelta("before ".into()),
            StreamEvent::TextDelta(tag.to_string()),
            StreamEvent::TextDelta(" after".into()),
            StreamEvent::Done,
        ]];
        let mut agent = make_agent_with_rounds(false, rounds).await;
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
        assert_eq!(chunks.concat(), "before  after");
        assert_eq!(tool_starts, vec!["x"]);
    }
}
