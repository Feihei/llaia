pub mod slash;

use anyhow::Result;
use std::path::Path;
use std::path::PathBuf;

use crate::config::{Config, WebUiConfig};

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
# compact_model = "qwen"  # optional: cheaper model for context compaction; defaults to main model
# vision_model = "gpt-4o"  # optional: model to describe images when main model lacks multimodal; defaults to sending images to main model
# ask_user_timeout_secs = 300       # optional: blocking ask_user clarification timeout (seconds)
# tool_result_cap = 32768           # optional: max chars per tool result text (truncated beyond; full result kept in sessions.db)
# keepalive_interval_secs = 600     # optional: "still working" heartbeat interval for long tasks (seconds)
# max_turn_duration_secs = 3600     # optional: hard stop for a single turn (seconds)
# cron_allow_inline_interpreter = false  # optional: let cron channels run inline interpreter commands (python -c etc.) without forced approval; out-of-scope ops stay denied. Default false
#
# Security hardening (ADR-0033)
# snapshot_enabled = true          # optional: workspace snapshots (SOUL/USER/MEMORY, sessions.db, uploads) to <config_dir>/snapshots/ before writes + periodic sweep; agent-proof read-only. Default true
# snapshot_retention_days = 14     # optional: snapshot retention window; older timestamped dirs are GC'd daily. Default 14
# scrub_child_env = true           # optional: strip inherited env vars matching *KEY*/*SECRET*/*TOKEN* from terminal/MCP child processes. Default true
#
# Generation Guard: output degeneration defense for small local models
# (repetition loops, runaway thinking, empty replies). On detection the
# stream is aborted, the partial output discarded, and one retry is made
# with a [guard] hint and thinking disabled. Consecutive failures append
# a warning telling you to adjust inference-side sampling params.
# output_guard = true              # optional: master switch, set false to disable entirely
# guard_repeat_window = 512        # optional: repetition detection sliding window (chars)
# guard_repeat_gram = 24           # optional: repetition detection n-gram length (chars)
# guard_repeat_threshold = 4       # optional: max occurrences of the same n-gram in window before abort
# guard_thinking_cap = 32000       # optional: thinking stream char cap (<think> block + reasoning_content); 0 = unlimited
# guard_max_retries = 1            # optional: retries after degeneration (with [guard] hint, thinking disabled)
# guard_breaker_threshold = 2      # optional: consecutive degenerated turns before a prominent warning

[log]
level = "info"
dir = "~/.llaia/logs"

# Providers: the unified connection registry. `type` picks the family:
#   llm family     (openai_compatible / anthropic / gemini) — hosts [model.<id>] entries, probeable
#   service family (search)                                 — pure credentials for search/extract, no models
# An explicit unknown type is a startup error; omitting `type` defaults to openai_compatible.
#
# Local Ollama example:
# [provider.default]
# type = "openai_compatible"
# base_url = "http://localhost:11434/v1"
# api_key = "${OLLAMA_API_KEY}"  # or leave empty
#
# Models: the global catalog. Each [model.<id>] entry references a provider.
# kind = chat (default) | tts | image | embedding — omitted kind means chat.
#
# [model.qwen]
# provider = "default"
# model = "qwen2.5:7b"           # the server-side model name
# native_tool_calling = false
# context_size = 32768           # optional; unset: local endpoints are probed, others
#                                # assume an optimistic 128000 and shrink reactively
#                                # when the provider rejects an oversized request
# capabilities = ["multimodal"]  # optional capability flags (v1: multimodal only)
# enabled = false                # optional; defaults to true. Keep the params on file but
#                                # hide the model from /models and the WebUI pickers.
#                                # Explicit references still work (e.g. an agent.model
#                                # already pointing here keeps running until you switch)
#
# [model.qwen.thinking]          # optional; per-model thinking capability (P1/P2,
#                                # docs/plans/2026-09-10-thinking-capability-model.md).
#                                # All fields default to unknown/unset: /reasoning then
#                                # rejects levels and legacy-falls-back for off. Fill in
#                                # from wire probes only — dialects bind to deployment,
#                                # never guess from the model name.
# default = "unknown"            # model's resting level: none|low|medium|high|max|unknown
# level_wire = "reasoning_effort"  # how levels are sent: reasoning_effort | none (no knob)
# off_wire = "reasoning_effort_none"  # how "off" is sent: enable_thinking_false |
#                                  # thinking_disabled | reasoning_effort_none | unsupported
# preserve = false               # echo reasoning_content back verbatim (default off;
#                                # enable only if the server verifiably consumes it)
#
# Cloud Anthropic example (also works with a gateway base_url). Note: anthropic/gemini
# providers only accept kind = "chat" model entries.
# [provider.claude]
# type = "anthropic"
# api_key = "${ANTHROPIC_API_KEY}"
#
# [model.sonnet]
# provider = "claude"
# model = "claude-sonnet-4-20250514"
# max_tokens = 8192              # required for Anthropic; defaults to 4096 if unset
#
# Search services (service family): one `search` type, `platform` picks the wire
# adapter (tavily | baidu | brave). Pure credentials, no model entries, never appear
# in the add-model probe list. Referenced by [tools.search].provider below.
# [provider.tv]
# type = "search"
# platform = "tavily"
# api_key = "${TAVILY_API_KEY}"
# [provider.bd]
# type = "search"
# platform = "baidu"
# api_key = "${BAIDU_API_KEY}"
# [provider.br]
# type = "search"
# platform = "brave"
# api_key = "${BRAVE_API_KEY}"

