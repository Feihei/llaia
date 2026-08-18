use crate::agent::runner::ToolRegistry;
use crate::agent::sink::{run_turn, OutputSink};
use crate::agent::Agent;
use crate::agent::AgentRegistry;
use crate::channels::Channel;
use crate::commands::slash::{try_handle, SlashOutcome};
use crate::config::{AgentConfig, Config};
use crate::memory::sqlite::SessionStore;
use crate::memory::trim::trim_memory_to_budget;
use crate::memory::{ensure_template, load_md, MEMORY_TEMPLATE, SOUL_TEMPLATE, USER_TEMPLATE};
use crate::provider::Provider;
use crate::tool_call::build_tool_instructions;
use crate::tools::cron::CronTool;
use crate::tools::delegate::DelegateTool;
use crate::tools::file::{FileEdit, FileRead, FileWrite};
use crate::tools::memory::MemoryWrite;
use crate::tools::search::UnifiedSearch;
use crate::tools::send_media::{SendFile, SendImage};
use crate::tools::terminal::Terminal;
use crate::tools::web::WebFetch;
use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, RwLock};

/// CLI 输出 sink：即时打印到 stdout
struct CliSink;

#[async_trait]
impl OutputSink for CliSink {
    async fn on_chunk(&mut self, delta: &str) {
        print!("{}", delta);
        let _ = std::io::stdout().flush();
    }
    async fn on_tool_start(&mut self, name: &str) {
        println!("\n[tool: {}]", name);
    }
    async fn on_tool_result(&mut self, output: &str) {
        // 200 字符边界安全截断（与原 cli.rs 行为一致）
        let preview = if output.len() > 200 {
            let mut end = 200;
            while end > 0 && !output.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}...(truncated)", &output[..end])
        } else {
            output.to_string()
        };
        println!("[result: {}]", preview);
    }
    async fn on_media(&mut self, path: &str, kind: crate::agent::MediaKind) {
        let label = match kind {
            crate::agent::MediaKind::Image => "image",
            crate::agent::MediaKind::File => "file",
        };
        println!("[sent {}: {}]", label, path);
    }
    async fn on_done(&mut self) {
        println!("\n");
    }
    async fn on_error(&mut self, message: &str) {
        println!("\n[error: {}]\n", message);
    }
    async fn on_interrupted(&mut self) {
        println!("\n[stopped]");
    }
}

pub struct CliChannel;

impl Default for CliChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl CliChannel {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Channel for CliChannel {
    async fn run(self: Arc<Self>, registry: Arc<AgentRegistry>) -> Result<()> {
        let agent = registry.main.clone();
        // 缓存 workspace 路径，用于解析 @path 图片引用的相对路径
        let workspace = {
            let a = agent.lock().await;
            a.workspace.clone()
        };
        // 欢迎 billboard 与 serve 共用同一份文案（见 crate::banner）
        print!("{}", crate::banner::billboard());

        // 后台读 stdin 的 task：把每行输入送到 mpsc，EOF 时发 None
        let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Option<String>>(16);
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            loop {
                let mut line = String::new();
                match stdin.lock().read_line(&mut line) {
                    Ok(0) => {
                        let _ = stdin_tx.blocking_send(None);
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line.trim().to_string();
                        if !trimmed.is_empty() && stdin_tx.blocking_send(Some(trimmed)).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = stdin_tx.blocking_send(None);
                        break;
                    }
                }
            }
        });

        // 待处理输入队列：生成中收到的普通输入排入此处
        let mut queued_inputs: Vec<String> = Vec::new();

