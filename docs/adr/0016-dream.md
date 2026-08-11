# ADR-0016: 「做梦」——闲时自动整理记忆

- 状态：Accepted
- 日期：2026-08-10
- 关联：[ADR-0013 cron 定时任务调度](0013-cron-scheduling.md)、[docs/plan.md P4](../../plan.md)

## 背景

MEMORY.md 目前只在超限时被动压缩（`src/memory/markdown.rs` 的 `compress_memory`），长期使用必然堆积重复、过期、互相矛盾的条目；而会话里产生的有效信息，只有用户手动 `/remember` 才能沉淀。用户提出「闲时自动整理记忆」（做梦）：

- 空闲窗口触发一次离线自省：读近期 sessions.db 会话 → 抽取值得长期保留的事实 → 合并进 MEMORY.md（必要时更新 USER.md 画像）→ 去重压缩。

初步方案曾写成「独立轻量闭环 / 影子 agent」，经 grilling 被用户否定——它凭空造出第三类东西，与 LLAIA 现有架构（主 Agent + Channel 共享同一个 `Arc<Mutex<Agent>>`、cron 走「agent 模式唤醒主 Agent」）不对齐。多轮 grill 后锁定如下设计。

## 决策

### 1. 架构（最关键）：复用 cron agent 模式，不引入新 agent 架构

做梦 = 一个 **Agent 模式的 cron 任务**，复用 ADR-0013 的 `run_agent_mode`（`src/cron/runner.rs`）：拿共享的 `Arc<Mutex<Agent>>` → 创建独立 session（`source = cron:<id>`）→ `run_isolated_turn` 跑一轮 → 回复推送 pusher。

- `run_isolated_turn`（`src/agent/mod.rs:195`）先存下主会话 `session_id` + `context`，换上干净 context 跑标准 `handle_input`（全套工具含 `memory_write` 都在），跑完再还原——**主会话历史零污染**，记忆改动经 `memory_write` 落盘，做梦轮的「思考」留在 `cron:<id>` 独立 session（WebUI 可过滤）。
- 否决的「影子 agent / 独立闭环」：会造出新架构类别，且与「复用主 Agent」能力重复，没必要。
- 因此**不新增任何 agent 架构类别**，做梦只是既有的 cron + 独立 session 组合。

### 2. 触发：cron 定时 + 运行时空闲门控

- cron 定时调度（如每日凌晨）+ 运行时「距最后一条用户消息 N 分钟无交互」的空闲门，二者都满足才执行。
- 不做独立 idle 检测器（参考 openclaw 即纯 cron；用户认同 cron + 空闲检查足够）。
- 空闲门的「距上次消息多久」依赖可信本地钟 → 依赖 P4-a 时区改造，但为「配套」非「阻塞」（cron 时间本身可配置）。

### 3. 模型：主模型 + fallback

- v1 直接用主模型（本地模型零成本，且 idle 门已保证不与活跃会话争资源）。
- 不起独立 `compact_provider` 实例。若未来要显式切便宜模型，是给 cron 加可选 `agent_profile` 字段的小增强，仍属复用 cron 模式，非新架构。

### 4. 写入边界：MEMORY 全编辑，USER.md v1 不碰

- MEMORY.md 放开**全编辑（增 / 改 / 删）**——这是「去重压缩」价值成立的必要条件；只追加会废掉核心价值。
- USER.md v1 **不自动改写**（身份绑定文件，自动改画像风险过高）；做梦最多在 diff 摘要里「提议」USER 改动，不真写。

### 5. 安全兜底：让「全编辑」不变成静默灾难

| 环节 | 做法 |
|---|---|
| 事前备份 | 跑前复制 MEMORY.md 为带时间戳备份，留最近 N 份 |
| 事后 diff | 跑后算「备份 vs 新 MEMORY」差异，把「新增 / 改动 / 删除」条目摘要推送给用户（走 pusher）——**记忆变更绝不静默** |
| 手动触发 | `/dream` 斜杠命令立即触发本次做梦（即便自动关闭也能手动跑） |
| 回滚 | `/dream-rollback` 从最近一份备份还原 MEMORY.md |
| 默认开关 | **默认开**：idle 门（不打断）+ diff 通知（不静默）+ 回滚（错了能撤）三道防线使默认开风险可控 |

### 6. 与「Agent 状态栏」对齐（关联 P4-a 时区）

做梦的空闲读数（距上次消息 N 分钟）与全局时间感知共用 P4-a 的「Agent 状态栏」机制，依据《深入理解 AI Agent》李博杰 v1.2：

- §2.6.3 规定动态元信息作为**一条 user 角色消息插入上下文末尾**（借用 user 消息槽位，内容非真实用户输入），**不修改开头 system 消息**以保住 system 前缀的 KV Cache。
- §2.6.5 进一步指出：**只给裸时间戳模型不会据此改变行为**（与「不给」相差仅几个百分点）；真正把通过率拉高 +19~+49 个百分点的是**附带「操作手册」**——所以状态栏要带「读数 + 简短用法提示」（如「距上次对话已过 12 分钟，空闲中」「当前非工作时间」），做梦与全局时间感知共用这一读数。

## 不做

- 不做独立 idle 检测器子系统（cron + 空闲门足够）。
- 不引入新 agent 架构类别（否决影子 agent / 独立闭环）。
- v1 不起独立便宜模型实例（主模型 + fallback）。
- USER.md 自动改写（v1）。
- 「只追加 / 追加 + 标记待清理」的保守写入（会废掉去重压缩价值）。

## 影响

### 代码变更

- `cron.toml` 新增一个 agent 模式任务（或内置默认 dream 任务），prompt 指示「回顾最近 N 天会话，整理进 MEMORY.md，去重压缩，先备份」。
- 新增 `/dream`、`/dream-rollback` 斜杠命令。
- `src/memory/` 新增 MEMORY.md 备份 / 回滚辅助（带时间戳，留最近 N 份）。
- 复用：cron scheduler、`run_agent_mode`、`memory_write`、sessions.db、pusher。

### 依赖

- 依赖 P4-a 时区改造（空闲读数的本地钟）。
- 与 P4-c「更聪明的上下文压缩」共用抽取 → 合并 → 压缩思路。

## 参考

- [ADR-0013](0013-cron-scheduling.md) cron 定时任务调度（run_agent_mode / 独立 session）
- 《深入理解 AI Agent》李博杰 v1.2：§2.6.3 Agent 状态栏（位置）、§2.6.5 物理时间感知、§2.7.2 / §2.7.3 上下文压缩与 KV Cache
- 本轮 grilling（Q1 触发 / Q2 架构 / Q3 模型 / Q4 写入边界 / Q5 安全兜底 + 状态栏）
- openclaw 的 dream 机制（即 cron，作为 cron + 空闲检查的先例）
