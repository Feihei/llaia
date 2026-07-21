# (੭˃ᴗ˂)੭ LAIA — Come On~ 来啊~

> **Lightweight AI Assistant in My Favor**  
> 我自己的极简 AI 小助手

LAIA 是一个为单用户打造的私人 AI 助理，轻量、本地优先，帮你读写文件、运行命令、搜索信息、记录记忆——一切尽在终端，简单直接。

## 核心亮点

- **单用户专属**：没有多用户体系，只为一个人服务
- **本地优先**：接入 Ollama、LM Studio 等本地模型，数据不离本机
- **持久记忆**：自动维护人格设定、用户画像与长期记忆，越用越懂你
- **上下文智能压缩**：会话无限延续，旧记录自动归档摘要
- **极简工具集**：文件读写、终端命令、网页抓取、网络搜索、记忆写入——够用就好

## 快速开始

```bash
cargo run -- chat
```

更多命令：`laia config`、`laia doctor`、`laia remember`

斜杠命令：`/new`、`/compact`、`/remember`、`/help`

## 版本规划

| 版本 | 状态 | 重点 |
|---|---|---|
| **v1** | 已完成 | CLI REPL + 本地 OpenAI 兼容 Provider + 主 Agent 单干 + 基础工具集 + 记忆持久化 |
| **v1.5** | 开发中 | QQ Bot 频道接入（腾讯官方开放平台，C2C 单聊） |
| **v2** | 规划中 | Web 面板、子 Agent 委派、MCP/Skill 支持、流式输出、邮箱频道、cron 任务 |

**v1 不做**：多用户、群聊、流式输出、自动环境发现、自己打包 Python/Node.js 环境。

## QQ 频道（v1.5）

在 `~/.laia/config.toml` 加 `[channels.qq]` 节即可启用 QQ 单聊：

```toml
[channels.cli]
enabled = true

[channels.qq]
enabled = true
app_id = "你的 app_id"
token = "你的 token"
bot_qq = "机器人的 QQ 号"
confirm_mode = "always"   # always（默认，跳过有副作用工具）/ whitelist / none
```

- 接入腾讯官方 QQ 开放平台，走 WebSocket + HTTPS，无需公网回调
- CLI 和 QQ 同进程，共享同一 session（跨频道接续对话）
- 长回复自动分片（≤1800 字符/片），代码块跨片时闭合重开
- QQ 下默认禁用 `file_write` / `file_edit` / `terminal` / `memory_write` 等有副作用的工具

详见 [docs/adr/0009-qq-channel.md](docs/adr/0009-qq-channel.md)。

## 参考

- [openclaw](https://github.com/openclaw/openclaw) — 助手类 Agent 代表性项目
- [astrbot](https://github.com/AstrBotDevs/AstrBot) — 主从 Agent 协作、简明 WebUI
- [zeroclaw](https://github.com/zeroclaw-labs/zeroclaw) — Rust 实现、记忆系统、CLI 命令
- [goose](https://github.com/aaif-goose/goose) — Rust coding agent
- [深入理解 AI Agent：设计原理与工程实践](https://github.com/bojieli/ai-agent-book)

---

*内部技术细节与开发文档见 [agents.md](agents.md)。*
