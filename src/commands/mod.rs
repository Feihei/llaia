pub mod slash;

use anyhow::Result;
use std::path::Path;
use std::path::PathBuf;

use crate::config::Config;

/// 渲染 `native_tool_calling` 展示：None（auto）= "auto"（跟随 Compat 探测，#10）。
fn native_label(v: Option<bool>) -> String {
    v.map(|b| b.to_string()).unwrap_or_else(|| "auto".into())
}

/// P5 S1：启动时扫描 config.toml 明文敏感字段并 log warn（不自动迁移）。
/// 读原始 TOML（内存 config 已被 expand_paths 展开为明文，不能用作判断）。
fn warn_plaintext_secrets(config_dir: &Path) {
    let config_path = config_dir.join("config.toml");
    match crate::config::secrets::count_plaintext_secrets(&config_path) {
        Ok(n) if n > 0 => {
            tracing::warn!(
                count = n,
                "plaintext secrets found in config.toml; run /migrate-secrets or save via WebUI to move them into .env"
            );
        }
        _ => {}
    }
}

/// Default config template for `init`: provider/agent placeholders commented out, all channels off by default.
/// Paths inside the template use ~/.llaia as a placeholder; Config::load expands ~ at load time.
const CONFIG_TEMPLATE: &str = r#"# LLAIA configuration file
# Field reference: docs/adr/0008-config-schema-v1.1.md

[runtime]
context_threshold = 0.7
max_iterations = 10
# permission = "default"  # optional: permission tier default / read-only / yolo; defaults to default. Switch at runtime with /permission (not persisted)
# timezone = "Asia/Shanghai"     # optional: IANA timezone name (e.g. Asia/Shanghai / America/New_York); defaults to system timezone
# compact_model = "default.qwen"  # optional: cheaper model for context compaction; defaults to main model
# vision_model = "default.gpt-4o"  # optional: model to describe images when main model lacks multimodal; defaults to sending images to main model

[log]
level = "info"
dir = "~/.llaia/logs"

# Provider: connect an LLM service
# Local Ollama example:
# [provider.default]
# type = "openai_compatible"
# base_url = "http://localhost:11434/v1"
# api_key = "${OLLAMA_API_KEY}"  # or leave empty
#
# [provider.default.qwen]
# model = "qwen2.5:7b"
# native_tool_calling = false
# context_size = 32768

# Cloud Anthropic example (also works with a gateway base_url):
# [provider.claude]
# type = "anthropic"
# api_key = "${ANTHROPIC_API_KEY}"
#
# [provider.claude.sonnet]
# model = "claude-sonnet-4-20250514"
# max_tokens = 8192              # required for Anthropic; defaults to 4096 if unset

# Main Agent: leave model empty to enter degraded mode (no provider, WebUI config only)
# After configuring a provider above, set e.g. "default.qwen" to enable chat
# fallback = ["default.qwen"]    # optional: model ref chain tried in order when the main model fails
# workspace / soul / user / memory fields are deprecated; auto-resolved to ~/.llaia/workspace/
[agent.main]
model = ""

# Sub-agent example (uncomment to enable; workspace auto-resolves to ~/.llaia/workspace/subagent/<alias>/)
# [agent.coder]
# model = "default.qwen"
# denied_tools = ["memory_write"]
# delegate_timeout = 180

[channels.qq]
enabled = false
app_id = ""                    # supports "${QQ_APP_ID}" env var reference
app_secret = ""                # supports "${QQ_APP_SECRET}" env var reference
confirm_mode = "none"        # none / always / session
owner_openid = ""              # optional: default cron push target; auto-learned from first C2C message otherwise

# [channels.telegram]
# enabled = false
# bot_token = "${TELEGRAM_BOT_TOKEN}"  # issued by @BotFather
# allow_chat_id = 0            # only respond to this chat (single-user lock); 0 = no restriction
# owner_chat_id = 0            # optional cron push target; 0 = fall back to allow_chat_id

# [channels.dingtalk]
# enabled = false
# client_id = "${DINGTALK_CLIENT_ID}"
# client_secret = "${DINGTALK_CLIENT_SECRET}"
# allow_staff_id = ""          # only respond to this staffId; empty = no restriction

# [channels.feishu]
# enabled = false
# app_id = "${FEISHU_APP_ID}"
# app_secret = "${FEISHU_APP_SECRET}"
# allow_open_id = ""           # only respond to this open_id (single-user lock); empty = no restriction
# mention_only = false         # group chat: reply only when @-mentioned (true); DMs always reply

# [channels.wechat]
# enabled = false              # WeChat ClawBot (ilink bot); prints a QR login link on first start, scan with phone
# allow_user_id = ""           # only respond to this ilink_user_id; empty = no restriction
# owner_user_id = ""           # optional cron push target; auto-learned from first inbound message otherwise

[webui]
host = "127.0.0.1"
port = 51217
token = ""                   # empty => random token generated at startup and printed to logs

[tools.terminal]
confirm = "none"
command_policy = "blacklist"
command_whitelist = []

[tools.search]
provider = "tavily"            # search provider: tavily / baidu / brave
top_k = 8                      # default number of results

