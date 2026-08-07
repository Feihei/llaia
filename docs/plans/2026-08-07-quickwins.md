# P3+ 快赢项实现计划 — /provider 命令 + model fallback + WebUI 重启

- 日期：2026-08-07
- 状态：✅ 已完成（三项快赢均已实现并通过质量门：fmt/clippy(-D warnings)/全量测试）
- 依据：[specs/2026-08-07-provider-channel-expansion.md](../specs/2026-08-07-provider-channel-expansion.md) §三 第一批

## Task 1：`/provider` 斜杠命令（运行时切换模型）

**目标**：`/provider` 列出所有可用模型（当前标 `*`），`/provider <序号>` 或 `/provider <id>.<alias>` 运行时切换，不写 config.toml。

**改动**：

1. `src/provider/mod.rs`：`Provider` trait 加 `fn label(&self) -> String`，默认返回 `"unknown"`
2. `src/provider/openai_compat.rs`：override 返回 `self.model.clone()`
3. `src/agent/mod.rs`：`Agent` 加 `pub config: Arc<Config>` 字段（new 时 `Arc::new(config.clone())`），供 slash 命令枚举/构建 provider
4. `src/commands/slash.rs`：加 `/provider` 分支：
   - 无参：遍历 `config.provider`（id 排序，alias 排序）flatten 成编号列表，当前 provider 的 `label()` 匹配的行标 `*`
   - 带参：数字序号（1-based）或 `id.alias` → `Config::parse_model_ref` → 构建 `OpenAiCompatibleProvider` → `agent.reload_provider(Some(...))`
   - 构建失败/未找到 → 返回错误提示，不动现有 provider
   - compact_provider 不随动（仍按 runtime.compact_model）
5. `/help` 输出补 `/provider`

**验证**：单测覆盖列表渲染与切换逻辑（用 MockProvider label）；`cargo run -- chat` 手动验证切换后 `/stats` 不变、对话生效。

## Task 2：model fallback（provider 链降级）

**目标**：主模型请求失败时自动降级到备用模型，全程用户无感（仅日志提示）。

**改动**：

1. `src/config.rs`：`AgentConfig` 加 `#[serde(default)] pub fallback: Vec<String>`（model ref 列表，如 `["local.small", "cloud.big"]`）
2. 新文件 `src/provider/fallback.rs`：`FallbackProvider { chain: Vec<Arc<dyn Provider>> }` 实现 `Provider`：
   - `chat`：依次尝试，成功即返回；全失败返回最后一个错误（warn 日志记录每次降级）
   - `chat_stream`：依次尝试建流，第一个成功即返回该流（流开始后中断不降级——复杂度过高，超出快赢范围）
   - `native_tool_calling` / `label`：取 chain 首个
   - `detect_context_size`：取第一个 `Some`（兜底最小值更保守——取 min）
3. `src/web/mod.rs` `build_provider_from_config`：构建主 provider 后，若 `[agent.main].fallback` 非空，依次构建并 wrap 成 `FallbackProvider`（构建失败的 fallback 项跳过 + warn）
4. `src/channels/cli.rs` 构建 provider 处同步走该逻辑（提取公共构建函数到 `provider/mod.rs`：`build_chain(config) -> Result<Option<Arc<dyn Provider>>>`）

**验证**：单测 MockProvider 链（第一个 Err → 第二个成功）；fallback 配置序列化/反序列化测试。

## Task 3：WebUI 重启按钮

**目标**：Config 面板加 Restart 按钮，点击后 serve 进程自重启，浏览器刷新后自动恢复连接。

**改动**：

1. `src/web/mod.rs`：
   - `AppState` 加 `config_dir: PathBuf`
   - 新端点 `POST /api/restart`：
     - 取 `std::env::current_exe()`，spawn 延迟启动的新进程：
       - Windows：`cmd /C "ping -n 2 127.0.0.1 >nul & <exe> --config-dir <dir> serve"`
       - Unix：`sh -c "sleep 1 && exec <exe> --config-dir <dir> serve"`
     - 返回 JSON `{"restarting": true}` 后延迟 300ms `std::process::exit(0)`（确保响应送达）
   - pid.rs 只警告不阻止，新进程可正常接管
2. `src/web/static/index.html` + `app.js`：About section 或侧栏底部加 Restart 按钮（confirm 确认），点击后显示"重启中…"并轮询 `/api/status` 直到恢复
3. 前端恢复逻辑：轮询失败（旧进程退出）→ 继续轮询直到成功 → reload 页面

**验证**：`cargo test`（restart 端点用 mock 难测，主要靠手动）；手动验证 Windows 下重启流程。

## 质量门

- `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test`
- 回填 plan.md 勾选 + 本文档状态
