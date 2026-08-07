pub mod slash;

use anyhow::Result;
use std::path::Path;
use std::path::PathBuf;

use crate::config::Config;

/// init 默认配置模板：provider/agent 注释占位，QQ/WebUI 默认关闭。
/// 模板内路径用 ~/.llaia 占位，加载时 Config::load 会展开 ~。
const CONFIG_TEMPLATE: &str = r#"# LLAIA 配置文件
# 字段说明见 docs/adr/0008-config-schema-v1.1.md

[runtime]
context_threshold = 0.7
max_iterations = 10
# compact_model = "default.qwen"  # 可选：用更便宜的模型跑上下文压缩，未设置时复用主模型

[log]
level = "info"
dir = "~/.llaia/logs"

# Provider: 接入 LLM 服务
# 本地 Ollama 示例：
# [provider.default]
# type = "openai_compatible"
# base_url = "http://localhost:11434/v1"
# api_key = "${OLLAMA_API_KEY}"  # 或留空
#
# [provider.default.qwen]
# model = "qwen2.5:7b"
# native_tool_calling = false
# context_size = 32768

# 主 Agent：model 留空进入降级模式（无 provider，仅可配置 WebUI）
# 配好上面的 provider 后填 "default.qwen" 等引用即可启用聊天
# workspace / soul / user / memory 字段已废弃，自动推导到 ~/.llaia/workspace/
[agent.main]
model = ""

# 子 Agent 示例（取消注释启用；workspace 自动推导到 ~/.llaia/workspace/subagent/<alias>/）
# [agent.coder]
# model = "default.qwen"
# denied_tools = ["memory_write"]
# delegate_timeout = 180

[channels.qq]
enabled = false
app_id = ""                    # 支持 "${QQ_APP_ID}" 环境变量引用
app_secret = ""                # 支持 "${QQ_APP_SECRET}" 环境变量引用
confirm_mode = "none"        # none / always / session

[webui]
host = "127.0.0.1"
port = 51217
token = ""                   # 留空则启动时随机生成并打印日志

[tools.terminal]
confirm = "none"
command_policy = "blacklist"
command_whitelist = []

[tools.tavily]
api_key = ""                   # 支持 "${TAVILY_API_KEY}" 环境变量引用
"#;

/// init 默认 .env 模板：敏感凭据集中存放，避免写进 config.toml 明文。
/// .env 与 config.toml 同目录，启动时自动加载（CWD 下的 .env 也会被读取）。
const ENV_TEMPLATE: &str = r#"# LLAIA 环境变量（本文件不要提交到 git）
# config.toml 中可用 "${VAR_NAME}" 引用此处定义的变量

# OLLAMA_API_KEY=
# QQ_APP_ID=
# QQ_APP_SECRET=
# TAVILY_API_KEY=
"#;

/// init 默认 cron.toml 模板：所有任务注释，仅作文档。
const CRON_TEMPLATE: &str = r#"# LLAIA cron 定时任务配置
# 字段说明见 docs/adr/0013-cron-scheduling.md
# schedule: 5 字段 cron 表达式（分 时 日 月 周），内部自动转 6 字段供调度器使用
# mode: agent（唤醒主 agent）/ tools（直接跑工具链）
# channel: qq / cli / web（结果推送目标；cli 无持久连接用 NoopPusher 丢弃结果）
# enabled: 默认 true；false 则调度器不注册

# 示例：每天 8:00 唤醒 agent 查新闻推送
# [[task]]
# id = "morning_news"
# schedule = "0 8 * * *"
# mode = "agent"
# channel = "qq"
# enabled = true
# prompt = """
# 现在是早上 8:00。请查今天的 AI 科技热点，
# 整理成 3-5 条简讯推送给我。
# """

# 示例：每 30 分钟跑工具链（不消耗 LLM token）
# [[task]]
# id = "health_check"
# schedule = "*/30 * * * *"
# mode = "tools"
# channel = "web"
# enabled = true
# steps = [
#   { tool = "tavily_search", args = { query = "llaia" } },
#   { tool = "memory_write", args = { text = "checked at {{now}}" } },
# ]
"#;

