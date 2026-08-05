use crate::agent::sink::{run_turn, OutputSink};
use crate::agent::{AgentRegistry, MediaKind};
use crate::channels::Channel;
use crate::config::{Config, WebUiConfig};
use crate::image_utils;
use crate::provider::{ChatMessage, ContentPart, ImageUrlContent};
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
    Chunk { delta: String },
    ToolStart { id: String, name: String },
    ToolResult { id: String, output: String },
    Media { path: String, kind: MediaKind },
    Done,
    Error { message: String },
    Interrupted,
    // 协议层
    Pong,
    AuthOk,
    AuthFailed { reason: String },
    Busy { reason: String },
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
                                    // 构造消息
                                    let user_msg = build_user_message(&text, chat.images.as_deref(), &workspace);
                                    let sink = Box::new(WebSink::new(tx.clone(), end_tx.clone()));
                                    let stop_clone = stop.clone();
                                    let agent_clone = agent.clone();
                                    current_turn = Some(tokio::spawn(async move {
                                        let _ = run_turn(agent_clone, user_msg, "web".into(), sink, stop_clone).await;
                                    }));
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
}

impl WebChannel {
    pub fn new(
        web_config: WebUiConfig,
        registry: Arc<AgentRegistry>,
        config_full: Arc<RwLock<Config>>,
        config_path: PathBuf,
        workspace: PathBuf,
    ) -> Self {
        Self {
            config: web_config,
            registry,
            config_full,
            config_path,
            workspace,
        }
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
        let state = AppState {
            registry: self.registry.clone(),
            config: self.config_full.clone(),
            config_path: self.config_path.clone(),
            workspace: self.workspace.clone(),
            token: Arc::new(token),
        };
        // 系统级路由 + WS 路由，共享同一个 state
        build_system_routes()
            .route("/ws", axum::routing::get(ws_handler))
            .with_state(state)
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
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| anyhow::anyhow!("bind {}: {}", addr, e))?;
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
