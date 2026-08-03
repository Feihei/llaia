use crate::agent::sink::{OutputSink, run_turn};
use crate::agent::{AgentRegistry, MediaKind};
use crate::channels::Channel;
use crate::config::{Config, WebConfig};
use crate::image_utils;
use crate::provider::{ChatMessage, ContentPart, ImageUrlContent};
use async_trait::async_trait;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Multipart, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, Notify, RwLock};

/// WS 出向事件：扁平化 JSON，与 TurnEvent 一一对应 + 协议层事件
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebEvent {
    Chunk { delta: String },
    ToolStart { id: String, name: String },
    ToolResult { id: String, output: String },
    Media { path: String, kind: MediaKind },
    Done,
    Error { message: String },
    Interrupted,
    // 协议层
    Pong,
    AuthOk,
    AuthFailed { reason: String },
    Busy { reason: String },
}

/// turn 结束信号：on_done/on_error/on_interrupted 三个终态都发送
#[derive(Debug, Clone, Copy)]
pub struct TurnEndSignal;

/// Web 输出 sink：把 OutputSink 回调转成 WebEvent 推到 mpsc，WS 写 task 消费。
/// 持有 turn-end sender 让 WS handler 主循环感知 turn 结束以清理 current_turn。
pub struct WebSink {
    tx: mpsc::Sender<WebEvent>,
    turn_end_tx: mpsc::Sender<TurnEndSignal>,
}

impl WebSink {
    pub fn new(tx: mpsc::Sender<WebEvent>, turn_end_tx: mpsc::Sender<TurnEndSignal>) -> Self {
        Self { tx, turn_end_tx }
    }
}

#[async_trait]
impl OutputSink for WebSink {
    async fn on_chunk(&mut self, delta: &str) {
        let _ = self.tx.send(WebEvent::Chunk { delta: delta.into() }).await;
    }
    async fn on_tool_start(&mut self, name: &str) {
        let _ = self.tx.send(WebEvent::ToolStart { id: String::new(), name: name.into() }).await;
    }
    async fn on_tool_result(&mut self, output: &str) {
        let _ = self.tx.send(WebEvent::ToolResult { id: String::new(), output: output.into() }).await;
    }
    async fn on_media(&mut self, path: &str, kind: MediaKind) {
        let _ = self.tx.send(WebEvent::Media { path: path.into(), kind }).await;
    }
    async fn on_done(&mut self) {
        let _ = self.tx.send(WebEvent::Done).await;
        let _ = self.turn_end_tx.send(TurnEndSignal).await;
    }
    async fn on_error(&mut self, message: &str) {
        let _ = self.tx.send(WebEvent::Error { message: message.into() }).await;
        let _ = self.turn_end_tx.send(TurnEndSignal).await;
    }
    async fn on_interrupted(&mut self) {
        // 与 QqSink 一致：只 log，不回推 WS 帧（前端按钮状态本身体现中断）
        tracing::info!("web turn interrupted");
        let _ = self.turn_end_tx.send(TurnEndSignal).await;
    }
}

/// 共享状态：所有 handler 共用
#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<AgentRegistry>,
    pub config: Arc<RwLock<Config>>,
    pub config_path: std::path::PathBuf,
    pub workspace: std::path::PathBuf,
    pub token: Arc<String>,
}

