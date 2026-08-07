//! McpTool adapter：把 MCP server 提供的工具包装成 LLAIA `Tool` trait 实现。
//! 工具名带 `<server_id>__` 双下划线前缀（如 `filesystem__read_file`）。

use crate::mcp::client::McpRegistry;
use crate::mcp::protocol::McpToolDef;
use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

pub struct McpTool {
    /// 双下划线前缀名：`<server_id>__<tool_name>`
    prefixed_name: String,
    description: String,
    /// input_schema 用 Arc 共享，避免每次 spec 组装深拷贝（MCP schema 可能数十 KB）
    input_schema: Arc<Value>,
    /// 共享 registry，调用时路由到正确 server
    registry: Arc<McpRegistry>,
    /// safe_tools 白名单命中时免确认（默认 MCP 工具都有副作用，需确认）
    requires_confirm: bool,
}

impl McpTool {
    pub fn new(prefixed_name: String, def: McpToolDef, registry: Arc<McpRegistry>) -> Self {
        let requires_confirm = !registry.is_safe_tool(&prefixed_name);
        let description = def
            .description
            .clone()
            .unwrap_or_else(|| format!("MCP tool ({})", prefixed_name));
        Self {
            prefixed_name,
            description,
            input_schema: Arc::new(def.input_schema),
            registry,
            requires_confirm,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.prefixed_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        (*self.input_schema).clone()
    }

    fn requires_confirm(&self) -> bool {
        self.requires_confirm
    }

    async fn execute(&self, args: &Value, channel: &str) -> Result<String> {
        let _ = channel;
        // MCP server 不认识 LLAIA 的 confirm 相关注入字段，原样转发 args 即可；
        // registry 负责路由到正确 server，返回序列化后的文本
        self.registry
            .call_tool(&self.prefixed_name, args.clone())
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_def(name: &str, desc: Option<&str>) -> McpToolDef {
        McpToolDef {
            name: name.to_string(),
            description: desc.map(str::to_string),
            input_schema: json!({"type": "object"}),
        }
    }

    #[tokio::test]
    async fn test_mcp_tool_accessors() {
        let registry = Arc::new(McpRegistry::empty());
        let tool = McpTool::new(
            "fs__read_file".into(),
            make_def("read_file", Some("Read a file")),
            registry,
        );
        assert_eq!(tool.name(), "fs__read_file");
        assert_eq!(tool.description(), "Read a file");
        // 不在 registry 索引中 → is_safe_tool = false → 需确认
        assert!(tool.requires_confirm());
        let spec = tool.spec();
        assert_eq!(spec.name, "fs__read_file");
    }

    #[tokio::test]
    async fn test_mcp_tool_description_fallback() {
        let registry = Arc::new(McpRegistry::empty());
        let tool = McpTool::new("srv__x".into(), make_def("x", None), registry);
        assert_eq!(tool.description(), "MCP tool (srv__x)");
    }

    #[tokio::test]
    async fn test_mcp_tool_execute_unknown_tool_errors() {
        let registry = Arc::new(McpRegistry::empty());
        let tool = McpTool::new("srv__ghost".into(), make_def("ghost", None), registry);
        let err = tool.execute(&json!({}), "cli").await.unwrap_err();
        assert!(err.to_string().contains("unknown MCP tool"));
    }
}
