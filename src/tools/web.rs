use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;

pub struct WebFetch {
    client: reqwest::Client,
}

impl WebFetch {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder().build()?,
        })
    }
}

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &str {
        "web_fetch"
    }
    fn description(&self) -> &str {
        "Fetch the content of a web page (HTML)."
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
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| anyhow!("fetch {}: {}", url, e))?;
        if !resp.status().is_success() {
            return Err(anyhow!("HTTP {}", resp.status()));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| anyhow!("read body: {}", e))?;
        Ok(text)
    }
}
