# ADR 0009: QQ Channel 接入

- 状态：Accepted
- 日期：2026-07-21
- 关联：[docs/specs/2026-07-21-qq-channel-design.md](../specs/2026-07-21-qq-channel-design.md)、[docs/plans/2026-07-21-qq-channel.md](../plans/2026-07-21-qq-channel.md)

## 背景

v1 完成后用户希望接入 QQ 作为第二个交互频道，主要场景是在手机端通过 QQ 单聊唤醒桌面上的 LAIA 完成任务查询、记忆追加等。候选方案：

1. 腾讯官方 QQ 开放平台（qbot.qq.com）
2. OneBot / go-cqhttp 等第三方协议

## 决策

### 协议选择

采用腾讯官方 QQ 开放平台，不走 OneBot 等第三方协议。理由：

- 合规稳定，无封号风险
- 单聊场景下官方 API 能力足够（C2C 文本消息收发）
- 用户已有官方 bot 账号
- WebSocket + HTTPS 双通道，无需公网回调端点

### 鉴权流程

腾讯官方 v2 API 采用 `access_token` 鉴权（非老格式 `Bot appid.token`）：

1. **获取 access_token**：POST `https://bots.qq.com/app/getAppAccessToken`
   - 请求体：`{"appId": "<app_id>", "clientSecret": "<app_secret>"}`
   - 返回：`{"access_token": "...", "expires_in": 7200}`（有效期 2 小时）
2. **使用 access_token**：
   - HTTPS 请求头：`Authorization: QQBot <access_token>`
   - WS IDENTIFY 的 `token` 字段：`QQBot <access_token>`
3. **缓存与刷新**：`QqChannel` 内部 `Arc<Mutex<TokenState>>` 缓存 access_token + 过期时间戳，过期前 60 秒视为需要刷新。`get_access_token()` 方法负责缓存命中或换新。

配置字段（`[channels.qq]`）：
- `app_id` + `app_secret`（必填）：从 QQ 开放平台管理端获取
- 不再使用 `token`（老格式）和 `bot_qq`（C2C 单聊场景下 bot 不会收到自己发的消息，无需过滤）

### Channel 抽象

引入 `Channel` trait，CLI 和 QQ 各自实现：

```rust
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    async fn run(self: Arc<Self>, agent: Arc<Mutex<Agent>>) -> Result<()>;
}
```

- 每个 channel 负责自己的 I/O 循环（读用户输入、写回复）
- 共享同一个 Agent，通过 `Arc<tokio::sync::Mutex<Agent>>` 串行化访问
- 单用户场景下 Mutex 排队可接受；v2 多用户化时再考虑 per-user agent
- `chat_cmd` 根据 config 启用情况 `tokio::spawn` 多个 channel 任务

### QQ confirm 策略

QQ 下无法弹 stdin 等用户确认，独立设计三档：

- `always`（默认）：跳过有副作用的工具，回复用户原因
- `whitelist`：白名单内放行，其余跳过（v1.5 简化：等同于 always）
- `none`：全放行

`Tool` trait 加 `requires_confirm()` 方法（默认 `false`），`FileWrite`/`FileEdit`/`Terminal`/`MemoryWrite` override 为 `true`。`execute_tool_calls` 接收 `channel` 和 `qq_confirm_mode` 参数，QQ + 需确认工具时按 mode 决定是否执行。

### 长回复分片

QQ 单条消息上限约 2000 字符。LAIA 取 1800 作为安全阈值，纯函数 `split_reply(text, max)` 按三级 fallback 切分：

1. 段落（`\n\n`）
2. 行（`\n`）
3. 字符硬切

代码块跨片时，前片末尾补 ```` ``` ```` 闭合，后片以 ```` ```<lang> ```` 重开。

### 启动模型

- CLI 和 QQ 同进程，由 `chat_cmd` 用 `tokio::spawn` 启动两个 channel 任务
- 任一 channel 退出不影响另一个；都退出后进程退出
- WS 断开即进程退出，v1.5 不做自动重连 + RESUME

## 限制

- v1.5 只支持单进程运行（CLI 和 QQ 同进程）。分进程运行需要重新设计 `SessionStore`（SQLite 多写者）
- 只支持 C2C 文本消息。群聊、图片、语音、文件均不支持
- 不做主动消息推送（所有回复都是被动跟随用户消息）
- WS 断开即退出，不自动重连
- QQ confirm 在 `whitelist` 模式下与 `always` 行为相同（v1.5 简化）

## 影响的代码

- `Cargo.toml`：新增 `tokio-tungstenite`、`futures-util` 依赖
- `src/channels/mod.rs`：新增 `Channel` trait
- `src/channels/cli.rs`：`run_repl` 重构为 `impl Channel for CliChannel`，抽取 `build_agent` 共用
- `src/channels/qq.rs`：新增 `QqChannel`（access_token 缓存/刷新 + WS 接事件 + HTTPS 发消息 + 分片）
- `src/channels/qq_split.rs`：新增 `split_reply` 纯函数
- `src/config.rs`：`ChannelsConfig` 加 `qq: QqConfig`，`QqConfig` 字段为 `app_id`/`app_secret`/`confirm_mode`/`enabled`
- `src/tools/mod.rs`：`Tool` trait 加 `requires_confirm()` 默认 false
- `src/tools/{file,terminal,memory}.rs`：副作用工具 override `requires_confirm = true`
- `src/agent/mod.rs`：`Agent` 加 `qq_confirm_mode` 字段，`handle_input` 接收 `channel` 参数
- `src/agent/runner.rs`：`execute_tool_calls` 接收 `channel` + `qq_confirm_mode` 参数
- `src/commands/mod.rs`：`chat_cmd` 改为多 channel 启动
- `tests/qq_split.rs`：`split_reply` 集成测试
- `tests/qq_http.rs`：`QqChannel` access_token 缓存/HTTP 发送/重试/解析的 mockito 测试

## 遗留问题（推迟到后续 ADR）

- **环境变量插值**：QQ app_secret 明文存 config.toml，待 ADR 0008 遗留项一起处理
- **WS 自动重连**：生产级稳定性需要 RESUME 机制
- **主动心跳**：当前只被动响应服务端 op=1，不主动定时发心跳。生产级实现需要 `tokio::select!` + `tokio::time::interval`
- **per-user agent**：多用户场景下 `Arc<Mutex<Agent>>` 不再适用
- **QQ 群聊支持**：需要 `intents` 调整和群消息事件处理
