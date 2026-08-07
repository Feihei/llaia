//! MCP transport 层：stdio / streamable HTTP / SSE 三种实现（见 ADR-0014）。
//!
//! 调用模型：每个 server 串行请求（上层 McpServer 持锁），transport 内部
//! 按 JSON-RPC id 匹配响应并跳过 server 主动发的通知。SSE 因读写分离
//! 需要后台 reader 任务 + pending map。

use crate::mcp::protocol::{JsonRpcRequest, JsonRpcResponse, METHOD_NOT_FOUND};
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;

/// transport 层错误分类。client 据此决定是否重连：
/// 仅 `Closed` / `StaleSession` 触发 bounded reconnect。
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport closed: {0}")]
    Closed(String),
    #[error("stale session: {0}")]
    StaleSession(String),
    #[error("transport timeout: {0}")]
    Timeout(String),
    #[error("{0}")]
    Other(String),
}

#[async_trait]
pub trait McpTransport: Send + Sync {
    /// 发送请求并等待匹配 id 的响应。调用方保证同一 transport 上串行调用。
    async fn request(
        &self,
        req: &JsonRpcRequest,
        timeout: Duration,
    ) -> Result<JsonRpcResponse, TransportError>;

    /// 发送 notification（无响应）
    async fn notify(&self, req: &JsonRpcRequest) -> Result<(), TransportError>;

    /// 重置连接（重连用）：stdio 重新 spawn 子进程 / HTTP 清 session / SSE 重建长连接
    async fn reset(&self) -> Result<(), TransportError>;
}

// ───────────────────────── stdio ─────────────────────────

struct StdioInner {
    stdin: tokio::process::ChildStdin,
    stdout: tokio::io::BufReader<tokio::process::ChildStdout>,
    /// 持有 child 以便 reset 时 kill；drop transport 时 kill_on_drop 兜底
    child: tokio::process::Child,
}

/// stdio transport：启动子进程，stdin/stdout 行分隔 JSON-RPC
pub struct StdioTransport {
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    inner: tokio::sync::Mutex<Option<StdioInner>>,
}

impl StdioTransport {
    /// spawn 子进程并建立通道
    pub async fn connect(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self, TransportError> {
        let inner = spawn_child(command, args, env).await?;
        Ok(Self {
            command: command.to_string(),
            args: args.to_vec(),
            env: env.clone(),
            inner: tokio::sync::Mutex::new(Some(inner)),
        })
    }
}

async fn spawn_child(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<StdioInner, TransportError> {
    use tokio::process::Command;
    let mut child = Command::new(command)
        .args(args)
        .envs(env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| TransportError::Closed(format!("spawn '{}': {}", command, e)))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| TransportError::Closed("child stdin unavailable".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| TransportError::Closed("child stdout unavailable".into()))?;
    Ok(StdioInner {
        stdin,
        stdout: tokio::io::BufReader::new(stdout),
        child,
    })
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn request(
        &self,
        req: &JsonRpcRequest,
        timeout: Duration,
    ) -> Result<JsonRpcResponse, TransportError> {
        let mut guard = self.inner.lock().await;
        let inner = guard
            .as_mut()
            .ok_or_else(|| TransportError::Closed("stdio not connected".into()))?;
        let want_id = req.id.clone().unwrap_or(json!(null));

        use tokio::io::AsyncWriteExt;
        let mut line = serde_json::to_string(req)
            .map_err(|e| TransportError::Other(format!("serialize request: {}", e)))?;
        line.push('\n');
        inner
            .stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| TransportError::Closed(format!("write stdin: {}", e)))?;
        inner
            .stdin
            .flush()
            .await
            .map_err(|e| TransportError::Closed(format!("flush stdin: {}", e)))?;

        // 读行直到匹配 id 的响应；跳过 notification，拒绝 server→client 请求
        let read_fut = async {
            use tokio::io::AsyncBufReadExt;
            let mut buf = String::new();
            loop {
                buf.clear();
                let n = inner
                    .stdout
                    .read_line(&mut buf)
                    .await
                    .map_err(|e| TransportError::Closed(format!("read stdout: {}", e)))?;
                if n == 0 {
                    return Err(TransportError::Closed("stdout EOF".into()));
                }
                let trimmed = buf.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let v: serde_json::Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => continue, // 非 JSON 噪音（某些 server 往 stdout 打日志）
                };
                if v.get("method").is_some() {
                    if v.get("id").is_some() {
                        // server→client 请求（sampling/roots 等）：统一拒绝
                        let err_resp = json!({
                            "jsonrpc": "2.0",
                            "id": v["id"],
                            "error": { "code": METHOD_NOT_FOUND, "message": "not supported" }
                        });
                        let mut el = serde_json::to_string(&err_resp).unwrap_or_default();
                        el.push('\n');
                        let _ = inner.stdin.write_all(el.as_bytes()).await;
                        let _ = inner.stdin.flush().await;
                    }
                    continue; // notification 直接跳过
                }
                let resp: JsonRpcResponse = match serde_json::from_value(v) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                if resp
                    .id
                    .as_ref()
                    .map(|i| i == &want_id)
                    .unwrap_or(want_id.is_null())
                {
                    return Ok(resp);
                }
                // 不匹配 id 的响应：串行模型下不该出现，跳过
            }
        };
        match tokio::time::timeout(timeout, read_fut).await {
            Ok(res) => res,
            Err(_) => Err(TransportError::Timeout(format!(
                "stdio response timeout after {}s",
                timeout.as_secs()
            ))),
        }
    }