# Main Agent: leave model empty to enter degraded mode (no provider, WebUI config only)
# After configuring a provider + model above, set e.g. "qwen" to enable chat
# fallback = ["qwen"]            # optional: model id chain tried in order when the main model fails
# workspace / soul / user / memory fields are deprecated; auto-resolved to ~/.llaia/workspace/
[agent.main]
model = ""

# Sub-agent example (uncomment to enable; workspace auto-resolves to ~/.llaia/workspace/subagent/<alias>/)
# [agent.coder]
# model = "qwen"
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
interpret_inline = "approval"   # approval (default): force /ok approval for interpreter inline code like `python -c`, even inside workspace; off disables the gate

[tools.search]
provider = ""                  # provider id reference (service family, e.g. "tv" configured above); empty = no search tool
top_k = 8                      # default number of results

[tools.tts]                    # OpenAI-compatible /audio/speech via the model catalog
enabled = false
model = ""                     # model id of a kind = "tts" entry (endpoint/key come from its provider)
voice = "alloy"                # default voice (call parameter, not a model attribute)
[tools.image_gen]              # OpenAI-compatible /images/generations + /images/edits via the model catalog
enabled = false                # sd-server (stable-diffusion.cpp), agnes, OpenAI, ...
model = ""                     # model id of a kind = "image" entry; its `size` field is the default output size
timeout_secs = 300             # local diffusion can be slow

[tools.web_fetch]
max_chars = 20000
extract_provider = ""          # optional service-family provider id (tavily) for server-side extraction
# Domain allowlist (ADR-0033 egress gate): constrains NON-interactive channels only
# (cron/delegate/mail). Empty = fail-closed (non-interactive channels cannot fetch at all).
# Match is exact-or-subdomain, case-insensitive. Interactive channels (cli/web/qq/...)
# instead approve new domains once via the normal approval flow; approvals persist to
# web_domains.json. SSRF checks (private/loopback/metadata IPs) are always on and not configurable.
# allowed_domains = ["wikipedia.org", "news.ycombinator.com"]
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
# Tip: when an agent-mode task fetches web pages, consider lowering [tools.web_fetch]
# max_chars in config.toml (e.g. 4000~5000) — large page dumps blow up the context
# and can trap the agent in a "re-fetch the same URL" loop until it times out.

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

