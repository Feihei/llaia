use crate::agent::sink::{run_turn, OutputSink};
use crate::agent::{Agent, AgentRegistry, MediaKind};
use crate::channels::Channel;
use crate::config::QqConfig;
use crate::provider::{ChatMessage, ContentPart, ImageUrlContent};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use base64::Engine;
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
/// 2026-08 官方变更记录：接口调用域名统一为 api.bot.qq.com（旧 api.sgroup.qq.com 仍可用）
const DEFAULT_API_BASE: &str = "https://api.bot.qq.com";
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

/// 被动回复锚点：回复用户消息带 `msg_id`，响应互动事件（按钮点击）带 `event_id`，
/// 二者互斥（官方发消息接口字段说明）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyAnchor {
    /// 回复用户消息（C2C_MESSAGE_CREATE 事件的 d.id）
    Message(String),
    /// 响应互动事件（INTERACTION_CREATE 的 WS dispatch 外层 payload id；
    /// 事件体 d.id 仅用于 PUT /interactions/{id} 回应——单聊实测 d.id 作
    /// event_id 会被判 40034025「请求参数event_id无效」）
    Event(String),
}

/// 从 WS payload 提取的按钮点击互动（INTERACTION_CREATE type=11 消息按钮回调）
#[derive(Debug, Clone)]
pub struct IncomingInteraction {
    /// 互动 ID（事件体 d.id）：用于 PUT /interactions/{id} 回应（同一 id 只能回应一次）
    pub interaction_id: String,
    /// 事件 ID（WS dispatch 外层 payload id）：用于被动消息 event_id 锚点
    pub event_id: String,
    /// 点击者 openid（仅单聊场景有值）
    pub user_openid: String,
    /// 按钮的 action.data（发送按钮时设置的回调数据）
    pub button_data: String,
    /// 按钮的 id 字段
    pub button_id: String,
}

impl TokenState {
    fn is_valid(&self) -> bool {
        match self.expires_at {
            Some(t) => Instant::now() + Duration::from_secs(TOKEN_REFRESH_MARGIN) < t,
            None => false,
        }
    }
}

