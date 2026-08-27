//! 微信 ClawBot channel：腾讯官方 openclaw-weixin（ilink bot）接口。
//!
//! 协议（参考 AstrBot weixin_oc 实现，Apache/AGPL 行为参考）：
//! 1. 登录：`GET ilink/bot/get_bot_qrcode?bot_type=3` → 轮询 `get_qrcode_status`
//!    → `status=confirmed` 拿 `bot_token`（手机微信 ClawBot 插件扫码）
//! 2. 收消息：`POST ilink/bot/getupdates`（`get_updates_buf` 增量游标长轮询，免公网）
//! 3. 发消息：`POST ilink/bot/sendmessage`（需对方最近消息带来的 `context_token`）
//! 4. 媒体：`getuploadurl` + CDN（AES-128-ECB + PKCS7），响应头 `x-encrypted-param` 为下载凭据
//! 5. 登录态（token / sync_buf / context_tokens）持久化 `<config_dir>/wechat_state.json`
//!
//! v1 范围：文本收发 + 语音转文字接收 + 图片/文件发送；媒体接收仅文本占位（v2 再补下载解密）。

use crate::agent::sink::{run_turn, OutputSink};
use crate::agent::{AgentRegistry, MediaKind};
use crate::channels::qq::split_reply;
use crate::channels::Channel;
use crate::config::WechatConfig;
use aes::cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};
use aes::Aes128;
use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};

/// getupdates / get_qrcode_status 长轮询服务端超时（秒）
const LONG_POLL_SECS: u64 = 35;
/// errcode -14：登录态失效，需重新扫码
const SESSION_TIMEOUT_ERRCODE: i64 = -14;
/// 单条文本回复切分上限（与 QQ 一致；微信 sendmessage 对超长会以 ret=-2 "prepare failed" 拒绝）
const MAX_TEXT_LEN: usize = 1800;
/// item_list 类型
const ITEM_TEXT: i64 = 1;
const ITEM_IMAGE: i64 = 2;
const ITEM_FILE: i64 = 4;
/// getuploadurl media_type
const UPLOAD_IMAGE: i64 = 1;
const UPLOAD_FILE: i64 = 3;

/// 登录态 + 会话游标，独立于 config.toml 持久化
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WechatState {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub account_id: String,
    #[serde(default)]
    pub sync_buf: String,
    /// ilink_user_id -> 最近一条来信的 context_token（回复凭据）
    #[serde(default)]
    pub context_tokens: HashMap<String, String>,
    /// 自动捕获的 owner user_id（用于 cron 主动推送，跨重启持久化）；
    /// 仅在 config.owner_user_id 为空时写入
    #[serde(default)]
    pub owner_user_id: String,
}

pub struct WechatChannel {
    config: WechatConfig,
    state_dir: PathBuf,
    http: Client,
    state: Mutex<WechatState>,
}

impl WechatChannel {
    pub fn new(config: WechatConfig, state_dir: PathBuf) -> Self {
        Self {
            config,
            state_dir,
            // 长轮询 35s + 网络余量
            http: Client::builder()
                .timeout(Duration::from_secs(LONG_POLL_SECS + 20))
                .connect_timeout(Duration::from_secs(15))
                .build()
                .expect("build wechat http client cannot fail with static config"),
            state: Mutex::new(WechatState::default()),
        }
    }

    fn state_path(&self) -> PathBuf {
        self.state_dir.join("wechat_state.json")
    }

    async fn load_state(&self) {
        let path = self.state_path();
        // 首次启动无状态文件，读失败直接跳过
        if let Ok(s) = tokio::fs::read_to_string(&path).await {
            match serde_json::from_str::<WechatState>(&s) {
                Ok(st) => *self.state.lock().await = st,
                Err(e) => {
                    tracing::warn!(error = %e, "wechat_state.json parse failed, starting fresh")
                }
            }
        }
    }

