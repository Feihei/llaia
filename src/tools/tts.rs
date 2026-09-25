//! TTS 工具（P5 T1）：调用 OpenAI 兼容 `/audio/speech` 端点合成语音。
//!
//! - 合成（`tts` 工具）与发送（复用 `send_file` / MediaOutput）分离，对齐 `send_image` 模式
//! - v1 用 OpenAI TTS API（可测、稳定）；edge-tts（WS + Sec-MS-GEC 签名、不可测且接口
//!   脆弱）记 v2 待研究（见 `docs/plans/2026-08-17-p5-remaining.md` §T1 决策修订）
//! - 产物落 `workspace/tts/<uuid>.mp3`（默认），路径经 `resolve_within` 校验防越权

use crate::config::{Config, ModelKind, TtsConfig};
use crate::tools::file::resolve_within;
use crate::tools::Tool;
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// 单次合成文本上限（OpenAI TTS 限制 4096 字符）。
const MAX_TEXT_CHARS: usize = 4096;

pub struct TtsTool {
    base_url: String,
    api_key: String,
    model: String,
    voice: String,
    workspace: PathBuf,
}

impl TtsTool {
    /// 按 `[tools.tts]` + 模型目录构建（P8）：`model` 引用 kind=tts 条目，
    /// 端点/key 来自条目的 provider，服务端模型名来自条目的 `model` 字段。
    /// `enabled` 关闭或引用无效时返回 None（不注册）。本地 TTS 后端 provider
    /// 可以不配 key（api_key 为空时不发 Authorization 头）。
    pub fn build(
        cfg: &TtsConfig,
        config: &Config,
        workspace: PathBuf,
    ) -> Result<Option<Arc<dyn Tool>>> {
        if !cfg.enabled {
            return Ok(None);
        }
        let ref_id = cfg.model.trim();
        if ref_id.is_empty() {
            tracing::warn!(
                "tools.tts.enabled but tools.tts.model is empty; tts tool not registered"
            );
            return Ok(None);
        }
        let Some(entry) = config.models.get(ref_id) else {
            tracing::warn!(
                model = ref_id,
                "tools.tts.model references an unknown model entry; tts tool not registered"
            );
            return Ok(None);
        };
        if entry.kind != ModelKind::Tts {
            tracing::warn!(
                model = ref_id,
                kind = entry.kind.as_str(),
                "tools.tts.model must reference a kind = \"tts\" entry; tts tool not registered"
            );
            return Ok(None);
        }
        let Some(prov) = config.provider.get(&entry.provider) else {
            tracing::warn!(
                model = ref_id,
                provider = %entry.provider,
                "tts model references a missing provider; tts tool not registered"
            );
            return Ok(None);
        };
        Ok(Some(Arc::new(TtsTool {
            base_url: prov.base_url.trim_end_matches('/').to_string(),
            api_key: prov.api_key.clone(),
            model: entry.model.clone(),
            voice: cfg.voice.clone(),
            workspace,
        })))
    }

    /// 合成到 `workspace/tts/<uuid>.mp3`（纯逻辑，供单测走 mock HTTP）。
    async fn synthesize(&self, text: &str, voice: &str) -> Result<(PathBuf, Vec<u8>)> {
        let rel = format!("tts/{}.mp3", uuid::Uuid::new_v4());
        let out_path = resolve_within(&self.workspace, &rel)?;
        if let Some(parent) = out_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let url = format!("{}/audio/speech", self.base_url);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        let mut req = client.post(&url).json(&json!({
            "model": self.model,
            "input": text,
            "voice": voice,
            "response_format": "mp3",
        }));
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| anyhow!("TTS request failed: {e}"))?;
        if !resp.status().is_success() {
            bail!("TTS endpoint returned HTTP {}", resp.status());
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| anyhow!("read response: {e}"))?;
        if bytes.is_empty() {
            bail!("empty audio response from TTS endpoint");
        }
        Ok((out_path, bytes.to_vec()))
    }
}

#[async_trait]
impl Tool for TtsTool {
    fn name(&self) -> &str {
        "tts"
    }