/// 生成 32 字节随机 hex token
pub fn generate_token() -> String {
    let bytes: [u8; 32] = rand::thread_rng().gen();
    hex_encode(&bytes)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// 从请求中提取 token：优先 Authorization: Bearer，其次 cookie llaia_token，最后 query ?token=
pub fn extract_token(
    headers: &axum::http::HeaderMap,
    cookies: &str,
    query_token: Option<&str>,
) -> Option<String> {
    if let Some(auth) = headers.get(axum::http::header::AUTHORIZATION) {
        if let Ok(s) = auth.to_str() {
            if let Some(rest) = s.strip_prefix("Bearer ") {
                return Some(rest.to_string());
            }
        }
    }
    // cookie
    for kv in cookies.split(';') {
        let kv = kv.trim();
        if let Some(rest) = kv.strip_prefix("llaia_token=") {
            return Some(rest.to_string());
        }
    }
    query_token.map(|s| s.to_string())
}

/// 校验 token 是否匹配
pub fn check_token(provided: &str, expected: &str) -> bool {
    !expected.is_empty() && provided == expected
}

/// 解析相对路径到 base 内的绝对路径，拒绝 .. 逃逸和绝对路径。
/// 用于 uploads_dir 和 workspace 边界校验。
pub fn resolve_within(base: &Path, relative: &str) -> Result<PathBuf, String> {
    let p = Path::new(relative);
    if p.is_absolute() {
        return Err(format!("absolute path not allowed: {}", relative));
    }
    // 拒绝任何 Component::ParentDir
    for comp in p.components() {
        if matches!(comp, Component::ParentDir) {
            return Err(format!("path traversal not allowed: {}", relative));
        }
    }
    let joined = base.join(relative);
    // canonicalize 确认最终路径在 base 内（base 可能不存在，跳过 canonicalize 时回退到 join 比较前缀）
    let canon_base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    let canon_joined = joined.canonicalize().unwrap_or_else(|_| joined.clone());
    if !canon_joined.starts_with(&canon_base) {
        return Err(format!("path escapes base: {}", relative));
    }
    Ok(canon_joined)
}

#[derive(RustEmbed)]
#[folder = "src/channels/web/static/"]
struct StaticAsset;

/// GET / → index.html
pub async fn serve_index(State(_state): State<AppState>) -> Response {
    serve_static_path("index.html")
}

/// GET /static/*path
pub async fn serve_static(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    serve_static_path(&path)
}

fn serve_static_path(path: &str) -> Response {
    match StaticAsset::get(path) {
        Some(asset) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, mime.as_ref().parse().unwrap());
            (StatusCode::OK, headers, Body::from(asset.data.into_owned())).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

#[derive(Deserialize)]
pub struct TokenQuery {
    pub token: Option<String>,
}

#[derive(Deserialize)]
pub struct FilePathQueryWithToken {
    pub path: String,
    pub token: Option<String>,
}

/// 综合鉴权：header + cookie + query
pub fn authorize(state: &AppState, headers: &HeaderMap, q: &TokenQuery) -> bool {
    let cookie = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let provided = extract_token(headers, cookie, q.token.as_deref());
    match provided {
        Some(t) => check_token(&t, state.token.as_str()),
        None => false,
    }
}

pub fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
        axum::Json(serde_json::json!({ "error": "invalid token" })),
    )
        .into_response()
}

/// POST /upload：multipart/form-data 字段 file，保存到 workspace/uploads/
pub async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    mut multipart: Multipart,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let uploads_dir = state.workspace.join("uploads");
    let _ = tokio::fs::create_dir_all(&uploads_dir).await;

    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() == Some("file") {
            let filename = field.file_name().unwrap_or("upload.bin").to_string();
            let ct = field.content_type().unwrap_or("application/octet-stream").to_string();
            if !ct.starts_with("image/") {
                return (StatusCode::BAD_REQUEST, "only image/* allowed").into_response();
            }
            let data = match field.bytes().await {
                Ok(b) => b,
                Err(e) => return (StatusCode::BAD_REQUEST, format!("read: {}", e)).into_response(),
            };
            if data.len() > 20 * 1024 * 1024 {
                return (StatusCode::PAYLOAD_TOO_LARGE, "max 20MB").into_response();
            }
            let id = uuid::Uuid::new_v4().simple().to_string();
            let saved_name = format!("{}_{}", id, filename);
            let saved_path = uploads_dir.join(&saved_name);
            if tokio::fs::write(&saved_path, &data).await.is_err() {
                return (StatusCode::INTERNAL_SERVER_ERROR, "write failed").into_response();
            }
            let rel = format!("uploads/{}", saved_name);
            return axum::Json(serde_json::json!({ "path": rel, "size": data.len() })).into_response();
        }
    }
    (StatusCode::BAD_REQUEST, "no file field").into_response()
}

