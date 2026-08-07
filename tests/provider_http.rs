use llaia::provider::openai_compat::OpenAiCompatibleProvider;
use llaia::provider::{ChatMessage, ChatRequest, Provider};
use serde_json::json;

/// Windows 系统代理（注册表配置，如 Clash）常不 bypass loopback，
/// reqwest 默认读取系统代理，导致对 mockito 本地 server 的请求被代理截断。
fn bypass_proxy() {
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    std::env::set_var("no_proxy", "127.0.0.1,localhost");
}

#[tokio::test]
async fn test_native_tool_calling() {
    bypass_proxy();
    let mut server = mockito::Server::new_async().await;
    let delta = json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": {
                        "name": "file_read",
                        "arguments": "{\"path\":\"/tmp/x\"}"
                    }
                }]
            }
        }]
    });
    let sse_body = format!("data: {}\n\ndata: [DONE]\n\n", delta);
    let m = server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(sse_body)
        .create_async()
        .await;

    let provider = OpenAiCompatibleProvider::new(server.url(), "", "test-model", true).unwrap();
    let msgs = vec![ChatMessage::user("read /tmp/x")];
    let req = ChatRequest {
        messages: &msgs,
        tools: None,
    };
    let resp = provider.chat(&req).await.unwrap();

    m.assert_async().await;
    assert!(resp.text.is_none());
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].name, "file_read");
    assert_eq!(resp.tool_calls[0].arguments, json!({"path": "/tmp/x"}));
}

#[tokio::test]
async fn test_text_response() {
    bypass_proxy();
    let mut server = mockito::Server::new_async().await;
    let delta = json!({
        "choices": [{
            "delta": { "content": "hello back" }
        }]
    });
    let sse_body = format!("data: {}\n\ndata: [DONE]\n\n", delta);
    let m = server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(sse_body)
        .create_async()
        .await;

    let provider = OpenAiCompatibleProvider::new(server.url(), "", "test-model", true).unwrap();
    let msgs = vec![ChatMessage::user("hi")];
    let req = ChatRequest {
        messages: &msgs,
        tools: None,
    };
    let resp = provider.chat(&req).await.unwrap();

    m.assert_async().await;
    assert_eq!(resp.text.as_deref(), Some("hello back"));
    assert!(resp.tool_calls.is_empty());
}

#[tokio::test]
async fn test_error_response() {
    bypass_proxy();
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/chat/completions")
        .with_status(500)
        .with_body("internal error")
        .create_async()
        .await;

    let provider = OpenAiCompatibleProvider::new(server.url(), "", "test-model", true).unwrap();
    let msgs = vec![ChatMessage::user("hi")];
    let req = ChatRequest {
        messages: &msgs,
        tools: None,
    };
    let result = provider.chat(&req).await;

    m.assert_async().await;
    assert!(result.is_err());
}
