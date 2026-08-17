pub mod approval;
pub mod context;
pub mod registry;
pub mod runner;
pub mod sink;

pub use crate::agent::registry::AgentRegistry;

use crate::agent::context::Context;
use crate::agent::runner::{execute_tool_calls, ToolRegistry};
use crate::config::Config;
use crate::memory::sqlite::SessionStore;
use crate::provider::{
    ChatMessage, ChatRequest, ContentPart, ImageUrlContent, MessageContent, Provider, Role,
    StreamEvent,
};
use anyhow::Result;
use futures_util::StreamExt;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

/// Agent turn 事件（推给 channel 消费）
#[derive(Debug, Clone)]
pub enum TurnEvent {
    /// 文本增量（已过滤掉 tool_call 标签）
    Chunk { delta: String },
    /// 工具调用开始
    ToolStart { id: String, name: String },
    /// 工具执行结果
    ToolResult { id: String, output: String },
    /// 工具请求发送媒体给用户（channel 负责实际发送）
    MediaOutput { path: String, kind: MediaKind },
    /// 整轮结束（所有文本和工具调用完成）
    Done,
    /// 错误（已生成的文本保留，错误追加）
    Error { message: String },
}

/// 媒体类型：图片或文件
#[derive(Debug, Clone, Copy, Serialize)]
pub enum MediaKind {
    Image,
    File,
}

pub struct Agent {
    /// Provider 实例（可热替换）。
    /// - `Some(p)`：正常模式
    /// - `None`：降级模式（无 provider，handle_message_streaming 直接 sink Error）
    ///
    /// RwLock 保护：turn 开始时拿 snapshot（读锁），reload_provider 拿写锁替换。
    /// 正在进行的 turn 持有 snapshot 不受 reload 影响。
    pub provider: Arc<RwLock<Option<Arc<dyn Provider>>>>,
    /// 压缩上下文用的独立 provider（可选）。
    /// 未配置时复用 `provider`（兼容旧行为）；配置后用更便宜的模型跑 compact。
    /// 与 `provider` 一样支持热替换。
    pub compact_provider: Arc<RwLock<Option<Arc<dyn Provider>>>>,
    /// 图片描述用的独立 provider（可选）。
    /// 主模型无多模态能力时，用此 provider 描述图片，描述文本替换图片注入主模型上下文。
    /// 未配置时：图片直接发给主模型。
    pub vision_provider: Arc<RwLock<Option<Arc<dyn Provider>>>>,
    pub tools: Arc<ToolRegistry>,
    pub context: Context,
    pub session_store: Arc<SessionStore>,
    pub session_id: i64,
    /// 模型上下文窗口大小（tokens），用于判断何时触发自动压缩
    pub context_size: usize,
    pub context_threshold: f64,
    pub max_iterations: u32,
    /// 全局 confirm_mode（none / always / session），不再 per-channel
    /// [deprecated] P4-d 起由 permission profile 取代，保留字段仅为向后兼容
    pub confirm_mode: String,
    /// 审批门控（P4-d）：独立锁，保存待确认的审批请求
    pub approval_gate: Arc<crate::agent::approval::ApprovalGate>,
    /// 权限档位（P4-d）：read-only / default / yolo，运行时可切换
    pub permission_profile: Arc<RwLock<String>>,
    /// Agent 家目录（固定，不随 /move 变化）：SOUL/USER/MEMORY/sessions.db/uploads 都在这里。
    /// 与 workspace_root（文件/终端工具的实时作用域，/move 会改变它）区分开，避免记忆/历史串味。
    pub workspace: std::path::PathBuf,
    /// 与文件/终端工具共享的工作区根（Arc<RwLock>），/move 一处更新、所有工具即时生效
    pub workspace_root: Arc<RwLock<std::path::PathBuf>>,
    /// 配置根目录（~/.llaia/），agent 工具不可访问，但用于推导路径
    pub config_dir: std::path::PathBuf,
    /// 是否主 agent（决定能否读 subagent/）
    pub is_main: bool,
    /// agent 别名（main / 子 agent alias）
    pub alias: String,
    /// 审计日志（可选，测试时为 None）
    pub audit: Option<Arc<crate::audit::AuditLog>>,
    /// 本次 turn 的工具调用历史（供 delegate 提取产出文件清单）
    pub turn_tool_calls: Vec<TurnToolCall>,
    /// 启动时配置快照（供 /provider 等运行时命令枚举/构建 provider；
    /// 不随 config.toml 热加载更新——运行时切换本身就是临时态）
    pub config: Arc<Config>,
    /// 实时配置（ADR-0017）：读取那些"改了就该立刻生效、无需重启"的字段，
    /// 目前是 `[runtime].timezone`。
    ///
    /// 默认指向自己独占的一份启动快照（CLI 模式无热加载，与现状一致）；
    /// serve 模式下 `attach_live_config` 把它换成 WebUI 共享的那个 Arc，
    /// 于是 `PUT /api/config` 写入后下一轮 `to_messages` 立即读到新值。
    live_config: Arc<RwLock<Config>>,
    /// 系统提示词静态前缀（SOUL/USER/MEMORY/WORKSPACE），skills 段在其后追加。
    /// 热加载 skills 时只重建 skills 段，不动前缀（见 `reload_skills`）。
    system_prompt_base: String,
    /// provider 是否非 native tool calling（标签降级模式）。
    /// 决定 system 末尾是否追加 tool instructions（热加载 skills 时需重建）。
    system_has_tool_instructions: bool,
}

