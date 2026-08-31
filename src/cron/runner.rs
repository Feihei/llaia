//! cron 任务执行器：agent 模式 + tools 模式 + 占位符替换。
//!
//! - agent 模式：构造独立 session，唤醒主 agent 跑一轮，回复推送给 pusher。
//! - tools 模式：按 steps 顺序执行工具链，最后一步输出推送给 pusher。
//! - 失败处理：provider 报错 / 超时 / 交付门判定「白跑」→ agent 模式至多重试 3 次；
//!   仍失败则推送失败通知 + 返回 Err（不 disable）。

use crate::agent::{Agent, TurnToolCall};
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
            let msg = format!("[cron:{} failed] tools mode but steps is empty", task.id);
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

        // 取工具（clone 出 Arc 避免跨 await 持有 agent 锁）
        let tool = {
            let a = agent.lock().await;
            a.tools.get(&tool_name)
        };
        let tool = match tool {
            Some(t) => t,
            None => {
                let msg = format!(
                    "[cron:{} failed] tool {} not registered",
                    task.id, tool_name
                );
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
                let msg = format!(
                    "[cron:{} failed] step {} ({}): {}",
                    task.id, i, tool_name, e
                );
                tracing::error!("{}", msg);
                let _ = pusher.push(&msg).await;
                anyhow::bail!(msg);
            }
        }
    }
    Ok(())
}

/// 交付门：判断 agent 模式一轮的回复值不值得推给用户。
///
/// 只挡两种确定的「白跑」：
/// 1. 回复是空白；
/// 2. 本轮调用过工具但**全部失败**——模型常就着错误结果认输，回一句模板填充语。实测
///    ornith-1.5-35b 在唯一的 `send_file`（路径是它编的）失败后回了 `No response requested.`，
///    它非空、两次 provider 请求都 200，于是被当成成功简讯原样推给了用户，后台零报错。
///
/// 刻意保守：没有工具调用 + 有文本一律放行（凭上下文直接作答的任务是合法的）。不做
/// 「无意义文本」黑名单式判据——那是猜不完的，误杀正常简讯的代价比漏放一句残句更大。
fn evaluate_agent_reply(reply: &str, calls: &[TurnToolCall]) -> Result<(), String> {
    if reply.trim().is_empty() {
        return Err("agent returned an empty reply".to_string());
    }
    if !calls.is_empty() && calls.iter().all(|c| !c.ok) {
        let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
        return Err(format!(
            "all {} tool call(s) failed, nothing was accomplished: {}",
            calls.len(),
            names.join(", ")
        ));
    }
    Ok(())
}

