# LLAIA 开发文档

本文档面向开发者与 Agent，记录 LLAIA 的内部架构、工程约定与技术细节。

## 定位

单用户私人助理，次要承担电脑操作与文件读写任务。不支持多用户体系。

详见 [docs/adr/0001-product-positioning.md](docs/adr/0001-product-positioning.md)。

## 架构

- Rust 编写，轻量、可移植
- 主控 Agent + 多个专用 Agent 协作（**委派模式**）
- 用户只跟主 Agent 接触，特定任务主 Agent 通过 `delegate` 工具委派给后台子 Agent（子 Agent 在独立 workspace 运行，由 `[agent.<alias>]` 配置，受 `denied_tools` / `delegate_timeout` 约束）
- 子 Agent 借主 Agent 的工具集执行，结果回传主 Agent

### Channel 抽象

用户接入通道抽象为 `Channel` trait，CLI、WebUI 与各 IM 平台各自实现：

```rust
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    async fn run(self: Arc<Self>, agent: Arc<Mutex<Agent>>) -> Result<()>;
}
```

- 每个 channel 负责自己的 I/O 循环（读用户输入、写回复）
- 共享同一个 Agent，通过 `Arc<tokio::sync::Mutex<Agent>>` 串行化访问
- `serve_cmd` 根据 config 启用情况 `tokio::spawn` 多个 channel 任务（WebUI + 各 IM 频道）
- 当前实现：`CliChannel`（终端 REPL）、`WebChannel`（WebUI HTTP/WS，主交互界面）、`QqChannel`、`TelegramChannel`、`DingtalkChannel`、`WechatChannel`（微信 ClawBot）、`FeishuChannel`、`MailChannel`（IMAP/SMTP）

详见 [docs/adr/0009-qq-channel.md](docs/adr/0009-qq-channel.md)。

详见 [docs/adr/0002-agent-architecture.md](docs/adr/0002-agent-architecture.md)。

## 持久化

三份 Markdown 文件 + sqlite 会话记录，**均位于 `<config_dir>/workspace/`（agent 家目录，固定）**：

| 对象 | 形态 | 用途 |
|---|---|---|
| SOUL.md | 单文件 | Agent 人格设定 |
| USER.md | 单文件 | 用户画像、身份绑定清单、偏好 |
| MEMORY.md | 单文件，分条目 | 长期事实记忆 |
| sessions.db | sqlite | 会话历史（source of truth） |
| uploads/ | 目录 | 媒体/附件落盘 |

MEMORY.md 超限时先备份再由 LLM 去重压缩。上下文压缩时旧消息从内存移除但 sqlite 留底。

> **系统提示词 MEMORY 上限（ADR-0025）**：`build_single_agent` 把 MEMORY.md **全量**塞进 `system_prompt_base`，但在拼装前经 `src/memory/trim.rs::trim_memory_to_budget` 按 `[agent.<alias>].memory_token_budget`（默认 4000，chars/4 启发式）裁剪——超限时最旧溢出段交给 `compact_provider` 摘要、无则硬截断保留近期；SOUL/USER **永留全量、不计入预算**。裁剪结果由 `init_system_meta` 缓存，全频道共享且 skill 热重载稳定。主动持久化压缩用斜杠命令 `/memory-compact`（写前备份到 `workspace/backups/`）。

> **两个 `workspace` 的区别**：agent 家目录（SOUL/USER/MEMORY/sessions.db 所在，**固定不变**，位于 `config_dir/workspace/`）与文件/终端工具的实时作用域 `workspace_root`（可被 `/move` 切换）。`migrate.rs` 在 v0.2 后将旧版散落在 `~/.llaia/` 根的文件自动迁入 `workspace/`（写 `.migrated_v0.2` 标记，幂等）。

详见 [docs/adr/0003-persistence-model.md](docs/adr/0003-persistence-model.md)。

## 会话模型

- 同一用户同一会话，跨频道接续
- 手动 `/new` 开新会话，或上下文超阈值（默认 70%，可配）时自动压缩
- 压缩策略：关键消息保留（SOUL/USER 永留、首条用户消息留、工具调用结果可丢），其余旧消息 LLM 摘要替换

