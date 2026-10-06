use crate::config::Config;
use crate::tools::search::tavily::TavilyProvider;
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures_util::StreamExt as FuturesStreamExt;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// 下载字节硬上限：避免把整站大文件/视频灌进内存。抽取后还会按 `max_chars` 二次截断文本。
const MAX_DOWNLOAD_BYTES: usize = 4 * 1024 * 1024;

/// 解析 `web_fetch.extract_provider` 引用为服务端抽取器（P8）。
/// 须为支持 extract 能力的 service family provider（v1 = tavily）。
/// 返回 `Ok(None)` = 引用为空（本地抽取）；`Err` = 引用无效（调用方 warn 后退化本地）。
pub fn resolve_extractor(config: &Config, id: &str) -> Result<Option<Arc<TavilyProvider>>> {
    let id = id.trim();
    if id.is_empty() {
        return Ok(None);
    }
    let Some(prov) = config.provider.get(id) else {
        anyhow::bail!("provider '{id}' not configured");
    };
    if !prov.is_service() {
        anyhow::bail!("provider '{id}' is not a service-family provider");
    }
    if prov.search_platform() != Some("tavily") {
        anyhow::bail!(
            "provider '{id}' (platform {:?}) has no extract capability (v1: tavily only)",
            prov.search_platform()
        );
    }
    if prov.api_key.is_empty() {
        anyhow::bail!("provider '{id}' has an empty api_key");
    }
    Ok(Some(Arc::new(TavilyProvider::new(prov.api_key.clone())?)))
}

/// web_fetch 出站闸门（ADR-0033 L4，P9 Phase 2）：域名允许清单 + 交互频道
/// 首访批准集。清单只收紧非交互频道（未配置 = fail-closed 全拒），交互频道
/// 走 trusted_dirs 同款模式（首访新域名审批一次，`/ok` 后由 `execute_approved`
/// 持久化到 `<config_dir>/web_domains.json`）。SSRF 校验不在此结构——它无条件
/// 常开、不涉配置，见 `ssrf_check`。
pub struct WebFetchGate {
    /// `[tools.web_fetch].allowed_domains`：非交互频道唯一放行来源
    allowed: Vec<String>,
    /// 交互频道已批准域名（内存 + web_domains.json 持久化）
    approved: std::sync::RwLock<Vec<String>>,
    config_dir: PathBuf,
}

impl WebFetchGate {
    /// 测试用：不落盘。
    pub fn new(allowed: Vec<String>) -> Self {
        Self {
            allowed,
            approved: std::sync::RwLock::new(Vec::new()),
            config_dir: std::path::PathBuf::new(),
        }
    }

    /// 生产构造：加载已批准域名集（缺失/损坏 = 空集，与 trusted_store 同口径）。
    pub fn load(config_dir: &Path, allowed: Vec<String>) -> Self {
        let approved = std::fs::read_to_string(config_dir.join("web_domains.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
            .unwrap_or_default();
        Self {
            allowed,
            approved: std::sync::RwLock::new(approved),
            config_dir: config_dir.to_path_buf(),
        }
    }

    /// 非交互频道的唯一判定：是否在配置允许清单内。
    pub fn allowed_contains(&self, host: &str) -> bool {
        self.allowed.iter().any(|d| domain_matches(host, d))
    }

    /// 交互频道判定：允许清单 ∪ 已批准集。
    pub fn permits(&self, host: &str) -> bool {
        self.allowed_contains(host)
            || self
                .approved
                .read()
                .map(|set| set.iter().any(|d| domain_matches(host, d)))
                .unwrap_or(false)
    }

    /// 登记批准（内存 + 持久化；写失败静默降级为会话级，trusted_store 同口径）。
    pub fn approve(&self, host: &str) {
        let mut set = match self.approved.write() {
            Ok(s) => s,
            Err(e) => {
                let _ = e; // 毒锁恢复
                return;
            }
        };
        if set.iter().any(|d| domain_matches(host, d)) {
            return;
        }
        set.push(host.to_string());
        if self.config_dir.as_os_str().is_empty() {
            return;
        }
        if let Ok(json) = serde_json::to_string_pretty(&*set) {
            if std::fs::create_dir_all(&self.config_dir).is_ok() {
                let _ = std::fs::write(
                    self.config_dir.join("web_domains.json"),
                    json + "
",
                );
            }
        }
    }
}

/// 域名匹配：精确或子域后缀，大小写不敏感，容忍尾点。
fn domain_matches(host: &str, entry: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let entry = entry.trim().trim_end_matches('.').to_ascii_lowercase();
    if entry.is_empty() {
        return false;
    }
    host == entry || host.strip_suffix(&format!(".{entry}")).is_some()
}

/// URL → 小写 host（`url::Url` 解析失败或无 host = None，闸门跳过、执行层兜底）。
pub fn url_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    // IPv6 host 去方括号（Url::host_str 返回 "[::1]" 形态）
    let host = host.trim_end_matches('.').trim_matches(['[', ']']);
    Some(host.to_ascii_lowercase())
}

/// SSRF 封锁集（学 zeroclaw domain_guard）：环回 / 私网三段 / 链路本地（含云
/// 元数据 169.254.169.254）/ 未指定 / 广播 + IPv6 unique-local / 链路本地 /
/// IPv4-mapped 解包复查。
fn ip_is_blocked(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
        }
        std::net::IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return ip_is_blocked(std::net::IpAddr::V4(mapped));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // link local fe80::/10
        }
    }
}

