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
}

fn default_true() -> bool {
    true
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
    /// 工具黑名单：列出的工具子 Agent 不可用。默认空（继承所有工具）
    #[serde(default)]
    pub denied_tools: Vec<String>,
    /// 委派超时秒数（仅子 Agent 生效）。默认 120
    #[serde(default = "default_delegate_timeout")]
    pub delegate_timeout: u64,
}

fn default_delegate_timeout() -> u64 {
    120
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelsConfig {
    #[serde(default)]
    pub cli: CliChannelConfig,
    #[serde(default)]
    pub qq: QqConfig,
    #[serde(default)]
    pub web: WebConfig,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    #[serde(default)]
    pub enabled: bool,
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

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
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
    vec![
        "ls".into(),
        "cat".into(),
        "grep".into(),
        "pwd".into(),
        "dir".into(),
    ]
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
        self.channels.web.token = expand(&self.channels.web.token)?;
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
    pub fn default_for_workspace(workspace_dir: &str) -> Self {
        let ws = shellexpand::tilde(workspace_dir).into_owned();
        let mut provider: HashMap<String, ProviderConfig> = HashMap::new();
        let mut models: HashMap<String, ModelConfig> = HashMap::new();
        models.insert(
            "qwen".into(),
            ModelConfig {
                model: "qwen2.5:7b".into(),
                native_tool_calling: true,
                context_size: None,
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
                denied_tools: Vec::new(),
                delegate_timeout: default_delegate_timeout(),
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

/// 展开字符串中的 `~` 和 `${VAR}` 环境变量引用。
/// - `~` / `~/path` → 用户 home 目录（shellexpand::tilde）
/// - `${VAR}` → 环境变量值（变量名须匹配 `[A-Z_][A-Z0-9_]*`）
///
/// 未定义的环境变量会报错（fail fast），避免用空字符串当 api_key。
/// 先展开 env，再展开 tilde（env 值里的 `~` 不会被二次展开）。
fn expand_string(s: &str) -> Result<String> {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"\$\{([A-Z_][A-Z0-9_]*)\}").unwrap());

    let mut err: Option<anyhow::Error> = None;
    let expanded = re.replace_all(s, |caps: &regex::Captures| match std::env::var(&caps[1]) {
        Ok(v) => v,
        Err(e) => {
            if err.is_none() {
                err = Some(anyhow::anyhow!(
                    "environment variable '{}' referenced in config but not set: {}",
                    &caps[1],
                    e
                ));
            }
            String::new()
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
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

[channels.cli]
enabled = true

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
        assert!(a.workspace.ends_with(".llaia"));
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
workspace = "~/.llaia"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        assert!(!config.channels.qq.enabled);
        assert_eq!(config.channels.qq.confirm_mode, "always");
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
        let result = Config::load(&tmp.path().to_path_buf());
        assert!(result.is_err(), "should error on undefined env var");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("LLAIA_NONEXISTENT_VAR_2026"),
            "error should mention the missing var: {}",
            err
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
        assert!(!config.channels.web.enabled);
        assert_eq!(config.channels.web.host, "127.0.0.1");
        assert_eq!(config.channels.web.port, 51217);
        assert_eq!(config.channels.web.token, "");
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

[channels.web]
enabled = true
host = "0.0.0.0"
port = 9000
token = "secret-token"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        use std::io::Write;
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        assert!(config.channels.web.enabled);
        assert_eq!(config.channels.web.host, "0.0.0.0");
        assert_eq!(config.channels.web.port, 9000);
        assert_eq!(config.channels.web.token, "secret-token");
    }
}