    async fn notify(&self, req: &JsonRpcRequest) -> Result<(), TransportError> {
        let mut guard = self.inner.lock().await;
        let inner = guard
            .as_mut()
            .ok_or_else(|| TransportError::Closed("stdio not connected".into()))?;
        use tokio::io::AsyncWriteExt;
        let mut line = serde_json::to_string(req)
            .map_err(|e| TransportError::Other(format!("serialize notification: {}", e)))?;
        line.push('\n');
        inner
            .stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| TransportError::Closed(format!("write stdin: {}", e)))?;
        inner
            .stdin
            .flush()
            .await
            .map_err(|e| TransportError::Closed(format!("flush stdin: {}", e)))?;
        Ok(())
    }

    async fn reset(&self) -> Result<(), TransportError> {
        let mut guard = self.inner.lock().await;
        if let Some(mut inner) = guard.take() {
            let _ = inner.child.kill().await;
        }
        let inner = spawn_child(&self.command, &self.args, &self.env).await?;
        *guard = Some(inner);
        Ok(())
    }
}

// ───────────────────────── SSE 事件解析 ─────────────────────────

/// 增量 SSE 解析器：喂入字节块，产出完整的 (event_name, data) 对。
/// event 名缺省为 "message"。
pub(crate) struct SseParser {
    tail: String,
    event_name: Option<String>,
    data_lines: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            tail: String::new(),
            event_name: None,
            data_lines: Vec::new(),
        }
    }

    pub fn feed(&mut self, chunk: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        self.tail.push_str(chunk);
        // 逐行消费，保留未完整的尾行
        while let Some(p) = self.tail.find('\n') {
            let line = self.tail[..p].to_string();
            self.tail.drain(..p + 1);
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                // 空行 = 事件边界
                if !self.data_lines.is_empty() {
                    let name = self.event_name.take().unwrap_or_else(|| "message".into());
                    let data = self.data_lines.join("\n");
                    self.data_lines.clear();
                    out.push((name, data));
                } else {
                    self.event_name = None;
                }
            } else if let Some(rest) = line.strip_prefix("event:") {
                self.event_name = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                self.data_lines.push(rest.trim().to_string());
            }
            // ':' 注释与其他字段忽略
        }
        out
    }
}

/// 从完整 SSE 文本中提取第一个 JSON-RPC 响应（streamable HTTP POST 响应用）
pub(crate) fn parse_sse_response(body: &str) -> Option<JsonRpcResponse> {
    let mut parser = SseParser::new();
    for (name, data) in parser.feed(body) {
        if name != "message" {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
            if v.get("method").is_none() {
                if let Ok(resp) = serde_json::from_value::<JsonRpcResponse>(v) {
                    return Some(resp);
                }
            }
        }
    }
    None
}

/// 构建 MCP 用 reqwest client：不走系统代理。
/// MCP server 多为本地/内网服务，Windows 系统代理（Clash 等）常不 bypass
/// loopback，导致 127.0.0.1 请求被代理截断。
fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

// ───────────────────────── streamable HTTP ─────────────────────────

/// streamable HTTP transport（MCP 2025-06-18 spec）：
/// POST JSON-RPC，支持 `Mcp-Session-Id` 会话头；响应可能是 JSON 或 SSE。
pub struct HttpTransport {
    client: reqwest::Client,
    url: String,
    headers: HashMap<String, String>,
    session_id: tokio::sync::Mutex<Option<String>>,
}

