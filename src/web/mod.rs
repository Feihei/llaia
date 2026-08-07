use crate::agent::AgentRegistry;
use crate::channels::web::WebEvent;
use crate::config::Config;
use crate::provider::Provider;
use axum::body::Body;
use axum::extract::{Multipart, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use rand::Rng;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// 共享状态：所有 handler 共用
#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<AgentRegistry>,
    pub config: Arc<RwLock<Config>>,
    pub config_path: std::path::PathBuf,
    pub workspace: std::path::PathBuf,
    pub token: Arc<String>,
    /// active WS 连接注册表：id → event sender，用于主动推送（cron 任务结果等）
    pub active_ws: Arc<
        tokio::sync::Mutex<std::collections::HashMap<u64, tokio::sync::mpsc::Sender<WebEvent>>>,
    >,
    /// WS 连接 id 自增计数器
    pub next_ws_id: Arc<std::sync::atomic::AtomicU64>,
    /// cron.toml 路径（供 raw 编辑接口读写）
    pub cron_path: std::path::PathBuf,
    /// CronScheduler 实例（None 时 cron API 返回 503）
    pub cron_scheduler: Option<Arc<crate::cron::CronScheduler>>,
    /// mcp.toml 路径（供 raw 编辑 / 测试连接接口读写）
    pub mcp_path: std::path::PathBuf,
    /// McpRegistry 实例（None 时 MCP 状态 API 返回 503；raw 编辑不受影响）
    pub mcp_registry: Option<Arc<crate::mcp::client::McpRegistry>>,
    /// skills 目录（<config_dir>/skills，供 Skills API 读写）
    pub skills_dir: std::path::PathBuf,
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
#[folder = "src/web/static/"]
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
            let ct = field
                .content_type()
                .unwrap_or("application/octet-stream")
                .to_string();
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
            return axum::Json(serde_json::json!({ "path": rel, "size": data.len() }))
                .into_response();
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
pub fn mask_sensitive(mut config: Config) -> Config {
    for p in config.provider.values_mut() {
        if !p.api_key.is_empty() {
            p.api_key = MASK.into();
        }
    }
    if !config.channels.qq.app_secret.is_empty() {
        config.channels.qq.app_secret = MASK.into();
    }
    if !config.webui.token.is_empty() {
        config.webui.token = MASK.into();
    }
    if !config.tools.tavily.api_key.is_empty() {
        config.tools.tavily.api_key = MASK.into();
    }
    config
}

/// 用 new_config 覆盖，但 new_config 中仍为 MASK 的字段保留 old 原值
pub fn merge_masked(old: &Config, new: &Config) -> Config {
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
    if merged.webui.token == MASK {
        merged.webui.token = old.webui.token.clone();
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

/// PUT /api/config → 写盘 + 更新内存 + 热加载 provider
pub async fn put_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    body: axum::body::Bytes,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let body_str = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, &format!("invalid utf8: {}", e)),
    };
    let new_config: Config = match serde_json::from_str(body_str) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, body = %body_str, "PUT /api/config parse failed");
            return json_err(StatusCode::BAD_REQUEST, &format!("parse config: {}", e));
        }
    };
    let old = state.config.read().await.clone();
    let merged = merge_masked(&old, &new_config);

    // main agent 不可删除：[agent.main] 是系统必需 section
    if !merged.agent.contains_key("main") {
        return json_err(
            StatusCode::BAD_REQUEST,
            "[agent.main] 不可删除：main agent 是系统必需配置",
        );
    }

    let toml_str = match toml::to_string_pretty(&merged) {
        Ok(s) => s,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, &format!("serialize: {}", e)),
    };

    // 先尝试构建新 provider：失败则不写盘、不更新内存（回滚到旧 config）
    if let Err(e) = build_provider_from_config(&merged) {
        return json_err(
            StatusCode::BAD_REQUEST,
            &format!("provider 构建失败，配置未保存：{}", e),
        );
    }

    if let Err(e) = std::fs::write(&state.config_path, &toml_str) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("write: {}", e));
    }
    *state.config.write().await = merged.clone();

    // 热加载 provider + compact_provider
    if let Err(e) = hot_reload_providers(&state, &merged).await {
        tracing::warn!(error = %e, "hot_reload_providers failed (config saved)");
        return axum::Json(serde_json::json!({
            "ok": true,
            "note": "config saved but provider reload failed: ".to_string() + &e
        }))
        .into_response();
    }
    axum::Json(serde_json::json!({ "ok": true, "note": "config saved and provider reloaded" }))
        .into_response()
}

