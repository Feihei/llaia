use crate::agent::approval::{format_move_prompt, validate_move_target};
use crate::agent::Agent;
use crate::agent::AgentRegistry;
use crate::config::Config;
use crate::cron::{CronMode, CronTask};
use anyhow::Result;
use serde_json::json;
use std::sync::Arc;

pub enum SlashOutcome {
    Handled(String),
    Exit,
    NotSlash,
    /// 解决待定审批后，启动一轮 continuation turn 让模型基于结果继续。
    /// `notice` 立即呈现给用户（如工具结果摘要），`message` 作为 continuation 的用户消息喂给模型。
    Resume {
        notice: String,
        message: String,
    },
}

/// 处理斜杠命令，返回 (outcome, 输出文本)。
/// 输出文本由调用方决定如何呈现（CLI 打印，QQ 发回用户）。
/// `registry` 用于后台委派管理（/delegate-list / /delegate-cancel），无则相关命令提示缺失。
pub async fn try_handle(
    line: &str,
    agent: &mut Agent,
    registry: Option<Arc<AgentRegistry>>,
) -> Result<SlashOutcome> {
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
        "/help" => Ok(SlashOutcome::Handled(
            "commands: /new /exit /stop /compact /clear /stats /remember <text> /provider /permission [read-only|default|yolo] /ok <id> /deny <id> /move [<path>|home] (alias /cd) — 无参数或 /move home 恢复到原始 workspace /config /dream /dream-rollback /delegate-list /delegate-cancel <id> /help"
                .into(),
        )),
        "/permission" => {
            if args.is_empty() {
                let cur = agent.permission_profile.read().await.clone();
                return Ok(SlashOutcome::Handled(format!(
                    "current permission profile: {}\nusage: /permission <read-only|default|yolo>",
                    cur
                )));
            }
            let p = args.to_lowercase();
            if !matches!(p.as_str(), "read-only" | "default" | "yolo") {
                return Ok(SlashOutcome::Handled(
                    "invalid profile, use one of: read-only | default | yolo".into(),
                ));
            }
            agent.set_permission_profile(&p).await;
            Ok(SlashOutcome::Handled(format!("[permission profile set to {}]", p)))
        }
        "/ok" | "/deny" => {
            if args.is_empty() {
                return Ok(SlashOutcome::Handled(format!(
                    "usage: {} <approval-id>",
                    cmd
                )));
            }
            let approve = cmd == "/ok";
            match resolve_approval(agent, args, approve).await {
                Ok(Some((notice, message))) => {
                    Ok(SlashOutcome::Resume { notice, message })
                }
                Ok(None) => Ok(SlashOutcome::Handled(format!(
                    "[{}] 没有待确认的请求 {}",
                    cmd, args
                ))),
                Err(e) => Ok(SlashOutcome::Handled(format!("[{} failed: {}]", cmd, e))),
            }
        }
        "/move" | "/cd" => {
            let arg = args.trim();
            // 无参数 / home / ~ / - ：快速恢复到原始（家目录）workspace，无需审批
            if arg.is_empty() || arg == "home" || arg == "~" || arg == "-" {
                let home = agent.workspace.clone();
                let current = agent.workspace_root.read().await.clone();
                if current == home {
                    return Ok(SlashOutcome::Handled(format!(
                        "workspace 已是原始目录：{}",
                        home.display()
                    )));
                }
                agent.set_workspace(home.clone()).await;
                return Ok(SlashOutcome::Handled(format!(
                    "[已恢复到原始 workspace] {}（之前：{}）",
                    home.display(),
                    current.display()
                )));
            }
            match validate_move_target(args) {
                Ok(target) => {
                    let id = agent
                        .approval_gate
                        .register(
                            "__move_workspace",
                            &json!({ "path": target.to_string_lossy() }),
                            "move",
                            "cli",
                            &agent.alias,
                            false,
                        )
                        .await;
                    let prompt = format_move_prompt(&target, &id);
                    Ok(SlashOutcome::Handled(format!(
                        "[已登记切换请求] 请回复 `/ok {}` 确认切换，或 `/deny {}` 取消。{}",
                        id, id, prompt
                    )))
                }
                Err(e) => Ok(SlashOutcome::Handled(format!("[move failed: {}]", e))),
            }
        }
        "/new" => {
            agent.context.clear();
            agent.context.summary = None;
            Ok(SlashOutcome::Handled("[new session]".into()))
        }
        "/clear" => {
            agent.context.clear();
            agent.context.summary = None;
            Ok(SlashOutcome::Handled("[context cleared]".into()))
        }
        "/compact" => match agent.provider_for_compact().await {
            Some(p) => match agent.context.compact(p.as_ref(), 6, agent.context_size).await {
                Ok(_) => Ok(SlashOutcome::Handled("[compacted]".into())),
                Err(e) => Ok(SlashOutcome::Handled(format!("[compact failed: {}]", e))),
            },
            None => Ok(SlashOutcome::Handled(
                "[compact failed: provider not configured]".into(),
            )),
        },
        "/stats" => {
            let tokens = agent.context.estimate_tokens();
            let threshold_tokens = (agent.context_size as f64 * agent.context_threshold) as usize;
            let usage = if agent.context_size > 0 {
                (tokens as f64 / agent.context_size as f64 * 100.0) as u32
            } else {
                0
            };
            let summary_status = if agent.context.summary.is_some() {
                "yes"
            } else {
                "no"
            };
            let info = format!(
                "context_size: {}\ncontext_threshold: {} ({} tokens)\n\
                 current tokens (est.): {} ({}% used)\n\
                 history msgs: {}
session_id: {}
summary: {}
tools: {:?}
\
                 compact_provider: {}",
                agent.context_size,
                agent.context_threshold,
                threshold_tokens,
                tokens,
                usage,
                agent.context.history.len(),
                agent.session_id,
                summary_status,
                agent.tools.names(),
                if agent.compact_provider_snapshot().await.is_some() {
                    "configured"
                } else {
                    "fallback to main"
                },
            );
            Ok(SlashOutcome::Handled(info))
        }
        "/remember" => {
            if args.is_empty() {
                Ok(SlashOutcome::Handled("usage: /remember <text>".into()))
            } else if let Some(tool) = agent.tools.get("memory_write") {
                let _ = tool
                    .execute(&serde_json::json!({"entry": args}), "cli")
                    .await;
                Ok(SlashOutcome::Handled("[remembered]".into()))
            } else {
                Ok(SlashOutcome::Handled(
                    "[memory_write tool not registered]".into(),
                ))
            }
        }
        "/provider" => {
            if args.is_empty() {
                Ok(SlashOutcome::Handled(list_providers(agent).await))
            } else {
                match switch_provider(agent, args).await {
                    Ok(msg) => Ok(SlashOutcome::Handled(msg)),
                    Err(e) => Ok(SlashOutcome::Handled(format!("[switch failed: {}]", e))),
                }
            }
        }
        "/config" => {
            let info = format!(
                "context_threshold: {}\nmax_iterations: {}\ncontext_size: {}\nhistory msgs: {}\nsummary: {}\ntools: {:?}",
                agent.context_threshold,
                agent.max_iterations,
                agent.context_size,
                agent.context.history.len(),
                agent.context.summary.is_some(),
                agent.tools.names()
            );
            Ok(SlashOutcome::Handled(info))
        }
        "/dream" => {
            // 手动触发一次做梦：两阶段记忆整理（跳过空闲门控）。
            let task = CronTask {
                id: "dream".into(),
                schedule: "0 4 * * *".into(),
                mode: CronMode::Agent,
                channel: "cli".into(),
                enabled: true,
                prompt: None,
                steps: None,
                kind: Some("dream".into()),
                idle_minutes: Some(30),
            };
            match crate::cron::dream::run_dream(agent, &task, true).await {
                Ok(summary) => Ok(SlashOutcome::Handled(summary)),
                Err(e) => Ok(SlashOutcome::Handled(format!("[dream failed: {}]", e))),
            }
        }
        "/dream-rollback" => {
            // 回滚 MEMORY.md 到最近一份 .bak 备份。
            let memory_path = agent.workspace.join("MEMORY.md");
            let backup_dir = agent.workspace.join("MEMORY.backups");
            match crate::memory::dream::restore_memory(&memory_path, &backup_dir, None).await {
                Ok(restored) => Ok(SlashOutcome::Handled(format!(
                    "[dream rolled back to backup: {}]",
                    restored.display()
                ))),
                Err(e) => Ok(SlashOutcome::Handled(format!("[dream-rollback failed: {}]", e))),
            }
        }
        "/delegate-list" => {
            match &registry {
                Some(reg) => {
                    let tasks = reg.background_tasks.lock().unwrap();
                    if tasks.is_empty() {
                        Ok(SlashOutcome::Handled("[无后台委派任务]".into()))
                    } else {
                        let mut s = String::from("后台委派任务:\n");
                        for t in tasks.values() {
                            let secs = t.started.elapsed().as_secs();
                            s.push_str(&format!(
                                "- {} [{}] 已运行 {}s\n",
                                t.id,
                                t.agent_name,
                                secs
                            ));
                        }
                        Ok(SlashOutcome::Handled(s))
                    }
                }
                None => Ok(SlashOutcome::Handled(
                    "[delegate-list] 当前环境无 registry".into(),
                )),
            }
        }
        "/delegate-cancel" => {
            if args.is_empty() {
                Ok(SlashOutcome::Handled("usage: /delegate-cancel <id>".into()))
            } else {
                match &registry {
                    Some(reg) => {
                        let mut tasks = reg.background_tasks.lock().unwrap();
                        match tasks.remove(args) {
                            Some(t) => {
                                drop(tasks);
                                t.handle.abort();
                                Ok(SlashOutcome::Handled(format!(
                                    "[已取消后台任务 {}]",
                                    args
                                )))
                            }
                            None => Ok(SlashOutcome::Handled(format!(
                                "[delegate-cancel] 无此任务 {}",
                                args
                            ))),
                        }
                    }
                    None => Ok(SlashOutcome::Handled(
                        "[delegate-cancel] 当前环境无 registry".into(),
                    )),
                }
            }
        }
        _ => Ok(SlashOutcome::Handled(format!("[unknown command: {}]", cmd))),
    }
}

