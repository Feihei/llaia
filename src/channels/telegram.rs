//! Telegram channel：官方 Bot API + HTTP long polling，免公网回调。
//!
//! 单用户简化实现（对比 zeroclaw telegram.rs 7.2k 行砍掉群聊/命令菜单/审批按钮等）：
//! - `getUpdates` 长轮询收消息（timeout 30s）
//! - 文本消息 → agent turn（`run_turn` + `TelegramSink` 缓冲后整条发送）
//! - 图片/文件输出 → `sendPhoto` / `sendDocument`（multipart 上传）
//! - `allow_chat_id` 非 0 时只响应指定 chat（单用户安全锁）
//!
//! 参考：Telegram Bot API <https://core.telegram.org/bots/api>

use crate::agent::sink::{run_turn, OutputSink};
use crate::agent::{AgentRegistry, MediaKind};
use crate::channels::qq::split_reply;
use crate::channels::Channel;
use crate::config::TelegramConfig;
use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};

/// Telegram 消息长度上限（sendMessage），留余量按 4000 切
const MAX_TEXT_LEN: usize = 4000;
/// getUpdates 长轮询服务端超时（秒）
const LONG_POLL_TIMEOUT_SECS: u64 = 30;

pub struct TelegramChannel {
    config: TelegramConfig,
    http: Client,
}

// ---- Bot API 响应模型（只取需要的字段） ----

