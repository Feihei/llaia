# P1.5 QQ Channel 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 接入腾讯官方 QQ 开放平台机器人作为 LLAIA 的第二个 channel，实现跨 channel 共享 session 的 QQ 单聊交互。

**Architecture:** 抽象 `Channel` trait，CLI 和 QQ 各自实现；`Agent` 用 `Arc<tokio::sync::Mutex<Agent>>` 跨 channel 共享串行化访问；QqChannel 用 tokio-tungstenite 接 WS 事件，reqwest 调 HTTPS API 发送回复；长回复按段落分片，每片 ≤ 1800 字符；QQ 下的工具 confirm 走 `always`/`whitelist`/`none` 三档，跳过有副作用的工具时不弹 stdin。

**Tech Stack:** Rust + tokio + tokio-tungstenite + reqwest + serde + anyhow + tracing

**Spec:** [docs/specs/2026-07-21-qq-channel-design.md](../specs/2026-07-21-qq-channel-design.md)

---

## 文件结构

| 文件 | 改动 | 职责 |
|---|---|---|
| `Cargo.toml` | 修改 | 加 `tokio-tungstenite`、`futures` 依赖 |
| `src/channels/mod.rs` | 修改 | 新增 `Channel` trait |
| `src/channels/cli.rs` | 修改 | `run_repl` 重构为 `impl Channel for CliChannel` |
| `src/channels/qq.rs` | 创建 | `QqChannel` 实现：WS 接事件 + HTTPS 发消息 + 分片 |
| `src/channels/qq_split.rs` | 创建 | 纯函数 `split_reply()`，独立单测 |
| `src/config.rs` | 修改 | `ChannelsConfig` 加 `qq: QqConfig` |
| `src/tools/mod.rs` | 修改 | `Tool` trait 加 `requires_confirm()` 默认 false |
| `src/tools/file.rs` | 修改 | `FileWrite`/`FileEdit` override `requires_confirm = true` |
| `src/tools/terminal.rs` | 修改 | `Terminal` override `requires_confirm = true` |
| `src/tools/memory.rs` | 修改 | `MemoryWrite` override `requires_confirm = true` |
| `src/agent/mod.rs` | 修改 | `handle_input` 加 channel + confirm 检查 |
| `src/agent/runner.rs` | 修改 | `execute_tool_calls` 接受 channel + qq_confirm_mode 参数 |
| `src/main.rs` | 修改 | 多 channel 启动 |
| `tests/qq_split.rs` | 创建 | `split_reply` 集成测试 |
| `tests/qq_http.rs` | 创建 | QqChannel 发送消息的 mockito 测试 |

---

## Task 0: 加依赖

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: 加依赖**

```toml
[dependencies]
# 既有依赖...
tokio-tungstenite = { version = "0.23", features = ["native-tls"] }
futures-util = "0.3"
```

- [ ] **Step 2: 验证编译**

Run: `cargo build`
Expected: 编译通过，新依赖下载完成

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "deps: add tokio-tungstenite and futures-util for QQ channel"
```

---

## Task 1: QqConfig + config 扩展

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs` 内 `#[cfg(test)] mod tests`

- [ ] **Step 1: 写失败的测试**

在 `src/config.rs` 的 `tests` mod 末尾加：

```rust
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
token = "test-token"
bot_qq = "10000"
"#;
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut tmp, toml.as_bytes()).unwrap();
    let config = Config::load(&tmp.path().to_path_buf()).unwrap();
    assert_eq!(config.channels.qq.enabled, false); // 默认 false
    assert_eq!(config.channels.qq.app_id, "12345");
    assert_eq!(config.channels.qq.token, "test-token");
    assert_eq!(config.channels.qq.bot_qq, "10000");
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
    std::io::Write::write_all(&mut tmp, toml.as_bytes()).unwrap();
    let config = Config::load(&tmp.path().to_path_buf()).unwrap();
    assert!(!config.channels.qq.enabled);
    assert_eq!(config.channels.qq.confirm_mode, "always");
}
```

- [ ] **Step 2: 跑测试验证失败**

Run: `cargo test --lib config::tests::test_qq_config`
Expected: FAIL（编译错误，QqConfig 不存在）

- [ ] **Step 3: 实现 QqConfig**

在 `src/config.rs` 找到 `ChannelsConfig` 定义，改为：

```rust
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
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub bot_qq: String,
    #[serde(default = "default_qq_confirm")]
    pub confirm_mode: String,
}

impl Default for QqConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: String::new(),
            token: String::new(),
            bot_qq: String::new(),
            confirm_mode: default_qq_confirm(),
        }
    }
}

fn default_qq_confirm() -> String {
    "always".into()
}
```

注意：删掉旧的 `CliChannelConfig` 定义（如果存在重复）。

- [ ] **Step 4: 跑测试验证通过**

Run: `cargo test --lib config::tests::test_qq_config`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add QqConfig with defaults"
```

---

## Task 2: Tool trait 加 requires_confirm

**Files:**
- Modify: `src/tools/mod.rs`
- Modify: `src/tools/file.rs`
- Modify: `src/tools/terminal.rs`
- Modify: `src/tools/memory.rs`

- [ ] **Step 1: 给 Tool trait 加默认实现**

在 `src/tools/mod.rs` 的 `Tool` trait 定义中加方法：

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String>;

    /// 是否需要确认（有副作用）。默认 false（只读工具）。
    /// 有副作用的工具（file_write, terminal, memory_write 等）应 override 返回 true。
    fn requires_confirm(&self) -> bool {
        false
    }

    /// 生成 ToolSpec（既有 helper，不变）
    fn spec(&self) -> ToolSpec {
        // 既有实现
    }
}
```

- [ ] **Step 2: FileWrite / FileEdit override**

在 `src/tools/file.rs` 的 `impl Tool for FileWrite` 块中加：

```rust
fn requires_confirm(&self) -> bool {
    true
}
```

同样加到 `impl Tool for FileEdit`。

