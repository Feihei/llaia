# /task → /session：会话与目录的建模

状态：**已定案**（2026-09-07，方案 2 骨架 + 方案 4 提示；rename-only + 双自愈 + 软提示，无锚概念、无作用域持久化）
日期：2026-09-07
关联：ADR-0031（任务线现状，本文构成其修订）、commit 5cbf60c（S1/S2/S4 上下文窗口修复，已完成，与本 plan 正交）

## 背景与问题

9/6 事故链暴露了任务线模型的三重错位（取证见 sessions.db session 20 + 当日日志）：

1. **重启续接选中任务线**：`latest_session()`（`src/memory/sqlite.rs:336-353`）不排除 `kind='task'`，16:03 重启直接落在 'llaia' 任务线上；而对话历史重启清零（ADR-0031 前提修正：`Context.history` 从空开始、不回灌）。**空记忆 + 有任务身份** 是最危险的组合——agent 带着 `[task]` 注入凭空脑补"在修 llaia 的 bug"，去 git log 挖仓库。
2. **bound_path 名不副实**：绑定目录是纯元数据（ADR-0031 定案"不参与审批/执行判定"），workspace_root 仍在家目录 → agent 名义上在 repo 干活、实际每次 file_read 撞 `outside workspace`，还幻觉出 `C:/e/play/...` 这类拼接路径。
3. **持久化语义不一致**：`/move` 的 root/trusted_dirs 是进程级内存、重启自愈；任务线状态却跨重启保留。两半各活在各的语义里。

改名 `/session` 的动机：与主流 coding agent（goose / Claude Code / Codex CLI / pi）词汇对齐，"task" 与 `cron` 的 mode=agent"任务"、todo 的"任务"三个概念撞名。

**设计前提（用户已定方向，本文不推翻）**：不强绑定 session↔目录——一个 working dir 可以有多个 session，一个 session 一生也可以 `/move` 过多个目录。以下所有候选都在这个 m:n 约束内展开。

## 现状矩阵

| 对象 | 现状 | 代码锚点 |
|---|---|---|
| 对话历史 | 重启清零（进程=记忆边界），sqlite 只留档 | `context.rs`，ADR-0031 line 85 前提修正 |
| 当前在哪条线 | 跨重启保留（`latest_session` 按 last_activity，含 task） | `cli.rs:621` |
| workspace_root（/move 结果） | 进程级 `Arc<RwLock<PathBuf>>`，全局共享，不持久 | `slash.rs:659-684` |
| trusted_dirs（#B） | 会话级内存（进程级），批准即登记、退线不撤销 | `agent/mod.rs:339-344` |
| bound_path | sqlite 持久，纯展示元数据 | `sqlite.rs:357-364` |
| task_state 注入 | 每回合 `refresh_task_state` 从 sqlite 现读 | `agent/mod.rs:342-371` |

已知连带缺口（不论选哪个方案都要单独拍板）：

- **cron/delegate 的 fork 共享全局 `workspace_root` Arc**（`fork_for_isolated`，`agent/mod.rs:566+`）：主线 `/move` 进仓库后，cron 的 terminal 也在仓库里跑。session 建模无论怎么选，fork 时都应 pin 明确的作用域（建议恒 pin 家目录，除非未来 session 自带 root）。
- 归档线不续接 ✓（`state='archived'` 已排除）；cron 会话排除 ✓（`channel NOT LIKE 'cron:%'`）。

## 参照项目对比（.ref 实测四家 + opencode 在线仓库实测）

