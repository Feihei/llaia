//! 飞书 / Lark channel：开放平台事件订阅「长连接」模式（WebSocket 免公网回调）。
//!
//! 单用户简化实现（对比 zeroclaw lark.rs 约 2000 行，砍掉图片/语音/审批卡/反应/代理层）：
//! - POST {ws_base}/callback/ws/endpoint 注册 → 拿 wss 地址 + PingInterval
//! - WS 收 protobuf 二进制帧（pbbp2.proto）：method=0 控制帧(ping/pong) / method=1 数据帧(事件)
//! - 收到事件立即回 ACK 帧（3 秒内，否则服务端重推）；大事件按 message_id/sum/seq 分片重组
//! - 心跳：定时发 ping，收不到任何二进制帧超 300s 重连
//! - 文本消息 → agent turn → POST {api_base}/im/v1/messages 文本回复
//! - `allow_open_id` 非空时只响应指定发送者（单用户安全锁）
//! - `mention_only` 为真时群聊仅在被 @ 时回复（需先解析 bot open_id）
//!
//! 参考：zeroclaw-channels/src/lark.rs（Apache-2.0 / MIT）
//! 文档：<https://open.feishu.cn/document/ukTMukTMukTM/uYDNxYjL2QjN24iN>

use crate::agent::sink::{run_turn, OutputSink};
use crate::agent::{Agent, AgentRegistry, MediaKind};
use crate::channels::qq::split_reply;
use crate::channels::Channel;
use crate::commands::slash::{try_handle, SlashOutcome};
use crate::config::FeishuConfig;
use crate::provider::ChatMessage;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use prost::Message as ProstMessage;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, Notify};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMsg};

/// 单条文本回复切分上限（飞书 text 消息体限约 30000 字节，留足余量）
const MAX_TEXT_LEN: usize = 4000;
/// 心跳超时：超过该时长未收到任何二进制帧则重连
const WS_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(300);
/// 消息去重窗口（飞书长连接可能重推）
const WS_DEDUP_WINDOW: Duration = Duration::from_secs(1800);

/// 飞书 WS 帧（pbbp2.proto）。
/// method=0 → CONTROL（ping/pong）；method=1 → DATA（事件）。
#[derive(Clone, ProstMessage)]
struct PbFrame {
    #[prost(uint64, tag = "1")]
    pub seq_id: u64,
    #[prost(uint64, tag = "2")]
    pub log_id: u64,
    #[prost(int32, tag = "3")]
    pub service: i32,
    #[prost(int32, tag = "4")]
    pub method: i32,
    #[prost(message, repeated, tag = "5")]
    pub headers: Vec<PbHeader>,
    #[prost(bytes = "vec", optional, tag = "8")]
    pub payload: Option<Vec<u8>>,
}

#[derive(Clone, ProstMessage)]
struct PbHeader {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(string, tag = "2")]
    pub value: String,
}

impl PbFrame {
    fn header_value(&self, key: &str) -> &str {
        self.headers
            .iter()
            .find(|h| h.key == key)
            .map(|h| h.value.as_str())
            .unwrap_or("")
    }
}

/// WS pong 帧里携带的客户端配置（PingInterval 用于动态调心跳）
#[derive(Debug, Deserialize, Default)]
struct WsClientConfig {
    #[serde(rename = "PingInterval")]
    ping_interval: Option<u64>,
}

