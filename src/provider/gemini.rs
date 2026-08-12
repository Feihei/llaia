//! Google Gemini REST provider（SSE 流式）。
//!
//! 与 OpenAI 兼容协议 / Anthropic 的差异（转换层职责）：
//! - system 消息提升到顶层 `systemInstruction` 字段
//! - assistant 消息 → role "model"，工具调用 → `functionCall` content part
//! - tool 结果 → user 消息里的 `functionResponse` part（需函数名；
//!   ChatMessage::tool 只带 tool_call_id，这里在遍历时缓存 assistant 的 id→name 映射补全）
//! - 相邻同 role 消息合并（API 要求 user/model 交替）
//! - `maxOutputTokens` 经 generationConfig 下发
//!
//! 参考实现：zeroclaw-providers/src/anthropic.rs（Apache-2.0 / MIT）的 SSE 解析思路，
//! 按 Gemini 的 REST 负载结构裁剪重写。
//! API 文档：<https://ai.google.dev/api/generate-content>

use crate::provider::{
    ChatRequest, ChatResponse, ContentPart, MessageContent, Provider, Role, StreamEvent, ToolCall,
};
use anyhow::{anyhow, Result};
use async_stream::try_stream;
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use reqwest::Client;
use serde::Serialize;
use std::collections::HashMap;
use std::time::Duration;

/// 流式响应单 chunk 读取超时（秒），与 openai_compat / anthropic 对齐
const STREAM_CHUNK_TIMEOUT_SECS: u64 = 120;
/// maxOutputTokens 未配置时的默认值
const DEFAULT_MAX_OUTPUT_TOKENS: usize = 4096;

pub struct GeminiProvider {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    max_output_tokens: usize,
}

impl GeminiProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        max_output_tokens: usize,
    ) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
            max_output_tokens: if max_output_tokens == 0 {
                DEFAULT_MAX_OUTPUT_TOKENS
            } else {
                max_output_tokens
            },
        })
    }

    /// 流式端点：`:streamGenerateContent?alt=sse`
    fn stream_url(&self) -> String {
        format!(
            "{}/v1beta/models/{}:streamGenerateContent?alt=sse",
            self.base_url, self.model
        )
    }

    /// 非流式端点：`:generateContent`
    fn generate_url(&self) -> String {
        format!(
            "{}/v1beta/models/{}:generateContent",
            self.base_url, self.model
        )
    }
}

// ---- 请求 payload 模型 ----

#[derive(Serialize)]
struct GenerateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<SystemInstruction>,
    contents: Vec<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
}

#[derive(Serialize)]
struct SystemInstruction {
    parts: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct Content {
    role: String,
    parts: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct GeminiTool {
    function_declarations: Vec<FunctionDeclaration>,
}

#[derive(Serialize)]
struct FunctionDeclaration {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Serialize)]
struct GenerationConfig {
    max_output_tokens: usize,
}

/// data:image/xxx;base64,... → Gemini inlineData part
fn data_url_to_inline_data(url: &str) -> Option<serde_json::Value> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(";base64,")?;
    Some(serde_json::json!({
        "inlineData": {
            "mimeType": meta,
            "data": data,
        }
    }))
}

