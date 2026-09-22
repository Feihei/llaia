//! MCP client 核心：`McpServer`（单 server 连接 + 握手 + 工具调用）
//! 与 `McpRegistry`（多 server 中央路由）。见 ADR-0014。

use crate::mcp::protocol::{
    JsonRpcRequest, McpToolDef, McpToolsListResult, MCP_PROTOCOL_VERSION,
    MCP_STATELESS_PROTOCOL_VERSION, MCP_SUPPORTED_PROTOCOL_VERSIONS, METHOD_NOT_FOUND,
};
use crate::mcp::transport::{McpTransport, SseTransport, StdioTransport, TransportError};
use crate::mcp::{McpServerConfig, McpTransportKind};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// initialize / tools/list 握手超时
pub const RECV_TIMEOUT_SECS: u64 = 30;
/// 工具调用默认超时
pub const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 180;
/// per-server 超时硬上限
pub const MAX_TOOL_TIMEOUT_SECS: u64 = 600;
/// bounded reconnect 参数
const MAX_RECONNECT_ATTEMPTS: u32 = 2;
const RECONNECT_BACKOFF_MS: u64 = 500;
/// isError 错误详情截断上限
const ERROR_DETAIL_MAX_CHARS: usize = 500;

/// 单个 MCP server 连接
pub struct McpServer {
    pub config: McpServerConfig,
    transport: Arc<dyn McpTransport>,
    tool_timeout: Duration,
    next_id: AtomicU64,
    /// 串行化请求（transport 按 id 匹配响应，不能并发）
    seq: tokio::sync::Mutex<()>,
    /// 握手后缓存的工具列表
    tools: tokio::sync::Mutex<Vec<McpToolDef>>,
    /// 无状态降级模式：server 对 initialize 回 -32601（新 spec 2026-07-28
    /// 删掉了 initialize 握手）时置位，此后所有请求带 `params._meta`
    stateless: AtomicBool,
    /// initialize 响应里 server 协商的 protocolVersion（记录 + 支持集校验）
    negotiated_version: std::sync::Mutex<Option<String>>,
}

impl McpServer {
    /// 建立连接并完成握手（initialize + initialized + tools/list）。
    /// 任一步失败返回 Err（调用方 log + 跳过，不阻塞启动）。
    pub async fn connect(config: McpServerConfig) -> anyhow::Result<Arc<Self>> {
        let transport = Self::build_transport(&config).await?;
        Self::finish_connect(config, transport).await
    }

    /// 按配置构建传输层（stdio / streamable HTTP / 旧版 SSE）
    async fn build_transport(config: &McpServerConfig) -> anyhow::Result<Arc<dyn McpTransport>> {
        let transport: Arc<dyn McpTransport> =
            match config.transport {
                McpTransportKind::Stdio => {
                    let command = config.command.clone().ok_or_else(|| {
                        anyhow::anyhow!("stdio server '{}' missing command", config.id)
                    })?;
                    Arc::new(StdioTransport::connect(&command, &config.args, &config.env).await?)
                }
                McpTransportKind::Http => Arc::new(crate::mcp::transport::HttpTransport::new(
                    config.url.as_deref().ok_or_else(|| {
                        anyhow::anyhow!("http server '{}' missing url", config.id)
                    })?,
                    config.headers.clone(),
                )),
                McpTransportKind::Sse => Arc::new(
                    SseTransport::connect(
                        config.url.as_deref().ok_or_else(|| {
                            anyhow::anyhow!("sse server '{}' missing url", config.id)
                        })?,
                        config.headers.clone(),
                    )
                    .await?,
                ),
            };
        Ok(transport)
    }

