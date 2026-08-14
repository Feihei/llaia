//! Provider 降级链：主模型请求失败时自动切换到备用模型。
//!
//! 行为：
//! - `chat`：依次尝试链中 provider，第一个成功即返回；全失败返回最后一个错误
//! - `chat_stream`：取首个事件探测失败（`StreamEvent::Error` 作为第一个事件），
//!   失败则换下一个 provider 重新建流；流开始产出正常事件后中断不再降级
//! - 其余方法（native_tool_calling / label / detect_context_size）取链首

use super::{ChatRequest, ChatResponse, Provider, StreamEvent};
use anyhow::Result;
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use std::sync::Arc;

pub struct FallbackProvider {
    chain: Vec<Arc<dyn Provider>>,
}

impl FallbackProvider {
    /// chain 至少含两个 provider（主 + 备用）；调用方保证非空。
    pub fn new(chain: Vec<Arc<dyn Provider>>) -> Self {
        Self { chain }
    }

    fn main(&self) -> &Arc<dyn Provider> {
        &self.chain[0]
    }
}

#[async_trait]
impl Provider for FallbackProvider {
    async fn chat(&self, req: &ChatRequest<'_>) -> Result<ChatResponse> {
        let mut last_err: Option<anyhow::Error> = None;
        for (i, p) in self.chain.iter().enumerate() {
            match p.chat(req).await {
                Ok(resp) => {
                    if i > 0 {
                        tracing::info!(index = i, model = %p.label(), "fallback provider succeeded");
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    tracing::warn!(
                        index = i,
                        model = %p.label(),
                        error = %e,
                        "provider failed, trying next in fallback chain"
                    );
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("empty fallback chain")))
    }

    async fn chat_stream(&self, req: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>> {
        let mut idx = 0;
        loop {
            let mut stream = self.chain[idx].chat_stream(req).await;
            let first = stream.next().await;
            // 首个事件即 Error 且还有备用 → 换下一个 provider 重建流
            let first_is_error = matches!(&first, Some(Ok(StreamEvent::Error(_))));
            if first_is_error && idx + 1 < self.chain.len() {
                tracing::warn!(
                    index = idx,
                    model = %self.chain[idx].label(),
                    "stream failed before first token, falling back"
                );
                idx += 1;
                continue;
            }
            // 把已取出的首个事件拼回流头部（Option 自身实现 IntoIterator：0 或 1 个元素）
            return Box::pin(futures_util::stream::iter(first).chain(stream));
        }
    }

    fn native_tool_calling(&self) -> bool {
        self.main().native_tool_calling()
    }

    async fn detect_context_size(&self) -> Option<usize> {
        // 保守起见取链中最小的上下文窗口
        let mut min: Option<usize> = None;
        for p in &self.chain {
            if let Some(n) = p.detect_context_size().await {
                min = Some(min.map_or(n, |m: usize| m.min(n)));
            }
        }
        min
    }

    fn label(&self) -> String {
        self.main().label()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatMessage, ToolCall};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 可配置失败次数与返回文本的 mock provider
    struct MockProvider {
        label: String,
        fail_times: AtomicU32,
        reply: String,
        context_size: Option<usize>,
    }

    impl MockProvider {
        fn new(label: &str, fail_times: u32, reply: &str, context_size: Option<usize>) -> Self {
            Self {
                label: label.into(),
                fail_times: AtomicU32::new(fail_times),
                reply: reply.into(),
                context_size,
            }
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat(&self, _req: &ChatRequest<'_>) -> Result<ChatResponse> {
            if self.fail_times.fetch_sub(1, Ordering::SeqCst) > 0 {
                return Err(anyhow::anyhow!("mock failure from {}", self.label));
            }
            Ok(ChatResponse {
                text: Some(self.reply.clone()),
                tool_calls: vec![],
                usage: None,
                finish_reason: None,
            })
        }

        async fn chat_stream(&self, _req: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>> {
            if self.fail_times.fetch_sub(1, Ordering::SeqCst) > 0 {
                return Box::pin(futures_util::stream::iter(vec![Ok(StreamEvent::Error(
                    format!("mock stream failure from {}", self.label),
                ))]));
            }
            let reply = self.reply.clone();
            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta(reply)),
                Ok(StreamEvent::Done),
            ]))
        }

        fn native_tool_calling(&self) -> bool {
            true
        }

        async fn detect_context_size(&self) -> Option<usize> {
            self.context_size
        }

        fn label(&self) -> String {
            self.label.clone()
        }
    }

    #[tokio::test]
    async fn test_chat_falls_back_on_error() {
        let chain = vec![
            Arc::new(MockProvider::new("bad", 1, "never", None)) as Arc<dyn Provider>,
            Arc::new(MockProvider::new("good", 0, "ok", None)) as Arc<dyn Provider>,
        ];
        let fb = FallbackProvider::new(chain);
        let msgs = vec![ChatMessage::user("hi")];
        let r = ChatRequest {
            messages: &msgs,
            tools: None,
        };
        let resp = fb.chat(&r).await.unwrap();
        assert_eq!(resp.text.as_deref(), Some("ok"));
    }

    #[tokio::test]
    async fn test_chat_all_fail_returns_last_error() {
        let chain = vec![
            Arc::new(MockProvider::new("bad1", 1, "", None)) as Arc<dyn Provider>,
            Arc::new(MockProvider::new("bad2", 1, "", None)) as Arc<dyn Provider>,
        ];
        let fb = FallbackProvider::new(chain);
        let msgs = vec![ChatMessage::user("hi")];
        let r = ChatRequest {
            messages: &msgs,
            tools: None,
        };
        let err = fb.chat(&r).await.unwrap_err();
        assert!(err.to_string().contains("bad2"));
    }

    #[tokio::test]
    async fn test_stream_falls_back_on_first_error_event() {
        let chain = vec![
            Arc::new(MockProvider::new("bad", 1, "never", None)) as Arc<dyn Provider>,
            Arc::new(MockProvider::new("good", 0, "streamed", None)) as Arc<dyn Provider>,
        ];
        let fb = FallbackProvider::new(chain);
        let msgs = vec![ChatMessage::user("hi")];
        let r = ChatRequest {
            messages: &msgs,
            tools: None,
        };
        let events: Vec<_> = fb.chat_stream(&r).await.collect().await;
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            Ok(StreamEvent::TextDelta(t)) if t == "streamed"
        ));
        assert!(matches!(&events[1], Ok(StreamEvent::Done)));
    }

    #[tokio::test]
    async fn test_metadata_from_chain_head() {
        let chain = vec![
            Arc::new(MockProvider::new("main", 0, "", Some(4096))) as Arc<dyn Provider>,
            Arc::new(MockProvider::new("backup", 0, "", Some(2048))) as Arc<dyn Provider>,
        ];
        let fb = FallbackProvider::new(chain);
        assert_eq!(fb.label(), "main");
        assert!(fb.native_tool_calling());
        // detect_context_size 取链中最小值
        assert_eq!(fb.detect_context_size().await, Some(2048));
    }

    #[tokio::test]
    async fn test_tool_calls_pass_through() {
        struct ToolProvider;
        #[async_trait]
        impl Provider for ToolProvider {
            async fn chat(&self, _req: &ChatRequest<'_>) -> Result<ChatResponse> {
                Ok(ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "1".into(),
                        name: "file_read".into(),
                        arguments: serde_json::json!({}),
                    }],
                    usage: None,
                    finish_reason: None,
                })
            }
            async fn chat_stream(
                &self,
                _req: &ChatRequest<'_>,
            ) -> BoxStream<'_, Result<StreamEvent>> {
                Box::pin(futures_util::stream::empty())
            }
            fn native_tool_calling(&self) -> bool {
                true
            }
        }
        let chain = vec![
            Arc::new(MockProvider::new("bad", 1, "", None)) as Arc<dyn Provider>,
            Arc::new(ToolProvider) as Arc<dyn Provider>,
        ];
        let fb = FallbackProvider::new(chain);
        let msgs = vec![ChatMessage::user("hi")];
        let r = ChatRequest {
            messages: &msgs,
            tools: None,
        };
        let resp = fb.chat(&r).await.unwrap();
        assert_eq!(resp.tool_calls.len(), 1);
    }
}