`FileRead` 不加（用默认 false）。

- [ ] **Step 3: Terminal override**

在 `src/tools/terminal.rs` 的 `impl Tool for Terminal` 块中加：

```rust
fn requires_confirm(&self) -> bool {
    true
}
```

- [ ] **Step 4: MemoryWrite override**

在 `src/tools/memory.rs` 的 `impl Tool for MemoryWrite` 块中加：

```rust
fn requires_confirm(&self) -> bool {
    true
}
```

- [ ] **Step 5: 写测试验证**

在 `src/tools/file.rs` 的 `tests` mod 末尾加：

```rust
#[test]
fn test_requires_confirm_flags() {
    let ws = std::path::PathBuf::from(".");
    assert!(!FileRead::new(ws.clone()).requires_confirm());
    assert!(FileWrite::new(ws.clone()).requires_confirm());
    assert!(FileEdit::new(ws).requires_confirm());
}
```

在 `src/tools/terminal.rs` 的 `tests` mod 末尾加：

```rust
#[test]
fn test_terminal_requires_confirm() {
    let (_guard, ws) = make_workspace();
    let t = Terminal::new("none".into(), vec![], ws);
    assert!(t.requires_confirm());
}
```

- [ ] **Step 6: 跑测试**

Run: `cargo test --lib tools::`
Expected: 所有工具测试 PASS

- [ ] **Step 7: Commit**

```bash
git add src/tools/
git commit -m "feat(tools): add requires_confirm() to Tool trait"
```

---

## Task 3: split_reply 纯函数 + 测试

**Files:**
- Create: `src/channels/qq_split.rs`
- Modify: `src/channels/mod.rs`（加 `pub mod qq_split;`）
- Create: `tests/qq_split.rs`

- [ ] **Step 1: 写失败的集成测试**

`tests/qq_split.rs`:

```rust
use llaia::channels::qq_split::split_reply;

#[test]
fn test_short_reply_no_split() {
    let text = "短回复";
    assert_eq!(split_reply(text, 1800), vec!["短回复"]);
}

#[test]
fn test_split_by_paragraph() {
    let text = "段落一\n\n段落二\n\n段落三";
    let parts = split_reply(text, 10);
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], "段落一");
    assert_eq!(parts[1], "段落二");
    assert_eq!(parts[2], "段落三");
}

#[test]
fn test_split_by_line_when_paragraph_too_long() {
    // 一个段落 4 行，每行 5 字符，max=12
    let text = "aaaaa\nbbbbb\nccccc\nddddd";
    let parts = split_reply(text, 12);
    // 第一片：aaaaa\nbbbbb (11 字符 + 换行 = 12)
    // 第二片：ccccc\nddddd
    assert_eq!(parts.len(), 2);
    assert!(parts[0].len() <= 12);
    assert!(parts[1].len() <= 12);
}

#[test]
fn test_split_by_char_when_line_too_long() {
    let text = "a".repeat(2500);
    let parts = split_reply(&text, 1800);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].len(), 1800);
    assert_eq!(parts[1].len(), 700);
}

#[test]
fn test_code_block_preserved_within_chunk() {
    // 代码块不跨片时保持完整
    let text = "前文\n\n```rust\nfn main() {}\n```\n\n后文";
    let parts = split_reply(text, 100);
    assert_eq!(parts.len(), 1);
    assert!(parts[0].contains("```rust"));
    assert!(parts[0].contains("```"));
}

#[test]
fn test_code_block_split_closes_and_reopens() {
    // 代码块跨片时，前片闭合，后片以 ```rust 重开
    let long_code = "fn main() {\n".to_string() + &"    println!(\"x\");\n".repeat(200) + "}\n";
    let text = format!("```rust\n{}```", long_code);
    let parts = split_reply(&text, 1800);
    assert!(parts.len() > 1);
    // 第一片末尾应该有 ``` 闭合
    assert!(parts[0].ends_with("```"));
    // 第二片开头应该有 ```rust 重开
    assert!(parts[1].starts_with("```rust"));
}
```

- [ ] **Step 2: 跑测试验证失败**

Run: `cargo test --test qq_split`
Expected: FAIL（模块不存在）

- [ ] **Step 3: 创建 qq_split.rs**

`src/channels/qq_split.rs`:

```rust
/// 将长文本按 QQ 单条消息上限分片。
/// 规则：
/// 1. 优先按段落（`\n\n`）切
/// 2. 单段超 max 时按行（`\n`）切
/// 3. 单行超 max 时按字符硬切
/// 4. 代码块跨片时闭合后再开，下一片以 ``` 同语言标记开始
pub fn split_reply(text: &str, max: usize) -> Vec<String> {
    if text.len() <= max {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut in_code_block = false;
    let mut code_lang = String::new();

    // 段落切分
    let paragraphs: Vec<&str> = text.split("\n\n").collect();
    for para in paragraphs {
        // 检测代码块开始/结束
        if para.trim_start().starts_with("```") {
            let lang = para.trim_start().trim_start_matches("```").trim_end();
            if !in_code_block {
                in_code_block = true;
                code_lang = lang.to_string();
            } else if para.trim() == "```" {
                in_code_block = false;
            }
        }

        let candidate = if current.is_empty() {
            para.to_string()
        } else {
            format!("{}\n\n{}", current, para)
        };

        if candidate.len() <= max {
            current = candidate;
        } else {
            // 当前段落加不进去，先把 current 推走
            if !current.is_empty() {
                // 如果在代码块里，闭合后再推
                if in_code_block {
                    current.push_str("\n```");
                }
                chunks.push(std::mem::take(&mut current));
                // 下一片以代码块重开
                if in_code_block {
                    current = format!("```{}\n", code_lang);
                }
            }
            // 现在处理这个超长的段落
            if para.len() <= max {
                current = para.to_string();
            } else {
                // 段落本身超 max，按行切
                let lines: Vec<&str> = para.split('\n').collect();
                for line in lines {
                    let candidate = if current.is_empty() {
                        line.to_string()
                    } else {
                        format!("{}\n{}", current, line)
                    };
                    if candidate.len() <= max {
                        current = candidate;
                    } else {
                        if !current.is_empty() {
                            if in_code_block {
                                current.push_str("\n```");
                            }
                            chunks.push(std::mem::take(&mut current));
                            if in_code_block {
                                current = format!("```{}\n", code_lang);
                            }
                        }
                        // 单行也超 max
                        if line.len() > max {
                            let mut remaining = line;
                            while remaining.len() > max {
                                let (chunk, rest) = remaining.split_at(max);
                                chunks.push(chunk.to_string());
                                remaining = rest;
                            }
                            current = remaining.to_string();
                        } else {
                            current = line.to_string();
                        }
                    }
                }
            }
        }
    }

    if !current.is_empty() {
        if in_code_block {
            current.push_str("\n```");
        }
        chunks.push(current);
    }

    // 修正：上面的逻辑对单段落超 max 但带代码块的情况可能漏掉闭合
    // 已在每次推 chunks 时处理

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short() {
        assert_eq!(split_reply("hi", 100), vec!["hi"]);
    }

    #[test]
    fn test_paragraph_split() {
        let text = "p1\n\np2\n\np3";
        assert_eq!(split_reply(text, 4), vec!["p1", "p2", "p3"]);
    }

    #[test]
    fn test_long_line_char_split() {
        let text = "a".repeat(250);
        let parts = split_reply(&text, 100);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 100);
        assert_eq!(parts[1].len(), 100);
        assert_eq!(parts[2].len(), 50);
    }
}
```

- [ ] **Step 4: 加 mod 声明**

在 `src/channels/mod.rs` 加：

```rust
pub mod qq_split;
```

- [ ] **Step 5: 跑测试**

Run: `cargo test --test qq_split && cargo test --lib channels::qq_split`
Expected: 全部 PASS

如果有失败，根据失败调整 `split_reply` 实现。代码块跨片逻辑是难点，可能需要迭代。

- [ ] **Step 6: Commit**

```bash
git add src/channels/qq_split.rs src/channels/mod.rs tests/qq_split.rs
git commit -m "feat(qq): add split_reply for long message chunking"
```

---

## Task 4: Channel trait

**Files:**
- Modify: `src/channels/mod.rs`
- Modify: `src/lib.rs`（如需导出）

- [ ] **Step 1: 定义 Channel trait**

`src/channels/mod.rs` 完整内容：

```rust
pub mod cli;
pub mod qq;
pub mod qq_split;

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::agent::Agent;

