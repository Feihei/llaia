use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use crate::provider::compat::CompatConfig;

/// 敏感信息 .env 自动化（P5 S1）：收集明文敏感字段、写入 .env、替换为 `${VAR}` 引用。
pub mod secrets;

/// 顶层配置。对应 ~/.llaia/config.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub log: LogConfig,
    /// provider id => ProviderConfig（统一连接注册表：llm family + service family）
    #[serde(default)]
    pub provider: HashMap<String, ProviderConfig>,
    /// model id => ModelEntry（P8 模型目录：全局唯一，按 kind 分型）。
    /// rename 为单数 `model`：用户可见的 TOML/JSON 键是 `[model.<id>]`（对齐已提交的
    /// 规划文档与 CONFIG_TEMPLATE），Rust 侧字段名保持复数 models。
    #[serde(default, rename = "model")]
    pub models: BTreeMap<String, ModelEntry>,
    /// agent alias => AgentConfig
    #[serde(default)]
    pub agent: HashMap<String, AgentConfig>,
    #[serde(default)]
    pub webui: WebUiConfig,
    #[serde(default)]
    pub channels: ChannelsConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
}

/// 全局运行时参数（与具体 agent 无关）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    #[serde(default = "default_threshold")]
    pub context_threshold: f64,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    /// 上下文压缩用的模型引用（P8：`[model.<id>]` 目录条目 id）。
    /// 未设置时复用 agent 自身的 provider（兼容旧行为）。
    /// 设置后会构建独立的 compact provider，可用更便宜的模型做压缩。
    #[serde(default)]
    pub compact_model: Option<String>,
    /// IANA 时区名（如 "Asia/Shanghai"），决定 agent 状态栏与用户可见日期。
    /// None（默认）= 跟随宿主机本地时区，与旧行为一致。
    /// 非法值在 Config::load 里 warn + 置 None。
    #[serde(default)]
    pub timezone: Option<String>,
    /// 图片描述用的模型引用（P8：`[model.<id>]` 目录条目 id，须 kind=chat）。
    /// 主模型无多模态能力时，用此模型描述图片，描述文本替换图片注入主模型上下文。
    /// 未设置时：图片直接发给主模型（主模型不支持则由 provider 决定如何处理）。
    #[serde(default)]
    pub vision_model: Option<String>,
    /// 权限档位（P4-d）：read-only / default / yolo。
    /// 决定哪些有副作用的操作需要交互式审批，以及审批范围。
    /// None（默认）= "default"。非法值 warn + 置 None。
    #[serde(default)]
    pub permission: Option<String>,
    /// ask_user 阻塞式澄清的超时秒数（ADR-0022）。
    /// pending question 在 timeout_secs 内未收到回答，下一条用户消息到达时
    /// 自动按"用户未回答，已按最合理假设继续"续跑。默认 300（对齐 zeroclaw）。
    #[serde(default = "default_ask_user_timeout")]
    pub ask_user_timeout_secs: usize,
    /// 单个工具结果文本的最大字符数（非图片内容）。
    /// 超过则截断并附占位说明（完整内容已写入会话记录 sqlite 留底，可随时回查）。
    /// 工具返回的图片（data:image base64）不占此额度：识别后走多模态读图
    /// （主模型 / vision provider）或截断，避免 280k 字符的 base64 撑爆上下文。
    #[serde(default = "default_tool_result_cap")]
    pub tool_result_cap: usize,
    /// 长任务心跳间隔（秒）：聊天频道每这么久发一条 "still working"，避免用户以为卡死。
    /// 默认 600（10 分钟）。按墙钟计，与事件是否密集无关。
    #[serde(default = "default_keepalive_interval")]
    pub keepalive_interval_secs: u64,
    /// 单轮最大运行时长（秒）：超过即自动中断，防止模型无限循环却对用户静默。
    /// 默认 3600（1 小时）；需大于心跳间隔才有意义。
    #[serde(default = "default_max_turn_duration")]
    pub max_turn_duration_secs: u64,
    /// 输出退化防护总开关（Generation Guard，docs/plans/2026-09-03-generation-guard.md）。
    /// 开启后：思考流超限 / 可见文本重复 / 空输出 → 中止 + 带提示重试 →
    /// 重试耗尽诊断收尾，连续失败附加醒目警告（只报警不拒服）。
    #[serde(default = "default_output_guard")]
    pub output_guard: bool,
    /// 重复检测滑动窗口（字符）。字符级而非 token 级：框架无 tokenizer，引擎无关。
    #[serde(default = "default_guard_repeat_window")]
    pub guard_repeat_window: usize,
    /// 重复检测 n-gram 长度（字符）。
    #[serde(default = "default_guard_repeat_gram")]
    pub guard_repeat_gram: usize,
    /// 窗口内同一 n-gram 出现次数达到该值即判退化（原始提案 3 次对代码重复行
    /// 误报风险偏高，保守取 4；触发时记日志，按实测调）。
    #[serde(default = "default_guard_repeat_threshold")]
    pub guard_repeat_threshold: u32,
    /// 思考流字符上限（`<think>` 块与 reasoning_content 累计），0 = 不限。
    /// 兜底值给得宽松（重复检测通常先命中）；超限中止并关思考重试。
    #[serde(default = "default_guard_thinking_cap")]
    pub guard_thinking_cap: usize,
    /// 判退化后的重试次数（重试请求附加 [guard] 提示并强制关思考）。
    #[serde(default = "default_guard_max_retries")]
    pub guard_max_retries: u32,
    /// 连续退化回合数达到该值时在诊断消息中附加醒目警告（熔断只报警不拒服）。
    #[serde(default = "default_guard_breaker_threshold")]
    pub guard_breaker_threshold: u32,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            context_threshold: default_threshold(),
            max_iterations: default_max_iterations(),
            compact_model: None,
            timezone: None,
            vision_model: None,
            permission: None,
            ask_user_timeout_secs: default_ask_user_timeout(),
            tool_result_cap: default_tool_result_cap(),
            keepalive_interval_secs: default_keepalive_interval(),
            max_turn_duration_secs: default_max_turn_duration(),
            output_guard: default_output_guard(),
            guard_repeat_window: default_guard_repeat_window(),
            guard_repeat_gram: default_guard_repeat_gram(),
            guard_repeat_threshold: default_guard_repeat_threshold(),
            guard_thinking_cap: default_guard_thinking_cap(),
            guard_max_retries: default_guard_max_retries(),
            guard_breaker_threshold: default_guard_breaker_threshold(),
        }
    }
}

fn default_threshold() -> f64 {
    0.7
}

fn default_max_iterations() -> u32 {
    10
}

fn default_ask_user_timeout() -> usize {
    300
}

fn default_tool_result_cap() -> usize {
    32_768
}

fn default_keepalive_interval() -> u64 {
    600
}

fn default_max_turn_duration() -> u64 {
    3600
}

fn default_output_guard() -> bool {
    true
}

fn default_guard_repeat_window() -> usize {
    512
}

fn default_guard_repeat_gram() -> usize {
    24
}

fn default_guard_repeat_threshold() -> u32 {
    4
}

fn default_guard_thinking_cap() -> usize {
    32_000
}

fn default_guard_max_retries() -> u32 {
    1
}

fn default_guard_breaker_threshold() -> u32 {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    #[serde(default = "default_level")]
    pub level: String,
    #[serde(default = "default_log_dir")]
    pub dir: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_level(),
            dir: default_log_dir(),
        }
    }
}

fn default_level() -> String {
    "info".into()
}

fn default_log_dir() -> String {
    "~/.llaia/logs".into()
}

/// llm family：可承载 `[model.<id>]` 条目的端点类型（同一 OpenAI 兼容端点可同时
/// 挂 chat/tts/image 多能力模型——kind 分在 model 上而非 provider 上的根因）。
pub const LLM_PROVIDER_TYPES: &[&str] = &["openai_compatible", "anthropic", "gemini"];

/// service family：纯凭据服务（搜索/抽取），无 model 条目、不进添加模型的 probe 列表。
/// 统一为单一 `search` 类型；具体平台由 `platform` 字段判别（各家 wire 协议不同，
/// 端点 URL 是编译期常量、配置不消费——base_url 对该 family 无意义）。
pub const SERVICE_PROVIDER_TYPES: &[&str] = &["search"];

/// P8 短暂数日用过的旧 service type 写法（load 期归一化为 search + platform）
pub const LEGACY_SERVICE_TYPES: &[&str] = &["tavily", "baidu", "brave"];

/// search 类型下支持的平台（`platform` 字段合法值；新增搜索商 = 加一个 variant +
/// 对应 adapter，无共享协议前不合并进同一 wire 实现）
pub const SEARCH_PLATFORMS: &[&str] = &["tavily", "baidu", "brave"];

/// 一个 provider 端点（连接信息）：统一连接注册表条目。`type` 即判别器，分两族：
///
/// - **llm family**（`openai_compatible`/`anthropic`/`gemini`）：可被 `[model.<id>]`
///   条目引用，出现在添加模型的 probe 列表
/// - **service family**（`search`）：纯凭据，`platform` 选具体平台
///   （tavily/baidu/brave），`[tools.search]` / `web_fetch.extract_provider` 按引用消费
///
/// `type` 缺省 = openai_compatible（存量习惯保留）；**显式未知值在 `Config::load`
/// 报错**——静默回退会让打错的 service type 变成一个 LLM provider 蹲进 probe 列表。
/// 旧写法 `type = "tavily"` / `"baidu"` / `"brave"` 在加载期归一化为
/// `type = "search"` + 对应 `platform`（P8 短暂数日，不值得留正式别名）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// 端点类型，见上。`deny_unknown_fields` 同时是 P8 一步到位的安全网：
    /// 旧配置的 `[provider.<id>.<alias>]` model 子表在此显式报错而非静默丢失。
    #[serde(rename = "type", default)]
    pub provider_type: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    /// search 类型的平台判别（tavily/baidu/brave）；llm family 不消费。
    /// 缺省 None：load 期校验对 type=search 强制要求该字段存在且合法。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// 兼容覆盖层 `[provider.<id>.compat.*]`；仅 llm family 的 chat 请求消费。
    /// 优先级高于 base_url 自动探测；未设置时按 base_url 子串探测（ollama/llamacpp）。
    #[serde(default)]
    pub compat: Option<CompatConfig>,
}

impl ProviderConfig {
    /// 解析后的有效 type（缺省回退 openai_compatible）
    pub fn effective_type(&self) -> &str {
        if self.provider_type.is_empty() {
            "openai_compatible"
        } else {
            &self.provider_type
        }
    }

    /// search 平台（type=search 时必有的判别值；其他 family 返回 None）
    pub fn search_platform(&self) -> Option<&str> {
        if self.is_service() {
            self.platform.as_deref()
        } else {
            None
        }
    }

