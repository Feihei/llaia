use crate::agent::Agent;
use anyhow::Result;

pub enum SlashOutcome {
    Handled,
    Exit,
    NotSlash,
}

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
            } else if let Some(tool) = agent.tools.get("memory_write") {
                let _ = tool
                    .execute(&serde_json::json!({"entry": args}))
                    .await;
                println!("[remembered]");
            } else {
                println!("[memory_write tool not registered]");
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
