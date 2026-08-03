# WebUI Channel 与配置可视化设计

**日期：** 2026-08-03
**状态：** 设计已确认，待实现
**关联：** 承接 P1.5 channel sink 抽象重构（`2026-08-01-channel-sink-abstraction-design.md`）

---

## 1. 背景与目标

P1.5 完成了 `OutputSink` trait + `run_turn` 抽象，使新增 channel 的边际成本大幅降低。本阶段（P2-b）开发 WebUI channel，承担两个职责：

1. **Chat channel**：浏览器作为对话入口，与 CLI / QQ 对称
2. **配置可视化**：交互式编辑 provider / agent / channels / tools 等配置，并支持直接编辑 `config.toml` 文本

参考 AstrBot dashboard 的配置可视化形态，但前端栈选用 Alpine.js（零 node 构建）而非 Vue，与项目"轻量、单二进制"定位一致。

### 目标

- 新增 `WebChannel`，复用 `run_turn`，与 CLI/QQ 行为对称
- 浏览器可对话、上传图片、查看 agent `send_image` 产物、中止 turn
- 提供 REST API 读写结构化配置与原始 TOML 文本
- 前端单页应用，顶部 tab 切换 Chat / 配置 / 关于
- 配置修改写盘后提示重启生效（不热重载）
- MCP / Skills 配置页面占位（后端未实现，UI 标"开发中"）

### 非目标

- 多用户 / RBAC
- 历史 session 切换 UI（sqlite 已存，Web 暂不暴露 `/sessions` 列表）
- 主题切换、暗色模式
- 工具调用确认弹窗（web 暂不接 confirm_mode，所有工具直接执行，与 CLI 行为一致）
- 流式 token 速度调节
- 移动端单独适配（响应式 CSS 足够）
- 配置热重载
- 自动化 e2e 测试

---

## 2. 整体架构

```
                       ┌──────────────────────────────────────────┐
                       │              llaia serve                 │
                       │  (commands::serve_cmd)                   │
                       └──┬───────────────────────────────┬───────┘
                          │                               │
              ┌───────────▼──────────┐         ┌──────────▼──────────┐
              │     QqChannel        │         │   WebChannel        │
              │  (无改动)            │         │   (新增)            │
              └──────────────────────┘         └──────────┬──────────┘
                                                          │
                                              ┌───────────▼───────────┐
                                              │  HTTP server (axum)   │
                                              │                       │
                                              │  静态资源路由：        │
                                              │   GET /               │
                                              │   GET /static/*       │
                                              │                       │
                                              │  Chat 路由：           │
                                              │   GET /ws (upgrade)   │
                                              │   POST /upload        │
                                              │   GET /file?path=     │
                                              │                       │
                                              │  Config 路由：         │
                                              │   GET  /api/config    │
                                              │   PUT  /api/config    │
                                              │   GET  /api/config/raw│
                                              │   PUT  /api/config/raw│
                                              │   POST /api/config/validate
                                              │   GET  /api/status    │
                                              └───────────┬───────────┘
                                                          │
                                              ┌───────────▼───────────┐
                                              │  per-连接 state       │
                                              │  - WebSink            │
                                              │  - stop: Notify       │
                                              │  - workspace ref      │
                                              └───────────┬───────────┘
                                                          │
                                              ┌───────────▼───────────┐
                                              │  Arc<AgentRegistry>   │
                                              │  + Arc<RwLock<Config>>│
                                              └──────────────────────┘
```

### 模块分层

`src/channels/web.rs`（按 `qq.rs` 对称结构组织）：

1. `WebChannel` — 持有 `WebConfig`、`Arc<AgentRegistry>`、`Arc<RwLock<Config>>`、静态资源（`rust-embed` 嵌入）、上传/媒体根目录；实现 `Channel` trait
2. `WebServer` — axum router 构建 + 监听
3. `WsHandler` — 单个 WS 连接生命周期（chat 用）
4. `WebSink` — `OutputSink` 实现，持有 `mpsc::Sender<WebEvent>` 把回调事件转给 WS 写 task
5. `WebEvent` — 扁平化 JSON 事件枚举（与 `TurnEvent` 一一对应但面向前端）
6. `config_api` 模块 — `/api/config` 系列路由 handler 集合
7. `about_api` — `/api/status` handler