    /// 是否 service family（纯凭据，不承载 model 条目）
    pub fn is_service(&self) -> bool {
        SERVICE_PROVIDER_TYPES.contains(&self.effective_type())
    }
}

/// 模型能力分型（P8）：kind 决定 wire API 归属与 WebUI 表单形态。
/// 单选——一个条目只归属一种 wire API；正交能力位（multimodal 等）走 `capabilities`。
/// 缺省 Chat：probe 结果大多数是 LLM，未选 kind 时落在这里（SiliconFlow 类端点
/// 会混入非 LLM 模型，用户可改）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    #[default]
    Chat,
    Tts,
    Image,
    Embedding,
}

impl ModelKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Tts => "tts",
            Self::Image => "image",
            Self::Embedding => "embedding",
        }
    }
}

fn is_chat(k: &ModelKind) -> bool {
    *k == ModelKind::Chat
}

/// 模型目录条目（P8）：全局唯一 model id，引用 llm family provider。
///
/// serde 方向守则（沿 2026-09-09 enabled 教训）：bool/enum 缺省值的 skip 方向是
/// 「等于缺省时省略」，反了会在 put_config replace 合并下静默蒸发/回读漂移。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    /// provider id 引用；指向 service family provider 在 `Config::load` 报错
    pub provider: String,
    /// 服务端模型名（请求体 model 字段）
    pub model: String,
    /// 能力分型；skip = 盘面干净，回读缺省即 chat
    #[serde(default, skip_serializing_if = "is_chat")]
    pub kind: ModelKind,
    /// 是否对外可见。**只管可发现性**（下拉列表、`/models` 目录），
    /// `model_from_ref` 仍接受显式引用——关掉当前 `agent.model` 指向的模型
    /// 不该让下次启动直接失败。skip 方向：必须 true 时省略（沿 enabled 教训）。
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// 模型上下文窗口大小（tokens），用于判断何时触发自动压缩。
    /// 未配置时启动后从服务端懒探测（llama.cpp /props 或 Ollama /api/show）；
    /// 探测不到的端点（多为远程 API）回退乐观默认 128000。取 min(配置值, 探测值)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_size: Option<usize>,
    /// 单次生成最大 token 数。Anthropic Messages API 必传 max_tokens，
    /// 未配置时默认 4096；OpenAI 兼容 provider 忽略此项。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,
    /// 是否用 OpenAI function calling 协议（发 `tools` + 期待结构化 `tool_calls`）。
    /// `None`（缺省）= auto：跟随 `Compat` 探测/配置结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_tool_calling: Option<bool>,
    /// 思考能力声明（P1 D2）：`[model.<id>.thinking]`。None（未写段）= 能力未知。
    /// None 时省略——provider/model 子树走 replace 合并（缺失即删），
    /// 若写成「Some 时省略」会让已声明的 thinking 在 WebUI 保存时静默蒸发。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    /// kind=image 的默认输出尺寸（"WIDTHxHEIGHT"）；其他 kind 上出现 → load warn。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    /// 正交能力位数组，v1 仅 "multimodal"；未知值 load warn（向前兼容新能力位）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

impl ModelEntry {
    /// 是否声明某能力位
    pub fn has_capability(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }
}

fn default_true() -> bool {
    true
}

/// 思考能力声明（P1 D2）：`[provider.<id>.<model>.thinking]`。
/// 三个字段全部「缺省 = unknown」：不写就是能力未知，请求层按 unknown 语义处理
/// （Off 意图走 legacy 兜底、档位意图被门禁拒绝、回显 unknown 三态）。
/// 字段值全部来自 wire 实测（probe 控制组），不做任何模型名猜测。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    /// 模型默认档位：`none|low|medium|high|max`；缺省或字面 `unknown` = 未知
    /// （实测只能断言「非 none」时写 unknown）。仅用于生效态回显，不发请求。
    #[serde(default, deserialize_with = "deserialize_level_or_unknown")]
    pub default: Option<crate::provider::ThinkingLevel>,
    /// 档位怎么表达：`reasoning_effort`（顶层字段，服务端校验非法值 ⇒ 可信）或
    /// `none`（不给旋钮——端点不校验档位，发了也白发）。缺省 = unknown，按 none 处理。
    #[serde(default)]
    pub level_wire: Option<crate::provider::ThinkingLevelWire>,
    /// 「关」怎么表达：`enable_thinking_false`（嵌套 chat_template_kwargs，llama.cpp 实测）/
    /// `thinking_disabled`（DeepSeek 方言，实测 400 规则来自错误消息）/
    /// `reasoning_effort_none`（Ollama/agnes 实测唯一有效的关）/
    /// `unsupported`（GLM 实测 400，任何关意图短路）。缺省 = unknown → legacy 兜底
    /// （compat.disable_thinking_template 门内注入 chat_template_kwargs）。
    #[serde(default)]
    pub off_wire: Option<crate::provider::ThinkingOffWire>,
    /// 是否把历史 assistant 消息的思考留存**逐字**回传出站（P2-11，D6：默认关）。
    /// 开启前提是服务端确认消费该字段——llama.cpp 新 build（≥10867）有
    /// `--reasoning-preserve`（默认 enabled）；远端 b10645 实测在请求解析阶段丢弃、
    /// Ollama 双通路控制组实测不消费（plan「待实测 6」）。红线：缺就缺，
    /// 框架绝不补空或代写；未写 `[thinking]` 段时请求体不发任何新键（性质 1）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserve: Option<bool>,
}

/// `default = "unknown"` → None（未知）；其余按档位名解析，拼错直接报错不静默吞。
fn deserialize_level_or_unknown<'de, D>(
    d: D,
) -> Result<Option<crate::provider::ThinkingLevel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    if s == "unknown" {
        return Ok(None);
    }
    crate::provider::ThinkingLevel::parse(&s)
        .map(Some)
        .ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown thinking level '{s}' (expected none|low|medium|high|max|unknown)"
            ))
        })
}

fn is_true(b: &bool) -> bool {
    *b
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// 引用 `[model.<id>]` 目录条目 id（P8），例如 "qwen3"
    pub model: String,
    /// [deprecated] workspace 字段已移除（P3-a 起自动推导，见 derive_workspace），
    /// 存量配置里的该键由 serde 直接忽略（未开 deny_unknown_fields）。
    /// [deprecated] 缺省时从 agent 家目录推导为 <workspace>/SOUL.md 等
    pub soul: Option<String>,
    pub user: Option<String>,
    pub memory: Option<String>,
    /// 工具黑名单：列出的工具子 Agent 不可用。默认空（继承所有工具）
    #[serde(default)]
    pub denied_tools: Vec<String>,
    /// 委派超时秒数（仅子 Agent 生效）。默认 120
    #[serde(default = "default_delegate_timeout")]
    pub delegate_timeout: u64,
    /// 备用模型链（model ref 列表）：主模型请求失败时按序降级。
    /// 例：fallback = ["local.small", "cloud.big"]
    #[serde(default)]
    pub fallback: Vec<String>,
    /// MEMORY.md 注入 system prompt 的 token 预算（chars/4 启发式）。
    /// 超限时最旧溢出段经 compact_provider 摘要压缩（无则硬截断保留近期）。
    /// 默认 4000。SOUL/USER 永留全量、不计入此预算（见 ADR-0025）。
    #[serde(default = "default_memory_token_budget")]
    pub memory_token_budget: usize,
}

pub fn default_memory_token_budget() -> usize {
    4000
}

impl AgentConfig {
    /// 推导 agent workspace 根路径
    /// main → config_dir/workspace/
    /// 子 agent → config_dir/workspace/subagent/<alias>/
    pub fn derive_workspace(&self, config_dir: &std::path::Path, alias: &str) -> PathBuf {
        if alias == "main" {
            config_dir.join("workspace")
        } else {
            config_dir.join("workspace").join("subagent").join(alias)
        }
    }
}

fn default_delegate_timeout() -> u64 {
    120
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelsConfig {
    #[serde(default)]
    pub qq: QqConfig,
    #[serde(default)]
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub dingtalk: DingtalkConfig,
    #[serde(default)]
    pub wechat: WechatConfig,
    #[serde(default)]
    pub mail: MailConfig,
    #[serde(default)]
    pub feishu: FeishuConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QqConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub app_id: String,
    /// QQ 开放平台 AppSecret（用于换 access_token）
    #[serde(default)]
    pub app_secret: String,
    #[serde(default = "default_qq_confirm")]
    pub confirm_mode: String,
    /// cron 主动推送的默认目标 openid（手动指定；留空则用运行时自动捕获的 openid，
    /// 自动捕获值持久化在 workspace/channel_state.json）
    #[serde(default)]
    pub owner_openid: String,
}

impl Default for QqConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: String::new(),
            app_secret: String::new(),
            confirm_mode: default_qq_confirm(),
            owner_openid: String::new(),
        }
    }
}

fn default_qq_confirm() -> String {
    "none".into()
}

/// Telegram 频道：官方 Bot API + long polling，免公网回调。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    #[serde(default)]
    pub enabled: bool,
    /// BotFather 颁发的 token，支持 ${VAR} 引用 .env
    #[serde(default)]
    pub bot_token: String,
    /// 只响应此 chat 的消息（单用户安全锁）；0 = 不限制
    #[serde(default)]
    pub allow_chat_id: i64,
    /// cron 主动推送的默认目标 chat_id（手动指定）；0 = 未指定，回退到 allow_chat_id
    #[serde(default)]
    pub owner_chat_id: i64,
    /// API base（测试可指到 mock），默认官方地址
    #[serde(default = "default_telegram_api_base")]
    pub api_base: String,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: String::new(),
            allow_chat_id: 0,
            owner_chat_id: 0,
            api_base: default_telegram_api_base(),
        }
    }
}

fn default_telegram_api_base() -> String {
    "https://api.telegram.org".into()
}

/// 钉钉频道：开放平台机器人 + Stream Mode WebSocket，免公网回调。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DingtalkConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 应用凭证（开发者后台 client_id / client_secret），支持 ${VAR} 引用 .env
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    /// 只响应此 staffId 的消息（单用户安全锁）；空 = 不限制
    #[serde(default)]
    pub allow_staff_id: String,
    /// gateway API base（测试可指到 mock），默认官方地址
    #[serde(default = "default_dingtalk_api_base")]
    pub api_base: String,
}

impl Default for DingtalkConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            client_id: String::new(),
            client_secret: String::new(),
            allow_staff_id: String::new(),
            api_base: default_dingtalk_api_base(),
        }
    }
}

fn default_dingtalk_api_base() -> String {
    "https://api.dingtalk.com".into()
}