#[derive(Debug, Deserialize)]
#[serde(bound = "T: serde::de::DeserializeOwned")]
struct ApiEnvelope<T> {
    ok: bool,
    #[serde(default)]
    result: Option<T>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Update {
    update_id: i64,
    #[serde(default)]
    message: Option<TgMessage>,
}

#[derive(Debug, Deserialize)]
struct TgMessage {
    message_id: i64,
    chat: TgChat,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    caption: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgChat {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct BotUser {
    username: Option<String>,
}

impl TelegramChannel {
    pub fn new(config: TelegramConfig) -> Result<Self> {
        let http = Client::builder()
            // 长轮询 timeout 30s + 网络余量
            .timeout(Duration::from_secs(LONG_POLL_TIMEOUT_SECS + 20))
            .connect_timeout(Duration::from_secs(15))
            .build()
            .context("build telegram http client")?;
        Ok(Self { config, http })
    }

    fn method_url(&self, method: &str) -> String {
        format!(
            "{}/bot{}/{}",
            self.config.api_base.trim_end_matches('/'),
            self.config.bot_token,
            method
        )
    }

    /// 启动时校验 token 有效性，返回 bot username
    pub async fn get_me(&self) -> Result<String> {
        let url = self.method_url("getMe");
        let resp = self.http.get(&url).send().await.context("getMe request")?;
        let env: ApiEnvelope<BotUser> = resp.json().await.context("getMe parse")?;
        if !env.ok {
            anyhow::bail!(
                "getMe failed: {}",
                env.description.unwrap_or_else(|| "unknown".into())
            );
        }
        Ok(env
            .result
            .and_then(|u| u.username)
            .unwrap_or_else(|| "?".into()))
    }

    /// 长轮询拉取更新
    async fn get_updates(&self, offset: i64) -> Result<Vec<Update>> {
        let url = self.method_url("getUpdates");
        let body = serde_json::json!({
            "offset": offset,
            "timeout": LONG_POLL_TIMEOUT_SECS,
            "allowed_updates": ["message"],
        });
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("getUpdates request")?;
        let env: ApiEnvelope<Vec<Update>> = resp.json().await.context("getUpdates parse")?;
        if !env.ok {
            anyhow::bail!(
                "getUpdates failed: {}",
                env.description.unwrap_or_else(|| "unknown".into())
            );
        }
        Ok(env.result.unwrap_or_default())
    }

    /// 发送纯文本回复
    pub async fn send_text(&self, chat_id: i64, text: &str) -> Result<()> {
        let url = self.method_url("sendMessage");
        let body = serde_json::json!({ "chat_id": chat_id, "text": text });
        let resp = self.http.post(&url).json(&body).send().await?;
        let env: ApiEnvelope<serde_json::Value> = resp.json().await?;
        if !env.ok {
            anyhow::bail!(
                "sendMessage failed: {}",
                env.description.unwrap_or_else(|| "unknown".into())
            );
        }
        Ok(())
    }

    /// 发送媒体：Image → sendPhoto，File → sendDocument
    async fn send_media(&self, chat_id: i64, path: &str, kind: MediaKind) -> Result<()> {
        let file_path = std::path::Path::new(path);
        let file_name = file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "upload".into());
        let bytes = tokio::fs::read(file_path)
            .await
            .with_context(|| format!("read media file: {}", path))?;
        let part = reqwest::multipart::Part::bytes(bytes).file_name(file_name);
        let form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part(
                if matches!(kind, MediaKind::Image) {
                    "photo"
                } else {
                    "document"
                },
                part,
            );
        let method = if matches!(kind, MediaKind::Image) {
            "sendPhoto"
        } else {
            "sendDocument"
        };
        let url = self.method_url(method);
        let resp = self.http.post(&url).multipart(form).send().await?;
        let env: ApiEnvelope<serde_json::Value> = resp.json().await?;
        if !env.ok {
            anyhow::bail!(
                "{} failed: {}",
                method,
                env.description.unwrap_or_else(|| "unknown".into())
            );
        }
        Ok(())
    }

    /// 处理单条 update
    async fn handle_update(
        self: &Arc<Self>,
        update: &Update,
        agent: &Arc<Mutex<crate::agent::Agent>>,
        stop: &Arc<Notify>,
        registry: &Arc<AgentRegistry>,
    ) -> Result<()> {
        let Some(msg) = &update.message else {
            return Ok(());
        };
        let chat_id = msg.chat.id;
        // 单用户安全锁
        if self.config.allow_chat_id != 0 && chat_id != self.config.allow_chat_id {
            tracing::debug!(chat_id, "message from non-allowed chat, ignored");
            return Ok(());
        }
        // 文本优先 text，媒体消息用 caption
        let text = msg
            .text
            .clone()
            .or_else(|| msg.caption.clone())
            .unwrap_or_default();
        if text.trim().is_empty() {
            return Ok(());
        }
        let text = text.trim();
        tracing::info!(
            chat_id,
            msg_id = msg.message_id,
            len = text.len(),
            "telegram message in"
        );

        // Telegram 客户端自带的 /start：回个招呼，不走 agent
        if text == "/start" || text.starts_with("/start ") {
            let _ = self
                .send_text(
                    chat_id,
                    "你好，我是 LLAIA 私人助理。直接发消息即可对话，/help 查看命令。",
                )
                .await;
            return Ok(());
        }

        // 斜杠命令
        if text.starts_with('/') {
            if text.trim().eq_ignore_ascii_case("/stop") {
                stop.notify_waiters();
                let _ = self.send_text(chat_id, "[stop signal sent]").await;
                return Ok(());
            }
            let outcome = {
                let mut a = agent.lock().await;
                crate::commands::slash::try_handle(text, &mut a, Some(registry.clone())).await?
            };
            match outcome {
                crate::commands::slash::SlashOutcome::Exit => {
                    let _ = self
                        .send_text(chat_id, "[/exit 在 Telegram 下不可用]")
                        .await;
                }
                crate::commands::slash::SlashOutcome::Handled(m) => {
                    let _ = self.send_text(chat_id, &m).await;
                }
                crate::commands::slash::SlashOutcome::NotSlash => {}
                crate::commands::slash::SlashOutcome::Resume { notice, message } => {
                    let _ = self.send_text(chat_id, &notice).await;
                    let sink = TelegramSink {
                        tg: Arc::clone(self),
                        chat_id,
                        buffer: String::new(),
                    };
                    registry.set_delivery(
                        self.clone()
                            .pusher()
                            .map(crate::tools::delegate::DeliveryTarget::Pusher),
                    );
                    let _ = run_turn(
                        agent.clone(),
                        crate::provider::ChatMessage::user(&message),
                        "telegram".into(),
                        Box::new(sink),
                        stop.clone(),
                    )
                    .await;
                }
            }
            return Ok(());
        }

        // 普通消息：跑一轮 agent turn
        let sink = TelegramSink {
            tg: Arc::clone(self),
            chat_id,
            buffer: String::new(),
        };
        registry.set_delivery(
            self.clone()
                .pusher()
                .map(crate::tools::delegate::DeliveryTarget::Pusher),
        );
        run_turn(
            agent.clone(),
            crate::provider::ChatMessage::user(text),
            "telegram".into(),
            Box::new(sink),
            stop.clone(),
        )
        .await
    }
}

#[async_trait]
impl crate::channels::Channel for TelegramChannel {
    async fn run(self: Arc<Self>, registry: Arc<AgentRegistry>) -> Result<()> {
        let agent = registry.main.clone();
        if self.config.bot_token.is_empty() {
            anyhow::bail!("telegram enabled but bot_token is empty");
        }
        let bot_name = self.get_me().await.context("telegram getMe on startup")?;
        tracing::info!(bot = %bot_name, "TelegramChannel starting (long polling)");

        let stop = Arc::new(Notify::new());
        let mut offset: i64 = 0;
        loop {
            match self.get_updates(offset).await {
                Ok(updates) => {
                    for u in &updates {
                        offset = u.update_id + 1;
                        if let Err(e) = self.handle_update(u, &agent, &stop, &registry).await {
                            tracing::error!(update_id = u.update_id, error = %e, "handle update failed");
                        }
                    }
                }
                Err(e) => {
                    // 网络抖动/token 失效等：等 5s 重试，避免空转刷屏
                    tracing::warn!(error = %e, "getUpdates failed, retry in 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
}

/// 解析主动推送目标 chat_id：优先 `owner_chat_id`，其次回退 `allow_chat_id`。
/// 两者都为 0（未配置）返回 None，调用方跳过推送。
fn proactive_chat_id(cfg: &TelegramConfig) -> Option<i64> {
    if cfg.owner_chat_id != 0 {
        Some(cfg.owner_chat_id)
    } else if cfg.allow_chat_id != 0 {
        Some(cfg.allow_chat_id)
    } else {
        None
    }
}

impl TelegramChannel {
    /// 主动推送消息：用于 cron 任务结果推送。
    /// 目标 chat_id：① `owner_chat_id`（手动指定）② `allow_chat_id`（回退，单用户锁）。
    /// 都没有则 log + 返回 Ok（不报错，cron 不因此失败）。
    pub async fn send_proactive(&self, message: &str) -> Result<()> {
        match proactive_chat_id(&self.config) {
            Some(chat_id) => self.send_text(chat_id, message).await,
            None => {
                tracing::warn!(
                    "cron push to telegram skipped: no owner chat_id (set [channels.telegram] owner_chat_id or allow_chat_id)"
                );
                Ok(())
            }
        }
    }
}

#[async_trait]
impl crate::cron::ProactivePusher for TelegramChannel {
    async fn push(&self, message: &str) -> Result<()> {
        self.send_proactive(message).await
    }
}

/// 输出汇聚：缓冲全部文本后整条发送（Telegram 无编辑成本考虑，避免流式刷屏）
struct TelegramSink {
    tg: Arc<TelegramChannel>,
    chat_id: i64,
    buffer: String,
}

#[async_trait]
impl OutputSink for TelegramSink {
    async fn on_chunk(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }

    async fn on_tool_start(&mut self, name: &str) {
        let _ = self
            .tg
            .send_text(self.chat_id, &format!("🔧 {}...", name))
            .await;
    }

    async fn on_media(&mut self, path: &str, kind: MediaKind) {
        if let Err(e) = self.tg.send_media(self.chat_id, path, kind).await {
            tracing::error!(error = %e, path, "telegram send media failed");
            let _ = self
                .tg
                .send_text(self.chat_id, &format!("[发送媒体失败: {}]", e))
                .await;
        }
    }

    async fn on_done(&mut self) {
        let reply = if self.buffer.trim().is_empty() {
            "[已完成（无文本输出）]"
        } else {
            self.buffer.trim_start_matches(['\n', '\r'])
        };
        for chunk in split_reply(reply, MAX_TEXT_LEN) {
            if let Err(e) = self.tg.send_text(self.chat_id, &chunk).await {
                tracing::error!(error = %e, "telegram send reply failed");
            }
        }
    }

    async fn on_error(&mut self, message: &str) {
        // 已生成的文本先发出，再附错误
        if !self.buffer.trim().is_empty() {
            let _ = self.tg.send_text(self.chat_id, self.buffer.trim()).await;
        }
        let _ = self
            .tg
            .send_text(self.chat_id, &format!("[出错了: {}]", message))
            .await;
    }

    async fn on_interrupted(&mut self) {
        let mut text = self.buffer.clone();
        if !text.trim().is_empty() {
            text.push_str("\n\n");
        }
        text.push_str("[已中断]");
        let _ = self.tg.send_text(self.chat_id, &text).await;
    }
}

#[cfg(test)]
mod proactive_tests {
    use super::*;
    use crate::config::TelegramConfig;

    #[test]
    fn test_proactive_chat_id_owner_priority() {
        let cfg = TelegramConfig {
            owner_chat_id: 111,
            allow_chat_id: 222,
            ..Default::default()
        };
        assert_eq!(proactive_chat_id(&cfg), Some(111), "owner wins over allow");
    }

    #[test]
    fn test_proactive_chat_id_fallback_to_allow() {
        let cfg = TelegramConfig {
            allow_chat_id: 222,
            ..Default::default()
        };
        assert_eq!(
            proactive_chat_id(&cfg),
            Some(222),
            "fallback to allow_chat_id"
        );
    }

    #[test]
    fn test_proactive_chat_id_none_when_unset() {
        let cfg = TelegramConfig::default();
        assert_eq!(proactive_chat_id(&cfg), None, "no target -> skip");
    }
}
