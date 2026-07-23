use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// 顶层配置。对应 ~/.laia/config.toml
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
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            context_threshold: default_threshold(),
            max_iterations: default_max_iterations(),
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
    "~/.laia/logs".into()
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
    /// 默认 8192，大模型请按实际配置（如 128000）。
    #[serde(default = "default_context_size")]
    pub context_size: usize,
}

fn default_true() -> bool {
    true
}

fn default_context_size() -> usize {
    8192
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// 引用 "provider_id.model_alias"，例如 "default.qwen3"
    pub model: String,
    /// 该 agent 的 md 文件根目录，sessions.db 也在其下
    pub workspace: String,
    /// 以下三项缺省时从 workspace 推导为 <workspace>/SOUL.md 等
    pub soul: Option<String>,
    pub user: Option<String>,
    pub memory: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelsConfig {
    #[serde(default)]
    pub cli: CliChannelConfig,
    #[serde(default)]
    pub qq: QqConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CliChannelConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
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
    "always".into()
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
}

impl Default for TerminalToolConfig {
    fn default() -> Self {
        Self {
            confirm: default_confirm(),
            whitelist: default_whitelist(),
        }
    }
}

fn default_confirm() -> String {
    "whitelist".into()
}

fn default_whitelist() -> Vec<String> {
    vec!["ls".into(), "cat".into(), "grep".into(), "pwd".into(), "dir".into()]
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
        // log.dir 未显式配置时（仍为 serde 默认值），跟随 config 文件所在目录
        // 注意：必须在 expand_paths 之前比较，因为 expand 后路径会变
        if config.log.dir == default_log_dir() {
            if let Some(parent) = path.parent() {
                config.log.dir = parent.join("logs").to_string_lossy().into_owned();
            }
        }
        config.expand_paths();
        Ok(config)
    }

    fn expand_paths(&mut self) {
        let expand = |s: &str| -> String { shellexpand::tilde(s).into_owned() };
        for a in self.agent.values_mut() {
            a.workspace = expand(&a.workspace);
            a.soul = a.soul.as_ref().map(|s| expand(s));
            a.user = a.user.as_ref().map(|s| expand(s));
            a.memory = a.memory.as_ref().map(|s| expand(s));
        }
        self.log.dir = expand(&self.log.dir);
    }

    /// 解析 "provider_id.model_alias"，返回 (provider_id, model_alias)
    pub fn parse_model_ref(ref_str: &str) -> Result<(&str, &str)> {
        ref_str
            .split_once('.')
            .context("agent.model must be 'provider_id.model_alias'")
    }

    /// 默认配置（首次启动用），结构最小化
    pub fn default_for_workspace(workspace_dir: &str) -> Self {
        let ws = shellexpand::tilde(workspace_dir).into_owned();
        let mut provider: HashMap<String, ProviderConfig> = HashMap::new();
        let mut models: HashMap<String, ModelConfig> = HashMap::new();
        models.insert(
            "qwen".into(),
            ModelConfig {
                model: "qwen2.5:7b".into(),
                native_tool_calling: true,
                context_size: default_context_size(),
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
                workspace: ws.clone(),
                soul: None,
                user: None,
                memory: None,
            },
        );

        Config {
            runtime: RuntimeConfig::default(),
            log: LogConfig {
                level: default_level(),
                dir: format!("{}/logs", ws),
            },
            provider,
            agent,
            channels: ChannelsConfig::default(),
            tools: ToolsConfig::default(),
        }
    }
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

[channels.cli]
enabled = true

[tools.terminal]
confirm = "always"
whitelist = ["ls"]

[tools.tavily]
api_key = "tvly-test"

[log]
level = "debug"
dir = "~/.laia-test/logs"
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
        let config = Config::default_for_workspace("~/.laia");
        let p = config.provider.get("default").unwrap();
        assert_eq!(p.provider_type, "openai_compatible");
        let m = p.model.get("qwen").unwrap();
        assert!(m.native_tool_calling);
        let a = config.agent.get("main").unwrap();
        assert_eq!(a.model, "default.qwen");
        assert!(a.soul.is_none());
        assert!(a.workspace.ends_with(".laia"));
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
workspace = "~/.laia"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        // 缺省 native_tool_calling 默认 true
        assert!(config
            .provider
            .get("default")
            .unwrap()
            .model
            .get("qwen")
            .unwrap()
            .native_tool_calling);
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
workspace = "~/.laia"
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
workspace = "~/.laia"

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
        assert_eq!(config.channels.qq.confirm_mode, "always"); // 默认 always
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
workspace = "~/.laia"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        assert!(!config.channels.qq.enabled);
        assert_eq!(config.channels.qq.confirm_mode, "always");
    }
}
