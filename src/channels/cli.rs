use crate::agent::runner::ToolRegistry;
use crate::agent::Agent;
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
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

pub async fn run_repl() -> Result<()> {
    let config = crate::commands::load_config_or_init()?;

    let log_dir = PathBuf::from(&config.log.dir);
    let _ = crate::log::init(&config.log.level, &log_dir);

    let agent_cfg = config
        .agent
        .get("main")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("agent.main not configured"))?;

    // workspace 是该 agent 的根目录，所有 md 和 sessions.db 都从这里推导
    let workspace = PathBuf::from(&agent_cfg.workspace);
    std::fs::create_dir_all(&workspace).ok();

    // md 路径：显式 > workspace 推导
    let soul_path = resolve_md_path(&agent_cfg.soul, &workspace, "SOUL.md");
    let user_path = resolve_md_path(&agent_cfg.user, &workspace, "USER.md");
    let memory_path = resolve_md_path(&agent_cfg.memory, &workspace, "MEMORY.md");
    ensure_template(&soul_path, SOUL_TEMPLATE).await?;
    ensure_template(&user_path, USER_TEMPLATE).await?;
    ensure_template(&memory_path, MEMORY_TEMPLATE).await?;

    let soul = load_md(&soul_path).await?;
    let user = load_md(&user_path).await?;
    let memory = load_md(&memory_path).await?;

    // 解析 "provider_id.model_alias"，取出连接信息和模型配置
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
        .ok_or_else(|| anyhow::anyhow!("provider.{}.model.{} not configured", prov_id, model_alias))?;

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

    // 拼 system prompt：告知 workspace + 标签降级模式下注入工具协议说明
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

/// 解析 md 文件路径：显式 > workspace 拼接
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