### 关键约束

- 配置 API 鉴权与 chat 路由共用同一 token 中间件
- `Arc<RwLock<Config>>` 启动时构建一次；保存成功后更新内存副本（实际生效要重启）
- 保存路径 = 启动时加载的 `config_dir/config.toml`
- WS 写 task 与 `WebSink` 间用 `mpsc<WebEvent>` 解耦，使 sink 不阻塞在 socket 写上

### 依赖增量

- `axum` 0.7（含 `ws` feature）
- `tower-http`（cors / fs，备用）
- `rust-embed`（嵌入静态资源）
- `mime_guess`（推断静态资源 Content-Type）
- `rand`（token 生成，可能已在依赖中）

---

## 3. WebSocket 消息协议

### 客户端 → 服务端（入向）

```jsonc
// 发送对话消息（文本 + 可选图片）
{
  "type": "chat",
  "text": "帮我画个流程图",
  "images": ["uploads/abc123.png"]   // 相对 uploads dir 的路径，POST /upload 返回
}

// 中止当前正在执行的 turn
{ "type": "stop" }

// 心跳（可选，浏览器 30s 一次，后端 60s 无消息视为死连接）
{ "type": "ping" }
```

### 服务端 → 客户端（出向）

与 `TurnEvent` 一一对应，但扁平化为 `{type, ...}` 形式：

```jsonc
{ "type": "chunk", "delta": "hello" }
{ "type": "tool_start", "id": "call_1", "name": "terminal" }
{ "type": "tool_result", "id": "call_1", "output": "..." }
{ "type": "media", "path": "out/diagram.png", "kind": "image" }
{ "type": "done" }
{ "type": "error", "message": "..." }
{ "type": "interrupted" }

// 协议层（非 TurnEvent 映射）
{ "type": "pong" }
{ "type": "auth_ok" }
{ "type": "auth_failed", "reason": "invalid token" }
{ "type": "busy", "reason": "another turn running" }
```

### 关键约束

1. **单连接串行 turn**：一个 WS 连接同时只允许一个 turn 执行。新 `chat` 在已有 turn 时返回 `busy`，客户端禁用发送按钮直到 `done`/`error`/`interrupted`。与 CLI queued_inputs / QQ per-user `running_stops` 行为一致，避免 main agent 锁竞争。

2. **图片路径安全**：`chat.images` 字段服务端必须校验路径在 `uploads_dir` 内（canonicalize 比较），防止 `../../etc/passwd` 逃逸。

3. **媒体路径暴露**：`media` 事件的 `path` 是 agent workspace 相对路径。前端展示图片时拼成 `GET /file?path=<urlencoded>`，服务端再走 workspace 边界校验返回文件流。**不直接暴露绝对路径给前端**。

4. **连接断开 = 中止**：WS 断开等同 CLI Ctrl+C / QQ `/stop`，`stop.notify_one()` 触发 `run_turn` 走 `on_interrupted` 路径。

### 与 sink 抽象的对齐

`WebSink` 持有 `mpsc::Sender<WebEvent>`，每个 `on_*` 方法把数据封装成 `WebEvent` 发到 channel；`run_turn` 完全不感知 WS 协议细节。

---

## 4. 鉴权与配置

### 配置 schema 扩展

`config.toml` 增加 `[channels.web]` 段：

```toml
[channels.web]
enabled = true
# 监听地址：LAN 多设备场景 0.0.0.0；本机专用 127.0.0.1
bind = "0.0.0.0:8080"
# 静态 token，浏览器首次访问输入后存 localStorage
# 留空则启动时随机生成并打印到日志（首次复制即可）
token = ""
```

`WebConfig` 结构（与 `QqConfig` 同级）：

```rust
pub struct WebConfig {
    pub enabled: bool,
    pub bind: String,      // "ip:port"
    pub token: String,     // 空则启动随机生成
}
```

`Config::default_for_workspace` 给出 `bind = "127.0.0.1:8080"`、`token = ""` 的默认值。

### 鉴权流程

