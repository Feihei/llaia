# LAIA v1 实现计划

> **For agentic workers:** 按 Phase 顺序执行，每个 Task 完成后跑 `cargo test` + `cargo clippy` + commit。

**Goal:** 实现一个能 `cargo run -- chat` 进 REPL 多轮对话、调本地 Ollama/LMStudio、用基础工具、自动压缩上下文、SOUL/USER/MEMORY 持久化的单用户私人助理。

**Architecture:** 单 crate Rust 项目，主 Agent 单干，OpenAI 兼容 provider + 原生/标签降级工具调用，sqlite 存会话历史，三份 md 存人格/用户/记忆。

**Tech Stack:** Rust + tokio + reqwest + serde + toml + rusqlite + anyhow + tracing + clap + regex

**参考 ADR:** [docs/adr/](adr/) ADR-0001 到 ADR-0007

---

## 依赖清单

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["json"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
rusqlite = { version = "0.31", features = ["bundled"] }
anyhow = "1"
thiserror = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
clap = { version = "4", features = ["derive"] }
regex = "1"
dirs = "5"
async-trait = "0.1"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4"] }
shellexpand = "3"

[dev-dependencies]
mockito = "1"
tempfile = "3"
tokio = { version = "1", features = ["full", "test-util"] }
```

## 文件结构

```
laia/
├── Cargo.toml
├── src/
│   ├── main.rs                  # CLI 入口、子命令分发
│   ├── lib.rs                   # crate 根
│   ├── config.rs                # toml 配置加载
│   ├── error.rs                 # 错误类型（v1 主要用 anyhow，少量 thiserror）
│   ├── log.rs                   # tracing 初始化
│   ├── provider/
│   │   ├── mod.rs               # Provider trait + 类型定义
│   │   └── openai_compat.rs     # OpenAI 兼容实现
│   ├── tool_call/
│   │   ├── mod.rs               # 工具调用解析入口
│   │   └── tag_parser.rs        # <tool_call> 标签解析
│   ├── tools/
│   │   ├── mod.rs               # Tool trait + ToolSpec
│   │   ├── file.rs              # file_read/write/edit
│   │   ├── terminal.rs          # terminal + 白名单确认
│   │   ├── web.rs               # web_fetch
│   │   ├── tavily.rs            # tavily_search
│   │   └── memory.rs            # memory_read/write
│   ├── memory/
│   │   ├── mod.rs               # SOUL/USER/MEMORY 加载
│   │   ├── markdown.rs          # md 文件读写、MEMORY 压缩
│   │   └── sqlite.rs            # 会话持久化
│   ├── agent/
│   │   ├── mod.rs               # 主 Agent 循环
│   │   ├── context.rs           # 上下文管理、压缩
│   │   └── runner.rs            # 工具调用执行器
│   ├── channels/
│   │   ├── mod.rs               # Channel trait
│   │   └── cli.rs               # CLI REPL
│   └── commands/
│       ├── mod.rs               # 斜杠命令分发
│       └── slash.rs             # 斜杠命令实现
├── tests/
│   └── integration.rs           # 集成测试（冒烟）
└── docs/
    ├── plan.md                  # 本文件
    ├── glossary.md
    └── adr/
```

---

## Phase 0: 项目骨架

### Task 0.1: cargo init + 依赖

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/lib.rs`

- [ ] **Step 1: cargo init**

```bash
cd e:\play\coding\laia
cargo init --name laia
```

- [ ] **Step 2: 写 Cargo.toml**

```toml
[package]
name = "laia"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["json"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
rusqlite = { version = "0.31", features = ["bundled"] }
anyhow = "1"
thiserror = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
clap = { version = "4", features = ["derive"] }
regex = "1"
dirs = "5"
async-trait = "0.1"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4"] }
shellexpand = "3"

[dev-dependencies]
mockito = "1"
tempfile = "3"
```

- [ ] **Step 3: 写 src/main.rs 占位**

```rust
fn main() -> anyhow::Result<()> {
    println!("laia v0.1.0");
    Ok(())
}
```

- [ ] **Step 4: 写 src/lib.rs 占位**

```rust
```

- [ ] **Step 5: 验证编译**

Run: `cargo build`
Expected: 成功

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "chore: cargo init with dependencies"
```

### Task 0.2: tracing 日志初始化

**Files:**
- Create: `src/log.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 写 src/log.rs**

```rust
use anyhow::Result;
use std::path::PathBuf;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// 初始化 tracing：输出到 stderr + 文件。
/// level: "debug" / "info" / "warn" / "error"
/// log_dir: 日志目录
pub fn init(level: &str, log_dir: &PathBuf) -> Result<()> {
    std::fs::create_dir_all(log_dir)?;
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("laia.log"))?;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

    let stderr_layer = fmt::layer().with_writer(std::io::stderr);
    let file_layer = fmt::layer()
        .with_writer(std::sync::Mutex::new(log_file))
        .with_ansi(false);

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();
    Ok(())
}
```

- [ ] **Step 2: 更新 src/lib.rs**

```rust
pub mod log;
```

- [ ] **Step 3: 验证编译**

Run: `cargo build`
Expected: 成功

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: add tracing log initialization"
```

---

## Phase 1: 配置加载

### Task 1.1: 配置类型定义

**Files:**
- Create: `src/config.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 写 src/config.rs**

```rust
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

fn default_true() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    #[serde(default = "default_threshold")]
    pub context_threshold: f64,
    pub soul: String,
    pub user: String,
    pub memory: String,
}

fn default_threshold() -> f64 { 0.7 }

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
    pub confirm: String, // "none" / "whitelist" / "always"
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

fn default_confirm() -> String { "whitelist".to_string() }
fn default_whitelist() -> Vec<String> {
    vec!["ls".into(), "cat".into(), "grep".into(), "pwd".into(), "dir".into()]
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
        Self { dir: default_workspace() }
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

fn default_level() -> String { "info".to_string() }
fn default_log_dir() -> String { "~/.laia/logs".to_string() }

impl Config {
    /// 从 toml 文件加载配置。缺省字段用默认值。
    pub fn load(path: &PathBuf) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {:?}", path))?;
        let mut config: Config = toml::from_str(&content)
            .with_context(|| format!("failed to parse config: {:?}", path))?;
        config.expand_paths();
        Ok(config)
    }

    /// 展开所有路径中的 ~
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

    /// 生成默认配置（写入新工作区时用）
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
        config.provider.insert("default".into(), ProviderConfig {
            provider_type: "openai_compatible".into(),
            base_url: "http://localhost:11434/v1".into(),
            api_key: String::new(),
            model: "qwen2.5:7b".into(),
            native_tool_calling: true,
        });
        config.agent.insert("main".into(), AgentConfig {
            context_threshold: 0.7,
            soul: format!("{}/SOUL.md", ws),
            user: format!("{}/USER.md", ws),
            memory: format!("{}/MEMORY.md", ws),
        });
        config
    }
}
```

- [ ] **Step 2: 更新 src/lib.rs**

```rust
pub mod config;
pub mod log;
```

- [ ] **Step 3: 验证编译**

Run: `cargo build`
Expected: 成功

### Task 1.2: 配置加载测试

**Files:**
- Modify: `src/config.rs`（追加测试模块）

- [ ] **Step 1: 在 src/config.rs 末尾追加测试**

```rust
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
        // ~ 应被展开
        assert!(config.agent.get("main").unwrap().soul.contains("custom/SOUL.md"));
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
        // native_tool_calling 缺省应为 true
        assert!(config.provider.get("default").unwrap().native_tool_calling);
        // context_threshold 缺省应为 0.7
        assert_eq!(config.agent.get("main").unwrap().context_threshold, 0.7);
        // terminal 缺省应为 whitelist
        assert_eq!(config.tools.terminal.confirm, "whitelist");
    }
}
```

- [ ] **Step 2: 跑测试**

Run: `cargo test config`
Expected: 3 个测试通过

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat: config loading with toml + path expansion"
```

---

## Phase 2: Provider 抽象 + OpenAI 兼容实现

### Task 2.1: Provider 类型与 trait

**Files:**
- Create: `src/provider/mod.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 写 src/provider/mod.rs**

```rust
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod openai_compat;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    /// 工具调用（assistant 消息携带，原生协议用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// 工具调用结果（tool 消息携带，原生协议用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: Role::System, content: content.into(), tool_calls: None, tool_call_id: None }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: Role::User, content: content.into(), tool_calls: None, tool_call_id: None }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: content.into(), tool_calls: None, tool_call_id: None }
    }
    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self { role: Role::Assistant, content: content.into(), tool_calls: Some(tool_calls), tool_call_id: None }
    }
    pub fn tool(content: impl Into<String>, tool_call_id: impl Into<String>) -> Self {
        Self { role: Role::Tool, content: content.into(), tool_calls: None, tool_call_id: Some(tool_call_id.into()) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// 参数，JSON 对象
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema 描述参数
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ChatRequest<'a> {
    pub messages: &'a [ChatMessage],
    pub tools: Option<&'a [ToolSpec]>,
}

#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    /// 纯文本回复（可能为空，若模型只发工具调用）
    pub text: Option<String>,
    /// 工具调用（原生协议返回）
    pub tool_calls: Vec<ToolCall>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, req: &ChatRequest<'_>) -> Result<ChatResponse>;
    /// 是否支持原生 function calling
    fn native_tool_calling(&self) -> bool;
}
```

- [ ] **Step 2: 更新 src/lib.rs**

```rust
pub mod config;
pub mod log;
pub mod provider;
```

- [ ] **Step 3: 验证编译**

Run: `cargo build`
Expected: 成功

### Task 2.2: OpenAI 兼容 Provider 实现

**Files:**
- Create: `src/provider/openai_compat.rs`

- [ ] **Step 1: 写 src/provider/openai_compat.rs**

```rust
use crate::provider::{ChatRequest, ChatResponse, Provider, ToolCall, ToolSpec};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct OpenAiCompatibleProvider {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    native_tool_calling: bool,
}

impl OpenAiCompatibleProvider {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, model: impl Into<String>, native_tool_calling: bool) -> Result<Self> {
        Ok(Self {
            client: Client::builder().build()?,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
            native_tool_calling,
        })
    }
}

#[derive(Serialize)]
struct ChatCompletionsRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAiMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

#[derive(Serialize)]
struct OpenAiMessage<'a> {
    role: &'a str,
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiToolCallSer<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
}

#[derive(Serialize)]
struct OpenAiToolCallSer<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    tool_type: &'a str, // "function"
    function: OpenAiFunctionSer<'a>,
}

#[derive(Serialize)]
struct OpenAiFunctionSer<'a> {
    name: &'a str,
    arguments: String, // JSON string
}

#[derive(Serialize)]
struct OpenAiTool {
    #[serde(rename = "type")]
    tool_type: &'a str, // "function"
    function: OpenAiFunctionSpec,
}

#[derive(Serialize)]
struct OpenAiFunctionSpec {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Deserialize)]
struct ChatCompletionsResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<OpenAiToolCallDe>>,
}

#[derive(Deserialize)]
struct OpenAiToolCallDe {
    id: String,
    function: OpenAiFunctionDe,
}

#[derive(Deserialize)]
struct OpenAiFunctionDe {
    name: String,
    arguments: String, // JSON string
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    async fn chat(&self, req: &ChatRequest<'_>) -> Result<ChatResponse> {
        let url = format!("{}/chat/completions", self.base_url);

        // 构造 messages
        let messages: Vec<OpenAiMessage> = req.messages.iter().map(|m| {
            let role = match m.role {
                crate::provider::Role::System => "system",
                crate::provider::Role::User => "user",
                crate::provider::Role::Assistant => "assistant",
                crate::provider::Role::Tool => "tool",
            };
            OpenAiMessage {
                role,
                content: &m.content,
                tool_calls: m.tool_calls.as_ref().map(|tcs| {
                    tcs.iter().map(|tc| OpenAiToolCallSer {
                        id: &tc.id,
                        tool_type: "function",
                        function: OpenAiFunctionSer {
                            name: &tc.name,
                            arguments: tc.arguments.to_string(),
                        },
                    }).collect()
                }),
                tool_call_id: m.tool_call_id.as_deref(),
            }
        }).collect();

        // 构造 tools（仅 native 模式）
        let tools: Option<Vec<OpenAiTool>> = if self.native_tool_calling {
            req.tools.map(|ts| {
                ts.iter().map(|t| OpenAiTool {
                    tool_type: "function",
                    function: OpenAiFunctionSpec {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.parameters.clone(),
                    },
                }).collect()
            })
        } else {
            None
        };

        let tool_choice = if tools.is_some() { Some("auto".to_string()) } else { None };

        let body = ChatCompletionsRequest {
            model: &self.model,
            messages,
            tools,
            tool_choice,
        };

        let mut request = self.client.post(&url).json(&body);
        if !self.api_key.is_empty() {
            request = request.bearer_auth(&self.api_key);
        }

        let resp = request.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("provider returned {}: {}", status, text));
        }

        let parsed: ChatCompletionsResponse = resp.json().await?;
        let choice = parsed.choices.into_iter().next()
            .ok_or_else(|| anyhow!("provider returned no choices"))?;

        let tool_calls = choice.message.tool_calls.unwrap_or_default()
            .into_iter()
            .map(|tc| {
                let args: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(serde_json::Value::Null);
                ToolCall {
                    id: tc.id,
                    name: tc.function.name,
                    arguments: args,
                }
            })
            .collect();

        Ok(ChatResponse {
            text: choice.message.content,
            tool_calls,
        })
    }

    fn native_tool_calling(&self) -> bool {
        self.native_tool_calling
    }
}
```

- [ ] **Step 2: 在 src/provider/mod.rs 末尾追加测试模块**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_message_constructors() {
        let m = ChatMessage::system("hello");
        assert_eq!(m.role, Role::System);
        assert_eq!(m.content, "hello");
        assert!(m.tool_calls.is_none());

        let m = ChatMessage::assistant_with_tools("", vec![ToolCall {
            id: "1".into(),
            name: "file_read".into(),
            arguments: serde_json::json!({"path": "/tmp"}),
        }]);
        assert_eq!(m.role, Role::Assistant);
        assert_eq!(m.tool_calls.as_ref().unwrap().len(), 1);
    }
}
```

- [ ] **Step 3: 验证编译 + 单测**

Run: `cargo test provider`
Expected: 1 个测试通过

### Task 2.3: Provider HTTP 集成测试（mockito）

**Files:**
- Create: `tests/provider_http.rs`

- [ ] **Step 1: 写 tests/provider_http.rs**

```rust
use laia::provider::{openai_compat::OpenAiCompatibleProvider, ChatMessage, ChatRequest, Provider, ToolSpec};
use serde_json::json;

