//! 统一搜索抽象（ADR-0023）。
//!
//! 对外只暴露一个 `search` 工具；内部按 `[tools.search].provider` 选定**单一**
//! provider 执行，不串试、不聚合。各搜索源实现 [`SearchProvider`]，把自家响应
//! 归一化成 [`SearchResult`]。

use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;

use crate::config::{Config, ToolsConfig};
use crate::tools::Tool;

pub mod baidu;
pub mod brave;
pub mod tavily;

/// 归一化后的单条搜索结果。
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// 搜索 provider 抽象：各内置搜索源实现此 trait，把自家响应归一化成 `SearchResult`。
/// wire 协议各家各异（GET/POST、header、参数名），全部封在实现里——对配置与
/// 调用方只暴露统一的 query/top_k → hits 契约（P8 前提 8：新增一家 = adapter +
/// type 枚举项，schema/WebUI 零改动）。
#[async_trait]
pub trait SearchProvider: Send + Sync {
    /// provider 标识（tavily / baidu / brave …），用于日志。
    fn name(&self) -> &str;
    /// 执行一次搜索，返回至多 `top_k` 条归一化结果。
    async fn search(&self, query: &str, top_k: usize) -> Result<Vec<SearchResult>>;
    /// 是否支持服务端正文抽取（`web_fetch.extract_provider` 的引用校验依据）。
    /// v1 仅 tavily（`/extract`）；jina reader / tinyfish Fetch 接入时改写此项。
    fn supports_extract(&self) -> bool {
        false
    }
}

/// 统一 `search` 工具：持有单一 provider，对 agent 只暴露 `query` + 可选 `top_k`。
pub struct UnifiedSearch {
    provider: Arc<dyn SearchProvider>,
    default_top_k: usize,
}

impl UnifiedSearch {
    pub fn new(provider: Arc<dyn SearchProvider>, default_top_k: usize) -> Self {
        Self {
            provider,
            default_top_k,
        }
    }

    /// 按 `[tools.search].provider`（service family provider 的 **id 引用**）解析
    /// adapter；引用为空/未知/指向 llm family/凭据为空 → `None`（不注册 `search`
    /// 工具，与老 tavily `if !api_key.is_empty()` 行为一致）。
    pub fn build(tools: &ToolsConfig, config: &Config) -> Result<Option<Arc<dyn Tool>>> {
        let search_cfg = &tools.search;
        let id = search_cfg.provider.trim();
        if id.is_empty() {
            return Ok(None);
        }
        let Some(prov) = config.provider.get(id) else {
            tracing::warn!(
                provider = id,
                "tools.search.provider references an unknown provider; search tool not registered"
            );
            return Ok(None);
        };
        if !prov.is_service() {
            tracing::warn!(
                provider = id,
                "tools.search.provider must reference a search service provider (type = \"search\"); search tool not registered"
            );
            return Ok(None);
        }
        let api_key = prov.api_key.clone();
        // 平台判别：type=search 统一入口，platform（load 期校验必填且合法）选 adapter。
        // 各家 wire 协议不同（tavily key-in-body / baidu Bearer+chat-style / brave GET+专用头），
        // 端点 URL 是编译期常量，配置不消费。
        let provider: Option<Arc<dyn SearchProvider>> = match prov.search_platform() {
            Some("tavily") if !api_key.is_empty() => {
                Some(Arc::new(tavily::TavilyProvider::new(api_key)?))
            }
            Some("baidu") if !api_key.is_empty() => {
                Some(Arc::new(baidu::BaiduProvider::new(api_key)?))
            }
            Some("brave") if !api_key.is_empty() => {
                Some(Arc::new(brave::BraveProvider::new(api_key)?))
            }
            platform => {
                if api_key.is_empty() {
                    tracing::warn!(
                        provider = id,
                        "search provider api_key is empty; search tool not registered"
                    );
                } else {
                    tracing::warn!(
                        provider = id,
                        ?platform,
                        "unknown or unimplemented search platform; search tool not registered"
                    );
                }
                None
            }
        };
        Ok(provider.map(|p| Arc::new(UnifiedSearch::new(p, search_cfg.top_k)) as Arc<dyn Tool>))
    }
}