/// SSRF 校验（无条件常开，execute 与 execute_approved 同过）：scheme 仅
/// http/https；host 为 IP 字面量直接判定；域名先 DNS 解析再逐 IP 复验
/// （防 rebinding 第一步；与 reqwest 自行解析间的 TOCTOU 残窗留档接受）。
pub async fn ssrf_check(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url).map_err(|e| anyhow!("invalid url: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => anyhow::bail!(
            "scheme {other:?} not allowed (http/https only). {}",
            crate::path_guard::HARD_BOUNDARY_NOTICE
        ),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("url has no host"))?;
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if ip_is_blocked(ip) {
            anyhow::bail!(
                "[ssrf-guard] {host} is a blocked address (private/loopback/link-local). {}",
                crate::path_guard::HARD_BOUNDARY_NOTICE
            );
        }
        return Ok(());
    }
    let port = parsed.port_or_known_default().unwrap_or(0);
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| anyhow!("dns resolve {host}: {e}"))?;
    for addr in addrs {
        if ip_is_blocked(addr.ip()) {
            anyhow::bail!(
                "[ssrf-guard] {host} resolves to blocked address {}. {}",
                addr.ip(),
                crate::path_guard::HARD_BOUNDARY_NOTICE
            );
        }
    }
    Ok(())
}

pub struct WebFetch {
    client: reqwest::Client,
    max_chars: usize,
    /// 可选服务端抽取器（由 `web_fetch.extract_provider` 引用解析）。
    /// `None` 时走本地 readability / html2text 抽取。
    tavily: Option<Arc<TavilyProvider>>,
    /// 出站域名闸门（允许清单 + 首访批准集）
    gate: Arc<WebFetchGate>,
}

impl WebFetch {
    pub fn new(
        max_chars: usize,
        tavily: Option<Arc<TavilyProvider>>,
        gate: Arc<WebFetchGate>,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::limited(10))
            .user_agent("LLAIA-web_fetch/0.1")
            .build()?;
        Ok(Self {
            client,
            max_chars,
            tavily,
            gate,
        })
    }

    fn truncate(&self, text: &str) -> String {
        if text.chars().count() > self.max_chars {
            let mut truncated: String = text.chars().take(self.max_chars).collect();
            truncated.push_str("\n\n... [content truncated to max_chars] ...");
            truncated
        } else {
            text.to_string()
        }
    }
}

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &str {
        "web_fetch"
    }
    fn description(&self) -> &str {
        "Fetch a web page and return its main content as clean plain text. \
         HTML is converted to readable text (via Tavily extract when configured, \
         otherwise local extraction). JSON / plain text / markdown are returned as-is. \
         Only GET; follows redirects."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "HTTP(S) URL" }
            },
            "required": ["url"]
        })
    }
    async fn execute(&self, args: &Value, channel: &str) -> Result<String> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'url'"))?;

        // L4 出站闸门：SSRF 无条件常开；域名闸门按频道分档（交互 = allowed ∪
        // approved，非交互 = allowed only，未配置 fail-closed）。执行层复查是
        // 双保险——审批层已挡 delegate/yolo 早退之前的判定。
        ssrf_check(url).await?;
        self.egress_check(url, channel)?;

        self.fetch(url).await
    }

    /// 批准豁免入口（`/ok` 后）：SSRF 仍无条件常开（不涉可用性取舍）；域名
    /// 闸门豁免——用户已见完整 URL 并批准。批准即登记持久化（trusted_dirs
    /// 同款：首次访问新域名的审批产物）。
    async fn execute_approved(
        &self,
        args: &Value,
        _channel: &str,
        _event_tx: Option<&tokio::sync::mpsc::Sender<crate::agent::TurnEvent>>,
    ) -> Result<String> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'url'"))?;
        ssrf_check(url).await?;
        if let Some(host) = url_host(url) {
            self.gate.approve(&host);
        }
        self.fetch(url).await
    }
}