#[tokio::test]
async fn test_native_tool_calling() {
    let mut server = mockito::Server::new_async().await;
    let body = json!({
        "choices": [{
            "message": {
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "function": {
                        "name": "file_read",
                        "arguments": "{\"path\":\"/tmp/x\"}"
                    }
                }]
            }
        }]
    });
    let m = server.mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body.to_string())
        .create_async().await;

    let provider = OpenAiCompatibleProvider::new(server.url(), "", "test-model", true).unwrap();
    let msgs = vec![ChatMessage::user("read /tmp/x")];
    let req = ChatRequest { messages: &msgs, tools: None };
    let resp = provider.chat(&req).await.unwrap();

    m.assert_async().await;
    assert!(resp.text.is_none());
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].name, "file_read");
    assert_eq!(resp.tool_calls[0].arguments, json!({"path": "/tmp/x"}));
}

#[tokio::test]
async fn test_text_response() {
    let mut server = mockito::Server::new_async().await;
    let body = json!({
        "choices": [{
            "message": { "content": "hello back" }
        }]
    });
    let m = server.mock("POST", "/chat/completions")
        .with_status(200)
        .with_body(body.to_string())
        .create_async().await;

    let provider = OpenAiCompatibleProvider::new(server.url(), "", "test-model", true).unwrap();
    let msgs = vec![ChatMessage::user("hi")];
    let req = ChatRequest { messages: &msgs, tools: None };
    let resp = provider.chat(&req).await.unwrap();

    m.assert_async().await;
    assert_eq!(resp.text.as_deref(), Some("hello back"));
    assert!(resp.tool_calls.is_empty());
}

#[tokio::test]
async fn test_error_response() {
    let mut server = mockito::Server::new_async().await;
    let m = server.mock("POST", "/chat/completions")
        .with_status(500)
        .with_body("internal error")
        .create_async().await;

    let provider = OpenAiCompatibleProvider::new(server.url(), "", "test-model", true).unwrap();
    let msgs = vec![ChatMessage::user("hi")];
    let req = ChatRequest { messages: &msgs, tools: None };
    let result = provider.chat(&req).await;

    m.assert_async().await;
    assert!(result.is_err());
}
```

- [ ] **Step 2: 跑测试**

Run: `cargo test --test provider_http`
Expected: 3 个测试通过

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat: provider trait + OpenAI compatible implementation"
```

---

## Phase 3: 工具调用解析器（标签降级）

### Task 3.1: Tool trait 定义

**Files:**
- Create: `src/tools/mod.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 写 src/tools/mod.rs**

```rust
use crate::provider::ToolSpec;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

pub mod file;
pub mod memory;
pub mod terminal;
pub mod tavily;
pub mod web;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema 描述参数
    fn parameters_schema(&self) -> Value;

    async fn execute(&self, args: &Value) -> Result<String>;

    /// 转成 ToolSpec 给 provider
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }
}
```

- [ ] **Step 2: 更新 src/lib.rs**

```rust
pub mod config;
pub mod log;
pub mod provider;
pub mod tools;
```

- [ ] **Step 3: 创建 tools 子模块占位**

```bash
# 创建空文件占位，后续任务填充
```

为每个工具模块创建占位 `src/tools/file.rs`、`src/tools/terminal.rs`、`src/tools/web.rs`、`src/tools/tavily.rs`、`src/tools/memory.rs`，内容暂为空。

- [ ] **Step 4: 验证编译**

Run: `cargo build`
Expected: 成功

### Task 3.2: 标签解析器

**Files:**
- Create: `src/tool_call/mod.rs`
- Create: `src/tool_call/tag_parser.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 写 src/tool_call/tag_parser.rs**

