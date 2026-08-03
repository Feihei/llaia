# WebUI Channel 与配置可视化 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新增 WebChannel，浏览器可对话 + 配置可视化编辑，复用 P1.5 的 `run_turn` 抽象。

**Architecture:** axum HTTP server + WS，`WebSink` 实现 `OutputSink` 通过 mpsc 与 WS 写 task 解耦；REST `/api/config` 读写结构化配置与原始 TOML；前端 Alpine.js 单页 + CodeMirror 6，顶部 tab 切换 Chat / 配置 / 关于。

**Tech Stack:** Rust（axum 0.7 / tower-http / rust-embed / tokio-tungstenite 已有），前端（Alpine.js 3 / marked.js / highlight.js / CodeMirror 6，全部本地 vendor 零构建）。

**Spec:** `docs/specs/2026-08-03-webui-channel-design.md`

---

## 文件结构

| 文件 | 职责 |
|------|------|
| `Cargo.toml` | 加 axum / tower-http / rust-embed 依赖 |
| `build.rs` | 新建：注入 GIT_HASH 环境变量 |
| `src/config.rs` | 加 `WebConfig` + `ChannelsConfig.web` + `expand_paths` 处理 token + 默认值 |
| `src/channels/mod.rs` | `pub mod web;` + 重新导出 |
| `src/channels/web.rs` | 新建：WebEvent / WebSink / WebChannel / WsHandler / 路由 / config_api / about_api / 鉴权 |
| `src/channels/web/static/index.html` | 单页结构 + Alpine 组件 |
| `src/channels/web/static/app.js` | 主逻辑：tab / token / WS 客户端 |
| `src/channels/web/static/chat.js` | Chat tab：消息渲染 / 上传 / 中止 |
| `src/channels/web/static/config.js` | 配置 tab：表单 + CodeMirror + 保存 |
| `src/channels/web/static/about.js` | 关于 tab |
| `src/channels/web/static/vendor/*` | alpine.min.js / marked.min.js / highlight.min.js / codemirror/ |
| `src/commands/mod.rs` | `serve_cmd` 加 web channel 启动分支 |
| `tests/web_sink.rs` | WebSink 单元测试 |
| `tests/web_api.rs` | 路由集成测试（鉴权 / config / status / upload / file） |

---

## Task 1: 依赖、build.rs 与 WebConfig 配置 schema

**Files:**
- Modify: `Cargo.toml`
- Create: `build.rs`
- Modify: `src/config.rs`
- Test: `src/config.rs` 内 `#[cfg(test)]`

- [ ] **Step 1: 在 Cargo.toml 加依赖**

在 `[dependencies]` 末尾（`base64 = "0.22"` 之后、`mime_guess = "2"` 之后）追加：

```toml
axum = { version = "0.7", features = ["ws", "macros"] }
tower-http = { version = "0.5", features = ["fs"] }
rust-embed = "8"
rand = "0.8"
```

注意：`tokio-tungstenite` 已存在（QQ 用），axum 0.7 的 ws feature 内部用 `tungstenite`，与 tokio-tungstenite 不冲突（axum 自带）。`mime_guess` 已存在不重复加。

- [ ] **Step 2: 创建 build.rs**

在仓库根目录创建 `build.rs`：

```rust
fn main() {
    println!("cargo:rustc-env=GIT_HASH={}", git_hash());
    println!("cargo:rerun-if-changed=.git/HEAD");
}

fn git_hash() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
```

- [ ] **Step 3: 在 src/config.rs 加 WebConfig**

在 `QqConfig` 的 `impl Default` 块之后（约 165 行后）插入：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_web_bind")]
    pub bind: String,
    /// 鉴权 token；留空则启动时随机生成并打印日志
    #[serde(default)]
    pub token: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: default_web_bind(),
            token: String::new(),
        }
    }
}

fn default_web_bind() -> String {
    "127.0.0.1:8080".into()
}
```

- [ ] **Step 4: 在 ChannelsConfig 加 web 字段**

把 `ChannelsConfig` 结构改为：

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelsConfig {
    #[serde(default)]
    pub cli: CliChannelConfig,
    #[serde(default)]
    pub qq: QqConfig,
    #[serde(default)]
    pub web: WebConfig,
}
```

- [ ] **Step 5: 在 expand_paths 展开 web.token**

在 `expand_paths` 方法中，`self.channels.qq.app_secret = expand(...)?;` 之后加：

```rust
        self.channels.web.token = expand(&self.channels.web.token)?;
```

- [ ] **Step 6: 在 default_for_workspace 给 web 默认值**

`Config::default_for_workspace` 末尾构造 `Config { ... channels: ChannelsConfig::default(), ... }` 已经包含 web 默认值（enabled=false, bind=127.0.0.1:8080, token=""),无需改动。确认 `ChannelsConfig::default()` 能工作即可。

- [ ] **Step 7: 写失败测试**

在 `src/config.rs` 的 `mod tests` 末尾加：

```rust
    #[test]
    fn test_web_config_defaults() {
        let config = Config::default_for_workspace("~/.llaia");
        assert!(!config.channels.web.enabled);
        assert_eq!(config.channels.web.bind, "127.0.0.1:8080");
        assert_eq!(config.channels.web.token, "");
    }

    #[test]
    fn test_web_config_loaded_from_toml() {
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[provider.default.qwen]
model = "qwen2.5:7b"

[agent.main]
model = "default.qwen"
workspace = "~/.llaia"

[channels.web]
enabled = true
bind = "0.0.0.0:9000"
token = "secret-token"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        use std::io::Write;
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        assert!(config.channels.web.enabled);
        assert_eq!(config.channels.web.bind, "0.0.0.0:9000");
        assert_eq!(config.channels.web.token, "secret-token");
    }
```

- [ ] **Step 8: 运行测试验证通过**

Run: `cargo test --lib config::tests -- --nocapture`
Expected: PASS（包含两个新测试）

- [ ] **Step 9: 全量编译**