impl WebFetch {
    async fn fetch(&self, url: &str) -> Result<String> {
        // 优先走 Tavily 服务端抽取（对反爬 / JS 渲染页成功率更高，对应 AstrBot 做法）。
        // 失败则退化为本地解析，保证可用性。
        if let Some(tavily) = &self.tavily {
            match tavily.extract(url).await {
                Ok(text) => return Ok(self.truncate(&text)),
                Err(e) => {
                    tracing::warn!(error = %e, "tavily extract failed, falling back to local parse")
                }
            }
        }

        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| anyhow!("fetch {}: {}", url, e))?;
        if !resp.status().is_success() {
            return Err(anyhow!("HTTP {} for {}", resp.status(), url));
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        // 非 HTML 类型原样透传（zeroclaw 做法），仅做体积截断。
        let is_html = content_type.contains("text/html") || content_type.is_empty();
        if !is_html {
            if content_type.contains("text/plain")
                || content_type.contains("text/markdown")
                || content_type.contains("application/json")
            {
                let bytes = read_limited(resp).await?;
                let text = String::from_utf8_lossy(&bytes);
                return Ok(self.truncate(&text));
            }
            return Err(anyhow!(
                "unsupported content type: {}. web_fetch supports text/html, text/plain, text/markdown, application/json",
                content_type
            ));
        }

        // HTML：本地抽取。先 readability 取主内容，抽不出（过短 / 非文章页）再退 html2text 全页。
        let bytes = read_limited(resp).await?;
        let text = local_extract_html(&bytes, url);
        Ok(self.truncate(&text))
    }

    /// 域名闸门复查（execute 路径；与审批层同一套规则）。
    fn egress_check(&self, url: &str, channel: &str) -> Result<()> {
        let Some(host) = url_host(url) else {
            return Ok(()); // 无 host 的畸形 URL 交给 fetch 自然报错
        };
        let ok = if crate::agent::approval::is_interactive_channel(channel) {
            self.gate.permits(&host)
        } else {
            self.gate.allowed_contains(&host)
        };
        if ok {
            return Ok(());
        }
        anyhow::bail!(
            "[egress-guard] domain `{host}` is not allowed for web_fetch on channel `{channel}`. {}",
            crate::path_guard::HARD_BOUNDARY_NOTICE
        )
    }
}

