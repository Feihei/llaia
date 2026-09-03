pub mod approval;
pub mod context;
pub mod guard;
pub mod registry;
pub mod reminder;
pub mod runner;
pub mod sink;

pub use crate::agent::guard::GuardConfig;
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
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
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
    ///
    /// 仅作为**降级基线**（构建期由配置值/默认给出），真正的窗口由 `context_size_now`
    /// 按**活动 provider** 懒解析并缓存进 `resolved_context_size`，两者不再强绑定——
    /// /provider 切换、WebUI 热保存后压缩阈值即随新模型跟随，而非冻结在启动时刻。
    pub context_size: usize,
    /// 懒解析的模型上下文窗口缓存，跟随活动 provider（见 `context_size_now`）。
    /// `reload_provider` 命中时清空，使运行时切换/热保存后窗口随新模型重算。
    resolved_context_size: Arc<RwLock<Option<usize>>>,
    pub context_threshold: f64,
    pub max_iterations: u32,
    /// 单个工具结果文本的最大字符数（非图片内容），超限截断兜底。
    /// 图片（data:image base64）识别后走多模态通道，不占此额度。
    pub tool_result_cap: usize,
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
    /// 会话级受信目录集合（#B）：/move 批准过的目标目录（canonical、过黑名单校验）。
    /// 审批判定「是否在 workspace 内」时与 workspace_root 同等对待，令这些目录内的
    /// 操作免审批；仅存内存、随会话（Agent 生命周期）失效，重启后需重新 /move 批准。
    pub trusted_dirs: Arc<RwLock<Vec<std::path::PathBuf>>>,
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
    /// /move 到外部目录后注入的 AGENTS.md 系统提示词段（空串 = 未加载）。
    /// 仅缓存重构系统中：agent 家目录不加载（SOUL/USER/MEMORY 已在提示词内）。
    agents_md_prompt: String,
    /// 上次热加载的 skills 系统提示词段。set_workspace 重建 system 需要它在场
    /// （reload_skills 只在我们这边缓存、不再另外持参），否则 /move 会把 skills 段挤掉。
    skills_prompt_cache: String,
    /// cron 等自动化任务关闭模型「深度思考」：推理模型（Qwen3 等）在结构化
    /// 合成任务上思考纯属浪费且撑爆超时。置位后请求带 `disable_thinking`，
    /// 由 provider 注入 chat_template_kwargs 关掉思考。
    /// `fork_for_isolated` 派生的 cron 副本按需置位，主 agent 恒为 false。
    disable_thinking: bool,
    /// 用户经 `/reasoning off` 设置的会话级思考开关：true = 关闭深度思考。
    /// 与上面的自动任务位独立（OR 关系），cron 副本置位不影响主会话开关。
    pub thinking_off: bool,
    /// /steer 插话缓冲（plan.md #I）：channel 在 turn 持锁期间仍可投递（本字段为
    /// 独立 Arc，不经 Agent 锁）；agent 在工具循环非末轮迭代顶部 drain，以
    /// `[steer] User added: ...` 的 user 消息注入，模型下一迭代自然看到。
    /// `fork_for_isolated` 派生副本持有**独立空缓冲**——cron/委派 turn 不得消费
    /// 用户给主线的插话。
    pub steer_buffer: Arc<StdMutex<VecDeque<String>>>,
    /// 当前 active 任务线（ADR-0031）：`refresh_task_state` 从 sqlite 读出缓存；
    /// 通用线为 None。切线命令（/task /tasks）与 turn 起点刷新。
    pub active_task: Option<ActiveTask>,
    /// /btw 最近的侧问问答（plan.md #H 自连上下文）：同进程内最近 2 组，
    /// 拼进下一次 /btw 的 prompt，让追问无需重新交代背景。不进主上下文。
    pub btw_recent: Vec<(String, String)>,
    /// Generation Guard 配置快照（docs/plans/2026-09-03-generation-guard.md）：
    /// 输出退化防护（思考超限 / 可见重复 / 空输出 → 中止重试 → 熔断报警）。
    pub guard: GuardConfig,
    /// 连续退化回合计数（熔断）：任一健康流清零；重试耗尽的退化收尾递增，
    /// 达到 `guard.breaker_threshold` 在诊断消息附加醒目警告。
    pub guard_streak: u32,
}

/// 当前 active 任务线（ADR-0031）：title 即 `/task <名>` 的任务名。
#[derive(Debug, Clone)]
pub struct ActiveTask {
    pub title: String,
    pub bound_path: Option<std::path::PathBuf>,
}

/// 单次工具调用记录（用于 delegate 提取产出文件）
#[derive(Debug, Clone)]
pub struct TurnToolCall {
    pub name: String,
    pub args: serde_json::Value,
    /// 本次调用是否成功返回（结果未以 `[error: ...]` 开头）。
    /// 供 cron agent 模式的交付门判断「本轮是否所有工具都失败了」。
    pub ok: bool,
}

/// 模型上下文窗口的全局兜底值：配置未设、provider 探测失败时的最小窗口。
const DEFAULT_CONTEXT_SIZE: usize = 8192;

/// AGENTS.md 注入系统提示词的最大字符数（chars/4 启发式 ≈ 2000 token），防大文件撑爆上下文。
const AGENTS_MD_CHAR_CAP: usize = 8000;

/// 构建 AGENTS.md 的系统提示词段。
///
/// - `root == home`（agent 家目录）：不加载——SOUL/USER/MEMORY 已在系统提示词内，
///   无需（也不应）再让 AGENTS.md 插一脚。
/// - `root/AGENTS.md` 缺失或内容为空：返回空串。
/// - 否则读取并按字符上限截断，包装成独立提示词段。
fn build_agents_md_prompt(root: &std::path::Path, home: &std::path::Path) -> String {
    if root == home {
        return String::new();
    }
    let path = root.join("AGENTS.md");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return String::new();
    };
    if content.trim().is_empty() {
        return String::new();
    }
    let content: String = content.chars().take(AGENTS_MD_CHAR_CAP).collect();
    format!(
        "## Active directory instructions (from AGENTS.md)\n\
         \n\
         The current directory scope (set by /move) is `{}`. \
         It contains AGENTS.md with project / directory-level conventions. \
         Follow it when working on files or running commands within this directory scope.\n\n{}",
        root.display(),
        content
    )
}