Run: `cargo build`
Expected: 编译通过（build.rs 生效，axum 等依赖下载）

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml build.rs src/config.rs
git commit -m "feat(web): add web config schema, build.rs git hash, axum deps"
```

---

## Task 2: WebEvent 枚举与序列化

**Files:**
- Create: `src/channels/web.rs`
- Modify: `src/channels/mod.rs`
- Test: `src/channels/web.rs` 内 `#[cfg(test)]`

- [ ] **Step 1: 在 src/channels/mod.rs 注册 web 模块**

把 `mod.rs` 改为：

```rust
pub mod cli;
pub mod qq;
pub mod web;

// 重新导出，方便外部使用
pub use cli::CliChannel;
pub use qq::QqChannel;
pub use web::WebChannel;

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

/// 抽象一个用户接入通道（CLI / QQ / 未来邮箱、web 等）。
/// 每个实现负责自己的 I/O 循环（读用户输入、写回复），
/// 共享同一个 AgentRegistry（main + sub_agents，通过 Arc<Mutex> 串行化访问）。
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    /// 启动 channel，阻塞运行直到退出。
    async fn run(self: Arc<Self>, registry: Arc<crate::agent::AgentRegistry>) -> Result<()>;
}
```

- [ ] **Step 2: 创建 src/channels/web.rs 骨架 + WebEvent**

```rust
use crate::agent::MediaKind;
use serde::Serialize;

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
```

注意：`MediaKind` 需要实现 `Serialize`。检查 `src/agent/mod.rs` 中 `MediaKind` 定义，若未派生 `Serialize` 则在该定义上加 `#[derive(Serialize)]`（用于 WebEvent 序列化）。先尝试编译，若报错再改。

- [ ] **Step 3: 写失败测试**

在 `src/channels/web.rs` 末尾加：

```rust
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
}
```

- [ ] **Step 4: 运行测试验证通过**

Run: `cargo test --lib channels::web::tests`
Expected: PASS

若 `MediaKind` 未实现 `Serialize`，编译报错则去 `src/agent/mod.rs` 给 `MediaKind` 加 `Serialize` 派生（同时确认不影响其他反序列化）。

- [ ] **Step 5: Commit**

```bash
git add src/channels/mod.rs src/channels/web.rs src/agent/mod.rs
git commit -m "feat(web): add WebEvent enum and serialization"
```

---

## Task 3: WebSink 实现

**Files:**
- Modify: `src/channels/web.rs`
- Test: `src/channels/web.rs` 内 `#[cfg(test)]`

- [ ] **Step 1: 在 web.rs 加 WebSink**

在 WebEvent 定义之后加：

```rust
use crate::agent::sink::OutputSink;
use async_trait::async_trait;
use tokio::sync::mpsc;

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
```

注意 `id` 字段：`OutputSink::on_tool_start` 只传 `name`，无 id。这里 `id` 留空字符串。后续若 TurnEvent 的 ToolStart 带 id 想透传，需要扩展 OutputSink trait（本阶段不做，保持与 CLI/QQ 一致）。

- [ ] **Step 2: 写失败测试**

在 `web.rs` 的 `mod tests` 加：

```rust
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
```

- [ ] **Step 3: 运行测试验证通过**

Run: `cargo test --lib channels::web::tests`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/channels/web.rs
git commit -m "feat(web): implement WebSink with mpsc event forwarding"
```

---

## Task 4: 鉴权中间件与 token 生成

**Files:**
- Modify: `src/channels/web.rs`
- Test: `src/channels/web.rs` 内 `#[cfg(test)]`

- [ ] **Step 1: 在 web.rs 加 AppState 与鉴权工具**

在 WebSink 之后加：

```rust
use crate::agent::AgentRegistry;
use crate::config::{Config, WebConfig};
use axum::extract::State;
use rand::Rng;
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
```

- [ ] **Step 2: 写测试**

在 `mod tests` 加：

```rust
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
```

- [ ] **Step 3: 运行测试**

Run: `cargo test --lib channels::web::tests`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/channels/web.rs
git commit -m "feat(web): add auth helpers and token generation"
```

---

## Task 5: 路径安全工具函数

**Files:**
- Modify: `src/channels/web.rs`
- Test: `src/channels/web.rs` 内 `#[cfg(test)]`

- [ ] **Step 1: 在 web.rs 加路径校验函数**

在鉴权工具之后加：

```rust
use std::path::{Component, Path, PathBuf};

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

/// 相对 workspace 的路径用于 /file 路由
pub fn resolve_workspace_path(workspace: &Path, relative: &str) -> Result<PathBuf, String> {
    resolve_within(workspace, relative)
}
```

- [ ] **Step 2: 写测试**

在 `mod tests` 加：

```rust
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
        assert!(r.starts_with(base));
    }

    #[test]
    fn test_resolve_within_rejects_windows_drive() {
        let tmp = tempfile::tempdir().unwrap();
        let r = resolve_within(tmp.path(), "C:\\Windows\\system32");
        assert!(r.is_err());
    }
```

- [ ] **Step 3: 运行测试**

Run: `cargo test --lib channels::web::tests`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/channels/web.rs
git commit -m "feat(web): add path safety helpers for uploads and files"
```

---

## Task 6: 静态资源路由（rust-embed）

**Files:**
- Modify: `src/channels/web.rs`
- Create: `src/channels/web/static/index.html`（占位，Task 13 完善）

- [ ] **Step 1: 创建占位 index.html**

创建 `src/channels/web/static/index.html`：

```html
<!DOCTYPE html>
<html lang="zh">
<head><meta charset="utf-8"><title>Llaia</title></head>
<body>placeholder</body>
</html>
```

- [ ] **Step 2: 在 web.rs 加 rust-embed 与静态资源 handler**

在路径工具之后加：

```rust
use axum::body::Body;
use axum::extract::Query;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;
use serde::Deserialize;

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
```

- [ ] **Step 3: 编译验证**

Run: `cargo build`
Expected: 编译通过（rust-embed 嵌入 static 目录）

- [ ] **Step 4: Commit**

```bash
git add src/channels/web.rs src/channels/web/static/index.html
git commit -m "feat(web): add static asset serving via rust-embed"
```

---

## Task 7: POST /upload 与 GET /file 路由

**Files:**
- Modify: `src/channels/web.rs`
- Test: `tests/web_api.rs`

- [ ] **Step 1: 在 web.rs 加 upload handler**

在静态资源之后加：

```rust
use axum::extract::Multipart;

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

