use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// 顶层配置。对应 ~/.llaia/config.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub log: LogConfig,
    /// provider id => ProviderConfig
    #[serde(default)]
    pub provider: HashMap<String, ProviderConfig>,
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
    /// 上下文压缩用的模型引用 "provider_id.model_alias"。
    /// 未设置时复用 agent 自身的 provider（兼容旧行为）。
    /// 设置后会构建独立的 compact provider，可用更便宜的模型做压缩。
    #[serde(default)]
    pub compact_model: Option<String>,
    /// IANA 时区名（如 "Asia/Shanghai"），决定 agent 状态栏与用户可见日期。
    /// None（默认）= 跟随宿主机本地时区，与旧行为一致。
    /// 非法值在 Config::load 里 warn + 置 None。
    #[serde(default)]
    pub timezone: Option<String>,
    /// 图片描述用的模型引用 "provider_id.model_alias"。
    /// 主模型无多模态能力时，用此模型描述图片，描述文本替换图片注入主模型上下文。
    /// 未设置时：图片直接发给主模型（主模型不支持则由 provider 决定如何处理）。
    #[serde(default)]
    pub vision_model: Option<String>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            context_threshold: default_threshold(),
            max_iterations: default_max_iterations(),
            compact_model: None,
            timezone: None,
            vision_model: None,
        }
    }
}

fn default_threshold() -> f64 {
    0.7
}

fn default_max_iterations() -> u32 {
    10
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

/// 一个 provider 端点（连接信息），下挂多个 model 配置。
/// TOML 写法 `[provider.<id>.<model_alias>]` 会被 flatten 收入 `model` HashMap。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(rename = "type")]
    pub provider_type: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    /// model alias => ModelConfig，通过 `#[serde(flatten)]` 直接捕获
    /// TOML 中 `[provider.<id>.<model_alias>]` 的子表
    #[serde(flatten, default)]
    pub model: HashMap<String, ModelConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub model: String,
    #[serde(default = "default_true")]
    pub native_tool_calling: bool,
    /// 模型上下文窗口大小（tokens），用于判断何时触发自动压缩。
    /// 未配置时启动时从服务端探测（llama.cpp /props 或 Ollama /api/show），
    /// 探测失败回退默认 8192。取 min(配置值, 探测值)。
    #[serde(default)]
    pub context_size: Option<usize>,
    /// 单次生成最大 token 数。Anthropic Messages API 必传 max_tokens，
    /// 未配置时默认 4096；OpenAI 兼容 provider 忽略此项。
    #[serde(default)]
    pub max_tokens: Option<usize>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// 引用 "provider_id.model_alias"，例如 "default.qwen3"
    pub model: String,
    /// [deprecated] 该 agent 的 md 文件根目录。P3-a 起自动推导：
    ///   main → <config_dir>/workspace/
    ///   子 agent → <config_dir>/workspace/subagent/<alias>/
    /// 字段保留向后兼容，加载时 warn 并用自动推导值覆盖
    pub workspace: String,
    /// [deprecated] 缺省时从 workspace 推导为 <workspace>/SOUL.md 等
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
}

impl Default for QqConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: String::new(),
            app_secret: String::new(),
            confirm_mode: default_qq_confirm(),
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
    #[serde(default)]
    pub tavily: TavilyConfig,
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
}

