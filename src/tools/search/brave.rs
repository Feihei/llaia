//! Brave Search provider。
//!
//! 端点：`GET https://api.search.brave.com/res/v1/web/search`
//! 鉴权：请求头 `X-Subscription-Token: <api_key>`
//! 响应：`web.results[]` 每项含 `title` / `url` / `description`

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;

use super::{SearchProvider, SearchResult};

const BRAVE_URL: &str = "https://api.search.brave.com/res/v1/web/search";

pub struct BraveProvider {
    client: reqwest::Client,
    api_key: String,
}

impl BraveProvider {
    pub fn new(api_key: String) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder().build()?,
            api_key,
        })
    }
}

#[derive(Deserialize)]
struct BraveResponse {
    web: BraveWeb,
}

#[derive(Deserialize)]
struct BraveWeb {
    results: Vec<BraveResult>,
}

#[derive(Deserialize)]
struct BraveResult {
    title: String,
    url: String,
    description: String,
}

#[async_trait]
impl SearchProvider for BraveProvider {
    fn name(&self) -> &str {
        "brave"
    }

    async fn search(&self, query: &str, top_k: usize) -> Result<Vec<SearchResult>> {
        if self.api_key.is_empty() {
            return Err(anyhow!("brave api_key not configured"));
        }
        let count = top_k.to_string();
        let resp = self
            .client
            .get(BRAVE_URL)
            .header("X-Subscription-Token", &self.api_key)
            .header("Accept", "application/json")
            .query(&[("q", query), ("count", count.as_str())])
            .send()
            .await
            .map_err(|e| anyhow!("brave request: {}", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("brave {}: {}", status, text));
        }
        let parsed: BraveResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("brave parse: {}", e))?;

        Ok(parsed
            .web
            .results
            .into_iter()
            .take(top_k)
            .map(|r| SearchResult {
                title: r.title,
                url: r.url,
                snippet: r.description,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_with_api_key() {
        let p = BraveProvider::new("test-token".into()).unwrap();
        assert_eq!(p.name(), "brave");
    }
}
