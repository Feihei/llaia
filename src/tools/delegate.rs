use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::OnceCell;

use crate::agent::registry::BackgroundTask;
use crate::agent::Agent;
use crate::agent::AgentRegistry;
use crate::agent::TurnEvent;
use crate::cron::ProactivePusher;
use crate::tools::Tool;

/// 后台委派完成后的结果投递目标。
/// - `Pusher`：serve channel（qq/web/mail 等已实现 ProactivePusher），结果推回发起委派的 channel。
/// - `Stdout`：CLI 模式，结果直接打印到终端。
#[derive(Clone)]
pub enum DeliveryTarget {
    Pusher(Arc<dyn ProactivePusher>),
    Stdout,
}

impl DeliveryTarget {
    async fn push(&self, message: &str) {
        match self {
            DeliveryTarget::Pusher(p) => {
                if let Err(e) = p.push(message).await {
                    tracing::error!(error = %e, "delivery push failed");
                }
            }
            DeliveryTarget::Stdout => {
                println!("{}", message);
            }
        }
    }
}

pub struct DelegateTool {
    registry: OnceCell<Arc<AgentRegistry>>,
    timeout_secs: u64,
}

impl DelegateTool {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            registry: OnceCell::new(),
            timeout_secs,
        }
    }

    pub fn set_registry(&self, registry: Arc<AgentRegistry>) {
        let _ = self.registry.set(registry);
    }

    fn get_registry(&self) -> Option<&Arc<AgentRegistry>> {
        self.registry.get()
    }
}

#[async_trait]
impl Tool for DelegateTool {
    fn name(&self) -> &str {
        "delegate"
    }

    fn description(&self) -> &str {
        "Delegate a task to a sub-agent for execution. A sub-agent has its own specialized capabilities and toolset. Suitable for tasks that require specific expertise."
    }

    fn parameters_schema(&self) -> Value {
        let agents: Vec<String> = self
            .get_registry()
            .map(|r| r.available_sub_agents())
            .unwrap_or_default();
        json!({
            "type": "object",
            "properties": {
                "agent_name": {
                    "type": "string",
                    "description": "Name of the sub-agent to delegate to",
                    "enum": agents
                },
                "task": {
                    "type": "string",
                    "description": "Description of the task to delegate to the sub-agent"
                },
                "file_paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of file paths to pass to the sub-agent (relative paths within the main agent workspace); they will be copied to the sub-agent's .inbox/",
                },
                "async": {
                    "type": "boolean",
                    "description": "Whether to run asynchronously in the background. true = return immediately and push the result when done (user can keep chatting); false = block until the sub-agent finishes (default)",
                    "default": false
                }
            },
            "required": ["agent_name", "task"]
        })
    }

    fn requires_confirm(&self) -> bool {
        false
    }

    async fn execute(&self, args: &Value, channel: &str) -> Result<String> {
        self.execute_with_events(args, channel, None).await
    }

    async fn execute_with_events(
        &self,
        args: &Value,
        _channel: &str,
        event_tx: Option<&mpsc::Sender<TurnEvent>>,
    ) -> Result<String> {
        let registry = match self.get_registry() {
            Some(r) => r.clone(),
            None => return Ok("[delegation failed: registry not initialized]".into()),
        };

        let agent_name = args["agent_name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing agent_name"))?;
        let task = args["task"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing task"))?;
        let is_async = args["async"].as_bool().unwrap_or(false);
        let file_paths: Vec<String> = args["file_paths"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let sub_agent = match registry.get(agent_name) {
            Ok(a) => a.clone(),
            Err(e) => return Ok(format!("[delegation failed: {}]", e)),
        };

        // 获取主 agent 和子 agent 的 workspace
        // main_workspace 从 registry 缓存读取，避免在 main agent 持有锁的调用链中再次 lock main 导致死锁
        // （tokio::sync::Mutex 不可重入：handle_message_streaming 已持 main 锁 → execute_tool_calls → delegate.execute_with_events）
        let main_workspace = registry.main_workspace.clone();
        let sub_workspace = sub_agent.lock().await.workspace.clone();

        // .inbox 机制：清空后复制主 agent 指定文件到子 agent .inbox/
        let inbox_dir = sub_workspace.join(".inbox");
        if !file_paths.is_empty() {
            // 清空 .inbox
            if inbox_dir.exists() {
                tokio::fs::remove_dir_all(&inbox_dir).await.ok();
            }
            tokio::fs::create_dir_all(&inbox_dir).await?;

            // 复制文件
            for fp in &file_paths {
                let src = match crate::path_guard::validate_path(&main_workspace, fp, None) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(file = %fp, error = %e, "skip file outside workspace");
                        continue;
                    }
                };
                if !src.exists() {
                    tracing::warn!(file = %fp, "source file not exist, skip");
                    continue;
                }
                let filename = src.file_name().unwrap_or_default();
                let dst = inbox_dir.join(filename);
                tokio::fs::copy(&src, &dst).await?;
                tracing::info!(file = %fp, dst = %dst.display(), "copied to subagent .inbox");
            }
        }

        // task 文本追加 .inbox 提示
        let full_task = if file_paths.is_empty() {
            task.to_string()
        } else {
            format!(
                "{}\n\n[input files placed in .inbox/: {}]",
                task,
                file_paths.join(", ")
            )
        };

        // ── 异步分支：后台 spawn，立即返回 ack，完成由 delivery 主动推送 ──
        if is_async {
            // 并发上限（每会话 3）
            {
                let tasks = registry.background_tasks.lock().unwrap();
                if tasks.len() >= 3 {
                    return Ok(
                        "[delegation failed: background delegation limit reached (3)]".into(),
                    );
                }
            }
            let id = uuid::Uuid::new_v4().to_string();
            let delivery = registry.clone_delivery();
            let child = sub_agent.clone();
            let task_txt = full_task.clone();
            let to = self.timeout_secs;
            let sub_name = agent_name.to_string();
            let bg = registry.clone();
            let task_id = id.clone();
            let handle = tokio::spawn(async move {
                let return_value = run_child(child, task_txt, to, None).await;
                let msg = format!(
                    "[sub-agent {} completed]\n{}",
                    sub_name,
                    format_delivery_text(&return_value)
                );
                match delivery {
                    Some(d) => d.push(&msg).await,
                    None => {
                        tracing::warn!(id = %task_id, "async delegate finished but no delivery target");
                    }
                }
                bg.background_tasks.lock().unwrap().remove(&task_id);
            });
            registry.background_tasks.lock().unwrap().insert(
                id.clone(),
                BackgroundTask {
                    id: id.clone(),
                    agent_name: agent_name.to_string(),
                    started: std::time::Instant::now(),
                    handle,
                },
            );
            return Ok(format!(
                "Started sub-agent {name} in the background (task {id}); you will be notified when it completes. Use /delegate-list to view and /delegate-cancel to cancel.",
                name = agent_name,
                id = id
            ));
        }

        // ── 同步分支（默认）：阻塞到子 Agent 完成，转发 Chunk 进度 ──
        let return_value =
            run_child(sub_agent.clone(), full_task, self.timeout_secs, event_tx).await;
        Ok(return_value)
    }
}