    /// 由现成 transport 构造 server 并完成握手（生产走 `connect`；测试注入 mock transport）
    async fn finish_connect(
        config: McpServerConfig,
        transport: Arc<dyn McpTransport>,
    ) -> anyhow::Result<Arc<Self>> {
        let tool_timeout_secs = config
            .tool_timeout_secs
            .unwrap_or(DEFAULT_TOOL_TIMEOUT_SECS)
            .min(MAX_TOOL_TIMEOUT_SECS);
        let server = Arc::new(Self {
            config,
            transport,
            tool_timeout: Duration::from_secs(tool_timeout_secs),
            next_id: AtomicU64::new(1),
            seq: tokio::sync::Mutex::new(()),
            tools: tokio::sync::Mutex::new(Vec::new()),
            stateless: AtomicBool::new(false),
            negotiated_version: std::sync::Mutex::new(None),
        });
        server.handshake().await?;
        Ok(server)
    }

    /// initialize + notifications/initialized + tools/list。
    /// 三态：① 旧协议 server 正常握手（主路径，不动）；② server 对 initialize 回
    /// -32601（新 spec 2026-07-28 删掉了握手）→ warn 一行 + 跳过握手降级无状态，
    /// 后续请求带 `params._meta`，不发 notifications/initialized；③ 其余错误原样上抛。
    async fn handshake(&self) -> anyhow::Result<()> {
        let timeout = Duration::from_secs(RECV_TIMEOUT_SECS);
        let init_params = json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "llaia", "version": env!("CARGO_PKG_VERSION") }
        });
        match self.raw_request("initialize", init_params, timeout).await {
            Ok(result) => self.record_negotiated_version(&result),
            Err(TransportError::JsonRpc { code, message }) if code == METHOD_NOT_FOUND => {
                self.stateless.store(true, Ordering::Relaxed);
                tracing::warn!(
                    server = %self.config.id,
                    error = %message,
                    "MCP server rejected initialize (method not found); skipping handshake, \
                     continuing stateless (new spec, requests carry _meta)"
                );
            }
            Err(e) => return Err(e.into()),
        }
        if !self.stateless.load(Ordering::Relaxed) {
            self.transport
                .notify(&JsonRpcRequest::notification(
                    "notifications/initialized",
                    json!({}),
                ))
                .await?;
        }
        let result = self.raw_request("tools/list", json!({}), timeout).await?;
        let list: McpToolsListResult = serde_json::from_value(result)
            .map_err(|e| anyhow::anyhow!("parse tools/list result: {}", e))?;
        tracing::info!(
            server = %self.config.id,
            tools = list.tools.len(),
            stateless = self.stateless.load(Ordering::Relaxed),
            "MCP server handshake ok"
        );
        *self.tools.lock().await = list.tools;
        Ok(())
    }

    /// 记录 server 协商的 protocolVersion；支持集外 warn（不拒连——本 client
    /// 的线上用法只有 tools/list + tools/call，跨版本容忍是安全的）
    fn record_negotiated_version(&self, result: &Value) {
        match result.get("protocolVersion").and_then(|v| v.as_str()) {
            Some(v) => {
                if !MCP_SUPPORTED_PROTOCOL_VERSIONS.contains(&v) {
                    tracing::warn!(
                        server = %self.config.id,
                        negotiated = %v,
                        supported = ?MCP_SUPPORTED_PROTOCOL_VERSIONS,
                        "MCP server negotiated a protocol version outside the supported set; \
                         continuing anyway"
                    );
                }
                tracing::info!(
                    server = %self.config.id,
                    version = %v,
                    "MCP protocol version negotiated"
                );
                if let Ok(mut slot) = self.negotiated_version.lock() {
                    *slot = Some(v.to_string());
                }
            }
            None => tracing::warn!(
                server = %self.config.id,
                "MCP initialize result missing protocolVersion; server may not conform to any known spec"
            ),
        }
    }

    /// 无状态降级模式下给请求 params 注入 `_meta`
    /// （新 spec：每请求自带 protocolVersion + clientInfo）
    fn with_stateless_meta(&self, mut params: Value) -> Value {
        if !self.stateless.load(Ordering::Relaxed) {
            return params;
        }
        if let Some(obj) = params.as_object_mut() {
            obj.insert(
                "_meta".to_string(),
                json!({
                    "protocolVersion": MCP_STATELESS_PROTOCOL_VERSION,
                    "clientInfo": { "name": "llaia", "version": env!("CARGO_PKG_VERSION") }
                }),
            );
        }
        params
    }

    /// 发送 JSON-RPC 请求并返回 result（JSON-RPC error 映射为
    /// `TransportError::JsonRpc`，code 可编程判别）。串行执行。
    async fn raw_request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, TransportError> {
        let _guard = self.seq.lock().await;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest::new(id, method, self.with_stateless_meta(params));
        let resp = self.transport.request(&req, timeout).await?;
        if let Some(err) = resp.error {
            return Err(TransportError::JsonRpc {
                code: err.code,
                message: err.message,
            });
        }
        Ok(resp.result.unwrap_or(Value::Null))
    }

    /// 工具列表快照
    pub async fn tools_snapshot(&self) -> Vec<McpToolDef> {
        self.tools.lock().await.clone()
    }

    /// 调用工具，处理 isError envelope + bounded reconnect。
    /// 返回拼接后的文本内容（或 structuredContent JSON）。
    pub async fn call_tool(&self, name: &str, args: Value) -> anyhow::Result<String> {
        let mut attempts = 0;
        loop {
            let params = json!({ "name": name, "arguments": args });
            match self
                .raw_request("tools/call", params, self.tool_timeout)
                .await
            {
                Ok(result) => return check_result_is_error(&result),
                Err(e) => {
                    let retriable = matches!(
                        e,
                        TransportError::Closed(_) | TransportError::StaleSession(_)
                    );
                    if !retriable || attempts >= MAX_RECONNECT_ATTEMPTS {
                        return Err(anyhow::anyhow!("mcp tool '{}' call failed: {}", name, e));
                    }
                    attempts += 1;
                    tracing::warn!(
                        server = %self.config.id,
                        error = %e,
                        attempt = attempts,
                        "MCP transport broken, reconnecting"
                    );
                    tokio::time::sleep(Duration::from_millis(RECONNECT_BACKOFF_MS)).await;
                    if let Err(re) = self.transport.reset().await {
                        tracing::warn!(server = %self.config.id, error = %re, "MCP reconnect reset failed");
                        continue;
                    }
                    if let Err(he) = self.handshake().await {
                        tracing::warn!(server = %self.config.id, error = %he, "MCP re-handshake failed");
                        continue;
                    }
                }
            }
        }
    }
}