/// 解析一条待确认审批：从门控取出 pending，按批准/拒绝决定执行与否。
///
/// - 普通工具：批准则 `execute_with_events` 真正执行，拒绝则返回拒绝提示。
/// - `__move_workspace`：批准则把 agent 工作目录切到目标，拒绝则不动。
///
/// 返回 `Some((notice, message))` 时，调用方应启动一次 continuation turn，
/// 把 `message` 作为用户消息喂给模型，让其基于工具结果继续。
async fn resolve_approval(
    agent: &mut Agent,
    id: &str,
    approve: bool,
) -> Result<Option<(String, String)>> {
    let pending = agent.approval_gate.take(id).await;
    let pending = match pending {
        Some(p) => p,
        None => return Ok(None),
    };

    if pending.tool_name == "__move_workspace" {
        if approve {
            let target = validate_move_target(
                pending
                    .args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            )?;
            agent.set_workspace(target.clone()).await;
            let notice = format!("[已切换工作目录到 {}]", target.display());
            return Ok(Some((notice.clone(), notice)));
        } else {
            let notice = "[已拒绝] 切换工作目录（保持原 workspace）".to_string();
            return Ok(Some((notice.clone(), notice)));
        }
    }

    let tool = match agent.tools.get(&pending.tool_name) {
        Some(t) => t.clone(),
        None => {
            return Ok(Some((
                format!("[工具不存在: {}]", pending.tool_name),
                String::new(),
            )))
        }
    };

    let result = if approve {
        match tool
            .execute_with_events(&pending.args, &pending.channel, None)
            .await
        {
            Ok(s) => s,
            Err(e) => format!("[error: {}]", e),
        }
    } else {
        format!("用户拒绝执行：{}", pending.tool_name)
    };

    let notice = format!(
        "[{}] {}",
        if approve { "已批准" } else { "已拒绝" },
        pending.tool_name
    );
    Ok(Some((notice, result)))
}

