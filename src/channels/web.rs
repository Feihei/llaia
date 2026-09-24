use crate::agent::instances::{spawn_task_instance, InstanceHandle};
use crate::agent::sink::{run_turn, OutputSink};
use crate::agent::{Agent, AgentRegistry, MediaKind};
use crate::channels::Channel;
use crate::commands::slash::{try_handle, SlashOutcome};
use crate::config::{Config, WebUiConfig};
use crate::image_utils;
use crate::provider::{ChatMessage, ContentPart, ImageUrlContent};
use crate::tools::cron::CronTool;
use crate::web::{
    build_system_routes, check_token, generate_token, resolve_within, AppState, TokenQuery,
};
use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, Notify, RwLock};

/// WS 出向事件：扁平化 JSON，与 TurnEvent 一一对应 + 协议层事件
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebEvent {
    Chunk {
        delta: String,
    },
    /// 思考流增量（chat 界面折叠思考块，流式期间自动展开）
    Reasoning {
        delta: String,
    },
    ToolStart {
        id: String,
        name: String,
    },
    ToolResult {
        id: String,
        output: String,
    },
    Media {
        path: String,
        kind: MediaKind,
    },
    /// 待审批操作（聊天流内渲染为审批卡片；按钮等价于 /ok /deny 文本命令）
    Approval {
        id: String,
        tool_name: String,
        summary: String,
        within_workspace: bool,
    },
    /// ask_user 待回答问题（聊天流内渲染为问题卡片；选项按钮/自定义输入
    /// 等价于 /answer <id> <text> 文本命令）
    Question {
        id: String,
        question: String,
        choices: Option<Vec<String>>,
    },
    Done,
    Error {
        message: String,
    },
    Interrupted,
    // 协议层
    Pong,
    AuthOk,
    AuthFailed {
        reason: String,
    },
    Busy {
        reason: String,
    },
    /// 侧问/插话回执（/btw 答案、/steer 已排队提示）：独立样式消息块，
    /// 不参与 turn 事件流（不发 Done、不解除 busy）。
    Side {
        text: String,
    },
    /// 主动推送（cron 任务结果等，非 turn 事件）
    Proactive {
        message: String,
    },
    /// 实例面板路由封装（ADR-0032 T7）：把 turn 事件投给指定实例的聊天桶。
    /// `instance == "main"` 的事件**不封装**（保持既有协议形态，向后兼容）；
    /// 任务实例的 turn 事件全部包在这一层里，前端解包后按实例分桶渲染。
    Instance {
        instance: String,
        event: Box<WebEvent>,
    },
}

/// turn 结束信号：on_done/on_error/on_interrupted 三个终态都发送。
/// 携带实例名（ADR-0032 T7）：一条连接可同时跑多个实例的 turn，
/// WS 主循环据此从 per-instance turns 表里清掉对应条目。
#[derive(Debug, Clone)]
pub struct TurnEndSignal(pub String);

/// Web 输出 sink：把 OutputSink 回调转成 WebEvent 推到 mpsc，WS 写 task 消费。
/// 持有 turn-end sender 让 WS handler 主循环感知 turn 结束以清理 current_turn。
/// `instance` 非 main 时事件包一层 `WebEvent::Instance`（前端按实例分桶）。
pub struct WebSink {
    tx: mpsc::Sender<WebEvent>,
    turn_end_tx: mpsc::Sender<TurnEndSignal>,
    instance: String,
}

impl WebSink {
    pub fn new(tx: mpsc::Sender<WebEvent>, turn_end_tx: mpsc::Sender<TurnEndSignal>) -> Self {
        Self {
            tx,
            turn_end_tx,
            instance: "main".into(),
        }
    }

    /// 指定实例的 sink（ADR-0032 T7）：该实例的 turn 事件打实例标。
    pub fn for_instance(
        tx: mpsc::Sender<WebEvent>,
        turn_end_tx: mpsc::Sender<TurnEndSignal>,
        instance: &str,
    ) -> Self {
        Self {
            tx,
            turn_end_tx,
            instance: instance.to_string(),
        }
    }

    async fn send_event(&self, ev: WebEvent) {
        if self.instance == "main" {
            let _ = self.tx.send(ev).await;
        } else {
            let _ = self
                .tx
                .send(WebEvent::Instance {
                    instance: self.instance.clone(),
                    event: Box::new(ev),
                })
                .await;
        }
    }
}