/// 从 USER.md 的 `- qq: <openid>` 行解析 owner openid（cron 主动推送兜底）。
/// 找不到返回 None。
fn parse_openid_from_user_md(workspace: &Path) -> Option<String> {
    let user_path = workspace.join("USER.md");
    let content = std::fs::read_to_string(&user_path).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("- qq:") {
            let id = rest.trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// channel 运行时状态文件路径（`workspace/channel_state.json`）。
/// 存放各 channel 自动捕获的"默认推送目标"（如 `{"qq": {"owner_openid": "..."}}`），
/// 由 channel 自己维护，与 USER.md（用户信息内容）职责分离。
fn channel_state_path(ws: &Path) -> PathBuf {
    ws.join("channel_state.json")
}

/// 从 channel_state.json 读 qq 的 owner openid（自动捕获的持久化值）。
async fn read_owner_openid_from_state(ws: &Path) -> Option<String> {
    let content = tokio::fs::read_to_string(channel_state_path(ws))
        .await
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;
    v.get("qq")?
        .get("owner_openid")?
        .as_str()
        .map(str::to_string)
}

/// 把 qq 的 owner openid 写入 channel_state.json（read-modify-write，保留其它 channel 字段）。
async fn write_owner_openid_to_state(ws: &Path, openid: &str) -> Result<()> {
    let path = channel_state_path(ws);
    let mut v: serde_json::Value = match tokio::fs::read_to_string(&path).await {
        Ok(c) => serde_json::from_str(&c).unwrap_or_else(|_| serde_json::json!({})),
        Err(_) => serde_json::json!({}),
    };
    v["qq"]["owner_openid"] = serde_json::Value::String(openid.to_string());
    tokio::fs::write(&path, serde_json::to_string_pretty(&v)?).await?;
    Ok(())
}

/// QQ 分片上传要求的文件校验值：(整文件 MD5, 整文件 SHA1, 前 10002432 字节 MD5)。
/// md5_10m 的口径按官方文档为 10002432 字节（不是惯用的 10MB=10485760），不足则取整文件。
fn qq_file_checksums(bytes: &[u8]) -> (String, String, String) {
    const MD5_10M_LEN: usize = 10_002_432;
    let md5 = qq_md5_hex(bytes);
    let sha1 = {
        use sha1::Digest;
        let mut h = sha1::Sha1::new();
        h.update(bytes);
        qq_hex(&h.finalize())
    };
    let md5_10m = qq_md5_hex(&bytes[..bytes.len().min(MD5_10M_LEN)]);
    (md5, sha1, md5_10m)
}

fn qq_md5_hex(bytes: &[u8]) -> String {
    use md5::Digest;
    let mut h = md5::Md5::new();
    h.update(bytes);
    qq_hex(&h.finalize())
}

fn qq_hex(digest: &[u8]) -> String {
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// 解析审批按钮的 action.data。约定格式 `ap:ok:<id>` / `ap:deny:<id>`，
/// 其余一律拒绝（键盘 data 也可能被旧客户端/平台注入其它内容）。
/// 返回 (是否批准, 审批 id)。
fn parse_approval_button_data(data: &str) -> Option<(bool, &str)> {
    let mut parts = data.split(':');
    match (parts.next()?, parts.next()?, parts.next()?, parts.next()) {
        ("ap", "ok", id, None) if !id.is_empty() => Some((true, id)),
        ("ap", "deny", id, None) if !id.is_empty() => Some((false, id)),
        _ => None,
    }
}

/// 键盘发送被 QQ「明确拒绝」（键盘/权限类校验错误，非网络类）才降级纯文本重发：
/// 40034029 内联键盘行列超限、304062 订阅按钮数超限、40034127 无 markdown 模板权限。
/// 网络类失败不做降级重发——首次请求可能已实际送达，换 msg_seq 重发会造成重复投递。
fn is_keyboard_rejection(err: &str) -> bool {
    err.contains("40034029") || err.contains("304062") || err.contains("40034127")
}

/// 构造审批按钮键盘（QQ 内嵌键盘自定义布局）。
///
/// - 每个待审批 id 一行：[✅ 通过 style=4] [❌ 拒绝 style=3]，label ≤10 字符
/// - 两个按钮共享 `group_id`：点击其一后同组按钮变灰（仅 action.type=1 生效）
/// - `permission.type=2`（所有人可点）：`type=0`+`specify_user_ids` 的客户端本地
///   权限校验跨端不一致——iOS 不认 C2C openid、本地拦截"无权限操作"且不发回调
///   （2026-09-19 实测）；点击者身份由 handle_interaction 的服务端 owner 校验兜底
/// - 回调数据 `ap:ok:<id>` / `ap:deny:<id>`，点击后平台推送 INTERACTION_CREATE
/// - `unsupport_tips`：旧客户端不渲染按钮时的提示文案
fn approval_keyboard(approval_ids: &[String]) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = approval_ids
        .iter()
        .map(|id| {
            let group = format!("ap-{id}");
            let action = |data: String| {
                serde_json::json!({
                    "type": 1,
                    "permission": {
                        "type": 2,
                    },
                    "data": data,
                    "unsupport_tips": "QQ 版本过低，请回复 /ok 或 /deny",
                })
            };
            serde_json::json!({
                "buttons": [
                    {
                        "id": format!("ok-{id}"),
                        "render_data": {
                            "label": "✅ 通过",
                            "visited_label": "已批准",
                            "style": 4,
                        },
                        "action": action(format!("ap:ok:{id}")),
                        "group_id": group,
                    },
                    {
                        "id": format!("deny-{id}"),
                        "render_data": {
                            "label": "❌ 拒绝",
                            "visited_label": "已拒绝",
                            "style": 3,
                        },
                        "action": action(format!("ap:deny:{id}")),
                        "group_id": group,
                    },
                ]
            })
        })
        .collect();
    serde_json::json!({ "content": { "rows": rows } })
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
    /// owner openid：从收到的 C2C 消息中跟踪，用于 cron 主动推送
    owner_openid: Arc<Mutex<Option<String>>>,
    /// 主 agent workspace（用于读 USER.md 解析 owner openid 兜底）
    workspace: Option<PathBuf>,
    /// 审批 id → 注册时回合的回复锚点（通常是原用户消息 msg_id）。
    /// 按钮点击续跑时优先用它锚定回复：msg_id 被动回复窗口 60 分钟/4 次，
    /// 实测可靠；event_id（INTERACTION_CREATE 外层 id）作无登记时兜底。
    approval_anchors: Mutex<HashMap<String, ReplyAnchor>>,
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
            owner_openid: Arc::new(Mutex::new(None)),
            workspace: None,
            approval_anchors: Mutex::new(HashMap::new()),
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
            owner_openid: Arc::new(Mutex::new(None)),
            workspace: None,
            approval_anchors: Mutex::new(HashMap::new()),
        }
    }

    /// 登记审批 id 与注册时回合的回复锚点（QqSink::on_approval_request 调用）。
    /// 同 id 重复登记以后写为准。
    async fn remember_approval_anchor(&self, approval_id: &str, anchor: ReplyAnchor) {
        self.approval_anchors
            .lock()
            .await
            .insert(approval_id.to_string(), anchor);
    }

    /// 取走审批的登记锚点（按钮点击续跑时调用，取后即删）。
    /// 无登记（如 serve 重启后点击旧按钮）返回 None。
    async fn take_approval_anchor(&self, approval_id: &str) -> Option<ReplyAnchor> {
        self.approval_anchors.lock().await.remove(approval_id)
    }

    /// 注入主 agent workspace（用于 cron 主动推送时读 channel_state.json / USER.md 解析 owner openid）。
    /// serve_cmd 构造 QqChannel 后调用。
    pub fn with_workspace(mut self, ws: PathBuf) -> Self {
        self.workspace = Some(ws);
        self
    }

    /// 主动推送消息：用于 cron 任务结果推送。
    /// openid 来源：① config `[channels.qq] owner_openid` ② 已跟踪的 owner openid（用户发过消息）
    /// ③ channel_state.json ④ USER.md 的 `- qq:` 字段（legacy 兜底）。
    /// 都没有则 log + 返回 Ok（不报错，cron 不因此失败）。
    pub async fn send_proactive(&self, message: &str) -> Result<()> {
        let openid = self.resolve_owner_openid().await;
        match openid {
            Some(id) => self.send_c2c_message(&id, message, None).await,
            None => {
                tracing::warn!(
                    "cron push to qq skipped: no owner openid (set [channels.qq] owner_openid, or wait for an inbound C2C message)"
                );
                Ok(())
            }
        }
    }

    /// 解析 owner openid。优先级（从高到低）：
    /// ① config `[channels.qq] owner_openid`（手动指定，最高优先）
    /// ② 本进程已跟踪的 owner openid（收到过 C2C 消息）
    /// ③ `workspace/channel_state.json` 的 `qq.owner_openid`（自动捕获的持久化值，跨重启）
    /// ④ USER.md 的 `- qq:` 行（legacy 兜底，兼容旧版本写入的绑定）
    async fn resolve_owner_openid(&self) -> Option<String> {
        if !self.config.owner_openid.trim().is_empty() {
            return Some(self.config.owner_openid.trim().to_string());
        }
        if let Some(id) = self.owner_openid.lock().await.clone() {
            return Some(id);
        }
        if let Some(ws) = self.workspace.as_ref() {
            if let Some(id) = read_owner_openid_from_state(ws).await {
                return Some(id);
            }
        }
        self.workspace
            .as_ref()
            .and_then(|ws| parse_openid_from_user_md(ws))
    }

    /// 把 owner openid 持久化到 `workspace/channel_state.json`（自动捕获值，跨重启），
    /// 与 USER.md（用户信息内容）职责分离。写失败时静默降级，不影响消息处理主流程。
    async fn persist_owner_openid(&self, openid: &str) {
        let ws = match self.workspace.as_ref() {
            Some(ws) => ws,
            None => return,
        };
        if let Err(e) = write_owner_openid_to_state(ws, openid).await {
            tracing::warn!(
                error = %e,
                path = %channel_state_path(ws).display(),
                "persist qq openid to channel_state.json failed"
            );
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
            return Err(anyhow!(
                "getAppAccessToken failed: status={}, body={}",
                status,
                text
            ));
        }
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| anyhow!("getAppAccessToken parse json: {}, body={}", e, text))?;
        let access_token = v
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("getAppAccessToken missing access_token: {}", text))?
            .to_string();
        let expires_in = v.get("expires_in").and_then(|v| v.as_u64()).unwrap_or(7200);
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
                // 首次失败若是 token 过期，强制刷新 token 重试一次。
                // QQ 网关的过期响应形态不统一：既有英文 "token not exist or expire"，
                // 也有 {"code":11244,"message":"AccessToken无效或过期"} 的中文形态，
                // 后者曾被漏匹配导致重连循环带着失效 token 无限重试
                let err_text = first_err.to_string();
                if err_text.contains("token not exist or expire")
                    || err_text.contains("11244")
                    || err_text.contains("AccessToken无效或过期")
                {
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

    /// 从 WS payload 中提取按钮点击互动（INTERACTION_CREATE）。
    /// 只处理 type=11（消息按钮回调）；其余互动类型（消息反馈/清空会话/授权等）
    /// 与本 channel 的按钮无关，忽略。需 user_openid（单聊场景）与 button_data。
    pub fn extract_interaction(payload: &serde_json::Value) -> Option<IncomingInteraction> {
        let t = payload.get("t").and_then(|v| v.as_str())?;
        if t != "INTERACTION_CREATE" {
            return None;
        }
        let d = payload.get("d")?;
        // 11 = 消息按钮回调（INLINE_KEYBOARD）
        if d.get("type").and_then(|v| v.as_i64()) != Some(11) {
            return None;
        }
        let interaction_id = d.get("id")?.as_str()?.to_string();
        // 被动消息 event_id 用 WS dispatch 外层 payload id（官方 Payload 通用结构的
        // "id 事件id"）；事件体 d.id 实测只被互动回调接口认，发消息会判 40034025。
        // 外层 id 缺失（异常 payload）时退回 d.id 兜底。
        let event_id = payload
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or(interaction_id.as_str())
            .to_string();
        let user_openid = d.get("user_openid")?.as_str()?.to_string();
        let resolved = d.get("data")?.get("resolved")?;
        let button_data = resolved
            .get("button_data")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let button_id = resolved
            .get("button_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if button_data.is_empty() {
            return None;
        }
        Some(IncomingInteraction {
            interaction_id,
            event_id,
            user_openid,
            button_data,
            button_id,
        })
    }

    /// 通过 HTTPS API 发送 C2C 消息（msg_id 被动回复，无键盘）。
    pub async fn send_c2c_message(
        &self,
        user_openid: &str,
        content: &str,
        msg_id: Option<&str>,
    ) -> Result<()> {
        self.send_c2c_anchored(
            user_openid,
            content,
            msg_id.map(|m| ReplyAnchor::Message(m.to_string())).as_ref(),
            None,
        )
        .await
    }

    /// 全量发送入口：`anchor` 区分 msg_id（回复消息）/ event_id（响应互动事件）/
    /// 无锚点（主动消息）三种形态，`keyboard` 附加内嵌按钮键盘（按钮审批用）。
    /// 带 keyboard 时消息自动升级为 msg_type=2 markdown（QQ 平台要求键盘必须
    /// 挂在 markdown 消息上，纯文本会被静默丢弃）。
    ///
    /// keyboard 被 QQ 以键盘/权限类错误码明确拒绝时自动降级纯文本重发一次
    /// （按钮是增强，文本提示 + `/ok` `/deny` 命令始终兜底，审批不因按钮失败而阻断）。
    pub async fn send_c2c_anchored(
        &self,
        user_openid: &str,
        content: &str,
        anchor: Option<&ReplyAnchor>,
        keyboard: Option<&serde_json::Value>,
    ) -> Result<()> {
        let url = format!("{}/v2/users/{}/messages", self.api_base, user_openid);
        // msg_seq 在本次调用内取一次：重试与键盘降级重发共用，
        // 保证「同 msg_id + 同 msg_seq」的幂等去重语义不变（err_code 40054005）
        let seq = match anchor {
            Some(ReplyAnchor::Message(_)) => self.next_msg_seq(),
            _ => 0,
        };
        let build_body = |kb: Option<&serde_json::Value>| {
            // QQ 平台约束：keyboard 只能挂在 markdown 消息上。msg_type=0 纯文本 +
            // keyboard 时 QQ 不报错但直接丢弃键盘（按钮不渲染、无错误可降级），
            // 因此带键盘的消息必须走 msg_type=2 + markdown.content
            // （2026-08 官方确认单聊/群聊自定义 markdown 已对所有机器人开放）。
            let mut body = if kb.is_some() {
                serde_json::json!({
                    "msg_type": 2,
                    "markdown": { "content": content },
                })
            } else {
                serde_json::json!({
                    "content": content,
                    "msg_type": 0,  // 0 = 文本
                })
            };
            match anchor {
                Some(ReplyAnchor::Message(id)) => {
                    body["msg_id"] = serde_json::Value::String(id.clone());
                    // 被动回复必须带递增 msg_seq，否则同一 msg_id 的后续回复被去重
                    body["msg_seq"] = serde_json::Value::from(seq);
                }
                Some(ReplyAnchor::Event(id)) => {
                    body["event_id"] = serde_json::Value::String(id.clone());
                }
                None => {}
            }
            if let Some(k) = kb {
                body["keyboard"] = k.clone();
            }
            body
        };

        match self
            .post_message_with_retries(&url, &build_body(keyboard))
            .await
        {
            Ok(()) => Ok(()),
            Err(e) if keyboard.is_some() && is_keyboard_rejection(&e.to_string()) => {
                tracing::warn!(error = %e, "qq keyboard rejected, falling back to plain text");
                self.post_message_with_retries(&url, &build_body(None))
                    .await
            }
            Err(e) => Err(e),
        }
    }

    /// POST 消息体到 QQ 消息接口。3 次指数退避重试：200ms / 400ms / 800ms；
    /// token 过期（code 11244）仅强制刷新一次，避免无限循环。
    async fn post_message_with_retries(&self, url: &str, body: &serde_json::Value) -> Result<()> {
        let delays = [200u64, 400, 800];
        let mut last_err: Option<anyhow::Error> = None;
        // token 过期（code 11244）仅强制刷新一次，避免无限循环
        let mut token_refreshed = false;
        for (attempt, delay) in delays.iter().enumerate() {
            // 每次重试都取 token：首次失效或刷新后会拿到新 token
            let token = self.get_access_token().await?;
            let resp = self
                .http
                .post(url)
                .header("Authorization", format!("QQBot {}", token))
                .header("Content-Type", "application/json")
                .json(body)
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    tracing::debug!(attempt, "qq send ok");
                    return Ok(());
                }
                Ok(r) => {
                    let status = r.status();
                    let text = r.text().await.unwrap_or_default();
                    // QQ 返回 token 失效（11244）时，强制刷新一次后重试，
                    // 否则会带着旧 token 把 3 次重试全耗光（podman/native 双实例共用凭证时高频触发）
                    if !token_refreshed
                        && (text.contains("token not exist or expire") || text.contains("11244"))
                    {
                        tracing::warn!(
                            attempt,
                            "qq send rejected: token expired, force refreshing and retrying"
                        );
                        self.invalidate_token().await;
                        token_refreshed = true;
                    } else {
                        tracing::warn!(attempt, %status, %text, "qq send failed, retrying");
                    }
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

    /// 响应互动事件（按钮点击）。INTERACTION_CREATE type=11/12 必须在 ~3 秒内回应，
    /// 否则用户客户端一直 loading；同一 interaction_id 只能回应一次，超时失效。
    /// code：0=成功 1=操作失败 2=操作频繁 3=重复操作 4=没有权限 5=仅管理员。
    /// 单次尝试（3s 时限内重试无意义），失败仅返回 Err 由调用方决定是否兜底。
    pub async fn ack_interaction(&self, interaction_id: &str, code: i32) -> Result<()> {
        let url = format!("{}/interactions/{}", self.api_base, interaction_id);
        let body = serde_json::json!({ "code": code });
        let send = |token: String| {
            self.http
                .put(&url)
                .header("Authorization", format!("QQBot {}", token))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
        };
        let token = self.get_access_token().await?;
        let resp = send(token).await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let text = resp.text().await.unwrap_or_default();
        // token 失效：刷新一次后重试（失败的回应不消费 interaction_id，重试合法）
        if text.contains("token not exist or expire") || text.contains("11244") {
            tracing::warn!("qq ack interaction rejected: token expired, retrying once");
            self.invalidate_token().await;
            let token = self.get_access_token().await?;
            let resp = send(token).await?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status.is_success() {
                return Ok(());
            }
            return Err(anyhow!(
                "ack interaction failed: status={}, body={}",
                status,
                text
            ));
        }
        Err(anyhow!(
            "ack interaction failed: status={}, body={}",
            status,
            text
        ))
    }

    /// 向 QQ 用户发送媒体文件（图片或文件）。
    /// 流程：上传到 QQ 文件服务拿 file_info → 发送 msg_type=7 富媒体消息。
    ///
    /// 上传路径选择：
    /// - 图片：base64 直传（历史验证可用的小体量路径），失败时降级分片上传重试；
    /// - 文件：官方分片上传（base64 通道对大文件返回 500/850012 "call inner proxy error"，
    ///   且文件类上传从未在 base64 通道成功过；分片是文档对「本地文件/大文件」的正路，上限 200MB）。
    pub async fn send_media_to_user(
        &self,
        user_openid: &str,
        path: &str,
        kind: crate::agent::MediaKind,
        msg_id: Option<&str>,
    ) -> Result<()> {
        let file_type = match kind {
            crate::agent::MediaKind::Image => 1, // 1=图片
            crate::agent::MediaKind::File => 4,  // 4=文件
        };

        // 读文件内容
        let file_bytes = tokio::fs::read(path)
            .await
            .map_err(|e| anyhow!("read media file {:?}: {}", path, e))?;
        if file_bytes.is_empty() {
            // QQ 对空文件返回 "file data empty" (code 10000)，这里提前给出明确错误
            return Err(anyhow!("media file is empty (0 bytes): {:?}", path));
        }
        // 缺 file_name 时 QQ 端显示为未命名文件（社区实践确认该字段必带）
        let file_name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());

        // 1. 上传媒体到 QQ 文件服务拿 file_info
        let file_info = match kind {
            crate::agent::MediaKind::Image => {
                match self
                    .upload_media_base64(user_openid, file_type, &file_name, &file_bytes)
                    .await
                {
                    Ok(fi) => fi,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "qq base64 media upload failed, falling back to chunked upload"
                        );
                        self.upload_media_chunked(user_openid, file_type, &file_name, &file_bytes)
                            .await?
                    }
                }
            }
            crate::agent::MediaKind::File => {
                self.upload_media_chunked(user_openid, file_type, &file_name, &file_bytes)
                    .await?
            }
        };
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

        let mut token = self.get_access_token().await?;
        let mut resp = self
            .http
            .post(&send_url)
            .header("Authorization", format!("QQBot {}", token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;
        let mut status = resp.status();
        let mut text = resp.text().await.unwrap_or_default();
        // token 失效（11244）：刷新一次后重试
        if !status.is_success()
            && (text.contains("token not exist or expire") || text.contains("11244"))
        {
            tracing::warn!("qq media send rejected: token expired, force refreshing and retrying");
            self.invalidate_token().await;
            token = self.get_access_token().await?;
            resp = self
                .http
                .post(&send_url)
                .header("Authorization", format!("QQBot {}", token))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await?;
            status = resp.status();
            text = resp.text().await.unwrap_or_default();
        }
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

    /// base64 直传（`/v2/users/{openid}/files` + `file_data`）。
    /// v2 接口规范：JSON body 传 `file_type` + `file_data`(base64) 或 `url` + `srv_send_msg`，
    /// 不再支持 multipart 文件上传（旧实现发 multipart `file` 字段被拒，40093006/40093007）。
    /// 见 https://bot.q.qq.com/wiki/develop/api-v2/server-inter/message/send-receive/rich-media.html
    async fn upload_media_base64(
        &self,
        user_openid: &str,
        file_type: i32,
        file_name: &str,
        file_bytes: &[u8],
    ) -> Result<String> {
        // 构造上传 JSON body 的闭包（token 过期刷新后需重建）
        let build_body = || {
            let file_data = base64::engine::general_purpose::STANDARD.encode(file_bytes);
            serde_json::json!({
                "file_type": file_type,
                "file_data": file_data,
                "file_name": file_name,
                // false：只上传拿 file_info，随后走 msg_type=7 消息接口（与 send 路径一致）
                "srv_send_msg": false,
            })
        };

        let upload_url = format!("{}/v2/users/{}/files", self.api_base, user_openid);
        let do_upload = || async {
            let token = self.get_access_token().await?;
            let resp = self
                .http
                .post(&upload_url)
                .header("Authorization", format!("QQBot {}", token))
                .json(&build_body())
                .send()
                .await?;
            Ok::<_, anyhow::Error>(resp)
        };
        let resp = do_upload().await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let text = if !status.is_success() {
            // token 失效（11244）：刷新一次后重试
            if text.contains("token not exist or expire") || text.contains("11244") {
                tracing::warn!(
                    "qq media upload rejected: token expired, force refreshing and retrying"
                );
                self.invalidate_token().await;
                let resp = do_upload().await?;
                let s = resp.status();
                let t = resp.text().await.unwrap_or_default();
                if !s.is_success() {
                    return Err(anyhow!("upload media failed: status={}, body={}", s, t));
                }
                t
            } else {
                return Err(anyhow!(
                    "upload media failed: status={}, body={}",
                    status,
                    text
                ));
            }
        } else {
            text
        };
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| anyhow!("parse upload response: {}, body={}", e, text))?;
        v.get("file_info")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("upload response missing file_info: {}", text))
    }

    /// 官方分片上传（文档对「本地文件/大文件」的推荐路径，上限 200MB）：
    /// `upload_prepare` 拿任务与预签名分片地址 → 逐片 PUT + `upload_part_finish`
    /// → `/files` 带 `upload_id` 合并换 `file_info`。
    ///
    /// 背景：大文件走 base64 `file_data` 单包上传时，QQ 内部代理返回
    /// 500/850012 "call inner proxy error"（非文档错误码），文件类上传实测失败。
    async fn upload_media_chunked(
        &self,
        user_openid: &str,
        file_type: i32,
        file_name: &str,
        file_bytes: &[u8],
    ) -> Result<String> {
        let (md5_hex, sha1_hex, md5_10m_hex) = qq_file_checksums(file_bytes);

        // 1. 预上传：声明文件元信息，换 upload_id 与预签名分片地址列表
        let prepare_url = format!("{}/v2/users/{}/upload_prepare", self.api_base, user_openid);
        let prepare_body = serde_json::json!({
            "file_type": file_type,
            "file_size": file_bytes.len().to_string(),
            "file_name": file_name,
            "md5": md5_hex,
            "sha1": sha1_hex,
            // 文档口径：文件前 10002432 字节的 MD5（不足则整文件）
            "md5_10m": md5_10m_hex,
        });
        let v = self.post_json_authed(&prepare_url, &prepare_body).await?;
        let upload_id = v
            .get("upload_id")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow!("upload_prepare response missing upload_id: {}", v))?
            .to_string();
        let parts = v
            .get("parts")
            .and_then(|x| x.as_array())
            .ok_or_else(|| anyhow!("upload_prepare response missing parts: {}", v))?;
        if parts.is_empty() {
            return Err(anyhow!("upload_prepare returned no parts: {}", v));
        }

        // 2. 逐片 PUT 到预签名地址 + 通知该分片完成
        let finish_url = format!(
            "{}/v2/users/{}/upload_part_finish",
            self.api_base, user_openid
        );
        let mut offset = 0usize;
        for part in parts {
            let index = part
                .get("index")
                .and_then(|x| x.as_i64())
                .ok_or_else(|| anyhow!("part missing index: {}", part))?;
            let presigned = part
                .get("presigned_url")
                .and_then(|x| x.as_str())
                .ok_or_else(|| anyhow!("part missing presigned_url: {}", part))?;
            let size: usize = part
                .get("block_size")
                .and_then(|x| x.as_str())
                .unwrap_or("0")
                .parse()
                .map_err(|e| anyhow!("part block_size invalid: {}, {}", part, e))?;
            if offset >= file_bytes.len() {
                return Err(anyhow!(
                    "server returned parts beyond file size (offset={}, len={})",
                    offset,
                    file_bytes.len()
                ));
            }
            let end = (offset + size).min(file_bytes.len());
            let chunk = &file_bytes[offset..end];

            // PUT 分片：预签名地址自带鉴权，不带 QQBot token；失败重试一次
            let mut last_err: Option<anyhow::Error> = None;
            for attempt in 1..=2 {
                match self
                    .http
                    .put(presigned)
                    .header("Content-Type", "application/octet-stream")
                    .body(chunk.to_vec())
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        last_err = None;
                        break;
                    }
                    Ok(resp) => {
                        let status = resp.status();
                        let text = resp.text().await.unwrap_or_default();
                        last_err = Some(anyhow!(
                            "upload part {} failed: status={}, body={}",
                            index,
                            status,
                            text
                        ));
                    }
                    Err(e) => {
                        last_err = Some(anyhow!("upload part {} error: {}", index, e));
                    }
                }
                if attempt == 1 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            if let Some(e) = last_err {
                return Err(e);
            }

            // 通知分片完成（block_size 按文档为字符串，md5 为该分片校验值）
            let finish_body = serde_json::json!({
                "upload_id": upload_id,
                "part_index": index,
                "block_size": chunk.len().to_string(),
                "md5": qq_md5_hex(chunk),
            });
            self.post_json_authed(&finish_url, &finish_body).await?;
            offset = end;
        }
        if offset != file_bytes.len() {
            return Err(anyhow!(
                "parts covered {} bytes but file is {} bytes",
                offset,
                file_bytes.len()
            ));
        }

        // 3. 带 upload_id 请求 /files 走分片合并，换 file_info
        let files_url = format!("{}/v2/users/{}/files", self.api_base, user_openid);
        let merge_body = serde_json::json!({
            "file_type": file_type,
            "file_name": file_name,
            "upload_id": upload_id,
            "srv_send_msg": false,
        });
        let v = self.post_json_authed(&files_url, &merge_body).await?;
        v.get("file_info")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("files merge response missing file_info: {}", v))
    }

    /// 带 QQBot token 的 JSON POST：token 失效（11244）时刷新一次并重试。
    /// 非 2xx 返回含 status 与 body 的错误；成功返回解析后的 JSON。
    async fn post_json_authed(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let mut token_refreshed = false;
        loop {
            let token = self.get_access_token().await?;
            let resp = self
                .http
                .post(url)
                .header("Authorization", format!("QQBot {}", token))
                .json(body)
                .send()
                .await?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status.is_success() {
                return serde_json::from_str(&text)
                    .map_err(|e| anyhow!("parse qq api response: {}, body={}", e, text));
            }
            if !token_refreshed
                && (text.contains("token not exist or expire") || text.contains("11244"))
            {
                tracing::warn!(url = %url, "qq api rejected: token expired, force refreshing and retrying");
                self.invalidate_token().await;
                token_refreshed = true;
                continue;
            }
            return Err(anyhow!(
                "qq api {} failed: status={}, body={}",
                url,
                status,
                text
            ));
        }
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
        let safe_name = att.filename.replace(['/', '\\'], "_");
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
        registry: &Arc<AgentRegistry>,
    ) -> Result<()> {
        let text = &incoming.text;
        let msg_id = &incoming.msg_id;
        tracing::info!(
            user = %user_openid,
            text = %text,
            attachments = incoming.attachments.len(),
            "qq received message"
        );

        // /steer（plan.md #I）：turn 运行中非阻塞投递（不 lock agent，
        // 缓冲经 registry 共享 Arc）；空闲则降级为普通消息（剥前缀继续往下走）。
        let steer_stripped;
        let text: &str = if let Some(rest) = crate::commands::slash::split_steer(text) {
            let running = self.running_stops.lock().await.contains_key(user_openid);
            if running {
                if rest.is_empty() {
                    let _ = self
                        .send_c2c_message(
                            user_openid,
                            "usage: /steer <message>",
                            Some(msg_id.as_str()),
                        )
                        .await;
                } else {
                    // ADR-0032：steer 投给频道当前附着的实例（turn 持 agent 锁，
                    // 经 InstanceHandle 缓存的 steer Arc 投递，不取 Agent 锁）
                    let steer = registry.instances.attached("qq").await.steer_buffer.clone();
                    steer.lock().unwrap().push_back(rest.to_string());
                    let _ = self
                        .send_c2c_message(
                            user_openid,
                            &format!("[steer queued: {}]", rest),
                            Some(msg_id.as_str()),
                        )
                        .await;
                }
                return Ok(());
            }
            // 空闲：剥前缀当普通消息跑（对齐 OpenClaw 降级）；空参给 usage
            if rest.is_empty() {
                let _ = self
                    .send_c2c_message(
                        user_openid,
                        "usage: /steer <message>",
                        Some(msg_id.as_str()),
                    )
                    .await;
                return Ok(());
            }
            steer_stripped = rest.to_string();
            &steer_stripped
        } else {
            text
        };

        // 斜杠命令：在锁内处理，把输出发回用户（忽略附件）
        if text.trim().starts_with('/') {
            return self
                .handle_slash_text(
                    agent,
                    registry,
                    user_openid,
                    ReplyAnchor::Message(msg_id.clone()),
                    text,
                )
                .await;
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
                    text: text.to_string(),
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
                                        text: format!("[image preprocess failed: {}]", e),
                                    });
                                }
                            }
                        } else {
                            // 非图片文件：仅告知 agent 附件名和保存路径
                            parts.push(ContentPart::Text {
                                text: format!(
                                    "[attachment saved to workspace/uploads/{}_{}]",
                                    msg_id, att.filename
                                ),
                            });
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, filename = %att.filename, "download attachment failed");
                        parts.push(ContentPart::Text {
                            text: format!("[attachment download failed: {}]", e),
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
            anchor: ReplyAnchor::Message(msg_id.to_string()),
            buffer: String::new(),
            tool_names: Vec::new(),
            notified_tools: false,
            approval_ids: Vec::new(),
        });

        registry.set_delivery(
            self.clone()
                .pusher()
                .map(crate::tools::delegate::DeliveryTarget::Pusher),
        );
        let turn_result = run_turn(agent.clone(), user_msg, "qq".into(), sink, stop).await;

        // 清理中断信号注册
        {
            let mut stops = self.running_stops.lock().await;
            stops.remove(user_openid);
        }

        turn_result?;
        Ok(())
    }

    /// 斜杠命令处理（普通 C2C 消息与按钮点击互动共用）。
    /// `anchor` 决定回复锚点：消息回复带 msg_id，互动响应带 event_id；
    /// Resume 续跑 turn 的 sink 也沿用同一锚点。
    async fn handle_slash_text(
        self: Arc<Self>,
        agent: &Arc<Mutex<Agent>>,
        registry: &Arc<AgentRegistry>,
        user_openid: &str,
        anchor: ReplyAnchor,
        text: &str,
    ) -> Result<()> {
        // /btw（plan.md #H ①）：turn 运行中读不到上下文（锁被 turn 持有，
        // 排队等锁会变成「turn 结束后才答」），明确拒绝优于静默延迟。
        if text.trim().to_ascii_lowercase().starts_with("/btw")
            && self.running_stops.lock().await.contains_key(user_openid)
        {
            let _ = self
                .send_c2c_anchored(
                    user_openid,
                    "[btw busy: a turn is running; ask again when idle]",
                    Some(&anchor),
                    None,
                )
                .await;
            return Ok(());
        }
        // /stop：中断当前正在执行的 turn（不需要 lock agent，避免被长任务阻塞）
        if text.trim().eq_ignore_ascii_case("/stop") {
            let notify = {
                let mut stops = self.running_stops.lock().await;
                stops.remove(user_openid)
            };
            if let Some(n) = notify {
                n.notify_one();
                let _ = self
                    .send_c2c_anchored(
                        user_openid,
                        "[current task interrupted]",
                        Some(&anchor),
                        None,
                    )
                    .await;
            } else {
                let _ = self
                    .send_c2c_anchored(
                        user_openid,
                        "[no task currently running]",
                        Some(&anchor),
                        None,
                    )
                    .await;
            }
            return Ok(());
        }
        // ADR-0032 T3：/session 家族在实例层接管（切线 = 换绑附着，不原地改写
        // agent）。在取 agent 锁之前拦截——busy 判定（try_lock）要求此刻未持锁。
        if let Some(outcome) =
            crate::commands::slash::try_session_command(text, registry, "qq").await
        {
            match outcome? {
                crate::commands::slash::SlashOutcome::Handled(msg) => {
                    let _ = self
                        .send_c2c_anchored(user_openid, &msg, Some(&anchor), None)
                        .await;
                }
                _ => {}
            }
            return Ok(());
        }
        let outcome = {
            let mut a = agent.lock().await;
            crate::commands::slash::try_handle(text, &mut a, Some(registry.clone())).await?
        };
        match outcome {
            crate::commands::slash::SlashOutcome::Exit => {
                // QQ 下忽略 /exit，不退出
                let _ = self
                    .send_c2c_anchored(
                        user_openid,
                        "[/exit not available on QQ channel]",
                        Some(&anchor),
                        None,
                    )
                    .await;
            }
            crate::commands::slash::SlashOutcome::Handled(msg) => {
                let _ = self
                    .send_c2c_anchored(user_openid, &msg, Some(&anchor), None)
                    .await;
            }
            crate::commands::slash::SlashOutcome::NotSlash => {
                // 不会走到这里（调用方已检查 starts_with '/'）
            }
            crate::commands::slash::SlashOutcome::Resume { notice, message } => {
                let _ = self
                    .send_c2c_anchored(user_openid, &notice, Some(&anchor), None)
                    .await;
                let stop = Arc::new(Notify::new());
                {
                    let mut stops = self.running_stops.lock().await;
                    stops.insert(user_openid.to_string(), stop.clone());
                }
                let sink = Box::new(QqSink {
                    qq: self.clone(),
                    user_openid: user_openid.to_string(),
                    anchor: anchor.clone(),
                    buffer: String::new(),
                    tool_names: Vec::new(),
                    notified_tools: false,
                    approval_ids: Vec::new(),
                });
                registry.set_delivery(
                    self.clone()
                        .pusher()
                        .map(crate::tools::delegate::DeliveryTarget::Pusher),
                );
                let turn_result = run_turn(
                    agent.clone(),
                    crate::provider::ChatMessage::user(&message),
                    "qq".into(),
                    sink,
                    stop,
                )
                .await;
                {
                    let mut stops = self.running_stops.lock().await;
                    stops.remove(user_openid);
                }
                turn_result?;
            }
        }
        Ok(())
    }

    /// 处理按钮点击互动（INTERACTION_CREATE type=11，intent 1<<26）：
    /// owner 校验（键盘 permission 之外的代码层双保险）→ 解析 button_data
    /// （`ap:ok:<id>` / `ap:deny:<id>`）→ ack 互动 → 复用 /ok /deny 的
    /// 审批解析与续跑通路。审批已被文本命令解决时按重复操作处理（幂等）。
    async fn handle_interaction(
        self: Arc<Self>,
        agent: &Arc<Mutex<Agent>>,
        registry: &Arc<AgentRegistry>,
        inter: &IncomingInteraction,
    ) -> Result<()> {
        let owner = self.resolve_owner_openid().await;
        if owner.as_deref() != Some(inter.user_openid.as_str()) {
            tracing::warn!(
                user = %inter.user_openid,
                id = %inter.interaction_id,
                "qq button click from non-owner, ignored"
            );
            let _ = self.ack_interaction(&inter.interaction_id, 4).await;
            return Ok(());
        }
        let Some((approve, approval_id)) = parse_approval_button_data(&inter.button_data) else {
            tracing::warn!(data = %inter.button_data, "unrecognized qq button_data");
            let _ = self.ack_interaction(&inter.interaction_id, 1).await;
            return Ok(());
        };
        // 幂等：pending 已被文本 /ok /deny 或 /cancel 处理 → code=3（重复操作），
        // 提示文本走 event_id 被动回复，不再触发续跑
        let exists = {
            let a = agent.lock().await;
            a.approval_gate
                .list()
                .await
                .iter()
                .any(|p| p.id == approval_id)
        };
        if !exists {
            tracing::info!(id = %approval_id, "qq button click on already-resolved approval");
            let _ = self.ack_interaction(&inter.interaction_id, 3).await;
            let anchor = self
                .take_approval_anchor(approval_id)
                .await
                .unwrap_or_else(|| ReplyAnchor::Event(inter.event_id.clone()));
            let _ = self
                .send_c2c_anchored(
                    &inter.user_openid,
                    &format!("[no pending approval {} — already resolved]", approval_id),
                    Some(&anchor),
                    None,
                )
                .await;
            return Ok(());
        }
        // ack 必须在 ~3s 内发出（否则客户端一直 loading），先 ack 再做耗时的审批解析
        let _ = self.ack_interaction(&inter.interaction_id, 0).await;
        let text = format!("/{} {}", if approve { "ok" } else { "deny" }, approval_id);
        // 续跑回复锚点：优先登记的原消息 msg_id（60 分钟/4 次被动窗口，实测可靠），
        // 无登记（serve 重启等）退回 event_id（INTERACTION_CREATE 外层 id）
        let anchor = self
            .take_approval_anchor(approval_id)
            .await
            .unwrap_or_else(|| ReplyAnchor::Event(inter.event_id.clone()));
        self.handle_slash_text(agent, registry, &inter.user_openid, anchor, &text)
            .await
    }
}

