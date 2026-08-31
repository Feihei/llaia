# ADR-0016: 「做梦」——闲时自动整理记忆

- 状态：Accepted（2026-08-11 增补两阶段管线设计）
- 日期：2026-08-10
- 关联：[ADR-0013 cron 定时任务调度](0013-cron-scheduling.md)、[docs/plan.md P4](../../plan.md)
- 参考实现：nanobot 的 Dream 两阶段设计（Consolidator → `history.jsonl`；Dream → 手术式编辑 SOUL/USER/MEMORY.md，git 版本化）

## 背景

MEMORY.md 目前只在超限时被动压缩（`src/memory/markdown.rs` 的 `compress_memory`），长期使用必然堆积重复、过期、互相矛盾的条目；而会话里产生的有效信息，只有用户手动 `/remember` 才能沉淀。用户提出「闲时自动整理记忆」（做梦）：

- 空闲窗口触发一次离线自省：读近期 sessions.db 会话 → 抽取值得长期保留的事实 → 合并进 MEMORY.md（必要时更新 USER.md 画像）→ 去重压缩。

初步方案曾写成「独立轻量闭环 / 影子 agent」，经 grilling 被用户否定——它凭空造出第三类东西，与 LLAIA 现有架构（主 Agent + Channel 共享同一个 `Arc<Mutex<Agent>>`、cron 走「agent 模式唤醒主 Agent」）不对齐。多轮 grill 后锁定如下设计。

2026-08-11 增补：做梦的写入落地方式从「单趟读历史→直接改 MEMORY」升级为**两阶段管线**（参考 nanobot）。短期/长期记忆分离的隐喻更贴切，且把「昂贵的 MEMORY 编辑」与「高频的历史蒸馏」解耦。详见 §2 与 §7。

## 决策

### 1. 架构（最关键）：复用 cron agent 模式，不引入新 agent 架构

做梦 = 一个 **Agent 模式的 cron 任务**，复用 ADR-0013 的 `run_agent_mode`（`src/cron/runner.rs`）：拿共享的 `Arc<Mutex<Agent>>` → 创建独立 session（`source = cron:<id>`）→ `run_isolated_turn` 跑一轮 → 回复推送 pusher。

- `run_isolated_turn`（`src/agent/mod.rs:234`）先存下主会话 `session_id` + `context`，换上干净 context 跑标准 `handle_input`（全套工具含 `memory_write` 都在），跑完再还原——**主会话历史零污染**，记忆改动经 `memory_write` 落盘，做梦轮的「思考」留在 `cron:<id>` 独立 session（WebUI 可过滤）。
- 否决的「影子 agent / 独立闭环」：会造出新架构类别，且与「复用主 Agent」能力重复，没必要。
- 因此**不新增任何 agent 架构类别**，做梦只是既有的 cron + 独立 session 组合。

### 2. 两阶段管线（2026-08-11 增补，参考 nanobot）

做梦不再「单趟读历史→直接改 MEMORY」，而是拆成顺次执行的两阶段，**两阶段都跑在独立 cron 会话（`run_isolated_turn`）里**，复用同一套 machinery，不学 nanobot 的行内/后台双路径：

| 阶段 | 做什么 | 落哪个文件 | 是否进上下文 |
|---|---|---|---|
| **stage1 蒸馏** | 临时 dream 会话读取增量历史，抽取值得长期保留的事实，LLM 总结成草稿 | `dream_draft.md`（中间缓冲） | ❌ 不进上下文 |
| **stage2 整理** | 基于 `dream_draft.md` + 当前 MEMORY.md，手术式合并/去重/删陈旧 | `MEMORY.md`（最终记忆） | ✅ 进上下文 |

- **为什么拆两阶段**：
  1. stage1 可高频跑（纯蒸馏、便宜），stage2 低频跑（昂贵的 MEMORY 编辑不必每次都做）；
  2. stage2 失败**不丢** stage1 缓冲（历史不漏，下次 stage2 接着消化）；
  3. 游标增量天然落在两阶段之间（见 §3），崩溃可续跑、不重复消化旧历史。
- **中间文件命名 `dream_draft.md`，不是 `dream.md`**：被否决的 openclaw 概念里 `dream.md` 是「梦境日记」式**最终记忆**；这里的中间文件是**草稿/缓冲**，语义相反。最终记忆永远只在 `MEMORY.md`，`dream_draft.md` 只是离线过渡产物（可定期清理）。
- **stage1 写 `dream_draft.md`**：通过 `file_write` 覆盖式重写（每轮基于「上次 draft + 本轮新历史」重算），或追加式累积；v1 用覆盖式重写，单文件最简。
- **stage2 改 `MEMORY.md`**：用扩展后的 `memory_write`（支持全编辑，见 §4）做增/改/删；`memory_write` 由做梦轮调用，普通对话轮仍只用追加语义。

