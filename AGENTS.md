# LLAIA 开发文档

本文档面向开发者与 Agent，记录 LLAIA 的内部架构、工程约定与技术细节。

## 定位

单用户私人助理，次要承担电脑操作与文件读写任务。不支持多用户体系。

详见 [docs/adr/0001-product-positioning.md](docs/adr/0001-product-positioning.md)。

## 架构

- Rust 编写，轻量、可移植
- 主控 Agent + 多个专用 Agent 协作（**委派模式**，P2 引入；P1 主 Agent 单干）
- 用户只跟主 Agent 接触，特定任务主 Agent 委派给特定子 Agent
- P1 子 Agent 不实现，所有任务由主 Agent 完成

### Channel 抽象（P1.5 引入）

用户接入通道抽象为 `Channel` trait，CLI 和 QQ 各自实现：

```rust
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    async fn run(self: Arc<Self>, agent: Arc<Mutex<Agent>>) -> Result<()>;
}
```

- 每个 channel 负责自己的 I/O 循环（读用户输入、写回复）
- 共享同一个 Agent，通过 `Arc<tokio::sync::Mutex<Agent>>` 串行化访问
- `chat_cmd` 根据 config 启用情况 `tokio::spawn` 多个 channel 任务
- P1.5 实现：`CliChannel`（终端 REPL）、`QqChannel`（腾讯官方 QQ 开放平台 C2C 单聊）

详见 [docs/adr/0009-qq-channel.md](docs/adr/0009-qq-channel.md)。

详见 [docs/adr/0002-agent-architecture.md](docs/adr/0002-agent-architecture.md)。

## 持久化

三份 Markdown 文件 + sqlite 会话记录：

| 对象 | 形态 | 用途 |
|---|---|---|
| SOUL.md | 单文件 | Agent 人格设定 |
| USER.md | 单文件 | 用户画像、身份绑定清单、偏好 |
| MEMORY.md | 单文件，分条目 | 长期事实记忆 |
| sessions.db | sqlite | 会话历史（source of truth） |

MEMORY.md 超限时先备份再由 LLM 去重压缩。上下文压缩时旧消息从内存移除但 sqlite 留底。

详见 [docs/adr/0003-persistence-model.md](docs/adr/0003-persistence-model.md)。

## 会话模型

- 同一用户同一会话，跨频道接续
- 手动 `/new` 开新会话，或上下文超阈值（默认 70%，可配）时自动压缩
- 压缩策略：关键消息保留（SOUL/USER 永留、首条用户消息留、工具调用结果可丢），其余旧消息 LLM 摘要替换

详见 [docs/adr/0004-session-and-context.md](docs/adr/0004-session-and-context.md)。

## Provider 与工具调用

- P1 只实现 OpenAI 兼容 provider（覆盖 Ollama、Llama.cpp、LMStudio 等本地端点）
- 工具调用协议：**原生优先 + 标签降级**
  - `native_tool_calling = true` → OpenAI function calling
  - `native_tool_calling = false` → system prompt 注入 `<tool_call>...</tool_call>` 协议
- P1 不做流式输出（SSE）

详见 [docs/adr/0005-provider-and-tool-calling.md](docs/adr/0005-provider-and-tool-calling.md)。

## 工具集（P1 最小集）

| 工具 | 用途 |
|---|---|
| `file_read` / `file_write` / `file_edit` | 文件读写、精确修改 |
| `terminal` | 终端命令（含 ls/grep 等，不单列） |
| `web_fetch` | 获取网页 |
| `tavily_search` | 搜索（需 api_key） |
| `memory_write` | 写 MEMORY.md |

终端命令安全：配置项控制（`none` / `whitelist` 默认 / `always`）。

### 工具副作用标记（P1.5 引入）

`Tool` trait 加 `requires_confirm()` 方法（默认 `false`）。`FileWrite` / `FileEdit` / `Terminal` / `MemoryWrite` override 为 `true`。

QQ channel 下无法弹 stdin 等用户确认，`execute_tool_calls` 接收 `channel` 和 `qq_confirm_mode` 参数，按 `[channels.qq].confirm_mode` 决定是否执行：

- `always`（默认）：跳过需确认工具，回复用户原因
- `whitelist`：P1.5 简化，等同于 `always`
- `none`：全放行

CLI 子命令：`llaia chat`（默认）/ `llaia config` / `llaia doctor` / `llaia remember`。
斜杠命令：`/new` `/exit` `/compact` `/clear` `/remember` `/config` `/help`。

详见 [docs/adr/0006-tools-and-cli.md](docs/adr/0006-tools-and-cli.md) 与 [docs/adr/0009-qq-channel.md](docs/adr/0009-qq-channel.md)。

## 工作区与工程约定

默认工作区 `~/.llaia/`，可配置：

```
~/.llaia/
  config.toml
  SOUL.md
  USER.md
  MEMORY.md
  sessions.db
  logs/
```

- 配置格式：toml，命名式 section（`[provider.<id>]` / `[provider.<id>.<model_alias>]` / `[agent.<alias>]` / `[channels.<cli|qq>]`）
- workspace 同时作为 state dir 和 tool working dir（P1.1 起，详见 ADR 0008）
- 错误处理：`anyhow::Result`（P1 简单优先）
- 日志：tracing，输出到文件 + stderr
- P1 单 crate，P2 视复杂度再考虑拆分

### Channels 配置（P1.5）

```toml
[channels.cli]
enabled = true                # 默认 true

[channels.qq]
enabled = false               # 默认 false
app_id = ""
app_secret = ""
confirm_mode = "always"       # always / whitelist / none
```

鉴权流程：启动时用 `app_id` + `app_secret` 调 `https://bots.qq.com/app/getAppAccessToken` 换取 `access_token`（有效期 7200 秒，过期前 60 秒自动刷新），HTTPS 请求头 `Authorization: QQBot {access_token}`，WS IDENTIFY 的 `token` 字段同此格式。

详见 [docs/adr/0007-project-structure-and-conventions.md](docs/adr/0007-project-structure-and-conventions.md) 与 [docs/adr/0008-config-schema-v1.1.md](docs/adr/0008-config-schema-v1.1.md)。

## P1 MVP 验收标准

- 能 `cargo run -- chat` 进 REPL 多轮对话
- 能调本地 Ollama / LMStudio
- 主 Agent 能调文件读写 / 终端 / 网页 / 搜索
- `/remember` 写 MEMORY，下次加载生效
- 自动压缩，sqlite 留底
- `llaia config` / `llaia doctor` 可用

## 设计文档索引

- [docs/adr/](docs/adr/) — 架构决策记录（ADR-0001 到 ADR-0010）
- [docs/glossary.md](docs/glossary.md) — 术语表
- [docs/specs/](docs/specs/) — 规格文档
- [docs/plans/](docs/plans/) — 实现计划