/// init 默认 mcp.toml 模板：所有 server 注释，仅作文档。
const MCP_TEMPLATE: &str = r#"# LLAIA MCP server 配置（修改后需重启 llaia serve/chat 生效）
# 字段说明见 docs/adr/0014-mcp-client.md
# 工具命名：<server id>__<tool_name>（如 filesystem__read_file）
# MCP 工具默认 requires_confirm = true，safe_tools 里的工具免确认

# 示例：stdio transport（本地子进程）
# [[server]]
# id = "filesystem"
# enabled = true
# transport = "stdio"
# command = "npx"
# args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed/dir"]
# safe_tools = ["read_file", "list_directory"]
# # tool_timeout_secs = 180

# 示例：streamable HTTP transport（远程 server）
# [[server]]
# id = "remote"
# enabled = true
# transport = "http"
# url = "https://internal-mcp.corp/mcp"
#
# [server.headers]
# Authorization = "Bearer ${MCP_TOKEN}"   # secret 放 .env，不落盘

# 示例：旧版 SSE transport
# [[server]]
# id = "legacy-sse"
# enabled = true
# transport = "sse"
# url = "https://legacy-mcp.corp/sse"
"#;

/// llaia init：生成 ~/.llaia/ 目录骨架 + 基础模板，提示进入 WebUI 完成配置。
/// 幂等：已存在的文件不覆盖（除非 force）。
pub fn init_cmd(config_dir: &Path, force: bool) -> Result<()> {
    let config_dir_expanded = shellexpand::tilde(&config_dir.to_string_lossy()).into_owned();
    let config_dir = PathBuf::from(&config_dir_expanded);

    // 1. 创建目录骨架
    let workspace = config_dir.join("workspace");
    let logs_dir = config_dir.join("logs");
    let uploads_dir = workspace.join("uploads");
    let subagent_dir = workspace.join("subagent");
    std::fs::create_dir_all(&config_dir)?;
    std::fs::create_dir_all(&logs_dir)?;
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&uploads_dir)?;
    std::fs::create_dir_all(&subagent_dir)?;

    // 2. 生成 config.toml
    let config_path = config_dir.join("config.toml");
    write_file_if_needed(&config_path, CONFIG_TEMPLATE, force)?;
    println!("✓ created directory structure at {}", config_dir.display());
    println!("✓ generated config.toml (with commented template)");

    // 3. 生成 SOUL.md / USER.md / MEMORY.md 模板（同步阻塞，文件小）
    let soul_path = workspace.join("SOUL.md");
    let user_path = workspace.join("USER.md");
    let memory_path = workspace.join("MEMORY.md");
    write_file_if_needed(&soul_path, crate::memory::SOUL_TEMPLATE, force)?;
    write_file_if_needed(&user_path, crate::memory::USER_TEMPLATE, force)?;
    write_file_if_needed(&memory_path, crate::memory::MEMORY_TEMPLATE, force)?;
    println!("✓ generated SOUL.md / USER.md / MEMORY.md templates");

    // 4. 生成 cron.toml 模板
    let cron_path = config_dir.join("cron.toml");
    write_file_if_needed(&cron_path, CRON_TEMPLATE, force)?;
    println!("✓ generated cron.toml (cron template, all commented by default)");

    // 5. 生成 mcp.toml 模板
    let mcp_path = config_dir.join("mcp.toml");
    write_file_if_needed(&mcp_path, MCP_TEMPLATE, force)?;
    println!("✓ generated mcp.toml (MCP server template, all commented by default)");

    // 6. 生成 .env 模板（敏感凭据集中存放，config.toml 用 ${VAR} 引用）
    let env_path = config_dir.join(".env");
    write_file_if_needed(&env_path, ENV_TEMPLATE, force)?;
    println!("✓ generated .env (secret template; fill in real values, do not commit to git)");

    // 7. 终端输出引导
    println!();
    println!("Next steps:");
    println!("  1. edit ~/.llaia/.env and fill in API keys and other secrets");
    println!("  2. edit ~/.llaia/config.toml, set model reference in [agent.main] (e.g. default.qwen)");
    println!("     or run llaia serve and configure via WebUI at http://127.0.0.1:51217");
    println!("  3. start the service: llaia serve");
    println!("  4. CLI debug: llaia chat");
    Ok(())
}

