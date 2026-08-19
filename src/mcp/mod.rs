//! MCP (Model Context Protocol) client 模块。
//!
//! LLAIA 仅作为 MCP client 消费外部 server 的工具（见 ADR-0014）。
//! 配置来自 `~/.llaia/mcp.toml`（与 cron.toml 一致的独立文件策略）。

pub mod client;
pub mod protocol;
pub mod transport;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// mcp.toml 根配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(default)]
    pub server: Vec<McpServerConfig>,
}

/// transport 类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpTransportKind {
    /// 启动子进程，stdin/stdout JSON-RPC
    Stdio,
    /// streamable HTTP（MCP 2025-06-18 spec，POST JSON-RPC + Mcp-Session-Id）
    Http,
    /// 旧版 SSE transport（GET 长连接读 + POST 写）
    Sse,
}

/// 单个 MCP server 配置（mcp.toml 的 `[[server]]`）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// server 标识，用于工具前缀 `<id>__<tool_name>`
    pub id: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub transport: McpTransportKind,
    // ── stdio 专用 ──
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    // ── HTTP / SSE 专用 ──
    #[serde(default)]
    pub url: Option<String>,
    /// 支持 `${ENV_VAR}` 插值（secret 不落盘）
    #[serde(default)]
    pub headers: HashMap<String, String>,
    // ── 通用 ──
    /// per-server 工具调用超时（秒），覆盖默认 180s，硬上限 600s
    #[serde(default)]
    pub tool_timeout_secs: Option<u64>,
    /// 只读工具白名单：这些工具 `requires_confirm = false`（按原始工具名，不带前缀）
    #[serde(default)]
    pub safe_tools: Vec<String>,
}

fn default_enabled() -> bool {
    true
}

/// 解析失败时的笔误提示：识别 `true` / `false` / `None` 的非法变体——
/// 大写（`True`，Python 习惯）或全角字符（`ｔｒｕｅ`，中文输入法全角模式），
/// 后者在终端里肉眼看与小写 true 无差别。
///
/// toml crate 对无法识别的裸值统一报 `invalid string, expected `"`, `'`，
/// 误导用户去加引号，进而得到 `invalid type: string, expected a boolean`，
/// 报错形成死循环；这里补一条带行号、非 ASCII 字符转义显示的修正提示。
fn bare_value_hint(content: &str) -> Option<String> {
    for (idx, line) in content.lines().enumerate() {
        // 去掉行内注释后再看 k = v
        let bare = line.split('#').next().unwrap_or(line).trim();
        let Some((_, v)) = bare.split_once('=') else {
            continue;
        };
        let v = v.trim();
        if v.is_empty() {
            continue;
        }
        let norm = normalize_ascii(v);
        let reason = match norm.as_str() {
            "true" | "false" => {
                if v == norm {
                    continue; // 已是合法写法，问题在别处
                }
                "TOML booleans must be exactly lowercase ASCII `true` / `false`"
            }
            "none" => "`None` is not valid TOML (use `false`)",
            _ => continue,
        };
        // 转义非 ASCII 字符，让全角/不可见字符现形
        let shown: String = v
            .chars()
            .map(|c| {
                if c.is_ascii_graphic() {
                    c.to_string()
                } else {
                    format!("\\u{{{:04x}}}", c as u32)
                }
            })
            .collect();
        return Some(format!(
            "hint: line {}: `{}` — {} (value shown with escapes: `{}`)",
            idx + 1,
            bare,
            reason,
            shown
        ));
    }
    None
}

/// 归一化比较用：全角字符（U+FF01..U+FF5E）映射回 ASCII，剔除零宽/空白填充字符，
/// 再转小写。仅用于识别 `true`/`false`/`none` 的变体，不修改原值。
fn normalize_ascii(v: &str) -> String {
    v.chars()
        .filter(|c| !matches!(c, '\u{200b}' | '\u{feff}' | '\u{a0}' | '\u{3000}'))
        .map(|c| {
            if ('\u{ff01}'..='\u{ff5e}').contains(&c) {
                // unwrap 安全：全角区减 0xFEE0 后必落在可打印 ASCII 区
                char::from_u32(c as u32 - 0xfee0).unwrap_or(c)
            } else {
                c
            }
        })
        .flat_map(char::to_lowercase)
        .collect()
}

