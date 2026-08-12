# 配置参考

主配置文件是 `~/.llaia/config.toml`（可用 `--config-dir` 改路径）。敏感凭据集中放同目录的 `.env`，config 里用 `${VAR}` 引用，避免明文落盘。

> 配置 schema 的设计背景与完整示例见开发文档 [ADR-0008](../adr/0008-config-schema-v1.1.md)。本页是从用户视角使用配置的速查。

## 路径展开规则

- `~` / `~/path` → 用户 home 目录。
- `${VAR}` → 环境变量值（变量名须匹配 `[A-Z_][A-Z0-9_]*`；找不到则替换为空串并告警，让 `serve` 能进降级模式而非直接挂掉）。
- 顺序：先展开 env，后展开 tilde。

## `[runtime]` — 全局运行时

| 字段 | 默认值 | 说明 |
|---|---|---|
| `context_threshold` | `0.7` | 上下文压缩阈值（占 context_size 比例），超过自动压缩。 |
| `max_iterations` | `10` | agent 工具循环上限。 |
| `timezone` | 未设（跟随系统） | IANA 时区名，如 `Asia/Shanghai`。非法值告警并回退系统本地时区。 |
| `compact_model` | 未设 | 用更便宜的模型跑上下文压缩，格式 `"provider_id.model_alias"`；不配则复用主模型。 |
| `vision_model` | 未设 | 主模型无多模态时，用此模型描述图片（文本替换图片注入主模型）。 |
| `permission` | 未设（= `default`） | 权限档位：`read-only` / `default` / `yolo`。详见 [权限与安全](permissions.md)。 |

## `[log]`

| 字段 | 默认值 | 说明 |
|---|---|---|
| `level` | `info` | 日志级别：`debug` / `info` / `warn` / `error`。 |
| `dir` | `~/.llaia/logs` | 日志目录（未显式配时跟随 config 文件所在目录的 `logs/`）。 |

## `[provider.<id>]` 与 `[provider.<id>.<model_alias>]`

provider 定义"连接"（base_url + api_key），model 定义"具体模型组合"。agent 用 `"<id>.<alias>"` 引用。

```toml
[provider.default]
type = "openai_compatible"          # 或 "anthropic"
base_url = "http://localhost:11434/v1"
api_key = "${OLLAMA_API_KEY}"       # 留空或引用 .env

[provider.default.qwen]
model = "qwen2.5:7b"
native_tool_calling = false          # true=OpenAI function calling；false=标签协议降级
context_size = 32768                 # 可选，不配则启动时探测，取 min(配置, 探测)

[provider.claude]                     # 云端 Anthropic 示例
type = "anthropic"
api_key = "${ANTHROPIC_API_KEY}"

[provider.claude.sonnet]
model = "claude-sonnet-4-20250514"
max_tokens = 8192                     # Anthropic 必传，未配默认 4096
```

`[provider.<id>]` 的 `type` 决定走哪套实现：`anthropic` 走 Anthropic Provider；缺省/未知回退 OpenAI 兼容（存量配置不受影响）。

## `[agent.main]`

| 字段 | 说明 |
|---|---|
| `model` | `"provider_id.model_alias"` 引用；留空 = 降级模式（仅可配置 Web UI）。 |
| `fallback` | 备用模型链，如 `["local.small", "cloud.big"]`，主模型失败依序降级。 |
| `workspace` / `soul` / `user` / `memory` | **已废弃**，自动推导到 `~/.llaia/workspace/`（子 agent 到 `workspace/subagent/<alias>/`）。显式设置会告警并用推导值覆盖。 |
| `denied_tools` | 子 agent 工具黑名单（主 agent 一般留空）。 |
| `delegate_timeout` | 委派超时秒数，默认 120（仅子 agent 生效）。 |

> 子 agent（`[agent.<alias>]`）可由主 agent 委派任务，P2 能力；日常个人使用通常只用 `main`。

## `[webui]`

| 字段 | 默认值 | 说明 |
|---|---|---|
| `host` | `127.0.0.1` | 监听地址（默认仅本机；改 `0.0.0.0` 需自行负责安全）。 |
| `port` | `51217` | 监听端口（避开 8080 等常见服务）。 |
| `token` | 空 | 鉴权 token；留空则启动时随机生成并打印日志。 |

旧 `[channels.web]` 会自动迁移到 `[webui]`（向后兼容）。详见 [Web UI](webui.md)。

## `[tools.terminal]`

| 字段 | 默认值 | 说明 |
|---|---|---|
| `confirm` | `whitelist` | 已废弃，仅作兼容；实际以 [权限档位](permissions.md) 为准。 |
| `command_policy` | `blacklist` | `blacklist` / `whitelist` / `none`。 |
| `command_whitelist` | `[]` | 仅 `policy=whitelist` 时生效。 |
| `whitelist` | `[ls, cat, grep, pwd, dir]` | 旧字段，兼容保留。 |

## `[tools.tavily]`

| 字段 | 说明 |
|---|---|
| `api_key` | Tavily 搜索 key，支持 `${VAR}`。留空则搜索工具不可用。 |

## `[channels.*]`

各频道默认关闭。凭据用 `${VAR}` 引用 `.env`。详见 [频道](channels.md) 获取每个频道的字段与单用户安全锁（`allow_*`）。

## 校验配置

```bash
llaia config        # 打印生效配置
llaia doctor        # 连通性 + 文件完整性诊断
```

Web UI 也提供 `/api/config/validate` 校验接口（见 [Web UI](webui.md)）。