    fn description(&self) -> &str {
        "Synthesize text to speech using the configured TTS provider (OpenAI-compatible /audio/speech). Returns the path to the generated MP3 file in the workspace; use send_file to deliver it to the user."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "Text to synthesize (max 4096 chars)." },
                "voice": {
                    "type": "string",
                    "description": "Optional voice override (e.g. alloy, echo, fable, onyx, nova, shimmer)."
                }
            },
            "required": ["text"]
        })
    }

    /// 只写 workspace 内文件 → 免确认。
    fn requires_confirm(&self) -> bool {
        false
    }

    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'text' argument"))?
            .trim();
        if text.is_empty() {
            bail!("text must not be empty");
        }
        if text.chars().count() > MAX_TEXT_CHARS {
            bail!(
                "text too long ({} chars, max {})",
                text.chars().count(),
                MAX_TEXT_CHARS
            );
        }
        let voice = args
            .get("voice")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.voice);
        let (path, bytes) = self.synthesize(text, voice).await?;
        tokio::fs::write(&path, &bytes).await?;
        Ok(format!("audio synthesized: {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ModelEntry, ProviderConfig};

    /// 组装一份带 tts 目录条目的最小 Config
    fn config() -> Config {
        let mut config = Config::default_for_workspace("~/.llaia");
        config.provider.insert(
            "ttsprov".into(),
            ProviderConfig {
                provider_type: "openai_compatible".into(),
                base_url: "http://localhost:9/v1".into(),
                api_key: "sk-test".into(),
                compat: None,
            },
        );
        config.models.insert(
            "tts1".into(),
            ModelEntry {
                provider: "ttsprov".into(),
                model: "tts-1".into(),
                kind: ModelKind::Tts,
                enabled: true,
                context_size: None,
                max_tokens: None,
                native_tool_calling: None,
                thinking: None,
                size: None,
                capabilities: Vec::new(),
            },
        );
        config
    }

    fn cfg() -> TtsConfig {
        TtsConfig {
            enabled: true,
            model: "tts1".into(),
            voice: "alloy".into(),
        }
    }

    #[test]
    fn build_returns_none_when_disabled_or_ref_unusable() {
        let config = config();
        let mut c = cfg();
        c.enabled = false;
        assert!(TtsTool::build(&c, &config, PathBuf::from("."))
            .unwrap()
            .is_none());
        // 引用未知条目
        let c = TtsConfig {
            enabled: true,
            model: "nope".into(),
            voice: "alloy".into(),
        };
        assert!(TtsTool::build(&c, &config, PathBuf::from("."))
            .unwrap()
            .is_none());
        // 引用非 tts 条目（default_for_workspace 的 qwen 是 chat）
        let c = TtsConfig {
            enabled: true,
            model: "qwen".into(),
            voice: "alloy".into(),
        };
        assert!(TtsTool::build(&c, &config, PathBuf::from("."))
            .unwrap()
            .is_none());
    }

    #[test]
    fn build_returns_tool_when_enabled() {
        let tool = TtsTool::build(&cfg(), &config(), PathBuf::from("."))
            .unwrap()
            .unwrap();
        assert_eq!(tool.name(), "tts");
    }

    #[tokio::test]
    async fn execute_requires_text() {
        let tool = TtsTool::build(&cfg(), &config(), PathBuf::from("."))
            .unwrap()
            .unwrap();
        let err = tool.execute(&json!({}), "cli").await.unwrap_err();
        assert!(err.to_string().contains("missing 'text'"));
    }

    #[tokio::test]
    async fn execute_rejects_empty_and_long_text() {
        let tool = TtsTool::build(&cfg(), &config(), PathBuf::from("."))
            .unwrap()
            .unwrap();
        let err = tool
            .execute(&json!({ "text": "   " }), "cli")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("empty"));
        let long = "x".repeat(MAX_TEXT_CHARS + 1);
        let err = tool
            .execute(&json!({ "text": long }), "cli")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too long"));
    }

    #[test]
    fn schema_exposes_text_and_voice() {
        let tool = TtsTool::build(&cfg(), &config(), PathBuf::from("."))
            .unwrap()
            .unwrap();
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["text"].is_object());
        assert!(schema["properties"]["voice"].is_object());
        assert_eq!(schema["required"][0], "text");
    }
}
