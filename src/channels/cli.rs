use crate::agent::runner::ToolRegistry;
use crate::agent::Agent;
use crate::agent::TurnEvent;
use crate::channels::Channel;
use crate::commands::slash::{try_handle, SlashOutcome};
use crate::config::Config;
use crate::memory::sqlite::SessionStore;
use crate::memory::{ensure_template, load_md, MEMORY_TEMPLATE, SOUL_TEMPLATE, USER_TEMPLATE};
use crate::provider::openai_compat::OpenAiCompatibleProvider;
use crate::provider::Provider;
use crate::tool_call::build_tool_instructions;
use crate::tools::file::{FileEdit, FileRead, FileWrite};
use crate::tools::memory::MemoryWrite;
use crate::tools::tavily::TavilySearch;
use crate::tools::terminal::Terminal;
use crate::tools::web::WebFetch;
use anyhow::Result;
use async_trait::async_trait;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct CliChannel;

impl CliChannel {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Channel for CliChannel {
    async fn run(self: Arc<Self>, agent: Arc<Mutex<Agent>>) -> Result<()> {
        println!("laia v0.1.5 - type /help for commands, /exit to quit\n");
        let stdin = std::io::stdin();
        loop {
            print!("> ");
            std::io::stdout().flush()?;
            let mut line = String::new();
            if stdin.lock().read_line(&mut line)? == 0 {
                break;
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // 用独立块限定 MutexGuard 生命周期，避免其延续到 match arm 内导致死锁
            let outcome = {
                let mut a = agent.lock().await;
                try_handle(line, &mut *a).await?
            };
            match outcome {
                SlashOutcome::Exit => break,
                SlashOutcome::Handled(msg) => {
                    println!("{}", msg);
                    continue;
                }
                SlashOutcome::NotSlash => {
                    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
                    let agent_clone = agent.clone();
                    let input_clone = line.to_string();
                    tokio::spawn(async move {
                        let mut a = agent_clone.lock().await;
                        if let Err(e) = a
                            .handle_input_streaming(&input_clone, "cli", tx)
                            .await
                        {
                            tracing::error!(error = %e, "handle_input_streaming failed");
                        }
                    });
                    println!();  // 换行，分隔 prompt 和回复
                    while let Some(ev) = rx.recv().await {
                        match ev {
                            TurnEvent::Chunk { delta } => {
                                print!("{}", delta);
                                std::io::stdout().flush().ok();
                            }
                            TurnEvent::ToolStart { name, .. } => {
                                println!("\n[tool: {}]", name);
                            }
                            TurnEvent::ToolResult { output, .. } => {
                                let preview = if output.len() > 200 {
                                    format!("{}...(truncated)", &output[..200])
                                } else {
                                    output
                                };
                                println!("[result: {}]", preview);
                            }
                            TurnEvent::Done => {
                                println!("\n");
                            }
                            TurnEvent::Error { message } => {
                                println!("\n[error: {}]\n", message);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// 构建 Agent 实例（CLI 和 QQ 共用）
pub async fn build_agent(config: &Config) -> Result<Arc<Mutex<Agent>>> {
    let agent_cfg = config
        .agent
        .get("main")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("agent.main not configured"))?;

    let workspace = PathBuf::from(&agent_cfg.workspace);
    std::fs::create_dir_all(&workspace).ok();

    let soul_path = resolve_md_path(&agent_cfg.soul, &workspace, "SOUL.md");
    let user_path = resolve_md_path(&agent_cfg.user, &workspace, "USER.md");
    let memory_path = resolve_md_path(&agent_cfg.memory, &workspace, "MEMORY.md");
    ensure_template(&soul_path, SOUL_TEMPLATE).await?;
    ensure_template(&user_path, USER_TEMPLATE).await?;
    ensure_template(&memory_path, MEMORY_TEMPLATE).await?;

    let soul = load_md(&soul_path).await?;
    let user = load_md(&user_path).await?;
    let memory = load_md(&memory_path).await?;

    let (prov_id, model_alias) = Config::parse_model_ref(&agent_cfg.model)?;
    let prov_cfg = config
        .provider
        .get(prov_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("provider.{} not configured", prov_id))?;
    let model_cfg = prov_cfg
        .model
        .get(model_alias)
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!("provider.{}.model.{} not configured", prov_id, model_alias)
        })?;

    let provider: Arc<dyn Provider> = Arc::new(OpenAiCompatibleProvider::new(
        &prov_cfg.base_url,
        &prov_cfg.api_key,
        &model_cfg.model,
        model_cfg.native_tool_calling,
    )?);

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(FileRead::new(workspace.clone())));
    registry.register(Arc::new(FileWrite::new(workspace.clone())));
    registry.register(Arc::new(FileEdit::new(workspace.clone())));
    registry.register(Arc::new(Terminal::new(
        config.tools.terminal.confirm.clone(),
        config.tools.terminal.whitelist.clone(),
        workspace.clone(),
    )));
    registry.register(Arc::new(WebFetch::new()?));
    if !config.tools.tavily.api_key.is_empty() {
        registry.register(Arc::new(TavilySearch::new(
            config.tools.tavily.api_key.clone(),
        )?));
    }
    registry.register(Arc::new(MemoryWrite::new(memory_path.clone())));

    let mut system_prompt = format!(
        "# SOUL\n{}\n\n# USER\n{}\n\n# MEMORY\n{}\n\n# WORKSPACE\n{}\n\n工作目录说明：所有工具的相对路径都相对于 WORKSPACE 解析；terminal 命令在 WORKSPACE 下执行。需要写到其它位置时请使用绝对路径。",
        soul, user, memory, workspace.display()
    );
    if !provider.native_tool_calling() {
        system_prompt.push_str(&build_tool_instructions(&registry.specs()));
    }
    let registry = Arc::new(registry);

    let db_path = workspace.join("sessions.db");
    let session_store = Arc::new(SessionStore::open(&db_path)?);

    let session_id = match session_store.latest_session()? {
        Some((id, _)) => id,
        None => {
            let uuid = uuid::Uuid::new_v4().to_string();
            session_store.create_session(&uuid, "cli")?
        }
    };

    let agent = Agent::new(
        config,
        provider,
        registry,
        session_store,
        session_id,
        system_prompt,
        model_cfg.context_size,
    )
    .await;

    Ok(Arc::new(Mutex::new(agent)))
}

fn resolve_md_path(explicit: &Option<String>, workspace: &PathBuf, default_name: &str) -> PathBuf {
    match explicit {
        Some(s) => {
            let p = PathBuf::from(s);
            if p.is_absolute() {
                p
            } else {
                workspace.join(s)
            }
        }
        None => workspace.join(default_name),
    }
}
