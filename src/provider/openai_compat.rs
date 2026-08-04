use crate::provider::{
    ChatRequest, ChatResponse, MessageContent, Provider, Role, StreamEvent, ToolCall,
};
use anyhow::{anyhow, Result};
use async_stream::try_stream;
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use reqwest::Client;
use serde::Serialize;
use std::time::Duration;

/// 流式响应单 chunk 读取超时（秒）。
/// LLM 生成可能有间隔，但超过此时间无任何数据视为连接挂起。
const STREAM_CHUNK_TIMEOUT_SECS: u64 = 120;

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
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            client,
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
    content: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiToolCallSer<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
}

/// 把 MessageContent 转为 OpenAI 兼容的 JSON value：
/// Text → 字符串；Multimodal → content 数组
fn content_to_json(content: &MessageContent) -> serde_json::Value {
    match content {
        MessageContent::Text(s) => serde_json::Value::String(s.clone()),
        MessageContent::Multimodal(parts) => serde_json::to_value(parts).unwrap_or(serde_json::Value::Null),
    }
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
                    content: content_to_json(&m.content),
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
            let mut tc_accum: std::collections::HashMap<u32, ToolCallAccum> = std::collections::HashMap::new();
            let mut tc_order: Vec<u32> = Vec::new();

            loop {
                // per-chunk 超时：防止 provider 挂起导致 agent 锁永久持有
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
                                tracing::info!("provider stream done ([DONE])");
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
            tracing::info!("provider stream done (stream ended)");
            yield StreamEvent::Done;
        };
        Box::pin(s)
    }

    fn native_tool_calling(&self) -> bool {
        self.native_tool_calling
    }

    /// 探测模型上下文窗口大小。
    /// 先尝试 llama.cpp 特征端点 /props，再尝试 Ollama 的 /api/tags + /api/show。
    /// 探测失败返回 None。
    async fn detect_context_size(&self) -> Option<usize> {
        // llama.cpp: GET /props → default_generation_settings.n_ctx
        if let Some(n) = self.try_llamacpp_props().await {
            tracing::info!(n_ctx = n, "detected context_size from llama.cpp /props");
            return Some(n);
        }
        // Ollama: POST /api/show → model_info["<arch>.context_length"]
        if let Some(n) = self.try_ollama_show().await {
            tracing::info!(n_ctx = n, "detected context_size from ollama /api/show");
            return Some(n);
        }
        None
    }
}

impl OpenAiCompatibleProvider {
    /// 尝试 llama.cpp 的 /props 端点
    async fn try_llamacpp_props(&self) -> Option<usize> {
        let url = format!("{}/props", self.base_url);
        let resp = self.client.get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: serde_json::Value = resp.json().await.ok()?;
        v.get("default_generation_settings")?
            .get("n_ctx")?
            .as_u64()
            .map(|n| n as usize)
    }

    /// 尝试 Ollama 的 /api/show 端点
    async fn try_ollama_show(&self) -> Option<usize> {
        // 先用 /api/tags 确认是 Ollama 后端
        let tags_url = format!("{}/api/tags", self.base_url);
        let tags_resp = self.client.get(&tags_url).send().await.ok()?;
        if !tags_resp.status().is_success() {
            return None;
        }
        // POST /api/show {"model": "<model>"}
        let show_url = format!("{}/api/show", self.base_url);
        let resp = self
            .client
            .post(&show_url)
            .json(&serde_json::json!({ "model": self.model }))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: serde_json::Value = resp.json().await.ok()?;
        // model_info 里找 *.context_length
        if let Some(info) = v.get("model_info").and_then(|m| m.as_object()) {
            for (k, val) in info {
                if k.ends_with(".context_length") {
                    if let Some(n) = val.as_u64() {
                        return Some(n as usize);
                    }
                }
            }
        }
        // 回退：parameters 字段解析 "num_ctx <n>"
        if let Some(params) = v.get("parameters").and_then(|p| p.as_str()) {
            for line in params.lines() {
                if let Some(rest) = line.trim().strip_prefix("num_ctx ") {
                    if let Ok(n) = rest.trim().parse::<usize>() {
                        return Some(n);
                    }
                }
            }
        }
        None
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