/// 处理 MCP tools/call 结果：`isError: true` → Err（含 content 文本）；
/// 否则提取 content[].text 拼接（无 text 时用 structuredContent JSON）。
fn check_result_is_error(result: &Value) -> anyhow::Result<String> {
    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let text = extract_content_text(result);
    if is_error {
        anyhow::bail!("{}", scrub_and_truncate(&text));
    }
    Ok(text)
}

/// 提取 result.content[].text 拼接；无 text 时回退 structuredContent / 原始 JSON
fn extract_content_text(result: &Value) -> String {
    if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
        let mut parts = Vec::new();
        for item in content {
            if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                    parts.push(t.to_string());
                }
            }
        }
        if !parts.is_empty() {
            return parts.join("\n");
        }
    }
    if let Some(sc) = result.get("structuredContent") {
        return sc.to_string();
    }
    result.to_string()
}

/// secret scrubbing：掩码常见 key/token/password/secret 键值对，
/// 避免错误详情把凭据泄漏到日志或 LLM 上下文
pub(crate) fn scrub_and_truncate(s: &str) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // 命中敏感 key 后，把值整段掩掉直到逗号/换行/串尾，
        // 避免 "Authorization: Bearer xxx" 这类带前缀的值漏网
        regex::Regex::new(
            r#"(?i)\b(api[_-]?key|token|secret|password|authorization|auth)\b\s*[:=]\s*[^\n,]*"#,
        )
        .expect("static regex must compile")
    });
    let scrubbed = re.replace_all(s, "${1}: ***").to_string();
    let char_count = scrubbed.chars().count();
    if char_count <= ERROR_DETAIL_MAX_CHARS {
        scrubbed
    } else {
        let cut: String = scrubbed.chars().take(ERROR_DETAIL_MAX_CHARS).collect();
        format!("{}...", cut)
    }
}

