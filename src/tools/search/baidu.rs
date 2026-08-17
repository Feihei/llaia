//! 百度千帆 AI Search provider。
//!
//! 端点：`POST https://qianfan.baidubce.com/v2/ai_search/web_search`
//! 鉴权：请求头 `Authorization: Bearer <api_key>`（AppBuilder / 千帆 API Key）
//! 请求体：`messages` + `search_source=baidu_search_v2` + `resource_type_filter`
//! 响应：`references[]` 每项含 `title` / `url` / `snippet`（或 `content`）

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;

use super::{SearchProvider, SearchResult};

const BAIDU_URL: &str = "https://qianfan.baidubce.com/v2/ai_search/web_search";

pub struct BaiduProvider {
    client: reqwest::Client,
    api_key: String,
}

impl BaiduProvider {
    pub fn new(api_key: String) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder().build()?,
            api_key,
        })
    }
}

#[derive(Deserialize)]
struct BaiduResponse {
    references: Vec<BaiduRef>,
}

#[derive(Deserialize)]
struct BaiduRef {
    title: String,
    url: String,
    snippet: Option<String>,
    content: Option<String>,
}

#[async_trait]
impl SearchProvider for BaiduProvider {
    fn name(&self) -> &str {
        "baidu"
    }

    async fn search(&self, query: &str, top_k: usize) -> Result<Vec<SearchResult>> {
        if self.api_key.is_empty() {
            return Err(anyhow!("baidu api_key not configured"));
        }
        let body = serde_json::json!({
            "messages": [ { "role": "user", "content": query } ],
            "search_source": "baidu_search_v2",
            "resource_type_filter": [ { "type": "web", "top_k": top_k } ]
        });
        let resp = self
            .client
            .post(BAIDU_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("baidu request: {}", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("baidu {}: {}", status, text));
        }
        let parsed: BaiduResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("baidu parse: {}", e))?;

        Ok(parsed
            .references
            .into_iter()
            .take(top_k)
            .map(|r| SearchResult {
                title: r.title,
                url: r.url,
                snippet: r.snippet.or(r.content).unwrap_or_default(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_with_api_key() {
        let p = BaiduProvider::new("test-key".into()).unwrap();
        assert_eq!(p.name(), "baidu");
    }
}
