# 实例化架构（ADR-0032）实现计划

状态：**待实施**
日期：2026-09-23
关联：[ADR-0032](../adr/0032-instance-architecture.md)（设计定案，本文为其实现计划）；[ADR-0031](../adr/0031-task-session-model.md)（被修订的会话线模型）

**Goal:** 任务线升级为可并行执行的 Agent 实例，serve 退化为宿主，WebUI 成为多实例并行入口。

**Architecture:** 在 `AgentRegistry` 之上加一层 `InstanceRegistry`（main + 任务实例，每实例独立 `Arc<Mutex<Agent>>`，进程内虚拟实例）；频道经 per-channel 附着指针串行路由（切线=idle 检查+换绑），WebUI 按面板自选实例并行；memory 分层（主 MEMORY 共享读 + 实例私有 MEMORY）；delegate worker 语义与 cron fork 通路不动。

**Tech Stack:** Rust（tokio，既有单 crate），sqlite WAL（复用既有 `SessionStore`），WebUI（Vue/Alpine + WS）。

---

## 关键现状锚点（2026-09-23 核实）

| 事实 | 锚点 |
|---|---|
| Channel trait 收 `Arc<AgentRegistry>` 而非裸 agent | `src/channels/mod.rs:24-39` |
| `AgentRegistry`：main + 子 agent map + steer/delivery | `src/agent/registry.rs:25-61` |
| `Agent` 字段：`session_store: Arc<SessionStore>`、`session_id: i64`、`context`、`approval_gate`、`workspace_root` | `src/agent/mod.rs:68-87` |
| `Agent::new` 构造 | `src/agent/mod.rs:276-308` |
| `fork_for_isolated`：共享 provider/store、独立 context/session 的 Agent 副本（**实例派生原形**） | `src/agent/mod.rs:638-661` |
| `run_turn(agent, msg, channel, sink, stop)` spawn 后 lock agent 执行全程（**锁被持有 = 回合进行中**） | `src/agent/sink.rs:57-94` |
| WebUI 每消息从 `state.registry.main.clone()` 取 agent | `src/channels/web.rs:231-240, 309-374` |
| `/session` 切线：`switch_session`/`switch_to_main`（session_id + context.clear + 回灌 + refresh_task_state） | `src/commands/slash.rs:862-966, 229-317` |
| `/cancel <id>` 已存在（question+审批通吃），文档漏记 | `src/commands/slash.rs:128-136` |
| `ApprovalGate`：HashMap pending，`list()` 可查空 | `src/agent/approval.rs:48-50, 142-146` |

**两条计划级简化（对 ADR 字面的偏离，实施时同步注回 ADR）**：

1. **SessionStore 共享不拆分**：ADR 写"每实例独立连接 + busy_timeout"——那是多进程载体的要求。v1 进程内虚拟实例共享现有单个 `Mutex<Connection>` 连接，进程内 Mutex 已串行化写入，零竞争且少写代码。独立连接 + `busy_timeout` 推迟到真进程载体落地时再做。
2. **busy 判定免新增标志**：回合期间 `Mutex<Agent>` 被持有（锚点见上表），`try_lock` 失败即 in-flight；成功后再查 gate 空。不引入 `turn_active: AtomicBool`。

---

## Task 0: 文档补漏（顺手项）

**Files:** Modify `AGENTS.md`（斜杠命令清单）

- [ ] 斜杠命令清单补 `/cancel <id>`（取消 pending question / 审批，已存在于 slash.rs:128）
- [ ] Commit: `docs: add missing /cancel to command list`

## Task 1: InstanceRegistry 核心模块

**Files:**
- Create: `src/agent/instances.rs`
- Modify: `src/agent/mod.rs`（`pub mod instances;`）
- Test: `src/agent/instances.rs` 内 `#[cfg(test)]`

- [ ] **Step 1: 写失败测试**——`test_instance_idle_requires_gate_empty`：构造带 pending question 的 gate + 未锁 agent，断言 `is_idle() == false`；清空后 `== true`；持有 agent 锁时 `== false`

