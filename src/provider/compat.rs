//! Provider 兼容层（Compat）：针对 OpenAI 兼容端点（Ollama / Llama.cpp 等）的行为差异做归一。
//!
//! 设计见 `docs/adr/0026-provider-compat.md`。
//!
//! 核心约束：`Compat::default()` 必须等于 `OpenAiCompatibleProvider` 改造前的 bare 行为，
//! 这样未显式配置、且 base_url 未命中预设的 provider（含现有 Ollama / LMStudio 用户）零回归。

use serde::{Deserialize, Serialize};

/// OpenAI chat/completions 请求里 `max_tokens` 字段名。
///
/// 默认 `None` = 不发送 max_tokens（即当前 bare 行为，保证零回归）。
/// 仅当 provider 实际持有 max_tokens 值（`ModelConfig.max_tokens`）时才发送对应字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    /// 不发送（= 当前 bare 行为）
    #[default]
    None,
    MaxTokens,
    MaxCompletionTokens,
}

/// 单 provider 的兼容开关集合。
///
/// `Compat::default()` 等于 `OpenAiCompatibleProvider` 改造前的 bare 行为（零回归）：
/// 不折叠 reasoning、不请求 usage、不推断 finish_reason、不补 assistant 占位、不发送 max_tokens。
#[derive(Debug, Clone, PartialEq)]
pub struct Compat {
    /// `false` 时把 developer 内容并入 system。
    /// llaia 当前仅发 `system` role（无 developer），保留该钩子以兼容未来 developer role 引入。
    pub supports_developer_role: bool,
    /// `true` 时把 `reasoning_content` / `thinking` 折回 `content`，避免某些端点丢思考。
    pub reasoning_to_content: bool,
    /// 发送 max_tokens 的字段名；`None` 不发送（当前 bare 行为）。
    pub max_tokens_field: MaxTokensField,
    /// `true` 时流式也请求并解析 usage。
    pub streaming_usage: bool,
    /// `true` 时从 tool_calls 是否存在推断 `finish_reason = "tool_calls"`。
    pub infer_finish_reason: bool,
    /// `true` 时多轮 tool 结果后补一条空 assistant 占位（Ollama 某些版本需要）。
    pub requires_assistant_after_tool: bool,
    /// `true` 时允许在请求里注入 `chat_template_kwargs: {enable_thinking: false}` 以关闭
    /// 推理模型深度思考。仅对支持该参数的端点（llama.cpp / Ollama 等）有效，其它端点忽略即可。
    /// 仅在请求的 `disable_thinking` 为真时才注入，故不对普通交互请求产生任何影响（零回归）。
    pub disable_thinking_template: bool,
}

impl Default for Compat {
    fn default() -> Self {
        Self {
            supports_developer_role: true,
            reasoning_to_content: false,
            max_tokens_field: MaxTokensField::None,
            streaming_usage: false,
            infer_finish_reason: false,
            requires_assistant_after_tool: false,
            disable_thinking_template: true,
        }
    }
}

impl Compat {
    /// Ollama 预设：reasoning 折回 content、流式 usage、推断 finish_reason、tool 后补 assistant 占位。
    pub fn ollama() -> Self {
        Self {
            supports_developer_role: false,
            reasoning_to_content: true,
            max_tokens_field: MaxTokensField::None,
            streaming_usage: true,
            infer_finish_reason: true,
            requires_assistant_after_tool: true,
            disable_thinking_template: true,
        }
    }

    /// Llama.cpp 预设：reasoning 折回 content、流式 usage、推断 finish_reason。
    pub fn llamacpp() -> Self {
        Self {
            supports_developer_role: false,
            reasoning_to_content: true,
            max_tokens_field: MaxTokensField::None,
            streaming_usage: true,
            infer_finish_reason: true,
            requires_assistant_after_tool: false,
            disable_thinking_template: true,
        }
    }

    /// 按 base_url host 子串探测兼容预设。
    ///
    /// - 含 `ollama` → ollama 预设
    /// - 含 `llama` / `llamacpp` → llamacpp 预设
    /// - 其余 → `Compat::default()`（bare 行为，零回归）
    pub fn detect(base_url: &str) -> Compat {
        let lower = base_url.to_ascii_lowercase();
        if lower.contains("ollama") {
            Compat::ollama()
        } else if lower.contains("llama") || lower.contains("llamacpp") {
            Compat::llamacpp()
        } else {
            Compat::default()
        }
    }

