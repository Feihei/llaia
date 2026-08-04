use crate::agent::runner::ToolRegistry;
use crate::agent::sink::{OutputSink, run_turn};
use crate::agent::Agent;
use crate::agent::AgentRegistry;
use crate::channels::Channel;
use crate::commands::slash::{try_handle, SlashOutcome};
use crate::config::{AgentConfig, Config};
use crate::memory::sqlite::SessionStore;
use crate::memory::{ensure_template, load_md, MEMORY_TEMPLATE, SOUL_TEMPLATE, USER_TEMPLATE};
use crate::provider::openai_compat::OpenAiCompatibleProvider;
use crate::provider::Provider;
use crate::tool_call::build_tool_instructions;
use crate::tools::delegate::DelegateTool;
use crate::tools::file::{FileEdit, FileRead, FileWrite};
use crate::tools::memory::MemoryWrite;
use crate::tools::send_media::{SendFile, SendImage};
use crate::tools::tavily::TavilySearch;
use crate::tools::terminal::Terminal;
use crate::tools::web::WebFetch;
use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

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
        println!("(੭aᴗa)੭ Llaia - Come On~\nllaia v0.1.0 - type /help for commands, /exit to quit, /stop to interrupt\n");
        println!("生成中可继续输入，/stop 立即中断，Ctrl+C 紧急中断\n");

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
                        if !trimmed.is_empty() {
                            if stdin_tx.blocking_send(Some(trimmed)).is_err() {
                                break;
                            }
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
                    // 空闲态 Ctrl+C：等价于 /exit，优雅退出
                    _ = tokio::signal::ctrl_c() => {
                        println!("\n[goodbye]");
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
                    try_handle(&line, &mut *a).await?
                };
                match outcome {
                    SlashOutcome::Exit => break,
                    SlashOutcome::Handled(msg) => {
                        println!("{}", msg);
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
                    parts.push(crate::provider::ContentPart::Text {
                        text: text.clone(),
                    });
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
        Ok(())
    }
}

/// 构建单个 Agent 实例。返回 (Agent, 可能的 delegate 工具)
/// is_main=true 且 config 有子 Agent 时，挂载 delegate 工具并返回其引用用于后续注入 registry
async fn build_single_agent(
    config: &Config,
    alias: &str,
    agent_cfg: AgentConfig,
    is_main: bool,
) -> Result<(Arc<Mutex<Agent>>, Option<Arc<DelegateTool>>)> {
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
    tracing::info!(
        agent = alias,
        workspace = %workspace.display(),
        soul_path = %soul_path.display(),
        soul_len = soul.len(),
        user_len = user.len(),
        memory_len = memory.len(),
        "loaded soul/user/memory"
    );

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

    // 构建完整工具集
    let mut all_tools: Vec<Arc<dyn Tool>> = Vec::new();
    all_tools.push(Arc::new(FileRead::new(workspace.clone())));
    all_tools.push(Arc::new(FileWrite::new(workspace.clone())));
    all_tools.push(Arc::new(FileEdit::new(workspace.clone())));
    all_tools.push(Arc::new(Terminal::new(
        config.tools.terminal.confirm.clone(),
        config.tools.terminal.whitelist.clone(),
        workspace.clone(),
    )));
    all_tools.push(Arc::new(WebFetch::new()?));
    if !config.tools.tavily.api_key.is_empty() {
        all_tools.push(Arc::new(TavilySearch::new(
            config.tools.tavily.api_key.clone(),
        )?));
    }
    all_tools.push(Arc::new(MemoryWrite::new(memory_path.clone())));
    all_tools.push(Arc::new(SendImage::new(workspace.clone())));
    all_tools.push(Arc::new(SendFile::new(workspace.clone())));

    // 按 denied_tools 过滤
    let denied: std::collections::HashSet<&str> = agent_cfg
        .denied_tools
        .iter()
        .map(|s| s.as_str())
        .collect();
    let mut delegate_tool: Option<Arc<DelegateTool>> = None;
    let mut registry = ToolRegistry::new();
    for tool in all_tools {
        if !denied.contains(tool.name()) {
            registry.register(tool);
        }
    }

    // main Agent 且配置了子 Agent 时挂 delegate 工具
    let has_delegate = is_main && config.agent.len() > 1;
    if has_delegate {
        let d = Arc::new(DelegateTool::new(agent_cfg.delegate_timeout));
        registry.register(d.clone());
        delegate_tool = Some(d);
    }

    let mut system_prompt = format!(
        "# SOUL\n{}\n\n# USER\n{}\n\n# MEMORY\n{}\n\n# WORKSPACE\n{}\n\n工作目录说明：所有工具的相对路径都相对于 WORKSPACE 解析；terminal 命令在 WORKSPACE 下执行。需要写到其它位置时请使用绝对路径。",
        soul, user, memory, workspace.display()
    );
    // 标签降级模式下注入 tool instructions 到 system prompt。
    // 注意：有 delegate 工具时，OnceCell registry 尚未注入（在 build_agent 末尾才注入），
    // 此时 delegate 的 enum 为空。所以推迟到 build_agent 里 set_registry 后再注入。
    if !provider.native_tool_calling() && !has_delegate {
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
    let detected = provider.detect_context_size().await;
    let context_size = match (model_cfg.context_size, detected) {
        (Some(cfg), Some(det)) => cfg.min(det),
        (Some(cfg), None) => cfg,
        (None, Some(det)) => det,
        (None, None) => 8192,
    };
    tracing::info!(
        agent = alias,
        configured = ?model_cfg.context_size,
        detected = ?detected,
        final = context_size,
        "context_size resolved"
    );

    let agent = Agent::new(
        config,
        provider,
        registry,
        session_store,
        session_id,
        system_prompt,
        context_size,
        workspace.clone(),
    )
    .await;

    Ok((Arc::new(Mutex::new(agent)), delegate_tool))
}

/// 构建 AgentRegistry（main + 所有子 Agent）
pub async fn build_agent(config: &Config) -> Result<Arc<AgentRegistry>> {
    // 构建子 Agent（跳过 main）
    let mut sub_agents: Vec<(String, Arc<Mutex<Agent>>)> = Vec::new();
    for (alias, cfg) in &config.agent {
        if alias == "main" {
            continue;
        }
        let (agent, _) = build_single_agent(config, alias, cfg.clone(), false).await?;
        sub_agents.push((alias.clone(), agent));
    }

    // 构建 main Agent
    let main_cfg = config
        .agent
        .get("main")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("agent.main not configured"))?;
    let (main_agent, delegate_tool) = build_single_agent(config, "main", main_cfg, true).await?;

    let mut registry = AgentRegistry::new(main_agent);
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
        if !a.provider.native_tool_calling() {
            let instructions = build_tool_instructions(&a.tools.specs());
            a.context.system.push_str(&instructions);
        }
    }

    tracing::info!(
        sub_agents = registry.available_sub_agents().len(),
        "AgentRegistry built"
    );
    Ok(registry)
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
