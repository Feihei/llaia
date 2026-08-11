use crate::provider::{
    ChatMessage, ChatRequest, ChatResponse, ContentPart, MessageContent, Provider, Role,
};
use anyhow::Result;

/// 压缩时工具消息内容保留的最大字符数（超出截断，完整内容已在 sqlite 留底）。
const TOOL_TRIM_CAP: usize = 500;

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

    /// 组装送给 provider 的消息列表。
    ///
    /// `tz` 为 `[runtime].timezone`，用于生成末尾的运行时状态栏（ADR-0017）。
    /// 状态栏不写入 `history`：每轮现算、只挂在尾部，system 前缀
    /// （SOUL/USER/MEMORY/Skills）逐轮字节一致，KV cache 才能命中。
    pub fn to_messages(&self, tz: &Option<String>) -> Vec<ChatMessage> {
        let mut msgs = vec![ChatMessage::system(&self.system)];
        if let Some(s) = &self.summary {
            msgs.push(ChatMessage::system(format!(
                "[Previous conversation summary]\n{}",
                s
            )));
        }
        msgs.extend(self.history.iter().cloned());
        msgs.push(ChatMessage::user(crate::time::status_bar(tz)));
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

    /// 压缩上下文：先跑廉价抽取式归一化（不调 LLM），若仍在预算内则跳过 LLM 摘要；
    /// 否则摘要旧消息、保留最近 `keep_recent` 条 + 首条用户消息锚点（ADR-0004）。
    ///
    /// 返回是否真的调了 LLM（cheap-first 命中时为 `false`，供调用方日志/统计）。
    /// `token_budget` 为压缩目标 token 上限，通常用 `context_size`。
    pub async fn compact(
        &mut self,
        provider: &dyn Provider,
        keep_recent: usize,
        token_budget: usize,
    ) -> Result<bool> {
        // 1) 廉价抽取式归一化（不调 LLM，每次必跑）
        self.cheap_normalize();

        // 2) cheap-first：归一化后已回到预算内则跳过 LLM，
        //    保留 summary 前缀稳定（KV cache 友好）也省一次调用
        if self.history.len() <= keep_recent || self.estimate_tokens() <= token_budget {
            return Ok(false);
        }

        // 3) 重要性锚点（ADR-0004「首条用户消息留」）：落在 to_compress 区的首条用户消息
        //    提出来前置到 to_keep，且不进摘要 dump
        let first_user_idx = self.history.iter().position(|m| m.role == Role::User);
        let anchor = first_user_idx
            .filter(|&i| i < self.history.len() - keep_recent)
            .map(|i| self.history[i].clone());

        let split = self.history.len() - keep_recent;
        let mut to_compress: Vec<ChatMessage> = self.history[..split].to_vec();
        let mut to_keep: Vec<ChatMessage> = self.history[split..].to_vec();

        if let Some(a) = anchor {
            if let Some(i) = first_user_idx {
                to_compress.remove(i);
            }
            to_keep.insert(0, a);
        }

        // 4) 摘要 dump：工具消息只给一行归档注记，不让大段工具输出进 LLM
        let mut dump = String::new();
        for m in &to_compress {
            if m.role == Role::Tool {
                let t = m.content.as_text();
                let note = if t.chars().count() > 80 {
                    format!("{}…", t.chars().take(80).collect::<String>())
                } else {
                    t
                };
                dump.push_str(&format!("[tool] (结果已归档) {}\n", note));
                continue;
            }
            let role_str = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool", // 不可达，上面已 continue
            };
            dump.push_str(&format!("[{}] {}\n", role_str, m.content.as_text()));
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

        // to_keep 已是 cheap_normalize 后的内容（图片已降级、工具已截断），直接采用
        self.history = to_keep;
        Ok(true)
    }

    /// 廉价抽取式归一化（不调 LLM）：丢弃空消息、图片降级、工具消息截断、连续重复去重。
    /// 在 `compact` 每轮开头对整段 history 跑一次，幂等。
    fn cheap_normalize(&mut self) {
        let mut out: Vec<ChatMessage> = Vec::with_capacity(self.history.len());
        for mut m in self.history.drain(..) {
            let text = m.content.as_text();
            // 丢弃空消息（工具消息除外，保留与 assistant 的 tool_call 配对）
            if text.trim().is_empty() && m.role != Role::Tool {
                continue;
            }
            // 多模态图片降级为文本占位（省 token，图片信息已由 vision 模型描述进入上下文）
            if m.content.has_image() {
                if let MessageContent::Multimodal(parts) = &m.content {
                    let t = parts
                        .iter()
                        .map(|p| match p {
                            ContentPart::Text { text } => text.clone(),
                            ContentPart::ImageUrl { .. } => "[图片]".to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    m.content = MessageContent::Text(t);
                }
            }
            // 工具消息截断到上限（完整内容已在 sqlite 留底，ADR-0004「工具调用结果可丢」）
            if m.role == Role::Tool {
                let t = m.content.as_text();
                if t.chars().count() > TOOL_TRIM_CAP {
                    let head: String = t.chars().take(TOOL_TRIM_CAP).collect();
                    m.content =
                        MessageContent::Text(format!("{}…[已截断，完整结果见会话记录]", head));
                }
            }
            // 连续重复用户消息去重（只留一条）
            if m.role == Role::User {
                if let Some(last) = out.last() {
                    if last.role == Role::User && last.content.as_text() == m.content.as_text() {
                        continue;
                    }
                }
            }
            out.push(m);
        }
        self.history = out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Role;

    #[test]
    fn test_to_messages_includes_system() {
        let ctx = Context::new("SOUL".into());
        let msgs = ctx.to_messages(&None);
        // system + 状态栏
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::System);
    }

    #[test]
    fn test_summary_inserted() {
        let mut ctx = Context::new("SOUL".into());
        ctx.summary = Some("old stuff".into());
        ctx.push(ChatMessage::user("hi"));
        let msgs = ctx.to_messages(&None);
        // system + summary + history + 状态栏
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[1].role, Role::System);
    }

    #[test]
    fn test_status_bar_is_last_and_not_persisted() {
        let mut ctx = Context::new("SOUL".into());
        ctx.push(ChatMessage::user("hi"));
        let msgs = ctx.to_messages(&Some("Asia/Shanghai".into()));
        let last = msgs.last().unwrap();
        assert_eq!(last.role, Role::User);
        assert!(last.content.as_text().contains("Asia/Shanghai"));
        // 状态栏只在 to_messages 里现算，不落进 history
        assert_eq!(ctx.history.len(), 1);
    }

    #[test]
    fn test_system_prefix_stable_across_turns() {
        // 缓存友好性回归：前缀部分逐轮必须字节一致，只有尾部状态栏会变
        let mut ctx = Context::new("SOUL".into());
        ctx.push(ChatMessage::user("hi"));
        let a = ctx.to_messages(&None);
        let b = ctx.to_messages(&None);
        assert_eq!(
            a[..a.len() - 1]
                .iter()
                .map(|m| m.content.as_text())
                .collect::<Vec<_>>(),
            b[..b.len() - 1]
                .iter()
                .map(|m| m.content.as_text())
                .collect::<Vec<_>>()
        );
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

    /// 超出预算 → 真调 LLM：history 收敛到 keep_recent，summary 生成。
    #[tokio::test]
    async fn test_compact_runs_llm_when_over_budget() {
        let mut ctx = Context::new("SOUL".into());
        for i in 0..10 {
            ctx.push(ChatMessage::user(format!("msg {}", i).repeat(40)));
        }
        // 10 条 × ~160 字符 ≈ 400 token，budget=100 → 必超
        let used_llm = ctx.compact(&MockProvider, 3, 100).await.unwrap();
        assert!(used_llm);
        // 首条用户消息作为锚点被前置保留 → 长度 = keep_recent + 1
        assert_eq!(ctx.history.len(), 4);
        assert!(ctx.history[0].content.as_text().contains("msg 0"));
        assert!(ctx.summary.is_some());
        assert!(ctx.summary.as_ref().unwrap().contains("summary of old"));
    }

    /// cheap-first：归一化后已回到预算内 → 不调 LLM（返回 false），history 不动。
    #[tokio::test]
    async fn test_compact_skips_llm_under_budget() {
        let mut ctx = Context::new("SOUL".into());
        for i in 0..10 {
            ctx.push(ChatMessage::user(format!("msg {}", i))); // 小消息
        }
        let used_llm = ctx.compact(&MockProvider, 3, 10_000).await.unwrap();
        assert!(!used_llm);
        assert_eq!(ctx.history.len(), 10);
        assert!(ctx.summary.is_none());
    }

    /// ADR-0004「首条用户消息留」：首条用户消息永不被摘要掉。
    #[tokio::test]
    async fn test_compact_preserves_first_user_message() {
        let mut ctx = Context::new("SOUL".into());
        ctx.push(ChatMessage::user(
            "IMPORTANT initial instruction".repeat(20),
        ));
        for i in 0..10 {
            ctx.push(ChatMessage::user(format!("msg {}", i).repeat(40)));
        }
        ctx.compact(&MockProvider, 3, 100).await.unwrap();
        let first = ctx.history.first().expect("history non-empty");
        assert_eq!(first.role, Role::User);
        assert!(first
            .content
            .as_text()
            .contains("IMPORTANT initial instruction"));
    }

    /// 工具消息截断：大段工具输出被砍到 TOOL_TRIM_CAP 内。
    #[tokio::test]
    async fn test_compact_trims_tool_messages() {
        let mut ctx = Context::new("SOUL".into());
        ctx.push(ChatMessage::assistant("let me check"));
        ctx.push(ChatMessage::tool("x".repeat(5000), "call_1"));
        ctx.push(ChatMessage::user("result?"));
        // budget 极小 → 走 LLM 路径也会先归一化截断工具消息
        ctx.compact(&MockProvider, 3, 1).await.unwrap();
        let tool_msg = ctx
            .history
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("tool message kept");
        assert!(tool_msg.content.as_text().chars().count() <= TOOL_TRIM_CAP + 60);
        assert!(tool_msg.content.as_text().contains("已截断"));
    }

    /// 廉价去重：连续重复用户消息只留一条。
    #[tokio::test]
    async fn test_compact_dedup_consecutive_user() {
        let mut ctx = Context::new("SOUL".into());
        ctx.push(ChatMessage::user("same"));
        ctx.push(ChatMessage::user("same"));
        ctx.push(ChatMessage::user("same"));
        ctx.push(ChatMessage::user("different"));
        // budget 足够大 → 不进 LLM，但 cheap_normalize 仍去重
        ctx.compact(&MockProvider, 3, 10_000).await.unwrap();
        let users: Vec<&ChatMessage> = ctx
            .history
            .iter()
            .filter(|m| m.role == Role::User)
            .collect();
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].content.as_text(), "same");
        assert_eq!(users[1].content.as_text(), "different");
    }
}
