# ADR-0013: cron 定时任务调度

- 状态：Proposed
- 日期：2026-08-04
- 关联：[ADR-0002](0002-agent-architecture.md)、[docs/plan.md P3-c](../plan.md)

## 背景

LLAIA 当前所有交互都是被动响应用户消息。用户希望让 agent 主动做事：

- 每天 8:00 查新闻摘要推送
- 每周一生成上周工作总结
- 每 30 分钟检查某个数据源

AstrBot 有 `CronMessageEvent` + `background_task` 机制：定时任务到点后向 agent 投递消息事件，agent 像处理用户消息一样处理。ZeroClaw 内置 `cron/schedule tasks` 工具，agent 可以自己创建定时任务。

LLAIA 需要决定：

1. cron 任务如何定义（配置形态）
2. 到点后触发什么（直接跑工具链 vs 唤醒 agent）
3. 如何调度（依赖库 vs 自实现）
4. 结果如何推送回用户
5. 进程重启后如何恢复

## 决策

采用 **双模式 + 配置文件定义 + 进程内调度** 方案。

### 1. 配置形态

cron 任务定义在 `~/.llaia/cron.toml`（独立文件，不放 config.toml，避免主配置膨胀）：

```toml
# ~/.llaia/cron.toml

[[task]]
id = "morning_news"
schedule = "0 8 * * *"           # 5 字段 cron 表达式（分 时 日 月 周）
mode = "agent"                    # agent / tools
channel = "qq"                    # 推送目标：qq / cli / web
enabled = true

# mode = "agent" 时：注入到主 agent 上下文的提示词
prompt = """
现在是早上 8:00。请用 tavily_search 查今天的 AI 科技热点，
整理成 3-5 条简讯，每条一句话 + 链接，最后推送给我。
"""

# mode = "tools" 时：预定义工具链（不消耗 LLM token）
# 最后一步输出自动推送到 channel，不需要 send_message 工具
# [[task]]
# id = "health_check"
# schedule = "*/30 * * * *"
# mode = "tools"
# channel = "web"
# enabled = true
# steps = [
#   { tool = "tavily_search", args = { query = "llaia release notes" } },
#   { tool = "memory_write", args = { text = "last check at {{now}}" } },
# ]
# # 最后一步 memory_write 的输出自动推送到 web channel
```

### 2. 双模式

#### mode = "agent"

到点后唤醒主 agent：

1. 构造一条系统消息：`[cron:<id>] <prompt>`
2. 通过 `run_turn` 走完整 agent 循环（system prompt + 工具调用 + 回复）
3. agent 输出的最终回复作为结果推送到指定 channel
4. agent 可自主调工具（tavily_search / file_write / memory_write 等）

特点：灵活，能处理复杂任务；消耗 LLM token；agent 可能跑偏（需要好提示词）

#### mode = "tools"

到点后直接按 `steps` 顺序执行工具链：

1. 解析 steps，逐个调 `tool.execute(args, channel)`
2. 占位符替换（极简，仅两个）：
   - `{{prev}}`：上一步工具的输出
   - `{{now}}`：当前时间（RFC3339 字符串）
3. 全部完成后**自动把最后一步输出推送到 `channel`**（不需要 `send_message` 工具）
4. 任何一步失败则终止，推送失败通知到 `channel`（见 §7），记录失败日志

特点：不消耗 LLM token；可预测；灵活度低（无判断能力）；适合确定性任务（健康检查、定时备份等）

**占位符只做极简支持**：不引入 `{{step_N}}` / `{{env.VAR}}` / JSONPath 等。需要复杂逻辑（条件判断、循环、多步骤引用）时用 `mode = "agent"` 让 LLM 自己处理。

### 3. 调度器实现

用 `tokio_cron_scheduler` crate（已有 tokio 生态，无新依赖类型）：

- 进程启动时（`serve_cmd`）加载 `cron.toml`，注册所有 enabled 任务
- 每个 task 一个 `Job`，到点时 `tokio::spawn` 执行
- 进程退出时调度器一起 drop（不持久化运行时状态，任务定义在文件里已持久化）
- 进程重启后从文件恢复，无需额外状态

### 4. 结果推送

`channel` 字段指定推送目标：

- `qq`：通过 QqChannel 的 send_c2c 主动发消息（需要 owner openid，从 USER.md 读取）
- `cli`：仅在 CLI channel 启用时有效，输出到终端（如果没人在用 CLI 则 log + skip）
- `web`：通过 WebChannel 的 WS 主动推一条消息（需要前端有 active session，否则 log + skip）

主动推送是新能力（之前所有回复都是被动跟随用户消息），需要在 QqChannel 和 WebChannel 加 `send_proactive(message)` 方法。

### 5. agent 模式的上下文

cron 触发的 agent 调用是否共享用户会话上下文？

**决策：开独立会话**，不污染用户当前会话：

- 用 `SessionStore::new_session()` 创建临时 session
- 加载 SOUL/USER/MEMORY 但不复用 history
- 任务完成后 session 保留（写入 sessions.db，source 标记为 `cron:<id>`）
- **WebUI 默认隐藏 cron 会话**：会话列表只展示用户交互会话，cron 会话单独在"cron 执行历史"tab 展示（避免污染用户会话列表）。用户可查看 cron 触发的对话内容用于调试

### 6. WebUI 管理