详见 [docs/adr/0004-session-and-context.md](docs/adr/0004-session-and-context.md)。

## Provider 与工具调用

- Provider 类型（按 `[provider.<id>].type` 区分）：
  - `openai_compatible`（默认，未写 `type` 也走这个）：覆盖 Ollama、Llama.cpp、LMStudio 等 OpenAI 兼容端点
  - `anthropic`：Anthropic Messages API（需 `max_tokens`）
  - `gemini`：Google Gemini API（需 `max_tokens`）
  - 未知 `type` 一律按 `openai_compatible` 处理（存量无 type 配置也能跑）
  - 支持 **fallback 备用模型链**：`[agent.<alias>].fallback` 列出备用 model ref，主模型请求失败时按序降级
- 工具调用协议：**原生优先 + 标签降级**
  - `native_tool_calling = true` → OpenAI function calling
  - `native_tool_calling = false` → system prompt 注入 `<tool_call>...</tool_call>` 协议
- 流式输出：`Provider` trait 已定义 `chat_stream`（SSE）接口，但 chat 主路径当前仍整块返回，未启用流式。

### Provider Compat 层（ADR-0026）

OpenAI 兼容端点各家实现参差，`OpenAiCompatibleProvider` 通过 `Compat` 结构体做响应归一化，抹平以下差异：

| 字段 | 含义 | 默认（bare） |
|---|---|---|
| `supports_developer_role` | 是否支持 `developer` role（Ollama/Llama.cpp 等不支持，自动并入 system） | `false` |
| `reasoning_to_content` | 把 `reasoning_content` / `thinking` 折回 `content` | `false` |
| `max_tokens_field` | 发送上限字段名：`none` / `max_tokens` / `max_completion_tokens` | `none`（不发送） |
| `streaming_usage` | 流式带 `stream_options.include_usage` 并解析末帧 `usage` | `false` |
| `infer_finish_reason` | 无 `finish_reason` 但含 tool_calls 时推断为 `tool_calls` | `false` |
| `requires_assistant_after_tool` | tool 消息后补空 assistant 占位（Ollama 某些版本要求） | `false` |

**自动探测**：`Compat::detect(base_url)` 按 host 子串匹配，命中即套用预设——
- `ollama`（含 `11434` / `ollama`）→ 开启除 `supports_developer_role` 外的全部归一化
- `llamacpp`（含 `llama` / `8080` / `completion`）→ 同上但 `requires_assistant_after_tool=false`
- 其余（含 LMStudio `1234`）→ 保持 bare，不改动原行为

**手动覆盖**：在 `config.toml` 的 `[provider.<id>]` 下加 `[provider.<id>.compat]` 子表，任一字段均可单独覆盖探测结果：

```toml
[provider.ollama_local]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"   # 自动探测命中 ollama 预设
model = [["default", { model = "qwen3:14b", native_tool_calling = true }]]

[provider.ollama_local.compat]
reasoning_to_content = true
max_tokens_field = "max_completion_tokens"   # 该模型用 max_completion_tokens
requires_assistant_after_tool = false          # 覆盖预设里的 true
```

`[provider.<id>].model.<alias>.max_tokens`（usize，可选）会随着 `max_tokens_field` 选定的字段名发送上限。`ChatResponse` 现额外返回 `usage: Option<Usage>` 与 `finish_reason: Option<String>`，便于上层做 token 统计与结束判定。

详见 [docs/adr/0026-provider-compat.md](docs/adr/0026-provider-compat.md) 与规划 [docs/plans/2026-08-14-provider-compat.md](docs/plans/2026-08-14-provider-compat.md)。

详见 [docs/adr/0005-provider-and-tool-calling.md](docs/adr/0005-provider-and-tool-calling.md)。

## 工具集

