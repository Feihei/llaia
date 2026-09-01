# ADR-0031: 任务 session 模型——通用线 + 任务线

- 状态：Accepted（已实现 2026-09-01，实现记录见文末）
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

## 决策分支（四个待决问题；grill 2026-09-01·3 全部拍板，定案见文末「grill 定案记录」）

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

## 追加 grill 轮（2026-09-01·2）：必要性重审与新未决

**前提修正（用户质疑，代码核实成立）**：session 切分的上下文收益只存在于单次进程长运行期内——启动时 `Context.history` 从空开始、历史不回灌，`context.clear()` 是唯一真正丢弃垃圾上下文的机制（自动压缩只摘要化，摘要留在上下文且层层再压缩）；重启后切分差异归零，历史可达性全靠跨 session 的 `memory_research`。因此「按任务切分通用线」的必要性被高估。

**调整方向（grill 2026-09-01·3 裁决）**：

- ~~通用线自动周期切分~~——**否决**：短上下文模型会频繁触发自动压缩，若切分挂压缩则 session 列表被高频新建灌爆（用户反例）；且自动切分收益只存在于进程长运行期，该场景手动 `context.clear()` 已覆盖。**通用线维持现状**（一条流水 + `/new` + 自动压缩，零改动）。
- **任务线保留**：价值锚点重定义成立——隔离（垃圾上下文不出任务线）+ 可整体归档 + 绑定受信目录，而非「历史可用性」（memory_research 已覆盖）。低频显式机制。

**新增未决（grill 2026-09-01·3 已裁决，见下方定案记录）**：

5. Q3 现案「切回任务 session 仅 context.clear() 不回灌」使「续做任务线」体验不成立——切回时是否从 sqlite 回灌该 session 尾部 N 条（或注入其存档摘要）？倾向：回灌尾部（`state='archived'` 的不回灌，提示已归档）。
6. ~~自动切分触发器选型~~——随自动切分否决而作废。
7. ~~自动切分与任务线的相互作用~~——随自动切分否决而作废。

## grill 定案记录（2026-09-01·3，逐项拍板完毕）

**总形态**：通用线维持现状，**不做任何自动周期切分**（Q6/Q7 作废）；任务线为显式低频的 `/task` 机制。

**回灌（Q5 定案）**：一切线间切换都**回灌目标线 sqlite 尾部**（纯 SELECT、零 LLM 调用、无性能成本，与「回灌摘要」方案相比不受触发频率影响）：

- 进任务线：回灌**通用线**尾部若干条，任务不丢主线背景；
- 切回任务线：回灌**该任务线**尾部（`state='archived'` 不回灌，仅提示已归档）；
- 退出任务线回通用线：回灌**通用线**尾部——通用线在 sqlite 持续增长而进程内未加载，不回灌则「切换即失忆」。
- 实现细节：按字符预算封顶、不截断半条消息；回灌按 session 记录游标（last backfilled message id），同进程内对同一 session 反复切换不重复回灌已灌部分。

**Q1 触发（定案，推荐案 B）**：显式 `/task <名>` + `/move` 批准后**提示建议**为该目录开任务 session（不自动创建）；按目录/行为规则自动创建否决。

**Q2 可发现性（定案，推荐案 A）**：新增 `/tasks` 命令列未归档任务线，与 `/task <名>` 进出；**与 todo 并行不合并**——实现层零成本佐证：todo 存储本就按 session uuid 分文件（`tools/todo.rs::file_path`），任务 session 天然独享自己的 todo。

**Q3 持久化（定案，按推荐 schema）**：`sessions` 幂等补 `kind`（main|task）/ `bound_path` 两列；归档复用 `state='archived'`；title 复用现有列（`/task <名>` 写入）。

**Q4 跨频道（定案，推荐案 B）**：`/task <名>` 任意频道可切换，**全局单活跃**；不做 per-channel 并行活跃。

**无参 `/task`（未决 2 定案）**：切回 `kind=main` 的最近活跃线并回灌尾部；看列表用 `/tasks`。

**任务线内 `/move`（未决 3 定案）**：新目录照 #B 进 `trusted_dirs`，**`bound_path` 不改写**——bound_path 是元数据（归档列表/展示用），不参与审批/执行判定（判定以 workspace_root ∪ trusted_dirs 并集为基准），改写反而丢失多目录任务的归属历史。

**WebUI（未决 4 定案）**：会话列表 `kind` 徽标区分 main/task，`list_sessions` 带出 `kind`，归档会话可筛看；列为实施附带小项，不单独设计。

**状态**：已定案、待实现。

## 实现记录（2026-09-01）

按「grill 定案记录」全量落地：

- **schema**（`memory/sqlite.rs`）：`sessions` 幂等补 `kind`（默认 `main`）/ `bound_path` 两列（存量库 `ALTER TABLE`，同 title 先例）；`create_task_session` / `find_open_task`（按名取最近活跃）/ `list_open_tasks` / `archive_session`（state='archived'）/ `latest_main_session`；`latest_session` 排除归档线（归档不续接）。
- **切线回灌**：`recent_messages_within_budget`（尾部按 6000 字符预算封顶、不截半条）+ `slash.rs::backfill_context`（只回灌 user/assistant 正文——tool 消息的 tool_call_id 配对无法从 messages 表重建，硬塞会产生孤儿 tool 消息违反 OpenAI 协议）；切换必 `context.clear()` → 回灌天然幂等，无需跨切换游标。
- **命令**（`commands/slash.rs`）：`/task <名>`（存在→切换+回灌该线尾部；不存在→新建+回灌通用线尾部当 brief，bound_path=当前目录≠home 时绑定）；`/task`（无参→回通用线+回灌）；`/task close`（归档+回通用线）；`/tasks`（列表，`*` 标当前）。`/new` 顺带 `refresh_task_state`。
- **Runtime Context**：`Context.task_state`（`agent/mod.rs::refresh_task_state`，turn 起点与切线时刷新）注入任务名+绑定目录，与 todo/env 同区（KV 缓存友好）。
- **/move 提示**：批准路径 notice 追加「tip: `/task <name>` 可开绑定该目录的隔离任务」（不自动创建）。
- **WebUI**：`list_sessions` 带出 `kind`，会话列表 `[task]` / `[archived]` 徽标。
- **回归测试**：`test_task_switch_backfill_and_archive`（新建/切回/无参回主线/close 归档/同名重建五段）、`test_task_session_lifecycle`、`test_recent_messages_within_budget`、`test_task_columns_added_to_legacy_db`（存量库迁移）、`test_refresh_task_state_injects_runtime_context`。
