//! TTS 工具（P5 T1）：调用 OpenAI 兼容 `/audio/speech` 端点合成语音。
//!
//! - 合成（`tts` 工具）与发送（复用 `send_file` / MediaOutput）分离，对齐 `send_image` 模式
//! - v1 用 OpenAI TTS API（可测、稳定）；edge-tts（WS + Sec-MS-GEC 签名、不可测且接口
//!   脆弱）记 v2 待研究（见 `docs/plans/2026-08-17-p5-remaining.md` §T1 决策修订）
//! - 产物落 `workspace/tts/<uuid>.mp3`（默认），路径经 `resolve_within` 校验防越权

use crate::config::TtsConfig;
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
    /// 按 `[tools.tts]` 构建；`enabled` 或 `api_key` 缺失时返回 None（不注册）。
    pub fn build(cfg: &TtsConfig, workspace: PathBuf) -> Result<Option<Arc<dyn Tool>>> {
        if !cfg.enabled || cfg.api_key.is_empty() {
            return Ok(None);
        }
        Ok(Some(Arc::new(TtsTool {
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            api_key: cfg.api_key.clone(),
            model: cfg.model.clone(),
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
        let resp = client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&json!({
                "model": self.model,
                "input": text,
                "voice": voice,
                "response_format": "mp3",
            }))
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

    fn cfg() -> TtsConfig {
        TtsConfig {
            enabled: true,
            base_url: "http://localhost:9/v1".into(),
            api_key: "sk-test".into(),
            model: "tts-1".into(),
            voice: "alloy".into(),
        }
    }

    #[test]
    fn build_returns_none_when_disabled_or_no_key() {
        let mut c = cfg();
        c.enabled = false;
        assert!(TtsTool::build(&c, PathBuf::from(".")).unwrap().is_none());
        let mut c = cfg();
        c.api_key = String::new();
        assert!(TtsTool::build(&c, PathBuf::from(".")).unwrap().is_none());
    }

    #[test]
    fn build_returns_tool_when_enabled() {
        let tool = TtsTool::build(&cfg(), PathBuf::from(".")).unwrap().unwrap();
        assert_eq!(tool.name(), "tts");
    }

    #[tokio::test]
    async fn execute_requires_text() {
        let tool = TtsTool::build(&cfg(), PathBuf::from(".")).unwrap().unwrap();
        let err = tool.execute(&json!({}), "cli").await.unwrap_err();
        assert!(err.to_string().contains("missing 'text'"));
    }

    #[tokio::test]
    async fn execute_rejects_empty_and_long_text() {
        let tool = TtsTool::build(&cfg(), PathBuf::from(".")).unwrap().unwrap();
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
        let tool = TtsTool::build(&cfg(), PathBuf::from(".")).unwrap().unwrap();
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["text"].is_object());
        assert!(schema["properties"]["voice"].is_object());
        assert_eq!(schema["required"][0], "text");
    }
}
