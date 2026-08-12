# 权限与安全

LLAIA 能读/写你的文件、跑 shell 命令——能力越大风险越大。它用「权限档位 + 交互式审批 + 硬边界」三层来保证你永远掌控有副作用的操作。

> 设计背景见开发文档 [ADR-0020](../adr/0020-permission-and-approval.md)。

## 权限档位（permission profile）

在 `[runtime].permission` 配置，或会话内用 `/permission <profile>` 运行时切换（**不写 config**）：

| 档位 | 语义 | 审批范围 |
|---|---|---|
| `default`（默认） | 常规 | 仅「有副作用 **且** 落在 workspace **外**」的操作需审批 |
| `read-only` | 只读倾向 | 所有「有副作用」操作都需审批（workspace 内外都算） |
| `yolo` | 全放行 | 不审批 |

「有副作用」= 工具的 `requires_confirm()`（file_write / file_edit / terminal / memory_write / MCP 工具等）。「在 workspace 内」由 path_guard 判定（file 工具看 `path`，terminal 看命令行路径 token）。MCP 工具一律视为「外」。

## 交互式审批

遇到需要审批的操作时：

1. 注册 `PendingApproval { id, tool_name, args, 目标目录, channel }`；
2. 向用户推送提示（含操作内容 + 目标目录 + `id`）；
3. 本轮 agent turn **暂停**（返回「⏳ 等待确认」占位），不继续调模型，避免重复触发；
4. 你回复 `/ok <id>` 或 `/deny <id>`：
   - `/ok`：执行工具（或 `/move` 时切换 workspace），真实结果写回上下文，模型基于结果继续；
   - `/deny`：写回「已拒绝」，模型改方案。

这套流程对所有频道一致（CLI / QQ / Telegram / 钉钉 / 微信 / Web）。非交互频道（cron / delegate）等不了用户，**自动拒绝**并说明。

## `/move` / `/cd`

`/move <path>`（别名 `/cd`）切换 agent 工作目录：

- 无参数 / `home` / `~` / `-`：恢复到原始 workspace，**无需审批**；
- 其它路径：解析并校验（须真实存在、非危险前缀），注册 `__move_workspace` 伪 pending，需 `/ok <id>` 确认后才切换。

切换即时生效——所有工具与边界校验共享同一个 workspace 根。

## 硬边界（永远生效）

无论哪个档位，以下**硬边界始终拦截**，无法用权限绕过：

- 路径黑名单（禁止写出 workspace 外的危险路径）；
- 命令黑名单（禁止危险 shell 命令）；
- shell 包装拒绝（特殊命令形态）。

## 旧字段 `confirm_mode`

`[channels.qq].confirm_mode`（`none` / `always` / `session`）已废弃，语义被权限档位取代。加载时若非 `none` 仅告警，不再驱动逻辑；`whitelist` 会自动回退为 `none`。新部署请用[权限档位](permissions.md)。