```rust
// src/agent/instances.rs 测试骨架
#[tokio::test]
async fn test_instance_idle_requires_gate_empty() {
    let inst = test_instance().await; // 辅助：Agent::new + SessionStore::open(tempdir)
    assert!(inst.is_idle().await);
    let gate = inst.agent.lock().await.approval_gate.clone();
    gate.register_question("q?", None, "cli", "main", 0).await;
    assert!(!inst.is_idle().await);          // gate 非空 → 非 idle
    let _g = inst.agent.lock().await;         // 模拟 in-flight 回合持锁
    assert!(!inst.is_idle().await);
}
```

- [ ] **Step 2: 跑测试确认编译失败**（`cargo test instances`，Expected: 模块不存在）
- [ ] **Step 3: 实现**

```rust
// src/agent/instances.rs
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use crate::agent::Agent;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InstanceKind { Main, Task }

/// 实例 = 任务线的运行时形态（ADR-0032）。
/// dormant 化 = 从 map 移除 handle（Agent 被 drop，session 线留 sqlite）。
pub struct InstanceHandle {
    pub name: String,
    pub kind: InstanceKind,
    pub agent: Arc<Mutex<Agent>>,
    pub session_id: i64,
    pub bound_path: Option<PathBuf>,
}

impl InstanceHandle {
    /// idle = 无 in-flight 回合（Mutex 未被持有）且无未决 ask_user/审批。
    /// 频道内消息串行处理，无并发竞态。
    pub async fn is_idle(&self) -> bool {
        let Ok(agent) = self.agent.try_lock() else { return false };
        agent.approval_gate.is_empty().await
    }
}

/// 宿主级实例注册表。attachments：IM/CLI 频道名 → 实例名（WebUI 不走此表，
/// 每面板自选实例）。
pub struct InstanceRegistry {
    instances: RwLock<HashMap<String, Arc<InstanceHandle>>>,
    attachments: RwLock<HashMap<String, String>>,
}

impl InstanceRegistry {
    pub fn new() -> Self { Self { instances: RwLock::new(HashMap::new()), attachments: RwLock::new(HashMap::new()) } }

    pub async fn register(&self, h: Arc<InstanceHandle>) {
        self.instances.write().await.insert(h.name.clone(), h);
    }
    pub async fn get(&self, name: &str) -> Option<Arc<InstanceHandle>> {
        self.instances.read().await.get(name).cloned()
    }
    pub async fn list(&self) -> Vec<Arc<InstanceHandle>> {
        let mut v: Vec<_> = self.instances.read().await.values().cloned().collect();
        v.sort_by_key(|h| h.kind != InstanceKind::Main); // main 置顶
        v
    }
    /// dormant 化（非 main、且 idle）：移除 handle，Agent 随之 drop。
    pub async fn dormantize(&self, name: &str) -> Result<(), String> {
        let h = self.get(name).await.ok_or("no such instance")?;
        if h.kind == InstanceKind::Main { return Err("main cannot dormantize".into()); }
        if !h.is_idle().await { return Err("instance busy".into()); }
        self.instances.write().await.remove(name);
        Ok(())
    }
    /// 频道附着：默认 main；/session 切线时更新。
    pub async fn attach(&self, channel: &str, name: &str) -> Result<(), String> {
        if self.get(name).await.is_none() { return Err(format!("unknown instance {name}")); }
        self.attachments.write().await.insert(channel.into(), name.into());
        Ok(())
    }
    pub async fn attached(&self, channel: &str) -> Arc<InstanceHandle> {
        let name = self.attachments.read().await.get(channel).cloned().unwrap_or_else(|| "main".into());
        self.get(&name).await.expect("attachment target missing")
    }
}
```

- [ ] **Step 4: `ApprovalGate` 补 `is_empty`**（`src/agent/approval.rs`，紧邻 `list()`）：

```rust
pub async fn is_empty(&self) -> bool {
    self.inner.lock().await.is_empty()
}
```

- [ ] **Step 5: 跑测试**（`cargo test instances`，Expected: PASS）
- [ ] **Step 6: Commit**: `feat: instance registry with idle check and channel attachment`

## Task 2: 实例派生——从任务线 spawn

**Files:**
- Modify: `src/agent/instances.rs`（spawn 函数）
- Modify: `src/agent/mod.rs`（`fork_for_isolated` 抽参：pin 目标从恒 home 改为调用方指定）
- Test: `src/agent/instances.rs` 内

