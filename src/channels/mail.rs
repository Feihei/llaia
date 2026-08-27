use crate::agent::sink::{run_turn, OutputSink};
use crate::agent::{Agent, AgentRegistry, MediaKind};
use crate::channels::Channel;
use crate::config::MailConfig;
use crate::provider::ChatMessage;
use anyhow::{anyhow, Context, Result};
use async_imap::Client;
use async_trait::async_trait;
use futures_util::StreamExt;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use mailparse::MailHeaderMap;
use rustls::pki_types::ServerName;
use rustls::RootCertStore;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Notify};
use tokio_rustls::TlsConnector;

/// IMAP 连接类型：隐式 TLS（993）经 rustls 包装后的 session。
type MailSession = async_imap::Session<tokio_rustls::client::TlsStream<TcpStream>>;

/// 邮箱频道：IMAP 轮询收件 + SMTP 发信。
///
/// 设计要点：
/// - 单用户安全锁：仅响应 `owner_email` 发来的邮件，避免自动回复外部信件造成邮件循环。
/// - 轮询式（默认 30s 一次）：比 IMAP IDLE 简单、稳，且便于 reload_all 时自然退出（外层 loop 持有 config 快照）。
/// - 收信走 IMAP（隐式 TLS），发信走 SMTP（465 隐式 TLS / 587 STARTTLS），均用 rustls（无 OpenSSL 依赖）。
pub struct MailChannel {
    config: MailConfig,
    /// 主 agent workspace（用于保存附件 / 兜底读 owner_email）
    workspace: Option<PathBuf>,
}

impl MailChannel {
    pub fn new(config: MailConfig) -> Self {
        Self {
            config,
            workspace: None,
        }
    }

    /// 注入 workspace（serve_cmd 构造后调用）
    pub fn with_workspace(mut self, ws: PathBuf) -> Self {
        self.workspace = Some(ws);
        self
    }
}

#[async_trait]
impl Channel for MailChannel {
    fn pusher(self: Arc<Self>) -> Option<Arc<dyn crate::cron::ProactivePusher>> {
        Some(self as Arc<dyn crate::cron::ProactivePusher>)
    }
    async fn run(self: Arc<Self>, registry: Arc<AgentRegistry>) -> Result<()> {
        let agent = registry.main.clone();
        tracing::info!(
            server = %self.config.imap_server,
            mailbox = %self.config.mailbox,
            "MailChannel starting"
        );
        let poll = self.config.poll_interval_secs.max(5);
        // 轮询循环：单轮出错只 log 不退出，避免把整个 serve 进程拖垮。
        loop {
            if let Err(e) = self.clone().poll_once(&agent, &registry).await {
                tracing::error!(error = %e, "mail poll failed, will retry");
            }
            tokio::time::sleep(Duration::from_secs(poll)).await;
        }
    }
}

impl MailChannel {
    /// 单次轮询：拉未读 → 逐封处理（调 agent）→ 标记已读 → 退出。
    async fn poll_once(
        self: Arc<Self>,
        agent: &Arc<Mutex<Agent>>,
        registry: &Arc<AgentRegistry>,
    ) -> Result<()> {
        let cfg = self.config.clone();
        let mut session = connect_imap_session(&cfg).await?;

        session
            .select(&cfg.mailbox)
            .await
            .with_context(|| format!("select mailbox '{}' failed", cfg.mailbox))?;

        let uids = session
            .uid_search("UNSEEN")
            .await
            .context("uid_search UNSEEN failed")?;
        if uids.is_empty() {
            let _ = session.logout().await;
            return Ok(());
        }

        // 一次性取出所有未读邮件的 RFC822 体，后面处理不再依赖 session
        let uid_list: Vec<u32> = uids.into_iter().collect();
        let uid_str = uid_list
            .iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mut fetched: Vec<(u32, Vec<u8>)> = Vec::new();
        {
            // fetch_stream 持有 session 的可变借用，必须在其 drop 前完成收集，
            // 否则 mark-seen 阶段的 session.uid_store 会因借用冲突编译失败。
            let mut fetch_stream = session
                .uid_fetch(&uid_str, "RFC822 UID")
                .await
                .context("uid_fetch RFC822 failed")?;
            while let Some(item) = fetch_stream.next().await {
                let item = item.context("fetch item stream error")?;
                let uid = item.uid.unwrap_or(0);
                let body = item.body().map(|b| b.to_vec()).unwrap_or_default();
                if !body.is_empty() {
                    fetched.push((uid, body));
                }
            }
        } // fetch_stream 在此 drop，释放对 session 的借用

        let mut processed_uids: Vec<u32> = Vec::new();
        for (uid, body) in fetched {
            match self
                .clone()
                .process_message(agent, &cfg, &body, registry)
                .await
            {
                Ok(()) => processed_uids.push(uid),
                Err(e) => tracing::error!(error = %e, uid, "process mail failed, skipped"),
            }
        }

        if cfg.mark_seen && !processed_uids.is_empty() {
            let ids = processed_uids
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");
            // 标记已读失败只告警，不影响这一轮结果
            if let Err(e) = session.uid_store(&ids, "+FLAGS (\\Seen)").await {
                tracing::warn!(error = %e, "mark seen failed");
            }
        }
        let _ = session.logout().await;
        Ok(())
    }

