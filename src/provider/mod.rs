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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }
    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }
    pub fn tool(content: impl Into<String>, tool_call_id: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
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
    async fn chat_stream(
        &self,
        req: &ChatRequest<'_>,
    ) -> BoxStream<'_, Result<StreamEvent>>;
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
        assert_eq!(m.content, "hello");
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
}