| 工具 | 模块 | 用途 |
|---|---|---|
| `file_read` / `file_write` / `file_edit` | `tools/file` | 文件读写、精确修改 |
| `terminal` | `tools/terminal` | 终端命令（含 ls/grep 等，不单列），受 `tools.terminal` 命令策略约束 |
| `web_fetch` | `tools/web` | 获取网页 |
| `search` | `tools/search` | 联网搜索（统一 `search` 工具，按 `[tools.search].provider` 路由到 tavily/baidu/brave，需对应 provider 的 api_key）|
| `todo` | `tools/todo` | 规划后执行：每会话一份待办清单（`add`/`list`/`update`/`done`），自动注入 Runtime Context（ADR-0024）|
| `ask_user` | `tools/ask_user` | 执行中主动向用户抛问题并**阻塞等待**回答再继续；交互频道走软暂停+续跑，非交互频道按最合理假设继续（ADR-0022）|
| `memory_write` | `tools/memory` | 写 MEMORY.md |
| `delegate` | `tools/delegate` | 后台委派子 Agent 执行长任务（脱离主回合，结果回传） |
| `cron` | `tools/cron` | 注册/执行定时任务（Agent 模式 / Step 模式） |
| `mcp` | `tools/mcp` | 接入外部 MCP server 暴露的工具 |
| `send_media` | `tools/send_media` | 向频道回传图片/文件等媒体 |

终端命令安全：由 `[tools.terminal]` 控制——
- `confirm`（`none` / `whitelist` 默认 / `always`）：是否需要交互式确认
- `command_policy`（`blacklist` 默认 / `whitelist` / `none`）：命令黑白名单

### 工具副作用标记

`Tool` trait 提供 `requires_confirm()`（默认 `false`）。有副作用的工具（`file_write` / `file_edit` / `terminal` / `memory_write` 等）override 为 `true`，触发审批流。

全局权限档位（`runtime.permission`，P4-d）：`read-only` / `default` / `yolo`，控制有副作用操作是否需要交互式审批及审批范围，运行时可用 `/permission <profile>` 切换。无 stdin 的频道（QQ/Telegram 等）则依赖 `[channels.<name>].confirm_mode` 作为兜底：

- `none`（默认）：全放行（需确认工具仍走 `/ok` `/deny` 审批）
- `always`：跳过需确认工具，回复用户原因
- `whitelist`：已废弃，加载时 warn 并 fallback 到 `none`

CLI 子命令：`llaia chat`（默认）/ `llaia serve`（主入口，拉起 WebUI + 启用的 IM 频道）/ `llaia init`（生成配置骨架）/ `llaia config` / `llaia doctor` / `llaia remember <text>`。
斜杠命令：`/new` `/exit` `/stop` `/compact` `/clear` `/stats` `/remember <text>` `/provider` `/permission <profile>` `/ok <id>` `/deny <id>` `/move [<path>|home]`（别名 `/cd`）`/config` `/dream` `/dream-rollback` `/delegate-list` `/delegate-cancel <id>` `/help`。

详见 [docs/adr/0006-tools-and-cli.md](docs/adr/0006-tools-and-cli.md) 与 [docs/adr/0009-qq-channel.md](docs/adr/0009-qq-channel.md)。

## 工作区与工程约定

默认 state dir `~/.llaia/`，可用 `--config-dir` 覆盖：

```
~/.llaia/
  config.toml
  .env                       # 可选，API key 等敏感配置（支持 ${VAR} 引用）
  logs/
  workspace/                 # agent 家目录（固定）：SOUL.md / USER.md / MEMORY.md / sessions.db / uploads/ / subagent/
```

- 配置格式：toml，命名式 section（`[provider.<id>]` / `[provider.<id>.<model_alias>]` / `[agent.<alias>]` / `[webui]` / `[channels.<qq|telegram|dingtalk|wechat|mail|feishu>]` / `[tools.terminal]` / `[tools.search]` / `[tools.tavily]` / `[tools.baidu]` / `[tools.brave]` / `[runtime]`）
- `workspace`（agent 家目录，固定）同时作为 state dir；文件/终端工具的实时作用域是 `workspace_root`，可被 `/move` 切换（详见「持久化」）
- 错误处理：`anyhow::Result`
- 日志：tracing，输出到文件 + stderr；`log.dir` 未配置时跟随 config 目录下的 `logs/`
- 单 crate

### Channels 与 WebUI 配置

