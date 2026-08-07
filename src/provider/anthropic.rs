//! Anthropic Messages API provider（SSE 流式）。
//!
//! 与 OpenAI 兼容协议的差异（转换层职责）：
//! - system 消息提升到顶层 `system` 字段（多条拼接）
//! - assistant 的工具调用 → `tool_use` content block
//! - tool 结果 → user 消息里的 `tool_result` block
//! - 相邻同 role 消息合并（API 要求 user/assistant 交替）
//! - `max_tokens` 必填（API 硬要求）
//!
//! 参考实现：zeroclaw-providers/src/anthropic.rs（Apache-2.0 / MIT），按其 payload
//! 构造与 SSE 解析逻辑裁剪重写为 llaia 的 StreamEvent 模型。
//! API 文档：<https://docs.anthropic.com/en/api/messages>

use crate::provider::{
    ChatRequest, ChatResponse, ContentPart, MessageContent, Provider, Role, StreamEvent, ToolCall,
};
use anyhow::{anyhow, Result};
use async_stream::try_stream;
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use reqwest::Client;
use serde::Serialize;
use std::time::Duration;

/// 流式响应单 chunk 读取超时（秒），与 openai_compat 对齐
const STREAM_CHUNK_TIMEOUT_SECS: u64 = 120;
/// max_tokens 未配置时的默认值
const DEFAULT_MAX_TOKENS: usize = 4096;
/// API 版本头（当前稳定版）
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    max_tokens: usize,
}

impl AnthropicProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        max_tokens: usize,
    ) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
            max_tokens: if max_tokens == 0 {
                DEFAULT_MAX_TOKENS
            } else {
                max_tokens
            },
        })
    }
}

// ---- 请求 payload 模型 ----

#[derive(Serialize)]
struct MessagesRequest {
    model: String,
    max_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ApiTool>>,
    stream: bool,
}

#[derive(Serialize)]
struct ApiMessage {
    role: &'static str,
    content: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct ApiTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

/// 把 ChatMessage 序列转为 Anthropic content blocks。
/// system 消息返回 None（调用方提升到顶层 system 字段）。
fn message_to_blocks(
    msg: &crate::provider::ChatMessage,
) -> Option<(&'static str, Vec<serde_json::Value>)> {
    match msg.role {
        Role::System => None,
        Role::User => {
            let mut blocks = Vec::new();
            match &msg.content {
                MessageContent::Text(s) => {
                    if !s.is_empty() {
                        blocks.push(serde_json::json!({ "type": "text", "text": s }));
                    }
                }
                MessageContent::Multimodal(parts) => {
                    for p in parts {
                        match p {
                            ContentPart::Text { text } => {
                                blocks.push(serde_json::json!({ "type": "text", "text": text }));
                            }
                            ContentPart::ImageUrl { image_url } => {
                                // data URL → base64 block；http(s) URL 直传不被 API 支持，降级为文本说明
                                if let Some(block) = data_url_to_image_block(&image_url.url) {
                                    blocks.push(block);
                                } else {
                                    blocks.push(serde_json::json!({
                                        "type": "text",
                                        "text": format!("[image: {}]", image_url.url)
                                    }));
                                }
                            }
                        }
                    }
                }
            }
            Some(("user", blocks))
        }
        Role::Assistant => {
            let mut blocks = Vec::new();
            let text = msg.content.as_text();
            if !text.is_empty() {
                blocks.push(serde_json::json!({ "type": "text", "text": text }));
            }
            if let Some(tcs) = &msg.tool_calls {
                for tc in tcs {
                    blocks.push(serde_json::json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.arguments,
                    }));
                }
            }
            Some(("assistant", blocks))
        }
        Role::Tool => {
            let block = serde_json::json!({
                "type": "tool_result",
                "tool_use_id": msg.tool_call_id.clone().unwrap_or_default(),
                "content": msg.content.as_text(),
            });
            Some(("user", vec![block]))
        }
    }
}

/// data:image/xxx;base64,... → Anthropic image block
fn data_url_to_image_block(url: &str) -> Option<serde_json::Value> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(";base64,")?;
    Some(serde_json::json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": meta,
            "data": data,
        }
    }))
}