**HTTP 路由鉴权**（`/`、`/static/*`、`/upload`、`/file`、`/api/*`）：
- 优先从 cookie `llaia_token` 读
- 否则从 `Authorization: Bearer <token>` 读
- 都没有则返回 401 + `WWW-Authenticate: Bearer`，前端拦截 401 弹出 token 输入框

**WS upgrade 鉴权**（`GET /ws`）：
- 从 query `?token=<token>` 读（浏览器无法在 WS 握手时设自定义 header）
- 校验失败 → 关闭连接（close code 4001）+ 发送 `auth_failed` 帧
- 校验通过 → 发送 `auth_ok` 帧，进入消息循环

**Token 生成策略**：
- 配置留空时，启动时用 `rand` 生成 32 字节随机串，hex 编码
- 启动日志打印一行 `WebUI token: <hex>`（与 QQ app_secret 同等信任级别）
- 不持久化到磁盘 —— 重启换新 token，浏览器需重输（LAN 多设备场景可接受）

**CORS**：不开。token 校验已足够防止跨 origin 误访问。

### 与现有 PID 文件机制的关系

`llaia serve` 已有 PID 文件保护，Web channel 复用即可。同时跑 `llaia chat` 和 `llaia serve` 两个进程都持 main agent 是 P2-d 已确定的行为，Web 不改变。

---

## 5. 多媒体（图片收发）

### 入向：浏览器上传图片 → agent vision 输入

1. 浏览器 `POST /upload`（multipart/form-data，字段名 `file`）+ token 鉴权
2. 服务端保存到 `<workspace>/uploads/<uuid>_<filename>`（与 QQ 附件下载对称）
3. 返回 JSON：`{ "path": "uploads/abc123_xyz.png", "size": 12345 }`（相对 workspace）
4. 浏览器在后续 `chat` 消息的 `images` 数组里带上这个相对路径
5. WS handler 收到 `chat` 后：
   - 校验每个 image path 在 `uploads_dir` 内（canonicalize 比较）
   - 读图片 → `prepare_image_for_vision` 转 data URL（复用 `image_utils`）
   - 构造 `ChatMessage::user_multimodal(parts)`，parts 含 `ContentPart::Text` + `ContentPart::ImageUrl`
   - 调 `run_turn`
6. 上传后未在 `chat` 中使用的图片：不清理（与 QQ 一致）

**限制**：
- 单文件大小限制 20MB
- 仅接受 `image/*` MIME 类型，非图片返回 400
- 上传目录：复用 `agent.workspace.join("uploads")`，与 QQ 附件下载共享同一路径

### 出向：agent 调 send_image/send_file 工具 → 浏览器显示

1. agent 调用 `send_image` / `send_file` 工具（已存在，无改动）
2. 工具 `execute_with_events` 触发 `TurnEvent::MediaOutput { path, kind }`
3. `WebSink::on_media(path, kind)` 转 `WebEvent::Media { path, kind }` 推给浏览器
4. 浏览器收到后：
   - `kind == Image` → 渲染 `<img src="/file?path=<urlencoded>">`
   - `kind == File` → 显示下载链接 `<a href="/file?path=<urlencoded>" download>`
5. `GET /file` 路由：token 鉴权 + workspace 边界校验（canonicalize 必须在 workspace 内）+ 返回文件流

**关键约束**：
- 媒体路径统一用相对 workspace 的形式，前端永远不拿到绝对路径
- `GET /file` 严格校验路径不能含 `..` 逃逸，canonicalize 后必须以 workspace 为前缀
- `Content-Type` 用 `mime_guess::from_path` 推断，失败回退 `application/octet-stream`

### 与其他 channel 的一致性

| 能力 | CLI | QQ | Web |
|------|-----|----|----|
| 上传图片到 agent | `@path` 引用本地文件 | WS 附件自动下载 | `POST /upload` + `chat.images` |
| send_image 工具产物 | 打印路径 | QQ 富媒体消息推送 | `/file` 路由下载 |
| 上传目录 | N/A（直接读本地） | `<workspace>/uploads/` | `<workspace>/uploads/`（共享） |

---

## 6. 中止、错误处理与生命周期

### 中止机制

