# P1.5: QQ Channel 接入设计

- 状态：Spec
- 日期：2026-07-21
- 目标版本：P1.5

## 背景

P1 已完成 CLI channel 的 LLAIA MVP。P1.5 的目标是接入腾讯官方 QQ 开放平台机器人，让用户可以通过 QQ 单聊与 LLAIA 交互，实现"跨频道接续会话"的核心设计。

## 范围

### 包含

- 抽象 `Channel` trait，为未来邮箱 / web channel 铺路
- 实现 `QqChannel`，对接腾讯官方 QQ 开放平台
  - WebSocket 接收 C2C 消息事件
  - HTTPS API 发送回复
- 跨 channel 共享 Agent + SessionStore
- 长回复自动分片发送
- QQ 下的工具 confirm 策略（不弹 stdin）

### 不包含

- 群聊（@ 机器人、群消息事件）
- 图片 / 语音 / 文件消息（收发均只文本 + 代码块）
- 主动消息推送（只被动回复）
- markdown 原生渲染（依赖 QQ 客户端，代码块用 ``` 标记）
- 消息去重 / 限流（单用户场景）
- 流式输出

## 架构

### Channel trait

```rust
#[async_trait]
pub trait Channel {
    /// 启动 channel，阻塞运行直到退出
    async fn run(self: Arc<Self>, agent: Arc<Mutex<Agent>>) -> Result<()>;
}
```

- `Arc<Self>` 允许 channel 被 spawn 到 tokio task
- `Arc<Mutex<Agent>>` 共享 agent，串行化访问（单用户场景下足够）

### 启动流程

```rust
// main.rs
let agent = Arc::new(Mutex::new(Agent::new(...).await));
let mut tasks = vec![];

if config.channels.cli.enabled {
    let cli = Arc::new(CliChannel::new());
    let agent = agent.clone();
    tasks.push(tokio::spawn(async move { cli.run(agent).await }));
}
if config.channels.qq.enabled {
    let qq = Arc::new(QqChannel::new(config.channels.qq.clone()));
    let agent = agent.clone();
    tasks.push(tokio::spawn(async move { qq.run(agent).await }));
}

futures::future::join_all(tasks).await;
```

### QqChannel 工作流

1. WS 连接腾讯官方 endpoint，鉴权（app_id + token）
2. 接收 C2C 消息事件，提取文本
3. `agent.lock().await.handle_input(text, "qq").await` 拿到回复
4. 分片（按段落优先，每片 ≤ 1800 字符留余量）
5. 通过 HTTPS API 逐片发送
6. API 失败 3 次指数退避（200ms / 400ms / 800ms），最终失败记日志不丢消息（下次用户发消息时 agent 仍可看到上次未发送的内容，因为 sqlite 已留底）

### Session 共享

跨 channel 共享同一 session。CLI 谈的话题，QQ 上可接续，反之亦然。

由于 `Agent` 被 `Mutex` 串行化，CLI 跑长工具调用时 QQ 消息会排队等待——单用户场景可接受（不会真的同时在两端发消息）。

## QQ confirm 策略

QQ 下无法弹 stdin 确认，需要独立的 confirm 语义：

- `always`（默认）：跳过有副作用的工具，直接返回"QQ 频道下不能执行此操作：{tool_name}"
- `whitelist`：白名单内放行，其余跳过并回复同样提示
- `none`：全放行（用户自负责任，不推荐）

### 实现方式

`Tool` trait 加一个方法：

```rust
pub trait Tool {
    // ... 既有方法
    fn requires_confirm(&self) -> bool { false }  // 默认无副作用
}
```

- `FileRead`、`WebFetch`、`TavilySearch` → 默认 false
- `FileWrite`、`FileEdit`、`Terminal`、`MemoryWrite` → override 返回 true

`Agent::handle_input` 执行工具前检查：

```rust
if channel == "qq" {
    let mode = &config.channels.qq.confirm_mode;
    let needs = tool.requires_confirm();
    if needs && mode == "always" {
        return Err(format!("QQ 频道下不能执行此操作：{}", tool.name()));
    }
    if needs && mode == "whitelist" && !whitelist.contains(tool.name()) {
        return Err(...);
    }
    // mode == "none" 或 whitelist 命中：执行
}
// CLI 下走原有逻辑
```

## 长回复分片

腾讯 QQ 单条消息字符上限约 2000。LLAIA 取 1800 作为安全阈值。

分片规则：

1. 优先按段落（`\n\n`）切，每片总长 ≤ 1800
2. 单段超 1800 时，按行（`\n`）切
3. 单行超 1800 时，按字符硬切
4. 代码块跨片时：闭合后再开，下一片以 ``` 同语言标记开始

