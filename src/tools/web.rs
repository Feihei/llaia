use crate::tools::search::tavily::TavilyProvider;
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures_util::StreamExt as FuturesStreamExt;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// 下载字节硬上限：避免把整站大文件/视频灌进内存。抽取后还会按 `max_chars` 二次截断文本。
const MAX_DOWNLOAD_BYTES: usize = 4 * 1024 * 1024;

pub struct WebFetch {
    client: reqwest::Client,
    max_chars: usize,
    /// 可选 Tavily 服务端抽取器（复用 `[tools.tavily].api_key`）。
    /// `None` 时（未启用或 key 为空）走本地 readability / html2text 抽取。
    tavily: Option<Arc<TavilyProvider>>,
}

impl WebFetch {
    pub fn new(max_chars: usize, tavily: Option<Arc<TavilyProvider>>) -> Result<Self> {
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
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'url'"))?;

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
        let wf = WebFetch::new(10, None).unwrap();
        let out = wf.truncate("abcdefghijklmnopqrstuvwxyz");
        assert!(out.chars().count() > 10, "keeps max_chars plus marker");
        assert!(out.contains("truncated"), "missing truncation marker");
    }

    #[test]
    fn truncate_keeps_short_text() {
        let wf = WebFetch::new(10, None).unwrap();
        assert_eq!(wf.truncate("short"), "short");
    }

    #[test]
    fn constructs_without_tavily() {
        // 无 Tavily key 时仍应构造成功（纯本地抽取路径）。
        let wf = WebFetch::new(20_000, None).unwrap();
        assert_eq!(wf.name(), "web_fetch");
    }
}
