# LLAIA (੭aᴗa)੭ — Come On~ 来啊~

> **Llaia: Local Lightweight AI Assistant, in Personal Favor**
>
> **Llaia: 符合个人品味的本地轻量AI助手**

There are loads of AI agents out there and they keep getting heavier — most are trying to cover more scenarios and more users as their communities grow. But the way we each use an agent is different. I got tired of forcing my workflow into someone else's wheels, so I just built my own.

### ✔️ What for

- Local-first: your data stays on your own machine, no cloud required
- Personal use, single user
- Only one main agent helps you, delegating to a few internal subagents when needed

### ❌ NOT for

- Cloud / SaaS hosting
- Multi-user or team collaboration
- Multi-agent orchestration swarms
- Public-facing bots (customer-service, group-chat bot, etc.)

---

### Risk Notes

LLAIA is an AI assistant with file and terminal access — LLMs can read/write files and run shell commands on your machine. That power comes with risk. LLMs do **NOT** always generate outcomes as you espect. **It's you who owns the actions.** Double-check destructive operations (deletes, overwrites, `git` pushes, etc.) before approving.

LLAIA operates in your workspace directory only by default, **ALWAYS** backup your important data before letting it operate on, or you can use **git** to retrieve histroy.

### Prerequisites

- **A provider / LLM endpoint.** LLAIA does not bundle a model — you must point it at one:
  - **Local (recommended for privacy):** [Ollama](https://ollama.com), [LM Studio](https://lmstudio.ai), or [llama.cpp](https://github.com/ggerganov/llama.cpp) — each can expose an OpenAI-compatible server on your machine.
  - **Cloud API:** [OpenRouter](https://openrouter.ai) or any other OpenAI-compatible endpoint, plus **Anthropic** natively (Messages API). Needs an API key; conversation text is sent off-machine.
- **Bash toolchain — strongly recommended.** LLAIA's `terminal` tool runs shell commands, and it inherits the shell environment it was launched from. For consistent behavior we strongly recommend running LLAIA under **bash**. On **Windows, launch it from Git Bash** (not `cmd.exe` or PowerShell) — this avoids shell-incompatibility issues when the agent runs commands. (Ollama/LM Studio GUIs work fine regardless; this only affects the agent's own terminal tool.)
- **Build toolchain (only if building from source):** a Rust toolchain (`cargo`). Otherwise just download the prebuilt binary from the Release page.

---

## Quick Start

Clone the repo and step in:

```bash
git clone <repo-url> && cd llaia
cargo run -- [command]
```

Or grab the binary for your architecture from the [Release page](https://github.com/Feihei/llaia/releases), drop it in your system PATH, and run:

```bash
llaia [command]
```

For the list of commands:

```bash
llaia help
```

### Step 1: Initialize

On first run, execute:

```bash
llaia init
```

It creates LLAIA's data structure under your home directory (default `~/.llaia/`):

```
~/.llaia/
 ├─ config.toml        # main config file (commented template; most sections commented out)
 ├─ logs/              # runtime logs
 └─ workspace/         # main agent workspace (also where the agent reads/writes files by default)
     ├─ SOUL.md        # agent persona
     ├─ USER.md        # your basic info and preferences
     ├─ MEMORY.md      # long-term memory (the agent writes here)
     ├─ uploads/       # uploaded-file staging
     ├─ subagent/      # sub-agent workspaces
     └─ sessions.db    # conversation history (auto-created on first launch)
```

Notes:
- **Idempotent**: existing files are not overwritten. To force regeneration, add `--force`:
  ```bash
  llaia init --force
  ```
- The `~` in paths expands to your home directory automatically. You can also point elsewhere with `llaia --config-dir <path> <command>`.

### Step 2: Connect a model (pick one)

LLAIA needs at least one LLM endpoint before chat works.

**A. Web UI config (recommended for beginners)** — skip manual editing, go to Step 3 to start the service, then fill it in from the browser.

**B. Edit the config manually** — open `~/.llaia/config.toml`, uncomment the `[provider.default]` and `[agent.main]` sections and fill in your endpoint. Minimal local-Ollama example:

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
model = "default.qwen"             # references provider.<id>.<model_alias> above
```

For cloud Anthropic, use `type = "anthropic"` and add `max_tokens` on the model entry (see the commented example in the generated template). Optional: set `fallback = [...]` under `[agent.main]` to auto-degrade to backup models when the primary fails.

### Step 3: Start using it

- **Web UI (recommended)**:
  ```bash
  llaia serve
  ```
  Then open `http://127.0.0.1:51217` in your browser. On first launch, if `webui.token` in `config.toml` is left empty, a random access token is generated and printed to the log — use it to log in, then configure the provider from the page.

- **Terminal chat**:
  ```bash
  llaia chat
  ```
  > Note: `llaia chat` runs purely in the terminal and **cannot configure a provider from the command line**. With no endpoint set it errors out and points you to `llaia serve` + Web UI.

---

## Common commands

| Command | What it does |
|---|---|
| `llaia chat` | Enter terminal chat mode (default, can be omitted) |
| `llaia serve` | Start background services: built-in Web UI (and optional messaging channels); no terminal chat |
| `llaia init [--force]` | Initialize the data directory: scaffold + default templates |
| `llaia config` | Print the currently effective config |
| `llaia doctor` | Diagnose provider connectivity and file integrity (troubleshooting) |
| `llaia remember "<a sentence>"` | Write a line of long-term memory into MEMORY.md directly |

In-session slash commands (in chat or Web UI): `/new` `/exit` `/compact` `/clear` `/remember` `/config` `/provider` (list & switch models at runtime) `/help`.

### Messaging channels

LLAIA can reach you beyond the Web UI. All channels are **disabled by default**; enable them in `config.toml` (credentials go in `.env` via `${VAR}` references — the init template has commented examples):

| Channel | Config section | Credentials |
|---|---|---|
| QQ (official bot platform, C2C) | `[channels.qq]` | app_id / app_secret |
| Telegram | `[channels.telegram]` | bot token from @BotFather (long polling, no public IP needed) |
| DingTalk | `[channels.dingtalk]` | client_id / client_secret (Stream Mode, no public IP needed) |
| WeChat (ClawBot / ilink) | `[channels.wechat]` | first launch prints a QR-code link — scan with your phone to log in |

Each channel has an `allow_*` field as a single-user safety lock (only your own messages are answered).

---

## The files it creates, and what you can edit

- **`config.toml`** — the master switch. Model endpoint, log level, QQ toggle, Web UI port, tool permissions — all here. `llaia init` generates a heavily-commented template.
- **`workspace/SOUL.md`** — what character you want the assistant to have (e.g. "concise, occasional joke").
- **`workspace/USER.md`** — your name, timezone, preferences (language, etc.) so it knows you better.
- **`workspace/MEMORY.md`** — the assistant's long-term memory; it writes here on its own, or you can add via `llaia remember`.
- **`workspace/sessions.db`** — all conversation history (SQLite); old messages are kept here as the source of truth when context is compressed.

All of these are plain Markdown / TOML — **open them in any editor and change them anytime**; the changes take effect on next launch.

---

## Reference

- [openclaw](https://github.com/openclaw/openclaw) - Most popular assistant Agent
- [astrbot](https://github.com/AstrBotDevs/AstrBot) - Local Agent with intuitive WebUI
- [zeroclaw](https://github.com/zeroclaw-labs/zeroclaw) - Lightweight agent in rust
- [goose](https://github.com/aaif-goose/goose) - coding agent in rust
- [深入理解 AI Agent：设计原理与工程实践](https://github.com/bojieli/ai-agent-book) - A Book