/// QQ 输出 sink：累积 chunk 后分片发送。
/// 工具通知收敛为单条：首个 ToolStart 发「🔧 正在调用工具…」，后续静默；
/// 全部工具名累积到回复开头一并返回（QQ 消息不可编辑，多条/更新只会刷屏）。
struct QqSink {
    qq: Arc<QqChannel>,
    user_openid: String,
    /// 被动回复锚点：普通消息回复 msg_id，按钮审批续跑回复 event_id
    anchor: ReplyAnchor,
    buffer: String,
    /// 本回合已调用的工具名（按序去重）
    tool_names: Vec<String>,
    /// 是否已发过工具通知（每回合最多一条）
    notified_tools: bool,
    /// 本回合注册的待审批 id（on_approval_request 收集）；
    /// on_done 时第一条消息附「通过/拒绝」按钮键盘
    approval_ids: Vec<String>,
}

#[async_trait]
impl OutputSink for QqSink {
    async fn on_chunk(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }
    // 工具执行出错时即时反馈，避免整轮合并后用户只看到"🔧 calling tools..."却无结果
    async fn on_tool_result(&mut self, output: &str) {
        let trimmed = output.trim();
        if trimmed.starts_with("[error:") {
            let _ = self
                .qq
                .send_c2c_anchored(&self.user_openid, trimmed, Some(&self.anchor), None)
                .await;
        }
    }
    // 长任务心跳：墙钟每 10 分钟一次并带上已运行分钟数，避免用户误以为卡死
    async fn on_keepalive(&mut self, elapsed: std::time::Duration) {
        let mins = elapsed.as_secs() / 60;
        let _ = self
            .qq
            .send_c2c_anchored(
                &self.user_openid,
                &crate::channels::keepalive_notice(mins),
                Some(&self.anchor),
                None,
            )
            .await;
    }
    // 单轮超时自动中断：向用户说明原因，避免静默卡死
    async fn on_auto_stopped(&mut self, reason: &str) {
        let _ = self
            .qq
            .send_c2c_anchored(&self.user_openid, reason, Some(&self.anchor), None)
            .await;
    }
    async fn on_tool_start(&mut self, name: &str) {
        if !self.tool_names.iter().any(|n| n == name) {
            self.tool_names.push(name.to_string());
        }
        if !self.notified_tools {
            self.notified_tools = true;
            let _ = self
                .qq
                .send_c2c_anchored(
                    &self.user_openid,
                    "🔧 calling tools...",
                    Some(&self.anchor),
                    None,
                )
                .await;
        }
    }
    // 待审批注册：记录 id，on_done 时附按钮键盘（runner 只在 NeedsApproval 分支发送）；
    // 同时把本回合锚点（原用户消息 msg_id）登记给按钮点击续跑用。
    // 工具名/摘要只服务于 WebUI 卡片，QQ 只用 id。
    async fn on_approval_request(&mut self, req: &crate::agent::sink::ApprovalRequest<'_>) {
        self.approval_ids.push(req.id.to_string());
        self.qq
            .remember_approval_anchor(req.id, self.anchor.clone())
            .await;
    }
    async fn on_media(&mut self, path: &str, kind: MediaKind) {
        // 富媒体接口只支持 msg_id 被动回复；event_id 锚点（按钮续跑）下发主动消息
        let media_msg_id = match &self.anchor {
            ReplyAnchor::Message(id) => Some(id.as_str()),
            ReplyAnchor::Event(_) => None,
        };
        if let Err(e) = self
            .qq
            .send_media_to_user(&self.user_openid, path, kind, media_msg_id)
            .await
        {
            tracing::error!(error = %e, path = path, "failed to send media");
            let _ = self
                .qq
                .send_c2c_anchored(
                    &self.user_openid,
                    &format!("[failed to send media: {}]", e),
                    Some(&self.anchor),
                    None,
                )
                .await;
        }
    }
    async fn on_done(&mut self) {
        // agent 可能只调工具无文本输出，buffer 为空时给占位回复
        // 否则 QQ 会因 content="" 返回 304061 invalid content
        let body = if self.buffer.trim().is_empty() {
            tracing::warn!(
                total_len = self.buffer.len(),
                "agent reply empty, sending placeholder"
            );
            "[done (no text output)]".to_string()
        } else {
            // 模型回复常以 \n\n 开头（思考结束留白/markdown 习惯），
            // CLI 下被终端吞掉不显眼，QQ 原样发送会显示成两个空行。
            // 这里 trim 前导换行符（保留首行前导空格，避免影响 markdown 缩进）。
            self.buffer.trim_start_matches(['\n', '\r']).to_string()
        };
        // 调用过工具时把清单拼在回复开头，同一条消息反馈（不额外发消息）
        let reply = if self.tool_names.is_empty() {
            body
        } else {
            format!("🔧 called: {}\n\n{}", self.tool_names.join(", "), body)
        };
        let chunks = split_reply(&reply, 1800);
        tracing::info!(
            chunks = chunks.len(),
            total_len = reply.len(),
            "sending reply"
        );
        // 审批键盘：本回合有 NeedsApproval 注册时，第一条消息附「通过/拒绝」按钮。
        // 键盘 permission=所有人可点（iOS 对 specify_user_ids 本地拦截），点击者身份
        // 由 handle_interaction 的服务端 owner 校验兜底，无需在此解析 owner。
        let keyboard = if self.approval_ids.is_empty() {
            None
        } else {
            Some(approval_keyboard(&self.approval_ids))
        };
        for (i, chunk) in chunks.iter().enumerate() {
            if chunk.trim().is_empty() {
                continue;
            }
            // 只有第一片带锚点（被动回复）+ 按钮，后续片用主动消息
            let (anchor, kb) = if i == 0 {
                (Some(&self.anchor), keyboard.as_ref())
            } else {
                (None, None)
            };
            if let Err(e) = self
                .qq
                .send_c2c_anchored(&self.user_openid, chunk, anchor, kb)
                .await
            {
                tracing::error!(error = %e, chunk = i, "failed to send chunk after retries");
            }
        }
    }
    async fn on_error(&mut self, message: &str) {
        let err_msg = if self.buffer.is_empty() {
            format!("[internal error: {}]", message)
        } else {
            // 保留已生成文本，错误追加（同样 trim 前导换行）
            self.buffer.trim_start_matches(['\n', '\r']).to_string()
        };
        let chunks = split_reply(&err_msg, 1800);
        for (i, chunk) in chunks.iter().enumerate() {
            let anchor = if i == 0 { Some(&self.anchor) } else { None };
            if let Err(e) = self
                .qq
                .send_c2c_anchored(&self.user_openid, chunk, anchor, None)
                .await
            {
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
    fn pusher(self: Arc<Self>) -> Option<Arc<dyn crate::cron::ProactivePusher>> {
        Some(self as Arc<dyn crate::cron::ProactivePusher>)
    }
    async fn run(self: Arc<Self>, registry: Arc<AgentRegistry>) -> Result<()> {
        tracing::info!(app_id = %self.config.app_id, "QqChannel starting");

        // 外层重连循环：ws 断开后等待 5 秒重连，避免 serve 进程退出
        loop {
            match self.clone().run_connection(&registry).await {
                Ok(()) => tracing::warn!("qq ws connection closed, will reconnect"),
                Err(e) => {
                    tracing::error!(error = %e, "qq ws connection ended with error, will reconnect")
                }
            }
            tracing::info!("reconnecting in 5 seconds...");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
}

impl QqChannel {
    /// 单次连接的完整生命周期：建连 → IDENTIFY → 消息/心跳循环 → 断开
    /// （ADR-0032：agent 不再缓存——每条消息/互动动态解析频道附着的实例）
    async fn run_connection(
        self: Arc<Self>,
        registry: &Arc<AgentRegistry>,
    ) -> Result<()> {
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
                                        // C2C 消息 (1<<25) + 互动事件 (1<<26，按钮点击回调)
                                        "intents": (1 << 25) | (1 << 26),
                                        "shard": [0, 1],
                                        "properties": {
                                            "$os": std::env::consts::OS,
                                            "$browser": "llaia",
                                            "$device": "llaia"
                                        }
                                    }
                                });
                                let _ = write.send(Message::Text(identify.to_string().into())).await;
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
                                    // 跟踪 owner openid（用于 cron 主动推送）
                                    *self.owner_openid.lock().await = Some(user_openid.clone());
                                    // 持久化到 USER.md，重启后 cron 主动推送仍可解析
                                    self.persist_owner_openid(&user_openid).await;
                                    let this = self.clone();
                                    // ADR-0032：每条消息动态解析频道附着的实例
                                    let agent =
                                        registry.instances.attached("qq").await.agent.clone();
                                    let registry = registry.clone();
                                    tokio::spawn(async move {
                                        if let Err(e) = this
                                            .handle_user_message(&agent, &user_openid, &incoming, &registry)
                                            .await
                                        {
                                            tracing::error!(error = %e, "handle_user_message failed");
                                        }
                                    });
                                    continue;
                                }

                                // 按钮点击互动（intent 1<<26）：走按钮审批通路
                                if let Some(inter) = Self::extract_interaction(&payload) {
                                    let this = self.clone();
                                    // ADR-0032：审批 pending 会阻塞切线（busy 守卫），
                                    // 所以按钮到达时频道必然仍附着在注册该审批的实例上
                                    let agent =
                                        registry.instances.attached("qq").await.agent.clone();
                                    let registry = registry.clone();
                                    tokio::spawn(async move {
                                        if let Err(e) = this
                                            .clone()
                                            .handle_interaction(&agent, &registry, &inter)
                                            .await
                                        {
                                            tracing::error!(error = %e, "handle_interaction failed");
                                            // 兜底 ack 避免客户端 loading 挂死
                                            //（若已 ack 过则本调用失败，无副作用）
                                            let _ = this
                                                .ack_interaction(&inter.interaction_id, 1)
                                                .await;
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
                        if let Err(e) = write.send(Message::Text(hb.to_string().into())).await {
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

#[async_trait]
impl crate::cron::ProactivePusher for QqChannel {
    async fn push(&self, message: &str) -> Result<()> {
        self.send_proactive(message).await
    }
}

#[cfg(test)]
mod button_approval_tests {
    use super::*;

    #[test]
    fn test_parse_approval_button_data() {
        assert_eq!(parse_approval_button_data("ap:ok:ap7"), Some((true, "ap7")));
        assert_eq!(
            parse_approval_button_data("ap:deny:ap1"),
            Some((false, "ap1"))
        );
        // 非法格式一律拒绝（键盘 data 可能被平台/旧客户端注入其它内容）
        assert_eq!(parse_approval_button_data("ap:ok"), None);
        assert_eq!(parse_approval_button_data("ap:ok:"), None);
        assert_eq!(parse_approval_button_data("x:ok:ap1"), None);
        assert_eq!(parse_approval_button_data("ap:ok:ap1:extra"), None);
        assert_eq!(parse_approval_button_data(""), None);
    }

    #[test]
    fn test_is_keyboard_rejection() {
        assert!(is_keyboard_rejection(
            "status: 400 Bad Request, body: {\"code\":40034029}"
        ));
        assert!(is_keyboard_rejection("body contains 304062 somewhere"));
        assert!(is_keyboard_rejection("body contains 40034127 somewhere"));
        // 网络类/其它错误不降级重发（首次请求可能已实际送达，防重复投递）
        assert!(!is_keyboard_rejection("status: 500, body: internal error"));
        assert!(!is_keyboard_rejection("error sending request"));
    }

    #[test]
    fn test_approval_keyboard_shape() {
        let kb = approval_keyboard(&["ap3".to_string()]);
        let rows = kb["content"]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        let buttons = rows[0]["buttons"].as_array().unwrap();
        assert_eq!(buttons.len(), 2);
        let (ok_btn, deny_btn) = (&buttons[0], &buttons[1]);
        // 回调动作：type=1 回调按钮 + 约定 data 格式
        assert_eq!(ok_btn["action"]["type"], 1);
        assert_eq!(ok_btn["action"]["data"], "ap:ok:ap3");
        assert_eq!(deny_btn["action"]["data"], "ap:deny:ap3");
        // 同组互斥：点击其一后同组按钮变灰
        assert_eq!(ok_btn["group_id"], "ap-ap3");
        assert_eq!(deny_btn["group_id"], "ap-ap3");
        // permission=所有人可点（iOS 对 specify_user_ids 本地拦截），身份由服务端校验
        assert_eq!(ok_btn["action"]["permission"]["type"], 2);
        assert!(ok_btn["action"]["permission"]
            .get("specify_user_ids")
            .is_none());
        // label ≤10 字符（QQ 上限）
        for b in buttons {
            let label = b["render_data"]["label"].as_str().unwrap();
            assert!(label.chars().count() <= 10, "label too long: {}", label);
        }
    }

    #[test]
    fn test_approval_keyboard_multiple_rows() {
        let ids = vec!["ap1".to_string(), "ap2".to_string()];
        let kb = approval_keyboard(&ids);
        let rows = kb["content"]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["buttons"][0]["action"]["data"], "ap:ok:ap1");
        assert_eq!(rows[1]["buttons"][1]["action"]["data"], "ap:deny:ap2");
    }

    #[test]
    fn test_extract_interaction() {
        // 正常按钮点击（单聊）
        let payload = serde_json::json!({
            "op": 0,
            "t": "INTERACTION_CREATE",
            "id": "EVENT_WS_OUTER_ID",
            "d": {
                "id": "1b13d569-4610",
                "type": 11,
                "scene": "c2c",
                "chat_type": 2,
                "user_openid": "USER_A",
                "data": {
                    "type": 11,
                    "resolved": { "button_data": "ap:ok:ap2", "button_id": "ok-ap2" }
                }
            }
        });
        let inter = QqChannel::extract_interaction(&payload).unwrap();
        assert_eq!(inter.interaction_id, "1b13d569-4610");
        assert_eq!(inter.event_id, "EVENT_WS_OUTER_ID");
        assert_eq!(inter.user_openid, "USER_A");
        assert_eq!(inter.button_data, "ap:ok:ap2");
        assert_eq!(inter.button_id, "ok-ap2");

        // 外层 id 缺失（异常 payload）→ event_id 退回 d.id
        let payload = serde_json::json!({
            "t": "INTERACTION_CREATE",
            "d": {
                "id": "1b13d569-4610",
                "type": 11,
                "user_openid": "USER_A",
                "data": { "resolved": { "button_data": "ap:ok:ap2" } }
            }
        });
        let inter = QqChannel::extract_interaction(&payload).unwrap();
        assert_eq!(inter.event_id, "1b13d569-4610");

        // 非按钮互动（type=13 消息反馈）不处理
        let payload = serde_json::json!({
            "t": "INTERACTION_CREATE",
            "d": { "id": "x", "type": 13, "user_openid": "U", "data": { "resolved": {} } }
        });
        assert!(QqChannel::extract_interaction(&payload).is_none());

        // 缺 user_openid（群聊场景）不处理
        let payload = serde_json::json!({
            "t": "INTERACTION_CREATE",
            "d": {
                "id": "x",
                "type": 11,
                "group_openid": "G",
                "data": { "resolved": { "button_data": "ap:ok:ap1" } }
            }
        });
        assert!(QqChannel::extract_interaction(&payload).is_none());

        // 空 button_data 不处理
        let payload = serde_json::json!({
            "t": "INTERACTION_CREATE",
            "d": {
                "id": "x",
                "type": 11,
                "user_openid": "U",
                "data": { "resolved": { "button_data": "" } }
            }
        });
        assert!(QqChannel::extract_interaction(&payload).is_none());

        // 非 INTERACTION_CREATE 事件不处理
        let payload = serde_json::json!({ "t": "C2C_MESSAGE_CREATE", "d": {} });
        assert!(QqChannel::extract_interaction(&payload).is_none());
    }
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
        assert!(
            parts.len() > 1,
            "expected multiple chunks, got {}",
            parts.len()
        );
        assert!(parts[0].ends_with("```"), "first chunk should end with ```");
        assert!(
            parts[1].starts_with("```rust"),
            "second chunk should start with ```rust"
        );
    }
}

#[cfg(test)]
mod proactive_tests {
    use super::*;
    use crate::config::QqConfig;
    use tempfile::tempdir;

    #[test]
    fn test_parse_openid_from_user_md_found() {
        let dir = tempdir().unwrap();
        let user_md = "# 基本信息\n\n- 姓名：测试\n- qq: OPENID_ABC123\n- email: x@y\n";
        std::fs::write(dir.path().join("USER.md"), user_md).unwrap();
        let openid = parse_openid_from_user_md(dir.path());
        assert_eq!(openid.as_deref(), Some("OPENID_ABC123"));
    }

    #[test]
    fn test_parse_openid_from_user_md_missing() {
        let dir = tempdir().unwrap();
        let user_md = "# 基本信息\n\n- 姓名：测试\n- email: x@y\n";
        std::fs::write(dir.path().join("USER.md"), user_md).unwrap();
        assert!(parse_openid_from_user_md(dir.path()).is_none());
    }

    #[test]
    fn test_parse_openid_from_user_md_no_file() {
        let dir = tempdir().unwrap();
        assert!(parse_openid_from_user_md(dir.path()).is_none());
    }

    #[tokio::test]
    async fn test_send_proactive_no_openid_returns_ok() {
        // 无 owner openid 且无 workspace：应 log + 返回 Ok（不报错）
        let qq = QqChannel::new(QqConfig::default());
        let result = qq.send_proactive("cron result").await;
        assert!(
            result.is_ok(),
            "send_proactive without openid should not error"
        );
    }

    #[tokio::test]
    async fn test_resolve_owner_openid_from_user_md_legacy() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("USER.md"),
            "# 基本信息\n\n- qq: WS_OPENID\n",
        )
        .unwrap();
        let qq = QqChannel::new(QqConfig::default()).with_workspace(dir.path().to_path_buf());
        let openid = qq.resolve_owner_openid().await;
        assert_eq!(openid.as_deref(), Some("WS_OPENID"));
    }

    #[tokio::test]
    async fn test_persist_owner_openid_writes_channel_state() {
        let dir = tempdir().unwrap();
        let qq = QqChannel::new(QqConfig::default()).with_workspace(dir.path().to_path_buf());
        qq.persist_owner_openid("OPENID_001").await;
        let state_path = channel_state_path(dir.path());
        let content = std::fs::read_to_string(&state_path).unwrap();
        assert!(
            content.contains("OPENID_001"),
            "state file should store openid"
        );
        // USER.md 不应被写入（职责分离：状态归状态文件，USER.md 归用户信息）
        assert!(
            !dir.path().join("USER.md").exists(),
            "persist must not write USER.md"
        );
    }

    #[tokio::test]
    async fn test_persist_owner_openid_idempotent() {
        let dir = tempdir().unwrap();
        let qq = QqChannel::new(QqConfig::default()).with_workspace(dir.path().to_path_buf());
        qq.persist_owner_openid("OPENID_001").await;
        let state_path = channel_state_path(dir.path());
        let content = std::fs::read_to_string(&state_path).unwrap();
        qq.persist_owner_openid("OPENID_001").await;
        let again = std::fs::read_to_string(&state_path).unwrap();
        assert_eq!(content, again, "idempotent persist should not rewrite");
    }

    #[tokio::test]
    async fn test_persist_owner_openid_replaces_state_value() {
        let dir = tempdir().unwrap();
        let qq = QqChannel::new(QqConfig::default()).with_workspace(dir.path().to_path_buf());
        qq.persist_owner_openid("OLD_OPENID").await;
        qq.persist_owner_openid("NEW_OPENID").await;
        let resolved = qq.resolve_owner_openid().await;
        assert_eq!(resolved.as_deref(), Some("NEW_OPENID"));
        // 重新构造（模拟重启，内存态清空）后 state 兜底仍能读到最新值
        let qq2 = QqChannel::new(QqConfig::default()).with_workspace(dir.path().to_path_buf());
        let resolved2 = qq2.resolve_owner_openid().await;
        assert_eq!(
            resolved2.as_deref(),
            Some("NEW_OPENID"),
            "state value survives restart"
        );
    }

    #[tokio::test]
    async fn test_resolve_owner_openid_config_takes_priority() {
        let dir = tempdir().unwrap();
        let cfg = QqConfig {
            owner_openid: "CFG_OPENID".into(),
            ..Default::default()
        };
        std::fs::write(
            dir.path().join("USER.md"),
            "# 基本信息\n\n- qq: MD_OPENID\n",
        )
        .unwrap();
        let qq = QqChannel::new(cfg).with_workspace(dir.path().to_path_buf());
        qq.persist_owner_openid("STATE_OPENID").await; // state 也有值
        let openid = qq.resolve_owner_openid().await;
        assert_eq!(
            openid.as_deref(),
            Some("CFG_OPENID"),
            "config owner_openid wins over state and USER.md"
        );
    }
}

#[cfg(test)]
mod checksum_tests {
    use super::*;

    #[test]
    fn test_qq_file_checksums_small_file() {
        // RFC 1321 / FIPS 180-1 经典测试向量
        let (md5, sha1, md5_10m) = qq_file_checksums(b"abc");
        assert_eq!(md5, "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
        // 不足 10002432 字节时 md5_10m == 整文件 MD5
        assert_eq!(md5_10m, md5);
    }

    #[test]
    fn test_qq_file_checksums_md5_10m_boundary() {
        // 超过 10002432 字节时，md5_10m 只取前 10002432 字节
        let mut bytes = vec![0u8; 10_002_433];
        bytes[10_002_432] = 0xFF; // 最后一字节不参与 md5_10m
        let (md5, _, md5_10m) = qq_file_checksums(&bytes);
        let expected_prefix = qq_md5_hex(&bytes[..10_002_432]);
        assert_eq!(md5_10m, expected_prefix);
        assert_ne!(md5_10m, md5, "整文件 MD5 与截断 MD5 不应相同");
    }

    #[test]
    fn test_qq_md5_hex_empty() {
        // 空输入的 MD5（分片逻辑不应走到，但函数本身须有定义）
        assert_eq!(qq_md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    }
}