    async fn save_state(&self) {
        let st = self.state.lock().await.clone();
        let json = serde_json::to_string_pretty(&st).unwrap_or_default();
        if let Err(e) = tokio::fs::write(self.state_path(), json).await {
            tracing::error!(error = %e, "save wechat_state.json failed");
        }
    }

    /// 覆盖当前状态（测试注入 / WebUI 登录流程用）
    pub async fn set_state(&self, st: WechatState) {
        *self.state.lock().await = st;
    }

    /// 当前状态快照
    pub async fn state_snapshot(&self) -> WechatState {
        self.state.lock().await.clone()
    }

    /// 统一 HTTP JSON 请求（带 ilink 特征头）
    async fn request_json(
        &self,
        method: reqwest::Method,
        endpoint: &str,
        query: &[(&str, &str)],
        payload: Option<serde_json::Value>,
        token_required: bool,
    ) -> Result<serde_json::Value> {
        let url = format!(
            "{}/{}",
            self.config.base_url.trim_end_matches('/'),
            endpoint.trim_start_matches('/')
        );
        let uin = base64::engine::general_purpose::STANDARD
            .encode(rand::random::<u32>().to_string().as_bytes());
        let mut req = self
            .http
            .request(method.clone(), &url)
            .query(query)
            .header("Content-Type", "application/json")
            .header("AuthorizationType", "ilink_bot_token")
            .header("X-WECHAT-UIN", uin);
        if token_required {
            let token = self.state.lock().await.token.clone();
            if token.is_empty() {
                anyhow::bail!("{} requires token but not logged in", endpoint);
            }
            req = req.header("Authorization", format!("Bearer {}", token));
        }
        if let Some(body) = payload {
            req = req.json(&body);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("{} {} request", method, endpoint))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("{} failed: {} {}", endpoint, status, text);
        }
        if text.trim().is_empty() {
            return Ok(serde_json::Value::Object(Default::default()));
        }
        serde_json::from_str(&text).with_context(|| format!("{} response parse", endpoint))
    }

    /// API 层成功判定：ret == 0 且 errcode == 0
    pub fn api_ok(payload: &serde_json::Value) -> bool {
        let ret = payload.get("ret").and_then(|v| v.as_i64()).unwrap_or(0);
        let errcode = payload.get("errcode").and_then(|v| v.as_i64()).unwrap_or(0);
        ret == 0 && errcode == 0
    }

    pub fn api_errcode(payload: &serde_json::Value) -> i64 {
        payload.get("errcode").and_then(|v| v.as_i64()).unwrap_or(0)
    }

    // ---- 登录流程 ----

    /// 申请登录二维码，返回 (qrcode id, 二维码图片内容)
    pub async fn get_qrcode(&self) -> Result<(String, String)> {
        let qr = self
            .request_json(
                reqwest::Method::GET,
                "ilink/bot/get_bot_qrcode",
                &[("bot_type", "3")],
                None,
                false,
            )
            .await?;
        let qrcode = qr
            .get("qrcode")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let img = qr
            .get("qrcode_img_content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if qrcode.is_empty() || img.is_empty() {
            anyhow::bail!("get_bot_qrcode response malformed: {}", qr);
        }
        Ok((qrcode, img))
    }

    /// 轮询扫码状态（服务端长轮询），返回原始响应 JSON
    pub async fn poll_qrcode_status(&self, qrcode: &str) -> Result<serde_json::Value> {
        self.request_json(
            reqwest::Method::GET,
            "ilink/bot/get_qrcode_status",
            &[("qrcode", qrcode)],
            None,
            false,
        )
        .await
    }

    /// 拉取一轮增量消息（长轮询），返回原始响应 JSON
    pub async fn fetch_updates(&self) -> Result<serde_json::Value> {
        let sync_buf = self.state.lock().await.sync_buf.clone();
        let payload = serde_json::json!({
            "base_info": { "channel_version": "llaia" },
            "get_updates_buf": sync_buf,
        });
        self.request_json(
            reqwest::Method::POST,
            "ilink/bot/getupdates",
            &[],
            Some(payload),
            true,
        )
        .await
    }

    /// 确保已登录（有 token 直接过；否则走扫码流程直到 confirmed）
    async fn ensure_login(&self) -> Result<()> {
        if !self.state.lock().await.token.is_empty() {
            return Ok(());
        }
        loop {
            let (qrcode, img) = self.get_qrcode().await?;
            self.present_qrcode(&qrcode, &img).await;

            // 轮询扫码状态直到 confirmed / expired / denied
            loop {
                let st = self.poll_qrcode_status(&qrcode).await?;
                let status = st.get("status").and_then(|v| v.as_str()).unwrap_or("wait");
                match status {
                    "confirmed" => {
                        let token = st
                            .get("bot_token")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if token.is_empty() {
                            anyhow::bail!("login confirmed but bot_token missing");
                        }
                        let mut state = self.state.lock().await;
                        state.token = token;
                        state.account_id = st
                            .get("ilink_bot_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        drop(state);
                        self.save_state().await;
                        tracing::info!("WeChat ClawBot login confirmed");
                        return Ok(());
                    }
                    "expired" => {
                        tracing::warn!("weixin qrcode expired, requesting new one");
                        break;
                    }
                    "cancel" | "canceled" | "denied" => {
                        tracing::warn!("weixin login denied/canceled, retrying");
                        break;
                    }
                    _ => {} // wait / scanned 等，继续长轮询
                }
            }
        }
    }

    /// 展示二维码：日志打印扫码 URL + 尝试把图片内容落盘供扫描
    async fn present_qrcode(&self, qrcode: &str, img_content: &str) {
        tracing::info!(
            qrcode = %qrcode,
            "WeChat ClawBot login: scan the QR code with WeChat (with the ClawBot plugin)"
        );
        // qrcode_img_content 可能是 data URL 或裸 base64
        let b64 = img_content
            .split_once(";base64,")
            .map(|(_, d)| d)
            .unwrap_or(img_content);
        match base64::engine::general_purpose::STANDARD.decode(b64.trim()) {
            Ok(bytes) => {
                let path = self.state_dir.join("wechat_qr.png");
                match tokio::fs::write(&path, &bytes).await {
                    Ok(()) => {
                        tracing::info!(path = %path.display(), "QR code image saved, please scan")
                    }
                    Err(e) => tracing::warn!(error = %e, "save qrcode image failed"),
                }
            }
            Err(_) => tracing::info!(img = %img_content, "qrcode_img_content output as-is"),
        }
    }

    // ---- 收消息 ----

    /// 长轮询一轮。返回 Err 表示需要重建登录态（session 超时）或发生错误。
    async fn poll_once(
        self: &Arc<Self>,
        agent: &Arc<Mutex<crate::agent::Agent>>,
        stop: &Arc<Notify>,
        registry: &Arc<AgentRegistry>,
    ) -> Result<()> {
        let data = self.fetch_updates().await?;
        if !Self::api_ok(&data) {
            if Self::api_errcode(&data) == SESSION_TIMEOUT_ERRCODE {
                anyhow::bail!("session timeout (errcode -14), re-login required");
            }
            anyhow::bail!("getupdates error: {}", data);
        }
        let mut need_save = false;
        if let Some(buf) = data.get("get_updates_buf").and_then(|v| v.as_str()) {
            if !buf.is_empty() {
                self.state.lock().await.sync_buf = buf.to_string();
                need_save = true;
            }
        }
        let msgs = data
            .get("msgs")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for msg in &msgs {
            if let Err(e) = self
                .handle_message(msg, agent, stop, registry.clone())
                .await
            {
                tracing::error!(error = %e, "handle wechat message failed");
            }
        }
        if need_save {
            self.save_state().await;
        }
        Ok(())
    }

    /// 提取消息文本：文本 item + 语音转文字；媒体 item 占位
    pub fn extract_text(msg: &serde_json::Value) -> String {
        let items = msg
            .get("item_list")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut parts = Vec::new();
        for item in &items {
            let item_type = item.get("type").and_then(|v| v.as_i64()).unwrap_or(0);
            match item_type {
                1 => {
                    if let Some(t) = item
                        .get("text_item")
                        .and_then(|t| t.get("text"))
                        .and_then(|t| t.as_str())
                    {
                        let t = t.trim();
                        if !t.is_empty() {
                            parts.push(t.to_string());
                        }
                    }
                }
                3 => {
                    // 语音：微信云端转录文本
                    if let Some(t) = item
                        .get("voice_item")
                        .and_then(|t| t.get("text"))
                        .and_then(|t| t.as_str())
                    {
                        let t = t.trim();
                        if !t.is_empty() {
                            parts.push(t.to_string());
                        }
                    }
                }
                2 => parts.push("[图片]".into()),
                4 => parts.push("[文件]".into()),
                5 => parts.push("[视频]".into()),
                _ => {}
            }
        }
        parts.join("\n")
    }

    async fn handle_message(
        self: &Arc<Self>,
        msg: &serde_json::Value,
        agent: &Arc<Mutex<crate::agent::Agent>>,
        stop: &Arc<Notify>,
        registry: Arc<AgentRegistry>,
    ) -> Result<()> {
        let from_user_id = msg
            .get("from_user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if from_user_id.is_empty() {
            return Ok(());
        }
        // 更新回复凭据 context_token
        if let Some(ct) = msg.get("context_token").and_then(|v| v.as_str()) {
            if !ct.is_empty() {
                let mut state = self.state.lock().await;
                let changed = state
                    .context_tokens
                    .get(&from_user_id)
                    .map(|old| old != ct)
                    .unwrap_or(true);
                if changed {
                    state
                        .context_tokens
                        .insert(from_user_id.clone(), ct.to_string());
                }
                // 自动捕获 owner user_id（cron 主动推送目标）；config 手动指定时不覆盖
                let owner_changed =
                    self.config.owner_user_id.is_empty() && state.owner_user_id != from_user_id;
                if owner_changed {
                    state.owner_user_id = from_user_id.clone();
                }
                if changed || owner_changed {
                    drop(state);
                    self.save_state().await;
                }
            }
        }
        // 单用户安全锁
        if !self.config.allow_user_id.is_empty() && from_user_id != self.config.allow_user_id {
            tracing::debug!(user = %from_user_id, "message from non-allowed user, ignored");
            return Ok(());
        }
        let text = Self::extract_text(msg);
        let text = text.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }
        tracing::info!(user = %from_user_id, len = text.len(), "wechat message in");

        // 斜杠命令
        if text.starts_with('/') {
            if text.trim().eq_ignore_ascii_case("/stop") {
                stop.notify_waiters();
                let _ = self.send_text(&from_user_id, "[stop signal sent]").await;
                return Ok(());
            }
            let outcome = {
                let mut a = agent.lock().await;
                crate::commands::slash::try_handle(&text, &mut a, Some(registry.clone())).await?
            };
            match outcome {
                crate::commands::slash::SlashOutcome::Exit => {
                    let _ = self
                        .send_text(&from_user_id, "[/exit not available on WeChat channel]")
                        .await;
                }
                crate::commands::slash::SlashOutcome::Handled(m) => {
                    let _ = self.send_text(&from_user_id, &m).await;
                }
                crate::commands::slash::SlashOutcome::NotSlash => {}
                crate::commands::slash::SlashOutcome::Resume { notice, message } => {
                    let _ = self.send_text(&from_user_id, &notice).await;
                    let sink = WechatSink {
                        wx: Arc::clone(self),
                        user_id: from_user_id.clone(),
                        buffer: String::new(),
                        tool_names: Vec::new(),
                        notified_tools: false,
                    };
                    registry.set_delivery(
                        self.clone()
                            .pusher()
                            .map(crate::tools::delegate::DeliveryTarget::Pusher),
                    );
                    let _ = run_turn(
                        agent.clone(),
                        crate::provider::ChatMessage::user(&message),
                        "wechat".into(),
                        Box::new(sink),
                        stop.clone(),
                    )
                    .await;
                }
            }
            return Ok(());
        }

        // 普通消息：跑一轮 agent turn
        let sink = WechatSink {
            wx: Arc::clone(self),
            user_id: from_user_id,
            buffer: String::new(),
            tool_names: Vec::new(),
            notified_tools: false,
        };
        registry.set_delivery(
            self.clone()
                .pusher()
                .map(crate::tools::delegate::DeliveryTarget::Pusher),
        );
        run_turn(
            agent.clone(),
            crate::provider::ChatMessage::user(text),
            "wechat".into(),
            Box::new(sink),
            stop.clone(),
        )
        .await
    }

    // ---- 发消息 ----

    /// 发送 item_list（内部方法，需 context_token）
    pub async fn send_items(&self, user_id: &str, item_list: serde_json::Value) -> Result<()> {
        let context_token = {
            let state = self.state.lock().await;
            state.context_tokens.get(user_id).cloned()
        };
        let Some(context_token) = context_token else {
            anyhow::bail!(
                "no context_token for user {}, the other party must send a message first to establish the session",
                user_id
            );
        };
        let payload = serde_json::json!({
            "base_info": { "channel_version": "llaia" },
            "msg": {
                "from_user_id": "",
                "to_user_id": user_id,
                "client_id": uuid::Uuid::new_v4().simple().to_string(),
                "message_type": 2,
                "message_state": 2,
                "context_token": context_token,
                "item_list": item_list,
            },
        });
        let resp = self
            .request_json(
                reqwest::Method::POST,
                "ilink/bot/sendmessage",
                &[],
                Some(payload),
                true,
            )
            .await?;
        if !Self::api_ok(&resp) {
            anyhow::bail!("sendmessage failed: {}", resp);
        }
        Ok(())
    }

    pub async fn send_text(&self, user_id: &str, text: &str) -> Result<()> {
        let items = serde_json::json!([{
            "type": ITEM_TEXT,
            "text_item": { "text": text },
        }]);
        self.send_items(user_id, items).await
    }

    /// 主动推送消息：用于 cron 任务结果推送。
    /// 目标 user_id：① config `owner_user_id`（手动指定）② state.owner_user_id（自动捕获）。
    /// 都没有则 log + 返回 Ok（不报错，cron 不因此失败）。
    pub async fn send_proactive(&self, message: &str) -> Result<()> {
        let target = {
            let state = self.state.lock().await;
            proactive_user_id(&self.config, &state)
        };
        match target {
            Some(user_id) => self.send_text(&user_id, message).await,
            None => {
                tracing::warn!(
                    "cron push to wechat skipped: no owner user_id (set [channels.wechat] owner_user_id, or wait for an inbound message)"
                );
                Ok(())
            }
        }
    }

    /// 发送媒体：getuploadurl → AES-128-ECB 加密 → CDN 上传 → sendmessage
    pub async fn send_media(&self, user_id: &str, path: &str, kind: MediaKind) -> Result<()> {
        let file_path = std::path::Path::new(path);
        let file_name = file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "upload".into());
        let raw = tokio::fs::read(file_path)
            .await
            .with_context(|| format!("read media file: {}", path))?;
        let is_image = matches!(kind, MediaKind::Image);

        let file_key = uuid::Uuid::new_v4().simple().to_string();
        let aes_key: [u8; 16] = rand::random();
        let aes_key_hex = hex_of(&aes_key);
        let encrypted = aes_ecb_encrypt(&aes_key, &raw);
        let raw_md5 = {
            use md5::Digest;
            let mut h = md5::Md5::new();
            h.update(&raw);
            hex_of(&h.finalize())
        };

        // 1. 申请上传凭据
        let up = self
            .request_json(
                reqwest::Method::POST,
                "ilink/bot/getuploadurl",
                &[],
                Some(serde_json::json!({
                    "filekey": file_key,
                    "media_type": if is_image { UPLOAD_IMAGE } else { UPLOAD_FILE },
                    "to_user_id": user_id,
                    "rawsize": raw.len(),
                    "rawfilemd5": raw_md5,
                    "filesize": encrypted.len(),
                    "no_need_thumb": true,
                    "aeskey": aes_key_hex,
                    "base_info": { "channel_version": "llaia" },
                })),
                true,
            )
            .await?;
        if !Self::api_ok(&up) {
            anyhow::bail!("getuploadurl failed: {}", up);
        }
        let upload_param = up
            .get("upload_param")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let upload_full_url = up
            .get("upload_full_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cdn_url = if !upload_full_url.is_empty() {
            upload_full_url
        } else if !upload_param.is_empty() {
            format!(
                "{}/upload?encrypted_query_param={}&filekey={}",
                self.config.cdn_base_url.trim_end_matches('/'),
                urlencode(&upload_param),
                urlencode(&file_key)
            )
        } else {
            anyhow::bail!("getuploadurl returned neither upload_full_url nor upload_param");
        };

        // 2. CDN 上传密文
        let encrypted_len = encrypted.len();
        let resp = self
            .http
            .post(&cdn_url)
            .header("Content-Type", "application/octet-stream")
            .body(encrypted)
            .send()
            .await
            .context("cdn upload request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            anyhow::bail!("cdn upload failed: {} {}", status, detail);
        }
        let download_param = resp
            .headers()
            .get("x-encrypted-param")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if download_param.is_empty() {
            anyhow::bail!("cdn upload missing x-encrypted-param header");
        }

        // 3. 组装媒体 item 并发送
        let aes_key_b64 = base64::engine::general_purpose::STANDARD.encode(aes_key_hex.as_bytes());
        let media = serde_json::json!({
            "encrypt_query_param": download_param,
            "aes_key": aes_key_b64,
            "encrypt_type": 1,
        });
        let item = if is_image {
            serde_json::json!({
                "type": ITEM_IMAGE,
                "image_item": { "media": media, "mid_size": encrypted_len },
            })
        } else {
            serde_json::json!({
                "type": ITEM_FILE,
                "file_item": {
                    "media": media,
                    "file_name": file_name,
                    "len": raw.len().to_string(),
                },
            })
        };
        self.send_items(user_id, serde_json::json!([item])).await
    }
}

