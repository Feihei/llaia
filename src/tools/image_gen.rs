//! Image generation/editing tools: OpenAI-compatible `/images/generations`
//! and `/images/edits` endpoints.
//!
//! - One `[tools.image_gen]` config drives both tools (`image_gen` / `image_edit`)
//! - Works with any OpenAI-compatible images backend: sd-server
//!   (stable-diffusion.cpp), agnes, OpenAI, ...
//! - Generation (tool) and delivery (`send_image` / MediaOutput) stay separated,
//!   mirroring the `tts` + `send_file` pattern
//! - Artifacts land in `workspace/images/<uuid>.png`; the response `data[0]` may
//!   carry `b64_json`, an http(s) `url`, or a `data:` URI — all handled
//! - `image_edit` input paths resolve within the same scope as `send_image`
//!   (workspace_root ∪ trusted dirs ∪ agent home), so files under `uploads/`
//!   stay reachable after `/move`

use crate::config::ImageGenConfig;
use crate::image_utils::is_image_file;
use crate::path_guard;
use crate::tools::file::resolve_within;
use crate::tools::Tool;
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// 生成的图片落 `workspace/images/`（与 tts/ 平级的固定产物目录）。
const OUT_DIR: &str = "images";

fn mime_from_ext(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => "image/png",
    }
}

/// 共享的端点访问参数（两个工具各持一份克隆）。
#[derive(Clone)]
struct Endpoint {
    base_url: String,
    api_key: String,
    model: String,
    size: String,
    timeout: Duration,
}

impl Endpoint {
    fn from_cfg(cfg: &ImageGenConfig) -> Self {
        Self {
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            api_key: cfg.api_key.clone(),
            model: cfg.model.trim().to_string(),
            size: cfg.size.trim().to_string(),
            timeout: Duration::from_secs(cfg.timeout_secs.max(1)),
        }
    }

    fn client(&self) -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder().timeout(self.timeout).build()?)
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.api_key.is_empty() {
            req
        } else {
            req.bearer_auth(&self.api_key)
        }
    }

    /// 解析 images API 响应 `data[0]` 并把图片字节落盘。
    async fn extract_and_save(&self, resp: reqwest::Response, workspace: &Path) -> Result<PathBuf> {
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            let snippet: String = text.chars().take(300).collect();
            bail!("image endpoint returned HTTP {status}: {snippet}");
        }
        let v: Value =
            serde_json::from_str(&text).map_err(|e| anyhow!("parse image response: {e}"))?;
        let item = v
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first())
            .ok_or_else(|| {
                let snippet: String = text.chars().take(200).collect();
                anyhow!("image response has no data[0]: {snippet}")
            })?;
        let bytes: Vec<u8> = if let Some(b64) = item.get("b64_json").and_then(|s| s.as_str()) {
            STANDARD
                .decode(b64.trim())
                .map_err(|e| anyhow!("decode b64_json: {e}"))?
        } else if let Some(url) = item.get("url").and_then(|s| s.as_str()) {
            if let Some(b64) = url
                .strip_prefix("data:")
                .and_then(|rest| rest.split_once(",base64,"))
            {
                STANDARD
                    .decode(b64.1.trim())
                    .map_err(|e| anyhow!("decode data URI: {e}"))?
            } else {
                let client = self.client()?;
                let resp = self
                    .auth(client.get(url))
                    .send()
                    .await
                    .map_err(|e| anyhow!("download image from {url}: {e}"))?;
                if !resp.status().is_success() {
                    bail!("download image from {url}: HTTP {}", resp.status());
                }
                resp.bytes().await?.to_vec()
            }
        } else {
            bail!("image response data[0] has neither b64_json nor url");
        };
        if bytes.is_empty() {
            bail!("empty image bytes from endpoint");
        }
        let rel = format!("{OUT_DIR}/{}.png", uuid::Uuid::new_v4());
        let out_path = resolve_within(workspace, &rel)?;
        if let Some(parent) = out_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&out_path, &bytes).await?;
        Ok(out_path)
    }
}

/// `image_gen`：文生图（POST /images/generations）。
pub struct ImageGenTool {
    ep: Endpoint,
    workspace: PathBuf,
}

