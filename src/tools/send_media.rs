use crate::agent::{MediaKind, TurnEvent};
use crate::image_utils::is_image_file;
use crate::tools::file::resolve_within;
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use tokio::sync::mpsc;

/// 发送图片给用户的工具。
/// 校验路径在 workspace 内且为图片，通过 MediaOutput 事件通知 channel 发送。
pub struct SendImage {
    workspace: PathBuf,
}

/// 发送文件给用户的工具。
/// 校验路径在 workspace 内，通过 MediaOutput 事件通知 channel 发送。
pub struct SendFile {
    workspace: PathBuf,
}

impl SendImage {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

impl SendFile {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for SendImage {
    fn name(&self) -> &str {
        "send_image"
    }
    fn description(&self) -> &str {
        "Send an image file to the user. The path must point to an image file (jpg/png/gif/webp/bmp) within the agent workspace."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the image file (relative to agent workspace)"
                }
            },
            "required": ["path"]
        })
    }
    /// false: 只读操作（不修改本地文件），workspace 边界检查保证安全
    fn requires_confirm(&self) -> bool {
        false
    }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        // 无事件通道时（非流式调用）：仅校验，不发送
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path' argument"))?;
        let resolved = resolve_within(&self.workspace, path)?;
        if !is_image_file(&resolved) {
            return Err(anyhow!(
                "path {:?} is not an image file (expected jpg/png/gif/webp/bmp)",
                resolved
            ));
        }
        tokio::fs::metadata(&resolved)
            .await
            .map_err(|e| anyhow!("access {:?}: {}", resolved, e))?;
        Ok(format!("image ready: {}", resolved.display()))
    }

    /// 带事件转发的 execute：通过 MediaOutput 事件通知 channel 实际发送
    async fn execute_with_events(
        &self,
        args: &Value,
        _channel: &str,
        event_tx: Option<&mpsc::Sender<TurnEvent>>,
    ) -> Result<String> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path' argument"))?;
        let resolved = resolve_within(&self.workspace, path)?;
        if !is_image_file(&resolved) {
            return Err(anyhow!(
                "path {:?} is not an image file (expected jpg/png/gif/webp/bmp)",
                resolved
            ));
        }
        tokio::fs::metadata(&resolved)
            .await
            .map_err(|e| anyhow!("access {:?}: {}", resolved, e))?;

        // 通过事件通知 channel 发送图片
        if let Some(tx) = event_tx {
            let _ = tx
                .send(TurnEvent::MediaOutput {
                    path: resolved.to_string_lossy().to_string(),
                    kind: MediaKind::Image,
                })
                .await;
        }
        Ok(format!("sent image: {}", resolved.display()))
    }
}

#[async_trait]
impl Tool for SendFile {
    fn name(&self) -> &str {
        "send_file"
    }
    fn description(&self) -> &str {
        "Send a file to the user. The path must point to a file within the agent workspace."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file (relative to agent workspace)"
                }
            },
            "required": ["path"]
        })
    }
    fn requires_confirm(&self) -> bool {
        false
    }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path' argument"))?;
        let resolved = resolve_within(&self.workspace, path)?;
        tokio::fs::metadata(&resolved)
            .await
            .map_err(|e| anyhow!("access {:?}: {}", resolved, e))?;
        Ok(format!("file ready: {}", resolved.display()))
    }

    async fn execute_with_events(
        &self,
        args: &Value,
        _channel: &str,
        event_tx: Option<&mpsc::Sender<TurnEvent>>,
    ) -> Result<String> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path' argument"))?;
        let resolved = resolve_within(&self.workspace, path)?;
        tokio::fs::metadata(&resolved)
            .await
            .map_err(|e| anyhow!("access {:?}: {}", resolved, e))?;

        if let Some(tx) = event_tx {
            let _ = tx
                .send(TurnEvent::MediaOutput {
                    path: resolved.to_string_lossy().to_string(),
                    kind: MediaKind::File,
                })
                .await;
        }
        Ok(format!("sent file: {}", resolved.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_send_image_validates_image_extension() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();

        // 非图片文件应报错
        std::fs::write(ws_path.join("doc.txt"), "hello").unwrap();
        let tool = SendImage::new(ws_path.clone());
        let result = tool
            .execute(&json!({"path": "doc.txt"}), "cli")
            .await;
        assert!(result.is_err());

        // 图片文件应成功（仅校验，不发送）
        std::fs::write(ws_path.join("img.jpg"), b"fake jpg").unwrap();
        let result = tool
            .execute(&json!({"path": "img.jpg"}), "cli")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_send_image_workspace_boundary() {
        let ws = tempdir().unwrap();
        let tool = SendImage::new(ws.path().to_path_buf());
        // 路径逃逸应报错
        let result = tool
            .execute(&json!({"path": "../outside.jpg"}), "cli")
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_send_file_any_extension() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        std::fs::write(ws_path.join("data.bin"), b"binary").unwrap();

        let tool = SendFile::new(ws_path);
        let result = tool
            .execute(&json!({"path": "data.bin"}), "cli")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_send_image_with_events_emits_media_output() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        std::fs::write(ws_path.join("pic.png"), b"fake png").unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let tool = SendImage::new(ws_path);
        let result = tool
            .execute_with_events(&json!({"path": "pic.png"}), "cli", Some(&tx))
            .await;
        assert!(result.is_ok());

        let ev = rx.recv().await.unwrap();
        match ev {
            TurnEvent::MediaOutput { path, kind } => {
                assert!(path.ends_with("pic.png"));
                assert!(matches!(kind, MediaKind::Image));
            }
            _ => panic!("expected MediaOutput event"),
        }
    }
}
