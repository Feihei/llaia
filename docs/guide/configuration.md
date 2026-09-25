# 配置参考

主配置文件是数据目录下的 `config.toml`，默认 `~/.llaia/config.toml`。数据目录的解析优先级为 `--config-dir` > 环境变量 `LLAIA_HOME` > `~/.llaia`（见 [CLI 参考](cli.md)）。敏感凭据集中放同目录的 `.env`，config 里用 `${VAR}` 引用，避免明文落盘。

> 配置 schema 的设计背景与完整示例见开发文档 [ADR-0008](../adr/0008-config-schema-v1.1.md)。本页是从用户视角使用配置的速查。

## 路径展开规则

- `~` / `~/path` → 用户 home 目录。
- `${VAR}` → 环境变量值（变量名须匹配 `[A-Z_][A-Z0-9_]*`；找不到则替换为空串并告警，让 `serve` 能进降级模式而非直接挂掉）。
- 顺序：先展开 env，后展开 tilde。

## `[runtime]` — 全局运行时

| 字段 | 默认值 | 说明 |
|---|---|---|
| `context_threshold` | `0.7` | 上下文压缩阈值（占 context_size 比例），超过自动压缩。 |
| `max_iterations` | `10` | agent 工具循环上限。 |
| `timezone` | 未设（跟随系统） | IANA 时区名，如 `Asia/Shanghai`。非法值告警并回退系统本地时区。 |
| `compact_model` | 未设 | 用更便宜的模型跑上下文压缩，值为 `[model.<id>]` 目录条目 id（P8 一元引用）；不配则复用主模型。 |
| `vision_model` | 未设 | 主模型无多模态时，用此模型描述图片（文本替换图片注入主模型）。 |
| `permission` | 未设（= `default`） | 权限档位：`read-only` / `default` / `yolo`。详见 [权限与安全](permissions.md)。 |
| `ask_user_timeout_secs` | `300` | ask_user 阻塞式澄清超时秒数，超时按"按最合理假设继续"处理。 |
| `tool_result_cap` | `32768` | 单个工具结果文本最大字符数，超限截断（完整内容留底 sqlite）。 |
| `keepalive_interval_secs` | `600` | 长任务心跳间隔（秒），聊天频道周期发 "still working"。 |
| `max_turn_duration_secs` | `3600` | 单轮最大运行时长（秒），超过自动中断。 |
| `output_guard` | `true` | 输出退化防护总开关（Generation Guard）。详见下方说明。 |
| `guard_repeat_window` | `512` | 重复检测滑动窗口（字符）。 |
| `guard_repeat_gram` | `24` | 重复检测 n-gram 长度（字符）。 |
| `guard_repeat_threshold` | `4` | 窗口内同一 n-gram 出现次数达到该值即判退化。 |
| `guard_thinking_cap` | `32000` | 思考流字符上限（`<think>` 块与 reasoning_content 累计），`0` = 不限。 |
| `guard_max_retries` | `1` | 判退化后重试次数（重试注入 `[guard]` 提示并强制关思考）。 |
| `guard_breaker_threshold` | `2` | 连续退化回合数达到该值时在诊断消息附加醒目警告。 |

**Generation Guard（输出退化防护）**：针对小参数本地模型在长上下文下的退化
（重复循环、思考流失控、空输出）。流式判定退化 → 中途断流、丢弃退化产物 →
注入 `[guard]` 提示并强制关闭思考重试；重试耗尽以诊断消息收尾，连续多轮退化
附加警告提示调整推理端采样参数（只报警不拒服）。`output_guard = false` 可整体
关闭，行为与旧版一致。详见 `docs/plans/2026-09-03-generation-guard.md`。

## `[log]`

| 字段 | 默认值 | 说明 |
|---|---|---|
| `level` | `info` | 日志级别：`debug` / `info` / `warn` / `error`。 |
| `dir` | `~/.llaia/logs` | 日志目录（未显式配时跟随 config 文件所在目录的 `logs/`）。 |

## `[provider.<id>]` — 连接注册表（P8）

provider 只管"连接"：端点类型、base_url、api_key，可选兼容覆盖层。模型全部搬进顶层 `[model.<id>]` 目录（见下节）。

```toml
# llm family：可承载模型条目（type 决定 wire 协议）
[provider.default]
type = "openai_compatible"          # 或 "anthropic" / "gemini"
base_url = "http://localhost:11434/v1"
api_key = "${OLLAMA_API_KEY}"       # 留空或引用 .env

[provider.claude]                     # 云端 Anthropic 示例
type = "anthropic"
api_key = "${ANTHROPIC_API_KEY}"

# service family：纯凭据搜索/抽取服务，不承载模型、不进 probe 列表
[provider.tv]
type = "tavily"
api_key = "${TAVILY_API_KEY}"
```

