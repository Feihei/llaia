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

三份核心 Markdown 文件 + sqlite 会话记录，**均位于** **`<config_dir>/workspace/`（agent 家目录，固定）**：

| 对象          | 形态        | 用途                       |
| ----------- | --------- | ------------------------ |
| SOUL.md     | 单文件       | Agent 人格设定               |
| USER.md     | 单文件       | 用户画像、身份绑定清单、偏好           |
| MEMORY.md   | 单文件，分条目   | 长期事实记忆                   |
| reminder.md | 单文件（自动生成） | Tail Reminder 抗漂移要点（勿手改） |
| sessions.db | sqlite    | 会话历史（source of truth）    |
| uploads/    | 目录        | 媒体/附件落盘                  |

MEMORY.md 超限时先备份再由 LLM 去重压缩。上下文压缩时旧消息从内存移除但 sqlite 留底。

> **系统提示词 MEMORY 上限（ADR-0025）**：`build_single_agent` 把 MEMORY.md **全量**塞进 `system_prompt_base`，但在拼装前经 `src/memory/trim.rs::trim_memory_to_budget` 按 `[agent.<alias>].memory_token_budget`（默认 4000，chars/4 启发式）裁剪——超限时最旧溢出段交给 `compact_provider` 摘要、无则硬截断保留近期；SOUL/USER **永留全量、不计入预算**。裁剪结果由 `init_system_meta` 缓存，全频道共享且 skill 热重载稳定。主动持久化压缩用斜杠命令 `/memory-compact`（写前备份到 `workspace/backups/`）。

> **Tail Reminder 自动生成（P6，`src/agent/reminder.rs`）**：长会话风格漂移的根因是 LLM 自我模仿 + 中段注意力稀释（SOUL/USER 结构上每轮完整重发，并未丢失）。对策：回合起点对 SOUL+USER 求 md5，与 `workspace/reminder.md` 记录的 hash 失配（或缺失）时**后台隔离 turn** 让 LLM 提炼 ≤120 token 的行为指令清单（走 compact\_provider 回退主模型），写盘后下一轮作为请求**最后一条消息**注入（`Context.reminder`，排在状态栏/todo/env 之后）。MEMORY/skills 不参与 hash（避免 memory\_write 频繁重生成）；生成失败静默降级；文件头注释声明勿手改。

> **两个** **`workspace`** **的区别**：agent 家目录（SOUL/USER/MEMORY/sessions.db 所在，**固定不变**，位于 `config_dir/workspace/`）与文件/终端工具的实时作用域 `workspace_root`（可被 `/move` 切换）。`migrate.rs` 在 v0.2 后将旧版散落在 `~/.llaia/` 根的文件自动迁入 `workspace/`（写 `.migrated_v0.2` 标记，幂等）。

详见 [docs/adr/0003-persistence-model.md](docs/adr/0003-persistence-model.md)。

## 会话模型

- 同一用户同一会话，跨频道接续

- 手动 `/new` 开新会话，或上下文超阈值（默认 70%，可配）时自动压缩

- 压缩策略：关键消息保留（SOUL/USER 永留、首条用户消息留、工具调用结果可丢），其余旧消息 LLM 摘要替换

- **任务线（ADR-0031）**：通用线（`sessions.kind='main'`）之外可显式开任务线（`kind='task'`，`bound_path` 绑定目录元数据）——`/task <名>` 进出、`/tasks` 列表、`/task close` 归档（`state='archived'` 不可续写）；切线时回灌目标线 sqlite 尾部（6000 字符预算，只取 user/assistant 正文），任务名/绑定目录经 Runtime Context（`Context.task_state`）注入；`/move` 批准后提示开任务线。

详见 [docs/adr/0004-session-and-context.md](docs/adr/0004-session-and-context.md) 与 [docs/adr/0031-task-session-model.md](docs/adr/0031-task-session-model.md)。

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

| 字段                              | 含义                                                                    | 默认（bare）    |
| ------------------------------- | --------------------------------------------------------------------- | ----------- |
| `supports_developer_role`       | 是否支持 `developer` role（Ollama/Llama.cpp 等不支持，自动并入 system）              | `false`     |
| `reasoning_to_content`          | 把 `reasoning_content` / `thinking` 折回 `content`                       | `false`     |
| `max_tokens_field`              | 发送上限字段名：`none` / `max_tokens` / `max_completion_tokens`               | `none`（不发送） |
| `streaming_usage`               | 流式带 `stream_options.include_usage` 并解析末帧 `usage`                      | `false`     |
| `infer_finish_reason`           | 无 `finish_reason` 但含 tool\_calls 时推断为 `tool_calls`                    | `false`     |
| `requires_assistant_after_tool` | tool 消息后补空 assistant 占位（Ollama 某些版本要求）                                | `false`     |
| `disable_thinking_template`     | 请求显式 `disable_thinking` 时注入 `chat_template_kwargs` 关闭推理模型深度思考         | `true`      |
| `native_tool_calling`           | 原生 tool calling 探测默认：`ModelConfig.native_tool_calling=None(自动)` 时跟随该值 | `true`      |

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