/// 微信 ClawBot 频道：腾讯官方 openclaw-weixin（ilink bot）接口，扫码登录 + 长轮询免公网。
/// 登录态（token / sync_buf / context_tokens）不落 config，持久化在 <config_dir>/wechat_state.json。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WechatConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 只响应此 ilink_user_id 的消息（单用户安全锁）；空 = 不限制
    #[serde(default)]
    pub allow_user_id: String,
    /// cron 主动推送的默认目标 ilink_user_id（手动指定）；空 = 用运行时自动捕获的
    /// （自动捕获值持久化在 wechat_state.json 的 owner_user_id）
    #[serde(default)]
    pub owner_user_id: String,
    /// ilink API base（测试可指到 mock），默认官方地址
    #[serde(default = "default_wechat_base_url")]
    pub base_url: String,
    /// 媒体 CDN base
    #[serde(default = "default_wechat_cdn_base_url")]
    pub cdn_base_url: String,
}

impl Default for WechatConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_user_id: String::new(),
            owner_user_id: String::new(),
            base_url: default_wechat_base_url(),
            cdn_base_url: default_wechat_cdn_base_url(),
        }
    }
}

fn default_wechat_base_url() -> String {
    "https://ilinkai.weixin.qq.com".into()
}

fn default_wechat_cdn_base_url() -> String {
    "https://novac2c.cdn.weixin.qq.com/c2c".into()
}

/// 邮箱频道：IMAP 轮询收件 + SMTP 发信（个人助理入口，单用户安全锁）。
/// 收信走 IMAP（默认 993 隐式 TLS），发信用 SMTP（465 隐式 TLS / 587 STARTTLS）。
/// 仅响应 owner_email 发来的邮件，避免自动回复外部信件造成邮件循环。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailConfig {
    #[serde(default)]
    pub enabled: bool,
    /// IMAP 服务器（如 imap.gmail.com）
    #[serde(default)]
    pub imap_server: String,
    /// IMAP 端口（默认 993，隐式 TLS）
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    /// IMAP 登录账号（通常即邮箱地址）
    #[serde(default)]
    pub imap_user: String,
    /// IMAP 密码 / 授权码，支持 ${VAR} 引用 .env
    #[serde(default)]
    pub imap_pass: String,
    /// SMTP 服务器（如 smtp.gmail.com）
    #[serde(default)]
    pub smtp_server: String,
    /// SMTP 端口（默认 465 隐式 TLS；587 走 STARTTLS）
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    /// SMTP 登录账号（留空则复用 imap_user）
    #[serde(default)]
    pub smtp_user: String,
    /// SMTP 密码 / 授权码（留空则复用 imap_pass），支持 ${VAR}
    #[serde(default)]
    pub smtp_pass: String,
    /// 轮询间隔（秒），默认 30
    #[serde(default = "default_mail_poll")]
    pub poll_interval_secs: u64,
    /// 监听的邮箱文件夹，默认 INBOX
    #[serde(default = "default_mail_mailbox")]
    pub mailbox: String,
    /// 单用户安全锁：只响应此地址发来的邮件；为空则响应所有发件人（谨慎）
    #[serde(default)]
    pub owner_email: String,
    /// 发信显示的发件人名，默认 LLAIA
    #[serde(default = "default_mail_from_name")]
    pub from_name: String,
    /// 处理后标记已读，默认 true（避免重复处理）
    #[serde(default = "default_true")]
    pub mark_seen: bool,
    /// 单封邮件附件大小上限（MB），默认 10；超出的附件仅提示不下载
    #[serde(default = "default_mail_max_attach")]
    pub max_attachment_mb: u64,
}

impl Default for MailConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            imap_server: String::new(),
            imap_port: default_imap_port(),
            imap_user: String::new(),
            imap_pass: String::new(),
            smtp_server: String::new(),
            smtp_port: default_smtp_port(),
            smtp_user: String::new(),
            smtp_pass: String::new(),
            poll_interval_secs: default_mail_poll(),
            mailbox: default_mail_mailbox(),
            owner_email: String::new(),
            from_name: default_mail_from_name(),
            mark_seen: true,
            max_attachment_mb: default_mail_max_attach(),
        }
    }
}

fn default_imap_port() -> u16 {
    993
}
fn default_smtp_port() -> u16 {
    465
}
fn default_mail_poll() -> u64 {
    30
}
fn default_mail_mailbox() -> String {
    "INBOX".into()
}
fn default_mail_from_name() -> String {
    "LLAIA".into()
}
fn default_mail_max_attach() -> u64 {
    10
}

/// 飞书/Lark 频道：开放平台事件订阅「长连接」模式（WebSocket 免公网回调）。
/// 协议细节见 zeroclaw-channels/src/lark.rs（Apache-2.0 / MIT）：
/// 建连向 POST {ws_base}/callback/ws/endpoint 换 wss 地址，收 protobuf 二进制帧，
/// 收到事件 3 秒内回 ACK 帧，回复走 POST {api_base}/im/v1/messages。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 飞书应用 App ID，支持 ${VAR} 引用 .env
    #[serde(default)]
    pub app_id: String,
    /// 飞书应用 App Secret，支持 ${VAR} 引用 .env
    #[serde(default)]
    pub app_secret: String,
    /// 只响应此 open_id 的消息（单用户安全锁）；空 = 不限制
    #[serde(default)]
    pub allow_open_id: String,
    /// 群聊中是否仅在被 @ 时回复（true=仅 @ 时回复，false=群内所有消息都回复）。
    /// 私聊（p2p）不受影响，始终回复。默认 false。
    #[serde(default)]
    pub mention_only: bool,
    /// API base（测试可指到 mock），默认官方地址
    #[serde(default = "default_feishu_api_base")]
    pub api_base: String,
    /// WS base（取 wss 地址用），默认官方地址
    #[serde(default = "default_feishu_ws_base")]
    pub ws_base: String,
}

impl Default for FeishuConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: String::new(),
            app_secret: String::new(),
            allow_open_id: String::new(),
            mention_only: false,
            api_base: default_feishu_api_base(),
            ws_base: default_feishu_ws_base(),
        }
    }
}

fn default_feishu_api_base() -> String {
    "https://open.feishu.cn/open-apis".into()
}

fn default_feishu_ws_base() -> String {
    "https://open.feishu.cn".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebUiConfig {
    /// 监听地址，默认 127.0.0.1（仅本机访问）
    #[serde(default = "default_web_host")]
    pub host: String,
    /// 监听端口，默认 51217（避开 8080 等 llama.cpp/常见服务端口）
    #[serde(default = "default_web_port")]
    pub port: u16,
    /// 鉴权 token；留空则启动时随机生成并打印日志
    #[serde(default)]
    pub token: String,
}

impl Default for WebUiConfig {
    fn default() -> Self {
        Self {
            host: default_web_host(),
            port: default_web_port(),
            token: String::new(),
        }
    }
}

fn default_web_host() -> String {
    "127.0.0.1".into()
}

fn default_web_port() -> u16 {
    51217
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolsConfig {
    #[serde(default)]
    pub terminal: TerminalToolConfig,
    /// 统一搜索配置：`provider` 为 service family provider 的 **id 引用**（P8），
    /// 空 = 不注册 search 工具；引用缺失/指向 llm family → 工具不注册 + warn
    #[serde(default)]
    pub search: SearchConfig,
    /// TTS（P5 T1）：`model` 为模型目录中 kind=tts 条目的 id 引用；
    /// base_url/api_key/服务端模型名全部来自该条目与其 provider
    #[serde(default)]
    pub tts: TtsConfig,
    /// 图片生成/编辑：`model` 为 kind=image 条目的 id 引用
    #[serde(default)]
    pub image_gen: ImageGenConfig,
    /// web_fetch 正文抽取与体积上限
    #[serde(default)]
    pub web_fetch: WebFetchConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalToolConfig {
    #[serde(default = "default_confirm")]
    pub confirm: String,
    #[serde(default = "default_whitelist")]
    pub whitelist: Vec<String>,
    /// 命令策略：blacklist（默认）/ whitelist / none
    #[serde(default = "default_command_policy")]
    pub command_policy: String,
    /// 仅 policy=whitelist 时生效
    #[serde(default = "default_command_whitelist")]
    pub command_whitelist: Vec<String>,
    /// 解释器内联载荷策略（T3）：approval（默认，`python -c` / `node -e` 等内联执行
    /// 命中即强制人审，即使落在 workspace 内）/ off（关闭该闸门）
    #[serde(default = "default_interpret_inline")]
    pub interpret_inline: String,
    /// 删除护栏（2026-09-17）：trash（默认，bound path 内 rm/del/Remove-Item 等破坏性
    /// 命令转 .trash/ 免审可恢复，界外仍强制审批）/ off（关闭，旧行为：受信目录内
    /// 静默真删——不建议）
    #[serde(default = "default_delete_guard")]
    pub delete_guard: String,
}

impl Default for TerminalToolConfig {
    fn default() -> Self {
        Self {
            confirm: default_confirm(),
            whitelist: default_whitelist(),
            command_policy: default_command_policy(),
            command_whitelist: default_command_whitelist(),
            interpret_inline: default_interpret_inline(),
            delete_guard: default_delete_guard(),
        }
    }
}

fn default_confirm() -> String {
    "whitelist".into()
}

fn default_whitelist() -> Vec<String> {
    vec![
        "ls".into(),
        "cat".into(),
        "grep".into(),
        "pwd".into(),
        "dir".into(),
    ]
}

fn default_command_policy() -> String {
    "blacklist".into()
}

fn default_interpret_inline() -> String {
    "approval".into()
}

fn default_delete_guard() -> String {
    "trash".into()
}

fn default_command_whitelist() -> Vec<String> {
    Vec::new()
}

/// `web_fetch` 工具配置：控制正文抽取与体积上限。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebFetchConfig {
    /// 返回文本的最大字符数；超出截断并追加提示。默认 20000（约 6~8k token），
    /// 足够覆盖一篇新闻正文，又避免把整站 HTML/JS 灌进上下文。
    #[serde(default = "default_web_fetch_max_chars")]
    pub max_chars: usize,
    /// 服务端抽取的 provider id 引用（P8：须为支持 extract 能力的 service family
    /// provider，如 tavily）。空 = 本地抽取；引用无效 → 自动退化本地抽取 + warn。
    #[serde(default)]
    pub extract_provider: String,
}

impl Default for WebFetchConfig {
    fn default() -> Self {
        Self {
            max_chars: default_web_fetch_max_chars(),
            extract_provider: String::new(),
        }
    }
}

fn default_web_fetch_max_chars() -> usize {
    20_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    /// 搜索 provider 的 id 引用（P8：service family，如 tavily/baidu/brave）。
    /// 空 = 不注册 search 工具。
    #[serde(default)]
    pub provider: String,
    /// 默认返回条数
    #[serde(default = "default_search_top_k")]
    pub top_k: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            provider: String::new(),
            top_k: default_search_top_k(),
        }
    }
}

