//! Cron 任务管理工具：让 agent 通过工具调用动态增删改查定时任务。
//!
//! 设计参考 AstrBot 的 FutureTaskTool：单一工具 + action 参数路由到
//! create / update / delete / list 四种操作。LLAIA 是单用户私人助理，
//! 因此不做 sender_id / session 归属校验（zeroclaw / AstrBot 多用户才需要）。
//!
//! 调度器实例通过 OnceCell 延迟注入（与 DelegateTool 同模式）：
//! - chat 模式不启动 CronScheduler，工具返回友好错误提示
//! - serve 模式启动后注入，工具完整可用

use crate::cron::{CronMode, CronScheduler, CronTask, Step};
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::OnceCell;

pub struct CronTool {
    scheduler: OnceCell<Arc<CronScheduler>>,
}

impl CronTool {
    pub fn new() -> Self {
        Self {
            scheduler: OnceCell::new(),
        }
    }

    /// 注入 CronScheduler（serve_cmd 在 CronScheduler::start 成功后调用）。
    /// 重复调用会被忽略（OnceCell 语义）。
    pub fn set_scheduler(&self, s: Arc<CronScheduler>) {
        let _ = self.scheduler.set(s);
    }

    fn get_scheduler(&self) -> Option<&Arc<CronScheduler>> {
        self.scheduler.get()
    }
}

impl Default for CronTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for CronTool {
    fn name(&self) -> &str {
        "cron_task"
    }

    fn description(&self) -> &str {
        "Manage scheduled tasks (cron). Supports four operations: create/update/delete/list. \
         On trigger, executes by mode: agent mode wakes the main agent to run a turn (consumes tokens), \
         tools mode runs the tool chain directly (no tokens consumed). \
         Results are pushed to the channel specified by `channel` (qq/web/cli; cli has no persistent connection, so results are dropped). \
         Only available in 'llaia serve' mode; the debug 'llaia chat' mode cannot use it."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "update", "delete", "list"],
                    "description": "Operation type. list needs no other parameters. create requires a full task definition. update requires id plus the fields to update. delete only needs id."
                },
                "id": {
                    "type": "string",
                    "description": "Task ID (unique identifier, must not contain whitespace). Required for create/update/delete."
                },
                "schedule": {
                    "type": "string",
                    "description": "5-field cron expression (minute hour day month weekday), e.g. '0 8 * * *' (daily at 8:00), '*/30 * * * *' (every 30 minutes), '0 9 * * 1-5' (weekdays at 9:00)."
                },
                "mode": {
                    "type": "string",
                    "enum": ["agent", "tools"],
                    "description": "agent mode: wakes the main agent using prompt; tools mode: runs the tool chain directly using steps."
                },
                "channel": {
                    "type": "string",
                    "enum": ["qq", "web", "cli"],
                    "description": "Result push target. qq/web require the corresponding channel to be enabled; cli has no persistent connection, so results are dropped."
                },
                "enabled": {
                    "type": "boolean",
                    "description": "Whether the task is enabled (default true). If false, the scheduler will not register it, but the definition is kept."
                },
                "prompt": {
                    "type": "string",
                    "description": "Prompt for agent mode. Required when mode=agent; it is injected into the main agent's context."
                },
                "steps": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "tool": { "type": "string", "description": "Tool name (e.g. search, memory_write)" },
                            "args": { "type": "object", "description": "Tool arguments. Supports {{prev}} (previous step output) and {{now}} (current RFC3339 time) placeholders." }
                        },
                        "required": ["tool"]
                    },
                    "description": "Tool chain for tools mode (executed in order). Required when mode=tools."
                }
            },
            "required": ["action"]
        })
    }

    fn requires_confirm(&self) -> bool {
        true
    }

    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        // action 解析优先于 scheduler 检查：参数错误不依赖运行时状态
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'action'"))?
            .trim()
            .to_lowercase();

        // 未知 action 直接报错（不需要 scheduler）
        if !matches!(action.as_str(), "create" | "update" | "delete" | "list") {
            return Ok(format!(
                "error: unknown action '{}', expected create/update/delete/list",
                action
            ));
        }

        let scheduler = match self.get_scheduler() {
            Some(s) => s,
            None => {
                return Ok("error: cron scheduler not running. \
                     cron tasks are only available in 'llaia serve' mode; start with serve first, then operate cron tasks."
                    .into());
            }
        };

        match action.as_str() {
            "create" => {
                let task = parse_task(args)?;
                scheduler.add_task(task).await?;
                Ok(format!(
                    "created cron task: {}",
                    args["id"].as_str().unwrap_or("")
                ))
            }
            "update" => {
                let task = parse_task(args)?;
                let id = task.id.clone();
                scheduler.update_task(task).await?;
                Ok(format!("updated cron task: {}", id))
            }
            "delete" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("missing 'id' for delete"))?;
                scheduler.remove_task(id).await?;
                Ok(format!("deleted cron task: {}", id))
            }
            "list" => {
                let tasks = scheduler.list_tasks().await;
                if tasks.is_empty() {
                    return Ok("no cron tasks.".into());
                }
                let mut lines = Vec::new();
                for t in tasks {
                    let mode_str = match t.mode {
                        CronMode::Agent => "agent",
                        CronMode::Tools => "tools",
                    };
                    let detail = match t.mode {
                        CronMode::Agent => t
                            .prompt
                            .as_deref()
                            .unwrap_or("")
                            .chars()
                            .take(40)
                            .collect::<String>(),
                        CronMode::Tools => {
                            let n = t.steps.as_ref().map(|s| s.len()).unwrap_or(0);
                            format!("{} steps", n)
                        }
                    };
                    lines.push(format!(
                        "- {} | {} | mode={} | channel={} | enabled={} | {}",
                        t.id, t.schedule, mode_str, t.channel, t.enabled, detail
                    ));
                }
                Ok(lines.join("\n"))
            }
            // 上面已校验，不会走到这里
            _ => unreachable!("action already validated"),
        }
    }
}