| 工具                                       | 模块                                        | 用途                                                                                                                                                |
| ---------------------------------------- | ----------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| `file_read` / `file_write` / `file_edit` | `tools/file`                              | 文件读写、精确修改                                                                                                                                         |
| `terminal`                               | `tools/terminal`                          | 终端命令（含 ls/grep 等，不单列），受 `tools.terminal` 命令策略约束                                                                                                   |
| `web_fetch`                              | `tools/web`                               | 获取网页                                                                                                                                              |
| `search`                                 | `tools/search`                            | 联网搜索（统一 `search` 工具，按 `[tools.search].provider` 路由到 tavily/baidu/brave，需对应 provider 的 api\_key）                                                   |
| `todo`                                   | `tools/todo`                              | 规划后执行：每会话一份待办清单（`add`/`list`/`update`/`done`），自动注入 Runtime Context（ADR-0024）                                                                      |
| `ask_user`                               | `tools/ask_user`                          | 执行中主动向用户抛问题并**阻塞等待**回答再继续；交互频道走软暂停+续跑，非交互频道按最合理假设继续（ADR-0022）                                                                                     |
| `memory_write`                           | `tools/memory`                            | 写 MEMORY.md                                                                                                                                       |
| `skill_create` / `skill_edit`            | `tools/skill_create` / `tools/skill_edit` | agent 自管 skill（ADR-0027）：建 `<name>/SKILL.md` / 改已存在 SKILL.md（`content` 整篇覆盖、`old_string`+`new_string` 唯一命中替换、`append` 追加正文，三模式互斥；仅注册在 main agent） |
| `delegate`                               | `tools/delegate`                          | 后台委派子 Agent 执行长任务（脱离主回合，结果回传）                                                                                                                     |
| `cron`                                   | `tools/cron`                              | 注册/执行定时任务（Agent 模式 / Step 模式）                                                                                                                     |
| `mcp`                                    | `tools/mcp`                               | 接入外部 MCP server 暴露的工具                                                                                                                             |
| `send_media`                             | `tools/send_media`                        | 向频道回传图片/文件等媒体（作用域 = workspace\_root ∪ 受信目录，跟随 `/move`；家目录恒可发送，同 `file_read` extra\_readable 语义）                                                   |
| `tts`                                    | `tools/tts`                               | 文本合成语音（`[tools.tts]` 配置，OpenAI 兼容 `/audio/speech`，产物落 workspace/tts/，发送走 `send_file`；P5 T1）                                                       |

> **环境探测（P5 E1，非工具）**：`src/envprobe.rs` 启动时对 main agent 探测一次本机工具链（shell/python/node/npm/rustc/cargo/go/git/docker，2s/命令 timeout），以 Runtime Context 尾部注入（与 todo 同区，KV 缓存友好）；`/env` 命令手动刷新；WebUI 聊天页 ENV 只读面板（`GET /api/env` 缓存 / `POST /api/env/refresh` 重探）。

> **WebUI Doctor（P6）**：`commands::doctor_checks`（结构化版 CLI doctor，`GET /api/doctor`，WebUI Config 页 Doctor section 分色展示）分两层——**文件层** `template_file_checks`（config.toml / cron.toml / mcp.toml / `.env` 的存在性与可解析性、`.env` Unix 权限位）与**运行层**（provider 连通性，openai\_compatible 才探 `/models`，5s 超时；主模型链、context\_size 探测、sessions.db、cron/mcp 解析、skills 计数）。**doctor 是纯只读诊断，修复动作一律归** **`llaia init`**（曾议合并为 `doctor --fix` / `--init`，因检查面多数不可自动修、WebUI 复用要求只读而否决），闭环靠 detail 给可执行指引：缺模板 → `llaia init`（或等 serve/chat 自动补齐）、坏配置 → 备份后 `llaia init --force`。容错由 `load_config_for_doctor` 保证：**config.toml 解析失败作为一条 error 检查项报出并跳过依赖有效配置的探测，命令绝不 Err 退出**（否则最需要诊断的时刻恰好哑火）。CLI `doctor_cmd` 与 WebUI 共用上述检查，`/models` 同样带 5s 超时。