/// MCP registry：中央路由所有 server 的工具调用。
/// prefixed_name（`<server_id>__<tool_name>`）→ (server_idx, original_tool_name)
pub struct McpRegistry {
    servers: Vec<Arc<McpServer>>,
    tool_index: HashMap<String, (usize, String)>,
    /// 连接失败的 server（供 WebUI 展示 dead 状态）：id → 错误信息
    failed: HashMap<String, String>,
}

impl McpRegistry {
    /// 初始化所有 enabled server；单个失败 log + 跳过，不阻塞启动。
    /// 所有 server **并发**连接（plan.md 启动优化③）：单 server 握手超时（30s）
    /// 不再串行累加；连接完成后按原配置顺序注册，保证 tool_index 稳定。
    pub async fn connect_all(configs: &[McpServerConfig]) -> Self {
        let mut servers = Vec::new();
        let mut tool_index = HashMap::new();
        let mut failed = HashMap::new();

        for cfg in configs.iter().filter(|c| !c.enabled) {
            tracing::info!(server = %cfg.id, "MCP server disabled, skip");
        }
        let enabled: Vec<McpServerConfig> = configs.iter().filter(|c| c.enabled).cloned().collect();
        let results = futures_util::future::join_all(
            enabled.iter().map(|cfg| McpServer::connect(cfg.clone())),
        )
        .await;

        for (cfg, result) in enabled.iter().zip(results) {
            match result {
                Ok(server) => {
                    let idx = servers.len();
                    for def in server.tools_snapshot().await {
                        let prefixed = format!("{}__{}", cfg.id, def.name);
                        tool_index.insert(prefixed, (idx, def.name));
                    }
                    servers.push(server);
                }
                Err(e) => {
                    tracing::error!(
                        server = %cfg.id,
                        error = %e,
                        "MCP server connect failed, skipping its tools"
                    );
                    failed.insert(cfg.id.clone(), e.to_string());
                }
            }
        }

        tracing::info!(
            servers = servers.len(),
            tools = tool_index.len(),
            failed = failed.len(),
            "McpRegistry built"
        );
        Self {
            servers,
            tool_index,
            failed,
        }
    }

    /// 空 registry（无 mcp.toml 时用）
    pub fn empty() -> Self {
        Self {
            servers: Vec::new(),
            tool_index: HashMap::new(),
            failed: HashMap::new(),
        }
    }

    /// 路由工具调用到正确的 server
    pub async fn call_tool(&self, prefixed_name: &str, args: Value) -> anyhow::Result<String> {
        let (idx, name) = self
            .tool_index
            .get(prefixed_name)
            .ok_or_else(|| anyhow::anyhow!("unknown MCP tool: {}", prefixed_name))?;
        self.servers[*idx].call_tool(name, args).await
    }

    /// 所有工具定义（带前缀名），供注册到 ToolRegistry
    pub async fn tool_defs(&self) -> Vec<(String, McpToolDef)> {
        let mut out = Vec::new();
        for (prefixed, (idx, _)) in &self.tool_index {
            let server = &self.servers[*idx];
            // tool_index 与 tools_snapshot 同源，按原始名找回定义
            let name = prefixed
                .split_once("__")
                .map(|(_, n)| n)
                .unwrap_or(prefixed);
            let defs = server.tools_snapshot().await;
            if let Some(def) = defs.iter().find(|d| d.name == name) {
                out.push((prefixed.clone(), def.clone()));
            }
        }
        out
    }

