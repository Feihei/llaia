//! `ask_user` 工具（ADR-0022）：agent 在执行中主动向用户抛问题并阻塞等待回答。
//!
//! 注意：实际的"挂起—等待—续跑"由 agent 循环（`runner::execute_tool_calls`
//! 按工具名拦截 + `agent::handle_input_streaming` 检测单 pending 续答）处理，
//! 本文件只负责定义工具 schema 与参数解析，供模型看到并调用。
//! `execute` 在挂起路径下不会被调用（runner 拦截），保留为无害占位。

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

/// 工具名常量，供 runner / slash 复用，避免字符串散落。
pub const ASK_USER_TOOL_NAME: &str = "ask_user";

/// 解析 ask_user 的参数：
/// - `question`：必填，问题文本
/// - `choices`：可选，结构化单选项列表
pub fn parse_ask_user_args(args: &Value) -> Result<(String, Option<Vec<String>>)> {
    let question = args
        .get("question")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow!("ask_user: missing required `question`"))?
        .to_string();
    let choices = match args.get("choices") {
        Some(Value::Array(arr)) if !arr.is_empty() => {
            let mut out = Vec::new();
            for c in arr {
                if let Some(s) = c.as_str() {
                    out.push(s.to_string());
                }
            }
            if out.is_empty() {
                None
            } else {
                Some(out)
            }
        }
        _ => None,
    };
    Ok((question, choices))
}

pub struct AskUserTool;

#[async_trait]
impl crate::tools::Tool for AskUserTool {
    fn name(&self) -> &str {
        ASK_USER_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Ask the user a clarifying question and PAUSE until they answer, then continue. \
         Use for non-trivial decisions: choosing an approach, confirming scope, or filling gaps. \
         Provide `question` (required). Optionally provide `choices` (array of strings) for a structured single-choice question. \
         On interactive channels the next plain user message is treated as the answer (or use /answer <id> when multiple questions are pending). \
         On non-interactive channels this returns a best-guess-continued note instead of waiting."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The clarifying question to ask the user."
                },
                "choices": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional structured single-choice options. When provided, the user may pick one (reply the option text or its index)."
                }
            },
            "required": ["question"]
        })
    }

    async fn execute(&self, _args: &Value, _channel: &str) -> Result<String> {
        // 正常路径下 runner 会按工具名拦截 ask_user 并走挂起逻辑，
        // 不会走到这里。保留占位以便工具接口完整、可被单测直接调用。
        Ok("[ask_user] handled by the agent loop (suspend/resume)".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_requires_question() {
        let r = parse_ask_user_args(&json!({}));
        assert!(r.is_err());
        let r = parse_ask_user_args(&json!({ "question": "  " }));
        assert!(r.is_err());
    }

    #[test]
    fn parse_plain_question() {
        let (q, c) = parse_ask_user_args(&json!({ "question": "which lib?" })).unwrap();
        assert_eq!(q, "which lib?");
        assert!(c.is_none());
    }

    #[test]
    fn parse_with_choices() {
        let (q, c) = parse_ask_user_args(&json!({
            "question": "pick one",
            "choices": ["a", "b", "c"]
        }))
        .unwrap();
        assert_eq!(q, "pick one");
        assert_eq!(
            c,
            Some(vec!["a".to_string(), "b".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn parse_ignores_empty_choices() {
        let (_, c) = parse_ask_user_args(&json!({
            "question": "q",
            "choices": []
        }))
        .unwrap();
        assert!(c.is_none());
        let (_, c2) = parse_ask_user_args(&json!({
            "question": "q",
            "choices": [1, 2]
        }))
        .unwrap();
        assert!(c2.is_none());
    }
}
