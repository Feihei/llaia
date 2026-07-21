# LAIA - Lightweight AI Assistant in My Favor

# 来啊 —— 我自己的极简AI小助手

## 定位

单用户私人助理，次要承担电脑操作与文件读写任务。不支持多用户体系。

详见 [docs/adr/0001-product-positioning.md](docs/adr/0001-product-positioning.md)。

## 架构

- Rust 编写，轻量、可移植
- 主控 Agent + 多个专用 Agent 协作（**委派模式**，v2 引入；v1 主 Agent 单干）
- 用户只跟主 Agent 接触，特定任务主 Agent 委派给特定子 Agent
- v1 子 Agent 不实现，所有任务由主 Agent 完成

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

- v1 只实现 OpenAI 兼容 provider（覆盖 Ollama、Llama.cpp、LMStudio 等本地端点）
- 工具调用协议：**原生优先 + 标签降级**
  - `native_tool_calling = true` → OpenAI function calling
  - `native_tool_calling = false` → system prompt 注入 `<tool_call>...</tool_call>` 协议
- v1 不做流式输出（SSE）

详见 [docs/adr/0005-provider-and-tool-calling.md](docs/adr/0005-provider-and-tool-calling.md)。

## 工具集（v1 最小集）

| 工具 | 用途 |
|---|---|
| `file_read` / `file_write` / `file_edit` | 文件读写、精确修改 |
| `terminal` | 终端命令（含 ls/grep 等，不单列） |
| `web_fetch` | 获取网页 |
| `tavily_search` | 搜索（需 api_key） |
| `memory_write` | 写 MEMORY.md |

终端命令安全：配置项控制（`none` / `whitelist` 默认 / `always`）。

CLI 子命令：`laia chat`（默认）/ `laia config` / `laia doctor` / `laia remember`。
斜杠命令：`/new` `/exit` `/compact` `/clear` `/remember` `/config` `/help`。

详见 [docs/adr/0006-tools-and-cli.md](docs/adr/0006-tools-and-cli.md)。

## 工作区与工程约定

默认工作区 `~/.laia/`，可配置：

```
~/.laia/
  config.toml
  SOUL.md
  USER.md
  MEMORY.md
  sessions.db
  logs/
```

- 配置格式：toml，命名式 section（`[provider.<id>]` / `[agent.<alias>]`）
- 错误处理：`anyhow::Result`（v1 简单优先）
- 日志：tracing，输出到文件 + stderr
- v1 单 crate，v2 视复杂度再考虑拆分

详见 [docs/adr/0007-project-structure-and-conventions.md](docs/adr/0007-project-structure-and-conventions.md)。

## 版本规划

### v1（MVP）

- CLI REPL + 本地 OpenAI 兼容 provider
- 主 Agent 单干，挂载基础工具集
- SOUL/USER/MEMORY 三份 md + sqlite 会话
- 自动/手动上下文压缩
- 不做：流式、子 Agent、Web、QQ、邮箱、MCP、skill、cron、自动环境发现

**验收标准**：能 `cargo run -- chat` 进 REPL 多轮对话 + 调本地 Ollama/LMStudio +
主 Agent 调文件读写/终端/网页/搜索 + `/remember` 写 MEMORY 下次加载 +
自动压缩 sqlite 留底 + `laia config`/`laia doctor` 可用。

### v1.5

- 加 QQ bot 频道（单聊每条必回，已有 bot 账号）

### v2

- Web 控制面板（配 provider/频道/人格/权限/MCP/技能/cron/诊断/日志）
- 子 Agent 委派机制（用户预定义子 Agent + 工具白名单）
- 邮箱频道
- MCP、skill 支持
- cron 任务
- 自动环境发现（python/nodejs/rust/unix 命令）
- 流式输出（SSE）
- 多 provider、FTS5 全文搜索、向量记忆索引

## 不需要

- 多用户系统
- 频道群聊、机器人客服
- 自己打包 python、nodejs 等环境

## 参考项目

- [openclaw](https://github.com/openclaw/openclaw) 助手类 Agent 代表性项目
- [astrbot](https://github.com/AstrBotDevs/AstrBot) 主从 Agent 协作模式、简明的 WebUI
- [zeroclaw](https://github.com/zeroclaw-labs/zeroclaw) Rust 实现架构、记忆系统、cli 命令、守护进程等，源码已克隆本地 `e:\apps\zeroclaw\zeroclaw\`
- [goose](https://github.com/aaif-goose/goose) Rust 实现的 coding agent

## 参考书籍

- [深入理解 AI Agent：设计原理与工程实践](https://github.com/bojieli/ai-agent-book) docs/深入理解-AI-Agent-李博杰-v1.2.pdf

## 设计文档

- [docs/adr/](docs/adr/) — 架构决策记录（ADR-0001 到 ADR-0007）
- [docs/glossary.md](docs/glossary.md) — 术语表