/// 跑一轮子 Agent，收集输出与产出文件，返回与同步分支一致的 result 字符串。
/// `forward` 为 `Some` 时把子 Agent 的 Chunk 事件转发给主 channel（同步分支用）；
/// 异步分支传 `None`，仅最终通过 delivery 推送。
async fn run_child(
    sub_agent: Arc<tokio::sync::Mutex<Agent>>,
    full_task: String,
    timeout: u64,
    forward: Option<&mpsc::Sender<TurnEvent>>,
) -> String {
    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let task_clone = full_task.clone();
    // 并发 drain + 转发：必须边跑边消费。若等 turn 结束再收，子 agent 输出超 channel
    // 容量（64）时 send 永久阻塞，整个委派冻结到 timeout（与 handle_input 同款死锁）。
    let forward_cloned = forward.cloned();
    let drain = tokio::spawn(async move {
        let mut output = String::new();
        while let Some(ev) = rx.recv().await {
            if let TurnEvent::Chunk { delta } = ev {
                output.push_str(&delta);
                if let Some(tx) = &forward_cloned {
                    let _ = tx.send(TurnEvent::Chunk { delta }).await;
                }
            }
        }
        output
    });
    // 子 agent 用独立 channel 标识 "delegate"，不继承主 agent 的 channel。
    let result = tokio::time::timeout(Duration::from_secs(timeout), async {
        sub_agent
            .lock()
            .await
            .handle_input_streaming(&task_clone, "delegate", tx)
            .await
    })
    .await;

    // drain 任务随 tx 掉落自然结束；超时分支下先 abort 再取回已累积部分
    let output = match drain.await {
        Ok(o) => o,
        Err(e) if e.is_cancelled() => String::new(),
        Err(_) => String::new(),
    };

    // 从子 agent 本次 turn 的工具调用记录提取产出文件清单
    let output_files: Vec<String> = {
        let sub_a = sub_agent.lock().await;
        sub_a
            .turn_tool_calls
            .iter()
            .filter(|tc| tc.name == "file_write" || tc.name == "file_edit")
            .filter_map(|tc| {
                tc.args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect()
    };

    match result {
        Ok(Ok(_)) => {
            if output.is_empty() && output_files.is_empty() {
                "[sub-agent produced no output]".to_string()
            } else {
                serde_json::json!({
                    "text": output,
                    "output_files": output_files,
                })
                .to_string()
            }
        }
        Ok(Err(e)) => serde_json::json!({
            "text": format!("[sub-agent execution error: {}]", e),
            "output_files": output_files,
        })
        .to_string(),
        Err(_) => serde_json::json!({
            "text": format!("[sub-agent timed out ({}s)]", timeout),
            "output_files": output_files,
        })
        .to_string(),
    }
}

/// 把子 Agent 的 result 字符串整理为面向用户的最终消息（去掉 JSON 外壳，附产出文件）。
fn format_delivery_text(return_value: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(return_value) {
        if let Some(text) = v.get("text").and_then(|t| t.as_str()) {
            let mut s = text.to_string();
            if let Some(files) = v.get("output_files").and_then(|f| f.as_array()) {
                let names: Vec<String> = files
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect();
                if !names.is_empty() {
                    s.push_str(&format!("\n\nOutput files: {}", names.join(", ")));
                }
            }
            return s;
        }
    }
    return_value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::runner::ToolRegistry;
    use crate::agent::Agent;
    use crate::config::Config;
    use crate::memory::sqlite::SessionStore;
    use crate::provider::{ChatRequest, ChatResponse, Provider, StreamEvent, ToolCall};
    use async_stream::try_stream;
    use async_trait::async_trait;
    use futures_util::stream::BoxStream;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Mutex;

    /// 不做任何响应的 provider（chat_stream 会阻塞）
    struct HangingProvider;

    #[async_trait]
    impl Provider for HangingProvider {
        async fn chat(&self, _: &ChatRequest<'_>) -> Result<ChatResponse> {
            unreachable!()
        }
        async fn chat_stream(&self, _: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>> {
            // 先吐一个 chunk 再挂起，模拟部分输出 + 超时
            let s = try_stream! {
                yield StreamEvent::TextDelta("部分输出".into());
                // 永不结束：sleep 远超测试超时
                tokio::time::sleep(Duration::from_secs(60)).await;
                yield StreamEvent::Done;
            };
            Box::pin(s)
        }
        fn native_tool_calling(&self) -> bool {
            true
        }
    }

    /// 按脚本返回事件的 provider（每次 chat_stream 返回同一组事件）
    struct ScriptedProvider {
        native: bool,
        events: Vec<StreamEvent>,
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        async fn chat(&self, _: &ChatRequest<'_>) -> Result<ChatResponse> {
            unreachable!()
        }
        async fn chat_stream(&self, _: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>> {
            let events = self.events.clone();
            let s = try_stream! {
                for ev in events {
                    yield ev;
                }
            };
            Box::pin(s)
        }
        fn native_tool_calling(&self) -> bool {
            self.native
        }
    }

    async fn make_registry_with_sub(sub_alias: &str) -> Arc<AgentRegistry> {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("sub", "test").unwrap();
        let config = Config::default_for_workspace("/tmp/llaia-test");
        let sub_workspace =
            std::path::PathBuf::from("/tmp/llaia-test/workspace/subagent").join(sub_alias);
        let agent = Agent::new(
            &config,
            Some(Arc::new(HangingProvider)),
            None,
            None,
            Arc::new(ToolRegistry::new()),
            Arc::new(store),
            sid,
            "sub soul".into(),
            8192,
            sub_workspace.clone(),
            Arc::new(tokio::sync::RwLock::new(sub_workspace.clone())),
            std::path::PathBuf::from("/tmp/llaia-test"),
            false,
            sub_alias.into(),
            None,
        )
        .await;
        let registry = AgentRegistry::new(Arc::new(Mutex::new(agent)), sub_workspace);
        // 把同一个 agent 也注册为子 agent（测试用，避免构造两个）
        let dummy = registry.main.clone();
        registry.register_sub_agent(sub_alias.into(), dummy);
        Arc::new(registry)
    }

    /// 未知子 Agent 名：返回委派失败消息
    #[tokio::test]
    async fn test_unknown_sub_agent() {
        let registry = make_registry_with_sub("coder").await;
        let tool = DelegateTool::new(120);
        tool.set_registry(registry);

        let args = json!({"agent_name": "nonexistent", "task": "test"});
        let result = tool.execute(&args, "cli").await.unwrap();
        assert!(result.contains("delegation failed"), "got: {}", result);
        assert!(result.contains("nonexistent"), "got: {}", result);
    }

    /// 超时：返回 JSON 含超时提示，部分输出通过 Chunk 事件转发
    #[tokio::test]
    async fn test_timeout_preserves_partial_output() {
        let registry = make_registry_with_sub("slow").await;
        // 超时设为 1 秒（HangingProvider 会先吐 "部分输出" 再 sleep 60s）
        let tool = DelegateTool::new(1);
        tool.set_registry(registry);

        let args = json!({"agent_name": "slow", "task": "慢任务"});
        let (tx, mut rx) = mpsc::channel(64);
        let result = tool
            .execute_with_events(&args, "cli", Some(&tx))
            .await
            .unwrap();
        assert!(
            result.contains("timed out"),
            "should mention timeout, got: {}",
            result
        );
        // 新格式：返回值为 JSON {text, output_files}，text 不再含部分输出
        // 部分输出通过 Chunk 事件转发给主 channel
        let mut chunks = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let TurnEvent::Chunk { delta } = ev {
                chunks.push(delta);
            }
        }
        assert!(
            chunks.concat().contains("部分输出"),
            "partial output should be forwarded via Chunk events, got: {:?}",
            chunks
        );
    }

    /// registry 未初始化：返回委派失败
    #[tokio::test]
    async fn test_registry_not_initialized() {
        let tool = DelegateTool::new(120);
        // 不调 set_registry
        let args = json!({"agent_name": "any", "task": "test"});
        let result = tool.execute(&args, "cli").await.unwrap();
        assert!(result.contains("delegation failed"), "got: {}", result);
        assert!(result.contains("registry"), "got: {}", result);
    }

    /// 参数缺失：返回错误
    #[tokio::test]
    async fn test_missing_args() {
        let registry = make_registry_with_sub("coder").await;
        let tool = DelegateTool::new(120);
        tool.set_registry(registry);

        // 缺 task
        let args = json!({"agent_name": "coder"});
        let result = tool.execute(&args, "cli").await;
        assert!(result.is_err(), "missing task should error");
    }

    /// 回归测试：从 "qq" channel 调 delegate，子 agent 调 requires_confirm=true 的工具应能执行。
    /// 防止 channel 透传导致子 agent 被 QQ confirm_mode 拦截的 bug 再现。
    #[tokio::test]
    async fn test_sub_agent_not_blocked_by_qq_confirm() {
        // 子 agent 挂一个 requires_confirm=true 的工具
        let called = Arc::new(AtomicBool::new(false));

        struct ConfirmRequiredTool {
            called: Arc<AtomicBool>,
        }
        #[async_trait]
        impl Tool for ConfirmRequiredTool {
            fn name(&self) -> &str {
                "dangerous"
            }
            fn description(&self) -> &str {
                "dangerous"
            }
            fn parameters_schema(&self) -> Value {
                json!({"type":"object"})
            }
            fn requires_confirm(&self) -> bool {
                true
            }
            async fn execute(&self, _args: &Value, _channel: &str) -> Result<String> {
                self.called.store(true, Ordering::SeqCst);
                Ok("executed".into())
            }
        }

        // 子 agent 的 provider：第一轮调 dangerous 工具，第二轮返回空（结束）
        let provider = Arc::new(ScriptedProvider {
            native: true,
            events: vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "c1".into(),
                    name: "dangerous".into(),
                    arguments: json!({}),
                }),
                StreamEvent::Done,
            ],
        });
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("sub", "test").unwrap();
        let tools = ToolRegistry::new();
        tools.register(Arc::new(ConfirmRequiredTool {
            called: called.clone(),
        }));
        // config 默认 qq.confirm_mode = "none"（P3-a 起），子 agent channel 透传为 "delegate"
        let config = Config::default_for_workspace("/tmp/llaia-test");
        let sub_workspace = std::path::PathBuf::from("/tmp/llaia-test/workspace/subagent/coder");
        let sub_agent = Agent::new(
            &config,
            Some(provider),
            None,
            None,
            Arc::new(tools),
            Arc::new(store),
            sid,
            "sub".into(),
            8192,
            sub_workspace.clone(),
            Arc::new(tokio::sync::RwLock::new(sub_workspace.clone())),
            std::path::PathBuf::from("/tmp/llaia-test"),
            false,
            "coder".into(),
            None,
        )
        .await;

        let registry = AgentRegistry::new(Arc::new(Mutex::new(sub_agent)), sub_workspace);
        let sub = registry.main.clone();
        registry.register_sub_agent("coder".into(), sub);
        let registry = Arc::new(registry);

        let tool = DelegateTool::new(120);
        tool.set_registry(registry);

        // 从 "qq" channel 调 delegate
        let args = json!({"agent_name": "coder", "task": "do it"});
        let _ = tool.execute(&args, "qq").await.unwrap();

        assert!(
            called.load(Ordering::SeqCst),
            "子 agent 的 requires_confirm 工具应被执行（channel='delegate' 不受 QQ confirm 拦截）"
        );
    }
}
