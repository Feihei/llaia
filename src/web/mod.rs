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
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{Notify, RwLock};
use toml_edit::{DocumentMut, Entry, Item, Table};

/// 共享状态：所有 handler 共用
#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<AgentRegistry>,
    pub config: Arc<RwLock<Config>>,
    pub config_path: std::path::PathBuf,
    pub workspace: std::path::PathBuf,
    pub token: Arc<String>,
    /// 优雅停止信号：/api/shutdown handler 触发，serve_cmd 的 select! 监听并退出（ADR-0018）
    pub shutdown_signal: Arc<Notify>,
    /// active WS 连接注册表：id → event sender，用于主动推送（cron 任务结果等）
    pub active_ws: Arc<
        tokio::sync::Mutex<std::collections::HashMap<u64, tokio::sync::mpsc::Sender<WebEvent>>>,
    >,
    /// WS 连接 id 自增计数器
    pub next_ws_id: Arc<std::sync::atomic::AtomicU64>,
    /// cron.toml 路径（供 raw 编辑接口读写）
    pub cron_path: std::path::PathBuf,
    /// CronHandle 实例（None 时 cron API 返回 503）。
    /// 用 Arc<Mutex> 包一层，以便 reload_all 在保存配置后原地替换而不必重建 AppState。
    pub cron_scheduler: Arc<std::sync::Mutex<Option<Arc<crate::cron::CronHandle>>>>,
    /// mcp.toml 路径（供 raw 编辑 / 测试连接接口读写）
    pub mcp_path: std::path::PathBuf,
    /// McpRegistry 实例（None 时 MCP 状态 API 返回 503；raw 编辑不受影响）。
    /// 同 cron_scheduler，用 Arc<Mutex> 支持热加载时替换。
    pub mcp_registry: Arc<std::sync::Mutex<Option<Arc<crate::mcp::client::McpRegistry>>>>,
    /// skills 目录（<config_dir>/skills，供 Skills API 读写）
    pub skills_dir: std::path::PathBuf,
    /// CronTool 实例（热加载 cron 时用它重新指向新调度器）。
    /// 与 WebChannel 同款 Arc<Mutex<Option>> 槽位。
    pub cron_tool: Arc<std::sync::Mutex<Option<Arc<crate::tools::cron::CronTool>>>>,
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

/// 在实时作用域（当前工作目录 ∪ 受信目录 ∪ 家目录）内解析 `/file` 请求路径：
/// 绝对/相对路径均可（与 send 工具同一套 `validate_path_in_scope`，家目录作为
/// 固定额外范围）；相对路径按当前工作目录解析，若该处不存在则回退家目录
/// （如 `/upload` 产物 `uploads/<name>` 恒落家目录，`/move` 后相对引用不断链）。
pub fn resolve_file_in_scope(
    root: &Path,
    trusted: &[PathBuf],
    home: &Path,
    path: &str,
) -> Result<PathBuf, String> {
    let mut abs = crate::path_guard::validate_path_in_scope(root, trusted, path, Some(home))
        .map_err(|e| e.to_string())?;
    if !abs.exists() && !Path::new(path).is_absolute() {
        if let Ok(p) = crate::path_guard::validate_path(home, path, None) {
            abs = p;
        }
    }
    Ok(abs)
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
            // 每次都重新校验，避免 rebuild 后浏览器粘住旧的嵌入前端（曾导致 WebUI 静默黑屏）
            headers.insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-cache"),
            );
            (StatusCode::OK, headers, Body::from(asset.data.into_owned())).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

#[derive(Deserialize)]
pub struct TokenQuery {
    pub token: Option<String>,
}

/// GET /api/sessions 的查询参数（P5 W1）：token + 分页。
#[derive(Deserialize)]
pub struct SessionListQuery {
    pub token: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// GET /api/stats/tokens 的查询参数（plan.md W3）：token + 时间范围（天）。
#[derive(Deserialize)]
pub struct StatsQuery {
    pub token: Option<String>,
    pub days: Option<u32>,
}

#[derive(Deserialize)]
pub struct FilePathQueryWithToken {
    pub path: String,
    pub token: Option<String>,
}

/// 提供 token 的查询参数（TokenQuery / SessionListQuery 等共用 authorize）。
pub trait TokenProvider {
    fn token(&self) -> Option<&str>;
}

impl TokenProvider for TokenQuery {
    fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }
}

impl TokenProvider for SessionListQuery {
    fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }
}

impl TokenProvider for StatsQuery {
    fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }
}