impl Default for TerminalToolConfig {
    fn default() -> Self {
        Self {
            confirm: default_confirm(),
            whitelist: default_whitelist(),
            command_policy: default_command_policy(),
            command_whitelist: default_command_whitelist(),
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

fn default_command_whitelist() -> Vec<String> {
    Vec::new()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TavilyConfig {
    #[serde(default)]
    pub api_key: String,
}

impl Config {
    pub fn load(path: &PathBuf) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {:?}", path))?;
        let mut config: Config = toml::from_str(&content)
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
        // compact_model 引用校验：避免拼写错误到运行时才暴露
        if let Some(m) = &config.runtime.compact_model {
            if let Err(e) = Self::parse_model_ref(m) {
                tracing::warn!(
                    model = m.as_str(),
                    error = %e,
                    "runtime.compact_model is not a valid 'provider_id.model_alias' reference, will be ignored"
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
        // agent fallback 链引用校验：无效项移除（备用链是容错手段，不应阻塞启动）
        for (alias, agent_cfg) in config.agent.iter_mut() {
            let before = agent_cfg.fallback.len();
            agent_cfg
                .fallback
                .retain(|m| match Self::parse_model_ref(m) {
                    Ok(_) => true,
                    Err(e) => {
                        tracing::warn!(
                            agent = alias.as_str(),
                            model = m.as_str(),
                            error = %e,
                            "agent fallback entry is not a valid 'provider_id.model_alias' reference, removed"
                        );
                        false
                    }
                });
            if agent_cfg.fallback.len() != before {
                tracing::debug!(agent = alias.as_str(), "fallback chain sanitized");
            }
        }
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
            a.workspace = expand(&a.workspace)?;
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
        self.webui.token = expand(&self.webui.token)?;
        self.tools.tavily.api_key = expand(&self.tools.tavily.api_key)?;
        self.log.dir = expand(&self.log.dir)?;
        Ok(())
    }

    /// 解析 "provider_id.model_alias"，返回 (provider_id, model_alias)
    pub fn parse_model_ref(ref_str: &str) -> Result<(&str, &str)> {
        ref_str
            .split_once('.')
            .context("agent.model must be 'provider_id.model_alias'")
    }

    /// 默认配置（首次启动用），结构最小化
    /// config_dir 指向 ~/.llaia/，主 agent workspace 自动推导为 ~/.llaia/workspace/
    pub fn default_for_workspace(config_dir: &str) -> Self {
        let config_dir = shellexpand::tilde(config_dir).into_owned();
        let config_dir_path = std::path::PathBuf::from(&config_dir);
        let ws = config_dir_path.join("workspace");

        let mut provider: HashMap<String, ProviderConfig> = HashMap::new();
        let mut models: HashMap<String, ModelConfig> = HashMap::new();
        models.insert(
            "qwen".into(),
            ModelConfig {
                model: "qwen2.5:7b".into(),
                native_tool_calling: true,
                context_size: None,
                max_tokens: None,
            },
        );
        provider.insert(
            "default".into(),
            ProviderConfig {
                provider_type: "openai_compatible".into(),
                base_url: "http://localhost:11434/v1".into(),
                api_key: String::new(),
                model: models,
            },
        );

        let mut agent: HashMap<String, AgentConfig> = HashMap::new();
        agent.insert(
            "main".into(),
            AgentConfig {
                model: "default.qwen".into(),
                workspace: ws.to_string_lossy().into_owned(),
                soul: None,
                user: None,
                memory: None,
                denied_tools: Vec::new(),
                delegate_timeout: default_delegate_timeout(),
                fallback: Vec::new(),
            },
        );

        Config {
            runtime: RuntimeConfig::default(),
            log: LogConfig {
                level: default_level(),
                dir: format!("{}/logs", config_dir),
            },
            provider,
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
    fn test_load_full_config() {
        let toml = r#"
[runtime]
context_threshold = 0.8
max_iterations = 5

[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"
api_key = "sk-test"

[provider.default.qwen3]
model = "qwen-3.6-35b-MTP"
native_tool_calling = false

[provider.default.qwen2]
model = "qwen2.5:7b"
native_tool_calling = true

[agent.main]
model = "default.qwen3"
workspace = "~/custom-ws"

[tools.terminal]
confirm = "always"
whitelist = ["ls"]

[tools.tavily]
api_key = "tvly-test"

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

        // provider with nested models
        let p = config.provider.get("default").unwrap();
        assert_eq!(p.base_url, "http://localhost:11434/v1");
        let m1 = p.model.get("qwen3").unwrap();
        assert_eq!(m1.model, "qwen-3.6-35b-MTP");
        assert!(!m1.native_tool_calling);
        let m2 = p.model.get("qwen2").unwrap();
        assert!(m2.native_tool_calling);

        // agent: workspace 展开，soul/user/memory 缺省为 None
        let a = config.agent.get("main").unwrap();
        assert_eq!(a.model, "default.qwen3");
        assert!(a.workspace.ends_with("custom-ws"));
        assert!(!a.workspace.contains('~'));
        assert!(a.soul.is_none());

        // tools
        assert_eq!(config.tools.terminal.confirm, "always");
        assert_eq!(config.tools.tavily.api_key, "tvly-test");

        // log
        assert_eq!(config.log.level, "debug");
    }

    #[test]
    fn test_default_config() {
        let config = Config::default_for_workspace("~/.llaia");
        let p = config.provider.get("default").unwrap();
        assert_eq!(p.provider_type, "openai_compatible");
        let m = p.model.get("qwen").unwrap();
        assert!(m.native_tool_calling);
        let a = config.agent.get("main").unwrap();
        assert_eq!(a.model, "default.qwen");
        assert!(a.soul.is_none());
        // workspace 现在推导为 ~/.llaia/workspace
        let ws_path = std::path::PathBuf::from(&a.workspace);
        assert!(ws_path.ends_with(std::path::Path::new(".llaia/workspace")));
        // runtime 默认值
        assert_eq!(config.runtime.context_threshold, 0.7);
        assert_eq!(config.runtime.max_iterations, 10);
    }

    #[test]
    fn test_minimal_config_uses_defaults() {
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[provider.default.qwen]
model = "qwen2.5:7b"

[agent.main]
model = "default.qwen"
workspace = "~/.llaia"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        // 缺省 native_tool_calling 默认 true
        assert!(
            config
                .provider
                .get("default")
                .unwrap()
                .model
                .get("qwen")
                .unwrap()
                .native_tool_calling
        );
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
    fn test_parse_model_ref() {
        let (p, m) = Config::parse_model_ref("default.qwen3").unwrap();
        assert_eq!(p, "default");
        assert_eq!(m, "qwen3");
        assert!(Config::parse_model_ref("invalid").is_err());
    }

    #[test]
    fn test_explicit_md_paths_expanded() {
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[provider.default.qwen]
model = "qwen2.5:7b"

[agent.main]
model = "default.qwen"
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

[provider.default.qwen]
model = "qwen2.5:7b"

[agent.main]
model = "default.qwen"
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

[provider.default.qwen]
model = "qwen2.5:7b"

[agent.main]
model = "default.qwen"
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

[provider.default.qwen]
model = "qwen2.5:7b"

[agent.main]
model = "default.qwen"
workspace = "~/.llaia"

[agent.coder]
model = "default.qwen"
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

[provider.default.qwen]
model = "qwen2.5:7b"

[agent.main]
model = "default.qwen"
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

[provider.default.qwen]
model = "qwen2.5:7b"

[agent.main]
model = "default.qwen"
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

[provider.default.qwen]
model = "qwen2.5:7b"

[agent.main]
model = "default.qwen"
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

[provider.default.qwen]
model = "qwen2.5:7b"

[agent.main]
model = "default.qwen"
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

[provider.default.qwen]
model = "qwen2.5:7b"

[agent.main]
model = "default.qwen"
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

[provider.default.qwen]
model = "qwen2.5:7b"

[agent.main]
model = "default.qwen"
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

[provider.default.qwen]
model = "qwen2.5:7b"

[agent.main]
model = "default.qwen"
workspace = "~/.llaia"

[channels.qq]
confirm_mode = "whitelist"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        assert_eq!(config.channels.qq.confirm_mode, "none"); // 废弃后 fallback
    }
}
