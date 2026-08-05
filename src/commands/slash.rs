use crate::agent::Agent;
use anyhow::Result;

pub enum SlashOutcome {
    Handled(String),
    Exit,
    NotSlash,
}

/// 处理斜杠命令，返回 (outcome, 输出文本)。
/// 输出文本由调用方决定如何呈现（CLI 打印，QQ 发回用户）。
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
        "/help" => Ok(SlashOutcome::Handled(
            "commands: /new /exit /stop /compact /clear /remember <text> /config /help".into(),
        )),
        "/new" => {
            agent.context.clear();
            agent.context.summary = None;
            Ok(SlashOutcome::Handled("[new session]".into()))
        }
        "/clear" => {
            agent.context.clear();
            agent.context.summary = None;
            Ok(SlashOutcome::Handled("[context cleared]".into()))
        }
        "/compact" => {
            match agent.provider_snapshot().await {
                Some(p) => match agent.context.compact(p.as_ref(), 6).await {
                    Ok(_) => Ok(SlashOutcome::Handled("[compacted]".into())),
                    Err(e) => Ok(SlashOutcome::Handled(format!("[compact failed: {}]", e))),
                },
                None => Ok(SlashOutcome::Handled(
                    "[compact failed: 未配置 provider]".into(),
                )),
            }
        }
        "/remember" => {
            if args.is_empty() {
                Ok(SlashOutcome::Handled("usage: /remember <text>".into()))
            } else if let Some(tool) = agent.tools.get("memory_write") {
                let _ = tool
                    .execute(&serde_json::json!({"entry": args}), "cli")
                    .await;
                Ok(SlashOutcome::Handled("[remembered]".into()))
            } else {
                Ok(SlashOutcome::Handled(
                    "[memory_write tool not registered]".into(),
                ))
            }
        }
        "/config" => {
            let info = format!(
                "context_threshold: {}\nmax_iterations: {}\ncontext_size: {}\nhistory msgs: {}\nsummary: {}\ntools: {:?}",
                agent.context_threshold,
                agent.max_iterations,
                agent.context_size,
                agent.context.history.len(),
                agent.context.summary.is_some(),
                agent.tools.names()
            );
            Ok(SlashOutcome::Handled(info))
        }
        _ => Ok(SlashOutcome::Handled(format!("[unknown command: {}]", cmd))),
    }
}