/// 转换完整消息列表：system 提升 + 相邻同 role 合并。
/// 返回 (system, messages)。若合并后首条不是 user，前置占位 user 消息（API 要求 user 开头）。
fn convert_messages(
    messages: &[crate::provider::ChatMessage],
) -> (Option<String>, Vec<ApiMessage>) {
    let mut system_parts: Vec<String> = Vec::new();
    let mut out: Vec<ApiMessage> = Vec::new();
    for msg in messages {
        let Some((role, blocks)) = message_to_blocks(msg) else {
            let text = msg.content.as_text();
            if !text.trim().is_empty() {
                system_parts.push(text);
            }
            continue;
        };
        if blocks.is_empty() {
            continue;
        }
        // 相邻同 role 合并
        if let Some(last) = out.last_mut() {
            if last.role == role {
                last.content.extend(blocks);
                continue;
            }
        }
        out.push(ApiMessage {
            role,
            content: blocks,
        });
    }
    // API 要求首条为 user
    if out.first().is_some_and(|m| m.role != "user") {
        out.insert(
            0,
            ApiMessage {
                role: "user",
                content: vec![serde_json::json!({ "type": "text", "text": "(continue)" })],
            },
        );
    }
    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };
    (system, out)
}

#[async_trait]
impl Provider for AnthropicProvider {
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

    async fn chat_stream(&self, req: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>> {
        let url = format!("{}/v1/messages", self.base_url);
        let (system, messages) = convert_messages(req.messages);
        if messages.is_empty() {
            return Box::pin(try_stream! {
                yield StreamEvent::Error("no messages to send".to_string());
            });
        }
        let tools: Option<Vec<ApiTool>> = req.tools.map(|ts| {
            ts.iter()
                .map(|t| ApiTool {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    input_schema: t.parameters.clone(),
                })
                .collect()
        });
        let body = MessagesRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            system,
            messages,
            tools,
            stream: true,
        };

        let mut request = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body);
        // 部分兼容网关用 Bearer 鉴权，两者都给不冲突
        if !self.api_key.is_empty() {
            request = request.bearer_auth(&self.api_key);
        }

        tracing::info!(url = %url, model = %self.model, "provider request sending");