### 3. 游标增量（2026-08-11 增补，参考 nanobot `.dream_cursor`）

- sqlite 新增一张 `kv` 表（或复用现有机制）存 `last_dream_message_id`，记录做梦已处理到的 `messages.id` 游标。
- stage1 只读取 `messages.id > last_dream_message_id` 的**新消息**（分页批量，默认每轮上限 N 条，如 200），成功消化后**推进游标**。
- 首启迁移：游标初始置为「当前 messages 最大 id」，即**不重放整段老历史**（避免把历史爆米花重做一遍）；做梦只管「截至今夜之后的新内容」。
- 好处：增量、可续跑、崩溃安全、不重复消化。**优于 ADR-0016 原写的「回顾最近 N 天会话」全量重读**。

### 4. 触发：cron 定时 + 运行时空闲门控

- cron 定时调度（如每日凌晨）+ 运行时「距最后一条用户消息 N 分钟无交互」的空闲门，二者都满足才执行。
- 不做独立 idle 检测器（参考 openclaw 即纯 cron；用户认同 cron + 空闲检查足够）。
- 空闲门的「距上次消息多久」依赖可信本地钟 → 依赖 P4-a 时区改造，但为「配套」非「阻塞」（cron 时间本身可配置）。

### 5. 模型：主模型 + fallback

- v1 直接用主模型（本地模型零成本，且 idle 门已保证不与活跃会话争资源）。
- 不起独立 `compact_provider` 实例。若未来要显式切便宜模型，是给 cron 加可选 `agent_profile` 字段的小增强，仍属复用 cron 模式，非新架构。

### 6. 写入边界：MEMORY 全编辑，USER.md v1 不碰

- **最终记忆只落在 MEMORY.md**，`dream_draft.md` 只是离线草稿、永不进上下文、可定期清理。
- **stage2 的「全编辑（增 / 改 / 删）」通过「整体重写」实现**：做梦轮让 agent 产出**完整的新 MEMORY.md 内容**，由 coordinator 备份旧文件后覆盖写入。这等价于全编辑（增/改/删都发生），且比行级手术编辑更稳、不受 cron 下 `confirm_mode` 拦截工具的影响。
- agent 主动写记忆仍走 `memory_write`（只追加语义，不变）；做梦轮**不**调 `memory_write` 做行级编辑，避免 confirm 阻断 + 行级编辑脆弱。
- USER.md v1 **不自动改写**（身份绑定文件，自动改画像风险过高）；做梦最多在 diff 摘要里「提议」USER 改动，不真写。

### 7. 安全兜底：让「全编辑」不变成静默灾难

| 环节 | 做法 |
|---|---|
| 事前备份 | 跑前复制 MEMORY.md 为带时间戳备份，留最近 N 份 |
| 事后 diff | 跑后算「备份 vs 新 MEMORY」差异，把「新增 / 改动 / 删除」条目摘要推送给用户（走 pusher）——**记忆变更绝不静默** |
| 手动触发 | `/dream` 斜杠命令立即触发本次做梦（即便自动关闭也能手动跑） |
| 回滚 | `/dream-rollback` 从最近一份备份还原 MEMORY.md |
| 默认开关 | **默认开**：idle 门（不打断）+ diff 通知（不静默）+ 回滚（错了能撤）三道防线使默认开风险可控 |

**版本化方案：保留手写时间戳 `.bak`，不引入 git。** nanobot 用 git 给记忆文件做 diff/回滚；经评估，LLAIA 是本地单用户场景，手写 `.bak` + 自算 diff 已足够轻量、零额外依赖，故不引入 git 版本化（2026-08-11 决策）。

### 7.1 事后修订（2026-08-31）

本文有两处已被现实推翻，记录在此以免后人误信：

1. **§6「整体重写比行级手术编辑更稳」不成立。** stage2 让模型重写整份文件，而它读了喂进去的现有记忆后入戏——记忆里本身就写着「幽默、讽刺、自嘲，不对就直说」「称呼其为 Boss」这类人格指令——于是把「编辑文件」当成「跟用户说话」，回了一段反问用户「morning_news 是 7:30 还是 8:00 跑」的散文。它非空，被原样覆盖进 `MEMORY.md`，日志还记成 `dream completed`、游标照推：坏文件与「这批消息已消化」一起吞掉。同日已补 `memory::dream::validate_memory_candidate`（写盘前形状校验，不合规则**不写盘也不推进游标**），但这只能拒非法形状，防不住「形状合法却静默丢条目」——真正的病根是一次 LLM 输出的作用域等于整份长期记忆，根治见 **ADR-0029（提议取代 §6 的整体重写）**。
2. **本表「事后 diff → 推送给用户，记忆变更绝不静默」在内置 `dream` 任务上不成立。** 该任务 `channel = cli`，而 cli pusher 无持久连接、推送被丢弃——上面那次写坏连续几晚无人察觉，缺的就是这个反馈环。现 diff 摘要同时写入 info 日志（`dream completed` 带 `summary` 字段）作兜底；是否把推送通道改为「用户当前活跃频道」留给 ADR-0029 待议 1。