/// 综合鉴权：header + cookie + query
pub fn authorize<T: TokenProvider>(state: &AppState, headers: &HeaderMap, q: &T) -> bool {
    let cookie = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let provided = extract_token(headers, cookie, q.token());
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

/// GET /file?path=<path>&token=<token>：返回作用域内文件流。
/// 作用域与 send 工具同源：当前工作目录（workspace_root，/move 可变）∪ 受信目录
/// ∪ agent 家目录——从 main Agent 实时读取。绝对路径与相对路径均可（相对路径按
/// 当前工作目录解析）。
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
    // 实时作用域取自 registry 缓存的 Arc（与 main Agent 同一份，/move 原地可见）。
    // 不锁 main：turn 持锁期间锁 main 会阻塞到回合结束，恰好卡住媒体回显的文件拉取。
    // 未接（如直接构造 AppState 的测试）→ 退化为静态家目录（旧行为）。
    let (root, trusted) = match (
        &state.registry.main_workspace_root,
        &state.registry.main_trusted_dirs,
    ) {
        (Some(r), Some(t)) => (r.read().await.clone(), t.read().await.clone()),
        _ => (state.workspace.clone(), Vec::new()),
    };
    let home = state.workspace.clone();
    match resolve_file_in_scope(&root, &trusted, &home, &q.path) {
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
    for (ch, v) in [
        (&mut config.channels.qq.app_secret, true),
        (&mut config.channels.telegram.bot_token, true),
        (&mut config.channels.dingtalk.client_secret, true),
        (&mut config.channels.mail.imap_pass, true),
        (&mut config.channels.mail.smtp_pass, true),
        (&mut config.channels.feishu.app_secret, true),
    ] {
        if v && !ch.is_empty() {
            *ch = MASK.into();
        }
    }
    if !config.webui.token.is_empty() {
        config.webui.token = MASK.into();
    }
    if !config.tools.tavily.api_key.is_empty() {
        config.tools.tavily.api_key = MASK.into();
    }
    if !config.tools.baidu.api_key.is_empty() {
        config.tools.baidu.api_key = MASK.into();
    }
    if !config.tools.brave.api_key.is_empty() {
        config.tools.brave.api_key = MASK.into();
    }
    if !config.tools.tts.api_key.is_empty() {
        config.tools.tts.api_key = MASK.into();
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
    macro_rules! keep {
        ($dst:expr, $src:expr) => {
            if $dst == MASK {
                $dst = $src.clone();
            }
        };
    }
    keep!(merged.channels.qq.app_secret, old.channels.qq.app_secret);
    keep!(
        merged.channels.telegram.bot_token,
        old.channels.telegram.bot_token
    );
    keep!(
        merged.channels.dingtalk.client_secret,
        old.channels.dingtalk.client_secret
    );
    keep!(merged.channels.mail.imap_pass, old.channels.mail.imap_pass);
    keep!(merged.channels.mail.smtp_pass, old.channels.mail.smtp_pass);
    keep!(
        merged.channels.feishu.app_secret,
        old.channels.feishu.app_secret
    );
    if merged.webui.token == MASK {
        merged.webui.token = old.webui.token.clone();
    }
    if merged.tools.tavily.api_key == MASK {
        merged.tools.tavily.api_key = old.tools.tavily.api_key.clone();
    }
    if merged.tools.baidu.api_key == MASK {
        merged.tools.baidu.api_key = old.tools.baidu.api_key.clone();
    }
    if merged.tools.brave.api_key == MASK {
        merged.tools.brave.api_key = old.tools.brave.api_key.clone();
    }
    if merged.tools.tts.api_key == MASK {
        merged.tools.tts.api_key = old.tools.tts.api_key.clone();
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
    let mut merged = merge_masked(&old, &new_config);
    // 内存态副本先于 apply_refs 克隆：新落盘 secret 以明文留在内存（磁盘仍写 ${VAR}
    // 引用）。若等 apply_refs 后才克隆，expand_config_secrets 会对刚写入磁盘、尚未进
    // 入当前进程 env 的变量做 std::env::var 查询 → 命中「未设置」降级：报该警告并把
    // 新 provider 的 key 压成空串，热加载失效直到重启。此副本明文透传，不触发查询。
    let mut runtime_config = merged.clone();

    // P5 S1 敏感信息 .env 自动化：明文敏感字段先写入 .env（成功才替换为 ${VAR} 引用，
    // 失败保留明文 + warn 降级，保证配置保存不因安全改造而失败）。
    let secrets = crate::config::secrets::collect_plaintext_secrets(&merged);
    if !secrets.is_empty() {
        let env_path = state.config_path.parent().map(|p| p.join(".env"));
        if let Some(env_path) = env_path {
            let updates: Vec<(String, String)> = secrets
                .iter()
                .map(|e| (e.var.clone(), e.value.clone()))
                .collect();
            match crate::config::secrets::upsert_env(&env_path, &updates) {
                Ok(()) => {
                    crate::config::secrets::apply_refs(&mut merged, &secrets);
                    tracing::info!(count = secrets.len(), "plaintext secrets moved to .env");
                }
                Err(e) => {
                    tracing::warn!(error = %e, ".env write failed; secrets kept inline in config.toml");
                }
            }
        }
    }

    // main agent 不可删除：[agent.main] 是系统必需 section
    if !merged.agent.contains_key("main") {
        return json_err(
            StatusCode::BAD_REQUEST,
            "[agent.main] is not deletable: the main agent is a required system configuration",
        );
    }

    let toml_str = match std::fs::read_to_string(&state.config_path) {
        Ok(disk) => match merge_config_preserving_comments(&disk, &merged) {
            Ok(s) => s,
            Err(e) => {
                // 合并失败（如盘上 TOML 临时不可解析）不阻断保存：退回全量重写
                tracing::warn!(error = %e, "config comment-preserving merge failed; falling back to plain serialize");
                match toml::to_string_pretty(&merged) {
                    Ok(s) => s,
                    Err(e) => {
                        return json_err(StatusCode::BAD_REQUEST, &format!("serialize: {}", e))
                    }
                }
            }
        },
        // 盘上无文件（极少见：serve 已加载才走到这）：退回全量重写
        Err(_) => match toml::to_string_pretty(&merged) {
            Ok(s) => s,
            Err(e) => return json_err(StatusCode::BAD_REQUEST, &format!("serialize: {}", e)),
        },
    };

    // 先尝试构建新 provider：失败则不写盘、不更新内存（回滚到旧 config）。
    // 注意：写盘保留 ${VAR} 引用，但内存态须展开为明文（build_provider 不认 ${VAR}；
    // 下次启动 Config::load → expand_paths 再展开，行为一致）。runtime_config 已在
    // apply_refs 之前克隆，新落盘 secret 保持明文，这里只展开旧的 ${VAR} 引用。
    crate::config::secrets::expand_config_secrets(&mut runtime_config);
    if let Err(e) = build_provider_from_config(&runtime_config) {
        return json_err(
            StatusCode::BAD_REQUEST,
            &format!("failed to build provider, config not saved: {}", e),
        );
    }

    if let Err(e) = std::fs::write(&state.config_path, &toml_str) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("write: {}", e));
    }
    *state.config.write().await = runtime_config.clone();

    // provider 热加载（失败仅 warn，不阻断其它子系统的热加载）
    if let Err(e) = hot_reload_providers(&state, &runtime_config).await {
        tracing::warn!(error = %e, "hot_reload_providers failed (config saved)");
    }

    // 全量热加载：runtime / skills / mcp / cron / sub-agents（P4-f 轻量版）
    // 用展开后的 runtime_config（子 agent 构建同样需要真实 api_key）
    let config_dir = state.config_path.parent().unwrap_or_else(|| Path::new("."));
    let notes = reload_all(&state, &runtime_config, config_dir).await;
    axum::Json(serde_json::json!({
        "ok": true,
        "note": format!("config saved; {}", notes.join("; "))
    }))
    .into_response()
}

/// 结构化保存时保留磁盘上未改动段落的注释（替代旧的 `toml::to_string_pretty`
/// 全量重写——后者会丢掉所有注释）。
///
/// 做法：解析磁盘现有 TOML（含注释）→ 把 `merged` 序列化成 TOML 文档 → 逐 key 合并：
/// - `provider` / `agent` 子树走「覆盖 + 删除缺失」（replace 模式），以支持表单删除
///   provider / agent / model；
/// - `runtime` / `log` / `webui` / `channels` / `tools` 走「保留缺失」（preserve 模式），
///   保住表单未暴露的字段（如 `runtime.compact_model` / `vision_model`、`provider.compat`）
///   与这些段落的注释。
///
/// 注释之所以能保留：表单加载时会把完整 `Config`（含隐藏字段）回传，`merged` 是完整的，
/// 因此合并不会误删未改动项，仅覆盖表单实际改动的值。
fn merge_config_preserving_comments(disk_text: &str, merged: &Config) -> Result<String, String> {
    let mut disk_doc = DocumentMut::from_str(disk_text)
        .map_err(|e| format!("parse existing config.toml: {}", e))?;
    let new_toml = toml::to_string(merged).map_err(|e| format!("serialize config: {}", e))?;
    let new_doc =
        DocumentMut::from_str(&new_toml).map_err(|e| format!("parse serialized config: {}", e))?;

    let disk_tbl = disk_doc.as_table_mut();
    let new_tbl = new_doc.as_table();
    for (key, src_item) in new_tbl.iter() {
        // provider / agent 由表单完整管理，允许删除缺失项（表单删 provider/agent/model）
        let replace = key == "provider" || key == "agent";
        match disk_tbl.entry(key) {
            Entry::Occupied(mut e) => merge_item(e.get_mut(), src_item, replace),
            Entry::Vacant(e) => {
                e.insert(src_item.clone());
            }
        }
    }
    Ok(disk_doc.to_string())
}

/// 合并单个 item：两边都是表则递归；否则（标量 / 数组）直接覆盖。
fn merge_item(target: &mut Item, src: &Item, replace: bool) {
    match target {
        Item::Table(tt) => {
            if let Item::Table(st) = src {
                merge_tables(tt, st, replace);
            } else {
                *target = src.clone();
            }
        }
        _ => *target = src.clone(),
    }
}

fn merge_tables(target: &mut Table, src: &Table, replace: bool) {
    for (key, src_item) in src.iter() {
        match target.entry(key) {
            Entry::Occupied(mut e) => merge_item(e.get_mut(), src_item, replace),
            Entry::Vacant(e) => {
                e.insert(src_item.clone());
            }
        }
    }
    // replace 模式：删除 target 中 src 不存在的 key（表单删除的 provider/agent/model）
    if replace {
        let absent: Vec<String> = target
            .iter()
            .filter(|(k, _)| !src.contains_key(k))
            .map(|(k, _)| k.to_string())
            .collect();
        for k in absent {
            target.remove(&k);
        }
    }
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
    let new_vision = build_vision_provider_from_config(new_config);
    let agent = state.registry.main.lock().await;
    agent.reload_provider(new_provider).await;
    agent.reload_compact_provider(new_compact).await;
    agent.reload_vision_provider(new_vision).await;
    Ok(())
}

/// 全量热加载（P4-f 轻量版）：保存配置后，除 provider 外，再热加载
/// runtime 参数、skills、MCP 连接、cron 调度、子 Agent 定义。
/// 进行中的 turn 不受影响（各子系统均基于 Arc/RwLock 的 snapshot 语义）。
///
/// 返回人类可读的加载结果清单，用于 WebUI 反馈。
async fn reload_all(state: &AppState, merged: &Config, config_dir: &Path) -> Vec<String> {
    let mut notes: Vec<String> = Vec::new();
    let registry = state.registry.clone();

    // 1. runtime 参数 + skills（main agent）
    let skills = crate::skill::loader::load_skills(&config_dir.join("skills"));
    {
        let mut main = registry.main.lock().await;
        main.reload_runtime(merged).await;
        let skills_prompt = crate::skill::prompt::build_skills_prompt(&skills);
        main.reload_skills(&skills_prompt);
    }
    notes.push("agent runtime + skills reloaded".into());

    // 2. MCP 热重连（写入内存工具集，供子 agent 重建复用）
    let (mcp_note, mcp_tools) = reconnect_mcp(state).await;
    notes.push(mcp_note);

    // 3. cron 热重载
    notes.push(reload_cron(state).await);

    // 4. 子 Agent 重建
    registry
        .rebuild_sub_agents(merged, config_dir, mcp_tools, &skills)
        .await;
    notes.push("sub-agents rebuilt".into());

    notes
}

/// MCP 热重连：重读 mcp.toml，连接所有 enabled server，替换内存中的工具集。
/// 返回 (人类可读结果, 新工具列表) —— 工具列表需交给子 agent 重建复用。
async fn reconnect_mcp(state: &AppState) -> (String, Vec<Arc<dyn crate::tools::Tool>>) {
    let mcp_cfg = match crate::mcp::McpConfig::load(&state.mcp_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "mcp.toml load failed during reload");
            return (format!("mcp config load failed: {e}"), Vec::new());
        }
    };
    let new_mcp = Arc::new(crate::mcp::client::McpRegistry::connect_all(&mcp_cfg.server).await);
    let mut mcp_tools: Vec<Arc<dyn crate::tools::Tool>> = Vec::new();
    for (prefixed, def) in new_mcp.tool_defs().await {
        mcp_tools.push(Arc::new(crate::tools::mcp::McpTool::new(
            prefixed,
            def,
            new_mcp.clone(),
        )));
    }
    *state.mcp_registry.lock().unwrap() = Some(new_mcp.clone());
    let registry = state.registry.clone();
    registry
        .main
        .lock()
        .await
        .tools
        .replace_mcp_tools(mcp_tools.clone());
    for alias in registry.available_sub_agents() {
        if let Ok(a) = registry.get(&alias) {
            a.lock().await.tools.replace_mcp_tools(mcp_tools.clone());
        }
    }
    (
        format!("mcp reconnected ({} tools)", mcp_tools.len()),
        mcp_tools,
    )
}