    /// 用配置覆盖层叠加到探测结果之上（配置优先级高于探测）。
    pub fn apply_override(&mut self, o: &CompatConfig) {
        if let Some(v) = o.supports_developer_role {
            self.supports_developer_role = v;
        }
        if let Some(v) = o.reasoning_to_content {
            self.reasoning_to_content = v;
        }
        if let Some(v) = o.max_tokens_field {
            self.max_tokens_field = v;
        }
        if let Some(v) = o.streaming_usage {
            self.streaming_usage = v;
        }
        if let Some(v) = o.infer_finish_reason {
            self.infer_finish_reason = v;
        }
        if let Some(v) = o.requires_assistant_after_tool {
            self.requires_assistant_after_tool = v;
        }
        if let Some(v) = o.disable_thinking_template {
            self.disable_thinking_template = v;
        }
    }

    /// 纯函数：给定原始 finish_reason 与是否观察到 tool_calls，返回有效 finish_reason。
    ///
    /// 当 `infer_finish_reason` 开启且原始为空但有 tool_calls 时，补 `"tool_calls"`。
    pub fn effective_finish_reason(
        &self,
        raw: Option<&str>,
        has_tool_calls: bool,
    ) -> Option<String> {
        match raw {
            Some(s) if !s.is_empty() && s != "null" => Some(s.to_string()),
            _ => {
                if self.infer_finish_reason && has_tool_calls {
                    Some("tool_calls".to_string())
                } else {
                    None
                }
            }
        }
    }
}

/// 配置覆盖层：`[provider.<id>.compat.*]` 中未显式设置的字段为 `None`（保留探测结果）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompatConfig {
    #[serde(default)]
    pub supports_developer_role: Option<bool>,
    #[serde(default)]
    pub reasoning_to_content: Option<bool>,
    #[serde(default)]
    pub max_tokens_field: Option<MaxTokensField>,
    #[serde(default)]
    pub streaming_usage: Option<bool>,
    #[serde(default)]
    pub infer_finish_reason: Option<bool>,
    #[serde(default)]
    pub requires_assistant_after_tool: Option<bool>,
    #[serde(default)]
    pub disable_thinking_template: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_bare_behavior() {
        let c = Compat::default();
        assert!(c.supports_developer_role);
        assert!(!c.reasoning_to_content);
        assert_eq!(c.max_tokens_field, MaxTokensField::None);
        assert!(!c.streaming_usage);
        assert!(!c.infer_finish_reason);
        assert!(!c.requires_assistant_after_tool);
        // 仅当请求显式 disable_thinking 时才注入 chat_template_kwargs，普通请求零影响。
        assert!(c.disable_thinking_template);
    }

    #[test]
    fn detect_ollama() {
        let c = Compat::detect("http://ollama:11434/v1");
        assert!(c.reasoning_to_content);
        assert!(c.streaming_usage);
        assert!(c.infer_finish_reason);
        assert!(c.requires_assistant_after_tool);
        assert!(!c.supports_developer_role);
    }

    #[test]
    fn detect_llamacpp() {
        let c = Compat::detect("http://llama:8080/v1");
        assert!(c.reasoning_to_content);
        assert!(c.streaming_usage);
        assert!(c.infer_finish_reason);
        assert!(!c.requires_assistant_after_tool);
    }

    #[test]
    fn detect_unknown_is_bare() {
        let c = Compat::detect("http://localhost:1234/v1"); // LMStudio，未命中
        assert_eq!(c, Compat::default());
    }

    #[test]
    fn override_beats_detect() {
        let mut c = Compat::detect("http://ollama:11434/v1"); // ollama 预设
        c.apply_override(&CompatConfig {
            reasoning_to_content: Some(false),
            requires_assistant_after_tool: Some(false),
            max_tokens_field: Some(MaxTokensField::MaxCompletionTokens),
            ..Default::default()
        });
        assert!(!c.reasoning_to_content); // 被覆盖
        assert!(!c.requires_assistant_after_tool); // 被覆盖
        assert_eq!(c.max_tokens_field, MaxTokensField::MaxCompletionTokens);
        assert!(c.streaming_usage); // 保留探测值
    }

    #[test]
    fn effective_finish_reason_infer() {
        let c = Compat::ollama();
        assert_eq!(
            c.effective_finish_reason(None, true),
            Some("tool_calls".to_string())
        );
        assert_eq!(c.effective_finish_reason(None, false), None);
        // 原始有值时不覆盖
        assert_eq!(
            c.effective_finish_reason(Some("stop"), true),
            Some("stop".to_string())
        );
        // 默认（不推断）即使有 tool_calls 也返回 None
        assert_eq!(Compat::default().effective_finish_reason(None, true), None);
    }
}
