# LLAIA (੭aᴗa)੭ — Come On~ 来啊~

> **Llaia: Local Lightweight AI Assistant, in Personal Favor**
>
> **Llaia: 符合个人品味的本地轻量AI助手**

> 📖 [中文readme](readme-zh.md)

There are loads of AI agents out there and they keep getting heavier — most are trying to cover more scenarios and more users as their communities grow. But the way we each use an agent is different. I got tired of forcing my workflow into someone else's wheels, so I just built my own.

### ✔️ What for

- Local-first: your data stays on your own machine, no cloud required
- Personal use, single user
- One main agent with a few internal subagents for delegation
- Work **out-of-box** as a bundle, no dependency

### ❌ NOT for

- Cloud / SaaS hosting
- Multi-user or team collaboration
- Multi-agent orchestration swarms
- Public-facing bots (customer-service, group-chat bot, etc.)
- Everything modular, large eco-system

### Risk Notes

LLAIA is an AI assistant with file and terminal access — LLMs can read/write files and run shell commands on your machine. That power comes with risk. LLMs do **NOT** always generate outcomes as you expect. **It's you who owns the actions.** Double-check destructive operations (deletes, overwrites, `git` pushes, etc.) before approving.

LLAIA operates in your workspace directory only by default. **ALWAYS** back up your important data before letting it operate on them, or use **git** to retrieve history.

---

## Installation

Three ways to install — full steps, Docker compose, and browser sidecar in [**docs/guide/installation.md**](docs/guide/installation.md).

| Way | One-liner |
|---|---|
| **Binary** | Grab the prebuilt binary for your arch from the [Release page](https://github.com/Feihei/llaia/releases), drop it in `PATH`, run `llaia help`. |
| **Docker** | `docker run -d --name llaia -p 51217:51217 -v llaia-data:/data ghcr.io/feihei/llaia:latest` |
| **Source** | `cargo build --release` (needs a Rust toolchain; on Windows use Git Bash) |

LLAIA does **not** bundle a model — you point it at an OpenAI-compatible or Anthropic endpoint (local Ollama / LM Studio, or cloud OpenRouter / Anthropic).

---

## Quick Start

Full walkthrough in [**docs/guide/quick-start.md**](docs/guide/quick-start.md).

```bash
llaia init                              # scaffold ~/.llaia/ (config, .env, workspace, skills)
# edit ~/.llaia/.env + config.toml, or just run serve and configure in the browser
llaia serve                             # Web UI + background channels (recommended)
llaia chat                              # terminal-only interactive chat
```

- Web UI: open **http://127.0.0.1:51217** (random token printed to logs if `webui.token` is empty).
- `llaia doctor` diagnoses provider connectivity and file integrity before you dig in.

---

## Core Features & Docs

LLAIA is modular. Each capability has a dedicated user guide — start from the one you need:

| Area | Guide | What it covers |
|---|---|---|
| **CLI** | [docs/guide/cli.md](docs/guide/cli.md) | `chat` / `serve` / `init` / `config` / `doctor` / `remember`, global `--config-dir` |
| **Configuration** | [docs/guide/configuration.md](docs/guide/configuration.md) | full `config.toml` reference (runtime, provider, agent, webui, tools, channels) |
| **Web UI** | [docs/guide/webui.md](docs/guide/webui.md) | browser entry, token auth, hot-reload settings, file upload, management APIs |
| **Channels** | [docs/guide/channels.md](docs/guide/channels.md) | QQ / Telegram / DingTalk / WeChat / Mail / Feishu, single-user locks |
| **Cron** | [docs/guide/cron.md](docs/guide/cron.md) | scheduled tasks (`cron.toml`): agent mode / tool-chain mode |
| **MCP** | [docs/guide/mcp.md](docs/guide/mcp.md) | plug in external tools & data sources |
| **Skills** | [docs/guide/skills.md](docs/guide/skills.md) | reusable workflows (`SKILL.md`, user/project level) |
| **Memory & Context** | [docs/guide/memory-and-context.md](docs/guide/memory-and-context.md) | SOUL/USER/MEMORY, sessions.db, compaction |
| **Slash Commands** | [docs/guide/slash-commands.md](docs/guide/slash-commands.md) | in-session `/` commands |
| **Tools** | [docs/guide/tools.md](docs/guide/tools.md) | built-in tools (file / terminal / web / search / memory / delegate) |
| **Permissions & Safety** | [docs/guide/permissions.md](docs/guide/permissions.md) | permission profiles, interactive approval, hard boundaries |
| **Troubleshooting** | [docs/guide/faq.md](docs/guide/faq.md) | common errors & FAQ |

---

## Reference

- [openclaw](https://github.com/openclaw/openclaw) - Most popular assistant Agent
- [astrbot](https://github.com/AstrBotDevs/AstrBot) - Local Agent with intuitive WebUI
- [zeroclaw](https://github.com/zeroclaw-labs/zeroclaw) - Lightweight agent in rust
- [goose](https://github.com/aaif-goose/goose) - coding agent in rust
- [nanobot](https://github.com/HKUDS/nanobot) - Lightweight personal agent framework
- [pi](https://github.com/earendil-works/pi)- minimal agent toolkit
- [deepseek harness](https://github.com/deepseek-ai/deepseek-harness/)
- [深入理解 AI Agent：设计原理与工程实践](https://github.com/bojieli/ai-agent-book)
