# 记忆与上下文

LLAIA 用三份核心 Markdown 文件 + 一个 SQLite 数据库来持久化"它是谁、你是谁、记得什么、聊过什么"，外加一份自动维护的 `reminder.md`。

## 三份核心 Markdown + reminder

都位于 `workspace/`（默认 `~/.llaia/workspace/`）：

| 文件 | 内容 | 谁来改 |
|---|---|---|
| `SOUL.md` | agent 人格设定 | 你（初始化模板生成，可自由改） |
| `USER.md` | 你的信息、偏好、身份绑定 | 你 / agent（`/remember` 也能追加） |
| `MEMORY.md` | 长期事实记忆（分条） | agent 通过 `memory_write` 工具 / `llaia remember` / `/remember` |
| `reminder.md` | 自动生成的抗漂移要点（勿手改） | agent 自动维护 |

`SOUL.md` 与 `USER.md` 永驻上下文（压缩时不会丢）；`MEMORY.md` 是事实记忆。

## Tail Reminder（抗长会话漂移）

会话变长后回复风格可能逐渐偏离 `SOUL.md` 的设定（比如变得冗长、爱用列表）。这不是系统提示词丢了——SOUL/USER 每轮都完整在场——而是模型在长历史里开始模仿自己最近的回复。

LLAIA 的对策：由 LLM 从 SOUL+USER 自动提炼一份 ≤120 token 的关键行为指令清单（语气、称呼、硬偏好），存到 `workspace/reminder.md`，并作为每轮请求的**最后一条**消息注入（离生成点最近、注意力最强）。

- 全自动：SOUL 或 USER 修改后下一轮自动重新提炼，无需任何配置。
- 首次生成前（或生成失败时）没有 reminder，属正常降级。
- 文件头有「勿手改」注释；想影响其内容，改 SOUL/USER 对应段落即可。

## 会话与压缩

- 同一用户同一会话**跨频道接续**（Web UI / 终端 / QQ 等共用 session）。
- `/new` 开新会话；上下文超阈值（默认 70%，可配 `[runtime].context_threshold`）时**自动压缩**：关键消息保留（SOUL/USER 永留、首条用户消息留、工具结果可丢），其余旧消息由 LLM 摘要替换。
- 手动压缩：`/compact`。
- 用更便宜的模型压缩：`[runtime].compact_model`。
- 更聪明的压缩策略见 [ADR-0019](../adr/0019-smart-compaction.md)。

## 记忆整理

内置**没有**自动改写 `MEMORY.md` 的机制（原「做梦」已移除，见 [ADR-0030](../adr/0030-remove-dream.md)）。可选的替代做法：

- `/memory-compact`：MEMORY 超限时手动去重压缩（写前备份到 `workspace/backups/`）。
- 自建 cron 任务：普通 agent 模式定时任务，prompt 让 agent 用 `memory_research` 检索历史、把值得记的事实**推送给你审阅**（而非直接改写 MEMORY）。

## 会话历史

- `sessions.db`（SQLite）是会话历史的 **source of truth**，位于 `workspace/`。
- 压缩时旧消息从内存移除，但 SQLite 仍留底，可回溯。
- 首次启动自动创建；`llaia doctor` 会检查其存在性与大小。

## 多模态与时区

- 主模型无多模态时，用 `[runtime].vision_model` 描述图片（文本替换图片注入主模型上下文）。
- `[runtime].timezone` 决定状态栏与用户可见日期（IANA 时区名；未设跟随系统）。修改后 Web UI 热更新生效。

## 相关

- 写记忆的命令：`/remember`、CLI `llaia remember` —— 见 [斜杠命令](slash-commands.md) / [CLI 参考](cli.md)
- 持久化模型整体设计：[ADR-0003](../adr/0003-persistence-model.md) · [ADR-0004](../adr/0004-session-and-context.md)