/// cron 热重载：重读 cron.toml，复用已运行的调度器实例重排任务（不重启后台 ticker）。
async fn reload_cron(state: &AppState) -> String {
    // 先把 Option<Arc<CronHandle>> clone 出锁，避免 std::sync::MutexGuard
    // 跨 await 存活导致 future 非 Send（axum handler 要求 Send）。
    let cron_handle = state.cron_scheduler.lock().unwrap().clone();
    match cron_handle {
        Some(handle) => match handle.reload(&state.cron_path).await {
            Ok(()) => "cron rescheduled".into(),
            Err(e) => format!("cron reload failed: {e}"),
        },
        None => "cron scheduler not running".into(),
    }
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

fn build_vision_provider_from_config(config: &Config) -> Option<Arc<dyn Provider>> {
    let m = config.runtime.vision_model.as_ref()?;
    if m.is_empty() {
        return None;
    }
    match crate::provider::provider_from_ref(config, m) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(error = %e, model = m.as_str(), "build vision_provider failed");
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
/// 替代进程延迟 ~1s 启动，等旧进程释放端口（web channel bind 带重试兜底竞态）。
/// 容器内拒绝：exit(0) 会终止 PID 1 导致容器整体退出，替代进程无从接管。
pub async fn restart_service(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    if in_container() {
        return json_err(
            StatusCode::BAD_REQUEST,
            "running inside a container: self-restart would stop the container; restart the container instead",
        );
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
    let log_path = config_dir.join("logs").join("restart.log");
    match spawn_replacement(&exe, &config_dir, &log_path) {
        Ok(path) => {
            tracing::info!(
                "restart requested: replacement process spawned (output -> {}), exiting",
                path.display()
            );
        }
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
    // 先把响应送达浏览器，再延迟退出（给 axum 刷出响应的时间）
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        std::process::exit(0);
    });
    axum::Json(serde_json::json!({ "restarting": true })).into_response()
}

/// POST /api/shutdown → 优雅停止 serve 进程（ADR-0018）。
/// 触发 shutdown_signal，serve_cmd 的 select! 监听到后执行统一清理（cron 停止 + task abort）并退出。
/// 容器内允许：与 /api/restart 不同，shutdown 只是退出、不 spawn 替代进程，不会孤立 PID 1。
pub async fn shutdown_service(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    state.shutdown_signal.notify_one();
    axum::Json(serde_json::json!({ "ok": true, "note": "shutdown signaled" })).into_response()
}

/// 容器环境探测：docker/podman 会建 /.dockerenv 或设 container 环境变量。
fn in_container() -> bool {
    std::path::Path::new("/.dockerenv").exists() || std::env::var_os("container").is_some()
}

/// spawn 替代进程：Windows 用 cmd（ping 延时），Unix 用 sh（sleep 延时）。
/// stdout/stderr 重定向到 logs/restart.log——否则替代进程启动失败时错误无人可见
/// （serve 常无控制台，且旧进程 300ms 后就退出）。
/// 返回 Ok(log_path) 表示 spawn 成功。
fn spawn_replacement(exe: &Path, config_dir: &Path, log_path: &Path) -> Result<PathBuf, String> {
    let exe_s = exe.display().to_string();
    let dir_s = config_dir.display().to_string();
    // 确保 logs 目录存在（serve 正常启动时 log::init 已建，这里防御性兜底）
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| format!("open restart log {}: {}", log_path.display(), e))?;
    let stderr = log_file
        .try_clone()
        .map_err(|e| format!("clone restart log handle: {}", e))?;
    let spawn_result = {
        #[cfg(windows)]
        {
            // 必须用 raw_arg 直传命令行：std 的 args() 会给整个 script 再包一层引号，
            // cmd /c 首尾有引号时只剥最外层、内部引号保留为字面量，
            // 导致把 "E:\...\llaia.exe"（带引号）当可执行名查找而报错。
            use std::os::windows::process::CommandExt;
            let script = format!(
                "ping -n 2 127.0.0.1 >nul & \"{}\" --config-dir \"{}\" serve",
                exe_s, dir_s
            );
            let mut c = std::process::Command::new("cmd");
            c.raw_arg("/C");
            c.raw_arg(&script);
            c.stdout(std::process::Stdio::from(log_file))
                .stderr(std::process::Stdio::from(stderr));
            c.spawn()
        }
        #[cfg(not(windows))]
        {
            let script = format!(
                "sleep 1 && exec \"{}\" --config-dir \"{}\" serve",
                exe_s, dir_s
            );
            std::process::Command::new("sh")
                .args(["-c", &script])
                .stdout(std::process::Stdio::from(log_file))
                .stderr(std::process::Stdio::from(stderr))
                .spawn()
        }
    };
    spawn_result
        .map(|_| log_path.to_path_buf())
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
            "[agent.main] is not deletable: the main agent is a required system configuration",
        );
    }

    // 尝试构建新 provider：失败则不写盘（回滚到旧 config）
    if let Err(e) = build_provider_from_config(&parsed) {
        return json_err(
            StatusCode::BAD_REQUEST,
            &format!("failed to build provider, config not saved: {}", e),
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

    // provider 热加载（失败仅 warn，不阻断其它子系统的热加载）
    if let Err(e) = hot_reload_providers(&state, &parsed).await {
        tracing::warn!(error = %e, "hot_reload_providers failed (config saved)");
    }

    // 全量热加载：runtime / skills / mcp / cron / sub-agents（P4-f 轻量版）
    let config_dir = state.config_path.parent().unwrap_or_else(|| Path::new("."));
    let notes = reload_all(&state, &parsed, config_dir).await;
    axum::Json(serde_json::json!({
        "ok": true,
        "note": format!("config saved; {}", notes.join("; "))
    }))
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

/// 三段了点号版本号比较：`1.2.3 > 1.2.2`；忽略 pre-release/build 后缀（`1.2.3-beta` → `1.2.3`）。
/// 任一侧解析不出三段整型返回 `None`（无法比较）。
fn cmp_version(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    fn segs(v: &str) -> Vec<u64> {
        v.split(['-', '+'])
            .next()
            .unwrap_or("")
            .split('.')
            .filter_map(|s| s.parse::<u64>().ok())
            .collect()
    }
    let (va, vb) = (segs(a), segs(b));
    if va.is_empty() || vb.is_empty() {
        return None;
    }
    for i in 0..va.len().max(vb.len()) {
        match va
            .get(i)
            .copied()
            .unwrap_or(0)
            .cmp(&vb.get(i).copied().unwrap_or(0))
        {
            std::cmp::Ordering::Equal => continue,
            o => return Some(o),
        }
    }
    Some(std::cmp::Ordering::Equal)
}

/// GET /api/update/check 的结果（含失败原因，供前端展示）。
#[derive(Clone, serde::Serialize)]
struct UpdateCheck {
    current: String,
    latest: Option<String>,
    update_available: bool,
    url: Option<String>,
    error: Option<String>,
}

/// GitHub Releases latest 的分钟级内存缓存（防连点触无鉴权 60 次/h 限流）。
#[allow(clippy::type_complexity)]
static UPDATE_CACHE: std::sync::OnceLock<
    std::sync::Mutex<Option<(std::time::Instant, Result<UpdateCheck, String>)>>,
> = std::sync::OnceLock::new();

fn update_cache(
) -> &'static std::sync::Mutex<Option<(std::time::Instant, Result<UpdateCheck, String>)>> {
    UPDATE_CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

/// GET /api/update/check → 通过 GitHub Releases latest API 检查是否有新版。
///
/// 该 API 必须带 User-Agent 头否则 403（`https://api.github.com/repos/Feihei/llaia/releases/latest`）；
/// 返回的 `tag_name` 去 `v` 前缀后与当前版本做三段数值比较。结果做分钟级内存缓存。
pub async fn check_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let current = env!("CARGO_PKG_VERSION").to_string();

    let cache = update_cache();
    let now = std::time::Instant::now();
    let cached = cache.lock().unwrap_or_else(|p| p.into_inner()).take();
    let result = if let Some((at, cached)) = cached {
        if now.duration_since(at) < std::time::Duration::from_secs(60) {
            cached
        } else {
            fetch_latest_release().await
        }
    } else {
        fetch_latest_release().await
    };
    *cache.lock().unwrap_or_else(|p| p.into_inner()) =
        Some((std::time::Instant::now(), result.clone()));

    match result {
        Ok(info) => {
            let json = serde_json::json!(info);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                json.to_string(),
            )
                .into_response()
        }
        Err(e) => {
            // 失败透传原因：客户端已是最新或暂时无法获取时，让前端展示可读信息而非报错
            let json = serde_json::json!({
                "current": current,
                "latest": null,
                "update_available": false,
                "url": null,
                "error": e,
            });
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                json.to_string(),
            )
                .into_response()
        }
    }
}

