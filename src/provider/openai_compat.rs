use crate::provider::{
    compat::{Compat, MaxTokensField},
    ChatRequest, ChatResponse, MessageContent, Provider, Role, StreamEvent, ToolCall, Usage,
};
use anyhow::{anyhow, Result};
use async_stream::try_stream;
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use reqwest::Client;
use serde::Serialize;
use std::time::{Duration, Instant};

/// 流式响应单 chunk 读取超时（秒）。
/// LLM 生成可能有间隔，但超过此时间无任何数据视为连接挂起。
const STREAM_CHUNK_TIMEOUT_SECS: u64 = 120;

pub struct OpenAiCompatibleProvider {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    native_tool_calling: bool,
    /// 兼容开关集合：按 base_url 探测 + 配置覆盖，见 `docs/adr/0026-provider-compat.md`。
    compat: Compat,
    /// 单次生成最大 token 数；None 时不发送（bare 行为）。
    max_tokens: Option<usize>,
}

impl OpenAiCompatibleProvider {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        native_tool_calling: bool,
        max_tokens: Option<usize>,
        compat: Compat,
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
            max_tokens,
            compat,
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
        MessageContent::Multimodal(parts) => {
            serde_json::to_value(parts).unwrap_or(serde_json::Value::Null)
        }
    }
}

/// 解析流式 usage（token 统计）；字段缺失时补 0。仅在 compat.streaming_usage 时调用。
fn parse_usage(v: Option<&serde_json::Value>) -> Option<Usage> {
    let v = v?;
    Some(Usage {
        prompt_tokens: v.get("prompt_tokens").and_then(|n| n.as_u64()).unwrap_or(0) as u32,
        completion_tokens: v
            .get("completion_tokens")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as u32,
        total_tokens: v.get("total_tokens").and_then(|n| n.as_u64()).unwrap_or(0) as u32,
    })
}