/// 把 config 中所有 provider/model 组合 flatten 成有序 model ref 列表
/// （provider id 排序，alias 排序），同时作为 `/provider <序号>` 的索引基准。
pub fn flatten_model_refs(config: &Config) -> Vec<String> {
    let mut ids: Vec<&String> = config.provider.keys().collect();
    ids.sort();
    let mut refs = Vec::new();
    for id in ids {
        let prov = &config.provider[id];
        let mut aliases: Vec<&String> = prov.model.keys().collect();
        aliases.sort();
        for alias in aliases {
            refs.push(format!("{}.{}", id, alias));
        }
    }
    refs
}

/// `/provider`：列出所有可用模型，当前模型标 `*`
async fn list_providers(agent: &Agent) -> String {
    let refs = flatten_model_refs(&agent.config);
    if refs.is_empty() {
        return "no providers configured".into();
    }
    let current_label = match agent.provider_snapshot().await {
        Some(p) => p.label(),
        None => String::new(),
    };
    let mut out = String::from("providers:\n");
    for (i, r) in refs.iter().enumerate() {
        // refs 由 flatten 生成，格式保证合法
        let (prov_id, alias) = Config::parse_model_ref(r).unwrap_or(("", ""));
        let model_name = agent.config.provider[prov_id].model[alias].model.clone();
        let mark = if model_name == current_label {
            " *"
        } else {
            ""
        };
        out.push_str(&format!("{}. {} ({}){}\n", i + 1, r, model_name, mark));
    }
    out.push_str("usage: /provider <num> | /provider <id.alias>");
    out
}

