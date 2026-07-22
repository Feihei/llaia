use crate::agent::Agent;
use crate::channels::qq_split::split_reply;
use crate::channels::Channel;
use crate::config::QqConfig;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// 腾讯官方 API base URL
const DEFAULT_API_BASE: &str = "https://api.sgroup.qq.com";
/// 腾讯官方鉴权服务 base URL（getAppAccessToken 在这里）
const DEFAULT_AUTH_BASE: &str = "https://bots.qq.com";
/// access_token 刷新提前量（秒），过期前 60 秒视为需要刷新
const TOKEN_REFRESH_MARGIN: u64 = 60;

/// 缓存的 access_token 及其过期时间
#[derive(Default, Clone)]
struct TokenState {
    access_token: String,
    expires_at: Option<Instant>,
}

impl TokenState {
    fn is_valid(&self) -> bool {
        match self.expires_at {
            Some(t) => Instant::now() + Duration::from_secs(TOKEN_REFRESH_MARGIN) < t,
            None => false,
        }
    }
}

pub struct QqChannel {
    config: QqConfig,
    http: Client,
    api_base: String,
    auth_base: String,
    token: Arc<Mutex<TokenState>>,
}

impl QqChannel {
    pub fn new(config: QqConfig) -> Self {
        Self {
            config,
            http: Client::new(),
            api_base: DEFAULT_API_BASE.to_string(),
            auth_base: DEFAULT_AUTH_BASE.to_string(),
            token: Arc::new(Mutex::new(TokenState::default())),
        }
    }

    /// 测试用：允许注入 api_base（同时作为 api_base 和 auth_base，便于 mockito）
    pub fn new_with_api_base(config: QqConfig, api_base: String) -> Self {
        Self {
            config,
            http: Client::new(),
            auth_base: api_base.clone(),
            api_base,
            token: Arc::new(Mutex::new(TokenState::default())),
        }
    }

    /// 获取 access_token，缓存有效则直接返回，否则调 /app/getAppAccessToken 换新
    pub async fn get_access_token(&self) -> Result<String> {
        {
            let st = self.token.lock().await;
            if st.is_valid() {
                return Ok(st.access_token.clone());
            }
        }
        // 缓存失效，换新 token
        let url = format!("{}/app/getAppAccessToken", self.auth_base);
        let body = serde_json::json!({
            "appId": self.config.app_id,
            "clientSecret": self.config.app_secret,
        });
        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("getAppAccessToken failed: status={}, body={}", status, text));
        }
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| anyhow!("getAppAccessToken parse json: {}, body={}", e, text))?;
        let access_token = v
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("getAppAccessToken missing access_token: {}", text))?
            .to_string();
        let expires_in = v
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .unwrap_or(7200);
        let expires_at = Instant::now() + Duration::from_secs(expires_in);
        let mut st = self.token.lock().await;
        *st = TokenState {
            access_token: access_token.clone(),
            expires_at: Some(expires_at),
        };
        tracing::info!(expires_in, "qq access_token refreshed");
        Ok(access_token)
    }

    /// 从腾讯 gateway 接口获取 WebSocket URL
    pub async fn get_ws_url(&self) -> Result<String> {
        let token = self.get_access_token().await?;
        let url = format!("{}/gateway/bot", self.api_base);
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("QQBot {}", token))
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

    /// 从 WS payload 中提取 C2C 文本消息
    /// 返回 (user_openid, msg_id, text) 或 None
    pub fn extract_c2c_text(payload: &serde_json::Value) -> Option<(String, String, String)> {
        // 腾讯官方 C2C 消息事件 op=0, t="C2C_MESSAGE_CREATE"（私域）或 "PUBLIC_C2C_MESSAGE_CREATE"（公域）
        let t = payload.get("t").and_then(|v| v.as_str())?;
        if t != "C2C_MESSAGE_CREATE" && t != "PUBLIC_C2C_MESSAGE_CREATE" {
            return None;
        }
        let d = payload.get("d")?;
        let user_id = d.get("author")?.get("id")?.as_str()?.to_string();
        let msg_id = d.get("id")?.as_str()?.to_string();
        let content = d.get("content")?.as_str()?.to_string();
        if content.trim().is_empty() {
            return None;
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
        let token = self.get_access_token().await?;
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
                .header("Authorization", format!("QQBot {}", token))
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

        // 最近收到的 s 序列号，用于心跳
        let mut last_seq: Option<u64> = None;

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

                    // 记录 s 序列号（用于心跳）
                    if let Some(s) = payload.get("s").and_then(|v| v.as_u64()) {
                        last_seq = Some(s);
                    }

                    // heartbeat ack (op=11)
                    if op == 11 {
                        continue;
                    }

                    // heartbeat request (op=1)：回复最近 s
                    if op == 1 {
                        let d = match last_seq {
                            Some(s) => serde_json::Value::from(s),
                            None => serde_json::Value::Null,
                        };
                        let hb = serde_json::json!({ "op": 1, "d": d });
                        let _ = write.send(Message::Text(hb.to_string())).await;
                        continue;
                    }

                    // hello (op=10)：包含 heartbeat_interval，发送 IDENTIFY (op=2)
                    if op == 10 {
                        let interval = payload
                            .get("d")
                            .and_then(|d| d.get("heartbeat_interval"))
                            .and_then(|v| v.as_u64())
                            .unwrap_or(45000);
                        tracing::info!(interval, "qq ws hello, sending IDENTIFY");

                        let access_token = self.get_access_token().await?;
                        let identify = serde_json::json!({
                            "op": 2,
                            "d": {
                                "token": format!("QQBot {}", access_token),
                                "intents": 1 << 25,  // C2C 消息
                                "shard": [0, 1],
                                "properties": {
                                    "$os": std::env::consts::OS,
                                    "$browser": "laia",
                                    "$device": "laia"
                                }
                            }
                        });
                        let _ = write.send(Message::Text(identify.to_string())).await;
                        // v1.5 简化：心跳只在收到 op=1 时被动回复，不主动定时发送
                        // 如果服务端断连，进程退出。生产级实现需要 tokio::select! + interval 主动心跳
                        continue;
                    }

                    // dispatch 事件 (op=0)
                    if op == 0 {
                        let t = payload.get("t").and_then(|v| v.as_str()).unwrap_or("");
                        // READY 事件：鉴权成功
                        if t == "READY" {
                            let session_id = payload
                                .get("d")
                                .and_then(|d| d.get("session_id"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            tracing::info!(session_id, "qq ws IDENTIFY success (READY)");
                            continue;
                        }
                        // RESUMED 事件
                        if t == "RESUMED" {
                            tracing::info!("qq ws RESUMED");
                            continue;
                        }

                        // C2C 文本消息
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
