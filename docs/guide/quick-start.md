# 快速开始

本页带你在 5 分钟内跑通第一次对话。更细的配置、频道、定时任务、MCP、技能等，见本指南其余章节。

## 1. 初始化数据目录

```bash
llaia init
```

会在 `~/.llaia/`（可用 `--config-dir <path>` 改到其他地方）生成：

```
~/.llaia/
 ├─ .env               # 敏感凭据（API key 等），填真实值
 ├─ config.toml        # 主配置（带注释的模板）
 ├─ cron.toml          # 定时任务（默认全注释）
 ├─ mcp.toml           # MCP server 配置（默认全注释）
 ├─ logs/              # 运行日志
 ├─ skills/            # 用户级 / 项目级技能
 └─ workspace/         # agent 默认读写文件的地方
     ├─ SOUL.md        # agent 人格设定
     ├─ USER.md        # 你的信息与偏好
     ├─ MEMORY.md      # 长期记忆
     ├─ uploads/       # 上传文件暂存
     ├─ subagent/      # 子 agent 工作区
     └─ sessions.db    # 会话历史（SQLite，source of truth）
```

- **幂等**：已存在的文件不会被覆盖。`--force` 可强制重新生成模板。
- 想换数据目录：之后所有命令加 `--config-dir <path>`（全局参数）。

## 2. 配置模型 Provider

LLAIA 不内置模型，要指向一个 LLM 端点。

| 类型 | 示例 | 隐私 |
|---|---|---|
| 本地 | [Ollama](https://ollama.com)、[LM Studio](https://lmstudio.ai)、[llama.cpp](https://github.com/ggerganov/llama.cpp) | 数据留本机 |
| 云端 | [OpenRouter](https://openrouter.ai)、任意 OpenAI 兼容端点、Anthropic Messages API | 文本出本机 |

**A. Web UI（新手推荐）** — 先启动服务（见第 3 步），在浏览器里填 provider 设置。

**B. 手动编辑 config.toml** — 打开 `~/.llaia/config.toml`，取消注释 `[provider.default]` 与 `[agent.main]`：

```toml
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"   # 本地 Ollama 默认地址
api_key = ""                              # 本地 Ollama 通常留空

[provider.default.qwen]
model = "qwen2.5:7b"
native_tool_calling = false
context_size = 32768

[agent.main]
model = "default.qwen"                    # 引用 provider.<id>.<model_alias>
```

云端 Anthropic 用 `type = "anthropic"`，并在 model 下加 `max_tokens`（必填，未配默认 4096）。主模型不稳定时可在 `[agent.main]` 下加 `fallback = [...]` 自动降级。

完整的配置字段见 [配置参考](configuration.md)。

## 3. 启动

```bash
llaia serve       # Web UI + 后台频道（推荐）
llaia chat        # 纯终端交互
```

- `serve` 启动后打开 **http://127.0.0.1:51217**（若 `webui.token` 留空，随机 token 会打印在日志里）。
- `chat` 进入终端 REPL，适合调试。注意 `chat` 模式**必须有 provider**，否则直接报错引导你去 WebUI 或 config 配置。

## 4. 第一次对话

在 Web UI 或终端里直接发消息即可。agent 可以读/写文件、跑终端命令、抓网页、搜索（需 tavily key）、记长期记忆。

如果不确定能不能连上模型，先跑：

```bash
llaia doctor
```

它会检查 config 目录、provider 连通性、cron/mcp/skills、sessions.db 等。详见 [CLI 参考](cli.md) 与 [常见问题](faq.md)。

## 相关

- 命令大全：[CLI 参考](cli.md)
- 会话内命令：[斜杠命令](slash-commands.md)
- 接入多渠道：[频道](channels.md) · [Web UI](webui.md)