```rust
use crate::provider::ToolCall;
use serde_json::Value;

/// 从模型回复文本中解析 `<tool_call>{...}</tool_call>` 标签。
/// 返回 (纯文本部分, 工具调用列表)。
pub fn parse_tool_calls(text: &str) -> (String, Vec<ToolCall>) {
    let mut clean_text = String::new();
    let mut calls = Vec::new();
    let mut last_end = 0;

    // 匹配 <tool_call>...</tool_call>（容错大小写、空格）
    let re = regex::Regex::new(r"(?is)<tool_call>\s*(.*?)\s*</tool_call>").unwrap();

    for cap in re.captures_iter(text) {
        // 把标签前的纯文本加进去
        let match_start = cap.get(0).unwrap().start();
        clean_text.push_str(&text[last_end..match_start]);
        last_end = cap.get(0).unwrap().end();

        let body = cap.get(1).unwrap().as_str().trim();
        if let Ok(value) = serde_json::from_str::<Value>(body) {
            if let Some(call) = value_to_tool_call(&value) {
                calls.push(call);
                continue;
            }
        }
        // 解析失败：把原始标签文本当作纯文本保留
        clean_text.push_str(cap.get(0).unwrap().as_str());
    }
    clean_text.push_str(&text[last_end..]);

    (clean_text.trim().to_string(), calls)
}

fn value_to_tool_call(value: &Value) -> Option<ToolCall> {
    // 支持两种形态：
    // {"name": "...", "arguments": {...}}
    // {"name": "...", "arguments": "{...}"}
    let name = value.get("name")?.as_str()?.to_string();
    let arguments = match value.get("arguments") {
        Some(Value::Object(_)) => value.get("arguments").cloned().unwrap(),
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        _ => Value::Null,
    };
    let id = format!("tag_{}", uuid::Uuid::new_v4().simple());
    Some(ToolCall { id, name, arguments })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_single_tag() {
        let text = r#"我来读文件 <tool_call>{"name":"file_read","arguments":{"path":"/tmp/x"}}</tool_call> 看看"#;
        let (clean, calls) = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[0].arguments, json!({"path": "/tmp/x"}));
        assert!(clean.contains("我来读文件"));
        assert!(clean.contains("看看"));
        assert!(!clean.contains("tool_call"));
    }

    #[test]
    fn test_multiple_tags() {
        let text = r#"<tool_call>{"name":"a","arguments":{}}</tool_call><tool_call>{"name":"b","arguments":{}}</tool_call>"#;
        let (_, calls) = parse_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
    }

    #[test]
    fn test_no_tag() {
        let text = "普通回复";
        let (clean, calls) = parse_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(clean, "普通回复");
    }

    #[test]
    fn test_string_arguments() {
        let text = r#"<tool_call>{"name":"x","arguments":"{\"k\":1}"}</tool_call>"#;
        let (_, calls) = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, json!({"k": 1}));
    }

    #[test]
    fn test_multiline_body() {
        let text = r#"<tool_call>
{
  "name": "file_write",
  "arguments": {"path": "/tmp/y", "content": "hello"}
}
</tool_call>"#;
        let (_, calls) = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
    }

    #[test]
    fn test_malformed_kept_as_text() {
        let text = r#"<tool_call>not json</tool_call>"#;
        let (clean, calls) = parse_tool_calls(text);
        assert!(calls.is_empty());
        assert!(clean.contains("tool_call"));
    }
}
```

- [ ] **Step 2: 写 src/tool_call/mod.rs**

```rust
pub mod tag_parser;

pub use tag_parser::parse_tool_calls;
```

- [ ] **Step 3: 更新 src/lib.rs**

```rust
pub mod config;
pub mod log;
pub mod provider;
pub mod tool_call;
pub mod tools;
```

- [ ] **Step 4: 跑测试**

Run: `cargo test tool_call`
Expected: 6 个测试通过

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: tag-based tool call parser for fallback mode"
```

### Task 3.3: 标签降级的 system prompt 注入

**Files:**
- Create: `src/tool_call/prompt.rs`
- Modify: `src/tool_call/mod.rs`

- [ ] **Step 1: 写 src/tool_call/prompt.rs**

```rust
use crate::provider::ToolSpec;

/// 构造标签降级模式下的工具协议说明，注入 system prompt。
pub fn build_tool_instructions(tools: &[ToolSpec]) -> String {
    let mut s = String::from("\n\n## Tool Use Protocol\n\n");
    s.push_str("To use a tool, wrap a JSON object in <tool_call></tool_call> tags:\n\n");
    s.push_str("<tool_call>\n{\"name\": \"tool_name\", \"arguments\": {\"param\": \"value\"}}\n</tool_call>\n\n");
    s.push_str("Available tools:\n\n");
    for t in tools {
        s.push_str(&format!("- **{}**: {}\n", t.name, t.description));
        s.push_str(&format!("  parameters: {}\n", t.parameters));
    }
    s
}
```

- [ ] **Step 2: 更新 src/tool_call/mod.rs**

```rust
pub mod prompt;
pub mod tag_parser;

pub use prompt::build_tool_instructions;
pub use tag_parser::parse_tool_calls;
```

- [ ] **Step 3: 加测试到 src/tool_call/prompt.rs**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_build_instructions() {
        let tools = vec![
            ToolSpec {
                name: "file_read".into(),
                description: "Read a file".into(),
                parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            },
        ];
        let s = build_tool_instructions(&tools);
        assert!(s.contains("<tool_call>"));
        assert!(s.contains("file_read"));
        assert!(s.contains("Read a file"));
    }
}
```

- [ ] **Step 4: 跑测试**

Run: `cargo test tool_call`
Expected: 7 个测试通过

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: tool instruction prompt builder for fallback mode"
```

---

## Phase 4: 工具实现

### Task 4.1: 文件工具

**Files:**
- Modify: `src/tools/file.rs`

- [ ] **Step 1: 写 src/tools/file.rs**

```rust
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;

pub struct FileRead;
pub struct FileWrite;
pub struct FileEdit;

#[async_trait]
impl Tool for FileRead {
    fn name(&self) -> &str { "file_read" }
    fn description(&self) -> &str { "Read the content of a file at the given path." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute or relative file path" }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: &Value) -> Result<String> {
        let path = args.get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path' argument"))?;
        let content = tokio::fs::read_to_string(path).await
            .map_err(|e| anyhow!("read {:?}: {}", path, e))?;
        Ok(content)
    }
}

#[async_trait]
impl Tool for FileWrite {
    fn name(&self) -> &str { "file_write" }
    fn description(&self) -> &str { "Write content to a file (overwrites)." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        })
    }
    async fn execute(&self, args: &Value) -> Result<String> {
        let path = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing 'path'"))?;
        let content = args.get("content").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing 'content'"))?;
        tokio::fs::write(path, content).await
            .map_err(|e| anyhow!("write {:?}: {}", path, e))?;
        Ok(format!("wrote {} bytes to {}", content.len(), path))
    }
}

#[async_trait]
impl Tool for FileEdit {
    fn name(&self) -> &str { "file_edit" }
    fn description(&self) -> &str { "Replace old_string with new_string in a file." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    async fn execute(&self, args: &Value) -> Result<String> {
        let path = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing 'path'"))?;
        let old = args.get("old_string").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing 'old_string'"))?;
        let new = args.get("new_string").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing 'new_string'"))?;
        let content = tokio::fs::read_to_string(path).await
            .map_err(|e| anyhow!("read {:?}: {}", path, e))?;
        let new_content = if old.is_empty() {
            new.to_string()
        } else {
            let count = content.matches(old).count();
            if count == 0 {
                return Err(anyhow!("old_string not found in {}", path));
            }
            if count > 1 {
                return Err(anyhow!("old_string appears {} times in {}, need unique match", count, path));
            }
            content.replacen(old, new, 1)
        };
        tokio::fs::write(path, &new_content).await
            .map_err(|e| anyhow!("write {:?}: {}", path, e))?;
        Ok(format!("edited {}", path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::NamedTempFile;
    use std::io::Write;

    #[tokio::test]
    async fn test_file_read_write() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "hello world").unwrap();
        let path = tmp.path().to_str().unwrap();

        let tool = FileRead;
        let result = tool.execute(&json!({"path": path})).await.unwrap();
        assert!(result.contains("hello world"));
    }

    #[tokio::test]
    async fn test_file_write_and_read() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let write_tool = FileWrite;
        write_tool.execute(&json!({"path": &path, "content": "new content"})).await.unwrap();

        let read_tool = FileRead;
        let result = read_tool.execute(&json!({"path": &path})).await.unwrap();
        assert_eq!(result, "new content");
    }

    #[tokio::test]
    async fn test_file_edit() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "line1\nline2\nline3").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let tool = FileEdit;
        tool.execute(&json!({"path": &path, "old_string": "line2", "new_string": "LINE TWO"})).await.unwrap();

        let read = FileRead;
        let result = read.execute(&json!({"path": &path})).await.unwrap();
        assert!(result.contains("LINE TWO"));
        assert!(!result.contains("line2"));
    }

    #[tokio::test]
    async fn test_file_edit_no_match() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "hello").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let tool = FileEdit;
        let result = tool.execute(&json!({"path": &path, "old_string": "missing", "new_string": "x"})).await;
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: 跑测试**

Run: `cargo test tools::file`
Expected: 4 个测试通过

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat: file_read/write/edit tools"
```

### Task 4.2: 终端工具

**Files:**
- Modify: `src/tools/terminal.rs`

- [ ] **Step 1: 写 src/tools/terminal.rs**

```rust
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::io::Write;

pub struct Terminal {
    pub confirm_mode: String,      // "none" / "whitelist" / "always"
    pub whitelist: Vec<String>,
}

impl Terminal {
    pub fn new(confirm_mode: String, whitelist: Vec<String>) -> Self {
        Self { confirm_mode, whitelist }
    }

    /// 检查是否需要确认。返回 true 表示需要 y/n 确认。
    fn needs_confirmation(&self, command: &str) -> bool {
        let first_word = command.split_whitespace().next().unwrap_or("");
        match self.confirm_mode.as_str() {
            "none" => false,
            "always" => true,
            "whitelist" => !self.whitelist.iter().any(|w| w == first_word),
            _ => false,
        }
    }

    /// 同步地从 stdin 读一行 y/n。CLI 频道调用。
    pub fn prompt_confirm(command: &str) -> bool {
        use std::io::{self, BufRead};
        print!("[confirm] run `{}`? (y/N): ", command);
        io::stdout().flush().ok();
        let mut line = String::new();
        if io::stdin().lock().read_line(&mut line).is_err() {
            return false;
        }
        line.trim().eq_ignore_ascii_case("y")
    }
}

