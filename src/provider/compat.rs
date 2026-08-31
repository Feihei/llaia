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
    /// 探测另一端能否用 OpenAI function calling（发 `tools` + 期待结构化 `tool_calls`）。
    ///
    /// 作为 `ModelConfig.native_tool_calling` 缺省（`None=auto`）时的**探测默认**（#10）。
    /// 预设均默认 `true`（与历史缺省一致，零回归）；后续发现某端点不支持 native 时，
    /// 改对应预设为 `false`，用户无需改配置即自动降级到 `<tool_call>` 标签协议。
    pub native_tool_calling: bool,
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
            native_tool_calling: true,
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
            native_tool_calling: true,
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
            native_tool_calling: true,
        }
    }

    /// 按 base_url host 子串 + model slug 探测兼容预设（plan #4 / #10 同一套框架）。
    ///
    /// **第一步：provider 预设**（按 base_url host 子串 + 端口/路径特征）
    /// - 含 `ollama` → ollama 预设（默认端口 11434，host 里带 `ollama` 才命中）
    /// - 含 `llama` / `8080` / `completion` → llamacpp 预设（IP 型 host 不含 `llama`
    ///   字样时靠默认端口或路径识别，如 `http://10.0.11.187:8080/v1`）
    /// - 含 `deepseek` → 基础预设（流式 usage）
    /// - 含 `zhipu` / `bigmodel` / `glm` → 基础预设（流式 usage）
    /// - 含 `moonshot` / `kimi` → 基础预设（流式 usage）
    /// - 其余 → `Compat::default()`（bare 行为，零回归）
    ///
    /// **第二步：per-model 表**（按 model slug 子串，对齐 nanobot/goose 的 per-model 规则）
    /// 覆盖 provider 基础的 `max_tokens_field` / `reasoning_to_content`。
    pub fn detect(base_url: &str, model: &str) -> Compat {
        let lower = base_url.to_ascii_lowercase();
        let lm = model.to_ascii_lowercase();

        // provider 预设基础
        let mut c = if lower.contains("ollama") {
            Compat::ollama()
        } else if lower.contains("llama") || lower.contains("8080") || lower.contains("completion")
        {
            // llama.cpp 默认端口 8080 / 含 completion 的路径：命中 IP 型 host（如 10.0.11.187:8080）
            // 或自建端点也能正确识别，保证流式 usage（token 统计）上报。
            Compat::llamacpp()
        } else if lower.contains("deepseek")
            || lower.contains("zhipu")
            || lower.contains("bigmodel")
            || lower.contains("glm")
            || lower.contains("moonshot")
            || lower.contains("kimi")
        {
            // 线上 GPT/OpenAI 兼容端点普遍在流式里返回 usage
            Compat {
                streaming_usage: true,
                ..Compat::default()
            }
        } else {
            Compat::default()
        };

        // per-model 表
        if Self::model_uses_max_completion_tokens(&lm) {
            c.max_tokens_field = MaxTokensField::MaxCompletionTokens;
        }
        if Self::model_folds_reasoning(&lm) {
            c.reasoning_to_content = true;
        }
        c
    }

    /// 需要 `max_completion_tokens` 的模型（o 系、kimi-k3 等）才切字段；
    /// 其余保持当前 bare 语义（不发送 max_tokens），避免向不支持字段的端点误发报错。
    fn model_uses_max_completion_tokens(lm: &str) -> bool {
        // o 系：o1/o3/o4 及其 mini/preview 变种
        lm.starts_with("o1")
            || lm.starts_with("o3")
            || lm.starts_with("o4")
            || lm.contains("kimi-k3")
    }

    /// deepseek / kimi 推理模型把 `reasoning_content` 折回 `content`，避免思考被端点丢弃
    /// （对齐 nanobot：deepseek-R1/reasoner 走 `reasoning_content` 而非 `reasoning`）。
    fn model_folds_reasoning(lm: &str) -> bool {
        lm.contains("deepseek-reasoner")
            || lm.contains("deepseek-r1")
            || lm.contains("deepseek-reasoning")
            || lm.contains("kimi-k")
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
        if let Some(v) = o.native_tool_calling {
            self.native_tool_calling = v;
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
    #[serde(default)]
    pub native_tool_calling: Option<bool>,
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
        assert!(c.native_tool_calling); // 与历史缺省 true 一致
    }

    #[test]
    fn detect_ollama() {
        let c = Compat::detect("http://ollama:11434/v1", "qwen2.5:7b");
        assert!(c.reasoning_to_content);
        assert!(c.streaming_usage);
        assert!(c.infer_finish_reason);
        assert!(c.requires_assistant_after_tool);
        assert!(!c.supports_developer_role);
    }

    #[test]
    fn detect_llamacpp() {
        let c = Compat::detect("http://llama:8080/v1", "qwen2.5");
        assert!(c.reasoning_to_content);
        assert!(c.streaming_usage);
        assert!(c.infer_finish_reason);
        assert!(!c.requires_assistant_after_tool);
    }

    #[test]
    fn detect_llamacpp_via_port_or_path() {
        // IP 型 host 不含 "llama"，依赖 8080 端口 / completion 路径识别（GitHub issue: stats 抓不到数据）
        for base in [
            "http://10.0.11.187:8080/v1", // 家用 llama.cpp，主 agent 实际配置
            "http://192.168.1.10:8080/v1",
            "http://server.local/completion",
        ] {
            let c = Compat::detect(base, "ornith-1.5-35b");
            assert!(c.streaming_usage, "base={base}");
            assert!(c.infer_finish_reason, "base={base}");
            assert!(!c.requires_assistant_after_tool, "base={base}");
        }
    }

    #[test]
    fn detect_unknown_is_bare() {
        let c = Compat::detect("http://localhost:1234/v1", "llama-3.1"); // LMStudio，未命中 host 且 model 非 o 系
        assert_eq!(c, Compat::default());
    }

    #[test]
    fn detect_online_provider_preset() {
        // 线上深色系 host → 基础预设（流式 usage），不误切 max_tokens 字段
        for host in [
            "https://api.deepseek.com/v1",
            "https://open.bigmodel.cn/api/paas",
            "https://api.moonshot.cn/v1",
            "https://api.kimi.ai/v1",
        ] {
            let c = Compat::detect(host, "some-chat-model");
            assert!(c.streaming_usage, "host={host}");
            assert_eq!(c.max_tokens_field, MaxTokensField::None, "host={host}");
            assert!(!c.reasoning_to_content, "host={host}");
        }
    }

    #[test]
    fn detect_per_model_overrides() {
        // o 系 / kimi-k3 → max_completion_tokens（即使 host 是 bare）
        let o3 = Compat::detect("https://api.example.com/v1", "o3-mini");
        assert_eq!(o3.max_tokens_field, MaxTokensField::MaxCompletionTokens);
        let kimi = Compat::detect("https://api.example.com/v1", "kimi-k3-1025-preview");
        assert_eq!(kimi.max_tokens_field, MaxTokensField::MaxCompletionTokens);
        // deepseek-reasoner → 折回 reasoning
        let r1 = Compat::detect("https://api.deepseek.com/v1", "deepseek-reasoner");
        assert!(r1.reasoning_to_content);
        // deepseek-chat 不折回、不切字段
        let chat = Compat::detect("https://api.deepseek.com/v1", "deepseek-chat");
        assert!(!chat.reasoning_to_content);
        assert_eq!(chat.max_tokens_field, MaxTokensField::None);
        // 普通模型在线上 host 只开 usage，不切字段
        let glm = Compat::detect("https://api.example.com/v1", "glm-4-plus");
        assert_eq!(glm, Compat::default());
    }

    #[test]
    fn detect_native_flips_with_preset() {
        // 探测层原生默认 true；auto 场景由 model_cfg.unwrap_or 消费。
        assert!(Compat::detect("https://api.deepseek.com/v1", "deepseek-chat").native_tool_calling);
        // 配置覆盖可关闭（缺省跟随探测的原生并探）
        let mut c = Compat::detect("https://api.deepseek.com/v1", "deepseek-chat");
        c.apply_override(&CompatConfig {
            native_tool_calling: Some(false),
            ..Default::default()
        });
        assert!(!c.native_tool_calling);
    }

    #[test]
    fn override_beats_detect() {
        let mut c = Compat::detect("http://ollama:11434/v1", "qwen2.5"); // ollama 预设
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