fn json_err(code: StatusCode, msg: &str) -> Response {
    (code, axum::Json(serde_json::json!({ "error": msg }))).into_response()
}

/// 根据 config 构建新的 provider 实例（用于热加载）。
/// - 无 [agent.main] 或 model 为空 → Ok(None)（降级模式）
/// - 解析或构造失败 → Err（调用方应回滚到旧 config）
///
/// fallback 链：主模型失败时按序降级（fallback 项缺失仅 warn）
pub fn build_provider_from_config(config: &Config) -> Result<Option<Arc<dyn Provider>>, String> {
    let main_cfg = match config.agent.get("main") {
        Some(c) => c,
        None => return Ok(None),
    };
    if main_cfg.model.is_empty() {
        return Ok(None);
    }
    crate::provider::build_provider_chain(&main_cfg.model, &main_cfg.fallback, config)
        .map_err(|e| format!("build provider: {}", e))
}

/// 热加载 provider + compact_provider：
/// - 主 provider 按 [agent.main].model 构建
/// - compact_provider 按 runtime.compact_model 构建（未配置/失败则 None，回退到主 provider）
///
/// 主 agent 持有 RwLock，正在进行的 turn 不受影响。
async fn hot_reload_providers(state: &AppState, new_config: &Config) -> Result<(), String> {
    let new_provider = build_provider_from_config(new_config)?;
    let new_compact = build_compact_provider_from_config(new_config);
    let agent = state.registry.main.lock().await;
    agent.reload_provider(new_provider).await;
    agent.reload_compact_provider(new_compact).await;
    Ok(())
}

/// 根据 runtime.compact_model 构建 compact_provider。
/// 未配置 / provider 缺失 / 构建失败 → None（回退到主 provider）。
fn build_compact_provider_from_config(config: &Config) -> Option<Arc<dyn Provider>> {
    let m = config.runtime.compact_model.as_ref()?;
    if m.is_empty() {
        return None;
    }
    match crate::provider::provider_from_ref(config, m) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(error = %e, model = m.as_str(), "build compact_provider failed");
            None
        }
    }
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
            let line = e.span().map(|s| s.start.to_string()).unwrap_or_default();
            axum::Json(serde_json::json!({ "ok": false, "error": msg, "line": line }))
                .into_response()
        }
    }
}

/// POST /api/restart → 自重启 serve 进程：spawn 替代进程后退出。
/// 替代进程延迟 ~1s 启动，等旧进程释放端口。pid 文件仅警告不阻止，新进程可正常接管。
pub async fn restart_service(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let config_dir = match state.config_path.parent() {
        Some(d) => d.to_path_buf(),
        None => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot derive config_dir from config_path",
            )
        }
    };
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("current_exe: {}", e),
            )
        }
    };
    if let Err(e) = spawn_replacement(&exe, &config_dir) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, &e);
    }
    tracing::info!("restart requested: replacement process spawned, exiting");
    // 先把响应送达浏览器，再延迟退出（给 axum 刷出响应的时间）
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        std::process::exit(0);
    });
    axum::Json(serde_json::json!({ "restarting": true })).into_response()
}

/// spawn 替代进程：Windows 用 cmd（ping 延时），Unix 用 sh（sleep 延时）。
fn spawn_replacement(exe: &Path, config_dir: &Path) -> Result<(), String> {
    let exe_s = exe.display().to_string();
    let dir_s = config_dir.display().to_string();
    let spawn_result = {
        #[cfg(windows)]
        {
            let script = format!(
                "ping -n 2 127.0.0.1 >nul & \"{}\" --config-dir \"{}\" serve",
                exe_s, dir_s
            );
            std::process::Command::new("cmd")
                .args(["/C", &script])
                .spawn()
        }
        #[cfg(not(windows))]
        {
            let script = format!(
                "sleep 1 && exec \"{}\" --config-dir \"{}\" serve",
                exe_s, dir_s
            );
            std::process::Command::new("sh")
                .args(["-c", &script])
                .spawn()
        }
    };
    spawn_result
        .map(|_| ())
        .map_err(|e| format!("spawn replacement process: {}", e))
}

