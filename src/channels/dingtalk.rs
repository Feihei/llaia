//! 钉钉 channel：开放平台机器人 + Stream Mode WebSocket，免公网回调。
//!
//! 单用户简化实现（对比 zeroclaw dingtalk.rs 554 行砍掉 allowlist/多实例/proxy 层）：
//! - POST gateway connections/open 注册 → 拿 WS endpoint + ticket
//! - WS 收 CALLBACK 帧（文本消息）→ agent turn，SYSTEM 帧回 pong 保活
//! - 回复走每条消息自带的 sessionWebhook（markdown 格式）
//! - `allow_staff_id` 非空时只响应指定发送者（单用户安全锁）
//!
//! 参考：zeroclaw-channels/src/dingtalk.rs（Apache-2.0 / MIT）
//! 文档：<https://open.dingtalk.com/document/resourcedownload/introduction-to-stream-mode>

use crate::agent::sink::{run_turn, OutputSink};
use crate::agent::{AgentRegistry, MediaKind};
use crate::channels::qq::split_reply;
use crate::config::DingtalkConfig;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// 机器人消息回调 topic
const BOT_CALLBACK_TOPIC: &str = "/v1.0/im/bot/messages/get";
/// 单条 markdown 回复切分上限（钉钉限约 20000 字节，留足余量）
const MAX_TEXT_LEN: usize = 4000;

pub struct DingtalkChannel {
    config: DingtalkConfig,
    http: Client,
}

/// gateway 注册响应
#[derive(Debug, Deserialize)]
pub struct GatewayResponse {
    pub endpoint: String,
    pub ticket: String,
}

impl DingtalkChannel {
    pub fn new(config: DingtalkConfig) -> Self {
        Self {
            config,
            http: Client::new(),
        }
    }

