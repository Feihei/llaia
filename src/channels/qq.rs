use crate::agent::sink::{OutputSink, run_turn};
use crate::agent::{Agent, AgentRegistry, MediaKind};
use crate::channels::Channel;
use crate::config::QqConfig;
use crate::provider::{ChatMessage, ContentPart, ImageUrlContent};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify};
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

/// 从 QQ 收到的 C2C 消息（文本 + 附件）
#[derive(Debug, Clone)]
pub struct C2cIncoming {
    pub user_id: String,
    pub msg_id: String,
    pub text: String,
    pub attachments: Vec<Attachment>,
}

/// 消息附件（图片/文件）
#[derive(Debug, Clone)]
pub struct Attachment {
    pub content_type: String,
    pub filename: String,
    pub url: String,
}

impl Attachment {
    /// 是否为图片
    pub fn is_image(&self) -> bool {
        self.content_type.starts_with("image/")
    }
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
    /// 被动回复 msg_seq 递增计数器。
    /// QQ 要求同一 msg_id 下 msg_seq 递增，否则被去重（err_code 40054005）。
    msg_seq_counter: AtomicU32,
    /// 每个 user 正在执行的 turn 的中断信号。
    /// key: user_openid，value: Notify。/stop 时 notify 对应 turn 使其中断。
    running_stops: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
}

