use crate::provider::{ChatMessage, ToolCall};
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Arc;

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
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

pub async fn execute_tool_calls(
    registry: &ToolRegistry,
    calls: &[ToolCall],
) -> Result<Vec<ChatMessage>> {
    let mut results = Vec::new();
    for call in calls {
        let tool = registry
            .get(&call.name)
            .ok_or_else(|| anyhow!("unknown tool: {}", call.name))?;
        tracing::info!(tool = %call.name, args = %call.arguments, "executing tool");
        let outcome = match tool.execute(&call.arguments).await {
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
        async fn execute(&self, args: &Value) -> Result<String> {
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
        let msgs = execute_tool_calls(&reg, &calls).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, crate::provider::Role::Tool);
        assert!(msgs[0].content.contains("x"));
    }

    #[tokio::test]
    async fn test_unknown_tool() {
        let reg = ToolRegistry::new();
        let calls = vec![ToolCall {
            id: "1".into(),
            name: "missing".into(),
            arguments: json!({}),
        }];
        let result = execute_tool_calls(&reg, &calls).await;
        assert!(result.is_err());
    }
}