impl ImageGenTool {
    /// 按 `[tools.image_gen]` 构建；`enabled` 关闭时返回 None；本地 sd-server
    /// 通常无 key，`allow_no_key = true`（api_key 为空也注册）。
    pub fn build(
        cfg: &ImageGenConfig,
        allow_no_key: bool,
        workspace: PathBuf,
    ) -> Result<Option<Arc<dyn Tool>>> {
        if !cfg.enabled {
            return Ok(None);
        }
        if !allow_no_key && cfg.api_key.is_empty() {
            return Ok(None);
        }
        Ok(Some(Arc::new(ImageGenTool {
            ep: Endpoint::from_cfg(cfg),
            workspace,
        })))
    }

    async fn generate(&self, prompt: &str, size: Option<&str>) -> Result<PathBuf> {
        let mut body = json!({
            "prompt": prompt,
            "n": 1,
            "response_format": "b64_json",
        });
        if !self.ep.model.is_empty() {
            body["model"] = json!(self.ep.model);
        }
        let size = size
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.ep.size);
        if !size.is_empty() {
            body["size"] = json!(size);
        }
        let client = self.ep.client()?;
        let url = format!("{}/images/generations", self.ep.base_url);
        let resp = self
            .ep
            .auth(client.post(&url))
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("image_gen request failed: {e}"))?;
        self.ep.extract_and_save(resp, &self.workspace).await
    }
}

#[async_trait]
impl Tool for ImageGenTool {
    fn name(&self) -> &str {
        "image_gen"
    }

    fn description(&self) -> &str {
        "Generate an image from a text prompt via the configured OpenAI-compatible image endpoint (sd-server, agnes, OpenAI, ...). Saves the PNG into the workspace and returns its path; use send_image to deliver it to the user. Preferred for quick text-to-image generation."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "Image description. Backend-specific parameters may be appended inline, e.g. sd-server accepts a {\"seed\": 123} JSON blob inside the prompt." },
                "size": { "type": "string", "description": "Optional WIDTHxHEIGHT override (e.g. 1024x1024). Omit to use the configured default." }
            },
            "required": ["prompt"]
        })
    }

    /// 只写 workspace 内文件（images/）→ 免确认，对齐 tts。
    fn requires_confirm(&self) -> bool {
        false
    }

    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'prompt' argument"))?
            .trim();
        if prompt.is_empty() {
            bail!("prompt must not be empty");
        }
        let size = args.get("size").and_then(|v| v.as_str());
        let path = self.generate(prompt, size).await?;
        Ok(format!("image generated: {}", path.display()))
    }
}

/// `image_edit`：图片编辑（multipart POST /images/edits，image + 可选 mask）。
pub struct ImageEditTool {
    ep: Endpoint,
    workspace: PathBuf,
    /// 文件/终端工具同一套实时作用域（/move 实时生效）
    workspace_root: Arc<RwLock<PathBuf>>,
    trusted: Arc<RwLock<Vec<PathBuf>>>,
    /// agent 家目录：uploads/ 等恒可读
    home: PathBuf,
}

impl ImageEditTool {
    pub fn build(
        cfg: &ImageGenConfig,
        allow_no_key: bool,
        workspace: PathBuf,
        workspace_root: Arc<RwLock<PathBuf>>,
        trusted: Arc<RwLock<Vec<PathBuf>>>,
    ) -> Result<Option<Arc<dyn Tool>>> {
        if !cfg.enabled {
            return Ok(None);
        }
        if !allow_no_key && cfg.api_key.is_empty() {
            return Ok(None);
        }
        let home = workspace.clone();
        Ok(Some(Arc::new(ImageEditTool {
            ep: Endpoint::from_cfg(cfg),
            workspace,
            workspace_root,
            trusted,
            home,
        })))
    }

    /// 在「workspace_root ∪ 受信目录 ∪ 家目录」内解析输入图片路径（同 send_image）。
    async fn resolve_input(&self, path: &str) -> Result<PathBuf> {
        let ws = self.workspace_root.read().await;
        let trusted = self.trusted.read().await.clone();
        path_guard::validate_path_in_scope(&ws, &trusted, path, Some(&self.home))
    }