    /// 解析并交给 agent 处理一封邮件，回复通过 SMTP 发回发件人。
    async fn process_message(
        self: Arc<Self>,
        agent: &Arc<Mutex<Agent>>,
        cfg: &MailConfig,
        body: &[u8],
        registry: &Arc<AgentRegistry>,
    ) -> Result<()> {
        let parsed = mailparse::parse_mail(body).context("parse mail failed")?;
        let subject = parsed
            .headers
            .get_first_value("Subject")
            .unwrap_or_default();
        let from_raw = parsed.headers.get_first_value("From").unwrap_or_default();
        let from_addr = extract_email_address(&from_raw);

        // 单用户安全锁
        if !cfg.owner_email.is_empty() {
            let owner = cfg.owner_email.to_lowercase();
            if !from_addr.to_lowercase().contains(&owner) {
                tracing::info!(from = %from_addr, "mail ignored: sender is not owner_email");
                return Ok(());
            }
        }

        let text_body = extract_text_body(&parsed);
        tracing::info!(from = %from_addr, subject = %subject, "mail received");

        let mut text = String::new();
        if !subject.is_empty() {
            text.push_str(&format!("[email subject] {}\n\n", subject));
        }
        text.push_str(&text_body);

        // 附件：保存到 <workspace>/uploads，并告诉 agent 路径；超过 max_attachment_mb 则跳过并提示。
        let mut attachments: Vec<(String, Vec<u8>)> = Vec::new();
        collect_attachments(&parsed, &mut attachments);
        if !attachments.is_empty() {
            let uploads_dir = self.workspace.as_ref().map(|w| w.join("uploads"));
            let max_bytes = cfg.max_attachment_mb as usize * 1024 * 1024;
            for (fname, bytes) in &attachments {
                if bytes.len() > max_bytes {
                    text.push_str(&format!(
                        "\n[attachment {} exceeds the {}MB limit, skipped]\n",
                        fname, cfg.max_attachment_mb
                    ));
                    continue;
                }
                match &uploads_dir {
                    Some(dir) => {
                        if let Err(e) = tokio::fs::create_dir_all(dir).await {
                            tracing::warn!(error = %e, "create uploads dir failed");
                            continue;
                        }
                        let safe = fname.replace(['/', '\\'], "_");
                        let path = dir.join(&safe);
                        match tokio::fs::write(&path, bytes).await {
                            Ok(()) => text
                                .push_str(&format!("\n[attachment saved: {}]\n", path.display())),
                            Err(e) => text.push_str(&format!(
                                "\n[attachment {} save failed: {}]\n",
                                fname, e
                            )),
                        }
                    }
                    None => text.push_str(&format!(
                        "\n[attachment {} ({} bytes, no workspace, not saved)]\n",
                        fname,
                        bytes.len()
                    )),
                }
            }
        }

        let user_msg = ChatMessage::user(text);
        let sink = Box::new(MailSink {
            config: cfg.clone(),
            reply_to: from_addr,
            subject: subject.clone(),
            buffer: String::new(),
        });
        let stop = Arc::new(Notify::new());
        registry.set_delivery(
            self.clone()
                .pusher()
                .map(crate::tools::delegate::DeliveryTarget::Pusher),
        );
        run_turn(agent.clone(), user_msg, "mail".into(), sink, stop).await?;
        Ok(())
    }