/// 启动路径共享的目录准备：先迁移旧结构，再幂等补齐 init 模板，升级占位符画像，之后才加载配置。
/// 顺序约束：迁移必须先于模板补齐（否则模板会遮蔽待迁移的旧版散落文件），
/// 补齐必须先于配置加载（否则新生成的 config.toml 模板不会被本次加载，走了内存默认值）。
/// 画像升级排在补齐之后：本次刚生成的模板已是当前版本，比对不命中即零成本。
fn prepare_startup_dir(config_dir: &Path) -> Result<()> {
    crate::migrate::migrate_if_needed(config_dir)?;
    if init_scaffold(config_dir, false)? {
        tracing::info!(dir = %config_dir.display(), "scaffolded missing init templates");
    }
    if crate::migrate::refresh_placeholder_templates(config_dir)? {
        tracing::info!("profile placeholders upgraded to the current SOUL/USER template");
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

/// `llaia serve --host/--port` 的覆盖值。刻意**只影响绑定**：`config.toml` 与
/// WebUI Config 页（读写 `live_config` 里那份文件值）都不被污染——否则页面会显示
/// 一个并非来自配置文件的端口，用户下次保存时就把它静默写回盘上。
#[derive(Debug, Clone, Default)]
pub struct WebBindOverride {
    pub host: Option<String>,
    pub port: Option<u16>,
}

impl WebBindOverride {
    pub fn is_none(&self) -> bool {
        self.host.is_none() && self.port.is_none()
    }
}

/// 把 CLI 覆盖值套到文件配置上，得到「实际绑定用」的那份 WebUI 配置。
/// 纯函数，便于单测覆盖优先级（CLI > 文件）。
pub fn effective_webui(file: &WebUiConfig, bind: &WebBindOverride) -> WebUiConfig {
    let mut eff = file.clone();
    if let Some(host) = bind.host.as_deref() {
        eff.host = host.to_string();
    }
    if let Some(port) = bind.port {
        eff.port = port;
    }
    eff
}

/// 浏览器可访问的主机：`0.0.0.0` / `::` 是「监听所有网卡」，不是可点开的地址，
/// 打开浏览器时要换成回环地址（连接探测同理）。
fn browser_host(host: &str) -> &str {
    match host {
        "0.0.0.0" | "" => "127.0.0.1",
        "::" => "::1",
        h => h,
    }
}

/// 拼 WebUI 浏览器 URL：IPv6 主机加方括号；token 非空时以 `?token=` 附带——
/// 前端 app.js 会读取 query token 存入 localStorage，实现打开即免登录。
fn webui_browser_url(host: &str, port: u16, token: &str) -> String {
    let host = browser_host(host);
    let base = if host.contains(':') {
        format!("http://[{host}]:{port}")
    } else {
        format!("http://{host}:{port}")
    };
    if token.is_empty() {
        base
    } else {
        format!("{base}/?token={}", percent_encode_component(token))
    }
}

/// query 值的极简 percent-encode：RFC 3986 unreserved 之外全部转义。
/// 随机 token 是 hex 不受影响；显式配置的 token 可能含 `&`/`#` 等 URL 元字符，
/// 不转义会被浏览器截断 query。
fn percent_encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 端口是否已有服务在听（1s 超时；解析失败/连接拒绝/超时都视为未监听）。
async fn webui_port_open(host: &str, port: u16) -> bool {
    let addr = format!("{host}:{port}");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        tokio::net::TcpStream::connect(&addr),
    )
    .await
    .map(|res| res.is_ok())
    .unwrap_or(false)
}

/// 用系统默认浏览器打开 URL（open crate：Windows 走 ShellExecuteW）。
/// spawn_blocking 包一层，不阻塞 tokio worker。
async fn open_in_browser(url: &str) -> std::io::Result<()> {
    let url = url.to_string();
    match tokio::task::spawn_blocking(move || open::that_detached(&url)).await {
        Ok(res) => res,
        Err(e) => Err(std::io::Error::other(e)),
    }
}

/// WebUI token 解析（serve_cmd 进程级收口）：配置非空直接用并清掉随机值
/// （用户显式改了 token）；为空用进程级随机 token——get_or_insert_with 保证
/// 只生成一次、跨 reload 复用，修掉旧实现每轮 build_router 重新生成导致
/// 浏览器 localStorage 登录态失效的问题。纯函数便于单测。
fn resolve_webui_token(config_token: &str, random_token: &mut Option<String>) -> String {
    if config_token.is_empty() {
        random_token
            .get_or_insert_with(|| {
                let t = crate::web::generate_token();
                tracing::info!("WebUI token (randomly generated): {}", t);
                t
            })
            .clone()
    } else {
        *random_token = None;
        config_token.to_string()
    }
}

/// 等 WebUI 端口就绪后用系统浏览器打开（`--open`；无参数/双击启动自动生效）。
/// 由 serve_cmd 在首轮 serve_once 之前调用，每进程至多一次——放 serve_cmd 层
/// 而非 serve_once 层，/api/restart reload 不会重开浏览器标签。
fn spawn_browser_opener(host: &str, port: u16, token: String) {
    let probe_host = browser_host(host).to_string();
    tokio::spawn(async move {
        // WebChannel bind 自带 10×1s 重试，这里放宽到 20s 覆盖首启偏慢的机器
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if webui_port_open(&probe_host, port).await {
                let url = webui_browser_url(&probe_host, port, &token);
                match open_in_browser(&url).await {
                    Ok(()) => println!("  🌐 WebUI opened in browser: {url}"),
                    Err(e) => {
                        tracing::warn!(error = %e, url = %url, "failed to open system browser")
                    }
                }
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    host = %probe_host,
                    port,
                    "WebUI not reachable within 20s, browser not opened"
                );
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    });
}

/// 守护进程模式：启动所有非 CLI 的后台频道（QQ、未来 WebUI 等），不启动终端交互
/// serve 子系统一轮的退出原因（reload loop 的循环控制）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServeExit {
    /// 真退出：Ctrl+C 或 /api/shutdown
    Shutdown,
    /// 进程内重载：/api/restart —— 重读 config 并重建全部子系统，进程不退出
    Reload,
}