/// 把一条 ChatMessage 转成 Gemini 的 (role, parts)。
/// system 消息返回 None（调用方提升到 systemInstruction）。
/// `call_name_by_id` 用来在 tool 消息处补全 functionResponse 所需的函数名。
fn message_to_parts(
    msg: &crate::provider::ChatMessage,
    call_name_by_id: &mut HashMap<String, String>,
) -> Option<(String, Vec<serde_json::Value>)> {
    match msg.role {
        Role::System => None,
        Role::User => {
            let mut parts = Vec::new();
            match &msg.content {
                MessageContent::Text(s) => {
                    if !s.is_empty() {
                        parts.push(serde_json::json!({ "text": s }));
                    }
                }
                MessageContent::Multimodal(items) => {
                    for p in items {
                        match p {
                            ContentPart::Text { text } => {
                                parts.push(serde_json::json!({ "text": text }));
                            }
                            ContentPart::ImageUrl { image_url } => {
                                if let Some(block) = data_url_to_inline_data(&image_url.url) {
                                    parts.push(block);
                                } else {
                                    // http(s) URL 直传不被 inlineData 支持，降级为文本说明
                                    parts.push(serde_json::json!({
                                        "text": format!("[image: {}]", image_url.url)
                                    }));
                                }
                            }
                        }
                    }
                }
            }
            Some(("user".into(), parts))
        }
        Role::Assistant => {
            let mut parts = Vec::new();
            let text = msg.content.as_text();
            if !text.is_empty() {
                parts.push(serde_json::json!({ "text": text }));
            }
            if let Some(tcs) = &msg.tool_calls {
                for tc in tcs {
                    // 缓存 id→name，供随后的 tool 消息补全 functionResponse
                    call_name_by_id.insert(tc.id.clone(), tc.name.clone());
                    parts.push(serde_json::json!({
                        "functionCall": {
                            "name": tc.name,
                            "args": tc.arguments,
                        }
                    }));
                }
            }
            Some(("model".into(), parts))
        }
        Role::Tool => {
            let name = msg
                .tool_call_id
                .as_ref()
                .and_then(|id| call_name_by_id.get(id).cloned())
                .unwrap_or_else(|| msg.tool_call_id.clone().unwrap_or_default());
            let part = serde_json::json!({
                "functionResponse": {
                    "name": name,
                    "response": {
                        "result": msg.content.as_text(),
                    }
                }
            });
            Some(("user".into(), vec![part]))
        }
    }
}

/// 转换完整消息列表：system 提升 + 相邻同 role 合并。
/// 返回 (system, contents)。若合并后首条不是 user，前置占位 user 消息（API 要求 user 开头）。
fn convert_messages(
    messages: &[crate::provider::ChatMessage],
) -> (Option<SystemInstruction>, Vec<Content>) {
    let mut system_parts: Vec<String> = Vec::new();
    let mut out: Vec<Content> = Vec::new();
    let mut call_name_by_id: HashMap<String, String> = HashMap::new();
    for msg in messages {
        let Some((role, parts)) = message_to_parts(msg, &mut call_name_by_id) else {
            let text = msg.content.as_text();
            if !text.trim().is_empty() {
                system_parts.push(text);
            }
            continue;
        };
        if parts.is_empty() {
            continue;
        }
        // 相邻同 role 合并
        if let Some(last) = out.last_mut() {
            if last.role == role {
                last.parts.extend(parts);
                continue;
            }
        }
        out.push(Content { role, parts });
    }
    // API 要求首条为 user
    if out.first().is_some_and(|m| m.role != "user") {
        out.insert(
            0,
            Content {
                role: "user".into(),
                parts: vec![serde_json::json!({ "text": "(continue)" })],
            },
        );
    }
    let system = if system_parts.is_empty() {
        None
    } else {
        Some(SystemInstruction {
            parts: system_parts
                .into_iter()
                .map(|s| serde_json::json!({ "text": s }))
                .collect(),
        })
    };
    (system, out)
}

