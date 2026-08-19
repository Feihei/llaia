use futures_util::StreamExt;
use llaia::provider::compat::Compat;
use llaia::provider::openai_compat::OpenAiCompatibleProvider;
use llaia::provider::{ChatMessage, ChatRequest, Provider, StreamEvent};
use serde_json::json;

/// Windows 系统代理（注册表配置，如 Clash）常不 bypass loopback，
/// reqwest 默认读取系统代理，导致对 mockito 本地 server 的请求被代理截断。
fn bypass_proxy() {
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    std::env::set_var("no_proxy", "127.0.0.1,localhost");
}

#[tokio::test]
async fn test_stream_text_deltas() {
    bypass_proxy();
    let mut server = mockito::Server::new_async().await;
    let sse_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let m = server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(sse_body)
        .create_async()
        .await;

    let provider = OpenAiCompatibleProvider::new(
        server.url(),
        "",
        "test-model",
        true,
        None,
        Compat::default(),
    )
    .unwrap();
    let msgs = vec![ChatMessage::user("hi")];
    let req = ChatRequest {
        messages: &msgs,
        tools: None,
        disable_thinking: false,
    };
    let mut stream = provider.chat_stream(&req).await;

    let mut deltas = Vec::new();
    let mut done = false;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            StreamEvent::TextDelta(d) => deltas.push(d),
            StreamEvent::Done => done = true,
            _ => {}
        }
    }
    m.assert_async().await;
    assert_eq!(deltas.concat(), "hello world");
    assert!(done);
}

#[tokio::test]
async fn test_stream_tool_calls_accumulated() {
    bypass_proxy();
    let mut server = mockito::Server::new_async().await;
    let sse_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"file_read\",\"arguments\":\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\":\\\"/tmp\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let m = server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(sse_body)
        .create_async()
        .await;

    let provider = OpenAiCompatibleProvider::new(
        server.url(),
        "",
        "test-model",
        true,
        None,
        Compat::default(),
    )
    .unwrap();
    let msgs = vec![ChatMessage::user("read /tmp")];
    let req = ChatRequest {
        messages: &msgs,
        tools: None,
        disable_thinking: false,
    };
    let mut stream = provider.chat_stream(&req).await;

    let mut tool_calls = Vec::new();
    let mut done = false;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            StreamEvent::ToolCall(tc) => tool_calls.push(tc),
            StreamEvent::Done => done = true,
            _ => {}
        }
    }
    m.assert_async().await;
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "call_1");
    assert_eq!(tool_calls[0].name, "file_read");
    assert_eq!(tool_calls[0].arguments, json!({"path": "/tmp"}));
    assert!(done);
}

#[tokio::test]
async fn test_stream_error_status() {
    bypass_proxy();
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/chat/completions")
        .with_status(500)
        .with_body("internal error")
        .create_async()
        .await;

    let provider = OpenAiCompatibleProvider::new(
        server.url(),
        "",
        "test-model",
        true,
        None,
        Compat::default(),
    )
    .unwrap();
    let msgs = vec![ChatMessage::user("hi")];
    let req = ChatRequest {
        messages: &msgs,
        tools: None,
        disable_thinking: false,
    };
    let mut stream = provider.chat_stream(&req).await;

    let ev = stream.next().await.unwrap().unwrap();
    match ev {
        StreamEvent::Error(msg) => assert!(msg.contains("500") || msg.contains("internal")),
        other => panic!("expected Error, got {:?}", other),
    }
    m.assert_async().await;
}