分片函数为纯函数 `split_reply(text: &str, max: usize) -> Vec<String>`，方便单测。

## 配置扩展

```toml
[channels.cli]
enabled = true                 # 可省，默认 true

[channels.qq]
enabled = false                # 默认 false
app_id = ""
token = ""
bot_qq = ""
confirm_mode = "always"        # 默认 always
```

`QqConfig` 字段：

```rust
pub struct QqConfig {
    pub enabled: bool,
    pub app_id: String,
    pub token: String,
    pub bot_qq: String,
    pub confirm_mode: String,  // "always" / "whitelist" / "none"
}
```

## 依赖

新增：

- `tokio-tungstenite = { version = "0.23", features = ["native-tls"] }` — WebSocket 客户端

既有复用：tokio、reqwest、serde、anyhow、tracing

## 文件结构

```
src/
  channels/
    mod.rs        — Channel trait
    cli.rs        — CliChannel impl（重构自现有 run_repl）
    qq.rs         — QqChannel impl
  agent/
    mod.rs        — handle_input 加 channel + confirm 检查
  tools/
    mod.rs        — Tool trait 加 requires_confirm()
    file.rs       — FileWrite/FileEdit override requires_confirm = true
    terminal.rs   — Terminal override requires_confirm = true
    memory.rs     — MemoryWrite override requires_confirm = true
  config.rs       — ChannelsConfig 加 qq: QqConfig
  main.rs         — 启动多 channel
```

## 测试策略

### 单元测试

- `QqConfig` 序列化 / 反序列化（含默认值）
- `split_reply(text, 1800)` 纯函数：
  - 短回复不切
  - 按段落切
  - 段落超长按行切
  - 代码块跨片正确闭合 / 重开
- `Tool::requires_confirm()` 各工具返回值
- QQ confirm 检查逻辑（mock agent，验证不同 mode 下工具被跳过 / 执行）

### 集成测试

- `QqChannel` 用 mockito mock 腾讯 HTTPS API，验证发送消息调用正确
- `CliChannel` 既有行为不回归（通过 cargo run -- chat 端到端）

### 端到端 smoke test

- 配置真实 QQ bot 凭据
- 用另一个 QQ 号发消息，验证 LLAIA 回复
- 验证 CLI 和 QQ 跨 channel 共享 session（CLI 谈话题 → QQ 接续）

## 风险与开放问题

1. **腾讯官方 API 鉴权细节**：spec 阶段未深入协议细节（WebSocket endpoint 路径、事件 payload schema、鉴权 handshake）。实现阶段需查阅官方文档：https://bot.q.qq.com/wiki/
2. **token 安全**：当前 config 明文存 token，与 P1 tavily api_key 一致。后续 ADR 单独讨论环境变量插值。
3. **CLI 跟 QQ 同时运行的进程模型**：spec 假设单进程多 task。如果用户希望 CLI 和 QQ 分进程运行（例如 QQ 守护进程常驻、CLI 按需启动），需要重新设计 session 共享（当前 SessionStore 是 `Arc<Mutex<Connection>>`，跨进程会冲突）。**P1.5 接受单进程限制，文档中注明。**