/// `/provider <n>` 或 `/provider <id.alias>`：运行时切换，不写 config.toml
async fn switch_provider(agent: &mut Agent, arg: &str) -> Result<String> {
    let refs = flatten_model_refs(&agent.config);
    let model_ref = if let Ok(n) = arg.parse::<usize>() {
        refs.get(
            n.checked_sub(1)
                .ok_or_else(|| anyhow::anyhow!("index starts at 1"))?,
        )
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("index {} out of range (1-{})", n, refs.len()))?
    } else {
        arg.to_string()
    };
    let provider = crate::provider::provider_from_ref(&agent.config, &model_ref)?;
    agent.reload_provider(Some(provider)).await;
    tracing::info!(model = model_ref.as_str(), "provider switched at runtime");
    Ok(format!("[switched to {}]", model_ref))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::config::{AgentConfig, ModelConfig, ProviderConfig};
    use crate::memory::sqlite::SessionStore;
    use crate::provider::{ChatRequest, ChatResponse, Provider, StreamEvent};
    use async_trait::async_trait;
    use futures_util::stream::BoxStream;
    use std::sync::Arc;

    struct LabelProvider(String);

    #[async_trait]
    impl Provider for LabelProvider {
        async fn chat(&self, _req: &ChatRequest<'_>) -> Result<ChatResponse> {
            Ok(ChatResponse::default())
        }
        async fn chat_stream(&self, _req: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>> {
            Box::pin(futures_util::stream::empty())
        }
        fn native_tool_calling(&self) -> bool {
            true
        }
        fn label(&self) -> String {
            self.0.clone()
        }
    }

    fn test_config() -> Config {
        let mut config = Config::default_for_workspace("/tmp/llaia-test");
        config.provider.insert(
            "b".into(),
            ProviderConfig {
                provider_type: "openai_compatible".into(),
                base_url: "http://localhost:8080/v1".into(),
                api_key: String::new(),
                model: [(
                    "small".into(),
                    ModelConfig {
                        model: "small-model".into(),
                        native_tool_calling: true,
                        context_size: None,
                        max_tokens: None,
                    },
                )]
                .into_iter()
                .collect(),
            },
        );
        config.provider.insert(
            "a".into(),
            ProviderConfig {
                provider_type: "openai_compatible".into(),
                base_url: "http://localhost:8081/v1".into(),
                api_key: String::new(),
                model: [(
                    "big".into(),
                    ModelConfig {
                        model: "big-model".into(),
                        native_tool_calling: true,
                        context_size: None,
                        max_tokens: None,
                    },
                )]
                .into_iter()
                .collect(),
            },
        );
        config.agent.insert(
            "main".into(),
            AgentConfig {
                model: "a.big".into(),
                workspace: String::new(),
                soul: None,
                user: None,
                memory: None,
                denied_tools: vec![],
                delegate_timeout: 120,
                fallback: vec![],
            },
        );
        config
    }

    async fn test_agent(config: Config) -> Agent {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("test", "test").unwrap();
        Agent::new(
            &config,
            Some(Arc::new(LabelProvider("big-model".into()))),
            None,
            None,
            Arc::new(crate::agent::runner::ToolRegistry::new()),
            Arc::new(store),
            sid,
            "sys".into(),
            8192,
            "/tmp/llaia-test/workspace".into(),
            std::sync::Arc::new(tokio::sync::RwLock::new(std::path::PathBuf::from(
                "/tmp/llaia-test/workspace",
            ))),
            "/tmp/llaia-test".into(),
            true,
            "main".into(),
            None,
        )
        .await
    }

    #[test]
    fn test_flatten_model_refs_sorted() {
        // default_for_workspace 自带 default.qwen，加上测试插入的 a/b
        let refs = flatten_model_refs(&test_config());
        assert_eq!(refs, vec!["a.big", "b.small", "default.qwen"]);
    }

    #[tokio::test]
    async fn test_provider_list_marks_current() {
        let agent = test_agent(test_config()).await;
        let out = list_providers(&agent).await;
        assert!(out.contains("1. a.big (big-model) *"));
        assert!(out.contains("2. b.small (small-model)"));
        assert!(!out.contains("2. b.small (small-model) *"));
    }

    #[tokio::test]
    async fn test_provider_switch_by_index_and_ref() {
        let mut agent = test_agent(test_config()).await;

        // 按序号切换
        let msg = switch_provider(&mut agent, "2").await.unwrap();
        assert_eq!(msg, "[switched to b.small]");
        let p = agent.provider_snapshot().await.unwrap();
        assert_eq!(p.label(), "small-model");

        // 按 ref 切换
        let msg = switch_provider(&mut agent, "a.big").await.unwrap();
        assert_eq!(msg, "[switched to a.big]");
        let p = agent.provider_snapshot().await.unwrap();
        assert_eq!(p.label(), "big-model");
    }

    #[tokio::test]
    async fn test_provider_switch_invalid() {
        let mut agent = test_agent(test_config()).await;
        assert!(switch_provider(&mut agent, "9").await.is_err());
        assert!(switch_provider(&mut agent, "0").await.is_err());
        assert!(switch_provider(&mut agent, "nope.missing").await.is_err());
        // 切换失败不动现有 provider
        let p = agent.provider_snapshot().await.unwrap();
        assert_eq!(p.label(), "big-model");
    }
}
