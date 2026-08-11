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
        "管理定时任务（cron）。支持 create/update/delete/list 四种操作。\
         到点触发时按 mode 执行：agent 模式唤醒主 agent 跑一轮（消耗 token），\
         tools 模式直接跑工具链（不消耗 token）。\
         结果推送到 channel 指定的频道（qq/web/cli，cli 无持久连接会丢弃结果）。\
         仅在 'llaia serve' 模式下可用，'llaia chat' 调试模式不可用。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "update", "delete", "list"],
                    "description": "操作类型。list 无需其他参数。create 需要完整任务定义。update 需要 id + 待更新字段。delete 只需 id。"
                },
                "id": {
                    "type": "string",
                    "description": "任务 ID（唯一标识，不含空白）。create/update/delete 必填。"
                },
                "schedule": {
                    "type": "string",
                    "description": "5 字段 cron 表达式（分 时 日 月 周），例如 '0 8 * * *'（每天 8:00）、'*/30 * * * *'（每 30 分钟）、'0 9 * * 1-5'（工作日 9:00）。"
                },
                "mode": {
                    "type": "string",
                    "enum": ["agent", "tools"],
                    "description": "agent 模式：用 prompt 唤醒主 agent；tools 模式：用 steps 直接跑工具链。"
                },
                "channel": {
                    "type": "string",
                    "enum": ["qq", "web", "cli"],
                    "description": "结果推送目标。qq/web 需对应频道启用；cli 无持久连接，结果丢弃。"
                },
                "enabled": {
                    "type": "boolean",
                    "description": "是否启用（默认 true）。false 则调度器不注册，但定义保留。"
                },
                "prompt": {
                    "type": "string",
                    "description": "agent 模式的提示词。mode=agent 时必填，会注入主 agent 上下文。"
                },
                "steps": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "tool": { "type": "string", "description": "工具名（如 tavily_search、memory_write）" },
                            "args": { "type": "object", "description": "工具参数。支持 {{prev}}（上一步输出）和 {{now}}（当前 RFC3339 时间）占位符。" }
                        },
                        "required": ["tool"]
                    },
                    "description": "tools 模式的工具链（按顺序执行）。mode=tools 时必填。"
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
                { "tool": "tavily_search", "args": { "query": "llaia" } },
                { "tool": "memory_write", "args": { "entry": "checked at {{now}}" } }
            ]
        });
        let task = parse_task(&args).unwrap();
        assert!(matches!(task.mode, CronMode::Tools));
        assert!(task.prompt.is_none());
        let steps = task.steps.unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].tool, "tavily_search");
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