        loop {
            if queued_inputs.is_empty() {
                print!("> ");
            } else {
                print!("> ({} queued) ", queued_inputs.len());
            }
            std::io::stdout().flush()?;

            // 取下一条输入：优先队列，否则等 stdin
            let line = if let Some(q) = queued_inputs.first().cloned() {
                queued_inputs.remove(0);
                q
            } else {
                tokio::select! {
                    input = stdin_rx.recv() => match input {
                        Some(Some(l)) => l,
                        Some(None) | None => break, // EOF
                    },
                    // 空闲态 Ctrl+C：等价于 /exit，优雅退出（退出语在循环结束后统一打印）
                    _ = tokio::signal::ctrl_c() => {
                        break;
                    }
                }
            };

            // /stop 在空闲态无意义
            if line == "/stop" {
                println!("[no active generation to stop]");
                continue;
            }

            // 斜杠命令（非 /stop）：同步处理
            if line.starts_with('/') {
                let outcome = {
                    let mut a = agent.lock().await;
                    try_handle(&line, &mut a, Some(registry.clone())).await?
                };
                match outcome {
                    SlashOutcome::Exit => break,
                    SlashOutcome::Handled(msg) => {
                        println!("{}", msg);
                        continue;
                    }
                    SlashOutcome::Resume { notice, message } => {
                        // 审批已解决：先回显结果摘要，再跑一轮 continuation turn
                        // 让模型基于工具结果继续（message 即工具输出的用户消息）。
                        println!("{}", notice);
                        let stop = Arc::new(Notify::new());
                        let sink = Box::new(CliSink);
                        let agent_clone = agent.clone();
                        registry.set_delivery(Some(crate::tools::delegate::DeliveryTarget::Stdout));
                        let _ = run_turn(
                            agent_clone,
                            crate::provider::ChatMessage::user(&message),
                            "cli".into(),
                            sink,
                            stop.clone(),
                        )
                        .await;
                        continue;
                    }
                    SlashOutcome::NotSlash => {} // 不会到这里（已 starts_with '/'）
                }
            }

            // 普通输入：解析 @path 图片引用，构造消息
            // @path/to/image.jpg 语法：@ 开头的 token 视为图片路径
            // 相对路径相对于 agent workspace 解析（与 file_read 等工具一致）
            let mut image_paths = Vec::new();
            let mut text_parts = Vec::new();
            for token in line.split_whitespace() {
                if let Some(p) = token.strip_prefix('@') {
                    image_paths.push(p.to_string());
                } else {
                    text_parts.push(token);
                }
            }
            let text = text_parts.join(" ");

            // 构造消息：有图片则多模态，否则纯文本
            let user_msg = if image_paths.is_empty() {
                Some(crate::provider::ChatMessage::user(&line))
            } else {
                let mut parts: Vec<crate::provider::ContentPart> = Vec::new();
                if !text.is_empty() {
                    parts.push(crate::provider::ContentPart::Text { text: text.clone() });
                }
                for img_path in &image_paths {
                    // 相对路径解析到 workspace；绝对路径直接用（resolve_within 内部处理）
                    let resolved = match crate::tools::file::resolve_within(&workspace, img_path) {
                        Ok(p) => p,
                        Err(e) => {
                            println!("[failed to resolve {}: {}]", img_path, e);
                            continue;
                        }
                    };
                    if !crate::image_utils::is_image_file(&resolved) {
                        println!("[skip {}: not an image]", img_path);
                        continue;
                    }
                    match crate::image_utils::prepare_image_for_vision(&resolved) {
                        Ok(data_url) => {
                            println!("[loaded image: {}]", img_path);
                            parts.push(crate::provider::ContentPart::ImageUrl {
                                image_url: crate::provider::ImageUrlContent { url: data_url },
                            });
                        }
                        Err(e) => {
                            println!("[failed to load {}: {}]", img_path, e);
                        }
                    }
                }
                if parts.is_empty() {
                    None // 没有有效内容，跳过
                } else {
                    Some(crate::provider::ChatMessage::user_multimodal(parts))
                }
            };

            let user_msg = match user_msg {
                Some(m) => m,
                None => continue,
            };

            // 用 run_turn 跑这一轮，sink 即时打印
            let stop = Arc::new(Notify::new());
            let sink = Box::new(CliSink);
            let agent_clone = agent.clone();
            registry.set_delivery(Some(crate::tools::delegate::DeliveryTarget::Stdout));
            let mut turn_handle = tokio::spawn(run_turn(
                agent_clone,
                user_msg,
                "cli".into(),
                sink,
                stop.clone(),
            ));

            println!(); // 换行，分隔 prompt 和回复
            print!(">> "); // 生成态提示符：可输入排队或 /stop（与回复同行）
            std::io::stdout().flush()?;

            // 生成态：select 监听 turn 结束 / stdin 输入 / Ctrl+C
            loop {
                tokio::select! {
                    // Ctrl+C：触发中断，等 run_turn 自己结束
                    _ = tokio::signal::ctrl_c() => {
                        stop.notify_one();
                    }
                    // stdin 有输入：/stop 中断，其他排入队列
                    input = stdin_rx.recv() => {
                        match input {
                            Some(Some(l)) if l == "/stop" => {
                                stop.notify_one();
                            }
                            Some(Some(l)) => {
                                println!("[queued: {}]", l);
                                queued_inputs.push(l);
                            }
                            Some(None) | None => {
                                // stdin EOF：等当前 turn 结束
                            }
                        }
                    }
                    res = &mut turn_handle => {
                        // run_turn 结束（正常/中断/错误都走 sink 回调已打印）
                        match res {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => tracing::error!(error = %e, "run_turn failed"),
                            Err(e) => tracing::error!(error = %e, "run_turn task panicked"),
                        }
                        break;
                    }
                }
            }
        }
        // /exit、EOF、空闲 Ctrl+C 等所有退出路径统一在此打印退出语
        println!("\n{}", crate::banner::GOODBYE);
        Ok(())
    }
}