async fn fetch_latest_release() -> Result<UpdateCheck, String> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("llaia-update-check")
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let resp = client
        .get("https://api.github.com/repos/Feihei/llaia/releases/latest")
        .send()
        .await
        .map_err(|e| format!("query GitHub releases: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(format!("GitHub API {} (离线或限流，稍后再试)", status));
    }
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse GitHub response: {e}"))?;
    let tag = json.get("tag_name").and_then(|v| v.as_str()).unwrap_or("");
    let url = json
        .get("html_url")
        .and_then(|v| v.as_str())
        .map(ToString::to_string);
    let latest = tag.strip_prefix('v').unwrap_or(tag).to_string();
    let update_available = cmp_version(&latest, &current)
        .map(|o| o == std::cmp::Ordering::Greater)
        .unwrap_or(false);
    Ok(UpdateCheck {
        current,
        latest: (!latest.is_empty()).then_some(latest),
        update_available,
        url,
        error: None,
    })
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
    let scheduler = state.cron_scheduler.lock().unwrap().clone();
    match scheduler {
        Some(s) => {
            let tasks = s.scheduler.list_tasks().await;
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
    let scheduler = state.cron_scheduler.lock().unwrap().clone();
    match scheduler {
        Some(s) => match s.scheduler.trigger(&id).await {
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
    // 写盘后热重载，无需重启
    let note = reload_cron(&state).await;
    tracing::info!(
        tasks = cfg.task.len(),
        note = %note,
        "cron.toml updated (hot-reloaded)"
    );
    axum::Json(serde_json::json!({
        "ok": true,
        "note": format!("cron.toml saved; {}", note)
    }))
    .into_response()
}

// ───────────────────────── MCP API ─────────────────────────

/// GET /api/mcp → MCP server 列表（含每个 server 的工具清单与连接状态）
///
/// 与旧实现（只返回 registry 中已连接的 server）不同：这里以 `mcp.toml` 的
/// 配置为准，列出 **全部** server —— 包括被 `enabled = false` 关掉的。否则被关掉的
/// server 在 `connect_all` 阶段就被跳过，UI 上看不到、也无法重新打开。
/// 连接状态（`status`/`error`/`tools`）叠加自内存 registry：
/// - 未启用 → `status = "disabled"`
/// - 启用且已连上 / 失败 → registry 的 `connected` / `dead`
/// - 启用但 registry 中尚无记录（如刚开、尚未重连）→ `unknown`
pub async fn list_mcp(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let cfg = crate::mcp::McpConfig::load(&state.mcp_path).unwrap_or_default();
    let registry = state.mcp_registry.lock().unwrap().clone();
    // registry 状态按 id 建索引（仅含已启用且 connect 过的；失败进 failed）
    let mut live_by_id: HashMap<String, serde_json::Value> = HashMap::new();
    if let Some(reg) = &registry {
        for s in reg.status().await {
            if let Some(id) = s.get("id").and_then(|v| v.as_str()) {
                live_by_id.insert(id.to_string(), s.clone());
            }
        }
    }
    let mut out = Vec::new();
    for srv in &cfg.server {
        let live = live_by_id.get(&srv.id);
        let (status, error, tools) = if !srv.enabled {
            (
                "disabled".to_string(),
                serde_json::Value::Null,
                serde_json::Value::Null,
            )
        } else if let Some(l) = live {
            (
                l.get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                l.get("error").cloned().unwrap_or(serde_json::Value::Null),
                l.get("tools").cloned().unwrap_or(serde_json::Value::Null),
            )
        } else {
            (
                "unknown".to_string(),
                serde_json::Value::Null,
                serde_json::Value::Null,
            )
        };
        out.push(serde_json::json!({
            "id": srv.id,
            "enabled": srv.enabled,
            "transport": srv.transport,
            "command": srv.command,
            "url": srv.url,
            "args": srv.args,
            "status": status,
            "error": error,
            "tools": tools,
        }));
    }
    axum::Json(serde_json::json!({ "servers": out })).into_response()
}

/// PUT /api/mcp → 保存 server 的 `enabled` 开关到 mcp.toml（结构化，不丢注释 / `${VAR}`）
///
/// 用 toml_edit 定点改写每个 `[[server]]` 的 `enabled` 字段，保留原文件其余内容
/// （注释、环境变量 `${VAR}` 插值、格式）。改完校验 + 写盘 + 热重连。
#[derive(Deserialize)]
pub struct McpServerPatch {
    pub id: String,
    pub enabled: bool,
}

#[derive(Deserialize)]
pub struct McpSaveBody {
    pub servers: Vec<McpServerPatch>,
}

pub async fn save_mcp(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::Json(body): axum::Json<McpSaveBody>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    if !state.mcp_path.exists() {
        return json_err(
            StatusCode::BAD_REQUEST,
            "no mcp.toml yet; add a server first",
        );
    }
    let raw = match std::fs::read_to_string(&state.mcp_path) {
        Ok(r) => r,
        Err(e) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("read mcp.toml: {e}"),
            )
        }
    };
    let patches: Vec<(String, bool)> = body
        .servers
        .iter()
        .map(|s| (s.id.clone(), s.enabled))
        .collect();
    let new_raw = match patch_mcp_enabled(&raw, &patches) {
        Ok(r) => r,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, &e),
    };
    if let Err(e) = crate::mcp::McpConfig::from_str_validate(&new_raw) {
        return json_err(
            StatusCode::BAD_REQUEST,
            &format!("invalid after patch: {e}"),
        );
    }
    if let Err(e) = std::fs::write(&state.mcp_path, &new_raw) {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("write mcp.toml: {e}"),
        );
    }
    let (note, _tools) = reconnect_mcp(&state).await;
    axum::Json(serde_json::json!({
        "ok": true,
        "note": format!("mcp.toml saved; {}", note)
    }))
    .into_response()
}