/// 事件信封：{ header:{event_type, event_id}, event:{...} }
#[derive(Debug, Deserialize)]
struct LarkEvent {
    header: LarkEventHeader,
    event: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct LarkEventHeader {
    event_type: String,
}

#[derive(Debug, Deserialize)]
struct MsgReceivePayload {
    sender: LarkSender,
    message: LarkMessage,
}

#[derive(Debug, Deserialize, Default)]
struct LarkSender {
    sender_id: LarkSenderId,
    #[serde(default)]
    sender_type: String,
}

#[derive(Debug, Deserialize, Default)]
struct LarkSenderId {
    open_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LarkMessage {
    message_id: String,
    chat_id: String,
    chat_type: String,
    message_type: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    mentions: Vec<serde_json::Value>,
}

/// 消费端从 WS 循环收到的已解析入站消息（去重与安全锁已在 listener 完成）
struct InboundMessage {
    text: String,
    reply_target: String,
}

pub struct FeishuChannel {
    config: FeishuConfig,
    http: reqwest::Client,
    /// tenant_access_token 缓存：(token, 过期时刻)
    tenant_token: Arc<Mutex<Option<(String, Instant)>>>,
    /// 去重：已处理的 message_id → 接收时刻
    ws_seen_ids: Arc<Mutex<HashMap<String, Instant>>>,
    /// 运行时解析的 bot open_id（群 @ 检测用）
    resolved_bot_open_id: Arc<std::sync::Mutex<Option<String>>>,
}

impl FeishuChannel {
    pub fn new(config: FeishuConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            tenant_token: Arc::new(Mutex::new(None)),
            ws_seen_ids: Arc::new(Mutex::new(HashMap::new())),
            resolved_bot_open_id: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// 向 {ws_base}/callback/ws/endpoint 注册，换取 wss 地址与 PingInterval
    async fn get_ws_endpoint(&self) -> Result<(String, u64)> {
        let url = format!(
            "{}/callback/ws/endpoint",
            self.config.ws_base.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "AppID": self.config.app_id,
            "AppSecret": self.config.app_secret,
        });
        let resp = self
            .http
            .post(&url)
            .header("locale", "zh")
            .json(&body)
            .send()
            .await
            .context("feishu ws endpoint request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("feishu ws endpoint request failed ({}): {}", status, err);
        }
        let data: serde_json::Value = resp.json().await.context("feishu ws endpoint parse")?;
        let code = data.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 0 {
            anyhow::bail!(
                "feishu ws endpoint failed: {}",
                data.get("msg")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown")
            );
        }
        let url_field = data
            .get("data")
            .and_then(|d| d.get("URL"))
            .and_then(|u| u.as_str())
            .ok_or_else(|| anyhow!("feishu ws endpoint missing URL"))?
            .to_string();
        let ping = data
            .get("data")
            .and_then(|d| d.get("ClientConfig"))
            .and_then(|c| c.get("PingInterval"))
            .and_then(|p| p.as_u64())
            .unwrap_or(120);
        Ok((url_field, ping))
    }

    /// 获取（或刷新）tenant_access_token，带缓存避免每次请求
    async fn get_tenant_access_token(&self) -> Result<String> {
        {
            let cached = self.tenant_token.lock().await;
            if let Some((ref token, expiry)) = *cached {
                if Instant::now() < expiry {
                    return Ok(token.clone());
                }
            }
        }
        let url = format!(
            "{}/auth/v3/tenant_access_token/internal",
            self.config.api_base.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "app_id": self.config.app_id,
            "app_secret": self.config.app_secret,
        });
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("tenant_access_token request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("tenant_access_token request failed ({}): {}", status, err);
        }
        let data: serde_json::Value = resp.json().await.context("tenant_access_token parse")?;
        let code = data.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 0 {
            anyhow::bail!(
                "tenant_access_token failed: {}",
                data.get("msg")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown")
            );
        }
        let token = data
            .get("tenant_access_token")
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow!("tenant_access_token missing in response"))?
            .to_string();
        let ttl = data.get("expire").and_then(|v| v.as_u64()).unwrap_or(7200);
        // 提前 120s 刷新，避免临界过期
        let expiry = Instant::now() + Duration::from_secs(ttl.saturating_sub(120).max(1));
        {
            let mut cached = self.tenant_token.lock().await;
            *cached = Some((token.clone(), expiry));
        }
        Ok(token)
    }