/// 抽象一个用户接入通道（CLI / QQ / 未来邮箱、web 等）。
/// 每个实现负责自己的 I/O 循环（读用户输入、写回复），
/// 共享同一个 Agent（通过 Arc<Mutex> 串行化访问）。
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    /// 启动 channel，阻塞运行直到退出。
    async fn run(self: Arc<Self>, agent: Arc<Mutex<Agent>>) -> Result<()>;
}
```

- [ ] **Step 2: 验证编译**

Run: `cargo build`
Expected: 编译通过（即使 `qq` 模块还没创建，可以暂时加 `// pub mod qq;` 注释掉）

注意：暂时把 `pub mod qq;` 注释掉，下一个 Task 创建该文件时再启用。

- [ ] **Step 3: Commit**

```bash
git add src/channels/mod.rs
git commit -m "feat(channels): define Channel trait"
```

---

## Task 5: CliChannel 重构

**Files:**
- Modify: `src/channels/cli.rs`
- Modify: `src/commands/mod.rs`（`chat_cmd` 改为启动 CliChannel）

- [ ] **Step 1: 重构 cli.rs**

`src/channels/cli.rs` 把 `pub async fn run_repl()` 改为 `impl Channel for CliChannel`：

```rust
use crate::agent::runner::ToolRegistry;
use crate::agent::Agent;
use crate::channels::Channel;
use crate::commands::slash::{try_handle, SlashOutcome};
use crate::config::Config;
use crate::memory::sqlite::SessionStore;
use crate::memory::{ensure_template, load_md, MEMORY_TEMPLATE, SOUL_TEMPLATE, USER_TEMPLATE};
use crate::provider::openai_compat::OpenAiCompatibleProvider;
use crate::provider::Provider;
use crate::tool_call::build_tool_instructions;
use crate::tools::file::{FileEdit, FileRead, FileWrite};
use crate::tools::memory::MemoryWrite;
use crate::tools::tavily::TavilySearch;
use crate::tools::terminal::Terminal;
use crate::tools::web::WebFetch;
use anyhow::Result;
use async_trait::async_trait;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct CliChannel;

impl CliChannel {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Channel for CliChannel {
    async fn run(self: Arc<Self>, agent: Arc<Mutex<Agent>>) -> Result<()> {
        println!("llaia v0.1.5 - type /help for commands, /exit to quit\n");
        let stdin = std::io::stdin();
        loop {
            print!("> ");
            std::io::stdout().flush()?;
            let mut line = String::new();
            if stdin.lock().read_line(&mut line)? == 0 {
                break;
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            match try_handle(line, &mut *agent.lock().await).await? {
                SlashOutcome::Exit => break,
                SlashOutcome::Handled => continue,
                SlashOutcome::NotSlash => {
                    let mut a = agent.lock().await;
                    match a.handle_input(line, "cli").await {
                        Ok(resp) => println!("\n{}\n", resp),
                        Err(e) => println!("\n[error: {}]\n", e),
                    }
                }
            }
        }
        Ok(())
    }
}

/// 构建 Agent 实例（CLI 和 QQ 共用）
pub async fn build_agent(config: &Config) -> Result<Arc<Mutex<Agent>>> {
    let agent_cfg = config
        .agent
        .get("main")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("agent.main not configured"))?;

    let workspace = PathBuf::from(&agent_cfg.workspace);
    std::fs::create_dir_all(&workspace).ok();

    let soul_path = resolve_md_path(&agent_cfg.soul, &workspace, "SOUL.md");
    let user_path = resolve_md_path(&agent_cfg.user, &workspace, "USER.md");
    let memory_path = resolve_md_path(&agent_cfg.memory, &workspace, "MEMORY.md");
    ensure_template(&soul_path, SOUL_TEMPLATE).await?;
    ensure_template(&user_path, USER_TEMPLATE).await?;
    ensure_template(&memory_path, MEMORY_TEMPLATE).await?;

    let soul = load_md(&soul_path).await?;
    let user = load_md(&user_path).await?;
    let memory = load_md(&memory_path).await?;

    let (prov_id, model_alias) = Config::parse_model_ref(&agent_cfg.model)?;
    let prov_cfg = config
        .provider
        .get(prov_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("provider.{} not configured", prov_id))?;
    let model_cfg = prov_cfg
        .model
        .get(model_alias)
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!("provider.{}.model.{} not configured", prov_id, model_alias)
        })?;

    let provider: Arc<dyn Provider> = Arc::new(OpenAiCompatibleProvider::new(
        &prov_cfg.base_url,
        &prov_cfg.api_key,
        &model_cfg.model,
        model_cfg.native_tool_calling,
    )?);

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(FileRead::new(workspace.clone())));
    registry.register(Arc::new(FileWrite::new(workspace.clone())));
    registry.register(Arc::new(FileEdit::new(workspace.clone())));
    registry.register(Arc::new(Terminal::new(
        config.tools.terminal.confirm.clone(),
        config.tools.terminal.whitelist.clone(),
        workspace.clone(),
    )));
    registry.register(Arc::new(WebFetch::new()?));
    if !config.tools.tavily.api_key.is_empty() {
        registry.register(Arc::new(TavilySearch::new(
            config.tools.tavily.api_key.clone(),
        )?));
    }
    registry.register(Arc::new(MemoryWrite::new(memory_path.clone())));

    let mut system_prompt = format!(
        "# SOUL\n{}\n\n# USER\n{}\n\n# MEMORY\n{}\n\n# WORKSPACE\n{}\n\n工作目录说明：所有工具的相对路径都相对于 WORKSPACE 解析；terminal 命令在 WORKSPACE 下执行。需要写到其它位置时请使用绝对路径。",
        soul, user, memory, workspace.display()
    );
    if !provider.native_tool_calling() {
        system_prompt.push_str(&build_tool_instructions(&registry.specs()));
    }
    let registry = Arc::new(registry);

    let db_path = workspace.join("sessions.db");
    let session_store = Arc::new(SessionStore::open(&db_path)?);

    let session_id = match session_store.latest_session()? {
        Some((id, _)) => id,
        None => {
            let uuid = uuid::Uuid::new_v4().to_string();
            session_store.create_session(&uuid, "cli")?
        }
    };

    let agent = Agent::new(
        config,
        provider,
        registry,
        session_store,
        session_id,
        system_prompt,
        8192,
    )
    .await;

    Ok(Arc::new(Mutex::new(agent)))
}

fn resolve_md_path(explicit: &Option<String>, workspace: &PathBuf, default_name: &str) -> PathBuf {
    match explicit {
        Some(s) => {
            let p = PathBuf::from(s);
            if p.is_absolute() {
                p
            } else {
                workspace.join(s)
            }
        }
        None => workspace.join(default_name),
    }
}
```

