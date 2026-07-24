# ADR 0008: Config Schema v1.1

- 状态：Accepted
- 日期：2026-07-21
- 替代：ADR 0007 中关于 config schema 的部分约定

## 背景

P1 实现完成后，用户在使用过程中发现 config schema 存在以下问题：

1. **md 文件路径需重复书写**：`[agent.main]` 下必须显式写 `soul/user/memory` 三个绝对路径，即使用户已通过 `[workspace].dir` 指定了工作区。
2. **`context_threshold` 放错位置**：P1 把它放在 `[agent.main]` 下，但它本质是全局运行时参数，与具体 agent 无关。
3. **`max_iterations` 未暴露**：硬编码在 `agent/mod.rs` 的 `10`，不可配置。
4. **provider 与 model 耦合**：P1 的 `[provider.default]` 直接平铺 `model` 和 `native_tool_calling`，导致一个 provider 端点（base_url + api_key）只能配一个模型组合。实际场景中同一 Ollama/LMStudio 实例下挂多个模型很常见。
5. **`workspace` 是顶层字段**：但 workspace 在语义上属于 agent（P2 多 agent 各自独立 workspace），放顶层不合理。
6. **sessions.db 路径硬编码**：代码里写死 `workspace.join("sessions.db")`，schema 没体现。
7. **log.dir 默认值写死 `~/.laia/logs`**：与 workspace 脱节。

## 决策

### 1. workspace 移到 agent 下

```toml
[agent.main]
workspace = "e:/play/coding/laia/.test"
```

- 每个 agent 独立 workspace，互不干扰
- P2 子 agent 在各自 `[agent.<alias>].workspace` 下
- md 文件 (`SOUL.md`/`USER.md`/`MEMORY.md`) 默认从 workspace 推导，可显式覆盖
- `sessions.db` 硬编码为 `<workspace>/sessions.db`，不暴露 config

### 2. provider 与 model 分层

```toml
[provider.default]              # 端点连接信息
type = "openai_compatible"
base_url = "http://10.0.11.218:8080/v1"
api_key = ""

[provider.default.qwen3]        # 该端点下的模型配置
model = "qwen-3.6-35b-MTP"
native_tool_calling = false

[provider.default.qwen2]
model = "qwen2.5:7b"
native_tool_calling = true
```

- provider 定义"连接"（base_url, api_key）
- `[provider.<id>.<model_alias>]` 定义具体模型组合
- agent 通过 `"default.qwen3"` 形式引用

实现上 `ProviderConfig.model` 字段使用 `#[serde(flatten)]`，TOML 子表 `[provider.default.qwen3]` 会被 serde 自动收入 HashMap。

### 3. 全局运行时参数独立成节

```toml
[runtime]
context_threshold = 0.7   # 上下文压缩阈值，默认 0.7
max_iterations = 10       # agent 工具循环上限，默认 10
```

- 从 `[agent.main]` 提到全局
- P2 多 agent 共享同一阈值；若未来需要差异化，再回退到 agent 节覆盖

### 4. md 路径推导规则

`AgentConfig.soul/user/memory` 改为 `Option<String>`：

- `None` → `<workspace>/<SOUL|USER|MEMORY>.md`
- `Some(相对路径)` → `<workspace>/<相对路径>`
- `Some(绝对路径)` → 按绝对路径

### 5. log.dir 保持全局

`[log].dir` 默认 `~/.laia/logs`，不跟 agent 走。理由：日志是程序级基础设施，不是 agent 级状态。

## 完整 schema 示例

```toml
[runtime]
context_threshold = 0.7
max_iterations = 10

[log]
level = "info"
dir = "~/.laia/logs"

[provider.default]
type = "openai_compatible"
base_url = "http://10.0.11.218:8080/v1"
api_key = ""

[provider.default.qwen3]
model = "qwen-3.6-35b-MTP"
native_tool_calling = false

[agent.main]
model = "default.qwen3"
workspace = "e:/play/coding/laia/.test"
# soul/user/memory 可省

[channels.cli]
enabled = true

[tools.terminal]
confirm = "whitelist"
whitelist = ["ls", "cat", "grep", "pwd", "dir", "echo"]

[tools.tavily]
api_key = ""
```

## 遗留问题（推迟到后续 ADR）

- **环境变量插值**：`${TAVILY_API_KEY}` 语法避免明文存储，待 P2 安全性增强时讨论。
- **`--config <path>` CLI 参数**：当前 config 路径写死 `~/.laia/config.toml`，多实例场景需 CLI 参数覆盖。
- **tools 改为 HashMap**：当前 `ToolsConfig` 是固定 struct，加新工具要改 struct。P2 加新工具多了再重构。
- **工具 enabled 字段**：当前 FileRead/FileWrite/Terminal 永远注册，无法关闭。P1.5 加 web 面板时一起改。

## 影响的代码

- `src/config.rs`：schema 重写，新增 `RuntimeConfig`/`ModelConfig`，`AgentConfig` 字段调整，`ProviderConfig.model` 加 `#[serde(flatten)]`
- `src/channels/cli.rs`：md 路径通过 `resolve_md_path()` 推导，provider/model 通过 `Config::parse_model_ref()` 解析
- `src/commands/mod.rs`：`doctor_cmd` 适配新字段，`remember_cmd` 用 workspace 推导 memory 路径
- `src/agent/mod.rs`：`Agent` 新增 `max_iterations` 字段，从 `config.runtime` 读取而非硬编码
- `src/commands/slash.rs`：`/config` 命令展示 `max_iterations`
- `C:\Users\THAD\.laia\config.toml`：用户需手动迁移到新格式
