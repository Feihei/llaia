# P9 Phase 1（workspace 快照基建）+ Phase 2 首刀（env 密钥剔除）实现计划

- 日期：2026-10-05
- 关联：[ADR-0033](../adr/0033-security-model.md)（安全模型）、[plan.md](../plan.md) P9
- 范围：P9 Phase 1 全量 + Phase 2 的 env 剔除项（ADR 明示「与 Phase 1 无依赖，可提前单独落地」）。Phase 2 其余项（基础设施文件只读守卫 / web_fetch 出站闸门 / terminal 网络命令标记 / 拒绝话术）另批实施。

## Phase 1 · workspace 快照基建（L1 可恢复性）

### 目标与不做项

ADR-0033 L1：A0（SOUL/USER/MEMORY 及实例私有 MEMORY）/A2（sessions.db、uploads/）**写前留底**，为全部闸门的放行宽松度供底气；快照存储本身对 agent 只读（防「先改本体再改快照」洗白）。

**不做**（留档）：git-backed 方案（外部 git 二进制依赖，违背轻量单 exe 定位，缺 git 时静默失效更糟）；快照的 WebUI/CLI 恢复界面（v1 恢复 = 从 `snapshots/` 手工拷回，目录即文档）；sub-agent 家目录的写前挂钩（其 SOUL/USER 是主 agent 的衍生副本，sweep 兜底即可）；快照加密/异地（单用户本地场景）。

### 方案：时间戳镜像目录 + hash 去重（纯 Rust，零新依赖）

```
<config_dir>/snapshots/            # 在 agent 家目录之外（config_dir 顶层，与 logs/ 同级）
  20261005-103000/MEMORY.md        # <本地时刻>/<key>，key 用相对家目录的 / 分隔路径
  20261005-103000/sessions.db
  20261005-104500/SOUL.md
  .state.json                      # {"files":{key:hash}, "last_db":ts, "last_gc":ts}
```

- `src/snapshot.rs::SnapshotStore`：`snapshot_file(abs, key, reason)`（sha1 变更检测去重——已有 sha1 crate，防篡改不靠它）+ `sweep(items)` + `gc()`（删超保留窗口的顶层时刻目录）+ `latest(key)`（供测试与未来恢复 UI）。
- 内容未变（hash 命中 `.state.json`）→ 不落新拷贝，写前快照与 sweep 的去重共享同一状态。快照失败 log warn **不阻断主操作**（与 reminder「生成失败静默降级」同口径；快照与目标同盘，快照失败时写入大概率也失败）。
- 单文件体积上限 200MB（超限跳过 + warn；uploads 的病态大文件防御）。
- **sessions.db 不做裸文件拷贝**（WAL 活拷贝可能不一致）：`SessionStore::snapshot_into` 用 `VACUUM INTO`（bundled SQLite 3.50 ✓）产出压缩一致副本；sweep 里**至多每 24h 一次**（消息每次落库 mtime 都变，按 sweep 频率拷贝会爆盘），独立于其他文件的 10 分钟节奏。

### 触发点

1. **写前挂钩（A0，确定性写路径）**：
   - `MemoryWrite::execute`（main）与 `runner.rs` 实例路由 `write_memory_entry`（经 `ApprovalContext.snapshot` 传入）；
   - `FileWrite` / `FileEdit` 目标落在**家目录**且文件名为 SOUL.md / USER.md / MEMORY.md 时（first-run bootstrap 与用户直接改人格都走这里；任务实例被 T6 守卫先行拒绝，到不了工具层）；
   - `/memory-compact`（经 `Agent.snapshot`）——既有 `workspace/backups/` 备份在家目录内、agent 可触碰，不构成 agent 之外的留底。
2. **定时 sweep（兜底 + A2）**：`build_single_agent`（is_main）spawn 后台任务，10 分钟一轮：A0 固定文件 + `instances/*/MEMORY.md` + `subagent/*/MEMORY.md` + `uploads/` 递归；sessions.db 按上述 24h 节奏。任务持 `Weak<SnapshotStore>` / `Weak<SessionStore>`，reload 重建后随旧 registry 一起消亡，不泄漏。
3. **GC**：启动时 + 每日，删保留窗口（`[runtime].snapshot_retention_days`，默认 14）外的时刻目录。

### 快照存储对 agent 只读

- 位置在 `<config_dir>/snapshots/`，天然在家目录 workspace 之外：file/terminal 默认界外，default 档写入需审批。
- **硬拒兜底**：`FileWrite` / `FileEdit` / `Terminal` 持 `snapshot_root`，目标（或命令路径 token 经 scope 校验解析后）落入快照目录即报错 `[snapshot-guard]`，**execute 与 execute_approved 同拒**（delegate 通道自动放行 + `execute_approved` 跳过范围检查的既有绕行，到这道闸为止；terminal 的 `bash -s` stdin 脚本内路径提取不可靠属 L5 固有局限，诚实留档，真正的硬边界归 Phase 3 T2）。
- 读不额外放行：快照目录不进 file_read 作用域（保持界外语义）。