#[async_trait]
impl Tool for Terminal {
    fn name(&self) -> &str { "terminal" }
    fn description(&self) -> &str { "Execute a shell command. Returns combined stdout+stderr." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" }
            },
            "required": ["command"]
        })
    }
    async fn execute(&self, args: &Value) -> Result<String> {
        let command = args.get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'command'"))?;

        if self.needs_confirmation(command) {
            // 在异步上下文里同步读 stdin 不理想，v1 简单用 blocking
            if !Self::prompt_confirm(command) {
                return Err(anyhow!("user denied command"));
            }
        }

        // Windows 用 cmd /C，Unix 用 sh -c
        #[cfg(windows)]
        let output = tokio::process::Command::new("cmd").args(["/C", command]).output().await;
        #[cfg(not(windows))]
        let output = tokio::process::Command::new("sh").args(["-c", command]).output().await;

        let output = output.map_err(|e| anyhow!("spawn: {}", e))?;
        let mut combined = String::new();
        if !output.stdout.is_empty() {
            combined.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        if !output.stderr.is_empty() {
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        if !output.status.success() {
            combined.push_str(&format!("\n[exit code: {}]", output.status.code().unwrap_or(-1)));
        }
        Ok(combined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_needs_confirmation_none() {
        let t = Terminal::new("none".into(), vec![]);
        assert!(!t.needs_confirmation("rm -rf /"));
    }

    #[test]
    fn test_needs_confirmation_always() {
        let t = Terminal::new("always".into(), vec![]);
        assert!(t.needs_confirmation("ls"));
    }

    #[test]
    fn test_needs_confirmation_whitelist() {
        let t = Terminal::new("whitelist".into(), vec!["ls".into(), "cat".into()]);
        assert!(!t.needs_confirmation("ls -la"));
        assert!(!t.needs_confirmation("cat foo"));
        assert!(t.needs_confirmation("rm foo"));
    }

    #[tokio::test]
    async fn test_execute_ls() {
        let t = Terminal::new("none".into(), vec![]);
        let result = t.execute(&serde_json::json!({"command": "echo hello"})).await.unwrap();
        assert!(result.contains("hello"));
    }
}
```

- [ ] **Step 2: 跑测试**

Run: `cargo test tools::terminal`
Expected: 4 个测试通过

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat: terminal tool with whitelist confirmation"
```

### Task 4.3: 网页获取工具

**Files:**
- Modify: `src/tools/web.rs`

- [ ] **Step 1: 写 src/tools/web.rs**

```rust
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;

pub struct WebFetch {
    client: reqwest::Client,
}

impl WebFetch {
    pub fn new() -> Result<Self> {
        Ok(Self { client: reqwest::Client::builder().build()? })
    }
}

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &str { "web_fetch" }
    fn description(&self) -> &str { "Fetch the content of a web page (HTML)." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "HTTP(S) URL" }
            },
            "required": ["url"]
        })
    }
    async fn execute(&self, args: &Value) -> Result<String> {
        let url = args.get("url").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing 'url'"))?;
        let resp = self.client.get(url).send().await
            .map_err(|e| anyhow!("fetch {}: {}", url, e))?;
        if !resp.status().is_success() {
            return Err(anyhow!("HTTP {}", resp.status()));
        }
        let text = resp.text().await
            .map_err(|e| anyhow!("read body: {}", e))?;
        // v1 不做 HTML 清洗，原样返回（v2 视需要加 readability 提取）
        Ok(text)
    }
}
```

- [ ] **Step 2: 验证编译**

Run: `cargo build`
Expected: 成功

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat: web_fetch tool"
```

### Task 4.4: Tavily 搜索工具

**Files:**
- Modify: `src/tools/tavily.rs`

- [ ] **Step 1: 写 src/tools/tavily.rs**

```rust
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

const TAVILY_URL: &str = "https://api.tavily.com/search";

pub struct TavilySearch {
    client: reqwest::Client,
    api_key: String,
}

impl TavilySearch {
    pub fn new(api_key: String) -> Result<Self> {
        Ok(Self { client: reqwest::Client::builder().build()?, api_key })
    }
}

#[derive(Deserialize)]
struct TavilyResponse {
    results: Vec<TavilyResult>,
}

#[derive(Deserialize)]
struct TavilyResult {
    title: String,
    url: String,
    content: String,
}

#[async_trait]
impl Tool for TavilySearch {
    fn name(&self) -> &str { "tavily_search" }
    fn description(&self) -> &str { "Search the web via Tavily." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "max_results": { "type": "integer", "default": 5 }
            },
            "required": ["query"]
        })
    }
    async fn execute(&self, args: &Value) -> Result<String> {
        if self.api_key.is_empty() {
            return Err(anyhow!("tavily api_key not configured"));
        }
        let query = args.get("query").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing 'query'"))?;
        let max_results = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(5);

        let body = serde_json::json!({
            "api_key": self.api_key,
            "query": query,
            "max_results": max_results,
        });
        let resp = self.client.post(TAVILY_URL).json(&body).send().await
            .map_err(|e| anyhow!("tavily request: {}", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("tavily {}: {}", status, text));
        }
        let parsed: TavilyResponse = resp.json().await
            .map_err(|e| anyhow!("tavily parse: {}", e))?;

        let mut out = String::new();
        for (i, r) in parsed.results.iter().enumerate() {
            out.push_str(&format!("{}. {}\n   URL: {}\n   {}\n\n", i + 1, r.title, r.url, r.content));
        }
        Ok(out)
    }
}
```

- [ ] **Step 2: 验证编译**

Run: `cargo build`
Expected: 成功

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat: tavily_search tool"
```

### Task 4.5: MEMORY 工具

**Files:**
- Modify: `src/tools/memory.rs`

- [ ] **Step 1: 写 src/tools/memory.rs**

```rust
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// 主 Agent 自动写 MEMORY 用。
pub struct MemoryWrite {
    pub memory_path: PathBuf,
    /// 锁防止并发写
    pub lock: Arc<Mutex<()>>,
}

impl MemoryWrite {
    pub fn new(memory_path: PathBuf) -> Self {
        Self { memory_path, lock: Arc::new(Mutex::new(())) }
    }
}

#[async_trait]
impl Tool for MemoryWrite {
    fn name(&self) -> &str { "memory_write" }
    fn description(&self) -> &str {
        "Write a short factual entry to long-term memory. Use for things the user said to remember."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "entry": { "type": "string", "description": "Short factual entry to remember" }
            },
            "required": ["entry"]
        })
    }
    async fn execute(&self, args: &Value) -> Result<String> {
        let entry = args.get("entry").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("missing 'entry'"))?;
        let _g = self.lock.lock().await;

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let line = format!("- [{}] {}\n", today, entry);

        // 追加写入
        let mut content = tokio::fs::read_to_string(&self.memory_path).await.unwrap_or_default();
        content.push_str(&line);
        tokio::fs::write(&self.memory_path, &content).await
            .map_err(|e| anyhow!("write memory: {}", e))?;
        Ok(format!("remembered: {}", entry))
    }
}
```

- [ ] **Step 2: 加测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_memory_write() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("MEMORY.md");
        let tool = MemoryWrite::new(path.clone());
        tool.execute(&serde_json::json!({"entry": "user likes rust"})).await.unwrap();

        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(content.contains("user likes rust"));
        assert!(content.contains("[2026-") || content.contains("[2025-") || content.contains("[2027-")); // 日期戳
    }
}
```

- [ ] **Step 3: 跑测试**

Run: `cargo test tools::memory`
Expected: 1 个测试通过

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: memory_write tool"
```

---

## Phase 5: 持久化

### Task 5.1: SOUL/USER/MEMORY 加载器

**Files:**
- Create: `src/memory/mod.rs`
- Create: `src/memory/markdown.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 写 src/memory/markdown.rs**

```rust
use anyhow::{Context, Result};
use std::path::PathBuf;

/// 加载 Markdown 文件内容。文件不存在时返回空字符串（不报错）。
pub async fn load_md(path: &PathBuf) -> Result<String> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => Ok(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("read {:?}", path)),
    }
}

/// 当文件不存在时，写入默认模板。
pub async fn ensure_template(path: &PathBuf, template: &str) -> Result<()> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        tokio::fs::write(path, template).await
            .with_context(|| format!("write template {:?}", path))?;
    }
    Ok(())
}

pub const SOUL_TEMPLATE: &str = r#"# 人格

<描述 LAIA 的性格>

# 行为准则

- 简洁直接，不啰嗦
- 不确定时主动询问

# 语气

<对话风格>
"#;

pub const USER_TEMPLATE: &str = r#"# 基本信息

- 姓名：
- 时区：Asia/Shanghai

# 身份绑定

- qq:
- email:
- web:

# 偏好

- 语言：中文
"#;

pub const MEMORY_TEMPLATE: &str = r#"# MEMORY

<!-- 格式：- [YYYY-MM-DD] <条目> -->
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_load_md_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.md");
        let content = load_md(&path).await.unwrap();
        assert_eq!(content, "");
    }

    #[tokio::test]
    async fn test_load_md_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("x.md");
        tokio::fs::write(&path, "hello").await.unwrap();
        let content = load_md(&path).await.unwrap();
        assert_eq!(content, "hello");
    }

    #[tokio::test]
    async fn test_ensure_template_creates() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("SOUL.md");
        ensure_template(&path, SOUL_TEMPLATE).await.unwrap();
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(content.contains("人格"));
    }

    #[tokio::test]
    async fn test_ensure_template_no_overwrite() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("SOUL.md");
        tokio::fs::write(&path, "existing").await.unwrap();
        ensure_template(&path, SOUL_TEMPLATE).await.unwrap();
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content, "existing");
    }
}
```

- [ ] **Step 2: 写 src/memory/mod.rs**

```rust
pub mod markdown;
pub mod sqlite;