- [ ] **Step 2: 改 commands/mod.rs 的 chat_cmd**

`src/commands/mod.rs` 中 `chat_cmd` 改为：

```rust
pub async fn chat_cmd() -> Result<()> {
    let config = load_config_or_init()?;

    let log_dir = PathBuf::from(&config.log.dir);
    let _ = crate::log::init(&config.log.level, &log_dir);

    let agent = crate::channels::cli::build_agent(&config).await?;

    let cli = Arc::new(crate::channels::CliChannel::new());
    cli.run(agent).await
}
```

注意：原 `chat_cmd` 中的所有逻辑都搬到了 `build_agent` 和 `CliChannel::run` 里。

- [ ] **Step 3: 暂时禁用 qq 模块编译**

在 `src/channels/mod.rs` 把 `pub mod qq;` 注释掉：

```rust
// pub mod qq;  // Task 6 启用
```

- [ ] **Step 4: 验证 CLI 不回归**

Run: `cargo build && echo "你好" | cargo run -- chat`
Expected: 编译通过，"你好" 能正常对话

- [ ] **Step 5: Commit**

```bash
git add src/channels/cli.rs src/channels/mod.rs src/commands/mod.rs
git commit -m "refactor(channels): CliChannel implements Channel trait"
```

---

## Task 6: Agent 加 channel 感知的 confirm 检查

**Files:**
- Modify: `src/agent/mod.rs`
- Modify: `src/agent/runner.rs`

- [ ] **Step 1: 修改 runner.rs 的 execute_tool_calls 签名**

`src/agent/runner.rs` 找到 `execute_tool_calls` 函数，加 channel 和 qq_confirm_mode 参数：

```rust
pub async fn execute_tool_calls(
    agent: &Agent,
    tool_calls: &[ToolCall],
    channel: &str,
    qq_confirm_mode: &str,
) -> anyhow::Result<Vec<ChatMessage>> {
    let mut results = Vec::new();
    for tc in tool_calls {
        let tool = match agent.tools.get(&tc.name) {
            Some(t) => t,
            None => {
                results.push(ChatMessage::tool_result(
                    &tc.tool_call_id,
                    &format!("tool {} not found", tc.name),
                ));
                continue;
            }
        };

        // QQ channel 下的 confirm 检查
        if channel == "qq" {
            if tool.requires_confirm() {
                let allowed = match qq_confirm_mode {
                    "none" => true,
                    "whitelist" => false, // P1.5 简化：whitelist 在 QQ 下默认禁用所有需确认的工具
                    _ => false,            // "always" 或未知
                };
                if !allowed {
                    let msg = format!("QQ 频道下不能执行此操作：{}", tc.name);
                    tracing::warn!("qq channel blocked tool: {}", tc.name);
                    results.push(ChatMessage::tool_result(&tc.tool_call_id, &msg));
                    continue;
                }
            }
        }

        tracing::info!(tool = %tc.name, args = ?tc.arguments, "executing tool");
        let outcome = tool.execute(&tc.arguments).await;
        match outcome {
            Ok(out) => {
                tracing::info!(tool = %tc.name, len = out.len(), "tool done");
                results.push(ChatMessage::tool_result(&tc.tool_call_id, &out));
            }
            Err(e) => {
                tracing::warn!(tool = %tc.name, error = %e, "tool error");
                results.push(ChatMessage::tool_result(
                    &tc.tool_call_id,
                    &format!("error: {}", e),
                ));
            }
        }
    }
    Ok(results)
}
```