    /// safe_tools 白名单查询（按前缀名）：true 表示该工具免确认
    pub fn is_safe_tool(&self, prefixed_name: &str) -> bool {
        let Some((idx, name)) = self.tool_index.get(prefixed_name) else {
            return false;
        };
        self.servers[*idx]
            .config
            .safe_tools
            .iter()
            .any(|s| s == name)
    }

    /// WebUI 状态快照：每个配置过的 server（含失败）+ 工具列表
    pub async fn status(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for server in &self.servers {
            let cfg = &server.config;
            let tools: Vec<Value> = server
                .tools_snapshot()
                .await
                .iter()
                .map(|d| {
                    json!({
                        "name": format!("{}__{}", cfg.id, d.name),
                        "description": d.description.clone().unwrap_or_default(),
                        "requires_confirm": !cfg.safe_tools.iter().any(|s| s == &d.name),
                    })
                })
                .collect();
            out.push(json!({
                "id": cfg.id,
                "transport": cfg.transport,
                "enabled": cfg.enabled,
                "status": "connected",
                "error": Value::Null,
                "tools": tools,
            }));
        }
        for (id, err) in &self.failed {
            out.push(json!({
                "id": id,
                "transport": Value::Null,
                "enabled": true,
                "status": "dead",
                "error": err,
                "tools": [],
            }));
        }
        out
    }

    pub fn tool_count(&self) -> usize {
        self.tool_index.len()
    }