    async fn edit(
        &self,
        image: &Path,
        prompt: &str,
        mask: Option<&Path>,
        size: Option<&str>,
    ) -> Result<PathBuf> {
        let img_bytes = tokio::fs::read(image)
            .await
            .map_err(|e| anyhow!("read source image {:?}: {e}", image))?;
        let img_mime = mime_from_ext(image);
        let mut form = reqwest::multipart::Form::new()
            .text("prompt", prompt.to_string())
            .part(
                "image",
                reqwest::multipart::Part::bytes(img_bytes)
                    .file_name("image.png")
                    .mime_str(img_mime)?,
            );
        if let Some(m) = mask {
            let mask_bytes = tokio::fs::read(m)
                .await
                .map_err(|e| anyhow!("read mask {:?}: {e}", m))?;
            form = form.part(
                "mask",
                reqwest::multipart::Part::bytes(mask_bytes)
                    .file_name("mask.png")
                    .mime_str(mime_from_ext(m))?,
            );
        }
        if !self.ep.model.is_empty() {
            form = form.text("model", self.ep.model.clone());
        }
        let size = size
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.ep.size);
        if !size.is_empty() {
            form = form.text("size", size.to_string());
        }
        form = form.text("n", "1").text("response_format", "b64_json");

        let client = self.ep.client()?;
        let url = format!("{}/images/edits", self.ep.base_url);
        let resp = self
            .ep
            .auth(client.post(&url))
            .multipart(form)
            .send()
            .await
            .map_err(|e| anyhow!("image_edit request failed: {e}"))?;
        self.ep.extract_and_save(resp, &self.workspace).await
    }
}

#[async_trait]
impl Tool for ImageEditTool {
    fn name(&self) -> &str {
        "image_edit"
    }