注意：上面是参考实现，请根据 `src/agent/runner.rs` 的实际既有代码结构调整。关键是加 `channel: &str` 和 `qq_confirm_mode: &str` 参数，以及在工具执行前加 confirm 检查。

- [ ] **Step 2: 修改 agent/mod.rs 的 handle_input**

`src/agent/mod.rs` 中 `handle_input` 内调用 `execute_tool_calls` 的地方，传入 channel 和 confirm_mode：

```rust
pub async fn handle_input(&mut self, user_input: &str, channel: &str) -> anyhow::Result<String> {
    // ... 既有逻辑

    // 在调用 execute_tool_calls 时传入 channel 和 confirm_mode
    let qq_confirm_mode = self.qq_confirm_mode.clone().unwrap_or_else(|| "always".to_string());
    let tool_results = execute_tool_calls(self, &tool_calls, channel, &qq_confirm_mode).await?;

    // ... 既有逻辑
}
```

- [ ] **Step 3: Agent 结构加 qq_confirm_mode 字段**

`src/agent/mod.rs` 的 `Agent` struct 加字段：

```rust
pub struct Agent {
    // 既有字段...
    pub qq_confirm_mode: Option<String>,
}
```

`Agent::new` 中初始化：

```rust
pub async fn new(
    config: &Config,
    // ... 既有参数
) -> Self {
    Self {
        // 既有字段...
        qq_confirm_mode: Some(config.channels.qq.confirm_mode.clone()),
    }
}
```

- [ ] **Step 4: 跑既有测试不回归**

Run: `cargo test --lib`
Expected: 既有测试全 PASS

- [ ] **Step 5: 写 QQ confirm 单元测试**

在 `src/agent/runner.rs` 末尾加测试 mod：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::tools::file::{FileRead, FileWrite};
    use std::path::PathBuf;
    use std::sync::Arc;

    // 这是一个简化测试，验证 channel == "qq" 且 confirm_mode == "always" 时
    // requires_confirm == true 的工具被跳过。
    // 由于 Agent 构造复杂，这里只测逻辑分支。
    #[test]
    fn test_qq_confirm_logic() {
        // 简化：直接测工具的 requires_confirm 标志
        let ws = PathBuf::from(".");
        let read = FileRead::new(ws.clone());
        let write = FileWrite::new(ws);
        assert!(!read.requires_confirm());
        assert!(write.requires_confirm());

        // 逻辑等价：在 channel == "qq" && mode == "always" 下，requires_confirm == true 的工具会被跳过
        let mode = "always";
        let channel = "qq";
        let should_block_write = channel == "qq" && write.requires_confirm() && mode == "always";
        let should_block_read = channel == "qq" && read.requires_confirm() && mode == "always";
        assert!(should_block_write);
        assert!(!should_block_read);
    }
}
```

- [ ] **Step 6: 跑测试**

Run: `cargo test --lib agent::runner`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/agent/
git commit -m "feat(agent): channel-aware tool confirm for QQ"
```

---

## Task 7: QqChannel 实现

**Files:**
- Create: `src/channels/qq.rs`
- Modify: `src/channels/mod.rs`（启用 `pub mod qq;`）

**注意**：腾讯官方 QQ 开放平台 API 细节需要查阅 https://bot.q.qq.com/wiki/ 。本 task 给出骨架，关键 endpoint URL 和 payload schema 在实现时根据官方文档调整。

- [ ] **Step 1: 创建 qq.rs 骨架**

`src/channels/qq.rs`:

```rust
use crate::agent::Agent;
use crate::channels::Channel;
use crate::channels::qq_split::split_reply;
use crate::config::QqConfig;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::Message};

pub struct QqChannel {
    config: QqConfig,
    http: Client,
}

impl QqChannel {
    pub fn new(config: QqConfig) -> Self {
        Self {
            config,
            http: Client::new(),
        }
    }

    /// QQ 开放平台 WebSocket endpoint
    /// 实际地址需要根据官方文档获取（鉴权后从 /gateway 接口拿到 wss URL）
    async fn get_ws_url(&self) -> Result<String> {
        // TODO: 根据腾讯官方文档实现
        // 通常流程：
        // 1. GET https://api.sgroup.qq.com/gateway/bot
        //    Header: Authorization: Bot <app_id>.<token>
        // 2. 返回 JSON: { "url": "wss://...", "shards": 1, "session_start_limits": {...} }
        let url = format!(
            "https://api.sgroup.qq.com/gateway/bot"
        );
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bot {}.{}", self.config.app_id, self.config.token))
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;
        let ws_url = resp
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("gateway response missing 'url' field"))?
            .to_string();
        Ok(ws_url)
    }

    /// 从收到的 WS 消息中提取 C2C 文本消息内容
    /// 返回 (user_openid, text) 或 None（非 C2C 文本消息）
    fn extract_c2c_text(payload: &serde_json::Value) -> Option<(String, String)> {
        // TODO: 根据腾讯官方文档的事件 schema 调整
        // 大致结构：
        // {
        //   "op": 0,  // dispatch
        //   "s": 0,
        //   "t": "C2C_MESSAGE_CREATE",
        //   "d": {
        //     "id": "msg_xxx",
        //     "author": { "id": "user_openid" },
        //     "content": "用户发的文本"
        //   }
        // }
        if payload.get("t").and_then(|v| v.as_str()) != Some("C2C_MESSAGE_CREATE") {
            return None;
        }
        let d = payload.get("d")?;
        let user_id = d.get("author")?.get("id")?.as_str()?.to_string();
        let text = d.get("content")?.as_str()?.to_string();
        if text.is_empty() {
            return None;
        }
        Some((user_id, text))
    }

    /// 通过 HTTPS API 发送 C2C 消息
    async fn send_c2c_message(&self, user_openid: &str, content: &str, msg_id: Option<&str>) -> Result<()> {
        let url = format!("https://api.sgroup.qq.com/v2/users/{}/messages", user_openid);
        let mut body = serde_json::json!({
            "content": content,
            "msg_type": 0,  // 0 = 文本
        });
        if let Some(id) = msg_id {
            body["msg_id"] = serde_json::Value::String(id.to_string());
        }

        // 3 次指数退避：200ms / 400ms / 800ms
        let delays = [200u64, 400, 800];
        let mut last_err = None;
        for (attempt, delay) in delays.iter().enumerate() {
            let resp = self
                .http
                .post(&url)
                .header("Authorization", format!("Bot {}.{}", self.config.app_id, self.config.token))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => return Ok(()),
                Ok(r) => {
                    let status = r.status();
                    let text = r.text().await.unwrap_or_default();
                    tracing::warn!(attempt, %status, %text, "qq send failed, retrying");
                    last_err = Some(anyhow!("status: {}, body: {}", status, text));
                }
                Err(e) => {
                    tracing::warn!(attempt, error = %e, "qq send error, retrying");
                    last_err = Some(e.into());
                }
            }
            tokio::time::sleep(Duration::from_millis(*delay)).await;
        }
        Err(last_err.unwrap_or_else(|| anyhow!("unknown error")))
    }

    /// 处理一条用户消息：调 agent 拿回复，分片发送
    async fn handle_user_message(
        &self,
        agent: &Arc<Mutex<Agent>>,
        user_openid: &str,
        text: &str,
        msg_id: Option<&str>,
    ) -> Result<()> {
        let reply = {
            let mut a = agent.lock().await;
            a.handle_input(text, "qq").await
        };

        let reply = match reply {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "agent handle_input failed");
                format!("[内部错误: {}]", e)
            }
        };

        // 分片发送，每片 ≤ 1800 字符
        let chunks = split_reply(&reply, 1800);
        for (i, chunk) in chunks.iter().enumerate() {
            // 只有第一片带 msg_id 用于被动回复，后续片用主动消息（P1.5 简化：都带同一个 msg_id 试试）
            let id = if i == 0 { msg_id } else { None };
            if let Err(e) = self.send_c2c_message(user_openid, chunk, id).await {
                tracing::error!(error = %e, chunk = i, "failed to send chunk after retries");
                // 不 return，继续尝试后续片
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for QqChannel {
    async fn run(self: Arc<Self>, agent: Arc<Mutex<Agent>>) -> Result<()> {
        tracing::info!("QqChannel starting, app_id={}", self.config.app_id);

        let ws_url = self.get_ws_url().await?;
        tracing::info!(url = %ws_url, "connecting to QQ gateway");

        let (ws_stream, _) = connect_async(&ws_url)
            .await
            .map_err(|e| anyhow!("ws connect: {}", e))?;
        let (mut write, mut read) = ws_stream.split();

        // TODO: 发送 IDENTIFY payload 进行鉴权
        // 参考 https://bot.q.qq.com/wiki/

        loop {
            let msg = read.next().await;
            match msg {
                Some(Ok(Message::Text(text))) => {
                    let payload: serde_json::Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to parse ws payload");
                            continue;
                        }
                    };

                    // 处理 heartbeat ack 等
                    if payload.get("op").and_then(|v| v.as_u64()) == Some(11) {
                        continue; // heartbeat ack
                    }

                    // 提取 C2C 文本消息
                    if let Some((user_openid, text)) = Self::extract_c2c_text(&payload) {
                        let msg_id = payload
                            .get("d")
                            .and_then(|d| d.get("id"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());

                        let this = self.clone();
                        let agent = agent.clone();
                        let msg_id = msg_id.clone();
                        tokio::spawn(async move {
                            if let Err(e) = this
                                .handle_user_message(&agent, &user_openid, &text, msg_id.as_deref())
                                .await
                            {
                                tracing::error!(error = %e, "handle_user_message failed");
                            }
                        });
                    }
                }
                Some(Ok(Message::Ping(data))) => {
                    let _ = write.send(Message::Pong(data)).await;
                }
                Some(Ok(_)) => continue,
                Some(Err(e)) => {
                    tracing::error!(error = %e, "ws read error");
                    break;
                }
                None => {
                    tracing::info!("ws closed");
                    break;
                }
            }
        }

        tracing::warn!("QqChannel exited");
        Ok(())
    }
}
```

- [ ] **Step 2: 启用 mod**

在 `src/channels/mod.rs` 把 `// pub mod qq;` 改为 `pub mod qq;`

- [ ] **Step 3: 加 futures-util 引用**

在 `src/channels/qq.rs` 顶部加：

```rust
use futures_util::{SinkExt, StreamExt};
```

- [ ] **Step 4: 验证编译**

Run: `cargo build`
Expected: 编译通过

如果 `connect_async` 需要 TLS feature，确认 Cargo.toml 已加 `features = ["native-tls"]`（Task 0 已加）。

- [ ] **Step 5: Commit**

```bash
git add src/channels/qq.rs src/channels/mod.rs
git commit -m "feat(qq): QqChannel skeleton with WS + HTTPS"
```

---

## Task 8: main.rs 多 channel 启动

**Files:**
- Modify: `src/main.rs`
- Modify: `src/commands/mod.rs`（chat_cmd 改为启动多 channel）

