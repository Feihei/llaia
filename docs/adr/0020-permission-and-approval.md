# ADR-0020: 权限档位与交互式审批

- 状态：提议（P4-d 实施）
- 日期：2026-08-11
- 关联：P3-a（能力边界 `confirm_mode`）、P4-d（边界与授权）

## 背景

P3-a 用 `confirm_mode`（none / always / session）做全局开关，但有两个硬伤：

1. **只有 CLI 能交互确认**——其它频道（QQ/Telegram/Web…）遇到需确认的工具只能「拒绝」，导致这些频道下 agent 几乎不能做任何有副作用的事。
2. **判定维度单一**——只区分「要不要确认」，不区分「操作是否危险」「是否在 workspace 内」。

P4-d 要求：三档权限 profile（`read-only` / `default` / `yolo`）+ 跨频道一致的 `/ok` `/deny` 审批交互 + 可放宽边界的 `/move` `/cd`。

## 决策

### 1. 权限档位（permission profile）

`[runtime].permission`：

| profile | 语义 | 审批范围 |
|---|---|---|
| `default`（默认） | 常规 | 仅「有副作用 **且** 落在 workspace **外**」的操作需审批 |
| `read-only` | 只读倾向 | 所有「有副作用」操作都需审批（无论是否在 workspace 内） |
| `yolo` | 全放行 | 不审批（路径/命令黑名单等**硬边界**仍生效，无法绕过） |

「有副作用」= 工具的 `requires_confirm()`（file_write / file_edit / terminal / memory_write / MCP 工具等）。
「在 workspace 内」由 `path_guard` 既有校验判定（`file_write/file_edit` 看 `path`，`terminal` 看命令行路径 token）。MCP 工具一律视为「外」，安全默认。

档位可运行时切换：`/permission <profile>`（不写 config，与 `/provider` 一致）。

### 2. 交互式审批（跨频道一致）

引入 `ApprovalGate`（`Arc<Mutex<…>>`，独立于 agent 锁）：当操作需要审批时：

1. 注册 `PendingApproval { id, tool_name, args, tool_call_id, channel, within_workspace }`；
2. 通过 `TurnEvent::Chunk` 向用户推送提示（含操作内容 + 目标目录 + `id`）；
3. 本轮 agent turn **暂停**（返回占位 tool 结果 `⏳ 等待确认`），不继续调用模型，避免重复触发；
4. 用户发 `/ok <id>` 或 `/deny <id>` → 解析 pending：
   - `/ok`：执行工具（或 `/move` 时切换 workspace），把真实结果写回上下文，返回 `SlashOutcome::Resume`，频道据此**启动一轮 continuation turn** 让模型基于结果继续；
   - `/deny`：写回「已拒绝」结果，同样 `Resume`，模型改方案。

非交互频道（cron / delegate）不进入 pending：delegate 视为放行；cron 等需要审批时**自动拒绝**（不可能等用户）并说明。

### 3. workspace 共享根（支持 /move）

文件/终端工具的 workspace 不再是构造时拷贝的 `PathBuf`，而是与 Agent 共享的
`Arc<RwLock<PathBuf>>`（`workspace_root`）。`/move <path>` 经审批后调用
`Agent::set_workspace()`，一处更新、所有工具与边界校验即时生效。

### 4. /move /cd

`/move <path>`（别名 `/cd`）：解析并校验目标目录（须真实存在、非危险前缀），
注册一个 `__move_workspace` 伪 pending，提示风险并要求 `/ok <id>` 确认后才切换。
与审批流完全复用，无需单独机制。

## 与既有边界的关系

- 不删除 P3-a 的 `confirm_mode` 字段（保留向后兼容），但其语义被 profile 取代；
  加载时若 `confirm_mode != "none"` 仅 warn，不再驱动逻辑。
- 路径黑名单、命令黑名单、shell 包装拒绝等**硬边界**在所有 profile 下始终生效。

## 风险与权衡

- continuation turn 会在上下文追加一条「等待确认」占位结果 + 真实结果，历史里出现两条 tool 结果对应一次调用；模型可容忍，后续可优化为替换。
- Resume 需每个频道在 `SlashOutcome::Resume` 分支里启动 continuation turn（cli/qq/telegram/dingtalk/wechat/web 均已接入）。