#[async_trait]
impl Provider for GeminiProvider {
    async fn chat(&self, req: &ChatRequest<'_>) -> Result<ChatResponse> {
        // 非流式路径：直接调 :generateContent，避免 SSE 开销
        let (system, contents) = convert_messages(req.messages);
        if contents.is_empty() {
            return Err(anyhow!("no messages to send"));
        }
        let tools: Option<Vec<GeminiTool>> = req.tools.map(|ts| {
            vec![GeminiTool {
                function_declarations: ts
                    .iter()
                    .map(|t| FunctionDeclaration {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.parameters.clone(),
                    })
                    .collect(),
            }]
        });
        let body = GenerateRequest {
            system_instruction: system,
            contents,
            tools,
            generation_config: Some(GenerationConfig {
                max_output_tokens: self.max_output_tokens,
            }),
        };

        let resp = self
            .client
            .post(self.generate_url())
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("request failed: {}", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("provider returned {}: {}", status, text));
        }
        let v: serde_json::Value = resp.json().await.map_err(|e| anyhow!("parse: {}", e))?;
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        if let Some(cands) = v.get("candidates").and_then(|c| c.as_array()) {
            for cand in cands {
                if let Some(parts) = cand
                    .get("content")
                    .and_then(|c| c.get("parts"))
                    .and_then(|p| p.as_array())
                {
                    for part in parts {
                        if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                            text.push_str(t);
                        } else if let Some(fc) = part.get("functionCall") {
                            tool_calls.push(ToolCall {
                                id: fc
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| "call".into()),
                                name: fc
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                arguments: fc
                                    .get("args")
                                    .cloned()
                                    .unwrap_or(serde_json::Value::Null),
                            });
                        }
                    }
                }
            }
        }
        Ok(ChatResponse {
            text: if text.is_empty() { None } else { Some(text) },
            tool_calls,
        })
    }

    async fn chat_stream(&self, req: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>> {
        let (system, contents) = convert_messages(req.messages);
        if contents.is_empty() {
            return Box::pin(try_stream! {
                yield StreamEvent::Error("no messages to send".to_string());
            });
        }
        let tools: Option<Vec<GeminiTool>> = req.tools.map(|ts| {
            vec![GeminiTool {
                function_declarations: ts
                    .iter()
                    .map(|t| FunctionDeclaration {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.parameters.clone(),
                    })
                    .collect(),
            }]
        });
        let body = GenerateRequest {
            system_instruction: system,
            contents,
            tools,
            generation_config: Some(GenerationConfig {
                max_output_tokens: self.max_output_tokens,
            }),
        };

        let resp = match self
            .client
            .post(self.stream_url())
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
        {
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

        let mut resp = resp;
        tracing::info!("provider stream started");

        let s = try_stream! {
            let mut buf = String::new();
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
                    // Gemini SSE：每个 data 是一份 GenerateContentResponse
                    if let Some(err) = v.get("error") {
                        let msg = err
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("unknown gemini error");
                        yield StreamEvent::Error(msg.to_string());
                        return;
                    }
                    if let Some(cands) = v.get("candidates").and_then(|c| c.as_array()) {
                        for cand in cands {
                            if let Some(parts) = cand
                                .get("content")
                                .and_then(|c| c.get("parts"))
                                .and_then(|p| p.as_array())
                            {
                                for part in parts {
                                    if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                        if !t.is_empty() {
                                            yield StreamEvent::TextDelta(t.to_string());
                                        }
                                    } else if let Some(fc) = part.get("functionCall") {
                                        let name = fc
                                            .get("name")
                                            .and_then(|n| n.as_str())
                                            .unwrap_or_default()
                                            .to_string();
                                        let id = if name.is_empty() {
                                            "call".to_string()
                                        } else {
                                            name.clone()
                                        };
                                        let arguments = fc
                                            .get("args")
                                            .cloned()
                                            .unwrap_or(serde_json::Value::Null);
                                        yield StreamEvent::ToolCall(ToolCall {
                                            id,
                                            name,
                                            arguments,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            tracing::info!("provider stream done (stream ended)");
            yield StreamEvent::Done;
        };
        Box::pin(s)
    }

    /// Gemini 只支持 native tool calling
    fn native_tool_calling(&self) -> bool {
        true
    }

    /// Gemini 无本地探测端点，返回 None 走配置值/默认
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
        let (system, contents) = convert_messages(&msgs);
        assert!(system.is_some());
        let parts = system.unwrap().parts;
        assert_eq!(parts.len(), 2);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, "user");
    }

    #[test]
    fn test_adjacent_same_role_merge() {
        let msgs = vec![
            ChatMessage::user("问题1"),
            ChatMessage::user("问题2"),
            ChatMessage::assistant("答"),
        ];
        let (_, contents) = convert_messages(&msgs);
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0].parts.len(), 2); // 两条 user 合并
        assert_eq!(contents[1].role, "model");
    }

    #[test]
    fn test_tool_call_and_result() {
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
        let (_, contents) = convert_messages(&msgs);
        // user / model(functionCall) / user(functionResponse)
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[1].role, "model");
        assert_eq!(contents[1].parts[0]["functionCall"]["name"], "file_read");
        assert_eq!(contents[2].role, "user");
        assert_eq!(
            contents[2].parts[0]["functionResponse"]["name"],
            "file_read"
        );
        assert_eq!(
            contents[2].parts[0]["functionResponse"]["response"]["result"],
            "文件内容"
        );
    }

    #[test]
    fn test_leading_model_gets_placeholder() {
        let msgs = vec![ChatMessage::assistant("先说话")];
        let (_, contents) = convert_messages(&msgs);
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0].role, "user");
        assert_eq!(contents[1].role, "model");
    }
}