/// 构建单个 Agent 实例。返回 (Agent, 可能的 delegate 工具, 可能的 cron 工具)
/// is_main=true 且 config 有子 Agent 时，挂载 delegate 工具并返回其引用用于后续注入 registry
/// is_main=true 时挂载 cron_task 工具并返回其引用用于后续注入 scheduler
#[allow(clippy::too_many_arguments)]
pub async fn build_single_agent(
    config: &Config,
    config_dir: &std::path::Path,
    alias: &str,
    agent_cfg: AgentConfig,
    is_main: bool,
    audit: Option<Arc<crate::audit::AuditLog>>,
    mcp_tools: Vec<Arc<dyn Tool>>,
    skills: &[crate::skill::SkillManifest],
) -> Result<(
    Arc<Mutex<Agent>>,
    Option<Arc<DelegateTool>>,
    Option<Arc<CronTool>>,
    PathBuf,
)> {
    // workspace 自动推导（忽略配置中的 workspace 字段）
    let workspace = agent_cfg.derive_workspace(config_dir, alias);
    std::fs::create_dir_all(&workspace).ok();
    // 与文件/终端工具共享的工作区根（P4-d /move 一处更新、所有工具即时生效）
    let workspace_root = Arc::new(RwLock::new(workspace.clone()));

    let soul_path = workspace.join("SOUL.md");
    let user_path = workspace.join("USER.md");
    let memory_path = workspace.join("MEMORY.md");

    ensure_template(&soul_path, SOUL_TEMPLATE).await?;
    ensure_template(&memory_path, MEMORY_TEMPLATE).await?;

    // USER.md 同步：子 agent 启动时从主 agent 复制覆盖
    if !is_main {
        let main_user = config_dir.join("workspace").join("USER.md");
        if main_user.exists() {
            ensure_template(&main_user, USER_TEMPLATE).await?;
            tokio::fs::copy(&main_user, &user_path).await?;
            tracing::info!(agent = alias, "synced USER.md from main agent");
        } else {
            ensure_template(&user_path, USER_TEMPLATE).await?;
        }
    } else {
        ensure_template(&user_path, USER_TEMPLATE).await?;
    }

    let soul = load_md(&soul_path).await?;
    let user = load_md(&user_path).await?;
    let raw_memory = load_md(&memory_path).await?;
    tracing::info!(
        agent = alias,
        workspace = %workspace.display(),
        soul_path = %soul_path.display(),
        soul_len = soul.len(),
        user_len = user.len(),
        memory_len = raw_memory.len(),
        "loaded soul/user/memory"
    );

    // 尝试构建 provider：model 为空 → 降级模式（provider = None）
    // model 非空但 provider/model 配置缺失 → 报错（用户意图配置但配错）
    // fallback 链：主模型失败时按序降级（fallback 项缺失仅 warn 不阻塞）
    let (provider, model_cfg, has_delegate) = if agent_cfg.model.is_empty() {
        tracing::warn!(
            agent = alias,
            "agent.model not set, entering degraded mode (no provider)"
        );
        (None, None, false)
    } else {
        let (prov_id, model_alias) = Config::parse_model_ref(&agent_cfg.model)?;
        let model_cfg = config
            .provider
            .get(prov_id)
            .and_then(|p| p.model.get(model_alias))
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("provider.{}.model.{} not configured", prov_id, model_alias)
            })?;

        let provider =
            crate::provider::build_provider_chain(&agent_cfg.model, &agent_cfg.fallback, config)?;
        (provider, Some(model_cfg), is_main && config.agent.len() > 1)
    };
    let provider_ref = provider.as_ref();

    // 构建 compact_provider（独立于主 provider，用更便宜的模型跑上下文压缩）
    // compact_model 未配置 / 解析失败 / provider 不存在 → None（回退到主 provider）
    let compact_provider: Option<Arc<dyn Provider>> = match &config.runtime.compact_model {
        Some(m) if !m.is_empty() => match build_compact_provider(config, m) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(model = m.as_str(), error = %e, "build compact_provider failed, falling back to main provider");
                None
            }
        },
        _ => None,
    };

    // 构建 vision_provider（独立于主 provider，用于描述图片）
    // vision_model 未配置 / 解析失败 → None（图片直接发给主模型）
    let vision_provider: Option<Arc<dyn Provider>> = match &config.runtime.vision_model {
        Some(m) if !m.is_empty() => match crate::provider::provider_from_ref(config, m) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(model = m.as_str(), error = %e, "build vision_provider failed, images will be sent to main provider");
                None
            }
        },
        _ => None,
    };

    // MEMORY 预算裁剪（ADR-0025）：超限时旧段经 compact_provider 摘要、或硬截断保留近期。
    // 裁剪结果进入 system_prompt_base 与 init_system_meta 缓存，全频道共享且热重载稳定。
    let memory = trim_memory_to_budget(
        &raw_memory,
        agent_cfg.memory_token_budget,
        compact_provider.as_ref(),
    )
    .await;
    tracing::debug!(
        agent = alias,
        memory_trimmed_len = memory.len(),
        memory_token_budget = agent_cfg.memory_token_budget,
        "trimmed MEMORY to budget"
    );

    // 构建完整工具集（用新字段）
    let skills_dir = config_dir.join("skills");
    // 规划后执行（ADR-0024）：每会话一份 todo 清单，agent 与工具共享同一 TodoStore。
    let todo_store = Arc::new(crate::tools::todo::TodoStore::new(workspace.clone()));
    let mut all_tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(FileRead::new(
            workspace_root.clone(),
            is_main,
            Some(skills_dir.clone()),
        )),
        Arc::new(FileWrite::new(workspace_root.clone(), is_main)),
        Arc::new(FileEdit::new(workspace_root.clone(), is_main)),
        Arc::new(Terminal::new(
            config.tools.terminal.command_policy.clone(),
            config.tools.terminal.command_whitelist.clone(),
            workspace_root.clone(),
        )),
        Arc::new({
            // web_fetch 正文抽取：若启用且配置了 Tavily key，则复用其做服务端抽取。
            let tavily = if config.tools.web_fetch.use_tavily_extract
                && !config.tools.tavily.api_key.is_empty()
            {
                Some(Arc::new(
                    crate::tools::search::tavily::TavilyProvider::new(
                        config.tools.tavily.api_key.clone(),
                    )?,
                ))
            } else {
                None
            };
            WebFetch::new(config.tools.web_fetch.max_chars, tavily)?
        }),
        Arc::new(
            MemoryWrite::new(memory_path.clone(), user_path.clone(), is_main)
                .with_timezone(config.runtime.timezone.clone()),
        ),
        Arc::new(SendImage::new(workspace.clone())),
        Arc::new(SendFile::new(workspace.clone())),
        // todo 工具无条件注册（无需 api_key）：agent 自管当前会话子步骤清单。
        Arc::new(crate::tools::todo::TodoTool::new(todo_store.clone())),
        // ask_user 工具无条件注册（无需 api_key）：agent 执行中主动向用户抛问题并阻塞等待。
        Arc::new(crate::tools::ask_user::AskUserTool),
    ];
    if let Some(search_tool) = UnifiedSearch::build(&config.tools)? {
        all_tools.push(search_tool);
    }
    // TTS（P5 T1）：enabled 且有 api_key 时注册 tts 工具（合成到 workspace/tts/）。
    if let Some(tts_tool) = crate::tools::tts::TtsTool::build(&config.tools.tts, workspace.clone())?
    {
        all_tools.push(tts_tool);
    }
    // skill 自管工具（ADR-0027）：仅 main agent 注册。
    // agent 通过它们直接写/改 skill 目录（落在 workspace 之外，file_write 够不到）。
    if is_main {
        all_tools.push(Arc::new(crate::tools::skill_create::SkillCreateTool::new(
            config_dir.to_path_buf(),
            workspace.clone(),
        )));
        all_tools.push(Arc::new(crate::tools::skill_edit::SkillEditTool::new(
            config_dir.to_path_buf(),
            workspace.clone(),
        )));
        // 长期目标工具（ADR-0021）：仅 main agent 注册，落盘到 agent 家目录 goal.md。
        all_tools.push(Arc::new(crate::tools::goal::GoalTool::new(
            workspace.clone(),
        )));
    }
    // MCP 工具（共享 registry，受下方 denied_tools 过滤）
    all_tools.extend(mcp_tools);

    // 按 denied_tools 过滤
    let denied: std::collections::HashSet<&str> =
        agent_cfg.denied_tools.iter().map(|s| s.as_str()).collect();
    let mut delegate_tool: Option<Arc<DelegateTool>> = None;
    let mut cron_tool: Option<Arc<CronTool>> = None;
    let mut registry = ToolRegistry::new();
    // 用真实（带 workspace 落盘）的 TodoStore 替换默认禁用态。
    registry.todo_store = todo_store;
    for tool in all_tools {
        if !denied.contains(tool.name()) {
            registry.register(tool);
        }
    }

    // main Agent 且配置了子 Agent 时挂 delegate 工具
    if has_delegate {
        let d = Arc::new(DelegateTool::new(agent_cfg.delegate_timeout));
        registry.register(d.clone());
        delegate_tool = Some(d);
    }

    // main Agent 挂载 cron_task 工具（动态管理定时任务，scheduler 在 serve_cmd 启动后注入）
    if is_main {
        let c = Arc::new(CronTool::new());
        registry.register(c.clone());
        cron_tool = Some(c);
    }

    let system_prompt_base = format!(
        "# SOUL\n{}\n\n# USER\n{}\n\n# MEMORY\n{}\n\n# WORKSPACE\n{}\n\n工作目录说明：所有工具的相对路径都相对于 WORKSPACE 解析；terminal 命令在 WORKSPACE 下执行。需要写到其它位置时请使用绝对路径。",
        soul, user, memory, workspace.display()
    );
    // Skills（P3-e）：Progressive Disclosure，只注入 active skill 的 name + description + 路径
    let skills_prompt = crate::skill::prompt::build_skills_prompt(skills);
    let mut system_prompt = system_prompt_base.clone();
    if !skills_prompt.is_empty() {
        system_prompt.push_str("\n\n");
        system_prompt.push_str(&skills_prompt);
    }
    // 标签降级模式下注入 tool instructions 到 system prompt。
    // 注意：有 delegate 工具时，OnceCell registry 尚未注入（在 build_agent 末尾才注入），
    // 此时 delegate 的 enum 为空。所以推迟到 build_agent 里 set_registry 后再注入。
    let has_tool_instructions = provider_ref
        .map(|p| !p.native_tool_calling())
        .unwrap_or(false);
    if has_tool_instructions && !has_delegate {
        system_prompt.push_str(&build_tool_instructions(&registry.specs()));
    }
    let registry = Arc::new(registry);

    let db_path = workspace.join("sessions.db");
    let session_store = Arc::new(SessionStore::open(&db_path)?);

    let session_id = match session_store.latest_session()? {
        Some((id, _)) => id,
        None => {
            let uuid = uuid::Uuid::new_v4().to_string();
            session_store.create_session(&uuid, alias)?
        }
    };

    // context_size: min(配置值, 探测值)，探测不到用配置值，都没有用默认 8192
    // 降级模式（无 provider）直接用 8192
    let context_size = match (
        model_cfg.as_ref().and_then(|m| m.context_size),
        provider_ref,
    ) {
        (Some(cfg), Some(p)) => {
            let detected = p.detect_context_size().await;
            match detected {
                Some(det) => cfg.min(det),
                None => cfg,
            }
        }
        (Some(cfg), None) => cfg,
        (None, Some(p)) => p.detect_context_size().await.unwrap_or(8192),
        (None, None) => 8192,
    };
    tracing::info!(
        agent = alias,
        configured = ?model_cfg.as_ref().and_then(|m| m.context_size),
        final = context_size,
        degraded = provider.is_none(),
        "context_size resolved"
    );

    let mut agent = Agent::new(
        config,
        provider,
        compact_provider,
        vision_provider,
        registry,
        session_store,
        session_id,
        system_prompt,
        context_size,
        workspace.clone(),
        workspace_root,
        config_dir.to_path_buf(),
        is_main,
        alias.to_string(),
        audit,
    )
    .await;
    // 记录 system 前缀与 tool-instructions 标记，供热加载 skills 时重建
    agent.init_system_meta(system_prompt_base, has_tool_instructions);

    // 环境探测（P5 E1）：仅 main agent 启动时探测一次，注入 Runtime Context；
    // 子 agent（委派任务）不探测，避免启动开销。/env 命令可手动刷新。
    if is_main {
        let env_text = crate::envprobe::probe().await;
        agent.context.env_state = (!env_text.is_empty()).then_some(env_text);
    }

    Ok((
        Arc::new(Mutex::new(agent)),
        delegate_tool,
        cron_tool,
        workspace,
    ))
}

