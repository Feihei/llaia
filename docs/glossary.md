# LLAIA 术语表

## Agent 相关

### 主 Agent（Main Agent）
LLAIA P1 唯一的 Agent，单干所有任务。用户所有交互都直接与主 Agent 进行。
P2 引入子 Agent 后，主 Agent 负责委派与结果整合。
配置 section：`[agent.main]`。

### 子 Agent（Sub Agent）
P2 引入的概念。由用户通过 Web 面板预定义（起名、写提示词、勾选工具白名单）。
主 Agent 遇到特定任务时整体委派给对应子 Agent，子 Agent 起独立会话执行，
结果回传主 Agent 整合后再回用户。P1 不存在子 Agent。

### 委派（Delegation）
主 Agent 把特定任务整体甩给子 Agent 独立完成的模式。
区别于"编排者模式"（主 Agent 拆任务、调度子 Agent 执行）和"人格切换模式"（同时只跑一个 Agent）。
委派路由采用混合策略：默认由主 Agent 的 LLM 判断，可被强制指令覆盖。

### Agent 循环（Agent Loop）
主 Agent 处理一次用户输入的流程：接收输入 → 拼 system prompt（含 SOUL/USER/MEMORY）→
调 provider → 若返回工具调用则执行工具 → 把工具结果塞回上下文 → 再调 provider →
直到返回纯文本回复 → 输出给用户。

## 持久化相关

### SOUL.md
Agent 人格设定文件。结构：
```markdown
# 人格
<自由文本>

# 行为准则
- <规则>

# 语气
<对话风格>
```
用户编辑，Agent 启动时加载拼入 system prompt。永驻上下文，压缩时保留。

### USER.md
用户画像文件。结构：
```markdown
# 基本信息
- 姓名：
- 时区：Asia/Shanghai

# 身份绑定
- qq: <openid>
- email: <addr>
- web: <username>

# 偏好
- <语言、技术栈、习惯>
```
用户编辑 + Agent 可写入偏好。任一频道身份命中清单即认作 owner。永驻上下文。

### MEMORY.md
长期事实记忆文件。结构：
```markdown
- [2026-07-21] <条目内容>
```
Agent 写入，触发方式：手动 `/remember` 或主 Agent 自动判断。
限定总 token 数，超限时先备份再由 LLM 去重压缩覆写。

### sessions.db
sqlite 数据库文件，存会话记录。是会话历史的 source of truth。
schema 见 ADR-0004。上下文压缩时旧消息从内存移除但 sqlite 留底。

### 会话（Session）
一次连续对话。由 `session_uuid` 标识，跨频道共享上下文。
同一用户同一会话——CLI 说的话 QQ 也能看到接续。
手动 `/new` 开新会话，或上下文超阈值时自动压缩。

### 上下文（Context）
当前会话中拼给 provider 的消息序列。含 system prompt（SOUL/USER/MEMORY）+ 历史消息。
占用超过 `context_threshold`（默认 70%）时触发压缩。

### 上下文压缩（Context Compaction）
策略：关键消息保留（SOUL/USER 永留、首条用户消息留、工具调用结果可丢），
其余旧消息被 LLM 摘要成一段替换。压缩后旧消息从内存移除，sqlite 留底。
可手动 `/compact` 触发，或 `/clear` 直接清空内存上下文（sqlite 留底）。

## Provider 相关

### Provider
LLM 服务提供方抽象。P1 只实现 `OpenAiCompatible` 一种，配置 section：`[provider.default]`。
Provider trait 含 `native_tool_calling: bool` 能力声明，决定工具调用协议走原生还是标签降级。

### OpenAiCompatible Provider
兼容 OpenAI Chat Completions API 的 provider，覆盖 Ollama、Llama.cpp、LMStudio 等本地端点。
配置项：`base_url`、`api_key`、`model`、`native_tool_calling`。

### 原生工具调用（Native Tool Calling）
走 OpenAI function calling 协议。`native_tool_calling = true` 时启用。
provider 返回结构化的 `tool_calls` 字段，无需文本解析。

