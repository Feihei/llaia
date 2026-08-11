# ADR-0018：`/api/shutdown` 优雅退出 serve

- 状态：已决议（2026-08-10）
- 关联：plan.md P4-a「进程生命周期与重启机制」、ADR-0017（同属 P4-a）
- 参考：zeroclaw `.ref/zeroclaw` respawn 层（仅借鉴「脱管进程」痛点，不借鉴其 daemon 方案）

## 背景

WebUI 已有 `/api/restart`（`web/mod.rs:466`）：spawn 替代进程后 `exit(0)`。
但**没有** `/api/shutdown`。`serve_cmd`（`commands/mod.rs:463`）只靠
`tokio::select! { ctrl_c / 所有 channel task 结束 }` 退出，且两条退出路径都**无显式清理**：

- cron 调度器不调用 `CronScheduler::shutdown()`（`cron/mod.rs:299`），直接随进程退出；
- spawn 出的 channel task（QQ/Web/…）被丢弃，不 abort/await。

这造成一个真实痛点：终端启动的用户点 restart 后，替代进程**故意脱离终端**
（zeroclaw respawn 思路），旧进程 `exit(0)` 后用户失去 Ctrl+C 控制，只能任务管理器杀。
优雅的「停止」能直接消解这个痛点——不再需要 restart 来"甩掉"失控进程。

## 决策

### 1. 共享 shutdown 信号（从 handler 触发 serve_cmd 退出）
- `WebChannel` 新增字段 `pub shutdown_signal: Arc<Notify>`；`new()` 内 `Arc::new(Notify::new())`。
- `build_router`（`web.rs:361`）将其 clone 进 `AppState`（新增同名字段）。
- `serve_cmd` 在 spawn 前 `let shutdown_signal = web.shutdown_signal.clone();`，持有同一 Arc 用于 `select!`。

### 2. 新 handler `shutdown_service`（`web/mod.rs`，仿 `restart_service`）
- `authorize` 通过后 `state.shutdown_signal.notify_one()`。
- **与 restart 不同，容器内允许**（stop 是用户主动想要的；停止 PID 1 = 停容器，合理）。
- `build_system_routes()` 注册 `.route("/api/shutdown", post(shutdown_service))`。

### 3. `serve_cmd` 收尾（`commands/mod.rs:463`）
`select!` 增加 `_ = shutdown_signal.notified()` 分支，与 ctrl_c 共用清理逻辑：
```rust
if let Some(cron) = &_cron { let _ = cron.shutdown().await; }  // cron/mod.rs:299
for t in &tasks { t.abort(); }
for t in tasks { let _ = t.await; }
println!("\n{}", crate::banner::GOODBYE);
```
`tasks` 不再被 `for t in tasks { t.await }` 消费，改为 `for t in &mut tasks { … }` 借用，
以便 shutdown 分支能 `abort` 它们。

### 4. 响应时序
handler 先 `return Json({shutting_down:true})`，再 `tokio::spawn` 延迟 ~100ms 后 `notify_one()`，
确保浏览器收到响应、WebUI 切「已停止」态，再触发进程收尾（避免 web task 被 abort 前响应未刷出）。

### 5. WebUI 前端
`index.html:377` 的 `Restart Service` 旁加 `Stop Service` 按钮；
`app.js:499 restartService()` 旁加 `shutdownService()`（POST `/api/shutdown`，成功显示「Service stopped」而非轮询重启）。

## 不做
- 不做同 PID 原地 reload / spawn-after-teardown（P4-f，无强痛点，等本项上线后复评）。
- 不把 MCP 注册表单独 shutdown：transport 已 `kill_on_drop(true)`，进程收尾时 Arc 释放即杀子进程。

## 影响
- `WebChannel` / `AppState` 各增一个 `Arc<Notify>` 字段。
- 新增 `shutdown_service` handler + 路由。
- `serve_cmd` 的 `select!` 增加分支、清理逻辑抽共用。
- 前端 `index.html` + `app.js` 各加一个按钮/方法。
- 无破坏性：CLI chat 模式不受影响（shutdown 仅 serve 挂载）。

## 验证
1. 点停止按钮 → 进程 ~1s 内干净退出，终端打印 GOODBYE，无残留子进程（含 MCP child）。
2. 停止后 cron 任务不再触发（shutdown 已调）。
3. Ctrl+C 路径行为不变（仍优雅退出）。
4. 容器内点停止能停掉容器；restart 仍拒绝（行为保持）。