    pub fn server_count(&self) -> usize {
        self.servers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::protocol::JsonRpcResponse;

    #[test]
    fn test_check_result_is_error_ok() {
        let result = json!({
            "content": [{ "type": "text", "text": "hello" }]
        });
        assert_eq!(check_result_is_error(&result).unwrap(), "hello");
    }

    #[test]
    fn test_check_result_is_error_multiple_texts_joined() {
        let result = json!({
            "content": [
                { "type": "text", "text": "line1" },
                { "type": "text", "text": "line2" }
            ]
        });
        assert_eq!(check_result_is_error(&result).unwrap(), "line1\nline2");
    }

    #[test]
    fn test_check_result_is_error_flag_maps_to_err() {
        let result = json!({
            "isError": true,
            "content": [{ "type": "text", "text": "boom" }]
        });
        let err = check_result_is_error(&result).unwrap_err();
        assert_eq!(err.to_string(), "boom");
    }

    #[test]
    fn test_extract_falls_back_to_structured_content() {
        let result = json!({ "structuredContent": { "k": 1 } });
        assert_eq!(check_result_is_error(&result).unwrap(), "{\"k\":1}");
    }

    #[test]
    fn test_scrub_masks_secrets() {
        let s = r#"auth failed: Authorization: Bearer abc123, api_key="sk-xyz""#;
        let scrubbed = scrub_and_truncate(s);
        assert!(!scrubbed.contains("abc123"), "got: {}", scrubbed);
        assert!(!scrubbed.contains("sk-xyz"), "got: {}", scrubbed);
        assert!(scrubbed.contains("***"));
    }

    #[test]
    fn test_scrub_truncates_long_text() {
        let s = "x".repeat(1000);
        let out = scrub_and_truncate(&s);
        assert_eq!(out.chars().count(), ERROR_DETAIL_MAX_CHARS + 3); // 500 + "..."
    }

    #[tokio::test]
    async fn test_empty_registry_unknown_tool() {
        let registry = McpRegistry::empty();
        let err = registry
            .call_tool("nowhere__ghost", json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown MCP tool"));
    }

    #[tokio::test]
    async fn test_connect_all_skips_invalid_stdio() {
        // 不存在的命令 → 连接失败但不 panic，记录到 failed
        let cfg = crate::mcp::McpServerConfig {
            id: "bad".into(),
            enabled: true,
            transport: McpTransportKind::Stdio,
            command: Some("llaia_nonexistent_cmd_2026".into()),
            args: vec![],
            env: HashMap::new(),
            url: None,
            headers: HashMap::new(),
            tool_timeout_secs: None,
            safe_tools: vec![],
        };
        let registry = McpRegistry::connect_all(&[cfg]).await;
        assert_eq!(registry.server_count(), 0);
        let status = registry.status().await;
        assert_eq!(status.len(), 1);
        assert_eq!(status[0]["status"], "dead");
    }

    // ── mock transport：按 method 分派 canned 响应，记录收到的请求 ──
    // （新 spec 降级 / 版本协商行为测试用；不 spawn 真实子进程）

    struct MockTransport {
        /// method → result 载荷（或 `{"code":..,"message":..}` 错误对象）
        replies: HashMap<String, Value>,
        received: std::sync::Mutex<Vec<(String, Option<Value>)>>,
        notifications: std::sync::Mutex<Vec<String>>,
    }

    impl MockTransport {
        fn new(replies: HashMap<String, Value>) -> Self {
            Self {
                replies,
                received: std::sync::Mutex::new(Vec::new()),
                notifications: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl McpTransport for MockTransport {
        async fn request(
            &self,
            req: &JsonRpcRequest,
            _timeout: Duration,
        ) -> Result<JsonRpcResponse, TransportError> {
            self.received
                .lock()
                .unwrap()
                .push((req.method.clone(), req.params.clone()));
            let raw = self.replies.get(&req.method).cloned().ok_or_else(|| {
                TransportError::Other(format!("mock: no reply for {}", req.method))
            })?;
            let is_error = raw.get("code").is_some() && raw.get("result").is_none();
            let resp = if is_error {
                JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: req.id.clone(),
                    result: None,
                    error: Some(serde_json::from_value(raw).map_err(|e| {
                        TransportError::Other(format!("mock bad error payload: {}", e))
                    })?),
                }
            } else {
                JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: req.id.clone(),
                    result: Some(raw),
                    error: None,
                }
            };
            Ok(resp)
        }

        async fn notify(&self, req: &JsonRpcRequest) -> Result<(), TransportError> {
            self.notifications.lock().unwrap().push(req.method.clone());
            Ok(())
        }

        async fn reset(&self) -> Result<(), TransportError> {
            Ok(())
        }
    }

    fn mock_cfg(id: &str) -> McpServerConfig {
        McpServerConfig {
            id: id.to_string(),
            enabled: true,
            transport: McpTransportKind::Stdio,
            command: Some("mock".into()),
            args: vec![],
            env: HashMap::new(),
            url: None,
            headers: HashMap::new(),
            tool_timeout_secs: None,
            safe_tools: vec![],
        }
    }

    fn mock_replies(initialize: Value, tools_list: Value) -> HashMap<String, Value> {
        HashMap::from([
            ("initialize".to_string(), initialize),
            ("tools/list".to_string(), tools_list),
        ])
    }

    #[tokio::test]
    async fn test_handshake_legacy_records_negotiated_version() {
        // 旧协议 server：正常握手 → 不降级、记录协商版本、initialized 照发、
        // initialize params 不带 _meta（旧协议主路径不动）
        let replies = mock_replies(
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "serverInfo": { "name": "mock", "version": "1" }
            }),
            json!({ "tools": [{ "name": "echo", "inputSchema": {} }] }),
        );
        let transport = Arc::new(MockTransport::new(replies));
        let server = McpServer::finish_connect(mock_cfg("legacy"), transport.clone())
            .await
            .unwrap();

        assert!(!server.stateless.load(Ordering::Relaxed));
        assert_eq!(
            server.negotiated_version.lock().unwrap().as_deref(),
            Some("2025-06-18")
        );
        assert_eq!(server.tools_snapshot().await.len(), 1);
        assert_eq!(
            *transport.notifications.lock().unwrap(),
            vec!["notifications/initialized".to_string()]
        );
        let received = transport.received.lock().unwrap();
        let methods: Vec<&str> = received.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(methods, vec!["initialize", "tools/list"]);
        let (_, init_params) = &received[0];
        assert!(init_params.as_ref().unwrap().get("_meta").is_none());
    }

    #[tokio::test]
    async fn test_handshake_initialize_rejected_downgrades_to_stateless() {
        // 新 spec server（2026-07-28 删掉了 initialize）：-32601 → 跳过握手、
        // 不发 initialized、tools/list 请求带 _meta（新 spec 档版本号 + clientInfo）
        let replies = mock_replies(
            json!({ "code": -32601, "message": "Method not found" }),
            json!({ "tools": [] }),
        );
        let transport = Arc::new(MockTransport::new(replies));
        let server = McpServer::finish_connect(mock_cfg("newspec"), transport.clone())
            .await
            .unwrap();

        assert!(server.stateless.load(Ordering::Relaxed));
        assert!(server.negotiated_version.lock().unwrap().is_none());
        assert!(transport.notifications.lock().unwrap().is_empty());
        let received = transport.received.lock().unwrap();
        let methods: Vec<&str> = received.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(methods, vec!["initialize", "tools/list"]);
        let (_, list_params) = &received[1];
        let meta = list_params.as_ref().unwrap().get("_meta").unwrap();
        assert_eq!(meta["protocolVersion"], MCP_STATELESS_PROTOCOL_VERSION);
        assert_eq!(meta["clientInfo"]["name"], "llaia");
    }

    #[tokio::test]
    async fn test_stateless_tool_call_carries_meta() {
        // 降级后 tools/call 同样带 _meta，且 name/arguments 原样保留
        let mut replies = mock_replies(
            json!({ "code": -32601, "message": "Method not found" }),
            json!({ "tools": [{ "name": "echo", "inputSchema": {} }] }),
        );
        replies.insert(
            "tools/call".to_string(),
            json!({ "content": [{ "type": "text", "text": "hi" }] }),
        );
        let transport = Arc::new(MockTransport::new(replies));
        let server = McpServer::finish_connect(mock_cfg("newspec"), transport.clone())
            .await
            .unwrap();

        let out = server
            .call_tool("echo", json!({ "msg": "x" }))
            .await
            .unwrap();
        assert_eq!(out, "hi");
        let received = transport.received.lock().unwrap();
        let (_, call_params) = received
            .iter()
            .find(|(m, _)| m == "tools/call")
            .expect("tools/call recorded");
        let p = call_params.as_ref().unwrap();
        assert_eq!(p["name"], "echo");
        assert!(
            p.get("_meta").is_some(),
            "stateless tool call must carry _meta"
        );
    }

    #[tokio::test]
    async fn test_negotiated_unknown_version_still_connects() {
        // 支持集外的协商版本：warn（不拒连）+ 照实记录
        let replies = mock_replies(
            json!({ "protocolVersion": "1999-01-01" }),
            json!({ "tools": [] }),
        );
        let transport = Arc::new(MockTransport::new(replies));
        let server = McpServer::finish_connect(mock_cfg("weird"), transport)
            .await
            .unwrap();
        assert_eq!(
            server.negotiated_version.lock().unwrap().as_deref(),
            Some("1999-01-01")
        );
    }

    #[tokio::test]
    async fn test_handshake_other_rpc_error_fails_connect() {
        // -32601 之外的 JSON-RPC 错误不是降级信号，照常失败
        let replies = mock_replies(
            json!({ "code": -32602, "message": "Invalid params" }),
            json!({ "tools": [] }),
        );
        let err = match McpServer::finish_connect(
            mock_cfg("broken"),
            Arc::new(MockTransport::new(replies)),
        )
        .await
        {
            Ok(_) => panic!("expected handshake failure on -32602"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("JSON-RPC error -32602"),
            "got: {}",
            err
        );
    }
}
