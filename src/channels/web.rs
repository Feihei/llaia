use crate::agent::sink::{run_turn, OutputSink};
use crate::agent::{AgentRegistry, MediaKind};
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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, Notify, RwLock};

/// WS 出向事件：扁平化 JSON，与 TurnEvent 一一对应 + 协议层事件
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebEvent {
    Chunk {
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
    /// 主动推送（cron 任务结果等，非 turn 事件）
    Proactive {
        message: String,
    },
}

/// turn 结束信号：on_done/on_error/on_interrupted 三个终态都发送
#[derive(Debug, Clone, Copy)]
pub struct TurnEndSignal;

/// Web 输出 sink：把 OutputSink 回调转成 WebEvent 推到 mpsc，WS 写 task 消费。
/// 持有 turn-end sender 让 WS handler 主循环感知 turn 结束以清理 current_turn。
pub struct WebSink {
    tx: mpsc::Sender<WebEvent>,
    turn_end_tx: mpsc::Sender<TurnEndSignal>,
}

impl WebSink {
    pub fn new(tx: mpsc::Sender<WebEvent>, turn_end_tx: mpsc::Sender<TurnEndSignal>) -> Self {
        Self { tx, turn_end_tx }
    }
}

#[async_trait]
impl OutputSink for WebSink {
    async fn on_chunk(&mut self, delta: &str) {
        let _ = self
            .tx
            .send(WebEvent::Chunk {
                delta: delta.into(),
            })
            .await;
    }
    async fn on_tool_start(&mut self, name: &str) {
        let _ = self
            .tx
            .send(WebEvent::ToolStart {
                id: String::new(),
                name: name.into(),
            })
            .await;
    }
    async fn on_tool_result(&mut self, output: &str) {
        let _ = self
            .tx
            .send(WebEvent::ToolResult {
                id: String::new(),
                output: output.into(),
            })
            .await;
    }
    async fn on_media(&mut self, path: &str, kind: MediaKind) {
        let _ = self
            .tx
            .send(WebEvent::Media {
                path: path.into(),
                kind,
            })
            .await;
    }
    async fn on_done(&mut self) {
        let _ = self.tx.send(WebEvent::Done).await;
        let _ = self.turn_end_tx.send(TurnEndSignal).await;
    }
    async fn on_error(&mut self, message: &str) {
        let _ = self
            .tx
            .send(WebEvent::Error {
                message: message.into(),
            })
            .await;
        let _ = self.turn_end_tx.send(TurnEndSignal).await;
    }
    async fn on_interrupted(&mut self) {
        // 与 QqSink 一致：只 log，不回推 WS 帧（前端按钮状态本身体现中断）
        tracing::info!("web turn interrupted");
        let _ = self.turn_end_tx.send(TurnEndSignal).await;
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
            serde_json::to_string(&WebEvent::AuthOk).unwrap(),
        ))
        .await;

    // 写 task：rx → ws_sink
    let write_task = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let json = match serde_json::to_string(&ev) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if ws_sink.send(Message::Text(json)).await.is_err() {
                break;
            }
        }
    });

    let agent = state.registry.main.clone();
    let workspace = {
        let a = agent.lock().await;
        a.workspace.clone()
    };
    let stop: Arc<Notify> = Arc::new(Notify::new());
    let mut current_turn: Option<tokio::task::JoinHandle<()>> = None;

    loop {
        tokio::select! {
            // WS 入向消息
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(s))) => {
                        let chat: Option<ChatIn> = serde_json::from_str(&s).ok();
                        match chat.as_ref().map(|c| c.kind.as_str()) {
                            Some("ping") => { let _ = tx.send(WebEvent::Pong).await; }
                            Some("stop") => {
                                if current_turn.is_some() {
                                    stop.notify_one();
                                }
                            }
                            Some("chat") => {
                                if current_turn.is_some() {
                                    let _ = tx.send(WebEvent::Busy { reason: "another turn running".into() }).await;
                                } else {
                                    let chat: ChatIn = serde_json::from_str(&s).unwrap();
                                    let text = chat.text.unwrap_or_default();
                                    tracing::info!(text = %text, images = chat.images.as_ref().map(|v| v.len()).unwrap_or(0), "web received message");

                                    // 斜杠命令拦截（P4-d）：与其它频道一致地走审批/续跑流。
                                    // web 用 type:"stop" 中断（见上面 "stop" 分支），不认 /stop，故排除之。
                                    let slash_outcome = if text.starts_with('/') && text.trim() != "/stop" {
                                        let mut a = agent.lock().await;
                                        Some(try_handle(&text, &mut a).await)
                                    } else {
                                        None
                                    };
                                    match slash_outcome {
                                        Some(Ok(SlashOutcome::Handled(msg))) => {
                                            let _ = tx.send(WebEvent::Chunk { delta: msg }).await;
                                            let _ = tx.send(WebEvent::Done).await;
                                        }
                                        Some(Ok(SlashOutcome::Resume { notice, message })) => {
                                            // 先回显结果摘要，再跑 continuation turn 让模型基于工具结果继续
                                            let _ = tx.send(WebEvent::Chunk { delta: notice }).await;
                                            let sink = Box::new(WebSink::new(tx.clone(), end_tx.clone()));
                                            let stop_clone = stop.clone();
                                            let agent_clone = agent.clone();
                                            current_turn = Some(tokio::spawn(async move {
                                                let _ = run_turn(agent_clone, ChatMessage::user(&message), "web".into(), sink, stop_clone).await;
                                            }));
                                        }
                                        Some(Ok(SlashOutcome::Exit)) => {
                                            // web 常驻连接，/exit 无意义，忽略
                                        }
                                        Some(Ok(SlashOutcome::NotSlash)) | None => {
                                            // 普通消息：构造并跑一轮 agent turn
                                            let user_msg = build_user_message(&text, chat.images.as_deref(), &workspace);
                                            let sink = Box::new(WebSink::new(tx.clone(), end_tx.clone()));
                                            let stop_clone = stop.clone();
                                            let agent_clone = agent.clone();
                                            current_turn = Some(tokio::spawn(async move {
                                                let _ = run_turn(agent_clone, user_msg, "web".into(), sink, stop_clone).await;
                                            }));
                                        }
                                        Some(Err(e)) => {
                                            let _ = tx.send(WebEvent::Error { message: e.to_string() }).await;
                                            let _ = tx.send(WebEvent::Done).await;
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
            // turn 结束信号
            _ = end_rx.recv() => {
                if let Some(h) = current_turn.take() {
                    let _ = h.await;
                }
            }
        }
    }

    // 清理
    if let Some(h) = current_turn.take() {
        stop.notify_one();
        let _ = h.await;
    }
    // 从 active_ws 注销（避免向已关闭连接广播）
    state.active_ws.lock().await.remove(&ws_id);
    write_task.abort();
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
    let uploads_dir = workspace.join("uploads");
    for img_rel in imgs {
        match resolve_within(&uploads_dir, img_rel) {
            Ok(abs) => {
                if !image_utils::is_image_file(&abs) {
                    parts.push(ContentPart::Text {
                        text: format!("[not an image: {}]", img_rel),
                    });
                    continue;
                }
                match image_utils::prepare_image_for_vision(&abs) {
                    Ok(data_url) => {
                        parts.push(ContentPart::ImageUrl {
                            image_url: ImageUrlContent { url: data_url },
                        });
                    }
                    Err(e) => {
                        parts.push(ContentPart::Text {
                            text: format!("[image load failed: {}]", e),
                        });
                    }
                }
            }
            Err(e) => {
                parts.push(ContentPart::Text {
                    text: format!("[invalid path: {}]", e),
                });
            }
        }
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
}

impl WebChannel {
    pub fn new(
        web_config: WebUiConfig,
        registry: Arc<AgentRegistry>,
        config_full: Arc<RwLock<Config>>,
        config_path: PathBuf,
        workspace: PathBuf,
        shutdown_signal: Arc<Notify>,
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
    async fn run(self: Arc<Self>, _registry: Arc<AgentRegistry>) -> Result<(), anyhow::Error> {
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
}
