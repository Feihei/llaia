pub mod context;
pub mod runner;

use crate::agent::context::Context;
use crate::agent::runner::{execute_tool_calls, ToolRegistry};
use crate::config::Config;
use crate::memory::sqlite::SessionStore;
use crate::provider::{ChatMessage, ChatRequest, Provider, Role};
use crate::tool_call::parse_tool_calls;
use anyhow::Result;
use std::sync::Arc;

pub struct Agent {
    pub provider: Arc<dyn Provider>,
    pub tools: Arc<ToolRegistry>,
    pub context: Context,
    pub session_store: Arc<SessionStore>,
    pub session_id: i64,
    pub max_tokens: usize,
    pub context_threshold: f64,
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
        let context_threshold = config
            .agent
            .get("main")
            .map(|a| a.context_threshold)
            .unwrap_or(0.7);
        Self {
            provider,
            tools,
            context: Context::new(system_prompt),
            session_store,
            session_id,
            max_tokens,
            context_threshold,
        }
    }

    pub async fn handle_input(&mut self, user_input: &str, _channel: &str) -> Result<String> {
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

        let max_iters = 10;
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

            let resp = self.provider.chat(&req).await?;

            let (final_text, final_calls) = if !self.provider.native_tool_calling() {
                let text = resp.text.unwrap_or_default();
                let (clean, calls) = parse_tool_calls(&text);
                (Some(clean), calls)
            } else {
                (resp.text, resp.tool_calls)
            };

            if final_calls.is_empty() {
                let text = final_text.unwrap_or_default();
                self.session_store
                    .append_message(self.session_id, &Role::Assistant, &text)?;
                self.context.push(ChatMessage::assistant(&text));
                return Ok(text);
            }

            let assistant_msg = ChatMessage::assistant_with_tools(
                final_text.clone().unwrap_or_default(),
                final_calls.clone(),
            );
            let assistant_msg_id = self.session_store.append_message(
                self.session_id,
                &Role::Assistant,
                &final_text.clone().unwrap_or_default(),
            )?;
            self.context.push(assistant_msg);

            for tc in &final_calls {
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

            let tool_msgs = execute_tool_calls(&self.tools, &final_calls).await?;
            for msg in tool_msgs.iter() {
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
        Ok(fallback.into())
    }
}
