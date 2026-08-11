use crate::agent::Agent;
use crate::config::Config;
use anyhow::Result;

pub enum SlashOutcome {
    Handled(String),
    Exit,
    NotSlash,
}

/// 处理斜杠命令，返回 (outcome, 输出文本)。
/// 输出文本由调用方决定如何呈现（CLI 打印，QQ 发回用户）。
pub async fn try_handle(line: &str, agent: &mut Agent) -> Result<SlashOutcome> {
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
            "commands: /new /exit /stop /compact /clear /stats /remember <text> /provider /config /help"
                .into(),
        )),
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
            Some(p) => match agent.context.compact(p.as_ref(), 6).await {
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
        _ => Ok(SlashOutcome::Handled(format!("[unknown command: {}]", cmd))),
    }
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
