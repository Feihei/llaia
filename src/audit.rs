use anyhow::Result;
use std::path::PathBuf;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

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
        let _g = self.lock.lock().await;
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
        // tokio 的 File 在 drop 时不保证数据落盘，必须显式 flush，
        // 否则调用方紧接着读文件时可能读到空/不完整内容（CI 上偶发竞态）。
        file.flush().await?;
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