/// 用 toml_edit 定点改写 mcp.toml 里每个 `[[server]]` 的 `enabled`，返回改完的文本。
/// 保留原文件其余内容：注释、`${VAR}` 环境变量插值、格式都原样保留。
fn patch_mcp_enabled(raw: &str, patches: &[(String, bool)]) -> Result<String, String> {
    let mut doc = DocumentMut::from_str(raw).map_err(|e| format!("parse mcp.toml: {e}"))?;
    let patch: HashMap<&str, bool> = patches.iter().map(|(id, en)| (id.as_str(), *en)).collect();
    let mut matched = 0usize;
    if let Some(servers) = doc
        .get_mut("server")
        .and_then(|v| v.as_array_of_tables_mut())
    {
        for tbl in servers.iter_mut() {
            if let Some(idv) = tbl.get("id").and_then(|v| v.as_str()) {
                if let Some(&en) = patch.get(idv) {
                    // 原地改写 bool，保留原值的 decor（含同行尾注）。
                    // 仅在原本不是 bool 时（极少见）才整值替换，会丢该行尾注。
                    let mut done = false;
                    if let Some(v) = tbl.get_mut("enabled").and_then(|i| i.as_value_mut()) {
                        if let toml_edit::Value::Boolean(b) = v {
                            let decor = b.decor().clone();
                            let mut nb = toml_edit::Value::from(en);
                            let dm = nb.decor_mut();
                            dm.set_prefix(
                                decor
                                    .prefix()
                                    .and_then(|r| r.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            );
                            dm.set_suffix(
                                decor
                                    .suffix()
                                    .and_then(|r| r.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            );
                            *v = nb;
                            done = true;
                        }
                    }
                    if !done {
                        tbl.insert("enabled", toml_edit::value(en));
                    }
                    matched += 1;
                }
            }
        }
    }
    if matched == 0 {
        return Err("no matching server id found in mcp.toml".to_string());
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod mcp_patch_tests {
    use super::patch_mcp_enabled;

    #[test]
    fn toggles_enabled_and_keeps_comments_and_env_var() {
        let raw = r#"# top comment preserved
[[server]]
id = "a"
enabled = true          # keep me
transport = "stdio"
command = "run"
env = { TOKEN = "${MY_TOKEN}" }   # interpolation must survive

[[server]]
id = "b"
enabled = true
transport = "http"
url = "https://x"
"#;
        // 关掉 a、打开 b（b 已是 true，幂等）
        let out =
            patch_mcp_enabled(raw, &[("a".into(), false), ("b".into(), true)]).expect("patch ok");
        // 注释保留
        assert!(
            out.contains("# top comment preserved"),
            "comment lost:\n{}",
            out
        );
        assert!(out.contains("# keep me"), "inline comment lost:\n{}", out);
        // ${VAR} 插值原样保留（不能因为序列化被展开）
        assert!(
            out.contains("${MY_TOKEN}"),
            "env var interpolation lost:\n{}",
            out
        );
        // enabled 已改
        let a = out.split("id = \"a\"").nth(1).unwrap();
        let a_block = a.split("[[server]]").next().unwrap();
        assert!(
            a_block.contains("enabled = false"),
            "a not disabled:\n{}",
            out
        );
        let b = out.split("id = \"b\"").nth(1).unwrap();
        assert!(b.contains("enabled = true"), "b not kept on:\n{}", out);
        // 改动后仍可被 mcp 解析
        crate::mcp::McpConfig::from_str_validate(&out).expect("patched toml must validate");
    }

    #[test]
    fn errors_when_id_missing() {
        let raw = "[[server]]\nid = \"a\"\nenabled = true\n";
        let err = patch_mcp_enabled(raw, &[("ghost".into(), false)]).unwrap_err();
        assert!(err.contains("no matching server"), "got: {}", err);
    }
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
    // 写盘后热重连，无需重启
    let (note, _tools) = reconnect_mcp(&state).await;
    tracing::info!(
        servers = cfg.server.len(),
        note = %note,
        "mcp.toml updated (hot-reloaded)"
    );
    axum::Json(serde_json::json!({
        "ok": true,
        "note": format!("mcp.toml saved; {}", note)
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

/// GET /api/todos → 当前会话 todo 清单（只读展示，ADR-0024）。
pub async fn get_todos(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let agent = state.registry.main.lock().await;
    let items = agent.tools.todo_store.list().unwrap_or_default();
    let json = serde_json::json!({ "todos": items });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        json.to_string(),
    )
        .into_response()
}

/// GET /api/questions → 当前待回答问题（只读展示，ADR-0022）。
pub async fn get_questions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let agent = state.registry.main.lock().await;
    let questions: Vec<serde_json::Value> = agent
        .approval_gate
        .questions()
        .await
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "question": p.question,
                "choices": p.choices,
                "channel": p.channel,
            })
        })
        .collect();
    let json = serde_json::json!({ "questions": questions });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        json.to_string(),
    )
        .into_response()
}

/// GET /api/env → 环境探测结果（只读展示，P6）。返回启动时（或 `/env` / 刷新按钮）
/// 探测到的本机工具链缓存文本；未探测到时 `env` 为空串。
pub async fn get_env(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let agent = state.registry.main.lock().await;
    let env = agent.context.env_state.clone().unwrap_or_default();
    let json = serde_json::json!({ "env": env });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        json.to_string(),
    )
        .into_response()
}

