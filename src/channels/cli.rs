use crate::agent::runner::ToolRegistry;
use crate::agent::Agent;
use crate::commands::slash::{try_handle, SlashOutcome};
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
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

pub async fn run_repl() -> Result<()> {
    let config = crate::commands::load_config_or_init()?;

    let log_dir = PathBuf::from(&config.log.dir);
    let _ = crate::log::init(&config.log.level, &log_dir);

    let workspace = PathBuf::from(&config.workspace.dir);
    std::fs::create_dir_all(&workspace).ok();
    let agent_cfg = config
        .agent
        .get("main")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("agent.main not configured"))?;
    let soul_path = PathBuf::from(&agent_cfg.soul);
    let user_path = PathBuf::from(&agent_cfg.user);
    let memory_path = PathBuf::from(&agent_cfg.memory);
    ensure_template(&soul_path, SOUL_TEMPLATE).await?;
    ensure_template(&user_path, USER_TEMPLATE).await?;
    ensure_template(&memory_path, MEMORY_TEMPLATE).await?;

    let soul = load_md(&soul_path).await?;
    let user = load_md(&user_path).await?;
    let memory = load_md(&memory_path).await?;

    let prov_cfg = config
        .provider
        .get("default")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("provider.default not configured"))?;
    let provider: Arc<dyn Provider> = Arc::new(OpenAiCompatibleProvider::new(
        &prov_cfg.base_url,
        &prov_cfg.api_key,
        &prov_cfg.model,
        prov_cfg.native_tool_calling,
    )?);

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(FileRead));
    registry.register(Arc::new(FileWrite));
    registry.register(Arc::new(FileEdit));
    registry.register(Arc::new(Terminal::new(
        config.tools.terminal.confirm.clone(),
        config.tools.terminal.whitelist.clone(),
    )));
    registry.register(Arc::new(WebFetch::new()?));
    if !config.tools.tavily.api_key.is_empty() {
        registry.register(Arc::new(TavilySearch::new(
            config.tools.tavily.api_key.clone(),
        )?));
    }
    registry.register(Arc::new(MemoryWrite::new(memory_path.clone())));

    // 拼 system prompt：标签降级模式下注入工具协议说明
    let mut system_prompt = format!(
        "# SOUL\n{}\n\n# USER\n{}\n\n# MEMORY\n{}",
        soul, user, memory
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

    let mut agent = Agent::new(
        &config,
        provider,
        registry,
        session_store,
        session_id,
        system_prompt,
        8192,
    )
    .await;

    println!("laia v0.1.0 - type /help for commands, /exit to quit\n");
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

        match try_handle(line, &mut agent).await? {
            SlashOutcome::Exit => break,
            SlashOutcome::Handled => continue,
            SlashOutcome::NotSlash => match agent.handle_input(line, "cli").await {
                Ok(resp) => println!("\n{}\n", resp),
                Err(e) => println!("\n[error: {}]\n", e),
            },
        }
    }
    Ok(())
}
