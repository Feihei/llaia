use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

const TAVILY_URL: &str = "https://api.tavily.com/search";

pub struct TavilySearch {
    client: reqwest::Client,
    api_key: String,
}

impl TavilySearch {
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
impl Tool for TavilySearch {
    fn name(&self) -> &str {
        "tavily_search"
    }
    fn description(&self) -> &str {
        "Search the web via Tavily."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "max_results": { "type": "integer", "default": 5 }
            },
            "required": ["query"]
        })
    }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        if self.api_key.is_empty() {
            return Err(anyhow!("tavily api_key not configured"));
        }
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'query'"))?;
        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(5);

        let body = serde_json::json!({
            "api_key": self.api_key,
            "query": query,
            "max_results": max_results,
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

        let mut out = String::new();
        for (i, r) in parsed.results.iter().enumerate() {
            out.push_str(&format!(
                "{}. {}\n   URL: {}\n   {}\n\n",
                i + 1,
                r.title,
                r.url,
                r.content
            ));
        }
        Ok(out)
    }
}