/// GET /file?path=<rel>&token=<token>：返回 workspace 内文件流
pub async fn serve_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<FilePathQueryWithToken>,
) -> Response {
    // 用 query token 鉴权（<img src> 无法带 header）
    let provided = q.token.clone();
    let cookie = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let from_header = extract_token(&headers, cookie, None);
    let token_ok = match (from_header, provided) {
        (Some(t), _) => check_token(&t, state.token.as_str()),
        (_, Some(t)) => check_token(&t, state.token.as_str()),
        _ => false,
    };
    if !token_ok {
        return unauthorized();
    }
    match resolve_within(&state.workspace, &q.path) {
        Ok(abs) => match tokio::fs::read(&abs).await {
            Ok(data) => {
                let mime = mime_guess::from_path(&abs).first_or_octet_stream();
                let mut h = HeaderMap::new();
                h.insert(header::CONTENT_TYPE, mime.as_ref().parse().unwrap());
                (StatusCode::OK, h, Body::from(data)).into_response()
            }
            Err(_) => (StatusCode::NOT_FOUND, "file not found").into_response(),
        },
        Err(e) => (StatusCode::BAD_REQUEST, format!("path: {}", e)).into_response(),
    }
}

/// 敏感字段掩码
const MASK: &str = "••••";

/// 标记哪些字段是敏感的（返回时掩码，保存时若仍为掩码则保留原值）
fn mask_sensitive(mut config: Config) -> Config {
    for p in config.provider.values_mut() {
        if !p.api_key.is_empty() {
            p.api_key = MASK.into();
        }
    }
    if !config.channels.qq.app_secret.is_empty() {
        config.channels.qq.app_secret = MASK.into();
    }
    if !config.channels.web.token.is_empty() {
        config.channels.web.token = MASK.into();
    }
    if !config.tools.tavily.api_key.is_empty() {
        config.tools.tavily.api_key = MASK.into();
    }
    config
}

/// 用 new_config 覆盖，但 new_config 中仍为 MASK 的字段保留 old 原值
fn merge_masked(old: &Config, new: &Config) -> Config {
    let mut merged = new.clone();
    for (k, np) in &mut merged.provider {
        if np.api_key == MASK {
            if let Some(op) = old.provider.get(k) {
                np.api_key = op.api_key.clone();
            }
        }
    }
    if merged.channels.qq.app_secret == MASK {
        merged.channels.qq.app_secret = old.channels.qq.app_secret.clone();
    }
    if merged.channels.web.token == MASK {
        merged.channels.web.token = old.channels.web.token.clone();
    }
    if merged.tools.tavily.api_key == MASK {
        merged.tools.tavily.api_key = old.tools.tavily.api_key.clone();
    }
    merged
}

/// GET /api/config → 掩码后的结构化 JSON
pub async fn get_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let cfg = state.config.read().await.clone();
    axum::Json(mask_sensitive(cfg)).into_response()
}

/// PUT /api/config → 写盘 + 更新内存
pub async fn put_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::Json(new_config): axum::Json<Config>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let old = state.config.read().await.clone();
    let merged = merge_masked(&old, &new_config);
    let toml_str = match toml::to_string_pretty(&merged) {
        Ok(s) => s,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("serialize: {}", e)).into_response(),
    };
    if let Err(e) = std::fs::write(&state.config_path, &toml_str) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("write: {}", e)).into_response();
    }
    *state.config.write().await = merged;
    axum::Json(serde_json::json!({ "ok": true, "note": "restart llaia to take effect" })).into_response()
}

