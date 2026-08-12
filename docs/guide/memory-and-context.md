# 记忆与上下文

LLAIA 用三份 Markdown 文件 + 一个 SQLite 数据库来持久化"它是谁、你是谁、记得什么、聊过什么"。

## 三份 Markdown

都位于 `workspace/`（默认 `~/.llaia/workspace/`）：

| 文件 | 内容 | 谁来改 |
|---|---|---|
| `SOUL.md` | agent 人格设定 | 你（初始化模板生成，可自由改） |
| `USER.md` | 你的信息、偏好、身份绑定 | 你 / agent（`/remember` 也能追加） |
| `MEMORY.md` | 长期事实记忆（分条） | agent 通过 `memory_write` 工具 / `llaia remember` / `/remember` |

`SOUL.md` 与 `USER.md` 永驻上下文（压缩时不会丢）；`MEMORY.md` 是事实记忆，会被「做梦」定期整理。

## 会话与压缩

- 同一用户同一会话**跨频道接续**（Web UI / 终端 / QQ 等共用 session）。
- `/new` 开新会话；上下文超阈值（默认 70%，可配 `[runtime].context_threshold`）时**自动压缩**：关键消息保留（SOUL/USER 永留、首条用户消息留、工具结果可丢），其余旧消息由 LLM 摘要替换。
- 手动压缩：`/compact`。
- 用更便宜的模型压缩：`[runtime].compact_model`。
- 更聪明的压缩策略见 [ADR-0019](../adr/0019-smart-compaction.md)。

## 做梦（Dream）

「做梦」是两阶段记忆整理：把零散对话沉淀为干净的长期记忆，写入 `MEMORY.md`。

- 空闲 30 分钟自动触发一次（daily `0 4 * * *` 的兜底也保留）。
- 手动触发：`/dream`（跳过空闲门控，立即整理）。
- 回滚：`/dream-rollback` 把 `MEMORY.md` 恢复到最近一份 `MEMORY.backups/` 备份。
- 备份：`MEMORY.md` 每次被做梦改写前会落到 `workspace/MEMORY.backups/`。

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
