pub mod slash;

use anyhow::Result;

pub async fn chat_cmd() -> Result<()> {
    crate::channels::cli::run_repl().await
}

pub fn config_cmd() -> Result<()> {
    let cfg = load_config_or_init()?;
    println!("{}", toml::to_string_pretty(&cfg).unwrap_or_default());
    Ok(())
}

pub async fn doctor_cmd() -> Result<()> {
    let cfg = load_config_or_init()?;
    println!("workspace dir: {}", cfg.workspace.dir);
    println!(
        "soul: {}",
        cfg.agent
            .get("main")
            .map(|a| a.soul.as_str())
            .unwrap_or("(missing)")
    );
    println!(
        "user: {}",
        cfg.agent
            .get("main")
            .map(|a| a.user.as_str())
            .unwrap_or("(missing)")
    );
    println!(
        "memory: {}",
        cfg.agent
            .get("main")
            .map(|a| a.memory.as_str())
            .unwrap_or("(missing)")
    );

    if let Some(p) = cfg.provider.get("default") {
        println!("\nprovider: {}", p.base_url);
        match reqwest::Client::new()
            .get(format!("{}/models", p.base_url.trim_end_matches('/')))
            .send()
            .await
        {
            Ok(resp) => println!("  status: {}", resp.status()),
            Err(e) => println!("  error: {}", e),
        }
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
    let memory_path = std::path::PathBuf::from(&agent_cfg.memory);
    // 确保目录和模板存在
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

pub fn load_config_or_init() -> Result<crate::config::Config> {
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let config_path = home.join(".laia/config.toml");
    if config_path.exists() {
        crate::config::Config::load(&config_path)
    } else {
        Ok(crate::config::Config::default_for_workspace("~/.laia"))
    }
}