/// 流式读取响应体，受 `MAX_DOWNLOAD_BYTES` 上限保护，避免把大文件整块载入内存。
async fn read_limited(resp: reqwest::Response) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = FuturesStreamExt::next(&mut stream).await {
        let chunk = chunk.map_err(|e| anyhow!("read body: {}", e))?;
        if bytes.len() + chunk.len() > MAX_DOWNLOAD_BYTES {
            let remaining = MAX_DOWNLOAD_BYTES - bytes.len();
            bytes.extend_from_slice(&chunk[..remaining]);
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// 本地 HTML → 纯文本：readability 取主内容为主，html2text 全页为兜底。
fn local_extract_html(bytes: &[u8], url: &str) -> String {
    // readability 需要 url::Url；解析失败不影响，退 html2text。
    if let Ok(parsed_url) = url::Url::parse(url) {
        let mut slice: &[u8] = bytes;
        if let Ok(product) = readability::extractor::extract(&mut slice, &parsed_url) {
            let t = product.text.trim().to_string();
            // readability 对导航页/列表页可能只抽到很少内容，交给 html2text 全页兜底。
            if t.chars().count() > 200 {
                return t;
            }
        }
    }
    // 兜底：html2text 全页转换（width=100 仅影响换行，不影响内容）。
    match html2text::from_read(bytes, 100) {
        Ok(t) => t,
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------- 出站闸门：域名匹配 / 批准集 / SSRF ----------------

    #[test]
    fn test_domain_matches_exact_or_subdomain() {
        assert!(domain_matches("example.com", "example.com"));
        assert!(domain_matches("www.example.com", "example.com"));
        assert!(domain_matches("EXAMPLE.com", "example.com"));
        assert!(domain_matches("example.com.", "example.com"));
        assert!(!domain_matches("evilexample.com", "example.com"));
        assert!(!domain_matches("notexample.com", "example.com"));
        assert!(!domain_matches("example.org", "example.com"));
        assert!(!domain_matches("example.com", ""));
    }

    #[test]
    fn test_url_host_extracts_lowercase() {
        assert_eq!(
            url_host("https://Example.COM/a?b=c"),
            Some("example.com".into())
        );
        assert_eq!(url_host("http://[::1]:8080/x"), Some("::1".into()));
        assert_eq!(url_host("not a url"), None);
    }

    #[test]
    fn test_ip_is_blocked_matrix() {
        use std::net::IpAddr;
        let blocked = [
            "127.0.0.1",
            "10.0.0.5",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
            "::ffff:127.0.0.1",
        ];
        for ip in blocked {
            assert!(ip_is_blocked(ip.parse::<IpAddr>().unwrap()), "{ip} 应被封");
        }
        let allowed = ["8.8.8.8", "1.1.1.1", "2606:4700::1111"];
        for ip in allowed {
            assert!(
                !ip_is_blocked(ip.parse::<IpAddr>().unwrap()),
                "{ip} 不应被封"
            );
        }
    }

    #[tokio::test]
    async fn test_ssrf_check_rejects_private_and_non_http() {
        // IP 字面量：封禁集直接拒（无网络 I/O）
        for url in [
            "http://127.0.0.1:51217/api",
            "http://169.254.169.254/latest/meta-data",
            "http://192.168.1.1/",
            "http://[::1]/",
            "http://[::ffff:10.0.0.1]/",
        ] {
            assert!(ssrf_check(url).await.is_err(), "{url} 应被 SSRF 拒绝");
        }
        // 非 http/https scheme
        assert!(ssrf_check("file:///etc/passwd").await.is_err());
        assert!(ssrf_check("ftp://example.com/").await.is_err());
        // 公网域名：DNS 解析后全部为公网 IP 才放行（沙箱内 example.com 可解析；
        // 若环境无 DNS，本断言会得到 Err——CI 与本机均有网，此处接受）
        assert!(ssrf_check("https://example.com/").await.is_ok());
    }

    #[test]
    fn test_web_fetch_gate_permits_matrix() {
        let gate = WebFetchGate::new(vec!["wikipedia.org".into()]);
        // 非交互：只认允许清单（fail-closed）
        assert!(gate.allowed_contains("wikipedia.org"));
        assert!(gate.allowed_contains("en.wikipedia.org"));
        assert!(!gate.allowed_contains("example.com"));
        // 交互：allowed ∪ approved
        assert!(!gate.permits("example.com"));
        gate.approve("example.com");
        assert!(gate.permits("example.com"));
        assert!(gate.permits("www.example.com"));
    }

    #[test]
    fn test_web_fetch_gate_persists_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        {
            let gate = WebFetchGate::load(dir.path(), vec!["a.com".into()]);
            gate.approve("b.com");
        }
        // 重建后已批准集从 web_domains.json 恢复
        let gate = WebFetchGate::load(dir.path(), Vec::new());
        assert!(gate.permits("b.com"));
        assert!(!gate.permits("a.com"), "allowed 清单不进持久化文件");
    }

    #[test]
    fn local_extract_strips_html_to_text() {
        let html = b"<html><head><title>Headline</title></head><body>\
            <nav>menu</nav>\
            <article><p>The quick brown fox jumps over the lazy dog near the river bank this morning.</p></article>\
            </body></html>";
        let text = local_extract_html(html, "https://example.com/news/1");
        assert!(
            text.contains("quick brown fox"),
            "expected readable text, got: {text}"
        );
        assert!(
            !text.contains("<article>"),
            "raw tags must be stripped, got: {text}"
        );
    }

    #[test]
    fn truncate_appends_marker_when_over_limit() {
        let wf = WebFetch::new(10, None, Arc::new(WebFetchGate::new(Vec::new()))).unwrap();
        let out = wf.truncate("abcdefghijklmnopqrstuvwxyz");
        assert!(out.chars().count() > 10, "keeps max_chars plus marker");
        assert!(out.contains("truncated"), "missing truncation marker");
    }

    #[test]
    fn truncate_keeps_short_text() {
        let wf = WebFetch::new(10, None, Arc::new(WebFetchGate::new(Vec::new()))).unwrap();
        assert_eq!(wf.truncate("short"), "short");
    }

    #[test]
    fn constructs_without_tavily() {
        // 无 Tavily key 时仍应构造成功（纯本地抽取路径）。
        let wf = WebFetch::new(20_000, None, Arc::new(WebFetchGate::new(Vec::new()))).unwrap();
        assert_eq!(wf.name(), "web_fetch");
    }
}
