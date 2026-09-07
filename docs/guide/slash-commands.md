# 斜杠命令（Slash Commands）

在终端或 Web UI 对话里以 `/` 开头的命令，用于控制会话、切换模型、审批操作等。

> 实际分发逻辑见 `src/commands/slash.rs`。

| 命令 | 作用 |
|---|---|
| `/new` | 开新会话（清空当前上下文）。 |
| `/exit` · `/quit` | 退出当前交互。 |
| `/stop` | 停止当前生成（同退出语义视频道实现）。 |
| `/compact` | 手动压缩上下文（需要已配 provider）。 |
| `/clear` | 清空上下文（等同 `/new` 的清空效果）。 |
| `/stats` | 显示上下文统计：context_size、阈值、当前 token 占比（正文 + 工具 schema 分项）、历史条数、session id、摘要状态、工具分组计数、压缩用 provider。 |
| `/remember <text>` | 往 `MEMORY.md` 追加一条记忆（等价 CLI `llaia remember`）。 |
| `/provider` | 列出所有可用模型，当前模型标 `*`。 |
| `/provider <num>` · `/provider <id.alias>` | 运行时切换模型（不写 config；保留 fallback 降级链）。 |
| `/permission [read-only\|default\|yolo]` | 查看或切换权限档位（不写 config）。 |
| `/reasoning [on\|off]` | 会话级开关推理模型的深度思考。`off` 提速日常问答（对 llama.cpp / Ollama / vLLM 等支持 `chat_template_kwargs` 的端点生效，其它忽略）；仅当前会话有效，不写 config。 |
| `/ok <id>` | 批准一个待确认的操作（交互式审批）。 |
| `/deny <id>` | 拒绝一个待确认的操作。 |
| `/move [<path>\|home]` · `/cd` | 切换工作目录；无参数 / `home` / `~` / `-` 恢复到原始 workspace；其它路径需 `/ok` 确认。在会话线内时，批准即自动把该线的绑定目录改写为新目录（回显 `was`）；回 `home` 则解绑。重启后作用域恢复家目录（不持久）。 |
| `/session [<名>\|close]`（别名 `/task`） | 开/切/归档**会话线**：无参回通用线，`名`不存在则新建（回灌通用线尾部当 brief），存在则切回并回灌该线尾部；`close` 归档不可续写。注意：会话线只管历史与上下文，目录作用域归 `/move` 管；切线 notice 的 `[scope]` 行会提示两者是否对应（软提示，不强制同步）。 |
| `/sessions`（名 `/tasks`） | 列出未归档会话线（名、绑定目录、频道、最后活动），`*` 标当前。 |
| `/config` | 显示当前生效的运行参数（阈值、迭代上限、上下文大小、工具等）。 |
| `/delegate-list` | 列出后台委派任务（主 agent 委派给子 agent 的异步任务）。 |
| `/delegate-cancel <id>` | 取消某个后台委派任务。 |
| `/help` | 打印上述命令列表。 |

## 审批交互

需要确认的操作（如写文件、跑命令、切 workspace）会注册一个 `PendingApproval` 并暂停本轮，提示里带 `id`。你回复：

```
/ok <id>       # 批准并执行，模型基于结果继续
/deny <id>     # 拒绝，模型改方案
```

这是跨频道一致的审批流，详见 [权限与安全](permissions.md)。
