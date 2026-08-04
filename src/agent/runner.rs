use crate::provider::{ChatMessage, ToolCall};
use crate::tools::Tool;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }
    pub fn specs(&self) -> Vec<crate::provider::ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

/// 执行工具调用。`event_tx` 用于工具（如 delegate）向 channel 转发进度事件。
pub async fn execute_tool_calls(
    registry: &ToolRegistry,
    calls: &[ToolCall],
    channel: &str,
    qq_confirm_mode: &str,
    event_tx: Option<&mpsc::Sender<crate::agent::TurnEvent>>,
) -> Result<Vec<ChatMessage>> {
    let mut results = Vec::new();
    for call in calls {
        let tool = match registry.get(&call.name) {
            Some(t) => t,
            None => {
                tracing::warn!(tool = %call.name, "unknown tool");
                results.push(ChatMessage::tool(
                    format!("[error: unknown tool {}]", call.name),
                    &call.id,
                ));
                continue;
            }
        };

        // QQ channel 下的 confirm 检查：跳过有副作用的工具
        if channel == "qq" && tool.requires_confirm() {
            let allowed = match qq_confirm_mode {
                "none" => true,
                _ => false, // "always"（默认）和 "whitelist"（v1.5 简化：禁用所有需确认工具）
            };
            if !allowed {
                let msg = format!("QQ 频道下不能执行此操作：{}", call.name);
                tracing::warn!(tool = %call.name, mode = qq_confirm_mode, "qq blocked");
                results.push(ChatMessage::tool(msg, &call.id));
                continue;
            }
        }

        tracing::info!(tool = %call.name, args = %call.arguments, "executing tool");
        let outcome = match tool
            .execute_with_events(&call.arguments, channel, event_tx)
            .await
        {
            Ok(s) => s,
            Err(e) => format!("[error: {}]", e),
        };
        tracing::info!(tool = %call.name, len = outcome.len(), "tool done");
        results.push(ChatMessage::tool(outcome, &call.id));
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::{json, Value};

    struct EchoTool;
    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echo back"
        }
        fn parameters_schema(&self) -> Value {
            json!({"type":"object"})
        }
        async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
            Ok(format!("{}", args))
        }
    }

    #[tokio::test]
    async fn test_execute_calls() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let calls = vec![ToolCall {
            id: "1".into(),
            name: "echo".into(),
            arguments: json!({"x": 1}),
        }];
        let msgs = execute_tool_calls(&reg, &calls, "cli", "always", None)
            .await
            .unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, crate::provider::Role::Tool);
        assert!(msgs[0].content.as_text().contains("x"));
    }

    #[tokio::test]
    async fn test_unknown_tool() {
        let reg = ToolRegistry::new();
        let calls = vec![ToolCall {
            id: "1".into(),
            name: "missing".into(),
            arguments: json!({}),
        }];
        // unknown tool 不再返回 Err，而是返回一条错误消息
        let msgs = execute_tool_calls(&reg, &calls, "cli", "always", None)
            .await
            .unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.as_text().contains("unknown tool"));
    }

    /// 验证 QQ channel + always 模式下，requires_confirm=true 的工具被跳过
    #[tokio::test]
    async fn test_qq_blocks_confirm_required_tool() {
        struct DangerousTool;
        #[async_trait]
        impl Tool for DangerousTool {
            fn name(&self) -> &str {
                "dangerous"
            }
            fn description(&self) -> &str {
                "dangerous"
            }
            fn parameters_schema(&self) -> Value {
                json!({"type":"object"})
            }
            fn requires_confirm(&self) -> bool {
                true
            }
            async fn execute(&self, _args: &Value, _channel: &str) -> Result<String> {
                Ok("executed".into())
            }
        }

        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(DangerousTool));
        let calls = vec![ToolCall {
            id: "1".into(),
            name: "dangerous".into(),
            arguments: json!({}),
        }];

        // QQ + always：应被跳过，返回提示消息
        let msgs = execute_tool_calls(&reg, &calls, "qq", "always", None)
            .await
            .unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0]
            .content
            .as_text()
            .contains("QQ 频道下不能执行此操作"));

        // QQ + none：应执行
        let msgs = execute_tool_calls(&reg, &calls, "qq", "none", None)
            .await
            .unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content.as_text(), "executed");

        // CLI + always：应执行（CLI 不受 QQ confirm 限制）
        let msgs = execute_tool_calls(&reg, &calls, "cli", "always", None)
            .await
            .unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content.as_text(), "executed");
    }
}
