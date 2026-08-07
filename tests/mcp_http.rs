//! MCP client 集成测试：用 mockito 模拟 streamable HTTP MCP server，
//! 覆盖握手（initialize + initialized + tools/list）、工具调用、isError、SSE 响应解析。

use llaia::mcp::client::McpRegistry;
use llaia::mcp::{McpServerConfig, McpTransportKind};
use mockito::Matcher;
use serde_json::json;
use std::collections::HashMap;

fn http_cfg(url: &str) -> McpServerConfig {
    McpServerConfig {
        id: "mock".into(),
        enabled: true,
        transport: McpTransportKind::Http,
        command: None,
        args: vec![],
        env: HashMap::new(),
        url: Some(format!("{}/mcp", url)),
        headers: HashMap::new(),
        tool_timeout_secs: None,
        safe_tools: vec!["read_file".into()],
    }
}

/// 注册握手三连 mock：initialize（带 session 头）/ initialized 通知 / tools/list
async fn mock_handshake(server: &mut mockito::Server) {
    // initialize（client 首个请求 id=1）
    server
        .mock("POST", "/mcp")
        .match_body(Matcher::Regex("\"initialize\"".into()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_header("mcp-session-id", "sess-1")
        .with_body(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "serverInfo": { "name": "mock", "version": "0" }
                }
            })
            .to_string(),
        )
        .expect(1)
        .create_async()
        .await;

    // notifications/initialized（无 id，应答 202）
    server
        .mock("POST", "/mcp")
        .match_body(Matcher::Regex("notifications/initialized".into()))
        .with_status(202)
        .expect(1)
        .create_async()
        .await;

    // tools/list（id=2），必须携带握手拿到的 session 头
    server
        .mock("POST", "/mcp")
        .match_body(Matcher::Regex("tools/list".into()))
        .match_header("mcp-session-id", "sess-1")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "tools": [
                        {
                            "name": "read_file",
                            "description": "Read a file",
                            "inputSchema": { "type": "object", "properties": { "path": { "type": "string" } } }
                        },
                        {
                            "name": "write_file",
                            "description": "Write a file",
                            "inputSchema": { "type": "object" }
                        }
                    ]
                }
            })
            .to_string(),
        )
        .expect(1)
        .create_async()
        .await;
}

#[tokio::test]
async fn test_http_handshake_and_tool_call() {
    let mut server = mockito::Server::new_async().await;
    mock_handshake(&mut server).await;

    // tools/call read_file（id=3）
    server
        .mock("POST", "/mcp")
        .match_body(Matcher::Regex("read_file".into()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "result": { "content": [{ "type": "text", "text": "file content" }] }
            })
            .to_string(),
        )
        .expect(1)
        .create_async()
        .await;

    let registry = McpRegistry::connect_all(&[http_cfg(&server.url())]).await;
    assert_eq!(registry.server_count(), 1);
    assert_eq!(registry.tool_count(), 2);

    let out = registry
        .call_tool("mock__read_file", json!({"path": "/tmp/a"}))
        .await
        .unwrap();
    assert_eq!(out, "file content");

    // safe_tools 命中 → 免确认
    assert!(registry.is_safe_tool("mock__read_file"));
    assert!(!registry.is_safe_tool("mock__write_file"));
}

#[tokio::test]
async fn test_http_is_error_envelope() {
    let mut server = mockito::Server::new_async().await;
    mock_handshake(&mut server).await;

    server
        .mock("POST", "/mcp")
        .match_body(Matcher::Regex("write_file".into()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "result": {
                    "isError": true,
                    "content": [{ "type": "text", "text": "permission denied" }]
                }
            })
            .to_string(),
        )
        .expect(1)
        .create_async()
        .await;

    let registry = McpRegistry::connect_all(&[http_cfg(&server.url())]).await;
    let err = registry
        .call_tool("mock__write_file", json!({}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("permission denied"));
}

#[tokio::test]
async fn test_http_sse_response_body() {
    // streamable HTTP 允许 POST 响应用 SSE 承载
    let mut server = mockito::Server::new_async().await;
    mock_handshake(&mut server).await;

    let sse_body = format!(
        "data: {}\n\n",
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "result": { "content": [{ "type": "text", "text": "via sse" }] }
        })
    );
    server
        .mock("POST", "/mcp")
        .match_body(Matcher::Regex("read_file".into()))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(sse_body)
        .expect(1)
        .create_async()
        .await;

    let registry = McpRegistry::connect_all(&[http_cfg(&server.url())]).await;
    let out = registry
        .call_tool("mock__read_file", json!({}))
        .await
        .unwrap();
    assert_eq!(out, "via sse");
}

#[tokio::test]
async fn test_http_jsonrpc_error_response() {
    let mut server = mockito::Server::new_async().await;
    mock_handshake(&mut server).await;

    server
        .mock("POST", "/mcp")
        .match_body(Matcher::Regex("read_file".into()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "error": { "code": -32602, "message": "invalid params" }
            })
            .to_string(),
        )
        .expect(1)
        .create_async()
        .await;

    let registry = McpRegistry::connect_all(&[http_cfg(&server.url())]).await;
    let err = registry
        .call_tool("mock__read_file", json!({}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("invalid params"));
}

#[tokio::test]
async fn test_connect_failure_recorded_not_fatal() {
    // 连接被拒 → connect_all 不报错，registry 为空
    let cfg = McpServerConfig {
        id: "dead".into(),
        enabled: true,
        transport: McpTransportKind::Http,
        command: None,
        args: vec![],
        env: HashMap::new(),
        // 随机端口，基本不会有服务
        url: Some("http://127.0.0.1:1/mcp".into()),
        headers: HashMap::new(),
        tool_timeout_secs: None,
        safe_tools: vec![],
    };
    let registry = McpRegistry::connect_all(&[cfg]).await;
    assert_eq!(registry.server_count(), 0);
    assert_eq!(registry.tool_count(), 0);
    let status = registry.status().await;
    assert_eq!(status.len(), 1);
    assert_eq!(status[0]["status"], "dead");
}