impl QqChannel {
    pub fn new(config: QqConfig) -> Self {
        Self {
            config,
            http: Client::new(),
            api_base: DEFAULT_API_BASE.to_string(),
            auth_base: DEFAULT_AUTH_BASE.to_string(),
            token: Arc::new(Mutex::new(TokenState::default())),
            msg_seq_counter: AtomicU32::new(1),
            running_stops: Arc::new(Mutex::new(HashMap::new())),
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
            msg_seq_counter: AtomicU32::new(1),
            running_stops: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 取下一个递增的 msg_seq（用于被动回复去重）
    fn next_msg_seq(&self) -> u32 {
        self.msg_seq_counter.fetch_add(1, Ordering::Relaxed)
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

    /// 清空 token 缓存，下次 get_access_token 会强制刷新
    async fn invalidate_token(&self) {
        let mut st = self.token.lock().await;
        *st = TokenState::default();
    }

    /// 从腾讯 gateway 接口获取 WebSocket URL
    pub async fn get_ws_url(&self) -> Result<String> {
        match self.get_ws_url_inner(false).await {
            Ok(url) => Ok(url),
            Err(first_err) => {
                // 首次失败若是 token 过期，强制刷新 token 重试一次
                if first_err.to_string().contains("token not exist or expire") {
                    tracing::warn!("gateway returned token-expired, force refreshing and retrying");
                    self.invalidate_token().await;
                    self.get_ws_url_inner(true).await
                } else {
                    Err(first_err)
                }
            }
        }
    }

    async fn get_ws_url_inner(&self, _force: bool) -> Result<String> {
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

    /// 从 WS payload 中提取 C2C 消息（文本 + 附件）
    /// 返回 C2cIncoming 或 None
    pub fn extract_c2c_message(payload: &serde_json::Value) -> Option<C2cIncoming> {
        // 腾讯官方 C2C 消息事件 op=0, t="C2C_MESSAGE_CREATE"（私域）或 "PUBLIC_C2C_MESSAGE_CREATE"（公域）
        let t = payload.get("t").and_then(|v| v.as_str())?;
        if t != "C2C_MESSAGE_CREATE" && t != "PUBLIC_C2C_MESSAGE_CREATE" {
            return None;
        }
        let d = payload.get("d")?;
        let user_id = d.get("author")?.get("id")?.as_str()?.to_string();
        let msg_id = d.get("id")?.as_str()?.to_string();
        let content = d
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // 提取附件（图片/文件）
        let attachments: Vec<Attachment> = d
            .get("attachments")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| {
                        Some(Attachment {
                            content_type: a.get("content_type")?.as_str()?.to_string(),
                            filename: a
                                .get("filename")
                                .and_then(|v| v.as_str())
                                .unwrap_or("file")
                                .to_string(),
                            url: a.get("url")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        // 文本和附件都为空才跳过
        if content.trim().is_empty() && attachments.is_empty() {
            return None;
        }
        Some(C2cIncoming {
            user_id,
            msg_id,
            text: content,
            attachments,
        })
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
            // 被动回复必须带递增 msg_seq，否则同一 msg_id 的后续回复被去重 (err_code 40054005)
            body["msg_seq"] = serde_json::Value::from(self.next_msg_seq());
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

    /// 向 QQ 用户发送媒体文件（图片或文件）。
    /// 流程：上传到 QQ 文件服务拿 file_info → 发送 msg_type=7 富媒体消息。
    pub async fn send_media_to_user(
        &self,
        user_openid: &str,
        path: &str,
        kind: crate::agent::MediaKind,
        msg_id: Option<&str>,
    ) -> Result<()> {
        let token = self.get_access_token().await?;
        let file_type = match kind {
            crate::agent::MediaKind::Image => 1,  // 1=图片
            crate::agent::MediaKind::File => 4,   // 4=文件
        };

        // 读文件内容
        let file_bytes = tokio::fs::read(path)
            .await
            .map_err(|e| anyhow!("read media file {:?}: {}", path, e))?;
        let file_name = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();

        // 1. 上传媒体到 QQ 文件服务
        let upload_url = format!("{}/v2/users/{}/files", self.api_base, user_openid);
        let part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(file_name.clone())
            .mime_str("application/octet-stream")?;
        let form = reqwest::multipart::Form::new()
            .text("file_type", file_type.to_string())
            .part("file", part);

        let resp = self
            .http
            .post(&upload_url)
            .header("Authorization", format!("QQBot {}", token))
            .multipart(form)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "upload media failed: status={}, body={}",
                status,
                text
            ));
        }
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| anyhow!("parse upload response: {}, body={}", e, text))?;
        let file_info = v
            .get("file_info")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("upload response missing file_info: {}", text))?
            .to_string();
        tracing::info!(file = %path, file_info = %file_info, "media uploaded");

        // 2. 发送富媒体消息 msg_type=7
        let send_url = format!("{}/v2/users/{}/messages", self.api_base, user_openid);
        let mut body = serde_json::json!({
            "msg_type": 7,
            "media": {
                "file_info": file_info,
            },
        });
        if let Some(id) = msg_id {
            body["msg_id"] = serde_json::Value::String(id.to_string());
            body["msg_seq"] = serde_json::Value::from(self.next_msg_seq());
        }

        let resp = self
            .http
            .post(&send_url)
            .header("Authorization", format!("QQBot {}", token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "send media message failed: status={}, body={}",
                status,
                text
            ));
        }
        tracing::info!(user = %user_openid, file = %path, "media sent");
        Ok(())
    }

    /// 下载 QQ 消息附件到本地 uploads 目录。
    /// 保存路径：`<uploads_dir>/<msg_id>_<filename>`（用 msg_id 防止同名冲突）。
    /// 返回本地文件路径。
    pub async fn download_attachment(
        &self,
        att: &Attachment,
        uploads_dir: &Path,
        msg_id: &str,
    ) -> Result<PathBuf> {
        let token = self.get_access_token().await?;
        let url = &att.url;
        // QQ 附件 url 可能是相对路径，需补全为 api_base 下的绝对 URL
        let full_url = if url.starts_with("http://") || url.starts_with("https://") {
            url.clone()
        } else {
            format!("{}{}", self.api_base, url)
        };

        let resp = self
            .http
            .get(&full_url)
            .header("Authorization", format!("QQBot {}", token))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "download attachment failed: status={}, body={}",
                status,
                text
            ));
        }
        let bytes = resp.bytes().await?;

        // 保存到 uploads/<msg_id>_<filename>
        tokio::fs::create_dir_all(uploads_dir)
            .await
            .map_err(|e| anyhow!("create uploads dir: {}", e))?;
        let safe_name = att.filename.replace('/', "_").replace('\\', "_");
        let local_path = uploads_dir.join(format!("{}_{}", msg_id, safe_name));
        tokio::fs::write(&local_path, &bytes)
            .await
            .map_err(|e| anyhow!("write attachment file: {}", e))?;