pub use markdown::{ensure_template, load_md, MEMORY_TEMPLATE, SOUL_TEMPLATE, USER_TEMPLATE};
```

- [ ] **Step 3: 更新 src/lib.rs**

```rust
pub mod config;
pub mod log;
pub mod memory;
pub mod provider;
pub mod tool_call;
pub mod tools;
```

- [ ] **Step 4: 跑测试**

Run: `cargo test memory::markdown`
Expected: 4 个测试通过

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: markdown loader with templates"
```

### Task 5.2: sqlite 会话持久化

**Files:**
- Create: `src/memory/sqlite.rs`

- [ ] **Step 1: 写 src/memory/sqlite.rs**

```rust
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::provider::{ChatMessage, Role, ToolCall};

pub struct SessionStore {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub session_uuid: String,
    pub channel: String,
    pub created_at: String,
    pub last_activity: String,
    pub token_count: i64,
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: i64,
    pub session_id: i64,
    pub role: String,
    pub content: String,
    pub reasoning_content: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct ToolCallRow {
    pub id: i64,
    pub message_id: i64,
    pub tool_call_id: String,
    pub tool_name: String,
    pub payload: String,
    pub outcome: Option<String>,
    pub created_at: String,
}

impl SessionStore {
    pub fn open(db_path: &PathBuf) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(db_path)
            .with_context(|| format!("open sqlite {:?}", db_path))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        Self::init_schema(&conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(r#"
CREATE TABLE IF NOT EXISTS sessions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    session_uuid  TEXT NOT NULL UNIQUE,
    channel       TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    last_activity TEXT NOT NULL,
    token_count   INTEGER NOT NULL DEFAULT 0,
    state         TEXT NOT NULL DEFAULT 'idle'
);

CREATE TABLE IF NOT EXISTS messages (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id        INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role              TEXT NOT NULL,
    content           TEXT NOT NULL,
    reasoning_content TEXT,
    created_at        TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tool_calls (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id   INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    tool_call_id TEXT NOT NULL,
    tool_name    TEXT NOT NULL,
    payload      TEXT NOT NULL,
    outcome      TEXT,
    created_at   TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
CREATE INDEX IF NOT EXISTS idx_tool_calls_message ON tool_calls(message_id);
"#)?;
        Ok(())
    }

    /// 创建新会话，返回 session_id（内部数字 id）
    pub fn create_session(&self, session_uuid: &str, channel: &str) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (session_uuid, channel, created_at, last_activity, state) VALUES (?1, ?2, ?3, ?3, 'idle')",
            rusqlite::params![session_uuid, channel, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 查最近一个 session（按 last_activity 排序），返回 (数字 id, uuid)
    pub fn latest_session(&self) -> Result<Option<(i64, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, session_uuid FROM sessions ORDER BY last_activity DESC LIMIT 1")?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            Ok(Some((row.get(0)?, row.get(1)?)))
        } else {
            Ok(None)
        }
    }

    /// 追加一条消息，返回 message id
    pub fn append_message(&self, session_id: i64, role: &Role, content: &str) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        let role_str = match role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO messages (session_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, role_str, content, now],
        )?;
        let msg_id = conn.last_insert_rowid();
        // 更新 session 的 last_activity
        conn.execute(
            "UPDATE sessions SET last_activity = ?1 WHERE id = ?2",
            rusqlite::params![now, session_id],
        )?;
        Ok(msg_id)
    }

    /// 关联工具调用到某条消息
    pub fn append_tool_call(&self, message_id: i64, tool_call_id: &str, tool_name: &str, payload: &str, outcome: Option<&str>) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO tool_calls (message_id, tool_call_id, tool_name, payload, outcome, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![message_id, tool_call_id, tool_name, payload, outcome, now],
        )?;
        Ok(())
    }

    /// 读最近 N 条消息（按 id 升序）
    pub fn recent_messages(&self, session_id: i64, limit: i64) -> Result<Vec<MessageRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, reasoning_content, created_at
             FROM messages WHERE session_id = ?1 ORDER BY id DESC LIMIT ?2"
        )?;
        let rows: Result<Vec<MessageRow>, _> = stmt.query_map(rusqlite::params![session_id, limit], |row| {
            Ok(MessageRow {
                id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                reasoning_content: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?.collect();
        let mut msgs = rows?;
        msgs.reverse(); // 改回时间升序
        Ok(msgs)
    }

    /// 更新 session token 计数
    pub fn update_token_count(&self, session_id: i64, delta: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET token_count = token_count + ?1 WHERE id = ?2",
            rusqlite::params![delta, session_id],
        )?;
        Ok(())
    }

    /// 列出某 session 的全部消息（调试/导出用）
    pub fn all_messages(&self, session_id: i64) -> Result<Vec<MessageRow>> {
        self.recent_messages(session_id, i64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn open_temp() -> SessionStore {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.db");
        SessionStore::open(&path).unwrap()
    }

    #[test]
    fn test_create_and_latest_session() {
        let store = open_temp();
        let id1 = store.create_session("uuid-1", "cli").unwrap();
        let id2 = store.create_session("uuid-2", "cli").unwrap();
        let latest = store.latest_session().unwrap().unwrap();
        assert_eq!(latest.0, id2);
        assert_eq!(latest.1, "uuid-2");
        // 让 id1 成为最新
        store.append_message(id1, &Role::User, "hi").unwrap();
        let latest = store.latest_session().unwrap().unwrap();
        assert_eq!(latest.0, id1);
    }

    #[test]
    fn test_append_and_read_messages() {
        let store = open_temp();
        let sid = store.create_session("uuid", "cli").unwrap();
        store.append_message(sid, &Role::User, "hello").unwrap();
        store.append_message(sid, &Role::Assistant, "hi back").unwrap();
        let msgs = store.recent_messages(sid, 10).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "hello");
        assert_eq!(msgs[1].content, "hi back");
    }

    #[test]
    fn test_tool_call_persistence() {
        let store = open_temp();
        let sid = store.create_session("uuid", "cli").unwrap();
        let msg_id = store.append_message(sid, &Role::Assistant, "calling tool").unwrap();
        store.append_tool_call(msg_id, "call_1", "file_read", "{\"path\":\"/tmp\"}", Some("content")).unwrap();
        // 读回验证（直接查 tool_calls 表）
        let conn = store.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tool_calls WHERE message_id = ?1",
            rusqlite::params![msg_id],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 1);
    }
}
```

- [ ] **Step 2: 跑测试**

Run: `cargo test memory::sqlite`
Expected: 3 个测试通过

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat: sqlite session store with schema"
```

### Task 5.3: MEMORY 压缩（LLM 去重）

**Files:**
- Modify: `src/memory/markdown.rs`

- [ ] **Step 1: 在 src/memory/markdown.rs 追加压缩逻辑**

```rust
use crate::provider::{ChatMessage, ChatRequest, ChatResponse, Provider, Role};

/// MEMORY.md 压缩：先备份，再调 LLM 去重压缩，覆写。
pub async fn compress_memory(
    memory_path: &PathBuf,
    provider: &dyn Provider,
    backup_dir: &PathBuf,
) -> Result<()> {
    let content = tokio::fs::read_to_string(memory_path).await
        .with_context(|| format!("read {:?}", memory_path))?;

    // 备份
    tokio::fs::create_dir_all(backup_dir).await.ok();
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let backup_path = backup_dir.join(format!("MEMORY.{}.md", ts));
    tokio::fs::write(&backup_path, &content).await?;

    // 调 LLM 压缩
    let system = "You are a memory compactor. Given a list of memory entries, output a deduplicated, compressed version. Keep the same format: '- [YYYY-MM-DD] <entry>'. Remove duplicates and merge related entries. Preserve dates. Output only the list, no commentary.";
    let user = format!("Compress this memory:\n\n{}", content);
    let messages = vec![ChatMessage::system(system), ChatMessage::user(user)];
    let req = ChatRequest { messages: &messages, tools: None };
    let resp: ChatResponse = provider.chat(&req).await?;
    let new_content = resp.text.unwrap_or_default();

    if !new_content.trim().is_empty() {
        tokio::fs::write(memory_path, &new_content).await?;
    }
    Ok(())
}
```

- [ ] **Step 2: Commit**

```bash
git add -A
git commit -m "feat: memory compression via LLM"
```

---

## Phase 6: 上下文管理

### Task 6.1: 上下文结构 + token 估算

**Files:**
- Create: `src/agent/context.rs`
- Create: `src/agent/mod.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 写 src/agent/context.rs**

```rust
use crate::provider::ChatMessage;

/// 当前会话的上下文窗口。
pub struct Context {
    /// system prompt（SOUL + USER + MEMORY 拼接）
    pub system: String,
    /// 历史消息（不含 system）
    pub history: Vec<ChatMessage>,
    /// 压缩后的旧消息摘要（若有）
    pub summary: Option<String>,
}

impl Context {
    pub fn new(system: String) -> Self {
        Self { system, history: Vec::new(), summary: None }
    }

    /// 追加消息
    pub fn push(&mut self, msg: ChatMessage) {
        self.history.push(msg);
    }

    /// 拼给 provider 的完整消息列表（含 system）
    pub fn to_messages(&self) -> Vec<ChatMessage> {
        let mut msgs = vec![ChatMessage::system(&self.system)];
        if let Some(s) = &self.summary {
            msgs.push(ChatMessage::system(format!("[Previous conversation summary]\n{}", s)));
        }
        msgs.extend(self.history.iter().cloned());
        msgs
    }

    /// 粗略 token 估算：v1 用字符数 / 4（中英混合近似）。
    pub fn estimate_tokens(&self) -> usize {
        let system_tokens = self.system.chars().count() / 4;
        let summary_tokens = self.summary.as_ref().map(|s| s.chars().count() / 4).unwrap_or(0);
        let history_tokens: usize = self.history.iter().map(|m| m.content.chars().count() / 4).sum();
        system_tokens + summary_tokens + history_tokens
    }

    /// 清空历史（不清 system 和 summary）
    pub fn clear(&mut self) {
        self.history.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Role;

    #[test]
    fn test_to_messages_includes_system() {
        let ctx = Context::new("SOUL".into());
        let msgs = ctx.to_messages();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::System);
    }

    #[test]
    fn test_summary_inserted() {
        let mut ctx = Context::new("SOUL".into());
        ctx.summary = Some("old stuff".into());
        ctx.push(ChatMessage::user("hi"));
        let msgs = ctx.to_messages();
        assert_eq!(msgs.len(), 3); // system + summary + user
        assert_eq!(msgs[1].role, Role::System);
    }

    #[test]
    fn test_token_estimate() {
        let mut ctx = Context::new("a".repeat(40)); // 10 tokens
        ctx.push(ChatMessage::user("b".repeat(40))); // 10 tokens
        assert_eq!(ctx.estimate_tokens(), 20);
    }
}
```