/// 构造发送给 OpenAI 兼容端点的 messages，并按 compat 应用：
/// - `requires_assistant_after_tool`：多轮 tool 结果后补一条空 assistant 占位。
fn build_openai_messages<'a>(req: &'a ChatRequest<'a>, compat: &Compat) -> Vec<OpenAiMessage<'a>> {
    let mut messages: Vec<OpenAiMessage<'_>> = req
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
    if compat.requires_assistant_after_tool {
        if let Some(last) = messages.last() {
            if last.role == "tool" {
                messages.push(OpenAiMessage {
                    role: "assistant",
                    content: serde_json::Value::String(String::new()),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
        }
    }
    messages
}

/// 按 compat 选择 max_tokens 字段名；返回 `(max_tokens, max_completion_tokens)`，仅其一为 Some。
/// `None` 字段名 → 两者皆 None（= bare 行为，不发送 max_tokens）。
fn select_max_tokens_fields(compat: &Compat, value: Option<usize>) -> (Option<u32>, Option<u32>) {
    match compat.max_tokens_field {
        MaxTokensField::MaxTokens => (value.map(|v| v as u32), None),
        MaxTokensField::MaxCompletionTokens => (None, value.map(|v| v as u32)),
        MaxTokensField::None => (None, None),
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
        let mut usage = None;
        let mut finish_reason = None;
        while let Some(ev) = stream.next().await {
            match ev? {
                StreamEvent::TextDelta(d) => text.push_str(&d),
                StreamEvent::ToolCall(tc) => tool_calls.push(tc),
                StreamEvent::Usage(u) => usage = Some(u),
                StreamEvent::FinishReason(fr) => finish_reason = Some(fr),
                StreamEvent::Done => break,
                StreamEvent::Error(msg) => return Err(anyhow!("stream error: {}", msg)),
            }
        }
        Ok(ChatResponse {
            text: if text.is_empty() { None } else { Some(text) },
            tool_calls,
            usage,
            finish_reason,
        })
    }

    async fn chat_stream(&self, req: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>> {
        let url = format!("{}/chat/completions", self.base_url);

        // 构造 messages（含 requires_assistant_after_tool 占位）
        let messages = build_openai_messages(req, &self.compat);

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

        // max_tokens 字段名随 compat 切换；仅当实际持有值且字段名非 None 时才发送（bare 行为不发送）。
        let (max_tokens, max_completion_tokens) =
            select_max_tokens_fields(&self.compat, self.max_tokens);
        // streaming_usage：请求 include_usage，流式尾包才会带 usage。
        let stream_options = if self.compat.streaming_usage {
            Some(StreamOptions {
                include_usage: true,
            })
        } else {
            None
        };

        // disable_thinking：内部/自动化 turn 关掉推理模型的深度思考，避免无谓的长推理撑爆超时。
        // 仅当 compat 支持（llama.cpp/Ollama 预设）且本请求显式要求时注入。
        let chat_template_kwargs = if self.compat.disable_thinking_template && req.disable_thinking
        {
            Some(ChatTemplateKwargs {
                enable_thinking: false,
            })
        } else {
            None
        };

        let body = ChatCompletionsStreamRequest {
            model: &self.model,
            messages,
            tools,
            tool_choice,
            stream: true,
            max_tokens,
            max_completion_tokens,
            stream_options,
            chat_template_kwargs,
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
            // 原始 finish_reason（尾 chunk 携带），配合 compat 推断有效 finish_reason
            let mut raw_finish: Option<String> = None;
            // 空闲计时锚点：仅「真实 data 事件」会刷新；keepalive 注释 `: ping` 不会。
            let mut last_data = Instant::now();

            loop {
                // 空闲超时（idle timeout）：按「距上次真实 data」计算剩余窗口，而非每次
                // `resp.chunk()` 都重置。否则 provider 持续发 SSE keepalive（`: ping`）会
                // 不断重置 per-chunk 计时器，使挂起的流永远卡不到超时（agnes 流式挂起时
                // 实测每 ~120s 发一次 keepalive，把 120s 超时拖到 300s 顶层兜底才断）。
                let remaining = STREAM_CHUNK_TIMEOUT_SECS.saturating_sub(last_data.elapsed().as_secs());
                if remaining == 0 {
                    yield StreamEvent::Error(format!(
                        "stream idle timeout (no real data in {}s)",
                        STREAM_CHUNK_TIMEOUT_SECS
                    ));
                    return;
                }
                let chunk = match tokio::time::timeout(Duration::from_secs(remaining), resp.chunk()).await {
                    Ok(Ok(Some(c))) => c,
                    Ok(Ok(None)) => break,
                    Ok(Err(e)) => {
                        yield StreamEvent::Error(format!("stream chunk error: {}", e));
                        return;
                    }
                    Err(_) => {
                        yield StreamEvent::Error(format!(
                            "stream idle timeout (no real data in {}s)",
                            STREAM_CHUNK_TIMEOUT_SECS
                        ));
                        return;
                    }
                };
                // 该 chunk 是否携带真实 SSE data 事件（keepalive 注释 `: ping` 不含 `data:`）
                let chunk_str = std::str::from_utf8(&chunk).unwrap_or("");
                if chunk_str.lines().any(|l| l.trim_start().starts_with("data:")) {
                    last_data = Instant::now();
                }
                buf.push_str(chunk_str);
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
                                let has_tc = !tc_accum.is_empty();
                                if let Some(fr) = self.compat.effective_finish_reason(raw_finish.as_deref(), has_tc) {
                                    yield StreamEvent::FinishReason(fr);
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
                                tracing::info!("provider stream done ([DONE])");
                                yield StreamEvent::Done;
                                return;
                            }
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                                // streaming usage（仅 compat.streaming_usage 时请求+解析；尾包 choices 可能为空）
                                if self.compat.streaming_usage {
                                    if let Some(u) = parse_usage(v.get("usage")) {
                                        yield StreamEvent::Usage(u);
                                    }
                                }
                                let choice = v.get("choices").and_then(|c| c.get(0));
                                if let Some(choice) = choice {
                                    // finish_reason 由尾 chunk 携带，先记录原始值
                                    if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                                        raw_finish = Some(fr.to_string());
                                    }
                                    if let Some(delta) = choice.get("delta") {
                                        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                                            if !content.is_empty() {
                                                yield StreamEvent::TextDelta(content.to_string());
                                            }
                                        }
                                        // reasoning 折回 content（Ollama / Llama.cpp 深度思考模型）
                                        if self.compat.reasoning_to_content {
                                            if let Some(r) = delta.get("reasoning_content").and_then(|c| c.as_str()) {
                                                if !r.is_empty() {
                                                    yield StreamEvent::TextDelta(r.to_string());
                                                }
                                            }
                                            if let Some(t) = delta.get("thinking").and_then(|c| c.as_str()) {
                                                if !t.is_empty() {
                                                    yield StreamEvent::TextDelta(t.to_string());
                                                }
                                            }
                                        }
                                        if let Some(tcs) = delta.get("tool_calls") {
                                            if let Some(tcs_arr) = tcs.as_array() {
                                                for tc in tcs_arr {
                                                    let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
                                                    if !tc_order.contains(&idx) {
                                                        tc_order.push(idx);
                                                    }
                                                    let acc = tc_accum.entry(idx).or_default();
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
            }
            let has_tc = !tc_accum.is_empty();
            if let Some(fr) = self.compat.effective_finish_reason(raw_finish.as_deref(), has_tc) {
                yield StreamEvent::FinishReason(fr);
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

    fn label(&self) -> String {
        self.model.clone()
    }
}

impl OpenAiCompatibleProvider {
    /// 探测端点根地址。OpenAI 兼容 `base_url` 通常以 `/v1` 结尾，而
    /// llama.cpp `/props`、Ollama `/api/*` 等管理端点挂在服务根路径，
    /// 需剥掉 `/v1` 后缀再拼（否则请求 `/v1/props` → 404，探测永远失败）。
    fn probe_base(&self) -> &str {
        let trimmed = self.base_url.trim_end_matches('/');
        trimmed.strip_suffix("/v1").unwrap_or(trimmed)
    }

    /// 尝试 llama.cpp 的 /props 端点
    async fn try_llamacpp_props(&self) -> Option<usize> {
        let url = format!("{}/props", self.probe_base());
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
        let base = self.probe_base();
        // 先用 /api/tags 确认是 Ollama 后端
        let tags_url = format!("{}/api/tags", base);
        let tags_resp = self.client.get(&tags_url).send().await.ok()?;
        if !tags_resp.status().is_success() {
            return None;
        }
        // POST /api/show {"model": "<model>"}
        let show_url = format!("{}/api/show", base);
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
    /// `max_tokens`：仅 compat.max_tokens_field = MaxTokens 且持有值时发送
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    /// `max_completion_tokens`：仅 compat.max_tokens_field = MaxCompletionTokens 且持有值时发送
    #[serde(
        rename = "max_completion_tokens",
        skip_serializing_if = "Option::is_none"
    )]
    max_completion_tokens: Option<u32>,
    /// 流式 usage 开关：仅 compat.streaming_usage 时发送（请求尾包带 usage）
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    /// 关闭模型「深度思考」：仅 compat.disable_thinking_template 且请求 disable_thinking 时发送。
    /// llama.cpp / Ollama 等支持，其它 OpenAI 兼容端点忽略即可（无害）。
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<ChatTemplateKwargs>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct ChatTemplateKwargs {
    #[serde(rename = "enable_thinking")]
    enable_thinking: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::compat::Compat;
    use crate::provider::ChatMessage;
    use serde_json::json;

    fn sse(obj: serde_json::Value) -> String {
        format!("data: {}\n\n", obj)
    }

    fn done() -> String {
        "data: [DONE]\n\n".to_string()
    }

    #[tokio::test]
    async fn default_bare_no_normalization() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_body(format!(
                "{}{}",
                sse(json!({"choices":[{"delta":{"content":"hello"}}]})),
                done()
            ))
            .create();
        let p = OpenAiCompatibleProvider::new(server.url(), "", "m", true, None, Compat::default())
            .unwrap();
        let msgs = vec![ChatMessage::user("hi")];
        let req = ChatRequest {
            messages: &msgs,
            tools: None,
            disable_thinking: false,
        };
        let resp = p.chat(&req).await.unwrap();
        assert_eq!(resp.text.as_deref(), Some("hello"));
        assert!(resp.tool_calls.is_empty());
        assert!(resp.usage.is_none());
        assert!(resp.finish_reason.is_none());
        m.assert();
    }

    #[tokio::test]
    async fn ollama_folds_reasoning_and_parses_usage() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_body(format!(
                "{}{}{}{}{}",
                sse(json!({"choices":[{"delta":{"reasoning_content":"I think...","content":""}}]})),
                sse(json!({"choices":[{"delta":{"content":"The answer is 42"}}]})),
                sse(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"file_read","arguments":"{\"path\":\"/tmp\"}"}}]}}]})),
                sse(json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}})),
                done()
            ))
            .create();
        let p = OpenAiCompatibleProvider::new(server.url(), "", "m", true, None, Compat::ollama())
            .unwrap();
        let msgs = vec![ChatMessage::user("hi")];
        let req = ChatRequest {
            messages: &msgs,
            tools: None,
            disable_thinking: false,
        };
        let resp = p.chat(&req).await.unwrap();
        // reasoning_content 折回 content
        assert!(resp.text.as_deref().unwrap().contains("I think..."));
        assert!(resp.text.as_deref().unwrap().contains("The answer is 42"));
        // tool_calls 解析
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "file_read");
        // usage 解析
        let u = resp.usage.expect("usage should be present");
        assert_eq!(u.total_tokens, 15);
        // finish_reason 原值保留
        assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
        m.assert();
    }

    #[tokio::test]
    async fn llamacpp_infers_finish_reason_without_raw() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_body(format!(
                "{}{}{}{}",
                sse(json!({"choices":[{"delta":{"thinking":"hmm","content":""}}]})),
                sse(json!({"choices":[{"delta":{"content":"done"}}]})),
                sse(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"web_fetch","arguments":"{}"}}]}}]})),
                done()
            ))
            .create();
        let p =
            OpenAiCompatibleProvider::new(server.url(), "", "m", true, None, Compat::llamacpp())
                .unwrap();
        let msgs = vec![ChatMessage::user("hi")];
        let req = ChatRequest {
            messages: &msgs,
            tools: None,
            disable_thinking: false,
        };
        let resp = p.chat(&req).await.unwrap();
        // thinking 折回 content
        assert!(resp.text.as_deref().unwrap().contains("hmm"));
        assert!(resp.text.as_deref().unwrap().contains("done"));
        // 无 finish_reason 但有 tool_calls → 推断 tool_calls
        assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
        // 流式未带 usage → None
        assert!(resp.usage.is_none());
        m.assert();
    }

    #[test]
    fn max_tokens_field_selection() {
        let v = Some(100usize);
        assert_eq!(
            select_max_tokens_fields(&Compat::default(), v),
            (None, None)
        );
        let c = Compat {
            max_tokens_field: MaxTokensField::MaxTokens,
            ..Default::default()
        };
        assert_eq!(select_max_tokens_fields(&c, v), (Some(100), None));
        let c2 = Compat {
            max_tokens_field: MaxTokensField::MaxCompletionTokens,
            ..Default::default()
        };
        assert_eq!(select_max_tokens_fields(&c2, v), (None, Some(100)));
        // 无值则不发送
        assert_eq!(select_max_tokens_fields(&c, None), (None, None));
    }

    #[test]
    fn assistant_placeholder_after_tool() {
        let msgs = vec![
            ChatMessage::user("hi"),
            ChatMessage::assistant_with_tools(
                "",
                vec![ToolCall {
                    id: "1".into(),
                    name: "file_read".into(),
                    arguments: json!({}),
                }],
            ),
            ChatMessage::tool("ok", "1"),
        ];
        let req = ChatRequest {
            messages: &msgs,
            tools: None,
            disable_thinking: false,
        };
        // ollama 预设：requires_assistant_after_tool = true
        let with_ph = build_openai_messages(&req, &Compat::ollama());
        assert_eq!(with_ph.last().unwrap().role, "assistant");
        // 默认：不补占位
        let no_ph = build_openai_messages(&req, &Compat::default());
        assert_eq!(no_ph.last().unwrap().role, "tool");
        // 不以 tool 结尾时不补
        let msgs2 = vec![ChatMessage::user("hi")];
        let req2 = ChatRequest {
            messages: &msgs2,
            tools: None,
            disable_thinking: false,
        };
        assert_eq!(build_openai_messages(&req2, &Compat::ollama()).len(), 1);
    }

    #[test]
    fn probe_base_strips_v1_suffix() {
        let mk = |base: &str| {
            OpenAiCompatibleProvider::new(base, "", "m", true, None, Compat::default()).unwrap()
        };
        assert_eq!(mk("http://h:8080/v1").probe_base(), "http://h:8080");
        // 带尾斜杠同样剥掉
        assert_eq!(mk("http://h:8080/v1/").probe_base(), "http://h:8080");
        // 不以 /v1 结尾保持原样
        assert_eq!(mk("http://h:8080").probe_base(), "http://h:8080");
        // 路径里只有别处的 v1 不受影响
        assert_eq!(
            mk("http://h:8080/api/v1/openai").probe_base(),
            "http://h:8080/api/v1/openai"
        );
    }

    #[tokio::test]
    async fn detect_context_size_hits_props_at_root_despite_v1_base_url() {
        // 回归：llama.cpp base_url 带 /v1 时，/props 必须打到服务根路径，
        // 旧实现拼成 /v1/props → 404 → 探测失败落到默认 8192。
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("GET", "/props")
            .with_status(200)
            .with_body(json!({"default_generation_settings": {"n_ctx": 131072}}).to_string())
            .create();
        let p = OpenAiCompatibleProvider::new(
            format!("{}/v1", server.url()),
            "",
            "qwen3",
            true,
            None,
            Compat::llamacpp(),
        )
        .unwrap();
        assert_eq!(p.detect_context_size().await, Some(131072));
        m.assert();
    }

    #[tokio::test]
    async fn detect_context_size_ollama_show_with_v1_base_url() {
        // Ollama 同理：/api/show 挂根路径，base_url 带 /v1 也能探测
        let mut server = mockito::Server::new_async().await;
        let tags = server
            .mock("GET", "/api/tags")
            .with_status(200)
            .with_body(json!({"models": []}).to_string())
            .create();
        let show = server
            .mock("POST", "/api/show")
            .with_status(200)
            .with_body(json!({"model_info": {"qwen3.context_length": 131072u64}}).to_string())
            .create();
        let p = OpenAiCompatibleProvider::new(
            format!("{}/v1", server.url()),
            "",
            "qwen3",
            true,
            None,
            Compat::ollama(),
        )
        .unwrap();
        assert_eq!(p.detect_context_size().await, Some(131072));
        tags.assert();
        show.assert();
    }
}
