//! MCP (Model Context Protocol) JSON-RPC 2.0 协议类型与常量。
//!
//! 自实现协议层（不引入 rmcp 等外部 SDK），参考 ADR-0014。

use serde::{Deserialize, Serialize};

pub const JSONRPC_VERSION: &str = "2.0";
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// 标准 JSON-RPC 2.0 错误码
pub const METHOD_NOT_FOUND: i32 = -32601;

/// 出站 JSON-RPC 请求（client → MCP server）。
/// method call 带 id；notification 无 id（server 不回复）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl JsonRpcRequest {
    /// 构造带数字 id 的 method call 请求
    pub fn new(id: u64, method: impl Into<String>, params: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(serde_json::Value::Number(id.into())),
            method: method.into(),
            params: Some(params),
        }
    }

    /// 构造 notification（无 id，server 不回复）
    pub fn notification(method: impl Into<String>, params: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: None,
            method: method.into(),
            params: Some(params),
        }
    }
}

/// 入站 JSON-RPC 响应（MCP server → client）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC 错误对象
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// MCP server 通过 `tools/list` 声明的工具定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

/// `tools/list` 结果载荷
#[derive(Debug, Deserialize)]
pub struct McpToolsListResult {
    #[serde(default)]
    pub tools: Vec<McpToolDef>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_serializes_with_id() {
        let req = JsonRpcRequest::new(1, "tools/list", json!({}));
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"id\":1"));
        assert!(s.contains("\"method\":\"tools/list\""));
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
    }

    #[test]
    fn notification_omits_id() {
        let notif = JsonRpcRequest::notification("notifications/initialized", json!({}));
        let s = serde_json::to_string(&notif).unwrap();
        assert!(!s.contains("\"id\""));
    }

    #[test]
    fn response_deserializes_result_and_error() {
        let ok: JsonRpcResponse =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#).unwrap();
        assert!(ok.result.is_some());
        assert!(ok.error.is_none());

        let err: JsonRpcResponse = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#,
        )
        .unwrap();
        let e = err.error.unwrap();
        assert_eq!(e.code, METHOD_NOT_FOUND);
        assert_eq!(e.message, "Method not found");
    }

    #[test]
    fn tool_def_deserializes_input_schema() {
        let json =
            r#"{"name":"read_file","description":"Read a file","inputSchema":{"type":"object"}}"#;
        let def: McpToolDef = serde_json::from_str(json).unwrap();
        assert_eq!(def.name, "read_file");
        assert!(def.input_schema.is_object());
    }

    #[test]
    fn tool_def_description_and_schema_optional() {
        let def: McpToolDef = serde_json::from_str(r#"{"name":"x"}"#).unwrap();
        assert!(def.description.is_none());
        assert!(def.input_schema.is_null());
    }

    #[test]
    fn tools_list_result_deserializes() {
        let json = r#"{"tools":[{"name":"a","inputSchema":{}},{"name":"b","inputSchema":{}}]}"#;
        let result: McpToolsListResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.tools.len(), 2);
        let empty: McpToolsListResult = serde_json::from_str(r#"{}"#).unwrap();
        assert!(empty.tools.is_empty());
    }

    #[test]
    fn protocol_version_constant() {
        assert_eq!(MCP_PROTOCOL_VERSION, "2024-11-05");
    }
}
