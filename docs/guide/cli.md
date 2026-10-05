# CLI 参考

LLAIA 的命令行入口是 `llaia`。**不带子命令时默认进入 `serve` 模式并自动用系统浏览器打开 Web UI**——Windows 下双击 `llaia.exe` 等效于无参数启动，弹出控制台窗口即服务本体（关窗或 Ctrl+C 停止服务；启动报错时窗口会停住等一次回车，来得及读错误）。终端交互需要显式 `llaia chat`。

## 全局参数

| 参数 | 说明 |
|---|---|
| `--config-dir <path>` | 数据目录（`config.toml` / `.env` / `logs/` / `workspace/` 的所在），默认 `~/.llaia`。**全局参数**，放在子命令之前。 |

优先级：`--config-dir` > 环境变量 `LLAIA_HOME` > 默认 `~/.llaia`。三条都拿不到（且取不到用户主目录）时报错退出，不会退化成往当前工作目录写状态。

```bash
llaia --config-dir /data/llaia serve
LLAIA_HOME=/tmp/llaia-trial llaia serve --port 51220   # 一次性试验实例：不碰正式数据，也不抢默认端口
```

## 子命令

| 命令 | 作用 |
|---|---|
| `llaia chat` | 终端交互模式。需先配好 provider，否则报错引导。 |
| `llaia serve [--host <addr>] [--port <n>] [--open]` | 后台服务模式（**无参数启动的默认行为**）：启动 Web UI + 所有已启用的频道（QQ / Telegram / 钉钉 / 微信 / 邮箱 / 飞书）。无 provider 时进入**降级模式**（聊天不可用，但 Web UI 仍可配置）。`--host` / `--port` **只覆盖本次监听的地址与端口**，不回写 `config.toml`，Config 页显示的仍是文件里的值（免得临时端口被下一次保存静默固化）；覆盖生效时日志打一条 `webui bind overridden by CLI`。`--open`（无参数启动时自动生效）在端口就绪后用系统浏览器打开 Web UI：token 以 `?token=` 附带、浏览器记住后免登录；`webui.host` 配成 `0.0.0.0`/`::` 时自动换成回环地址打开。若启动时发现目标端口已有实例在听（多半是另一个 llaia），则不再起第二实例——直接打开浏览器指向已有实例并退出。 |
| `llaia init [--force]` | 生成数据目录骨架与默认模板（config / .env / cron / mcp / SOUL·USER·MEMORY）。serve / chat 启动时会自动补齐缺失文件，故此命令通常无需手动执行。显式用途：`llaia init` = **修复**（只补缺失项，已有文件不动）；`llaia init --force` = **覆盖**（用模板重建全部文件，会重置 config.toml 与 .env）。 |
| `llaia config` | 打印当前生效配置（解析后、展开 `~` 与 `${VAR}` 之后）。 |
| `llaia doctor` | 诊断：模板文件（config / cron / mcp / .env 存在性与可解析性）、provider 连通性（`/models`，5s 超时）、runtime 参数、cron/mcp 任务与 server、skills、sessions.db。**config.toml 解析失败也会出诊断**（作为 `error` 项报出并跳过依赖它的探测）；缺失项提示 `llaia init` 补齐。纯只读，不写任何文件。 |
| `llaia remember "<text>"` | 往 `MEMORY.md` 追加一行长期记忆（带日期前缀）。等价于会话内 `/remember <text>`。 |

### 示例

```bash
llaia            # = llaia serve + 自动打开 Web UI（双击 exe 同效）
llaia chat
llaia serve
llaia serve --open
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
