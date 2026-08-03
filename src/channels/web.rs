use crate::agent::MediaKind;
use serde::Serialize;

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
}