/// GET /api/config/raw → TOML 文本
pub async fn get_config_raw(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    // 读盘上原始文本（含未掩码密钥）—— 已通过鉴权
    match std::fs::read_to_string(&state.config_path) {
        Ok(s) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            s,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "config not found").into_response(),
    }
}

#[derive(Deserialize)]
pub struct ValidateBody {
    pub toml: String,
}

/// POST /api/config/validate → 校验 TOML 语法
pub async fn validate_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::Json(body): axum::Json<ValidateBody>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    match toml::from_str::<Config>(&body.toml) {
        Ok(_) => axum::Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => {
            let msg = e.to_string();
            let line = e
                .span()
                .map(|s| s.start.to_string())
                .unwrap_or_default();
            axum::Json(serde_json::json!({ "ok": false, "error": msg, "line": line })).into_response()
        }
    }
}

/// PUT /api/config/raw → 写 TOML 文本到盘
pub async fn put_config_raw(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::Json(body): axum::Json<ValidateBody>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    // 先校验
    match toml::from_str::<Config>(&body.toml) {
        Ok(parsed) => {
            if let Err(e) = std::fs::write(&state.config_path, &body.toml) {
                return (StatusCode::INTERNAL_SERVER_ERROR, format!("write: {}", e)).into_response();
            }
            *state.config.write().await = parsed;
            axum::Json(serde_json::json!({ "ok": true, "note": "restart llaia to take effect" })).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, format!("invalid toml: {}", e)).into_response(),
    }
}

#[derive(Serialize)]
pub struct ChannelStatus {
    pub name: String,
    pub enabled: bool,
    pub listening: Option<String>,
}

#[derive(Serialize)]
pub struct StatusInfo {
    pub version: String,
    pub build_hash: String,
    pub workspace: String,
    pub config_path: String,
    pub pid: u32,
    pub channels: Vec<ChannelStatus>,
    pub db_size_bytes: u64,
    pub log_dir: String,
    pub uploads_count: u64,
}

/// GET /api/status
pub async fn get_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let cfg = state.config.read().await;
    let db_path = state.workspace.join("sessions.db");
    let db_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    let uploads_count = std::fs::read_dir(state.workspace.join("uploads"))
        .map(|d| d.count() as u64)
        .unwrap_or(0);
    let info = StatusInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        build_hash: env!("GIT_HASH").into(),
        workspace: state.workspace.display().to_string(),
        config_path: state.config_path.display().to_string(),
        pid: std::process::id(),
        channels: vec![
            ChannelStatus { name: "cli".into(), enabled: cfg.channels.cli.enabled, listening: None },
            ChannelStatus { name: "qq".into(), enabled: cfg.channels.qq.enabled, listening: None },
            ChannelStatus { name: "web".into(), enabled: cfg.channels.web.enabled, listening: Some(cfg.channels.web.bind.clone()) },
        ],
        db_size_bytes: db_size,
        log_dir: cfg.log.dir.clone(),
        uploads_count,
    };
    axum::Json(info).into_response()
}