#[async_trait]
impl OutputSink for WebSink {
    async fn on_chunk(&mut self, delta: &str) {
        self.send_event(WebEvent::Chunk {
            delta: delta.into(),
        })
        .await;
    }
    async fn on_reasoning(&mut self, delta: &str) {
        self.send_event(WebEvent::Reasoning {
            delta: delta.into(),
        })
        .await;
    }
    async fn on_tool_start(&mut self, name: &str) {
        self.send_event(WebEvent::ToolStart {
            id: String::new(),
            name: name.into(),
        })
        .await;
    }
    async fn on_tool_result(&mut self, output: &str) {
        self.send_event(WebEvent::ToolResult {
            id: String::new(),
            output: output.into(),
        })
        .await;
    }
    async fn on_media(&mut self, path: &str, kind: MediaKind) {
        self.send_event(WebEvent::Media {
            path: path.into(),
            kind,
        })
        .await;
    }
    async fn on_approval_request(&mut self, req: &crate::agent::sink::ApprovalRequest<'_>) {
        self.send_event(WebEvent::Approval {
            id: req.id.into(),
            tool_name: req.tool_name.into(),
            summary: req.summary.into(),
            within_workspace: req.within_workspace,
        })
        .await;
    }
    async fn on_question_asked(&mut self, req: &crate::agent::sink::QuestionRequest<'_>) {
        self.send_event(WebEvent::Question {
            id: req.id.into(),
            question: req.question.into(),
            choices: req.choices.map(|cs| cs.to_vec()),
        })
        .await;
    }
    async fn on_done(&mut self) {
        self.send_event(WebEvent::Done).await;
        let _ = self
            .turn_end_tx
            .send(TurnEndSignal(self.instance.clone()))
            .await;
    }
    async fn on_error(&mut self, message: &str) {
        self.send_event(WebEvent::Error {
            message: message.into(),
        })
        .await;
        let _ = self
            .turn_end_tx
            .send(TurnEndSignal(self.instance.clone()))
            .await;
    }
    async fn on_interrupted(&mut self) {
        // 与 QqSink 一致：只 log，不回推 WS 帧（前端按钮状态本身体现中断）
        tracing::info!("web turn interrupted");
        let _ = self
            .turn_end_tx
            .send(TurnEndSignal(self.instance.clone()))
            .await;
    }
}

