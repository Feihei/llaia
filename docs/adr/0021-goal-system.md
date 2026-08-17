# ADR-0021: 长期目标系统（/goal）

- 状态：已采纳（文件方案，见「修订 2026-08-17」节）
- 日期：2026-08-14
- 关联：P5 目标系统；参考 zeroclaw、hermes、nanobot

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
