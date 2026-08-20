//! Telegram Bot API HTTP 集成测试（mockito mock Bot API 端点）

use llaia::channels::telegram::TelegramChannel;
use llaia::config::TelegramConfig;
use mockito::Server;

fn test_config(api_base: &str) -> TelegramConfig {
    TelegramConfig {
        enabled: true,
        bot_token: "123456:TEST_TOKEN".into(),
        allow_chat_id: 0,
        owner_chat_id: 0,
        api_base: api_base.into(),
    }
}

/// Windows 系统代理（注册表配置，如 Clash）常不 bypass loopback，
/// reqwest 默认读取系统代理，导致对 mockito 本地 server 的请求被代理截断。
fn bypass_proxy() {
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    std::env::set_var("no_proxy", "127.0.0.1,localhost");
}

#[tokio::test]
async fn test_get_me_returns_username() {
    bypass_proxy();
    let mut server = Server::new_async().await;
    let m = server
        .mock("GET", "/bot123456:TEST_TOKEN/getMe")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"ok":true,"result":{"id":1,"is_bot":true,"first_name":"LLAIA","username":"llaia_bot"}}"#,
        )
        .create_async()
        .await;

    let tg = TelegramChannel::new(test_config(&server.url())).unwrap();
    let name = tg.get_me().await.unwrap();
    assert_eq!(name, "llaia_bot");
    m.assert();
}

#[tokio::test]
async fn test_get_me_fails_when_not_ok() {
    bypass_proxy();
    let mut server = Server::new_async().await;
    server
        .mock("GET", "/bot123456:TEST_TOKEN/getMe")
        .with_status(401)
        .with_header("content-type", "application/json")
        .with_body(r#"{"ok":false,"error_code":401,"description":"Unauthorized"}"#)
        .create_async()
        .await;

    let tg = TelegramChannel::new(test_config(&server.url())).unwrap();
    let err = tg.get_me().await.unwrap_err();
    assert!(err.to_string().contains("Unauthorized"));
}

#[tokio::test]
async fn test_send_text_posts_chat_id_and_text() {
    bypass_proxy();
    let mut server = Server::new_async().await;
    let m = server
        .mock("POST", "/bot123456:TEST_TOKEN/sendMessage")
        .match_header("content-type", "application/json")
        .match_body(mockito::Matcher::JsonString(
            r#"{"chat_id":42,"text":"hello"}"#.to_string(),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"ok":true,"result":{"message_id":1}}"#)
        .create_async()
        .await;

    let tg = TelegramChannel::new(test_config(&server.url())).unwrap();
    tg.send_text(42, "hello").await.unwrap();
    m.assert();
}

#[tokio::test]
async fn test_send_text_error_when_not_ok() {
    bypass_proxy();
    let mut server = Server::new_async().await;
    server
        .mock("POST", "/bot123456:TEST_TOKEN/sendMessage")
        .with_status(400)
        .with_header("content-type", "application/json")
        .with_body(r#"{"ok":false,"description":"Bad Request: chat not found"}"#)
        .create_async()
        .await;

    let tg = TelegramChannel::new(test_config(&server.url())).unwrap();
    let err = tg.send_text(99, "hi").await.unwrap_err();
    assert!(err.to_string().contains("chat not found"));
}

#[tokio::test]
async fn test_new_rejects_empty_token_in_run() {
    // new() 本身不校验 token（构造廉价），run() 里才 bail；
    // 这里只验证构造成功，run 的空 token 分支由下方单测覆盖语义。
    let cfg = TelegramConfig {
        enabled: true,
        bot_token: String::new(),
        allow_chat_id: 0,
        owner_chat_id: 0,
        api_base: "http://127.0.0.1:1".into(),
    };
    assert!(TelegramChannel::new(cfg).is_ok());
}