fn default_search_top_k() -> usize {
    8
}

/// TTS 配置（P5 T1）：OpenAI 兼容 `/audio/speech` 端点。
/// v1 用 OpenAI TTS API（可测、稳定）；edge-tts（WS + Sec-MS-GEC 签名、
/// 不可测且接口脆弱）记 v2 待研究。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsConfig {
    /// 是否注册 `tts` 工具（还需 model 引用解析到 kind=tts 条目）
    #[serde(default)]
    pub enabled: bool,
    /// 模型目录条目 id（kind=tts）；端点与 key 来自条目的 provider，服务端模型名
    /// 来自条目的 `model` 字段。引用无效 → 工具不注册 + warn
    #[serde(default)]
    pub model: String,
    /// 默认音色，默认 alloy（调用参数，非模型属性）
    #[serde(default = "default_tts_voice")]
    pub voice: String,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: String::new(),
            voice: default_tts_voice(),
        }
    }
}

fn default_tts_voice() -> String {
    "alloy".into()
}

/// Image generation/editing config: OpenAI-compatible `/images/generations`
/// and `/images/edits` endpoints. Works with any compatible backend
/// (sd-server from stable-diffusion.cpp, agnes, OpenAI, ...).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenConfig {
    /// Register `image_gen` / `image_edit` tools (also requires the model ref to
    /// resolve to a kind=image entry; local sd-server typically needs no key,
    /// so an empty provider api_key is fine)
    #[serde(default)]
    pub enabled: bool,
    /// 模型目录条目 id（kind=image）；端点与 key 来自条目的 provider，服务端模型名
    /// 与默认尺寸来自条目的 `model` / `size` 字段。引用无效 → 工具不注册 + warn
    #[serde(default)]
    pub model: String,
    /// Per-request timeout in seconds (local diffusion can be slow)
    #[serde(default = "default_image_gen_timeout")]
    pub timeout_secs: u64,
}

impl Default for ImageGenConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: String::new(),
            timeout_secs: default_image_gen_timeout(),
        }
    }
}

fn default_image_gen_timeout() -> u64 {
    300
}

/// P8 一步到位的兼容层：v0.5 旧结构 → 模型目录，反序列化**前**对 raw TOML 原地改写。
///
/// 处理三类遗留形态（互相独立，缺哪补哪）：
/// 1. `[provider.<pid>.<alias>]` model 子表（含嵌套 thinking）→ `[model."<pid>.<alias>"]`
///    目录条目。model id 刻意沿用 `"pid.alias"` 两段式——agent `model` / `fallback` /
///    `compact_model` / `vision_model` 里的旧引用无需任何改动即继续解析。
/// 2. `[tools.tavily|baidu|brave]` 凭据段 → 同名 service family provider
///    （`[tools.search].provider = "tavily"` 的旧语义是类型名，恰好等于新建 id，直通）。
/// 3. `[tools.tts]` / `[tools.image_gen]` 内联 base_url/api_key/model → provider +
///    目录条目 + 引用改写；`[tools.web_fetch].use_tavily_extract` → `extract_provider`。
///
/// 迁移只改内存态：磁盘文件保持原样（避免 toml 序列化毁掉注释与手排格式）。
/// 每次启动都会重复迁移，属于幂等无害操作；在 WebUI 保存一次即可把新结构写盘，
/// 届时迁移按 id 冲突检测自动跳过已迁移条目。返回的 notice 逐条由调用方 warn。
fn migrate_legacy_config(raw: &mut toml::Value) -> Vec<String> {
    let mut notices = Vec::new();
    let Some(root) = raw.as_table_mut() else {
        return notices;
    };

    // 全程 owned 操作（remove 下来处理完再写回），避免同时持有 provider / model /
    // tools 三棵子树的可变借用。pending_* 收集本轮要写入的条目，统一做 id 冲突检测。
    const PROVIDER_RESERVED: [&str; 4] = ["type", "base_url", "api_key", "compat"];

    let mut providers = root
        .remove("provider")
        .and_then(|v| v.as_table().cloned())
        .unwrap_or_default();
    let mut tools = root
        .remove("tools")
        .and_then(|v| v.as_table().cloned())
        .unwrap_or_default();
    let mut catalog = root
        .remove("model")
        .and_then(|v| v.as_table().cloned())
        .unwrap_or_default();

    // ---- 1. provider 层的 legacy model 子表 → 目录条目 ----
    let mut pids: Vec<String> = providers.keys().cloned().collect();
    pids.sort(); // 输出顺序稳定，notice 可读
    for pid in pids {
        let Some(ptbl) = providers.get_mut(&pid).and_then(|v| v.as_table_mut()) else {
            continue;
        };
        let aliases: Vec<String> = ptbl
            .iter()
            .filter(|(k, v)| !PROVIDER_RESERVED.contains(&k.as_str()) && v.is_table())
            .map(|(k, _)| k.clone())
            .collect();
        for alias in aliases {
            let Some(sub) = ptbl.remove(&alias).and_then(|v| v.as_table().cloned()) else {
                continue;
            };
            // 服务端模型名是目录条目的硬要求，缺失时丢弃并提示（P7 配置不该出现）
            let Some(server_model) = sub.get("model").and_then(|v| v.as_str()).to_owned() else {
                notices.push(format!(
                    "legacy [provider.{pid}.{alias}] has no `model` field; dropped"
                ));
                continue;
            };
            let model_id = format!("{pid}.{alias}");
            if catalog.contains_key(&model_id) {
                // 用户已手工迁移过同名条目：显式 [model] 条目优先，legacy 段让位
                notices.push(format!(
                    "model {model_id} already defined in [model.\"{model_id}\"]; \
                     legacy [provider.{pid}.{alias}] table dropped"
                ));
                continue;
            }
            let mut entry = toml::Table::new();
            entry.insert("provider".into(), toml::Value::from(pid.clone()));
            entry.insert("model".into(), toml::Value::from(server_model));
            for key in [
                "context_size",
                "max_tokens",
                "enabled",
                "native_tool_calling",
                "thinking",
                "size",
            ] {
                if let Some(v) = sub.get(key) {
                    entry.insert(key.into(), v.clone());
                }
            }
            catalog.insert(model_id.clone(), toml::Value::Table(entry));
            notices.push(format!(
                "migrated legacy [provider.{pid}.{alias}] -> [model.\"{model_id}\"]"
            ));
        }
    }

    // 追加 provider（不存在才写，显式配置优先）
    fn ensure_provider(
        providers: &mut toml::Table,
        id: &str,
        ptype: &str,
        base_url: &str,
        api_key: &str,
        notices: &mut Vec<String>,
    ) {
        if providers.contains_key(id) {
            return;
        }
        let mut p = toml::Table::new();
        p.insert("type".into(), toml::Value::from(ptype));
        p.insert("base_url".into(), toml::Value::from(base_url));
        p.insert("api_key".into(), toml::Value::from(api_key));
        providers.insert(id.into(), toml::Value::Table(p));
        notices.push(format!(
            "migrated legacy [tools.{id}] credentials -> [provider.{id}] (type = \"{ptype}\")"
        ));
    }
    // 追加目录条目（不存在才写）
    fn ensure_model(
        catalog: &mut toml::Table,
        id: &str,
        provider: &str,
        server_model: &str,
        kind: &str,
        size: Option<toml::Value>,
        notices: &mut Vec<String>,
    ) {
        if catalog.contains_key(id) {
            return;
        }
        let mut entry = toml::Table::new();
        entry.insert("provider".into(), toml::Value::from(provider));
        entry.insert("model".into(), toml::Value::from(server_model));
        entry.insert("kind".into(), toml::Value::from(kind));
        if let Some(size) = size {
            entry.insert("size".into(), size);
        }
        catalog.insert(id.into(), toml::Value::Table(entry));
        notices.push(format!(
            "migrated legacy [tools.{provider}] endpoint -> [model.\"{id}\"] (kind = \"{kind}\")"
        ));
    }

    // ---- 2. 搜索凭据段 → service family provider（type=search + platform 判别） ----
    for name in ["tavily", "baidu", "brave"] {
        let Some(sec) = tools.remove(name).and_then(|v| v.as_table().cloned()) else {
            continue;
        };
        let api_key = sec
            .get("api_key")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        ensure_provider(&mut providers, name, "search", "", &api_key, &mut notices);
        // platform 补在 ensure_provider 之后：仅当条目是本次新建时写入，
        // 用户已有同名 type=search provider 时不覆盖其显式 platform
        if let Some(p) = providers.get_mut(name).and_then(|v| v.as_table_mut()) {
            if !p.contains_key("platform") {
                p.insert("platform".into(), toml::Value::from(name));
            }
        }
        // [tools.search].provider 旧语义是类型名（tavily/baidu/brave），与新建
        // provider id 恰好同串，无需改写；其余取值留给 load 校验去 warn。
    }

    // ---- 3a. tts 内联端点 → provider + kind=tts 条目 ----
    if let Some(tts) = tools.get_mut("tts").and_then(|v| v.as_table_mut()) {
        let base = tts
            .remove("base_url")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let api_key = tts
            .remove("api_key")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let server_model = tts
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if !base.trim().is_empty() && !server_model.trim().is_empty() {
            let model_id = format!("tts.{server_model}");
            ensure_provider(
                &mut providers,
                "tts",
                "openai_compatible",
                &base,
                &api_key,
                &mut notices,
            );
            ensure_model(
                &mut catalog,
                &model_id,
                "tts",
                &server_model,
                "tts",
                None,
                &mut notices,
            );
            tts.insert("model".into(), toml::Value::from(model_id));
        } else if tts.get("enabled").and_then(|v| v.as_bool()) == Some(true) {
            // 迁移条件不满足：legacy 键已摘除，残留的 model 值会被当作目录引用
            // 解析失败 → 工具不注册 + warn，与旧版"配置不完整则不可用"等价
            notices.push(
                "tools.tts.enabled but legacy base_url/model incomplete; configure \
                 [model.<id>] (kind = \"tts\") and point tools.tts.model at it"
                    .to_string(),
            );
        }
    }

    // ---- 3b. image_gen 内联端点 → provider + kind=image 条目 ----
    if let Some(ig) = tools.get_mut("image_gen").and_then(|v| v.as_table_mut()) {
        let base = ig
            .remove("base_url")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let api_key = ig
            .remove("api_key")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let size = ig.remove("size");
        let server_model = ig
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if !base.trim().is_empty() && !server_model.trim().is_empty() {
            // 旧配置的 base_url 常是完整端点（…/images/generations）；新代码按
            // provider.base_url + "/images/generations" 拼接，须剥掉尾巴防双写
            let provider_base = base
                .trim_end_matches('/')
                .trim_end_matches("/images/generations")
                .to_string();
            let model_id = format!("image_gen.{server_model}");
            ensure_provider(
                &mut providers,
                "image_gen",
                "openai_compatible",
                &provider_base,
                &api_key,
                &mut notices,
            );
            ensure_model(
                &mut catalog,
                &model_id,
                "image_gen",
                &server_model,
                "image",
                size,
                &mut notices,
            );
            ig.insert("model".into(), toml::Value::from(model_id));
        } else if ig.get("enabled").and_then(|v| v.as_bool()) == Some(true) {
            notices.push(
                "tools.image_gen.enabled but legacy base_url/model incomplete; configure \
                 [model.<id>] (kind = \"image\") and point tools.image_gen.model at it"
                    .to_string(),
            );
        }
    }

    // ---- 3c. web_fetch 抽取开关改引用 ----
    if let Some(wf) = tools.get_mut("web_fetch").and_then(|v| v.as_table_mut()) {
        if let Some(use_tavily) = wf.remove("use_tavily_extract").and_then(|v| v.as_bool()) {
            if use_tavily {
                wf.insert("extract_provider".into(), toml::Value::from("tavily"));
                notices.push(
                    "migrated tools.web_fetch.use_tavily_extract -> extract_provider = \"tavily\""
                        .to_string(),
                );
            }
        }
    }

    // ---- 写回（无迁移发生时写回空表也无副作用：Config 各字段均有 default） ----
    root.insert("provider".into(), toml::Value::Table(providers));
    root.insert("tools".into(), toml::Value::Table(tools));
    if !catalog.is_empty() {
        root.insert("model".into(), toml::Value::Table(catalog));
    }

    notices
}

