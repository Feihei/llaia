//! 钉钉 Stream Mode HTTP 集成测试（mockito mock gateway + sessionWebhook）

use llaia::channels::dingtalk::DingtalkChannel;
use llaia::config::DingtalkConfig;
use mockito::Server;

fn test_config(api_base: &str) -> DingtalkConfig {
    DingtalkConfig {
        enabled: true,
        client_id: "test_client".into(),
        client_secret: "test_secret".into(),
        allow_staff_id: String::new(),
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
async fn test_register_connection_returns_endpoint() {
    bypass_proxy();
    let mut server = Server::new_async().await;
    let m = server
        .mock("POST", "/v1.0/gateway/connections/open")
        .match_body(mockito::Matcher::Regex(
            r#""clientId":"test_client".*"topic":"/v1\.0/im/bot/messages/get""#.to_string(),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"endpoint":"wss://example.com/connect","ticket":"tk_abc"}"#)
        .create_async()
        .await;

    let dt = DingtalkChannel::new(test_config(&server.url()));
    let gw = dt.register_connection().await.unwrap();
    assert_eq!(gw.endpoint, "wss://example.com/connect");
    assert_eq!(gw.ticket, "tk_abc");
    m.assert();
}

#[tokio::test]
async fn test_register_connection_fails_on_error_status() {
    bypass_proxy();
    let mut server = Server::new_async().await;
    server
        .mock("POST", "/v1.0/gateway/connections/open")
        .with_status(401)
        .with_body(r#"{"code":"invalidCredential","message":"bad secret"}"#)
        .create_async()
        .await;

    let dt = DingtalkChannel::new(test_config(&server.url()));
    let err = dt.register_connection().await.unwrap_err();
    assert!(err.to_string().contains("401"));
}

#[tokio::test]
async fn test_send_markdown_posts_to_webhook() {
    bypass_proxy();
    let mut server = Server::new_async().await;
    let m = server
        .mock("POST", "/robot/sendBySession")
        .match_header("content-type", "application/json")
        // serde_json 无 preserve_order 时按字母序：markdown 对象在前，msgtype 在后
        .match_body(mockito::Matcher::Regex(
            r#""text":"回复内容".*"msgtype":"markdown""#.to_string(),
        ))
        .with_status(200)
        .with_body("{}")
        .create_async()
        .await;

    let dt = DingtalkChannel::new(test_config(&server.url()));
    dt.send_markdown(&format!("{}/robot/sendBySession", server.url()), "回复内容")
        .await
        .unwrap();
    m.assert();
}

#[tokio::test]
async fn test_send_markdown_fails_on_error_status() {
    bypass_proxy();
    let mut server = Server::new_async().await;
    server
        .mock("POST", "/robot/sendBySession")
        .with_status(403)
        .with_body("session expired")
        .create_async()
        .await;

    let dt = DingtalkChannel::new(test_config(&server.url()));
    let err = dt
        .send_markdown(&format!("{}/robot/sendBySession", server.url()), "hi")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("403"));
}
