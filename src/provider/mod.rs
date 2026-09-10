use anyhow::Result;
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub mod anthropic;
pub mod compat;
pub mod fallback;
pub mod gemini;
pub mod openai_compat;
pub mod probe;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 消息内容：纯文本（向后兼容）或多模态（文本+图片等）。
/// 序列化时 Text 变体为字符串，Multimodal 变体为 OpenAI content 数组。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Multimodal(Vec<ContentPart>),
}

impl MessageContent {
    /// 获取纯文本部分（用于 token 估算、压缩 dump、日志等场景）。
    /// 多模态变体只拼接 Text part，图片部分忽略。
    pub fn as_text(&self) -> String {
        match self {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Multimodal(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.clone()),
                    ContentPart::ImageUrl { .. } => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        }
    }

    /// 是否包含图片（用于压缩降级判断）
    pub fn has_image(&self) -> bool {
        match self {
            MessageContent::Text(_) => false,
            MessageContent::Multimodal(parts) => parts
                .iter()
                .any(|p| matches!(p, ContentPart::ImageUrl { .. })),
        }
    }
}

/// 多模态 content 数组的一个 part。序列化为 OpenAI 格式：
/// `{"type":"text","text":"..."}` / `{"type":"image_url","image_url":{"url":"..."}}`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrlContent },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageUrlContent {
    /// data:image/jpeg;base64,... 或 http(s) URL
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: MessageContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// 思考文本留存（P0，docs/plans/2026-09-10-thinking-capability-model.md）：
    /// provider 流收集的 reasoning_content/thinking 原文，落 sqlite 与 WebUI 渲染。
    /// 仅随留存走，出站请求不携带（`build_openai_messages` 等序列化器不读该字段），
    /// 回传（preserve）是 P2 的事。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: MessageContent::Text(content.into()),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: MessageContent::Text(content.into()),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::Text(content.into()),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }
    /// 带 thinking 留存的 assistant 消息（P0）：`reasoning` 为 None/空等价于 `assistant`。
    pub fn assistant_with_reasoning(content: impl Into<String>, reasoning: Option<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::Text(content.into()),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: reasoning.filter(|r| !r.is_empty()),
        }
    }
    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::Text(content.into()),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            reasoning_content: None,
        }
    }
    pub fn tool(content: impl Into<String>, tool_call_id: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: MessageContent::Text(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            reasoning_content: None,
        }
    }
    /// 多模态用户消息：parts 至少含一个文本 part 和/或图片 part
    pub fn user_multimodal(parts: Vec<ContentPart>) -> Self {
        Self {
            role: Role::User,
            content: MessageContent::Multimodal(parts),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// 规范档位（P1 D1）：`none|low|medium|high|max`。
/// `none` = 要求关；与请求意图 `ThinkingIntent::Auto`（不发任何参数）严格区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    None,
    Low,
    Medium,
    High,
    Max,
}

impl ThinkingLevel {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "max" => Some(Self::Max),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
        }
    }
}

/// 档位方言（P1 D2）：怎么在请求里表达档位与「关」。值集来自五家端点 wire 实测，
/// 不做模型名猜测（plan「明确否决」节）。serde 名即 toml 字面值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ThinkingLevelWire {
    /// 不给旋钮：端点不校验档位（bogus 也 200），发了也白发
    #[serde(rename = "none")]
    None,
    /// 顶层 `reasoning_effort` 字段，服务端校验非法值（400 ⇒ 支持）
    #[serde(rename = "reasoning_effort")]
    ReasoningEffort,
}

/// 「关」的方言（P1 D2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ThinkingOffWire {
    /// 嵌套 `chat_template_kwargs:{enable_thinking:false}`（llama.cpp 实测有效）
    #[serde(rename = "enable_thinking_false")]
    EnableThinkingFalse,
    /// `thinking:{type:"disabled"}`（sensenova DeepSeek 实测；非法组合 400 规则来自错误消息）
    #[serde(rename = "thinking_disabled")]
    ThinkingDisabled,
    /// 顶层 `reasoning_effort:"none"`（Ollama / agnes 实测唯一有效的关）
    #[serde(rename = "reasoning_effort_none")]
    ReasoningEffortNone,
    /// 该端点明确拒绝关思考（GLM 实测 400）——任何关意图整个短路
    #[serde(rename = "unsupported")]
    Unsupported,
}

