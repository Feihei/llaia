# 多媒体收发设计

**日期**：2026-07-24
**范围**：P2-e 子项目 A（图片/文件收发）

## 目标

让 Agent 能接收用户发送的图片和文件，能主动发送图片和文件给用户。

## 范围

| 场景 | 描述 |
|---|---|
| A. 收图给 Agent 看 | 用户发图 → 下载/缩放 → 作为多模态消息送入主 provider（vision） |
| B. 收文件给 Agent 读 | 用户发文件 → 下载到 workspace → Agent 用 file_read 读取 |
| C. Agent 主动发图 | Agent 调 send_image 工具发图 |
| D. Agent 主动发文件 | Agent 调 send_file 工具发文件 |

视频不在范围内。

## 关键决策

### 1. Provider 接口：MessageContent 枚举

`ChatMessage.content` 从 `String` 改为枚举：

```rust
pub enum MessageContent {
    Text(String),
    Multimodal(Vec<ContentPart>),
}

pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrlContent },
}

pub struct ImageUrlContent {
    pub url: String,  // data:image/jpeg;base64,... 或 http(s) URL
}
```

- 纯文本序列化为 `"content": "hello"`（向后兼容）
- 多模态序列化为 `"content": [{"type":"text",...}, {"type":"image_url",...}]`
- 构造函数 `user()`/`assistant()` 等返回 `Text` 变体，向后兼容

### 2. 图片编码：base64 data URL

图片读为 base64，组成 `data:image/jpeg;base64,...` 送入 OpenAI content 数组。通用，不依赖模型端可达 URL。

### 3. 图片自动缩放

依赖 `image` crate。超过 1024×1024 等比缩放到 1024 内。JPEG 重新编码质量 85。

helper 函数：`prepare_image_for_vision(path) -> Result<(path, base64_data_url)>`

### 4. 媒体存储

用户发来的媒体下载到 `<workspace>/uploads/<msgid>_<filename>`。不分日期目录。文件名带 msg_id 避免同名冲突。

### 5. 上下文压缩：图片降级

`Context::compact()` 调用 LLM 之前，遍历 history 把 `Multimodal` 变体的图片 part 替换为 `Text("[图片: /uploads/xxx.jpg]")`，保留 text part 不变。降级为 `Text` 变体。

### 6. 发送工具：两个独立工具

- `send_image(path)`：发图片
- `send_file(path)`：发文件

参数为 workspace 内路径（与 file_read 一致的边界检查）。

### 7. QQ 媒体 API

- **接收**：C2C 消息附件（`attachments` 字段）含 `content_type` 和 `url`，下载到 uploads/
- **发送**：先 POST `/v2/users/{openid}/files` 上传媒体拿 `file_info`，再 POST `/v2/users/{openid}/messages` 发 `msg_type:7` 富媒体消息

### 8. CLI 图片接收

终端无原生图片拖拽支持。用 `@/path/to/image.jpg` 语法：
- 用户输入 `@/path/to/image.jpg 描述` → 解析 `@path` 提取图片路径，剩余作为文本
- 多张图片：`@/path1.jpg @/path2.jpg 描述`
- 路径需在 workspace 内（resolve_within 边界检查）
- CLI 发送图片/文件：打印本地路径（终端原生不支持图片显示）

### 9. Vision 触发方式

用户发图时自动作为多模态消息传给主 provider，文本描述同步发送。Agent 无需调工具即可看图。不引入独立的 caption provider（未来如需可扩展）。

### 10. 大小限制

使用 QQ 官方限制：图片 10MB / 文件 100MB。超过拒绝并提示。

## 架构

```
用户输入（文本+图片/文件）
  ↓
Channel 层（CLI/QQ）解析媒体
  ↓
下载/校验/缩放图片 → uploads/
  ↓
构造 MessageContent::Multimodal 给 Agent
  ↓
Agent 调 Provider（自动序列化为 OpenAI 多模态格式）
  ↓
上下文压缩时图片降级为文字占位
  ↓
Agent 调 send_image/send_file 工具 → Channel 层上传+发送
```

## 影响范围

| 文件 | 改动 |
|---|---|
| `Cargo.toml` | 加 `image` crate |
| `src/provider/mod.rs` | `MessageContent` 枚举 + `ChatMessage` 字段类型变更 |
| `src/provider/openai_compat.rs` | 序列化适配（content 字符串/数组分发） |
| `src/agent/mod.rs` | 新增 `handle_input_multimodal` 或改造 `handle_input_streaming` |
| `src/agent/context.rs` | `compact()` 图片降级 |
| `src/channels/cli.rs` | `@path` 解析 + 多模态消息构造 |
| `src/channels/qq.rs` | 附件下载 + 媒体发送 API |
| `src/tools/mod.rs` | 新增 `send_image` / `send_file` 模块 |
| `src/tools/send_media.rs` | 新文件：发送工具实现 |
| `src/image_utils.rs` | 新文件：图片缩放 + base64 编码 |
| `src/config.rs` | 可选：`[channels.qq] max_upload_size` |

## 测试策略

- 单元测试：`MessageContent` 序列化（纯文本/多模态）、图片缩放、路径边界检查
- 集成测试：CLI `@path` 解析、QQ 附件下载 mock、send_image 工具调用
- 手动测试：QQ 发图给 Agent、Agent 调 send_image 发图、CLI `@path` 发图