#[derive(Deserialize)]
pub struct TokenQuery {
    pub token: Option<String>,
}

#[derive(Deserialize)]
pub struct FilePathQuery {
    pub path: String,
}

/// GET /file?path=<rel>：返回 workspace 内文件流
pub async fn serve_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<FilePathQuery>,
) -> Response {
    // file 路由 token 可能在 header 或 query（图片 src 无法带 header，允许 query ?path=&token=）
    // 但为安全，file 路由单独接受 query token
    let provided = extract_token(&headers, "", None)
        .or_else(|| extract_token(&headers, "", Some(&q.path))); // 简化：实际从前端用 fetch 带 header
    // 简化：直接用 Authorization header；前端 <img src> 需特殊处理（见 Task 14）
    let _ = q;
    unauthorized()
}
```

注意：`/file` 的鉴权 + `<img>` 标签无法带 header 的问题在 Task 14 用 query token 解决。这里先留骨架，Task 9 完善实现。

- [ ] **Step 2: 加 authorize / unauthorized 辅助函数**

在路径工具之后加：

```rust
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
```

- [ ] **Step 3: 完善 serve_file 实现**

把上面占位的 `serve_file` 替换为：

```rust
/// GET /file?path=<rel>&token=<token>：返回 workspace 内文件流
pub async fn serve_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<FilePathQueryWithToken>,
) -> Response {
    #[derive(Deserialize)]
    struct FilePathQueryWithToken {
        path: String,
        token: Option<String>,
    }
    let _ = q;
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
```

（注：axum Query 的类型必须实现 Deserialize；上面内联定义 struct 不合法。改为在函数外定义 `FilePathQueryWithToken`。）

修正：在 `serve_file` 之上定义：

```rust
#[derive(Deserialize)]
pub struct FilePathQueryWithToken {
    pub path: String,
    pub token: Option<String>,
}
```

并让 `serve_file` 签名用 `Query<FilePathQueryWithToken>`，删除内联 struct。

- [ ] **Step 4: 写集成测试**

创建 `tests/web_api.rs`：

```rust
use llaia::channels::web::{resolve_within, check_token, generate_token, extract_token};
use axum::http::HeaderMap;

#[test]
fn test_resolve_within_rejects_traversal_integration() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(resolve_within(tmp.path(), "../../etc/passwd").is_err());
}

#[test]
fn test_check_token_integration() {
    let t = generate_token();
    assert!(check_token(&t, &t));
    assert!(!check_token("wrong", &t));
}

#[test]
fn test_extract_token_priority() {
    let mut h = HeaderMap::new();
    h.insert("authorization", "Bearer from-header".parse().unwrap());
    assert_eq!(extract_token(&h, "", None).as_deref(), Some("from-header"));
}
```

- [ ] **Step 5: 运行测试**

Run: `cargo test --test web_api`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/channels/web.rs tests/web_api.rs
git commit -m "feat(web): add /upload and /file routes with path safety"
```

---

## Task 8: 配置 API（GET/PUT 结构化 + raw + validate）

**Files:**
- Modify: `src/channels/web.rs`
- Test: `tests/web_api.rs`

- [ ] **Step 1: 在 web.rs 加 config API handler**

在 serve_file 之后加：

```rust
use crate::config::Config as Cfg;

/// 敏感字段掩码
const MASK: &str = "••••";

/// 标记哪些字段是敏感的（返回时掩码，保存时若仍为掩码则保留原值）
fn mask_sensitive(mut config: Cfg) -> Cfg {
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
fn merge_masked(old: &Cfg, new: &Cfg) -> Cfg {
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
pub async fn get_config(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<TokenQuery>) -> Response {
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
    axum::Json(new_config): axum::Json<Cfg>,
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
pub async fn get_config_raw(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<TokenQuery>) -> Response {
    if !authorize(&state, &headers, &q) {
        return unauthorized();
    }
    // 读盘上原始文本（含未掩码密钥）—— 已通过鉴权
    match std::fs::read_to_string(&state.config_path) {
        Ok(s) => (StatusCode::OK, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], s).into_response(),
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
    match toml::from_str::<Cfg>(&body.toml) {
        Ok(_) => axum::Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => {
            let msg = e.to_string();
            let line = e.span().and_then(|s| Some(s.start)).map(|n| n.to_string()).unwrap_or_default();
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
    match toml::from_str::<Cfg>(&body.toml) {
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
```

- [ ] **Step 2: 写失败测试（掩码逻辑）**

在 `src/channels/web.rs` 的 `mod tests` 加：

```rust
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
```

- [ ] **Step 3: 运行测试**

