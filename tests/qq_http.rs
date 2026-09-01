use llaia::channels::qq::QqChannel;
use llaia::config::QqConfig;
use mockito::Server;

fn test_config() -> QqConfig {
    QqConfig {
        enabled: true,
        app_id: "test_app".into(),
        app_secret: "test_secret".into(),
        confirm_mode: "always".into(),
        owner_openid: String::new(),
    }
}

/// Windows 系统代理（注册表配置，如 Clash）常不 bypass loopback，
/// reqwest 默认读取系统代理，导致对 mockito 本地 server 的请求被代理截断。
fn bypass_proxy() {
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    std::env::set_var("no_proxy", "127.0.0.1,localhost");
}

/// 在 mockito server 上 mock getAppAccessToken 接口
async fn mock_access_token(server: &mut Server) -> mockito::Mock {
    bypass_proxy();
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

// ---------- 媒体上传（base64 直传 + 分片上传） ----------

use base64::Engine;
use llaia::agent::MediaKind;

fn md5_hex(b: &[u8]) -> String {
    use md5::Digest;
    let mut h = md5::Md5::new();
    h.update(b);
    h.finalize().iter().map(|x| format!("{:02x}", x)).collect()
}

fn sha1_hex(b: &[u8]) -> String {
    use sha1::Digest;
    let mut h = sha1::Sha1::new();
    h.update(b);
    h.finalize().iter().map(|x| format!("{:02x}", x)).collect()
}

/// 文件类（file_type=4）走官方分片上传：prepare → PUT 分片 → part_finish → /files 合并
/// → msg_type=7。回归用户报告：大文件 base64 直传被 QQ 内部代理 500/850012 拒绝。
#[tokio::test]
async fn test_send_file_uses_chunked_upload_flow() {
    let mut server = Server::new_async().await;
    let _token_mock = mock_access_token(&mut server).await;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("report.pptx");
    let content = b"0123456789"; // 10 字节，切成 6+4 两片
    std::fs::write(&file_path, content).unwrap();

    // 1. upload_prepare：校验元信息字段（含三种校验值），返回分片计划
    let prepare_match = serde_json::json!({
        "file_type": 4,
        "file_size": "10",
        "file_name": "report.pptx",
        "md5": md5_hex(content),
        "sha1": sha1_hex(content),
        "md5_10m": md5_hex(content), // 不足 10002432 字节时等于整文件
    })
    .to_string();
    let prepare_mock = server
        .mock("POST", "/v2/users/U1/upload_prepare")
        .match_header("authorization", "QQBot mock_token_abc")
        .match_body(mockito::Matcher::PartialJsonString(prepare_match))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(format!(
            r#"{{"upload_id":"up1","block_size":"6","parts":[
                {{"index":0,"presigned_url":"{}/chunk0","block_size":"6"}},
                {{"index":1,"presigned_url":"{}/chunk1","block_size":"4"}}
            ],"upload_config":{{"concurrency":1,"retry_timeout":300,"retry_delay":1}}}}"#,
            server.url(),
            server.url()
        ))
        .create_async()
        .await;

    // 2. PUT 分片到预签名地址（不带 QQBot token，内容精确匹配）
    let put0 = server
        .mock("PUT", "/chunk0")
        .match_body(mockito::Matcher::Exact("012345".to_string()))
        .with_status(200)
        .create_async()
        .await;
    let put1 = server
        .mock("PUT", "/chunk1")
        .match_body(mockito::Matcher::Exact("6789".to_string()))
        .with_status(200)
        .create_async()
        .await;

    // 3. 每片上传后通知完成（upload_id 关联，共两次）
    let finish_mock = server
        .mock("POST", "/v2/users/U1/upload_part_finish")
        .match_body(mockito::Matcher::PartialJsonString(
            r#"{"upload_id":"up1"}"#.to_string(),
        ))
        .expect(2)
        .with_status(200)
        .with_body("{}")
        .create_async()
        .await;

    // 4. /files 带 upload_id 合并（不带 file_data），返回 file_info
    let merge_mock = server
        .mock("POST", "/v2/users/U1/files")
        .match_body(mockito::Matcher::PartialJsonString(
            r#"{"file_type":4,"file_name":"report.pptx","upload_id":"up1","srv_send_msg":false}"#
                .to_string(),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"file_info":"FI123"}"#)
        .create_async()
        .await;

    // 5. msg_type=7 富媒体消息
    let msg_mock = server
        .mock("POST", "/v2/users/U1/messages")
        .match_body(mockito::Matcher::PartialJsonString(
            r#"{"msg_type":7,"media":{"file_info":"FI123"},"msg_id":"mid1"}"#.to_string(),
        ))
        .with_status(200)
        .with_body(r#"{"id":"m1"}"#)
        .create_async()
        .await;

    let qq = QqChannel::new_with_api_base(test_config(), server.url());
    qq.send_media_to_user(
        "U1",
        file_path.to_str().unwrap(),
        MediaKind::File,
        Some("mid1"),
    )
    .await
    .unwrap();

    prepare_mock.assert();
    put0.assert();
    put1.assert();
    finish_mock.assert();
    merge_mock.assert();
    msg_mock.assert();
}

/// 图片仍走 base64 直传（历史验证路径），且上传 body 必带 file_name。
#[tokio::test]
async fn test_send_image_still_uses_base64_upload() {
    let mut server = Server::new_async().await;
    let _token_mock = mock_access_token(&mut server).await;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("pic.png");
    std::fs::write(&file_path, b"fake png").unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(b"fake png");

    let upload_mock = server
        .mock("POST", "/v2/users/U2/files")
        .match_body(mockito::Matcher::PartialJsonString(
            serde_json::json!({
                "file_type": 1,
                "file_data": b64,
                "file_name": "pic.png",
                "srv_send_msg": false,
            })
            .to_string(),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"file_info":"FI9"}"#)
        .create_async()
        .await;
    let msg_mock = server
        .mock("POST", "/v2/users/U2/messages")
        .match_body(mockito::Matcher::PartialJsonString(
            r#"{"msg_type":7,"media":{"file_info":"FI9"}}"#.to_string(),
        ))
        .with_status(200)
        .with_body(r#"{"id":"m2"}"#)
        .create_async()
        .await;

    let qq = QqChannel::new_with_api_base(test_config(), server.url());
    qq.send_media_to_user("U2", file_path.to_str().unwrap(), MediaKind::Image, None)
        .await
        .unwrap();

    upload_mock.assert();
    msg_mock.assert();
}

/// 图片 base64 上传被拒（如 500/850012）时降级分片上传重试。
#[tokio::test]
async fn test_send_image_falls_back_to_chunked_on_base64_failure() {
    let mut server = Server::new_async().await;
    let _token_mock = mock_access_token(&mut server).await;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("big.png");
    std::fs::write(&file_path, b"imgdata!").unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(b"imgdata!");

    // base64 通道失败（模拟 QQ 内部代理 500）：精确匹配首包 body
    let base64_body = serde_json::json!({
        "file_type": 1,
        "file_data": b64,
        "file_name": "big.png",
        "srv_send_msg": false,
    })
    .to_string();
    let _base64_fail = server
        .mock("POST", "/v2/users/U3/files")
        .match_body(mockito::Matcher::JsonString(base64_body))
        .with_status(500)
        .with_body(r#"{"message":"call inner proxy error","code":850012}"#)
        .create_async()
        .await;

    // 分片通道兜底（单片）
    let prepare_mock = server
        .mock("POST", "/v2/users/U3/upload_prepare")
        .match_body(mockito::Matcher::PartialJsonString(
            r#"{"file_type":1,"file_name":"big.png"}"#.to_string(),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(format!(
            r#"{{"upload_id":"up2","block_size":"8","parts":[
                {{"index":0,"presigned_url":"{}/chunkA","block_size":"8"}}
            ]}}"#,
            server.url()
        ))
        .create_async()
        .await;
    let put_a = server
        .mock("PUT", "/chunkA")
        .match_body(mockito::Matcher::Exact("imgdata!".to_string()))
        .with_status(200)
        .create_async()
        .await;
    let finish_mock = server
        .mock("POST", "/v2/users/U3/upload_part_finish")
        .with_status(200)
        .with_body("{}")
        .create_async()
        .await;
    let merge_mock = server
        .mock("POST", "/v2/users/U3/files")
        .match_body(mockito::Matcher::PartialJsonString(
            r#"{"upload_id":"up2","srv_send_msg":false}"#.to_string(),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"file_info":"FIFB"}"#)
        .create_async()
        .await;
    let msg_mock = server
        .mock("POST", "/v2/users/U3/messages")
        .with_status(200)
        .with_body(r#"{"id":"m3"}"#)
        .create_async()
        .await;

    let qq = QqChannel::new_with_api_base(test_config(), server.url());
    qq.send_media_to_user("U3", file_path.to_str().unwrap(), MediaKind::Image, None)
        .await
        .unwrap();

    prepare_mock.assert();
    put_a.assert();
    finish_mock.assert();
    merge_mock.assert();
    msg_mock.assert();
}
