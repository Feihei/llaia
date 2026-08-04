use crate::provider::{ChatMessage, ChatRequest, ChatResponse, Provider, Role};
use anyhow::Result;

/// 当前会话的上下文窗口。
pub struct Context {
    pub system: String,
    pub history: Vec<ChatMessage>,
    pub summary: Option<String>,
}

impl Context {
    pub fn new(system: String) -> Self {
        Self {
            system,
            history: Vec::new(),
            summary: None,
        }
    }

    pub fn push(&mut self, msg: ChatMessage) {
        self.history.push(msg);
    }

    pub fn to_messages(&self) -> Vec<ChatMessage> {
        let mut msgs = vec![ChatMessage::system(&self.system)];
        if let Some(s) = &self.summary {
            msgs.push(ChatMessage::system(format!(
                "[Previous conversation summary]\n{}",
                s
            )));
        }
        msgs.extend(self.history.iter().cloned());
        msgs
    }

    pub fn estimate_tokens(&self) -> usize {
        let system_tokens = self.system.chars().count() / 4;
        let summary_tokens = self
            .summary
            .as_ref()
            .map(|s| s.chars().count() / 4)
            .unwrap_or(0);
        let history_tokens: usize = self
            .history
            .iter()
            .map(|m| m.content.as_text().chars().count() / 4)
            .sum();
        system_tokens + summary_tokens + history_tokens
    }

    pub fn clear(&mut self) {
        self.history.clear();
    }

    pub fn needs_compaction(&self, context_size: usize, threshold: f64) -> bool {
        let current = self.estimate_tokens();
        (current as f64 / context_size as f64) > threshold
    }

    pub async fn compact(&mut self, provider: &dyn Provider, keep_recent: usize) -> Result<()> {
        if self.history.len() <= keep_recent {
            return Ok(());
        }
        let to_compress: Vec<ChatMessage> =
            self.history[..self.history.len() - keep_recent].to_vec();
        let mut to_keep: Vec<ChatMessage> =
            self.history[self.history.len() - keep_recent..].to_vec();

        let mut dump = String::new();
        for m in &to_compress {
            let role_str = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            dump.push_str(&format!("[{}] {}\n", role_str, m.content.as_text()));
            // 多模态消息的图片部分用占位标注（base64 不送入 summary 生成）
            if let crate::provider::MessageContent::Multimodal(parts) = &m.content {
                let img_count = parts
                    .iter()
                    .filter(|p| matches!(p, crate::provider::ContentPart::ImageUrl { .. }))
                    .count();
                if img_count > 0 {
                    dump.push_str(&format!("[{}张图片]\n", img_count));
                }
            }
        }

        let system = "You are a conversation summarizer. Summarize the following conversation into a concise paragraph preserving key facts, decisions, and context. Output only the summary.";
        let messages = vec![ChatMessage::system(system), ChatMessage::user(dump)];
        let req = ChatRequest {
            messages: &messages,
            tools: None,
        };
        let resp: ChatResponse = provider.chat(&req).await?;
        let summary = resp.text.unwrap_or_default();

        let new_summary = match &self.summary {
            Some(old) => format!("{}\n\n[Later]\n{}", old, summary),
            None => summary,
        };
        self.summary = Some(new_summary);

        // 对保留的消息做图片降级：把含图片的 Multimodal 降级为 Text（省 token）
        // 图片信息已进入 summary，保留在 history 的图片 base64 会持续消耗 token
        for m in &mut to_keep {
            if let crate::provider::MessageContent::Multimodal(parts) = &m.content {
                let text = parts
                    .iter()
                    .map(|p| match p {
                        crate::provider::ContentPart::Text { text } => text.clone(),
                        crate::provider::ContentPart::ImageUrl { .. } => "[图片]".to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                m.content = crate::provider::MessageContent::Text(text);
            }
        }
        self.history = to_keep;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Role;

    #[test]
    fn test_to_messages_includes_system() {
        let ctx = Context::new("SOUL".into());
        let msgs = ctx.to_messages();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::System);
    }

    #[test]
    fn test_summary_inserted() {
        let mut ctx = Context::new("SOUL".into());
        ctx.summary = Some("old stuff".into());
        ctx.push(ChatMessage::user("hi"));
        let msgs = ctx.to_messages();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1].role, Role::System);
    }

    #[test]
    fn test_token_estimate() {
        let mut ctx = Context::new("a".repeat(40));
        ctx.push(ChatMessage::user("b".repeat(40)));
        assert_eq!(ctx.estimate_tokens(), 20);
    }

    #[test]
    fn test_needs_compaction() {
        let mut ctx = Context::new("a".repeat(80));
        ctx.push(ChatMessage::user("b".repeat(80)));
        assert!(ctx.needs_compaction(100, 0.3));
        assert!(!ctx.needs_compaction(100, 0.5));
    }
}

#[cfg(test)]
mod compact_tests {
    use super::*;
    use crate::provider::{ChatRequest, ChatResponse, Provider, StreamEvent};
    use async_trait::async_trait;

    struct MockProvider;
    #[async_trait]
    impl Provider for MockProvider {
        async fn chat(&self, _req: &ChatRequest<'_>) -> Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some("summary of old".into()),
                tool_calls: vec![],
            })
        }
        async fn chat_stream(
            &self,
            _req: &ChatRequest<'_>,
        ) -> futures_util::stream::BoxStream<'_, Result<StreamEvent>> {
            let s = async_stream::try_stream! {
                yield StreamEvent::TextDelta("summary of old".into());
                yield StreamEvent::Done;
            };
            Box::pin(s)
        }
        fn native_tool_calling(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn test_compact() {
        let mut ctx = Context::new("SOUL".into());
        for i in 0..10 {
            ctx.push(ChatMessage::user(format!("msg {}", i)));
        }
        ctx.compact(&MockProvider, 3).await.unwrap();
        assert_eq!(ctx.history.len(), 3);
        assert!(ctx.summary.is_some());
        assert!(ctx.summary.as_ref().unwrap().contains("summary of old"));
    }
}