> **工具结果防护（超大返回 / base64 图片）**：`execute_tool_calls` 返回的文本结果 push 进 history 前在 `src/agent/mod.rs` 做两道处理——
>
> 1. **图片识别**（`src/image_utils.rs::extract_data_url_images`）：`data:image/<fmt>;base64,` 从文本剥离为 `[图片]` 占位。配了 `runtime.vision_model` → 用 vision provider 描述图片，描述文本进上下文；未配（主模型多模态）→ 桥接一条 user 多模态消息让主模型直接读图（结构：assistant(tool\_calls) → tool(占位) → user(图片) → assistant）。图片经 `prepare_base64_for_vision` 缩放重编码（最长边 1024 / JPEG85）后**落盘** **`workspace_root/tmp/`** **并发** **`TurnEvent::MediaOutput`** **回显给用户**。
> 2. **非图片超长截断**：超过 `runtime.tool_result_cap`（默认 32768 字符）保留头部 + 占位说明（完整内容已在 sqlite 留底）。`context.rs::cheap_normalize`（compact 时的 TOOL\_TRIM\_CAP=500）不受影响。

终端命令安全：由 `[tools.terminal]` 控制——

- `confirm`（`none` / `whitelist` 默认 / `always`）：是否需要交互式确认

- `command_policy`（`blacklist` 默认 / `whitelist` / `none`）：命令黑白名单

> **Windows 执行器**：优先探测 Git Bash（白名单安装路径 + PATH，排除 WSL `System32`/`WindowsApps` 假 bash，`$MSYSTEM` 非空校验），以 `bash -s` 经 **stdin** 喂命令——绕开 MSVCRT argv 转义层，双引号 / `;` 链 / `$VAR` / 中文（UTF-8）按 bash 语义正确执行；无 Git Bash 时回退 `cmd /C` + `raw_arg` 原样传参（引号不再二次转义，但 `;`/`$VAR`/中文受 cmd 限制）。非 Windows 走 `sh -c`。

### 工具副作用标记

`Tool` trait 提供 `requires_confirm()`（默认 `false`）。有副作用的工具（`file_write` / `file_edit` / `terminal` / `memory_write` 等）override 为 `true`，触发审批流。

全局权限档位（`runtime.permission`，P4-d）：`read-only` / `default` / `yolo`，控制有副作用操作是否需要交互式审批及审批范围，运行时可用 `/permission <profile>` 切换。无 stdin 的频道（QQ/Telegram 等）则依赖 `[channels.<name>].confirm_mode` 作为兜底：

- `none`（默认）：全放行（需确认工具仍走 `/ok` `/deny` 审批）

- `always`：跳过需确认工具，回复用户原因

- `whitelist`：已废弃，加载时 warn 并 fallback 到 `none`

CLI 子命令：`llaia chat`（默认）/ `llaia serve`（主入口，拉起 WebUI + 启用的 IM 频道）/ `llaia init`（显式生成配置骨架；`--force` 覆盖重建。serve / chat 启动时经 `prepare_startup_dir` 自动做「迁移 → 幂等补齐模板 → 加载配置」，缺啥补啥、绝不覆盖已有文件，裸 `llaia serve` 在全新机器可直接跑）/ `llaia config` / `llaia doctor` / `llaia remember <text>`。
斜杠命令：`/new` `/task [<名>|close]` `/tasks` `/exit` `/stop` `/compact` `/memory-compact` `/clear` `/stats` `/remember <text>` `/provider` `/permission <profile>` `/reasoning [on|off]`（会话级思考开关） `/btw <question>`（侧问：读上下文零污染，答案落 `side_messages` 独立表、WebUI Side 样式渲染） `/steer <msg>`（运行中插话：channel 层拦截投 `Agent.steer_buffer`，agent 工具循环非末轮迭代顶部以 `[steer] User added:` user 消息注入；空闲时降级为普通消息） `/ok <id>` `/deny <id>` `/move [<path>|home]`（别名 `/cd`）`/config` `/env` `/migrate-secrets` `/delegate-list` `/delegate-cancel <id>` `/help`。

> **敏感信息 .env 自动化（P5 S1）**：`src/config/secrets.rs`。WebUI `PUT /api/config` 保存时，明文敏感字段（provider api\_key、频道 token/secret、搜索 key、TTS key、webui token）**先写入** **`<config_dir>/.env`**（幂等 upsert、Unix 0600 权限），config.toml 只保留 `${VAR}` 引用；内存态再展开回明文供热加载（`build_provider_from_config` 不认 `${VAR}`）。`.env` 写入失败 → 保留明文 + warn 降级。存量迁移用 `/migrate-secrets`（toml\_edit 定点替换保注释）；启动时扫描明文敏感字段并 warn。`GET /api/config` 返回时敏感字段掩码为 `••••`（保存时空输入 = 保留原值，见 `mask_sensitive`/`merge_masked`）。