/// 单次工具调用记录（用于 delegate 提取产出文件）
#[derive(Debug, Clone)]
pub struct TurnToolCall {
    pub name: String,
    pub args: serde_json::Value,
}

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        config: &Config,
        provider: Option<Arc<dyn Provider>>,
        compact_provider: Option<Arc<dyn Provider>>,
        vision_provider: Option<Arc<dyn Provider>>,
        tools: Arc<ToolRegistry>,
        session_store: Arc<SessionStore>,
        session_id: i64,
        system_prompt: String,
        context_size: usize,
        workspace: std::path::PathBuf,
        workspace_root: Arc<RwLock<std::path::PathBuf>>,
        config_dir: std::path::PathBuf,
        is_main: bool,
        alias: String,
        audit: Option<Arc<crate::audit::AuditLog>>,
    ) -> Self {
        let permission = config
            .runtime
            .permission
            .clone()
            .unwrap_or_else(|| "default".to_string());
        Self {
            provider: Arc::new(RwLock::new(provider)),
            compact_provider: Arc::new(RwLock::new(compact_provider)),
            vision_provider: Arc::new(RwLock::new(vision_provider)),
            tools,
            context: Context::new(system_prompt),
            session_store,
            session_id,
            context_size,
            context_threshold: config.runtime.context_threshold,
            max_iterations: config.runtime.max_iterations,
            confirm_mode: config.channels.qq.confirm_mode.clone(),
            approval_gate: crate::agent::approval::ApprovalGate::new(),
            permission_profile: Arc::new(RwLock::new(permission)),
            workspace: workspace.clone(),
            workspace_root,
            config_dir,
            is_main,
            alias,
            audit,
            turn_tool_calls: Vec::new(),
            config: Arc::new(config.clone()),
            live_config: Arc::new(RwLock::new(config.clone())),
            system_prompt_base: String::new(),
            system_has_tool_instructions: false,
        }
    }

    /// 运行时切换权限档位（/permission 命令）。不写 config.toml。
    pub async fn set_permission_profile(&self, profile: &str) {
        *self.permission_profile.write().await = profile.to_string();
    }

    /// 切换工具作用域（/move 命令）：只更新 workspace_root（文件/终端工具实时生效），
    /// 不动 workspace（agent 家目录，SOUL/USER/MEMORY/sessions.db 所在，固定不变）。
    pub async fn set_workspace(&mut self, new_workspace: std::path::PathBuf) {
        *self.workspace_root.write().await = new_workspace;
    }

    /// 接入共享的实时配置（serve 模式下由 `serve_cmd` 注入 WebUI 持有的同一个 Arc）。
    /// 不调用时 agent 用自己的启动快照，行为与热加载前完全一致。
    pub fn attach_live_config(&mut self, live: Arc<RwLock<Config>>) {
        self.live_config = live;
    }

    /// 当前生效的时区设置（`[runtime].timezone`），热更新即时可见。
    pub async fn timezone(&self) -> Option<String> {
        self.live_config.read().await.runtime.timezone.clone()
    }

    /// 拿当前 provider 的 snapshot（Arc 克隆）。
    /// turn 开始时调用一次，整个 turn 用这个 snapshot。
    /// None 表示降级模式（无 provider）。
    pub async fn provider_snapshot(&self) -> Option<Arc<dyn Provider>> {
        self.provider.read().await.clone()
    }

    /// 拿 compact provider 的 snapshot：未配置时返回 None，调用方应回退到主 provider。
    pub async fn compact_provider_snapshot(&self) -> Option<Arc<dyn Provider>> {
        self.compact_provider.read().await.clone()
    }

    /// 拿用于压缩的 provider：优先 compact_provider，否则回退到主 provider。
    /// 两者都为 None（降级模式）时返回 None。
    pub async fn provider_for_compact(&self) -> Option<Arc<dyn Provider>> {
        if let Some(p) = self.compact_provider_snapshot().await {
            return Some(p);
        }
        self.provider_snapshot().await
    }

    /// 热替换 provider。
    /// - `Some(p)`：切换到新 provider
    /// - `None`：进入降级模式
    ///
    /// 正在进行的 turn 持有旧 snapshot 不受影响，新 turn 用新 provider。
    pub async fn reload_provider(&self, new_provider: Option<Arc<dyn Provider>>) {
        let mut guard = self.provider.write().await;
        *guard = new_provider;
    }

    /// 热替换 compact_provider（仅 compact_model 变更时调用）。
    pub async fn reload_compact_provider(&self, new_provider: Option<Arc<dyn Provider>>) {
        let mut guard = self.compact_provider.write().await;
        *guard = new_provider;
    }

    /// 拿 vision provider 的 snapshot：未配置时返回 None。
    pub async fn vision_provider_snapshot(&self) -> Option<Arc<dyn Provider>> {
        self.vision_provider.read().await.clone()
    }

    /// 热替换 vision_provider（vision_model 变更时调用）。
    pub async fn reload_vision_provider(&self, new_provider: Option<Arc<dyn Provider>>) {
        let mut guard = self.vision_provider.write().await;
        *guard = new_provider;
    }

    /// 初始化系统提示词元数据（构建期由 build_single_agent 调用，供热加载 skills 重建用）。
    /// 仅记录前缀与 tool-instructions 标记，运行期 `reload_skills` 据此重建。
    pub(crate) fn init_system_meta(&mut self, base: String, has_tool_instructions: bool) {
        self.system_prompt_base = base;
        self.system_has_tool_instructions = has_tool_instructions;
    }

    /// 热加载 runtime 参数（permission / context_threshold / max_iterations）。
    /// 时区由 live_config 通道已即时生效，这里只覆盖其余 runtime 字段。
    pub async fn reload_runtime(&mut self, config: &Config) {
        let perm = config
            .runtime
            .permission
            .clone()
            .unwrap_or_else(|| "default".to_string());
        *self.permission_profile.write().await = perm;
        self.context_threshold = config.runtime.context_threshold;
        self.max_iterations = config.runtime.max_iterations;
    }

    /// 热加载 skills：重建 system 提示词的 skills 段（前缀与 tool instructions 不变）。
    pub fn reload_skills(&mut self, skills_prompt: &str) {
        let mut sys = self.system_prompt_base.clone();
        if !skills_prompt.is_empty() {
            sys.push_str("\n\n");
            sys.push_str(skills_prompt);
        }
        if self.system_has_tool_instructions {
            sys.push_str(&crate::tool_call::prompt::build_tool_instructions(
                &self.tools.specs(),
            ));
        }
        self.context.system = sys;
    }

    /// 是否处于降级模式（无 provider）。
    pub async fn has_provider(&self) -> bool {
        self.provider.read().await.is_some()
    }

    /// 非流式版本（保留向后兼容）：内部调 handle_input_streaming + 收集
    pub async fn handle_input(&mut self, user_input: &str, channel: &str) -> Result<String> {
        let (tx, mut rx) = mpsc::channel(64);
        let result = self.handle_input_streaming(user_input, channel, tx).await;
        let mut text = String::new();
        while let Some(ev) = rx.recv().await {
            match ev {
                TurnEvent::Chunk { delta } => text.push_str(&delta),
                TurnEvent::Error { message } => {
                    return Err(anyhow::anyhow!(message));
                }
                _ => {}
            }
        }
        result?;
        Ok(text)
    }

    /// 跑一轮独立 turn：用临时 session_id 和全新 context（不复用用户会话历史），
    /// 跑完后恢复原 session_id 和 context。供 cron agent 模式使用。
    ///
    /// - `prompt`：注入到 agent 上下文的用户消息
    /// - `channel`：触发渠道（用于审计 + 工具 confirm 判断），cron 用 "cron"
    /// - `session_id`：独立会话 id（由调用方通过 session_store.create_session 创建）
    ///
    /// 返回 agent 最终回复文本。无论成功失败，原 session_id 和 context 都会恢复。
    pub async fn run_isolated_turn(
        &mut self,
        prompt: &str,
        channel: &str,
        session_id: i64,
    ) -> Result<String> {
        let saved_session_id = self.session_id;
        let saved_system = self.context.system.clone();
        let saved_context = std::mem::replace(
            &mut self.context,
            crate::agent::context::Context::new(saved_system),
        );
        self.session_id = session_id;
        let result = self.handle_input(prompt, channel).await;
        // 无论成功失败都恢复原状态
        self.session_id = saved_session_id;
        self.context = saved_context;
        result
    }

    /// 流式版本：通过 event_tx 推送 TurnEvent
    pub async fn handle_input_streaming(
        &mut self,
        user_input: &str,
        channel: &str,
        event_tx: mpsc::Sender<TurnEvent>,
    ) -> Result<String> {
        // ask_user 续答（ADR-0022）：若当前恰有一个 pending question，
        // 则把本次输入当作对该问题的回答，跑一轮 continuation turn。
        // 多 pending 时不清，需用户显式 /answer <id> 消歧。
        if let Some(answer) = self.pending_answer_message(user_input, channel).await {
            return self
                .handle_message_streaming(ChatMessage::user(&answer), channel, event_tx)
                .await;
        }
        self.handle_message_streaming(ChatMessage::user(user_input), channel, event_tx)
            .await
    }

    /// 检测单 pending question 并把当前输入当作答案。
    /// 返回 Some(包装后的答案文本) 时，调用方应据此跑 continuation turn；
    /// 返回 None 表示无需续答（普通消息走原流程）。
    ///
    /// 若唯一 pending 已超时，则丢弃它并注入超时说明（走普通流程）。
    async fn pending_answer_message(&mut self, user_input: &str, channel: &str) -> Option<String> {
        use crate::agent::approval::{is_interactive_channel, ApprovalGate};
        if !is_interactive_channel(channel) {
            return None;
        }
        let q = self.approval_gate.single_question().await?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if ApprovalGate::is_question_expired(&q, now) {
            // 超时：丢弃 pending，注入说明，交给普通流程（用户的真实消息照常到达）
            self.approval_gate.take_question(&q.id).await;
            let note = format!(
                "[系统提示] 你之前向用户提出的问题（id={}）在 {} 秒内未收到回答，已按最合理假设继续。",
                q.id, q.timeout_secs
            );
            self.context.push(ChatMessage::system(&note));
            return None;
        }

        // 消费 pending，构造答案
        self.approval_gate.take_question(&q.id).await;
        let raw = user_input.trim();
        let answered = match &q.choices {
            Some(cs) => map_ask_user_choice(raw, cs),
            None => raw.to_string(),
        };
        Some(format!(
            "[用户对你刚才提出的问题给出了回答]\n问题：{}\n回答：{}",
            q.question, answered
        ))
    }

    /// 图片描述降级：消息含图片且配置了 vision_provider 时，用 vision_provider
    /// 逐张描述图片，把描述文本 + 原始文本组合成纯文本消息返回。
    /// 无图片或无 vision_provider 时原样返回。
    async fn maybe_describe_images(&self, msg: ChatMessage) -> ChatMessage {
        if !msg.content.has_image() {
            return msg;
        }
        let vision_provider = match self.vision_provider_snapshot().await {
            Some(p) => p,
            None => return msg, // 无 vision_provider，图片直接发给主模型
        };

        let parts = match &msg.content {
            MessageContent::Multimodal(parts) => parts,
            _ => return msg,
        };

        let mut text_parts = Vec::new();
        let mut image_urls = Vec::new();
        for part in parts {
            match part {
                ContentPart::Text { text } => text_parts.push(text.clone()),
                ContentPart::ImageUrl { image_url } => image_urls.push(image_url.url.clone()),
            }
        }

        // 逐张描述图片
        let mut descriptions = Vec::new();
        for (i, url) in image_urls.iter().enumerate() {
            let desc = self
                .describe_single_image(vision_provider.as_ref(), url)
                .await;
            descriptions.push(format!("[图片{}描述] {}", i + 1, desc));
        }

        // 组合改写后的纯文本消息
        let mut combined = descriptions.join("\n");
        if !text_parts.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(&text_parts.join(""));
        }
        tracing::debug!(
            images = image_urls.len(),
            "described images via vision_provider"
        );
        ChatMessage::user(combined)
    }

    /// 用 vision provider 描述单张图片。失败时返回占位文本，不阻塞对话。
    async fn describe_single_image(&self, provider: &dyn Provider, image_url: &str) -> String {
        let parts = vec![
            ContentPart::Text {
                text: "请详细描述这张图片的内容，包括文字、物体、场景等关键信息。".into(),
            },
            ContentPart::ImageUrl {
                image_url: ImageUrlContent {
                    url: image_url.into(),
                },
            },
        ];
        let req_msg = ChatMessage::user_multimodal(parts);
        let req = ChatRequest {
            messages: std::slice::from_ref(&req_msg),
            tools: None,
        };
        match provider.chat(&req).await {
            Ok(resp) => resp.text.unwrap_or_else(|| "[图片描述为空]".into()),
            Err(e) => {
                tracing::warn!(error = %e, "vision provider describe image failed");
                "[图片描述失败]".into()
            }
        }
    }

    /// 多模态流式版本：接收任意 ChatMessage（支持文本+图片）。
    /// 文本消息存入 sqlite，多模态消息只存文本部分（图片 base64 不持久化）。
    pub async fn handle_message_streaming(
        &mut self,
        user_msg: ChatMessage,
        channel: &str,
        event_tx: mpsc::Sender<TurnEvent>,
    ) -> Result<String> {
        // 清空本次 turn 的工具调用历史
        self.turn_tool_calls.clear();
        // 图片描述降级：主模型无多模态时，用 vision_provider 描述图片
        let user_msg = self.maybe_describe_images(user_msg).await;
        // 持久化：只存文本部分（图片 base64 太大不存 sqlite；已描述则存描述后文本）
        let text_for_store = user_msg.content.as_text();
        self.session_store
            .append_message(self.session_id, &Role::User, &text_for_store)?;
        self.context.push(user_msg);

        // 规划后执行（ADR-0024）：把当前 session_uuid 写入共享 TodoStore，
        // 并刷新本轮回注的 todo 清单文本（Runtime Context）。
        let current_uuid = self
            .session_store
            .session_uuid(self.session_id)?
            .unwrap_or_else(|| format!("session-{}", self.session_id));
        self.tools.todo_store.set_current_session(&current_uuid);
        let todo_text = self.tools.todo_store.current_list_text();
        self.context.todo_state = if todo_text.is_empty() {
            None
        } else {
            Some(todo_text)
        };

        // 长期目标（ADR-0021）：每轮从 agent 家目录 goal.md 重新读取，仅 active 注入。
        // 路径用 self.workspace（家目录，固定不随 /move 变化），与 SOUL/USER/MEMORY 同处。
        let goal_line = crate::goal::read_active_goal_line(&self.workspace);
        self.context.goal_state = goal_line;

        // 拿 provider snapshot：整个 turn 用这个 snapshot，reload 不影响进行中的 turn
        let provider = match self.provider_snapshot().await {
            Some(p) => p,
            None => {
                // 降级模式：无 provider，直接 sink Error 提示用户配置
                let msg = "未配置 provider，请先在 WebUI 配置 [provider.default] section 或编辑 config.toml 取消注释".to_string();
                let _ = event_tx
                    .send(TurnEvent::Error {
                        message: msg.clone(),
                    })
                    .await;
                return Err(anyhow::anyhow!(msg));
            }
        };

        if self
            .context
            .needs_compaction(self.context_size, self.context_threshold)
        {
            // 优先用 compact_provider，未配置时回退到主 provider
            let compact_provider = self.provider_for_compact().await;
            match compact_provider.as_ref() {
                Some(p) => {
                    if let Err(e) = self.context.compact(p.as_ref(), 6, self.context_size).await {
                        tracing::warn!(error = %e, "auto-compact failed");
                    }
                }
                None => tracing::warn!("skip auto-compact: no provider available"),
            }
        }

        let max_iters = self.max_iterations;
        // 时区快照：整轮用同一个值。turn 中途 WebUI 改配置不会让同一轮里的
        // 状态栏前后矛盾，下一轮自然读到新值。
        let tz = self.timezone().await;

        // 重复工具调用检测：连续相同 (name, args) 调用计数
        let mut last_tool_name: Option<String> = None;
        let mut last_tool_args: Option<String> = None;
        let mut same_tool_streak: u32 = 0;

        for i in 0..max_iters {
            // 达到 max_iterations 前一步：拔掉工具 + 注入强制总结提示词
            let force_summary = i + 1 >= max_iters;
            if force_summary {
                tracing::warn!(
                    iter = i,
                    max = max_iters,
                    "approaching max_iterations, forcing summary"
                );
                self.context.push(ChatMessage::user(
                    "已达工具调用次数上限，请停止调用工具，基于已获取的信息总结任务并直接回复用户。",
                ));
            }
            let messages = self.context.to_messages(&tz);
            let tools = if force_summary {
                None
            } else {
                let specs = self.tools.specs();
                if specs.is_empty() {
                    None
                } else {
                    Some(specs)
                }
            };
            let tools_ref = tools.as_deref();
            let req = ChatRequest {
                messages: &messages,
                tools: tools_ref,
            };

            let mut stream = provider.chat_stream(&req).await;
            let mut iter_text = String::new();
            let mut calls: Vec<crate::provider::ToolCall> = Vec::new();
            let mut parser = crate::tool_call::ToolCallStreamParser::new();

            while let Some(ev) = stream.next().await {
                // 用户中止（Ctrl+C）：event_tx 被关闭，提前结束并保存部分输出
                if event_tx.is_closed() {
                    tracing::info!(iter = i, "stream aborted by user (tx closed)");
                    if !iter_text.is_empty() {
                        self.session_store.append_message(
                            self.session_id,
                            &Role::Assistant,
                            &iter_text,
                        )?;
                        self.context.push(ChatMessage::assistant(&iter_text));
                    }
                    return Ok(iter_text);
                }
                match ev? {
                    StreamEvent::TextDelta(d) => {
                        // 统一走 parser：剥离 think 标签 + 提取 tool_call 标签。
                        // 无论 native 与否都跑——native 模式下模型偶发把
                        // <think>/<tool_call> 泄露到文本流，parser 兜底清洗。
                        // 对无标签文本 parser 是透传的，不影响正常输出。
                        // iter_text 存清洗后文本（think/标签不进 context/sqlite）。
                        let user_text = parser.feed(&d);
                        if !user_text.is_empty() {
                            iter_text.push_str(&user_text);
                            let _ = event_tx.send(TurnEvent::Chunk { delta: user_text }).await;
                        }
                        let new_calls = parser.take_tool_calls();
                        calls.extend(new_calls);
                    }
                    StreamEvent::ToolCall(tc) => {
                        calls.push(tc);
                    }
                    StreamEvent::Usage(_) => {}
                    StreamEvent::FinishReason(_) => {}
                    StreamEvent::Done => break,
                    StreamEvent::Error(msg) => {
                        let _ = event_tx
                            .send(TurnEvent::Error {
                                message: msg.clone(),
                            })
                            .await;
                        return Err(anyhow::anyhow!(msg));
                    }
                }
            }

            // 统一 finish：流结束时输出 parser 残留（未闭合标签的 buffer）
            let rest = parser.finish();
            if !rest.is_empty() {
                let _ = event_tx
                    .send(TurnEvent::Chunk {
                        delta: rest.clone(),
                    })
                    .await;
                iter_text.push_str(&rest);
            }

            if calls.is_empty() {
                self.session_store
                    .append_message(self.session_id, &Role::Assistant, &iter_text)?;
                self.context.push(ChatMessage::assistant(&iter_text));
                let _ = event_tx.send(TurnEvent::Done).await;
                return Ok(iter_text);
            }

            // 重复工具调用检测
            for tc in &calls {
                let args_str = tc.arguments.to_string();
                if last_tool_name.as_deref() == Some(tc.name.as_str())
                    && last_tool_args.as_deref() == Some(args_str.as_str())
                {
                    same_tool_streak += 1;
                } else {
                    last_tool_name = Some(tc.name.clone());
                    last_tool_args = Some(args_str);
                    same_tool_streak = 1;
                }
                if same_tool_streak >= 3 {
                    let warning = build_repeated_tool_warning(&tc.name, same_tool_streak);
                    tracing::warn!(tool = %tc.name, streak = same_tool_streak, "repeated tool call");
                    // 追加到工具结果前，作为系统提示注入下一轮
                    self.context.push(ChatMessage::system(&warning));
                }
            }

            let assistant_msg = ChatMessage::assistant_with_tools(iter_text.clone(), calls.clone());
            let assistant_msg_id =
                self.session_store
                    .append_message(self.session_id, &Role::Assistant, &iter_text)?;
            self.context.push(assistant_msg);

            for tc in &calls {
                self.session_store
                    .append_tool_call(
                        assistant_msg_id,
                        &tc.id,
                        &tc.name,
                        &tc.arguments.to_string(),
                        None,
                    )
                    .ok();
            }

            // 工具调用开始事件
            for tc in &calls {
                let _ = event_tx
                    .send(TurnEvent::ToolStart {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                    })
                    .await;
            }

            // 记录工具调用到 turn_tool_calls（供 delegate 提取产出文件）
            for tc in &calls {
                self.turn_tool_calls.push(TurnToolCall {
                    name: tc.name.clone(),
                    args: tc.arguments.clone(),
                });
            }

            let ctx = crate::agent::approval::ApprovalContext {
                profile: self.permission_profile.read().await.clone(),
                workspace: self.workspace_root.read().await.clone(),
                gate: self.approval_gate.clone(),
                agent_alias: self.alias.clone(),
                audit: self.audit.clone(),
                ask_user_timeout_secs: self.config.runtime.ask_user_timeout_secs as u64,
            };
            let (tool_msgs, deferred) =
                execute_tool_calls(&self.tools, &calls, channel, &ctx, Some(&event_tx)).await?;
            for msg in tool_msgs.iter() {
                let text = msg.content.as_text();
                let _ = event_tx
                    .send(TurnEvent::ToolResult {
                        id: msg.tool_call_id.clone().unwrap_or_default(),
                        output: text.clone(),
                    })
                    .await;
                self.session_store
                    .append_message(self.session_id, &Role::Tool, &text)?;
                self.context.push(msg.clone());
            }

            tracing::info!(iter = i, "tool iteration done");

            // 有待确认请求：本轮暂停，等待用户 /ok /deny 解析后再续跑，
            // 不继续调用模型，避免重复触发同一审批。
            if deferred {
                let _ = event_tx.send(TurnEvent::Done).await;
                return Ok(iter_text);
            }

            // 工具执行后检测用户中止：保存已完成的工具结果，提前返回
            if event_tx.is_closed() {
                tracing::info!(iter = i, "aborted by user after tool execution");
                return Ok(String::new());
            }
        }

        // 兜底：循环结束仍未返回（理论上 force_summary 那轮会返回，这里防御性处理）
        let fallback = "[已达工具调用次数上限，未能生成总结]";
        self.session_store
            .append_message(self.session_id, &Role::Assistant, fallback)?;
        self.context.push(ChatMessage::assistant(fallback));
        let _ = event_tx
            .send(TurnEvent::Chunk {
                delta: fallback.into(),
            })
            .await;
        let _ = event_tx.send(TurnEvent::Done).await;
        Ok(fallback.into())
    }
}