impl McpConfig {
    /// 从文件加载 mcp.toml；文件不存在返回空配置（无 MCP server）。
    /// 加载时对 url / headers / env 值做 `${VAR}` 环境变量插值。
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        let mut cfg = Self::parse(&content)
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {}", path.display(), e))?;
        cfg.expand_env()?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// 仅解析校验（不做 env 插值），供 WebUI 保存前检查。
    pub fn from_str_validate(raw: &str) -> anyhow::Result<Self> {
        let cfg = Self::parse(raw)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// 解析 TOML；失败时附上常见笔误提示（bare_value_hint）。
    fn parse(raw: &str) -> anyhow::Result<Self> {
        match toml::from_str(raw) {
            Ok(cfg) => Ok(cfg),
            Err(e) => {
                let mut msg = e.to_string();
                if let Some(hint) = bare_value_hint(raw) {
                    msg.push('\n');
                    msg.push_str(&hint);
                }
                anyhow::bail!("{}", msg)
            }
        }
    }

    /// url / headers / env 值做 `${VAR}` 插值（复用 config.toml 的机制）
    fn expand_env(&mut self) -> anyhow::Result<()> {
        for s in &mut self.server {
            if let Some(url) = &s.url {
                s.url = Some(crate::config::expand_string(url)?);
            }
            for v in s.headers.values_mut() {
                *v = crate::config::expand_string(v)?;
            }
            for v in s.env.values_mut() {
                *v = crate::config::expand_string(v)?;
            }
        }
        Ok(())
    }

    /// 校验所有 server 配置：id 合法 + transport 字段齐全 + id 唯一
    pub fn validate(&self) -> anyhow::Result<()> {
        let mut seen = std::collections::HashSet::new();
        for s in &self.server {
            if s.id.trim().is_empty() {
                anyhow::bail!("mcp server id must not be empty");
            }
            if s.id.contains("__") {
                anyhow::bail!(
                    "mcp server id must not contain '__' (reserved for tool prefix): {}",
                    s.id
                );
            }
            if s.id.contains(char::is_whitespace) {
                anyhow::bail!("mcp server id must not contain whitespace: {}", s.id);
            }
            if !seen.insert(s.id.clone()) {
                anyhow::bail!("duplicate mcp server id: {}", s.id);
            }
            match s.transport {
                McpTransportKind::Stdio => {
                    if s.command
                        .as_deref()
                        .map(|c| c.trim().is_empty())
                        .unwrap_or(true)
                    {
                        anyhow::bail!("mcp server '{}' transport=stdio requires command", s.id);
                    }
                }
                McpTransportKind::Http | McpTransportKind::Sse => {
                    if s.url
                        .as_deref()
                        .map(|u| u.trim().is_empty())
                        .unwrap_or(true)
                    {
                        anyhow::bail!(
                            "mcp server '{}' transport={:?} requires url",
                            s.id,
                            s.transport
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STDIO_TOML: &str = r#"
[[server]]
id = "filesystem"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[server.env]
FOO = "bar"
"#;

    #[test]
    fn test_parse_stdio_server() {
        let cfg: McpConfig = toml::from_str(STDIO_TOML).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.server.len(), 1);
        let s = &cfg.server[0];
        assert_eq!(s.id, "filesystem");
        assert!(s.enabled);
        assert_eq!(s.transport, McpTransportKind::Stdio);
        assert_eq!(s.command.as_deref(), Some("npx"));
        assert_eq!(s.args.len(), 3);
        assert_eq!(s.env.get("FOO").unwrap(), "bar");
        assert!(s.safe_tools.is_empty());
    }

    #[test]
    fn test_parse_http_server() {
        let toml = r#"
[[server]]
id = "remote"
transport = "http"
url = "https://example.com/mcp"
tool_timeout_secs = 300
safe_tools = ["read_file"]

[server.headers]
Authorization = "Bearer abc"
"#;
        let cfg: McpConfig = toml::from_str(toml).unwrap();
        cfg.validate().unwrap();
        let s = &cfg.server[0];
        assert_eq!(s.transport, McpTransportKind::Http);
        assert_eq!(s.tool_timeout_secs, Some(300));
        assert_eq!(s.safe_tools, vec!["read_file".to_string()]);
        assert_eq!(s.headers.get("Authorization").unwrap(), "Bearer abc");
    }

    #[test]
    fn test_validate_stdio_requires_command() {
        let toml = r#"
[[server]]
id = "broken"
transport = "stdio"
"#;
        let cfg: McpConfig = toml::from_str(toml).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_validate_http_requires_url() {
        let toml = r#"
[[server]]
id = "broken"
transport = "http"
"#;
        let cfg: McpConfig = toml::from_str(toml).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_double_underscore_id() {
        let toml = r#"
[[server]]
id = "bad__id"
transport = "stdio"
command = "echo"
"#;
        let cfg: McpConfig = toml::from_str(toml).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_duplicate_id() {
        let toml = r#"
[[server]]
id = "dup"
transport = "stdio"
command = "echo"

[[server]]
id = "dup"
transport = "stdio"
command = "echo"
"#;
        let cfg: McpConfig = toml::from_str(toml).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_load_missing_file_returns_empty() {
        let cfg = McpConfig::load(Path::new("/nonexistent/mcp.toml")).unwrap();
        assert!(cfg.server.is_empty());
    }

    #[test]
    fn test_parse_error_hint_for_capitalized_bool() {
        // Python 风格大写 True：toml crate 报误导性的 "invalid string"，
        // 我们的提示必须指出行号与正确写法
        let raw =
            "[[server]]\nid = \"x\"\ntransport = \"stdio\"\ncommand = \"echo\"\nenabled = True\n";
        let err = McpConfig::from_str_validate(raw).unwrap_err().to_string();
        assert!(err.contains("invalid string"), "got: {}", err);
        assert!(err.contains("line 5"), "got: {}", err);
        assert!(err.contains("enabled = True"), "got: {}", err);
        assert!(err.contains("lowercase"), "got: {}", err);
    }

    #[test]
    fn test_parse_error_hint_for_fullwidth_true() {
        // 中文输入法全角模式下的 ｔｒｕｅ：终端里肉眼看与小写 true 无差别，
        // 提示必须把非 ASCII 字符转义显示出来
        let raw = "[[server]]\nid = \"x\"\ntransport = \"stdio\"\ncommand = \"echo\"\nenabled = \u{ff54}\u{ff52}\u{ff55}\u{ff45}\n".to_string();
        let err = McpConfig::from_str_validate(&raw).unwrap_err().to_string();
        assert!(err.contains("invalid string"), "got: {}", err);
        assert!(err.contains("\\u{ff54}"), "got: {}", err);
        assert!(err.contains("lowercase"), "got: {}", err);
    }

    #[test]
    fn test_parse_error_hint_ignores_valid_lines() {
        // 合法小写 true 不触发提示（错误另有原因时不得给出误导 hint）
        let hint = bare_value_hint("enabled = true\ncommand = \"echo\"\n");
        assert!(hint.is_none());
        // 引号包裹的字符串值不误报
        let hint = bare_value_hint("command = \"True\"\n");
        assert!(hint.is_none());
    }

    #[test]
    fn test_parse_error_hint_for_none() {
        let raw =
            "[[server]]\nid = \"x\"\ntransport = \"stdio\"\ncommand = \"echo\"\nenabled = None\n";
        let err = McpConfig::from_str_validate(raw).unwrap_err().to_string();
        assert!(err.contains("enabled = None"), "got: {}", err);
    }

    #[test]
    fn test_env_interpolation() {
        std::env::set_var("LLAIA_MCP_TEST_TOKEN_2026", "secret123");
        let toml = r#"
[[server]]
id = "remote"
transport = "http"
url = "https://example.com/mcp"

[server.headers]
Authorization = "Bearer ${LLAIA_MCP_TEST_TOKEN_2026}"
"#;
        let mut cfg: McpConfig = toml::from_str(toml).unwrap();
        cfg.expand_env().unwrap();
        assert_eq!(
            cfg.server[0].headers.get("Authorization").unwrap(),
            "Bearer secret123"
        );
        std::env::remove_var("LLAIA_MCP_TEST_TOKEN_2026");
    }
}