/// 写文件：文件不存在则写入；存在时若 force=true 覆盖，否则跳过。
fn write_file_if_needed(path: &Path, content: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        tracing::debug!(path = %path.display(), "file exists, skip (use --force to overwrite)");
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

/// 终端交互模式：只启动 CliChannel，不连 QQ 等后台频道
pub async fn chat_cmd(config_dir: &Path) -> Result<()> {
    let config = load_config_or_init(config_dir)?;

    // 目录结构迁移
    if crate::migrate::migrate_if_needed(config_dir)? {
        tracing::info!("directory migrated, reloading config");
    }

    let log_dir = PathBuf::from(&config.log.dir);
    let _ = crate::log::init(&config.log.level, &log_dir);

    let pid_file = crate::pid::PidFile::new(config_dir);
    pid_file.acquire()?;
    let _pid_guard = PidGuard(pid_file);

    let (registry, _cron_tool, _mcp_registry) =
        crate::channels::cli::build_agent(&config, config_dir).await?;

    // chat 模式必须有 provider：纯 CLI 无法配置，无 provider 直接报错引导
    {
        let a = registry.main.lock().await;
        if !a.has_provider().await {
            anyhow::bail!(
                "No provider configured, cannot start chat.\nRun `llaia serve` first and configure the provider via WebUI at http://{}:{}, \nor edit {}/config.toml to uncomment [provider.default] / [agent.main].",
                config.webui.host,
                config.webui.port,
                config_dir.display()
            );
        }
    }

    let cli = std::sync::Arc::new(crate::channels::CliChannel::new());
    crate::channels::Channel::run(cli, registry).await
}

/// 守护进程模式：启动所有非 CLI 的后台频道（QQ、未来 WebUI 等），不启动终端交互
pub async fn serve_cmd(config_dir: &Path) -> Result<()> {
    let config = load_config_or_init(config_dir)?;

    // 目录结构迁移
    if crate::migrate::migrate_if_needed(config_dir)? {
        tracing::info!("directory migrated, reloading config");
    }

    let log_dir = PathBuf::from(&config.log.dir);
    let _ = crate::log::init(&config.log.level, &log_dir);

    // 与 chat 共用同一份欢迎 billboard（见 crate::banner）
    print!("{}", crate::banner::billboard());
    println!("  background service mode: QQ / WebUI channels, press Ctrl+C to quit\n");

    let pid_file = crate::pid::PidFile::new(config_dir);
    pid_file.acquire()?;
    let _pid_guard = PidGuard(pid_file);

    let (registry, cron_tool, mcp_registry) =
        crate::channels::cli::build_agent(&config, config_dir).await?;

    // serve 模式：无 provider 时 warn 但继续启动（WebUI 配置功能不依赖 provider，聊天降级提示）
    {
        let a = registry.main.lock().await;
        if !a.has_provider().await {
            tracing::warn!(
                "No provider configured; chat is unavailable. Configure [provider.default] in WebUI (http://{}:{})",
                config.webui.host,
                config.webui.port
            );
        }
    }

    let mut tasks = Vec::new();

    // 主 agent workspace（QQ channel 需注入用于 cron 主动推送时读 USER.md）
    let workspace = {
        let a = registry.main.lock().await;
        a.workspace.clone()
    };

    // QQ channel：启用时构造并 spawn，同时克隆一份 Arc 给 cron pusher
    let qq_pusher_for_cron: Option<std::sync::Arc<dyn crate::cron::ProactivePusher>> =
        if config.channels.qq.enabled {
            let qq = std::sync::Arc::new(
                crate::channels::qq::QqChannel::new(config.channels.qq.clone())
                    .with_workspace(workspace.clone()),
            );
            let pusher: std::sync::Arc<dyn crate::cron::ProactivePusher> = qq.clone();
            let registry = registry.clone();
            tasks.push(tokio::spawn(async move {
                if let Err(e) = crate::channels::Channel::run(qq, registry).await {
                    tracing::error!(error = %e, "QqChannel exited with error");
                }
            }));
            tracing::info!("QqChannel started");
            Some(pusher)
        } else {
            None
        };

    // webui 随 serve 无条件启动（serve 模式下用户唯一保证可用的交互入口）
    // 注意：WebChannel 先创建，但不立即 spawn —— 需要在 CronScheduler 启动后注入 cron_scheduler，
    // 再 spawn WebChannel::run（build_router 在 run 内调用，此时 cron_scheduler 已就位）
    let config_path = config_dir.join("config.toml");
    let web = std::sync::Arc::new(crate::channels::web::WebChannel::new(
        config.webui.clone(),
        registry.clone(),
        std::sync::Arc::new(tokio::sync::RwLock::new(config.clone())),
        config_path,
        workspace.clone(),
    ));
    web.set_mcp_registry(mcp_registry);
    let web_pusher_for_cron: std::sync::Arc<dyn crate::cron::ProactivePusher> = web.clone();
    let web_host = config.webui.host.clone();
    let web_port = config.webui.port;

    // 启动 cron 调度器（仅 serve 模式）
    let cron_path = config_dir.join("cron.toml");
    let mut pushers: std::collections::HashMap<
        String,
        std::sync::Arc<dyn crate::cron::ProactivePusher>,
    > = std::collections::HashMap::new();
    if let Some(p) = qq_pusher_for_cron {
        pushers.insert("qq".into(), p);
    }
    pushers.insert("web".into(), web_pusher_for_cron);
    // cli：无持久连接，不注册 pusher（channel="cli" 的任务会用 NoopPusher 丢弃结果）
    let _cron = match crate::cron::CronScheduler::start(&cron_path, registry.clone(), pushers).await
    {
        Ok(s) => {
            tracing::info!("CronScheduler started");
            let s = std::sync::Arc::new(s);
            // 注入给 WebChannel（共享槽，build_router 读取快照填 AppState）
            web.set_cron_scheduler(s.clone());
            tracing::info!("CronScheduler injected into WebChannel");
            // 注入给 CronTool，让 agent 能通过工具管理 cron 任务
            if let Some(ct) = &cron_tool {
                ct.set_scheduler(s.clone());
                tracing::info!("CronScheduler injected into CronTool");
            }
            Some(s)
        }
        Err(e) => {
            tracing::error!(error = %e, "CronScheduler start failed, cron disabled");
            None
        }
    };

    // cron 注入完成后再 spawn WebChannel（确保 build_router 时 cron_scheduler 已就位）
    let registry_clone = registry.clone();
    let web_for_spawn = web.clone();
    tasks.push(tokio::spawn(async move {
        if let Err(e) = crate::channels::Channel::run(web_for_spawn, registry_clone).await {
            tracing::error!(error = %e, "WebChannel exited with error");
        }
    }));
    tracing::info!("WebChannel starting on {}:{}", web_host, web_port);

    if tasks.is_empty() {
        anyhow::bail!("no service channel enabled in config (QQ/WebUI/...)");
    }

    tracing::info!(
        channels = tasks.len(),
        "serve mode: channels running, press Ctrl+C to stop"
    );

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("received Ctrl+C, shutting down");
            // 与 chat 共用同一句退出语
            println!("\n{}", crate::banner::GOODBYE);
        }
        _ = async {
            for t in tasks {
                let _ = t.await;
            }
        } => {}
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

    // provider 配置检查：无 [provider.<id>] section 时 warn（不 error，serve 可降级启动）
    if cfg.provider.is_empty() {
        println!("\n[warn] No provider configured; llaia serve will start in degraded mode (chat unavailable, WebUI config usable)");
        println!(
            "       suggestion: run `llaia init`, then edit {}/config.toml to uncomment [provider.default]",
            config_dir.display()
        );
    } else {
        for (pid, p) in &cfg.provider {
            println!("\nprovider.{}: {}", pid, p.base_url);
            for (alias, m) in &p.model {
                println!(
                    "  model.{}: {} (native_tool_calling={})",
                    alias, m.model, m.native_tool_calling
                );
            }
        }
    }

    // cron.toml 检查
    let cron_path = config_dir.join("cron.toml");
    if cron_path.exists() {
        match crate::cron::CronConfig::load(&cron_path) {
            Ok(c) => {
                let enabled = c.task.iter().filter(|t| t.enabled).count();
                println!(
                    "\ncron.toml: {} ({} tasks, {} enabled)",
                    cron_path.display(),
                    c.task.len(),
                    enabled
                );
                for t in &c.task {
                    println!(
                        "  - {} [{:?}] schedule={} channel={} {}",
                        t.id,
                        t.mode,
                        t.schedule,
                        t.channel,
                        if t.enabled { "enabled" } else { "disabled" }
                    );
                }
            }
            Err(e) => println!("\n[warn] failed to parse cron.toml: {}", e),
        }
    } else {
        println!("\ncron.toml: not found (no cron tasks; run `llaia init` to generate a template)");
    }

    // mcp.toml 检查
    let mcp_path = config_dir.join("mcp.toml");
    if mcp_path.exists() {
        match crate::mcp::McpConfig::load(&mcp_path) {
            Ok(c) => {
                let enabled = c.server.iter().filter(|s| s.enabled).count();
                println!(
                    "\nmcp.toml: {} ({} servers, {} enabled)",
                    mcp_path.display(),
                    c.server.len(),
                    enabled
                );
                for s in &c.server {
                    println!(
                        "  - {} [{:?}] {}",
                        s.id,
                        s.transport,
                        if s.enabled { "enabled" } else { "disabled" }
                    );
                }
            }
            Err(e) => println!("\n[warn] failed to parse mcp.toml: {}", e),
        }
    } else {
        println!("\nmcp.toml: not found (no MCP server; run `llaia init` to generate a template)");
    }

    let agent_cfg = match cfg.agent.get("main") {
        Some(a) => a,
        None => {
            println!("\n[warn] [agent.main] not configured (degraded mode)");
            // 仍检查 sessions.db 存在性（基于推导的 workspace 路径）
            let workspace = config_dir.join("workspace");
            let db_path = workspace.join("sessions.db");
            if !db_path.exists() {
                println!(
                    "[warn] sessions.db not found: {} (created automatically on first start)",
                    db_path.display()
                );
            } else {
                println!(
                    "sessions.db: {} ({} bytes)",
                    db_path.display(),
                    std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0)
                );
            }
            return Ok(());
        }
    };
    // 自动推导 workspace
    let workspace = agent_cfg.derive_workspace(config_dir, "main");
    println!("\nagent.main:");
    println!("  model: {}", agent_cfg.model);
    println!("  workspace (derived): {}", workspace.display());

    // sessions.db 存在性检查
    let db_path = workspace.join("sessions.db");
    if !db_path.exists() {
        println!(
            "[warn] sessions.db not found: {} (created automatically on first start)",
            db_path.display()
        );
    } else {
        println!(
            "sessions.db: {} ({} bytes)",
            db_path.display(),
            std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0)
        );
    }

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
                println!("\n[warn] provider.{} not configured", prov_id);
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
    let workspace = agent_cfg.derive_workspace(config_dir, "main");
    let memory_path = workspace.join("MEMORY.md");
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