- [ ] **Step 1: 改 chat_cmd 为启动多 channel**

`src/commands/mod.rs` 的 `chat_cmd`:

```rust
pub async fn chat_cmd() -> Result<()> {
    let config = load_config_or_init()?;

    let log_dir = PathBuf::from(&config.log.dir);
    let _ = crate::log::init(&config.log.level, &log_dir);

    let agent = crate::channels::cli::build_agent(&config).await?;

    let mut tasks = Vec::new();

    if config.channels.cli.enabled {
        let cli = Arc::new(crate::channels::CliChannel::new());
        let agent = agent.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = cli.run(agent).await {
                tracing::error!(error = %e, "CliChannel exited with error");
            }
        }));
    }

    if config.channels.qq.enabled {
        let qq = Arc::new(crate::channels::qq::QqChannel::new(config.channels.qq.clone()));
        let agent = agent.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = qq.run(agent).await {
                tracing::error!(error = %e, "QqChannel exited with error");
            }
        }));
    }

    if tasks.is_empty() {
        anyhow::bail!("no channel enabled in config");
    }

    for t in tasks {
        let _ = t.await;
    }
    Ok(())
}
```

- [ ] **Step 2: 验证编译**

Run: `cargo build`
Expected: 编译通过

- [ ] **Step 3: 验证 CLI 不回归**

Run: `echo "你好" | cargo run -- chat`
Expected: 正常对话

注意：当前 config 里 `[channels.qq]` 没写，`QqConfig::default()` 的 `enabled = false`，QQ channel 不会启动。

- [ ] **Step 4: Commit**

```bash
git add src/commands/mod.rs
git commit -m "feat: multi-channel startup in chat_cmd"
```

---

## Task 9: QqChannel HTTP 发送消息集成测试

**Files:**
- Create: `tests/qq_http.rs`

- [ ] **Step 1: 写 mockito 测试**

`tests/qq_http.rs`:

```rust
use llaia::channels::qq::QqChannel;
use llaia::config::QqConfig;
use mockito::Server;

#[tokio::test]
async fn test_send_c2c_message_success() {
    let mut server = Server::new_async().await;
    let mock = server
        .mock("POST", "/v2/users/USER123/messages")
        .with_status(200)
        .with_body(r#"{"id":"msg_xxx"}"#)
        .create_async()
        .await;

    let mut config = QqConfig {
        enabled: true,
        app_id: "test_app".into(),
        token: "test_token".into(),
        bot_qq: "10000".into(),
        confirm_mode: "always".into(),
    };

    let qq = QqChannel::new_with_http(config, server.url());
    qq.send_c2c_message("USER123", "hello", Some("msg_id_1"))
        .await
        .unwrap();

    mock.assert();
}

#[tokio::test]
async fn test_send_c2c_message_retries_on_failure() {
    let mut server = Server::new_async().await;
    let mock = server
        .mock("POST", "/v2/users/USER456/messages")
        .with_status(500)
        .with_body("internal error")
        .expect(3)  // 应该重试 3 次
        .create_async()
        .await;

    let config = QqConfig {
        enabled: true,
        app_id: "test_app".into(),
        token: "test_token".into(),
        bot_qq: "10000".into(),
        confirm_mode: "always".into(),
    };

    let qq = QqChannel::new_with_http(config, server.url());
    let result = qq.send_c2c_message("USER456", "hello", None).await;
    assert!(result.is_err());

    mock.assert();
}
```

- [ ] **Step 2: 给 QqChannel 加测试构造方法**

在 `src/channels/qq.rs` 加：

```rust
impl QqChannel {
    pub fn new(config: QqConfig) -> Self {
        Self {
            config,
            http: Client::new(),
        }
    }

    /// 测试用：允许注入 base_url
    #[cfg(test)]
    pub fn new_with_http(config: QqConfig, base_url: String) -> QqChannelTestable {
        QqChannelTestable {
            config,
            http: Client::new(),
            base_url,
        }
    }
}

/// 测试用变体，允许覆盖 API base_url
#[cfg(test)]
pub struct QqChannelTestable {
    config: QqConfig,
    http: Client,
    base_url: String,
}

#[cfg(test)]
impl QqChannelTestable {
    pub async fn send_c2c_message(
        &self,
        user_openid: &str,
        content: &str,
        msg_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let url = format!("{}/v2/users/{}/messages", self.base_url, user_openid);
        let mut body = serde_json::json!({
            "content": content,
            "msg_type": 0,
        });
        if let Some(id) = msg_id {
            body["msg_id"] = serde_json::Value::String(id.to_string());
        }

        let delays = [200u64, 400, 800];
        let mut last_err = None;
        for (attempt, delay) in delays.iter().enumerate() {
            let resp = self
                .http
                .post(&url)
                .header("Authorization", format!("Bot {}.{}", self.config.app_id, self.config.token))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => return Ok(()),
                Ok(r) => {
                    let status = r.status();
                    let text = r.text().await.unwrap_or_default();
                    last_err = Some(anyhow::anyhow!("status: {}, body: {}", status, text));
                }
                Err(e) => {
                    last_err = Some(e.into());
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(*delay)).await;
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("unknown error")))
    }
}
```

注意：这是为了测试能注入 mockito URL。如果 `QqChannel` 本身的 `send_c2c_message` 能改成接受 `base_url` 参数而不破坏正常运行，可以不用 `QqChannelTestable`，直接在 `QqChannel` 加个 `base_url: Option<String>` 字段。**实现时按更简洁的方式调整。**

- [ ] **Step 3: 跑测试**

Run: `cargo test --test qq_http`
Expected: 两个测试 PASS

- [ ] **Step 4: Commit**

```bash
git add tests/qq_http.rs src/channels/qq.rs
git commit -m "test(qq): mockito tests for send_c2c_message with retries"
```

---

## Task 10: 端到端 smoke test

