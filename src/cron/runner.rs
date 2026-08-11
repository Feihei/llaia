//! cron 任务执行器：agent 模式 + tools 模式 + 占位符替换。
//!
//! - agent 模式：构造独立 session，唤醒主 agent 跑一轮，回复推送给 pusher。
//! - tools 模式：按 steps 顺序执行工具链，最后一步输出推送给 pusher。
//! - 失败处理：推送失败通知 + 返回 Err（不重试、不 disable）。

use crate::agent::Agent;
use crate::cron::{CronMode, CronTask, ProactivePusher};
use serde_json::Value;
use std::sync::Arc;

/// 替换 args 中的 `{{prev}}` 和 `{{now}}` 占位符。
/// - `prev`：上一步工具输出
/// - `now`：当前 RFC3339 时间
///
/// 仅替换字符串值内的占位符，递归处理对象/数组；其他类型原样返回。
pub fn substitute_placeholders(args: &Value, prev: &str, now: &str) -> Value {
    match args {
        Value::String(s) => Value::String(s.replace("{{prev}}", prev).replace("{{now}}", now)),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), substitute_placeholders(v, prev, now));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|v| substitute_placeholders(v, prev, now))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// tools 模式：顺序执行 steps，最后一步输出推送到 pusher。
/// 任一步失败：推送失败通知 + 返回 Err（不重试）。
pub async fn run_tools_mode(
    agent: Arc<tokio::sync::Mutex<Agent>>,
    task: &CronTask,
    pusher: &dyn ProactivePusher,
) -> anyhow::Result<()> {
    let steps = match &task.steps {
        Some(s) if !s.is_empty() => s,
        _ => {
            let msg = format!("[cron:{} 失败] tools 模式但 steps 为空", task.id);
            tracing::error!("{}", msg);
            let _ = pusher.push(&msg).await;
            anyhow::bail!(msg);
        }
    };

    let mut prev = String::new();
    let now = chrono::Local::now().to_rfc3339();
    for (i, step) in steps.iter().enumerate() {
        let args = substitute_placeholders(&step.args, &prev, &now);
        let tool_name = step.tool.clone();

        // 取工具（克隆 Arc 避免 lock 持有跨 await）
        let tool = {
            let a = agent.lock().await;
            a.tools.get(&tool_name).cloned()
        };
        let tool = match tool {
            Some(t) => t,
            None => {
                let msg = format!("[cron:{} 失败] 工具 {} 未注册", task.id, tool_name);
                tracing::error!("{}", msg);
                let _ = pusher.push(&msg).await;
                anyhow::bail!(msg);
            }
        };

        let result = tool.execute(&args, "cron").await;
        let is_last = i + 1 == steps.len();
        match result {
            Ok(output) => {
                if is_last {
                    // 最后一步输出推送到 channel
                    if let Err(e) = pusher.push(&output).await {
                        tracing::warn!(error = %e, task = %task.id, "push last step output failed");
                    }
                }
                prev = output;
            }
            Err(e) => {
                let msg = format!("[cron:{} 失败] step {} ({}): {}", task.id, i, tool_name, e);
                tracing::error!("{}", msg);
                let _ = pusher.push(&msg).await;
                anyhow::bail!(msg);
            }
        }
    }
    Ok(())
}