在 WebUI 配置面板加 cron tab：

- 列表展示所有 cron 任务（id / schedule / mode / channel / enabled）
- 表单增删改查
- 直接编辑 `cron.toml` 原始文本（CodeMirror）
- "立即执行"按钮（手动触发一次，用于调试）
- **cron 执行历史**：单独 tab，展示 cron 触发的会话记录（从 sessions.db 查 `source = "cron:<id>"`），不混入用户会话列表

### 7. 失败处理

cron 任务执行失败时的处理策略：

- **不重试**：到点执行一次，失败即失败，不自动重试（简化实现；避免 provider 临时不通时反复消耗 token）
- **推送失败通知**：失败时向 `channel` 推送一条失败通知，格式 `[cron:<id> 失败] <错误信息>`，让用户知道任务没跑成
- **记录日志**：`tracing::error!` 记录详细错误 + `audit.log` 记录失败事件
- **不自动 disable**：连续失败不自动禁用任务（单用户场景，用户自己判断是否禁用；避免因短期故障导致任务被静默禁用）

失败通知推送失败时（如 QQ 通道不可用）只 log，不再二次兜底。

## 不做

- **agent 自我创建 cron 任务**：不让 agent 通过工具调用动态创建定时任务（ZeroClaw 风格）。理由：单用户场景下用户自己配 cron.toml 更可控，agent 自建易失控（循环触发、资源耗尽）
- **跨进程调度**：不在多进程间协调 cron（如多台机器跑同一个 LLAIA）。单进程内调度足够
- **复杂工作流**：不支持条件分支、循环、子任务编排。需要复杂逻辑时用 mode=agent 让 LLM 自己判断
- **错过任务补跑**：进程停机期间错过的任务不补跑（简化实现）。下一次到点正常执行
- **任务失败重试**：不自动重试（避免 provider 临时不通时反复消耗 token；失败通知到 channel 即可）
- **连续失败自动 disable**：不自动禁用任务（单用户场景用户自己判断；避免短期故障导致任务被静默禁用）
- **`send_message` 工具**：不引入内置 `send_message` 工具。tools 模式最后一步输出自动推送；agent 模式 agent 最终回复自动推送
- **复杂占位符**：tools 模式只支持 `{{prev}}` + `{{now}}`，不支持 `{{step_N}}` / `{{env.VAR}}` / JSONPath。需要复杂逻辑用 mode=agent

## 影响

### 新增依赖

- `tokio_cron_scheduler = "0.13"`：cron 调度

### 配置文件

- 新增 `~/.llaia/cron.toml`（可选，不存在时无 cron 任务）
- `llaia init` 生成空的 cron.toml 模板

### 代码变更

- 新增 `src/cron/mod.rs`：`CronTask` 结构、`CronScheduler` 包装 `tokio_cron_scheduler::JobScheduler`
- 新增 `src/cron/runner.rs`：agent 模式 / tools 模式执行器
  - agent 模式：构造 `[cron:<id>] <prompt>` 系统消息 → `run_turn`（独立 session，source 标记 `cron:<id>`）→ 最终回复推送 channel
  - tools 模式：逐 step 调 `tool.execute(args, channel)`，`{{prev}}`/`{{now}}` 占位符替换 → 最后一步输出推送 channel
  - 失败处理：任一步失败 → 推送 `[cron:<id> 失败] <错误信息>` 到 channel → `tracing::error!` + `audit.log` 记录
- `src/commands/mod.rs`：`serve_cmd` 启动 cron scheduler（仅 serve 模式启动，chat 模式不启动）
- `src/channels/qq.rs`：加 `send_proactive(message)` 方法（owner openid 从 USER.md 读）
- `src/channels/web.rs`：加 `send_proactive(message)` 方法（向所有 active WS session 推送）
- `src/web/mod.rs`：
  - 加 `/api/cron` 路由（GET 列表 / POST 创建 / PUT 更新 / DELETE 删除 / POST `/api/cron/<id>/trigger` 立即执行）
  - 加 `/api/cron/history` 路由（GET 查 cron 会话历史，从 sessions.db 查 `source LIKE "cron:%"`）
- `src/web/static/app.js`：加 cron tab + cron 执行历史 tab

### 与 P3-a 的依赖

- cron agent 模式触发主 agent 时，主 agent 在 `~/.llaia/workspace/` 工作（按 ADR-0011 的 agent workspace 模型，与 channel 无关）
- agent 调用工具受 agent workspace 边界 + 命令策略 + confirm_mode 约束
- cron 任务本身不写 audit.log（属于系统行为），但 agent 模式下 agent 调用工具有副作用时仍写 audit

## 参考

- AstrBot `CronMessageEvent` + `background_task` 方案：定时任务到点向 agent 投递消息事件
- ZeroClaw 内置 `cron/schedule tasks` 工具：agent 可自建定时任务（本 ADR 不采用）
- grilling 第四轮 Q2：用户选择"双模式可选"
- grilling 第八轮（P3-c 细化）：
  - Q1: tools 模式占位符极简（只 `{{prev}}` + `{{now}}`，复杂逻辑走 agent 模式）
  - Q2: cron 会话默认隐藏，WebUI 单独 tab 展示执行历史
  - Q3: 失败不重试但推送失败通知到 channel，不自动 disable
  - Q4: 不引入 `send_message` 工具，tools 模式最后一步输出自动推送
