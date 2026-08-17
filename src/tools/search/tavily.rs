//! Tavily provider（从原 `src/tools/tavily.rs` 迁移为 `SearchProvider` 实现）。

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;

use super::{SearchProvider, SearchResult};

const TAVILY_URL: &str = "https://api.tavily.com/search";

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
