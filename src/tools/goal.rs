//! `goal` 工具：让 agent 自管长期目标的进度与状态（ADR-0021 决策 #6 路径①）。
//!
//! 用户侧用 /goal 系列 slash 命令设定；本工具供 agent 在执行中回写进度、
//! 判定达成后标记 done。落盘位置与 /goal 命令一致：`<config_dir>/workspace/goal.md`。
use crate::goal::{update_progress, update_status, GoalStatus};
use crate::tools::Tool;
use anyhow::Result;
use serde_json::json;
use serde_json::Value;

pub const GOAL_TOOL_NAME: &str = "goal";

pub struct GoalTool {
    workspace: std::path::PathBuf,
}

impl GoalTool {
    pub fn new(workspace: std::path::PathBuf) -> Self {
        Self { workspace }
    }
}

#[async_trait::async_trait]
impl Tool for GoalTool {
    fn name(&self) -> &str {
        GOAL_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Manage the long-term goal persisted in goal.md (the agent home directory). \
         Actions: `done` (mark the active goal achieved), `cancel` (abandon it), \
         `progress` (record a progress note; requires `text`), `set` (reset the objective; \
         requires `text`). Use this to keep the goal's Progress section up to date and to mark \
         completion once the objective is met. No user confirmation needed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["done", "cancel", "progress", "set"],
                    "description": "What to do with the long-term goal."
                },
                "text": {
                    "type": "string",
                    "description": "For 'progress': the progress note. For 'set': the new objective. Ignored for done/cancel."
                }
            },
            "required": ["action"]
        })
    }

    fn requires_confirm(&self) -> bool {
        false
    }

    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'action' (done|cancel|progress|set)"))?;
        match action {
            "done" => {
                update_status(&self.workspace, GoalStatus::Done)?;
                Ok("[goal] marked done.".into())
            }
            "cancel" => {
                update_status(&self.workspace, GoalStatus::Cancelled)?;
                Ok("[goal] cancelled.".into())
            }
            "progress" => {
                let text = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if text.is_empty() {
                    return Err(anyhow::anyhow!(
                        "'progress' action requires a non-empty 'text'"
                    ));
                }
                update_progress(&self.workspace, text)?;
                Ok(format!("[goal] progress updated: {text}"))
            }
            "set" => {
                let text = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if text.is_empty() {
                    return Err(anyhow::anyhow!("'set' action requires a non-empty 'text'"));
                }
                crate::goal::set_goal(&self.workspace, text)?;
                Ok(format!("[goal] goal (re)set: {text}"))
            }
            other => Err(anyhow::anyhow!("unknown goal action: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::GoalStatus;
    use crate::tools::Tool;
    use tempfile::tempdir;

    fn tool() -> GoalTool {
        let tmp = tempdir().unwrap();
        GoalTool::new(tmp.path().to_path_buf())
    }

    #[tokio::test]
    async fn set_and_done() {
        let t = tool();
        let r = t
            .execute(&json!({ "action": "set", "text": "build X" }), "cli")
            .await
            .unwrap();
        assert!(r.contains("build X"));
        let r = t
            .execute(&json!({ "action": "done" }), "cli")
            .await
            .unwrap();
        assert!(r.contains("done"));
        let s = crate::goal::read_goal(t.workspace.as_path()).unwrap();
        assert_eq!(s.status, GoalStatus::Done);
    }

    #[tokio::test]
    async fn progress_requires_text() {
        let t = tool();
        t.execute(&json!({ "action": "set", "text": "x" }), "cli")
            .await
            .unwrap();
        assert!(t
            .execute(&json!({ "action": "progress" }), "cli")
            .await
            .is_err());
        let r = t
            .execute(&json!({ "action": "progress", "text": "midway" }), "cli")
            .await
            .unwrap();
        assert!(r.contains("midway"));
    }

    #[tokio::test]
    async fn done_without_goal_errors() {
        let t = tool();
        assert!(t
            .execute(&json!({ "action": "done" }), "cli")
            .await
            .is_err());
    }
}