/// 请求级思考意图（P1 D1）：这轮要多少思考。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingIntent {
    /// 默认：不发任何思考参数，维持服务端默认行为（取代旧 `on` 的语义）
    Auto,
    /// 要求关思考（能不能关由该模型的 `off_wire` 决定，见 `resolve_thinking`）
    None,
    /// 要求指定档位（能不能调由 `level_wire` 决定）
    Level(ThinkingLevel),
}

impl ThinkingIntent {
    /// 命令回显用名（与 `/reasoning` 参数一致）
    pub fn name(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::Level(l) => l.as_str(),
        }
    }
}

/// 意图 × 能力声明 → 出站参数决议（P1 D2 映射规则，provider 层唯一收口）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedThinking {
    /// 不发任何字段（Auto；unsupported 短路；未知端点的档位意图被门禁拒绝）
    Nothing,
    /// 顶层 `reasoning_effort: <level>`（含 "none"：off_wire=reasoning_effort_none）
    Effort(ThinkingLevel),
    /// 嵌套 `chat_template_kwargs:{enable_thinking:false}`（off_wire 显式声明，不经 compat 门）
    EnableThinkingFalse,
    /// `thinking:{type:"disabled"}`（DeepSeek 方言；单意图构造下 effort 字段天然不并发，
    /// D2 的「同时抑制 effort」自动满足）
    ThinkingDisabled,
    /// 能力未知时的 legacy 兜底：仅在 compat.disable_thinking_template 门内注入 kwargs
    /// （= 旧 `disable_thinking` 的行为：llama.cpp 有效，忽略方无害，字节兼容存量）
    LegacyDisable,
}

/// P1 D2/D3：请求意图 × `[provider.<id>.<model>.thinking]` 声明 → 出站决议。
/// 这是唯一允许把思考参数写进请求体的地方。
pub fn resolve_thinking(
    intent: Option<ThinkingIntent>,
    cfg: Option<&crate::config::ThinkingConfig>,
) -> ResolvedThinking {
    let Some(intent) = intent else {
        return ResolvedThinking::Nothing;
    };
    match intent {
        ThinkingIntent::Auto => ResolvedThinking::Nothing,
        ThinkingIntent::None => match cfg.and_then(|c| c.off_wire) {
            Some(ThinkingOffWire::EnableThinkingFalse) => ResolvedThinking::EnableThinkingFalse,
            Some(ThinkingOffWire::ThinkingDisabled) => ResolvedThinking::ThinkingDisabled,
            Some(ThinkingOffWire::ReasoningEffortNone) => {
                ResolvedThinking::Effort(ThinkingLevel::None)
            }
            // unsupported：任何关意图整个短路（GLM 实测发 disabled 必 400）
            Some(ThinkingOffWire::Unsupported) => ResolvedThinking::Nothing,
            // unknown：legacy 兜底（llama.cpp/Ollama 预设门内注入，其余端点不发）
            None => ResolvedThinking::LegacyDisable,
        },
        ThinkingIntent::Level(l) => match cfg.and_then(|c| c.level_wire) {
            Some(ThinkingLevelWire::ReasoningEffort) => ResolvedThinking::Effort(l),
            // none/unknown：不发（命令层已拒绝；guard 不会发档位意图）
            _ => ResolvedThinking::Nothing,
        },
    }
}

