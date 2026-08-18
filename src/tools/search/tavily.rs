//! Tavily provider（从原 `src/tools/tavily.rs` 迁移为 `SearchProvider` 实现）。

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;

use super::{SearchProvider, SearchResult};

const TAVILY_URL: &str = "https://api.tavily.com/search";
/// Tavily 正文抽取端点（对应 AstrBot 的 `tavily_extract_web_page`）：
/// 服务端抓取并清洗成纯文本，对反爬 / JS 渲染页成功率远高于本地解析。
const TAVILY_EXTRACT_URL: &str = "https://api.tavily.com/extract";

pub struct TavilyProvider {
    client: reqwest::Client,
    api_key: String,
}

impl TavilyProvider {
    pub fn new(api_key: String) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder().build()?,
            api_key,
        })
    }

    /// 服务端抽取单个 URL 的正文（Tavily `/extract`）。成功返回清洗后的纯文本；
    /// 失败（网络/鉴权/该页抽取失败）返回 Err，由调用方退化为本地解析。
    pub async fn extract(&self, url: &str) -> Result<String> {
        if self.api_key.is_empty() {
            return Err(anyhow!("tavily api_key not configured"));
        }
        let body = serde_json::json!({
            "api_key": self.api_key,
            "urls": [url],
            "extract_depth": "basic",
        });
        let resp = self
            .client
            .post(TAVILY_EXTRACT_URL)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("tavily extract request: {}", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("tavily extract {}: {}", status, text));
        }
        let parsed: TavilyExtractResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("tavily extract parse: {}", e))?;
        if let Some(r) = parsed.results.into_iter().next() {
            if !r.raw_content.trim().is_empty() {
                return Ok(r.raw_content);
            }
        }
        Err(anyhow!("tavily extract returned no content for {}", url))
    }
}

#[derive(Deserialize)]
struct TavilyResponse {
    results: Vec<TavilyResult>,
}

#[derive(Deserialize)]
struct TavilyResult {
    title: String,
    url: String,
    content: String,
}

#[derive(Deserialize)]
struct TavilyExtractResponse {
    results: Vec<TavilyExtractResult>,
    #[serde(default)]
    #[allow(dead_code)]
    failed_results: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct TavilyExtractResult {
    #[allow(dead_code)]
    url: String,
    raw_content: String,
}

#[async_trait]
impl SearchProvider for TavilyProvider {
    fn name(&self) -> &str {
        "tavily"
    }

    async fn search(&self, query: &str, top_k: usize) -> Result<Vec<SearchResult>> {
        if self.api_key.is_empty() {
            return Err(anyhow!("tavily api_key not configured"));
        }
        let body = serde_json::json!({
            "api_key": self.api_key,
            "query": query,
            "max_results": top_k,
        });
        let resp = self
            .client
            .post(TAVILY_URL)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("tavily request: {}", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("tavily {}: {}", status, text));
        }
        let parsed: TavilyResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("tavily parse: {}", e))?;

        Ok(parsed
            .results
            .into_iter()
            .map(|r| SearchResult {
                title: r.title,
                url: r.url,
                snippet: r.content,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_with_api_key() {
        // 不发起网络请求，仅验证构造成功
        let p = TavilyProvider::new("tvly-test".into()).unwrap();
        assert_eq!(p.name(), "tavily");
    }
}