        tracing::info!(
            filename = %att.filename,
            content_type = %att.content_type,
            size = bytes.len(),
            "attachment downloaded"
        );
        Ok(local_path)
    }

    /// 处理一条用户消息：先检查斜杠命令，否则下载附件构造多模态消息，再流式调 agent。
    async fn handle_user_message(
        self: Arc<Self>,
        agent: &Arc<Mutex<Agent>>,
        user_openid: &str,
        incoming: &C2cIncoming,
    ) -> Result<()> {
        let text = &incoming.text;
        let msg_id = &incoming.msg_id;
        tracing::info!(
            user = %user_openid,
            text = %text,
            attachments = incoming.attachments.len(),
            "qq received message"
        );

        // 斜杠命令：在锁内处理，把输出发回用户（忽略附件）
        if text.trim().starts_with('/') {
            // /stop：中断当前正在执行的 turn（不需要 lock agent，避免被长任务阻塞）
            if text.trim() == "/stop" {
                let notify = {
                    let mut stops = self.running_stops.lock().await;
                    stops.remove(user_openid)
                };
                if let Some(n) = notify {
                    n.notify_one();
                    let _ = self
                        .send_c2c_message(user_openid, "[已中断当前任务]", Some(msg_id.as_str()))
                        .await;
                } else {
                    let _ = self
                        .send_c2c_message(user_openid, "[没有正在执行的任务]", Some(msg_id.as_str()))
                        .await;
                }
                return Ok(());
            }
            let outcome = {
                let mut a = agent.lock().await;
                crate::commands::slash::try_handle(text, &mut *a).await?
            };
            match outcome {
                crate::commands::slash::SlashOutcome::Exit => {
                    // QQ 下忽略 /exit，不退出
                    let _ = self
                        .send_c2c_message(user_openid, "[/exit 在 QQ 下不可用]", Some(msg_id.as_str()))
                        .await;
                }
                crate::commands::slash::SlashOutcome::Handled(msg) => {
                    let _ = self.send_c2c_message(user_openid, &msg, Some(msg_id.as_str())).await;
                }
                crate::commands::slash::SlashOutcome::NotSlash => {
                    // 不会走到这里（已检查 starts_with '/'）
                }
            }
            return Ok(());
        }

        // 构造消息：有附件则下载并构造多模态，否则纯文本
        let user_msg = if incoming.attachments.is_empty() {
            ChatMessage::user(text)
        } else {
            // 获取 workspace 路径（短暂持锁）
            let workspace = {
                let a = agent.lock().await;
                a.workspace.clone()
            };
            let uploads_dir = workspace.join("uploads");

            let mut parts: Vec<ContentPart> = Vec::new();
            if !text.is_empty() {
                parts.push(ContentPart::Text {
                    text: text.clone(),
                });
            }
            for att in &incoming.attachments {
                match self.download_attachment(att, &uploads_dir, msg_id).await {
                    Ok(local_path) => {
                        if att.is_image() {
                            // 图片：缩放并转 base64 data URL，发给 vision 模型
                            match crate::image_utils::prepare_image_for_vision(&local_path) {
                                Ok(data_url) => {
                                    parts.push(ContentPart::ImageUrl {
                                        image_url: ImageUrlContent { url: data_url },
                                    });
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "prepare image failed");
                                    parts.push(ContentPart::Text {
                                        text: format!("[图片预处理失败: {}]", e),
                                    });
                                }
                            }
                        } else {
                            // 非图片文件：仅告知 agent 附件名和保存路径
                            parts.push(ContentPart::Text {
                                text: format!(
                                    "[附件已保存至 workspace/uploads/{}_{}]",
                                    msg_id,
                                    att.filename
                                ),
                            });
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, filename = %att.filename, "download attachment failed");
                        parts.push(ContentPart::Text {
                            text: format!("[附件下载失败: {}]", e),
                        });
                    }
                }
            }

            if parts.is_empty() {
                ChatMessage::user(text)
            } else if parts.len() == 1 {
                // 仅文本 part（所有附件下载失败）
                if let Some(ContentPart::Text { text: t }) = parts.first() {
                    ChatMessage::user(t)
                } else {
                    ChatMessage::user_multimodal(parts)
                }
            } else {
                ChatMessage::user_multimodal(parts)
            }
        };

        // 普通消息：用 run_turn 跑这一轮，QqSink 负责输出
        let stop = Arc::new(Notify::new());
        {
            let mut stops = self.running_stops.lock().await;
            stops.insert(user_openid.to_string(), stop.clone());
        }

        let sink = Box::new(QqSink {
            qq: self.clone(),
            user_openid: user_openid.to_string(),
            msg_id: msg_id.to_string(),
            buffer: String::new(),
        });

        let turn_result = run_turn(agent.clone(), user_msg, "qq".into(), sink, stop).await;

        // 清理中断信号注册
        {
            let mut stops = self.running_stops.lock().await;
            stops.remove(user_openid);
        }

        turn_result?;
        Ok(())
    }
}