/// P1 D5：`/reasoning` 意图按能力求值三态回显。
/// `Ok(生效描述)` = 接受设置；`Err(拒绝理由)` = 该模型不支持（不发请求、不改设置——
/// `none` 落在不支持关的端点上报错并列出可用路径，不静默降级，性质 6）。
pub fn evaluate_reasoning(
    intent: ThinkingIntent,
    cfg: Option<&crate::config::ThinkingConfig>,
) -> Result<String, String> {
    let default_desc = match cfg.and_then(|c| c.default) {
        Some(l) => l.as_str().to_string(),
        None => "unknown".to_string(),
    };
    Ok(match intent {
        ThinkingIntent::Auto => {
            format!("effective: auto (no thinking params sent; model default: {default_desc})")
        }
        ThinkingIntent::None => match cfg.and_then(|c| c.off_wire) {
            Some(ThinkingOffWire::EnableThinkingFalse) => {
                "effective: off (wire: chat_template_kwargs.enable_thinking=false)".into()
            }
            Some(ThinkingOffWire::ThinkingDisabled) => {
                "effective: off (wire: thinking.type=disabled)".into()
            }
            Some(ThinkingOffWire::ReasoningEffortNone) => {
                "effective: off (wire: reasoning_effort=none)".into()
            }
            Some(ThinkingOffWire::Unsupported) => {
                return Err(
                    "this model cannot disable thinking (off_wire=unsupported: the disable \
                     dialect is rejected with HTTP 400); use /reasoning auto to leave the \
                     model default untouched"
                        .into(),
                );
            }
            None => format!(
                "effective: unknown — no [thinking] section configured; falling back to legacy \
                 chat_template_kwargs (best-effort). model default: {default_desc}"
            ),
        },
        ThinkingIntent::Level(l) => match cfg.and_then(|c| c.level_wire) {
            Some(ThinkingLevelWire::ReasoningEffort) => {
                format!("effective: {} (wire: reasoning_effort)", l.as_str())
            }
            _ => {
                return Err(
                    "this endpoint has no level knob (level_wire=none/unknown: effort params \
                     are ignored or unvalidated); use /reasoning none|auto instead"
                        .into(),
                );
            }
        },
    })
}

#[derive(Debug, Clone)]
pub struct ChatRequest<'a> {
    pub messages: &'a [ChatMessage],
    pub tools: Option<&'a [ToolSpec]>,
    /// 请求级思考意图（P1，docs/plans/2026-09-10-thinking-capability-model.md D1）。
    /// `None`（缺省）= Auto：不发任何思考参数，请求体与今天逐字节一致（性质 1）。
    /// 由 `resolve_thinking` × 该模型的 `[thinking]` 能力声明唯一收口成出站参数；
    /// 旧 `disable_thinking: bool` 的全部语义由 `ThinkingIntent::None` + legacy 兜底承接。
    pub thinking: Option<ThinkingIntent>,
}

#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    /// 流式 usage（token 统计）；仅部分 provider 在 compat 开启时填充。
    pub usage: Option<Usage>,
    /// 有效 finish_reason（含 compat 推断）；默认 None。
    pub finish_reason: Option<String>,
    /// 思考文本留存（P0）：`reasoning_to_content=false` 时 provider 从 SSE 收集的
    /// reasoning_content/thinking 原文；折回 content 的通路不填（避免双份）。
    pub reasoning: Option<String>,
}