/// POST /api/env/refresh → 重新探测本机环境（同 `/env` 命令）。
pub async fn refresh_env(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let mut agent = state.registry.main.lock().await;
    let env_text = crate::envprobe::probe().await;
    agent.context.env_state = (!env_text.is_empty()).then_some(env_text.clone());
    let json = serde_json::json!({ "env": env_text });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        json.to_string(),
    )
        .into_response()
}

/// GET /api/doctor → 运行诊断检查（provider 连通性 / context_size 探测 / .env /
/// sessions.db / cron / mcp / skills）。结果为结构化 check 列表，前端 Config 页展示。
pub async fn get_doctor(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let config_dir = state.registry.main.lock().await.config_dir.clone();
    match crate::commands::doctor_checks(&config_dir).await {
        Ok(checks) => {
            let json = serde_json::json!({ "checks": checks });
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                json.to_string(),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("doctor failed: {e}"),
        )
            .into_response(),
    }
}

// ---- P5 W1 WebUI 会话历史 ----

/// GET /api/sessions → 会话列表（含消息数），按 last_activity 降序，分页。
pub async fn list_sessions_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<SessionListQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let agent = state.registry.main.lock().await;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let sessions = agent
        .session_store
        .list_sessions(limit, offset)
        .unwrap_or_default();
    let json = serde_json::json!({ "sessions": sessions });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        json.to_string(),
    )
        .into_response()
}

/// GET /api/stats/tokens?days=N → token 用量聚合（plan.md W3）。
/// N 缺省 7，线上 clamp 到 1..=30。单用户数据量小，直接单查询聚合即可。
pub async fn stats_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<StatsQuery>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let days = q.days.unwrap_or(7).clamp(1, 30);
    let agent = state.registry.main.lock().await;
    let stats = agent.session_store.token_stats(days).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "token_stats query failed");
        crate::memory::sqlite::TokenStats {
            days,
            total_prompt: 0,
            total_completion: 0,
            total_tokens: 0,
            total_requests: 0,
            series: Vec::new(),
            by_model: Vec::new(),
            by_session: Vec::new(),
        }
    });
    let json = serde_json::json!(stats);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        json.to_string(),
    )
        .into_response()
}
pub async fn get_session_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::extract::Path(uuid): axum::extract::Path<String>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let agent = state.registry.main.lock().await;
    let Some((sid, row)) = agent.session_store.session_by_uuid(&uuid).unwrap_or(None) else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "session not found" })),
        )
            .into_response();
    };
    let messages = agent
        .session_store
        .messages_with_tool_calls(sid)
        .unwrap_or_default();
    let json = serde_json::json!({
        "session": row,
        "messages": messages,
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        json.to_string(),
    )
        .into_response()
}

/// DELETE /api/sessions/:uuid → 删除会话（cascade 删 messages/tool_calls）。
pub async fn delete_session_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::extract::Path(uuid): axum::extract::Path<String>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let agent = state.registry.main.lock().await;
    let deleted = agent.session_store.delete_session(&uuid).unwrap_or(false);
    if deleted {
        (
            StatusCode::OK,
            axum::Json(serde_json::json!({ "deleted": uuid })),
        )
            .into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "session not found" })),
        )
            .into_response()
    }
}

/// 删除消息请求体：`{ ids: [..] }`（单条时 ids 长度 1）。
#[derive(Deserialize)]
pub struct DeleteMessagesBody {
    pub ids: Option<Vec<i64>>,
}

/// 删除某会话内指定 ids 的消息（可多条）。仅清理历史存储，不改动 token 预算。
pub async fn delete_session_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<StatsQuery>,
    axum::extract::Path(uuid): axum::extract::Path<String>,
    body: Option<axum::extract::Json<DeleteMessagesBody>>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let agent = state.registry.main.lock().await;
    let Some((sid, _row)) = agent.session_store.session_by_uuid(&uuid).unwrap_or(None) else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "session not found" })),
        )
            .into_response();
    };
    let Some(b) = body else {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({ "error": "missing body" })),
        )
            .into_response();
    };
    let b = b.0; // 解包 Json 提取器
    let mut ids = b.ids.unwrap_or_default();
    if ids.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({ "error": "provide a non-empty `ids` list" })),
        )
            .into_response();
    }
    ids.sort_unstable();
    ids.dedup();
    // 校验所有 id 确实属于该会话，防止跨会话 id 误删
    for &id in &ids {
        if !agent
            .session_store
            .message_in_session(sid, id)
            .unwrap_or(false)
        {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": "message(s) not in this session" })),
            )
                .into_response();
        }
    }
    let deleted = agent.session_store.delete_messages(sid, &ids).unwrap_or(0);
    (
        StatusCode::OK,
        axum::Json(serde_json::json!({ "deleted": deleted })),
    )
        .into_response()
}

/// GET /api/sessions/:uuid/export → 导出会话为 JSON（消息 + 工具调用完整留底）。
pub async fn export_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::extract::Path(uuid): axum::extract::Path<String>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let agent = state.registry.main.lock().await;
    let Some((sid, row)) = agent.session_store.session_by_uuid(&uuid).unwrap_or(None) else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "session not found" })),
        )
            .into_response();
    };
    let messages = agent
        .session_store
        .messages_with_tool_calls(sid)
        .unwrap_or_default();
    let json = serde_json::json!({
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "session": row,
        "messages": messages,
    });
    let body = serde_json::to_string_pretty(&json).unwrap_or_else(|_| "{}".into());
    let disposition = axum::http::HeaderValue::from_str(&format!(
        "attachment; filename=\"session-{}.json\"",
        uuid
    ))
    .unwrap_or_else(|_| axum::http::HeaderValue::from_static("attachment"));
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/json; charset=utf-8"),
            ),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
        .into_response()
}

// ---- P5 W2 WebUI provider 模型探测 ----

/// POST /api/providers/:id/models 的请求体：可覆盖 base_url / api_key（默认用当前配置）。
#[derive(Deserialize)]
pub struct ProbeModelsBody {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
}