/// QQ 输出 sink：累积 chunk 后分片发送，工具调用即时通知
struct QqSink {
    qq: Arc<QqChannel>,
    user_openid: String,
    msg_id: String,
    buffer: String,
}

#[async_trait]
impl OutputSink for QqSink {
    async fn on_chunk(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }
    async fn on_tool_start(&mut self, name: &str) {
        let notice = format!("🔧 {}...", name);
        let _ = self
            .qq
            .send_c2c_message(&self.user_openid, &notice, Some(&self.msg_id))
            .await;
    }
    async fn on_media(&mut self, path: &str, kind: MediaKind) {
        if let Err(e) = self
            .qq
            .send_media_to_user(&self.user_openid, path, kind, Some(&self.msg_id))
            .await
        {
            tracing::error!(error = %e, path = path, "failed to send media");
            let _ = self
                .qq
                .send_c2c_message(
                    &self.user_openid,
                    &format!("[发送媒体失败: {}]", e),
                    Some(&self.msg_id),
                )
                .await;
        }
    }
    async fn on_done(&mut self) {
        // agent 可能只调工具无文本输出，buffer 为空时给占位回复
        // 否则 QQ 会因 content="" 返回 304061 invalid content
        let reply = if self.buffer.trim().is_empty() {
            tracing::warn!(total_len = self.buffer.len(), "agent reply empty, sending placeholder");
            "[已完成（无文本输出）]"
        } else {
            self.buffer.as_str()
        };
        let chunks = split_reply(reply, 1800);
        tracing::info!(chunks = chunks.len(), total_len = reply.len(), "sending reply");
        for (i, chunk) in chunks.iter().enumerate() {
            if chunk.trim().is_empty() {
                continue;
            }
            // 只有第一片带 msg_id（被动回复），后续片用主动消息
            let id = if i == 0 { Some(self.msg_id.as_str()) } else { None };
            if let Err(e) = self.qq.send_c2c_message(&self.user_openid, chunk, id).await {
                tracing::error!(error = %e, chunk = i, "failed to send chunk after retries");
            }
        }
    }
    async fn on_error(&mut self, message: &str) {
        let err_msg = if self.buffer.is_empty() {
            format!("[内部错误: {}]", message)
        } else {
            // 保留已生成文本，错误追加
            self.buffer.clone()
        };
        let chunks = split_reply(&err_msg, 1800);
        for (i, chunk) in chunks.iter().enumerate() {
            let id = if i == 0 { Some(self.msg_id.as_str()) } else { None };
            if let Err(e) = self.qq.send_c2c_message(&self.user_openid, chunk, id).await {
                tracing::error!(error = %e, chunk = i, "failed to send chunk after retries");
            }
        }
    }
    async fn on_interrupted(&mut self) {
        // /stop 的回复文本由中断触发方（QQ /stop handler）发送，这里只 log
        tracing::info!(user = %self.user_openid, "turn interrupted by /stop");
    }
}

