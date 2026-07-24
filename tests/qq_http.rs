use laia::channels::qq::QqChannel;
use laia::config::QqConfig;
use mockito::Server;

fn test_config() -> QqConfig {
    QqConfig {
        enabled: true,
        app_id: "test_app".into(),
        app_secret: "test_secret".into(),
        confirm_mode: "always".into(),
    }
}

/// 在 mockito server 上 mock getAppAccessToken 接口
async fn mock_access_token(server: &mut Server) -> mockito::Mock {
    server
        .mock("POST", "/app/getAppAccessToken")
        .match_body(mockito::Matcher::JsonString(
            r#"{"appId":"test_app","clientSecret":"test_secret"}"#.to_string(),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"access_token":"mock_token_abc","expires_in":7200}"#)
        .create_async()
        .await
}

#[tokio::test]
async fn test_get_access_token_fetches_and_caches() {
    let mut server = Server::new_async().await;
    let m = mock_access_token(&mut server).await;

    let qq = QqChannel::new_with_api_base(test_config(), server.url());
    // 第一次：应该请求 token
    let t1 = qq.get_access_token().await.unwrap();
    assert_eq!(t1, "mock_token_abc");

    // 第二次：应该命中缓存，不再次请求
    let t2 = qq.get_access_token().await.unwrap();
    assert_eq!(t2, "mock_token_abc");

    // 只调了一次 getAppAccessToken
    m.assert();
}

#[tokio::test]
async fn test_send_c2c_message_success() {
    let mut server = Server::new_async().await;
    let _token_mock = mock_access_token(&mut server).await;
    let send_mock = server
        .mock("POST", "/v2/users/USER123/messages")
        .match_header("authorization", "QQBot mock_token_abc")
        .match_header("content-type", "application/json")
        .with_status(200)
        .with_body(r#"{"id":"msg_xxx"}"#)
        .create_async()
        .await;

    let qq = QqChannel::new_with_api_base(test_config(), server.url());
    qq.send_c2c_message("USER123", "hello", Some("msg_id_1"))
        .await
        .unwrap();

    send_mock.assert();
}

#[tokio::test]
async fn test_send_c2c_message_retries_on_failure() {
    let mut server = Server::new_async().await;
    let _token_mock = mock_access_token(&mut server).await;
    let send_mock = server
        .mock("POST", "/v2/users/USER456/messages")
        .with_status(500)
        .with_body("internal error")
        .expect(3)
        .create_async()
        .await;

    let qq = QqChannel::new_with_api_base(test_config(), server.url());
    let result = qq.send_c2c_message("USER456", "hello", None).await;
    assert!(result.is_err());

    send_mock.assert();
}

#[tokio::test]
async fn test_send_c2c_message_succeeds_on_second_attempt() {
    let mut server = Server::new_async().await;
    let _token_mock = mock_access_token(&mut server).await;
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
    let _token_mock = mock_access_token(&mut server).await;
    server
        .mock("GET", "/gateway/bot")
        .match_header("authorization", "QQBot mock_token_abc")
        .with_status(200)
        .with_body(r#"{"url":"wss://example.com/ws","shards":1}"#)
        .create_async()
        .await;

    let qq = QqChannel::new_with_api_base(test_config(), server.url());
    let ws_url = qq.get_ws_url().await.unwrap();
    assert_eq!(ws_url, "wss://example.com/ws");
}

#[tokio::test]
async fn test_extract_c2c_message() {
    use serde_json::json;

    // 正常私域 C2C 文本消息
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
    let m = QqChannel::extract_c2c_message(&payload).unwrap();
    assert_eq!(m.user_id, "user_openid_xxx");
    assert_eq!(m.msg_id, "msg_abc");
    assert_eq!(m.text, "你好");
    assert!(m.attachments.is_empty());

    // 公域 C2C 消息也应识别
    let payload = json!({
        "t": "PUBLIC_C2C_MESSAGE_CREATE",
        "d": {
            "id": "msg_def",
            "author": { "id": "u2" },
            "content": "hi"
        }
    });
    let m = QqChannel::extract_c2c_message(&payload).unwrap();
    assert_eq!(m.text, "hi");

    // 非 C2C 消息
    let payload = json!({
        "t": "GROUP_AT_MESSAGE_CREATE",
        "d": { "content": "hi" }
    });
    assert!(QqChannel::extract_c2c_message(&payload).is_none());

    // 空内容且无附件
    let payload = json!({
        "t": "C2C_MESSAGE_CREATE",
        "d": { "id": "m1", "author": { "id": "u1" }, "content": "  " }
    });
    assert!(QqChannel::extract_c2c_message(&payload).is_none());

    // 带附件的消息（图片）
    let payload = json!({
        "t": "C2C_MESSAGE_CREATE",
        "d": {
            "id": "msg_img",
            "author": { "id": "u3" },
            "content": "看看这张图",
            "attachments": [
                {
                    "content_type": "image/png",
                    "filename": "pic.png",
                    "url": "https://api.sgroup.qq.com/files/pic"
                }
            ]
        }
    });
    let m = QqChannel::extract_c2c_message(&payload).unwrap();
    assert_eq!(m.text, "看看这张图");
    assert_eq!(m.attachments.len(), 1);
    assert!(m.attachments[0].is_image());
    assert_eq!(m.attachments[0].filename, "pic.png");

    // 仅附件无文本也应识别
    let payload = json!({
        "t": "C2C_MESSAGE_CREATE",
        "d": {
            "id": "msg_file_only",
            "author": { "id": "u4" },
            "content": "",
            "attachments": [
                {
                    "content_type": "application/pdf",
                    "filename": "doc.pdf",
                    "url": "https://api.sgroup.qq.com/files/doc"
                }
            ]
        }
    });
    let m = QqChannel::extract_c2c_message(&payload).unwrap();
    assert_eq!(m.text, "");
    assert_eq!(m.attachments.len(), 1);
    assert!(!m.attachments[0].is_image());
}
