# ADR-0012: llaia init 引导命令

- 状态：Proposed
- 日期：2026-08-04
- 关联：[ADR-0008](0008-config-schema-v1.1.md)、[docs/plan.md P3-b](../plan.md)

## 背景

当前 LLAIA 无初始化引导：用户首次运行 `llaia chat` 时，`load_config_or_init` 在 config.toml 不存在时直接用 `Config::default_for_workspace` 内存构造默认配置，但不写盘。这导致：

1. 用户不知道配置文件位置和格式，无法编辑
2. SOUL.md / USER.md / MEMORY.md 不会自动创建，agent 启动时 memory 模块才 ensure_template 创建空文件
3. `~/.llaia/` 目录结构不完整（缺 logs/ uploads/ workspaces/ 等）
4. 用户不知道下一步该做什么（如何加 provider、如何启用 QQ/Web）

`llaia config` 只打印配置，`llaia doctor` 只诊断，都不解决"从零到能跑"的引导问题。

## 决策

新增 `llaia init` 子命令：**纯模板生成 + 终端提示，不交互问答**。生成后引导用户运行 `llaia serve` 进 WebUI 完成详细配置。

### 1. 命令形态

```bash
llaia init [--config-dir <path>] [--force]
```

- `--config-dir`：指定配置根目录，默认 `~/.llaia`（注意：这是配置根，不是 agent workspace；agent workspace 固定在 `<config-dir>/workspace/` 下，不可单独指定）
- `--force`：覆盖已存在的文件（默认不覆盖，幂等）

### 2. 生成的目录结构

按 ADR-0011 的 agent workspace 模型生成：

```
~/.llaia/
  config.toml          # 默认模板（含注释说明）
  llaia.pid            # 运行时生成
  logs/                # 日志目录（tracing + audit.log）
  workspace/           # 主 agent 工作区（详见 ADR-0011）
    SOUL.md            # 人格模板
    USER.md            # 用户画像模板
    MEMORY.md          # 记忆模板（空条目）
    sessions.db        # 首次启动时由 sqlite 自动创建
    uploads/           # 用户上传媒体目录
    subagent/          # 子 agent 工作区集合（空目录，按需创建）
```

### 3. config.toml 默认模板

```toml
# LLAIA 配置文件
# 详细字段说明见 https://github.com/<owner>/llaia/blob/main/docs/adr/0008-config-schema-v1.1.md

[runtime]
context_threshold = 0.7
max_iterations = 10

[log]
level = "info"
dir = "~/.llaia/logs"

# Provider: 接入 LLM 服务
# 本地 Ollama 示例：
# [provider.default]
# type = "openai_compatible"
# base_url = "http://localhost:11434/v1"
# api_key = "${OLLAMA_API_KEY}"  # 或留空
#
# [provider.default.qwen]
# model = "qwen2.5:7b"
# native_tool_calling = false
# context_size = 32768

# 主 Agent（workspace / soul / user / memory 字段已废弃，自动推导到 ~/.llaia/workspace/）
# [agent.main]
# model = "default.qwen"        # 引用 provider.<id>.<model_alias>

[channels.cli]
enabled = true

[channels.qq]
enabled = false
app_id = "${QQ_APP_ID}"
app_secret = "${QQ_APP_SECRET}"
confirm_mode = "always"

[channels.web]
enabled = false
host = "127.0.0.1"
port = 8080
token = ""               # 留空则启动时随机生成并打印日志

[tools.terminal]
confirm = "none"
command_policy = "blacklist"
command_whitelist = []

[tools.tavily]
api_key = "${TAVILY_API_KEY}"
```

### 4. SOUL.md / USER.md / MEMORY.md 模板

模板文件生成在 `~/.llaia/workspace/` 下（按 ADR-0011 的 agent workspace 模型，agent 文件归 workspace 而非根目录）。

沿用 `memory::ensure_template` 现有的 `MEMORY_TEMPLATE`，新增 `SOUL_TEMPLATE` / `USER_TEMPLATE` 常量在 `src/memory/mod.rs`。

SOUL 模板示例：

```markdown
# 人格

你是 LLAIA，一个单用户私人助理。次要承担电脑操作与文件读写任务。

# 行为准则

- 优先用工具完成任务，不要只说不做
- 遇到不确定时主动询问用户
- 工具调用失败时报告错误并建议下一步

# 语气

简洁、直接、不啰嗦。
```

USER 模板示例：

```markdown
# 基本信息

- 姓名：
- 时区：Asia/Shanghai

# 身份绑定

- qq:
- email:
- web:

# 偏好

- 沟通语言：中文
```

### 5. 终端输出引导

```
✓ 已创建 ~/.llaia/ 目录结构
✓ 已生成 config.toml（含注释模板）
✓ 已生成 SOUL.md / USER.md / MEMORY.md 模板

下一步：
  1. 编辑 ~/.llaia/config.toml，取消 [provider.default] / [agent.main] 注释并填入你的 LLM 端点
     或运行 llaia serve 后在浏览器访问 http://127.0.0.1:8080 通过 WebUI 配置
  2. 启用 WebUI：把 [channels.web].enabled 改为 true
  3. 启动服务：llaia serve
  4. CLI 调试：llaia chat
```

## 不做