/// PUT /api/config/raw → 写 TOML 文本到盘 + 热加载 provider
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
    let parsed = match toml::from_str::<Config>(&body.toml) {
        Ok(p) => p,
        Err(e) => {
            let msg = e.to_string();
            let line = e.span().map(|s| s.start.to_string()).unwrap_or_default();
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": msg, "line": line })),
            )
                .into_response();
        }
    };

    // main agent 不可删除：[agent.main] 是系统必需 section
    if !parsed.agent.contains_key("main") {
        return json_err(
            StatusCode::BAD_REQUEST,
            "[agent.main] 不可删除：main agent 是系统必需配置",
        );
    }

    // 尝试构建新 provider：失败则不写盘（回滚到旧 config）
    if let Err(e) = build_provider_from_config(&parsed) {
        return json_err(
            StatusCode::BAD_REQUEST,
            &format!("provider 构建失败，配置未保存：{}", e),
        );
    }

    if let Err(e) = std::fs::write(&state.config_path, &body.toml) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": format!("write: {}", e) })),
        )
            .into_response();
    }
    *state.config.write().await = parsed.clone();

    // 热加载 provider + compact_provider
    if let Err(e) = hot_reload_providers(&state, &parsed).await {
        tracing::warn!(error = %e, "hot_reload_providers failed (config saved)");
        return axum::Json(serde_json::json!({
            "ok": true,
            "note": "config saved but provider reload failed: ".to_string() + &e
        }))
        .into_response();
    }
    axum::Json(serde_json::json!({ "ok": true, "note": "config saved and provider reloaded" }))
        .into_response()
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
            ChannelStatus {
                name: "qq".into(),
                enabled: cfg.channels.qq.enabled,
                listening: None,
            },
            ChannelStatus {
                name: "web".into(),
                enabled: true,
                listening: Some(format!("{}:{}", cfg.webui.host, cfg.webui.port)),
            },
        ],
        db_size_bytes: db_size,
        log_dir: cfg.log.dir.clone(),
        uploads_count,
    };
    axum::Json(info).into_response()
}

/// GET /api/cron → 列出所有 cron 任务定义（含 disabled）
pub async fn list_cron(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    match &state.cron_scheduler {
        Some(s) => {
            let tasks = s.list_tasks().await;
            axum::Json(tasks).into_response()
        }
        None => json_err(
            StatusCode::SERVICE_UNAVAILABLE,
            "cron scheduler not running",
        ),
    }
}

/// GET /api/cron/history → cron 触发的会话历史（channel LIKE 'cron:%'）
pub async fn cron_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let agent = state.registry.main.lock().await;
    match agent.session_store.list_sessions_by_channel_prefix("cron:") {
        Ok(rows) => axum::Json(rows).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("query: {}", e)),
    }
}

/// POST /api/cron/:id/trigger → 手动触发一个任务
pub async fn trigger_cron(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    match &state.cron_scheduler {
        Some(s) => match s.trigger(&id).await {
            Ok(()) => axum::Json(serde_json::json!({ "ok": true })).into_response(),
            Err(e) => json_err(StatusCode::NOT_FOUND, &format!("trigger: {}", e)),
        },
        None => json_err(
            StatusCode::SERVICE_UNAVAILABLE,
            "cron scheduler not running",
        ),
    }
}

/// GET /api/cron/raw → cron.toml 原始文本
pub async fn get_cron_raw(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    if !state.cron_path.exists() {
        return axum::Json(serde_json::json!({ "raw": "" })).into_response();
    }
    match std::fs::read_to_string(&state.cron_path) {
        Ok(content) => axum::Json(serde_json::json!({ "raw": content })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("read: {}", e)),
    }
}

#[derive(Deserialize)]
pub struct CronRawBody {
    pub raw: String,
}

/// PUT /api/cron/raw → 写 cron.toml 文本（先校验可解析，不热加载，需重启 serve 生效）
pub async fn put_cron_raw(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::Json(body): axum::Json<CronRawBody>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    // 先校验能解析
    let cfg: crate::cron::CronConfig = match toml::from_str(&body.raw) {
        Ok(c) => c,
        Err(e) => {
            let msg = e.to_string();
            let line = e.span().map(|s| s.start.to_string()).unwrap_or_default();
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": msg, "line": line })),
            )
                .into_response();
        }
    };
    if let Err(e) = std::fs::write(&state.cron_path, &body.raw) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("write: {}", e));
    }
    tracing::info!(
        tasks = cfg.task.len(),
        "cron.toml updated (reload requires restart)"
    );
    axum::Json(serde_json::json!({
        "ok": true,
        "note": "cron.toml saved (restart serve to apply)"
    }))
    .into_response()
}

// ───────────────────────── MCP API ─────────────────────────

/// GET /api/mcp → MCP server 状态列表（含每个 server 的工具清单）
pub async fn list_mcp(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let Some(registry) = state.mcp_registry.clone() else {
        return json_err(
            StatusCode::SERVICE_UNAVAILABLE,
            "MCP registry not available",
        );
    };
    axum::Json(serde_json::json!({ "servers": registry.status().await })).into_response()
}