[tools.tavily]
api_key = ""                   # supports "${TAVILY_API_KEY}" env var reference
[tools.baidu]
api_key = ""                   # Baidu Qianfan AI Search; supports "${BAIDU_API_KEY}"
[tools.brave]
api_key = ""                   # Brave Search API; supports "${BRAVE_API_KEY}"
[tools.tts]                    # P5 T1: OpenAI-compatible /audio/speech
enabled = false
base_url = "https://api.openai.com/v1"
api_key = ""                   # supports "${TTS_API_KEY}"
model = "tts-1"
voice = "alloy"
"#;

/// Default .env template for `init`: secrets live here, kept out of config.toml plaintext.
/// .env sits next to config.toml and is loaded automatically at startup (a CWD .env is also read).
const ENV_TEMPLATE: &str = r#"# LLAIA environment variables (do not commit this file to git)
# Reference these here as "${VAR_NAME}" from config.toml

# OLLAMA_API_KEY=
# ANTHROPIC_API_KEY=
# QQ_APP_ID=
# QQ_APP_SECRET=
# TELEGRAM_BOT_TOKEN=
# DINGTALK_CLIENT_ID=
# DINGTALK_CLIENT_SECRET=
# FEISHU_APP_ID=
# FEISHU_APP_SECRET=
# TAVILY_API_KEY=
# BAIDU_API_KEY=
# BRAVE_API_KEY=
# TTS_API_KEY=
"#;

/// Default cron.toml template for `init`: all tasks commented out, docs only.
const CRON_TEMPLATE: &str = r#"# LLAIA cron schedule configuration
# Field reference: docs/adr/0013-cron-scheduling.md
# schedule: 5-field cron expression (min hour day month weekday); internally expanded to 6 fields for the scheduler
# mode: agent (wake main agent) / tools (run a tool chain directly)
# channel: qq / cli / web (where results are pushed; cli has no persistent connection, uses NoopPusher to drop results)
# enabled: defaults to true; false means the scheduler won't register it

# Example: wake agent every day at 08:00 to fetch news and push
# [[task]]
# id = "morning_news"
# schedule = "0 8 * * *"
# mode = "agent"
# channel = "qq"
# enabled = true
# prompt = """
# It's 8:00 AM. Check today's AI/tech headlines and
# summarize them into 3-5 short briefs for me.
# Fetch at most 3 sources, never fetch the same URL twice,
# then summarize immediately.
# """
# 提示：agent 模式任务抓网页时，建议把 config.toml 中 [tools.web_fetch] 的
# max_chars 调小（如 4000~5000），避免大段网页文本撑爆上下文导致 agent
# 陷入"重复抓取同一 URL"死循环直至超时。

# Example: run a tool chain every 30 minutes (no LLM token cost)
# [[task]]
# id = "health_check"
# schedule = "*/30 * * * *"
# mode = "tools"
# channel = "web"
# enabled = true
# steps = [
#   { tool = "search", args = { query = "llaia" } },
#   { tool = "memory_write", args = { text = "checked at {{now}}" } },
# ]
"#;

/// Default mcp.toml template for `init`: all servers commented out, docs only.
const MCP_TEMPLATE: &str = r#"# LLAIA MCP server configuration (restart llaia serve/chat after changes)
# Field reference: docs/adr/0014-mcp-client.md
# Tool naming: <server id>__<tool_name> (e.g. filesystem__read_file)
# MCP tools require confirmation by default; tools listed in safe_tools skip confirmation

# Example: stdio transport (local subprocess)
# [[server]]
# id = "filesystem"
# enabled = true
# transport = "stdio"
# command = "npx"
# args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed/dir"]
# safe_tools = ["read_file", "list_directory"]
# # tool_timeout_secs = 180

# Example: streamable HTTP transport (remote server)
# [[server]]
# id = "remote"
# enabled = true
# transport = "http"
# url = "https://internal-mcp.corp/mcp"
#
# [server.headers]
# Authorization = "Bearer ${MCP_TOKEN}"   # secret goes in .env, not on disk

# Example: legacy SSE transport
# [[server]]
# id = "legacy-sse"
# enabled = true
# transport = "sse"
# url = "https://legacy-mcp.corp/sse"
"#;

/// llaia init: scaffold ~/.llaia/ and base templates, then point the user to the WebUI to finish setup.
/// Idempotent: existing files are not overwritten (unless force).
pub fn init_cmd(config_dir: &Path, force: bool) -> Result<()> {
    init_scaffold(config_dir, force)?;

    // Terminal onboarding output
    println!("✓ created directory structure at {}", config_dir.display());
    println!("✓ generated config.toml (with commented template)");
    println!("✓ generated SOUL.md / USER.md / MEMORY.md templates");
    println!("✓ generated cron.toml (cron template, all commented by default)");
    println!("✓ generated mcp.toml (MCP server template, all commented by default)");
    println!("✓ generated .env (secret template; fill in real values, do not commit to git)");
    println!();
    println!("Next steps:");
    println!("  1. edit ~/.llaia/.env and fill in API keys and other secrets");
    println!(
        "  2. edit ~/.llaia/config.toml, set model reference in [agent.main] (e.g. default.qwen)"
    );
    println!("     or run llaia serve and configure via WebUI at http://127.0.0.1:51217");
    println!("  3. start the service: llaia serve");
    println!("  4. CLI debug: llaia chat");
    Ok(())
}