Run: `cargo test --lib channels::web::tests`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/channels/web.rs
git commit -m "feat(web): add config API with secret masking and merge"
```

---

## Task 9: 状态 API（GET /api/status）

**Files:**
- Modify: `src/channels/web.rs`
- Test: `tests/web_api.rs`

- [ ] **Step 1: 在 web.rs 加 status handler**

在 config API 之后加：

```rust
use serde::Serialize;

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
pub async fn get_status(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<TokenQuery>) -> Response {
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
```

- [ ] **Step 2: 编译验证**

Run: `cargo build`
Expected: 编译通过

- [ ] **Step 3: Commit**

```bash
git add src/channels/web.rs
git commit -m "feat(web): add /api/status endpoint"
```

---

## Task 10: WS handler 与 WebChannel::run

**Files:**
- Modify: `src/channels/web.rs`
- Modify: `src/commands/mod.rs`

- [ ] **Step 1: 在 web.rs 加 WS handler**

在 status handler 之后加：

```rust
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::Notify;
use crate::agent::sink::run_turn;
use crate::provider::{ChatMessage, ContentPart, ImageUrlContent};
use crate::image_utils;

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
    let _ = ws_sink.send(Message::Text(serde_json::to_string(&WebEvent::AuthOk).unwrap())).await;

    // 写 task：rx → ws_sink
    let mut write_task = tokio::spawn(async move {
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
                        match chat.map(|c| c.kind.as_str()) {
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

fn build_user_message(text: &str, images: Option<&[String]>, workspace: &std::path::Path) -> ChatMessage {
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
```

- [ ] **Step 2: 在 web.rs 加 WebChannel + 路由构建 + run**

在 WS handler 之后加：

```rust
pub struct WebChannel {
    pub config: WebConfig,
    pub registry: Arc<AgentRegistry>,
    pub config_full: Arc<RwLock<Config>>,
    pub config_path: std::path::PathBuf,
    pub workspace: std::path::PathBuf,
}

impl WebChannel {
    pub fn new(
        web_config: WebConfig,
        registry: Arc<AgentRegistry>,
        config_full: Arc<RwLock<Config>>,
        config_path: std::path::PathBuf,
        workspace: std::path::PathBuf,
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
    async fn run(self: Arc<Self>, _registry: Arc<AgentRegistry>) -> Result<()> {
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
```

- [ ] **Step 3: 修改 src/commands/mod.rs 的 serve_cmd**

把 `serve_cmd` 中 QQ 分支之后、`if tasks.is_empty()` 之前加 web 分支。需要先从 registry 取 workspace。

把 `serve_cmd` 改为（在 QQ 分支后插入）：

```rust
    if config.channels.web.enabled {
        let workspace = {
            let a = registry.main.lock().await;
            a.workspace.clone()
        };
        let config_path = config_dir.join("config.toml");
        let web = std::sync::Arc::new(crate::channels::web::WebChannel::new(
            config.channels.web.clone(),
            registry.clone(),
            std::sync::Arc::new(tokio::sync::RwLock::new(config.clone())),
            config_path,
            workspace,
        ));
        let registry_clone = registry.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = crate::channels::Channel::run(web, registry_clone).await {
                tracing::error!(error = %e, "WebChannel exited with error");
            }
        }));
        tracing::info!("WebChannel starting on {}", config.channels.web.bind);
    }
```

注意：`config` 在 QQ 分支已 move 进 closure？检查 —— QQ 分支用 `config.channels.qq.clone()`，未 move 整个 config。web 分支用 `config.channels.web.clone()` 和 `config.clone()`，需要 `config` 仍可用。把 `config` 的 clone 提前。实际 `config` 是 `Config` 值，`config.channels.web.clone()` 借用，`config.clone()` 也行。但 `config_dir` 是 `&Path`，`config_dir.join(...)` 借用。OK，无需 move。

但有个问题：`config` 在函数开头 `let config = load_config_or_init(config_dir)?;`，后面多个分支借用。QQ 分支 `config.channels.qq.clone()` 只借用，OK。web 分支同理。`tasks.is_empty()` 检查在末尾。所以 `config` 全程可用。

- [ ] **Step 4: 编译验证**

Run: `cargo build`
Expected: 编译通过。若有未使用 import 警告，按需清理。

- [ ] **Step 5: 冒烟启动测试**

先在 `config.toml` 加 `[channels.web] enabled = true bind = "127.0.0.1:8080" token = "test"`，运行：

Run: `cargo run -- serve`
Expected: 日志打印 "WebChannel listening on 127.0.0.1:8080"，浏览器访问 `http://127.0.0.1:8080/?token=test` 看到 placeholder（或 401 无 token）。

- [ ] **Step 6: Commit**

```bash
git add src/channels/web.rs src/commands/mod.rs
git commit -m "feat(web): add WS handler, WebChannel::run, serve_cmd integration"
```

---

## Task 11: 前端 vendor 库下载

**Files:**
- Create: `src/channels/web/static/vendor/alpine.min.js`
- Create: `src/channels/web/static/vendor/marked.min.js`
- Create: `src/channels/web/static/vendor/highlight.min.js`
- Create: `src/channels/web/static/vendor/codemirror/*`

- [ ] **Step 1: 下载 vendor 库**

用 PowerShell 下载（版本固定）：

```powershell
$dir = "src/channels/web/static/vendor"
New-Item -ItemType Directory -Force -Path $dir, "$dir/codemirror"
Invoke-WebRequest "https://cdn.jsdelivr.net/npm/alpinejs@3.14.1/dist/cdn.min.js" -OutFile "$dir/alpine.min.js"
Invoke-WebRequest "https://cdn.jsdelivr.net/npm/marked@12.0.2/marked.min.js" -OutFile "$dir/marked.min.js"
Invoke-WebRequest "https://cdn.jsdelivr.net/npm/highlight.js@11.9.0/lib/highlight.min.js" -OutFile "$dir/highlight.min.js"
# CodeMirror 6 UMD bundle（社区打包）
Invoke-WebRequest "https://cdn.jsdelivr.net/npm/codemirror@5.65.16/lib/codemirror.js" -OutFile "$dir/codemirror/codemirror.js"
Invoke-WebRequest "https://cdn.jsdelivr.net/npm/codemirror@5.65.16/lib/codemirror.css" -OutFile "$dir/codemirror/codemirror.css"
Invoke-WebRequest "https://cdn.jsdelivr.net/npm/codemirror@5.65.16/mode/toml/toml.js" -OutFile "$dir/codemirror/toml.js"
Invoke-WebRequest "https://cdn.jsdelivr.net/npm/codemirror@5.65.16/theme/material-darker.css" -OutFile "$dir/codemirror/material-darker.css"
```

注：spec 写的是 CodeMirror 6，但 6 没有 UMD bundle，需 npm 打包。为保持"零 node 构建"，这里用 CodeMirror 5.65（提供 UMD + toml mode），体验等价（TOML 高亮、行号、错误提示）。若坚持 CM6 需引入 npm，与方案冲突。

- [ ] **Step 2: 验证文件存在**

Run: `dir src\channels\web\static\vendor`
Expected: 列出 alpine.min.js / marked.min.js / highlight.min.js / codemirror/

- [ ] **Step 3: Commit**

```bash
git add src/channels/web/static/vendor
git commit -m "chore(web): vendor frontend libs (alpine, marked, highlight, codemirror)"
```

---

## Task 12: 前端 index.html + app.js（骨架 + tab + token + WS）

**Files:**
- Modify: `src/channels/web/static/index.html`
- Create: `src/channels/web/static/app.js`
- Create: `src/channels/web/static/chat.js`
- Create: `src/channels/web/static/config.js`
- Create: `src/channels/web/static/about.js`

- [ ] **Step 1: 写 index.html**

```html
<!DOCTYPE html>
<html lang="zh">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Llaia Web</title>
  <link rel="stylesheet" href="/static/vendor/codemirror/codemirror.css">
  <link rel="stylesheet" href="/static/vendor/codemirror/material-darker.css">
  <style>
    body { font-family: system-ui, sans-serif; margin: 0; background: #1e1e1e; color: #ddd; }
    #app { display: flex; flex-direction: column; height: 100vh; }
    header { background: #252526; padding: 8px 16px; display: flex; align-items: center; gap: 16px; border-bottom: 1px solid #333; }
    header h1 { font-size: 16px; margin: 0; }
    .tabs { display: flex; gap: 4px; }
    .tab { padding: 6px 14px; cursor: pointer; border-radius: 4px; background: #333; }
    .tab.active { background: #0e639c; }
    .spacer { flex: 1; }
    .token-area { font-size: 12px; color: #888; }
    .token-area input { width: 200px; background: #333; border: 1px solid #555; color: #ddd; padding: 2px 6px; }
    main { flex: 1; overflow: hidden; }
    .pane { display: none; height: 100%; }
    .pane.active { display: flex; flex-direction: column; }
    button { background: #0e639c; color: #fff; border: none; padding: 6px 12px; border-radius: 4px; cursor: pointer; }
    button:disabled { background: #555; cursor: not-allowed; }
    input, textarea { background: #2d2d2d; color: #ddd; border: 1px solid #555; padding: 4px 8px; border-radius: 4px; }
    .msg { margin: 8px 0; padding: 8px; border-radius: 4px; }
    .msg.user { background: #2a2a3a; }
    .msg.assistant { background: #252526; }
    .msg.tool { background: #1a2a1a; font-family: monospace; font-size: 12px; }
    .msg pre { background: #1a1a1a; padding: 8px; border-radius: 4px; overflow-x: auto; }
    .msg img { max-width: 400px; border-radius: 4px; }
    /* chat layout */
    #chat-pane .messages { flex: 1; overflow-y: auto; padding: 16px; }
    #chat-pane .composer { display: flex; gap: 8px; padding: 8px; border-top: 1px solid #333; align-items: center; }
    #chat-pane .composer input { flex: 1; }
    /* config layout */
    #config-pane { flex-direction: row; }
    #config-pane .sidebar { width: 200px; background: #252526; padding: 8px; overflow-y: auto; }
    #config-pane .sidebar div { padding: 6px 8px; cursor: pointer; border-radius: 4px; }
    #config-pane .sidebar div.active { background: #0e639c; }
    #config-pane .content { flex: 1; overflow-y: auto; padding: 16px; }
    #config-pane .form-row { margin: 8px 0; }
    #config-pane .form-row label { display: inline-block; width: 180px; }
    #config-pane .provider-card, #config-pane .agent-card { background: #2d2d2d; padding: 12px; border-radius: 6px; margin: 8px 0; }
    #config-pane .placeholder { color: #888; font-style: italic; }
    /* about */
    #about-pane { padding: 24px; overflow-y: auto; }
    #about-pane dl dt { font-weight: bold; color: #0e639c; margin-top: 8px; }
    #about-pane dl dd { margin-left: 16px; }
  </style>
</head>
<body>
<div id="app" x-data="llaiaApp()" x-init="init()">
  <header>
    <h1>Llaia</h1>
    <div class="tabs">
      <div class="tab" :class="{active: tab==='chat'}" @click="tab='chat'">Chat</div>
      <div class="tab" :class="{active: tab==='config'}" @click="switchConfig()">配置</div>
      <div class="tab" :class="{active: tab==='about'}" @click="switchAbout()">关于</div>
    </div>
    <div class="spacer"></div>
    <div class="token-area">
      Token: <input type="password" x-model="token" @keydown.enter="saveToken()" placeholder="输入 token">
      <button @click="saveToken()">保存</button>
    </div>
  </header>
  <main>
    <div id="chat-pane" class="pane" :class="{active: tab==='chat'}">
      <div class="messages" x-ref="messages">
        <template x-for="(m, i) in messages" :key="i">
          <div class="msg" :class="m.role">
            <template x-if="m.role==='user'"><div><b>用户:</b> <span x-text="m.text"></span></div></template>
            <template x-if="m.role==='assistant'"><div><b>助手:</b> <div x-html="renderMd(m.text)"></div></div></template>
            <template x-if="m.role==='tool'"><div><b>工具:</b> <span x-text="m.text"></span></div></template>
            <template x-if="m.role==='media'">
              <div><b>媒体:</b> <img :src="'/file?path='+encodeURIComponent(m.path)+'&token='+encodeURIComponent(token)"></div>
            </template>
          </div>
        </template>
      </div>
      <div class="composer">
        <input type="file" accept="image/*" @change="onUpload($event)" multiple disabled>
        <input type="text" x-model="inputText" @keydown.enter="send()" :disabled="busy" placeholder="输入消息...">
        <button @click="send()" :disabled="busy">发送</button>
        <button @click="stop()" :disabled="!busy">停止</button>
        <span x-text="uploaded.map(u=>u.path).join(', ')"></span>
      </div>
    </div>

    <div id="config-pane" class="pane" :class="{active: tab==='config'}">
      <div class="sidebar">
        <div :class="{active: configSection==='runtime'}" @click="configSection='runtime'">运行时参数</div>
        <div :class="{active: configSection==='log'}" @click="configSection='log'">日志</div>
        <div :class="{active: configSection==='provider'}" @click="configSection='provider'">Provider</div>
        <div :class="{active: configSection==='agent'}" @click="configSection='agent'">Agent</div>
        <div :class="{active: configSection==='channels'}" @click="configSection='channels'">Channels</div>
        <div :class="{active: configSection==='tools'}" @click="configSection='tools'">Tools</div>
        <div :class="{active: configSection==='mcp'}" @click="configSection='mcp'">MCP (开发中)</div>
        <div :class="{active: configSection==='skills'}" @click="configSection='skills'">Skills (开发中)</div>
        <div :class="{active: configSection==='raw'}" @click="configSection='raw'">原始 TOML</div>
      </div>
      <div class="content">
        <template x-if="configSection==='runtime'">
          <div>
            <div class="form-row"><label>context_threshold</label><input type="number" step="0.1" min="0" max="1" x-model="cfg.runtime.context_threshold"></div>
            <div class="form-row"><label>max_iterations</label><input type="number" min="1" x-model="cfg.runtime.max_iterations"></div>
            <button @click="saveConfig()">保存</button>
          </div>
        </template>
        <template x-if="configSection==='log'">
          <div>
            <div class="form-row"><label>level</label>
              <select x-model="cfg.log.level"><option>debug</option><option>info</option><option>warn</option><option>error</option></select>
            </div>
            <div class="form-row"><label>dir</label><input type="text" x-model="cfg.log.dir" style="width:400px"></div>
            <button @click="saveConfig()">保存</button>
          </div>
        </template>
        <template x-if="configSection==='provider'">
          <div>
            <template x-for="(p, pid) in cfg.provider" :key="pid">
              <div class="provider-card">
                <h3 x-text="pid"></h3>
                <div class="form-row"><label>type</label><input x-model="p.type"></div>
                <div class="form-row"><label>base_url</label><input x-model="p.base_url" style="width:400px"></div>
                <div class="form-row"><label>api_key</label><input type="password" x-model="p.api_key"></div>
                <h4>Models</h4>
                <template x-for="(m, alias) in p.model" :key="alias">
                  <div class="form-row">
                    <label x-text="alias"></label>
                    <input x-model="m.model" style="width:200px">
                    <label><input type="checkbox" x-model="m.native_tool_calling"> native_tool_calling</label>
                    <input type="number" placeholder="context_size" x-model="m.context_size">
                  </div>
                </template>
              </div>
            </template>
            <button @click="saveConfig()">保存</button>
          </div>
        </template>
        <template x-if="configSection==='agent'">
          <div>
            <template x-for="(a, alias) in cfg.agent" :key="alias">
              <div class="agent-card">
                <h3 x-text="alias"></h3>
                <div class="form-row"><label>model</label><input x-model="a.model"></div>
                <div class="form-row"><label>workspace</label><input x-model="a.workspace" style="width:400px"></div>
                <div class="form-row"><label>soul</label><input x-model="a.soul"></div>
                <div class="form-row"><label>user</label><input x-model="a.user"></div>
                <div class="form-row"><label>memory</label><input x-model="a.memory"></div>
                <div class="form-row"><label>delegate_timeout</label><input type="number" x-model="a.delegate_timeout"></div>
              </div>
            </template>
            <button @click="saveConfig()">保存</button>
          </div>
        </template>
        <template x-if="configSection==='channels'">
          <div>
            <h3>CLI</h3>
            <div class="form-row"><label><input type="checkbox" x-model="cfg.channels.cli.enabled"> enabled</label></div>
            <h3>QQ</h3>
            <div class="form-row"><label><input type="checkbox" x-model="cfg.channels.qq.enabled"> enabled</label></div>
            <div class="form-row"><label>app_id</label><input x-model="cfg.channels.qq.app_id"></div>
            <div class="form-row"><label>app_secret</label><input type="password" x-model="cfg.channels.qq.app_secret"></div>
            <div class="form-row"><label>confirm_mode</label><input x-model="cfg.channels.qq.confirm_mode"></div>
            <h3>Web</h3>
            <div class="form-row"><label><input type="checkbox" x-model="cfg.channels.web.enabled"> enabled</label></div>
            <div class="form-row"><label>bind</label><input x-model="cfg.channels.web.bind"></div>
            <div class="form-row"><label>token</label><input type="password" x-model="cfg.channels.web.token"></div>
            <button @click="saveConfig()">保存</button>
          </div>
        </template>
        <template x-if="configSection==='tools'">
          <div>
            <h3>terminal</h3>
            <div class="form-row"><label>confirm</label><input x-model="cfg.tools.terminal.confirm"></div>
            <div class="form-row"><label>whitelist (逗号分隔)</label><input :value="cfg.tools.terminal.whitelist.join(',')" @input="cfg.tools.terminal.whitelist=$event.target.value.split(',')"></div>
            <h3>tavily</h3>
            <div class="form-row"><label>api_key</label><input type="password" x-model="cfg.tools.tavily.api_key"></div>
            <button @click="saveConfig()">保存</button>
          </div>
        </template>
        <template x-if="configSection==='mcp'"><div class="placeholder">MCP 配置开发中，敬请期待。</div></template>
        <template x-if="configSection==='skills'"><div class="placeholder">Skills 配置开发中，敬请期待。</div></template>
        <template x-if="configSection==='raw'">
          <div>
            <textarea x-ref="rawEditor" x-model="rawToml" style="width:100%;height:500px;font-family:monospace"></textarea>
            <button @click="validateRaw()">校验</button>
            <button @click="saveRaw()">保存</button>
            <span x-text="rawMsg"></span>
          </div>
        </template>
      </div>
    </div>

    <div id="about-pane" class="pane" :class="{active: tab==='about'}">
      <template x-if="status">
        <dl>
          <dt>Version</dt><dd x-text="status.version"></dd>
          <dt>Build</dt><dd x-text="status.build_hash"></dd>
          <dt>Workspace</dt><dd x-text="status.workspace"></dd>
          <dt>Config</dt><dd x-text="status.config_path"></dd>
          <dt>PID</dt><dd x-text="status.pid"></dd>
          <dt>Channels</dt>
          <dd>
            <template x-for="c in status.channels" :key="c.name">
              <div x-text="(c.enabled?'✓':'✗')+' '+c.name+(c.listening?' ('+c.listening+')':'')"></div>
            </template>
          </dd>
          <dt>DB Size</dt><dd x-text="formatBytes(status.db_size_bytes)"></dd>
          <dt>Log Dir</dt><dd x-text="status.log_dir"></dd>
          <dt>Uploads</dt><dd x-text="status.uploads_count+' files'"></dd>
        </dl>
      </template>
    </div>
  </main>
</div>
<script src="/static/vendor/marked.min.js"></script>
<script src="/static/vendor/highlight.min.js"></script>
<script src="/static/vendor/codemirror/codemirror.js"></script>
<script src="/static/vendor/codemirror/toml.js"></script>
<script src="/static/vendor/alpine.min.js" defer></script>
<script src="/static/app.js"></script>
</body>
</html>
```

- [ ] **Step 2: 写 app.js**

```javascript
function llaiaApp() {
  return {
    tab: 'chat',
    token: localStorage.getItem('llaia_token') || '',
    // chat
    messages: [],
    inputText: '',
    busy: false,
    uploaded: [],
    ws: null,
    // config
    cfg: { runtime:{}, log:{}, provider:{}, agent:{}, channels:{cli:{},qq:{},web:{}}, tools:{terminal:{whitelist:[]},tavily:{}} },
    configSection: 'runtime',
    rawToml: '',
    rawMsg: '',
    // about
    status: null,

    async init() {
      if (!this.token) { this.token = prompt('请输入 WebUI token:'); if (this.token) localStorage.setItem('llaia_token', this.token); }
      this.connectWs();
    },
    saveToken() { localStorage.setItem('llaia_token', this.token); this.connectWs(); },

    // ---- WS ----
    connectWs() {
      if (this.ws) this.ws.close();
      const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
      this.ws = new WebSocket(`${proto}//${location.host}/ws?token=${encodeURIComponent(this.token)}`);
      this.ws.onmessage = (e) => this.onWsMessage(JSON.parse(e.data));
      this.ws.onclose = () => { setTimeout(() => this.connectWs(), 3000); };
    },
    onWsMessage(ev) {
      switch (ev.type) {
        case 'auth_ok': break;
        case 'auth_failed': alert('token 错误'); break;
        case 'chunk':
          if (this.messages.length === 0 || this.messages[this.messages.length-1].role !== 'assistant') {
            this.messages.push({ role: 'assistant', text: ev.delta });
          } else {
            this.messages[this.messages.length-1].text += ev.delta;
          }
          this.scrollBottom();
          break;
        case 'tool_start':
          this.messages.push({ role: 'tool', text: `${ev.name}...` });
          break;
        case 'tool_result':
          this.messages.push({ role: 'tool', text: ev.output });
          break;
        case 'media':
          this.messages.push({ role: 'media', path: ev.path, kind: ev.kind });
          break;
        case 'done':
        case 'error':
        case 'interrupted':
          this.busy = false;
          if (ev.type === 'error') this.messages.push({ role: 'tool', text: `[error: ${ev.message}]` });
          if (ev.type === 'interrupted') this.messages.push({ role: 'tool', text: '[已中断]' });
          break;
        case 'busy': alert(ev.reason); break;
        case 'pong': break;
      }
    },
    send() {
      if (!this.inputText.trim() && this.uploaded.length === 0) return;
      this.busy = true;
      this.messages.push({ role: 'user', text: this.inputText });
      this.ws.send(JSON.stringify({ type: 'chat', text: this.inputText, images: this.uploaded.map(u=>u.path) }));
      this.inputText = '';
      this.uploaded = [];
      this.scrollBottom();
    },
    stop() { this.ws.send(JSON.stringify({ type: 'stop' })); },
    async onUpload(e) {
      for (const f of e.target.files) {
        const fd = new FormData();
        fd.append('file', f);
        const r = await fetch('/upload?token=' + encodeURIComponent(this.token), { method: 'POST', body: fd });
        if (r.ok) { const j = await r.json(); this.uploaded.push(j); }
        else { alert('上传失败: ' + await r.text()); }
      }
    },
    renderMd(text) { try { return marked.parse(text || ''); } catch { return text; } },
    scrollBottom() { this.$nextTick(() => { const el = this.$refs.messages; if (el) el.scrollTop = el.scrollHeight; }); },

    // ---- config ----
    async switchConfig() {
      this.tab = 'config';
      const r = await fetch('/api/config?token=' + encodeURIComponent(this.token));
      if (r.ok) { this.cfg = await r.json(); }
      const rr = await fetch('/api/config/raw?token=' + encodeURIComponent(this.token));
      if (rr.ok) { this.rawToml = await rr.text(); this.$nextTick(() => this.initEditor()); }
    },
    initEditor() {
      if (this._editor) { this._editor.setValue(this.rawToml); return; }
      if (this.$refs.rawEditor && window.CodeMirror) {
        this._editor = CodeMirror.fromTextArea(this.$refs.rawEditor, { mode: 'toml', theme: 'material-darker', lineNumbers: true });
        this._editor.setValue(this.rawToml);
      }
    },
    async saveConfig() {
      const r = await fetch('/api/config?token=' + encodeURIComponent(this.token), { method: 'PUT', headers: {'Content-Type':'application/json'}, body: JSON.stringify(this.cfg) });
      const j = await r.json();
      if (r.ok) { alert('已保存，重启 llaia 生效'); this.switchConfig(); } else { alert('保存失败: ' + (j.error||r.status)); }
    },
    async validateRaw() {
      const toml = this._editor ? this._editor.getValue() : this.rawToml;
      const r = await fetch('/api/config/validate?token=' + encodeURIComponent(this.token), { method: 'POST', headers: {'Content-Type':'application/json'}, body: JSON.stringify({ toml }) });
      const j = await r.json();
      this.rawMsg = j.ok ? '✓ 校验通过' : '✗ ' + j.error;
    },
    async saveRaw() {
      const toml = this._editor ? this._editor.getValue() : this.rawToml;
      const r = await fetch('/api/config/raw?token=' + encodeURIComponent(this.token), { method: 'PUT', headers: {'Content-Type':'application/json'}, body: JSON.stringify({ toml }) });
      const j = await r.json();
      if (r.ok) { alert('已保存，重启 llaia 生效'); this.switchConfig(); } else { alert('保存失败: ' + (j.error||r.status)); }
    },

    // ---- about ----
    async switchAbout() {
      this.tab = 'about';
      const r = await fetch('/api/status?token=' + encodeURIComponent(this.token));
      if (r.ok) { this.status = await r.json(); }
    },
    formatBytes(n) { if (n < 1024) return n + ' B'; if (n < 1048576) return (n/1024).toFixed(1)+' KB'; return (n/1048576).toFixed(1)+' MB'; },
  };
}
```

注意：app.js 已包含 chat/config/about 全部逻辑，无需单独 chat.js/config.js/about.js。删除 spec 中提到的这些文件以简化。

- [ ] **Step 3: 验证前端加载**

Run: `cargo run -- serve`，浏览器访问 `http://127.0.0.1:8080/?token=test`
Expected: 看到 Chat tab，token 输入框，输入消息可发送。

- [ ] **Step 4: Commit**

```bash
git add src/channels/web/static/index.html src/channels/web/static/app.js
git commit -m "feat(web): add frontend SPA (alpine) with chat/config/about tabs"
```

---

## Task 13: 集成测试与最终冒烟

**Files:**
- Modify: `tests/web_api.rs`

- [ ] **Step 1: 完善 web_api.rs 集成测试**

把 `tests/web_api.rs` 改为：

```rust
use llaia::channels::web::{resolve_within, check_token, generate_token, extract_token, mask_sensitive, merge_masked};
use llaia::config::Config;
use axum::http::HeaderMap;

#[test]
fn test_resolve_within_rejects_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(resolve_within(tmp.path(), "../../etc/passwd").is_err());
}

#[test]
fn test_resolve_within_accepts_inside() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("uploads")).unwrap();
    std::fs::write(tmp.path().join("uploads/a.png"), b"x").unwrap();
    assert!(resolve_within(tmp.path(), "uploads/a.png").is_ok());
}

#[test]
fn test_check_token() {
    let t = generate_token();
    assert!(check_token(&t, &t));
    assert!(!check_token("wrong", &t));
}

#[test]
fn test_extract_token_bearer() {
    let mut h = HeaderMap::new();
    h.insert("authorization", "Bearer abc".parse().unwrap());
    assert_eq!(extract_token(&h, "", None).as_deref(), Some("abc"));
}

#[test]
fn test_mask_sensitive_redacts() {
    let mut c = Config::default_for_workspace("/tmp/x");
    c.provider.get_mut("default").unwrap().api_key = "sk-x".into();
    let m = mask_sensitive(c);
    assert_eq!(m.provider.get("default").unwrap().api_key, "••••");
}

#[test]
fn test_merge_masked_preserves_secret() {
    let mut old = Config::default_for_workspace("/tmp/x");
    old.provider.get_mut("default").unwrap().api_key = "sk-orig".into();
    let mut new = old.clone();
    new.provider.get_mut("default").unwrap().api_key = "••••".into();
    let merged = merge_masked(&old, &new);
    assert_eq!(merged.provider.get("default").unwrap().api_key, "sk-orig");
}

#[test]
fn test_web_config_round_trip() {
    let mut c = Config::default_for_workspace("/tmp/x");
    c.channels.web.enabled = true;
    c.channels.web.bind = "0.0.0.0:9999".into();
    let s = toml::to_string(&c).unwrap();
    let parsed: Config = toml::from_str(&s).unwrap();
    assert!(parsed.channels.web.enabled);
    assert_eq!(parsed.channels.web.bind, "0.0.0.0:9999");
}
```

- [ ] **Step 2: 运行全量测试**

Run: `cargo test`
Expected: 全部通过（含原有 111 个 + 新增）

- [ ] **Step 3: 冒烟测试 checklist**

启动 `cargo run -- serve`（config.toml 里 web.enabled=true, token=test），浏览器验证：

- [ ] 访问 `http://127.0.0.1:8080/`（无 token）→ 401 或跳转
- [ ] 访问 `http://127.0.0.1:8080/?token=test` → 看到 UI
- [ ] Chat tab：发消息 "你好" → 收到 chunk 流式回复 + done
- [ ] 上传图片 → 发送 → agent 看到图片
- [ ] agent 调 send_image → 浏览器显示图片
- [ ] 点"停止" → 中断
- [ ] 配置 tab：能看到结构化配置 + TOML 编辑器
- [ ] 修改 runtime.context_threshold → 保存 → 提示重启
- [ ] TOML 编辑器改文本 → 校验 → 保存
- [ ] 关于 tab：显示 version / pid / channels
- [ ] 关闭 tab 再开 → WS 重连

- [ ] **Step 4: Commit**

```bash
git add tests/web_api.rs
git commit -m "test(web): add integration tests for path safety, auth, config masking"
```

---

## Self-Review

**Spec coverage:**
- §2 架构：Task 1-10 实现后端，Task 11-12 前端 ✓
- §3 WS 协议：Task 2 (WebEvent) + Task 10 (WS handler) ✓
- §4 鉴权配置：Task 1 (WebConfig) + Task 4 (auth) ✓
- §5 多媒体：Task 7 (upload/file) + Task 12 (前端上传/图片显示) ✓
- §6 中止错误生命周期：Task 3 (sink) + Task 10 (ws handler 中止) ✓
- §7 前端结构：Task 12 ✓
- §8 配置可视化：Task 8 (config API) + Task 12 (前端配置表单) ✓
- §9 关于页面：Task 9 (status API) + Task 12 (关于 tab) ✓
- §11 测试：Task 13 + 各 task 内嵌测试 ✓

**Placeholder scan:** 无 TBD/TODO，所有代码块完整。

**Type consistency:** `WebSink::new(tx, end_tx)`、`WebEvent` 变体、`AppState` 字段、`resolve_within` 签名在各 task 间一致。

**已知简化（非 placeholder）：**
- on_tool_start 的 `id` 留空（OutputSink trait 不传 id，与 CLI/QQ 一致）
- CodeMirror 用 5.x UMD 而非 6（零 node 构建约束）
- app.js 合并了 chat/config/about 逻辑，未拆分 chat.js 等（spec 提及但实现合并更简，YAGNI）
