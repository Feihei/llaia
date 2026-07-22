use crate::provider::{ChatRequest, ChatResponse, Provider, Role, StreamEvent, ToolCall};
use anyhow::{anyhow, Result};
use async_stream::try_stream;
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use reqwest::Client;
use serde::Serialize;

pub struct OpenAiCompatibleProvider {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    native_tool_calling: bool,
}

impl OpenAiCompatibleProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        native_tool_calling: bool,
    ) -> Result<Self> {
        Ok(Self {
            client: Client::builder().build()?,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
            native_tool_calling,
        })
    }
}

#[derive(Serialize)]
struct OpenAiMessage<'a> {
    role: &'a str,
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiToolCallSer<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
}

#[derive(Serialize)]
struct OpenAiToolCallSer<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    tool_type: &'a str,
    function: OpenAiFunctionSer<'a>,
}

#[derive(Serialize)]
struct OpenAiFunctionSer<'a> {
    name: &'a str,
    arguments: String,
}

#[derive(Serialize)]
struct OpenAiTool {
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: OpenAiFunctionSpec,
}

#[derive(Serialize)]
struct OpenAiFunctionSpec {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
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

    async fn chat_stream(
        &self,
        req: &ChatRequest<'_>,
    ) -> BoxStream<'_, Result<StreamEvent>> {
        let url = format!("{}/chat/completions", self.base_url);

        let messages: Vec<OpenAiMessage> = req
            .messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                };
                OpenAiMessage {
                    role,
                    content: &m.content,
                    tool_calls: m.tool_calls.as_ref().map(|tcs| {
                        tcs.iter()
                            .map(|tc| OpenAiToolCallSer {
                                id: &tc.id,
                                tool_type: "function",
                                function: OpenAiFunctionSer {
                                    name: &tc.name,
                                    arguments: tc.arguments.to_string(),
                                },
                            })
                            .collect()
                    }),
                    tool_call_id: m.tool_call_id.as_deref(),
                }
            })
            .collect();

        let tools: Option<Vec<OpenAiTool>> = if self.native_tool_calling {
            req.tools.map(|ts| {
                ts.iter()
                    .map(|t| OpenAiTool {
                        tool_type: "function",
                        function: OpenAiFunctionSpec {
                            name: t.name.clone(),
                            description: t.description.clone(),
                            parameters: t.parameters.clone(),
                        },
                    })
                    .collect()
            })
        } else {
            None
        };

        let tool_choice = if tools.is_some() {
            Some("auto".to_string())
        } else {
            None
        };

        let body = ChatCompletionsStreamRequest {
            model: &self.model,
            messages,
            tools,
            tool_choice,
            stream: true,
        };

        let mut request = self.client.post(&url).json(&body);
        if !self.api_key.is_empty() {
            request = request.bearer_auth(&self.api_key);
        }

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

        let s = try_stream! {
            let mut buf = String::new();
            let mut tc_accum: std::collections::HashMap<u32, ToolCallAccum> = std::collections::HashMap::new();
            let mut tc_order: Vec<u32> = Vec::new();

            loop {
                let chunk = match resp.chunk().await {
                    Ok(Some(c)) => c,
                    Ok(None) => break,
                    Err(e) => {
                        yield StreamEvent::Error(format!("stream chunk error: {}", e));
                        return;
                    }
                };
                buf.push_str(std::str::from_utf8(&chunk).unwrap_or(""));
                while let Some(pos) = buf.find("\n\n") {
                    let event_str = buf[..pos].to_string();
                    buf = buf[pos + 2..].to_string();
                    for line in event_str.lines() {
                        let line = line.trim();
                        if line.is_empty() || line.starts_with(':') {
                            continue;
                        }
                        if let Some(data) = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:")) {
                            let data = data.trim();
                            if data == "[DONE]" {
                                let mut indices: Vec<u32> = tc_order.clone();
                                indices.sort();
                                for idx in indices {
                                    if let Some(acc) = tc_accum.remove(&idx) {
                                        let args: serde_json::Value = serde_json::from_str(&acc.arguments)
                                            .unwrap_or(serde_json::Value::Null);
                                        yield StreamEvent::ToolCall(ToolCall {
                                            id: acc.id,
                                            name: acc.name,
                                            arguments: args,
                                        });
                                    }
                                }
                                yield StreamEvent::Done;
                                return;
                            }
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                                if let Some(delta) = v.get("choices").and_then(|c| c.get(0)).and_then(|c| c.get("delta")) {
                                    if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                                        if !content.is_empty() {
                                            yield StreamEvent::TextDelta(content.to_string());
                                        }
                                    }
                                    if let Some(tcs) = delta.get("tool_calls") {
                                        if let Some(tcs_arr) = tcs.as_array() {
                                            for tc in tcs_arr {
                                                let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
                                                if !tc_order.contains(&idx) {
                                                    tc_order.push(idx);
                                                }
                                                let acc = tc_accum.entry(idx).or_insert_with(|| ToolCallAccum::default());
                                                if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                                                    acc.id = id.to_string();
                                                }
                                                if let Some(func) = tc.get("function") {
                                                    if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                                                        acc.name = name.to_string();
                                                    }
                                                    if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
                                                        acc.arguments.push_str(args);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let mut indices: Vec<u32> = tc_order.clone();
            indices.sort();
            for idx in indices {
                if let Some(acc) = tc_accum.remove(&idx) {
                    let args: serde_json::Value = serde_json::from_str(&acc.arguments)
                        .unwrap_or(serde_json::Value::Null);
                    yield StreamEvent::ToolCall(ToolCall {
                        id: acc.id,
                        name: acc.name,
                        arguments: args,
                    });
                }
            }
            yield StreamEvent::Done;
        };
        Box::pin(s)
    }

    fn native_tool_calling(&self) -> bool {
        self.native_tool_calling
    }
}

#[derive(Default)]
struct ToolCallAccum {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct ChatCompletionsStreamRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAiMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    stream: bool,
}