/// 创建目录骨架并按模板补齐缺失文件（serve/chat 启动时也会调用，force=false）。
/// 返回是否有文件被新建/覆盖，供调用方记录日志。
fn init_scaffold(config_dir: &Path, force: bool) -> Result<bool> {
    let config_dir_expanded = shellexpand::tilde(&config_dir.to_string_lossy()).into_owned();
    let config_dir = PathBuf::from(&config_dir_expanded);

    // 1. Create directory skeleton
    let workspace = config_dir.join("workspace");
    let logs_dir = config_dir.join("logs");
    let uploads_dir = workspace.join("uploads");
    let subagent_dir = workspace.join("subagent");
    std::fs::create_dir_all(&config_dir)?;
    std::fs::create_dir_all(&logs_dir)?;
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&uploads_dir)?;
    std::fs::create_dir_all(&subagent_dir)?;

    // 2. Generate config.toml
    let mut changed = false;
    let config_path = config_dir.join("config.toml");
    changed |= write_file_if_needed(&config_path, CONFIG_TEMPLATE, force)?;

    // 3. Generate SOUL.md / USER.md / MEMORY.md templates (sync, small files)
    let soul_path = workspace.join("SOUL.md");
    let user_path = workspace.join("USER.md");
    let memory_path = workspace.join("MEMORY.md");
    changed |= write_file_if_needed(&soul_path, crate::memory::SOUL_TEMPLATE, force)?;
    changed |= write_file_if_needed(&user_path, crate::memory::USER_TEMPLATE, force)?;
    changed |= write_file_if_needed(&memory_path, crate::memory::MEMORY_TEMPLATE, force)?;

    // 4. Generate cron.toml template
    let cron_path = config_dir.join("cron.toml");
    changed |= write_file_if_needed(&cron_path, CRON_TEMPLATE, force)?;

    // 5. Generate mcp.toml template
    let mcp_path = config_dir.join("mcp.toml");
    changed |= write_file_if_needed(&mcp_path, MCP_TEMPLATE, force)?;

    // 6. Generate .env template (secrets centralized; config.toml references via ${VAR})
    let env_path = config_dir.join(".env");
    changed |= write_file_if_needed(&env_path, ENV_TEMPLATE, force)?;

    Ok(changed)
}

