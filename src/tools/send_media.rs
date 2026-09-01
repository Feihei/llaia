use crate::agent::{MediaKind, TurnEvent};
use crate::image_utils::is_image_file;
use crate::path_guard;
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

/// 发送图片给用户的工具。
/// 校验路径在「当前工作目录 ∪ 受信目录 ∪ agent 家目录」内且为图片，通过 MediaOutput 事件通知 channel 发送。
/// 作用域与文件工具同源（共享 workspace_root / trusted_dirs Arc），/move 后实时生效；
/// 家目录恒可发送（tts/、uploads/ 等产出不因 /move 失联）。
pub struct SendImage {
    workspace: Arc<RwLock<PathBuf>>,
    trusted: Arc<RwLock<Vec<PathBuf>>>,
    /// agent 家目录（固定）：作为额外可发送范围，语义同 file_read 的 extra_readable
    home: PathBuf,
}

/// 发送文件给用户的工具。
/// 校验路径在「当前工作目录 ∪ 受信目录 ∪ agent 家目录」内，通过 MediaOutput 事件通知 channel 发送。
/// 作用域与文件工具同源（共享 workspace_root / trusted_dirs Arc），/move 后实时生效；
/// 家目录恒可发送（tts/、uploads/ 等产出不因 /move 失联）。
pub struct SendFile {
    workspace: Arc<RwLock<PathBuf>>,
    trusted: Arc<RwLock<Vec<PathBuf>>>,
    /// agent 家目录（固定）：作为额外可发送范围，语义同 file_read 的 extra_readable
    home: PathBuf,
}

impl SendImage {
    pub fn new(
        workspace: Arc<RwLock<PathBuf>>,
        trusted: Arc<RwLock<Vec<PathBuf>>>,
        home: PathBuf,
    ) -> Self {
        Self {
            workspace,
            trusted,
            home,
        }
    }
}

impl SendFile {
    pub fn new(
        workspace: Arc<RwLock<PathBuf>>,
        trusted: Arc<RwLock<Vec<PathBuf>>>,
        home: PathBuf,
    ) -> Self {
        Self {
            workspace,
            trusted,
            home,
        }
    }
}

/// 在「workspace_root ∪ 受信目录 ∪ 家目录」内解析路径。
/// 前两者与文件工具同一套 validate_path_in_scope 语义；家目录作为额外可发送范围。
async fn resolve_in_scope(
    workspace: &Arc<RwLock<PathBuf>>,
    trusted: &Arc<RwLock<Vec<PathBuf>>>,
    home: &Path,
    path: &str,
) -> Result<PathBuf> {
    let ws = workspace.read().await;
    let trusted = trusted.read().await.clone();
    path_guard::validate_path_in_scope(&ws, &trusted, path, Some(home))
}