### 标签降级（Prompt-Guided Tool Calling）
`native_tool_calling = false` 时启用。system prompt 注入协议说明，
模型用 `<tool_call>{"name":"...","arguments":{...}}</tool_call>` 包裹调用，
回复文本由解析器抽取。兼容不支持 function calling 的本地模型。

## 工具相关

### Tool
主 Agent 可调用的能力。P1 最小集：`file_read`、`file_write`、`file_edit`、
`terminal`、`web_fetch`、`search`、`memory_write`。
`memory_read` 和 `session_*` 为内部实现，不暴露给 LLM。
P2 引入工具白名单，按子 Agent 过滤可见工具。

### 终端命令安全（Terminal Safety）
配置项 `[tools.terminal].confirm` 控制：
- `none`：全部直接执行
- `whitelist`（默认）：白名单内免确认，其他每次 y/n
- `always`：全部需确认

## 频道相关

### 频道（Channel）
消息出入口。P1 只有 CLI 频道，P1.5 加 QQ，P2 加 Web 面板、邮箱。
所有频道共享同一会话上下文（同用户同会话）。

### CLI REPL
`llaia chat` 进入的交互式命令行。支持斜杠命令。

### 斜杠命令（Slash Command）
REPL 内以 `/` 开头的指令。P1 清单：
`/new` `/exit` `/compact` `/clear` `/remember` `/config` `/help`。

## CLI 子命令

### `llaia chat`
进入交互式 REPL。默认子命令（`llaia` 无参数等价于此）。

### `llaia config`
打印当前配置。

### `llaia doctor`
诊断 provider 连通性、文件完整性。

### `llaia remember "<text>"`
一次性写 MEMORY.md，等价于 REPL 内 `/remember`。

## 版本规划

### P1（MVP）
单主 Agent + CLI + 本地 provider + 基础工具 + 三份 md + sqlite 会话。
验收标准见 ADR-0006。

### P1.5
加 QQ bot 频道（单聊每条必回，已有 bot 账号）+ 流式输出。

### P2
子 Agent 委派、流式交互增强、进程运维、Web 控制面板（配置为主）、多媒体收发。