#[async_trait]
impl Channel for QqChannel {
    async fn run(self: Arc<Self>, registry: Arc<AgentRegistry>) -> Result<()> {
        let agent = registry.main.clone();
        tracing::info!(app_id = %self.config.app_id, "QqChannel starting");

        // 外层重连循环：ws 断开后等待 5 秒重连，避免 serve 进程退出
        loop {
            match self.clone().run_connection(&agent).await {
                Ok(()) => tracing::warn!("qq ws connection closed, will reconnect"),
                Err(e) => tracing::error!(error = %e, "qq ws connection ended with error, will reconnect"),
            }
            tracing::info!("reconnecting in 5 seconds...");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
}

impl QqChannel {
    /// 单次连接的完整生命周期：建连 → IDENTIFY → 消息/心跳循环 → 断开
    async fn run_connection(self: Arc<Self>, agent: &Arc<Mutex<Agent>>) -> Result<()> {
        let ws_url = self.get_ws_url().await?;
        tracing::info!(url = %ws_url, "connecting to QQ gateway");

        let (ws_stream, _resp) = connect_async(&ws_url)
            .await
            .map_err(|e| anyhow!("ws connect: {}", e))?;
        let (mut write, mut read) = ws_stream.split();

        // 最近收到的 s 序列号，用于心跳
        let mut last_seq: Option<u64> = None;
        // 心跳间隔（毫秒），收到 op=10 HELLO 后设置
        let mut heartbeat_interval: u64 = 0;
        // 下次发心跳的时间，None 表示还没收到 HELLO
        let mut next_heartbeat: Option<Instant> = None;

        loop {
            // 计算心跳超时：没收到 HELLO 时等很久（实际会很快收到 op=10）
            let timeout = match next_heartbeat {
                Some(t) => {
                    let now = Instant::now();
                    if t <= now {
                        Duration::ZERO
                    } else {
                        t - now
                    }
                }
                None => Duration::from_secs(3600),
            };

            tokio::select! {
                msg = read.next() => {
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

                            // heartbeat ack (op=11)：服务端确认收到心跳
                            if op == 11 {
                                continue;
                            }

                            // hello (op=10)：包含 heartbeat_interval，发送 IDENTIFY (op=2)
                            if op == 10 {
                                heartbeat_interval = payload
                                    .get("d")
                                    .and_then(|d| d.get("heartbeat_interval"))
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(45000);
                                tracing::info!(heartbeat_interval, "qq ws hello, sending IDENTIFY");

                                let access_token = self.get_access_token().await?;
                                let identify = serde_json::json!({
                                    "op": 2,
                                    "d": {
                                        "token": format!("QQBot {}", access_token),
                                        "intents": 1 << 25,  // C2C 消息
                                        "shard": [0, 1],
                                        "properties": {
                                            "$os": std::env::consts::OS,
                                            "$browser": "llaia",
                                            "$device": "llaia"
                                        }
                                    }
                                });
                                let _ = write.send(Message::Text(identify.to_string())).await;
                                // 安排首次心跳
                                next_heartbeat = Some(Instant::now() + Duration::from_millis(heartbeat_interval));
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

                                // C2C 消息（文本 + 可能的附件）
                                if let Some(incoming) = Self::extract_c2c_message(&payload) {
                                    let user_openid = incoming.user_id.clone();
                                    let this = self.clone();
                                    let agent = agent.clone();
                                    tokio::spawn(async move {
                                        if let Err(e) = this
                                            .handle_user_message(&agent, &user_openid, &incoming)
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
                _ = tokio::time::sleep(timeout) => {
                    // 主动发心跳 op=1，d 为最近收到的 s（或 null）
                    if heartbeat_interval > 0 {
                        let d = match last_seq {
                            Some(s) => serde_json::Value::from(s),
                            None => serde_json::Value::Null,
                        };
                        let hb = serde_json::json!({ "op": 1, "d": d });
                        if let Err(e) = write.send(Message::Text(hb.to_string())).await {
                            tracing::error!(error = %e, "failed to send heartbeat");
                            break;
                        }
                        tracing::debug!(seq = ?last_seq, "heartbeat sent");
                        next_heartbeat = Some(Instant::now() + Duration::from_millis(heartbeat_interval));
                    }
                }
            }
        }

        Ok(())
    }
}

/// 将长文本按 QQ 单条消息上限分片。
///
/// 规则：
/// 1. 优先按段落（`\n\n`）切
/// 2. 单段超 max 时按行（`\n`）切
/// 3. 单行超 max 时按字符硬切
/// 4. 代码块跨片时闭合后再开，下一片以 ``` 同语言标记开始
pub fn split_reply(text: &str, max: usize) -> Vec<String> {
    if text.len() <= max {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut in_code_block = false;
    let mut code_lang = String::new();

    /// 把 current 推到 chunks。如果在代码块里，先闭合。
    /// 推完后，如果还在代码块里，current 重置为 ```{lang}\n 准备接续。
    fn flush(
        current: &mut String,
        chunks: &mut Vec<String>,
        in_code_block: &mut bool,
        code_lang: &str,
    ) {
        if current.is_empty() {
            return;
        }
        let was_in_code = *in_code_block;
        if was_in_code {
            current.push_str("\n```");
        }
        chunks.push(std::mem::take(current));
        if was_in_code {
            // 仍在代码块内，下一片以代码块开头续接
            *current = format!("```{}\n", code_lang);
        }
    }

    let paragraphs: Vec<&str> = text.split("\n\n").collect();
    for para in paragraphs {
        // 检测代码块状态变化
        let trimmed = para.trim_start();
        if trimmed.starts_with("```") {
            if !in_code_block {
                in_code_block = true;
                code_lang = trimmed.trim_start_matches("```").trim_end().to_string();
            } else if trimmed == "```" {
                in_code_block = false;
            }
        }

        let candidate = if current.is_empty() {
            para.to_string()
        } else {
            format!("{}\n\n{}", current, para)
        };

        if candidate.len() <= max {
            current = candidate;
        } else {
            // 段落加不进去，先把 current 推走
            flush(&mut current, &mut chunks, &mut in_code_block, &code_lang);

            if para.len() <= max {
                // 段落本身不超 max，直接放到 current（current 可能已有代码块开头）
                if current.is_empty() {
                    current = para.to_string();
                } else {
                    // current 是 ```{lang}\n，追加段落
                    current.push_str(para);
                }
            } else {
                // 段落本身超 max，按行切
                let lines: Vec<&str> = para.split('\n').collect();
                for line in lines {
                    let candidate = if current.is_empty() {
                        line.to_string()
                    } else if current.ends_with('\n') {
                        format!("{}{}", current, line)
                    } else {
                        format!("{}\n{}", current, line)
                    };

                    if candidate.len() <= max {
                        current = candidate;
                    } else {
                        // 当前行加不进去
                        flush(&mut current, &mut chunks, &mut in_code_block, &code_lang);

                        if line.len() > max {
                            // 单行也超 max，按字符硬切
                            // current 可能是 ```{lang}\n，先把这部分作为前缀
                            let prefix = if !current.is_empty() {
                                current.clone()
                            } else {
                                String::new()
                            };
                            let prefix_len = prefix.len();
                            let avail = max.saturating_sub(prefix_len);

                            if prefix_len >= max {
                                // 前缀本身就超 max（极端情况），先推走
                                chunks.push(std::mem::take(&mut current));
                                let mut remaining = line;
                                while remaining.len() > max {
                                    let (chunk, rest) = remaining.split_at(max);
                                    chunks.push(chunk.to_string());
                                    remaining = rest;
                                }
                                current = remaining.to_string();
                            } else {
                                // 第一片带前缀
                                let mut remaining = line;
                                // 先把能装进第一片的装进去
                                let (chunk, rest) = remaining.split_at(avail);
                                current.push_str(chunk);
                                chunks.push(std::mem::take(&mut current));
                                remaining = rest;
                                // 后续片不带前缀，纯字符切
                                while remaining.len() > max {
                                    let (chunk, rest) = remaining.split_at(max);
                                    chunks.push(chunk.to_string());
                                    remaining = rest;
                                }
                                current = remaining.to_string();
                            }
                        } else {
                            // 单行不超 max，直接放入 current
                            if current.is_empty() {
                                current = line.to_string();
                            } else if current.ends_with('\n') {
                                current.push_str(line);
                            } else {
                                current.push('\n');
                                current.push_str(line);
                            }
                        }
                    }
                }
            }
        }
    }

    if !current.is_empty() {
        if in_code_block {
            current.push_str("\n```");
        }
        chunks.push(current);
    }

    chunks
}

#[cfg(test)]
mod split_reply_tests {
    use super::*;

    #[test]
    fn test_short_no_split() {
        assert_eq!(split_reply("hi", 100), vec!["hi"]);
    }

    #[test]
    fn test_paragraph_split() {
        let text = "p1\n\np2\n\np3";
        assert_eq!(split_reply(text, 4), vec!["p1", "p2", "p3"]);
    }

    #[test]
    fn test_long_line_char_split() {
        let text = "a".repeat(250);
        let parts = split_reply(&text, 100);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 100);
        assert_eq!(parts[1].len(), 100);
        assert_eq!(parts[2].len(), 50);
    }

    #[test]
    fn test_line_split_when_paragraph_too_long() {
        let text = "aaaaa\nbbbbb\nccccc\nddddd";
        let parts = split_reply(text, 12);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].len() <= 12);
        assert!(parts[1].len() <= 12);
    }

    #[test]
    fn test_code_block_preserved_within_chunk() {
        let text = "前文\n\n```rust\nfn main() {}\n```\n\n后文";
        let parts = split_reply(text, 100);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].contains("```rust"));
    }

    #[test]
    fn test_code_block_split_closes_and_reopens() {
        let long_line = "    println!(\"x\");\n".repeat(100);
        let text = format!("```rust\nfn main() {{\n{}}}\n```", long_line);
        let parts = split_reply(&text, 1800);
        assert!(parts.len() > 1, "expected multiple chunks, got {}", parts.len());
        assert!(parts[0].ends_with("```"), "first chunk should end with ```");
        assert!(parts[1].starts_with("```rust"), "second chunk should start with ```rust");
    }
}