| 触发源 | 信号路径 | 行为 |
|--------|---------|------|
| 浏览器点"停止"按钮 | WS 收 `{type:"stop"}` → `stop.notify_one()` | `run_turn` 走 `on_interrupted` |
| 浏览器关闭 tab | WS 读返回 `None` → 检测到 → `stop.notify_one()` | 同上，agent task 检测 tx closed 保存部分输出 |
| 服务端 shutdown | `WebChannel::run` 收到 cancel 信号 → 关闭 listener + 通知所有 stop | 等同中断所有活跃 turn |

`WebSink::on_interrupted` 只 log，不回推任何 WS 帧（与 QqSink 对称 —— 中断的"已中断"反馈由前端按钮状态本身体现）。

### 错误处理分层

```
Layer 1: HTTP/WS 协议错误
- 鉴权失败、路径越界、上传超限、JSON 解析失败
- 行为：返回 HTTP 4xx 或 WS close，不进入 run_turn

Layer 2: turn 执行错误（sink 回调路径）
- agent task panic、provider 网络失败、内部错误
- run_turn 走 on_error(message) 回调
- WebSink 转 WebEvent::Error 推给浏览器
- turn 结束后 WS 连接保持，可继续发新 chat

Layer 3: WS 写失败 / 连接断开
- mpsc::Sender::send 返回 Err（receiver dropped）
- WebSink 各方法忽略 send 错误（let _ = ...）
- WS 写 task 退出，WS handler 检测到后清理
```

**关键决策**：
- `WebSink` 的 `on_chunk` 等方法遇到 mpsc send 失败时**不 panic**也不向上传递 —— 浏览器可能已断开，agent 继续跑完保存到 sqlite 是合理的（与 CLI/QQ 一致：sink 失败不影响 agent 持久化）
- `run_turn` 本身的 `Result<()>` 在 WS handler 里 `.ok()` 忽略，错误已通过 sink 传达给前端

### WS 连接生命周期

```rust
// 伪代码
async fn ws_handler(ws, agent, config) {
    let (ws_sink, mut ws_stream) = ws.split();
    let (tx, rx) = mpsc::channel::<WebEvent>(64);

    // 写 task：rx → ws_sink
    let write_task = tokio::spawn(async move {
        let mut rx = rx;
        while let Some(ev) = rx.recv().await {
            let json = serde_json::to_string(&ev).unwrap();
            if ws_sink.send(Message::Text(json)).await.is_err() {
                break;
            }
        }
    });

    let stop = Arc::new(Notify::new());
    let mut current_turn: Option<JoinHandle<()>> = None;

    loop {
        tokio::select! {
            msg = ws_stream.next() => match msg {
                Some(Ok(Text(s))) => {
                    // 解析 + 分发：chat / stop / ping
                    // chat 时 spawn run_turn，current_turn = Some(handle)
                    // 收到 done/error/interrupted 后 current_turn = None
                    // （通过 WebSink 持有的 turn-end mpsc 信号回传）
                }
                Some(Ok(Close(_))) | None => break,
                _ => {}
            },
            _ = stop.notified() => {
                // run_turn 自己会结束，这里不主动 break
            }
        }
    }

    // 清理：若 turn 还在跑，通知中止并等待
    if let Some(h) = current_turn.take() {
        stop.notify_one();
        let _ = h.await;
    }
    // drop tx 让 write_task 退出
    drop(tx);
    let _ = write_task.await;
}
```

**注意点**：
- "turn 结束"信号回传：`WebSink` 持有 `mpsc::Sender<TurnEndSignal>`，`on_done`/`on_error`/`on_interrupted` 三个终态方法都发一个信号，主循环 select! 接收后清理 `current_turn`

### WebChannel::run 集成到 serve

`commands::serve_cmd` 改造（与 QQ 完全对称）：

```rust
if config.channels.web.enabled {
    let web = Arc::new(WebChannel::new(config.channels.web.clone()));
    let registry = registry.clone();
    tasks.push(tokio::spawn(async move {
        if let Err(e) = Channel::run(web, registry).await {
            tracing::error!(error = %e, "WebChannel exited with error");
        }
    }));
    tracing::info!("WebChannel started on {}", config.channels.web.bind);
}
```

---

## 7. 前端结构

### 文件组织