/// 构建 AgentRegistry（main + 所有子 Agent）
/// 返回 (registry, 可能的 cron 工具引用)。
/// cron_tool 在 serve_cmd 启动 CronScheduler 后通过 set_scheduler 注入。
pub async fn build_agent(
    config: &Config,
    config_dir: &std::path::Path,
) -> Result<(
    Arc<AgentRegistry>,
    Option<Arc<CronTool>>,
    Arc<crate::mcp::client::McpRegistry>,
)> {
    // 审计日志
    let log_dir = PathBuf::from(&config.log.dir);
    let audit = Arc::new(crate::audit::AuditLog::new(&log_dir));

    // MCP：加载 mcp.toml，连接所有 enabled server（单个失败 log + 跳过，不阻塞启动）
    let mcp_path = config_dir.join("mcp.toml");
    let mcp_cfg = crate::mcp::McpConfig::load(&mcp_path)?;
    let mcp_registry =
        Arc::new(crate::mcp::client::McpRegistry::connect_all(&mcp_cfg.server).await);
    let mut mcp_tools: Vec<Arc<dyn Tool>> = Vec::new();
    for (prefixed, def) in mcp_registry.tool_defs().await {
        mcp_tools.push(Arc::new(crate::tools::mcp::McpTool::new(
            prefixed,
            def,
            mcp_registry.clone(),
        )));
    }
    if !mcp_tools.is_empty() {
        tracing::info!(tools = mcp_tools.len(), "MCP tools loaded");
    }

    // Skills（P3-e）：扫描 <config_dir>/skills/，首次运行时种子内置示例并同步 skills.json
    let skills = crate::skill::loader::load_skills(&config_dir.join("skills"));
    let active_skills = skills.iter().filter(|s| s.active).count();
    if !skills.is_empty() {
        tracing::info!(
            total = skills.len(),
            active = active_skills,
            "skills loaded"
        );
    }

    // 构建子 Agent（跳过 main）
    let mut sub_agents: Vec<(String, Arc<Mutex<Agent>>)> = Vec::new();
    for (alias, cfg) in &config.agent {
        if alias == "main" {
            continue;
        }
        let (agent, _, _, _) = build_single_agent(
            config,
            config_dir,
            alias,
            cfg.clone(),
            false,
            Some(audit.clone()),
            mcp_tools.clone(),
            &skills,
        )
        .await?;
        sub_agents.push((alias.clone(), agent));
    }

    // 构建 main Agent
    // 若 [agent.main] 未配置（init 模板默认状态），用空 model 构造降级 agent
    let main_cfg = config.agent.get("main").cloned().unwrap_or_else(|| {
        tracing::warn!(
            "[agent.main] not configured, main agent entering degraded mode (no provider)"
        );
        AgentConfig {
            model: String::new(),
            workspace: String::new(),
            soul: None,
            user: None,
            memory: None,
            denied_tools: Vec::new(),
            delegate_timeout: 120,
            fallback: Vec::new(),
            memory_token_budget: crate::config::default_memory_token_budget(),
        }
    });
    let (main_agent, delegate_tool, cron_tool, main_workspace) = build_single_agent(
        config,
        config_dir,
        "main",
        main_cfg,
        true,
        Some(audit.clone()),
        mcp_tools,
        &skills,
    )
    .await?;

    let registry = AgentRegistry::new(main_agent, main_workspace);
    for (alias, agent) in sub_agents {
        registry.register_sub_agent(alias, agent);
    }
    let registry = Arc::new(registry);

    // 注入 registry 给 delegate 工具（OnceCell 延迟注入）
    if let Some(d) = delegate_tool {
        d.set_registry(registry.clone());

        // 标签降级模式下，set_registry 后 delegate 的 enum 才有值，
        // 此时重新生成 tool instructions 追加到 main agent 的 system prompt
        let mut a = registry.main.lock().await;
        if let Some(p) = a.provider_snapshot().await {
            if !p.native_tool_calling() {
                let instructions = build_tool_instructions(&a.tools.specs());
                a.context.system.push_str(&instructions);
            }
        }
    }

    tracing::info!(
        sub_agents = registry.available_sub_agents().len(),
        "AgentRegistry built"
    );
    Ok((registry, cron_tool, mcp_registry))
}

/// 根据 "provider_id.model_alias" 引用从 config 构建 compact_provider。
/// compact_model 未配置 / provider 不存在 / model 不存在 → Err（调用方降级处理）
fn build_compact_provider(
    config: &Config,
    model_ref: &str,
) -> anyhow::Result<Option<Arc<dyn Provider>>> {
    Ok(Some(crate::provider::provider_from_ref(config, model_ref)?))
}
