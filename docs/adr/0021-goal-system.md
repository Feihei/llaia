# ADR-0021: 长期目标系统（/goal）

- 状态：**已撤销**（2026-08-31，随 v0.3.2 移除；v0.3.0 起曾实现并发布）
- 日期：2026-08-14
- 关联：P5 目标系统；参考 zeroclaw、hermes、nanobot

## 撤销（2026-08-31）：功能整体移除

实现（文件方案）交付后从未产生真实使用，复核时判定其收益不抵引入的问题，整体删除：`src/goal/`、`src/tools/goal.rs`、`/goal` `/goal-list` `/goal-done` `/goal-cancel` 四条命令、`Context.goal_state` 注入、`GET /api/goal` 与 WebUI GOAL 面板一并摘除。理由：

1. **没有自动收尾 = 永久污染**。单活跃目标存放在 agent 家目录一份 `goal.md`，跨 session、跨全部频道生效，而 `Active → Done/Cancelled` 完全依赖用户记得手敲 `/goal-done` 或 agent 自觉调 `goal` 工具——无过期时间、无失效判定。目标一旦作废又没关掉，此后每一轮对话都会被强行注入一句过期意图，把无关提问往旧目标上扯。这是本设计结构性缺陷，不是使用技巧能弥补的。
2. **目标本身难以定义**。"跨多轮持续推进的同一意图"在单用户私人助理的真实用法里没有清晰边界：用户提出的多数诉求当轮就该结束，长期意图（身份、偏好、待办方向）天然属于 MEMORY.md（已在 system prompt 全量常驻），一次任务的步骤拆解属于 `todo`（会话级，有明确的 done 状态）。三者职责重叠，`/goal` 提供的是第四套平行机制。
3. **agent 可覆写用户设定的目标**。`goal` 工具的 `set` 动作允许模型改写 objective，且 `requires_confirm() = false`——把用户手写的长期意图交给模型改写，收益不明显而风险方向明确。
4. **无使用数据支撑**。开发者本人未使用过，默认 workspace 下亦无 `goal.md` 产物。

**不做"加开关"折中**：项目约定不为无人使用的功能保留 feature flag，`goal.enabled` 只是给死代码续命。若日后确有需要，应重新立项，且必须先解决收尾问题（过期时间 / 每次注入需显式确认 / 会话级而非全局级）。

> 长意图的替代路径：`/remember <text>` 写 MEMORY.md（每轮在 system prompt，永久常驻）；会话内拆解用 `todo`；定时推进用 `cron`；长任务脱离主回合用 `delegate`。

以下内容为原设计记录，保留备查。

## 修订（2026-08-17）：持久化改为 `goal.md` 文件，不进 session schema

P5-7 实施前复核，将原先「决策 #2：sessions 表加 `metadata` 列」整体推翻，改为**文件持久化**。理由：

1. **语义**：长期目标是跨多轮持续推进的同一意图，本就跨 session；而 llaia 的 `sessions` 一行 = 一场对话（[`src/memory/sqlite.rs`](../src/memory/sqlite.rs)）。把跨会话意图绑在单场会话 metadata 上语义拧，新开会话也不会再注入。
2. **零迁移 / 零回滚风险**：虽 `ALTER TABLE sessions ADD COLUMN metadata TEXT` 本身非破坏、向后兼容，但为低频功能引入 schema 仍不必要；文件方案彻底不碰 `sessions`。
3. **更干净**：goal 根本不进消息历史，省 token、无需"压缩时永留"的特殊处理；每轮直接从文件重新注入，永远新鲜。
4. **一致**：`<config_dir>/workspace/goal.md`（默认 `~/.llaia/workspace/goal.md`）与 SOUL.md/USER.md/MEMORY.md/sessions.db 同处 agent 家目录；该路径对 `file_write`/`file_edit` 不可见（`config_dir` agent 工具不可访问，[`src/agent/mod.rs`](../src/agent/mod.rs)），由专用 `/goal` 命令 + 专用 `goal` 工具读写，不蹭通用文件工具。

**新决策（取代原决策 #2）**：
- 单活跃 goal，持久化于 `<config_dir>/workspace/goal.md`（YAML frontmatter：`status`/`created_at`/`updated_at` + 正文 `# Goal` 目标与 `## Progress` agent 维护进度）。
- 不新增 `sessions` 任何列；原决策 #2 引用的「压缩时永留」随之作废（goal 不入历史）。
- 原决策 #3（Runtime Context 注入）、#5（slash 命令）、#6（双轨完成）、#7（WebUI 可视化）不变。
- 难度从 ★★☆ 降为 ★☆☆。

