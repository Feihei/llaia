use futures_util::StreamExt;
use llaia::provider::openai_compat::OpenAiCompatibleProvider;
use llaia::provider::{ChatMessage, ChatRequest, Provider, StreamEvent};
use serde_json::json;

#[tokio::test]
async fn test_stream_text_deltas() {
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

    let provider = OpenAiCompatibleProvider::new(server.url(), "", "test-model", true).unwrap();
    let msgs = vec![ChatMessage::user("hi")];
    let req = ChatRequest { messages: &msgs, tools: None };
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

    let provider = OpenAiCompatibleProvider::new(server.url(), "", "test-model", true).unwrap();
    let msgs = vec![ChatMessage::user("read /tmp")];
    let req = ChatRequest { messages: &msgs, tools: None };
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
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/chat/completions")
        .with_status(500)
        .with_body("internal error")
        .create_async()
        .await;

    let provider = OpenAiCompatibleProvider::new(server.url(), "", "test-model", true).unwrap();
    let msgs = vec![ChatMessage::user("hi")];
    let req = ChatRequest { messages: &msgs, tools: None };
    let mut stream = provider.chat_stream(&req).await;

    let ev = stream.next().await.unwrap().unwrap();
    match ev {
        StreamEvent::Error(msg) => assert!(msg.contains("500") || msg.contains("internal")),
        other => panic!("expected Error, got {:?}", other),
    }
    m.assert_async().await;
}
