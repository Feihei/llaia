pub mod slash;

use anyhow::Result;
use std::path::Path;
use std::path::PathBuf;

use crate::config::Config;

/// 终端交互模式：只启动 CliChannel，不连 QQ 等后台频道
pub async fn chat_cmd(config_dir: &Path) -> Result<()> {
    let config = load_config_or_init(config_dir)?;

    let log_dir = PathBuf::from(&config.log.dir);
    let _ = crate::log::init(&config.log.level, &log_dir);

    let pid_file = crate::pid::PidFile::new(config_dir);
    pid_file.acquire()?;
    let _pid_guard = PidGuard(pid_file);

    let registry = crate::channels::cli::build_agent(&config).await?;

    let cli = std::sync::Arc::new(crate::channels::CliChannel::new());
    crate::channels::Channel::run(cli, registry).await
}

/// 守护进程模式：启动所有非 CLI 的后台频道（QQ、未来 WebUI 等），不启动终端交互
pub async fn serve_cmd(config_dir: &Path) -> Result<()> {
    let config = load_config_or_init(config_dir)?;

    let log_dir = PathBuf::from(&config.log.dir);
    let _ = crate::log::init(&config.log.level, &log_dir);

    let pid_file = crate::pid::PidFile::new(config_dir);
    pid_file.acquire()?;
    let _pid_guard = PidGuard(pid_file);

    let registry = crate::channels::cli::build_agent(&config).await?;

    let mut tasks = Vec::new();

    if config.channels.qq.enabled {
        let qq = std::sync::Arc::new(crate::channels::qq::QqChannel::new(
            config.channels.qq.clone(),
        ));
        let registry = registry.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = crate::channels::Channel::run(qq, registry).await {
                tracing::error!(error = %e, "QqChannel exited with error");
            }
        }));
        tracing::info!("QqChannel started");
    }

    if config.channels.web.enabled {
        let workspace = {
            let a = registry.main.lock().await;
            a.workspace.clone()
        };
        let config_path = config_dir.join("config.toml");
        let web = std::sync::Arc::new(crate::channels::web::WebChannel::new(
            config.channels.web.clone(),
            registry.clone(),
            std::sync::Arc::new(tokio::sync::RwLock::new(config.clone())),
            config_path,
            workspace,
        ));
        let registry_clone = registry.clone();
        let host = config.channels.web.host.clone();
        let port = config.channels.web.port;
        tasks.push(tokio::spawn(async move {
            if let Err(e) = crate::channels::Channel::run(web, registry_clone).await {
                tracing::error!(error = %e, "WebChannel exited with error");
            }
        }));
        tracing::info!("WebChannel starting on {}:{}", host, port);
    }

    if tasks.is_empty() {
        anyhow::bail!("no service channel enabled in config (QQ/WebUI/...)");
    }

    tracing::info!(channels = tasks.len(), "serve mode: channels running");

    for t in tasks {
        let _ = t.await;
    }
    Ok(())
}

pub fn config_cmd(config_dir: &Path) -> Result<()> {
    let cfg = load_config_or_init(config_dir)?;
    println!("{}", toml::to_string_pretty(&cfg).unwrap_or_default());
    Ok(())
}

pub async fn doctor_cmd(config_dir: &Path) -> Result<()> {
    let cfg = load_config_or_init(config_dir)?;

    println!("config_dir: {}", config_dir.display());
    println!("log.dir: {}", cfg.log.dir);
    println!(
        "runtime.context_threshold: {}",
        cfg.runtime.context_threshold
    );
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
                println!("\nprovider.{}: {}", prov_id, p.base_url);
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
                    println!(
                        "  [model.{} not found under provider.{}]",
                        model_alias, prov_id
                    );
                }
            } else {
                println!("\n[provider.{} not configured]", prov_id);
            }
        }
        Err(e) => println!("\n[invalid agent.model: {}]", e),
    }

    Ok(())
}

pub async fn remember_cmd(text: &str, config_dir: &Path) -> Result<()> {
    let cfg = load_config_or_init(config_dir)?;
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
            if p.is_absolute() {
                p
            } else {
                workspace.join(s)
            }
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

/// 加载配置：config_dir 下找 config.toml，不存在则用默认配置
pub fn load_config_or_init(config_dir: &Path) -> Result<Config> {
    let config_path = config_dir.join("config.toml");
    if config_path.exists() {
        Config::load(&config_path)
    } else {
        Ok(Config::default_for_workspace(&config_dir.to_string_lossy()))
    }
}

/// RAII guard：作用域结束时自动释放 PID 文件
struct PidGuard(crate::pid::PidFile);

impl Drop for PidGuard {
    fn drop(&mut self) {
        self.0.release();
    }
}
