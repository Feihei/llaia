use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::OnceCell;

use crate::agent::AgentRegistry;
use crate::agent::TurnEvent;
use crate::tools::Tool;

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
        "委派任务给子 Agent 执行。子 Agent 有独立的专业能力和工具集。适用于需要特定专业技能的任务。"
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
                    "description": "要委派的子 Agent 名称",
                    "enum": agents
                },
                "task": {
                    "type": "string",
                    "description": "要委派给子 Agent 执行的任务描述"
                },
                "file_paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "要传递给子 Agent 的文件路径列表（主 agent workspace 内的相对路径），系统会复制到子 agent .inbox/"
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
            None => return Ok("[委派失败: registry 未初始化]".into()),
        };

        let agent_name = args["agent_name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing agent_name"))?;
        let task = args["task"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing task"))?;
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
            Err(e) => return Ok(format!("[委派失败: {}]", e)),
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
                "{}\n\n[输入文件已放在 .inbox/: {}]",
                task,
                file_paths.join(", ")
            )
        };

        let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
        let task_clone = full_task.clone();
        let timeout = self.timeout_secs;

        // 子 agent 用独立 channel 标识 "delegate"，不继承主 agent 的 channel。
        let result = tokio::time::timeout(Duration::from_secs(timeout), async {
            sub_agent
                .lock()
                .await
                .handle_input_streaming(&task_clone, "delegate", tx)
                .await
        })
        .await;

        // 收集子 Agent 的事件：Chunk 转发给主 channel（让用户看到委派进度），同时累积输出
        let mut output = String::new();
        while let Ok(ev) = rx.try_recv() {
            // 其他事件（ToolStart/ToolResult/Done/Error）不转发，避免主 channel 噪音
            if let TurnEvent::Chunk { delta } = ev {
                output.push_str(&delta);
                if let Some(tx) = event_tx {
                    let _ = tx.send(TurnEvent::Chunk { delta }).await;
                }
            }
        }

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

        let return_value = match result {
            Ok(Ok(_)) => {
                if output.is_empty() && output_files.is_empty() {
                    "[子 Agent 无输出]".to_string()
                } else {
                    serde_json::json!({
                        "text": output,
                        "output_files": output_files,
                    })
                    .to_string()
                }
            }
            Ok(Err(e)) => serde_json::json!({
                "text": format!("[子 Agent 执行错误: {}]", e),
                "output_files": output_files,
            })
            .to_string(),
            Err(_) => serde_json::json!({
                "text": format!("[子 Agent 超时({}秒)]", timeout),
                "output_files": output_files,
            })
            .to_string(),
        };

        Ok(return_value)
    }
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
        let mut registry = AgentRegistry::new(Arc::new(Mutex::new(agent)), sub_workspace);
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
        assert!(result.contains("委派失败"), "got: {}", result);
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
            result.contains("超时"),
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
        assert!(result.contains("委派失败"), "got: {}", result);
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
        let mut tools = ToolRegistry::new();
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

        let mut registry = AgentRegistry::new(Arc::new(Mutex::new(sub_agent)), sub_workspace);
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
