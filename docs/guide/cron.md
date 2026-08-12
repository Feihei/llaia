# 定时任务（Cron）

在 `cron.toml` 里定义重复或一次性任务，唤醒主 agent 或直接跑工具链，结果可推送到指定频道。`llaia init` 会生成全注释的模板。

> 调度设计与字段语义见开发文档 [ADR-0013](../adr/0013-cron-scheduling.md)。

## 任务字段

每个任务写在 `[[task]]` 表里：

| 字段 | 说明 |
|---|---|
| `id` | 任务唯一 ID（触发/历史用）。 |
| `schedule` | 5 字段 cron 表达式（分 时 日 月 周），内部自动转 6 字段。 |
| `mode` | `agent`（唤醒主 agent 跑 prompt）或 `tools`（直接跑工具链，不耗 LLM token）。 |
| `channel` | 结果推送目标：`qq` / `web` / `mail` / `cli`。`cli` 无持久连接，结果被丢弃（NoopPusher）。 |
| `enabled` | 默认 `true`；`false` 则不注册。 |
| `prompt` | `mode=agent` 时给 agent 的指令（多行用 `"""`）。 |
| `steps` | `mode=tools` 时的工具链步骤数组。 |

## 示例

```toml
# 每天 8:00 唤醒 agent 查新闻推送
[[task]]
id = "morning_news"
schedule = "0 8 * * *"
mode = "agent"
channel = "qq"
enabled = true
prompt = """
现在是早上 8:00。请查今天的 AI 科技热点，
整理成 3-5 条简讯推送给我。
"""

# 每 30 分钟跑工具链（不消耗 LLM token）
[[task]]
id = "health_check"
schedule = "*/30 * * * *"
mode = "tools"
channel = "web"
enabled = true
steps = [
  { tool = "tavily_search", args = { query = "llaia" } },
  { tool = "memory_write", args = { text = "checked at {{now}}" } },
]
```

## 管理

- **Web UI**：查看列表、增改、手动触发、看执行历史（见 [Web UI](webui.md) 的 `/api/cron` 系列接口）。
- **CLI**：`llaia doctor` 会列出 `cron.toml` 中的任务与启用状态。
- **会话内**：agent 也能通过内置 `cron` 工具管理任务（需 `serve` 模式下 cron 调度器就绪）。

## 注意

- cron 仅在 `serve` 模式下运行；`chat` 模式不启动调度器。
- 非交互推送目标遇到需审批的操作会自动拒绝（无法等待用户），任务设计时要考虑这一点。