### P3
QQ 能力边界重塑（workspace 隔离 + 命令拦截）、`llaia init` 引导、cron 定时任务、MCP client 接入、Skill 技能框架。详见 [plan.md](plan.md#p3--能力扩展与生态接入)。

## P3 相关术语

### workspace 隔离（按 agent）
P3-a 引入。每个 agent 有独立工作目录：
- 主 agent：`~/.llaia/workspace/`
- 子 agent：`~/.llaia/workspace/subagent/<name>/`

agent 的 file/terminal 工具只能在自己 workspace 内操作（类似容器挂载目录）。
主 agent 可读 `subagent/` 子目录（整合子 agent 产出），但不可写（`.inbox/` 例外由系统层处理）。
config.toml / 敏感信息 / logs 在 `~/.llaia/` 根目录，agent 工具不可访问。
**不按 channel 隔离**：channel 只是 I/O 入口，同一 agent 跨 channel 共享 workspace。
不按 UMO（会话）隔离，单用户场景无意义。详见 [ADR-0011](adr/0011-qq-capability-boundary.md)。

### 跨 workspace 协作
P3-a 引入。按 agent 隔离后，三个协作机制：
1. **主→子文件传递**：delegate 工具的 `file_paths` 参数，系统层把主 agent workspace 内文件复制到子 agent `.inbox/`（每次委派先清空再复制）
2. **子→主产出回传**：delegate 返回值 `{text, output_files}`，output_files 从子 agent 本次 turn 工具调用记录提取 file_write/file_edit 路径去重；主 agent 用 `file_read subagent/<name>/<path>` 整合
3. **USER.md 同步**：启动时从主 agent workspace 复制覆盖到子 agent；子 agent memory_write 拒写 USER.md（身份绑定统一在主 agent）；SOUL.md 各自独立

### 命令拦截（command_policy）
P3-a 引入。terminal 工具有两个正交维度：

**命令策略**（哪些命令允许执行，三档，对所有 agent 生效不区分 channel）：
- `blacklist`（默认）：黑名单拦截（rm -rf / sudo / shutdown 等），其余放行
- `whitelist`：仅白名单内命令放行
- `none`：全放行（CLI 交互场景默认，向后兼容）

**路径防御**（命令能访问哪些路径，三层深度防御，防 LLM 误操作不防恶意用户）：
1. shell 包装拒绝：词法解析拦截 `bash -c` / `eval` / `$()` / 反引号等任意代码执行
2. 路径白名单（主防御）：路径 token canonicalize 后必须 `starts_with` 当前 agent workspace，不存在路径回溯祖先检查
3. 路径黑名单（兜底）：跨平台危险目录前缀（Linux `/root`/`/usr`/`/etc`、macOS `/System`/`/Library`、Windows `C:\Windows`/`C:\Program Files` 等）

terminal 工具的 cwd 固定为当前 agent workspace 根。file 工具复用第二三层（不需要 shell 词法解析）。详见 [ADR-0011](adr/0011-qq-capability-boundary.md)。

### confirm_mode（P3-a 重定义）
P3-a 重定义为**全局开关**（不再 per-channel）：
- `none`（新默认）：不弹确认，agent 工具受 workspace 边界 + 命令策略约束即可
- `always`：所有有副作用工具调用前弹确认（CLI 弹 stdin，QQ/Web 拒绝并提示）
- `session`：首次确认后 N 分钟内放行同类工具

`whitelist` 模式废弃，加载时 warn + fallback 到 `none`。

### audit.log
P3-a 引入。`~/.llaia/logs/audit.log` 记录所有有副作用工具的调用（timestamp / agent / channel / tool / args / result），文本追加，不做链式哈希。

### llaia init
P3-b 引入。`llaia init [--workspace <path>] [--force]` 生成 `~/.llaia/` 目录骨架 + config.toml/SOUL.md/USER.md/MEMORY.md 模板，纯模板生成不交互问答，引导用户运行 `llaia serve` 进 WebUI 配置。详见 [ADR-0012](adr/0012-llaia-init.md)。

### cron 任务（Cron Task）
P3-c 引入。定时任务定义在 `~/.llaia/cron.toml`，双模式：
- `mode = "agent"`：到点唤醒主 agent，注入提示词走完整 agent 循环
- `mode = "tools"`：到点直接按预定义 steps 顺序执行工具链，不消耗 LLM token

结果推送到指定 channel（qq/cli/web）。agent 模式开独立 session，不污染用户当前会话。详见 [ADR-0013](adr/0013-cron-scheduling.md)。

### MCP（Model Context Protocol）
P3-d 引入。Anthropic 提出的开放协议，用于 LLM 与外部工具/数据源标准化通信。
LLAIA 仅作为 **client**（消费外部 MCP server 的工具），不作为 server 暴露自身能力。
**协议层自实现**（不引入 `rmcp` 等 SDK，借鉴 zeroclaw 实现模式）。支持 stdio + HTTP（streamable）+ SSE 三种 transport。配置在 `~/.llaia/mcp.toml`。详见 [ADR-0014](adr/0014-mcp-client.md)。

### MCP 工具适配（McpTool）
MCP server 通过 `tools/list` 返回的工具，包装成 LLAIA `Tool` trait 实现，加 `<server_id>__` 前缀（双下划线）避免与内置工具冲突（如 `filesystem__read_file` vs 内置 `file_read`）。默认 `requires_confirm = true`，受 P3-a 边界约束。input_schema 用 `Arc<serde_json::Value>` 共享避免深拷贝。调用走 `McpRegistry::call_tool(prefixed_name, args)` 中央路由。

### Skill
P3-e 引入（依赖 P3-d）。`~/.llaia/skills/<name>/SKILL.md` 定义的技能包（markdown + YAML frontmatter，对齐 OpenAI Codex CLI / Anthropic Claude Skills / AstrBot 业界标准）。
frontmatter 含 `name` / `description` / `duration`（turn / session）/ `tools`（prompt 提示，不控制挂载）。
body 是给 LLM 的详细指令。**Progressive Disclosure**：system prompt 只注 name+description，LLM 用时自己 file_read 完整 SKILL.md。
触发方式：agent 判断（LLM 看 name+description 自行决定），不做关键词匹配。详见 [ADR-0015](adr/0015-skill-framework.md)。

## 工程约定

### 命名式配置 Section
`[provider.<id>]` / `[agent.<alias>]` 结构。P1 只认 `default` 和 `main`，
P2 加多 provider/多 agent 时只增 section 不改 schema。

### 错误处理
P1 用 `anyhow::Result` 全局兜底。P2 视需要对外 API 引入 `thiserror`。

### 日志
tracing，P1 只配一个 fmt layer 输出到文件（`~/.llaia/logs/`）+ stderr。

### 工作区目录
默认 `~/.llaia/`，用户可在 `[workspace].dir` 配置中改到别处。
```
~/.llaia/                              # 根目录（敏感配置 + 进程状态）
  config.toml                         # 主配置（详见 ADR-0008）
  cron.toml                           # cron 任务定义（P3-c 引入，可选）
  mcp.toml                            # MCP server 配置（P3-d 引入，可选）
  llaia.pid                           # PID 文件（P2-d 引入）
  logs/                               # 日志目录（tracing + audit.log）
  workspace/                          # 主 agent 工作区（P3-a 引入）
    SOUL.md                           # 主 agent 人格
    USER.md                           # 用户画像
    MEMORY.md                         # 长期记忆
    sessions.db                       # 主 agent 会话历史
    uploads/                          # 用户上传媒体（P2-e 多媒体引入）
    subagent/                         # 子 agent 工作区集合（P3-a 引入）
      <name>/                         # 如 coder / searcher
        SOUL.md / USER.md / MEMORY.md / sessions.db / ...
  skills/                             # Skill 技能包（P3-e 引入，可选）
    <name>/SKILL.md                   # markdown + YAML frontmatter
  skills.json                         # Skill active 开关（P3-e 引入，可选）
```
`~/.llaia/` 根只放配置 + 敏感信息，agent 工具不可访问。
agent 工作文件在 `~/.llaia/workspace/`（主）或 `~/.llaia/workspace/subagent/<name>/`（子）。
`llaia init` 生成基础骨架（不含 cron.toml/mcp.toml/skills/，这些按需创建）。

## 参考项目

### zeroclaw
Rust 实现的 AI Agent 运行时。LLAIA 参考其：会话 sqlite schema、Provider trait 设计、
工具调用混合策略（原生优先 + 标签降级）、CLI 子命令形态、命名式配置 section、
deny-by-default 安全模型、命令白名单（具体到 `/usr/bin/curl`）、OS 沙箱（Landlock/Bubblewrap/Seatbelt）、链式审计回执。
LLAIA 相对 zeroclaw 的简化：单 crate、单用户（无多租户）、不引入 OS 沙箱、不做链式审计哈希。
P3-a 的 workspace 隔离 + 命令黑名单是 zeroclaw 安全模型的轻量化版本。详见 [ADR-0011](adr/0011-qq-capability-boundary.md)。

### AstrBot
Python 实现的 IM-native 智能体平台（QQ/飞书/钉钉/Telegram 等多平台）。
LLAIA 参考其：管理员/普通用户区分、UMO（统一消息来源）标识、每会话 workspace 隔离、
危险命令黑名单（rm -rf / sudo / shutdown 等）、`CronMessageEvent` + `background_task` 定时任务方案。
LLAIA 不采用 AstrBot 的编排者模式（A），而是委派模式（C）。
P3-a 的 QQ 能力边界主要借鉴 AstrBot 的 workspace 隔离 + 命令拦截路线（不引入沙箱）。
P3-c 的 cron agent 模式借鉴 AstrBot 的 CronMessageEvent 思路。

### goose
Rust 实现的 coding agent，README 列为参考。