/// POST /api/providers/:id/models → 探测该 provider 的可用模型列表。
/// v1 仅 OpenAI 兼容端点（GET /models）。成功 `{ok:true, models:[{id,name}]}`，
/// 失败 `{ok:false, error}`（HTTP 200，便于前端统一渲染错误文本）。
pub async fn probe_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::Json(body): axum::Json<ProbeModelsBody>,
) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    let cfg = state.config.read().await;
    let Some(provider) = cfg.provider.get(&id) else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "provider not found" })),
        )
            .into_response();
    };
    let base_url = body.base_url.as_deref().unwrap_or(&provider.base_url);
    let api_key = body.api_key.as_deref().unwrap_or(&provider.api_key);
    match crate::provider::probe::probe_openai_compatible(base_url, Some(api_key)).await {
        Ok(models) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({ "ok": true, "models": models })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
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
        .route("/api/shutdown", axum::routing::post(shutdown_service))
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
        .route("/api/mcp", axum::routing::get(list_mcp).put(save_mcp))
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
        // 规划后执行（ADR-0024）：只读展示当前会话 todo 清单
        .route("/api/todos", axum::routing::get(get_todos))
        // ask_user（ADR-0022）：只读展示当前待回答问题
        .route("/api/questions", axum::routing::get(get_questions))
        .route("/api/env", axum::routing::get(get_env))
        .route("/api/env/refresh", axum::routing::post(refresh_env))
        .route("/api/doctor", axum::routing::get(get_doctor))
        .route("/api/update/check", axum::routing::get(check_update))
        .route("/api/stats/tokens", axum::routing::get(stats_tokens))
        // 会话历史（P5 W1）：列表 / 详情 / 删除 / 导出
        .route("/api/sessions", axum::routing::get(list_sessions_api))
        .route(
            "/api/sessions/:uuid",
            axum::routing::get(get_session_detail).delete(delete_session_api),
        )
        .route(
            "/api/sessions/:uuid/messages",
            axum::routing::delete(delete_session_messages),
        )
        .route(
            "/api/sessions/:uuid/export",
            axum::routing::get(export_session),
        )
        // 模型探测（P5 W2）：POST /api/providers/:id/models
        .route(
            "/api/providers/:id/models",
            axum::routing::post(probe_models),
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
    fn test_cmp_version() {
        use std::cmp::Ordering;
        // 正常三段比较
        assert_eq!(cmp_version("1.2.3", "1.2.2"), Some(Ordering::Greater));
        assert_eq!(cmp_version("1.2.3", "1.2.3"), Some(Ordering::Equal));
        assert_eq!(cmp_version("1.2.2", "1.2.3"), Some(Ordering::Less));
        // 忽略 pre-release / build 后缀
        assert_eq!(cmp_version("1.2.3-beta", "1.2.2"), Some(Ordering::Greater));
        assert_eq!(cmp_version("1.2.3", "1.2.3+build.7"), Some(Ordering::Equal));
        // 三段 vs 两段：缺位按 0 补
        assert_eq!(cmp_version("1.2.1", "1.2"), Some(Ordering::Greater));
        // 不可解析 → None
        assert_eq!(cmp_version("dev", "1.2.3"), None);
        assert_eq!(cmp_version("8.8.8.8", "1.2.3"), Some(Ordering::Greater));
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

    /// 回归（用户报告同源）：/move 后 WebUI `/file` 仍能取到媒体——
    /// moved 目录绝对/相对路径可达、家目录作为额外范围可达、
    /// 家目录相对产物（uploads/）回退不断链、作用域外拒绝。
    #[test]
    fn test_resolve_file_in_scope_after_move() {
        let home = tempfile::tempdir().unwrap();
        let moved = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let home_p = home.path().to_path_buf();
        let moved_p = moved.path().to_path_buf();

        fs::create_dir_all(home_p.join("uploads")).unwrap();
        fs::write(home_p.join("uploads/pic.png"), b"home upload").unwrap();
        fs::write(home_p.join("voice.mp3"), b"tts output").unwrap();
        fs::write(moved_p.join("report.pptx"), b"moved file").unwrap();
        fs::write(outside.path().join("stranger.pptx"), b"x").unwrap();

        // 模拟 /move：root=moved，受信集合登记 moved 目标，家目录不在受信里
        let trusted = vec![moved_p.clone()];

        // moved 目录内绝对路径（媒体事件携带的形态）→ 可达
        let abs = moved_p.join("report.pptx");
        assert!(
            resolve_file_in_scope(&moved_p, &trusted, &home_p, abs.to_str().unwrap()).is_ok(),
            "moved 目录内绝对路径应可达"
        );

        // 相对路径按新根解析
        assert!(
            resolve_file_in_scope(&moved_p, &trusted, &home_p, "report.pptx").is_ok(),
            "相对路径应解析到 moved 目录"
        );

        // 家目录不在受信集合，但作为额外范围恒可达（/move 后媒体不断联）
        let voice = home_p.join("voice.mp3");
        assert!(
            resolve_file_in_scope(&moved_p, &trusted, &home_p, voice.to_str().unwrap()).is_ok(),
            "家目录产物应可达"
        );

        // 家目录相对产物：新根下不存在 → 回退家目录解析（/upload 产物场景）
        let r = resolve_file_in_scope(&moved_p, &trusted, &home_p, "uploads/pic.png")
            .expect("uploads/ 相对路径应回退家目录");
        assert!(r.exists(), "回退后应指向家目录的真实文件");

        // 作用域之外 → 拒绝
        let stranger = outside.path().join("stranger.pptx");
        assert!(
            resolve_file_in_scope(&moved_p, &trusted, &home_p, stranger.to_str().unwrap()).is_err(),
            "作用域外路径必须被拒绝"
        );
    }

    fn sample_config() -> Config {
        let mut c = Config::default_for_workspace("/tmp/llaia-test");
        c.provider.get_mut("default").unwrap().api_key = "sk-secret".into();
        c.channels.qq.app_secret = "qq-secret".into();
        c.tools.tavily.api_key = "tvly-secret".into();
        c.tools.baidu.api_key = "bd-secret".into();
        c.tools.brave.api_key = "br-secret".into();
        c
    }

    #[test]
    fn test_mask_sensitive_redacts_secrets() {
        let masked = mask_sensitive(sample_config());
        assert_eq!(masked.provider.get("default").unwrap().api_key, "••••");
        assert_eq!(masked.channels.qq.app_secret, "••••");
        assert_eq!(masked.tools.tavily.api_key, "••••");
        assert_eq!(masked.tools.baidu.api_key, "••••");
        assert_eq!(masked.tools.brave.api_key, "••••");
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

    #[test]
    fn test_merge_config_preserves_comments_and_deletes_provider() {
        // 模拟盘上 TOML：多段落带注释 + 两个 provider + agent 无 fallback
        let disk = r#"
# 顶部注释：应保留
[runtime]
# runtime 注释：应保留
context_threshold = 0.7

[provider.local]
# local 注释：应保留
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[provider.cloud]
# cloud 注释：应被删除（表单删了此 provider）
type = "openai_compatible"
base_url = "http://cloud"

[agent.main]
# main 注释：应保留
model = "local.m"
workspace = ""
"#;
        // 表单回传的 merged：改了 runtime、删了 cloud provider、给 main 加了 fallback
        let mut merged: Config = toml::from_str(disk).expect("disk should parse");
        merged.runtime.context_threshold = 0.5;
        merged.provider.remove("cloud");
        merged
            .agent
            .get_mut("main")
            .unwrap()
            .fallback
            .push("local.m".into());

        let out = merge_config_preserving_comments(disk, &merged).expect("merge ok");

        // 注释保留
        assert!(
            out.contains("# 顶部注释：应保留"),
            "top comment lost:\n{}",
            out
        );
        assert!(
            out.contains("# runtime 注释：应保留"),
            "runtime comment lost:\n{}",
            out
        );
        assert!(
            out.contains("# local 注释：应保留"),
            "local comment lost:\n{}",
            out
        );
        assert!(
            out.contains("# main 注释：应保留"),
            "main comment lost:\n{}",
            out
        );
        // 表单改动生效
        assert!(
            out.contains("context_threshold = 0.5"),
            "runtime change lost:\n{}",
            out
        );
        assert!(out.contains("fallback"), "fallback not written:\n{}", out);
        // 表单删除生效（cloud provider 整段移除）
        assert!(
            !out.contains("provider.cloud"),
            "deleted provider still present:\n{}",
            out
        );
        assert!(
            !out.contains("# cloud 注释"),
            "deleted provider comment still present:\n{}",
            out
        );
    }

    #[test]
    fn test_merge_config_keeps_hidden_runtime_fields() {
        // runtime 表单只暴露部分字段；compact_model / vision_model 不应被丢
        let disk = r#"
[runtime]
context_threshold = 0.7
compact_model = "local.small"
vision_model = "local.vision"

[agent.main]
model = "local.m"
workspace = ""
"#;
        let mut merged: Config = toml::from_str(disk).expect("disk should parse");
        // 仅改动 context_threshold（模拟表单保存）
        merged.runtime.context_threshold = 0.8;

        let out = merge_config_preserving_comments(disk, &merged).expect("merge ok");
        assert!(
            out.contains("compact_model = \"local.small\""),
            "hidden compact_model lost:\n{}",
            out
        );
        assert!(
            out.contains("vision_model = \"local.vision\""),
            "hidden vision_model lost:\n{}",
            out
        );
        assert!(
            out.contains("context_threshold = 0.8"),
            "change lost:\n{}",
            out
        );
    }

    #[test]
    fn test_agent_fallback_persists_via_json_put_path() {
        // 模拟前端结构化保存回传的 JSON：provider 已 flat 回顶层、agent 含 fallback 列表
        let json = r#"{
            "runtime": { "context_threshold": 0.7 },
            "provider": {
                "local": { "type": "openai_compatible", "base_url": "http://localhost:11434/v1", "m": { "model": "qwen3", "native_tool_calling": true } }
            },
            "agent": {
                "main": { "model": "local.m", "fallback": ["local.m", "cloud.big"], "denied_tools": [], "delegate_timeout": 120, "memory_token_budget": 4000 }
            }
        }"#;
        let new_config: Config = serde_json::from_str(json).expect("frontend json should parse");
        assert_eq!(
            new_config.agent.get("main").unwrap().fallback,
            vec!["local.m".to_string(), "cloud.big".to_string()],
            "fallback lost during JSON deserialize"
        );
        let merged = merge_masked(&new_config, &new_config);
        let disk = r#"
[agent.main]
model = "local.m"
"#;
        let out = merge_config_preserving_comments(disk, &merged).expect("merge ok");
        assert!(
            out.contains("fallback") && out.contains("cloud.big"),
            "fallback not written to disk:\n{}",
            out
        );
    }

    /// 静态资源嵌入回归测试：防止 build.rs 未触发重编译导致 index.html/app.js/theme.css
    /// 的改动被 rust-embed 静默吞掉（沿用增量缓存里的旧副本）。
    #[test]
    fn test_static_assets_embedded_up_to_date() {
        let idx = StaticAsset::get("index.html")
            .expect("index.html embedded")
            .data
            .to_vec();
        let idx = String::from_utf8(idx).expect("index.html utf8");
        assert!(
            idx.contains("channel-grid") && idx.contains("webui-card"),
            "index.html missing channel-card markup (stale embed?)"
        );
        assert!(
            idx.contains("mcp-card") && idx.contains("saveMcp"),
            "index.html missing mcp collapsible-card markup (stale embed?)"
        );
        assert!(
            idx.contains("tool-result"),
            "index.html missing tool-result collapsible markup (stale embed?)"
        );
        assert!(
            idx.contains("m.role === 'tool'"),
            "index.html missing tool-message content folding (stale embed?)"
        );

        let js = StaticAsset::get("app.js")
            .expect("app.js embedded")
            .data
            .to_vec();
        let js = String::from_utf8(js).expect("app.js utf8");
        assert!(
            js.contains("channelCards"),
            "app.js missing channelCards metadata (stale embed?)"
        );
        assert!(
            js.contains("checkingUpdate")
                && js.contains("checkUpdate")
                && idx.contains("Check Updates"),
            "index.html/app.js missing plan.md W2 About update-check button (stale embed?)"
        );

        let css = StaticAsset::get("theme.css")
            .expect("theme.css embedded")
            .data
            .to_vec();
        let css = String::from_utf8(css).expect("theme.css utf8");
        assert!(
            css.contains(".channel-card"),
            "theme.css missing .channel-card styles (stale embed?)"
        );
        assert!(
            css.contains(".app-frame") && idx.contains("app-frame"),
            "index.html/theme.css missing plan.md W1 flex-height-chain fix (stale embed?)"
        );
        assert!(
            css.contains(".scroll-fab") && idx.contains("scroll-fab"),
            "index.html/theme.css missing plan.md W1 session scroll-fab buttons (stale embed?)"
        );
    }

    /// app.js 顶层键同名冲突回归测试：llaiaApp() 返回单个对象字面量，data 属性与方法
    /// 同名时后者覆盖前者。曾因 `probeModels` 既是结果 map（data）又是方法，方法体内
    /// `this.probeModels = {...}` 把方法覆盖成普通对象，导致首次探测后所有 Probe 按钮
    /// 点击抛 `probeModels is not a function`、需刷新页面才恢复（已改名 runProbe 修复）。
    /// 此检查兜住整个 bug 类别：任何顶层同名键（data/data、data/方法、方法/方法）都会失败。
    #[test]
    fn test_app_js_no_duplicate_top_level_keys() {
        let js = StaticAsset::get("app.js")
            .expect("app.js embedded")
            .data
            .to_vec();
        let js = String::from_utf8(js).expect("app.js utf8");
        // 顶层键：恰好 4 空格缩进的 `name:`（data）或 `async name(` / `name(`（方法）
        let re = regex::Regex::new(r"(?m)^    (?:async\s+)?([A-Za-z_$][A-Za-z0-9_$]*)(?:\s*:|\()")
            .expect("valid regex");
        let mut seen = std::collections::HashMap::<String, usize>::new();
        for cap in re.captures_iter(&js) {
            let key = cap[1].to_string();
            let line = 1 + js[..cap.get(0).unwrap().start()]
                .bytes()
                .filter(|&b| b == b'\n')
                .count();
            if let Some(&prev) = seen.get(&key) {
                panic!(
                    "app.js duplicate top-level key `{key}` (first at line {prev}, again at line {line}); \
                     data 属性与方法同名会相互覆盖，改名其一（参见 probeModels/runProbe 修复）"
                );
            }
            seen.insert(key, line);
        }
        assert!(
            seen.contains_key("runProbe"),
            "app.js missing runProbe method (probeModels fix was reverted?)"
        );
    }
}