| 项目 | 存储模型 | session↔目录 | 重启/续接 | 关键锚点 |
|---|---|---|---|---|
| **goose** | sqlite `sessions` 表，`working_dir NOT NULL` | **1 session : 1 目录（可变）**；resume 时 current≠saved → 交互问"切回去？"默认 yes，非交互 warn-and-stay | `--resume` 或隐式取 updated_at 最近的 **User 类** session（Scheduled/SubAgent/Hidden 排除） | `session_manager.rs:998-1028`、`builder.rs:352-412`、ACP 反向更新 `acp/server.rs:881` |
| **pi** | JSONL 按目录命名空间：`~/.pi/agent/sessions/<encoded-cwd>/<ts>_<uuid>.jsonl`；header 必含 cwd | **目录一等公民**：一目录多 session 天然成立；一 session 一 cwd、**不可移动**，换目录=fork 到新 cwd | `continueRecent(cwd)`：只在本目录的命名空间取最近；stored cwd 消失 → 问"就在当前目录续？"或硬报错 | `config.ts:571`、`session-manager.ts:1552-1675`、`session-cwd.ts:14-58`、per-folder 信任 `project-trust.ts` |
| **jcode** | 扁平 `sessions/<id>.json` + journal，旁挂 sqlite 元数据索引（working_dir 只是列） | **m:n 最彻底**：working_dir Optional 可变；客户端跨目录 attach 可**改绑**（带防 $HOME 覆写守卫）；`/clear` 换 session 记录但保留 working_dir | 显式 `--resume <id>`；崩溃恢复靠 active_pids 提示 | `session.rs:153`、`client_session.rs:518-553`、`recent_session_index.rs:40` |
| **deepseek-harness** | 全局 id 寻址、目录不入存储路径；cwd 在 header **optional** | 记录上可有可无，但 API 层 **owner-immutable 硬校验**：`ensureSession(id, cwd)` 不匹配直接 throw `ApiSessionCwdConflict`——"一 session 一目录"是强制约束不是元数据 | 工具每次调用按 `session.header.cwd` 解析相对路径（绝不跟 process.cwd()）；bash 另收 per-call workdir | `session-controller/commands.ts:65-97`、`agent.ts:29-41`、`tool-fs/session-cwd.ts:22-45` |
| **opencode**（`/move` 的灵感来源，实测 V2/dev 树） | 全局单库 sqlite `session` 表：`project_id`(FK) + `directory` + `path`(worktree 内相对子路径) + per-session permission 列；存储**不**按目录分家 | **目录可变、项目不可变**：`move-session` 只允许同一 project（git worktree 根）内换目录/子目录，跨项目直接 `DestinationProjectMismatchError`；移动=追加一条 `SessionEvent.Moved` 改列，**历史消息不改写**（旧消息带旧 cwd 语境），可选 `moveChanges` 把未提交 git 改动搬去目的地 | `--continue` 在**当前 project 作用域**取最近线；全局列表带目录标签供跨目录选择；stored dir 消失→按请求报错、无自动修复 | `session.ts:224-244`、`move-session.ts:77-138`、`projector.ts:242-255`、`workspace-routing.ts:86-88` |

样本结论修正：**"目录跟 session 走（可变）"有两家**——goose（单值可换 + resume 提示）与 opencode（可换，但被 project 围栏锁在一个仓库内）；jcode 是完全无围栏的 m:n；pi/DSH 不可移动。真正的一线差别不在"能不能换目录"，而在**有没有一个比 session 更稳的锚**（opencode = project/git worktree 根；审批池、LSP、配置等运行时状态按 directory 键控成 per-instance 运行时，与数据面分离），以及**移动时历史如何处置**（opencode：只改指针、不重写）。用户设想的 m:n 在 opencode 就成立（一目录多 session、一 session 多目录），但它给"一个 session 一生 touch 哪些目录"划了界：**仓库范围内随便动，跨仓库请开新 session**。

## 候选方案（grill 材料，均满足：改名 /session + m:n 前提）

### 方案 1：目录跟线不跟人（mutable last-writer，jcode / opencode 形、goose 启动形）

session 记录带一个 `current_dir`（可空=家目录）。`/move` 批准 = 更新**当前 session** 的 `current_dir` + 登记 trusted_dirs（写入 sqlite，取代纯内存）；进/切 session 时若 `current_dir`≠当前 root → 自动切换（目录存在性校验，消失则 warn 回家目录）。

- m:n 天然成立：同一目录开多 session 各自记各自的 current_dir；一个 session 多次 /move 留最后值（历史可选 journal 化）。
- **opencode 验证过该形态的主干**：move 只改指针列、不重写历史（`SessionEvent.Moved` + projector）；但它用 project 围栏限"锚内可动"——LLAIA 要不要这层围栏见开放问题 7。
- 一致性叙事最顺："进程=对话记忆边界；session=目录作用域的持久身份"。/move 与 /session 不再是两套语义。
- 待 grill 风险：**重启自动把 workspace_root 恢复到外部目录**=隐式恢复旧授权（trusted_dirs 要不要一起持久？不持久则首写仍需审批但 cwd 已在外，半吊子；持久则违背"重启自愈"）。单 Agent 全局 root 与 per-session root 的落差（切线才生效，同进程内 cron/多频道看到的仍是漂移的全局值）。

### 方案 2：目录跟进程，session 只带"出生地"（现状正名）

rename-only：`/task`→`/session`，`bound_path`→`origin_dir`（纯展示/检索元数据，同 DSH 的 query 字段或 pi 的列表分组思想，可加 `/sessions --dir X` 过滤）。`/move` 保持进程级不持久；重启**一律回家目录 + 回通用线**（`latest_session` 加 `kind='main'`，即上轮讨论的方向 A）。

