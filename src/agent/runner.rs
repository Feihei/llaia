use crate::agent::approval::{
    approval_decision, format_approval_prompt, is_interactive_channel, ApprovalAction,
    ApprovalContext,
};
use crate::agent::TurnEvent;
use crate::provider::{ChatMessage, ToolCall};
use crate::tools::todo::TodoStore;
use crate::tools::Tool;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

pub struct ToolRegistry {
    tools: RwLock<HashMap<String, Arc<dyn Tool>>>,
    /// 规划后执行（ADR-0024）的共享 todo 存储：agent 每轮写入 current_session，
    /// todo 工具按它路由；未挂真实 workspace 时（测试/降级）为禁用态。
    pub todo_store: Arc<TodoStore>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
            todo_store: Arc::new(TodoStore::disabled()),
        }
    }
    pub fn register(&self, tool: Arc<dyn Tool>) {
        self.tools
            .write()
            .unwrap()
            .insert(tool.name().to_string(), tool);
    }
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.read().unwrap().get(name).cloned()
    }
    pub fn specs(&self) -> Vec<crate::provider::ToolSpec> {
        self.tools
            .read()
            .unwrap()
            .values()
            .map(|t| t.spec())
            .collect()
    }
    pub fn names(&self) -> Vec<String> {
        self.tools.read().unwrap().keys().cloned().collect()
    }
    /// 替换所有 MCP 来源的工具（名称含 "__" 前缀）。
    /// 热加载 MCP 时调用：先移除旧 MCP 工具，再注册新集合，内置/delegate/cron 工具不受影响。
    pub fn replace_mcp_tools(&self, new: Vec<Arc<dyn Tool>>) {
        let mut g = self.tools.write().unwrap();
        g.retain(|k, _| !k.contains("__"));
        for t in new {
            g.insert(t.name().to_string(), t);
        }
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
    let (profile, workspace, trusted, gate, agent_alias, audit) = (
        &ctx.profile,
        &ctx.workspace,
        &ctx.trusted,
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

        // ask_user 阻塞式澄清（ADR-0022）：在审批判定之前拦截。
        // 交互频道 → 注册 pending question + 占位结果 + 本轮暂停（deferred）；
        // 非交互频道（mail 等）→ 直接返回"按最合理假设继续"，不暂停。
        if call.name == crate::tools::ask_user::ASK_USER_TOOL_NAME {
            match crate::tools::ask_user::parse_ask_user_args(&call.arguments) {
                Ok((question, choices)) => {
                    if is_interactive_channel(channel) {
                        let id = gate
                            .register_question(
                                &question,
                                choices.clone(),
                                channel,
                                agent_alias,
                                ctx.ask_user_timeout_secs,
                            )
                            .await;
                        let choice_hint = match &choices {
                            Some(cs) => format!(
                                "\n   Available answers: {} (reply with the option text or its number)",
                                cs.iter()
                                    .enumerate()
                                    .map(|(i, c)| format!("{}. {}", i + 1, c))
                                    .collect::<Vec<_>>()
                                    .join("  ")
                            ),
                            None => String::new(),
                        };
                        let prompt = format!(
                            "\n❓ Please answer a question (id={}):\n   {}\n   {}Just reply with your answer (use `/answer {} your-answer` to disambiguate when several are pending).\n",
                            id, question, choice_hint, id
                        );
                        tracing::info!(id = %id, question = %question, "pending question registered");
                        if let Some(tx) = event_tx {
                            let _ = tx
                                .send(TurnEvent::Chunk {
                                    delta: prompt.clone(),
                                })
                                .await;
                        }
                        results.push(ChatMessage::tool(
                            format!(
                                "[⏳ waiting for your answer] the question has been submitted to you (id={}); reply once you have an answer and I will continue.",
                                id
                            ),
                            &call.id,
                        ));
                        deferred = true;
                        continue;
                    } else {
                        results.push(ChatMessage::tool(
                            "[cannot ask the user] this channel is non-interactive and cannot wait for an answer; proceeding with the most reasonable assumption.",
                            &call.id,
                        ));
                        continue;
                    }
                }
                Err(e) => {
                    results.push(ChatMessage::tool(
                        format!("[ask_user error: {}]", e),
                        &call.id,
                    ));
                    continue;
                }
            }
        }

        match approval_decision(
            tool.as_ref(),
            &call.arguments,
            workspace,
            trusted,
            profile,
            channel,
        ) {
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
                    format!(
                        "[awaiting confirmation id={}] this operation needs your confirmation.",
                        id
                    ),
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
        let reg = ToolRegistry::new();
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
                trusted: Vec::new(),
                gate: ApprovalGate::new(),
                agent_alias: "main".into(),
                audit: None,
                ask_user_timeout_secs: 0,
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
                trusted: Vec::new(),
                gate: ApprovalGate::new(),
                agent_alias: "main".into(),
                audit: None,
                ask_user_timeout_secs: 0,
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

        let reg = ToolRegistry::new();
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
                trusted: Vec::new(),
                gate: ApprovalGate::new(),
                agent_alias: "main".into(),
                audit: None,
                ask_user_timeout_secs: 0,
            },
            None,
        )
        .await
        .unwrap();
        assert!(deferred);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.as_text().contains("awaiting confirmation"));

        // qq + yolo：直接执行
        let (msgs, deferred) = execute_tool_calls(
            &reg,
            &calls,
            "qq",
            &ApprovalContext {
                profile: "yolo".into(),
                workspace: PathBuf::from("/tmp"),
                trusted: Vec::new(),
                gate: ApprovalGate::new(),
                agent_alias: "main".into(),
                audit: None,
                ask_user_timeout_secs: 0,
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
                trusted: Vec::new(),
                gate: ApprovalGate::new(),
                agent_alias: "main".into(),
                audit: None,
                ask_user_timeout_secs: 0,
            },
            None,
        )
        .await
        .unwrap();
        assert!(!deferred);
        assert_eq!(msgs[0].content.as_text(), "executed");
    }
}
