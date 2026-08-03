use crate::agent::sink::OutputSink;
use crate::agent::MediaKind;
use async_trait::async_trait;
use serde::Serialize;
use tokio::sync::mpsc;

/// WS 出向事件：扁平化 JSON，与 TurnEvent 一一对应 + 协议层事件
#[derive(Debug, Clone, Serialize)]
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
        let _ = self.tx.send(WebEvent::Chunk { delta: delta.into() }).await;
    }
    async fn on_tool_start(&mut self, name: &str) {
        let _ = self.tx.send(WebEvent::ToolStart { id: String::new(), name: name.into() }).await;
    }
    async fn on_tool_result(&mut self, output: &str) {
        let _ = self.tx.send(WebEvent::ToolResult { id: String::new(), output: output.into() }).await;
    }
    async fn on_media(&mut self, path: &str, kind: MediaKind) {
        let _ = self.tx.send(WebEvent::Media { path: path.into(), kind }).await;
    }
    async fn on_done(&mut self) {
        let _ = self.tx.send(WebEvent::Done).await;
        let _ = self.turn_end_tx.send(TurnEndSignal).await;
    }
    async fn on_error(&mut self, message: &str) {
        let _ = self.tx.send(WebEvent::Error { message: message.into() }).await;
        let _ = self.turn_end_tx.send(TurnEndSignal).await;
    }
    async fn on_interrupted(&mut self) {
        // 与 QqSink 一致：只 log，不回推 WS 帧（前端按钮状态本身体现中断）
        tracing::info!("web turn interrupted");
        let _ = self.turn_end_tx.send(TurnEndSignal).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_web_event_chunk_serialization() {
        let ev = WebEvent::Chunk { delta: "hello".into() };
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
        let ev = WebEvent::Media { path: "out/a.png".into(), kind: MediaKind::Image };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains(r#""type":"media""#));
        assert!(json.contains(r#""path":"out/a.png""#));
    }

    #[test]
    fn test_web_event_auth_failed_serialization() {
        let ev = WebEvent::AuthFailed { reason: "invalid token".into() };
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