## 背景

LLAIA 目前没有"跨多轮持续推进的同一目标"机制：每次对话都是独立的请求-响应，agent 不会在用户离开后继续推进一个长期意图。`/goal` 的需求来自用户，参考了 zeroclaw / hermes 的 goals 概念与 nanobot 的 sustained goals 实现。

nanobot 的做法值得借鉴：
- `/goal` 把目标写进 **session metadata**（`{status:"active", objective, ui_summary}`）；
- 激活后每轮把目标文本注入 **Runtime Context**，让模型始终"记得"在干什么；
- 长目标 turn **豁免 LLM wall-clock 超时**（长任务不被掐断）；
- WebUI 通过 WS `goal_state` 事件展示进度。

核心区分：`cron` 是"定时触发一次"；`/goal` 是"持续推进的同一目标"，二者正交（goal 可内部派生子任务或定时触发，但不自动等价于 cron）。

## 决策

### 1. 单活跃 goal 模型

每个 session 至多一个 active goal，结构：

```rust
pub struct GoalState {
    pub status: GoalStatus,        // Active | Done | Cancelled
    pub objective: String,         // 用户设定的目标文本
    pub ui_summary: Option<String>,// 一句话进度摘要（agent 维护）
    pub created_at: i64,
    pub updated_at: i64,
}
pub enum GoalStatus { Active, Done, Cancelled }
```

多 goal 并发不在首版范围（nanobot 也是单活跃）。`/goal <新目标>` 覆盖式重设。

### 2. 持久化：sessions 表 metadata 列（sqlite 落盘）

`llaia` 的 `sessions` 表当前无 metadata 列，需**新增 `metadata TEXT` 列**（JSON blob），单活跃 goal 存一份 JSON（与 nanobot 的 `goal_state` key 一致），随 `sessions.db` 落盘。**压缩时不摘要掉**：goal 是用户长期意图，按 SOUL/USER 同级别永留（压缩策略里把 goal 列入"关键消息保留"集合）。

### 3. 运行时注入 Runtime Context

激活时，每轮 turn 前把目标文本追加进 Runtime Context（仿 nanobot `goal_state_runtime_lines`）：

```
Goal (active):
<objective>
Summary: <ui_summary>
```

### 4. 超时处理（已复核：无需豁免）

经核查，llaia 的 provider 调用**没有整块 wall-clock 超时**——只有 30s 连接超时、`max_tokens` 上限，以及流式的"120s 分片空闲超时"（连续 120s 不出 token 才触发，实际几乎不发生）。因此"长目标 turn 豁免超时"无对应超时可豁免，原决策作废。长目标 turn 天然不被掐断；唯一需留意的是极端长生成下的流式空闲超时，属 provider 侧行为，不在本 ADR 范围。

### 5. 命令

- `/goal <目标>`：设/覆盖 active goal；
- `/goal-list`：查看当前 goal 状态 + ui_summary；
- `/goal-done`：手动标记 `Done`（双轨之一，见决策 #6）；
- `/goal-cancel`：置 `Cancelled`（保留记录，不再注入上下文）。

### 6. 完成判定（双轨）

goal 进入 `Done` 有两条路径：① agent 判断目标达成后内部标记；② 用户显式 `/goal-done` 手动收尾。状态机为 `Active → Done / Cancelled`，避免"永远 Active 无人关"。`/goal-list` 同时展示 `ui_summary`。

### 7. 进度可视化

WebUI 时间线事件 + CLI 状态行；goal 状态变更经 WS 事件推送（仿 nanobot `goal_state` / `goal_status` 事件）。

## 与既有机制的关系

- 不依赖 cron；但 goal 推进过程中 agent 可自由调用 `cron` / `delegate` / `todo` 等工具。
- 复用现有 session 存储与压缩框架，无新存储后端。

## 后果

- 需改：session metadata 读写、`Agent` 运行时注入点、slash 命令、WebUI 事件与进度面板。
- 属结构性改造（新增持久化语义 + 运行时注入 + 超时分支），故立此 ADR。
- 不引入新依赖；纯 Rust + 现有 sqlite。