    /// 主动推送：cron 任务结果通过邮件发给 owner_email。
    pub async fn send_proactive(&self, message: &str) -> Result<()> {
        let owner = self.config.owner_email.clone();
        if owner.is_empty() {
            tracing::warn!("mail proactive push skipped: owner_email not configured");
            return Ok(());
        }
        let sink = MailSink {
            config: self.config.clone(),
            reply_to: owner,
            subject: String::new(),
            buffer: String::new(),
        };
        sink.send_reply(message).await
    }
}

/// 建立 IMAP 隐式 TLS 连接并完成登录。
async fn connect_imap_session(cfg: &MailConfig) -> Result<MailSession> {
    let root_store: RootCertStore = webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect();
    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let connector = TlsConnector::from(std::sync::Arc::new(client_config));
    let server_name = ServerName::try_from(cfg.imap_server.clone())
        .map_err(|e| anyhow!("invalid IMAP server name '{}': {}", cfg.imap_server, e))?;

    let tcp = TcpStream::connect((cfg.imap_server.as_str(), cfg.imap_port))
        .await
        .with_context(|| format!("TCP connect {}:{} failed", cfg.imap_server, cfg.imap_port))?;
    let tls = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| anyhow!("IMAP TLS handshake failed: {}", e))?;

    let client = Client::new(tls);
    let session = match client.login(&cfg.imap_user, &cfg.imap_pass).await {
        Ok(s) => s,
        Err((e, _client)) => return Err(anyhow!("IMAP login failed: {}", e)),
    };
    Ok(session)
}

/// 从 "Name <addr>" 或裸 "addr" 中提取邮箱地址。
fn extract_email_address(from: &str) -> String {
    if let Some(start) = from.find('<') {
        if let Some(end) = from.find('>') {
            if end > start {
                return from[start + 1..end].trim().to_string();
            }
        }
    }
    from.trim().to_string()
}

/// 递归提取正文：优先 text/plain，否则退回 text/html 或首个子部分。
fn extract_text_body(mail: &mailparse::ParsedMail) -> String {
    if mail.ctype.mimetype == "text/plain" {
        return mail.get_body().unwrap_or_default();
    }
    if mail.subparts.is_empty() {
        return mail.get_body().unwrap_or_default();
    }
    for sub in &mail.subparts {
        let t = extract_text_body(sub);
        if !t.trim().is_empty() {
            return t;
        }
    }
    mail.get_body().unwrap_or_default()
}

/// 递归收集邮件附件（Content-Disposition: attachment 的叶子部分）。
fn collect_attachments(mail: &mailparse::ParsedMail, out: &mut Vec<(String, Vec<u8>)>) {
    let disposition = mail
        .headers
        .get_first_value("Content-Disposition")
        .unwrap_or_default();
    if disposition.to_lowercase().starts_with("attachment") {
        let fname = parse_attachment_filename(&disposition).or_else(|| {
            mail.headers
                .get_first_value("Content-Type")
                .and_then(|ct| parse_attachment_filename(&ct))
        });
        let bytes = mail.get_body_raw().unwrap_or_default();
        out.push((fname.unwrap_or_else(|| "attachment.bin".to_string()), bytes));
        return;
    }
    if mail.ctype.mimetype.starts_with("multipart/") {
        for sub in &mail.subparts {
            collect_attachments(sub, out);
        }
    }
}

