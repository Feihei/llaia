use crate::agent::AgentRegistry;
use crate::config::Config;
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
pub fn mask_sensitive(mut config: Config) -> Config {
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
    let toml_str = match toml::to_string_pretty(&merged) {
        Ok(s) => s,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, &format!("serialize: {}", e)),
    };
    if let Err(e) = std::fs::write(&state.config_path, &toml_str) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("write: {}", e));
    }
    *state.config.write().await = merged;
    axum::Json(serde_json::json!({ "ok": true, "note": "restart llaia to take effect" })).into_response()
}

fn json_err(code: StatusCode, msg: &str) -> Response {
    (code, axum::Json(serde_json::json!({ "error": msg }))).into_response()
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
                return (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(serde_json::json!({ "error": format!("write: {}", e) }))).into_response();
            }
            *state.config.write().await = parsed;
            axum::Json(serde_json::json!({ "ok": true, "note": "restart llaia to take effect" })).into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            let line = e.span().map(|s| s.start.to_string()).unwrap_or_default();
            (StatusCode::BAD_REQUEST, axum::Json(serde_json::json!({ "error": msg, "line": line }))).into_response()
        }
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
            ChannelStatus { name: "web".into(), enabled: cfg.channels.web.enabled, listening: Some(format!("{}:{}", cfg.channels.web.host, cfg.channels.web.port)) },
        ],
        db_size_bytes: db_size,
        log_dir: cfg.log.dir.clone(),
        uploads_count,
    };
    axum::Json(info).into_response()
}

/// 构建系统级 Web 路由（不含 WS）。
/// 返回 `Router<AppState>`，由调用方合并 WS 路由后统一 `with_state`。
pub fn build_system_routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/", axum::routing::get(serve_index))
        .route("/static/*path", axum::routing::get(serve_static))
        .route("/upload", axum::routing::post(upload))
        .route("/file", axum::routing::get(serve_file))
        .route("/api/config", axum::routing::get(get_config).put(put_config))
        .route("/api/config/raw", axum::routing::get(get_config_raw).put(put_config_raw))
        .route("/api/config/validate", axum::routing::post(validate_config))
        .route("/api/status", axum::routing::get(get_status))
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