- [ ] **Step 2: 写 src/agent/mod.rs 占位**

```rust
pub mod context;
pub mod runner;
```

- [ ] **Step 3: 更新 src/lib.rs**

```rust
pub mod agent;
pub mod config;
pub mod log;
pub mod memory;
pub mod provider;
pub mod tool_call;
pub mod tools;
```

- [ ] **Step 4: 跑测试**

Run: `cargo test agent::context`
Expected: 3 个测试通过

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: context window with token estimation"
```

### Task 6.2: 上下文压缩

**Files:**
- Modify: `src/agent/context.rs`

- [ ] **Step 1: 在 src/agent/context.rs 追加压缩逻辑**

```rust
use crate::provider::{ChatRequest, Provider, Role};
use anyhow::Result;

impl Context {
    /// 检查是否达到压缩阈值。
    /// current_tokens / max_tokens > threshold
    pub fn needs_compaction(&self, max_tokens: usize, threshold: f64) -> bool {
        let current = self.estimate_tokens();
        (current as f64 / max_tokens as f64) > threshold
    }

    /// 压缩：保留 system + 最近几条 + 旧消息摘要。
    /// keep_recent: 保留最近 N 条不压缩
    pub async fn compact(&mut self, provider: &dyn Provider, keep_recent: usize) -> Result<()> {
        if self.history.len() <= keep_recent {
            return Ok(());
        }
        let to_compress: Vec<ChatMessage> = self.history[..self.history.len() - keep_recent].to_vec();
        let to_keep: Vec<ChatMessage> = self.history[self.history.len() - keep_recent..].to_vec();

        // 构造摘要请求
        let mut dump = String::new();
        for m in &to_compress {
            dump.push_str(&format!("[{}] {}\n", match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            }, m.content));
        }

        let system = "You are a conversation summarizer. Summarize the following conversation into a concise paragraph preserving key facts, decisions, and context. Output only the summary.";
        let messages = vec![
            ChatMessage::system(system),
            ChatMessage::user(dump),
        ];
        let req = ChatRequest { messages: &messages, tools: None };
        let resp = provider.chat(&req).await?;
        let summary = resp.text.unwrap_or_default();

        // 合并到已有 summary
        let new_summary = match &self.summary {
            Some(old) => format!("{}\n\n[Later]\n{}", old, summary),
            None => summary,
        };
        self.summary = Some(new_summary);
        self.history = to_keep;
        Ok(())
    }
}
```

- [ ] **Step 2: 加测试**

```rust
#[cfg(test)]
mod compact_tests {
    use super::*;
    use async_trait::async_trait;
    use crate::provider::{ChatRequest, ChatResponse};

    struct MockProvider;
    #[async_trait]
    impl Provider for MockProvider {
        async fn chat(&self, _req: &ChatRequest<'_>) -> Result<ChatResponse> {
            Ok(ChatResponse { text: Some("summary of old".into()), tool_calls: vec![] })
        }
        fn native_tool_calling(&self) -> bool { true }
    }

    #[tokio::test]
    async fn test_compact() {
        let mut ctx = Context::new("SOUL".into());
        for i in 0..10 {
            ctx.push(ChatMessage::user(format!("msg {}", i)));
        }
        ctx.compact(&MockProvider, 3).await.unwrap();
        assert_eq!(ctx.history.len(), 3);
        assert!(ctx.summary.is_some());
        assert!(ctx.summary.as_ref().unwrap().contains("summary of old"));
    }

    #[test]
    fn test_needs_compaction() {
        let mut ctx = Context::new("a".repeat(80)); // 20 tokens
        ctx.push(ChatMessage::user("b".repeat(80))); // 20 tokens
        // max=100, threshold=0.3 → 40/100=0.4 > 0.3 → true
        assert!(ctx.needs_compaction(100, 0.3));
        // max=100, threshold=0.5 → 40/100=0.4 < 0.5 → false
        assert!(!ctx.needs_compaction(100, 0.5));
    }
}
```

- [ ] **Step 3: 跑测试**

Run: `cargo test agent::context`
Expected: 5 个测试通过

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: context compaction via LLM summary"
```

---

## Phase 7: Agent 主循环

### Task 7.1: 工具执行器

**Files:**
- Create: `src/agent/runner.rs`

- [ ] **Step 1: 写 src/agent/runner.rs**

```rust
use crate::provider::{ChatMessage, ChatResponse, ToolCall};
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Arc;

/// 工具注册表。按 name 查找。
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: HashMap::new() }
    }
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }
    pub fn specs(&self) -> Vec<crate::provider::ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

/// 执行一批工具调用，返回对应的 tool 消息（结果）。
pub async fn execute_tool_calls(
    registry: &ToolRegistry,
    calls: &[ToolCall],
) -> Result<Vec<ChatMessage>> {
    let mut results = Vec::new();
    for call in calls {
        let tool = registry.get(&call.name)
            .ok_or_else(|| anyhow!("unknown tool: {}", call.name))?;
        tracing::info!(tool = %call.name, args = %call.arguments, "executing tool");
        let outcome = match tool.execute(&call.arguments).await {
            Ok(s) => s,
            Err(e) => format!("[error: {}]", e),
        };
        tracing::info!(tool = %call.name, len = outcome.len(), "tool done");
        results.push(ChatMessage::tool(outcome, &call.id));
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::{json, Value};

    struct EchoTool;
    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str { "echo" }
        fn description(&self) -> &str { "echo back" }
        fn parameters_schema(&self) -> Value { json!({"type":"object"}) }
        async fn execute(&self, args: &Value) -> Result<String> {
            Ok(format!("{}", args))
        }
    }

    #[tokio::test]
    async fn test_execute_calls() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let calls = vec![ToolCall {
            id: "1".into(),
            name: "echo".into(),
            arguments: json!({"x": 1}),
        }];
        let msgs = execute_tool_calls(&reg, &calls).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, crate::provider::Role::Tool);
        assert!(msgs[0].content.contains("x"));
    }

    #[tokio::test]
    async fn test_unknown_tool() {
        let reg = ToolRegistry::new();
        let calls = vec![ToolCall {
            id: "1".into(),
            name: "missing".into(),
            arguments: json!({}),
        }];
        let result = execute_tool_calls(&reg, &calls).await;
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: 跑测试**

Run: `cargo test agent::runner`
Expected: 2 个测试通过

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat: tool registry and executor"
```

### Task 7.2: Agent 主循环

**Files:**
- Modify: `src/agent/mod.rs`

- [ ] **Step 1: 写 src/agent/mod.rs**