/// 从 Content-Disposition / Content-Type 头里抠出 filename。
fn parse_attachment_filename(header: &str) -> Option<String> {
    let re = regex::Regex::new(r#"filename="([^"]*)""#).ok()?;
    if let Some(cap) = re.captures(header) {
        let name = cap.get(1)?.as_str().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    let re2 = regex::Regex::new(r"filename=([^;]+)").ok()?;
    if let Some(cap) = re2.captures(header) {
        let name = cap.get(1)?.as_str().trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

/// 邮件回复 sink：累积 agent 输出，整轮结束后通过 SMTP 发回发件人。
struct MailSink {
    config: MailConfig,
    reply_to: String,
    subject: String,
    buffer: String,
}

#[async_trait]
impl OutputSink for MailSink {
    async fn on_chunk(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }
    async fn on_tool_start(&mut self, _name: &str) {
        // 邮件是异步的，无法流式回传；工具调用仅记日志
    }
    async fn on_media(&mut self, path: &str, _kind: MediaKind) {
        self.buffer
            .push_str(&format!("\n[generated file: {}]\n", path));
    }
    async fn on_done(&mut self) {
        let reply = if self.buffer.trim().is_empty() {
            "[done processing (no text output)]".to_string()
        } else {
            self.buffer.trim().to_string()
        };
        if let Err(e) = self.send_reply(&reply).await {
            tracing::error!(error = %e, "send mail reply failed");
        }
    }
    async fn on_error(&mut self, message: &str) {
        let msg = format!("[processing error] {}", message);
        if let Err(e) = self.send_reply(&msg).await {
            tracing::error!(error = %e, "send mail error reply failed");
        }
    }
    async fn on_interrupted(&mut self) {
        tracing::info!(to = %self.reply_to, "mail turn interrupted");
    }
}

impl MailSink {
    /// 通过 SMTP 发送一封回复邮件给 `reply_to`。
    async fn send_reply(&self, body: &str) -> Result<()> {
        let smtp_user = if self.config.smtp_user.is_empty() {
            self.config.imap_user.clone()
        } else {
            self.config.smtp_user.clone()
        };
        let smtp_pass = if self.config.smtp_pass.is_empty() {
            self.config.imap_pass.clone()
        } else {
            self.config.smtp_pass.clone()
        };
        let port = self.config.smtp_port;

        let builder = if port == 465 {
            // 465 隐式 TLS（连接即 TLS）
            AsyncSmtpTransport::<Tokio1Executor>::relay(&self.config.smtp_server)
                .map_err(|e| anyhow!("smtp relay build failed: {}", e))?
        } else {
            // 587 等走 STARTTLS
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.config.smtp_server)
                .map_err(|e| anyhow!("smtp starttls_relay build failed: {}", e))?
        };
        let builder = builder.port(port);
        let builder = if !smtp_user.is_empty() {
            builder.credentials(Credentials::new(smtp_user.clone(), smtp_pass))
        } else {
            builder
        };
        let mailer = builder.build();

        let from = Mailbox::new(
            Some(self.config.from_name.clone()),
            smtp_user
                .parse()
                .map_err(|e| anyhow!("invalid smtp_user '{}': {}", smtp_user, e))?,
        );
        let to = self
            .reply_to
            .parse::<lettre::Address>()
            .map_err(|e| anyhow!("invalid reply-to '{}': {}", self.reply_to, e))?;
        let to_box = Mailbox::new(None, to);
        let subject = if self.subject.is_empty() {
            "Reply".to_string()
        } else {
            format!("Re: {}", self.subject)
        };

        let email = Message::builder()
            .from(from)
            .to(to_box)
            .subject(subject)
            .body(body.to_string())
            .map_err(|e| anyhow!("build mail message failed: {}", e))?;

        mailer
            .send(email)
            .await
            .map_err(|e| anyhow!("smtp send failed: {}", e))?;
        Ok(())
    }
}

#[async_trait]
impl crate::cron::ProactivePusher for MailChannel {
    async fn push(&self, message: &str) -> Result<()> {
        self.send_proactive(message).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_email_address_with_angle() {
        assert_eq!(
            extract_email_address("Feihei <feihei@example.com>"),
            "feihei@example.com"
        );
    }

    #[test]
    fn test_extract_email_address_bare() {
        assert_eq!(extract_email_address("x@y.com"), "x@y.com");
    }

    #[test]
    fn test_extract_email_address_malformed() {
        // 缺 '>'：退回裸截取
        assert_eq!(extract_email_address("Name <x@y.com"), "Name <x@y.com");
    }

    #[test]
    fn test_extract_text_body_prefers_plain() {
        let raw = b"From: a@b.com\r\n\
Content-Type: multipart/alternative; boundary=BOUND\r\n\r\n\
--BOUND\r\nContent-Type: text/plain\r\n\r\nhello plain\r\n\
--BOUND\r\nContent-Type: text/html\r\n\r\n<html>hi html</html>\r\n\
--BOUND--\r\n";
        let parsed = mailparse::parse_mail(raw).unwrap();
        assert_eq!(extract_text_body(&parsed).trim(), "hello plain");
    }

    #[test]
    fn test_extract_text_body_plain_only() {
        let raw = b"From: a@b.com\r\nContent-Type: text/plain\r\n\r\njust text\r\n";
        let parsed = mailparse::parse_mail(raw).unwrap();
        assert_eq!(extract_text_body(&parsed).trim(), "just text");
    }
}