IM 频道在 `[channels.<name>]` 下配置（均含 `enabled`，默认 false，及单用户安全锁字段）；Web UI 是顶层 `[webui]`（**不是** `[channels.web]`，旧写法会在 `Config::load` 时自动迁移到 `[webui]`）：

> **WebUI 配置保存的两种路径**
> - **结构化表单保存**（`PUT /api/config`，Config 页各表单的 Save）：用 `toml_edit` 做**定点合并**——只覆盖表单改动的 key，保留未改动段落的注释；`provider`/`agent` 子树走覆盖+删除缺失（支持表单删 provider/agent/model），`runtime`/`log`/`webui`/`channels`/`tools` 走保留缺失（保住表单未暴露的字段如 `runtime.compact_model`/`vision_model`、`provider.compat` 与这些段落的注释）。
> - **Raw TOML 编辑器**（`PUT /api/config/raw`，Config → Raw TOML 标签）：原文写回，注释完全保留，适合手改 schema 内任意字段（如 `[provider.<id>].compat.*` 覆盖层、agent `fallback`）。
>
> 表单目前暴露的 agent 字段：`model`（下拉选 `provider_id.model_alias`）、`fallback`（可增删的备用模型链标签列表）、`delegate_timeout`；provider 字段：`type`、`base_url`、`api_key`、以及折叠的 **Compatibility** 高级面板（`compat` 覆盖层的 6 个开关，`provider` 级、绝不会混入 model 列表）。schema 内但表单未单列的项，仍可用 Raw TOML 维护。

```toml
[webui]
host = "127.0.0.1"           # 默认仅本机；改 0.0.0.0 需自担风险
port = 51217                 # 默认端口
token = ""                   # 留空则启动时随机生成并打印日志

[channels.qq]
enabled = false
app_id = ""
app_secret = ""
confirm_mode = "none"        # none（默认，全放行）/ always（跳过需确认工具）/ whitelist（已废弃→none）

[channels.telegram]
enabled = false
bot_token = "${TG_BOT_TOKEN}"
allow_chat_id = 0            # 单用户安全锁，0=不限制

[channels.dingtalk]
enabled = false
client_id = ""
client_secret = ""

[channels.wechat]            # 微信 ClawBot（ilink）
enabled = false
allow_user_id = ""

[channels.mail]              # IMAP 收信 + SMTP 发信
enabled = false
imap_server = "imap.gmail.com"
# imap_port=993 / imap_user / imap_pass / smtp_server / smtp_port / owner_email ...

[channels.feishu]
enabled = false
app_id = ""
app_secret = ""
```

> 各频道完整字段见 `src/config.rs` 中的 `*Config` 结构体。

QQ 鉴权流程：启动时用 `app_id` + `app_secret` 调 `https://bots.qq.com/app/getAppAccessToken` 换取 `access_token`（有效期 7200 秒，过期前 60 秒自动刷新），HTTPS 请求头 `Authorization: QQBot {access_token}`，WS IDENTIFY 的 `token` 字段同此格式。

详见 [docs/adr/0007-project-structure-and-conventions.md](docs/adr/0007-project-structure-and-conventions.md) 与 [docs/adr/0008-config-schema-v1.1.md](docs/adr/0008-config-schema-v1.1.md)。

## P1 MVP 验收标准（历史基线，已超集实现）

- 能 `cargo run --` 进入交互（默认 `chat` REPL，或用 `llaia serve` 拉起 WebUI）
- 能调本地 Ollama / LMStudio，以及 Anthropic / Gemini 等云端 provider
- 主 Agent 能调文件读写 / 终端 / 网页 / 搜索 / 委派 / 定时任务
- `/remember` 写 MEMORY，下次加载生效
- 自动压缩，sqlite 留底
- `llaia config` / `llaia doctor` / `llaia init` 可用

## 编码约定

- 不用 `_` 前缀或 `#[allow(dead_code)]` 掩盖无用代码——删掉、接入逻辑，或开 issue 追踪。
- 不加"先占着"的 config key 或 feature flag——没有具体用例就不写。
- 生产路径不用 `unwrap()` / `expect()`——传播错误，或注释说明为何不可能 panic。

### 提交前检查（对齐 CI 质量门）