/// 写文件：文件不存在则写入；存在时若 force=true 覆盖，否则跳过。返回是否有写入动作。
fn write_file_if_needed(path: &Path, content: &str, force: bool) -> Result<bool> {
    if path.exists() && !force {
        tracing::debug!(path = %path.display(), "file exists, skip (use --force to overwrite)");
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(true)
}

/// 启动路径共享的目录准备：先迁移旧结构，再幂等补齐 init 模板，之后才加载配置。
/// 顺序约束：迁移必须先于模板补齐（否则模板会遮蔽待迁移的旧版散落文件），
/// 补齐必须先于配置加载（否则新生成的 config.toml 模板不会被本次加载，走了内存默认值）。
fn prepare_startup_dir(config_dir: &Path) -> Result<()> {
    crate::migrate::migrate_if_needed(config_dir)?;
    if init_scaffold(config_dir, false)? {
        tracing::info!(dir = %config_dir.display(), "scaffolded missing init templates");
    }
    Ok(())
}

/// 终端交互模式：只启动 CliChannel，不连 QQ 等后台频道
pub async fn chat_cmd(config_dir: &Path) -> Result<()> {
    prepare_startup_dir(config_dir)?;
    let config = load_config_or_init(config_dir)?;
    warn_plaintext_secrets(config_dir);

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
    prepare_startup_dir(config_dir)?;
    let config = load_config_or_init(config_dir)?;
    warn_plaintext_secrets(config_dir);

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

    // serve 模式：让主 agent 与 WebChannel 共享同一份 live_config，
    // 这样 WebUI 修改 [runtime].timezone 后下一轮对话即可生效（ADR-0017 热更新）。
    let live_config = std::sync::Arc::new(tokio::sync::RwLock::new(config.clone()));
    {
        let mut a = registry.main.lock().await;
        a.attach_live_config(live_config.clone());
    }
    // WebUI 优雅停止信号：/api/shutdown 触发后让 serve_cmd 退出（ADR-0018）
    let shutdown_signal = std::sync::Arc::new(tokio::sync::Notify::new());

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

    // Telegram channel：启用时构造并 spawn（long polling 免公网），克隆一份 Arc 给 cron pusher
    let tg_pusher_for_cron: Option<std::sync::Arc<dyn crate::cron::ProactivePusher>> =
        if config.channels.telegram.enabled {
            match crate::channels::telegram::TelegramChannel::new(config.channels.telegram.clone())
            {
                Ok(tg) => {
                    let tg = std::sync::Arc::new(tg);
                    let pusher: std::sync::Arc<dyn crate::cron::ProactivePusher> = tg.clone();
                    let registry = registry.clone();
                    tasks.push(tokio::spawn(async move {
                        if let Err(e) = crate::channels::Channel::run(tg, registry).await {
                            tracing::error!(error = %e, "TelegramChannel exited with error");
                        }
                    }));
                    tracing::info!("TelegramChannel started");
                    Some(pusher)
                }
                Err(e) => {
                    tracing::error!(error = %e, "TelegramChannel init failed, disabled");
                    None
                }
            }
        } else {
            None
        };

    // 钉钉 channel：启用时构造并 spawn（Stream Mode WS 免公网）
    if config.channels.dingtalk.enabled {
        let dt = std::sync::Arc::new(crate::channels::dingtalk::DingtalkChannel::new(
            config.channels.dingtalk.clone(),
        ));
        let registry = registry.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = crate::channels::Channel::run(dt, registry).await {
                tracing::error!(error = %e, "DingtalkChannel exited with error");
            }
        }));
        tracing::info!("DingtalkChannel started");
    }

    // 微信 ClawBot channel：启用时构造并 spawn（扫码登录 + 长轮询免公网），克隆一份 Arc 给 cron pusher
    // 登录态持久化在 <config_dir>/wechat_state.json
    let wechat_pusher_for_cron: Option<std::sync::Arc<dyn crate::cron::ProactivePusher>> =
        if config.channels.wechat.enabled {
            let wx = std::sync::Arc::new(crate::channels::wechat::WechatChannel::new(
                config.channels.wechat.clone(),
                config_dir.to_path_buf(),
            ));
            let pusher: std::sync::Arc<dyn crate::cron::ProactivePusher> = wx.clone();
            let registry = registry.clone();
            tasks.push(tokio::spawn(async move {
                if let Err(e) = crate::channels::Channel::run(wx, registry).await {
                    tracing::error!(error = %e, "WechatChannel exited with error");
                }
            }));
            tracing::info!("WechatChannel started");
            Some(pusher)
        } else {
            None
        };

    // 邮箱 channel：启用时构造并 spawn（IMAP 轮询收件 + SMTP 发信）
    // 作为 cron pusher 注册为 "mail"，主动推送结果发往 owner_email。
    let mail_pusher_for_cron: Option<std::sync::Arc<dyn crate::cron::ProactivePusher>> =
        if config.channels.mail.enabled {
            let mail = std::sync::Arc::new(
                crate::channels::mail::MailChannel::new(config.channels.mail.clone())
                    .with_workspace(workspace.clone()),
            );
            let pusher: std::sync::Arc<dyn crate::cron::ProactivePusher> = mail.clone();
            let registry = registry.clone();
            tasks.push(tokio::spawn(async move {
                if let Err(e) = crate::channels::Channel::run(mail, registry).await {
                    tracing::error!(error = %e, "MailChannel exited with error");
                }
            }));
            tracing::info!("MailChannel started");
            Some(pusher)
        } else {
            None
        };

    // 飞书 channel：启用时构造并 spawn（事件订阅长连接 WS 免公网）
    if config.channels.feishu.enabled {
        let fs = std::sync::Arc::new(crate::channels::feishu::FeishuChannel::new(
            config.channels.feishu.clone(),
        ));
        let registry = registry.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = crate::channels::Channel::run(fs, registry).await {
                tracing::error!(error = %e, "FeishuChannel exited with error");
            }
        }));
        tracing::info!("FeishuChannel started");
    }

    // webui 随 serve 无条件启动（serve 模式下用户唯一保证可用的交互入口）
    // 注意：WebChannel 先创建，但不立即 spawn —— 需要在 CronScheduler 启动后注入 cron_scheduler，
    // 再 spawn WebChannel::run（build_router 在 run 内调用，此时 cron_scheduler 已就位）
    let config_path = config_dir.join("config.toml");
    let web = std::sync::Arc::new(crate::channels::web::WebChannel::new(
        config.webui.clone(),
        registry.clone(),
        live_config.clone(),
        config_path,
        workspace.clone(),
        shutdown_signal.clone(),
    ));
    web.set_mcp_registry(mcp_registry);
    // cron_tool 注入 WebChannel，供热加载 cron 时重新指向新调度器（P4-f）
    web.set_cron_tool(cron_tool.clone());
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
    if let Some(p) = tg_pusher_for_cron {
        pushers.insert("telegram".into(), p);
    }
    if let Some(p) = wechat_pusher_for_cron {
        pushers.insert("wechat".into(), p);
    }
    if let Some(p) = &mail_pusher_for_cron {
        pushers.insert("mail".into(), p.clone());
    }
    pushers.insert("web".into(), web_pusher_for_cron);
    // cli：无持久连接，不注册 pusher（channel="cli" 的任务会用 NoopPusher 丢弃结果）
    let _cron = match crate::cron::CronHandle::start(
        &cron_path,
        registry.clone(),
        pushers,
        crate::time::resolve_tz(&config.runtime.timezone),
    )
    .await
    {
        Ok(s) => {
            tracing::info!("CronScheduler started");
            // 注入给 WebChannel（共享槽，build_router 读取快照填 AppState）
            web.set_cron_scheduler(s.clone());
            tracing::info!("CronHandle injected into WebChannel");
            // 注入给 CronTool，让 agent 能通过工具管理 cron 任务
            // （CronTool 需要的是下层 Arc<CronScheduler>，取其 scheduler 字段）
            if let Some(ct) = &cron_tool {
                ct.set_scheduler(s.scheduler.clone());
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
        _ = shutdown_signal.notified() => {
            tracing::info!("received /api/shutdown, shutting down");
            println!("\n{}", crate::banner::GOODBYE);
        }
    }

    // 共享清理逻辑（ADR-0018）：cron 调度器停止 + 各 channel task abort
    shutdown_serve(&_cron, &tasks).await;
    Ok(())
}

pub fn config_cmd(config_dir: &Path) -> Result<()> {
    let cfg = load_config_or_init(config_dir)?;
    println!("{}", toml::to_string_pretty(&cfg).unwrap_or_default());
    Ok(())
}

/// 诊断专用容错加载：`config.toml` **解析失败不阻断诊断**，回退内存默认值并把错误文本交回调用方。
/// `doctor_cmd` / `doctor_checks` 第一版的 `load_config_or_init(...)?` 会在用户手改坏配置的瞬间
/// 直接退出——那恰恰是最需要 doctor 的时刻，故此路径必须容错。
fn load_config_for_doctor(config_dir: &Path) -> (Config, Option<String>) {
    let config_path = config_dir.join("config.toml");
    if !config_path.exists() {
        return (
            Config::default_for_workspace(&config_dir.to_string_lossy()),
            None,
        );
    }
    match Config::load(&config_path) {
        Ok(cfg) => (cfg, None),
        Err(e) => (
            Config::default_for_workspace(&config_dir.to_string_lossy()),
            Some(format!("{:#}", e)),
        ),
    }
}

/// 模板类文件检查（config / cron / mcp / .env）：这些不依赖 config 是否可解析，
/// 坏配置下也必须报，且缺失项统一指向 `llaia init`（serve / chat 启动也会自动补齐）。
fn template_file_checks(config_dir: &Path, parse_err: Option<&str>) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    let config_path = config_dir.join("config.toml");
    checks.push(match parse_err {
        Some(e) => DoctorCheck::error(
            "config.toml",
            format!(
                "{}: {} — 手工修正语法后重跑；确认救不回再备份并 `llaia init --force` 重置模板",
                config_path.display(),
                // detail 保持单行（WebUI 表格内展示）；完整多行错误由 CLI 头部打印
                e.lines().next().unwrap_or(e)
            ),
        ),
        None if config_path.exists() => {
            DoctorCheck::ok("config.toml", config_path.display().to_string())
        }
        None => DoctorCheck::warn(
            "config.toml",
            "not found（下次 `llaia serve` / `llaia chat` 会自动生成模板，或显式 `llaia init`）",
        ),
    });

    let cron_path = config_dir.join("cron.toml");
    if cron_path.exists() {
        match crate::cron::CronConfig::load(&cron_path) {
            Ok(c) => checks.push(DoctorCheck::ok(
                "cron.toml",
                format!("{} ({} tasks)", cron_path.display(), c.task.len()),
            )),
            Err(e) => checks.push(DoctorCheck::error(
                "cron.toml",
                format!("{}: {}", cron_path.display(), e),
            )),
        }
    } else {
        checks.push(DoctorCheck::warn(
            "cron.toml",
            "not found（`llaia init` 可补齐模板，或等下次 serve/chat 自动生成）",
        ));
    }

    let mcp_path = config_dir.join("mcp.toml");
    if mcp_path.exists() {
        match crate::mcp::McpConfig::load(&mcp_path) {
            Ok(c) => checks.push(DoctorCheck::ok(
                "mcp.toml",
                format!("{} ({} servers)", mcp_path.display(), c.server.len()),
            )),
            Err(e) => checks.push(DoctorCheck::error(
                "mcp.toml",
                format!("{}: {}", mcp_path.display(), e),
            )),
        }
    } else {
        checks.push(DoctorCheck::warn(
            "mcp.toml",
            "not found（`llaia init` 可补齐模板，或等下次 serve/chat 自动生成）",
        ));
    }

    // .env 存在性（敏感信息自动化 P5 S1）；Unix 额外查权限位
    let env_path = config_dir.join(".env");
    if env_path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&env_path)
                .map(|m| m.permissions().mode())
                .unwrap_or(0);
            if mode & 0o077 != 0 {
                checks.push(DoctorCheck::warn(
                    ".env",
                    format!("permissions too open: {:o} (expected 0600)", mode & 0o777),
                ));
            } else {
                checks.push(DoctorCheck::ok(
                    ".env",
                    format!("{} (0600)", env_path.display()),
                ));
            }
        }
        #[cfg(not(unix))]
        checks.push(DoctorCheck::ok(".env", env_path.display().to_string()));
    } else {
        checks.push(DoctorCheck::warn(
            ".env",
            "not found (plaintext secrets stay in config.toml; save via WebUI or run /migrate-secrets)",
        ));
    }

    checks
}

