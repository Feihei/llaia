use anyhow::Result;
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub mod anthropic;
pub mod compat;
pub mod fallback;
pub mod gemini;
pub mod openai_compat;
pub mod probe;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 消息内容：纯文本（向后兼容）或多模态（文本+图片等）。
/// 序列化时 Text 变体为字符串，Multimodal 变体为 OpenAI content 数组。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Multimodal(Vec<ContentPart>),
}

impl MessageContent {
    /// 获取纯文本部分（用于 token 估算、压缩 dump、日志等场景）。
    /// 多模态变体只拼接 Text part，图片部分忽略。
    pub fn as_text(&self) -> String {
        match self {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Multimodal(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.clone()),
                    ContentPart::ImageUrl { .. } => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        }
    }

    /// 是否包含图片（用于压缩降级判断）
    pub fn has_image(&self) -> bool {
        match self {
            MessageContent::Text(_) => false,
            MessageContent::Multimodal(parts) => parts
                .iter()
                .any(|p| matches!(p, ContentPart::ImageUrl { .. })),
        }
    }
}

/// 多模态 content 数组的一个 part。序列化为 OpenAI 格式：
/// `{"type":"text","text":"..."}` / `{"type":"image_url","image_url":{"url":"..."}}`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrlContent },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageUrlContent {
    /// data:image/jpeg;base64,... 或 http(s) URL
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: MessageContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: MessageContent::Text(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: MessageContent::Text(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::Text(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }
    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::Text(content.into()),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }
    pub fn tool(content: impl Into<String>, tool_call_id: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: MessageContent::Text(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
    /// 多模态用户消息：parts 至少含一个文本 part 和/或图片 part
    pub fn user_multimodal(parts: Vec<ContentPart>) -> Self {
        Self {
            role: Role::User,
            content: MessageContent::Multimodal(parts),
            tool_calls: None,
            tool_call_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ChatRequest<'a> {
    pub messages: &'a [ChatMessage],
    pub tools: Option<&'a [ToolSpec]>,
}

#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    /// 流式 usage（token 统计）；仅部分 provider 在 compat 开启时填充。
    pub usage: Option<Usage>,
    /// 有效 finish_reason（含 compat 推断）；默认 None。
    pub finish_reason: Option<String>,
}

/// 单次生成的 token 用量统计。
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// 流式事件
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// 文本增量
    TextDelta(String),
    /// 工具调用（native 模式下完整 ToolCall；标签模式不产生此事件，由 Agent 状态机解析）
    ToolCall(ToolCall),
    /// 本轮流式结束
    Done,
    /// 错误
    Error(String),
    /// 流式 usage（token 统计），仅在 compat.streaming_usage 时产生
    Usage(Usage),
    /// 有效 finish_reason（含 compat 推断）
    FinishReason(String),
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, req: &ChatRequest<'_>) -> Result<ChatResponse>;
    async fn chat_stream(&self, req: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>>;
    fn native_tool_calling(&self) -> bool;
    /// 探测模型上下文窗口大小（tokens）。默认返回 None，由具体 provider 实现。
    async fn detect_context_size(&self) -> Option<usize> {
        None
    }
    /// 可读标识（模型名），用于 `/provider` 列表标记当前模型。默认 "unknown"。
    fn label(&self) -> String {
        "unknown".into()
    }
}

/// 从 model ref（"provider_id.model_alias"）构建单个 provider 实例。
pub fn provider_from_ref(
    config: &crate::config::Config,
    model_ref: &str,
) -> Result<Arc<dyn Provider>> {
    let (prov_id, model_alias) = crate::config::Config::parse_model_ref(model_ref)?;
    let prov_cfg = config
        .provider
        .get(prov_id)
        .ok_or_else(|| anyhow::anyhow!("provider.{} not configured", prov_id))?;
    let model_cfg = prov_cfg.model.get(model_alias).ok_or_else(|| {
        anyhow::anyhow!("provider.{}.model.{} not configured", prov_id, model_alias)
    })?;
    match prov_cfg.provider_type.as_str() {
        "anthropic" => {
            let base_url = if prov_cfg.base_url.is_empty() {
                "https://api.anthropic.com"
            } else {
                &prov_cfg.base_url
            };
            Ok(Arc::new(anthropic::AnthropicProvider::new(
                base_url,
                &prov_cfg.api_key,
                &model_cfg.model,
                model_cfg.max_tokens.unwrap_or(0),
            )?))
        }
        "gemini" => {
            let base_url = if prov_cfg.base_url.is_empty() {
                "https://generativelanguage.googleapis.com"
            } else {
                &prov_cfg.base_url
            };
            Ok(Arc::new(gemini::GeminiProvider::new(
                base_url,
                &prov_cfg.api_key,
                &model_cfg.model,
                model_cfg.max_tokens.unwrap_or(0),
            )?))
        }
        // openai_compatible 及未知 type 都走 OpenAI 兼容协议（存量配置无 type 也能跑）
        _ => {
            // 兼容层：先按 base_url 探测预设，再用 [provider.<id>.compat.*] 覆盖
            let mut compat = compat::Compat::detect(&prov_cfg.base_url);
            if let Some(c) = &prov_cfg.compat {
                compat.apply_override(c);
            }
            Ok(Arc::new(openai_compat::OpenAiCompatibleProvider::new(
                &prov_cfg.base_url,
                &prov_cfg.api_key,
                &model_cfg.model,
                model_cfg.native_tool_calling,
                model_cfg.max_tokens,
                compat,
            )?))
        }
    }
}

/// 构建主 provider 链：主 model + fallback 备用链。
/// - main_ref 为空 → Ok(None)（降级模式）
/// - 主 model 构建失败 → Err（配置错误应暴露）
/// - fallback 项构建失败 → warn 跳过（备用链是容错手段，不应阻塞启动）
/// - fallback 全部不可用/未配置 → 返回裸主 provider
pub fn build_provider_chain(
    main_ref: &str,
    fallback: &[String],
    config: &crate::config::Config,
) -> Result<Option<Arc<dyn Provider>>> {
    if main_ref.is_empty() {
        return Ok(None);
    }
    let main = provider_from_ref(config, main_ref)?;
    if fallback.is_empty() {
        return Ok(Some(main));
    }
    let mut chain = vec![main];
    for f in fallback {
        match provider_from_ref(config, f) {
            Ok(p) => chain.push(p),
            Err(e) => tracing::warn!(
                model = f.as_str(),
                error = %e,
                "fallback provider build failed, skipped"
            ),
        }
    }
    if chain.len() == 1 {
        return Ok(Some(chain.remove(0)));
    }
    Ok(Some(Arc::new(fallback::FallbackProvider::new(chain))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_message_constructors() {
        let m = ChatMessage::system("hello");
        assert_eq!(m.role, Role::System);
        assert_eq!(m.content.as_text(), "hello");
        assert!(m.tool_calls.is_none());

        let m = ChatMessage::assistant_with_tools(
            "",
            vec![ToolCall {
                id: "1".into(),
                name: "file_read".into(),
                arguments: serde_json::json!({"path": "/tmp"}),
            }],
        );
        assert_eq!(m.role, Role::Assistant);
        assert_eq!(m.tool_calls.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_multimodal_message() {
        let parts = vec![
            ContentPart::Text {
                text: "这张图是什么？".into(),
            },
            ContentPart::ImageUrl {
                image_url: ImageUrlContent {
                    url: "data:image/jpeg;base64,xxx".into(),
                },
            },
        ];
        let m = ChatMessage::user_multimodal(parts);
        assert_eq!(m.role, Role::User);
        assert!(m.content.has_image());
        assert_eq!(m.content.as_text(), "这张图是什么？");
    }

    #[test]
    fn test_message_content_serialize() {
        // 纯文本序列化为字符串
        let m = ChatMessage::user("hello");
        let v = serde_json::to_value(&m.content).unwrap();
        assert_eq!(v, serde_json::json!("hello"));

        // 多模态序列化为数组
        let parts = vec![
            ContentPart::Text {
                text: "desc".into(),
            },
            ContentPart::ImageUrl {
                image_url: ImageUrlContent {
                    url: "data:image/jpeg;base64,x".into(),
                },
            },
        ];
        let m = ChatMessage::user_multimodal(parts);
        let v = serde_json::to_value(&m.content).unwrap();
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 2);
        assert_eq!(v[0]["type"], "text");
        assert_eq!(v[1]["type"], "image_url");
    }
}