/// agent 模式：构造独立 session，唤醒主 agent 跑一轮，回复推送到 pusher。
pub async fn run_agent_mode(
    agent: Arc<tokio::sync::Mutex<Agent>>,
    task: &CronTask,
    pusher: &dyn ProactivePusher,
) -> anyhow::Result<()> {
    let prompt = task.prompt.as_deref().unwrap_or("");
    if prompt.is_empty() {
        let msg = format!("[cron:{} failed] agent mode but prompt is empty", task.id);
        tracing::error!("{}", msg);
        let _ = pusher.push(&msg).await;
        anyhow::bail!(msg);
    }

    let cron_prompt = format!("[cron:{}] {}", task.id, prompt);

    // 复用同一 cron 任务的会话（按 channel = `cron:<id>` 精确查找），
    // 否则每次触发都新建一个会话、历史被碎片化。找不到时才新建。
    let channel = format!("cron:{}", task.id);
    let session_id = {
        let a = agent.lock().await;
        match a.session_store.session_by_channel(&channel)? {
            Some(id) => id,
            None => {
                let uuid = uuid::Uuid::new_v4().to_string();
                a.session_store.create_session(&uuid, &channel)?
            }
        }
    };

    // 派生独立 agent 跑 turn，避免整轮持全局锁冻结主会话 / WebUI。
    // 顶层超时兜底 + 重试：provider 流式挂起（SSE keepalive 使 per-chunk 120s 超时不触发）时
    // 单次会失败，但往往为瞬时抖动，重试常能恢复；最多重试 CRON_TURN_MAX_ATTEMPTS 次，
    // 每次重新派生（丢弃上一轮可能残留的上下文）从干净状态重发，避免无限占着资源。
    const CRON_TURN_TIMEOUT_SECS: u64 = 300;
    const CRON_TURN_MAX_ATTEMPTS: usize = 3;
    let mut forked = {
        let a = agent.lock().await;
        a.fork_for_isolated(session_id, true)
    };
    let mut attempt = 0usize;
    let result = loop {
        attempt += 1;
        let r = match tokio::time::timeout(
            std::time::Duration::from_secs(CRON_TURN_TIMEOUT_SECS),
            forked.handle_input(&cron_prompt, "cron"),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => Err(anyhow::anyhow!(
                "cron isolated turn timed out after {}s",
                CRON_TURN_TIMEOUT_SECS
            )),
        };
        match r {
            Ok(text) => {
                // 交付门：provider 顺利返回不等于「真的把活干了」。白跑一次也按失败重试，
                // 耗尽次数则返回 Err——宁可推一条失败通知，也不把残句当简讯推给频道。
                match evaluate_agent_reply(&text, &forked.turn_tool_calls) {
                    Ok(()) => break Ok(text),
                    Err(why) => {
                        if attempt >= CRON_TURN_MAX_ATTEMPTS {
                            break Err(anyhow::anyhow!(why));
                        }
                        tracing::warn!(
                            task = %task.id,
                            attempt,
                            max = CRON_TURN_MAX_ATTEMPTS,
                            reason = %why,
                            "cron turn delivered nothing useful, retrying"
                        );
                        forked = {
                            let a = agent.lock().await;
                            a.fork_for_isolated(session_id, true)
                        };
                    }
                }
            }
            Err(e) => {
                if attempt >= CRON_TURN_MAX_ATTEMPTS {
                    break Err(e);
                }
                tracing::warn!(
                    task = %task.id,
                    attempt,
                    max = CRON_TURN_MAX_ATTEMPTS,
                    "cron turn attempt failed, retrying"
                );
                // 重新派生，丢弃上一轮可能残留的上下文，从干净状态重试
                forked = {
                    let a = agent.lock().await;
                    a.fork_for_isolated(session_id, true)
                };
            }
        }
    };

    match result {
        Ok(reply) => {
            if let Err(e) = pusher.push(&reply).await {
                tracing::warn!(error = %e, task = %task.id, "push agent reply failed");
            }
            Ok(())
        }
        Err(e) => {
            let msg = format!("[cron:{} failed] agent turn: {}", task.id, e);
            tracing::error!("{}", msg);
            let _ = pusher.push(&msg).await;
            anyhow::bail!(msg)
        }
    }
}

/// 执行一个 cron 任务：按 mode 分发。
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

    fn call(name: &str, ok: bool) -> TurnToolCall {
        TurnToolCall {
            name: name.into(),
            args: json!({}),
            ok,
        }
    }

    #[test]
    fn test_gate_rejects_empty_reply() {
        assert!(evaluate_agent_reply("   \n ", &[]).is_err());
    }

    #[test]
    fn test_gate_rejects_all_tools_failed_regression() {
        // 2026-08-31 morning_news：模型一次搜索都没做，唯一的 send_file（路径是编的）失败，
        // 然后就着错误回了句模板填充语。非空 + provider 全 200，旧代码原样推给了用户。
        let calls = vec![call("send_file", false)];
        let err = evaluate_agent_reply("No response requested.", &calls).unwrap_err();
        assert!(err.contains("all 1 tool call(s) failed"), "got: {}", err);
        assert!(err.contains("send_file"), "理由要点名失败的工具: {}", err);
    }

    #[test]
    fn test_gate_passes_when_any_tool_succeeded() {
        let calls = vec![call("web_fetch", false), call("search", true)];
        assert!(evaluate_agent_reply("今天的三条热点……", &calls).is_ok());
    }

    #[test]
    fn test_gate_passes_toolless_text_reply() {
        // 不调工具直接作答的任务合法：门里不含「文本有没有意义」的猜测式判据
        assert!(evaluate_agent_reply("今天没什么新东西，跳过。", &[]).is_ok());
    }
}