#[async_trait]
impl crate::channels::Channel for WechatChannel {
    async fn run(self: Arc<Self>, registry: Arc<AgentRegistry>) -> Result<()> {
        let agent = registry.main.clone();
        self.load_state().await;
        let stop = Arc::new(Notify::new());
        loop {
            if let Err(e) = self.ensure_login().await {
                tracing::warn!(error = %e, "weixin login failed, retry in 30s");
                tokio::time::sleep(Duration::from_secs(30)).await;
                continue;
            }
            match self.poll_once(&agent, &stop, &registry).await {
                Ok(()) => {}
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("re-login") {
                        // 登录态失效：清空重新扫码
                        tracing::warn!("weixin session timeout, clearing state for re-login");
                        *self.state.lock().await = WechatState::default();
                        self.save_state().await;
                    } else {
                        tracing::warn!(error = %e, "weixin getupdates failed, retry in 5s");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        }
    }
}

/// 输出汇聚：累积文本 + 工具名，done 后一次性分片发送。
/// 工具通知收敛为单条：和 QQ 保持一致，紧凑发送避免刷屏/拆分。
struct WechatSink {
    wx: Arc<WechatChannel>,
    user_id: String,
    buffer: String,
    /// 本回合已调用的工具名（按序去重）
    tool_names: Vec<String>,
    /// 是否已发过工具通知（每回合最多一条）
    notified_tools: bool,
}

#[async_trait]
impl OutputSink for WechatSink {
    async fn on_chunk(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }

    async fn on_tool_start(&mut self, name: &str) {
        if !self.tool_names.iter().any(|n| n == name) {
            self.tool_names.push(name.to_string());
        }
        if !self.notified_tools {
            self.notified_tools = true;
            let _ = self
                .wx
                .send_text(&self.user_id, "🔧 calling tools...")
                .await;
        }
    }

    async fn on_media(&mut self, path: &str, kind: MediaKind) {
        if let Err(e) = self.wx.send_media(&self.user_id, path, kind).await {
            tracing::error!(error = %e, path, "wechat send media failed");
            let _ = self
                .wx
                .send_text(&self.user_id, &format!("[failed to send media: {}]", e))
                .await;
        }
    }

    async fn on_done(&mut self) {
        let body = if self.buffer.trim().is_empty() {
            "[done (no text output)]".to_string()
        } else {
            self.buffer.trim_start_matches(['\n', '\r']).to_string()
        };
        // 调用过工具时把清单拼在回复开头，同一条消息反馈（不额外发消息）
        let reply = if self.tool_names.is_empty() {
            body
        } else {
            format!("🔧 called: {}\n\n{}", self.tool_names.join(", "), body)
        };
        for chunk in split_reply(&reply, MAX_TEXT_LEN) {
            if let Err(e) = self.wx.send_text(&self.user_id, &chunk).await {
                // 微信对单条超长消息会以 ret=-2 prepare failed 拒绝；MAX_TEXT_LEN 已足够短，
                // 这里顺带提示（不再把错误当致命，避免刷屏）
                if tracing::enabled!(tracing::Level::DEBUG) {
                    tracing::debug!(error = %e, chunk_len = chunk.len(), "wechat send reply failed");
                }
            }
        }
    }

    async fn on_error(&mut self, message: &str) {
        if !self.buffer.trim().is_empty() {
            let _ = self.wx.send_text(&self.user_id, self.buffer.trim()).await;
        }
        let _ = self
            .wx
            .send_text(&self.user_id, &format!("[error: {}]", message))
            .await;
    }

    async fn on_interrupted(&mut self) {
        let mut text = self.buffer.clone();
        if !text.trim().is_empty() {
            text.push_str("\n\n");
        }
        text.push_str("[interrupted]");
        let _ = self.wx.send_text(&self.user_id, &text).await;
    }
}

// ---- 加密/编码工具 ----

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// 极简 URL 编码（CDN query 参数只需转义保留字符）
fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

pub fn pkcs7_pad(data: &[u8], block_size: usize) -> Vec<u8> {
    let pad_len = block_size - (data.len() % block_size);
    let mut out = data.to_vec();
    out.extend(std::iter::repeat_n(pad_len as u8, pad_len));
    out
}

pub fn pkcs7_unpad(data: &[u8], block_size: usize) -> Vec<u8> {
    if data.is_empty() {
        return data.to_vec();
    }
    let pad_len = *data.last().unwrap_or(&0) as usize;
    if pad_len == 0 || pad_len > block_size || pad_len > data.len() {
        return data.to_vec();
    }
    if !data[data.len() - pad_len..]
        .iter()
        .all(|&b| b == pad_len as u8)
    {
        return data.to_vec();
    }
    data[..data.len() - pad_len].to_vec()
}

/// AES-128-ECB 加密（CDN 媒体上传用），自动 PKCS7 padding
pub fn aes_ecb_encrypt(key: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut buf = pkcs7_pad(data, 16);
    // pkcs7_pad 保证长度是 16 的倍数，无余数块
    for chunk in buf.as_chunks_mut::<16>().0 {
        cipher.encrypt_block(GenericArray::from_mut_slice(chunk));
    }
    buf
}

/// 解析主动推送目标 user_id：优先 config `owner_user_id`，其次 state.owner_user_id（自动捕获）。
/// 都没有返回 None，调用方跳过推送。
fn proactive_user_id(cfg: &WechatConfig, state: &WechatState) -> Option<String> {
    if !cfg.owner_user_id.trim().is_empty() {
        Some(cfg.owner_user_id.trim().to_string())
    } else if !state.owner_user_id.trim().is_empty() {
        Some(state.owner_user_id.clone())
    } else {
        None
    }
}

#[async_trait]
impl crate::cron::ProactivePusher for WechatChannel {
    async fn push(&self, message: &str) -> Result<()> {
        self.send_proactive(message).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_pkcs7_roundtrip() {
        for len in [0usize, 1, 15, 16, 17, 32] {
            let data: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let padded = pkcs7_pad(&data, 16);
            assert_eq!(padded.len() % 16, 0);
            assert!(padded.len() > data.len());
            assert_eq!(pkcs7_unpad(&padded, 16), data);
        }
    }

    #[test]
    fn test_aes_ecb_encrypt_size_and_deterministic() {
        let key = [0xABu8; 16];
        let data = b"hello wechat cdn";
        let c1 = aes_ecb_encrypt(&key, data);
        let c2 = aes_ecb_encrypt(&key, data);
        assert_eq!(c1, c2);
        // 16 字节明文 → padding 整块 → 32 字节密文
        assert_eq!(c1.len(), 32);
        assert_ne!(&c1[..16], &data[..]);
    }

    #[test]
    fn test_extract_text_plain_and_voice() {
        let msg = json!({
            "from_user_id": "u1",
            "item_list": [
                { "type": 1, "text_item": { "text": "你好" } },
                { "type": 3, "voice_item": { "text": "语音转录" } },
                { "type": 2, "image_item": {} }
            ]
        });
        assert_eq!(WechatChannel::extract_text(&msg), "你好\n语音转录\n[图片]");
    }

    #[test]
    fn test_api_ok_checks_ret_and_errcode() {
        assert!(WechatChannel::api_ok(&json!({"ret": 0, "errcode": 0})));
        assert!(WechatChannel::api_ok(&json!({})));
        assert!(!WechatChannel::api_ok(&json!({"errcode": -14})));
        assert!(!WechatChannel::api_ok(&json!({"ret": 1})));
        assert_eq!(WechatChannel::api_errcode(&json!({"errcode": -14})), -14);
    }

    #[test]
    fn test_urlencode() {
        assert_eq!(urlencode("abc-_.~"), "abc-_.~");
        assert_eq!(urlencode("a b/c"), "a%20b%2Fc");
    }

    #[test]
    fn test_proactive_user_id_config_priority() {
        let cfg = WechatConfig {
            owner_user_id: "cfg_user".into(),
            ..Default::default()
        };
        let state = WechatState {
            owner_user_id: "auto_user".into(),
            ..Default::default()
        };
        assert_eq!(
            proactive_user_id(&cfg, &state).as_deref(),
            Some("cfg_user"),
            "config owner_user_id wins over auto-captured"
        );
    }

    #[test]
    fn test_proactive_user_id_state_fallback() {
        let cfg = WechatConfig::default();
        let state = WechatState {
            owner_user_id: "auto_user".into(),
            ..Default::default()
        };
        assert_eq!(
            proactive_user_id(&cfg, &state).as_deref(),
            Some("auto_user"),
            "fallback to auto-captured state"
        );
    }

    #[test]
    fn test_proactive_user_id_none_when_unset() {
        let cfg = WechatConfig::default();
        let state = WechatState::default();
        assert_eq!(
            proactive_user_id(&cfg, &state),
            None,
            "no target -> skip push"
        );
    }
}
