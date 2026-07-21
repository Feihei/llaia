use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::io::Write;

pub struct Terminal {
    pub confirm_mode: String,
    pub whitelist: Vec<String>,
}

impl Terminal {
    pub fn new(confirm_mode: String, whitelist: Vec<String>) -> Self {
        Self {
            confirm_mode,
            whitelist,
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
        "Execute a shell command. Returns combined stdout+stderr."
    }
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
            .output()
            .await;
        #[cfg(not(windows))]
        let output = tokio::process::Command::new("sh")
            .args(["-c", command])
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
    async fn test_execute_echo() {
        let t = Terminal::new("none".into(), vec![]);
        let result = t
            .execute(&serde_json::json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert!(result.contains("hello"));
    }
}
