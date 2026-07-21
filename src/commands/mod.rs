pub mod slash;

use anyhow::Result;
use std::path::PathBuf;

use crate::config::Config;

pub async fn chat_cmd() -> Result<()> {
    let config = load_config_or_init()?;

    let log_dir = PathBuf::from(&config.log.dir);
    let _ = crate::log::init(&config.log.level, &log_dir);

    let agent = crate::channels::cli::build_agent(&config).await?;

    let mut tasks = Vec::new();

    if config.channels.cli.enabled {
        let cli = std::sync::Arc::new(crate::channels::CliChannel::new());
        let agent = agent.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = crate::channels::Channel::run(cli, agent).await {
                tracing::error!(error = %e, "CliChannel exited with error");
            }
        }));
    }

    if config.channels.qq.enabled {
        let qq = std::sync::Arc::new(crate::channels::qq::QqChannel::new(
            config.channels.qq.clone(),
        ));
        let agent = agent.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = crate::channels::Channel::run(qq, agent).await {
                tracing::error!(error = %e, "QqChannel exited with error");
            }
        }));
    }

    if tasks.is_empty() {
        anyhow::bail!("no channel enabled in config");
    }

    for t in tasks {
        let _ = t.await;
    }
    Ok(())
}

pub fn config_cmd() -> Result<()> {
    let cfg = load_config_or_init()?;
    println!("{}", toml::to_string_pretty(&cfg).unwrap_or_default());
    Ok(())
}

pub async fn doctor_cmd() -> Result<()> {
    let cfg = load_config_or_init()?;

    println!("log.dir: {}", cfg.log.dir);
    println!("runtime.context_threshold: {}", cfg.runtime.context_threshold);
    println!("runtime.max_iterations: {}", cfg.runtime.max_iterations);

    let agent_cfg = match cfg.agent.get("main") {
        Some(a) => a,
        None => {
            println!("\n[agent.main not configured]");
            return Ok(());
        }
    };
    println!("\nagent.main:");
    println!("  model: {}", agent_cfg.model);
    println!("  workspace: {}", agent_cfg.workspace);

    // 解析 model 引用，展示 provider 端点
    match Config::parse_model_ref(&agent_cfg.model) {
        Ok((prov_id, model_alias)) => {
            if let Some(p) = cfg.provider.get(prov_id) {
                println!(
                    "\nprovider.{}: {}",
                    prov_id, p.base_url
                );
                if let Some(m) = p.model.get(model_alias) {
                    println!(
                        "  model.{}: {} (native_tool_calling={})",
                        model_alias, m.model, m.native_tool_calling
                    );
                    match reqwest::Client::new()
                        .get(format!("{}/models", p.base_url.trim_end_matches('/')))
                        .send()
                        .await
                    {
                        Ok(resp) => println!("  /models status: {}", resp.status()),
                        Err(e) => println!("  /models error: {}", e),
                    }
                } else {
                    println!("  [model.{} not found under provider.{}]", model_alias, prov_id);
                }
            } else {
                println!("\n[provider.{} not configured]", prov_id);
            }
        }
        Err(e) => println!("\n[invalid agent.model: {}]", e),
    }

    Ok(())
}

pub async fn remember_cmd(text: &str) -> Result<()> {
    let cfg = load_config_or_init()?;
    let agent_cfg = cfg
        .agent
        .get("main")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("agent.main not configured"))?;
    let workspace = PathBuf::from(&agent_cfg.workspace);
    // memory 路径：显式 > workspace 推导
    let memory_path = match &agent_cfg.memory {
        Some(s) => {
            let p = PathBuf::from(s);
            if p.is_absolute() { p } else { workspace.join(s) }
        }
        None => workspace.join("MEMORY.md"),
    };
    crate::memory::ensure_template(&memory_path, crate::memory::MEMORY_TEMPLATE).await?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let line = format!("- [{}] {}\n", today, text);
    let mut content = tokio::fs::read_to_string(&memory_path)
        .await
        .unwrap_or_default();
    content.push_str(&line);
    tokio::fs::write(&memory_path, &content).await?;
    println!("remembered: {}", text);
    Ok(())
}

pub fn load_config_or_init() -> Result<Config> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let config_path = home.join(".laia/config.toml");
    if config_path.exists() {
        Config::load(&config_path)
    } else {
        Ok(Config::default_for_workspace("~/.laia"))
    }
}
