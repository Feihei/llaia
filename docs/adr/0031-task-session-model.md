# ADR-0031: 任务 session 模型——通用线 + 任务线

- 状态：Proposed（设计草稿，供 grill 评审；**未实现**）
- 日期：2026-09-01
- 关联：plan.md #D（本 ADR 即其设计探索产出）；plan.md #B 受信目录（已实现，任务 session 与其共用目录信任语义）

## 背景 / Context

通用助手沿用了 coding agent 的 session 模型：一条流水式会话，靠 `/new` 手动分段、自动压缩维持窗口。coding 场景里「任务」天然是 session 边界；通用助手的日常杂货（问个问题、改个配置）挤进同一条线，导致：

- 一段有边界的工作（如「帮某目录做一轮整理」）与日常闲聊共享上下文，跑完的垃圾上下文持续稀释后续对话质量，只能靠压缩兜底；
- `/new` 是全有全无的重置：旧线整个丢开，没有「这段工作先挂起、回头接着来」的中间形态。

用户定案方向（grill 2026-08-24）：**一条常驻通用 session + 按需显式开启的任务 session**。任务完成/用户关闭即归档，独享完整上下文，不污染通用线。

与 `/move` 的耦合：移动到 workspace 外目录往往意味着「在该目录执行主线以外的任务」→ 可作为自动建议任务 session 的信号。#B 受信目录已落地（`Agent.trusted_dirs`，/move 批准即登记、会话内免审批），**任务 session ≈ 受信目录 + 独立上下文边界**，两者天然成一套。

## 现状盘点（2026-09-01 核实）

- `sessions` 表（`memory/sqlite.rs:165`）：`channel / created_at / last_activity / token_count / state('idle') / title`。**无类型字段**，所有 session 同质。
- session 切换机制已有先例：`/new`（`slash.rs`）= `create_session` + 换 `agent.session_id` + `context.clear()`。任务线的「进出」可复用同一路径。
- todo 工具（ADR-0024）：per-session 文件存储（`TodoStore::set_current_session`），是「当前会话内的步骤清单」，**无上下文边界、生命周期跟随会话**。
- 受信目录（#B，本次交付）：`/move` 批准 → canonical 目录进 `Agent.trusted_dirs`，会话级内存，审批按 workspace_root ∪ trusted 判定。

## 决策分支（四个待决问题，供 grill 逐项拍板）

### Q1 触发边界：任务线怎么开启？

| 选项 | 说明 | 评估 |
|---|---|---|
| A | 仅显式 `/task <名>` 命令 | 最简单；但 /move 场景每次要多敲一步 |
| **B（推荐）** | 显式 `/task <名>` + **/move 批准后提示**「是否为该目录开启任务 session」 | plan.md 已定方向（显式为主，规则猜测不可靠）；/move 是现成的意图信号，提示而非自动创建，用户可拒绝 |
| C | 按目录/行为规则自动创建 | **否决**：猜测不可靠，误创建的清理成本高于少敲一步 |

### Q2 可发现性：任务列表入口；与 todo 合并还是并行？

| 选项 | 说明 | 评估 |
|---|---|---|
| **A（推荐）** | 新增 `/tasks` 斜杠命令：枚举 sqlite 中 `kind='task'` 且未归档的 session（名称/绑定目录/最后活动时间/频道），`/task <名>` 进出 | todo 与 task session 粒度不同：todo 是「一次任务内的步骤清单」（短命、无边界），task session 是「有边界的一段工作」（跨多轮、独享上下文）。**不合并**，硬合并会让两者语义都含糊 |
| B | 把 todo 升级为任务容器（todo 条目即任务线） | 否决：todo 现有语义（add/list/update/done 四动作、跟随当前 session）会被破坏，且 todo 文件存储要迁 sqlite，改造面大收益小 |

### Q3 持久化：独立上下文怎么落 sqlite？

推荐 schema（存量库幂等 `ALTER TABLE` 补列，同 `title` 列先例）：

```sql
ALTER TABLE sessions ADD COLUMN kind TEXT NOT NULL DEFAULT 'main';   -- main | task
ALTER TABLE sessions ADD COLUMN bound_path TEXT;                     -- 任务绑定的受信目录（kind=task 时非空）
-- 归档复用 state 列：state = 'archived'（现值 'idle' 语义不变）
```

- 任务 session 的 `title` 复用现有列（/task `<名>` 直接写入）。
- 归档语义：`state='archived'` 后不可续写（切回时提示已归档），消息仍可被 `memory_research` 检索（messages 表不动）。
- 上下文隔离：任务 session 的 messages 天然按 `session_id` 分区，无需额外机制；`Context` 内存态在切换时 clear（同 /new）。

### Q4 跨频道：换频道能否回到某条任务线？

| 选项 | 说明 | 评估 |
|---|---|---|
| A | 任务 session 绑定创建频道，不跨频道 | 最简，但「在电脑上开任务、路上用 QQ 续」是自然诉求，砍掉可惜 |
| **B（推荐，简化版）** | `/task <名>` 任意频道可切换，**全局单活跃**：一次只有一条 active 线（通用线或某任务线），切换即全局换 `session_id` | 当前 Agent 就是单一 `session_id` + agent 锁串行化，切换是现成机制；不做「双频道各挂一条线并行活跃」（那需要 per-channel session 路由，复杂度跳档，收益不明） |

## 影响面（实施时）

- `slash.rs`：`/task <名>`（开/切）、`/tasks`（列表）、`/task close`（归档）；`/move` 批准路径加建议提示。
- `memory/sqlite.rs`：sessions 补两列 + 任务枚举/归档查询。
- `agent/mod.rs`：session 切换复用 `/new` 路径；进出任务线时同步 `trusted_dirs`（进：bound_path 已在其中则免审批天然成立；出：保留，受信不随退出撤销）。
- Runtime Context：当前 active 线名称/绑定目录注入状态栏（模型需要知道自己身在哪个任务）。

## 明确不做

- 并行双任务线（per-channel session 路由）。
- 自动归档规则（「完成」的判定靠人，`/task close` 显式触发）。
- 任务线独立的压缩策略/预算（沿用现有 context 压缩，先跑起来再看）。

## 未决（grill 时确认）

1. Q1/Q2/Q4 的推荐项是否成立。
2. `/task` 无参时行为：列出任务并提示用法，还是回到通用线？（倾向：无参 = 回通用线，`/tasks` 看列表）
3. 任务线里再 `/move` 到别的目录：允许（bound_path 更新？）还是拒绝（先回通用线再 move）？（倾向：允许，bound_path 更新为新受信目录——与 #B 语义一致）
4. WebUI 会话列表是否区分展示任务线（kind 标签）。