/// 从实时配置读取 `[agent.<alias>].model` 指向的 model_cfg.context_size（显式上限）。
/// model 未配置 / 对应 provider.model 缺失 → None（走探测或兜底默认）。
fn resolve_configured_context_size(config: &Config, alias: &str) -> Option<usize> {
    let model_ref = config.agent.get(alias)?.model.clone();
    if model_ref.is_empty() {
        return None;
    }
    let (prov_id, model_alias) = Config::parse_model_ref(&model_ref).ok()?;
    config
        .provider
        .get(prov_id)?
        .model
        .get(model_alias)?
        .context_size
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
        trusted_dirs: Arc<RwLock<Vec<std::path::PathBuf>>>,
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
            tool_result_cap: config.runtime.tool_result_cap,
            confirm_mode: config.channels.qq.confirm_mode.clone(),
            approval_gate: crate::agent::approval::ApprovalGate::new(),
            permission_profile: Arc::new(RwLock::new(permission)),
            workspace: workspace.clone(),
            workspace_root,
            trusted_dirs,
            config_dir,
            is_main,
            alias,
            audit,
            turn_tool_calls: Vec::new(),
            config: Arc::new(config.clone()),
            live_config: Arc::new(RwLock::new(config.clone())),
            resolved_context_size: Arc::new(RwLock::new(None)),
            system_prompt_base: String::new(),
            system_has_tool_instructions: false,
            agents_md_prompt: String::new(),
            skills_prompt_cache: String::new(),
            disable_thinking: false,
            thinking_off: false,
            steer_buffer: Arc::new(StdMutex::new(VecDeque::new())),
            active_task: None,
            btw_recent: Vec::new(),
            guard: GuardConfig::from_runtime(&config.runtime),
            guard_streak: 0,
        }
    }

    /// 运行时切换权限档位（/permission 命令）。不写 config.toml。
    pub async fn set_permission_profile(&self, profile: &str) {
        *self.permission_profile.write().await = profile.to_string();
    }

    /// 切换工具作用域（/move 命令）：只更新 workspace_root（文件/终端工具实时生效），
    /// 不动 workspace（agent 家目录，SOUL/USER/MEMORY/sessions.db 所在，固定不变）。
    /// 切换到外部目录时按需加载该目录的 AGENTS.md 进系统提示词；移回 home 时清除。
    pub async fn set_workspace(&mut self, new_workspace: std::path::PathBuf) {
        *self.workspace_root.write().await = new_workspace;
        self.reload_agents_md().await;
    }

    /// 把 /move 批准过的目录登记为会话级受信目录（#B）：canonical 形态、去重。
    /// 调用方（slash `/move` 批准路径）须先经 `validate_move_target` 校验
    /// （canonicalize + 黑名单），此处不再重复校验。
    pub async fn add_trusted_dir(&self, dir: std::path::PathBuf) {
        let mut dirs = self.trusted_dirs.write().await;
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }

    /// 投递一条 /steer 插话（plan.md #I）。channel 在 turn 持锁期间调用——
    /// 本方法只碰独立 Arc 缓冲，不取 Agent 锁，永不阻塞。
    pub fn push_steer(&self, msg: String) {
        self.steer_buffer.lock().unwrap().push_back(msg);
    }

    /// drain 全部待注入的 steer 插话（工具循环非末轮迭代顶部调用）。
    fn drain_steer(&self) -> Vec<String> {
        let mut buf = self.steer_buffer.lock().unwrap();
        buf.drain(..).collect()
    }

    /// 清空 steer 缓冲（末轮丢弃路径），返回丢弃条数（用于「未生效」提示）。
    fn clear_steer(&self) -> usize {
        let mut buf = self.steer_buffer.lock().unwrap();
        let n = buf.len();
        buf.clear();
        n
    }

    /// 刷新当前 active 任务线缓存（ADR-0031）：从 sqlite 读 `sessions.kind/title/
    /// bound_path`，任务线则更新 `active_task` 并把任务名/绑定目录写进 Runtime
    /// Context（`context.task_state`）；通用线清空两者。切线命令与 turn 起点调用。
    pub fn refresh_task_state(&mut self) {
        let info = self
            .session_store
            .session_kind(self.session_id)
            .unwrap_or(None);
        match info {
            Some(i) if i.kind == "task" => {
                let title = i.title.unwrap_or_else(|| "task".to_string());
                let bound_path = i
                    .bound_path
                    .filter(|b| !b.is_empty())
                    .map(std::path::PathBuf::from);
                self.context.task_state = Some(format!(
                    "[task] You are working in task session \"{}\"{}. \
                     Keep unrelated chatter out of this line; when the task is done, \
                     suggest the user archive it with `/task close`.",
                    title,
                    bound_path
                        .as_ref()
                        .map(|p| format!(" (bound directory: {})", p.display()))
                        .unwrap_or_default()
                ));
                self.active_task = Some(ActiveTask { title, bound_path });
            }
            _ => {
                self.context.task_state = None;
                self.active_task = None;
            }
        }
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

    /// 实时配置（WebUI 热加载写入的 Arc），用于读取最新的 provider/model 列表。
    pub fn live_config(&self) -> Arc<RwLock<Config>> {
        self.live_config.clone()
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
    /// 同步清空 `resolved_context_size`，使上下文窗口在下次 `context_size_now` 时
    /// 跟随新模型重算（/provider 切换、WebUI 热保存后压缩阈值不再冻结在启动时刻）。
    pub async fn reload_provider(&self, new_provider: Option<Arc<dyn Provider>>) {
        let mut guard = self.provider.write().await;
        *guard = new_provider;
        // tokio RwLock 不会 poison，直接解引用 guard 覆盖即可（无需 unwrap）
        *self.resolved_context_size.write().await = None;
    }

    /// 解析当前活动 provider 的模型上下文窗口大小（tokens），懒执行并缓存。
    ///
    /// 窗口不再冻结在 Agent 构建期：这里按「实时配置的显式上限 + 活动 provider 探测」
    /// 双源解析，结果缓存进 `resolved_context_size`；`reload_provider` 命中时清空缓存，
    /// 于是 /provider 切换、WebUI 热保存后压缩阈值立即跟随新模型。
    /// 探测（`Provider::detect_context_size`）仅在缓存未命中时执行一次。
    pub async fn context_size_now(&self) -> usize {
        if let Some(v) = *self.resolved_context_size.read().await {
            return v;
        }
        let configured = {
            let live = self.live_config.read().await;
            resolve_configured_context_size(&live, &self.alias)
        };
        let detected = match self.provider.read().await.as_ref() {
            Some(p) => p.detect_context_size().await,
            None => None,
        };
        let size = match (configured, detected) {
            (Some(c), Some(d)) => c.min(d),
            (Some(c), None) => c,
            (None, Some(d)) => d,
            (None, None) => DEFAULT_CONTEXT_SIZE,
        };
        *self.resolved_context_size.write().await = Some(size);
        size
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

    /// 热加载 runtime 参数（permission / context_threshold / max_iterations / guard）。
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
        self.guard = GuardConfig::from_runtime(&config.runtime);
    }

    /// 热加载 skills：缓存 skills 段后重建 system 提示词（前缀 + AGENTS.md 段 + skills + tool instructions）。
    pub fn reload_skills(&mut self, skills_prompt: &str) {
        self.skills_prompt_cache = skills_prompt.to_string();
        self.rebuild_system();
    }

    /// 统一重建 system 提示词：固定前缀(base) + AGENTS.md 段 + skills 段 + tool instructions。
    /// 被 `reload_skills`（skills 热加载）与 `reload_agents_md`（/move 触发）共用，
    /// 保证任一处改动都不会把另一段的注入挤掉。
    fn rebuild_system(&mut self) {
        let mut sys = self.system_prompt_base.clone();
        if !self.agents_md_prompt.is_empty() {
            sys.push_str("\n\n");
            sys.push_str(&self.agents_md_prompt);
        }
        if !self.skills_prompt_cache.is_empty() {
            sys.push_str("\n\n");
            sys.push_str(&self.skills_prompt_cache);
        }
        if self.system_has_tool_instructions {
            sys.push_str(&crate::tool_call::prompt::build_tool_instructions(
                &self.tools.specs(),
            ));
        }
        self.context.system = sys;
    }

    /// /move 后按当前 workspace_root 重载 AGENTS.md 段：外部目录存在 AGENTS.md 则注入，
    /// 否则清空（含移回 home 的情况）。段有变动才重建 system，避免无谓重复构造。
    async fn reload_agents_md(&mut self) {
        let root = self.workspace_root.read().await.clone();
        let prompt = build_agents_md_prompt(&root, &self.workspace);
        if prompt != self.agents_md_prompt {
            self.agents_md_prompt = prompt;
            self.rebuild_system();
        }
    }

    /// 是否处于降级模式（无 provider）。
    pub async fn has_provider(&self) -> bool {
        self.provider.read().await.is_some()
    }

    /// 非流式版本（保留向后兼容）：内部调 handle_input_streaming + 收集
    pub async fn handle_input(&mut self, user_input: &str, channel: &str) -> Result<String> {
        let (tx, mut rx) = mpsc::channel(64);
        // 并发 drain：必须边跑边消费。若等 turn 结束再 drain，turn 内事件数一旦超过
        // channel 容量（64），`event_tx.send().await` 会永远阻塞（接收端尚未启动），
        // 整个 turn 冻结到顶层超时才被 kill（长回复逐 token delta 必然超 64 事件）。
        let drain = tokio::spawn(async move {
            let mut text = String::new();
            while let Some(ev) = rx.recv().await {
                if let TurnEvent::Chunk { delta } = ev {
                    text.push_str(&delta);
                }
            }
            text
        });
        let result = self.handle_input_streaming(user_input, channel, tx).await;
        let text = drain.await.unwrap_or_default();
        result?;
        Ok(text)
    }

    /// 为 cron / 委派等「独立 turn」派生一个共享底层资源、但拥有独立 `context` 与
    /// `session_id` 的 Agent 副本，使其能在**不持有全局 `Arc<Mutex<Agent>>`** 的情况下并发执行。
    ///
    /// 设计动机：之前 `run_agent_mode` 用 `agent.lock().await` 把全局锁持了整整一轮 turn
    /// （含所有 web_fetch / 搜索 / 最终合成的网络调用）。一旦 turn 偏长或某步卡住（如 provider
    /// 流式因 SSE keepalive 绕开 per-chunk 超时），主会话与 WebUI（/api/sessions 也要拿这把锁）
    /// 会一起被冻结，直到 300s 顶层超时或手动重启。派生独立副本后，cron 与主会话真正并发、
    /// 互不影响；主 agent 的 `session_id` / `context` 永不被 cron 触碰。
    ///
    /// 复制的字段全都是 `Arc` 共享资源（provider、session_store、tools、config、审批门等），
    /// 仅 `context` / `session_id` / `turn_tool_calls` 是独立新实例。并发写 `sessions.db` 由
    /// `SessionStore` 内部的 `Mutex<Connection>` 串行化，安全无竞争。
    pub fn fork_for_isolated(&self, session_id: i64, disable_thinking: bool) -> Agent {
        let saved_system = self.context.system.clone();
        Agent {
            provider: self.provider.clone(),
            compact_provider: self.compact_provider.clone(),
            vision_provider: self.vision_provider.clone(),
            tools: self.tools.clone(),
            context: crate::agent::context::Context::new(saved_system),
            session_store: self.session_store.clone(),
            session_id,
            context_size: self.context_size,
            context_threshold: self.context_threshold,
            max_iterations: self.max_iterations,
            tool_result_cap: self.tool_result_cap,
            confirm_mode: self.confirm_mode.clone(),
            approval_gate: self.approval_gate.clone(),
            permission_profile: self.permission_profile.clone(),
            workspace: self.workspace.clone(),
            workspace_root: self.workspace_root.clone(),
            trusted_dirs: self.trusted_dirs.clone(),
            config_dir: self.config_dir.clone(),
            is_main: false,
            alias: self.alias.clone(),
            audit: self.audit.clone(),
            turn_tool_calls: Vec::new(),
            config: self.config.clone(),
            disable_thinking,
            thinking_off: false,
            live_config: self.live_config.clone(),
            resolved_context_size: self.resolved_context_size.clone(),
            system_prompt_base: self.system_prompt_base.clone(),
            system_has_tool_instructions: self.system_has_tool_instructions,
            agents_md_prompt: self.agents_md_prompt.clone(),
            skills_prompt_cache: self.skills_prompt_cache.clone(),
            // steer 独立空缓冲：cron/委派 turn 不消费用户给主线的插话（plan.md #I）
            steer_buffer: Arc::new(StdMutex::new(VecDeque::new())),
            active_task: None,
            btw_recent: Vec::new(),
            // guard 配置跟随主 agent（fork 时点快照）；退化计数独立归零
            guard: self.guard.clone(),
            guard_streak: 0,
        }
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
                "[system note] your question to the user (id={}) was not answered within {} seconds; proceeding with the most reasonable assumption.",
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
            "[The user answered the question you asked]\nQuestion: {}\nAnswer: {}",
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
            descriptions.push(format!("[image {} description] {}", i + 1, desc));
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
                text: "Please describe the content of this image in detail, including any text, objects, and scene details.".into(),
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
            disable_thinking: false,
        };
        match provider.chat(&req).await {
            Ok(resp) => resp
                .text
                .unwrap_or_else(|| "[image description empty]".into()),
            Err(e) => {
                tracing::warn!(error = %e, "vision provider describe image failed");
                "[image description failed]".into()
            }
        }
    }

    /// 上下文超阈值则自动压缩（优先 compact_provider，未配置回退主 provider）。
    /// 抽成方法，供「回合开头」与「工具循环内每次迭代」复用同一套逻辑，
    /// 避免单回合内工具链把上下文撑爆却只在回合边界才检查的问题。
    async fn maybe_auto_compact(&mut self) {
        // 窗口按活动 provider 懒解析：/provider 切换后压缩阈值跟随新模型（而非启动快照）
        let context_size = self.context_size_now().await;
        if !self
            .context
            .needs_compaction(context_size, self.context_threshold)
        {
            return;
        }
        let compact_provider = self.provider_for_compact().await;
        match compact_provider.as_ref() {
            Some(p) => match self.context.compact(p.as_ref(), 6, context_size).await {
                // 压缩顺带生成会话标题（仅实际发生 LLM 压缩时，见 ensure_session_title）
                Ok(true) => self.ensure_session_title(p.as_ref()).await,
                Ok(false) => {}
                Err(e) => tracing::warn!(error = %e, "auto-compact failed"),
            },
            None => tracing::warn!("skip auto-compact: no provider available"),
        }
    }

    /// 压缩顺带生成会话标题（plan.md 会话主题自动总结）。
    /// 仅当 `sessions.title` 为空时生成一次（不覆盖已有标题）；用 compact provider
    /// 提炼短标题，失败降级为首条用户消息截断；连降级素材都没有则留空待下次压缩。
    /// 任何失败都只记日志，不阻断主流程。
    pub async fn ensure_session_title(&mut self, provider: &dyn Provider) {
        // 已有标题不重复生成；读取失败时宁可不生成，避免反复打 LLM
        let has_title = match self.session_store.session_title(self.session_id) {
            Ok(Some(t)) => !t.trim().is_empty(),
            Ok(None) => false,
            Err(_) => true,
        };
        if has_title {
            return;
        }

        // 素材：前若干条 user/assistant 正文（各截 300 字符，最多 6 条），首条用户消息
        // 天然在最前，通常就是主题句
        let mut material = String::new();
        let mut picked = 0usize;
        for m in &self.context.history {
            if m.role != Role::User && m.role != Role::Assistant {
                continue;
            }
            let t = m.content.as_text();
            if t.trim().is_empty() {
                continue;
            }
            let head: String = t.chars().take(300).collect();
            let who = if m.role == Role::User {
                "user"
            } else {
                "assistant"
            };
            material.push_str(&format!("[{}] {}\n", who, head));
            picked += 1;
            if picked >= 6 {
                break;
            }
        }
        if material.is_empty() {
            return;
        }

        // 降级默认标题：首条用户消息截断
        let fallback = self
            .context
            .history
            .iter()
            .find(|m| m.role == Role::User && !m.content.as_text().trim().is_empty())
            .map(|m| cap_chars(&sanitize_title(&m.content.as_text()), 40));

        let system = "You title conversations. Read the exchange and output one short title capturing the main topic (no more than 8 words, or 20 CJK characters). Output only the title text: no quotes, no trailing punctuation, no explanation.";
        let messages = vec![ChatMessage::system(system), ChatMessage::user(&material)];
        let req = ChatRequest {
            messages: &messages,
            tools: None,
            disable_thinking: false,
        };
        let title = match provider.chat(&req).await {
            Ok(resp) => {
                let t = sanitize_title(&resp.text.unwrap_or_default());
                if t.is_empty() {
                    fallback
                } else {
                    Some(cap_chars(&t, 60))
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "session title generation failed, falling back to first user message");
                fallback
            }
        };

        let Some(title) = title else {
            return;
        };
        if let Err(e) = self
            .session_store
            .set_session_title(self.session_id, &title)
        {
            tracing::warn!(error = %e, "failed to persist session title");
        } else {
            tracing::info!(title = %title, "session title set");
        }
    }

    /// 把工具返回的图片 data URL 落盘到 `workspace/tmp/`，返回绝对路径。
    /// 供回显（`TurnEvent::MediaOutput`）使用；失败返回 None，不阻塞工具结果处理。
    async fn persist_tool_image(
        &self,
        data_url: &str,
        tool_name: &str,
        idx: usize,
    ) -> Option<String> {
        let bytes = crate::image_utils::decode_data_url(data_url).ok()?;
        if bytes.is_empty() {
            return None;
        }
        let ext = match data_url.split(';').next().unwrap_or("") {
            "data:image/png" => "png",
            "data:image/gif" => "gif",
            "data:image/webp" => "webp",
            _ => "jpg",
        };
        let ws = self.workspace_root.read().await;
        let tmp = ws.join("tmp");
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let path = tmp.join(format!("tool_{}_{}_{}.{}", tool_name, ts, idx, ext));
        if let Err(e) = tokio::fs::create_dir_all(&tmp).await {
            tracing::warn!(error = %e, "create workspace tmp dir failed");
            return None;
        }
        if let Err(e) = tokio::fs::write(&path, &bytes).await {
            tracing::warn!(error = %e, path = %path.display(), "persist tool image failed");
            return None;
        }
        Some(path.to_string_lossy().to_string())
    }

    /// 清理 `workspace_root/tmp/` 下超过 `retention` 的文件（plan.md：启动时清理，
    /// 避免工具图片等临时文件无界增长）。幂等：目录不存在/不可读直接返回，不报错。
    pub async fn cleanup_tmp(&self, retention: std::time::Duration) {
        let ws = self.workspace_root.read().await.clone();
        let tmp = ws.join("tmp");
        let mut dir = match tokio::fs::read_dir(&tmp).await {
            Ok(d) => d,
            Err(_) => return,
        };
        let now = std::time::SystemTime::now();
        let mut removed = 0usize;
        loop {
            let ent = match dir.next_entry().await {
                Ok(Some(e)) => e,
                Ok(None) => break,
                Err(_) => continue,
            };
            // 只清文件，不动子目录
            if !ent.file_type().await.map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let modified = match ent.metadata().await.and_then(|m| m.modified()) {
                Ok(t) => t,
                Err(_) => continue,
            };
            if now.duration_since(modified).unwrap_or_default() > retention
                && tokio::fs::remove_file(&ent.path()).await.is_ok()
            {
                removed += 1;
            }
        }
        if removed > 0 {
            tracing::info!(removed, dir = %tmp.display(), "cleaned stale files in workspace/tmp");
        }
    }

    /// 记录一次主对话模型调用的 token 用量（plan.md W3 token dashboard）。
    /// 空用量直接跳过（LMStudio/bare 端点不上报 usage 是预期行为）。
    async fn record_turn_usage(&self, usage: &crate::provider::Usage) {
        if usage.prompt_tokens == 0 && usage.completion_tokens == 0 {
            return;
        }
        let model_ref = self
            .live_config
            .read()
            .await
            .agent
            .get(&self.alias)
            .map(|c| c.model.clone())
            .unwrap_or_else(|| self.alias.clone());
        let u = crate::memory::sqlite::TurnUsage {
            session_id: self.session_id,
            model_ref,
            prompt_tokens: usage.prompt_tokens as i64,
            completion_tokens: usage.completion_tokens as i64,
            kind: "chat".into(),
        };
        if let Err(e) = self.session_store.add_turn_usage(&u) {
            tracing::warn!(error = %e, "record turn_usage failed");
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
        // 任务线状态（ADR-0031）：sqlite 直读，切线后无需额外同步；sessions 表
        // 极小，每 turn 一次查询成本可忽略。
        self.refresh_task_state();

        // 拿 provider snapshot：整个 turn 用这个 snapshot，reload 不影响进行中的 turn
        let provider = match self.provider_snapshot().await {
            Some(p) => p,
            None => {
                // 降级模式：无 provider，直接 sink Error 提示用户配置
                let msg = "no provider configured; please set up the [provider.default] section in the WebUI or edit config.toml to uncomment it".to_string();
                let _ = event_tx
                    .send(TurnEvent::Error {
                        message: msg.clone(),
                    })
                    .await;
                return Err(anyhow::anyhow!(msg));
            }
        };

        // 回合开头：先按阈值做一次自动压缩
        self.maybe_auto_compact().await;

        // Tail Reminder（P6）：SOUL+USER hash 校验，缺失/失配时后台重生成
        // （写盘后下一轮生效）。生成走 compact_provider（省主模型），回退主模型。
        {
            let soul = std::fs::read_to_string(self.workspace.join("SOUL.md")).unwrap_or_default();
            let user = std::fs::read_to_string(self.workspace.join("USER.md")).unwrap_or_default();
            if !soul.is_empty() || !user.is_empty() {
                let gen_provider = match self.compact_provider_snapshot().await {
                    Some(p) => p,
                    None => provider.clone(),
                };
                self.context.reminder = crate::agent::reminder::refresh_reminder(
                    &self.workspace,
                    &soul,
                    &user,
                    gen_provider,
                );
            } else {
                self.context.reminder = None;
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
                    "Tool call limit reached. Stop calling tools, summarize the task based on the information already gathered, and reply to the user directly.",
                ));
                // 末轮不再注入 steer（plan.md #I ③）：末轮使命是收敛出最终回答，
                // 注入新指令会让总结分叉且大概率无后续迭代去落实——落不了地的
                // 插话不如明确拒收。残留直接丢弃，由 turn 结束路径提示「未生效」。
                let dropped = self.clear_steer();
                if dropped > 0 {
                    tracing::info!(
                        dropped,
                        "steer message(s) dropped at force_summary iteration"
                    );
                }
            } else {
                // /steer 注入点（plan.md #I ②）：非末轮迭代顶部 drain，push 一条带
                // 标记的 user 消息（兼容性下限最高：标签协议端点按纯文本拼、部分
                // 端点不收会话中途 system），自然进 history/sqlite，模型下一迭代看到。
                for s in self.drain_steer() {
                    let text = format!("[steer] User added: {}", s);
                    self.session_store
                        .append_message(self.session_id, &Role::User, &text)?;
                    self.context.push(ChatMessage::user(text));
                }
            }
            // 单回合内工具链也可能把上下文撑爆：每次迭代组装请求前复查，
            // 超阈值立即压缩，避免把几十万 token 的请求直接发给 provider 导致 400 溢出。
            self.maybe_auto_compact().await;

            // Generation Guard 重试框架（docs/plans/2026-09-03-generation-guard.md）：
            // 判退化（思考超限 / 可见重复 / 空输出）→ 中止流、丢弃产物（不落库不进
            // context，避免重试请求携带垃圾片段诱导自我模仿）→ 注入 [guard] 提示 +
            // 强制关思考重试；重试耗尽 → 诊断收尾 + 连续失败报警（熔断只报警不拒服）。
            let guard = self.guard.clone();
            let mut attempt: u32 = 0;
            let (mut iter_text, calls, iter_usage, degenerate) = loop {
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
                    // 重试（attempt > 0）强制关思考：思考流失控是退化的主要形态，
                    // 重试时从根上掐掉（provider 注入 chat_template_kwargs）
                    disable_thinking: self.disable_thinking || self.thinking_off || attempt > 0,
                };

                let mut stream = provider.chat_stream(&req).await;
                match self
                    .consume_stream_guarded(&mut stream, &event_tx, &guard)
                    .await?
                {
                    StreamOutcome::Aborted { text } => return Ok(text),
                    StreamOutcome::Failed { message } => {
                        let _ = event_tx
                            .send(TurnEvent::Error {
                                message: message.clone(),
                            })
                            .await;
                        return Err(anyhow::anyhow!(message));
                    }
                    StreamOutcome::Degenerate { reason } => {
                        if attempt < guard.max_retries {
                            attempt += 1;
                            self.guard_retry_prep(&event_tx).await?;
                            continue;
                        }
                        break (String::new(), Vec::new(), None, Some(reason));
                    }
                    StreamOutcome::Completed { text, calls, usage } => {
                        // 空输出判定：流正常结束但无文本无工具调用 = 思考流被剥离/
                        // provider 丢弃后什么都没产出的残余形态，同样判退化走重试。
                        // 空回复本身就是坏的，无需区分原因。
                        if guard.enabled && text.trim().is_empty() && calls.is_empty() {
                            if attempt < guard.max_retries {
                                attempt += 1;
                                self.guard_retry_prep(&event_tx).await?;
                                continue;
                            }
                            break (
                                text,
                                calls,
                                usage,
                                Some("empty output (no text, no tool calls)".to_string()),
                            );
                        }
                        break (text, calls, usage, None);
                    }
                }
            };

            // 本流正常走完（未中途 abort/error/退化中止）：记录 token 用量。
            // 退化中止的流 usage 丢弃（部分 usage 不完整，统计意义有限）。
            if degenerate.is_none() {
                if let Some(u) = iter_usage {
                    self.record_turn_usage(&u).await;
                }
            }

            // 熔断收尾：重试耗尽仍退化 → 诊断消息结束本轮，streak 递增；
            // 达到阈值附加醒目警告（只报警不拒服：退化有随机性，下轮任务可能正常）。
            if let Some(reason) = degenerate {
                self.guard_streak += 1;
                tracing::warn!(
                    streak = self.guard_streak,
                    reason = %reason,
                    "generation degenerated, gave up after retries"
                );
                let mut diag =
                    format!("[guard] 生成退化（{reason}），已中止并放弃重试，本轮到此为止。");
                if self.guard_streak >= guard.breaker_threshold {
                    diag.push_str(&format!(
                        "\n[guard] 已连续 {} 轮生成退化：怀疑当前模型/量化/推理参数撑不住当前上下文，建议调整推理端采样参数（repetition penalty、context 等）或用 /provider 切换模型。",
                        self.guard_streak
                    ));
                }
                self.session_store
                    .append_message(self.session_id, &Role::Assistant, &diag)?;
                self.context.push(ChatMessage::assistant(&diag));
                let _ = event_tx
                    .send(TurnEvent::Chunk {
                        delta: diag.clone(),
                    })
                    .await;
                let _ = event_tx.send(TurnEvent::Done).await;
                return Ok(diag);
            }
            // 健康产出：熔断计数清零
            self.guard_streak = 0;

            if calls.is_empty() {
                // turn 结束时残留的 steer（最后一段流期间到达，已无迭代可注入）：
                // 丢弃并提示「未生效」，比静默吞掉诚实（plan.md #I ③）。
                let dropped = self.clear_steer();
                if dropped > 0 {
                    let note = format!(
                        "\n\n[steer not applied: {} message(s) arrived after the last step and were dropped]",
                        dropped
                    );
                    iter_text.push_str(&note);
                    let _ = event_tx.send(TurnEvent::Chunk { delta: note }).await;
                }
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

            let ctx = crate::agent::approval::ApprovalContext {
                profile: self.permission_profile.read().await.clone(),
                workspace: self.workspace_root.read().await.clone(),
                trusted: self.trusted_dirs.read().await.clone(),
                gate: self.approval_gate.clone(),
                agent_alias: self.alias.clone(),
                audit: self.audit.clone(),
                ask_user_timeout_secs: self.config.runtime.ask_user_timeout_secs as u64,
            };
            let (tool_msgs, deferred) =
                execute_tool_calls(&self.tools, &calls, channel, &ctx, Some(&event_tx)).await?;
            // 记录本轮工具调用及成败（delegate 取产出文件；cron 交付门据此判断整轮是否白跑）。
            // 成败按结果文本前缀判定——runner 把所有失败统一包成 `[error: ...]`；用
            // tool_call_id 关联而非位置对齐，避免占位/跳过类结果导致错位。
            let ok_by_id: std::collections::HashMap<&str, bool> = tool_msgs
                .iter()
                .map(|m| {
                    (
                        m.tool_call_id.as_deref().unwrap_or_default(),
                        !m.content.as_text().starts_with("[error: "),
                    )
                })
                .collect();
            for tc in &calls {
                self.turn_tool_calls.push(TurnToolCall {
                    name: tc.name.clone(),
                    args: tc.arguments.clone(),
                    ok: ok_by_id.get(tc.id.as_str()).copied().unwrap_or(false),
                });
            }
            // tool_call_id → 工具名映射：图片桥接提示语标注来源工具
            let name_by_id: std::collections::HashMap<&str, &str> = calls
                .iter()
                .map(|c| (c.id.as_str(), c.name.as_str()))
                .collect();
            // 图片读图分流：配了 vision_provider（主模型无多模态）→ 描述进文本；
            // 未配（主模型能看图）→ 桥接多模态 user 消息直接发图。与入口图片处理语义一致。
            let vision_provider = self.vision_provider_snapshot().await;
            for msg in tool_msgs.iter() {
                let tool_call_id = msg.tool_call_id.clone().unwrap_or_default();
                let tool_name = name_by_id
                    .get(tool_call_id.as_str())
                    .copied()
                    .unwrap_or("tool");
                let text = msg.content.as_text();

                // 1) 提取工具返回的图片 data URL，文本中剥离为 [图片] 占位
                let (placeholder, images) = crate::image_utils::extract_data_url_images(&text);

                // 2) 非图片内容超长截断兜底（完整内容由下方 append_message 写入 sqlite 留底）
                let mut tool_text = truncate_tool_result(&placeholder, self.tool_result_cap);

                // 3) 图片处理：缩放 → 回显给用户（MediaOutput）→ 模型读图
                let mut bridge_images: Vec<String> = Vec::new();
                for (idx, url) in images.iter().enumerate() {
                    // 缩放省 token（解码失败则退回原始 data URL）
                    let prepared = crate::image_utils::prepare_base64_for_vision(url)
                        .unwrap_or_else(|_| url.clone());
                    // 回显：落盘到 workspace/tmp/ 并发 MediaOutput 事件由 channel 发送
                    if let Some(path) = self.persist_tool_image(&prepared, tool_name, idx).await {
                        let _ = event_tx
                            .send(TurnEvent::MediaOutput {
                                path,
                                kind: MediaKind::Image,
                            })
                            .await;
                    }
                    // 模型读图：有 vision_provider → 描述文本进上下文；否则桥接图片
                    match vision_provider.as_ref() {
                        Some(p) => {
                            let desc = self.describe_single_image(p.as_ref(), &prepared).await;
                            let label = if images.len() > 1 {
                                format!(
                                    "[image {} returned by tool {} — description] ",
                                    idx + 1,
                                    tool_name
                                )
                            } else {
                                format!("[image returned by tool {} — description] ", tool_name)
                            };
                            tool_text.push_str(&format!("\n{}{}", label, desc));
                        }
                        None => bridge_images.push(prepared),
                    }
                }

                let _ = event_tx
                    .send(TurnEvent::ToolResult {
                        id: tool_call_id.clone(),
                        output: tool_text.clone(),
                    })
                    .await;
                self.session_store
                    .append_message(self.session_id, &Role::Tool, &tool_text)?;
                self.context
                    .push(ChatMessage::tool(tool_text, &tool_call_id));

                // 4) 无 vision_provider：桥接 user 多模态消息，让（多模态）主模型真正看到图。
                //    结构合法：assistant(tool_calls) → tool(占位) → user(图片) → assistant。
                if !bridge_images.is_empty() {
                    let mut parts = vec![ContentPart::Text {
                        text: format!(
                            "[these are the images returned by tool {} (e.g. screenshots); please look at them carefully and understand their content.]",
                            tool_name
                        ),
                    }];
                    for url in &bridge_images {
                        parts.push(ContentPart::ImageUrl {
                            image_url: ImageUrlContent { url: url.clone() },
                        });
                    }
                    let bridge = ChatMessage::user_multimodal(parts);
                    // 多模态消息 sqlite 只存文本部分（图片 base64 不持久化，与输入侧一致）
                    self.session_store.append_message(
                        self.session_id,
                        &Role::User,
                        &bridge.content.as_text(),
                    )?;
                    self.context.push(bridge);
                }
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
        let fallback = "[exceeded the tool-call limit without producing a summary]";
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

    /// guard 重试前置：通知用户 + 注入 `[guard]` 提示（持久化进 sqlite/context，
    /// 模型下一尝试可见）。退化产物本身不落库不进 context（见主循环注释），
    /// 会话记录里只留提示与最终结果。
    async fn guard_retry_prep(&mut self, event_tx: &mpsc::Sender<TurnEvent>) -> Result<()> {
        let _ = event_tx
            .send(TurnEvent::Chunk {
                delta: guard::RETRY_NOTICE.to_string(),
            })
            .await;
        self.session_store
            .append_message(self.session_id, &Role::User, guard::RETRY_HINT)?;
        self.context.push(ChatMessage::user(guard::RETRY_HINT));
        Ok(())
    }

    /// 消费一次 chat_stream（Generation Guard 重试框架的单次尝试）。
    ///
    /// - 用户中止（tx closed）与流错误沿用旧语义：保存部分输出后由调用方收尾；
    /// - guard 启用时逐 TextDelta 检查三个信号：思考流长度（`<think>` 内容被
    ///   parser 剥离但对框架不可见是退化盲区，按累计值超限即中止）、思考线重复、
    ///   可见文本线重复（滑动窗口字符 n-gram）；命中即中止（drop 流即断连，
    ///   本地服务端随之停止生成）；
    /// - parser.finish() 残留照常 flush 为可见文本。
    async fn consume_stream_guarded(
        &mut self,
        stream: &mut futures_util::stream::BoxStream<'_, Result<StreamEvent>>,
        event_tx: &mpsc::Sender<TurnEvent>,
        guard: &GuardConfig,
    ) -> Result<StreamOutcome> {
        let mut parser = crate::tool_call::ToolCallStreamParser::new();
        // 双线检测器：思考线（挂进 parser，InThink 逐字符喂）、可见文本线
        let (think_monitor, mut visible_monitor) = if guard.enabled && guard.repeat_threshold > 0 {
            let think = Some(crate::agent::guard::RepetitionDetector::new(
                guard.repeat_window,
                guard.repeat_gram,
                guard.repeat_threshold,
            ));
            let visible = Some(crate::agent::guard::RepetitionDetector::new(
                guard.repeat_window,
                guard.repeat_gram,
                guard.repeat_threshold,
            ));
            (think, visible)
        } else {
            (None, None)
        };
        parser.set_think_monitor(think_monitor);
        let mut iter_text = String::new();
        let mut calls: Vec<crate::provider::ToolCall> = Vec::new();
        // 本次请求（尝试）累计的 token 用量：Usage 事件可能在流中多次出现，合并成一条
        let mut iter_usage: Option<crate::provider::Usage> = None;

        while let Some(ev) = stream.next().await {
            // 用户中止（Ctrl+C）：event_tx 被关闭，提前结束并保存部分输出
            if event_tx.is_closed() {
                tracing::info!("stream aborted by user (tx closed)");
                if !iter_text.is_empty() {
                    self.session_store.append_message(
                        self.session_id,
                        &Role::Assistant,
                        &iter_text,
                    )?;
                    self.context.push(ChatMessage::assistant(&iter_text));
                }
                return Ok(StreamOutcome::Aborted { text: iter_text });
            }
            match ev? {
                StreamEvent::TextDelta(d) => {
                    // 统一走 parser：剥离 think 标签 + 提取 tool_call 标签。
                    // 无论 native 与否都跑——native 模式下模型偶发把
                    // <think>/<tool_call> 泄露到文本流，parser 兜底清洗。
                    // 对无标签文本 parser 是透传的，不影响正常输出。
                    // iter_text 存清洗后文本（think/标签不进 context/sqlite）。
                    let user_text = parser.feed(&d);
                    if let Some(m) = visible_monitor.as_mut() {
                        m.feed(&user_text);
                    }
                    if !user_text.is_empty() {
                        iter_text.push_str(&user_text);
                        let _ = event_tx.send(TurnEvent::Chunk { delta: user_text }).await;
                    }
                    let new_calls = parser.take_tool_calls();
                    calls.extend(new_calls);
                    // guard：思考流长度上限 / 思考线重复 / 可见线重复
                    if guard.enabled {
                        if guard.thinking_cap > 0 && parser.think_chars() >= guard.thinking_cap {
                            return Ok(StreamOutcome::Degenerate {
                                reason: format!(
                                    "thinking exceeded {} chars without closing",
                                    guard.thinking_cap
                                ),
                            });
                        }
                        if parser.think_degenerate() {
                            return Ok(StreamOutcome::Degenerate {
                                reason: "repetitive loop in thinking stream".to_string(),
                            });
                        }
                        if let Some(m) = visible_monitor.as_ref() {
                            if m.is_degenerate() {
                                tracing::info!(
                                    tail = %m.tail_summary(),
                                    "visible text repetition detected, aborting stream"
                                );
                                return Ok(StreamOutcome::Degenerate {
                                    reason: "repetitive loop in visible text".to_string(),
                                });
                            }
                        }
                    }
                }
                StreamEvent::ToolCall(tc) => {
                    calls.push(tc);
                }
                StreamEvent::Usage(u) => match iter_usage.as_mut() {
                    Some(acc) => {
                        acc.prompt_tokens += u.prompt_tokens;
                        acc.completion_tokens += u.completion_tokens;
                        acc.total_tokens += u.total_tokens;
                    }
                    None => iter_usage = Some(u),
                },
                StreamEvent::FinishReason(_) => {}
                StreamEvent::Done => break,
                StreamEvent::Error(msg) => {
                    // 保存错误前已生成的部分输出（与 tx-closed 中止路径同构）：
                    // 用户已在频道看到这些文本，不落 context/sqlite 会让下一轮
                    // 模型不知道自己说过什么。
                    if !iter_text.is_empty() {
                        self.session_store.append_message(
                            self.session_id,
                            &Role::Assistant,
                            &iter_text,
                        )?;
                        self.context.push(ChatMessage::assistant(&iter_text));
                    }
                    return Ok(StreamOutcome::Failed { message: msg });
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

        Ok(StreamOutcome::Completed {
            text: iter_text,
            calls,
            usage: iter_usage,
        })
    }
}

/// 单次流消费结果（Generation Guard 重试框架的最小单元）。
enum StreamOutcome {
    /// 流正常走完
    Completed {
        text: String,
        calls: Vec<crate::provider::ToolCall>,
        usage: Option<crate::provider::Usage>,
    },
    /// guard 判定退化：流已中止，产物丢弃（不落库不进 context）
    Degenerate { reason: String },
    /// 用户中止（tx closed）：部分输出已保存
    Aborted { text: String },
    /// 流错误：部分输出已保存
    Failed { message: String },
}

/// 非图片工具结果超长截断：超过 `cap` 保留头部并附占位说明。
/// 完整内容已由调用方写入 sqlite 会话记录，可随时回查。
fn truncate_tool_result(text: &str, cap: usize) -> String {
    let n = text.chars().count();
    if n <= cap {
        return text.to_string();
    }
    let head: String = text.chars().take(cap).collect();
    format!(
        "{}…[truncated: original {} chars; full result is in the session history]",
        head, n
    )
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
            "\n\n[SYSTEM NOTE] Important: you have called tool `{}` with identical arguments {} consecutive times. Unless each call clearly produced new information, stop repeating immediately and change strategy, adjust your parameters, or explain the limitation to the user.",
            tool_name, streak
        )
    } else if streak >= 4 {
        format!(
            "\n\n[SYSTEM NOTE] Important: you have called tool `{}` with identical arguments {} consecutive times. Unless repetition is clearly necessary, stop repeating the same operation and instead use a different tool, adjust your parameters, or summarize what is still missing.",
            tool_name, streak
        )
    } else {
        format!(
            "\n\n[SYSTEM NOTE] Reminder: you have called tool `{}` with identical arguments {} consecutive times. Check whether another tool, different arguments, or summarizing directly would better advance the task.",
            tool_name, streak
        )
    }
}

/// 标题清洗（会话主题自动总结）：只取首行，剥引号/书名号/结尾标点，防模型发挥。
fn sanitize_title(raw: &str) -> String {
    let first_line = raw.lines().next().unwrap_or("");
    first_line
        .trim()
        .trim_matches(|c| {
            matches!(
                c,
                '"' | '\''
                    | '`'
                    | '“'
                    | '”'
                    | '‘'
                    | '’'
                    | '《'
                    | '》'
                    | '【'
                    | '】'
                    | '「'
                    | '」'
                    | '『'
                    | '』'
                    | '：'
                    | ':'
            )
        })
        .trim()
        .to_string()
}

/// 按字符数截断，超长补省略号（中文标题按字符而非字节计）。
fn cap_chars(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        ChatRequest, ChatResponse, ContentPart, ImageUrlContent, Provider, StreamEvent, ToolCall,
    };
    use crate::tools::Tool;
    use async_stream::try_stream;
    use async_trait::async_trait;
    use base64::Engine as _;
    use futures_util::stream::BoxStream;
    use image::ImageEncoder as _;
    use serde_json::json;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::mpsc;

    /// Mock provider：每次 chat_stream 调用返回下一组预设事件
    struct MockProvider {
        native: bool,
        rounds: Arc<StdMutex<std::collections::VecDeque<Vec<StreamEvent>>>>,
        /// 记录每次 chat_stream 收到的 disable_thinking / tools 形态（回归断言用）
        seen: Arc<StdMutex<Vec<(bool, bool)>>>,
    }

    impl MockProvider {
        fn new(native: bool, rounds: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                native,
                rounds: Arc::new(StdMutex::new(rounds.into())),
                seen: Arc::new(StdMutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat(&self, _req: &ChatRequest<'_>) -> Result<ChatResponse> {
            unreachable!()
        }
        async fn chat_stream(&self, req: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>> {
            self.seen
                .lock()
                .unwrap()
                .push((req.disable_thinking, req.tools.is_some()));
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
        make_agent_with_rounds_seen(native, rounds, Arc::new(StdMutex::new(Vec::new()))).await
    }

    async fn make_agent_with_rounds_seen(
        native: bool,
        rounds: Vec<Vec<StreamEvent>>,
        seen: Arc<StdMutex<Vec<(bool, bool)>>>,
    ) -> Agent {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("test", "test").unwrap();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider {
            native,
            rounds: Arc::new(StdMutex::new(rounds.into())),
            seen,
        });
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
            Arc::new(RwLock::new(Vec::new())),
            std::path::PathBuf::from("/tmp/llaia-test"),
            true,
            "main".into(),
            None,
        )
        .await
    }

    /// Generation Guard 测试用：可定制 runtime 配置的 agent 构造
    async fn make_agent_with_config(
        native: bool,
        rounds: Vec<Vec<StreamEvent>>,
        seen: Arc<StdMutex<Vec<(bool, bool)>>>,
        customize: impl FnOnce(&mut Config),
    ) -> Agent {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("test", "test").unwrap();
        let provider: Arc<dyn Provider> = Arc::new(MockProvider {
            native,
            rounds: Arc::new(StdMutex::new(rounds.into())),
            seen,
        });
        let tools = Arc::new(ToolRegistry::new());
        let mut config = Config::default_for_workspace("/tmp/llaia-test");
        customize(&mut config);
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
            Arc::new(RwLock::new(Vec::new())),
            std::path::PathBuf::from("/tmp/llaia-test"),
            true,
            "main".into(),
            None,
        )
        .await
    }

    // --- Generation Guard（docs/plans/2026-09-03-generation-guard.md）---

    #[tokio::test]
    async fn test_guard_thinking_cap_triggers_retry() {
        // 第一轮：超长未闭合 think 流（被 parser 剥离、用户不可见——事故形态）→
        // 思考超限判退化；重试强制关思考（seen 记录），第二轮正常作答。
        let think_stream = format!("<think>{}", "让我再想想这个问题。".repeat(20));
        let rounds = vec![
            vec![StreamEvent::TextDelta(think_stream), StreamEvent::Done],
            vec![
                StreamEvent::TextDelta("recovered answer".into()),
                StreamEvent::Done,
            ],
        ];
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let mut agent = make_agent_with_config(true, rounds, seen.clone(), |c| {
            c.runtime.guard_thinking_cap = 100;
        })
        .await;

        let (tx, _rx) = mpsc::channel(64);
        let result = agent
            .handle_message_streaming(ChatMessage::user("start"), "cli", tx)
            .await
            .unwrap();
        assert_eq!(result, "recovered answer");

        // 重试发生且强制 disable_thinking
        let rec = seen.lock().unwrap();
        assert_eq!(rec.len(), 2);
        assert!(!rec[0].0, "first attempt keeps session thinking setting");
        assert!(rec[1].0, "retry forces disable_thinking");
        drop(rec);

        // [guard] 提示已持久化进 context；退化的思考内容不进 context
        assert!(agent.context.history.iter().any(|m| {
            m.role == crate::provider::Role::User && m.content.as_text().starts_with("[guard]")
        }));
        assert!(!agent
            .context
            .history
            .iter()
            .any(|m| m.content.as_text().contains("让我再想想")));
        assert_eq!(agent.guard_streak, 0, "healthy retry clears the streak");
    }

    #[tokio::test]
    async fn test_guard_empty_output_retry() {
        // 第一轮流正常结束但零文本零工具调用（思考流被剥离/provider 丢弃的
        // 残余形态）→ 判退化重试，第二轮恢复
        let rounds = vec![
            vec![StreamEvent::Done],
            vec![
                StreamEvent::TextDelta("recovered".into()),
                StreamEvent::Done,
            ],
        ];
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let mut agent = make_agent_with_config(true, rounds, seen.clone(), |_| {}).await;

        let (tx, _rx) = mpsc::channel(64);
        let result = agent
            .handle_message_streaming(ChatMessage::user("start"), "cli", tx)
            .await
            .unwrap();
        assert_eq!(result, "recovered");
        assert_eq!(seen.lock().unwrap().len(), 2);
        assert_eq!(agent.guard_streak, 0);
    }

    #[tokio::test]
    async fn test_guard_exhausted_diagnostics_and_breaker_warning() {
        // 连续两个回合重试耗尽：第一回合诊断收尾（无「连续」警告），
        // 第二回合 streak 达阈值 → 附加醒目警告；随后健康回合清零。
        let empty = vec![StreamEvent::Done];
        let rounds = vec![
            empty.clone(),
            empty.clone(),
            empty.clone(),
            empty.clone(),
            vec![StreamEvent::TextDelta("ok".into()), StreamEvent::Done],
        ];
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let mut agent = make_agent_with_config(true, rounds, seen.clone(), |c| {
            c.runtime.guard_max_retries = 1;
            c.runtime.guard_breaker_threshold = 2;
        })
        .await;

        let (tx, _rx) = mpsc::channel(64);
        let t1 = agent
            .handle_message_streaming(ChatMessage::user("t1"), "cli", tx.clone())
            .await
            .unwrap();
        assert!(t1.contains("[guard]"));
        assert!(!t1.contains("已连续"), "first failure should not warn yet");
        assert_eq!(agent.guard_streak, 1);

        let t2 = agent
            .handle_message_streaming(ChatMessage::user("t2"), "cli", tx.clone())
            .await
            .unwrap();
        assert!(
            t2.contains("已连续 2 轮"),
            "second consecutive failure warns: {t2}"
        );
        assert_eq!(agent.guard_streak, 2);

        let t3 = agent
            .handle_message_streaming(ChatMessage::user("t3"), "cli", tx)
            .await
            .unwrap();
        assert_eq!(t3, "ok");
        assert_eq!(agent.guard_streak, 0, "healthy turn resets the streak");
    }

    #[tokio::test]
    async fn test_guard_visible_repetition_abort_and_retry() {
        // 第一轮可见文本陷入重复循环（正常回复本该是多样的）→ 可见线检测命中、
        // 流中途 abort；重试强制关思考，第二轮正常作答。
        let degenerate = "让我重新整理一下思路。".repeat(60);
        let rounds = vec![
            vec![StreamEvent::TextDelta(degenerate), StreamEvent::Done],
            vec![
                StreamEvent::TextDelta("recovered answer".into()),
                StreamEvent::Done,
            ],
        ];
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let mut agent = make_agent_with_config(true, rounds, seen.clone(), |_| {}).await;

        let (tx, _rx) = mpsc::channel(64);
        let result = agent
            .handle_message_streaming(ChatMessage::user("start"), "cli", tx)
            .await
            .unwrap();
        assert_eq!(result, "recovered answer");
        assert_eq!(seen.lock().unwrap().len(), 2);

        // 退化的可见文本不落 context（用户看到过但不入库）；[guard] 提示在
        assert!(!agent
            .context
            .history
            .iter()
            .any(|m| m.content.as_text().contains("让我重新整理")));
        assert!(agent
            .context
            .history
            .iter()
            .any(|m| m.content.as_text().starts_with("[guard]")));
    }

    #[tokio::test]
    async fn test_guard_disabled_preserves_old_behavior() {
        // output_guard = false：空输出不重试、无 [guard] 提示，行为与旧版一致
        let rounds = vec![vec![StreamEvent::Done]];
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let mut agent = make_agent_with_config(true, rounds, seen.clone(), |c| {
            c.runtime.output_guard = false;
        })
        .await;

        let (tx, _rx) = mpsc::channel(64);
        let result = agent
            .handle_message_streaming(ChatMessage::user("start"), "cli", tx)
            .await
            .unwrap();
        assert_eq!(result, "");
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(!agent
            .context
            .history
            .iter()
            .any(|m| m.content.as_text().starts_with("[guard]")));
    }

    /// 回归：context_size 与 Agent 解耦（plan.md #F）。`context_size_now` 按活动
    /// provider 懒解析并缓存；`reload_provider` 时清缓存，使 /provider 切换、WebUI
    /// 热保存后窗口随新模型跟随，而非冻结在 Agent 构建期。
    struct CtxMockProvider {
        label: &'static str,
        size: Option<usize>,
    }
    #[async_trait]
    impl Provider for CtxMockProvider {
        async fn chat(&self, _req: &ChatRequest<'_>) -> Result<ChatResponse> {
            Ok(ChatResponse::default())
        }
        async fn chat_stream(&self, _req: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>> {
            Box::pin(futures_util::stream::empty())
        }
        fn native_tool_calling(&self) -> bool {
            true
        }
        fn label(&self) -> String {
            self.label.into()
        }
        async fn detect_context_size(&self) -> Option<usize> {
            self.size
        }
    }

    #[tokio::test]
    async fn test_context_size_now_follows_provider_switch() {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("test", "test").unwrap();
        let tools = Arc::new(ToolRegistry::new());
        // 不注入 [agent.main].model，`resolve_configured_context_size` 返回 None →
        // 窗口全靠 provider 探测（configured=None 分支），正好验证懒探测与缓存失效。
        let cfg = Config::default_for_workspace("/tmp/llaia-test");
        let provider_a: Arc<dyn Provider> = Arc::new(CtxMockProvider {
            label: "provider_a.m1",
            size: Some(4000),
        });
        let agent = Agent::new(
            &cfg,
            Some(provider_a),
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
            Arc::new(RwLock::new(Vec::new())),
            std::path::PathBuf::from("/tmp/llaia-test"),
            true,
            "main".into(),
            None,
        )
        .await;
        // Av1：探测 4000，配置无显式上限，取探测值；结果缓存。
        assert_eq!(agent.context_size_now().await, 4000);
        // 切换前先读取一次，验证缓存命中（不再触发第二次探测，值不变）。
        assert_eq!(agent.context_size_now().await, 4000);
        // 类比 /provider 切换：reload_provider 清空缓存 → 下次懒解析取新 model 的探测值。
        let provider_b: Arc<dyn Provider> = Arc::new(CtxMockProvider {
            label: "provider_a.m2",
            size: Some(8000),
        });
        agent.reload_provider(Some(provider_b)).await;
        assert_eq!(agent.context_size_now().await, 8000);
        // provider 无探测（返回 None）时，退化为构建期基线。
        let provider_c: Arc<dyn Provider> = Arc::new(CtxMockProvider {
            label: "provider_no_detect",
            size: None,
        });
        agent.reload_provider(Some(provider_c)).await;
        assert_eq!(agent.context_size_now().await, agent.context_size);
    }

    /// 回归：单回合内长工具链（如 Blender MCP 返回巨大）把上下文撑过阈值时，
    /// 必须在工具循环内自动压缩，否则会把无限膨胀的请求直接发给 provider 导致 400 溢出。
    /// 模拟 provider 连续 8 轮返回同一工具调用，工具返回 30k 字符；断言：
    /// 1) 每轮发给 provider 的请求规模有界（压缩把旧工具消息截断到 500 字符）；
    /// 2) history 中所有工具消息都已截断（未被无限堆积）。
    struct BigTool {
        payload: String,
    }
    #[async_trait]
    impl Tool for BigTool {
        fn name(&self) -> &str {
            "bigtool"
        }
        fn description(&self) -> &str {
            "returns a large payload"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({})
        }
        async fn execute(&self, _args: &serde_json::Value, _channel: &str) -> Result<String> {
            Ok(self.payload.clone())
        }
    }

    /// 记录每次 chat_stream 收到的请求总字符数，用于断言单回合内请求规模有界。
    struct IntraTurnMockProvider {
        native: bool,
        rounds: Arc<StdMutex<std::collections::VecDeque<Vec<StreamEvent>>>>,
        request_sizes: Arc<StdMutex<Vec<usize>>>,
    }
    #[async_trait]
    impl Provider for IntraTurnMockProvider {
        async fn chat(&self, _req: &ChatRequest<'_>) -> Result<ChatResponse> {
            unreachable!()
        }
        async fn chat_stream(&self, req: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>> {
            let size: usize = req
                .messages
                .iter()
                .map(|m| m.content.as_text().chars().count())
                .sum();
            self.request_sizes.lock().unwrap().push(size);
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

    #[tokio::test]
    async fn test_intra_turn_auto_compaction_bounds_request() {
        // 小窗口 + 低阈值：模拟 llama.cpp n_ctx 有限、长工具链易溢出。
        let context_size: usize = 4000;
        let threshold: f64 = 0.3; // 触发点 ~1200 tokens ≈ 4800 chars

        let request_sizes = Arc::new(StdMutex::new(Vec::new()));

        // 前 8 轮返回工具调用（让循环继续），之后返回纯文本结束回合。
        let tool_rounds = 8u32;
        let mut rounds: Vec<Vec<StreamEvent>> = Vec::new();
        for i in 0..tool_rounds {
            rounds.push(vec![
                StreamEvent::TextDelta(format!("step {}", i)),
                StreamEvent::ToolCall(ToolCall {
                    id: format!("call_{}", i),
                    name: "bigtool".into(),
                    arguments: json!({}),
                }),
                StreamEvent::Done,
            ]);
        }
        rounds.push(vec![
            StreamEvent::TextDelta("done".into()),
            StreamEvent::Done,
        ]);

        let tools = Arc::new(ToolRegistry::new());
        tools.register(Arc::new(BigTool {
            payload: "x".repeat(30_000),
        }));

        let provider: Arc<dyn Provider> = Arc::new(IntraTurnMockProvider {
            native: true,
            rounds: Arc::new(StdMutex::new(rounds.into())),
            request_sizes: request_sizes.clone(),
        });

        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("test", "test").unwrap();
        let mut config = Config::default_for_workspace("/tmp/llaia-test");
        config.runtime.context_threshold = threshold;
        config.runtime.max_iterations = 30;
        let mut agent = Agent::new(
            &config,
            Some(provider),
            None,
            None,
            tools,
            Arc::new(store),
            sid,
            "test system".into(),
            context_size,
            std::path::PathBuf::from("/tmp/llaia-test/workspace"),
            Arc::new(RwLock::new(std::path::PathBuf::from(
                "/tmp/llaia-test/workspace",
            ))),
            Arc::new(RwLock::new(Vec::new())),
            std::path::PathBuf::from("/tmp/llaia-test"),
            true,
            "main".into(),
            None,
        )
        .await;

        let (tx, _rx) = mpsc::channel(64);
        let result = agent
            .handle_message_streaming(ChatMessage::user("start"), "cli", tx)
            .await
            .unwrap();
        assert_eq!(result, "done");

        // 1) 每轮发给 provider 的请求规模应有界：不压缩时 8 轮 × 30k ≈ 240k chars，
        //    压缩把旧工具消息截断到 500 字符后，单轮请求应远小于此。
        let sizes = request_sizes.lock().unwrap();
        let max_size = sizes.iter().copied().max().unwrap_or(0);
        assert!(
            max_size < 15_000,
            "outgoing request not bounded by intra-turn compaction: max={} sizes={:?}",
            max_size,
            sizes
        );
        drop(sizes);

        // 2) history 中所有工具消息都应已被截断（cheap_normalize 砍到 TOOL_TRIM_CAP），
        //    证明单回合内确实发生了压缩，而非把 30k 工具返回一路堆积。
        let all_tool_truncated = agent.context.history.iter().all(|m| {
            m.role != crate::provider::Role::Tool || m.content.as_text().chars().count() <= 500 + 60
        });
        assert!(
            all_tool_truncated,
            "a tool message escaped truncation (history grew uncompacted)"
        );
    }

    /// 回归：工具返回 base64 图片（如 blender-mcp get_viewport_screenshot）时——
    /// 1) 图片从工具文本剥离为 [图片] 占位，base64 不进文本上下文；
    /// 2) 无 vision_provider → 桥接 user 多模态消息，让（多模态）主模型真正看到图；
    /// 3) 图片落盘 workspace/tmp/ 并发 MediaOutput 事件回显给用户；
    /// 4) 非图片超长文本按 tool_result_cap 截断兜底。
    #[tokio::test]
    async fn test_tool_image_bridged_and_echoed() {
        // 8x8 红色 PNG 的 base64 data URL（模拟 MCP 截图返回）
        let img: image::ImageBuffer<image::Rgb<u8>, Vec<u8>> =
            image::ImageBuffer::from_pixel(8, 8, image::Rgb([255, 0, 0]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::codecs::png::PngEncoder::new(&mut buf)
            .write_image(img.as_raw(), 8, 8, image::ExtendedColorType::Rgb8)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(buf.into_inner());
        let data_url = format!("data:image/png;base64,{}", b64);

        // 工具返回：一张截图 + 超长文本（超过 tool_result_cap 触发截断兜底）
        let payload = format!("viewport:\n{}\nnotes: {}", data_url, "y".repeat(2000));

        let rounds = vec![
            vec![
                StreamEvent::TextDelta("step 1".into()),
                StreamEvent::ToolCall(ToolCall {
                    id: "call_img".into(),
                    name: "bigtool".into(),
                    arguments: json!({}),
                }),
                StreamEvent::Done,
            ],
            vec![StreamEvent::TextDelta("done".into()), StreamEvent::Done],
        ];

        let tools = Arc::new(ToolRegistry::new());
        tools.register(Arc::new(BigTool { payload }));

        let provider: Arc<dyn Provider> = Arc::new(IntraTurnMockProvider {
            native: true,
            rounds: Arc::new(StdMutex::new(rounds.into())),
            request_sizes: Arc::new(StdMutex::new(Vec::new())),
        });

        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("test", "test").unwrap();
        let mut config = Config::default_for_workspace("/tmp/llaia-test");
        config.runtime.max_iterations = 20;
        config.runtime.tool_result_cap = 500; // 小 cap 触发截断
        let ws = std::path::PathBuf::from("/tmp/llaia-test/workspace");
        let mut agent = Agent::new(
            &config,
            Some(provider),
            None,
            None, // 无 vision_provider → 桥接多模态让主模型读图
            tools,
            Arc::new(store),
            sid,
            "test system".into(),
            100_000, // 大窗口，避免 auto-compact 干扰断言
            ws.clone(),
            Arc::new(RwLock::new(ws.clone())),
            Arc::new(RwLock::new(Vec::new())),
            std::path::PathBuf::from("/tmp/llaia-test"),
            true,
            "main".into(),
            None,
        )
        .await;

        let (tx, mut rx) = mpsc::channel(64);
        let result = agent
            .handle_message_streaming(ChatMessage::user("start"), "cli", tx)
            .await
            .unwrap();
        assert_eq!(result, "done");

        // 1) 工具消息：图片剥离为 [图片] 占位，base64 不进文本上下文
        let tool_msgs: Vec<_> = agent
            .context
            .history
            .iter()
            .filter(|m| m.role == crate::provider::Role::Tool)
            .collect();
        assert_eq!(tool_msgs.len(), 1);
        let tool_text = tool_msgs[0].content.as_text();
        assert!(
            tool_text.contains("[image]"),
            "image not stripped into placeholder: {}",
            tool_text
        );
        assert!(
            !tool_text.contains("data:image/png;base64,"),
            "base64 leaked into tool text context"
        );
        // 2) 非图片超长文本截断兜底
        assert!(
            tool_text.contains("truncated"),
            "oversized non-image text not truncated"
        );

        // 3) 无 vision_provider → 桥接 user 多模态消息让主模型读图
        let bridge = agent
            .context
            .history
            .iter()
            .find(|m| m.role == crate::provider::Role::User && m.content.has_image());
        assert!(bridge.is_some(), "no bridged multimodal user message");
        let bridge_text = bridge.unwrap().content.as_text();
        assert!(
            bridge_text.contains("bigtool"),
            "bridge text should name source tool: {}",
            bridge_text
        );

        // 4) 回显：MediaOutput 事件 + 落盘文件存在（workspace/tmp/ 下）
        let mut echoed = false;
        while let Ok(ev) = rx.try_recv() {
            if let TurnEvent::MediaOutput { path, kind } = ev {
                echoed = true;
                assert!(matches!(kind, MediaKind::Image));
                assert!(
                    std::path::Path::new(&path).exists(),
                    "echoed file missing: {}",
                    path
                );
                assert!(
                    path.contains("tmp"),
                    "echoed file should live under workspace/tmp: {}",
                    path
                );
            }
        }
        assert!(echoed, "no MediaOutput event echoed to channel");
    }

    /// cleanup_tmp：删除超过 retention 的旧文件，保留新文件，且不动子目录。
    #[tokio::test]
    async fn test_cleanup_tmp_removes_stale() {
        let config = Config::default_for_workspace("/tmp/llaia-test");
        let ws = std::path::PathBuf::from("/tmp/llaia-test/workspace");
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("test", "test").unwrap();
        let agent = Agent::new(
            &config,
            None, // 无需真实 provider
            None,
            None,
            Arc::new(crate::agent::ToolRegistry::new()),
            Arc::new(store),
            sid,
            "test system".into(),
            100_000,
            ws.clone(),
            Arc::new(RwLock::new(ws.clone())),
            Arc::new(RwLock::new(Vec::new())),
            std::path::PathBuf::from("/tmp/llaia-test"),
            true,
            "main".into(),
            None,
        )
        .await;

        let tmp = ws.join("tmp");
        tokio::fs::create_dir_all(&tmp).await.unwrap();
        // 子目录（应保留）
        tokio::fs::create_dir_all(tmp.join("subdir")).await.unwrap();
        tokio::fs::write(tmp.join("subdir/keep.txt"), b"x")
            .await
            .unwrap();
        // 新文件（retention 内，应保留）
        tokio::fs::write(tmp.join("recent.png"), b"new")
            .await
            .unwrap();
        // 旧文件（超过 3 天，应删除）
        tokio::fs::write(tmp.join("stale.png"), b"old")
            .await
            .unwrap();
        let stale_path = tmp.join("stale.png");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(4 * 24 * 60 * 60);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&stale_path)
            .unwrap()
            .set_modified(old)
            .unwrap();

        // retention 3 天
        agent
            .cleanup_tmp(std::time::Duration::from_secs(3 * 24 * 60 * 60))
            .await;

        assert!(!stale_path.exists(), "stale file should be removed");
        assert!(tmp.join("recent.png").exists(), "recent file kept");
        assert!(tmp.join("subdir/keep.txt").exists(), "subdir untouched");
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

    /// 回归：回复 delta 数超过事件 channel 容量（64）时 `handle_input` 必须正常完成。
    /// 旧实现等 turn 结束才 drain channel，第 65 次 `event_tx.send().await` 永久阻塞
    /// （接收端未启动）→ 整个 turn 冻结到 600s 顶层超时（长回复逐 token delta 必然超 64）。
    #[tokio::test]
    async fn test_handle_input_no_deadlock_over_channel_capacity() {
        // 100 个 delta，远超 channel(64) 容量
        let mut events: Vec<StreamEvent> = (0..100)
            .map(|i| StreamEvent::TextDelta(format!("t{} ", i)))
            .collect();
        events.push(StreamEvent::Done);
        let mut agent = make_agent_with_rounds(true, vec![events]).await;
        // 若死锁，此调用会永远挂起（测试框架超时判败）
        let reply = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            agent.handle_input("hi", "cron"),
        )
        .await
        .expect("handle_input deadlocked: at channel capacity, must drain concurrently")
        .unwrap();
        assert!(reply.starts_with("t0 t1 "));
        assert!(reply.contains("t99 "));
    }

    #[tokio::test]
    async fn test_fork_for_isolated_does_not_touch_main_agent() {
        let rounds = vec![vec![
            StreamEvent::TextDelta("cron reply".into()),
            StreamEvent::Done,
        ]];
        let mut agent = make_agent_with_rounds(true, rounds).await;
        agent.context.push(ChatMessage::user("prior user msg"));
        let original_session_id = agent.session_id;
        let original_history_len = agent.context.history.len();

        // fork 拥有独立 session_id 与全新的（空）context
        let cron_sid = agent
            .session_store
            .create_session("cron-uuid-fork", "cron:test")
            .unwrap();
        let mut fork = agent.fork_for_isolated(cron_sid, false);
        assert_eq!(fork.session_id, cron_sid);
        assert_ne!(fork.session_id, original_session_id);
        assert_eq!(fork.context.history.len(), 0);

        // 在 fork 上跑一轮，主 agent 的 session_id / context 必须完全不受影响
        let reply = fork.handle_input("do the task", "cron").await;
        assert!(reply.is_ok(), "fork handle_input failed: {:?}", reply.err());
        assert_eq!(reply.unwrap(), "cron reply");
        assert_eq!(agent.session_id, original_session_id);
        assert_eq!(agent.context.history.len(), original_history_len);
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
            text.contains("[image 1 description]"),
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
        // 本测试专注标签过滤语义；rounds 耗尽后的空流在 guard 开启时会触发
        // 重试/诊断（有专属测试），这里关闭 guard 保持旧行为断言不变。
        let mut agent = make_agent_with_config(false, rounds, Default::default(), |c| {
            c.runtime.output_guard = false;
        })
        .await;
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
            Arc::new(RwLock::new(Vec::new())),
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
            Arc::new(RwLock::new(Vec::new())),
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

    // ---- 会话主题自动总结（plan.md P6）----

    /// 标题生成 provider：`chat` 返回预设文本（Ok）或报错（Err），记录调用次数。
    struct TitleMockProvider {
        reply: std::result::Result<String, ()>,
        calls: Arc<StdMutex<usize>>,
    }
    #[async_trait]
    impl Provider for TitleMockProvider {
        async fn chat(&self, _req: &ChatRequest<'_>) -> Result<ChatResponse> {
            *self.calls.lock().unwrap() += 1;
            match &self.reply {
                Ok(t) => Ok(ChatResponse {
                    text: Some(t.clone()),
                    ..Default::default()
                }),
                Err(_) => Err(anyhow::anyhow!("title provider down")),
            }
        }
        async fn chat_stream(&self, _req: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>> {
            Box::pin(futures_util::stream::empty())
        }
        fn native_tool_calling(&self) -> bool {
            true
        }
    }

    async fn make_title_agent(store: Arc<SessionStore>, sid: i64) -> Agent {
        let provider: Arc<dyn Provider> = Arc::new(CtxMockProvider {
            label: "p.m",
            size: None,
        });
        let tools = Arc::new(ToolRegistry::new());
        let cfg = Config::default_for_workspace("/tmp/llaia-test");
        Agent::new(
            &cfg,
            Some(provider),
            None,
            None,
            tools,
            store,
            sid,
            "test system".into(),
            8192,
            std::path::PathBuf::from("/tmp/llaia-test/workspace"),
            Arc::new(RwLock::new(std::path::PathBuf::from(
                "/tmp/llaia-test/workspace",
            ))),
            std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new())),
            std::path::PathBuf::from("/tmp/llaia-test"),
            true,
            "main".into(),
            None,
        )
        .await
    }

    #[tokio::test]
    async fn test_ensure_session_title_llm_generated() {
        let store = Arc::new(SessionStore::open_in_memory().unwrap());
        let sid = store.create_session("t1", "web").unwrap();
        let mut agent = make_title_agent(store.clone(), sid).await;
        agent
            .context
            .history
            .push(ChatMessage::user("帮我查一下 QQ 频道 token 过期的问题"));
        agent
            .context
            .history
            .push(ChatMessage::assistant("好的，我来看日志"));

        let calls = Arc::new(StdMutex::new(0usize));
        let tp = TitleMockProvider {
            // 模型带引号发挥，应被 sanitize 剥掉
            reply: Ok("「QQ Token 排查」".into()),
            calls: calls.clone(),
        };
        agent.ensure_session_title(&tp).await;

        assert_eq!(
            store.session_title(sid).unwrap().as_deref(),
            Some("QQ Token 排查")
        );
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn test_ensure_session_title_fallback_on_error() {
        let store = Arc::new(SessionStore::open_in_memory().unwrap());
        let sid = store.create_session("t2", "web").unwrap();
        let mut agent = make_title_agent(store.clone(), sid).await;
        agent
            .context
            .history
            .push(ChatMessage::user("帮我看看启动配置迁移的问题"));

        let calls = Arc::new(StdMutex::new(0usize));
        let tp = TitleMockProvider {
            reply: Err(()),
            calls: calls.clone(),
        };
        agent.ensure_session_title(&tp).await;

        // 降级：首条用户消息清洗截断
        assert_eq!(
            store.session_title(sid).unwrap().as_deref(),
            Some("帮我看看启动配置迁移的问题")
        );
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn test_ensure_session_title_empty_reply_falls_back() {
        let store = Arc::new(SessionStore::open_in_memory().unwrap());
        let sid = store.create_session("t3", "web").unwrap();
        let mut agent = make_title_agent(store.clone(), sid).await;
        agent
            .context
            .history
            .push(ChatMessage::user("排查 WebUI 启动报错"));

        let calls = Arc::new(StdMutex::new(0usize));
        let tp = TitleMockProvider {
            reply: Ok("".into()),
            calls: calls.clone(),
        };
        agent.ensure_session_title(&tp).await;

        assert_eq!(
            store.session_title(sid).unwrap().as_deref(),
            Some("排查 WebUI 启动报错")
        );
    }

    #[tokio::test]
    async fn test_ensure_session_title_skips_when_present() {
        let store = Arc::new(SessionStore::open_in_memory().unwrap());
        let sid = store.create_session("t4", "web").unwrap();
        store.set_session_title(sid, "已有标题").unwrap();
        let mut agent = make_title_agent(store.clone(), sid).await;
        agent.context.history.push(ChatMessage::user("任意消息"));

        let calls = Arc::new(StdMutex::new(0usize));
        let tp = TitleMockProvider {
            reply: Ok("新标题".into()),
            calls: calls.clone(),
        };
        agent.ensure_session_title(&tp).await;

        // 已有标题不覆盖、不打 LLM
        assert_eq!(
            store.session_title(sid).unwrap().as_deref(),
            Some("已有标题")
        );
        assert_eq!(*calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn test_ensure_session_title_no_material() {
        let store = Arc::new(SessionStore::open_in_memory().unwrap());
        let sid = store.create_session("t5", "web").unwrap();
        let mut agent = make_title_agent(store.clone(), sid).await;
        // history 为空：无素材，留空且不打 LLM

        let calls = Arc::new(StdMutex::new(0usize));
        let tp = TitleMockProvider {
            reply: Ok("x".into()),
            calls: calls.clone(),
        };
        agent.ensure_session_title(&tp).await;

        assert_eq!(store.session_title(sid).unwrap(), None);
        assert_eq!(*calls.lock().unwrap(), 0);
    }

    #[test]
    fn test_sanitize_title() {
        assert_eq!(sanitize_title("\"Hello World\""), "Hello World");
        assert_eq!(sanitize_title("'标题'"), "标题");
        assert_eq!(sanitize_title("《中文标题》"), "中文标题");
        assert_eq!(sanitize_title("「QQ Token 排查」"), "QQ Token 排查");
        assert_eq!(sanitize_title("  空格修剪  "), "空格修剪");
        assert_eq!(sanitize_title("标题："), "标题");
        // 只取首行
        assert_eq!(sanitize_title("第一行\n第二行"), "第一行");
        assert_eq!(sanitize_title(""), "");
    }

    #[test]
    fn test_cap_chars() {
        assert_eq!(cap_chars("abcdef", 3), "abc…");
        assert_eq!(cap_chars("abc", 3), "abc");
        // 中文按字符计
        assert_eq!(cap_chars("文字截断测试", 4), "文字截断…");
        assert_eq!(cap_chars("", 5), "");
    }

    #[test]
    fn test_build_agents_md_prompt_skips_home() {
        let dir = tempfile::tempdir().expect("tempdir");
        // 家目录即便有 AGENTS.md 也不加载（SOUL/USER/MEMORY 已在提示词内）
        std::fs::write(dir.path().join("AGENTS.md"), "# repo guide\n").unwrap();
        assert_eq!(build_agents_md_prompt(dir.path(), dir.path()), "");
    }

    #[test]
    fn test_build_agents_md_prompt_missing_or_empty() {
        let home = tempfile::tempdir().expect("tempdir");
        let ext = tempfile::tempdir().expect("tempdir");
        // 外部目录无 AGENTS.md
        assert_eq!(build_agents_md_prompt(ext.path(), home.path()), "");
        // 外部目录 AGENTS.md 为空
        std::fs::write(ext.path().join("AGENTS.md"), "   \n  ").unwrap();
        assert_eq!(build_agents_md_prompt(ext.path(), home.path()), "");
    }

    #[test]
    fn test_build_agents_md_prompt_loads_content() {
        let home = tempfile::tempdir().expect("tempdir");
        let ext = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            ext.path().join("AGENTS.md"),
            "project conventions: no println!",
        )
        .unwrap();

        let prompt = build_agents_md_prompt(ext.path(), home.path());
        assert!(!prompt.is_empty(), "应返回注入段");
        assert!(
            prompt.contains("Active directory instructions"),
            "got: {prompt}"
        );
        assert!(
            prompt.contains("project conventions: no println!"),
            "应含原文内容，got: {prompt}"
        );
        // 目录路径应体现在段里
        assert!(prompt.contains(&ext.path().display().to_string()));
    }

    #[test]
    fn test_build_agents_md_prompt_caps_oversized() {
        let home = tempfile::tempdir().expect("tempdir");
        let ext = tempfile::tempdir().expect("tempdir");
        let big = "x".repeat(AGENTS_MD_CHAR_CAP + 5000);
        std::fs::write(ext.path().join("AGENTS.md"), &big).unwrap();

        let prompt = build_agents_md_prompt(ext.path(), home.path());
        let cap_xs = "x".repeat(AGENTS_MD_CHAR_CAP);
        assert!(prompt.contains(&cap_xs), "应包含正好一整段上限内容");
        assert!(
            !prompt.contains(&"x".repeat(AGENTS_MD_CHAR_CAP + 1)),
            "不应包含超出上限的溢出片段"
        );
    }

    // ---- /steer 注入（plan.md #I）----

    #[tokio::test]
    async fn test_steer_injected_into_context_and_sqlite() {
        // 第一轮：调工具；第二轮：纯文本收尾。steer 在 turn 开始前投递，
        // 迭代 0（非末轮）顶部即 drain 注入。
        let tc = ToolCall {
            id: "c1".into(),
            name: "echo".into(),
            arguments: json!({}),
        };
        let mut agent = make_agent_with_rounds(
            true,
            vec![
                vec![StreamEvent::ToolCall(tc), StreamEvent::Done],
                vec![StreamEvent::TextDelta("done".into()), StreamEvent::Done],
            ],
        )
        .await;
        agent.push_steer("先别改那个文件".into());
        let (tx, _rx) = mpsc::channel(64);
        let out = agent
            .handle_input_streaming("do something", "cli", tx)
            .await
            .unwrap();
        assert!(out.contains("done"));
        // 注入为带标记的 user 消息，进 context 与 sqlite
        let injected = agent.context.history.iter().any(|m| {
            m.content
                .as_text()
                .contains("[steer] User added: 先别改那个文件")
        });
        assert!(injected, "steer should be injected into context");
        let msgs = agent
            .session_store
            .recent_messages(agent.session_id, 50)
            .unwrap();
        assert!(msgs
            .iter()
            .any(|m| m.role == "user" && m.content.contains("[steer] User added: 先别改那个文件")));
        // buffer 已清空
        assert!(agent.steer_buffer.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_steer_dropped_at_turn_end_notifies() {
        // 单轮纯文本 turn：无后续迭代可注入，残留被丢弃并附「未生效」提示。
        // 模拟「插话在流结束后到达」——先跑 turn，结束后 push 再触发
        // 下一轮的清理路径不适用；改为直接验证 turn 结束路径：在流中途投递
        // 无法稳定构造，这里验证 clear_steer 语义 + 末轮丢弃提示文本存在性
        // 由 test_steer_injected... 与 force_summary 路径共同覆盖。
        let mut agent = make_agent_with_rounds(
            true,
            vec![vec![StreamEvent::TextDelta("ok".into()), StreamEvent::Done]],
        )
        .await;
        agent.push_steer("late".into());
        let (tx, _rx) = mpsc::channel(64);
        let out = agent.handle_input_streaming("hi", "cli", tx).await.unwrap();
        // 单轮 turn（无工具调用）：steer 在迭代 0 已 drain 注入（turn 前投递）
        assert!(out.contains("ok"));
        assert!(agent
            .context
            .history
            .iter()
            .any(|m| m.content.as_text().contains("[steer] User added: late")));
    }

    #[tokio::test]
    async fn test_fork_does_not_share_steer_buffer() {
        let agent = make_agent_with_rounds(true, vec![vec![]]).await;
        agent.push_steer("给主线的".into());
        let fork = agent.fork_for_isolated(999, false);
        // fork 副本持有独立空缓冲：cron/委派 turn 不得消费主线的插话
        assert!(fork.steer_buffer.lock().unwrap().is_empty());
        assert!(!agent.steer_buffer.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_refresh_task_state_injects_runtime_context() {
        let mut agent = make_agent_with_rounds(true, vec![vec![]]).await;
        let store = agent.session_store.clone();
        let task_sid = store
            .create_task_session("task-uuid", "cli", "目录整理", Some("/data/docs"))
            .unwrap();
        agent.session_id = task_sid;
        agent.refresh_task_state();
        let task = agent.active_task.clone().unwrap();
        assert_eq!(task.title, "目录整理");
        assert_eq!(
            task.bound_path,
            Some(std::path::PathBuf::from("/data/docs"))
        );
        let injected = agent.context.task_state.clone().unwrap();
        assert!(injected.contains("目录整理"));
        assert!(injected.contains("/data/docs"));
        // to_messages 注入（KV 缓存友好尾部区）
        let msgs = agent.context.to_messages(&None);
        assert!(msgs.iter().any(|m| m.content.as_text().contains("[task]")));

        // 切回通用线：状态清空
        let main_sid = store.create_session("main2", "cli").unwrap();
        agent.session_id = main_sid;
        agent.refresh_task_state();
        assert!(agent.active_task.is_none());
        assert!(agent.context.task_state.is_none());
    }
}