impl Config {
    pub fn load(path: &PathBuf) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {:?}", path))?;
        // P8 兼容层：先解析成 raw Value，把 v0.5 的旧结构原地改写成模型目录形态
        //（[provider.<pid>.<alias>] 子表 → [model."<pid>.<alias>"]、[tools.tavily]
        // 等凭据段 → service family provider、tts/image_gen 内联端点 → 目录引用），
        // 再交给 serde 反序列化。agent 引用（"pid.alias"）恰好等于迁移后的 model id，
        // 因此旧配置无需任何手工修改即可加载。
        let mut raw: toml::Value = toml::from_str(&content)
            .with_context(|| format!("failed to parse config: {:?}", path))?;
        for notice in migrate_legacy_config(&mut raw) {
            tracing::warn!("{}", notice);
        }
        let mut config: Config = raw
            .try_into()
            .with_context(|| format!("failed to parse config: {:?}", path))?;
        // 向后兼容：旧 [channels.web] → 新 [webui]
        // ChannelsConfig 已无 web 字段，serde 会静默忽略 toml 里的 [channels.web]，
        // 这里用 raw toml 检测并迁移（不覆盖用户已显式设置的 [webui]）
        if let Ok(raw) = toml::from_str::<toml::Value>(&content) {
            let has_explicit_webui = raw.get("webui").is_some();
            if !has_explicit_webui {
                if let Some(old_web) = raw.get("channels").and_then(|c| c.get("web")) {
                    if !old_web.as_table().map(|t| t.is_empty()).unwrap_or(false) {
                        tracing::warn!(
                            "[channels.web] is deprecated, use [webui] instead. Migrating automatically."
                        );
                        if let Ok(old_cfg) = old_web.clone().try_into::<WebUiConfig>() {
                            config.webui = old_cfg;
                        }
                    }
                }
            }
        }
        // log.dir 未显式配置时（仍为 serde 默认值），跟随 config 文件所在目录
        // 注意：必须在 expand_paths 之前比较，因为 expand 后路径会变
        if config.log.dir == default_log_dir() {
            if let Some(parent) = path.parent() {
                config.log.dir = parent.join("logs").to_string_lossy().into_owned();
            }
        }
        // whitelist confirm_mode 废弃：warn + fallback 到 none
        if config.channels.qq.confirm_mode == "whitelist" {
            tracing::warn!(
                "channels.qq.confirm_mode = \"whitelist\" is deprecated, falling back to \"none\""
            );
            config.channels.qq.confirm_mode = "none".into();
        }
        // [agent.main] 是系统必需 section：缺失时 warn（保留降级启动能力，
        // 让 `llaia init` 模板和空配置仍可进入 serve 配置 WebUI）
        if !config.agent.contains_key("main") {
            tracing::warn!(
                "[agent.main] missing in config.toml — main agent will start in degraded mode"
            );
        }
        // P8：provider type 显式未知 → 报错（不静默回退——打错的 service type
        // 会变出一个 LLM provider 蹲进 probe 列表）。缺省（未写 type）回退 openai_compatible。
        // 旧写法 type = tavily/baidu/brave（P8 短暂数日的形态）先归一化为
        // search + platform 再校验，存量配置零改动。
        for (id, p) in config.provider.iter_mut() {
            if LEGACY_SERVICE_TYPES.contains(&p.provider_type.as_str()) {
                let legacy = std::mem::take(&mut p.provider_type);
                p.provider_type = "search".to_string();
                p.platform = Some(legacy.clone());
                tracing::warn!(
                    "provider.{id}: normalized legacy type '{legacy}' -> type = \"search\", platform = \"{legacy}\""
                );
            }
        }
        for (id, p) in &config.provider {
            let t = p.provider_type.trim();
            if !t.is_empty()
                && !LLM_PROVIDER_TYPES.contains(&t)
                && !SERVICE_PROVIDER_TYPES.contains(&t)
            {
                anyhow::bail!(
                    "provider.{id}: unknown type '{t}' (llm family: {}, service family: {})",
                    LLM_PROVIDER_TYPES.join("|"),
                    SERVICE_PROVIDER_TYPES.join("|")
                );
            }
            // search 类型：platform 必填且必须受支持（adapter 按它分派 wire 实现）
            if p.is_service() {
                match p.platform.as_deref() {
                    Some(pl) if SEARCH_PLATFORMS.contains(&pl) => {}
                    Some(pl) => anyhow::bail!(
                        "provider.{id}: unsupported search platform '{pl}' (supported: {})",
                        SEARCH_PLATFORMS.join("|")
                    ),
                    None => anyhow::bail!(
                        "provider.{id}: type = \"search\" requires a platform field ({})",
                        SEARCH_PLATFORMS.join("|")
                    ),
                }
            }
        }
        // P8：model 目录条目校验——provider 存在、非 service family、
        // anthropic/gemini 只能挂 chat（校验放加载期，不留运行时报错）
        for (mid, m) in &config.models {
            let Some(p) = config.provider.get(&m.provider) else {
                anyhow::bail!(
                    "model.{mid}: provider '{}' not configured ([provider.{}])",
                    m.provider,
                    m.provider
                );
            };
            if p.is_service() {
                anyhow::bail!(
                    "model.{mid}: references service-family provider '{}' which cannot host models",
                    m.provider
                );
            }
            if matches!(p.effective_type(), "anthropic" | "gemini") && m.kind != ModelKind::Chat {
                anyhow::bail!(
                    "model.{mid}: provider '{}' (type {}) only supports kind = \"chat\"",
                    m.provider,
                    p.effective_type()
                );
            }
            if m.size.is_some() && m.kind != ModelKind::Image {
                tracing::warn!(
                    model = mid.as_str(),
                    "model entry has `size` set but kind is not \"image\"; the field will be ignored"
                );
            }
            for cap in &m.capabilities {
                if cap != "multimodal" {
                    tracing::warn!(
                        model = mid.as_str(),
                        capability = cap.as_str(),
                        "unknown model capability (known: multimodal); kept for forward compatibility"
                    );
                }
            }
        }
        // compact_model 引用校验：避免拼写错误到运行时才暴露
        if let Some(m) = config.runtime.compact_model.clone() {
            if let Err(e) = Self::validate_chat_model_ref(&config, &m) {
                tracing::warn!(
                    model = m.as_str(),
                    error = %e,
                    "runtime.compact_model is not a usable chat model reference, will be ignored"
                );
                config.runtime.compact_model = None;
            }
        }
        // timezone 校验：非法 IANA 名降级为跟随系统，而不是让每一轮状态栏都错
        if let Some(tz) = &config.runtime.timezone {
            if tz.trim().is_empty() {
                config.runtime.timezone = None;
            } else if !crate::time::is_valid_tz(tz.trim()) {
                tracing::warn!(
                    timezone = tz.as_str(),
                    "runtime.timezone is not a valid IANA name, falling back to system local time"
                );
                config.runtime.timezone = None;
            }
        }
        // permission 档位校验：非法值降级为 default，而不是让每轮都按未知档位处理
        if let Some(p) = &config.runtime.permission {
            let v = p.trim().to_lowercase();
            if v.is_empty() || !matches!(v.as_str(), "default" | "read-only" | "yolo") {
                tracing::warn!(
                    permission = p.as_str(),
                    "runtime.permission is not one of default/read-only/yolo, falling back to default"
                );
                config.runtime.permission = None;
            } else {
                config.runtime.permission = Some(v);
            }
        }
        // 长任务心跳/超时校验：过小或倒挂的值会让心跳刷屏或直接秒停，兜底恢复默认
        if config.runtime.keepalive_interval_secs < 30 {
            tracing::warn!(
                secs = config.runtime.keepalive_interval_secs,
                "runtime.keepalive_interval_secs too small (<30s), using default 600"
            );
            config.runtime.keepalive_interval_secs = default_keepalive_interval();
        }
        if config.runtime.max_turn_duration_secs < 60 {
            tracing::warn!(
                secs = config.runtime.max_turn_duration_secs,
                "runtime.max_turn_duration_secs too small (<60s), using default 3600"
            );
            config.runtime.max_turn_duration_secs = default_max_turn_duration();
        }
        if config.runtime.max_turn_duration_secs <= config.runtime.keepalive_interval_secs {
            tracing::warn!(
                max = config.runtime.max_turn_duration_secs,
                keep = config.runtime.keepalive_interval_secs,
                "runtime.max_turn_duration_secs <= keepalive_interval_secs, no heartbeat will ever fire"
            );
        }
        // agent fallback 链引用校验：无效项移除（备用链是容错手段，不应阻塞启动）。
        // 先快照可用 chat model id 集合，避免 iter_mut 期间再借 config。
        let valid_chat_refs: std::collections::HashSet<String> = config
            .models
            .iter()
            .filter(|(_, m)| m.kind == ModelKind::Chat)
            .map(|(id, _)| id.clone())
            .collect();
        for (alias, agent_cfg) in config.agent.iter_mut() {
            let before = agent_cfg.fallback.len();
            agent_cfg.fallback.retain(|m| {
                if valid_chat_refs.contains(m) {
                    true
                } else {
                    tracing::warn!(
                        agent = alias.as_str(),
                        model = m.as_str(),
                        "agent fallback entry is not a usable chat model reference, removed"
                    );
                    false
                }
            });
            if agent_cfg.fallback.len() != before {
                tracing::debug!(agent = alias.as_str(), "fallback chain sanitized");
            }
        }
        // disabled model（enabled = false）的间接引用收敛。WebUI put_config 不走 load，
        // 那边单独也调一次，否则「在 WebUI 里关掉模型」要重启才生效。
        config.reconcile_disabled_models();
        config.expand_paths()?;
        Ok(config)
    }

    /// 展开 `~` 和 `${VAR}` 环境变量引用。
    ///
    /// - `~` / `~/path` → 用户 home 目录
    /// - `${VAR}` → 环境变量值（变量名须匹配 `[A-Z_][A-Z0-9_]*`，找不到报错）
    ///
    /// 顺序：先 env 后 tilde（env 值里可以含 `~` 不会被二次展开，tilde 后不再处理 env）
    fn expand_paths(&mut self) -> Result<()> {
        let expand = |s: &str| -> Result<String> { expand_string(s) };
        for a in self.agent.values_mut() {
            a.soul = a.soul.as_ref().map(|s| expand(s)).transpose()?;
            a.user = a.user.as_ref().map(|s| expand(s)).transpose()?;
            a.memory = a.memory.as_ref().map(|s| expand(s)).transpose()?;
        }
        for p in self.provider.values_mut() {
            p.base_url = expand(&p.base_url)?;
            p.api_key = expand(&p.api_key)?;
        }
        self.channels.qq.app_id = expand(&self.channels.qq.app_id)?;
        self.channels.qq.app_secret = expand(&self.channels.qq.app_secret)?;
        self.channels.telegram.bot_token = expand(&self.channels.telegram.bot_token)?;
        self.channels.dingtalk.client_id = expand(&self.channels.dingtalk.client_id)?;
        self.channels.dingtalk.client_secret = expand(&self.channels.dingtalk.client_secret)?;
        self.channels.mail.imap_server = expand(&self.channels.mail.imap_server)?;
        self.channels.mail.imap_user = expand(&self.channels.mail.imap_user)?;
        self.channels.mail.imap_pass = expand(&self.channels.mail.imap_pass)?;
        self.channels.mail.smtp_server = expand(&self.channels.mail.smtp_server)?;
        self.channels.mail.smtp_user = expand(&self.channels.mail.smtp_user)?;
        self.channels.mail.smtp_pass = expand(&self.channels.mail.smtp_pass)?;
        self.channels.feishu.app_id = expand(&self.channels.feishu.app_id)?;
        self.channels.feishu.app_secret = expand(&self.channels.feishu.app_secret)?;
        self.webui.token = expand(&self.webui.token)?;
        self.log.dir = expand(&self.log.dir)?;
        Ok(())
    }

    /// 把引用了 disabled model（`[model.<id>].enabled = false`）的**间接**
    /// 引用收敛到可用集合：
    ///
    /// - `runtime.compact_model` / `runtime.vision_model` → warn + 置 None（回退主模型）
    /// - `agent.<alias>.fallback` → 剔除（备用链不该指向故意停用的模型）
    /// - `agent.<alias>.model` → **刻意不碰**：那是用户显式选定的当前模型，`enabled`
    ///   只管可发现性（见 `ModelEntry::enabled`），硬拦会让"关掉当前模型"变成下次
    ///   启动即失败、且 WebUI 保存路径连改回来都走不通。
    ///
    /// 由 `Config::load` 与 WebUI `put_config` 各调一次（后者不走 `load`）。
    pub fn reconcile_disabled_models(&mut self) {
        let disabled: Vec<String> = self
            .models
            .iter()
            .filter(|(_, m)| !m.enabled)
            .map(|(id, _)| id.clone())
            .collect();
        if disabled.is_empty() {
            return;
        }
        let is_disabled = |r: &str| disabled.iter().any(|d| d == r);

        if let Some(m) = &self.runtime.compact_model {
            if is_disabled(m) {
                tracing::warn!(
                    model = m.as_str(),
                    "runtime.compact_model references a disabled model (enabled = false), falling back to the main model"
                );
                self.runtime.compact_model = None;
            }
        }
        if let Some(m) = &self.runtime.vision_model {
            if is_disabled(m) {
                tracing::warn!(
                    model = m.as_str(),
                    "runtime.vision_model references a disabled model (enabled = false), images will go to the main model"
                );
                self.runtime.vision_model = None;
            }
        }
        for (alias, agent_cfg) in self.agent.iter_mut() {
            let before = agent_cfg.fallback.len();
            agent_cfg.fallback.retain(|m| !is_disabled(m));
            let removed = before - agent_cfg.fallback.len();
            if removed > 0 {
                tracing::warn!(
                    agent = alias.as_str(),
                    count = removed,
                    "removed agent fallback entries referencing disabled models (enabled = false)"
                );
            }
        }
    }

    /// 校验 model id 引用是否指向**可作 chat provider** 的目录条目：
    /// 存在、enabled 不拦（只管可发现性）、kind 必须是 chat。
    /// 供 compact_model / fallback 的加载期收敛；`agent.model` 刻意不走此校验
    /// （disabled 不拦，避免 brick 当前模型）。
    pub fn validate_chat_model_ref(config: &Config, model_id: &str) -> Result<()> {
        let entry = config.models.get(model_id).ok_or_else(|| {
            anyhow::anyhow!("model.{model_id} not configured in [model.{model_id}]")
        })?;
        if entry.kind != ModelKind::Chat {
            anyhow::bail!(
                "model.{model_id} has kind \"{}\" — only kind = \"chat\" entries can serve as a chat model",
                entry.kind.as_str()
            );
        }
        Ok(())
    }

    /// 默认配置（首次启动用），结构最小化
    pub fn default_for_workspace(config_dir: &str) -> Self {
        let config_dir = shellexpand::tilde(config_dir).into_owned();

        let mut provider: HashMap<String, ProviderConfig> = HashMap::new();
        provider.insert(
            "default".into(),
            ProviderConfig {
                provider_type: "openai_compatible".into(),
                base_url: "http://localhost:11434/v1".into(),
                api_key: String::new(),
                platform: None,
                compat: None,
            },
        );

        let mut models: BTreeMap<String, ModelEntry> = BTreeMap::new();
        models.insert(
            "qwen".into(),
            ModelEntry {
                provider: "default".into(),
                model: "qwen2.5:7b".into(),
                kind: ModelKind::Chat,
                enabled: true,
                context_size: None,
                max_tokens: None,
                native_tool_calling: Some(true),
                thinking: None,
                size: None,
                capabilities: Vec::new(),
            },
        );

        let mut agent: HashMap<String, AgentConfig> = HashMap::new();
        agent.insert(
            "main".into(),
            AgentConfig {
                model: "qwen".into(),
                soul: None,
                user: None,
                memory: None,
                denied_tools: Vec::new(),
                delegate_timeout: default_delegate_timeout(),
                fallback: Vec::new(),
                memory_token_budget: default_memory_token_budget(),
            },
        );

        Config {
            runtime: RuntimeConfig::default(),
            log: LogConfig {
                level: default_level(),
                dir: format!("{}/logs", config_dir),
            },
            provider,
            models,
            agent,
            webui: WebUiConfig::default(),
            channels: ChannelsConfig::default(),
            tools: ToolsConfig::default(),
        }
    }
}

