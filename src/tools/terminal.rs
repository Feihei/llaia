use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;

pub struct Terminal {
    pub confirm_mode: String,
    pub whitelist: Vec<String>,
    pub workspace: PathBuf,
}

impl Terminal {
    pub fn new(confirm_mode: String, whitelist: Vec<String>, workspace: PathBuf) -> Self {
        Self {
            confirm_mode,
            whitelist,
            workspace,
        }
    }

    fn needs_confirmation(&self, command: &str) -> bool {
        let first_word = command.split_whitespace().next().unwrap_or("");
        match self.confirm_mode.as_str() {
            "none" => false,
            "always" => true,
            "whitelist" => !self.whitelist.iter().any(|w| w == first_word),
            _ => false,
        }
    }

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

        if self.needs_confirmation(command) {
            if !Self::prompt_confirm(command) {
                return Err(anyhow!("user denied command"));
            }
        }

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
    fn test_needs_confirmation_none() {
        let (_guard, ws) = make_workspace();
        let t = Terminal::new("none".into(), vec![], ws);
        assert!(!t.needs_confirmation("rm -rf /"));
    }

    #[test]
    fn test_needs_confirmation_always() {
        let (_guard, ws) = make_workspace();
        let t = Terminal::new("always".into(), vec![], ws);
        assert!(t.needs_confirmation("ls"));
    }

    #[test]
    fn test_needs_confirmation_whitelist() {
        let (_guard, ws) = make_workspace();
        let t = Terminal::new(
            "whitelist".into(),
            vec!["ls".into(), "cat".into()],
            ws,
        );
        assert!(!t.needs_confirmation("ls -la"));
        assert!(!t.needs_confirmation("cat foo"));
        assert!(t.needs_confirmation("rm foo"));
    }

    #[tokio::test]
    async fn test_execute_echo() {
        let (_guard, ws) = make_workspace();
        let t = Terminal::new("none".into(), vec![], ws);
        let result = t
            .execute(&serde_json::json!({"command": "echo hello"}), "cli")
            .await
            .unwrap();
        assert!(result.contains("hello"));
    }

    #[tokio::test]
    async fn test_command_runs_in_workspace() {
        let (_guard, ws) = make_workspace();
        // 在 workspace 下创建一个标记文件
        std::fs::write(ws.join("marker.txt"), "I_AM_HERE").unwrap();

        let t = Terminal::new("none".into(), vec![], ws.clone());
        // 列当前目录，预期看到 marker.txt（证明 CWD 是 workspace）
        let cmd = if cfg!(windows) { "dir /b" } else { "ls" };
        let result = t
            .execute(&serde_json::json!({"command": cmd}), "cli")
            .await
            .unwrap();
        assert!(
            result.contains("marker.txt"),
            "expected 'marker.txt' in output, got: {}",
            result
        );
    }

    #[test]
    fn test_terminal_requires_confirm() {
        let (_guard, ws) = make_workspace();
        let t = Terminal::new("none".into(), vec![], ws);
        assert!(t.requires_confirm());
    }
}