    /// 向 gateway 注册连接，换取 WS endpoint + ticket
    pub async fn register_connection(&self) -> Result<GatewayResponse> {
        let url = format!(
            "{}/v1.0/gateway/connections/open",
            self.config.api_base.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "clientId": self.config.client_id,
            "clientSecret": self.config.client_secret,
            "subscriptions": [
                { "type": "CALLBACK", "topic": BOT_CALLBACK_TOPIC }
            ],
        });
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("gateway register request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("gateway registration failed ({}): {}", status, err);
        }
        let gw: GatewayResponse = resp.json().await.context("gateway response parse")?;
        Ok(gw)
    }

    /// 通过 sessionWebhook 发送 markdown 回复
    pub async fn send_markdown(&self, webhook_url: &str, text: &str) -> Result<()> {
        let body = serde_json::json!({
            "msgtype": "markdown",
            "markdown": { "title": "LLAIA", "text": text },
        });
        let resp = self
            .http
            .post(webhook_url)
            .json(&body)
            .send()
            .await
            .context("webhook reply request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("webhook reply failed ({}): {}", status, err);
        }
        Ok(())
    }

    /// 解析流帧里的 data 负载（字符串 JSON 或对象两种形态）
    pub fn parse_stream_data(frame: &serde_json::Value) -> Option<serde_json::Value> {
        match frame.get("data") {
            Some(serde_json::Value::String(raw)) => serde_json::from_str(raw).ok(),
            Some(serde_json::Value::Object(_)) => frame.get("data").cloned(),
            _ => None,
        }
    }

    /// 构造 SYSTEM/CALLBACK 帧的确认响应
    fn ack_frame(message_id: &str) -> serde_json::Value {
        serde_json::json!({
            "code": 200,
            "headers": { "contentType": "application/json", "messageId": message_id },
            "message": "OK",
            "data": "",
        })
    }

    /// 处理单条 CALLBACK 消息帧：解析 → 安全锁 → ACK → agent turn
    async fn handle_message(
        self: &Arc<Self>,
        frame: &serde_json::Value,
        agent: &Arc<Mutex<crate::agent::Agent>>,
        stop: &Arc<Notify>,
    ) -> Result<()> {
        let data = match Self::parse_stream_data(frame) {
            Some(d) => d,
            None => return Ok(()),
        };
        let text = data
            .get("text")
            .and_then(|t| t.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if text.is_empty() {
            return Ok(());
        }
        let sender_id = data
            .get("senderStaffId")
            .and_then(|s| s.as_str())
            .unwrap_or("unknown");
        // 单用户安全锁
        if !self.config.allow_staff_id.is_empty() && sender_id != self.config.allow_staff_id {
            tracing::debug!(sender_id, "message from non-allowed staff, ignored");
            return Ok(());
        }
        let webhook = data
            .get("sessionWebhook")
            .and_then(|w| w.as_str())
            .unwrap_or("")
            .to_string();
        if webhook.is_empty() {
            tracing::warn!("callback message without sessionWebhook, cannot reply");
            return Ok(());
        }
        tracing::info!(sender_id, len = text.len(), "dingtalk message in");

        // 斜杠命令
        if text.starts_with('/') {
            if text == "/stop" {
                stop.notify_waiters();
                let _ = self.send_markdown(&webhook, "[stop signal sent]").await;
                return Ok(());
            }
            let outcome = {
                let mut a = agent.lock().await;
                crate::commands::slash::try_handle(&text, &mut a).await?
            };
            match outcome {
                crate::commands::slash::SlashOutcome::Exit => {
                    let _ = self.send_markdown(&webhook, "[/exit 在钉钉下不可用]").await;
                }
                crate::commands::slash::SlashOutcome::Handled(m) => {
                    let _ = self.send_markdown(&webhook, &m).await;
                }
                crate::commands::slash::SlashOutcome::NotSlash => {}
                crate::commands::slash::SlashOutcome::Resume { notice, message } => {
                    let _ = self.send_markdown(&webhook, &notice).await;
                    let sink = DingtalkSink {
                        dt: Arc::clone(self),
                        webhook: webhook.clone(),
                        buffer: String::new(),
                    };
                    let _ = run_turn(
                        agent.clone(),
                        crate::provider::ChatMessage::user(&message),
                        "dingtalk".into(),
                        Box::new(sink),
                        stop.clone(),
                    )
                    .await;
                }
            }
            return Ok(());
        }

        // 普通消息：跑一轮 agent turn
        let sink = DingtalkSink {
            dt: Arc::clone(self),
            webhook,
            buffer: String::new(),
        };
        run_turn(
            agent.clone(),
            crate::provider::ChatMessage::user(text),
            "dingtalk".into(),
            Box::new(sink),
            stop.clone(),
        )
        .await
    }

    /// 单次连接生命周期：注册 → WS 建连 → 帧循环 → 断开返回 Err
    async fn run_connection(
        self: &Arc<Self>,
        agent: &Arc<Mutex<crate::agent::Agent>>,
        stop: &Arc<Notify>,
    ) -> Result<()> {
        let gw = self.register_connection().await?;
        let ws_url = format!("{}?ticket={}", gw.endpoint, gw.ticket);
        tracing::info!("dingtalk connecting to stream gateway");
        let (ws_stream, _) = connect_async(&ws_url)
            .await
            .map_err(|e| anyhow!("ws connect: {}", e))?;
        let (mut write, mut read) = ws_stream.split();
        tracing::info!("DingtalkChannel connected, listening");

        while let Some(msg) = read.next().await {
            let text = match msg {
                Ok(Message::Text(t)) => t,
                Ok(Message::Ping(data)) => {
                    let _ = write.send(Message::Pong(data)).await;
                    continue;
                }
                Ok(Message::Close(_)) => break,
                Ok(_) => continue,
                Err(e) => {
                    return Err(anyhow!("ws read error: {}", e));
                }
            };
            let frame: serde_json::Value = match serde_json::from_str(text.as_ref()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let frame_type = frame.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let message_id = frame
                .get("headers")
                .and_then(|h| h.get("messageId"))
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string();
            match frame_type {
                "SYSTEM" | "EVENT" | "CALLBACK" => {
                    // 先 ACK（钉钉要求及时确认，否则会重推）
                    let ack = Self::ack_frame(&message_id);
                    if let Err(e) = write.send(Message::Text(ack.to_string())).await {
                        return Err(anyhow!("ws ack send failed: {}", e));
                    }
                    if frame_type != "SYSTEM" {
                        if let Err(e) = self.handle_message(&frame, agent, stop).await {
                            tracing::error!(error = %e, "handle dingtalk message failed");
                        }
                    }
                }
                _ => {}
            }
        }
        Err(anyhow!("dingtalk websocket stream ended"))
    }
}

#[async_trait]
impl crate::channels::Channel for DingtalkChannel {
    async fn run(self: Arc<Self>, registry: Arc<AgentRegistry>) -> Result<()> {
        let agent = registry.main.clone();
        if self.config.client_id.is_empty() || self.config.client_secret.is_empty() {
            anyhow::bail!("dingtalk enabled but client_id/client_secret is empty");
        }
        let stop = Arc::new(Notify::new());
        loop {
            if let Err(e) = self.run_connection(&agent, &stop).await {
                // 网络抖动/凭证失效/gateway 踢连接：等 5s 重连
                tracing::warn!(error = %e, "dingtalk connection lost, reconnect in 5s");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// 输出汇聚：缓冲全部文本后走 sessionWebhook 整条发送（markdown）
struct DingtalkSink {
    dt: Arc<DingtalkChannel>,
    webhook: String,
    buffer: String,
}

#[async_trait]
impl OutputSink for DingtalkSink {
    async fn on_chunk(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }

    async fn on_tool_start(&mut self, name: &str) {
        let _ = self
            .dt
            .send_markdown(&self.webhook, &format!("🔧 {}...", name))
            .await;
    }

    async fn on_media(&mut self, path: &str, _kind: MediaKind) {
        // v1：sessionWebhook 媒体需 mediaId 上传流程，暂以文本提示代替
        let _ = self
            .dt
            .send_markdown(&self.webhook, &format!("[媒体文件: {}]", path))
            .await;
    }

    async fn on_done(&mut self) {
        let reply = if self.buffer.trim().is_empty() {
            "[已完成（无文本输出）]"
        } else {
            self.buffer.trim_start_matches(['\n', '\r'])
        };
        for chunk in split_reply(reply, MAX_TEXT_LEN) {
            if let Err(e) = self.dt.send_markdown(&self.webhook, &chunk).await {
                tracing::error!(error = %e, "dingtalk send reply failed");
            }
        }
    }

    async fn on_error(&mut self, message: &str) {
        if !self.buffer.trim().is_empty() {
            let _ = self
                .dt
                .send_markdown(&self.webhook, self.buffer.trim())
                .await;
        }
        let _ = self
            .dt
            .send_markdown(&self.webhook, &format!("[出错了: {}]", message))
            .await;
    }

    async fn on_interrupted(&mut self) {
        let mut text = self.buffer.clone();
        if !text.trim().is_empty() {
            text.push_str("\n\n");
        }
        text.push_str("[已中断]");
        let _ = self.dt.send_markdown(&self.webhook, &text).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_stream_data_string_payload() {
        let frame = json!({ "data": "{\"text\":{\"content\":\"hello\"}}" });
        let parsed = DingtalkChannel::parse_stream_data(&frame).unwrap();
        assert_eq!(parsed["text"]["content"], "hello");
    }

    #[test]
    fn test_parse_stream_data_object_payload() {
        let frame = json!({ "data": { "text": { "content": "hi" } } });
        let parsed = DingtalkChannel::parse_stream_data(&frame).unwrap();
        assert_eq!(parsed["text"]["content"], "hi");
    }

    #[test]
    fn test_parse_stream_data_missing() {
        let frame = json!({ "type": "SYSTEM" });
        assert!(DingtalkChannel::parse_stream_data(&frame).is_none());
    }

    #[test]
    fn test_ack_frame_carries_message_id() {
        let ack = DingtalkChannel::ack_frame("msg-1");
        assert_eq!(ack["code"], 200);
        assert_eq!(ack["headers"]["messageId"], "msg-1");
    }
}