/// 展开字符串中的 `~` 和 `${VAR}` 环境变量引用。
/// - `~` / `~/path` → 用户 home 目录（shellexpand::tilde）
/// - `${VAR}` → 环境变量值（变量名须匹配 `[A-Z_][A-Z0-9_]*`）
///
/// 未定义的环境变量替换为空字符串并 warn，让 serve 能进入降级模式（WebUI 配置可用），
/// 而不是直接挂掉。用户在 WebUI 里补全 key 后热加载即可恢复。
/// 先展开 env，再展开 tilde（env 值里的 `~` 不会被二次展开）。
pub(crate) fn expand_string(s: &str) -> Result<String> {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    // 常量正则：模式串编译期写死且合法，构造不可能失败
    let re = RE.get_or_init(|| regex::Regex::new(r"\$\{([A-Z_][A-Z0-9_]*)\}").unwrap());

    let expanded = re.replace_all(s, |caps: &regex::Captures| match std::env::var(&caps[1]) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                var = &caps[1],
                error = %e,
                "environment variable referenced in config but not set, replacing with empty string (degraded mode)"
            );
            String::new()
        }
    });
    let result = shellexpand::tilde(&expanded).into_owned();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn thinking_config_toml_roundtrip_and_skip_direction() {
        // P1 D2：thinking 段解析；unknown 字面值 → None；skip 方向（enabled 教训）：
        // None 时序列化省略键（model 子树 replace 合并缺失即删，方向不能反）
        let mc: ModelEntry = toml::from_str(
            r#"
provider = "p"
model = "m"
[thinking]
default = "unknown"
level_wire = "reasoning_effort"
off_wire = "enable_thinking_false"
"#,
        )
        .unwrap();
        let t = mc.thinking.as_ref().unwrap();
        assert_eq!(t.default, None, "literal 'unknown' deserializes to None");
        assert_eq!(
            t.level_wire,
            Some(crate::provider::ThinkingLevelWire::ReasoningEffort)
        );
        assert_eq!(
            t.off_wire,
            Some(crate::provider::ThinkingOffWire::EnableThinkingFalse)
        );
        // 具体档位值解析
        let mc2: ModelEntry =
            toml::from_str("provider = \"p\"\nmodel = \"m\"\n[thinking]\ndefault = \"high\"")
                .unwrap();
        assert_eq!(
            mc2.thinking.as_ref().unwrap().default,
            Some(crate::provider::ThinkingLevel::High)
        );
        // 拼错的档位直接报错（不静默吞）
        assert!(toml::from_str::<ModelEntry>(
            "provider = \"p\"\nmodel = \"m\"\n[thinking]\ndefault = \"hgh\""
        )
        .is_err());
        // None 时序列化省略 thinking 键
        let bare: ModelEntry = toml::from_str("provider = \"p\"\nmodel = \"m\"").unwrap();
        assert!(!toml::to_string(&bare).unwrap().contains("thinking"));
        // Some 时保留（put_config 往返不蒸发）
        assert!(toml::to_string(&mc).unwrap().contains("[thinking]"));
    }

    #[test]
    fn model_entry_kind_and_capabilities_skip_direction() {
        // P8：kind 缺省 = chat 且序列化省略；capabilities 空省略
        let bare: ModelEntry = toml::from_str("provider = \"p\"\nmodel = \"m\"").unwrap();
        assert_eq!(bare.kind, ModelKind::Chat);
        assert!(bare.capabilities.is_empty());
        let s = toml::to_string(&bare).unwrap();
        assert!(!s.contains("kind"), "chat 应省略 kind 键:\n{s}");
        assert!(!s.contains("capabilities"), "空 capabilities 应省略:\n{s}");

        let img: ModelEntry = toml::from_str(
            "provider = \"p\"\nmodel = \"sd\"\nkind = \"image\"\nsize = \"512x512\"",
        )
        .unwrap();
        assert_eq!(img.kind, ModelKind::Image);
        let s = toml::to_string(&img).unwrap();
        assert!(s.contains("kind = \"image\""), "非 chat 必须显式落盘:\n{s}");
        assert!(s.contains("size = \"512x512\""));

        // 拼错的 kind 直接报错（不静默吞）
        assert!(
            toml::from_str::<ModelEntry>("provider = \"p\"\nmodel = \"m\"\nkind = \"voice\"")
                .is_err()
        );
    }

    #[test]
    fn provider_unknown_type_is_rejected_but_legacy_subtables_migrate() {
        // 显式未知 type → 报错（静默回退会让打错的 service type 变成 LLM provider）
        let toml = r#"
[provider.typos]
type = "tavly"
api_key = "k"

[agent.main]
model = "qwen"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let err = Config::load(&tmp.path().to_path_buf()).unwrap_err();
        assert!(err.to_string().contains("unknown type 'tavly'"), "{err}");

        // P8 兼容层：旧配置的 [provider.<id>.<alias>] model 子表自动迁移为
        // [model."<id>.<alias>"]，agent 引用沿用两段式无需改动，加载成功
        let legacy = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[provider.default.qwen]
model = "qwen2.5:7b"
context_size = 32768
enabled = false

[agent.main]
model = "default.qwen"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", legacy).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        let entry = config.models.get("default.qwen").expect("migrated entry");
        assert_eq!(entry.provider, "default");
        assert_eq!(entry.model, "qwen2.5:7b");
        assert_eq!(entry.context_size, Some(32768));
        assert!(!entry.enabled);
        assert!(config.provider.contains_key("default"));
        // legacy 子表必须从 provider 表里消失，否则下次反序列化仍被 deny_unknown_fields 拒收
        //（本测试经迁移通路已验证；直接对新结构断言 provider 只剩保留键）
    }

    #[test]
    fn migrate_legacy_tools_sections_end_to_end() {
        // 模拟真实旧配置的 tools 全家桶：凭据段 + 内联 tts/image_gen 端点 + 抽取开关
        let legacy = r#"
[provider.llamacpp]
type = "openai_compatible"
base_url = "http://127.0.0.1:8080/v1"

[provider.llamacpp.qwen]
model = "qwen3"
context_size = 128000

[provider.llamacpp.qwen.thinking]
default = "medium"
preserve = false

[tools.tavily]
api_key = "tv-key"

[tools.search]
provider = "tavily"
top_k = 8

[tools.tts]
enabled = false
base_url = "https://api.openai.com/v1"
api_key = ""
model = "tts-1"
voice = "alloy"

[tools.image_gen]
enabled = true
base_url = "https://api.agnes-ai.cn/v1/images/generations"
api_key = "img-key"
model = "agnes-image-2.5-flash"
size = "1024x1024"
timeout_secs = 300

[tools.web_fetch]
max_chars = 6000
use_tavily_extract = true

[agent.main]
model = "llamacpp.qwen"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", legacy).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();

        // 1. model 子表 → 目录条目（含嵌套 thinking 原样搬运）
        let qwen = config.models.get("llamacpp.qwen").expect("chat entry");
        assert_eq!(qwen.model, "qwen3");
        assert!(matches!(
            qwen.thinking.as_ref().unwrap().default,
            Some(crate::provider::ThinkingLevel::Medium)
        ));

        // 2. 搜索凭据 → service family provider（type=search + platform）；引用直通
        let tv = config.provider.get("tavily").expect("tavily provider");
        assert_eq!(tv.effective_type(), "search");
        assert_eq!(tv.search_platform(), Some("tavily"));
        assert_eq!(tv.api_key, "tv-key");
        assert_eq!(config.tools.search.provider, "tavily");

        // 3. tts → provider + kind=tts 条目 + 引用改写
        let tts = config.models.get("tts.tts-1").expect("tts entry");
        assert_eq!(tts.kind, ModelKind::Tts);
        assert_eq!(tts.provider, "tts");
        assert_eq!(config.tools.tts.model, "tts.tts-1");
        assert_eq!(config.tools.tts.voice, "alloy");
        assert!(!config.tools.tts.enabled);

        // 4. image_gen → provider（base_url 剥掉 /images/generations 尾巴）+ kind=image 条目
        let img = config
            .models
            .get("image_gen.agnes-image-2.5-flash")
            .expect("image entry");
        assert_eq!(img.kind, ModelKind::Image);
        assert_eq!(img.size.as_deref(), Some("1024x1024"));
        let igp = config.provider.get("image_gen").unwrap();
        assert_eq!(igp.base_url, "https://api.agnes-ai.cn/v1");
        assert_eq!(
            config.tools.image_gen.model,
            "image_gen.agnes-image-2.5-flash"
        );

        // 5. web_fetch 开关 → 引用
        assert_eq!(config.tools.web_fetch.extract_provider, "tavily");
    }

    #[test]
    fn migrate_skips_conflicting_model_ids() {
        // 显式 [model] 条目优先：legacy 子表同 id 时让位并被丢弃
        let legacy = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[provider.default.qwen]
model = "old-name"

[model."default.qwen"]
provider = "default"
model = "new-name"

[agent.main]
model = "default.qwen"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", legacy).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        assert_eq!(config.models.get("default.qwen").unwrap().model, "new-name");
    }

    #[test]
    fn search_platform_validation_and_legacy_normalization() {
        // type=search 缺 platform → 报错
        let toml = r#"
[provider.tv]
type = "search"
api_key = "k"

[agent.main]
model = "qwen"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let err = Config::load(&tmp.path().to_path_buf()).unwrap_err();
        assert!(err.to_string().contains("requires a platform"), "{err}");

        // platform 不在支持列表 → 报错
        let toml = toml.replace("api_key = \"k\"", "platform = \"bing\"\napi_key = \"k\"");
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let err = Config::load(&tmp.path().to_path_buf()).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported search platform 'bing'"),
            "{err}"
        );

        // 旧写法 type = tavily/baidu/brave → 归一化 search + platform，存量零改动
        let toml = r#"
