# LLAIA (੭aᴗa)੭ — Come On~ 来啊~

> **Llaia: Local Lightweight AI Assistant, in Personal Favor**
>
> **Llaia: 符合个人品味的本地轻量AI助手**

> 📖 [readme EN](readme.md)

市面上的 AI Agent 越来越多，也越做越重——大多数都在随着社区壮大去覆盖更多场景、服务更多用户。但每个人用 Agent 的方式都不一样。我受够了把自己的工作流硬塞进别人的轮子，于是干脆自己造了一个。

### ✔️ 适合做什么

- 本地优先：数据留在你自己的机器上，不依赖云端
- 个人使用，单用户
- 一个主 Agent 配几个内部子 Agent 负责委派

### ❌ 不适合做什么

- 云端 / SaaS 托管
- 多用户或团队协作
- 多智能体编排集群
- 对外公开的机器人（客服、群聊机器人等）

### 风险须知

LLAIA 是一个具备文件与终端访问能力的 AI 助手——大语言模型可以在你的机器上读写文件、运行 shell 命令。这份能力伴随风险。大模型**并不**总能按你预期产生结果。**行动由你负责。** 在批准删除、覆盖、`git` 推送等破坏性操作前，务必再三确认。

LLAIA 默认只在你的工作区目录内运行。**务必**在让它操作重要数据之前先做好备份，或用 **git** 保留历史。

---

## 安装

三种安装方式——完整步骤、Docker compose、浏览器旁挂，见 [**docs/guide/installation.md**](docs/guide/installation.md)。

| 方式 | 一行命令 |
|---|---|
| **预编译二进制** | 从 [Release 页面](https://github.com/Feihei/llaia/releases) 下载对应架构的预编译二进制，放进 `PATH`，运行 `llaia help`。 |
| **Docker** | `docker run -d --name llaia -p 51217:51217 -v llaia-data:/data ghcr.io/feihei/llaia:latest` |
| **源码** | `cargo build --release`（需要 Rust 工具链；Windows 下用 Git Bash） |

LLAIA **不**内置模型——你需要把它指向一个 OpenAI 兼容或 Anthropic 的接口（本地 Ollama / LM Studio，或云端 OpenRouter / Anthropic）。

---

## 快速开始

完整指引见 [**docs/guide/quick-start.md**](docs/guide/quick-start.md)。

```bash
llaia init                              # 生成 ~/.llaia/（配置、.env、工作区、技能）
# 编辑 ~/.llaia/.env + config.toml，或直接运行 serve 后在浏览器里配置
llaia serve                             # Web UI + 后台频道（推荐）
llaia chat                              # 仅终端的交互式对话
```

- Web UI：打开 **http://127.0.0.1:51217**（若 `webui.token` 为空，随机 token 会打印到日志）。
- 深入排查前，先用 `llaia doctor` 诊断 provider 连通性与文件完整性。

---

## 核心功能与文档

LLAIA 是模块化的。每个能力都有专门的用户指南——从你需要的那篇看起：

| 领域 | 指南 | 内容覆盖 |
|---|---|---|
| **命令行 CLI** | [docs/guide/cli.md](docs/guide/cli.md) | `chat` / `serve` / `init` / `config` / `doctor` / `remember`，全局 `--config-dir` |
| **配置** | [docs/guide/configuration.md](docs/guide/configuration.md) | 完整 `config.toml` 参考（runtime、provider、agent、webui、tools、channels） |
| **Web UI** | [docs/guide/webui.md](docs/guide/webui.md) | 浏览器入口、token 鉴权、热重载设置、文件上传、管理 API |
| **频道 Channels** | [docs/guide/channels.md](docs/guide/channels.md) | QQ / Telegram / 钉钉 / 微信 / 邮箱 / 飞书，单用户锁 |
| **定时任务 Cron** | [docs/guide/cron.md](docs/guide/cron.md) | 计划任务（`cron.toml`）：agent 模式 / 工具链模式 |
| **MCP** | [docs/guide/mcp.md](docs/guide/mcp.md) | 接入外部工具与数据源 |
| **技能 Skills** | [docs/guide/skills.md](docs/guide/skills.md) | 可复用工作流（`SKILL.md`，用户级 / 项目级） |
| **记忆与上下文** | [docs/guide/memory-and-context.md](docs/guide/memory-and-context.md) | SOUL/USER/MEMORY、sessions.db、压缩、dream |
| **斜杠命令** | [docs/guide/slash-commands.md](docs/guide/slash-commands.md) | 会话内 `/` 命令 |
| **工具** | [docs/guide/tools.md](docs/guide/tools.md) | 内置工具（文件 / 终端 / 网页 / 搜索 / 记忆 / 委派） |
| **权限与安全** | [docs/guide/permissions.md](docs/guide/permissions.md) | 权限档案、交互式批准、硬性边界 |
| **故障排查** | [docs/guide/faq.md](docs/guide/faq.md) | 常见错误与 FAQ |

---

## 参考

- [openclaw](https://github.com/openclaw/openclaw) - 最受欢迎的助手 Agent
- [astrbot](https://github.com/AstrBotDevs/AstrBot) - 带直观 WebUI 的本地 Agent
- [zeroclaw](https://github.com/zeroclaw-labs/zeroclaw) - Rust 编写的轻量 agent
- [goose](https://github.com/aaif-goose/goose) - Rust 编写的编码 agent
- [深入理解 AI Agent：设计原理与工程实践](https://github.com/bojieli/ai-agent-book) - 一本书
