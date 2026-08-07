//! Anthropic Messages API 集成测试（mockito mock SSE 流）

use llaia::provider::anthropic::AnthropicProvider;
use llaia::provider::{ChatMessage, ChatRequest, Provider, ToolSpec};

/// Windows 系统代理（注册表配置，如 Clash）常不 bypass loopback，
/// reqwest 默认读取系统代理，导致对 mockito 本地 server 的请求被代理截断。
fn bypass_proxy() {
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    std::env::set_var("no_proxy", "127.0.0.1,localhost");
}

/// 构造包含文本增量 + tool_use block 的完整 SSE body
fn sse_text_and_tool() -> String {
    let events = [
        (
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant"}}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ),
        (
            "content_block_start",
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"file_read","input":{}}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"/tmp/x\"}"}}"#,
        ),
        (
            "content_block_stop",
            r#"{"type":"content_block_stop","index":1}"#,
        ),
        (
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ];
    let mut body = String::new();
    for (event, data) in events {
        body.push_str(&format!("event: {}\ndata: {}\n\n", event, data));
    }
    body
}

#[tokio::test]
async fn test_anthropic_stream_text_and_tool() {
    bypass_proxy();
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/v1/messages")
        .match_header("x-api-key", "sk-test-key")
        .match_header("anthropic-version", "2023-06-01")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(sse_text_and_tool())
        .create_async()
        .await;

    let provider =
        AnthropicProvider::new(server.url(), "sk-test-key", "claude-test", 1024).unwrap();
    let msgs = vec![ChatMessage::user("读 /tmp/x")];
    let req = ChatRequest {
        messages: &msgs,
        tools: None,
    };
    let resp = provider.chat(&req).await.unwrap();

    m.assert_async().await;
    assert_eq!(resp.text.as_deref(), Some("Hello world"));
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].id, "toolu_1");
    assert_eq!(resp.tool_calls[0].name, "file_read");
    assert_eq!(
        resp.tool_calls[0].arguments,
        serde_json::json!({"path": "/tmp/x"})
    );
}

#[tokio::test]
async fn test_anthropic_payload_system_and_tools() {
    bypass_proxy();
    let mut server = mockito::Server::new_async().await;
    // 校验 payload：max_tokens 必传、system 提升到顶层、tools 转 input_schema
    // （字段序为 struct 声明序：max_tokens 在 system 前，input_schema 在 tools 内）
    let m = server
        .mock("POST", "/v1/messages")
        .match_body(mockito::Matcher::Regex(
            r#""max_tokens":2048.*"system":"你是助手".*"input_schema":\{"type":"object"\}"#
                .to_string(),
        ))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n")
        .create_async()
        .await;

    let provider = AnthropicProvider::new(server.url(), "k", "claude-test", 2048).unwrap();
    let msgs = vec![ChatMessage::system("你是助手"), ChatMessage::user("hi")];
    let tools = vec![ToolSpec {
        name: "file_read".into(),
        description: "读文件".into(),
        parameters: serde_json::json!({"type": "object"}),
    }];
    let req = ChatRequest {
        messages: &msgs,
        tools: Some(&tools),
    };
    provider.chat(&req).await.unwrap();
    m.assert_async().await;
}

#[tokio::test]
async fn test_anthropic_error_status() {
    bypass_proxy();
    let mut server = mockito::Server::new_async().await;
    server
        .mock("POST", "/v1/messages")
        .with_status(401)
        .with_body(r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#)
        .create_async()
        .await;

    let provider = AnthropicProvider::new(server.url(), "bad-key", "claude-test", 1024).unwrap();
    let msgs = vec![ChatMessage::user("hi")];
    let req = ChatRequest {
        messages: &msgs,
        tools: None,
    };
    let err = provider.chat(&req).await.unwrap_err();
    assert!(err.to_string().contains("401"));
}

#[tokio::test]
async fn test_anthropic_sse_error_event() {
    bypass_proxy();
    let mut server = mockito::Server::new_async().await;
    server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"overloaded\"}}\n\n",
        )
        .create_async()
        .await;

    let provider = AnthropicProvider::new(server.url(), "k", "claude-test", 1024).unwrap();
    let msgs = vec![ChatMessage::user("hi")];
    let req = ChatRequest {
        messages: &msgs,
        tools: None,
    };
    let err = provider.chat(&req).await.unwrap_err();
    assert!(err.to_string().contains("overloaded"));
}