- [ ] **Step 1: 写失败测试** `test_spawn_task_instance_backfills_and_binds`：sqlite 建任务线（`create_task_session` + 写两条消息）→ `spawn_task_instance` → 断言 session_id、bound_path、context 尾部回灌非空、`instance_name == Some(name)`
- [ ] **Step 2: 跑测试确认失败**
- [ ] **Step 3: 实现**

```rust
// src/agent/instances.rs 续
/// 从 main agent 派生任务实例：fork 原语（共享 provider/store）+
/// 指定 session 线 + 回灌（复用 slash.rs::backfill_context 通路）+ bound_path。
pub async fn spawn_task_instance(
    main: &Agent,
    registry: &InstanceRegistry,
    name: &str,
) -> anyhow::Result<Arc<InstanceHandle>> {
    let session_id = crate::memory::find_or_create_task_session(main.store(), name).await?;
    let mut fork = crate::agent::fork_for_isolated(main, /*pin_root*/ main.workspace_root()).await?;
    fork.session_id = session_id;
    fork.instance_name = Some(name.to_string());
    // bound_path: sqlite 读；workspace_root 切到 bound_path（对齐 WebUI 切线语义）
    let bound = crate::memory::bound_path_of(main.store(), session_id).await?;
    if let Some(p) = &bound { *fork.workspace_root.write().await = p.clone(); }
    crate::commands::slash::backfill_context(&mut fork).await?;
    fork.refresh_task_state().await;
    let h = Arc::new(InstanceHandle { name: name.into(), kind: InstanceKind::Task,
        agent: Arc::new(Mutex::new(fork)), session_id, bound_path: bound });
    registry.register(h.clone()).await;
    Ok(h)
}
```

注意：`fork_for_isolated` 现签名 pin 恒家目录（ADR-0031 修订 5）。本任务把 pin 目标参数化（`pin_root: PathBuf`），**cron 调用点传家目录（行为不变），任务实例派生传 bound_path，实例内 delegate 派生传实例当前 root**。`Agent` 增加 `pub instance_name: Option<String>`（None=main）。

- [ ] **Step 4: 跑测试**（Expected: PASS；`cargo test fork` 回归既有 cron/delegate 测试不变绿转红）
- [ ] **Step 5: Commit**: `feat: spawn task instances from session lines via fork primitive`

## Task 3: `/session` 改造——idle 检查 + 换绑附着

**Files:**
- Modify: `src/commands/slash.rs:229-317`（`/session` 处理）与 `862-966`（switch 函数群）
- Modify: `src/channels/cli.rs`、各 IM channel（消息入口从 `registry.main` 改 `instance_registry.attached(channel)`）
- Test: `src/commands/slash.rs` 内既有 `test_task_switch_backfill_and_archive` 回归 + 新增两条

- [ ] **Step 1: 写失败测试**
  - `test_session_switch_rejected_when_not_idle`：附着实例 gate 挂 pending question → `/session other` 返回拒绝提示、附着不变
  - `test_session_switch_rebinds_attachment`：idle → `/session foo` 后 `attached("cli").name == "foo"`，旧实例从注册表 dormant（除非仍被 WebUI 订阅——v1 计数订阅者：`InstanceHandle.subscribers: AtomicUsize`，WebUI 面板 open +1 / close -1，dormantize 条件加 `subscribers == 0`）
- [ ] **Step 2: 跑测试确认失败**
- [ ] **Step 3: 实现**——`/session <名>` 流程改为：
  1. `attached(channel).is_idle()` → false 则拒绝：`[busy: 任务X 正在执行/等待回答/等待审批；处理完或 /stop、/cancel 后再切]`
  2. 目标线已有实例 → 换绑；无 → `spawn_task_instance` 后换绑
  3. 旧实例 `subscribers==0` 则 dormantize（main 永不）
  4. `/session close` = idle 检查（同 1）→ `archive_session` → dormantize → 回 main
  - `/sessions` 列表改列实例（活跃实例 + dormant 线，标注态）
  - **CLI/IM 消息入口**：`let inst = instance_registry.attached(self.name()).await; run_turn(inst.agent.clone(), ...)`——`run_turn` 签名不变
- [ ] **Step 4: 跑测试**（Expected: PASS）
- [ ] **Step 5: Commit**: `feat: session switch as instance rebind with idle guard`

## Task 4: 宿主化 serve_cmd + 重启语义

