use crate::audit::AuditLog;
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

/// 执行工具调用。confirm_mode 为全局开关（不再 per-channel）。
/// audit：可选审计日志写入器
pub async fn execute_tool_calls(
    registry: &ToolRegistry,
    calls: &[ToolCall],
    channel: &str,
    confirm_mode: &str,
    agent_alias: &str,
    audit: Option<Arc<AuditLog>>,
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

        // 全局 confirm_mode 检查（不再区分 channel）
        if tool.requires_confirm() && confirm_mode != "none" {
            // always / session 模式下，非 CLI channel 无法弹确认，拒绝
            if channel != "cli" {
                let msg = format!("该操作需在 CLI 确认：{}", call.name);
                tracing::warn!(tool = %call.name, mode = confirm_mode, channel, "blocked by confirm_mode");
                if let Some(a) = &audit {
                    let _ = a
                        .write(
                            agent_alias,
                            channel,
                            &call.name,
                            &call.arguments.to_string(),
                            "blocked",
                            Some("confirm_mode"),
                        )
                        .await;
                }
                results.push(ChatMessage::tool(msg, &call.id));
                continue;
            }
            // CLI channel：弹 stdin 确认（session 模式简化为每次弹，未来可加 token 缓存）
            if !crate::tools::terminal::Terminal::prompt_confirm(&call.name) {
                let msg = format!("用户拒绝执行：{}", call.name);
                if let Some(a) = &audit {
                    let _ = a
                        .write(
                            agent_alias,
                            channel,
                            &call.name,
                            &call.arguments.to_string(),
                            "blocked",
                            Some("user_denied"),
                        )
                        .await;
                }
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
            Err(e) => {
                let err_msg = format!("[error: {}]", e);
                if let Some(a) = &audit {
                    let _ = a
                        .write(
                            agent_alias,
                            channel,
                            &call.name,
                            &call.arguments.to_string(),
                            "error",
                            Some(&e.to_string()),
                        )
                        .await;
                }
                err_msg
            }
        };
        tracing::info!(tool = %call.name, len = outcome.len(), "tool done");

        // 审计成功执行
        if let Some(a) = &audit {
            let _ = a
                .write(
                    agent_alias,
                    channel,
                    &call.name,
                    &call.arguments.to_string(),
                    "ok",
                    None,
                )
                .await;
        }

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
        let msgs = execute_tool_calls(&reg, &calls, "cli", "none", "main", None, None)
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
        let msgs = execute_tool_calls(&reg, &calls, "cli", "none", "main", None, None)
            .await
            .unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.as_text().contains("unknown tool"));
    }

    /// 验证 confirm_mode=always + 非 CLI channel 下，requires_confirm=true 的工具被拒绝
    #[tokio::test]
    async fn test_non_cli_blocks_confirm_required_tool() {
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

        // qq + always：应被拒绝
        let msgs = execute_tool_calls(&reg, &calls, "qq", "always", "main", None, None)
            .await
            .unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.as_text().contains("需在 CLI 确认"));

        // qq + none：应执行
        let msgs = execute_tool_calls(&reg, &calls, "qq", "none", "main", None, None)
            .await
            .unwrap();
        assert_eq!(msgs[0].content.as_text(), "executed");

        // cli + none：CLI 不弹确认（none 模式），直接执行
        let msgs = execute_tool_calls(&reg, &calls, "cli", "none", "main", None, None)
            .await
            .unwrap();
        assert_eq!(msgs[0].content.as_text(), "executed");
    }
}