/// 单次生成的 token 用量统计。
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// 流式事件
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// 文本增量
    TextDelta(String),
    /// 思考流增量（P0 留存）：provider 从 SSE 的 reasoning_content/thinking 收集。
    /// 仅在 `reasoning_to_content=false`（不折回可见文本）时产生——折回通路里
    /// 思考已混进 TextDelta，为避免同一段文本双份存储不另发此事件。
    /// agent 层收集后只落 sqlite/留存，不向用户流式输出。
    ReasoningDelta(String),
    /// 工具调用（native 模式下完整 ToolCall；标签模式不产生此事件，由 Agent 状态机解析）
    ToolCall(ToolCall),
    /// 本轮流式结束
    Done,
    /// 错误
    Error(String),
    /// 流式 usage（token 统计），仅在 compat.streaming_usage 时产生
    Usage(Usage),
    /// 有效 finish_reason（含 compat 推断）
    FinishReason(String),
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, req: &ChatRequest<'_>) -> Result<ChatResponse>;
    async fn chat_stream(&self, req: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>>;
    fn native_tool_calling(&self) -> bool;
    /// 探测模型上下文窗口大小（tokens）。默认返回 None，由具体 provider 实现。
    async fn detect_context_size(&self) -> Option<usize> {
        None
    }
    /// 可读标识（模型名），用于 `/provider` 列表标记当前模型。默认 "unknown"。
    fn label(&self) -> String {
        "unknown".into()
    }
    /// 思考能力声明（P1 D2）：`[provider.<id>.<model>.thinking]` 的载入结果。
    /// `None` = 未配置（unknown）：Off 意图走 legacy 兜底，档位意图被门禁拒绝。
    /// `FallbackProvider` 委托链上主 provider。
    fn thinking_capability(&self) -> Option<crate::config::ThinkingConfig> {
        None
    }
    /// 提供器类型标识（诊断/测试用）：默认 `"provider"`，`FallbackProvider` 覆盖为 `"fallback"`。
    /// 供 `/provider` 切换等场景断言降级链是否保留。
    fn kind(&self) -> &'static str {
        "provider"
    }
}

/// 从 model ref（"provider_id.model_alias"）构建单个 provider 实例。
pub fn provider_from_ref(
    config: &crate::config::Config,
    model_ref: &str,
) -> Result<Arc<dyn Provider>> {
    let (prov_id, model_alias) = crate::config::Config::parse_model_ref(model_ref)?;
    let prov_cfg = config
        .provider
        .get(prov_id)
        .ok_or_else(|| anyhow::anyhow!("provider.{} not configured", prov_id))?;
    let model_cfg = prov_cfg.model.get(model_alias).ok_or_else(|| {
        anyhow::anyhow!("provider.{}.model.{} not configured", prov_id, model_alias)
    })?;
    match prov_cfg.provider_type.as_str() {
        "anthropic" => {
            let base_url = if prov_cfg.base_url.is_empty() {
                "https://api.anthropic.com"
            } else {
                &prov_cfg.base_url
            };
            Ok(Arc::new(anthropic::AnthropicProvider::new(
                base_url,
                &prov_cfg.api_key,
                &model_cfg.model,
                model_cfg.max_tokens.unwrap_or(0),
            )?))
        }
        "gemini" => {
            let base_url = if prov_cfg.base_url.is_empty() {
                "https://generativelanguage.googleapis.com"
            } else {
                &prov_cfg.base_url
            };
            Ok(Arc::new(gemini::GeminiProvider::new(
                base_url,
                &prov_cfg.api_key,
                &model_cfg.model,
                model_cfg.max_tokens.unwrap_or(0),
            )?))
        }
        // openai_compatible 及未知 type 都走 OpenAI 兼容协议（存量配置无 type 也能跑）
        _ => {
            // 兼容层：先按 base_url + model slug 探测预设（plan #4/#10），再用 [provider.<id>.compat.*] 覆盖
            let mut compat = compat::Compat::detect(&prov_cfg.base_url, &model_cfg.model);
            if let Some(c) = &prov_cfg.compat {
                compat.apply_override(c);
            }
            // native_tool_calling：显式配置优先，缺省（None=auto）跟随探测结果（#10）
            let native_tool_calling = model_cfg
                .native_tool_calling
                .unwrap_or(compat.native_tool_calling);
            // Generation Guard：思考流字符上限（output_guard 关闭时传 0 = 不限）
            let thinking_cap = if config.runtime.output_guard {
                config.runtime.guard_thinking_cap
            } else {
                0
            };
            Ok(Arc::new(
                openai_compat::OpenAiCompatibleProvider::new(
                    &prov_cfg.base_url,
                    &prov_cfg.api_key,
                    &model_cfg.model,
                    native_tool_calling,
                    model_cfg.max_tokens,
                    compat,
                )?
                .with_thinking_cap(thinking_cap)
                .with_thinking(model_cfg.thinking.clone()),
            ))
        }
    }
}