/// GET /api/mcp/raw → 读 mcp.toml 文本
pub async fn get_mcp_raw(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    if !state.mcp_path.exists() {
        return axum::Json(serde_json::json!({ "raw": "" })).into_response();
    }
    match std::fs::read_to_string(&state.mcp_path) {
        Ok(content) => axum::Json(serde_json::json!({ "raw": content })).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("read: {}", e)),
    }
}

/// PUT /api/mcp/raw → 写 mcp.toml 文本（先校验可解析，不热加载，需重启 serve 生效）
pub async fn put_mcp_raw(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::Json(body): axum::Json<CronRawBody>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let cfg = match crate::mcp::McpConfig::from_str_validate(&body.raw) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    if let Err(e) = std::fs::write(&state.mcp_path, &body.raw) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("write: {}", e));
    }
    tracing::info!(
        servers = cfg.server.len(),
        "mcp.toml updated (reload requires restart)"
    );
    axum::Json(serde_json::json!({
        "ok": true,
        "note": "mcp.toml saved (restart serve to apply)"
    }))
    .into_response()
}

/// POST /api/mcp/:id/test → 现场连接指定 server（initialize + tools/list），返回工具列表
pub async fn test_mcp(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let cfg = match crate::mcp::McpConfig::load(&state.mcp_path) {
        Ok(c) => c,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, &format!("load mcp.toml: {}", e)),
    };
    let server_cfg = match cfg.server.into_iter().find(|s| s.id == id) {
        Some(s) => s,
        None => {
            return json_err(
                StatusCode::NOT_FOUND,
                &format!("mcp server not found: {}", id),
            )
        }
    };
    match crate::mcp::client::McpServer::connect(server_cfg).await {
        Ok(server) => {
            let tools = server
                .tools_snapshot()
                .await
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "name": d.name,
                        "description": d.description.clone().unwrap_or_default(),
                    })
                })
                .collect::<Vec<_>>();
            axum::Json(serde_json::json!({ "ok": true, "tools": tools })).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            axum::Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ───────────────────────── Skills API ─────────────────────────

/// 校验 URL 路径中的 skill name（防路径穿越）
fn skill_name_or_err(name: &str) -> Result<(), Box<Response>> {
    if crate::skill::is_valid_skill_name(name) {
        Ok(())
    } else {
        Err(Box::new(json_err(
            StatusCode::BAD_REQUEST,
            &format!("invalid skill name: {}", name),
        )))
    }
}

/// GET /api/skills → skill 列表（name / description / duration / tools / active / path）
pub async fn list_skills(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let skills = crate::skill::loader::scan_skills(&state.skills_dir);
    axum::Json(serde_json::json!({ "skills": skills })).into_response()
}

#[derive(Deserialize)]
pub struct CreateSkillBody {
    pub name: String,
    /// SKILL.md 内容；缺省时用默认模板
    pub content: Option<String>,
}

/// POST /api/skills → 创建 skill（写 SKILL.md + skills.json 记为 active）
pub async fn create_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::Json(body): axum::Json<CreateSkillBody>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    if let Err(r) = skill_name_or_err(&body.name) {
        return *r;
    }
    let dir = state.skills_dir.join(&body.name);
    if dir.exists() {
        return json_err(
            StatusCode::CONFLICT,
            &format!("skill already exists: {}", body.name),
        );
    }
    let content = match body.content {
        Some(c) => {
            if let Err(e) = crate::skill::loader::validate_skill_md(&c) {
                return json_err(StatusCode::BAD_REQUEST, &format!("invalid SKILL.md: {}", e));
            }
            c
        }
        None => crate::skill::loader::default_skill_template(&body.name),
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("mkdir: {}", e));
    }
    if let Err(e) = std::fs::write(dir.join("SKILL.md"), &content) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("write: {}", e));
    }
    let _ = crate::skill::loader::set_active(&state.skills_dir, &body.name, true);
    axum::Json(serde_json::json!({
        "ok": true,
        "note": "skill created (restart serve to inject into system prompt)"
    }))
    .into_response()
}

/// DELETE /api/skills/:name → 删除 skill 目录 + skills.json 条目
pub async fn delete_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    if let Err(r) = skill_name_or_err(&name) {
        return *r;
    }
    let dir = state.skills_dir.join(&name);
    if !dir.exists() {
        return json_err(StatusCode::NOT_FOUND, &format!("skill not found: {}", name));
    }
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("remove: {}", e));
    }
    let _ = crate::skill::loader::remove_entry(&state.skills_dir, &name);
    axum::Json(serde_json::json!({ "ok": true })).into_response()
}