pub async fn serve_cmd(config_dir: &Path, bind: WebBindOverride, open_browser: bool) -> Result<()> {
    prepare_startup_dir(config_dir)?;
    let mut config = load_config_or_init(config_dir)?;

    // tracing 全局 subscriber 只能安装一次：reload 不跟随日志级别/目录变更（改日志需真重启）。
    let log_dir = PathBuf::from(&config.log.dir);
    let _ = crate::log::init(&config.log.level, &log_dir);

    // 与 chat 共用同一份欢迎 billboard（见 crate::banner）
    print!("{}", crate::banner::billboard());
    println!("  background service mode: QQ / WebUI channels, press Ctrl+C to quit\n");

    // `--open`（无参数/双击启动默认开启）：目标端口已有 WebUI 在听时不再起第二实例——
    // 那只会 bind 失败重试 10s 后报错；直接打开浏览器指向已有实例并退出。
    // 显式配置 token 时附带（同一份 config.toml，另一个实例认得）；随机 token 属于
    // 那个进程、外界拿不到，此时裸 URL，靠浏览器 localStorage 的既有登录态。
    if open_browser {
        let eff = effective_webui(&config.webui, &bind);
        if webui_port_open(browser_host(&eff.host), eff.port).await {
            let url = webui_browser_url(&eff.host, eff.port, &eff.token);
            if let Err(e) = open_in_browser(&url).await {
                tracing::warn!(error = %e, url = %url, "failed to open system browser");
            }
            println!("  llaia is already serving — WebUI: {url}");
            println!("  opened it in your browser; this process exits now.\n");
            return Ok(());
        }
    }

    let pid_file = crate::pid::PidFile::new(config_dir);
    pid_file.acquire()?;
    let _pid_guard = PidGuard(pid_file);

    // token 解析上提到进程级：配置非空用配置值，为空生成一次随机值并跨 reload 复用
    // （旧实现在 build_router 每轮重新生成，/api/restart 后浏览器 localStorage 的
    // 登录态即失效）。reload 时若用户改了显式 token 则跟随新值。
    let mut random_token: Option<String> = None;
    if open_browser {
        // 首轮 token 供浏览器 URL 用（loop 内首轮解析结果与此一致）
        let token = resolve_webui_token(&config.webui.token, &mut random_token);
        let eff = effective_webui(&config.webui, &bind);
        spawn_browser_opener(&eff.host, eff.port, token);
    }

    // Reload loop（zeroclaw 式，2026-09-26）：/api/restart 不再 spawn 替代进程，
    // 而是把整轮子系统推倒重建——agent 注册表/工具注册（含 image_gen 等）/MCP/
    // 频道/cron/web listener 全部从新 config 实例化。PID 与终端保持不变，
    // 观感等同 Ctrl+C 后重新 `llaia serve`；改坏 config 会在终端报错退出（可见）。
    loop {
        let webui_token = resolve_webui_token(&config.webui.token, &mut random_token);
        let exit = serve_once(config_dir, &config, &bind, webui_token).await?;
        match exit {
            ServeExit::Shutdown => return Ok(()),
            ServeExit::Reload => {
                // 先落盘重读（失败则带终端可见的错误退出，绝不带旧配置假装成功）
                config = load_config_or_init(config_dir)?;
                tracing::info!("config reloaded: all subsystems rebuilt in-process");
                println!("  ↻ config reloaded — channels, tools and MCP rebuilt (same process)\n");
            }
        }
    }
}