impl HttpTransport {
    pub fn new(url: &str, headers: HashMap<String, String>) -> Self {
        Self {
            client: build_http_client(),
            url: url.to_string(),
            headers,
            session_id: tokio::sync::Mutex::new(None),
        }
    }

    fn build_request(
        &self,
        body: &str,
        session: Option<&str>,
        timeout: Duration,
    ) -> Result<reqwest::RequestBuilder, TransportError> {
        let mut rb = self
            .client
            .post(&self.url)
            .timeout(timeout)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(body.to_string());
        for (k, v) in &self.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if let Some(sid) = session {
            rb = rb.header("Mcp-Session-Id", sid);
        }
        Ok(rb)
    }

    /// 解析 POST 响应：状态码检查 + session 头捕获 + JSON/SSE body 解析
    async fn read_response(
        &self,
        resp: reqwest::Response,
        had_session: bool,
    ) -> Result<JsonRpcResponse, TransportError> {
        let status = resp.status();
        // session 头更新（initialize 响应携带）
        if let Some(sid) = resp.headers().get("mcp-session-id") {
            if let Ok(s) = sid.to_str() {
                *self.session_id.lock().await = Some(s.to_string());
            }
        }
        if status == reqwest::StatusCode::NO_CONTENT {
            return Err(TransportError::Other("empty 204 response".into()));
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if had_session
                && (status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE)
            {
                return Err(TransportError::StaleSession(format!(
                    "HTTP {} {}",
                    status,
                    truncate(&body, 200)
                )));
            }
            return Err(TransportError::Other(format!(
                "HTTP {}: {}",
                status,
                truncate(&body, 200)
            )));
        }
        let ct = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = resp
            .text()
            .await
            .map_err(|e| TransportError::Closed(format!("read body: {}", e)))?;
        if ct.contains("text/event-stream") {
            parse_sse_response(&body)
                .ok_or_else(|| TransportError::Other("no JSON-RPC response in SSE body".into()))
        } else {
            serde_json::from_str(&body).map_err(|e| {
                TransportError::Other(format!(
                    "parse JSON response: {} ({})",
                    e,
                    truncate(&body, 200)
                ))
            })
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{}...", cut)
    }
}

#[async_trait]
impl McpTransport for HttpTransport {
    async fn request(
        &self,
        req: &JsonRpcRequest,
        timeout: Duration,
    ) -> Result<JsonRpcResponse, TransportError> {
        let body = serde_json::to_string(req)
            .map_err(|e| TransportError::Other(format!("serialize request: {}", e)))?;
        let session = self.session_id.lock().await.clone();
        let rb = self.build_request(&body, session.as_deref(), timeout)?;
        let resp = match rb.send().await {
            Ok(r) => r,
            Err(e) => {
                return Err(if e.is_timeout() {
                    TransportError::Timeout(format!("HTTP request timeout: {}", e))
                } else if e.is_connect() {
                    TransportError::Closed(format!("HTTP connect failed: {}", e))
                } else {
                    TransportError::Other(format!("HTTP request: {}", e))
                });
            }
        };
        self.read_response(resp, session.is_some()).await
    }

    async fn notify(&self, req: &JsonRpcRequest) -> Result<(), TransportError> {
        let body = serde_json::to_string(req)
            .map_err(|e| TransportError::Other(format!("serialize notification: {}", e)))?;
        let session = self.session_id.lock().await.clone();
        let rb = self.build_request(&body, session.as_deref(), Duration::from_secs(30))?;
        let resp = rb
            .send()
            .await
            .map_err(|e| TransportError::Other(format!("HTTP notify: {}", e)))?;
        // 202 Accepted 是 notification 的正常应答
        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::ACCEPTED {
            return Err(TransportError::Other(format!(
                "HTTP notify status {}",
                resp.status()
            )));
        }
        Ok(())
    }

    async fn reset(&self) -> Result<(), TransportError> {
        *self.session_id.lock().await = None;
        Ok(())
    }
}

// ───────────────────────── SSE（旧版兼容） ─────────────────────────

struct SseState {
    message_url: Option<String>,
    reader: Option<tokio::task::JoinHandle<()>>,
}

/// 旧版 SSE transport：GET 长连接读（endpoint/message 事件）+ POST 写。
/// 后台 reader 任务把响应分发到 pending map。
pub struct SseTransport {
    client: reqwest::Client,
    url: String,
    headers: HashMap<String, String>,
    state: tokio::sync::Mutex<SseState>,
    /// reader task 与 request 共享：id → 等待响应的 oneshot
    pending:
        Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<JsonRpcResponse>>>>,
}

use std::sync::Arc;

impl SseTransport {
    /// 建立 SSE 长连接并等待 `endpoint` 事件发现 message URL
    pub async fn connect(
        url: &str,
        headers: HashMap<String, String>,
    ) -> Result<Self, TransportError> {
        let t = Self {
            client: build_http_client(),
            url: url.to_string(),
            headers,
            state: tokio::sync::Mutex::new(SseState {
                message_url: None,
                reader: None,
            }),
            pending: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        };
        t.open_stream(Duration::from_secs(30)).await?;
        Ok(t)
    }

