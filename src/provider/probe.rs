//! 模型探测（P5 W2）：向 OpenAI 兼容端点 `GET /models` 拉取可用模型列表。
//!
//! 供 WebUI "Probe models" 按钮使用：探测成功后前端勾选生成 model 条目，
//! 走既有 `PUT /api/config` 保存（不新增保存路径）。
//! v1 仅支持 OpenAI 兼容端点（Ollama / Llama.cpp / LM Studio / OpenRouter / doubao / 百度等）；
//! Anthropic 无公开 models 列表端点、Gemini 留待 v2，均不做探测。

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::time::Duration;

/// 探测到的模型条目。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ProbableModel {
    pub id: String,
    /// 展示名（缺失时回退到 id）。
    pub name: String,
}

/// 探测 OpenAI 兼容端点 `/models`。
///
/// - `base_url`：端点地址（`http://host:port/v1` 或裸 host，均拼接 `/models`）
/// - `api_key`：可选；本地端点（Ollama/LM Studio）通常为空，空串不发 auth
///
/// 网络/HTTP/解析错误返回 Err；成功返回归一化模型列表（可能为空）。
pub async fn probe_openai_compatible(
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<ProbableModel>> {
    let base = base_url.trim_end_matches('/');
    let url = format!("{}/models", base);
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()?;

    let mut req = client.get(&url);
    if let Some(k) = api_key {
        if !k.is_empty() {
            req = req.bearer_auth(k);
        }
    }

    let resp = req
        .send()
        .await
        .map_err(|e| anyhow!("request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("models endpoint returned HTTP {}", resp.status()));
    }
    let json: Value = resp
        .json()
        .await
        .map_err(|e| anyhow!("bad response: {e}"))?;
    Ok(parse_openai_models(&json))
}

/// 解析 OpenAI 兼容 `/models` 响应（`{ data: [{ id, ... }] }`）。
/// 独立成纯函数便于单测（不依赖网络）。
fn parse_openai_models(json: &Value) -> Vec<ProbableModel> {
    let mut out = Vec::new();
    if let Some(arr) = json.get("data").and_then(|d| d.as_array()) {
        for m in arr {
            let Some(id) = m.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            let name = m
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(id)
                .to_string();
            out.push(ProbableModel {
                id: id.to_string(),
                name,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_openai_data_array() {
        let json = json!({
            "object": "list",
            "data": [
                {"id": "qwen3:14b", "object": "model", "created": 1, "owned_by": "local"},
                {"id": "llama3.1:8b", "object": "model"},
                {"id": "embedding-x", "object": "model", "name": "Embedding X"}
            ]
        });
        let models = parse_openai_models(&json);
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "qwen3:14b");
        assert_eq!(models[0].name, "qwen3:14b"); // 无 name → 回退 id
        assert_eq!(models[1].id, "llama3.1:8b");
        assert_eq!(models[2].name, "Embedding X"); // 有 name → 用之
    }

    #[test]
    fn parses_empty_or_missing_data() {
        assert_eq!(parse_openai_models(&json!({})), vec![]);
        assert_eq!(parse_openai_models(&json!({"data": []})), vec![]);
        assert_eq!(parse_openai_models(&json!({"data": "not-array"})), vec![]);
    }

    #[test]
    fn skips_entries_without_id() {
        let json = json!({
            "data": [
                {"name": "no-id"},
                {"id": "ok-model"}
            ]
        });
        let models = parse_openai_models(&json);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "ok-model");
    }
}