/// 构建主 provider 链：主 model + fallback 备用链。
/// - main_ref 为空 → Ok(None)（降级模式）
/// - 主 model 构建失败 → Err（配置错误应暴露）
/// - fallback 项构建失败 → warn 跳过（备用链是容错手段，不应阻塞启动）
/// - fallback 全部不可用/未配置 → 返回裸主 provider
pub fn build_provider_chain(
    main_ref: &str,
    fallback: &[String],
    config: &crate::config::Config,
) -> Result<Option<Arc<dyn Provider>>> {
    if main_ref.is_empty() {
        return Ok(None);
    }
    let main = provider_from_ref(config, main_ref)?;
    if fallback.is_empty() {
        return Ok(Some(main));
    }
    let mut chain = vec![main];
    for f in fallback {
        match provider_from_ref(config, f) {
            Ok(p) => chain.push(p),
            Err(e) => tracing::warn!(
                model = f.as_str(),
                error = %e,
                "fallback provider build failed, skipped"
            ),
        }
    }
    if chain.len() == 1 {
        return Ok(Some(chain.remove(0)));
    }
    Ok(Some(Arc::new(fallback::FallbackProvider::new(chain))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThinkingConfig;

    #[test]
    fn resolve_thinking_matrix() {
        // P1 D2 映射规则全矩阵：意图 × 能力声明 → 出站决议
        let cfg = |level_wire, off_wire| ThinkingConfig {
            default: Some(ThinkingLevel::High),
            level_wire,
            off_wire,
        };
        // None intent（缺省）≡ Auto：什么都不发（性质 1）
        assert_eq!(resolve_thinking(None, None), ResolvedThinking::Nothing);
        assert_eq!(
            resolve_thinking(Some(ThinkingIntent::Auto), None),
            ResolvedThinking::Nothing
        );
        assert_eq!(
            resolve_thinking(Some(ThinkingIntent::Auto), Some(&cfg(None, None))),
            ResolvedThinking::Nothing
        );
        // Off × 四种 off_wire + unknown
        assert_eq!(
            resolve_thinking(
                Some(ThinkingIntent::None),
                Some(&cfg(None, Some(ThinkingOffWire::EnableThinkingFalse)))
            ),
            ResolvedThinking::EnableThinkingFalse
        );
        assert_eq!(
            resolve_thinking(
                Some(ThinkingIntent::None),
                Some(&cfg(None, Some(ThinkingOffWire::ThinkingDisabled)))
            ),
            ResolvedThinking::ThinkingDisabled
        );
        assert_eq!(
            resolve_thinking(
                Some(ThinkingIntent::None),
                Some(&cfg(None, Some(ThinkingOffWire::ReasoningEffortNone)))
            ),
            ResolvedThinking::Effort(ThinkingLevel::None)
        );
        // unsupported：任何关意图整个短路（GLM 实测 400）
        assert_eq!(
            resolve_thinking(
                Some(ThinkingIntent::None),
                Some(&cfg(None, Some(ThinkingOffWire::Unsupported)))
            ),
            ResolvedThinking::Nothing
        );
        // unknown：legacy 兜底（compat 门内 kwargs）
        assert_eq!(
            resolve_thinking(Some(ThinkingIntent::None), Some(&cfg(None, None))),
            ResolvedThinking::LegacyDisable
        );
        // Level × level_wire
        assert_eq!(
            resolve_thinking(
                Some(ThinkingIntent::Level(ThinkingLevel::Medium)),
                Some(&cfg(
                    Some(ThinkingLevelWire::ReasoningEffort),
                    Some(ThinkingOffWire::ReasoningEffortNone)
                ))
            ),
            ResolvedThinking::Effort(ThinkingLevel::Medium)
        );
        // level_wire=none/unknown：档位意图不发（命令层已拒绝）
        assert_eq!(
            resolve_thinking(
                Some(ThinkingIntent::Level(ThinkingLevel::Low)),
                Some(&cfg(Some(ThinkingLevelWire::None), None))
            ),
            ResolvedThinking::Nothing
        );
        assert_eq!(
            resolve_thinking(
                Some(ThinkingIntent::Level(ThinkingLevel::Low)),
                Some(&cfg(None, None))
            ),
            ResolvedThinking::Nothing
        );
    }

    #[test]
    fn evaluate_reasoning_three_states() {
        // D5 三态：生效 / 该模型不支持（Err，不改设置）/ 通路被忽略（unknown 提示）
        let llamacpp = ThinkingConfig {
            default: Some(ThinkingLevel::High),
            level_wire: None, // caps 在场但未经重复采样，保守 none
            off_wire: Some(ThinkingOffWire::EnableThinkingFalse),
        };
        assert!(evaluate_reasoning(ThinkingIntent::None, Some(&llamacpp))
            .unwrap()
            .contains("effective: off"));
        // llamacpp 无档位旋钮 → 档位意图被拒收
        assert!(
            evaluate_reasoning(ThinkingIntent::Level(ThinkingLevel::Low), Some(&llamacpp)).is_err()
        );
        // GLM（tokenrouter）：关被拒收（D1：报错不静默降级）
        let glm = ThinkingConfig {
            default: Some(ThinkingLevel::Max),
            level_wire: Some(ThinkingLevelWire::None),
            off_wire: Some(ThinkingOffWire::Unsupported),
        };
        let err = evaluate_reasoning(ThinkingIntent::None, Some(&glm)).unwrap_err();
        assert!(err.contains("cannot disable thinking"), "{err}");
        assert!(evaluate_reasoning(ThinkingIntent::Level(ThinkingLevel::Low), Some(&glm)).is_err());
        // unknown：Off 仍可设但回显 unknown 三态
        let echo = evaluate_reasoning(ThinkingIntent::None, None).unwrap();
        assert!(echo.contains("effective: unknown"), "{echo}");
        // auto 永远可行
        assert!(evaluate_reasoning(ThinkingIntent::Auto, None).is_ok());
    }

    #[test]
    fn test_chat_message_constructors() {
        let m = ChatMessage::system("hello");
        assert_eq!(m.role, Role::System);
        assert_eq!(m.content.as_text(), "hello");
        assert!(m.tool_calls.is_none());

        let m = ChatMessage::assistant_with_tools(
            "",
            vec![ToolCall {
                id: "1".into(),
                name: "file_read".into(),
                arguments: serde_json::json!({"path": "/tmp"}),
            }],
        );
        assert_eq!(m.role, Role::Assistant);
        assert_eq!(m.tool_calls.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_multimodal_message() {
        let parts = vec![
            ContentPart::Text {
                text: "这张图是什么？".into(),
            },
            ContentPart::ImageUrl {
                image_url: ImageUrlContent {
                    url: "data:image/jpeg;base64,xxx".into(),
                },
            },
        ];
        let m = ChatMessage::user_multimodal(parts);
        assert_eq!(m.role, Role::User);
        assert!(m.content.has_image());
        assert_eq!(m.content.as_text(), "这张图是什么？");
    }

    #[test]
    fn test_message_content_serialize() {
        // 纯文本序列化为字符串
        let m = ChatMessage::user("hello");
        let v = serde_json::to_value(&m.content).unwrap();
        assert_eq!(v, serde_json::json!("hello"));

        // 多模态序列化为数组
        let parts = vec![
            ContentPart::Text {
                text: "desc".into(),
            },
            ContentPart::ImageUrl {
                image_url: ImageUrlContent {
                    url: "data:image/jpeg;base64,x".into(),
                },
            },
        ];
        let m = ChatMessage::user_multimodal(parts);
        let v = serde_json::to_value(&m.content).unwrap();
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 2);
        assert_eq!(v[0]["type"], "text");
        assert_eq!(v[1]["type"], "image_url");
    }
}