#[async_trait]
impl Tool for SendImage {
    fn name(&self) -> &str {
        "send_image"
    }
    fn description(&self) -> &str {
        "Send an image file to the user. The path must point to an image file (jpg/png/gif/webp/bmp) within the current working directory, a trusted directory, or the agent home workspace."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the image file (relative to the current working directory)"
                }
            },
            "required": ["path"]
        })
    }
    /// false: 只读操作（不修改本地文件），作用域边界检查保证安全
    fn requires_confirm(&self) -> bool {
        false
    }
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        // 无事件通道时（非流式调用）：仅校验，不发送
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'path' argument"))?;
        let resolved = resolve_in_scope(&self.workspace, &self.trusted, &self.home, path).await?;
        if !is_image_file(&resolved) {
            return Err(anyhow!(
                "path {:?} is not an image file (expected jpg/png/gif/webp/bmp)",
                resolved
            ));
        }
        let meta = tokio::fs::metadata(&resolved)
            .await
            .map_err(|e| anyhow!("access {:?}: {}", resolved, e))?;
        if meta.len() == 0 {
            return Err(anyhow!("image file {:?} is empty (0 bytes)", resolved));
        }
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
        let resolved = resolve_in_scope(&self.workspace, &self.trusted, &self.home, path).await?;
        if !is_image_file(&resolved) {
            return Err(anyhow!(
                "path {:?} is not an image file (expected jpg/png/gif/webp/bmp)",
                resolved
            ));
        }
        let meta = tokio::fs::metadata(&resolved)
            .await
            .map_err(|e| anyhow!("access {:?}: {}", resolved, e))?;
        if meta.len() == 0 {
            return Err(anyhow!("image file {:?} is empty (0 bytes)", resolved));
        }

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
        "Send a file to the user. The path must point to a file within the current working directory, a trusted directory, or the agent home workspace."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file (relative to the current working directory)"
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
        let resolved = resolve_in_scope(&self.workspace, &self.trusted, &self.home, path).await?;
        let meta = tokio::fs::metadata(&resolved)
            .await
            .map_err(|e| anyhow!("access {:?}: {}", resolved, e))?;
        if meta.len() == 0 {
            return Err(anyhow!("file {:?} is empty (0 bytes)", resolved));
        }
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
        let resolved = resolve_in_scope(&self.workspace, &self.trusted, &self.home, path).await?;
        let meta = tokio::fs::metadata(&resolved)
            .await
            .map_err(|e| anyhow!("access {:?}: {}", resolved, e))?;
        if meta.len() == 0 {
            return Err(anyhow!("file {:?} is empty (0 bytes)", resolved));
        }

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

    /// 测试夹具：一对 send 工具 + 可操纵的作用域句柄
    struct MediaTools {
        img: SendImage,
        file: SendFile,
        root: Arc<RwLock<PathBuf>>,
        trusted: Arc<RwLock<Vec<PathBuf>>>,
    }

    fn tool_pair(ws: PathBuf) -> MediaTools {
        let root = Arc::new(RwLock::new(ws.clone()));
        let trusted: Arc<RwLock<Vec<PathBuf>>> = Arc::new(RwLock::new(Vec::new()));
        MediaTools {
            img: SendImage::new(root.clone(), trusted.clone(), ws.clone()),
            file: SendFile::new(root.clone(), trusted.clone(), ws),
            root,
            trusted,
        }
    }

    #[tokio::test]
    async fn test_send_image_validates_image_extension() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();

        // 非图片文件应报错
        std::fs::write(ws_path.join("doc.txt"), "hello").unwrap();
        let pair = tool_pair(ws_path.clone());
        let result = pair.img.execute(&json!({"path": "doc.txt"}), "cli").await;
        assert!(result.is_err());

        // 图片文件应成功（仅校验，不发送）
        std::fs::write(ws_path.join("img.jpg"), b"fake jpg").unwrap();
        let result = pair.img.execute(&json!({"path": "img.jpg"}), "cli").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_send_image_workspace_boundary() {
        let ws = tempdir().unwrap();
        let pair = tool_pair(ws.path().to_path_buf());
        // 路径逃逸应报错
        let result = pair
            .img
            .execute(&json!({"path": "../outside.jpg"}), "cli")
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_send_file_any_extension() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        std::fs::write(ws_path.join("data.bin"), b"binary").unwrap();

        let pair = tool_pair(ws_path);
        let result = pair.file.execute(&json!({"path": "data.bin"}), "cli").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_send_image_with_events_emits_media_output() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        std::fs::write(ws_path.join("pic.png"), b"fake png").unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let pair = tool_pair(ws_path);
        let result = pair
            .img
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

    /// 回归（用户报告）：/move 到 workspace 外目录后，send_file 必须能发
    /// moved 目录内的文件。模拟生产形态：workspace_root 切到 moved 目录、
    /// 受信集合只含 moved 目标（/move 批准登记）、家目录不在受信里但恒可发送。
    #[tokio::test]
    async fn test_send_media_follows_moved_workspace() {
        let home = tempdir().unwrap();
        let moved = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let home_path = home.path().to_path_buf();
        let moved_path = moved.path().to_path_buf();

        std::fs::write(home_path.join("voice.mp3"), b"home artifact").unwrap();
        std::fs::write(moved_path.join("new_report.pptx"), b"moved file").unwrap();
        std::fs::write(outside.path().join("stranger.pptx"), b"outside").unwrap();

        let pair = tool_pair(home_path.clone());
        // 模拟 /move：workspace_root 切到 moved 目录，受信集合登记 moved 目标
        *pair.root.write().await = moved_path.clone();
        *pair.trusted.write().await = vec![moved_path.clone()];

        // moved 目录内绝对路径 → 放行（用户报告的核心场景）
        let r = pair
            .file
            .execute(
                &json!({"path": moved_path.join("new_report.pptx").to_string_lossy()}),
                "qq",
            )
            .await;
        assert!(r.is_ok(), "moved 目录内文件应可发送: {:?}", r);

        // 相对路径按新根解析
        let r = pair
            .file
            .execute(&json!({"path": "new_report.pptx"}), "qq")
            .await;
        assert!(r.is_ok(), "相对路径应解析到 moved 目录: {:?}", r);

        // 家目录不在受信集合里，但经 home 额外范围仍可达（tts/ 等产出不失联）
        let r = pair
            .file
            .execute(
                &json!({"path": home_path.join("voice.mp3").to_string_lossy()}),
                "qq",
            )
            .await;
        assert!(r.is_ok(), "家目录产物在 /move 后仍应可发送: {:?}", r);

        // 作用域之外的路径 → 拒绝
        let r = pair
            .file
            .execute(
                &json!({"path": outside.path().join("stranger.pptx").to_string_lossy()}),
                "qq",
            )
            .await;
        assert!(r.is_err(), "作用域外路径必须被拒绝");
    }
}