**Files:**
- Modify: `src/commands/mod.rs`（`serve_cmd`，锚点 L444-685）
- Test: 既有 serve/channel 集成测试回归

- [ ] **Step 1**: `serve_cmd` 构建 `Arc<InstanceRegistry>`：启动时注册 main 实例（`registry.main` 包成 `InstanceHandle{kind: Main, name: "main"}`），注入各 channel（channel 已收 `AgentRegistry`，追加传 `InstanceRegistry`——`Channel::run` 签名加一个参数或合并进既有 registry 结构，选**后者**：`AgentRegistry` 增加 `pub instances: InstanceRegistry` 字段，channel 签名零改动）
- [ ] **Step 2**: 重启只恢复 main（现状 `latest_session` 已限 `kind='main'`，天然满足）；任务实例不自动拉起（ADR 定案）
- [ ] **Step 3**: `cargo test` 全量回归 + `cargo clippy --all-targets -- -D warnings`
- [ ] **Step 4: Commit**: `refactor: serve_cmd as instance host, registry hosts instance table`

## Task 5: memory 分层

**Files:**
- Modify: `src/tools/memory.rs`（memory_write 路由）
- Modify: `src/memory/trim.rs` 或 system prompt 拼装点（grep `init_system_meta` 定位缓存；`trim_memory_to_budget` 所在文件）
- Test: `src/tools/memory.rs` 内新增

- [ ] **Step 1: 写失败测试** `test_memory_write_routes_to_instance_file`：`instance_name=Some("foo")` 的 agent 上 `memory_write` → 断言写 `workspace/instances/foo/MEMORY.md` 而主 MEMORY 不变；main agent 写主 MEMORY 不变
- [ ] **Step 2: 跑测试确认失败**
- [ ] **Step 3: 实现**
  - `memory_write` 目标：`agent.instance_name` → `Some(n)` 写 `<home>/workspace/instances/<n>/MEMORY.md`（目录不存在则建），`None` 写主 MEMORY（现状）
  - system prompt：主 MEMORY 经 `trim_memory_to_budget` 后，若 `instance_name` 非 None 追加实例 MEMORY 段（同预算参数独立 trim，段头 `## Task Memory (instance: <name>)`）
  - **缓存 per-instance 化**：`init_system_meta` 的缓存键从 agent alias 扩为 `(alias, instance_name)`（或 SystemMeta 存进 `InstanceHandle`）；启动时只建 main 的，实例 spawn 时按需建
  - `/memory-compact` 操作对象跟随当前 agent 的写入目标
- [ ] **Step 4: 跑测试 + 既有 memory/trim 测试回归**
- [ ] **Step 5: Commit**: `feat: layered memory - shared main memory plus per-instance memory`

## Task 6: 人格主权守卫

**Files:**
- Modify: `src/tools/file` 相关（写路径校验处）、`src/agent/bootstrap.rs`
- Test: 新增

- [ ] **Step 1: 写失败测试** `test_task_instance_cannot_write_home`：`instance_name=Some("foo")` 的 agent 调 `file_write` 目标 `<home>/workspace/SOUL.md`（及 USER.md、MEMORY.md）→ 拒绝；读 SOUL.md → 允许；main 写 → 允许
- [ ] **Step 2: 跑测试确认失败**
- [ ] **Step 3: 实现**——file_write/file_edit 路径校验处加一条：目标 canonical 路径位于 agent 家目录 workspace 下 **且** `instance_name.is_some()` → 拒绝（`[home workspace is read-only for task instances]`）。实例只读共享 SOUL/USER/主 MEMORY；自己的实例目录（memory 工具）与 bound_path 作用域不受影响。bootstrap 指令本就只注册在 main agent（fork 不复制 bootstrap 标志，确认 `fork_for_isolated` 分支不携带 `Context.bootstrap`）
- [ ] **Step 4: 跑测试**
- [ ] **Step 5: Commit**: `feat: home workspace read-only guard for task instances`

## Task 7: WebUI 多实例并行

**Files:**
- Modify: `src/channels/web.rs`（`/api/instances` + WS 实例路由）
- Modify: `src/web/static/app.js`、`index.html`（instance rail + 并行面板）
- Test: `src/channels/web.rs` 集成测试