#[derive(Deserialize)]
pub struct SkillActiveBody {
    pub active: bool,
}

/// PUT /api/skills/:name/active → 切换 active 开关（写 skills.json）
pub async fn set_skill_active(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::Json(body): axum::Json<SkillActiveBody>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    if let Err(r) = skill_name_or_err(&name) {
        return *r;
    }
    match crate::skill::loader::set_active(&state.skills_dir, &name, body.active) {
        Ok(()) => axum::Json(serde_json::json!({
            "ok": true,
            "note": "restart serve to apply"
        }))
        .into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("save: {}", e)),
    }
}

/// GET /api/skills/:name/content → 读 SKILL.md 原文
pub async fn get_skill_content(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    if let Err(r) = skill_name_or_err(&name) {
        return *r;
    }
    let path = state.skills_dir.join(&name).join("SKILL.md");
    match std::fs::read_to_string(&path) {
        Ok(content) => axum::Json(serde_json::json!({ "content": content })).into_response(),
        Err(_) => json_err(StatusCode::NOT_FOUND, &format!("skill not found: {}", name)),
    }
}

#[derive(Deserialize)]
pub struct SkillContentBody {
    pub content: String,
}

/// PUT /api/skills/:name/content → 写 SKILL.md（先校验 frontmatter 可解析）
pub async fn put_skill_content(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::Json(body): axum::Json<SkillContentBody>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    if let Err(r) = skill_name_or_err(&name) {
        return *r;
    }
    if let Err(e) = crate::skill::loader::validate_skill_md(&body.content) {
        return json_err(StatusCode::BAD_REQUEST, &format!("invalid SKILL.md: {}", e));
    }
    let dir = state.skills_dir.join(&name);
    if !dir.exists() {
        return json_err(StatusCode::NOT_FOUND, &format!("skill not found: {}", name));
    }
    if let Err(e) = std::fs::write(dir.join("SKILL.md"), &body.content) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("write: {}", e));
    }
    axum::Json(serde_json::json!({
        "ok": true,
        "note": "SKILL.md saved (restart serve to inject into system prompt)"
    }))
    .into_response()
}

/// 构建系统级 Web 路由（不含 WS）。
/// 返回 `Router<AppState>`，由调用方合并 WS 路由后统一 `with_state`。
pub fn build_system_routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/", axum::routing::get(serve_index))
        .route("/static/*path", axum::routing::get(serve_static))
        .route("/upload", axum::routing::post(upload))
        .route("/file", axum::routing::get(serve_file))
        .route(
            "/api/config",
            axum::routing::get(get_config).put(put_config),
        )
        .route(
            "/api/config/raw",
            axum::routing::get(get_config_raw).put(put_config_raw),
        )
        .route("/api/config/validate", axum::routing::post(validate_config))
        .route("/api/restart", axum::routing::post(restart_service))
        .route("/api/status", axum::routing::get(get_status))
        // cron API
        .route("/api/cron", axum::routing::get(list_cron))
        .route(
            "/api/cron/raw",
            axum::routing::get(get_cron_raw).put(put_cron_raw),
        )
        .route("/api/cron/history", axum::routing::get(cron_history))
        .route("/api/cron/:id/trigger", axum::routing::post(trigger_cron))
        // MCP API
        .route("/api/mcp", axum::routing::get(list_mcp))
        .route(
            "/api/mcp/raw",
            axum::routing::get(get_mcp_raw).put(put_mcp_raw),
        )
        .route("/api/mcp/:id/test", axum::routing::post(test_mcp))
        // Skills API
        .route(
            "/api/skills",
            axum::routing::get(list_skills).post(create_skill),
        )
        .route("/api/skills/:name", axum::routing::delete(delete_skill))
        .route(
            "/api/skills/:name/active",
            axum::routing::put(set_skill_active),
        )
        .route(
            "/api/skills/:name/content",
            axum::routing::get(get_skill_content).put(put_skill_content),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::http::HeaderMap;
    use std::fs;

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

    // Windows-only: on non-Windows the backslash is an ordinary char, so
    // "C:\Windows\system32" is a benign (weird) relative filename, not an
    // absolute path. The drive-letter rejection only applies on Windows.
    #[cfg(windows)]
    #[test]
    fn test_resolve_within_rejects_windows_drive() {
        let tmp = tempfile::tempdir().unwrap();
        let r = resolve_within(tmp.path(), "C:\\Windows\\system32");
        assert!(r.is_err());
    }

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