    /// 通过 /bot/v3/info 解析 bot open_id（群 @ 检测需要）
    async fn refresh_bot_open_id(&self) -> Result<Option<String>> {
        let token = self.get_tenant_access_token().await?;
        let url = format!("{}/bot/v3/info", self.config.api_base.trim_end_matches('/'));
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .context("bot info request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("bot info request failed ({}): {}", status, err);
        }
        let data: serde_json::Value = resp.json().await.context("bot info parse")?;
        let code = data.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 0 {
            anyhow::bail!(
                "bot info failed: {}",
                data.get("msg")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown")
            );
        }
        let id = data
            .pointer("/bot/open_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        self.set_resolved_bot_open_id(id.clone());
        Ok(id)
    }

    async fn ensure_bot_open_id(&self) {
        if !self.config.mention_only {
            return;
        }
        if self.resolved_bot_open_id().is_some() {
            return;
        }
        match self.refresh_bot_open_id().await {
            Ok(Some(id)) => tracing::info!(bot_open_id = %id, "feishu resolved bot open_id"),
            Ok(None) => tracing::warn!(
                "feishu bot open_id missing from /bot/v3/info; group @mention may not work"
            ),
            Err(e) => tracing::warn!(error = %e, "feishu resolve bot open_id failed"),
        }
    }

    fn resolved_bot_open_id(&self) -> Option<String> {
        self.resolved_bot_open_id
            .lock()
            .ok()
            .and_then(|g| g.clone())
    }

    fn set_resolved_bot_open_id(&self, id: Option<String>) {
        if let Ok(mut g) = self.resolved_bot_open_id.lock() {
            *g = id;
        }
    }

    /// 单用户安全锁：allow_open_id 为空则放行所有人
    fn is_user_allowed(&self, open_id: &str) -> bool {
        if self.config.allow_open_id.is_empty() {
            return true;
        }
        open_id == self.config.allow_open_id
    }

    /// 群聊是否应回复：mention_only 时要求 bot 被 @ 才回复
    fn should_respond(&self, mentions: &[serde_json::Value], _sender_open_id: &str) -> bool {
        if !self.config.mention_only {
            return true;
        }
        let bot_open_id = self.resolved_bot_open_id();
        let bot = match bot_open_id.as_deref() {
            Some(b) => b,
            None => return false,
        };
        mentions.iter().any(|m| {
            m.get("type").and_then(|t| t.as_str()) == Some("mention")
                && m.get("id").and_then(|i| i.as_str()) == Some(bot)
        })
    }

    /// 通过 im/v1/messages 以文本回复（receive_id = chat_id）
    async fn reply(&self, chat_id: &str, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        let token = self.get_tenant_access_token().await?;
        let url = format!(
            "{}/im/v1/messages?receive_id_type=chat_id",
            self.config.api_base.trim_end_matches('/')
        );
        let content = serde_json::to_string(&serde_json::json!({ "text": text }))
            .context("feishu reply content serialize")?;
        let body = serde_json::json!({
            "receive_id": chat_id,
            "msg_type": "text",
            "content": content,
        });
        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send()
            .await
            .context("feishu reply request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("feishu reply failed ({}): {}", status, err);
        }
        Ok(())
    }

    /// 单次连接生命周期：注册 → WS 建连 → 帧循环 → 断开返回 Err（由 run 重连）
    async fn listen_ws(&self, tx: &mpsc::Sender<InboundMessage>) -> Result<()> {
        self.ensure_bot_open_id().await;
        let (wss_url, ping_interval) = self.get_ws_endpoint().await?;
        let service_id = wss_url
            .split('?')
            .nth(1)
            .and_then(|qs| {
                qs.split('&')
                    .find(|kv| kv.starts_with("service_id="))
                    .and_then(|kv| kv.split('=').nth(1))
                    .and_then(|v| v.parse::<i32>().ok())
            })
            .unwrap_or(0);
        tracing::info!(
            "feishu connecting to ws endpoint (service_id={})",
            service_id
        );
        let (ws_stream, _) = connect_async(&wss_url)
            .await
            .map_err(|e| anyhow!("feishu ws connect: {}", e))?;
        let (mut write, mut read) = ws_stream.split();
        tracing::info!("FeishuChannel connected, listening");

        let mut ping_secs = ping_interval.max(10);
        let mut hb_interval = tokio::time::interval(Duration::from_secs(ping_secs));
        let mut timeout_check = tokio::time::interval(Duration::from_secs(10));
        hb_interval.tick().await; // 消费立即触发的首帧

        let mut seq: u64 = 0;
        let mut last_recv = Instant::now();

        // 立即发首帧 ping，让服务端开始回 pong 并校准 PingInterval
        seq = seq.wrapping_add(1);
        let init_ping = PbFrame {
            seq_id: seq,
            log_id: 0,
            service: service_id,
            method: 0,
            headers: vec![PbHeader {
                key: "type".into(),
                value: "ping".into(),
            }],
            payload: None,
        };
        if write
            .send(WsMsg::Binary(init_ping.encode_to_vec()))
            .await
            .is_err()
        {
            anyhow::bail!("feishu initial ping failed");
        }

        // message_id → (分片槽位, 创建时刻)
        type FragEntry = (Vec<Option<Vec<u8>>>, Instant);
        let mut frag_cache: HashMap<String, FragEntry> = HashMap::new();

        loop {
            tokio::select! {
                biased;

                _ = hb_interval.tick() => {
                    seq = seq.wrapping_add(1);
                    let ping = PbFrame {
                        seq_id: seq,
                        log_id: 0,
                        service: service_id,
                        method: 0,
                        headers: vec![PbHeader { key: "type".into(), value: "ping".into() }],
                        payload: None,
                    };
                    if write.send(WsMsg::Binary(ping.encode_to_vec())).await.is_err() {
                        tracing::warn!("feishu ping failed, reconnecting");
                        break;
                    }
                    // 清理 5 分钟以上的残留分片
                    let cutoff = Instant::now().checked_sub(Duration::from_secs(300)).unwrap_or(Instant::now());
                    frag_cache.retain(|_, (_, ts)| *ts > cutoff);
                }

                _ = timeout_check.tick() => {
                    if last_recv.elapsed() > WS_HEARTBEAT_TIMEOUT {
                        tracing::warn!("feishu heartbeat timeout, reconnecting");
                        break;
                    }
                }

                msg = read.next() => {
                    let raw = match msg {
                        Some(Ok(ws_msg)) => {
                            if matches!(ws_msg, WsMsg::Binary(_) | WsMsg::Ping(_) | WsMsg::Pong(_)) {
                                last_recv = Instant::now();
                            }
                            match ws_msg {
                                WsMsg::Binary(b) => b,
                                WsMsg::Ping(d) => { let _ = write.send(WsMsg::Pong(d)).await; continue; }
                                WsMsg::Close(_) => { tracing::info!("feishu ws closed, reconnecting"); break; }
                                _ => continue,
                            }
                        }
                        None => { tracing::info!("feishu ws closed, reconnecting"); break; }
                        Some(Err(e)) => {
                            tracing::error!(error = %e, "feishu ws read error");
                            break;
                        }
                    };

                    let frame = match PbFrame::decode(&raw[..]) {
                        Ok(f) => f,
                        Err(e) => { tracing::error!(error = %e, "feishu proto decode failed"); continue; }
                    };

                    // 控制帧：pong 携带客户端配置（PingInterval）
                    if frame.method == 0 {
                        if frame.header_value("type") == "pong" {
                            if let Some(p) = &frame.payload {
                                if let Ok(cfg) = serde_json::from_slice::<WsClientConfig>(p) {
                                    if let Some(secs) = cfg.ping_interval {
                                        let secs = secs.max(10);
                                        if secs != ping_secs {
                                            ping_secs = secs;
                                            hb_interval =
                                                tokio::time::interval(Duration::from_secs(ping_secs));
                                            tracing::info!(ping_secs, "feishu ping_interval updated");
                                        }
                                    }
                                }
                            }
                        }
                        continue;
                    }

                    // 数据帧：先回 ACK（3 秒内，否则服务端重推）
                    {
                        let mut ack = frame.clone();
                        ack.payload = Some(br#"{"code":200,"headers":{},"data":[]}"#.to_vec());
                        ack.headers.push(PbHeader { key: "biz_rt".into(), value: "0".into() });
                        let _ = write.send(WsMsg::Binary(ack.encode_to_vec())).await;
                    }

                    // 分片重组
                    let msg_type = frame.header_value("type").to_string();
                    let msg_id = frame.header_value("message_id").to_string();
                    let sum = frame.header_value("sum").parse::<usize>().unwrap_or(1).max(1);
                    let seq_num = frame.header_value("seq").parse::<usize>().unwrap_or(0);
                    let payload: Vec<u8> = if sum == 1 || msg_id.is_empty() || seq_num >= sum {
                        frame.payload.clone().unwrap_or_default()
                    } else {
                        let entry = frag_cache
                            .entry(msg_id.clone())
                            .or_insert_with(|| (vec![None; sum], Instant::now()));
                        if entry.0.len() != sum {
                            *entry = (vec![None; sum], Instant::now());
                        }
                        entry.0[seq_num] = frame.payload.clone();
                        if entry.0.iter().all(|s| s.is_some()) {
                            let full: Vec<u8> = entry.0.iter().flat_map(|s| s.as_deref().unwrap_or(&[])).copied().collect();
                            frag_cache.remove(&msg_id);
                            full
                        } else {
                            continue;
                        }
                    };

                    if msg_type != "event" {
                        continue;
                    }

                    let event: LarkEvent = match serde_json::from_slice(&payload) {
                        Ok(e) => e,
                        Err(e) => { tracing::error!(error = %e, "feishu event JSON parse failed"); continue; }
                    };
                    if event.header.event_type != "im.message.receive_v1" {
                        continue;
                    }

                    let recv: MsgReceivePayload = match serde_json::from_value(event.event.clone()) {
                        Ok(r) => r,
                        Err(e) => { tracing::error!(error = %e, "feishu message payload parse failed"); continue; }
                    };
                    // 忽略机器人/应用自己发出的消息
                    if recv.sender.sender_type == "app" || recv.sender.sender_type == "bot" {
                        continue;
                    }

                    let sender_open_id = recv.sender.sender_id.open_id.clone().unwrap_or_default();
                    if !self.is_user_allowed(&sender_open_id) {
                        tracing::debug!(sender_open_id, "feishu: ignored (not in allow_open_id)");
                        continue;
                    }

                    let lark_msg = &recv.message;

                    // 去重：防重推
                    {
                        let now = Instant::now();
                        let mut seen = self.ws_seen_ids.lock().await;
                        seen.retain(|_, t| now.duration_since(*t) < WS_DEDUP_WINDOW);
                        if seen.contains_key(&lark_msg.message_id) {
                            tracing::debug!(message_id = %lark_msg.message_id, "feishu: duplicate, skipped");
                            continue;
                        }
                        seen.insert(lark_msg.message_id.clone(), now);
                    }

                    // 解析文本内容（text / post；其余类型暂不支持）
                    let text = match lark_msg.message_type.as_str() {
                        "text" => {
                            let v: serde_json::Value = match serde_json::from_str(&lark_msg.content) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            match v.get("text").and_then(|t| t.as_str()).filter(|s| !s.is_empty()) {
                                Some(t) => t.to_string(),
                                None => continue,
                            }
                        }
                        "post" => match parse_post_text(&lark_msg.content) {
                            Some(t) => t,
                            None => continue,
                        },
                        other => {
                            tracing::debug!(msg_type = other, "feishu: unsupported message type, skipped");
                            continue;
                        }
                    };

                    let text = text.trim().to_string();
                    if text.is_empty() {
                        continue;
                    }

                    // 群聊 @ 检测
                    if lark_msg.chat_type == "group" && !self.should_respond(&lark_msg.mentions, &sender_open_id) {
                        continue;
                    }

                    let inbound = InboundMessage {
                        text,
                        reply_target: lark_msg.chat_id.clone(),
                    };
                    if tx.send(inbound).await.is_err() {
                        break;
                    }
                }
            }
        }
        Err(anyhow!("feishu websocket stream ended"))
    }

    /// 处理单条入站消息：斜杠命令 / 普通 agent turn
    async fn handle_message(
        self: &Arc<Self>,
        agent: &Arc<Mutex<Agent>>,
        stop: &Arc<Notify>,
        inbound: InboundMessage,
        registry: &Arc<AgentRegistry>,
    ) -> Result<()> {
        let text = inbound.text.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }

        // 斜杠命令
        if text.starts_with('/') {
            if text.trim().eq_ignore_ascii_case("/stop") {
                stop.notify_waiters();
                let _ = self
                    .reply(&inbound.reply_target, "[stop signal sent]")
                    .await;
                return Ok(());
            }
            let outcome = {
                let mut a = agent.lock().await;
                try_handle(&text, &mut a, Some(registry.clone())).await?
            };
            match outcome {
                SlashOutcome::Exit => {
                    let _ = self
                        .reply(
                            &inbound.reply_target,
                            "[/exit not available on Feishu channel]",
                        )
                        .await;
                }
                SlashOutcome::Handled(m) => {
                    let _ = self.reply(&inbound.reply_target, &m).await;
                }
                SlashOutcome::NotSlash => {}
                SlashOutcome::Resume { notice, message } => {
                    let _ = self.reply(&inbound.reply_target, &notice).await;
                    let sink = FeishuSink {
                        fs: self.clone(),
                        chat_id: inbound.reply_target.clone(),
                        buffer: String::new(),
                    };
                    registry.set_delivery(
                        self.clone()
                            .pusher()
                            .map(crate::tools::delegate::DeliveryTarget::Pusher),
                    );
                    let _ = run_turn(
                        agent.clone(),
                        ChatMessage::user(&message),
                        "feishu".into(),
                        Box::new(sink),
                        stop.clone(),
                    )
                    .await;
                }
            }
            return Ok(());
        }

        // 普通消息：跑一轮 agent turn
        let sink = FeishuSink {
            fs: self.clone(),
            chat_id: inbound.reply_target.clone(),
            buffer: String::new(),
        };
        registry.set_delivery(
            self.clone()
                .pusher()
                .map(crate::tools::delegate::DeliveryTarget::Pusher),
        );
        run_turn(
            agent.clone(),
            ChatMessage::user(&text),
            "feishu".into(),
            Box::new(sink),
            stop.clone(),
        )
        .await
    }
}

#[async_trait]
impl crate::channels::Channel for FeishuChannel {
    async fn run(self: Arc<Self>, registry: Arc<crate::agent::AgentRegistry>) -> Result<()> {
        let agent = registry.main.clone();
        if self.config.app_id.is_empty() || self.config.app_secret.is_empty() {
            anyhow::bail!("feishu enabled but app_id/app_secret is empty");
        }
        let stop = Arc::new(Notify::new());
        let (tx, mut rx) = mpsc::channel::<InboundMessage>(32);

        // 消费任务：与 WS 读循环解耦，避免 agent 思考阻塞 ACK 帧
        let consumer = {
            let self_clone = self.clone();
            let agent = agent.clone();
            let stop = stop.clone();
            let registry = registry.clone();
            tokio::spawn(async move {
                while let Some(msg) = rx.recv().await {
                    if let Err(e) = self_clone
                        .handle_message(&agent, &stop, msg, &registry)
                        .await
                    {
                        tracing::error!(error = %e, "feishu handle message failed");
                    }
                }
            })
        };
        std::mem::drop(consumer); // serve 退出时会 abort 所有 task（drop 仅丢弃 JoinHandle，后台任务继续跑，运行时关闭才 abort）

        loop {
            if let Err(e) = self.listen_ws(&tx).await {
                // 网络抖动 / 凭证失效 / 服务端踢连接：等 5s 重连
                tracing::warn!(error = %e, "feishu connection lost, reconnect in 5s");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// 输出汇聚：缓冲全部文本后整条发送（文本消息）
struct FeishuSink {
    fs: Arc<FeishuChannel>,
    chat_id: String,
    buffer: String,
}

#[async_trait]
impl OutputSink for FeishuSink {
    async fn on_chunk(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }

    async fn on_tool_start(&mut self, name: &str) {
        let _ = self
            .fs
            .reply(&self.chat_id, &format!("🔧 {}...", name))
            .await;
    }

    async fn on_media(&mut self, path: &str, _kind: MediaKind) {
        let _ = self
            .fs
            .reply(&self.chat_id, &format!("[media file: {}]", path))
            .await;
    }

    async fn on_done(&mut self) {
        let reply = if self.buffer.trim().is_empty() {
            "[done (no text output)]".to_string()
        } else {
            self.buffer.trim_start_matches(['\n', '\r']).to_string()
        };
        for chunk in split_reply(&reply, MAX_TEXT_LEN) {
            if let Err(e) = self.fs.reply(&self.chat_id, &chunk).await {
                tracing::error!(error = %e, "feishu send reply failed");
            }
        }
    }

    async fn on_error(&mut self, message: &str) {
        if !self.buffer.trim().is_empty() {
            let _ = self.fs.reply(&self.chat_id, self.buffer.trim()).await;
        }
        let _ = self
            .fs
            .reply(&self.chat_id, &format!("[error: {}]", message))
            .await;
    }

    async fn on_interrupted(&mut self) {
        let mut text = self.buffer.clone();
        if !text.trim().is_empty() {
            text.push_str("\n\n");
        }
        text.push_str("[interrupted]");
        let _ = self.fs.reply(&self.chat_id, &text).await;
    }
}

/// 从飞书 post 富文本里抽出纯文本（title + 各段落 text 标签）
fn parse_post_text(content: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(content).ok()?;
    let mut out = String::new();
    if let Some(title) = v.get("title").and_then(|t| t.as_str()) {
        if !title.is_empty() {
            out.push_str(title);
            out.push('\n');
        }
    }
    if let Some(rows) = v.get("content").and_then(|c| c.as_array()) {
        for row in rows {
            if let Some(items) = row.as_array() {
                for item in items {
                    if item.get("tag").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                            out.push_str(t);
                        }
                    }
                }
            }
        }
    }
    if out.trim().is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_post_text_extracts_text() {
        let content = r#"{"title":"标题","content":[[{"tag":"text","text":"你好"},{"tag":"text","text":"世界"}]]}"#;
        let text = parse_post_text(content).unwrap();
        assert_eq!(text, "标题\n你好世界");
    }

    #[test]
    fn test_parse_post_text_empty_returns_none() {
        let content = r#"{"content":[[{"tag":"image","image_key":"x"}]]}"#;
        assert!(parse_post_text(content).is_none());
    }

    #[test]
    fn test_pb_frame_roundtrip() {
        let frame = PbFrame {
            seq_id: 1,
            log_id: 0,
            service: 7,
            method: 1,
            headers: vec![PbHeader {
                key: "type".into(),
                value: "event".into(),
            }],
            payload: Some(b"{}".to_vec()),
        };
        let bytes = frame.encode_to_vec();
        let decoded = PbFrame::decode(&bytes[..]).unwrap();
        assert_eq!(decoded.method, 1);
        assert_eq!(decoded.header_value("type"), "event");
        assert_eq!(decoded.payload.unwrap(), b"{}".to_vec());
    }

    #[test]
    fn test_allow_open_id_lock() {
        let cfg = FeishuConfig {
            allow_open_id: "ou_abc".into(),
            ..Default::default()
        };
        let ch = FeishuChannel::new(cfg);
        assert!(ch.is_user_allowed("ou_abc"));
        assert!(!ch.is_user_allowed("ou_other"));
        // 空锁放行所有人
        let open_cfg = FeishuConfig::default();
        let open_ch = FeishuChannel::new(open_cfg);
        assert!(open_ch.is_user_allowed("anyone"));
    }
}
