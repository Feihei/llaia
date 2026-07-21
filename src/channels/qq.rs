use crate::agent::Agent;
use crate::channels::qq_split::split_reply;
use crate::channels::Channel;
use crate::config::QqConfig;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// 腾讯官方 QQ 开放平台 API base URL
const DEFAULT_API_BASE: &str = "https://api.sgroup.qq.com";

pub struct QqChannel {
    config: QqConfig,
    http: Client,
    api_base: String,
}

impl QqChannel {
    pub fn new(config: QqConfig) -> Self {
        Self {
            config,
            http: Client::new(),
            api_base: DEFAULT_API_BASE.to_string(),
        }
    }

    /// 测试用：允许注入 api_base_url（如 mockito URL）
    pub fn new_with_api_base(config: QqConfig, api_base: String) -> Self {
        Self {
            config,
            http: Client::new(),
            api_base,
        }
    }

    fn auth_header(&self) -> String {
        format!("Bot {}.{}", self.config.app_id, self.config.token)
    }

    /// 从腾讯 gateway 接口获取 WebSocket URL
    /// GET {api_base}/gateway/bot，返回 { "url": "wss://...", "shards": 1, ... }
    pub async fn get_ws_url(&self) -> Result<String> {
        let url = format!("{}/gateway/bot", self.api_base);
        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;
        let ws_url = resp
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("gateway response missing 'url' field: {}", resp))?
            .to_string();
        Ok(ws_url)
    }

    /// 从 WS 收到的 payload 中提取 C2C 文本消息
    /// 返回 (user_openid, msg_id, text) 或 None
    pub fn extract_c2c_text(payload: &serde_json::Value) -> Option<(String, String, String)> {
        // 腾讯官方 C2C 消息事件 op=0, t="C2C_MESSAGE_CREATE"
        if payload.get("t").and_then(|v| v.as_str()) != Some("C2C_MESSAGE_CREATE") {
            return None;
        }
        let d = payload.get("d")?;
        let user_id = d.get("author")?.get("id")?.as_str()?.to_string();
        let msg_id = d.get("id")?.as_str()?.to_string();
        let content = d.get("content")?.as_str()?.to_string();
        if content.trim().is_empty() {
            return None;
        }
        // 跳过自己（bot）发的消息
        if let Some(bot_id) = &payload.get("d").and_then(|d| d.get("author")).and_then(|a| a.get("id")).and_then(|v| v.as_str()) {
            // 这里 author.id 是发送者 id，bot 不会收到自己的消息，但保险起见
            // 如果未来需要从 self.config.bot_qq 判断，可以在这里加
            let _ = bot_id;
        }
        Some((user_id, msg_id, content))
    }

    /// 通过 HTTPS API 发送 C2C 消息
    /// 3 次指数退避：200ms / 400ms / 800ms
    pub async fn send_c2c_message(
        &self,
        user_openid: &str,
        content: &str,
        msg_id: Option<&str>,
    ) -> Result<()> {
        let url = format!("{}/v2/users/{}/messages", self.api_base, user_openid);
        let mut body = serde_json::json!({
            "content": content,
            "msg_type": 0,  // 0 = 文本
        });
        if let Some(id) = msg_id {
            body["msg_id"] = serde_json::Value::String(id.to_string());
        }

        let delays = [200u64, 400, 800];
        let mut last_err: Option<anyhow::Error> = None;
        for (attempt, delay) in delays.iter().enumerate() {
            let resp = self
                .http
                .post(&url)
                .header("Authorization", self.auth_header())
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    tracing::debug!(attempt, user = %user_openid, "qq send ok");
                    return Ok(());
                }
                Ok(r) => {
                    let status = r.status();
                    let text = r.text().await.unwrap_or_default();
                    tracing::warn!(attempt, %status, %text, "qq send failed, retrying");
                    last_err = Some(anyhow!("status: {}, body: {}", status, text));
                }
                Err(e) => {
                    tracing::warn!(attempt, error = %e, "qq send error, retrying");
                    last_err = Some(e.into());
                }
            }
            tokio::time::sleep(Duration::from_millis(*delay)).await;
        }
        Err(last_err.unwrap_or_else(|| anyhow!("unknown error")))
    }

    /// 处理一条用户消息：调 agent 拿回复，分片发送
    async fn handle_user_message(
        self: Arc<Self>,
        agent: &Arc<Mutex<Agent>>,
        user_openid: &str,
        text: &str,
        msg_id: &str,
    ) -> Result<()> {
        tracing::info!(user = %user_openid, text = %text, "qq received message");

        let reply = {
            let mut a = agent.lock().await;
            a.handle_input(text, "qq").await
        };

        let reply = match reply {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "agent handle_input failed");
                format!("[内部错误: {}]", e)
            }
        };

        // 分片发送，每片 ≤ 1800 字符
        let chunks = split_reply(&reply, 1800);
        tracing::info!(chunks = chunks.len(), total_len = reply.len(), "sending reply");
        for (i, chunk) in chunks.iter().enumerate() {
            // 只有第一片带 msg_id 用于被动回复，后续片用主动消息
            let id = if i == 0 { Some(msg_id) } else { None };
            if let Err(e) = self.send_c2c_message(user_openid, chunk, id).await {
                tracing::error!(error = %e, chunk = i, "failed to send chunk after retries");
                // 不 return，继续尝试后续片
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for QqChannel {
    async fn run(self: Arc<Self>, agent: Arc<Mutex<Agent>>) -> Result<()> {
        tracing::info!(app_id = %self.config.app_id, "QqChannel starting");

        let ws_url = self.get_ws_url().await?;
        tracing::info!(url = %ws_url, "connecting to QQ gateway");

        let (ws_stream, _resp) = connect_async(&ws_url)
            .await
            .map_err(|e| anyhow!("ws connect: {}", e))?;
        let (mut write, mut read) = ws_stream.split();

        // 注意：完整的腾讯官方鉴权 handshake (IDENTIFY op=2) 需要根据官方文档实现。
        // 这里只给出骨架。实际运行时需要发送：
        // {
        //   "op": 2,
        //   "d": {
        //     "token": "Bot {app_id}.{token}",
        //     "intents": 1 << 25,  // C2C_GROUP_AT_MESSAGE_CREATE
        //     "shard": [0, 1],
        //     "properties": { "$os": "linux", "$browser": "laia", "$device": "laia" }
        //   }
        // }
        // 详见 https://bot.q.qq.com/wiki/

        loop {
            let msg = read.next().await;
            match msg {
                Some(Ok(Message::Text(text))) => {
                    let payload: serde_json::Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to parse ws payload");
                            continue;
                        }
                    };

                    let op = payload.get("op").and_then(|v| v.as_u64()).unwrap_or(0);

                    // heartbeat ack (op=11)
                    if op == 11 {
                        continue;
                    }

                    // heartbeat request (op=1)
                    if op == 1 {
                        let _ = write.send(Message::Text("1".into())).await;
                        continue;
                    }

                    // hello (op=10)：服务端发送，包含 heartbeat_interval
                    if op == 10 {
                        // 应在此发送 IDENTIFY (op=2)
                        // 简化实现：TODO 根据 https://bot.q.qq.com/wiki/ 补全
                        let identify = serde_json::json!({
                            "op": 2,
                            "d": {
                                "token": format!("Bot {}.{}", self.config.app_id, self.config.token),
                                "intents": 1 << 25,
                                "shard": [0, 1],
                                "properties": {
                                    "$os": std::env::consts::OS,
                                    "$browser": "laia",
                                    "$device": "laia"
                                }
                            }
                        });
                        let _ = write.send(Message::Text(identify.to_string())).await;
                        continue;
                    }

                    // 提取 C2C 文本消息
                    if let Some((user_openid, msg_id, text)) = Self::extract_c2c_text(&payload) {
                        let this = self.clone();
                        let agent = agent.clone();
                        tokio::spawn(async move {
                            if let Err(e) = this
                                .handle_user_message(&agent, &user_openid, &text, &msg_id)
                                .await
                            {
                                tracing::error!(error = %e, "handle_user_message failed");
                            }
                        });
                    }
                }
                Some(Ok(Message::Ping(data))) => {
                    let _ = write.send(Message::Pong(data)).await;
                }
                Some(Ok(_)) => continue,
                Some(Err(e)) => {
                    tracing::error!(error = %e, "ws read error");
                    break;
                }
                None => {
                    tracing::info!("ws closed");
                    break;
                }
            }
        }

        tracing::warn!("QqChannel exited");
        Ok(())
    }
}