- **交互问答式向导**：不在 init 里问"用什么 provider？base_url 是？api_key 是？"。理由：交互体验在终端里受限（无 UI 提示、错误难纠正），WebUI 表单更适合详细配置。init 只负责"生成骨架 + 指路"
- **`llaia config --wizard` 子模式**：暂不拆分。如果未来用户反馈需要交互式向导，再单独加（参考 ADR-0006 的 CLI 子命令扩展）
- **自动启动 serve**：init 完成后不自动 `llaia serve`，让用户主动运行（避免端口冲突、权限意外）
- **init 预创建 sessions.db**：sqlite 文件由首次 chat/serve 时 sqlite 自动创建。init 不碰 sqlite，`llaia doctor` 对 sessions.db 不存在只 warn 不 error

## 无 provider 启动支持

init 生成的模板里 `[provider.default]` / `[agent.main]` 是注释状态，用户需手动取消注释或走 WebUI 配置。为支持"init → serve → WebUI 配置"流程，`llaia serve` 在无 provider 时也能启动：

### 1. serve 启动行为

- **WebUI**：完全正常工作。配置 API（`GET/PUT /api/config` / `/api/config/raw` / `/api/config/validate` / `/api/status`）不依赖 provider，用户可在 WebUI 填好 provider 配置并保存
- **聊天功能降级**：Agent 进入"降级模式"。WS/QQ 收到聊天消息时不调 provider，直接返回提示"未配置 provider，请先在配置面板添加 `[provider.default]` section"。CLI（若启用）同理
- **启动日志 warn**：`tracing::warn!("未配置 provider，聊天功能不可用，请在 WebUI 配置")`

### 2. llaia chat 行为

`llaia chat` 是纯 CLI 模式，无法配置 provider。无 provider 时**报错退出**，提示用户先运行 `llaia serve` 通过 WebUI 配置：

```
错误：未配置 provider，无法启动聊天。
请先运行 `llaia serve`，在浏览器访问 http://127.0.0.1:8080 配置 provider，
或手动编辑 ~/.llaia/config.toml 取消 [provider.default] 注释。
```

### 3. provider 热加载

用户在 WebUI 保存 provider 配置后，**无需重启 serve** 即可生效（参考 AstrBot 的热重载体验）：

- WebUI `PUT /api/config` 保存配置后，触发 `Agent::reload_provider()` 重建 provider 实例
- 正在进行的 turn 用旧 provider 完成后再切换（不中断进行中的对话）
- 切换后新 turn 用新 provider
- 热加载失败（如 base_url 不通）时回滚到旧 provider，WebUI 返回错误提示

### 4. `/provider` 斜杠命令（未来计划）

参考 AstrBot，加入 provider 管理斜杠命令：

- `/provider`：列出所有可用 provider 及模型，当前使用的标记 `*`
- `/provider <序号>`：切换到指定 provider/模型（运行时切换，不写 config.toml）
- `/provider <provider_id>.<model_alias>`：按全名切换

此命令归入 P3+ 阶段（不在 P3-b init 范围内），作为交互增强项。

### 5. doctor 检查项

`llaia doctor` 新增检查：

- provider 配置检查：无 `[provider.<id>]` section 时 warn（不 error，serve 可降级启动）
- sessions.db 检查：不存在时 warn（首次启动自动创建）

## 影响

### 代码变更

- `src/commands/mod.rs`：
  - 新增 `init_cmd(config_dir: &Path, force: bool) -> Result<()>`
  - `chat_cmd`：启动时检测无 provider，报错退出并提示引导
  - `serve_cmd`：启动时检测无 provider，warn 但继续启动
- `src/memory/mod.rs`：新增 `SOUL_TEMPLATE` / `USER_TEMPLATE` 常量，`ensure_template` 复用
- `src/main.rs`：注册 `init` 子命令（clap），参数 `--config-dir` / `--force`
- `src/agent/mod.rs`：
  - `Agent` 加 `has_provider: bool` 标志（或 provider 用 `Option<Arc<dyn Provider>>`）
  - 新增 `Agent::reload_provider(&self, new_config: &Config)` 方法：重建 provider 实例，用 `RwLock` 保护；正在进行的 turn 持有读锁用旧 provider，完成后新 turn 拿写锁切换
  - 降级模式：`handle_input_streaming` 检测无 provider，直接 sink `Error { message: "未配置 provider..." }` 不调 provider
- `src/web/mod.rs`：`PUT /api/config` 成功后触发 `Agent::reload_provider()`（通过 `AppState` 持有的 `Arc<Agent>` 或 `Arc<AgentRegistry>`）
- `src/commands/mod.rs` 的 `doctor_cmd`：加 provider 配置检查 + sessions.db 存在性检查
- 模板内容内嵌常量（避免 rust-embed 依赖扩展）

### 与现有命令的关系

- `llaia chat` / `llaia serve`：启动时仍调用 `load_config_or_init`，对未 init 的用户 fallback 到内存默认配置（向后兼容）。但首次运行时会提示"建议先运行 llaia init"
- `llaia config`：打印当前配置，不变
- `llaia doctor`：诊断，可加一项"检查是否已 init"提示

## 参考

- grilling 第四轮 Q3：用户选择"纯模板生成 + 提示"
- 用户补充："init 生成基础模板，然后 serve 拉起服务，引导用户进入 WebUI 进行设置"
- grilling 第七轮（P3-b 细化）：
  - Q1: provider/agent 注释保持，但 `llaia serve` 无 provider 能启动，WebUI 正常，聊天降级提示
  - Q2: `--workspace` 改名 `--config-dir`（语义清晰，与 agent workspace 区分）
  - Q3: init 不碰 sqlite，doctor 对 sessions.db 不存在只 warn
  - Q4: 模板内嵌常量
  - 新增：provider 热加载（WebUI 保存后无需重启，参考 AstrBot）；`/provider` 斜杠命令（查询/切换 provider）归入 P3+ 交互增强