/// GET /ws?token=... → WS upgrade
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(q): Query<TokenQuery>,
) -> Response {
    let provided = q.token.as_deref();
    let ok = match provided {
        Some(t) => check_token(t, state.token.as_str()),
        None => false,
    };
    if !ok {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    }
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

#[derive(Deserialize)]
pub struct ChatIn {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: Option<String>,
    pub images: Option<Vec<String>>,
}

async fn handle_ws(socket: WebSocket, state: AppState) {
    let (mut ws_sink, mut ws_stream) = socket.split();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<WebEvent>(64);
    let (end_tx, mut end_rx) = tokio::sync::mpsc::channel::<TurnEndSignal>(4);

    // 发 auth_ok
    let _ = ws_sink
        .send(Message::Text(serde_json::to_string(&WebEvent::AuthOk).unwrap()))
        .await;

    // 写 task：rx → ws_sink
    let write_task = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let json = match serde_json::to_string(&ev) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if ws_sink.send(Message::Text(json)).await.is_err() {
                break;
            }
        }
    });

    let agent = state.registry.main.clone();
    let workspace = {
        let a = agent.lock().await;
        a.workspace.clone()
    };
    let stop: Arc<Notify> = Arc::new(Notify::new());
    let mut current_turn: Option<tokio::task::JoinHandle<()>> = None;

    loop {
        tokio::select! {
            // WS 入向消息
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(s))) => {
                        let chat: Option<ChatIn> = serde_json::from_str(&s).ok();
                        match chat.as_ref().map(|c| c.kind.as_str()) {
                            Some("ping") => { let _ = tx.send(WebEvent::Pong).await; }
                            Some("stop") => {
                                if current_turn.is_some() {
                                    stop.notify_one();
                                }
                            }
                            Some("chat") => {
                                if current_turn.is_some() {
                                    let _ = tx.send(WebEvent::Busy { reason: "another turn running".into() }).await;
                                } else {
                                    let chat: ChatIn = serde_json::from_str(&s).unwrap();
                                    let text = chat.text.unwrap_or_default();
                                    // 构造消息
                                    let user_msg = build_user_message(&text, chat.images.as_deref(), &workspace);
                                    let sink = Box::new(WebSink::new(tx.clone(), end_tx.clone()));
                                    let stop_clone = stop.clone();
                                    let agent_clone = agent.clone();
                                    current_turn = Some(tokio::spawn(async move {
                                        let _ = run_turn(agent_clone, user_msg, "web".into(), sink, stop_clone).await;
                                    }));
                                }
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
            // turn 结束信号
            _ = end_rx.recv() => {
                if let Some(h) = current_turn.take() {
                    let _ = h.await;
                }
            }
        }
    }

    // 清理
    if let Some(h) = current_turn.take() {
        stop.notify_one();
        let _ = h.await;
    }
    write_task.abort();
}

fn build_user_message(text: &str, images: Option<&[String]>, workspace: &Path) -> ChatMessage {
    let imgs = images.unwrap_or(&[]);
    if imgs.is_empty() {
        return ChatMessage::user(text);
    }
    let mut parts: Vec<ContentPart> = Vec::new();
    if !text.is_empty() {
        parts.push(ContentPart::Text { text: text.into() });
    }
    let uploads_dir = workspace.join("uploads");
    for img_rel in imgs {
        match resolve_within(&uploads_dir, img_rel) {
            Ok(abs) => {
                if !image_utils::is_image_file(&abs) {
                    parts.push(ContentPart::Text { text: format!("[not an image: {}]", img_rel) });
                    continue;
                }
                match image_utils::prepare_image_for_vision(&abs) {
                    Ok(data_url) => {
                        parts.push(ContentPart::ImageUrl { image_url: ImageUrlContent { url: data_url } });
                    }
                    Err(e) => {
                        parts.push(ContentPart::Text { text: format!("[image load failed: {}]", e) });
                    }
                }
            }
            Err(e) => {
                parts.push(ContentPart::Text { text: format!("[invalid path: {}]", e) });
            }
        }
    }
    if parts.is_empty() {
        ChatMessage::user(text)
    } else {
        ChatMessage::user_multimodal(parts)
    }
}

pub struct WebChannel {
    pub config: WebConfig,
    pub registry: Arc<AgentRegistry>,
    pub config_full: Arc<RwLock<Config>>,
    pub config_path: PathBuf,
    pub workspace: PathBuf,
}

impl WebChannel {
    pub fn new(
        web_config: WebConfig,
        registry: Arc<AgentRegistry>,
        config_full: Arc<RwLock<Config>>,
        config_path: PathBuf,
        workspace: PathBuf,
    ) -> Self {
        Self { config: web_config, registry, config_full, config_path, workspace }
    }

