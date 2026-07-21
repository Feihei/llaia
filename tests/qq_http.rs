use laia::channels::qq::QqChannel;
use laia::config::QqConfig;
use mockito::Server;

fn test_config() -> QqConfig {
    QqConfig {
        enabled: true,
        app_id: "test_app".into(),
        token: "test_token".into(),
        bot_qq: "10000".into(),
        confirm_mode: "always".into(),
    }
}

#[tokio::test]
async fn test_send_c2c_message_success() {
    let mut server = Server::new_async().await;
    let mock = server
        .mock("POST", "/v2/users/USER123/messages")
        .match_header("authorization", "Bot test_app.test_token")
        .match_header("content-type", "application/json")
        .with_status(200)
        .with_body(r#"{"id":"msg_xxx"}"#)
        .create_async()
        .await;

    let qq = QqChannel::new_with_api_base(test_config(), server.url());
    qq.send_c2c_message("USER123", "hello", Some("msg_id_1"))
        .await
        .unwrap();

    mock.assert();
}

#[tokio::test]
async fn test_send_c2c_message_retries_on_failure() {
    let mut server = Server::new_async().await;
    let mock = server
        .mock("POST", "/v2/users/USER456/messages")
        .with_status(500)
        .with_body("internal error")
        .expect(3)
        .create_async()
        .await;

    let qq = QqChannel::new_with_api_base(test_config(), server.url());
    let result = qq.send_c2c_message("USER456", "hello", None).await;
    assert!(result.is_err());

    mock.assert();
}

#[tokio::test]
async fn test_send_c2c_message_succeeds_on_second_attempt() {
    let mut server = Server::new_async().await;
    // 第一次 500，第二次 200
    let _m1 = server
        .mock("POST", "/v2/users/USER789/messages")
        .with_status(500)
        .with_body("err")
        .create_async()
        .await;
    let m2 = server
        .mock("POST", "/v2/users/USER789/messages")
        .with_status(200)
        .with_body(r#"{"id":"ok"}"#)
        .create_async()
        .await;

    let qq = QqChannel::new_with_api_base(test_config(), server.url());
    qq.send_c2c_message("USER789", "hi", None).await.unwrap();

    m2.assert();
}

#[tokio::test]
async fn test_get_ws_url_extracts_url() {
    let mut server = Server::new_async().await;
    server
        .mock("GET", "/gateway/bot")
        .with_status(200)
        .with_body(r#"{"url":"wss://example.com/ws","shards":1}"#)
        .create_async()
        .await;

    let qq = QqChannel::new_with_api_base(test_config(), server.url());
    let ws_url = qq.get_ws_url().await.unwrap();
    assert_eq!(ws_url, "wss://example.com/ws");
}

#[tokio::test]
async fn test_extract_c2c_text() {
    use serde_json::json;

    // 正常 C2C 文本消息
    let payload = json!({
        "op": 0,
        "s": 0,
        "t": "C2C_MESSAGE_CREATE",
        "d": {
            "id": "msg_abc",
            "author": { "id": "user_openid_xxx" },
            "content": "你好"
        }
    });
    let (user, msg_id, text) = QqChannel::extract_c2c_text(&payload).unwrap();
    assert_eq!(user, "user_openid_xxx");
    assert_eq!(msg_id, "msg_abc");
    assert_eq!(text, "你好");

    // 非 C2C 消息
    let payload = json!({
        "t": "GROUP_AT_MESSAGE_CREATE",
        "d": { "content": "hi" }
    });
    assert!(QqChannel::extract_c2c_text(&payload).is_none());

    // 空内容
    let payload = json!({
        "t": "C2C_MESSAGE_CREATE",
        "d": { "id": "m1", "author": { "id": "u1" }, "content": "  " }
    });
    assert!(QqChannel::extract_c2c_text(&payload).is_none());
}
