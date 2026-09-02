# CLI 参考

LLAIA 的命令行入口是 `llaia`。不带子命令时默认进入 `chat` 模式。

## 全局参数

| 参数 | 说明 |
|---|---|
| `--config-dir <path>` | 数据目录，默认 `~/.llaia`。**全局参数**，放在子命令之前。 |

```bash
llaia --config-dir /data/llaia serve
```

## 子命令

| 命令 | 作用 |
|---|---|
| `llaia chat` | 终端交互模式（默认）。需先配好 provider，否则报错引导。 |
| `llaia serve` | 后台服务模式：启动 Web UI + 所有已启用的频道（QQ / Telegram / 钉钉 / 微信 / 邮箱 / 飞书）。无 provider 时进入**降级模式**（聊天不可用，但 Web UI 仍可配置）。 |
| `llaia init [--force]` | 生成数据目录骨架与默认模板（config / .env / cron / mcp / SOUL·USER·MEMORY）。serve / chat 启动时会自动补齐缺失文件，故此命令通常无需手动执行。显式用途：`llaia init` = **修复**（只补缺失项，已有文件不动）；`llaia init --force` = **覆盖**（用模板重建全部文件，会重置 config.toml 与 .env）。 |
| `llaia config` | 打印当前生效配置（解析后、展开 `~` 与 `${VAR}` 之后）。 |
| `llaia doctor` | 诊断：config 目录、provider 连通性（请求 `/models`）、cron.toml / mcp.toml / skills 扫描、sessions.db 存在性。 |
| `llaia remember "<text>"` | 往 `MEMORY.md` 追加一行长期记忆（带日期前缀）。等价于会话内 `/remember <text>`。 |

### 示例

```bash
llaia            # = llaia chat
llaia chat
llaia serve
llaia init
llaia init --force
llaia config
llaia doctor
llaia remember "我讨厌在命令前加 sudo"
```

## 会话内（REPL）命令

终端与 Web UI 里都可用的斜杠命令，单独成篇：[斜杠命令](slash-commands.md)。

## 提示

- `serve` 与 `chat` 启动时会抢占一个 PID 文件，避免重复实例。
- 优雅停止：终端 `Ctrl+C`，Web UI 调 `/api/shutdown`（见 [Web UI](webui.md)）。
- 配置字段的逐项解释见 [配置参考](configuration.md)。