CI 在 `push` / PR 时对 `main` 跑三道门：`cargo fmt --check` → `cargo clippy --all-targets -- -D warnings` → `cargo test`。本地未跑这些检查，是 CI 频繁变红的主因。约定如下：

- **每次 commit 前**：跑 `cargo fmt --all`（写，自动格式化，保持提交历史干净）。
- **每次 push 前**：跑与 CI 完全一致的检查，确认能绿再推：
  ```bash
  cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test
  ```
- 平台相关测试（依赖 Windows / Linux / macOS 特有行为）必须用 `#[cfg(target_os = "...")]` 门控，避免在非目标平台误报失败。
- 推荐把上面两步接入 git hook（`pre-commit` 跑 fmt、`pre-push` 跑 clippy+test），或封装成 `cargo xtask ci` / `make check`，避免手动记忆。

### 发版

- 走 `cargo-release` 自动化流程（配置见仓库根 `release.toml`）。工具本地一次性安装：`cargo install cargo-release`（或 `cargo binstall cargo-release`）。
- **cargo-release 1.x 与 0.x 的两处关键差异（已踩坑，必须注意）**：
  1. `release.toml` 必须是**平铺格式**（顶层不要 `[release]` 表包裹），否则报 `Failed to parse release.toml` 且 `cargo release` 完全无法运行。
  2. `cargo set-version` 子命令在 1.x 已移除；提版本号改用 `cargo release version <VERSION>`，或直接编辑 `Cargo.toml` 的 `version` 再 `cargo build` 同步 `Cargo.lock`（后者更确定可靠）。
- 工作区版本号始终保持为**下一个开发版本**（如 v0.1.0 发完后，`Cargo.toml` 为 `0.1.1`）。
- 提版本号（开发阶段，纯版本号改动，跳过测试）：
  ```bash
  # 方式一：cargo-release 1.x 子命令
  cargo release version 0.1.2
  # 方式二（确定可靠）：手动改 Cargo.toml version 后同步 lock
  #   编辑 Cargo.toml: version = "0.1.2"
  #   cargo build   # 同步 Cargo.lock
  git add Cargo.toml Cargo.lock
  git commit -m "chore: bump version to 0.1.2"
  ```
- 发布某个开发版本（生成 `chore: release X.Y.Z` 提交 + `vX.Y.Z` tag，不自动 push）：
  ```bash
  cargo release 0.1.1 --execute --no-publish --no-push --no-confirm
  git push --follow-tags
  ```
  - `--no-confirm` 必加：1.x 在非 CI 环境会交互询问 `Release? [y/N]` 并卡住；CI 环境设 `CI=true` 也可跳过。
- **网络注意**：即便 `publish = false`，`cargo release` 仍会访问 crates.io index 做"未发布"检查。本机沙箱/离线环境会因 `error sending request for url (https://index.crates.io/...)` 失败。此时手动兜底等价完成本地部分：
  ```bash
  git tag -a v0.1.1 -m "chore: release 0.1.1"   # tag 打在 HEAD
  ```
  （version 已是目标版本时 cargo-release 不会再生成额外 release 提交，核心产物就是 tag。）
- `git push --follow-tags` 推送 tag 后触发 `release.yml`，自动构建多平台二进制并上传到 GitHub Releases。
- **tag 必须带 `v` 前缀**（`release.toml` 已用 `tag-name = "v{{version}}"` 保证），否则 release 工作流不触发。
- 不发布到 crates.io（`release.toml` 中 `publish = false`）；`push = false` 由 Feihei 手动推送。

## 文档结构

- [docs/guide/](docs/guide/README.md) — **用户文档**（按功能模块）：安装、快速开始、CLI、配置、Web UI、频道、定时任务、MCP、技能、记忆与上下文、斜杠命令、工具、权限与安全、FAQ。README 只做入口，详情在此。
- [docs/adr/](docs/adr/) — 架构决策记录（ADR-0001 起持续追加，开发者向）
- [docs/glossary.md](docs/glossary.md) — 术语表
- [docs/specs/](docs/specs/) — 规格文档
- [docs/plans/](docs/plans/) — 实现计划
- [docs/issues/](docs/issues/) — 问题记录
