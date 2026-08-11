# LLAIA (੭aᴗa)੭ — Come On~ 来啊~

> **Llaia: Local Lightweight AI Assistant, in Personal Favor**
>
> **Llaia: 符合个人品味的本地轻量AI助手**

There are loads of AI agents out there and they keep getting heavier — most are trying to cover more scenarios and more users as their communities grow. But the way we each use an agent is different. I got tired of forcing my workflow into someone else's wheels, so I just built my own.

### ✔️ What for

- Local-first: your data stays on your own machine, no cloud required
- Personal use, single user
- One main agent with a few internal subagents for delegation

### ❌ NOT for

- Cloud / SaaS hosting
- Multi-user or team collaboration
- Multi-agent orchestration swarms
- Public-facing bots (customer-service, group-chat bot, etc.)

### Risk Notes

LLAIA is an AI assistant with file and terminal access — LLMs can read/write files and run shell commands on your machine. That power comes with risk. LLMs do **NOT** always generate outcomes as you expect. **It's you who owns the actions.** Double-check destructive operations (deletes, overwrites, `git` pushes, etc.) before approving.

LLAIA operates in your workspace directory only by default. **ALWAYS** back up your important data before letting it operate on them, or use **git** to retrieve history.

---

## Installation

### Binary

Grab the prebuilt binary for your architecture from the [Release page](https://github.com/Feihei/llaia/releases), drop it in your system `PATH`, and run:

```bash
llaia help
```

### Docker

The official image is published to **ghcr.io/feihei/llaia:latest** (~280 MB, Debian bookworm-slim).

It bundles a practical toolchain for the agent's terminal tool: `bash`, `curl`, `wget`, `git`, `jq`, `unzip`, `python3` (with `pip`), and [`uv`](https://github.com/astral-sh/uv) for fast Python package management.

```bash
docker run -d --name llaia \
  -p 51217:51217 \
  -v llaia-data:/data \
  ghcr.io/feihei/llaia:latest
```

On first launch the container auto-generates a minimal config under `/data` that enables the Web UI. Grab the access token from the container log, then open the browser and configure:

```bash
docker logs llaia | grep -i token
# → open http://127.0.0.1:51217
```

**docker compose:**

```yaml
# compose.yml
services:
  llaia:
    image: ghcr.io/feihei/llaia:latest
    container_name: llaia
    restart: unless-stopped
    ports:
      - "51217:51217"
    volumes:
      - llaia-data:/data

volumes:
  llaia-data:
```

**Browser automation sidecar** (page rendering, screenshots, form filling — optional):

```yaml
# compose.browser.yml (extend compose.yml)
services:
  browser:
    image: browserless/chrome:latest
    restart: unless-stopped
    ports:
      - "3000:3000"
    environment:
      - CONNECTION_TIMEOUT=600000
```

The agent can talk CDP to `http://browser:3000` from within the compose network. This is a starting point — wire it up yourself with Playwright or raw CDP.

### Build from source

Requires a **Rust toolchain** ([rustup](https://rustup.rs)). On Windows, run from **Git Bash**.

```bash
git clone https://github.com/Feihei/llaia.git && cd llaia
cargo build --release
# binary at ./target/release/llaia
```

---

## Getting Started

### 1. Initialize

```bash
llaia init
```

This creates the data directory under `~/.llaia/`:

```
~/.llaia/
 ├─ .env               # secrets (API keys etc.); fill in real values
 ├─ config.toml        # main config (commented template)
 ├─ cron.toml          # scheduled tasks (all commented by default)
 ├─ mcp.toml           # MCP server config (all commented by default)
 ├─ logs/              # runtime logs
 ├─ skills/            # user & project skills
 └─ workspace/         # agent reads/writes files here by default
     ├─ SOUL.md        # agent persona
     ├─ USER.md        # your info and preferences
     ├─ MEMORY.md      # long-term memory
     ├─ uploads/       # uploaded-file staging
     ├─ subagent/      # sub-agent workspaces
     └─ sessions.db    # conversation history (SQLite)
```

- **Idempotent**: existing files are not overwritten. Use `--force` to regenerate.
- Use `--config-dir <path>` to place the data directory elsewhere.

### 2. Configure a provider

LLAIA does not bundle a model — point it at one:

| Type | Examples | Privacy |
|---|---|---|
| Local | [Ollama](https://ollama.com), [LM Studio](https://lmstudio.ai), [llama.cpp](https://github.com/ggerganov/llama.cpp) | Data stays local |
| Cloud | [OpenRouter](https://openrouter.ai), any OpenAI-compatible endpoint, Anthropic Messages API | Text sent off-machine |

**A. Web UI (recommended for beginners)**
Start the service first (see Step 3), then fill in provider settings from the browser.

**B. Edit config manually**
Open `~/.llaia/config.toml`, uncomment `[provider.default]` and `[agent.main]`:

```toml
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"
api_key = ""                       # usually empty for local Ollama

[provider.default.qwen]
model = "qwen2.5:7b"
native_tool_calling = false
context_size = 32768

[agent.main]
model = "default.qwen"             # references provider.<id>.<model_alias>
```

For cloud Anthropic, use `type = "anthropic"` and add `max_tokens` on the model entry. Set `fallback = [...]` under `[agent.main]` to auto-degrade to backup models.

### 3. Launch

```bash
llaia serve       # Web UI + background channels (recommended)
llaia chat        # terminal-only interactive chat
```

Web UI: open `http://127.0.0.1:51217`. If `webui.token` is left empty, a random token is printed to the log on first launch.

### CLI commands

| Command | What it does |
|---|---|
| `llaia chat` | Terminal chat mode |
| `llaia serve` | Web UI + optional messaging channels |
| `llaia init [--force]` | Scaffold data directory + default templates |
| `llaia config` | Print the effective config |
| `llaia doctor` | Diagnose provider connectivity and file integrity |
| `llaia remember "<text>"` | Append a line to long-term memory |

### In-session commands

`/new` `/exit` `/compact` `/clear` `/remember` `/config` `/provider` `/help`

---

## Configuration Guide

### Channels

LLAIA can reach you beyond the Web UI. All channels are **disabled by default**; enable them in `config.toml`. Credentials go in `.env` via `${VAR}` references.

| Channel | Config section | Credentials |
|---|---|---|
| QQ (official bot, C2C) | `[channels.qq]` | `app_id` / `app_secret` |
| Telegram | `[channels.telegram]` | bot token from @BotFather (long polling, no public IP needed) |
| DingTalk | `[channels.dingtalk]` | `client_id` / `client_secret` (Stream Mode, no public IP needed) |
| WeChat (ClawBot / ilink) | `[channels.wechat]` | first launch prints a QR-code link — scan with your phone |

Each channel has an `allow_*` field as a single-user safety lock.

### Cron (scheduled tasks)

Define repeating or one-shot tasks in `cron.toml`. Each task wakes the main agent or runs a tool chain directly. Results can be pushed to a specific channel.

```toml
# cron.toml
[[tasks]]
name = "daily reminder"
schedule = "0 9 * * *"
prompt = "Good morning! Summarize today's schedule."
channel = "qq"
```

See [docs/adr/0013-cron-scheduling.md](docs/adr/0013-cron-scheduling.md) for details.

### MCP (Model Context Protocol)

Connect external tools and data sources via MCP servers. Define servers in `mcp.toml`:

```toml
# mcp.toml
[servers.filesystem]
command = "npx"
args = ["-y", "@anthropic/mcp-filesystem", "/path/to/data"]
enabled = true
```

Tools are named `<server>__<tool_name>` (e.g. `filesystem__read_file`). See [docs/adr/0014-mcp-client.md](docs/adr/0014-mcp-client.md).

### Skills

Skills extend the agent with reusable workflows, domain knowledge, and tool integrations. Drop skill folders into `~/.llaia/skills/` (user-level) or `.workbuddy/skills/` (project-level).

```
~/.llaia/skills/
 └── my-skill/
     └── SKILL.md       # skill definition (prompt + metadata)
```

The agent loads them at startup — no restart needed for project-level skills. See [docs/adr/0015-skill-framework.md](docs/adr/0015-skill-framework.md).

---

## Reference

- [openclaw](https://github.com/openclaw/openclaw) - Most popular assistant Agent
- [astrbot](https://github.com/AstrBotDevs/AstrBot) - Local Agent with intuitive WebUI
- [zeroclaw](https://github.com/zeroclaw-labs/zeroclaw) - Lightweight agent in rust
- [goose](https://github.com/aaif-goose/goose) - coding agent in rust
- [深入理解 AI Agent：设计原理与工程实践](https://github.com/bojieli/ai-agent-book) - A Book
