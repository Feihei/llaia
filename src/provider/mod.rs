use anyhow::Result;
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};

pub mod openai_compat;

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