- [ ] **Step 1: 后端 API**——`GET /api/instances` 返回 `[{name, kind, bound_path, idle, session_id}]`（含 dormant 线，从 `list_open_tasks` ∪ 活跃实例合成）；WS 消息体增加 `instance` 字段（缺省 main）：handler `get(instance)` → 无则 `spawn_task_instance` → `run_turn(inst.agent, ...)`；面板 open/close 维护 `subscribers` 计数
- [ ] **Step 2: 写失败测试** `test_web_two_instances_parallel_turns`：两个 WS 连接分别选 `main` 与 `foo` 同时发消息，两回合并发执行（断言二者互不阻塞——用阻塞型 fake provider 验证交错）
- [ ] **Step 3: 实现 WS 路由与事件打标**（TurnEvent 流按实例名标记回对应面板）；斜杠命令经 WS 走对应实例的 slash 处理（`/session` 在 WebUI 语义 = 面板切换，不走频道附着表）
- [ ] **Step 4: 前端**——session rail 升级 instance rail（main 置顶 + 活跃实例 + `[dormant]` 线点击即 spawn）；多面板：每实例一个聊天面板（复用现有 chat 组件，数据按实例名分桶），同屏平铺可折叠
- [ ] **Step 5: `cargo test` + 手动验收**（两浏览器面板并行对话互不阻塞；IM 频道切线拒绝提示正确）
- [ ] **Step 6: Commit**: `feat: webui parallel instance panels and instance rail`

## Task 8: 委派与 cron 作用域收口

**Files:**
- Modify: `src/tools/delegate.rs`（子 agent fork 的 pin_root）
- Test: 既有 delegate 测试回归 + 新增 1 条

- [ ] **Step 1: 写失败测试** `test_delegate_from_task_instance_pins_instance_root`：`instance_name=Some("foo")`、bound_path=P 的实例委派 → 子 agent `workspace_root == P`；main 委派 → pin 家目录（现状不变）
- [ ] **Step 2: 实现**——delegate 的 fork 调用点 pin_root = 调用方实例的当前 root（main=家目录；任务实例=bound_path）。cron 调用点传家目录（现状）
- [ ] **Step 3: 跑测试 + 回归**
- [ ] **Step 4: Commit**: `feat: delegate and cron fork pin instance scope`

## Task 9: 文档收口

**Files:** Modify `AGENTS.md`、`docs/adr/0032-instance-architecture.md`、`CHANGELOG.md`

- [ ] AGENTS.md：架构节改写（Channel/实例/宿主模型）、斜杠命令 `/session` 语义更新、SessionStore 简化注回 ADR-0032（v1 共享单连接，独立连接+busy_timeout 留给真进程载体）
- [ ] ADR-0032 状态改「已实现」，追加实现记录节（含两条计划级偏离）
- [ ] CHANGELOG 按发版惯例补条目
- [ ] Commit: `docs: instance architecture implementation notes`

---

## 验收清单（对 ADR-0032 逐条）

| ADR 决策 | 验收 | 任务 |
|---|---|---|
| 实例=任务线运行时形态 | spawn 自既有 task 线、bound_path 沿用 | T2 |
| main 也是实例；serve=宿主 | registry.main 包装为 Main 实例；重启只回 main | T4 |
| 频道 per-channel 单活跃 | 附着表 + 切线换绑 | T3 |
| WebUI 唯一并行入口 | 双面板并发回合测试 | T7 |
| 仅 idle 可切 | try_lock + gate 空判定 + 拒绝提示 | T1/T3 |
| 三态生命周期 | dormantize/close；subscribers 计数防误杀 WebUI 订阅 | T1/T3 |
| memory 分层读写分工 | 路由测试 + prompt 拼接 + per-instance 缓存 | T5 |
| SOUL/USER 主权归 main | 家目录只读守卫 | T6 |
| delegate worker 不动 + 作用域 | pin root 测试 | T8 |
| `/cancel` 补记 | AGENTS.md | T0 |

## 执行顺序与依赖

T0 独立可先行。T1 → T2 → T3 → T4 为主线（T3 依赖 T1/T2，T4 收口宿主）；T5/T6/T8 相互独立，可在 T4 后并行；T7 依赖 T3（附着/订阅计数）与 T5（实例 prompt）。每个 Task 结束跑 `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`（CI 同门）。