pub async fn doctor_cmd(config_dir: &Path) -> Result<()> {
    let (cfg, parse_err) = load_config_for_doctor(config_dir);
    println!("config_dir: {}", config_dir.display());

    // 配置解析失败：provider / agent / context_size 检查全部失去依据（内存默认值不是用户本意），
    // 报完文件层检查即退出——但绝不 panic、绝不吞掉诊断。
    if let Some(e) = &parse_err {
        println!("\n[error] config.toml 解析失败: {}", e);
        println!(
            "        修复：手工改正语法后重跑 `llaia doctor`；确认救不回时备份该文件再 `llaia init --force` 重置模板"
        );
        println!("\n其余文件层检查（provider / agent 检查依赖有效配置，已跳过）:");
        for c in template_file_checks(config_dir, Some(e)) {
            if c.name == "config.toml" {
                continue; // 已在上方展开
            }
            println!("  [{}] {}: {}", c.status, c.name, c.detail);
        }
        return Ok(());
    }

    // 正常路径也报模板文件状态：缺失时指向 `llaia init`（serve / chat 启动会自动补齐）
    let config_path = config_dir.join("config.toml");
    if config_path.exists() {
        println!("config.toml: {}", config_path.display());
    } else {
        println!(
            "\n[warn] config.toml not found —— 下次 `llaia serve` / `llaia chat` 会自动生成模板（也可 `llaia init`）；以下按内置默认值诊断"
        );
    }

    let env_path = config_dir.join(".env");
    if env_path.exists() {
        println!(".env: {}", env_path.display());
    } else {
        println!(
            "\n[warn] .env not found —— 敏感字段将以明文留在 config.toml；在 WebUI 保存配置会自动改为 ${{VAR}} 引用，或跑 /migrate-secrets 迁移存量"
        );
    }

    println!("log.dir: {}", cfg.log.dir);
    println!(
        "runtime.context_threshold: {}",
        cfg.runtime.context_threshold
    );
    println!("runtime.max_iterations: {}", cfg.runtime.max_iterations);
    match &cfg.runtime.timezone {
        Some(tz) => {
            if crate::time::is_valid_tz(tz.trim()) {
                println!("runtime.timezone: {} (resolved)", tz);
            } else {
                println!(
                    "[warn] runtime.timezone '{}' is not a valid IANA timezone name; runtime falls back to system local time",
                    tz
                );
            }
        }
        None => println!("runtime.timezone: <unset> (follows system local time)"),
    }
    match &cfg.runtime.permission {
        Some(p) => println!("runtime.permission: {}", p),
        None => println!("runtime.permission: <unset> (effective: default)"),
    }
    println!(
        "runtime.keepalive_interval_secs: {}s",
        cfg.runtime.keepalive_interval_secs
    );
    println!(
        "runtime.max_turn_duration_secs: {}s",
        cfg.runtime.max_turn_duration_secs
    );

    // provider 配置检查：无 [provider.<id>] section 时 warn（不 error，serve 可降级启动）
    if cfg.provider.is_empty() {
        println!("\n[warn] No provider configured; llaia serve will start in degraded mode (chat unavailable, WebUI config usable)");
        println!(
            "       suggestion: 启动 `llaia serve` 后在 WebUI Config 页填写 provider，或编辑 {}/config.toml 取消注释 [provider.default]",
            config_dir.display()
        );
    } else {
        for (pid, p) in &cfg.provider {
            println!("\nprovider.{}: {}", pid, p.base_url);
            for (alias, m) in &p.model {
                println!(
                    "  model.{}: {} (native_tool_calling={})",
                    alias,
                    m.model,
                    native_label(m.native_tool_calling)
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

    // skills 检查（只扫描不种子，doctor 不创建目录）
    let skills_dir = config_dir.join("skills");
    if skills_dir.exists() {
        let skills = crate::skill::loader::scan_skills(&skills_dir);
        let active = skills.iter().filter(|s| s.active).count();
        println!(
            "\nskills/: {} ({} skills, {} active)",
            skills_dir.display(),
            skills.len(),
            active
        );
        for s in &skills {
            println!(
                "  - {} [{}] {}",
                s.name,
                if s.active { "active" } else { "inactive" },
                s.description
            );
        }
    } else {
        println!(
            "\nskills/: not found (built-in example skills are seeded on first chat/serve start)"
        );
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
                        model_alias,
                        m.model,
                        native_label(m.native_tool_calling)
                    );
                    match reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(5))
                        .build()
                        .map_err(|e| anyhow::anyhow!("failed to build http client: {}", e))?
                        .get(format!("{}/models", p.base_url.trim_end_matches('/')))
                        .send()
                        .await
                    {
                        Ok(resp) => println!("  /models status: {}", resp.status()),
                        Err(e) => println!("  /models error: {} (5s timeout)", e),
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

/// 单项诊断结果（WebUI /api/doctor 用）：status ∈ ok | warn | error。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: String,
    pub detail: String,
}

impl DoctorCheck {
    fn ok(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: "ok".into(),
            detail: detail.into(),
        }
    }
    fn warn(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: "warn".into(),
            detail: detail.into(),
        }
    }
    fn error(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: "error".into(),
            detail: detail.into(),
        }
    }
}

/// 结构化诊断（与 CLI `doctor_cmd` 同源的检查集，供 WebUI 展示）：
/// 模板文件（config/cron/mcp/.env）、provider 连通性、主模型链、context_size 探测、sessions.db、skills。
/// 网络探测带 5s 超时；任何单项失败不阻断其余检查。**config.toml 解析失败时仍返回文件层结果而非 Err**。
pub async fn doctor_checks(config_dir: &Path) -> Result<Vec<DoctorCheck>> {
    let (cfg, parse_err) = load_config_for_doctor(config_dir);
    let mut checks = Vec::new();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;

    // 文件层检查先行：坏配置下也必须报，且缺失项统一指向 `llaia init`
    checks.extend(template_file_checks(config_dir, parse_err.as_deref()));
    if parse_err.is_some() {
        // provider / agent / context_size 依赖有效配置（内存默认值不代表用户意图），就此为止
        return Ok(checks);
    }

    // provider 连通性：仅探测 openai_compatible（anthropic/gemini 的 /models 语义不同）
    if cfg.provider.is_empty() {
        checks.push(DoctorCheck::warn(
            "providers",
            "no provider configured; serve starts in degraded mode",
        ));
    }
    for (pid, p) in &cfg.provider {
        if p.provider_type != "openai_compatible" {
            checks.push(DoctorCheck::ok(
                &format!("provider.{pid}"),
                format!("type={} (connectivity not probed)", p.provider_type),
            ));
            continue;
        }
        let url = format!("{}/models", p.base_url.trim_end_matches('/'));
        match client.get(&url).send().await {
            Ok(resp) => checks.push(DoctorCheck::ok(
                &format!("provider.{pid}"),
                format!("{} → {} {}", p.base_url, resp.status(), url),
            )),
            Err(e) => checks.push(DoctorCheck::error(
                &format!("provider.{pid}"),
                format!("{} unreachable: {} ({})", p.base_url, e, url),
            )),
        }
    }

    // 主模型链 + context_size 探测
    match cfg.agent.get("main") {
        None => checks.push(DoctorCheck::warn("agent.main", "not configured")),
        Some(a) => {
            match crate::provider::provider_from_ref(&cfg, &a.model) {
                Ok(p) => {
                    checks.push(DoctorCheck::ok("agent.main.model", p.label()));
                    match p.detect_context_size().await {
                        Some(n) => {
                            checks.push(DoctorCheck::ok("context_size", format!("detected {n}")))
                        }
                        None => checks.push(DoctorCheck::warn(
                            "context_size",
                            "probe failed; falls back to configured value or default 8192 \
                             (set [provider.<id>].model.<alias>].context_size to override)",
                        )),
                    }
                }
                Err(e) => checks.push(DoctorCheck::error("agent.main.model", e.to_string())),
            }
            // sessions.db
            let db_path = a.derive_workspace(config_dir, "main").join("sessions.db");
            if db_path.exists() {
                let size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
                checks.push(DoctorCheck::ok(
                    "sessions.db",
                    format!("{} ({} bytes)", db_path.display(), size),
                ));
            } else {
                checks.push(DoctorCheck::warn(
                    "sessions.db",
                    format!(
                        "{} not found (created automatically on first start)",
                        db_path.display()
                    ),
                ));
            }
        }
    }

    // skills 数量
    let skills_dir = config_dir.join("skills");
    if skills_dir.exists() {
        let skills = crate::skill::loader::scan_skills(&skills_dir);
        let active = skills.iter().filter(|s| s.active).count();
        checks.push(DoctorCheck::ok(
            "skills",
            format!("{} skills, {} active", skills.len(), active),
        ));
    }

    Ok(checks)
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
    let tz = cfg.runtime.timezone.clone();
    let today = crate::time::now(&tz).ymd();
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

/// 共享清理逻辑（ADR-0018）：停止 cron 调度器，并 abort 所有 channel task。
/// 在 serve_cmd 退出前调用，保证 Ctrl+C 与 /api/shutdown 走同一收尾路径。
async fn shutdown_serve(
    cron: &Option<std::sync::Arc<crate::cron::CronHandle>>,
    tasks: &[tokio::task::JoinHandle<()>],
) {
    if let Some(sched) = cron {
        sched.request_stop();
        tracing::info!("cron scheduler stopped");
    }
    for h in tasks {
        h.abort();
    }
    tracing::info!("channel tasks aborted, serve exiting");
}

/// RAII guard：作用域结束时自动释放 PID 文件
struct PidGuard(crate::pid::PidFile);

impl Drop for PidGuard {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &std::path::Path, base_url: &str) {
        let toml = format!(
            "[provider.local]\n\
             type = \"openai_compatible\"\n\
             base_url = \"{base_url}\"\n\
             \n\
             [provider.local.default]\n\
             model = \"test-model\"\n\
             native_tool_calling = true\n\
             \n\
             [agent.main]\n\
             model = \"local.default\"\n"
        );
        std::fs::write(dir.join("config.toml"), toml).unwrap();
    }

    #[tokio::test]
    async fn doctor_reports_ok_when_provider_reachable() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("GET", "/models")
            .with_status(200)
            .with_body(r#"{"data":[]}"#)
            .create();
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), &server.url());
        let checks = doctor_checks(dir.path()).await.unwrap();
        let prov = checks.iter().find(|c| c.name == "provider.local").unwrap();
        assert_eq!(prov.status, "ok");
        // context_size 探测失败（无 /props）→ warn 而非 error
        let ctx = checks.iter().find(|c| c.name == "context_size").unwrap();
        assert_eq!(ctx.status, "warn");
        m.assert();
    }

    #[tokio::test]
    async fn doctor_reports_error_when_provider_unreachable() {
        // 保留端口的 server socket 已关闭 → 连接必失败
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "http://127.0.0.1:9");
        let checks = doctor_checks(dir.path()).await.unwrap();
        let prov = checks.iter().find(|c| c.name == "provider.local").unwrap();
        assert_eq!(prov.status, "error");
    }

    #[tokio::test]
    async fn doctor_warns_on_missing_env_and_sessions_db() {
        let mut server = mockito::Server::new_async().await;
        server.mock("GET", "/models").with_status(200).create();
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), &server.url());
        let checks = doctor_checks(dir.path()).await.unwrap();
        assert!(checks
            .iter()
            .any(|c| c.name == ".env" && c.status == "warn"));
        assert!(checks
            .iter()
            .any(|c| c.name == "sessions.db" && c.status == "warn"));
    }

    #[test]
    fn init_scaffold_creates_full_layout_on_fresh_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(init_scaffold(dir.path(), false).unwrap());
        for rel in [
            "config.toml",
            "cron.toml",
            "mcp.toml",
            ".env",
            "workspace/SOUL.md",
            "workspace/USER.md",
            "workspace/MEMORY.md",
            "workspace/uploads",
            "workspace/subagent",
            "logs",
        ] {
            assert!(dir.path().join(rel).exists(), "missing {rel}");
        }
    }

    #[test]
    fn init_scaffold_is_idempotent_and_preserves_user_edits() {
        let dir = tempfile::tempdir().unwrap();
        init_scaffold(dir.path(), false).unwrap();
        let custom = "[agent.main]\nmodel = \"my.provider\"\n";
        std::fs::write(dir.path().join("config.toml"), custom).unwrap();

        assert!(!init_scaffold(dir.path(), false).unwrap());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.toml")).unwrap(),
            custom
        );
    }

    #[test]
    fn init_scaffold_force_overwrites_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        init_scaffold(dir.path(), false).unwrap();
        std::fs::write(dir.path().join("config.toml"), "garbage").unwrap();

        assert!(init_scaffold(dir.path(), true).unwrap());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.toml")).unwrap(),
            CONFIG_TEMPLATE
        );
    }

    #[test]
    fn prepare_startup_dir_migrates_old_layout_before_scaffolding() {
        // 旧版散落文件（无 .migrated_v0.2 标记）：迁移必须先于模板补齐，
        // 否则 workspace/SOUL.md 会被模板占据、旧文件被遮蔽
        let dir = tempfile::tempdir().unwrap();
        let old_soul = "# my precious soul\n";
        std::fs::write(dir.path().join("SOUL.md"), old_soul).unwrap();

        prepare_startup_dir(dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("workspace/SOUL.md")).unwrap(),
            old_soul
        );
        assert!(!dir.path().join("SOUL.md").exists());
        // 其余模板仍被补齐
        assert!(dir.path().join("config.toml").exists());
    }

    #[tokio::test]
    async fn doctor_survives_unparsable_config_and_reports_it_as_error() {
        // 回归：config.toml 语法坏掉时 doctor 必须继续诊断（曾直接返回 Err 退出）
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "this is [not valid toml [[[ model =\n",
        )
        .unwrap();

        let checks = doctor_checks(dir.path()).await.unwrap();
        let cfg_check = checks.iter().find(|c| c.name == "config.toml").unwrap();
        assert_eq!(cfg_check.status, "error");
        assert!(cfg_check.detail.contains("llaia init --force"));
        // 依赖有效配置的探测应被跳过，而不是拿内存默认值去冒充用户配置
        assert!(
            !checks.iter().any(|c| c.name.starts_with("provider.")),
            "provider probes must be skipped when config is unparsable"
        );
        assert!(!checks.iter().any(|c| c.name == "agent.main.model"));
    }

    #[tokio::test]
    async fn doctor_points_missing_template_files_to_init() {
        // 全新目录（无任何模板文件）：缺失项报 warn 并给出可执行的修复指引
        let dir = tempfile::tempdir().unwrap();
        let checks = doctor_checks(dir.path()).await.unwrap();

        for name in ["config.toml", "cron.toml", "mcp.toml"] {
            let c = checks
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("missing check {name}"));
            assert_eq!(c.status, "warn", "{name} should warn when absent");
            assert!(
                c.detail.contains("llaia init"),
                "{name} detail should point to `llaia init`: {}",
                c.detail
            );
        }
    }

    #[tokio::test]
    async fn doctor_reports_ok_for_existing_template_files() {
        let dir = tempfile::tempdir().unwrap();
        init_scaffold(dir.path(), false).unwrap();
        let checks = doctor_checks(dir.path()).await.unwrap();
        for name in ["config.toml", "cron.toml", "mcp.toml"] {
            let c = checks.iter().find(|c| c.name == name).unwrap();
            assert_eq!(c.status, "ok", "{name}: {}", c.detail);
        }
    }
}