/// serve 主体的一轮：构建 registry/频道/cron/web，运行到 Ctrl+C、/api/shutdown
/// 或 /api/restart。reload 场景下整个函数随外层 loop 重跑，无跨轮共享状态
/// （进程级单例除外：tracing subscriber、PID 文件、随机 WebUI token——见 serve_cmd）。
async fn serve_once(
    config_dir: &Path,
    config: &Config,
    bind: &WebBindOverride,
    webui_token: String,
) -> Result<ServeExit> {
    let mut webui = effective_webui(&config.webui, bind);
    // token 由 serve_cmd 解析后注入（配置值或进程级随机值），WebChannel 不再自行生成
    webui.token = webui_token;
    warn_plaintext_secrets(config_dir);

    let (registry, cron_tool, mcp_registry) =
        crate::channels::cli::build_agent(config, config_dir).await?;

    // serve 模式：让主 agent 与 WebChannel 共享同一份 live_config，
    // 这样 WebUI 修改 [runtime].timezone 后下一轮对话即可生效（ADR-0017 热更新）。
    let live_config = std::sync::Arc::new(tokio::sync::RwLock::new(config.clone()));
    {
        let mut a = registry.main.lock().await;
        a.attach_live_config(live_config.clone());
    }
    // WebUI 优雅停止/重载信号：/api/shutdown 与 /api/restart 各自触发，
    // serve_once 的 select! 监听后分别走 Shutdown / Reload（ADR-0018 + reload loop）。
    let shutdown_signal = std::sync::Arc::new(tokio::sync::Notify::new());
    let reload_signal = std::sync::Arc::new(tokio::sync::Notify::new());

    // serve 模式：无 provider 时 warn 但继续启动（WebUI 配置功能不依赖 provider，聊天降级提示）
    {
        let a = registry.main.lock().await;
        if !a.has_provider().await {
            tracing::warn!(
                "No provider configured; chat is unavailable. Configure [provider.default] in WebUI (http://{}:{})",
                webui.host,
                webui.port
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
    // 登录进度共享视图先建：channel 写、WebChannel 读（Config 页微信卡片显示二维码）
    let wechat_login = std::sync::Arc::new(tokio::sync::RwLock::new(
        crate::channels::wechat::WechatLoginView::default(),
    ));
    let wechat_pusher_for_cron: Option<std::sync::Arc<dyn crate::cron::ProactivePusher>> =
        if config.channels.wechat.enabled {
            let wx = std::sync::Arc::new(
                crate::channels::wechat::WechatChannel::new(
                    config.channels.wechat.clone(),
                    config_dir.to_path_buf(),
                )
                .with_login_view(wechat_login.clone()),
            );
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
        webui.clone(),
        registry.clone(),
        live_config.clone(),
        config_path,
        workspace.clone(),
        shutdown_signal.clone(),
        reload_signal.clone(),
        wechat_login.clone(),
    ));
    web.set_mcp_registry(mcp_registry);
    // cron_tool 注入 WebChannel，供热加载 cron 时重新指向新调度器（P4-f）
    web.set_cron_tool(cron_tool.clone());
    let web_pusher_for_cron: std::sync::Arc<dyn crate::cron::ProactivePusher> = web.clone();
    let web_host = webui.host.clone();
    let web_port = webui.port;
    if !bind.is_none() {
        // 覆盖必须可见：否则「页面里写着 51217、实际听 51220」看起来像 bug
        tracing::info!(
            cli_host = ?bind.host,
            cli_port = ?bind.port,
            file_host = %config.webui.host,
            file_port = config.webui.port,
            "webui bind overridden by CLI (config.toml and the Config page keep the file values)"
        );
    }

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
            shutdown_serve(&_cron, &tasks).await;
            return Ok(ServeExit::Shutdown);
        }
        _ = shutdown_signal.notified() => {
            tracing::info!("received /api/shutdown, shutting down");
            println!("\n{}", crate::banner::GOODBYE);
            shutdown_serve(&_cron, &tasks).await;
            return Ok(ServeExit::Shutdown);
        }
        _ = reload_signal.notified() => {
            tracing::info!("received /api/restart, reloading in place");
        }
    }

    // 共享清理逻辑（ADR-0018）：cron 调度器停止 + 各 channel task abort。
    // reload 与 shutdown 走同一条清理路径——WS 连接断开、前端 3s 重连兜底，
    // 端口释放后 serve_once 下一轮重新 bind（bind 自带 10×1s 重试兜底竞态）。
    shutdown_serve(&_cron, &tasks).await;
    Ok(ServeExit::Reload)
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
                "{}: {} — fix the syntax by hand and re-run; if it is unrecoverable, back this file up and reset the template with `llaia init --force`",
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
            "not found (the next `llaia serve` / `llaia chat` generates templates automatically, or run `llaia init`)",
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
            "not found (`llaia init` adds the template, or the next serve/chat generates it automatically)",
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
            "not found (`llaia init` adds the template, or the next serve/chat generates it automatically)",
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
        println!("\n[error] failed to parse config.toml: {}", e);
        println!(
            "        fix: correct the syntax by hand and re-run `llaia doctor`; if it is unrecoverable, back this file up, then `llaia init --force` to reset the template"
        );
        println!(
            "\nremaining file-level checks (provider / agent checks need a valid config, skipped):"
        );
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
            "\n[warn] config.toml not found - the next `llaia serve` / `llaia chat` generates templates automatically (or run `llaia init`); diagnosing against built-in defaults below"
        );
    }

    let env_path = config_dir.join(".env");
    if env_path.exists() {
        println!(".env: {}", env_path.display());
    } else {
        println!(
            "\n[warn] .env not found - sensitive fields stay in config.toml as plaintext; saving from the WebUI Config page moves them into ${{VAR}} references automatically, or run /migrate-secrets for existing ones"
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
            "       suggestion: run `llaia serve` and fill in a provider on the WebUI Config page, or edit {}/config.toml and uncomment [provider.default]",
            config_dir.display()
        );
    } else {
        for (pid, p) in &cfg.provider {
            println!(
                "\nprovider.{}: {} (type={})",
                pid,
                p.base_url,
                p.effective_type()
            );
        }
        for (mid, m) in &cfg.models {
            // /models 不列 disabled 模型，这里不标出来会显得两处视图不一致
            let hidden = if m.enabled { "" } else { " [disabled]" };
            println!(
                "  model.{}: {} (provider={}, kind={}, native_tool_calling={}){}",
                mid,
                m.model,
                m.provider,
                m.kind.as_str(),
                native_label(m.native_tool_calling),
                hidden
            );
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

    // 解析 model 目录条目，展示 provider 端点
    match cfg.models.get(&agent_cfg.model) {
        Some(m) => {
            if let Some(p) = cfg.provider.get(&m.provider) {
                println!("\nprovider.{}: {}", m.provider, p.base_url);
                println!(
                    "  model.{}: {} (kind={} native_tool_calling={})",
                    agent_cfg.model,
                    m.model,
                    m.kind.as_str(),
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
                println!("\n[warn] provider.{} not configured", m.provider);
            }
        }
        None => println!(
            "\n[warn] model '{}' not configured (missing [model.{}])",
            agent_cfg.model, agent_cfg.model
        ),
    }

    // T2 自检（ADR-0033 L2）：报告运行账户与提权状态，指引部署手册
    let priv_info = crate::privilege::elevation_info();
    println!("\nsecurity:");
    println!("  account: {}", priv_info.account);
    match priv_info.elevated {
        Some(true) => {
            println!("  privilege: elevated (High/System integrity or root)");
            println!(
                "  [warn] running with elevated privileges - prefer a dedicated low-privilege account per T2 (docs/guide/security-hardening.md)"
            );
        }
        Some(false) => {
            println!("  privilege: not elevated (medium integrity / non-root)");
            println!(
                "  hint: for full T2 hardening run under a dedicated low-privilege account (docs/guide/security-hardening.md)"
            );
        }
        None => {
            println!("  privilege: unknown (probe failed)");
        }
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

    // provider 连通性：service family 只报配置态（/models 语义不存在），
    // anthropic/gemini 的 /models 语义不同也只报配置态
    if cfg.provider.is_empty() {
        checks.push(DoctorCheck::warn(
            "providers",
            "no provider configured; serve starts in degraded mode",
        ));
    }
    for (pid, p) in &cfg.provider {
        if p.effective_type() != "openai_compatible" {
            let extra = if p.is_service() {
                format!(" platform={}", p.search_platform().unwrap_or("?"))
            } else {
                String::new()
            };
            checks.push(DoctorCheck::ok(
                &format!("provider.{pid}"),
                format!(
                    "type={}{} (connectivity not probed)",
                    p.effective_type(),
                    extra
                ),
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
            match crate::provider::model_from_ref(&cfg, &a.model) {
                Ok(p) => {
                    checks.push(DoctorCheck::ok("agent.main.model", p.label()));
                    // 三态：显式配置 > 探测命中 > 双皆无（回退乐观默认，需用户知情）
                    let configured = cfg.models.get(&a.model).and_then(|m| m.context_size);
                    match (configured, p.detect_context_size().await) {
                        (Some(n), _) => {
                            checks.push(DoctorCheck::ok("context_size", format!("configured {n}")))
                        }
                        (None, Some(n)) => {
                            checks.push(DoctorCheck::ok("context_size", format!("detected {n}")))
                        }
                        (None, None) => checks.push(DoctorCheck::warn(
                            "context_size",
                            format!(
                                "not configured and probe failed; falling back to optimistic \
                                 default {} — set [model.<id>].context_size \
                                 if the real window differs (overflow errors shrink it \
                                 at runtime)",
                                crate::agent::DEFAULT_CONTEXT_SIZE
                            ),
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

    // T2 自检（ADR-0033 L2）：提权状态探测，探测失败降级为 ok/unknown 而非 error
    let priv_info = crate::privilege::elevation_info();
    match priv_info.elevated {
        Some(true) => checks.push(DoctorCheck::warn(
            "security.privilege",
            format!(
                "running as '{}' with elevated privileges; prefer a dedicated low-privilege \
                 account per T2 (docs/guide/security-hardening.md)",
                priv_info.account
            ),
        )),
        Some(false) => checks.push(DoctorCheck::ok(
            "security.privilege",
            format!(
                "running as '{}', not elevated (T2 deployment guide: \
                 docs/guide/security-hardening.md)",
                priv_info.account
            ),
        )),
        None => checks.push(DoctorCheck::ok(
            "security.privilege",
            format!(
                "account '{}' (elevation unknown; probe failed) — T2 guide: \
                 docs/guide/security-hardening.md",
                priv_info.account
            ),
        )),
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

    /// `--host/--port` 只喂给绑定，**不得**改写文件态：WebUI Config 页读写的是
    /// `live_config` 里那份，一旦被覆盖值污染，用户下次保存就把临时端口静默落盘。
    #[test]
    fn web_bind_override_never_mutates_file_config() {
        let file = WebUiConfig {
            host: "127.0.0.1".into(),
            port: 51217,
            token: "t".into(),
        };
        let bind = WebBindOverride {
            host: Some("0.0.0.0".into()),
            port: Some(51220),
        };
        let eff = effective_webui(&file, &bind);
        assert_eq!((eff.host.as_str(), eff.port), ("0.0.0.0", 51220));
        assert_eq!(
            (file.host.as_str(), file.port),
            ("127.0.0.1", 51217),
            "文件值必须原样"
        );
        assert_eq!(eff.token, "t", "token 不参与覆盖");
    }

    #[test]
    fn web_bind_override_is_per_field() {
        let file = WebUiConfig {
            host: "127.0.0.1".into(),
            port: 51217,
            token: String::new(),
        };
        // 只给 port：host 沿用文件值（不是回落到某个默认地址）
        let only_port = effective_webui(
            &file,
            &WebBindOverride {
                host: None,
                port: Some(8080),
            },
        );
        assert_eq!(
            (only_port.host.as_str(), only_port.port),
            ("127.0.0.1", 8080)
        );
        // 无覆盖：逐字段等于文件值，且 is_none 为真（决定是否打那条 INFO）
        assert_eq!(effective_webui(&file, &Default::default()).port, 51217);
        assert!(WebBindOverride::default().is_none());
        assert!(!WebBindOverride {
            host: None,
            port: Some(1)
        }
        .is_none());
    }

    /// 浏览器 URL：通配监听地址要换成回环地址（0.0.0.0/:: 本身不可点开），
    /// IPv6 主机要加方括号。
    #[test]
    fn browser_url_normalizes_wildcard_hosts() {
        assert_eq!(
            webui_browser_url("0.0.0.0", 51217, ""),
            "http://127.0.0.1:51217"
        );
        assert_eq!(webui_browser_url("::", 51217, ""), "http://[::1]:51217");
        assert_eq!(
            webui_browser_url("127.0.0.1", 51217, ""),
            "http://127.0.0.1:51217"
        );
        assert_eq!(
            webui_browser_url("192.168.1.5", 8080, ""),
            "http://192.168.1.5:8080"
        );
    }

    /// token 非 URL 安全字符时必须 percent-encode：`&`/`#` 是 query 分隔符，
    /// 不转义会被浏览器截断，前端拿到的 token 就错了。
    #[test]
    fn browser_url_appends_percent_encoded_token() {
        assert_eq!(
            webui_browser_url("127.0.0.1", 51217, "abc123"),
            "http://127.0.0.1:51217/?token=abc123"
        );
        assert_eq!(
            webui_browser_url("127.0.0.1", 51217, "a&b#c d"),
            "http://127.0.0.1:51217/?token=a%26b%23c%20d"
        );
        // 空 token（随机 token 未知的 attach 场景）不附带 query
        assert_eq!(
            webui_browser_url("127.0.0.1", 51217, ""),
            "http://127.0.0.1:51217"
        );
    }

    /// 随机 token 跨 reload 复用（同一进程内多次解析得到同一个值）；
    /// 显式配置 token 直接生效并清掉随机值（用户改了 token，下轮生效）。
    #[test]
    fn token_resolution_stable_within_process() {
        let mut random = None;
        let first = resolve_webui_token("", &mut random);
        assert!(!first.is_empty());
        assert_eq!(resolve_webui_token("", &mut random), first);
        assert_eq!(random.as_deref(), Some(first.as_str()));

        assert_eq!(resolve_webui_token("fixed", &mut random), "fixed");
        assert!(random.is_none());
        // 显式 token 清空后再回到空配置：允许重新生成新随机值
        let again = resolve_webui_token("", &mut random);
        assert!(!again.is_empty());
    }

    fn write_config(dir: &std::path::Path, base_url: &str) {
        let toml = format!(
            "[provider.local]\n\
             type = \"openai_compatible\"\n\
             base_url = \"{base_url}\"\n\
             \n\
             [model.local-default]\n\
             provider = \"local\"\n\
             model = \"test-model\"\n\
             native_tool_calling = true\n\
             \n\
             [agent.main]\n\
             model = \"local-default\"\n"
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

    #[test]
    fn prepare_startup_dir_works_when_state_dir_missing() {
        // 回归：全新机器上 ~/.llaia 尚不存在，裸 `llaia serve` 首启动曾直接崩溃——
        // migrate_if_needed 在无旧文件分支写 .migrated_v0.2 标记时父目录缺失，
        // Windows 报 os error 3（ERROR_PATH_NOT_FOUND），Unix 报 ENOENT。
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join(".llaia");
        assert!(!fresh.exists());

        prepare_startup_dir(&fresh).unwrap();

        assert!(fresh.join(".migrated_v0.2").exists());
        assert!(fresh.join("config.toml").exists());
        assert!(fresh.join("workspace/SOUL.md").exists());
        assert!(fresh.join("logs").is_dir());
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