        let mut resp = match request.send().await {
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

        tracing::info!("provider stream started");

        let s = try_stream! {
            let mut buf = String::new();
            // 当前 tool_use block 累积状态（content_block_start 开，content_block_stop 出）
            let mut tc_id = String::new();
            let mut tc_name = String::new();
            let mut tc_args = String::new();
            let mut in_tool_block = false;

            loop {
                let chunk = match tokio::time::timeout(
                    Duration::from_secs(STREAM_CHUNK_TIMEOUT_SECS),
                    resp.chunk(),
                ).await {
                    Ok(Ok(Some(c))) => c,
                    Ok(Ok(None)) => break,
                    Ok(Err(e)) => {
                        yield StreamEvent::Error(format!("stream chunk error: {}", e));
                        return;
                    }
                    Err(_) => {
                        yield StreamEvent::Error(format!(
                            "stream chunk timeout (no data in {}s)",
                            STREAM_CHUNK_TIMEOUT_SECS
                        ));
                        return;
                    }
                };
                buf.push_str(std::str::from_utf8(&chunk).unwrap_or(""));
                while let Some(pos) = buf.find("\n\n") {
                    let event_str = buf[..pos].to_string();
                    buf = buf[pos + 2..].to_string();
                    // 一个 SSE event 可能有多行 data:，拼接后解析
                    let mut data = String::new();
                    for line in event_str.lines() {
                        let line = line.trim();
                        if let Some(d) = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:")) {
                            data.push_str(d.trim());
                        }
                    }
                    if data.is_empty() {
                        continue;
                    }
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) else {
                        continue;
                    };
                    let ev_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match ev_type {
                        "content_block_start" => {
                            if let Some(cb) = v.get("content_block") {
                                if cb.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                                    in_tool_block = true;
                                    tc_id = cb.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                                    tc_name = cb.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                                    tc_args.clear();
                                }
                            }
                        }
                        "content_block_delta" => {
                            if let Some(delta) = v.get("delta") {
                                match delta.get("type").and_then(|t| t.as_str()) {
                                    Some("text_delta") => {
                                        if let Some(t) = delta.get("text").and_then(|t| t.as_str()) {
                                            if !t.is_empty() {
                                                yield StreamEvent::TextDelta(t.to_string());
                                            }
                                        }
                                    }
                                    Some("input_json_delta") => {
                                        if let Some(p) = delta.get("partial_json").and_then(|p| p.as_str()) {
                                            tc_args.push_str(p);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        "content_block_stop" => {
                            if in_tool_block {
                                in_tool_block = false;
                                let args: serde_json::Value = serde_json::from_str(&tc_args)
                                    .unwrap_or(serde_json::Value::Null);
                                yield StreamEvent::ToolCall(ToolCall {
                                    id: std::mem::take(&mut tc_id),
                                    name: std::mem::take(&mut tc_name),
                                    arguments: args,
                                });
                            }
                        }
                        "error" => {
                            let msg = v.get("error")
                                .and_then(|e| e.get("message"))
                                .and_then(|m| m.as_str())
                                .unwrap_or("unknown anthropic error");
                            yield StreamEvent::Error(msg.to_string());
                            return;
                        }
                        "message_stop" => {
                            tracing::info!("provider stream done (message_stop)");
                            yield StreamEvent::Done;
                            return;
                        }
                        // message_start / ping / message_delta 等忽略
                        _ => {}
                    }
                }
            }
            tracing::info!("provider stream done (stream ended)");
            yield StreamEvent::Done;
        };
        Box::pin(s)
    }

    /// Anthropic 只支持 native tool calling
    fn native_tool_calling(&self) -> bool {
        true
    }

    /// Anthropic 无本地探测端点，返回 None 走配置值/默认
    async fn detect_context_size(&self) -> Option<usize> {
        None
    }

    fn label(&self) -> String {
        self.model.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ChatMessage;

    #[test]
    fn test_system_extraction() {
        let msgs = vec![
            ChatMessage::system("你是助手"),
            ChatMessage::system("说话简洁"),
            ChatMessage::user("hi"),
        ];
        let (system, messages) = convert_messages(&msgs);
        assert_eq!(system.as_deref(), Some("你是助手\n\n说话简洁"));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
    }

    #[test]
    fn test_adjacent_same_role_merge() {
        let msgs = vec![
            ChatMessage::user("问题1"),
            ChatMessage::user("问题2"),
            ChatMessage::assistant("答"),
        ];
        let (_, messages) = convert_messages(&msgs);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content.len(), 2); // 两条 user 合并
    }

    #[test]
    fn test_tool_result_becomes_user_block() {
        let msgs = vec![
            ChatMessage::user("读文件"),
            ChatMessage::assistant_with_tools(
                "",
                vec![ToolCall {
                    id: "tu_1".into(),
                    name: "file_read".into(),
                    arguments: serde_json::json!({"path": "/tmp"}),
                }],
            ),
            ChatMessage::tool("文件内容", "tu_1"),
        ];
        let (_, messages) = convert_messages(&msgs);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2].role, "user");
        assert_eq!(messages[2].content[0]["type"], "tool_result");
        assert_eq!(messages[2].content[0]["tool_use_id"], "tu_1");
        // assistant 的 tool_use block
        assert_eq!(messages[1].content[0]["type"], "tool_use");
        assert_eq!(messages[1].content[0]["id"], "tu_1");
    }

    #[test]
    fn test_leading_assistant_gets_placeholder() {
        let msgs = vec![ChatMessage::assistant("先说话")];
        let (_, messages) = convert_messages(&msgs);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[1].role, "assistant");
    }

    #[test]
    fn test_data_url_to_image_block() {
        let block = data_url_to_image_block("data:image/png;base64,AAA=").unwrap();
        assert_eq!(block["type"], "image");
        assert_eq!(block["source"]["media_type"], "image/png");
        assert_eq!(block["source"]["data"], "AAA=");
        assert!(data_url_to_image_block("https://example.com/a.png").is_none());
    }

    #[test]
    fn test_empty_messages_skipped() {
        let msgs = vec![ChatMessage::user(""), ChatMessage::assistant("hi")];
        let (_, messages) = convert_messages(&msgs);
        // 空 user 被跳过，assistant 变首条 → 前置占位 user
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
    }
}