```
src/channels/web/static/
├── index.html          # 单页结构 + 内联基础 CSS + Alpine 组件
├── app.js              # 主逻辑（WS 客户端、消息渲染、状态管理、tab 切换）
├── chat.js             # Chat tab 逻辑
├── config.js           # 配置 tab 逻辑
├── about.js            # 关于 tab 逻辑
└── vendor/
    ├── codemirror/     # CodeMirror 6 + lang-toml（本地 vendor）
    ├── marked.min.js   # Markdown 渲染
    └── highlight.min.js # 代码高亮
```

通过 `rust-embed` 嵌入到二进制：

```rust
#[derive(rust_embed::RustEmbed)]
#[folder = "src/channels/web/static/"]
struct StaticAsset;
```

### 前端 UI 结构（顶部 tab）

```
┌────────────────────────────────────────────────────────────┐
│ Llaia Web          [Chat] [配置] [关于]      [Token: •••]  │
├────────────────────────────────────────────────────────────┤
│ tab 内容区                                                 │
└────────────────────────────────────────────────────────────┘
```

- Markdown 渲染：`marked.js`（约 30KB，纯 JS 无依赖）
- 代码高亮：`highlight.js`（按需加载常用语言包）
- 状态：Alpine.js 响应式对象 + DOM 操作，不引入框架
- Token 存储：`localStorage.setItem("llaia_token", ...)`
- 重连：WS 断开后 3 秒自动重连（最多 5 次）

### Chat tab

```
┌─────────────────────────────────────────────┐
│                                  [停止]     │
├─────────────────────────────────────────────┤
│  消息流（user/assistant/tool 交替）         │
│                                             │
│  [user] 帮我画个流程图                      │
│  [assistant] 好的...                        │
│  [tool_start] terminal                      │
│  [tool_result] ...                          │
│  [assistant] <markdown 渲染>                │
│  [media:image] <img>                        │
├─────────────────────────────────────────────┤
│ [图片+] [输入框...........................] │
│ [已上传: img1.png img2.png]                 │
└─────────────────────────────────────────────┘
```

---

## 8. 配置可视化

### 页面结构（配置 tab）

```
┌────────────────────────────────────────────────────────────┐
│ 配置 tab（左侧分类导航 + 右侧表单）                        │
│                                                            │
│ ┌──────────────┐ ┌──────────────────────────────────────┐  │
│ │ 运行时参数   │ │ Provider 列表                         │  │
│ │ 日志         │ │ ┌──────────────────────────────────┐ │  │
│ │ Provider ●   │ │ │ default  [展开▼]  [删除]         │ │  │
│ │ Agent        │ │ │   type: openai_compatible        │ │  │
│ │  └ main      │ │ │   base_url: http://...           │ │  │
│ │  └ coder      │ │ │   api_key: ••••                 │ │  │
│ │ Channels     │ │ │   Models:                        │ │  │
│ │  └ CLI       │ │ │     qwen3 [删除]  model: qwen-... │ │  │
│ │  └ QQ        │ │ │     [+ 添加模型]                 │ │  │
│ │  └ Web       │ │ │   [+ 添加 Provider]              │ │  │
│ │ Tools        │ │ └──────────────────────────────────┘ │  │
│ │ MCP (开发中) │ │                                      │  │
│ │ Skills(开发中)│ │ Agent 列表...                       │  │
│ │              │ │                                      │  │
│ │              │ │ ─── 原始 TOML 编辑器 (CodeMirror) ── │  │
│ │              │ │ ┌──────────────────────────────────┐ │  │
│ │              │ │ │ [runtime]                        │ │  │
│ │              │ │ │ context_threshold = 0.7          │ │  │
│ │              │ │ └──────────────────────────────────┘ │  │
│ │ [保存]        │ │ [校验] [保存]                        │  │
│ └──────────────┘ └──────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────┘
```

### 配置分类与表单字段

