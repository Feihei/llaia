use crate::agent::approval::{
    approval_decision, format_approval_prompt, ApprovalAction, ApprovalContext,
};
use crate::agent::TurnEvent;
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

/// 执行工具调用。P4-d 起由权限档位（profile）+ 审批门控决定是否需要交互式确认。
///
/// 返回 `(工具结果消息, 是否有请求被推迟审批)`。当 `deferred == true` 时，调用方
/// （`handle_message_streaming`）应暂停本轮 agent turn，交由 `/ok` `/deny` 解析后续。
pub async fn execute_tool_calls(
    registry: &ToolRegistry,
    calls: &[ToolCall],
    channel: &str,
    ctx: &ApprovalContext,
    event_tx: Option<&mpsc::Sender<TurnEvent>>,
) -> Result<(Vec<ChatMessage>, bool)> {
    let (profile, workspace, gate, agent_alias, audit) = (
        &ctx.profile,
        &ctx.workspace,
        &ctx.gate,
        &ctx.agent_alias,
        &ctx.audit,
    );
    let mut results = Vec::new();
    let mut deferred = false;
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

        match approval_decision(tool.as_ref(), &call.arguments, workspace, profile, channel) {
            // 直接执行
            ApprovalAction::Approved => {}
            // 非交互频道无法等待用户：自动拒绝
            ApprovalAction::Denied { reason } => {
                tracing::warn!(tool = %call.name, channel, "auto-denied (non-interactive channel)");
                if let Some(a) = &audit {
                    let _ = a
                        .write(
                            agent_alias,
                            channel,
                            &call.name,
                            &call.arguments.to_string(),
                            "blocked",
                            Some("non_interactive"),
                        )
                        .await;
                }
                results.push(ChatMessage::tool(reason, &call.id));
                continue;
            }
            // 需要审批：注册 pending + 提示用户 + 占位结果，本轮推迟
            ApprovalAction::NeedsApproval { within_workspace } => {
                deferred = true;
                let id = gate
                    .register(
                        &call.name,
                        &call.arguments,
                        &call.id,
                        channel,
                        agent_alias,
                        within_workspace,
                    )
                    .await;
                let prompt = format_approval_prompt(
                    &call.name,
                    &call.arguments,
                    workspace,
                    &id,
                    within_workspace,
                );
                tracing::info!(tool = %call.name, id = %id, "pending approval registered");
                if let Some(tx) = event_tx {
                    let _ = tx
                        .send(TurnEvent::Chunk {
                            delta: prompt.clone(),
                        })
                        .await;
                }
                if let Some(a) = &audit {
                    let _ = a
                        .write(
                            agent_alias,
                            channel,
                            &call.name,
                            &call.arguments.to_string(),
                            "blocked",
                            Some("pending_approval"),
                        )
                        .await;
                }
                results.push(ChatMessage::tool(
                    format!("[等待确认 id={}] 该操作需要你确认后才能执行。", id),
                    &call.id,
                ));
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
    Ok((results, deferred))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::approval::{ApprovalContext, ApprovalGate};
    use async_trait::async_trait;
    use serde_json::{json, Value};
    use std::path::PathBuf;
    use std::sync::Arc;

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
        let (msgs, deferred) = execute_tool_calls(
            &reg,
            &calls,
            "cli",
            &ApprovalContext {
                profile: "default".into(),
                workspace: PathBuf::from("/tmp"),
                gate: ApprovalGate::new(),
                agent_alias: "main".into(),
                audit: None,
            },
            None,
        )
        .await
        .unwrap();
        assert!(!deferred);
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
        let (msgs, _) = execute_tool_calls(
            &reg,
            &calls,
            "cli",
            &ApprovalContext {
                profile: "default".into(),
                workspace: PathBuf::from("/tmp"),
                gate: ApprovalGate::new(),
                agent_alias: "main".into(),
                audit: None,
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.as_text().contains("unknown tool"));
    }

    /// 验证 P4-d 权限档位：read-only 对非 CLI 频道需要审批（deferred）；
    /// yolo 直接执行；default 下 workspace 内操作直接执行。
    #[tokio::test]
    async fn test_permission_profile_gates_confirm_required_tool() {
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

        // qq + read-only：需要审批 → deferred，结果含等待确认提示
        let (msgs, deferred) = execute_tool_calls(
            &reg,
            &calls,
            "qq",
            &ApprovalContext {
                profile: "read-only".into(),
                workspace: PathBuf::from("/tmp"),
                gate: ApprovalGate::new(),
                agent_alias: "main".into(),
                audit: None,
            },
            None,
        )
        .await
        .unwrap();
        assert!(deferred);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.as_text().contains("等待确认"));

        // qq + yolo：直接执行
        let (msgs, deferred) = execute_tool_calls(
            &reg,
            &calls,
            "qq",
            &ApprovalContext {
                profile: "yolo".into(),
                workspace: PathBuf::from("/tmp"),
                gate: ApprovalGate::new(),
                agent_alias: "main".into(),
                audit: None,
            },
            None,
        )
        .await
        .unwrap();
        assert!(!deferred);
        assert_eq!(msgs[0].content.as_text(), "executed");

        // cli + default：DangerousTool 视为 workspace 内（无路径特征）→ 直接执行
        let (msgs, deferred) = execute_tool_calls(
            &reg,
            &calls,
            "cli",
            &ApprovalContext {
                profile: "default".into(),
                workspace: PathBuf::from("/tmp"),
                gate: ApprovalGate::new(),
                agent_alias: "main".into(),
                audit: None,
            },
            None,
        )
        .await
        .unwrap();
        assert!(!deferred);
        assert_eq!(msgs[0].content.as_text(), "executed");
    }
}
