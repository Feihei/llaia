//! TinyFish provider（搜索 + 服务端正文抽取）。
//!
//! TinyFish 是面向 agent 的 web 基础设施，llaia 消费其中两个 API 面：
//!
//! - **TinySearch**：`GET https://api.search.tinyfish.ai`
//!   鉴权请求头 `X-API-Key`；响应 `{ query, results, total_results, page }`，
//!   每项含 `position` / `site_name` / `title` / `snippet` / `url`。
//!   官方延迟 1–3s，免费额度 12000 次/天。
//! - **TinyFetch**：`POST https://api.fetch.tinyfish.ai`
//!   请求体 `{ urls, format }`（单次最多 10 个 URL）；响应
//!   `{ results: [{ url, final_url, title, format, text }], errors: [{ url, error }] }`。
//!   免费额度 1000 URL/天；后端单 URL 超时 110s、CDN 上限 120s。
//!
//! 端点 URL 是编译期常量（与 tavily/baidu/brave 同口径），配置只提供 api_key。
//! 与 tavily 的差异：TinyFish 是 **header 鉴权**（tavily 把 key 塞 body），且
//! Fetch 返回 markdown 正文 —— 正好喂 `web_fetch` 的截断逻辑。

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Duration;

use super::{Extractor, SearchProvider, SearchResult};

const TINYFISH_SEARCH_URL: &str = "https://api.search.tinyfish.ai";
const TINYFISH_FETCH_URL: &str = "https://api.fetch.tinyfish.ai";

/// 搜索请求的 per-request 超时：官方建议 10s，留余量到 15s。
/// 覆盖 client 默认值（后者按 Fetch 慢路径设，见 [`TinyFishProvider::new`]）。
const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);

pub struct TinyFishProvider {
    client: reqwest::Client,
    api_key: String,
}

impl TinyFishProvider {
    pub fn new(api_key: String) -> Result<Self> {
        Ok(Self {
            // Fetch 后端单 URL 超时 110s、CDN 上限 120s，故 client 默认值取 150s
            // （与官方 SDK 建议的客户端超时一致）；搜索路径用 per-request timeout
            // 压回 15s，避免慢查询把回合拖满。
            client: crate::http::client_builder()
                .timeout(Duration::from_secs(150))
                .connect_timeout(Duration::from_secs(10))
                .build()?,
            api_key,
        })
    }
}

#[derive(Deserialize)]
struct TinyFishSearchResponse {
    results: Vec<TinyFishSearchResult>,
}

#[derive(Deserialize)]
struct TinyFishSearchResult {
    title: String,
    url: String,
    #[serde(default)]
    snippet: String,
}

#[derive(Deserialize)]
struct TinyFishFetchResponse {
    #[serde(default)]
    results: Vec<TinyFishFetchResult>,
    #[serde(default)]
    errors: Vec<TinyFishFetchError>,
}

#[derive(Deserialize)]
struct TinyFishFetchResult {
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct TinyFishFetchError {
    #[serde(default)]
    url: String,
    #[serde(default)]
    error: String,
}

#[async_trait]
impl SearchProvider for TinyFishProvider {
    fn name(&self) -> &str {
        "tinyfish"
    }

    async fn search(&self, query: &str, top_k: usize) -> Result<Vec<SearchResult>> {
        if self.api_key.is_empty() {
            return Err(anyhow!("tinyfish api_key not configured"));
        }
        let resp = self
            .client
            .get(TINYFISH_SEARCH_URL)
            .header("X-API-Key", &self.api_key)
            .header("Accept", "application/json")
            .query(&[("query", query)])
            .timeout(SEARCH_TIMEOUT)
            .send()
            .await
            .map_err(|e| anyhow!("tinyfish search request: {}", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("tinyfish search {}: {}", status, text));
        }
        let parsed: TinyFishSearchResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("tinyfish search parse: {}", e))?;

        // Search API 无 limit 参数（试过 recency/domain_type 一族，都是过滤而非截断），
        // 因此按 top_k 在客户端截断——语义与 brave 的 `.take(top_k)` 一致。
        Ok(parsed
            .results
            .into_iter()
            .take(top_k)
            .map(|r| SearchResult {
                title: r.title,
                url: r.url,
                snippet: r.snippet,
            })
            .collect())
    }
}

#[async_trait]
impl Extractor for TinyFishProvider {
    fn extractor_name(&self) -> &str {
        "tinyfish"
    }

    /// 服务端抽取：TinyFetch 渲染（必要时执行 JS）后返回 markdown。
    /// 单 URL 走 `urls` 数组（该接口支持批量，抽取路径刻意每次只要一个，
    /// 让 `web_fetch` 的失败退化粒度保持在单页级）。
    async fn extract(&self, url: &str) -> Result<String> {
        if self.api_key.is_empty() {
            return Err(anyhow!("tinyfish api_key not configured"));
        }
        let body = serde_json::json!({
            "urls": [url],
            "format": "markdown",
        });
        let resp = self
            .client
            .post(TINYFISH_FETCH_URL)
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("tinyfish fetch request: {}", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("tinyfish fetch {}: {}", status, text));
        }
        let parsed: TinyFishFetchResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("tinyfish fetch parse: {}", e))?;

        // per-URL 失败以 200 + errors[] 返回（不拖垮整个请求），先取正文再报错。
        if let Some(r) = parsed
            .results
            .into_iter()
            .find(|r| !r.text.trim().is_empty())
        {
            return Ok(r.text);
        }
        if let Some(e) = parsed.errors.into_iter().next() {
            return Err(anyhow!("tinyfish fetch failed for {}: {}", e.url, e.error));
        }
        Err(anyhow!("tinyfish fetch returned no content for {}", url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_with_api_key() {
        // 不发起网络请求，仅验证构造成功
        let p = TinyFishProvider::new("sk-tinyfish-test".into()).unwrap();
        assert_eq!(SearchProvider::name(&p), "tinyfish");
        assert_eq!(p.extractor_name(), "tinyfish");
    }

    #[test]
    fn both_capabilities_declared() {
        // 同一 struct 同时承担 search 与 extract —— 这是 tinyfish 相对 tavily 的
        // 配置优势：一个 provider 条目即可同时喂 [tools.search] 和 extract_provider。
        let p = TinyFishProvider::new("k".into()).unwrap();
        let search: &dyn SearchProvider = &p;
        let extractor: &dyn Extractor = &p;
        assert_eq!(search.name(), extractor.extractor_name());
    }
}