| 分类 | 字段 | 类型 | 说明 |
|------|------|------|------|
| **运行时** | `context_threshold` | number (0.0-1.0) | 上下文压缩阈值 |
| | `max_iterations` | number (≥1) | 工具调用迭代上限 |
| **日志** | `level` | select | debug/info/warn/error |
| | `dir` | string | 日志目录 |
| **Provider**（动态数组） | `type` | select | openai_compatible（当前唯一） |
| | `base_url` | string | 端点 URL |
| | `api_key` | password | 密码字段，支持 `${VAR}` |
| | `model`（子数组） | | alias → {model, native_tool_calling, context_size} |
| **Agent**（动态数组） | `model` | select | 从已配置 provider.model 派生选项 |
| | `workspace` | string | 工作目录 |
| | `soul`/`user`/`memory` | string? | 可选 md 路径，缺省推导 |
| | `denied_tools` | multiselect | 从已知工具名派生 |
| | `delegate_timeout` | number | 子 Agent 委派超时 |
| **Channels** | CLI: `enabled` | toggle | |
| | QQ: `enabled`/`app_id`/`app_secret`/`confirm_mode` | | |
| | Web: `enabled`/`bind`/`token` | | 自我配置（修改后需重启） |
| **Tools** | terminal: `confirm`/`whitelist` | | |
| | tavily: `api_key` | password | |
| **MCP** | 占位 | — | "开发中"提示 |
| **Skills** | 占位 | — | "开发中"提示 |

### 表单 ↔ TOML 双向同步策略

**核心：单一数据源 = TOML 文本**。表单和 TOML 编辑器都从同一份 `Config` 派生。

1. **进入配置 tab**：`GET /api/config` 返回结构化 JSON，Alpine 渲染表单；`GET /api/config/raw` 返回 TOML 文本，CodeMirror 渲染
2. **表单修改**：Alpine 双向绑定修改 JS 内存中的 Config 对象；用户点"保存"时 → `PUT /api/config`（结构化）→ 后端 `toml::to_string` + 写盘 + 回写 CodeMirror
3. **TOML 修改**：用户在 CodeMirror 编辑 → 点"校验" → `POST /api/config/validate`（带 toml 文本）→ 后端 `toml::from_str` 返回错误或 OK → 点"保存" → `PUT /api/config/raw` → 后端解析 + 写盘 + 回写表单
4. **保存后**：后端 `Arc<RwLock<Config>>` 更新内存副本，前端弹出"已保存，重启 llaia 生效"提示

**冲突处理**：表单和 TOML 同时编辑时，保存其中一个会覆盖另一个。前端在保存成功后用返回的最新数据重置另一边。不做"未保存改动"提示（YAGNI）。

### 鉴权与安全

- 所有 `/api/*` 路由共用 token 中间件
- `api_key`/`app_secret`/`token` 等敏感字段：`GET /api/config` 返回时**用掩码 `••••` 替代**，避免浏览器暴露；保存时若字段仍是 `••••` 则保留原值不覆盖
- 环境变量引用 `${VAR}`：表单中作为普通字符串处理，保存时原样写盘（不展开）

---

## 9. 关于页面与状态 API

### 关于页面内容

```
┌────────────────────────────────────────────────────────────┐
│ 关于                                                       │
├────────────────────────────────────────────────────────────┤
│ Llaia v0.x.x                                               │
│ Build: <git hash>                                          │
│ Workspace: /path/to/workspace                              │
│ Config: /path/to/config.toml                               │
│ PID: 12345                                                 │
│                                                            │
│ 运行中的 channels:                                         │
│   ✓ CLI (enabled)                                          │
│   ✓ QQ  (enabled)                                          │
│   ✓ Web  (enabled, listening 0.0.0.0:8080)                 │
│                                                            │
│ 数据目录:                                                  │
│   sessions.db: 12 MB                                       │
│   logs/: /var/log/llaia/                                   │
│   uploads/: 23 files                                       │
│                                                            │
│ 链接:                                                      │
│   - GitHub repo                                            │
│   - 文档                                                    │
└────────────────────────────────────────────────────────────┘
```

`GET /api/status` 返回：

```jsonc
{
  "version": "0.x.x",
  "build_hash": "abc1234",
  "workspace": "/path/to/workspace",
  "config_path": "/path/to/config.toml",
  "pid": 12345,
  "channels": [
    {"name": "cli", "enabled": true, "listening": null},
    {"name": "qq",  "enabled": true, "listening": null},
    {"name": "web", "enabled": true, "listening": "0.0.0.0:8080"}
  ],
  "db_size_bytes": 12582912,
  "log_dir": "/var/log/llaia",
  "uploads_count": 23
}
```

