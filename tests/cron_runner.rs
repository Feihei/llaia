use llaia::cron::runner::substitute_placeholders;
use llaia::cron::{CronMode, CronTask, ProactivePusher, Step};
use serde_json::json;
use std::sync::Arc;

/// 测试用 pusher：记录收到的消息
struct MockPusher {
    messages: tokio::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ProactivePusher for MockPusher {
    async fn push(&self, message: &str) -> anyhow::Result<()> {
        self.messages.lock().await.push(message.to_string());
        Ok(())
    }
}

#[tokio::test]
async fn test_substitute_placeholders_prev_and_now() {
    let args = json!({ "text": "prev={{prev}} now={{now}}" });
    let out = substitute_placeholders(&args, "hello", "2026-08-06T08:00:00Z");
    assert_eq!(out["text"], "prev=hello now=2026-08-06T08:00:00Z");
}

#[tokio::test]
async fn test_substitute_placeholders_no_match() {
    let args = json!({ "q": "no placeholder here" });
    let now = "2026-01-01T00:00:00Z";
    let out = substitute_placeholders(&args, "prev", now);
    assert_eq!(out["q"], "no placeholder here");
}

#[tokio::test]
async fn test_substitute_placeholders_nested() {
    let args = json!({
        "outer": "prev={{prev}}",
        "list": ["a", "{{now}}", {"inner": "{{prev}}"}]
    });
    let out = substitute_placeholders(&args, "P", "N");
    assert_eq!(out["outer"], "prev=P");
    assert_eq!(out["list"][0], "a");
    assert_eq!(out["list"][1], "N");
    assert_eq!(out["list"][2]["inner"], "P");
}

#[tokio::test]
async fn test_substitute_placeholders_non_string_unchanged() {
    let args = json!({ "count": 42, "flag": true, "null_val": null });
    let out = substitute_placeholders(&args, "prev", "now");
    assert_eq!(out["count"], json!(42));
    assert_eq!(out["flag"], json!(true));
    assert_eq!(out["null_val"], json!(null));
}

#[tokio::test]
async fn test_run_tools_mode_signature_compiles() {
    // 构造一个 cron task：tools 模式，两步。
    // 验证 CronTask / Step 结构可构造（接口存在）。
    // 完整执行流程（含 ToolRegistry）由集成测试覆盖。
    let task = CronTask {
        id: "test".into(),
        schedule: "0 0 * * *".into(),
        mode: CronMode::Tools,
        channel: "web".into(),
        enabled: true,
        prompt: None,
        steps: Some(vec![
            Step {
                tool: "tavily_search".into(),
                args: json!({"query": "test"}),
            },
            Step {
                tool: "memory_write".into(),
                args: json!({"text": "done {{now}}"}),
            },
        ]),
    };
    let pusher = Arc::new(MockPusher {
        messages: tokio::sync::Mutex::new(vec![]),
    });
    let _ = &task;
    let _ = pusher;
}

#[tokio::test]
async fn test_run_agent_mode_signature_compiles() {
    let task = CronTask {
        id: "agent_test".into(),
        schedule: "0 8 * * *".into(),
        mode: CronMode::Agent,
        channel: "qq".into(),
        enabled: true,
        prompt: Some("hello".into()),
        steps: None,
    };
    let _pusher = Arc::new(MockPusher {
        messages: tokio::sync::Mutex::new(vec![]),
    });
    let _ = &task;
}