- 最小实现、零新语义，把所有危险的决定（作用域持久化）留在门外。
- 代价：9/6 型"任务线续做"仍需两步（/session 名 + /move 路径），origin_dir 只是提示。agent"名义在 repo 实际碰不到"的错位靠 task_state 文案缓解而非根治（注入语改为"该线出生目录 X，当前作用域 Y，需要请 /move"）。

### 方案 3：目录命名空间（pi 形）

session 列表/存储按目录分组，`/sessions` 默认列当前目录的线；启动续接 = 本目录最近线。一 session 一出生目录，跨目录 = fork/重绑定为新线（显式命令）。

- "一个 working dir 多个 session"表达最充分，跨重启续做体验最顺（cd 到哪、接哪摊事）。
- 与"一个 session /move 多个目录"冲突（pi 用 fork 回避了这半题），改动最深（sessions 表/存储路径/WebUI/cron channel 全要重排）。

### 方案 4：1+2 折中——可查不可动

session 不持久化作用域，但 `/session close`/列表展示 origin/current 目录 + 启动时在通用线注入"未归档任务线清单（含各自目录）"提醒；恢复动作永远是显式的 `/session <名>` + 可选的跟随式 `/move` 提示（上轮 B-lite）。

## 定案（2026-09-07）

原则：**不复杂化**。/session（原 /task）只管历史记录与上下文回灌；/move 只管 bound_dir；workspace 权限始终是 `workspace_root ∪ trusted_dirs` 并集（现状不变）。重启双自愈：session 自动回主线、cwd 自动回家目录。两者其一修改后**提示**用户改另一个以对应，但不强制——同步与否留给用户。

**bound_path 语义澄清（m:n 的表达方式）**：单值列 = "该 session **当前**所在目录"（last-writer，jcode 同款），**不是出生地**。一个 session 多个 dir = 时间上的多次覆盖、只留最后——历史活在消息流里（与"回灌只取尾部、不重写历史"同一哲学），元数据只存"现在"；一个 dir 多个 session 无约束（多行同值，提示查询列出全部）。不用 origin 语义的原因：那样"主线 /move 有对应线"的提示会指向早已 move 走的线，提示不再是真话——矩阵每个 tip 都基于当前对应关系。

对照参照项目：这是 DSH"声明宽松"与 jcode"无锚 m:n"之间的最小编译——目录不进 session 的执行语义（bound_path 保持纯元数据），一致性靠提示而非机制。opencode 的锚/围栏、pi 的目录命名空间、goose 的 resume-cd 全部不引入。

对原开放问题的处置：

1. 重启恢复 → **消解**：双自愈，trusted_dirs/root 均不持久（维持"重启自愈"既有假设，bootstrap 依赖不变）。
2. 全局 root vs session root → **消解**：root 保持全局进程级；fork pin 家目录作为独立小修并入（见清单 6）。
3. 改名半径 → **命令面 rename + 保留 `/task` `/tasks` 别名**；`kind='task'` 值与内部命名（`ActiveTask`/`refresh_task_state`）**不动**（避免迁移与无效重构）；ADR-0031 追加修订节 + AGENTS.md/文档同步。
4. 隐式 resume → `latest_session()` 加 `kind='main'`：重启恒回主线；任务线靠显式 `/session <名>`（回灌机制不变）。pi 的消失目录问询不引入（bound_path 非执行语义，无从消失）。
5. cron 会话膨胀 → **保留为独立待办**，不在本 plan。
6. `/new` → 维持现状（同线清上下文），不动。
7. 锚概念 → **不引入**（"不要复杂化"直接否决）。
8. 移动的历史语义 → 回灌天然满足"只改指针不重写"；对应性提示落在切线 notice 里（清单 4）。
9. 审批池粒度 → 维持进程级 trusted_dirs 并集（"workspace 始终在并集"）。

### 实施清单