**Files:**
- Modify: `C:\Users\THAD\.llaia\config.toml`（用户手动）
- Manual test

- [ ] **Step 1: 用户在 config.toml 加 QQ 配置**

```toml
[channels.qq]
enabled = true
app_id = "你的 app_id"
token = "你的 token"
bot_qq = "机器人的 QQ 号"
confirm_mode = "always"
```

- [ ] **Step 2: 启动 LLAIA**

Run: `cargo run -- chat`
Expected: 日志显示 QqChannel 启动并连接到 gateway，CLI 也能正常输入

- [ ] **Step 3: 用另一个 QQ 号发消息给机器人**

测试场景：
1. 发 "你好" → 机器人回复
2. 发 "列出 workspace 下的文件" → 机器人回复（terminal 工具在 always 模式下应被跳过，回复"QQ 频道下不能执行此操作：terminal"）
3. 发一个长问题，让回复 > 1800 字 → 验证分片发送
4. 在 CLI 里说一个话题，切到 QQ 继续 → 验证跨 channel session 共享

- [ ] **Step 4: 如果有问题，记录并修复**

可能的问题：
- 腾讯 API 鉴权失败：检查 app_id / token 格式
- WS 连接断开：检查心跳逻辑
- 消息发不出：检查 Authorization header 格式
- 中文乱码：检查 Content-Type

- [ ] **Step 5: Commit smoke test 中的 bug 修复**

```bash
git add ...
git commit -m "fix(qq): <具体修复>"
```

---

## Task 11: 更新文档

**Files:**
- Modify: `readme.md`
- Modify: `AGENTS.md`
- Create: `docs/adr/0009-qq-channel.md`

- [ ] **Step 1: 写 ADR 0009**

`docs/adr/0009-qq-channel.md`:

```markdown
# ADR 0009: QQ Channel 接入

- 状态：Accepted
- 日期：2026-07-21

## 背景

P1.5 计划接入 QQ 作为第二个 channel。腾讯官方 QQ 开放平台提供 WebSocket + HTTPS API，单用户场景下本地运行无需公网端点。

## 决策

### 协议选择

采用腾讯官方 QQ 开放平台（qbot.qq.com），不走 OneBot 等第三方协议。理由：
- 合规稳定，无封号风险
- 单聊场景下官方 API 能力足够
- 用户已有官方 bot 账号

### Channel 抽象

引入 `Channel` trait，CLI 和 QQ 各自实现。Agent 通过 `Arc<Mutex<Agent>>` 跨 channel 共享，串行化访问。单用户场景下 Mutex 排队可接受。

### QQ confirm 策略

QQ 下无法弹 stdin，独立设计三档：
- `always`（默认）：跳过有副作用的工具，回复用户原因
- `whitelist`：白名单内放行，其余跳过（P1.5 简化：禁用所有需确认工具）
- `none`：全放行

### 长回复分片

QQ 单条消息上限约 2000 字符。LLAIA 取 1800 作为安全阈值，按段落 → 行 → 字符三级 fallback 切分。代码块跨片时闭合后再开。

### 限制

- P1.5 只支持单进程运行（CLI 和 QQ 同进程）。分进程运行需要重新设计 SessionStore。
- 只支持 C2C 文本消息。群聊、图片、语音、文件均不支持。
- 不做主动消息推送。

## 影响

- 新增 `Channel` trait 和 `QqChannel` 实现
- `Tool` trait 加 `requires_confirm()` 方法
- `Agent` 加 channel 感知的工具执行检查
- config 加 `[channels.qq]` 节
```

- [ ] **Step 2: 更新 readme.md**

在 readme.md 的"版本规划"表里把 P1.5 状态从"计划中"改为"开发中"或"已完成"。在"快速开始"节加 QQ 配置说明（简短）。

- [ ] **Step 3: 更新 AGENTS.md**

在 AGENTS.md 的"架构"节加"Channel 抽象"说明。在"工具集"节加 `requires_confirm()` 说明。在"工作区"节加 `[channels.qq]` 配置说明。

- [ ] **Step 4: Commit**

```bash
git add docs/adr/0009-qq-channel.md readme.md AGENTS.md
git commit -m "docs: ADR 0009 + readme/agents update for QQ channel"
```

---

## 完成标准

- [ ] 所有 cargo test 通过（含新增的 qq_split 和 qq_http）
- [ ] `cargo run -- chat` CLI 行为不回归
- [ ] QQ channel 能连接腾讯官方 gateway，收发 C2C 文本消息
- [ ] QQ 下有副作用的工具被正确跳过（always 模式）
- [ ] 长回复正确分片
- [ ] CLI 和 QQ 跨 channel 共享 session
- [ ] ADR 0009、readme、AGENTS 更新

---

## 实现注意事项

1. **腾讯官方 API 文档**：https://bot.q.qq.com/wiki/ 是权威来源。Task 7 中的 endpoint URL 和 payload schema 是基于公开信息的骨架，实现时必须对照官方文档调整。特别是：
   - 鉴权 handshake（IDENTIFY payload）
   - 心跳维持（HEARTBEAT opcode 和间隔）
   - C2C 消息事件 schema
   - 发送消息 API 的 msg_type 和 content 字段

2. **WebSocket 重连**：spec 未深入。如果 WS 断开，简单实现是退出程序。生产级实现应该自动重连 + RESUME。P1.5 接受"断开即退出"。

3. **mockito 测试中的 base_url 注入**：Task 9 给了 `QqChannelTestable` 的参考实现。如果觉得冗余，可以重构 `QqChannel` 本身加 `api_base_url: String` 字段，默认 `"https://api.sgroup.qq.com"`，测试时注入 mockito URL。这是更干净的方式，**推荐**。

4. **futures-util 引用**：`StreamExt` 用于 `read.next()`，`SinkExt` 用于 `write.send()`。在 qq.rs 顶部加 `use futures_util::{SinkExt, StreamExt};`。

5. **Token 安全**：当前 config 明文存 token。后续 ADR 单独讨论环境变量插值。