/// 从工具参数解析出 CronTask。create/update 共用：update 时未提供 enabled 默认 true。
fn parse_task(args: &Value) -> Result<CronTask> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'id'"))?
        .to_string();

    let schedule = args
        .get("schedule")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'schedule'"))?
        .to_string();

    let mode_str = args
        .get("mode")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'mode'"))?;
    let mode = match mode_str.to_lowercase().as_str() {
        "agent" => CronMode::Agent,
        "tools" => CronMode::Tools,
        other => anyhow::bail!("invalid mode '{}', expected 'agent' or 'tools'", other),
    };

    let channel = args
        .get("channel")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing 'channel'"))?
        .to_string();

    let enabled = args
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let (prompt, steps) = match mode {
        CronMode::Agent => {
            let p = args
                .get("prompt")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("mode=agent requires 'prompt'"))?
                .to_string();
            (Some(p), None)
        }
        CronMode::Tools => {
            let steps_arr = args
                .get("steps")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow!("mode=tools requires 'steps' array"))?;
            if steps_arr.is_empty() {
                anyhow::bail!("mode=tools requires non-empty 'steps'");
            }
            let steps: Vec<Step> = steps_arr.iter().map(parse_step).collect::<Result<_>>()?;
            (None, Some(steps))
        }
    };

    Ok(CronTask {
        id,
        schedule,
        mode,
        channel,
        enabled,
        prompt,
        steps,
        kind: None,
        idle_minutes: None,
    })
}