1. **rename**（`slash.rs`）：`/task`→`/session`、`/tasks`→`/sessions`；旧名保留为纯转发别名臂；`/help`、guide、WebUI 文案同步。
2. **重启回主线**（`sqlite.rs::latest_session`）：SQL 加 `AND kind = 'main'`；补/改测试。
3. **/move 管 bound_dir**（`slash.rs::resolve_approval` 的 `__move_workspace` 批准分支）：当前在任务线时，批准即 `set_bound_path(session_id, target)`（新增 SessionStore 方法，UPDATE bound_path）；主线不动 sqlite。notice 状态行显示 `bound to <dir> (was <旧值>)`——覆盖不静默。
4. **对应性软提示（提示矩阵，统一模板）**：`/session` 与 `/move` 的 notice 一律追加一行状态 `[scope] line "X" bound to D · current scope R`（或 `has no bound dir`），差异时附 tip、一致时只报状态——框架只摆事实，是否同步由用户判断：
   - 切线有 bound ≠ root → `tip: /move <dir> to align scope (optional)`；
   - 切线有 bound = root → 无 tip；
   - 切线无 bound → `tip: a /move inside this line will bind it`（线上 /move 经清单 3 自动绑定）；
   - 线上 /move → 显示 `bound to <dir> (was <旧值>)`（清单 3），绑定后必然一致无 tip；
   - 主线 /move 且**存在** open 线绑定目标目录 → `tip: /session <名> resumes the line bound here (optional)`（复用 `list_open_tasks()` 内存过滤，唯一新增查询）；
   - 主线 /move 无对应线 → 保留既有 tip（建议 `/session <名>` 开新线）。
5. **task_state 注入微调**（`agent/mod.rs::refresh_task_state`）：bound ≠ 当前 root 时加一句 "scope is currently Y; /move to align"（每回合现算，零存储）。
6. **fork pin 家目录**（`agent/mod.rs::fork_for_isolated`）：fork 后覆盖写 `workspace_root` 为 `self.workspace`（cron/delegate 永在家目录跑），修复"主线 /move 进仓库、cron 跟着进仓库"的共享 Arc 缺口。
7. **文档**：ADR-0031 追加修订节（重启续接改双自愈、未决项收录本文结论）、AGENTS.md 任务线段落、docs/guide/slash 命令页、CHANGELOG。

## 开放问题（原 grill 清单，已随定案逐条处置；5 号保留为独立待办）

1. **重启后 workspace_root/trusted_dirs 的恢复语义**——方案 1 的命门：自动恢复外部作用域是否越界？"上次批准过"跨重启还有效吗（单用户私人助理假设下可以放宽？）？
2. **全局 root vs session root**：LLAIA 一个 main Agent 实例挂多频道 + fork 共享 `Arc<RwLock<PathBuf>>`。若目录跟 session 走，cron/delegate fork 时 pin 什么？WebUI 多对话并发（未来）怎么隔离？这是比改名更根本的架构题，要不要现在就为它设计 per-turn root 快照？
3. **改名半径**：`kind='task'`→`'session'`（UPDATE 迁移 or 保留旧值）；`Context.task_state`/`ActiveTask`/`refresh_task_state` 内部命名；ADR-0031 追加修订节；斜杠命令要不要留 `/task` 别名一段时间？
4. **隐式 resume 规则**：重启续最近线时，任务线排除（方案 2）还是含线并跟随目录（方案 1）？pi 的"stored cwd 消失→问/报错"要不要抄？
5. **cron 会话归档膨胀**（顺带案）：session 6 攒了 2M 字符而 fork 永远空上下文——"复用会话防碎片化"的设计前提已死，cron 每次新开线或旧 cron 线定期归档，独立小决定。
6. **`/new` 与 session 的关系**：`/new` 是"同线清上下文"还是"新 session"？pi 的 `/clear`（换记录保留 working_dir）给了参照。
7. **要不要引入"锚"概念**（opencode 的 lesson）：它的可变目录被一个更稳的 project（worktree 根 / remote 哈希 / root-commit 哈希三级识别）围栏住——锚内随便 move，出锚直接拒绝（`DestinationProjectMismatchError`）。LLAIA 的对应物可以是"session 出生目录"或 worktree 根检测；不引入锚，则 jcode 的改绑守卫（拒绝把 session 绑回 `$HOME`）变成必选项。
8. **移动的历史语义**：opencode 明确"只改指针、不重写"，旧消息留在旧 cwd 语境。LLAIA 的回灌（只取 user/assistant 尾部）天然就是这个行为；grill 要定注入文案要不要显式告诉模型"这条线之前在 X 目录干过活"。
9. **审批池粒度**：opencode 的"总是允许"按 directory 键控、同目录多 session 共享、随实例销毁，与数据面分离；LLAIA 现状 trusted_dirs 是 Agent 进程级一维列表。要不要对齐 per-directory？cron fork 拿哪个池？

## 非目标（本 plan 明确不做）

- 不引入 per-message 多目录历史/journal（除非 grill 中方案 1 细化需要）；
- 不做目录级沙箱（`path_guard` 黑名单维持现状）；
- 不在本 plan 内动上下文压缩/窗口逻辑（5cbf60c 已收口）。
