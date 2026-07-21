use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub provider: HashMap<String, ProviderConfig>,
    pub agent: HashMap<String, AgentConfig>,
    #[serde(default)]
    pub channels: ChannelsConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    #[serde(default)]
    pub log: LogConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(rename = "type")]
    pub provider_type: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    pub model: String,
    #[serde(default = "default_true")]
    pub native_tool_calling: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    #[serde(default = "default_threshold")]
    pub context_threshold: f64,
    pub soul: String,
    pub user: String,
    pub memory: String,
}

fn default_threshold() -> f64 {
    0.7
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelsConfig {
    #[serde(default)]
    pub cli: CliChannelConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CliChannelConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
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
    "whitelist".to_string()
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    #[serde(default = "default_workspace")]
    pub dir: String,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            dir: default_workspace(),
        }
    }
}

fn default_workspace() -> String {
    "~/.laia".to_string()
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
    "info".to_string()
}

fn default_log_dir() -> String {
    "~/.laia/logs".to_string()
}

impl Config {
    pub fn load(path: &PathBuf) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {:?}", path))?;
        let mut config: Config =
            toml::from_str(&content).with_context(|| format!("failed to parse config: {:?}", path))?;
        config.expand_paths();
        Ok(config)
    }

    fn expand_paths(&mut self) {
        let expand = |s: &str| -> String { shellexpand::tilde(s).into_owned() };
        for a in self.agent.values_mut() {
            a.soul = expand(&a.soul);
            a.user = expand(&a.user);
            a.memory = expand(&a.memory);
        }
        self.workspace.dir = expand(&self.workspace.dir);
        self.log.dir = expand(&self.log.dir);
    }

    pub fn default_for_workspace(workspace_dir: &str) -> Self {
        let ws = shellexpand::tilde(workspace_dir).into_owned();
        let mut config = Config {
            provider: HashMap::new(),
            agent: HashMap::new(),
            channels: ChannelsConfig::default(),
            tools: ToolsConfig::default(),
            workspace: WorkspaceConfig { dir: ws.clone() },
            log: LogConfig {
                level: "info".into(),
                dir: format!("{}/logs", ws),
            },
        };
        config.provider.insert(
            "default".into(),
            ProviderConfig {
                provider_type: "openai_compatible".into(),
                base_url: "http://localhost:11434/v1".into(),
                api_key: String::new(),
                model: "qwen2.5:7b".into(),
                native_tool_calling: true,
            },
        );
        config.agent.insert(
            "main".into(),
            AgentConfig {
                context_threshold: 0.7,
                soul: format!("{}/SOUL.md", ws),
                user: format!("{}/USER.md", ws),
                memory: format!("{}/MEMORY.md", ws),
            },
        );
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_load_full_config() {
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"
api_key = "sk-test"
model = "qwen2.5:7b"
native_tool_calling = false

[agent.main]
context_threshold = 0.8
soul = "~/custom/SOUL.md"
user = "~/custom/USER.md"
memory = "~/custom/MEMORY.md"

[channels.cli]
enabled = true

[tools.terminal]
confirm = "always"
whitelist = ["ls"]

[tools.tavily]
api_key = "tvly-test"

[workspace]
dir = "~/.laia-test"

[log]
level = "debug"
dir = "~/.laia-test/logs"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();

        assert_eq!(config.provider.get("default").unwrap().model, "qwen2.5:7b");
        assert!(!config.provider.get("default").unwrap().native_tool_calling);
        assert_eq!(config.agent.get("main").unwrap().context_threshold, 0.8);
        assert!(config
            .agent
            .get("main")
            .unwrap()
            .soul
            .contains("custom/SOUL.md"));
        assert!(!config.agent.get("main").unwrap().soul.contains('~'));
        assert_eq!(config.tools.terminal.confirm, "always");
        assert_eq!(config.tools.tavily.api_key, "tvly-test");
    }

    #[test]
    fn test_default_config() {
        let config = Config::default_for_workspace("~/.laia");
        let p = config.provider.get("default").unwrap();
        assert_eq!(p.provider_type, "openai_compatible");
        assert!(p.native_tool_calling);
        let a = config.agent.get("main").unwrap();
        assert_eq!(a.context_threshold, 0.7);
        assert!(a.soul.ends_with("/SOUL.md"));
    }

    #[test]
    fn test_minimal_config_uses_defaults() {
        let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"
model = "qwen2.5:7b"

[agent.main]
soul = "~/SOUL.md"
user = "~/USER.md"
memory = "~/MEMORY.md"
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "{}", toml).unwrap();
        let config = Config::load(&tmp.path().to_path_buf()).unwrap();
        assert!(config.provider.get("default").unwrap().native_tool_calling);
        assert_eq!(config.agent.get("main").unwrap().context_threshold, 0.7);
        assert_eq!(config.tools.terminal.confirm, "whitelist");
    }
}