```rust
pub mod context;
pub mod runner;

use crate::agent::context::Context;
use crate::agent::runner::{execute_tool_calls, ToolRegistry};
use crate::config::Config;
use crate::memory::sqlite::SessionStore;
use crate::provider::{
    ChatMessage, ChatRequest, ChatResponse, Provider, Role, ToolCall,
};
use crate::tool_call::{build_tool_instructions, parse_tool_calls};
use anyhow::Result;
use std::sync::Arc;

pub struct Agent {
    pub provider: Arc<dyn Provider>,
    pub tools: Arc<ToolRegistry>,
    pub context: Context,
    pub session_store: Arc<SessionStore>,
    pub session_id: i64,
    pub max_tokens: usize,
    pub context_threshold: f64,
}

impl Agent {
    pub async fn new(
        config: &Config,
        provider: Arc<dyn Provider>,
        tools: Arc<ToolRegistry>,
        session_store: Arc<SessionStore>,
        session_id: i64,
        system_prompt: String,
        max_tokens: usize,
    ) -> Self {
        let agent_cfg = config.agent.get("main").cloned().unwrap_or_default_assumed();
        Self {
            provider,
            tools,
            context: Context::new(system_prompt),
            session_store,
            session_id,
            max_tokens,
            context_threshold: agent_cfg.context_threshold,
        }
    }

    /// 处理一条用户输入，返回助手回复文本。
    /// 最多迭代 max_iters 次工具调用。
    pub async fn handle_input(&mut self, user_input: &str, channel: &str) -> Result<String> {
        // 持久化用户消息
        self.session_store.append_message(self.session_id, &Role::User, user_input)?;
        // 入上下文
        self.context.push(ChatMessage::user(user_input));

        // 自动压缩检查
        if self.context.needs_compaction(self.max_tokens, self.context_threshold) {
            if let Err(e) = self.context.compact(self.provider.as_ref(), 6).await {
                tracing::warn!(error = %e, "auto-compact failed");
            }
        }

        let max_iters = 10;
        for i in 0..max_iters {
            let messages = self.context.to_messages();
            let tools = self.tools.specs();
            let tools_ref = if tools.is_empty() { None } else { Some(tools.as_slice()) };
            let req = ChatRequest { messages: &messages, tools: tools_ref };

            let resp = self.provider.chat(&req).await?;

            // 标签降级：若 native=false 且 resp.tool_calls 为空，尝试从 text 解析
            let (final_text, final_calls) = if !self.provider.native_tool_calling() {
                let text = resp.text.unwrap_or_default();
                let (clean, calls) = parse_tool_calls(&text);
                (Some(clean), calls)
            } else {
                (resp.text, resp.tool_calls)
            };

            if final_calls.is_empty() {
                // 无工具调用，本轮结束
                let text = final_text.unwrap_or_default();
                self.session_store.append_message(self.session_id, &Role::Assistant, &text)?;
                self.context.push(ChatMessage::assistant(&text));
                return Ok(text);
            }

            // 有工具调用：把 assistant 的工具调用消息入上下文
            let assistant_msg = ChatMessage::assistant_with_tools(
                final_text.clone().unwrap_or_default(),
                final_calls.clone(),
            );
            self.session_store.append_message(self.session_id, &Role::Assistant, &final_text.clone().unwrap_or_default())?;
            self.context.push(assistant_msg);

            // 持久化 tool_calls
            for tc in &final_calls {
                self.session_store.append_tool_call(
                    self.session_id, // 注意：这里应该用 message_id，v1 简化
                    &tc.id,
                    &tc.name,
                    &tc.arguments.to_string(),
                    None,
                ).ok();
            }

            // 执行工具
            let tool_msgs = execute_tool_calls(&self.tools, &final_calls).await?;
            for (i, msg) in tool_msgs.iter().enumerate() {
                self.session_store.append_message(self.session_id, &Role::Tool, &msg.content)?;
                self.context.push(msg.clone());
                // 记录 outcome 到 tool_calls 表（v1 简化：用消息 id 关联）
                let _ = i;
            }

            tracing::info!(iter = i, "tool iteration done");
        }

        let fallback = "[reached max tool iterations]";
        self.session_store.append_message(self.session_id, &Role::Assistant, fallback)?;
        self.context.push(ChatMessage::assistant(fallback));
        Ok(fallback.into())
    }
}

// 临时辅助：Config 缺 agent.main 时的兜底（实际应来自配置加载时保证存在）
trait AgentConfigExt {
    fn default_assumed() -> Self;
}

impl AgentConfigExt for crate::config::AgentConfig {
    fn default_assumed() -> Self {
        Self {
            context_threshold: 0.7,
            soul: String::new(),
            user: String::new(),
            memory: String::new(),
        }
    }
}
```

- [ ] **Step 2: 修复 tool_calls 持久化用 message_id**

注意 Step 1 中 `append_tool_call` 的第一个参数应是 message_id 而非 session_id。修正 `handle_input`：

把
```rust
self.session_store.append_message(self.session_id, &Role::Assistant, &final_text.clone().unwrap_or_default())?;
self.context.push(assistant_msg);
```
改为：
```rust
let assistant_msg_id = self.session_store.append_message(self.session_id, &Role::Assistant, &final_text.clone().unwrap_or_default())?;
self.context.push(assistant_msg);

// 持久化 tool_calls（关联到 assistant 消息）
for tc in &final_calls {
    self.session_store.append_tool_call(
        assistant_msg_id,
        &tc.id,
        &tc.name,
        &tc.arguments.to_string(),
        None,
    ).ok();
}
```

- [ ] **Step 3: 验证编译**

Run: `cargo build`
Expected: 成功

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: agent main loop with tool iteration"
```

---

## Phase 8: CLI + REPL + 斜杠命令

### Task 8.1: CLI 子命令定义

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: 写 src/main.rs 框架**

```rust
use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "laia", version = "0.1.0", about = "Lightweight AI Assistant")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 进入交互式对话（默认）
    Chat,
    /// 打印当前配置
    Config,
    /// 诊断 provider 连通性、文件完整性
    Doctor,
    /// 写一条记忆
    Remember { text: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Commands::Chat);
    match command {
        Commands::Chat => laia::commands::chat_cmd().await,
        Commands::Config => laia::commands::config_cmd(),
        Commands::Doctor => laia::commands::doctor_cmd().await,
        Commands::Remember { text } => laia::commands::remember_cmd(&text).await,
    }
}
```

- [ ] **Step 2: 创建 src/commands/mod.rs 占位**

```rust
pub mod slash;

use anyhow::Result;

pub async fn chat_cmd() -> Result<()> {
    crate::channels::cli::run_repl().await
}

pub fn config_cmd() -> Result<()> {
    let cfg = crate::commands::load_config_or_init()?;
    println!("{}", toml::to_string_pretty(&cfg).unwrap_or_default());
    Ok(())
}

pub async fn doctor_cmd() -> Result<()> {
    let cfg = crate::commands::load_config_or_init()?;
    println!("workspace dir: {}", cfg.workspace.dir);
    println!("soul: {}", cfg.agent.get("main").map(|a| a.soul.as_str()).unwrap_or("(missing)"));
    println!("user: {}", cfg.agent.get("main").map(|a| a.user.as_str()).unwrap_or("(missing)"));
    println!("memory: {}", cfg.agent.get("main").map(|a| a.memory.as_str()).unwrap_or("(missing)"));

    // 测 provider 连通性
    if let Some(p) = cfg.provider.get("default") {
        println!("\nprovider: {}", p.base_url);
        match reqwest::Client::new().get(format!("{}/models", p.base_url.trim_end_matches('/'))).send().await {
            Ok(resp) => println!("  status: {}", resp.status()),
            Err(e) => println!("  error: {}", e),
        }
    }
    Ok(())
}

pub async fn remember_cmd(text: &str) -> Result<()> {
    let cfg = crate::commands::load_config_or_init()?;
    let agent_cfg = cfg.agent.get("main").cloned().ok_or_else(|| anyhow::anyhow!("agent.main not configured"))?;
    let memory_path = std::path::PathBuf::from(&agent_cfg.memory);
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let line = format!("- [{}] {}\n", today, text);
    let mut content = tokio::fs::read_to_string(&memory_path).await.unwrap_or_default();
    content.push_str(&line);
    tokio::fs::write(&memory_path, &content).await?;
    println!("remembered: {}", text);
    Ok(())
}

pub fn load_config_or_init() -> Result<crate::config::Config> {
    use std::path::PathBuf;
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let config_path = home.join(".laia/config.toml");
    if config_path.exists() {
        crate::config::Config::load(&config_path)
    } else {
        // 用默认配置，不写文件（让用户自己决定）
        Ok(crate::config::Config::default_for_workspace("~/.laia"))
    }
}
```

- [ ] **Step 3: 创建 src/channels/mod.rs 占位**

```rust
pub mod cli;
```

- [ ] **Step 4: 创建 src/channels/cli.rs 占位**

```rust
use anyhow::Result;

pub async fn run_repl() -> Result<()> {
    println!("laia v0.1.0 - type /help for commands, /exit to quit");
    // 真正的实现在 Task 8.2
    Ok(())
}
```

- [ ] **Step 5: 更新 src/lib.rs**

```rust
pub mod agent;
pub mod channels;
pub mod commands;
pub mod config;
pub mod log;
pub mod memory;
pub mod provider;
pub mod tool_call;
pub mod tools;
```

- [ ] **Step 6: 验证编译**

Run: `cargo build`
Expected: 成功

- [ ] **Step 7: 测试子命令**

Run: `cargo run -- config`
Expected: 打印默认配置

Run: `cargo run -- doctor`
Expected: 打印 workspace 和 provider 状态

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat: CLI subcommands (chat/config/doctor/remember)"
```

### Task 8.2: 斜杠命令

**Files:**
- Create: `src/commands/slash.rs`

- [ ] **Step 1: 写 src/commands/slash.rs**

```rust
use crate::agent::Agent;
use anyhow::Result;

pub enum SlashOutcome {
    /// 命令已处理，REPL 继续
    Handled,
    /// 退出 REPL
    Exit,
    /// 不是斜杠命令，作为普通输入处理
    NotSlash,
}

/// 尝试处理斜杠命令。返回 None 表示不是斜杠命令。
pub async fn try_handle(line: &str, agent: &mut Agent) -> Result<SlashOutcome> {
    let trimmed = line.trim();
    if !trimmed.starts_with('/') {
        return Ok(SlashOutcome::NotSlash);
    }
    let (cmd, args) = match trimmed.split_once(' ') {
        Some((c, a)) => (c, a.trim()),
        None => (trimmed, ""),
    };
    match cmd {
        "/exit" | "/quit" => Ok(SlashOutcome::Exit),
        "/help" => {
            println!("commands: /new /exit /compact /clear /remember <text> /config /help");
            Ok(SlashOutcome::Handled)
        }
        "/new" => {
            // v1 简化：清空内存上下文（sqlite 留底），下一条输入开新 session
            agent.context.clear();
            agent.context.summary = None;
            println!("[new session]");
            Ok(SlashOutcome::Handled)
        }
        "/clear" => {
            agent.context.clear();
            agent.context.summary = None;
            println!("[context cleared]");
            Ok(SlashOutcome::Handled)
        }
        "/compact" => {
            match agent.context.compact(agent.provider.as_ref(), 6).await {
                Ok(_) => println!("[compacted]"),
                Err(e) => println!("[compact failed: {}]", e),
            }
            Ok(SlashOutcome::Handled)
        }
        "/remember" => {
            if args.is_empty() {
                println!("usage: /remember <text>");
            } else {
                let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                let line = format!("- [{}] {}\n", today, args);
                // 用 memory_write 工具间接写
                if let Some(tool) = agent.tools.get("memory_write") {
                    let _ = tool.execute(&serde_json::json!({"entry": args})).await;
                    println!("[remembered]");
                } else {
                    println!("[memory_write tool not registered]");
                }
            }
            Ok(SlashOutcome::Handled)
        }
        "/config" => {
            println!("context_threshold: {}", agent.context_threshold);
            println!("max_tokens: {}", agent.max_tokens);
            println!("history msgs: {}", agent.context.history.len());
            println!("summary: {}", agent.context.summary.is_some());
            println!("tools: {:?}", agent.tools.names());
            Ok(SlashOutcome::Handled)
        }
        _ => {
            println!("[unknown command: {}]", cmd);
            Ok(SlashOutcome::Handled)
        }
    }
}
```