    /// 打开 GET 长连接，spawn reader 任务，等待 endpoint 发现
    async fn open_stream(&self, timeout: Duration) -> Result<(), TransportError> {
        let mut rb = self
            .client
            .get(&self.url)
            // SSE 长连接不设整体超时（否则流会被截断）；连接阶段由外层 timeout 包裹
            .header("Accept", "text/event-stream");
        for (k, v) in &self.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        let resp = tokio::time::timeout(timeout, rb.send())
            .await
            .map_err(|_| TransportError::Timeout("SSE connect timeout".into()))?
            .map_err(|e| TransportError::Closed(format!("SSE connect: {}", e)))?;
        if !resp.status().is_success() {
            return Err(TransportError::Closed(format!(
                "SSE status {}",
                resp.status()
            )));
        }

        let (tx, rx) = tokio::sync::oneshot::channel::<Result<String, TransportError>>();
        let mut tx = Some(tx);
        let pending = Arc::clone(&self.pending);
        let base_url = self.url.clone();

        use futures_util::StreamExt;
        let mut stream = resp.bytes_stream();
        let handle = tokio::spawn(async move {
            let mut parser = SseParser::new();
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(_) => break,
                };
                let text = String::from_utf8_lossy(&chunk);
                for (name, data) in parser.feed(&text) {
                    match name.as_str() {
                        "endpoint" => {
                            let resolved = resolve_endpoint(&base_url, data.trim());
                            if let Some(tx) = tx.take() {
                                let _ = tx.send(Ok(resolved));
                            }
                        }
                        "message" => {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                                if v.get("method").is_none() {
                                    if let Ok(resp) =
                                        serde_json::from_value::<JsonRpcResponse>(v.clone())
                                    {
                                        let key = id_key(resp.id.as_ref());
                                        let mut p = pending.lock().await;
                                        if let Some(sender) = p.remove(&key) {
                                            let _ = sender.send(resp);
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            // 流结束：通知所有 pending 调用 transport 已关闭
            let mut p = pending.lock().await;
            for (_, sender) in p.drain() {
                // oneshot drop 即表示发送端关闭，receive 侧收到 RecvError
                drop(sender);
            }
        });

        let endpoint = tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| TransportError::Timeout("SSE endpoint discovery timeout".into()))?
            .map_err(|_| TransportError::Closed("SSE stream closed before endpoint".into()))??;

        let mut state = self.state.lock().await;
        state.message_url = Some(endpoint);
        state.reader = Some(handle);
        Ok(())
    }
}

fn id_key(id: Option<&serde_json::Value>) -> String {
    match id {
        Some(v) => v.to_string(),
        None => "null".to_string(),
    }
}

/// 把 SSE `endpoint` 事件给出的地址解析为绝对 URL（支持相对路径）
pub(crate) fn resolve_endpoint(base: &str, endpoint: &str) -> String {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        return endpoint.to_string();
    }
    // 提取 origin：scheme://host[:port]
    let origin_end = base.find("://").map(|i| {
        let rest = &base[i + 3..];
        match rest.find('/') {
            Some(p) => i + 3 + p,
            None => base.len(),
        }
    });
    match origin_end {
        Some(end) => {
            let origin = &base[..end];
            if endpoint.starts_with('/') {
                format!("{}{}", origin, endpoint)
            } else {
                format!("{}/{}", origin, endpoint)
            }
        }
        None => endpoint.to_string(),
    }
}

#[async_trait]
impl McpTransport for SseTransport {
    async fn request(
        &self,
        req: &JsonRpcRequest,
        timeout: Duration,
    ) -> Result<JsonRpcResponse, TransportError> {
        let (message_url, rx) = {
            let state = self.state.lock().await;
            let url = state
                .message_url
                .clone()
                .ok_or_else(|| TransportError::Closed("SSE not connected".into()))?;
            let (tx, rx) = tokio::sync::oneshot::channel();
            let key = id_key(req.id.as_ref());
            let mut p = self.pending.lock().await;
            p.insert(key, tx);
            (url, rx)
        };

        let body = serde_json::to_string(req)
            .map_err(|e| TransportError::Other(format!("serialize request: {}", e)))?;
        let mut rb = self
            .client
            .post(&message_url)
            .timeout(timeout)
            .header("Content-Type", "application/json")
            .body(body);
        for (k, v) in &self.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if let Err(e) = rb.send().await {
            let key = id_key(req.id.as_ref());
            self.pending.lock().await.remove(&key);
            return Err(if e.is_timeout() {
                TransportError::Timeout(format!("SSE POST timeout: {}", e))
            } else {
                TransportError::Closed(format!("SSE POST: {}", e))
            });
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => Err(TransportError::Closed("SSE stream closed".into())),
            Err(_) => {
                let key = id_key(req.id.as_ref());
                self.pending.lock().await.remove(&key);
                Err(TransportError::Timeout(format!(
                    "SSE response timeout after {}s",
                    timeout.as_secs()
                )))
            }
        }
    }

    async fn notify(&self, req: &JsonRpcRequest) -> Result<(), TransportError> {
        let message_url = {
            let state = self.state.lock().await;
            state
                .message_url
                .clone()
                .ok_or_else(|| TransportError::Closed("SSE not connected".into()))?
        };
        let body = serde_json::to_string(req)
            .map_err(|e| TransportError::Other(format!("serialize notification: {}", e)))?;
        let mut rb = self
            .client
            .post(&message_url)
            .timeout(Duration::from_secs(30))
            .header("Content-Type", "application/json")
            .body(body);
        for (k, v) in &self.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        rb.send()
            .await
            .map_err(|e| TransportError::Closed(format!("SSE notify: {}", e)))?;
        Ok(())
    }

    async fn reset(&self) -> Result<(), TransportError> {
        {
            let mut state = self.state.lock().await;
            if let Some(h) = state.reader.take() {
                h.abort();
            }
            state.message_url = None;
        }
        self.open_stream(Duration::from_secs(30)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sse_parser_basic_message() {
        let mut p = SseParser::new();
        let events = p.feed("data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "message");
        assert!(events[0].1.contains("\"id\":1"));
    }

    #[test]
    fn test_sse_parser_named_event() {
        let mut p = SseParser::new();
        let events = p.feed("event: endpoint\ndata: /messages?sid=1\n\n");
        assert_eq!(
            events,
            vec![("endpoint".to_string(), "/messages?sid=1".to_string())]
        );
    }

    #[test]
    fn test_sse_parser_chunked_feed() {
        let mut p = SseParser::new();
        assert!(p.feed("event: endpo").is_empty());
        assert!(p.feed("int\ndata: /m").is_empty());
        let events = p.feed("sg\n\n");
        assert_eq!(events, vec![("endpoint".to_string(), "/msg".to_string())]);
    }

    #[test]
    fn test_sse_parser_multiline_data() {
        let mut p = SseParser::new();
        let events = p.feed("data: line1\ndata: line2\n\n");
        assert_eq!(events[0].1, "line1\nline2");
    }

    #[test]
    fn test_sse_parser_ignores_comments() {
        let mut p = SseParser::new();
        let events = p.feed(": keep-alive\n\n");
        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_sse_response_picks_jsonrpc_response() {
        let body = "event: ping\ndata: {}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"ok\":true}}\n\n";
        let resp = parse_sse_response(body).unwrap();
        assert_eq!(resp.id.unwrap(), serde_json::json!(2));
    }

    #[test]
    fn test_parse_sse_response_skips_server_notifications() {
        let body =
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{}}\n\n";
        assert!(parse_sse_response(body).is_none());
    }

    #[test]
    fn test_resolve_endpoint_absolute() {
        assert_eq!(
            resolve_endpoint("https://a.com/sse", "https://b.com/msg"),
            "https://b.com/msg"
        );
    }

    #[test]
    fn test_resolve_endpoint_root_relative() {
        assert_eq!(
            resolve_endpoint("https://a.com/sse?x=1", "/messages?sid=9"),
            "https://a.com/messages?sid=9"
        );
    }

    #[test]
    fn test_resolve_endpoint_path_relative() {
        assert_eq!(
            resolve_endpoint("https://a.com:8080", "messages"),
            "https://a.com:8080/messages"
        );
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello...");
    }
}