/// ask_user 结构化单选：把用户原始回答映射到选项文本。
/// - 纯数字且落在 1..=len 范围内 → 对应选项
/// - 与某选项（忽略大小写/首尾空格）相等 → 该选项
/// - 否则原样返回
fn map_ask_user_choice(raw: &str, choices: &[String]) -> String {
    let raw = raw.trim();
    if let Ok(n) = raw.parse::<usize>() {
        if n >= 1 && n <= choices.len() {
            return choices[n - 1].clone();
        }
    }
    for c in choices {
        if c.trim().eq_ignore_ascii_case(raw) {
            return c.clone();
        }
    }
    raw.to_string()
}

/// 重复工具调用警告：三级渐进式
fn build_repeated_tool_warning(tool_name: &str, streak: u32) -> String {
    if streak >= 5 {
        format!(
            "\n\n[系统提示] 重要：你已经连续 {} 次用相同参数调用工具 `{}`。除非每次调用都明确产生了新信息，否则请立即停止重复，更换策略、调整参数，或向用户说明限制。",
            streak, tool_name
        )
    } else if streak >= 4 {
        format!(
            "\n\n[系统提示] 重要：你已经连续 {} 次用相同参数调用工具 `{}`。除非重复明显必要，否则请停止重复同一操作，改用其他工具、调整参数，或总结还缺什么。",
            streak, tool_name
        )
    } else {
        format!(
            "\n\n[系统提示] 提醒：你已经连续 {} 次用相同参数调用工具 `{}`。请检查是否有其他工具、不同参数或直接总结能更好地推进任务。",
            streak, tool_name
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        ChatRequest, ChatResponse, ContentPart, ImageUrlContent, Provider, StreamEvent, ToolCall,
    };
    use async_stream::try_stream;
    use async_trait::async_trait;
    use futures_util::stream::BoxStream;
    use serde_json::json;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::mpsc;

    /// Mock provider：每次 chat_stream 调用返回下一组预设事件
    struct MockProvider {
        native: bool,
        rounds: Arc<StdMutex<std::collections::VecDeque<Vec<StreamEvent>>>>,
    }

    impl MockProvider {
        fn new(native: bool, rounds: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                native,
                rounds: Arc::new(StdMutex::new(rounds.into())),
            }
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat(&self, _req: &ChatRequest<'_>) -> Result<ChatResponse> {
            unreachable!()
        }
        async fn chat_stream(&self, _req: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>> {
            let events = self.rounds.lock().unwrap().pop_front().unwrap_or_default();
            let s = try_stream! {
                for ev in events {
                    yield ev;
                }
            };
            Box::pin(s)
        }
        fn native_tool_calling(&self) -> bool {
            self.native
        }
    }

    async fn make_agent_with_rounds(native: bool, rounds: Vec<Vec<StreamEvent>>) -> Agent {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("test", "test").unwrap();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(native, rounds));
        let tools = Arc::new(ToolRegistry::new());
        let config = Config::default_for_workspace("/tmp/llaia-test");
        Agent::new(
            &config,
            Some(provider),
            None,
            None,
            tools,
            Arc::new(store),
            sid,
            "test system".into(),
            8192,
            std::path::PathBuf::from("/tmp/llaia-test/workspace"),
            Arc::new(RwLock::new(std::path::PathBuf::from(
                "/tmp/llaia-test/workspace",
            ))),
            std::path::PathBuf::from("/tmp/llaia-test"),
            true,
            "main".into(),
            None,
        )
        .await
    }

    #[tokio::test]
    async fn test_streaming_plain_text() {
        let rounds = vec![vec![
            StreamEvent::TextDelta("hello ".into()),
            StreamEvent::TextDelta("world".into()),
            StreamEvent::Done,
        ]];
        let mut agent = make_agent_with_rounds(true, rounds).await;
        let (tx, mut rx) = mpsc::channel(64);
        let result = agent.handle_input_streaming("hi", "cli", tx).await.unwrap();
        assert_eq!(result, "hello world");

        let mut chunks = Vec::new();
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                TurnEvent::Chunk { delta } => chunks.push(delta),
                TurnEvent::Done => done = true,
                _ => {}
            }
        }
        assert_eq!(chunks.concat(), "hello world");
        assert!(done);
    }

    #[tokio::test]
    async fn test_run_isolated_turn_restores_session_and_context() {
        let rounds = vec![vec![
            StreamEvent::TextDelta("cron reply".into()),
            StreamEvent::Done,
        ]];
        let mut agent = make_agent_with_rounds(true, rounds).await;

        // 模拟用户已有会话历史
        agent.context.push(ChatMessage::user("prior user msg"));
        agent
            .context
            .push(ChatMessage::assistant("prior assistant msg"));
        let original_session_id = agent.session_id;
        let original_history_len = agent.context.history.len();

        // 创建独立 session
        let cron_sid = agent
            .session_store
            .create_session("cron-uuid", "cron:test")
            .unwrap();

        // 跑独立 turn
        let reply = agent
            .run_isolated_turn("do the task", "cron", cron_sid)
            .await;
        assert!(reply.is_ok(), "run_isolated_turn failed: {:?}", reply.err());
        assert_eq!(reply.unwrap(), "cron reply");

        // 验证恢复：session_id 和 context 都回到原状
        assert_eq!(agent.session_id, original_session_id);
        assert_eq!(agent.context.history.len(), original_history_len);
        assert_eq!(agent.context.history[0].content.as_text(), "prior user msg");
        assert_eq!(
            agent.context.history[1].content.as_text(),
            "prior assistant msg"
        );
    }

    #[tokio::test]
    async fn test_streaming_native_tool_call() {
        let tc = ToolCall {
            id: "call_1".into(),
            name: "echo".into(),
            arguments: json!({}),
        };
        let rounds = vec![
            vec![StreamEvent::ToolCall(tc), StreamEvent::Done],
            vec![StreamEvent::TextDelta("done".into()), StreamEvent::Done],
        ];
        let mut agent = make_agent_with_rounds(true, rounds).await;
        let (tx, mut rx) = mpsc::channel(64);
        let _ = agent.handle_input_streaming("read", "cli", tx).await;

        let mut tool_starts = Vec::new();
        let mut chunks = Vec::new();
        let mut done_count = 0;
        while let Some(ev) = rx.recv().await {
            match ev {
                TurnEvent::ToolStart { name, .. } => tool_starts.push(name),
                TurnEvent::Chunk { delta } => chunks.push(delta),
                TurnEvent::Done => done_count += 1,
                _ => {}
            }
        }
        assert_eq!(tool_starts, vec!["echo"]);
        assert_eq!(chunks.concat(), "done");
        assert_eq!(done_count, 1);
    }

    /// native 模式下模型把 <think> 泄露到文本流，parser 应剥离推理内容
    #[tokio::test]
    async fn test_native_mode_strips_think_tags() {
        let think = format!(
            "{}secret reasoning{}visible reply",
            concat!("<", "think>"),
            concat!("<", "/think>")
        );
        let rounds = vec![vec![StreamEvent::TextDelta(think), StreamEvent::Done]];
        let mut agent = make_agent_with_rounds(true, rounds).await;
        let (tx, mut rx) = mpsc::channel(64);
        let result = agent.handle_input_streaming("hi", "cli", tx).await.unwrap();

        // 用户只看到 visible reply，think 内容被剥离
        assert_eq!(result, "visible reply");

        let mut chunks = Vec::new();
        while let Some(ev) = rx.recv().await {
            if let TurnEvent::Chunk { delta } = ev {
                chunks.push(delta);
            }
        }
        assert_eq!(chunks.concat(), "visible reply");
    }

    /// native 模式下模型把 <tool_call> 标签泄露到文本流，parser 应提取为工具调用并执行
    #[tokio::test]
    async fn test_native_mode_strips_tool_call_tags() {
        let tag = "\u{3c}tool_call\u{3e}{\"name\":\"echo\",\"arguments\":{}}\u{3c}/tool_call\u{3e}";
        let text = format!("before {} after", tag);
        let rounds = vec![
            vec![StreamEvent::TextDelta(text), StreamEvent::Done],
            vec![StreamEvent::TextDelta("done".into()), StreamEvent::Done],
        ];
        let mut agent = make_agent_with_rounds(true, rounds).await;
        let (tx, mut rx) = mpsc::channel(64);
        let _ = agent.handle_input_streaming("read", "cli", tx).await;

        let mut chunks = Vec::new();
        let mut tool_starts = Vec::new();
        while let Some(ev) = rx.recv().await {
            match ev {
                TurnEvent::Chunk { delta } => chunks.push(delta),
                TurnEvent::ToolStart { name, .. } => tool_starts.push(name),
                _ => {}
            }
        }
        // 用户看到 "before  after"，标签被剥离
        assert_eq!(chunks.concat(), "before  afterdone");
        // 工具调用被提取并执行
        assert_eq!(tool_starts, vec!["echo"]);
    }

    /// Mock vision provider：chat 返回固定描述文本
    struct VisionMockProvider {
        description: String,
    }

    #[async_trait]
    impl Provider for VisionMockProvider {
        async fn chat(&self, _req: &ChatRequest<'_>) -> Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some(self.description.clone()),
                tool_calls: vec![],
                usage: None,
                finish_reason: None,
            })
        }
        async fn chat_stream(&self, _req: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>> {
            unreachable!("vision provider should only use chat()")
        }
        fn native_tool_calling(&self) -> bool {
            true
        }
    }

    /// 配了 vision_provider 时，含图片消息被改写为纯文本（描述 + 原文本）
    #[tokio::test]
    async fn test_vision_provider_describes_images() {
        let rounds = vec![vec![
            StreamEvent::TextDelta("收到".into()),
            StreamEvent::Done,
        ]];
        let mut agent = make_agent_with_rounds(true, rounds).await;
        *agent.vision_provider.write().await = Some(Arc::new(VisionMockProvider {
            description: "一张红色方块的图片".into(),
        }));

        let msg = ChatMessage::user_multimodal(vec![
            ContentPart::Text {
                text: "这张图是什么？".into(),
            },
            ContentPart::ImageUrl {
                image_url: ImageUrlContent {
                    url: "data:image/jpeg;base64,xxx".into(),
                },
            },
        ]);

        let (tx, _rx) = mpsc::channel(64);
        let result = agent
            .handle_message_streaming(msg, "cli", tx)
            .await
            .unwrap();
        assert_eq!(result, "收到");

        // 验证 context 里的用户消息被改写为纯文本（含描述）
        let user_msg = &agent.context.history[0];
        let text = user_msg.content.as_text();
        assert!(
            text.contains("[图片1描述]"),
            "expected image description tag in: {}",
            text
        );
        assert!(
            text.contains("一张红色方块的图片"),
            "expected vision description in: {}",
            text
        );
        assert!(
            text.contains("这张图是什么？"),
            "expected original text in: {}",
            text
        );
        // 确保图片不再以多模态形式存在
        assert!(!user_msg.content.has_image());
    }

    /// 未配 vision_provider 时，图片直接发给主模型（消息原样，多模态保留）
    #[tokio::test]
    async fn test_no_vision_provider_passes_image_through() {
        let rounds = vec![vec![
            StreamEvent::TextDelta("回复".into()),
            StreamEvent::Done,
        ]];
        let mut agent = make_agent_with_rounds(true, rounds).await;

        let msg = ChatMessage::user_multimodal(vec![
            ContentPart::Text {
                text: "看图".into(),
            },
            ContentPart::ImageUrl {
                image_url: ImageUrlContent {
                    url: "data:image/jpeg;base64,xxx".into(),
                },
            },
        ]);

        let (tx, _rx) = mpsc::channel(64);
        let result = agent
            .handle_message_streaming(msg, "cli", tx)
            .await
            .unwrap();
        assert_eq!(result, "回复");

        // 验证 context 里的用户消息保持原样（多模态，未被改写）
        let user_msg = &agent.context.history[0];
        assert!(
            user_msg.content.has_image(),
            "image should be preserved without vision_provider"
        );
    }

    #[tokio::test]
    async fn test_streaming_tag_mode_filters_tags() {
        let tag = "\u{3c}tool_call\u{3e}{\"name\":\"x\",\"arguments\":{}}\u{3c}/tool_call\u{3e}";
        let rounds = vec![vec![
            StreamEvent::TextDelta("before ".into()),
            StreamEvent::TextDelta(tag.to_string()),
            StreamEvent::TextDelta(" after".into()),
            StreamEvent::Done,
        ]];
        let mut agent = make_agent_with_rounds(false, rounds).await;
        let (tx, mut rx) = mpsc::channel(64);
        let _ = agent.handle_input_streaming("hi", "cli", tx).await;

        let mut chunks = Vec::new();
        let mut tool_starts = Vec::new();
        while let Some(ev) = rx.recv().await {
            match ev {
                TurnEvent::Chunk { delta } => chunks.push(delta),
                TurnEvent::ToolStart { name, .. } => tool_starts.push(name),
                _ => {}
            }
        }
        assert_eq!(chunks.concat(), "before  after");
        assert_eq!(tool_starts, vec!["x"]);
    }

    /// max_iterations 达上限后强制总结：最后一轮拔工具 + 注入提示词，LLM 应返回纯文本
    #[tokio::test]
    async fn test_force_summary_on_max_iterations() {
        // max_iterations=2：第一轮调工具，第二轮强制总结（无工具可调）
        // 第一轮：调 echo 工具
        // 第二轮：应该返回纯文本总结
        let tc = ToolCall {
            id: "call_1".into(),
            name: "echo".into(),
            arguments: json!({}),
        };
        let rounds = vec![
            vec![StreamEvent::ToolCall(tc), StreamEvent::Done],
            vec![StreamEvent::TextDelta("总结完成".into()), StreamEvent::Done],
        ];
        let mut agent = make_agent_with_rounds(true, rounds).await;
        agent.max_iterations = 2;

        let (tx, mut rx) = mpsc::channel(64);
        let result = agent
            .handle_input_streaming("do task", "cli", tx)
            .await
            .unwrap();

        assert_eq!(result, "总结完成");

        let mut chunks = Vec::new();
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                TurnEvent::Chunk { delta } => chunks.push(delta),
                TurnEvent::Done => done = true,
                _ => {}
            }
        }
        assert_eq!(chunks.concat(), "总结完成");
        assert!(done);
    }

    /// 重复工具调用检测：连续 3 次相同调用后注入警告（不影响行为，但验证不 panic）
    #[tokio::test]
    async fn test_repeated_tool_detection_no_panic() {
        // 连续 3 轮调相同工具相同参数，第 4 轮返回纯文本
        let tc = ToolCall {
            id: "call_1".into(),
            name: "echo".into(),
            arguments: json!({"x": 1}),
        };
        let mut rounds = vec![];
        for _ in 0..3 {
            rounds.push(vec![StreamEvent::ToolCall(tc.clone()), StreamEvent::Done]);
        }
        rounds.push(vec![
            StreamEvent::TextDelta("done".into()),
            StreamEvent::Done,
        ]);

        let mut agent = make_agent_with_rounds(true, rounds).await;
        agent.max_iterations = 10;

        let (tx, mut rx) = mpsc::channel(64);
        let result = agent.handle_input_streaming("hi", "cli", tx).await.unwrap();
        assert_eq!(result, "done");

        // 确保事件流正常结束
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            if let TurnEvent::Done = ev {
                done = true;
            }
        }
        assert!(done);
    }

    /// 端到端委派：主 Agent 调 delegate 工具 → 子 Agent 执行 → 结果回传 → 主 Agent 整合回复
    #[tokio::test]
    async fn test_delegation_end_to_end() {
        use crate::tools::delegate::DelegateTool;
        use tokio::sync::Mutex as TokioMutex;

        // 子 Agent：返回固定文本
        let sub_provider: Arc<dyn Provider> = Arc::new(MockProvider::new(
            true,
            vec![vec![
                StreamEvent::TextDelta("子 Agent 完成任务".into()),
                StreamEvent::Done,
            ]],
        ));
        let sub_store = SessionStore::open_in_memory().unwrap();
        let sub_sid = sub_store.create_session("sub", "test").unwrap();
        let sub_tools = Arc::new(ToolRegistry::new());
        let config = Config::default_for_workspace("/tmp/llaia-test");
        let sub_agent = Agent::new(
            &config,
            Some(sub_provider),
            None,
            None,
            sub_tools,
            Arc::new(sub_store),
            sub_sid,
            "sub soul".into(),
            8192,
            std::path::PathBuf::from("/tmp/llaia-test/workspace/subagent/coder"),
            Arc::new(RwLock::new(std::path::PathBuf::from(
                "/tmp/llaia-test/workspace/subagent/coder",
            ))),
            std::path::PathBuf::from("/tmp/llaia-test"),
            false,
            "coder".into(),
            None,
        )
        .await;
        let sub_arc: Arc<TokioMutex<Agent>> = Arc::new(TokioMutex::new(sub_agent));

        // 主 Agent：第一轮调 delegate，第二轮基于结果整合回复
        let main_rounds = vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_1".into(),
                    name: "delegate".into(),
                    arguments: json!({"agent_name": "coder", "task": "写个函数"}),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("已委派完成".into()),
                StreamEvent::Done,
            ],
        ];
        let main_provider: Arc<dyn Provider> = Arc::new(MockProvider::new(true, main_rounds));
        let main_store = SessionStore::open_in_memory().unwrap();
        let main_sid = main_store.create_session("main", "test").unwrap();

        let delegate = Arc::new(DelegateTool::new(120));
        let main_tools = ToolRegistry::new();
        main_tools.register(delegate.clone());
        let main_tools = Arc::new(main_tools);

        let main_workspace = std::path::PathBuf::from("/tmp/llaia-test/workspace");
        let main_agent = Agent::new(
            &config,
            Some(main_provider),
            None,
            None,
            main_tools,
            Arc::new(main_store),
            main_sid,
            "main soul".into(),
            8192,
            main_workspace.clone(),
            Arc::new(RwLock::new(main_workspace.clone())),
            std::path::PathBuf::from("/tmp/llaia-test"),
            true,
            "main".into(),
            None,
        )
        .await;
        let main_arc: Arc<TokioMutex<Agent>> = Arc::new(TokioMutex::new(main_agent));

        let registry = AgentRegistry::new(main_arc, main_workspace);
        registry.register_sub_agent("coder".into(), sub_arc);
        let registry = Arc::new(registry);
        delegate.set_registry(registry.clone());

        // 执行
        let main = registry.main.clone();
        let (tx, mut rx) = mpsc::channel(64);
        let mut agent = main.lock().await;
        let result = agent
            .handle_input_streaming("帮我写个函数", "cli", tx)
            .await
            .unwrap();

        // 验证主 Agent 最终回复
        assert_eq!(result, "已委派完成");

        // 验证事件流
        let mut chunks = Vec::new();
        let mut tool_starts = Vec::new();
        let mut tool_results = Vec::new();
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                TurnEvent::Chunk { delta } => chunks.push(delta),
                TurnEvent::ToolStart { name, .. } => tool_starts.push(name),
                TurnEvent::ToolResult { output, .. } => tool_results.push(output),
                TurnEvent::Done => done = true,
                _ => {}
            }
        }
        assert_eq!(tool_starts, vec!["delegate"]);
        // delegate 转发子 Agent 的 Chunk，主 Agent 第二轮再加自己的回复
        assert_eq!(chunks.concat(), "子 Agent 完成任务已委派完成");
        assert!(
            tool_results.iter().any(|s| s.contains("子 Agent 完成任务")),
            "delegate tool result should contain sub agent output, got: {:?}",
            tool_results
        );
        assert!(done);
    }
}