    pub fn build_router(&self) -> axum::Router {
        // token：配置非空用配置，留空随机生成
        let token = if self.config.token.is_empty() {
            let t = generate_token();
            tracing::info!("WebUI token (randomly generated): {}", t);
            t
        } else {
            self.config.token.clone()
        };
        let state = AppState {
            registry: self.registry.clone(),
            config: self.config_full.clone(),
            config_path: self.config_path.clone(),
            workspace: self.workspace.clone(),
            token: Arc::new(token),
        };
        axum::Router::new()
            .route("/", axum::routing::get(serve_index))
            .route("/static/*path", axum::routing::get(serve_static))
            .route("/ws", axum::routing::get(ws_handler))
            .route("/upload", axum::routing::post(upload))
            .route("/file", axum::routing::get(serve_file))
            .route("/api/config", axum::routing::get(get_config).put(put_config))
            .route("/api/config/raw", axum::routing::get(get_config_raw).put(put_config_raw))
            .route("/api/config/validate", axum::routing::post(validate_config))
            .route("/api/status", axum::routing::get(get_status))
            .with_state(state)
    }
}

#[async_trait]
impl Channel for WebChannel {
    async fn run(self: Arc<Self>, _registry: Arc<AgentRegistry>) -> Result<(), anyhow::Error> {
        let addr: std::net::SocketAddr = self.config.bind.parse()
            .map_err(|e| anyhow::anyhow!("invalid bind addr: {}", e))?;
        let router = self.build_router();
        let listener = tokio::net::TcpListener::bind(addr).await
            .map_err(|e| anyhow::anyhow!("bind {}: {}", addr, e))?;
        tracing::info!("WebChannel listening on {}", addr);
        axum::serve(listener, router).await
            .map_err(|e| anyhow::anyhow!("web server: {}", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_web_event_chunk_serialization() {
        let ev = WebEvent::Chunk { delta: "hello".into() };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(json, r#"{"type":"chunk","delta":"hello"}"#);
    }

    #[test]
    fn test_web_event_done_serialization() {
        let ev = WebEvent::Done;
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(json, r#"{"type":"done"}"#);
    }

    #[test]
    fn test_web_event_media_serialization() {
        let ev = WebEvent::Media { path: "out/a.png".into(), kind: MediaKind::Image };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains(r#""type":"media""#));
        assert!(json.contains(r#""path":"out/a.png""#));
    }

    #[test]
    fn test_web_event_auth_failed_serialization() {
        let ev = WebEvent::AuthFailed { reason: "invalid token".into() };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(json, r#"{"type":"auth_failed","reason":"invalid token"}"#);
    }

    use crate::agent::sink::OutputSink;

    #[tokio::test]
    async fn test_web_sink_chunk_to_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<WebEvent>(8);
        let (end_tx, _end_rx) = tokio::sync::mpsc::channel::<TurnEndSignal>(8);
        let mut sink = WebSink::new(tx, end_tx);
        sink.on_chunk("hi").await;
        let ev = rx.recv().await.unwrap();
        match ev {
            WebEvent::Chunk { delta } => assert_eq!(delta, "hi"),
            _ => panic!("expected Chunk"),
        }
    }

    #[tokio::test]
    async fn test_web_sink_terminal_events_send_turn_end() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<WebEvent>(8);
        let (end_tx, mut end_rx) = tokio::sync::mpsc::channel::<TurnEndSignal>(8);
        let mut sink = WebSink::new(tx, end_tx);
        sink.on_done().await;
        assert!(end_rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn test_web_sink_send_failure_ignored() {
        // drop receiver 后 send 应返回 Err，但 sink 不 panic
        let (tx, rx) = tokio::sync::mpsc::channel::<WebEvent>(8);
        let (end_tx, _end_rx) = tokio::sync::mpsc::channel::<TurnEndSignal>(8);
        let mut sink = WebSink::new(tx, end_tx);
        drop(rx);
        // 不应 panic
        sink.on_chunk("hi").await;
        sink.on_done().await;
    }

    use axum::http::HeaderMap;

    #[test]
    fn test_extract_token_bearer_header() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer abc123".parse().unwrap());
        let token = extract_token(&headers, "", None);
        assert_eq!(token.as_deref(), Some("abc123"));
    }

    #[test]
    fn test_extract_token_cookie() {
        let headers = HeaderMap::new();
        let cookies = "other=x; llaia_token=secret; foo=bar";
        let token = extract_token(&headers, cookies, None);
        assert_eq!(token.as_deref(), Some("secret"));
    }

    #[test]
    fn test_extract_token_query() {
        let headers = HeaderMap::new();
        let token = extract_token(&headers, "", Some("from-query"));
        assert_eq!(token.as_deref(), Some("from-query"));
    }

    #[test]
    fn test_check_token() {
        assert!(check_token("abc", "abc"));
        assert!(!check_token("abc", "wrong"));
        assert!(!check_token("abc", "")); // 空 expected 拒绝
    }

    #[test]
    fn test_generate_token_length() {
        let t = generate_token();
        assert_eq!(t.len(), 64); // 32 bytes hex
    }

    use std::fs;

    #[test]
    fn test_resolve_within_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let r = resolve_within(base, "../../etc/passwd");
        assert!(r.is_err());
    }

    #[test]
    fn test_resolve_within_rejects_absolute() {
        let tmp = tempfile::tempdir().unwrap();
        let r = resolve_within(tmp.path(), "/etc/passwd");
        assert!(r.is_err());
    }

    #[test]
    fn test_resolve_within_accepts_inside() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        fs::create_dir_all(base.join("uploads")).unwrap();
        fs::write(base.join("uploads/abc.png"), b"x").unwrap();
        let r = resolve_within(base, "uploads/abc.png").unwrap();
        // Windows: canonicalize 给路径加 \\?\ 前缀，对比时统一用 canonicalized base
        let canon_base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
        assert!(r.starts_with(&canon_base));
    }

    #[test]
    fn test_resolve_within_rejects_windows_drive() {
        let tmp = tempfile::tempdir().unwrap();
        let r = resolve_within(tmp.path(), "C:\\Windows\\system32");
        assert!(r.is_err());
    }

    use crate::config::Config;

    fn sample_config() -> Config {
        let mut c = Config::default_for_workspace("/tmp/llaia-test");
        c.provider.get_mut("default").unwrap().api_key = "sk-secret".into();
        c.channels.qq.app_secret = "qq-secret".into();
        c.tools.tavily.api_key = "tvly-secret".into();
        c
    }

    #[test]
    fn test_mask_sensitive_redacts_secrets() {
        let masked = mask_sensitive(sample_config());
        assert_eq!(masked.provider.get("default").unwrap().api_key, "••••");
        assert_eq!(masked.channels.qq.app_secret, "••••");
        assert_eq!(masked.tools.tavily.api_key, "••••");
    }

    #[test]
    fn test_merge_masked_preserves_original_secret() {
        let old = sample_config();
        let mut new = old.clone();
        // 用户没改 api_key（仍为掩码）
        new.provider.get_mut("default").unwrap().api_key = "••••".into();
        let merged = merge_masked(&old, &new);
        assert_eq!(merged.provider.get("default").unwrap().api_key, "sk-secret");
    }

    #[test]
    fn test_merge_masked_uses_new_secret_when_changed() {
        let old = sample_config();
        let mut new = old.clone();
        new.provider.get_mut("default").unwrap().api_key = "sk-new".into();
        let merged = merge_masked(&old, &new);
        assert_eq!(merged.provider.get("default").unwrap().api_key, "sk-new");
    }
}