### 8. 与「Agent 状态栏」对齐（关联 P4-a 时区）

做梦的空闲读数（距上次消息 N 分钟）与全局时间感知共用 P4-a 的「Agent 状态栏」机制，依据《深入理解 AI Agent》李博杰 v1.2：

- §2.6.3 规定动态元信息作为**一条 user 角色消息插入上下文末尾**（借用 user 消息槽位，内容非真实用户输入），**不修改开头 system 消息**以保住 system 前缀的 KV Cache。
- §2.6.5 进一步指出：**只给裸时间戳模型不会据此改变行为**（与「不给」相差仅几个百分点）；真正把通过率拉高 +19~+49 个百分点的是**附带「操作手册」**——所以状态栏要带「读数 + 简短用法提示」（如「距上次对话已过 12 分钟，空闲中」「当前非工作时间」），做梦与全局时间感知共用这一读数。

## 不做

- 不做独立 idle 检测器子系统（cron + 空闲门足够）。
- 不引入新 agent 架构类别（否决影子 agent / 独立闭环）。
- v1 不起独立便宜模型实例（主模型 + fallback）。
- USER.md 自动改写（v1）。
- **git 版本化记忆**（本地单用户，`.bak` + diff 已够用、零依赖）。
- 「只追加 / 追加 + 标记待清理」的保守写入（会废掉去重压缩价值）。
- openclaw 式「梦境日记 dream.md」（最终记忆以 MEMORY.md 为准；`dream_draft.md` 只是离线草稿，非最终记忆）。

## 影响

### 代码变更

- 新增内置 `dream` cron 任务（默认 enabled，agent 模式），由专用入口 `run_dream` 编排两阶段，而非直接 `run_agent_mode`：先空闲门控 → 读增量历史（按 `last_dream_message_id` 游标）→ stage1 蒸馏（agent 产出草稿文本，coordinator 写 `dream_draft.md`）→ stage2 整理（agent 产出**完整新 MEMORY.md 内容**，coordinator 备份后覆盖）→ 推进游标 → 算 diff 推送。
- `src/memory/sqlite.rs`：新增 `kv` 表 + `get/set_last_dream_message_id`；新增 `messages_after(id, limit)`（排除 `cron:` 会话，防做梦轮自己被消化）+ `last_user_message_time()`（空闲门读数）。
- `src/cron/dream.rs`（新）：`run_dream` coordinator + stage1/stage2 Dream prompt 模板（基于 nanobot `dream.md` 改造）。
- `src/cron/runner.rs`：`run_agent_mode` 增加可选空闲门控（读 `last_user_message_time()`，距上次消息 < `idle_minutes` 则跳过）；CronTask 新增可选 `idle_minutes` 字段（仅 dream 等任务用，其它任务留空 = 不门控）。
- 新增 MEMORY.md 备份 / 回滚辅助（带时间戳 `.bak`，留最近 N 份）+ 备份 vs 新文件 diff 摘要（`src/memory/dream.rs` 新）。
- 新增 `/dream`、`/dream-rollback` 斜杠命令（slash.rs）。
- 复用：cron scheduler、`run_isolated_turn`、sessions.db、pusher、`compress_memory`、现有 `file_read`（做梦轮读 MEMORY.md 当前内容）。

### 依赖

- 依赖 P4-a 时区改造（空闲读数的本地钟）。
- 与 P4-c「更聪明的上下文压缩」共用抽取 → 合并 → 压缩思路。

## 参考

- [ADR-0013](0013-cron-scheduling.md) cron 定时任务调度（run_agent_mode / 独立 session）
- 《深入理解 AI Agent》李博杰 v1.2：§2.6.3 Agent 状态栏（位置）、§2.6.5 物理时间感知、§2.7.2 / §2.7.3 上下文压缩与 KV Cache
- 本轮 grilling（Q1 触发 / Q2 架构 / Q3 模型 / Q4 写入边界 / Q5 安全兜底 + 状态栏）
- openclaw 的 dream 机制（即 cron，作为 cron + 空闲检查的先例）