- [ ] **Step 2: 验证编译**

Run: `cargo build`
Expected: 成功

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat: slash commands"
```

### Task 8.3: CLI REPL 完整实现

**Files:**
- Modify: `src/channels/cli.rs`

- [ ] **Step 1: 写 src/channels/cli.rs**

```rust
use crate::agent::runner::ToolRegistry;
use crate::agent::Agent;
use crate::commands::slash::{try_handle, SlashOutcome};
use crate::config::Config;
use crate::memory::sqlite::SessionStore;
use crate::memory::{ensure_template, load_md, MEMORY_TEMPLATE, SOUL_TEMPLATE, USER_TEMPLATE};
use crate::provider::openai_compat::OpenAiCompatibleProvider;
use crate::provider::Provider;
use crate::tools::file::{FileEdit, FileRead, FileWrite};
use crate::tools::memory::MemoryWrite;
use crate::tools::tavily::TavilySearch;
use crate::tools::terminal::Terminal;
use crate::tools::web::WebFetch;
use anyhow::Result;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

pub async fn run_repl() -> Result<()> {
    let config = crate::commands::load_config_or_init()?;

    // 初始化日志
    let log_dir = PathBuf::from(&config.log.dir);
    let _ = crate::log::init(&config.log.level, &log_dir);

    // 确保工作区和 md 模板
    let workspace = PathBuf::from(&config.workspace.dir);
    std::fs::create_dir_all(&workspace).ok();
    let agent_cfg = config.agent.get("main").cloned().ok_or_else(|| anyhow::anyhow!("agent.main not configured"))?;
    let soul_path = PathBuf::from(&agent_cfg.soul);
    let user_path = PathBuf::from(&agent_cfg.user);
    let memory_path = PathBuf::from(&agent_cfg.memory);
    ensure_template(&soul_path, SOUL_TEMPLATE).await?;
    ensure_template(&user_path, USER_TEMPLATE).await?;
    ensure_template(&memory_path, MEMORY_TEMPLATE).await?;

    // 加载 md 拼成 system prompt
    let soul = load_md(&soul_path).await?;
    let user = load_md(&user_path).await?;
    let memory = load_md(&memory_path).await?;
    let system_prompt = format!("# SOUL\n{}\n\n# USER\n{}\n\n# MEMORY\n{}", soul, user, memory);

    // Provider
    let prov_cfg = config.provider.get("default").cloned().ok_or_else(|| anyhow::anyhow!("provider.default not configured"))?;
    let provider: Arc<dyn Provider> = Arc::new(OpenAiCompatibleProvider::new(
        &prov_cfg.base_url, &prov_cfg.api_key, &prov_cfg.model, prov_cfg.native_tool_calling,
    )?);

    // 工具注册
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(FileRead));
    registry.register(Arc::new(FileWrite));
    registry.register(Arc::new(FileEdit));
    registry.register(Arc::new(Terminal::new(
        config.tools.terminal.confirm.clone(),
        config.tools.terminal.whitelist.clone(),
    )));
    registry.register(Arc::new(WebFetch::new()?));
    if !config.tools.tavily.api_key.is_empty() {
        registry.register(Arc::new(TavilySearch::new(config.tools.tavily.api_key.clone())?));
    }
    registry.register(Arc::new(MemoryWrite::new(memory_path.clone())));
    let registry = Arc::new(registry);

    // sqlite
    let db_path = workspace.join("sessions.db");
    let session_store = Arc::new(SessionStore::open(&db_path)?);

    // 会话：复用最近一个，或新建
    let session_id = match session_store.latest_session()? {
        Some((id, _)) => id,
        None => {
            let uuid = uuid::Uuid::new_v4().to_string();
            session_store.create_session(&uuid, "cli")?
        }
    };

    let mut agent = Agent::new(
        &config,
        provider,
        registry,
        session_store,
        session_id,
        system_prompt,
        8192, // v1 硬编码 max_tokens，v2 可配
    ).await;

    // REPL 循环
    println!("laia v0.1.0 - type /help for commands, /exit to quit\n");
    let stdin = std::io::stdin();
    loop {
        print!("> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break; // EOF
        }
        let line = line.trim();
        if line.is_empty() { continue; }

        match try_handle(line, &mut agent).await? {
            SlashOutcome::Exit => break,
            SlashOutcome::Handled => continue,
            SlashOutcome::NotSlash => {
                match agent.handle_input(line, "cli").await {
                    Ok(resp) => println!("\n{}\n", resp),
                    Err(e) => println!("\n[error: {}]\n", e),
                }
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 2: 验证编译**

Run: `cargo build`
Expected: 成功

- [ ] **Step 3: 手动冒烟测试**

```bash
cargo run -- chat
> /help
> /config
> /exit
```

Expected: 不崩溃，命令响应正常

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: CLI REPL with full agent integration"
```

---

## Phase 9: 集成验收

### Task 9.1: 端到端冒烟测试

**Files:**
- Create: `tests/integration.rs`

- [ ] **Step 1: 写 tests/integration.rs（需真实 provider，用 #[ignore]）**

```rust
use laia::provider::{openai_compat::OpenAiCompatibleProvider, ChatMessage, ChatRequest, Provider};

/// 这个测试需要本地有 Ollama 跑 qwen2.5:7b。
/// 跑法：cargo test --test integration -- --ignored
#[tokio::test]
#[ignore]
async fn smoke_real_provider() {
    let provider = OpenAiCompatibleProvider::new(
        "http://localhost:11434/v1", "", "qwen2.5:7b", false,
    ).unwrap();
    let msgs = vec![ChatMessage::user("say hi in 3 words")];
    let req = ChatRequest { messages: &msgs, tools: None };
    let resp = provider.chat(&req).await.unwrap();
    assert!(resp.text.is_some());
    println!("reply: {:?}", resp.text);
}
```

- [ ] **Step 2: 跑普通测试**

Run: `cargo test`
Expected: 全部非 ignore 测试通过

- [ ] **Step 3: 跑冒烟测试（需 Ollama）**

Run: `cargo test --test integration -- --ignored`
Expected: 通过并打印 reply

### Task 9.2: 手动验收清单

- [ ] **Step 1: 完整手动验收**

按 README v1 验收标准走一遍：

```bash
# 1. 初始化
mkdir -p ~/.laia
cp <默认配置> ~/.laia/config.toml  # 手写或用 laia doctor 看默认
# 编辑 config.toml 填 provider.base_url 和 model

# 2. doctor
cargo run -- doctor
# 期望：workspace 路径打印 + provider status 200

# 3. chat 多轮
cargo run -- chat
> 你好
> 我叫 THAD
> 我刚叫什么？
# 期望：记得上一轮

# 4. 工具调用（terminal）
> 列一下当前目录的文件
# 期望：调用 terminal 工具，返回 ls 结果

# 5. 工具调用（file）
> 在 ~/.laia/test.txt 写入 "hello from laia"
# 期望：调用 file_write

# 6. remember
> /remember 用户喜欢 Rust
# 期望：写入 MEMORY.md

# 7. config
> /config
# 期望：打印当前配置

# 8. compact
> /compact
# 期望：压缩上下文

# 9. new
> /new
# 期望：开新会话

# 10. 退出
> /exit
```

### Task 9.3: clippy + 最终 commit

- [ ] **Step 1: clippy**

Run: `cargo clippy -- -D warnings`
Expected: 无 warning（修复所有 warning）

- [ ] **Step 2: README 更新**

更新 [readme.md](../readme.md) 的 v1 验收标准状态为已完成（实际发布时）。

- [ ] **Step 3: 最终 commit**

```bash
git add -A
git commit -m "chore: v1 acceptance complete"
git tag v0.1.0
```

---

## 自检清单

**Spec coverage**：对照 ADR-0001 到 ADR-0007：
- ADR-0001 产品定位：✅ 单用户私人助理，CLI 优先
- ADR-0002 Agent 架构：✅ v1 主 Agent 单干，Phase 7 实现
- ADR-0003 持久化：✅ Phase 5 三份 md + sqlite
- ADR-0004 会话模型：✅ Phase 5 sqlite + Phase 6 压缩
- ADR-0005 Provider：✅ Phase 2 OpenAiCompatible + Phase 3 标签降级
- ADR-0006 工具集 + CLI：✅ Phase 4 工具 + Phase 8 CLI
- ADR-0007 项目结构：✅ Phase 0 文件结构

**Placeholder scan**：无 TBD/TODO/"implement later"。所有代码骨架完整。

**Type consistency**：
- `Provider` trait 在 Phase 2 定义，Phase 6/7 使用一致
- `ToolCall`/`ChatMessage`/`ToolSpec` 在 Phase 2 定义，全项目复用
- `Context::compact` 签名在 Task 6.2 定义，Task 8.2 调用一致
- `ToolRegistry::get` 返回 `Option<&Arc<dyn Tool>>`，Task 8.2 slash.rs 用 `agent.tools.get("memory_write")` 调用一致

---

## 执行顺序

严格按 Phase 0 → 9 顺序执行，每个 Task 完成后：
1. `cargo build` 通过
2. `cargo test` 相关测试通过
3. `cargo clippy` 无 warning
4. git commit

遇到编译错误立即修，不要积累。每个 Task 的 commit 是检查点。