- `type` 缺省回退 `openai_compatible`；**显式写错的 type 启动即报错**。
- llm family 有 `[provider.<id>.compat]` 可选覆盖层，抹平 OpenAI 兼容端点的实现差异（自动按 base_url 探测，见 ADR-0026）。
- **旧配置自动迁移**：v0.5 的 `[provider.<id>.<alias>]` model 子表、`[tools.tavily|baidu|brave]` 凭据段、`[tools.tts]`/`[tools.image_gen]` 内联端点在启动时自动改写为新结构（内存态，磁盘文件不动）。model id 沿用 `<provider>.<alias>` 两段式，所以 agent 里的旧引用（如 `model = "default.qwen"`）无需任何改动。在 WebUI 保存一次即可把新结构写盘。

## `[model.<id>]` — 模型目录（P8）

模型全局唯一、按 kind 分型，引用一个 llm family provider：

```toml
[model.qwen]
provider = "default"
model = "qwen2.5:7b"                 # 服务端模型名（请求体 model 字段）
kind = "chat"                        # 缺省 chat；tts / image / embedding
native_tool_calling = false          # true=OpenAI function calling；false=标签协议降级；缺省 auto
context_size = 32768                 # 可选；不配则本地端点自动探测，探测不到按乐观默认 128000（provider 报溢出时自动收缩）。取 min(配置, 探测)
enabled = false                      # 可选；默认 true。把参数留在目录里，但不进 /models 列表与 WebUI 模型下拉
capabilities = ["multimodal"]        # 可选能力位数组，v1 仅 multimodal

[model.tts1]
provider = "default"
model = "qwen3-tts"                  # kind=tts：由 [tools.tts].model 引用
kind = "tts"

[model.sdxl]
provider = "default"
model = "sd_xl_base"
kind = "image"
size = "512x512"                     # kind=image 专属：默认输出尺寸

[model.sonnet]
provider = "claude"
model = "claude-sonnet-4-20250514"
max_tokens = 8192                     # Anthropic 必传，未配默认 4096
```

校验：`provider` 必须指向存在的 llm family provider（service family 会报错）；anthropic/gemini 只能挂 chat 条目。

### 模型级 `thinking`（思考能力声明）

```toml
[model.qwen35.thinking]
default = "high"                    # 模型默认档位：none|low|medium|high|max|unknown（缺省=unknown）
level_wire = "reasoning_effort"     # 档位方言：reasoning_effort（服务端校验非法值）或 none（不给旋钮）
off_wire = "reasoning_effort_none"  # 关方言：enable_thinking_false | thinking_disabled | reasoning_effort_none | unsupported
```

供 `/reasoning` 使用（详见 [斜杠命令](slash-commands.md)）：`level_wire` 决定档位怎么发（`reasoning_effort` = 顶层 `reasoning_effort` 字段），`off_wire` 决定「关」怎么发——`enable_thinking_false` 发嵌套 `chat_template_kwargs:{enable_thinking:false}`（llama.cpp 实测）、`thinking_disabled` 发 `thinking:{type:"disabled"}`（DeepSeek 系）、`reasoning_effort_none` 发顶层 `reasoning_effort:"none"`（Ollama/agnes 实测唯一有效的关）、`unsupported` 表示该端点拒绝关思考（GLM 经部分网关实测 400），`/reasoning none` 会被拒收。三个字段**全部缺省 = 能力未知**：档位意图被拒收、`none` 走 legacy 兜底（`compat.disable_thinking_template` 门内注入，字节兼容旧行为）。字段值请按实测填写，不要按模型名猜——方言挂在部署形态上，同名模型在不同网关/推理引擎下方言可能不同（详见 `docs/plans/2026-09-10-thinking-capability-model.md`）。`preserve`（思考回传）为 P2 字段，暂未开放。

### 模型级 `enabled`（可发现性开关）

用途：模型参数先记在目录里，但暂时不希望它被选中。

- 缺省即 `true`，存量配置无需改动；**只有显式 `enabled = false` 会写入** `config.toml`（启用态不落盘，避免每个模型多一行脏 diff）。WebUI Config → Models 的模型卡上有开关，关闭后卡片灰化但仍可编辑、可重新启用。
- 只影响**可发现性**，不影响可用性：`model_from_ref` 不拦显式引用，所以 `agent.<alias>.model` 指向已禁用模型时**继续生效**（否则"关掉当前模型"会变成下次启动即失败）；WebUI 下拉此时把当前值显示为 `xxx (current)`。
- `agent.<alias>.fallback` 中指向禁用模型的项被剔除并 warn。
- `runtime.compact_model` / `runtime.vision_model` 指向禁用模型时 warn 并回退主模型；**磁盘上的引用原样保留**，模型重新启用后自动恢复，无需重填。
- 校验发生在 `Config::reconcile_disabled_models`，由 `Config::load` 与 WebUI 保存路径各调一次，因此改动即时生效、不需重启。

## `[agent.main]`