### 配置

`[runtime]` 两个新 key（`deny_unknown_fields` 下需显式声明，均有真实用例，非占位）：

- `snapshot_enabled`（默认 true）：总开关，关掉则不建 store、不 spawn sweeper（磁盘受限用户的 kill switch）。
- `snapshot_retention_days`（默认 14）：保留窗口；GC 删除早于 N 天的时刻目录。

## Phase 2 首刀 · env 变量密钥剔除（L3/A1）

现状：dotenvy 把 `.env` 灌进进程 env（`main.rs`），terminal 子进程继承完整环境——一条 `env` 即倒出全部 API key（ADR-0033 §2.1 出站面盘点 + §4 归位审计「子进程环境变量：缺位」）。

- `src/child_env.rs`：`scrub_command_env(cmd: &mut tokio::process::Command, keep: &[String])`——按 codex 规则剔除 key 名含 `KEY` / `SECRET` / `TOKEN`（大小写不敏感）的继承变量；`keep` 是配置显式提供给子进程的变量（MCP server 的 `env` 段），这些不被剔除。
- 挂点：`tools/terminal.rs::run_command` 三处 spawn（Git Bash / cmd / sh）+ `mcp/transport.rs::spawn_child`（`StdioTransport` 存 scrub 标志，重连 respawn 同样生效）。标志经 `McpRegistry::connect_all(configs, scrub)` 从 runtime config 传入（web reload 重建处同步）。
- 配置：`[runtime].scrub_child_env`（默认 true，学 codex）。误伤面：Windows 系统变量与 PATH 无命中；真有 server 需要 *KEY* 形态的变量，走 mcp.toml 的 server env 段显式给（在 keep 白名单内）。

## 任务清单

1. `src/snapshot.rs` + 单测（去重、GC、latest、体积上限）
2. `SessionStore::snapshot_into`（VACUUM INTO）+ 单测
3. `RuntimeConfig` 三 key + `CONFIG_TEMPLATE` 注释
4. cli.rs 装配：store 构建（is_main）、工具注入、`Agent.snapshot` 字段、sweeper spawn、`ApprovalContext.snapshot`
5. file.rs / memory.rs / runner.rs / slash.rs 挂钩 + snapshot-guard 硬拒
6. terminal.rs spawn 环境剔除 + snapshot-guard；child_env.rs + mcp/transport.rs
7. 文档回填：plan.md P9 勾选、ADR-0033 §5 勾选、AGENTS.md 注记、本文件交付注记

## 验收

- `cargo fmt --all --check` / `cargo clippy --all-targets -- -D warnings` / `cargo test` 全绿（沙箱内 mockito 端口限制项除外，与本次无关）。
- 手工验收路径：改 MEMORY → `snapshots/<ts>/MEMORY.md` 出现旧内容；同内容重复写不再新增拷贝；agent `file_write` 到 `snapshots/` 被 `[snapshot-guard]` 拒绝；terminal `env` 输出不含 `*KEY*`/`*SECRET*`/`*TOKEN*`。

## 交付注记（2026-10-05）

全量交付：Phase 1 + env 剔除。实现与计划的偏差：

- 时刻目录名带毫秒（`%Y%m%d-%H%M%S-%3f`）：同秒内两次不同内容的写前快照不互相覆盖。
- 快照挂钩统一走 `SnapshotCtx::snapshot_target`（persona 判定 + 失败 warn），file/memory/runner/compact 四处共用；`ApprovalContext.snapshot` 由 `Agent.snapshot` 携带，fork 副本继承。
- MCP env 剔除把 config `env` 段的键整体划入白名单（实现比计划的「逐一排除」更宽——server 显式声明的变量没有理由被自己剥掉）。
- `test_model_enabled_default_and_serialize_direction` 的子串断言与新 key `snapshot_enabled = true` 撞车，收紧为行首匹配（`\nenabled = true`）——断言语义（ModelEntry.enabled 启用态省略）不变。
- CONFIG_TEMPLATE 补三 key 注释；AGENTS.md「快照基建与 env 剔除」段同步。
- 未做（留后续）：快照恢复的 CLI/WebUI 入口（手工拷回已可用）；`docs/guide/` 用户侧文档（随 Phase 2 剩余项一起写，避免同批文档反复改）。

Phase 2 剩余项（基础设施文件只读守卫 / web_fetch 出站闸门 / terminal 网络命令风险标记 / 拒绝话术两原则）按 plan.md P9 顺序另行实施。