    fn description(&self) -> &str {
        "Edit an existing image with a text prompt via the OpenAI-compatible /images/edits endpoint (multipart upload; optional mask: white areas get edited). Provide the path of the source image within the workspace, a trusted directory, or the agent home. Returns the edited PNG path in the workspace; use send_image to deliver it to the user."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "image": { "type": "string", "description": "Path to the source image (relative to cwd or absolute)." },
                "prompt": { "type": "string", "description": "Description of the edit to apply." },
                "mask": { "type": "string", "description": "Optional path to a mask image (white = edit, black = keep)." },
                "size": { "type": "string", "description": "Optional WIDTHxHEIGHT override. Omit to use the configured default." }
            },
            "required": ["image", "prompt"]
        })
    }

    fn requires_confirm(&self) -> bool {
        false
    }

    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let image = args
            .get("image")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'image' argument"))?;
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'prompt' argument"))?
            .trim();
        if prompt.is_empty() {
            bail!("prompt must not be empty");
        }
        let resolved = self.resolve_input(image).await?;
        if !is_image_file(&resolved) {
            bail!(
                "source {:?} is not an image file (expected jpg/png/gif/webp/bmp)",
                resolved
            );
        }
        let mask = match args.get("mask").and_then(|v| v.as_str()) {
            Some(m) if !m.trim().is_empty() => {
                let resolved_mask = self.resolve_input(m).await?;
                if !is_image_file(&resolved_mask) {
                    bail!(
                        "mask {:?} is not an image file (expected jpg/png/gif/webp/bmp)",
                        resolved_mask
                    );
                }
                Some(resolved_mask)
            }
            _ => None,
        };
        let size = args.get("size").and_then(|v| v.as_str());
        let path = self.edit(&resolved, prompt, mask.as_deref(), size).await?;
        Ok(format!("image edited: {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use tempfile::tempdir;

    fn cfg(base_url: &str) -> ImageGenConfig {
        ImageGenConfig {
            enabled: true,
            base_url: base_url.to_string(),
            api_key: "sk-test".into(),
            model: "sd-test".into(),
            size: "512x512".into(),
            timeout_secs: 10,
        }
    }

    /// 极简 mock：循环 accept，按序回 canned 响应并捕获请求体。
    /// `build` 在 bind 拿到端口后调用（响应体可内嵌自身地址）。
    fn spawn_mock(
        build: impl FnOnce(&str) -> Vec<(String, String)> + Send + 'static,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_cl = captured.clone();
        std::thread::spawn(move || {
            let responses = build(&format!("http://127.0.0.1:{port}/v1"));
            let mut it = responses.into_iter();
            for stream in listener.incoming() {
                let Some(resp) = it.next() else { break };
                let (status, body) = resp;
                let mut stream = stream.unwrap();
                let mut buf = [0u8; 65536];
                let mut raw = Vec::new();
                // 读到请求头结束 + 按 content-length 补 body（够 mock 用即可）
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    let s = String::from_utf8_lossy(&raw).to_string();
                    if let Some(pos) = s.find("\r\n\r\n") {
                        let len = s
                            .lines()
                            .find(|l| l.to_lowercase().starts_with("content-length:"))
                            .and_then(|l| l.split(':').nth(1))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if raw.len() >= pos + 4 + len {
                            break;
                        }
                    }
                }
                captured_cl
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&raw).to_string());
                let full = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(full.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}/v1"), captured)
    }

    fn fake_png_b64() -> String {
        STANDARD.encode(b"fake-png-bytes")
    }

    #[test]
    fn build_gates_on_enabled_and_key() {
        let ws = tempdir().unwrap();
        let mut c = cfg("http://localhost:9/v1");
        c.enabled = false;
        assert!(ImageGenTool::build(&c, false, ws.path().to_path_buf())
            .unwrap()
            .is_none());
        // enabled 但无 key 且不允许免 key → 不注册
        let mut c = cfg("http://localhost:9/v1");
        c.api_key = String::new();
        assert!(ImageGenTool::build(&c, false, ws.path().to_path_buf())
            .unwrap()
            .is_none());
        // 本地 sd-server：无 key 也注册（allow_no_key）
        assert!(ImageGenTool::build(&c, true, ws.path().to_path_buf())
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn generate_saves_b64_response() {
        let ws = tempdir().unwrap();
        let body = format!("{{\"data\":[{{\"b64_json\":\"{}\"}}]}}", fake_png_b64());
        let (base, _captured) = spawn_mock(move |_| vec![("200 OK".into(), body)]);
        let tool = ImageGenTool::build(&cfg(&base), false, ws.path().to_path_buf())
            .unwrap()
            .unwrap();
        let out = tool
            .execute(&json!({"prompt": "a cat"}), "cli")
            .await
            .unwrap();
        let path = out.strip_prefix("image generated: ").unwrap();
        let bytes = tokio::fs::read(path).await.unwrap();
        assert_eq!(bytes, b"fake-png-bytes");
        assert!(path.contains("images"));
    }

    #[tokio::test]
    async fn generate_downloads_url_response() {
        let ws = tempdir().unwrap();
        // 第二个响应：mock 对非 /images 路径原样回 body（客户端只看 200 + 字节）
        let (base, _captured) = spawn_mock(|base| {
            vec![
                (
                    "200 OK".into(),
                    format!("{{\"data\":[{{\"url\":\"{base}/download/img.png\"}}]}}"),
                ),
                ("200 OK".into(), "fake-png-bytes".into()),
            ]
        });
        let tool = ImageGenTool::build(&cfg(&base), false, ws.path().to_path_buf())
            .unwrap()
            .unwrap();
        let out = tool
            .execute(&json!({"prompt": "a cat"}), "cli")
            .await
            .unwrap();
        let path = out.strip_prefix("image generated: ").unwrap();
        assert_eq!(tokio::fs::read(path).await.unwrap(), b"fake-png-bytes");
    }

    #[tokio::test]
    async fn generate_error_status_is_reported() {
        let ws = tempdir().unwrap();
        let (base, _captured) = spawn_mock(|_| {
            vec![(
                "500 Internal Server Error".into(),
                "{\"error\":{\"message\":\"boom\"}}".into(),
            )]
        });
        let tool = ImageGenTool::build(&cfg(&base), false, ws.path().to_path_buf())
            .unwrap()
            .unwrap();
        let err = tool
            .execute(&json!({"prompt": "a cat"}), "cli")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("HTTP 500"));
    }

    #[tokio::test]
    async fn execute_requires_prompt() {
        let tool = ImageGenTool::build(&cfg("http://localhost:9/v1"), false, PathBuf::from("."))
            .unwrap()
            .unwrap();
        let err = tool.execute(&json!({}), "cli").await.unwrap_err();
        assert!(err.to_string().contains("missing 'prompt'"));
    }

    #[tokio::test]
    async fn edit_uploads_multipart_and_saves_result() {
        let ws = tempdir().unwrap();
        std::fs::write(ws.path().join("src.png"), b"source-image").unwrap();
        let body = format!("{{\"data\":[{{\"b64_json\":\"{}\"}}]}}", fake_png_b64());
        let (base, captured) = spawn_mock(move |_| vec![("200 OK".into(), body)]);
        let root = Arc::new(RwLock::new(ws.path().to_path_buf()));
        let trusted: Arc<RwLock<Vec<PathBuf>>> = Arc::new(RwLock::new(Vec::new()));
        let tool = ImageEditTool::build(&cfg(&base), false, ws.path().to_path_buf(), root, trusted)
            .unwrap()
            .unwrap();
        let out = tool
            .execute(
                &json!({"image": "src.png", "prompt": "make it blue"}),
                "cli",
            )
            .await
            .unwrap();
        assert!(out.starts_with("image edited: "));

        let req = captured.lock().unwrap().join("\n---\n");
        assert!(req.contains("multipart/form-data"));
        assert!(req.contains("name=\"image\""));
        assert!(req.contains("name=\"prompt\""));
        assert!(req.contains("make it blue"));
        assert!(req.contains("name=\"model\"") && req.contains("sd-test"));
        assert!(req.contains("name=\"size\"") && req.contains("512x512"));
        // 结果落盘
        let path = out.strip_prefix("image edited: ").unwrap();
        assert_eq!(tokio::fs::read(path).await.unwrap(), b"fake-png-bytes");
    }

    #[tokio::test]
    async fn edit_rejects_out_of_scope_and_non_image() {
        let ws = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(outside.path().join("stranger.png"), b"x").unwrap();
        std::fs::write(ws.path().join("doc.txt"), b"not an image").unwrap();
        let (base, _captured) = spawn_mock(|_| vec![]);
        let root = Arc::new(RwLock::new(ws.path().to_path_buf()));
        let trusted: Arc<RwLock<Vec<PathBuf>>> = Arc::new(RwLock::new(Vec::new()));
        let tool = ImageEditTool::build(&cfg(&base), false, ws.path().to_path_buf(), root, trusted)
            .unwrap()
            .unwrap();

        // 作用域外 → 拒绝
        let err = tool
            .execute(
                &json!({
                    "image": outside.path().join("stranger.png").to_string_lossy(),
                    "prompt": "edit"
                }),
                "cli",
            )
            .await
            .unwrap_err();
        // 作用域校验失败（路径守卫），绝不能打到端点
        assert!(!err.to_string().contains("image_edit request failed"));

        // 作用域内但非图片扩展名 → 拒绝
        let err = tool
            .execute(&json!({"image": "doc.txt", "prompt": "edit"}), "cli")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not an image"));
    }

    #[test]
    fn schemas_expose_expected_fields() {
        let c = cfg("http://localhost:9/v1");
        let gen = ImageGenTool::build(&c, false, PathBuf::from("."))
            .unwrap()
            .unwrap();
        assert_eq!(gen.name(), "image_gen");
        assert!(gen.parameters_schema()["properties"]["prompt"].is_object());
        let edit = ImageEditTool::build(
            &c,
            false,
            PathBuf::from("."),
            Arc::new(RwLock::new(PathBuf::from("."))),
            Arc::new(RwLock::new(Vec::new())),
        )
        .unwrap()
        .unwrap();
        assert_eq!(edit.name(), "image_edit");
        let schema = edit.parameters_schema();
        assert_eq!(schema["required"][0], "image");
        assert_eq!(schema["required"][1], "prompt");
        assert!(schema["properties"]["mask"].is_object());
    }
}
