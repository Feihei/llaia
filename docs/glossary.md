# LAIA 术语表

## Agent 相关

### 主 Agent（Main Agent）
LAIA v1 唯一的 Agent，单干所有任务。用户所有交互都直接与主 Agent 进行。
v2 引入子 Agent 后，主 Agent 负责委派与结果整合。
配置 section：`[agent.main]`。

### 子 Agent（Sub Agent）
v2 引入的概念。由用户通过 Web 面板预定义（起名、写提示词、勾选工具白名单）。
主 Agent 遇到特定任务时整体委派给对应子 Agent，子 Agent 起独立会话执行，
结果回传主 Agent 整合后再回用户。v1 不存在子 Agent。

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
LLM 服务提供方抽象。v1 只实现 `OpenAiCompatible` 一种，配置 section：`[provider.default]`。
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
主 Agent 可调用的能力。v1 最小集：`file_read`、`file_write`、`file_edit`、
`terminal`、`web_fetch`、`tavily_search`、`memory_write`。
`memory_read` 和 `session_*` 为内部实现，不暴露给 LLM。
v2 引入工具白名单，按子 Agent 过滤可见工具。

### 终端命令安全（Terminal Safety）
配置项 `[tools.terminal].confirm` 控制：
- `none`：全部直接执行
- `whitelist`（默认）：白名单内免确认，其他每次 y/n
- `always`：全部需确认

## 频道相关

### 频道（Channel）
消息出入口。v1 只有 CLI 频道，v1.5 加 QQ，v2 加 Web 面板、邮箱。
所有频道共享同一会话上下文（同用户同会话）。

### CLI REPL
`laia chat` 进入的交互式命令行。支持斜杠命令。

### 斜杠命令（Slash Command）
REPL 内以 `/` 开头的指令。v1 清单：
`/new` `/exit` `/compact` `/clear` `/remember` `/config` `/help`。

## CLI 子命令

### `laia chat`
进入交互式 REPL。默认子命令（`laia` 无参数等价于此）。

### `laia config`
打印当前配置。

### `laia doctor`
诊断 provider 连通性、文件完整性。

### `laia remember "<text>"`
一次性写 MEMORY.md，等价于 REPL 内 `/remember`。

## 版本规划

### v1（MVP）
单主 Agent + CLI + 本地 provider + 基础工具 + 三份 md + sqlite 会话。
验收标准见 ADR-0006 与 grilling 第六轮 Q34。

### v1.5
加 QQ bot 频道（单聊每条必回，已有 bot 账号）。

### v2
Web 控制面板、子 Agent 委派、邮箱频道、MCP、skill、cron、自动环境发现、
流式输出、多 provider、FTS5 全文搜索、向量记忆索引。

## 工程约定

### 命名式配置 Section
`[provider.<id>]` / `[agent.<alias>]` 结构。v1 只认 `default` 和 `main`，
v2 加多 provider/多 agent 时只增 section 不改 schema。

### 错误处理
v1 用 `anyhow::Result` 全局兜底。v2 视需要对外 API 引入 `thiserror`。

### 日志
tracing，v1 只配一个 fmt layer 输出到文件（`~/.laia/logs/`）+ stderr。

### 工作区目录
默认 `~/.laia/`，用户可在 `[workspace].dir` 配置中改到别处。
```
~/.laia/
  config.toml
  SOUL.md
  USER.md
  MEMORY.md
  sessions.db
  logs/
```

## 参考项目

### zeroclaw
Rust 实现的 AI Agent 项目，源码克隆于 `e:\apps\zeroclaw\zeroclaw\`。
LAIA 参考其：会话 sqlite schema（ACP 那套）、Provider trait 设计、
工具调用混合策略（原生优先 + 标签降级）、CLI 子命令形态、命名式配置 section。
LAIA 相对 zeroclaw 的简化：单 crate、单 provider、单 agent、无流式、无多频道并发。

### AstrBot
Python 实现的主从 Agent 协作项目，README 提到"类似 AstrBot"。
LAIA 实际采用委派模式（C），不是 AstrBot 的编排者模式（A）。

### goose
Rust 实现的 coding agent，README 列为参考。