/// agent 模式：构造独立 session，唤醒主 agent 跑一轮，回复推送到 pusher。
pub async fn run_agent_mode(
    agent: Arc<tokio::sync::Mutex<Agent>>,
    task: &CronTask,
    pusher: &dyn ProactivePusher,
) -> anyhow::Result<()> {
    // 内置任务走专用编排入口（如做梦两阶段管线）
    if task.kind.as_deref() == Some("dream") {
        let mut a = agent.lock().await;
        match crate::cron::dream::run_dream(&mut a, task, false).await {
            Ok(summary) => {
                if let Err(e) = pusher.push(&summary).await {
                    tracing::warn!(error = %e, task = %task.id, "push dream summary failed");
                }
                return Ok(());
            }
            Err(e) => {
                let msg = format!("[cron:{} 失败] dream: {}", task.id, e);
                tracing::error!("{}", msg);
                let _ = pusher.push(&msg).await;
                return Err(anyhow::anyhow!(msg));
            }
        }
    }

    let prompt = task.prompt.as_deref().unwrap_or("");
    if prompt.is_empty() {
        let msg = format!("[cron:{} 失败] agent 模式但 prompt 为空", task.id);
        tracing::error!("{}", msg);
        let _ = pusher.push(&msg).await;
        anyhow::bail!(msg);
    }

    let cron_prompt = format!("[cron:{}] {}", task.id, prompt);

    // 创建独立 session（source 标记 cron:<id>，便于 WebUI 历史过滤）
    let session_id = {
        let a = agent.lock().await;
        let uuid = uuid::Uuid::new_v4().to_string();
        a.session_store
            .create_session(&uuid, &format!("cron:{}", task.id))?
    };

    // 跑独立 turn
    let result = {
        let mut a = agent.lock().await;
        a.run_isolated_turn(&cron_prompt, "cron", session_id).await
    };

    match result {
        Ok(reply) => {
            if let Err(e) = pusher.push(&reply).await {
                tracing::warn!(error = %e, task = %task.id, "push agent reply failed");
            }
            Ok(())
        }
        Err(e) => {
            let msg = format!("[cron:{} 失败] agent turn: {}", task.id, e);
            tracing::error!("{}", msg);
            let _ = pusher.push(&msg).await;
            anyhow::bail!(msg)
        }
    }
}

/// 执行一个 cron 任务（按 mode 分发）。
pub async fn run_task(
    agent: Arc<tokio::sync::Mutex<Agent>>,
    task: &CronTask,
    pusher: &dyn ProactivePusher,
) {
    let task_id = task.id.clone();
    let result = match task.mode {
        CronMode::Agent => run_agent_mode(agent, task, pusher).await,
        CronMode::Tools => run_tools_mode(agent, task, pusher).await,
    };
    if let Err(e) = result {
        tracing::error!(task = %task_id, error = %e, "cron task failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_substitute_placeholders_string() {
        let args = json!({ "text": "v={{prev}} n={{now}}" });
        let out = substitute_placeholders(&args, "P", "2026-01-01T00:00:00Z");
        assert_eq!(out["text"], json!("v=P n=2026-01-01T00:00:00Z"));
    }

    #[test]
    fn test_substitute_placeholders_object_recursive() {
        let args = json!({ "a": { "b": "{{prev}}" } });
        let out = substitute_placeholders(&args, "P", "N");
        assert_eq!(out["a"]["b"], json!("P"));
    }

    #[test]
    fn test_substitute_placeholders_array_recursive() {
        let args = json!({ "list": ["{{prev}}", "{{now}}", "plain"] });
        let out = substitute_placeholders(&args, "P", "N");
        assert_eq!(out["list"][0], json!("P"));
        assert_eq!(out["list"][1], json!("N"));
        assert_eq!(out["list"][2], json!("plain"));
    }

    #[test]
    fn test_substitute_placeholders_non_string_unchanged() {
        let args = json!({ "n": 42, "b": true, "x": null });
        let out = substitute_placeholders(&args, "P", "N");
        assert_eq!(out["n"], json!(42));
        assert_eq!(out["b"], json!(true));
        assert_eq!(out["x"], json!(null));
    }

    #[test]
    fn test_substitute_placeholders_no_placeholder() {
        let args = json!({ "q": "plain text" });
        let out = substitute_placeholders(&args, "P", "N");
        assert_eq!(out["q"], json!("plain text"));
    }

    #[test]
    fn test_substitute_placeholders_multiple_in_one_string() {
        let args = json!({ "text": "{{prev}} then {{now}} then {{prev}}" });
        let out = substitute_placeholders(&args, "P", "N");
        assert_eq!(out["text"], json!("P then N then P"));
    }

    #[test]
    fn test_substitute_placeholders_empty_string() {
        let args = json!({ "text": "" });
        let out = substitute_placeholders(&args, "P", "N");
        assert_eq!(out["text"], json!(""));
    }

    #[test]
    fn test_substitute_placeholders_top_level_scalar() {
        // 顶层标量也应正常处理
        let args = json!("{{prev}}");
        let out = substitute_placeholders(&args, "P", "N");
        assert_eq!(out, json!("P"));
    }
}
