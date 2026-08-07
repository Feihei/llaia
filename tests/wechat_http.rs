//! 微信 ClawBot（openclaw-weixin / ilink bot）HTTP 集成测试

use llaia::channels::wechat::{WechatChannel, WechatState};
use llaia::config::WechatConfig;
use mockito::Server;
use std::collections::HashMap;

fn test_config(api_base: &str) -> WechatConfig {
    WechatConfig {
        enabled: true,
        allow_user_id: String::new(),
        base_url: api_base.into(),
        cdn_base_url: format!("{}/cdn", api_base),
    }
}

async fn channel(
    server: &Server,
    state: Option<WechatState>,
) -> (WechatChannel, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let wx = WechatChannel::new(test_config(&server.url()), dir.path().to_path_buf());
    if let Some(st) = state {
        wx.set_state(st).await;
    }
    (wx, dir)
}

/// Windows 系统代理（注册表配置，如 Clash）常不 bypass loopback，
/// reqwest 默认读取系统代理，导致对 mockito 本地 server 的请求被代理截断。
fn bypass_proxy() {
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    std::env::set_var("no_proxy", "127.0.0.1,localhost");
}

fn logged_in_state() -> WechatState {
    WechatState {
        token: "tk_test".into(),
        account_id: "bot_1".into(),
        sync_buf: String::new(),
        context_tokens: HashMap::new(),
    }
}

#[tokio::test]
async fn test_get_qrcode_returns_id_and_image() {
    bypass_proxy();
    let mut server = Server::new_async().await;
    let m = server
        .mock("GET", "/ilink/bot/get_bot_qrcode")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"qrcode":"qr_abc","qrcode_img_content":"aW1nX2Jhc2U2NA=="}"#)
        .create_async()
        .await;

    let (wx, _dir) = channel(&server, None).await;
    let (qrcode, img) = wx.get_qrcode().await.unwrap();
    assert_eq!(qrcode, "qr_abc");
    assert_eq!(img, "aW1nX2Jhc2U2NA==");
    m.assert();
}

#[tokio::test]
async fn test_get_qrcode_malformed_errors() {
    bypass_proxy();
    let mut server = Server::new_async().await;
    server
        .mock("GET", "/ilink/bot/get_bot_qrcode")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_body(r#"{"qrcode":""}"#)
        .create_async()
        .await;

    let (wx, _dir) = channel(&server, None).await;
    assert!(wx.get_qrcode().await.is_err());
}

#[tokio::test]
async fn test_poll_qrcode_status_confirmed() {
    bypass_proxy();
    let mut server = Server::new_async().await;
    server
        .mock("GET", "/ilink/bot/get_qrcode_status")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_body(
            r#"{"status":"confirmed","bot_token":"tk_123","ilink_bot_id":"bot_9","ilink_user_id":"u_1"}"#,
        )
        .create_async()
        .await;

    let (wx, _dir) = channel(&server, None).await;
    let st = wx.poll_qrcode_status("qr_abc").await.unwrap();
    assert_eq!(st["status"], "confirmed");
    assert_eq!(st["bot_token"], "tk_123");
}

#[tokio::test]
async fn test_fetch_updates_requires_token() {
    bypass_proxy();
    let server = Server::new_async().await;
    let (wx, _dir) = channel(&server, None).await;
    let err = wx.fetch_updates().await.unwrap_err();
    assert!(err.to_string().contains("not logged in"));
}

#[tokio::test]
async fn test_fetch_updates_returns_msgs() {
    bypass_proxy();
    let mut server = Server::new_async().await;
    let m = server
        .mock("POST", "/ilink/bot/getupdates")
        .match_header("authorization", "Bearer tk_test")
        .match_header("authorizationtype", "ilink_bot_token")
        .with_status(200)
        .with_body(
            r#"{"ret":0,"errcode":0,"get_updates_buf":"buf_next","msgs":[{"from_user_id":"u1","context_token":"ctx_1","item_list":[{"type":1,"text_item":{"text":"你好"}}]}]}"#,
        )
        .create_async()
        .await;

    let (wx, _dir) = channel(&server, Some(logged_in_state())).await;
    let data = wx.fetch_updates().await.unwrap();
    assert!(WechatChannel::api_ok(&data));
    assert_eq!(data["get_updates_buf"], "buf_next");
    let msg = &data["msgs"][0];
    assert_eq!(WechatChannel::extract_text(msg), "你好");
    m.assert();
}

#[tokio::test]
async fn test_send_text_uses_context_token() {
    bypass_proxy();
    let mut server = Server::new_async().await;
    // serde_json Map 按字母序：context_token 在 to_user_id 前，item_list 在 to_user_id 前
    let m = server
        .mock("POST", "/ilink/bot/sendmessage")
        .match_header("authorization", "Bearer tk_test")
        .match_body(mockito::Matcher::Regex(
            r#""context_token":"ctx_abc".*"text":"回复文本".*"to_user_id":"u1""#.to_string(),
        ))
        .with_status(200)
        .with_body(r#"{"ret":0,"errcode":0}"#)
        .create_async()
        .await;

    let mut st = logged_in_state();
    st.context_tokens.insert("u1".into(), "ctx_abc".into());
    let (wx, _dir) = channel(&server, Some(st)).await;
    wx.send_text("u1", "回复文本").await.unwrap();
    m.assert();
}

#[tokio::test]
async fn test_send_text_without_context_token_fails() {
    bypass_proxy();
    let server = Server::new_async().await;
    let (wx, _dir) = channel(&server, Some(logged_in_state())).await;
    let err = wx.send_text("u_unknown", "hi").await.unwrap_err();
    assert!(err.to_string().contains("context_token"));
}

#[tokio::test]
async fn test_session_timeout_errcode_detected() {
    let payload: serde_json::Value = serde_json::from_str(r#"{"ret":0,"errcode":-14}"#).unwrap();
    assert!(!WechatChannel::api_ok(&payload));
    assert_eq!(WechatChannel::api_errcode(&payload), -14);
}