| 字段 | 说明 |
|---|---|
| `model` | `[model.<id>]` 目录条目 id（P8 一元引用）；留空 = 降级模式（仅可配置 Web UI）。 |
| `fallback` | 备用模型链，如 `["small", "big"]`（同为目录条目 id），主模型失败依序降级。 |
| `workspace` | **已移除**，自动推导到 `~/.llaia/workspace/`（子 agent 到 `workspace/subagent/<alias>/`）。旧配置里写了该字段会被直接忽略，可安全删除。 |
| `soul` / `user` / `memory` | **已废弃**，自动从 agent 家目录推导，显式设置不生效。 |
| `denied_tools` | 子 agent 工具黑名单（主 agent 一般留空）。 |
| `delegate_timeout` | 委派超时秒数，默认 120（仅子 agent 生效）。 |

> 子 agent（`[agent.<alias>]`）可由主 agent 委派任务，P2 能力；日常个人使用通常只用 `main`。

## `[webui]`

| 字段 | 默认值 | 说明 |
|---|---|---|
| `host` | `127.0.0.1` | 监听地址（默认仅本机；改 `0.0.0.0` 需自行负责安全）。 |
| `port` | `51217` | 监听端口（避开 8080 等常见服务）。 |
| `token` | 空 | 鉴权 token；留空则启动时随机生成并打印日志。 |

旧 `[channels.web]` 会自动迁移到 `[webui]`（向后兼容）。详见 [Web UI](webui.md)。

## `[tools.terminal]`

| 字段 | 默认值 | 说明 |
|---|---|---|
| `confirm` | `whitelist` | 已废弃，仅作兼容；实际以 [权限档位](permissions.md) 为准。 |
| `command_policy` | `blacklist` | `blacklist` / `whitelist` / `none`。 |
| `command_whitelist` | `[]` | 仅 `policy=whitelist` 时生效。 |
| `whitelist` | `[ls, cat, grep, pwd, dir]` | 旧字段，兼容保留。 |
| `interpret_inline` | `approval` | 解释器内联载荷闸门：`approval`（默认）时，`python -c` / `node -e` / `php -r` / `curl … \| bash` 等内联执行即使落在 workspace 内也强制 `/ok` 人审；`off` 关闭。跑脚本文件（`python script.py`）不拦。yolo 档整体弃权审批，不受此闸门约束。 |

## `[tools.search]`

| 字段 | 默认值 | 说明 |
|---|---|---|
| `provider` | 空（不注册） | 选定的搜索 provider，须为 **service family**（`tavily` / `baidu` / `brave`）的 `[provider.<id>]` 条目 id（P8）。 |
| `top_k` | `8` | 默认返回条数。 |

统一 `search` 工具：对外只暴露一个 `search`，内部按所选 provider 的 type 分派到对应 adapter，不串试、不聚合。搜索源的 api\_key 配在 provider 条目上（凭据统一进注册表）。

> **解释器内联载荷为什么单独设闸**：terminal 的命令黑名单与路径校验都作用于命令行字符串本身，而 `python -c "…"` / `node -e "…"` 的真正文件操作发生在解释器内部，框架无法感知——静态分析载荷内容也不可靠。因此内联执行一律升级到人审（T3，2026-09-07 定案）：这是当下唯一能覆盖未知载荷的闸门；跑脚本文件不拦（脚本路径仍走路径校验，且写入动作在会话记录中可审计）。彻底封堵（进程级约束）见[安全加固指南](security-hardening.md)（T2 无特权账户）。

## `[tools.tts]`

| 字段 | 默认值 | 说明 |
|---|---|---|
| `enabled` | `false` | 是否注册 `tts` 工具。 |
| `model` | 空 | **kind=tts** 的 `[model.<id>]` 条目 id；端点与 key 来自条目的 provider。 |
| `voice` | `alloy` | 默认音色（调用参数，非模型属性）。 |

## `[tools.image_gen]`

| 字段 | 默认值 | 说明 |
|---|---|---|
| `enabled` | `false` | 是否注册 `image_gen` / `image_edit` 工具。 |
| `model` | 空 | **kind=image** 的 `[model.<id>]` 条目 id；端点、key 与默认尺寸（`size`）均来自条目。 |
| `timeout_secs` | `300` | 单次生成超时（本地扩散较慢）。 |

## `[tools.web_fetch]`

| 字段 | 默认值 | 说明 |
|---|---|---|
| `max_chars` | `20000` | 返回正文最大字符数，超出截断。 |
| `extract_provider` | 空（本地抽取） | 服务端正文抽取，须为支持 extract 的 service family provider（如 tavily）。 |

## `[channels.*]`

各频道默认关闭。凭据用 `${VAR}` 引用 `.env`。详见 [频道](channels.md) 获取每个频道的字段与单用户安全锁（`allow_*`）。

## 校验配置

```bash
llaia config        # 打印生效配置
llaia doctor        # 连通性 + 文件完整性诊断
```

Web UI 也提供 `/api/config/validate` 校验接口（见 [Web UI](webui.md)）。