#[async_trait]
impl Tool for UnifiedSearch {
    fn name(&self) -> &str {
        "search"
    }
    fn description(&self) -> &str {
        "Search the web. Returns a numbered list of results with title, URL and a content snippet. Use this to find current information, documentation, or sources; open specific URLs with web_fetch for full content."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "The search query." },
                "top_k": {
                    "type": "integer",
                    "default": self.default_top_k,
                    "description": "Max number of results to return."
                }
            },
            "required": ["query"]
        })
    }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'query'"))?;
        let top_k = args
            .get("top_k")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(self.default_top_k);

        let results = self.provider.search(query, top_k).await?;
        if results.is_empty() {
            return Ok("(no results)".to_string());
        }
        let mut out = String::new();
        for (i, r) in results.iter().enumerate() {
            out.push_str(&format!(
                "{}. {}\n   URL: {}\n   {}\n\n",
                i + 1,
                r.title,
                r.url,
                r.snippet
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProvider {
        results: Vec<SearchResult>,
    }

    #[async_trait]
    impl SearchProvider for FakeProvider {
        fn name(&self) -> &str {
            "fake"
        }
        async fn search(&self, _query: &str, top_k: usize) -> Result<Vec<SearchResult>> {
            Ok(self.results.iter().take(top_k).cloned().collect())
        }
    }

    #[tokio::test]
    async fn formats_results_with_title_url_snippet() {
        let provider = Arc::new(FakeProvider {
            results: vec![
                SearchResult {
                    title: "Rust".into(),
                    url: "https://rust-lang.org".into(),
                    snippet: "A language.".into(),
                },
                SearchResult {
                    title: "Docs".into(),
                    url: "https://doc.rust-lang.org".into(),
                    snippet: "Reference.".into(),
                },
            ],
        });
        let tool = UnifiedSearch::new(provider, 8);
        let out = tool
            .execute(&serde_json::json!({ "query": "rust" }), "cli")
            .await
            .unwrap();
        assert!(out.contains("1. Rust"));
        assert!(out.contains("URL: https://rust-lang.org"));
        assert!(out.contains("A language."));
        assert!(out.contains("2. Docs"));
    }

    #[tokio::test]
    async fn respects_top_k_override() {
        let provider = Arc::new(FakeProvider {
            results: (0..5)
                .map(|i| SearchResult {
                    title: format!("r{i}"),
                    url: format!("https://e{i}.com"),
                    snippet: "s".into(),
                })
                .collect(),
        });
        let tool = UnifiedSearch::new(provider, 8);
        let out = tool
            .execute(&serde_json::json!({ "query": "x", "top_k": 2 }), "cli")
            .await
            .unwrap();
        assert!(out.contains("1. r0"));
        assert!(out.contains("2. r1"));
        assert!(!out.contains("3. r2"));
    }

    #[tokio::test]
    async fn empty_results_yield_placeholder() {
        let provider = Arc::new(FakeProvider { results: vec![] });
        let tool = UnifiedSearch::new(provider, 8);
        let out = tool
            .execute(&serde_json::json!({ "query": "x" }), "cli")
            .await
            .unwrap();
        assert_eq!(out, "(no results)");
    }

    #[test]
    fn schema_exposes_query_and_top_k() {
        let provider = Arc::new(FakeProvider { results: vec![] });
        let tool = UnifiedSearch::new(provider, 8);
        let schema = tool.parameters_schema();
        assert_eq!(schema["properties"]["query"]["type"], "string");
        assert_eq!(schema["properties"]["top_k"]["type"], "integer");
        assert_eq!(schema["required"].as_array().unwrap()[0], "query");
    }
}
