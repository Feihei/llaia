# ADR-0017：时区由 config 注入 + 运行时事实「状态栏」注入

- 状态：已决议（2026-08-10）
- 关联：ADR-0016（做梦，依赖本项的时区与空闲门）、plan.md P4-a
- 参考：《深入理解 AI Agent》李博杰 v1.2 §2.6.3「Agent 状态栏」、§2.6.5「物理时间感知」

## 背景

P1 的"时间感知"实际是真空白：system prompt 仅在进程启动拼一次（`channels/cli.rs:441`），
USER.md 里的「时区：Asia/Shanghai」只是静态文本，模型不知道"现在几点、星期几"。
`chrono::Local::now()` 散落 6 处全依赖宿主机 TZ，Docker(UTC) 下记忆日期、审计、cron 触发整体偏 8 小时。

更关键的工程事实：`Agent` 持有 `config: Arc<Config>` **启动快照**（`agent/mod.rs:79,123`）,
`hot_reload_providers`（`web/mod.rs:391`）只换 provider、不更新该快照。因此任何"config 注入"若不通 live config 通道，WebUI 改了也白改。

## 决策

### 1. Config schema
`RuntimeConfig` 新增 `timezone: Option<String>`（IANA 名）。
- `None`（默认）= 跟随系统本地时区，**零配置行为与今完全一致、无回归**。
- `Some("Asia/Shanghai")` = 使用该时区。
- `Config::load` 校验：非法 IANA 名 warn + 置 `None`（写法对齐现有 `compact_model` 校验）。
- `Cargo.toml` 加 `chrono-tz`（兼容现有 `chrono 0.4`）。

### 2. 统一时间源 `src/time.rs`（新建）
- `Now { naive: NaiveDateTime, zone_label: String }` + `now(tz) -> Now`：set 走 `chrono_tz::Tz` 解析后 `Utc::now().with_timezone`；unset 走 `Local::now()`。用 `NaiveDateTime` 统一两种分支的类型，便于格式化/星期。
- `unix_now() -> i64`：`Utc::now().timestamp()`，**时区无关**，供做梦空闲门算 elapsed 秒数。
- `resolve_tz(tz) -> Option<Tz>`：`OnceLock<Mutex<HashMap>>` 缓存解析，非法/未设 → `None`。
- `status_bar(tz) -> String`：§2.6.3「Agent 状态栏」文本 = 当前本地时间(含星期) + 时区标签 + §2.6.5 **操作提示**（读数 + 简短用法，而非裸时间戳）。

### 3. live config 通道（热更新核心）
- `Agent` 新增字段 `live_config: Arc<RwLock<Config>>`，`new()` 接收。
- `build_agent`：WebUI 下传 `state.config.clone()`；CLI 下设 `Arc::new(RwLock::new(config.clone()))`（永不更新，与 CLI 无热加载现状一致）。
- `hot_reload_providers`（`web/mod.rs:391`）末尾补：
  `*state.registry.main.lock().await.live_config.write().await = new_config.clone();`
  下次 `to_messages` 即用新时区；进行中的 turn 持有旧 snapshot，不受影响。
- 保留原 `config: Arc<Config>` 快照给 `/provider` 等 provider 引用读取（slash.rs 无需改动）。

### 4. 状态栏注入点
- `Context::to_messages(&self) -> Vec<ChatMessage>`（`agent/context.rs:24`）改为 `to_messages(&self, tz: &Option<String>)`，末尾 `msgs.push(ChatMessage::user(status_bar(tz)))`。
- 调用方 `agent/mod.rs:293` 传 `&self.live_config.read().await.runtime.timezone`；`context.rs` 两处测试改 `to_messages(&None)`。
- **不**写入 `context.history`：状态栏每轮 `to_messages` 现算、仅挂尾 → 整段 system（SOUL/USER/MEMORY）作为稳定前缀被 KV Cache 命中（§2.6.3）。QQ/Telegram/Web 全经 `handle_message_streaming → to_messages`，自动覆盖。

### 5. 收敛零散 `Local::now()`
- 改走 tz（用户可见日期）：`tools/memory.rs:61`、`memory/markdown.rs:75`、`commands/mod.rs:682` → `time::now(tz).naive.format("%Y-%m-%d")`。memory 工具 v1 先传 `&None`（系统本地，无回归），tz 透传进工具列为后续小改进。
- 维持 UTC（运维日志，不动）：`audit.rs:41`、`memory/sqlite.rs:106/128/156`、`cron/runner.rs:54`。

### 6. 配套清理
`USER_TEMPLATE`（`memory/markdown.rs:46`）去「时区」行；旧 USER.md 不强制迁移；`llaia doctor` 加时区解析检查；WebUI 面板暴露 `[runtime].timezone`。

## 不做
- 不做独立 idle 检测器（做梦项已定 cron + 空闲门，见 ADR-0016）。
- 不为 memory 工具紧急透传 tz（v1 容忍，状态栏已是正确的模型时间感知）。
- 不改 audit/sqlite 的 UTC 时间戳（运维日志与用户时间解耦）。

## 影响
- 新增依赖 `chrono-tz`；新增 `src/time.rs`。
- `to_messages` 签名变更（1 个真实调用方 + 2 个测试）。
- `Agent` 新增 `live_config` 字段，`build_agent`/`new` 签名变更。
- 无破坏性：默认 `timezone = None` 时行为与现完全一致。

## 验证
1. 改 timezone 后不重启，下一轮状态栏即时反映新时区。
2. 多轮对话 system 前缀 token 稳定（cache 命中）。
3. `resolve_tz("Asia/Shanghai")`→`Some`、`resolve_tz("Mars/Phobos")`→`None`。
4. Docker(UTC) 下记忆落盘日期与配置时区一致，而非宿主机 UTC。