[provider.tv]
type = "tavily"
api_key = "k"

[provider.bd]
type = "baidu"
api_key = "k"

[agent.main]
model = "qwen"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        let tv = config.provider.get("tv").unwrap();
        assert_eq!(tv.effective_type(), "search");
        assert_eq!(tv.search_platform(), Some("tavily"));
        assert_eq!(
            config.provider.get("bd").unwrap().search_platform(),
            Some("baidu")
        );
    }

    #[test]
    fn model_entry_validation_errors() {
        // service family provider 不能挂 model 条目
        let toml = r#"
[provider.tv]
type = "search"
platform = "tavily"
api_key = "k"

[model.bad]
provider = "tv"
model = "x"

[agent.main]
model = "qwen"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let err = Config::load(&tmp.path().to_path_buf()).unwrap_err();
        assert!(err.to_string().contains("cannot host models"), "{err}");

        // anthropic provider 只能挂 chat 条目
        let toml = r#"
[provider.anth]
type = "anthropic"
api_key = "k"

[model.bad]
provider = "anth"
kind = "image"
model = "x"

[agent.main]
model = "qwen"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let err = Config::load(&tmp.path().to_path_buf()).unwrap_err();
        assert!(err.to_string().contains("only supports kind"), "{err}");
    }

    #[test]
    fn test_load_full_config() {
        let toml = r#"
[runtime]
context_threshold = 0.8
max_iterations = 5

[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"
api_key = "sk-test"

[model.qwen3]
provider = "default"
model = "qwen-3.6-35b-MTP"
native_tool_calling = false

[model.qwen2]
provider = "default"
model = "qwen2.5:7b"
native_tool_calling = true

[provider.tv]
type = "tavily"
api_key = "tvly-test"

[agent.main]
model = "qwen3"
workspace = "~/custom-ws"

[tools.terminal]
confirm = "always"
whitelist = ["ls"]

[tools.search]
provider = "tv"

[log]
level = "debug"
dir = "~/.llaia-test/logs"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();

        // runtime
        assert_eq!(config.runtime.context_threshold, 0.8);
        assert_eq!(config.runtime.max_iterations, 5);
        // 未显式配置的两个新字段走默认
        assert_eq!(config.runtime.keepalive_interval_secs, 600);
        assert_eq!(config.runtime.max_turn_duration_secs, 3600);

        // provider 注册表 + model 目录
        let p = config.provider.get("default").unwrap();
        assert_eq!(p.base_url, "http://localhost:11434/v1");
        let m1 = config.models.get("qwen3").unwrap();
        assert_eq!(m1.model, "qwen-3.6-35b-MTP");
        assert_eq!(m1.provider, "default");
        assert!(!m1.native_tool_calling.unwrap_or(true));
        let m2 = config.models.get("qwen2").unwrap();
        assert!(m2.native_tool_calling.unwrap_or(true));
        // service family provider（旧写法 type=tavily 已归一化为 search + platform）
        let tv = config.provider.get("tv").unwrap();
        assert!(tv.is_service());
        assert_eq!(tv.effective_type(), "search");
        assert_eq!(tv.search_platform(), Some("tavily"));

        // agent: workspace 字段已移除（存量配置中的该键被忽略），soul/user/memory 缺省为 None
        let a = config.agent.get("main").unwrap();
        assert_eq!(a.model, "qwen3");
        assert!(a.soul.is_none());

        // tools
        assert_eq!(config.tools.terminal.confirm, "always");
        assert_eq!(config.tools.search.provider, "tv");
        assert_eq!(config.tools.search.top_k, 8);

        // log
        assert_eq!(config.log.level, "debug");
    }

    #[test]
    fn test_default_config() {
        let config = Config::default_for_workspace("~/.llaia");
        let p = config.provider.get("default").unwrap();
        assert_eq!(p.provider_type, "openai_compatible");
        let m = config.models.get("qwen").unwrap();
        assert_eq!(m.provider, "default");
        assert_eq!(m.native_tool_calling, Some(true));
        let a = config.agent.get("main").unwrap();
        assert_eq!(a.model, "qwen");
        assert!(a.soul.is_none());
        // runtime 默认值
        assert_eq!(config.runtime.context_threshold, 0.7);
        assert_eq!(config.runtime.max_iterations, 10);
    }

    #[test]
    fn test_minimal_config_uses_defaults() {
        // 无 workspace 字段的最小配置也能加载（该字段已从 schema 移除）
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[model.qwen]
provider = "default"
model = "qwen2.5:7b"

[agent.main]
model = "qwen"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        // 缺省 native_tool_calling = None（auto：跟随探测），不显式声明
        assert!(config
            .models
            .get("qwen")
            .unwrap()
            .native_tool_calling
            .is_none());
        // 缺省 context_threshold 默认 0.7
        assert_eq!(config.runtime.context_threshold, 0.7);
        // 缺省 confirm 默认 whitelist
        assert_eq!(config.tools.terminal.confirm, "whitelist");
        // 缺省 log.dir 跟随 config 文件所在目录的 logs/
        let expected = tmp
            .path()
            .parent()
            .unwrap()
            .join("logs")
            .to_string_lossy()
            .to_string();
        assert_eq!(config.log.dir, expected);
    }

    #[test]
    fn test_validate_chat_model_ref() {
        let config = Config::default_for_workspace("~/.llaia");
        assert!(Config::validate_chat_model_ref(&config, "qwen").is_ok());
        assert!(Config::validate_chat_model_ref(&config, "nope").is_err());
    }

    #[test]
    fn test_explicit_md_paths_expanded() {
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[model.qwen]
provider = "default"
model = "qwen2.5:7b"

[agent.main]
model = "qwen"
workspace = "~/.llaia"
soul = "~/custom/SOUL.md"
user = "~/custom/USER.md"
memory = "~/custom/MEMORY.md"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        let a = config.agent.get("main").unwrap();
        assert!(a.soul.as_ref().unwrap().contains("custom/SOUL.md"));
        assert!(!a.soul.as_ref().unwrap().contains('~'));
    }

    #[test]
    fn test_qq_config_defaults() {
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[model.qwen]
provider = "default"
model = "qwen2.5:7b"

[agent.main]
model = "qwen"
workspace = "~/.llaia"

[channels.qq]
app_id = "12345"
app_secret = "test-secret"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        assert!(!config.channels.qq.enabled); // 默认 false
        assert_eq!(config.channels.qq.app_id, "12345");
        assert_eq!(config.channels.qq.app_secret, "test-secret");
        assert_eq!(config.channels.qq.confirm_mode, "none"); // 默认改为 none
    }

    #[test]
    fn test_qq_config_disabled_by_default() {
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[model.qwen]
provider = "default"
model = "qwen2.5:7b"

[agent.main]
model = "qwen"
workspace = "~/.llaia"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        assert!(!config.channels.qq.enabled);
        assert_eq!(config.channels.qq.confirm_mode, "none");
    }

    #[test]
    fn test_sub_agent_config_fields() {
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[model.qwen]
provider = "default"
model = "qwen2.5:7b"

[agent.main]
model = "qwen"
workspace = "~/.llaia"

[agent.coder]
model = "qwen"
workspace = "~/.llaia/agents/coder"
soul = "~/.llaia/agents/coder.md"
denied_tools = ["memory_write"]
delegate_timeout = 180
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();

        let main = config.agent.get("main").unwrap();
        assert!(main.denied_tools.is_empty());
        assert_eq!(main.delegate_timeout, 120);

        let coder = config.agent.get("coder").unwrap();
        assert_eq!(coder.denied_tools, vec!["memory_write"]);
        assert_eq!(coder.delegate_timeout, 180);
    }

    #[test]
    fn test_env_var_expansion() {
        // 用时间戳后缀避免测试间环境变量冲突
        let key_var = "LLAIA_TEST_API_KEY_2026";
        let url_var = "LLAIA_TEST_BASE_URL_2026";
        let secret_var = "LLAIA_TEST_SECRET_2026";
        std::env::set_var(key_var, "sk-from-env-12345");
        std::env::set_var(url_var, "http://example.com");
        std::env::set_var(secret_var, "qq-secret");

        let toml = format!(
            r#"
[provider.default]
type = "openai_compatible"
base_url = "${{{}}}/v1"
api_key = "${{{}}}"

[model.qwen]
provider = "default"
model = "qwen2.5:7b"

[agent.main]
model = "qwen"
workspace = "~/.llaia"

[channels.qq]
app_secret = "${{{}}}"
"#,
            url_var, key_var, secret_var
        );
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();

        let p = config.provider.get("default").unwrap();
        assert_eq!(p.base_url, "http://example.com/v1");
        assert_eq!(p.api_key, "sk-from-env-12345");
        assert_eq!(config.channels.qq.app_secret, "qq-secret");

        std::env::remove_var(key_var);
        std::env::remove_var(url_var);
        std::env::remove_var(secret_var);
    }

    #[test]
    fn test_env_var_not_found_errors() {
        std::env::remove_var("LLAIA_NONEXISTENT_VAR_2026");
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"
api_key = "${LLAIA_NONEXISTENT_VAR_2026}"

[model.qwen]
provider = "default"
model = "qwen2.5:7b"

[agent.main]
model = "qwen"
workspace = "~/.llaia"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).expect(
            "missing env var should NOT error; instead replace with empty string (degraded mode)",
        );
        // 未定义的 env var 替换为空字符串，让 serve 能进降级模式
        assert_eq!(
            config.provider.get("default").unwrap().api_key,
            "",
            "missing env var should be replaced with empty string"
        );
    }

    #[test]
    fn test_env_var_not_expanded_for_lowercase() {
        // 小写变量名不匹配 [A-Z_][A-Z0-9_]*，原样保留
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"
api_key = "${lowercase_var}"

[model.qwen]
provider = "default"
model = "qwen2.5:7b"

[agent.main]
model = "qwen"
workspace = "~/.llaia"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        // 小写不匹配，原样保留（不报错）
        assert_eq!(
            config.provider.get("default").unwrap().api_key,
            "${lowercase_var}"
        );
    }

    #[test]
    fn test_web_config_defaults() {
        let config = Config::default_for_workspace("~/.llaia");
        assert_eq!(config.webui.host, "127.0.0.1");
        assert_eq!(config.webui.port, 51217);
        assert_eq!(config.webui.token, "");
    }

    #[test]
    fn test_web_config_loaded_from_toml() {
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[model.qwen]
provider = "default"
model = "qwen2.5:7b"

[agent.main]
model = "qwen"
workspace = "~/.llaia"

[webui]
host = "0.0.0.0"
port = 9000
token = "secret-token"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        use std::io::Write;
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        assert_eq!(config.webui.host, "0.0.0.0");
        assert_eq!(config.webui.port, 9000);
        assert_eq!(config.webui.token, "secret-token");
    }

    #[test]
    fn test_web_config_migration_from_channels_web() {
        // 旧 [channels.web] 应自动迁移到 [webui]（向后兼容）
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[model.qwen]
provider = "default"
model = "qwen2.5:7b"

[agent.main]
model = "qwen"
workspace = "~/.llaia"

[channels.web]
host = "0.0.0.0"
port = 9000
token = "migrated-token"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        assert_eq!(config.webui.host, "0.0.0.0");
        assert_eq!(config.webui.port, 9000);
        assert_eq!(config.webui.token, "migrated-token");
    }

    #[test]
    fn test_web_config_explicit_webui_wins_over_channels_web() {
        // 同时存在 [webui] 和 [channels.web] 时，[webui] 优先（不迁移）
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[model.qwen]
provider = "default"
model = "qwen2.5:7b"

[agent.main]
model = "qwen"
workspace = "~/.llaia"

[webui]
host = "1.2.3.4"
port = 1111
token = "new-token"

[channels.web]
host = "5.6.7.8"
port = 9999
token = "old-token"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        assert_eq!(config.webui.host, "1.2.3.4");
        assert_eq!(config.webui.port, 1111);
        assert_eq!(config.webui.token, "new-token");
    }

    #[test]
    fn test_whitelist_confirm_mode_deprecated() {
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[model.qwen]
provider = "default"
model = "qwen2.5:7b"

[agent.main]
model = "qwen"
workspace = "~/.llaia"

[channels.qq]
confirm_mode = "whitelist"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        assert_eq!(config.channels.qq.confirm_mode, "none"); // 废弃后 fallback
    }

    /// 存量配置（无 enabled 键）读出 true；且序列化方向必须是「true 时省略」。
    /// 反了的话 disabled 不落盘，配合 model 子树 replace 合并会在保存时被静默删除。
    #[test]
    fn test_model_enabled_default_and_serialize_direction() {
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[model.legacy]
provider = "default"
model = "no-enabled-key"

[model.hidden]
provider = "default"
model = "explicit-off"
enabled = false

[agent.main]
model = "legacy"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        assert!(
            config.models["legacy"].enabled,
            "缺省应为启用（存量零迁移）"
        );
        assert!(!config.models["hidden"].enabled);

        let out = toml::to_string(&config).expect("serialize config");
        assert!(
            out.contains("enabled = false"),
            "disabled 必须显式落盘:\n{out}"
        );
        assert!(
            !out.contains("enabled = true"),
            "启用态应省略该键（否则每个 model 表都多一行脏 diff）:\n{out}"
        );

        // 往返：disabled 不会被写盘过程洗掉
        let back: Config = toml::from_str(&out).expect("re-parse serialized config");
        assert!(!back.models["hidden"].enabled);
        assert!(back.models["legacy"].enabled);
    }

    /// `enabled` 只管可发现性：间接引用（fallback / compact / vision）被收敛，
    /// 但 agent.main.model 指向 disabled 模型时刻意保持不变（否则关掉当前模型=下次启动失败）。
    #[test]
    fn test_reconcile_disabled_models() {
        let toml = r#"
[runtime]
compact_model = "hidden"
vision_model = "hidden"

[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[model.kept]
provider = "default"
model = "visible"

[model.hidden]
provider = "default"
model = "disabled"
enabled = false

[agent.main]
model = "hidden"
fallback = ["kept", "hidden"]
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();

        assert_eq!(config.agent["main"].model, "hidden", "不碰显式当前模型");
        assert_eq!(config.agent["main"].fallback, vec!["kept".to_string()]);
        assert_eq!(config.runtime.compact_model, None, "回退主模型");
        assert_eq!(config.runtime.vision_model, None, "图片回退主模型");
    }

    /// 没有任何 disabled 模型时不得误伤引用（尤其 fallback 顺序与 compact/vision）。
    #[test]
    fn test_reconcile_keeps_enabled_refs() {
        let toml = r#"
[runtime]
compact_model = "a"
vision_model = "b"

[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[model.a]
provider = "default"
model = "one"

[model.b]
provider = "default"
model = "two"
enabled = true

[agent.main]
model = "a"
fallback = ["b"]
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        assert_eq!(config.runtime.compact_model.as_deref(), Some("a"));
        assert_eq!(config.runtime.vision_model.as_deref(), Some("b"));
        assert_eq!(config.agent["main"].fallback, vec!["b".to_string()]);
    }
}