/// 解析单个 step：tool 必填，args 缺省为空对象 {}。
fn parse_step(v: &Value) -> Result<Step> {
    let tool = v
        .get("tool")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("step missing 'tool'"))?
        .to_string();
    let args = v.get("args").cloned().unwrap_or(json!({}));
    Ok(Step { tool, args })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_task_agent_mode() {
        let args = json!({
            "action": "create",
            "id": "morning_news",
            "schedule": "0 8 * * *",
            "mode": "agent",
            "channel": "web",
            "enabled": true,
            "prompt": "查今天的新闻"
        });
        let task = parse_task(&args).unwrap();
        assert_eq!(task.id, "morning_news");
        assert_eq!(task.schedule, "0 8 * * *");
        assert!(matches!(task.mode, CronMode::Agent));
        assert_eq!(task.channel, "web");
        assert!(task.enabled);
        assert_eq!(task.prompt.as_deref(), Some("查今天的新闻"));
        assert!(task.steps.is_none());
    }

    #[test]
    fn test_parse_task_tools_mode() {
        let args = json!({
            "id": "health_check",
            "schedule": "*/30 * * * *",
            "mode": "tools",
            "channel": "web",
            "steps": [
                { "tool": "search", "args": { "query": "llaia" } },
                { "tool": "memory_write", "args": { "entry": "checked at {{now}}" } }
            ]
        });
        let task = parse_task(&args).unwrap();
        assert!(matches!(task.mode, CronMode::Tools));
        assert!(task.prompt.is_none());
        let steps = task.steps.unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].tool, "search");
        assert_eq!(steps[1].args["entry"], "checked at {{now}}");
    }

    #[test]
    fn test_parse_task_defaults_enabled_true() {
        let args = json!({
            "id": "t",
            "schedule": "0 0 * * *",
            "mode": "agent",
            "channel": "web",
            "prompt": "p"
        });
        let task = parse_task(&args).unwrap();
        assert!(task.enabled, "enabled should default to true");
    }

    #[test]
    fn test_parse_task_step_args_default_empty() {
        let args = json!({
            "id": "t",
            "schedule": "0 0 * * *",
            "mode": "tools",
            "channel": "web",
            "steps": [ { "tool": "memory_write" } ]
        });
        let task = parse_task(&args).unwrap();
        let steps = task.steps.unwrap();
        assert_eq!(steps[0].args, json!({}));
    }

    #[test]
    fn test_parse_task_missing_id() {
        let args =
            json!({ "schedule": "0 0 * * *", "mode": "agent", "channel": "web", "prompt": "p" });
        assert!(parse_task(&args).is_err());
    }

    #[test]
    fn test_parse_task_agent_missing_prompt() {
        let args = json!({
            "id": "t", "schedule": "0 0 * * *", "mode": "agent", "channel": "web"
        });
        let err = parse_task(&args).unwrap_err().to_string();
        assert!(err.contains("prompt"));
    }

    #[test]
    fn test_parse_task_tools_empty_steps() {
        let args = json!({
            "id": "t", "schedule": "0 0 * * *", "mode": "tools", "channel": "web", "steps": []
        });
        let err = parse_task(&args).unwrap_err().to_string();
        assert!(err.contains("non-empty"));
    }

    #[test]
    fn test_parse_task_invalid_mode() {
        let args = json!({
            "id": "t", "schedule": "0 0 * * *", "mode": "invalid", "channel": "web"
        });
        let err = parse_task(&args).unwrap_err().to_string();
        assert!(err.contains("invalid mode"));
    }

    #[tokio::test]
    async fn test_execute_without_scheduler_returns_friendly_error() {
        let tool = CronTool::new();
        let result = tool
            .execute(&json!({"action": "list"}), "cli")
            .await
            .unwrap();
        assert!(result.contains("cron scheduler not running"));
        assert!(result.contains("llaia serve"));
    }

    #[tokio::test]
    async fn test_execute_missing_action() {
        let tool = CronTool::new();
        let result = tool.execute(&json!({}), "cli").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_unknown_action() {
        let tool = CronTool::new();
        let result = tool
            .execute(&json!({"action": "foobar"}), "cli")
            .await
            .unwrap();
        assert!(result.contains("unknown action"));
        assert!(result.contains("foobar"));
    }
}