/// GET /ws?token=... → WS upgrade
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(q): Query<TokenQuery>,
) -> Response {
    let provided = q.token.as_deref();
    let ok = match provided {
        Some(t) => check_token(t, state.token.as_str()),
        None => false,
    };
    if !ok {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    }
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

#[derive(Deserialize)]
pub struct ChatIn {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: Option<String>,
    pub images: Option<Vec<String>>,
    /// kind == "approval" 时的待审批 id
    pub id: Option<String>,
    /// kind == "approval" 时是批准(true)还是拒绝(false)
    pub approve: Option<bool>,
    /// 目标实例（ADR-0032 T7）：缺省 main；未知实例按需 spawn（幂等）
    pub instance: Option<String>,
}

async fn handle_ws(socket: WebSocket, state: AppState) {
    let (mut ws_sink, mut ws_stream) = socket.split();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<WebEvent>(64);
    let (end_tx, mut end_rx) = tokio::sync::mpsc::channel::<TurnEndSignal>(4);

    // 注册到 active_ws（用于 cron 主动推送广播）
    let ws_id = state
        .next_ws_id
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    state.active_ws.lock().await.insert(ws_id, tx.clone());

    // 发 auth_ok
    let _ = ws_sink
        .send(Message::Text(
            serde_json::to_string(&WebEvent::AuthOk).unwrap().into(),
        ))
        .await;

    // 写 task：rx → ws_sink
    let write_task = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let json = match serde_json::to_string(&ev) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if ws_sink.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
    });

    let agent = state.registry.main.clone();
    let workspace = {
        let a = agent.lock().await;
        a.workspace.clone()
    };
    // ADR-0032 T7：per-instance 并行 turn 表（实例名 → JoinHandle + stop 通知）。
    // main 与任务实例同等对待：同一条连接里多个实例的 turn 可同时跑，
    // 事件经 WebSink::for_instance 打实例标，前端按实例分桶渲染。
    let mut turns: HashMap<String, (tokio::task::JoinHandle<()>, Arc<Notify>)> = HashMap::new();

    loop {
        tokio::select! {
            // WS 入向消息
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(s))) => {
                        let chat: Option<ChatIn> = serde_json::from_str(&s).ok();
                        match chat.as_ref().map(|c| c.kind.as_str()) {
                            Some("ping") => { let _ = tx.send(WebEvent::Pong).await; }
                            Some("open") => {
                                // 实例 rail 点 dormant 线的唤醒帧（ADR-0032 T7）：
                                // 只解析/按需 spawn，不开 turn；幂等，重复点无害。
                                let chat: ChatIn = match serde_json::from_str(&s) {
                                    Ok(c) => c,
                                    Err(_) => continue,
                                };
                                let inst_name = chat.instance.clone().unwrap_or_else(|| "main".into());
                                let reply = match resolve_instance(&state, &agent, &inst_name).await {
                                    Ok(_) => format!("[instance \"{}\" ready]", inst_name),
                                    Err(e) => format!("[error: {}]", e),
                                };
                                route_event(&tx, &inst_name, WebEvent::Side { text: reply }).await;
                            }
                            Some("stop") => {
                                let target = chat
                                    .as_ref()
                                    .and_then(|c| c.instance.as_deref())
                                    .unwrap_or("main")
                                    .to_string();
                                if let Some((_, stop_n)) = turns.get(&target) {
                                    stop_n.notify_one();
                                }
                            }
                            Some("chat") | Some("approval") => {
                                let chat: ChatIn = match serde_json::from_str(&s) {
                                    Ok(c) => c,
                                    Err(e) => {
                                        tracing::warn!(error = %e, "web chat frame malformed");
                                        continue;
                                    }
                                };
                                // T7：帧目标实例（缺省 main，旧前端不带该字段仍照常工作）。
                                // 未知实例按需 spawn（幂等）：实例 rail 点击即开面板即跑。
                                let inst_name = chat
                                    .instance
                                    .clone()
                                    .unwrap_or_else(|| "main".into());
                                let inst = match resolve_instance(&state, &agent, &inst_name).await {
                                    Ok(i) => i,
                                    Err(e) => {
                                        route_event(&tx, &inst_name, WebEvent::Side {
                                            text: format!("[error: {}]", e),
                                        })
                                        .await;
                                        continue;
                                    }
                                };
                                // 审批卡片按钮不新增解析逻辑：翻译成用户本来就会敲的
                                // `/ok <id>` / `/deny <id>`，完整复用 slash → Resume 续跑
                                // 通路（含"已解析/无此 pending"的既有提示）。审批注册在
                                // 哪个实例的 gate 上，按钮就带哪个实例名回哪个实例。
                                let mut text = match chat.kind.as_str() {
                                    "approval" => match (chat.id.as_deref(), chat.approve) {
                                        (Some(id), Some(approve)) if !id.is_empty() => {
                                            approval_frame_command(id, approve)
                                        }
                                        _ => {
                                            tracing::warn!(
                                                id = ?chat.id,
                                                approve = ?chat.approve,
                                                "web approval frame missing id/approve"
                                            );
                                            continue;
                                        }
                                    },
                                    _ => chat.text.clone().unwrap_or_default(),
                                };

                                // /session 家族在 web 由 instance rail 取代（ADR-0032 T7）：
                                // 切线 = 点面板，避免文本命令与 per-instance 路由语义打架。
                                // CLI 频道仍走 try_session_command 的完整切线通路。
                                {
                                    let lower = text.trim().to_ascii_lowercase();
                                    let is_session_cmd = lower == "/session"
                                        || lower == "/sessions"
                                        || lower == "/task"
                                        || lower == "/tasks"
                                        || lower.starts_with("/session ")
                                        || lower.starts_with("/task ");
                                    if is_session_cmd {
                                        route_event(&tx, &inst_name, WebEvent::Side {
                                            text: "[switch or create task instances from the rail on the left; /session is a CLI command]".into(),
                                        }).await;
                                        continue;
                                    }
                                }

                                // /steer（plan.md #I）：投到目标实例自己的缓冲——
                                // InstanceHandle.steer_buffer 与实例 agent 共享同一 Arc，
                                // turn 持锁期间也能投递（不经 Agent 锁）。
                                if let Some(rest) = crate::commands::slash::split_steer(&text) {
                                    if turns.contains_key(&inst_name) {
                                        if rest.is_empty() {
                                            route_event(&tx, &inst_name, WebEvent::Side { text: "usage: /steer <message>".into() }).await;
                                        } else {
                                            inst.steer_buffer.lock().unwrap().push_back(rest.to_string());
                                            route_event(&tx, &inst_name, WebEvent::Side {
                                                text: format!("[steer queued: {}]", rest),
                                            }).await;
                                        }
                                        // 不发 Done：turn 仍在运行，前端 busy 状态保持
                                        continue;
                                    } else if rest.is_empty() {
                                        route_event(&tx, &inst_name, WebEvent::Side { text: "usage: /steer <message>".into() }).await;
                                        route_event(&tx, &inst_name, WebEvent::Done).await;
                                        continue;
                                    } else {
                                        // 空闲降级：剥前缀当普通消息跑（对齐 OpenClaw）
                                        text = rest.to_string();
                                    }
                                }

                                if turns.contains_key(&inst_name) {
                                    route_event(&tx, &inst_name, WebEvent::Busy { reason: "another turn running".into() }).await;
                                } else {
                                    tracing::info!(instance = %inst_name, text = %text, images = chat.images.as_ref().map(|v| v.len()).unwrap_or(0), "web received message");

                                    // /btw（plan.md #H）：侧问走目标实例的 agent（独立单轮
                                    // 调用，答案以 Side 事件独立样式呈现，不进 turn 流、
                                    // 不污染该实例上下文）。
                                    if text.starts_with('/') && text.trim().to_ascii_lowercase().starts_with("/btw") {
                                        let q = text
                                            .trim()
                                            .split_once(char::is_whitespace)
                                            .map(|x| x.1)
                                            .unwrap_or("")
                                            .trim()
                                            .to_string();
                                        let answer = if q.is_empty() {
                                            Err(anyhow::anyhow!("usage: /btw <question>"))
                                        } else {
                                            let mut a = inst.agent.lock().await;
                                            crate::commands::slash::run_btw(&mut a, &q).await
                                        };
                                        let out = match answer {
                                            Ok(ans) => ans,
                                            Err(e) => format!("[btw failed: {}]", e),
                                        };
                                        route_event(&tx, &inst_name, WebEvent::Side { text: out }).await;
                                        route_event(&tx, &inst_name, WebEvent::Done).await;
                                        continue;
                                    }

                                    // 斜杠命令拦截（P4-d）：在目标实例的 agent 上执行，
                                    // 与其它频道一致地走审批/续跑流。web 用 type:"stop"
                                    // 中断（见上面 "stop" 分支），不认 /stop，故排除之。
                                    let slash_outcome = if text.starts_with('/') && !text.trim().eq_ignore_ascii_case("/stop") {
                                        let mut a = inst.agent.lock().await;
                                        Some(try_handle(&text, &mut a, Some(state.registry.clone())).await)
                                    } else {
                                        None
                                    };
                                    match slash_outcome {
                                        Some(Ok(SlashOutcome::Handled(msg))) => {
                                            route_event(&tx, &inst_name, WebEvent::Chunk { delta: msg }).await;
                                            route_event(&tx, &inst_name, WebEvent::Done).await;
                                        }
                                        Some(Ok(SlashOutcome::Resume { notice, message })) => {
                                            // 先回显结果摘要，再跑 continuation turn 让模型基于工具结果继续
                                            route_event(&tx, &inst_name, WebEvent::Chunk { delta: notice }).await;
                                            let sink = Box::new(WebSink::for_instance(tx.clone(), end_tx.clone(), &inst_name));
                                            let stop_n = Arc::new(Notify::new());
                                            let stop_clone = stop_n.clone();
                                            let agent_clone = inst.agent.clone();
                                            let name = inst_name.clone();
                                            let h = tokio::spawn(async move {
                                                let _ = run_turn(agent_clone, ChatMessage::user(&message), "web".into(), sink, stop_clone).await;
                                            });
                                            turns.insert(name, (h, stop_n));
                                        }
                                        Some(Ok(SlashOutcome::Exit)) => {
                                            // web 常驻连接，/exit 无意义，忽略
                                        }
                                        Some(Ok(SlashOutcome::NotSlash)) | None => {
                                            // 普通消息：构造并跑一轮 agent turn。上传图固定
                                            // 解析到 main 家目录的 uploads/（/upload 只落那里，
                                            // 与实例的 workspace_root 无关）。
                                            let user_msg = build_user_message(&text, chat.images.as_deref(), &workspace);
                                            let sink = Box::new(WebSink::for_instance(tx.clone(), end_tx.clone(), &inst_name));
                                            let stop_n = Arc::new(Notify::new());
                                            let stop_clone = stop_n.clone();
                                            let agent_clone = inst.agent.clone();
                                            let name = inst_name.clone();
                                            let h = tokio::spawn(async move {
                                                let _ = run_turn(agent_clone, user_msg, "web".into(), sink, stop_clone).await;
                                            });
                                            turns.insert(name, (h, stop_n));
                                        }
                                        Some(Err(e)) => {
                                            route_event(&tx, &inst_name, WebEvent::Error { message: e.to_string() }).await;
                                            route_event(&tx, &inst_name, WebEvent::Done).await;
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
            // turn 结束信号：按实例名清理并行 turn 表
            sig = end_rx.recv() => {
                if let Some(TurnEndSignal(name)) = sig {
                    if let Some((h, _)) = turns.remove(&name) {
                        let _ = h.await;
                    }
                }
            }
        }
    }

    // 清理：停掉所有还在跑的实例 turn
    for (_, (h, stop_n)) in turns {
        stop_n.notify_one();
        let _ = h.await;
    }
    // 从 active_ws 注销（避免向已关闭连接广播）
    state.active_ws.lock().await.remove(&ws_id);
    write_task.abort();
}

/// 审批卡片按钮帧 → 等价的斜杠命令文本（`/ok <id>` / `/deny <id>`）。
///
/// 抽成纯函数是为了可测：这段映射是「卡片按钮」与「手敲 /ok」同源同路的唯一保证，
/// 一旦有人在这里加了新分支，测试会立刻发现两条通路已经分叉。
fn approval_frame_command(id: &str, approve: bool) -> String {
    format!("/{} {}", if approve { "ok" } else { "deny" }, id)
}

/// 手写事件的路由规则：main 直发（向后兼容协议形态），任务实例包一层
/// `WebEvent::Instance`。与 `WebSink::send_event` 必须保持同一规则——
/// WS 循环里直接构造的 Side/Busy/Done 与 sink 产生的事件要落进同一个桶。
async fn route_event(tx: &mpsc::Sender<WebEvent>, instance: &str, ev: WebEvent) {
    if instance == "main" {
        let _ = tx.send(ev).await;
    } else {
        let _ = tx
            .send(WebEvent::Instance {
                instance: instance.to_string(),
                event: Box::new(ev),
            })
            .await;
    }
}

/// 解析帧目标实例：已注册直接复用句柄；未注册且非 main 则按需 spawn
/// （幂等，同名重复请求拿到同一句柄）。main 缺失属宿主装配错误，报错不造。
async fn resolve_instance(
    state: &AppState,
    main: &Arc<Mutex<Agent>>,
    name: &str,
) -> anyhow::Result<Arc<InstanceHandle>> {
    if let Some(h) = state.registry.instances.get(name).await {
        return Ok(h);
    }
    if name == "main" {
        anyhow::bail!("main instance is not registered (host setup error)");
    }
    if !valid_instance_name(name) {
        anyhow::bail!("invalid instance name: 1-64 chars, no path separators or leading dots");
    }
    let a = main.lock().await;
    let (h, _) = spawn_task_instance(&a, &state.registry.instances, name).await?;
    Ok(h)
}

/// web 入向实例名门禁：实例名会拼进 `instances/<name>/` 目录路径（实例私有
/// MEMORY），不能让它当路径片段用。CLI /session 命令的同类校验是历史欠账，
/// 这里先把新开的 web 入口守住。
fn valid_instance_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && !name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
}

/// 归一化上传图的路径形态为 `uploads/...`：`/upload` 回传的是带前缀的相对路径，
/// 手填或旧前端可能只给裸文件名，两种都要能落到 uploads 目录里。反斜杠统一为
/// 正斜杠、剥掉开头斜杠；刻意不动中间的 `..`——那由 resolve_within 的门禁拒绝。
fn upload_rel(raw: &str) -> String {
    let unified = raw.replace('\\', "/");
    let bare = match unified.strip_prefix("uploads/") {
        Some(rest) => rest,
        None => unified.trim_start_matches('/'),
    };
    format!("uploads/{}", bare)
}

fn build_user_message(text: &str, images: Option<&[String]>, workspace: &Path) -> ChatMessage {
    let imgs = images.unwrap_or(&[]);
    if imgs.is_empty() {
        return ChatMessage::user(text);
    }
    let mut parts: Vec<ContentPart> = Vec::new();
    if !text.is_empty() {
        parts.push(ContentPart::Text { text: text.into() });
    }
    // base 用 workspace 本身（与 CLI 频道同一口径）：/upload 回传的相对路径**自带**
    // uploads/ 前缀，若把 base 也设成 uploads/ 会拼出 uploads/uploads/... 这个永远
    // 不存在的路径，图片于是全数退化成一段 invalid path 文本——模型根本看不到图。
    // 三处降级都只往消息里塞一段占位文本，历史上没有任何日志痕迹——用户只能从
    // agent 的怪行为（改用 PIL 猜像素）反推出图丢了。统一收口成一条 WARN。
    let mut degraded: Vec<String> = Vec::new();
    for img_rel in imgs {
        match resolve_within(workspace, &upload_rel(img_rel)) {
            Ok(abs) => {
                if !image_utils::is_image_file(&abs) {
                    let why = format!("[not an image: {}]", img_rel);
                    degraded.push(why.clone());
                    parts.push(ContentPart::Text { text: why });
                    continue;
                }
                match image_utils::prepare_image_for_vision(&abs) {
                    Ok(data_url) => {
                        parts.push(ContentPart::ImageUrl {
                            image_url: ImageUrlContent { url: data_url },
                        });
                    }
                    Err(e) => {
                        let why = format!("[image load failed: {}]", e);
                        degraded.push(why.clone());
                        parts.push(ContentPart::Text { text: why });
                    }
                }
            }
            Err(e) => {
                let why = format!("[invalid path: {}]", e);
                degraded.push(why.clone());
                parts.push(ContentPart::Text { text: why });
            }
        }
    }
    if !degraded.is_empty() {
        tracing::warn!(
            requested = ?imgs,
            degraded = ?degraded,
            "web image did not reach the model; message sent without it"
        );
    }
    if parts.is_empty() {
        ChatMessage::user(text)
    } else {
        ChatMessage::user_multimodal(parts)
    }
}

pub struct WebChannel {
    pub config: WebUiConfig,
    pub registry: Arc<AgentRegistry>,
    pub config_full: Arc<RwLock<Config>>,
    pub config_path: PathBuf,
    pub workspace: PathBuf,
    /// active WS 连接注册表：id → event sender，用于主动推送（cron 任务结果等）
    pub active_ws: Arc<tokio::sync::Mutex<std::collections::HashMap<u64, mpsc::Sender<WebEvent>>>>,
    /// cron.toml 路径（供 raw 编辑接口读写）
    pub cron_path: PathBuf,
    /// CronHandle 共享槽（serve_cmd 启动 cron 后通过 set_cron_scheduler 注入；
    /// build_router 时读取快照填 AppState）。启动失败保持 None，cron API 返回 503。
    pub cron_scheduler: Arc<std::sync::Mutex<Option<Arc<crate::cron::CronHandle>>>>,
    /// McpRegistry 共享槽（serve_cmd 通过 set_mcp_registry 注入，供 MCP API 展示状态）
    pub mcp_registry: Arc<std::sync::Mutex<Option<Arc<crate::mcp::client::McpRegistry>>>>,
    /// 优雅停止信号：serve_cmd 创建并持有，注入 AppState 后由 /api/shutdown handler 触发（ADR-0018）
    pub shutdown_signal: Arc<Notify>,
    /// CronTool 实例（serve 构建时注入），热加载 cron 时用它重新指向新调度器。
    /// 与 AppState 同款 Arc<Mutex<Option>> 槽位。
    pub cron_tool: Arc<std::sync::Mutex<Option<Arc<CronTool>>>>,
    /// 微信登录进度共享槽（serve_cmd 先于频道创建，WechatChannel 写、卡片端点读）
    pub wechat_login: Arc<tokio::sync::RwLock<crate::channels::wechat::WechatLoginView>>,
}

impl WebChannel {
    pub fn new(
        web_config: WebUiConfig,
        registry: Arc<AgentRegistry>,
        config_full: Arc<RwLock<Config>>,
        config_path: PathBuf,
        workspace: PathBuf,
        shutdown_signal: Arc<Notify>,
        wechat_login: Arc<tokio::sync::RwLock<crate::channels::wechat::WechatLoginView>>,
    ) -> Self {
        let cron_path = config_path.with_file_name("cron.toml");
        Self {
            config: web_config,
            registry,
            config_full,
            config_path,
            workspace,
            active_ws: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            cron_path,
            cron_scheduler: Arc::new(std::sync::Mutex::new(None)),
            mcp_registry: Arc::new(std::sync::Mutex::new(None)),
            shutdown_signal,
            cron_tool: Arc::new(std::sync::Mutex::new(None)),
            wechat_login,
        }
    }

    /// 注入 CronHandle（serve_cmd 在 CronHandle::start 成功后调用，spawn 前）
    pub fn set_cron_scheduler(&self, s: Arc<crate::cron::CronHandle>) {
        *self.cron_scheduler.lock().unwrap() = Some(s);
    }

    /// 注入 McpRegistry（serve_cmd 在 build_agent 后调用，spawn 前）
    pub fn set_mcp_registry(&self, r: Arc<crate::mcp::client::McpRegistry>) {
        *self.mcp_registry.lock().unwrap() = Some(r);
    }

    /// 注入 CronTool（serve_cmd 在 build_agent 后调用，spawn 前）
    pub fn set_cron_tool(&self, t: Option<Arc<CronTool>>) {
        *self.cron_tool.lock().unwrap() = t;
    }

    pub fn build_router(&self) -> axum::Router {
        // token：配置非空用配置，留空随机生成
        let token = if self.config.token.is_empty() {
            let t = generate_token();
            tracing::info!("WebUI token (randomly generated): {}", t);
            t
        } else {
            self.config.token.clone()
        };
        // 读取 cron_scheduler / mcp_registry / cron_tool 的共享槽（Arc 克隆，零拷贝）
        let state = AppState {
            registry: self.registry.clone(),
            config: self.config_full.clone(),
            config_path: self.config_path.clone(),
            workspace: self.workspace.clone(),
            token: Arc::new(token),
            shutdown_signal: self.shutdown_signal.clone(),
            active_ws: self.active_ws.clone(),
            next_ws_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            cron_path: self.cron_path.clone(),
            cron_scheduler: self.cron_scheduler.clone(),
            mcp_path: self.cron_path.with_file_name("mcp.toml"),
            mcp_registry: self.mcp_registry.clone(),
            skills_dir: self.cron_path.with_file_name("skills"),
            cron_tool: self.cron_tool.clone(),
            wechat_login: self.wechat_login.clone(),
        };
        // 系统级路由 + WS 路由，共享同一个 state
        build_system_routes()
            .route("/ws", axum::routing::get(ws_handler))
            .with_state(state)
    }

    /// 主动推送：向所有 active WS 连接广播 Proactive 事件。
    /// 断开（closed）的连接在推送时顺带清理。
    pub async fn send_proactive(&self, message: &str) {
        let mut to_remove = Vec::new();
        {
            let mut ws = self.active_ws.lock().await;
            for (id, sender) in ws.iter() {
                if sender.is_closed() {
                    to_remove.push(*id);
                    continue;
                }
                let _ = sender.try_send(WebEvent::Proactive {
                    message: message.to_string(),
                });
            }
            for id in to_remove {
                ws.remove(&id);
            }
        }
    }
}

#[async_trait]
impl crate::cron::ProactivePusher for WebChannel {
    async fn push(&self, message: &str) -> anyhow::Result<()> {
        self.send_proactive(message).await;
        Ok(())
    }
}

#[async_trait]
impl Channel for WebChannel {
    fn pusher(self: Arc<Self>) -> Option<Arc<dyn crate::cron::ProactivePusher>> {
        Some(self as Arc<dyn crate::cron::ProactivePusher>)
    }
    async fn run(self: Arc<Self>, _registry: Arc<AgentRegistry>) -> Result<(), anyhow::Error> {
        // 注入本轮结果投递目标（web 经 pusher 主动推送回 WS 会话；serve 单 channel 下持久生效）
        self.registry.set_delivery(
            self.clone()
                .pusher()
                .map(crate::tools::delegate::DeliveryTarget::Pusher),
        );
        let addr: std::net::SocketAddr = format!("{}:{}", self.config.host, self.config.port)
            .parse()
            .map_err(|e| {
                anyhow::anyhow!(
                    "invalid bind addr (host={}, port={}): {}",
                    self.config.host,
                    self.config.port,
                    e
                )
            })?;
        let router = self.build_router();
        // bind 重试：自重启场景下旧进程端口可能尚未完全释放，
        // 或 systemd/docker restart 策略与新进程并发拉起，重试兜底竞态。
        let mut listener = None;
        let mut last_err = anyhow::anyhow!("bind attempts exhausted");
        for attempt in 1..=10 {
            match tokio::net::TcpListener::bind(addr).await {
                Ok(l) => {
                    listener = Some(l);
                    break;
                }
                Err(e) => {
                    if attempt == 1 {
                        tracing::warn!("bind {} failed ({}), retrying up to 10s", addr, e);
                    }
                    last_err = anyhow::anyhow!("{}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
        let listener = listener.ok_or_else(|| anyhow::anyhow!("bind {}: {}", addr, last_err))?;
        tracing::info!("WebChannel listening on {}", addr);
        axum::serve(listener, router)
            .await
            .map_err(|e| anyhow::anyhow!("web server: {}", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_web_event_chunk_serialization() {
        let ev = WebEvent::Chunk {
            delta: "hello".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(json, r#"{"type":"chunk","delta":"hello"}"#);
    }

    #[test]
    fn test_web_event_done_serialization() {
        let ev = WebEvent::Done;
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(json, r#"{"type":"done"}"#);
    }

    #[test]
    fn test_web_event_media_serialization() {
        let ev = WebEvent::Media {
            path: "out/a.png".into(),
            kind: MediaKind::Image,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains(r#""type":"media""#));
        assert!(json.contains(r#""path":"out/a.png""#));
    }

    #[test]
    fn test_web_event_auth_failed_serialization() {
        let ev = WebEvent::AuthFailed {
            reason: "invalid token".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(json, r#"{"type":"auth_failed","reason":"invalid token"}"#);
    }

    use crate::agent::sink::OutputSink;

    #[tokio::test]
    async fn test_web_sink_chunk_to_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<WebEvent>(8);
        let (end_tx, _end_rx) = tokio::sync::mpsc::channel::<TurnEndSignal>(8);
        let mut sink = WebSink::new(tx, end_tx);
        sink.on_chunk("hi").await;
        let ev = rx.recv().await.unwrap();
        match ev {
            WebEvent::Chunk { delta } => assert_eq!(delta, "hi"),
            _ => panic!("expected Chunk"),
        }
    }

    #[tokio::test]
    async fn test_web_sink_terminal_events_send_turn_end() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<WebEvent>(8);
        let (end_tx, mut end_rx) = tokio::sync::mpsc::channel::<TurnEndSignal>(8);
        let mut sink = WebSink::new(tx, end_tx);
        sink.on_done().await;
        assert!(end_rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn test_web_sink_send_failure_ignored() {
        // drop receiver 后 send 应返回 Err，但 sink 不 panic
        let (tx, rx) = tokio::sync::mpsc::channel::<WebEvent>(8);
        let (end_tx, _end_rx) = tokio::sync::mpsc::channel::<TurnEndSignal>(8);
        let mut sink = WebSink::new(tx, end_tx);
        drop(rx);
        // 不应 panic
        sink.on_chunk("hi").await;
        sink.on_done().await;
    }

    /// 在临时目录里造一个 uploads/ 下的真图（能被 image::open 解码即可）
    fn make_uploads_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let uploads = dir.path().join("uploads");
        std::fs::create_dir_all(&uploads).unwrap();
        image::DynamicImage::new_rgb8(4, 4)
            .save(uploads.join("shot.png"))
            .unwrap();
        dir
    }

    fn parts_of(msg: &ChatMessage) -> &Vec<ContentPart> {
        match &msg.content {
            crate::provider::MessageContent::Multimodal(p) => p,
            other => panic!("期望多模态消息，实际 {:?}", other),
        }
    }

    /// 回归：WebUI 传图曾因 base 又拼了一次 uploads/ 前缀而**从未**成功过——
    /// 图片静默退化成一段 invalid path 文本，模型看不到图，只能改用 PIL 猜像素。
    #[test]
    fn test_build_user_message_attaches_prefixed_upload() {
        let dir = make_uploads_dir();
        // 前端原样提交 /upload 回传的形态（自带 uploads/ 前缀）
        let msg = build_user_message("看图", Some(&["uploads/shot.png".to_string()]), dir.path());
        let parts = parts_of(&msg);
        assert!(
            parts
                .iter()
                .any(|p| matches!(p, ContentPart::ImageUrl { image_url } if image_url.url.starts_with("data:image/"))),
            "图片未成为 ImageUrl，parts = {:?}",
            parts
        );
        assert!(
            !parts.iter().any(|p| matches!(p, ContentPart::Text { text }
                if text.contains("invalid path") || text.contains("load failed")),),
            "不该出现路径/读取失败的占位文本，parts = {:?}",
            parts
        );
    }

    /// 裸文件名（旧前端或手填）同样应落到 uploads/ 下。
    #[test]
    fn test_build_user_message_accepts_bare_filename() {
        let dir = make_uploads_dir();
        let msg = build_user_message("", Some(&["shot.png".to_string()]), dir.path());
        assert!(
            parts_of(&msg)
                .iter()
                .any(|p| matches!(p, ContentPart::ImageUrl { .. })),
            "裸文件名也应解析到 uploads/ 下"
        );
    }

    /// 不存在的图应报「读取失败」而不是被当成多模态成功；路径侧的语义由
    /// web::tests::test_resolve_within_missing_path_is_not_reported_as_traversal 锁定。
    #[test]
    fn test_build_user_message_missing_upload_degrades_to_text() {
        let dir = make_uploads_dir();
        let msg = build_user_message("看图", Some(&["uploads/gone.png".to_string()]), dir.path());
        assert!(
            parts_of(&msg)
                .iter()
                .any(|p| matches!(p, ContentPart::Text { text }
                if text.contains("image load failed"))),
            "缺失文件应降级为 load failed 占位"
        );
    }

    #[test]
    fn test_upload_rel_normalizes_without_swallowing_traversal() {
        assert_eq!(upload_rel("uploads/a.png"), "uploads/a.png");
        assert_eq!(upload_rel("a.png"), "uploads/a.png");
        assert_eq!(upload_rel("/a.png"), "uploads/a.png");
        assert_eq!(upload_rel("uploads\\a.png"), "uploads/a.png");
        // 中间的 ParentDir 原样透传，交给 resolve_within 的门禁拒绝
        assert_eq!(upload_rel("uploads/../secret"), "uploads/../secret");
    }

    // ---- P7 审批卡片 ----

    /// 按钮帧必须精确翻译成用户本来就会敲的那两条命令：卡片不是第二条通路，
    /// 只是 /ok /deny 的另一个入口。
    #[test]
    fn test_approval_frame_maps_to_slash_command() {
        assert_eq!(approval_frame_command("ap7", true), "/ok ap7");
        assert_eq!(approval_frame_command("ap7", false), "/deny ap7");
        assert_eq!(approval_frame_command("q3", true), "/ok q3");
    }

    /// 前端帧形状：{type:"approval", id, approve}（text/images 缺省）。
    #[test]
    fn test_approval_frame_deserializes() {
        let c: ChatIn = serde_json::from_str(r#"{"type":"approval","id":"ap2","approve":true}"#)
            .expect("approval frame should parse");
        assert_eq!(c.kind, "approval");
        assert_eq!(c.id.as_deref(), Some("ap2"));
        assert_eq!(c.approve, Some(true));
        assert!(c.text.is_none());
        // 裸 /ok 文本帧不受影响
        let c: ChatIn = serde_json::from_str(r#"{"type":"chat","text":"/ok"}"#).unwrap();
        assert!(c.id.is_none() && c.approve.is_none());
    }

    #[test]
    fn test_web_event_approval_serialization() {
        let ev = WebEvent::Approval {
            id: "ap1".into(),
            tool_name: "terminal".into(),
            summary: "rm -rf build".into(),
            within_workspace: false,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains(r#""type":"approval""#), "{json}");
        assert!(json.contains(r#""id":"ap1""#), "{json}");
        assert!(json.contains(r#""tool_name":"terminal""#), "{json}");
        assert!(json.contains(r#""within_workspace":false"#), "{json}");
    }

    /// sink 回调携带的字段必须原样落到 WS 事件上（卡片渲染靠它，不再回头查门控）。
    #[tokio::test]
    async fn test_web_sink_approval_request_to_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<WebEvent>(8);
        let (end_tx, _end_rx) = tokio::sync::mpsc::channel::<TurnEndSignal>(8);
        let mut sink = WebSink::new(tx, end_tx);
        sink.on_approval_request(&crate::agent::sink::ApprovalRequest {
            id: "ap9",
            tool_name: "file_write",
            summary: "/etc/hosts",
            within_workspace: true,
        })
        .await;
        match rx.recv().await.unwrap() {
            WebEvent::Approval {
                id,
                tool_name,
                summary,
                within_workspace,
            } => {
                assert_eq!(id, "ap9");
                assert_eq!(tool_name, "file_write");
                assert_eq!(summary, "/etc/hosts");
                assert!(within_workspace);
            }
            other => panic!("expected Approval, got {other:?}"),
        }
    }

    // ---- ADR-0032 T7：实例路由协议 ----

    /// 路由规则唯一性：手写事件（route_event）与 sink 事件（WebSink::send_event）
    /// 必须同形——main 直发，任务实例包一层 Instance。
    #[tokio::test]
    async fn test_route_event_wraps_non_main() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<WebEvent>(8);
        route_event(&tx, "main", WebEvent::Done).await;
        route_event(&tx, "foo", WebEvent::Done).await;
        let first = rx.recv().await.unwrap();
        assert!(
            matches!(first, WebEvent::Done),
            "main 事件必须直发，got {first:?}"
        );
        match rx.recv().await.unwrap() {
            WebEvent::Instance { instance, event } => {
                assert_eq!(instance, "foo");
                assert!(matches!(*event, WebEvent::Done));
            }
            other => panic!("task 实例事件必须包 Instance，got {other:?}"),
        }
    }

    /// Instance 封装的序列化形状：前端按 `type === 'instance'` 解包分桶，
    /// 内层 event 保序透传。
    #[test]
    fn test_web_event_instance_serialization() {
        let ev = WebEvent::Instance {
            instance: "foo".into(),
            event: Box::new(WebEvent::Chunk { delta: "hi".into() }),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains(r#""type":"instance""#), "{json}");
        assert!(json.contains(r#""instance":"foo""#), "{json}");
        assert!(json.contains(r#""type":"chunk""#), "{json}");
    }

    /// for_instance 的 sink 事件打实例标；TurnEndSignal 必须携带同名实例——
    /// WS 主循环按它清理 per-instance turns 表，带错名字会泄漏句柄或误清别人。
    #[tokio::test]
    async fn test_web_sink_for_instance_tags_events_and_signal() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<WebEvent>(8);
        let (end_tx, mut end_rx) = tokio::sync::mpsc::channel::<TurnEndSignal>(8);
        let mut sink = WebSink::for_instance(tx, end_tx, "research");
        sink.on_chunk("hi").await;
        sink.on_done().await;
        match rx.recv().await.unwrap() {
            WebEvent::Instance { instance, event } => {
                assert_eq!(instance, "research");
                assert!(matches!(*event, WebEvent::Chunk { .. }));
            }
            other => panic!("expected wrapped Chunk, got {other:?}"),
        }
        let sig = end_rx.recv().await.unwrap();
        assert_eq!(sig.0, "research");
    }

    /// 实例名门禁：实例名拼进 `instances/<name>/` 目录路径，路径片段一律拒绝；
    /// main 走专用分支不在此判（resolve_instance 先查 main）。
    #[test]
    fn test_valid_instance_name_rejects_path_shapes() {
        assert!(valid_instance_name("research"));
        assert!(valid_instance_name("refactor-2"));
        assert!(!valid_instance_name(""));
        assert!(!valid_instance_name(".."));
        assert!(!valid_instance_name(".hidden"));
        assert!(!valid_instance_name("a/b"));
        assert!(!valid_instance_name("a\\b"));
        assert!(!valid_instance_name("a:b"));
        assert!(!valid_instance_name(&"x".repeat(65)));
    }

    /// 前端帧形状：open 帧（rail 点 dormant 线唤醒）带 instance、无 text。
    #[test]
    fn test_open_frame_deserializes() {
        let c: ChatIn = serde_json::from_str(r#"{"type":"open","instance":"research"}"#)
            .expect("open frame should parse");
        assert_eq!(c.instance.as_deref(), Some("research"));
        assert!(c.text.is_none());
        // 缺省 instance 的旧帧仍解析（向后兼容）
        let c: ChatIn = serde_json::from_str(r#"{"type":"chat","text":"hi"}"#).unwrap();
        assert_eq!(c.instance, None);
    }
}