详见 [docs/adr/0006-tools-and-cli.md](docs/adr/0006-tools-and-cli.md) 与 [docs/adr/0009-qq-channel.md](docs/adr/0009-qq-channel.md)。

## 工作区与工程约定

默认 state dir `~/.llaia/`，可用 `--config-dir` 覆盖：

```
~/.llaia/
  config.toml
  .env                       # 可选，API key 等敏感配置（支持 ${VAR} 引用）
  logs/
  workspace/                 # agent 家目录（固定）：SOUL.md / USER.md / MEMORY.md / reminder.md / sessions.db / uploads/ / subagent/
```

- 配置格式：toml，命名式 section（`[provider.<id>]` / `[provider.<id>.<model_alias>]` / `[agent.<alias>]` / `[webui]` / `[channels.<qq|telegram|dingtalk|wechat|mail|feishu>]` / `[tools.terminal]` / `[tools.search]` / `[tools.tavily]` / `[tools.baidu]` / `[tools.brave]` / `[runtime]`）

- `workspace`（agent 家目录，固定）同时作为 state dir；文件/终端工具的实时作用域是 `workspace_root`，可被 `/move` 切换（详见「持久化」）

- 错误处理：`anyhow::Result`

- 日志：tracing，输出到文件 + stderr；`log.dir` 未配置时跟随 config 目录下的 `logs/`

- 单 crate

### Channels 与 WebUI 配置

IM 频道在 `[channels.<name>]` 下配置（均含 `enabled`，默认 false，及单用户安全锁字段）；Web UI 是顶层 `[webui]`（**不是** `[channels.web]`，旧写法会在 `Config::load` 时自动迁移到 `[webui]`）：

> **WebUI 配置保存的两种路径**
>
> - **结构化表单保存**（`PUT /api/config`，Config 页各表单的 Save）：用 `toml_edit` 做**定点合并**——只覆盖表单改动的 key，保留未改动段落的注释；`provider`/`agent` 子树走覆盖+删除缺失（支持表单删 provider/agent/model），`runtime`/`log`/`webui`/`channels`/`tools` 走保留缺失（保住表单未暴露的字段如 `runtime.compact_model`/`vision_model`、`provider.compat` 与这些段落的注释）。
>
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

> **QQ 媒体上传（双路径）**：图片走 base64 直传（`/v2/users/{openid}/files` + `file_data`），失败自动降级分片；文件类（pptx 等）走官方分片上传——`upload_prepare`（file\_type/file\_size/file\_name/md5/sha1/md5\_10m）→ 预签名地址 PUT 分片 → `upload_part_finish` → `/files` 带 `upload_id` 合并换 `file_info`，上限 200MB。背景：大文件 base64 单包会被 QQ 内部代理以 500/850012 拒绝（非文档错误码）。上传 body 必带 `file_name`（否则 QQ 端显示未命名）。

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

简单直接：**不用** **`cargo-release`**，发版 = 打 `git tag`，版本 bump = 手改文件。

- 版本 bump（开发阶段，纯版本号改动，跳过测试）：工作区版本号始终保持为**下一个开发版本**。`cargo build` 同步 `Cargo.lock` 后再提交：

  ```bash
  # 编辑 Cargo.toml: version = "0.3.2"   # 发完上一版后即为下一开发版
  cargo build          # 同步 Cargo.lock
  git add Cargo.toml Cargo.lock
  git commit -m "chore: bump version to 0.3.2"
  ```

- 发布某个开发版本（版本号已就位，核心产物就是 tag）：

  ```bash
  # 1. 打 tag 前先在 docs/release-notes/vX.Y.Z.md 写好简短英文 changelog
  #    （release.yml 的 release-notes job 会自动把该文件写入 GitHub release body，
  #     不会重复）
  git tag -a v0.3.2 -m "chore: release 0.3.2"
  git push origin main
  git push origin v0.3.2
  ```

- **tag 必须带** **`v`** **前缀**，否则 `release.yml` 不触发；push tag 后自动构建多平台二进制并上传 GitHub Releases。

- 不发布到 crates.io；push 由 Feihei 手动完成。

## 文档结构

- [docs/guide/](docs/guide/README.md) — **用户文档**（按功能模块）：安装、快速开始、CLI、配置、Web UI、频道、定时任务、MCP、技能、记忆与上下文、斜杠命令、工具、权限与安全、FAQ。README 只做入口，详情在此。

- [docs/adr/](docs/adr/) — 架构决策记录（ADR-0001 起持续追加，开发者向）

- [docs/glossary.md](docs/glossary.md) — 术语表

- [docs/specs/](docs/specs/) — 规格文档

- [docs/plans/](docs/plans/) — 实现计划

- [docs/issues/](docs/issues/) — 问题记录