### 版本与构建信息

通过 `env!("CARGO_PKG_VERSION")` 取版本，`env!("GIT_HASH")`（build.rs 注入，缺省 "unknown"）取构建 hash。

新增 `build.rs`：

```rust
fn main() {
    println!("cargo:rustc-env=GIT_HASH={}", git_hash());
}

fn git_hash() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
```

### 统一错误响应格式

所有 `/api/*` 路由统一 JSON 错误：

```jsonc
{ "error": "invalid token" }          // 401
{ "error": "invalid toml: line 5 ..." } // 400
{ "error": "internal error" }          // 500
```

---

## 10. 影响范围

| 文件 | 改动类型 |
|------|---------|
| `Cargo.toml` | 新增 axum / tower-http / rust-embed / mime_guess 依赖（rand 若已有则不增） |
| `build.rs` | 新建：注入 GIT_HASH 环境变量 |
| `src/config.rs` | 新增 `WebConfig` 结构 + `Config::channels.web` 字段 + `default_for_workspace` 默认值 |
| `src/channels/mod.rs` | `pub mod web;` |
| `src/channels/web.rs` | 新建：WebChannel + WebSink + WebEvent + WsHandler + config_api + about_api + routes |
| `src/channels/web/static/*` | 新建：前端静态资源（index.html / app.js / chat.js / config.js / about.js / vendor/*） |
| `src/commands/mod.rs` | `serve_cmd` 增加 web channel 启动分支 |

---

## 11. 测试策略

### 单元测试（Rust 侧）

1. **`WebSink` 行为测试**（与 sink.rs 现有 `MockSink` 测试对称）：
   - `test_web_sink_chunk_to_event`：`on_chunk("hi")` → `WebEvent::Chunk { delta: "hi" }` 进 mpsc
   - `test_web_sink_terminal_events`：`on_done`/`on_error`/`on_interrupted` 各发对应 `WebEvent` + turn-end signal
   - `test_web_sink_send_failure_ignored`：drop receiver 后 `on_chunk` 不 panic

2. **路径安全测试**：
   - `test_resolve_upload_rejects_traversal`：`../../etc/passwd` 拒绝
   - `test_resolve_upload_within_workspace`：`uploads/abc.png` 通过
   - `test_file_route_rejects_absolute`：`/etc/passwd`、`C:\Windows\...` 拒绝

3. **协议测试**：
   - `test_chat_message_with_images`：`{type:"chat", text, images}` 正确构造 `ChatMessage::user_multimodal`
   - `test_web_event_json_serialization`：`WebEvent` 序列化结果符合协议规范

4. **鉴权测试**：
   - `test_ws_rejects_invalid_token`：错 token → close code 4001
   - `test_http_returns_401_without_token`：无 token → 401

5. **复用 `run_turn` 集成测试**：用 `MockProvider` 跑完整 turn，断言 `WebSink` 发出的事件序列正确

6. **配置 API 测试**：
   - `test_get_config_returns_structured`：`GET /api/config` 返回完整结构
   - `test_put_config_writes_to_disk`：临时 config_dir，写入后读回验证
   - `test_validate_config_valid_toml`：合法 TOML 返回 OK
   - `test_validate_config_invalid_toml`：非法 TOML 返回错误行号
   - `test_get_config_masks_sensitive_fields`：`api_key` 字段为 `••••`
   - `test_put_config_preserves_masked_secrets`：保存时 `••••` 字段保留原值
   - `test_environment_variable_reference_preserved`：`${VAR}` 写入后原样在盘上
   - `test_save_path_within_config_dir`：保存路径不能逃逸 config_dir

7. **状态 API 测试**：
   - `test_get_status_returns_complete_fields`：关键字段非空
   - `test_api_routes_require_token`：所有 `/api/*` 路由无 token 返回 401

### 端到端测试

- 不写自动化 e2e（项目目前无 e2e 框架，CLI/QQ 都靠手动冒烟）
- `cargo run -- serve` 启动后浏览器手动验证：对话、上传图片、agent send_image 显示、中止、断线重连、配置读写

**不引入新测试框架**：保持 `#[cfg(test)]` + `#[tokio::test]` 风格，与项目现有测试一致。
