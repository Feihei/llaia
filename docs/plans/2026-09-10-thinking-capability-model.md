# 思考能力声明与 reasoning_content 留存/回传（thinking capability）

状态：**草案 v2**（2026-09-10 两轮真机探针后重写；目标按「agent 场景思考默认开是主流，控制面是档位+留存+回传，关是边缘能力」重定义）
日期：2026-09-10
关联：`docs/adr/0026-provider-compat.md`（compat 层，本方案加 thinking 声明）、`docs/plans/2026-09-03-generation-guard.md`（guard 强制关思考，本方案给它加能力门禁）、`docs/guide/slash-commands.md`、`docs/guide/configuration.md`

## 背景与问题

`/reasoning` 今天是一个 bool、单向、只有一条方言，而且**思考内容连留存都没有**：

| 对象 | 现状 | 锚点 |
| --- | --- | --- |
| 命令 | `/reasoning [on\|off]`，参数不做小写归一（`/reasoning Off` → unknown arg） | `src/commands/slash.rs:546-569`、`:43` |
| 状态 | `Agent.thinking_off: bool`，纯内存、不写 config、重启即丢；**进程级**（所有频道共享同一 `Arc<Mutex<Agent>>`），不跟任务线 | `src/agent/mod.rs:140-142` |
| 求值 | `disable_thinking: self.disable_thinking \|\| self.thinking_off \|\| attempt > 0` | `src/agent/mod.rs:1101` |
| 落地 | 仅 `chat_template_kwargs:{enable_thinking:false}`，**只会发 false**（`on` = 什么都不发） | `src/provider/openai_compat.rs:272-279`、`:694-709` |
| 消费面 | 只有 openai_compat 读；`anthropic.rs`/`gemini.rs` 零引用（静默 no-op 却回显 `[reasoning: off]`） | grep 无命中 |
| 流内 | 思考文本在 provider 层被**丢弃**（`reasoning_to_content=false` 时只计数）；`ChatResponse` 无 reasoning 字段 | `openai_compat.rs:420-455`、`provider/mod.rs:163-170` |
| 落库 | **`INSERT INTO messages` 只写四列**，`reasoning_content` 列建了但无写入通路 → 恒为 NULL；三个读路径都在取空值 | `src/memory/sqlite.rs:618`、`:212`、`:516`/`:649`/`:970` |
| 回传 | `ChatMessage` 无该字段 → 续轮永不回传思考 | `src/provider/mod.rs:76-83` |

所以「默认态 = 模型默认、不回传」这个判断成立，但要补一句：**也不落 session**。留存与回传是同一处缺口的两个下游。

## 实测：五家端点的真实控制面

方法：对 `~/.llaia/config.toml` 里实际在用的 provider 发极小请求（`max_tokens:128`、prompt 一句），变体 = 基线 / `chat_template_kwargs.enable_thinking=false` / 顶层 `enable_thinking` / `thinking.type=disabled` / `reasoning_effort=low|max|bogus` / 回传 `reasoning_content`。脚本与原始输出在 scratch（`probe_thinking.py`、`cloud_probe.txt`、`cloud_probe2.txt`）。

| 通路 | 默认是否思考 | 关 | 档位 | 保留 | 其他关键事实 |
| --- | --- | --- | --- | --- | --- |
| `llamacpp.qwen3_8_27b`（qwen-3.8-27b，本地 router） | **开**（reason 117 字符 / 33 tok / 3.4s） | ✅ `chat_template_kwargs.enable_thinking=false` → reason **0**、2 tok、1.2s | caps `supports_reasoning_effort: true`，但单发对比判不了（见方法论） | caps `supports_preserve_reasoning: true`（＝模板会渲染，**不**等于接受回传，见「撤回」节） | **顶层 `enable_thinking` 无效**（reason 55），必须嵌套；`--jinja` 不在 argv 也照样生效（新版内置 minja，caps 在场即证据） |
| `llamacpp.qwen3_6_35b`（qwen-3.6-35b-MTP） | 开（同一部署形态） | ✅ 同上（模板含 `enable_thinking` 分支） | **`supports_reasoning_effort: false`** | `supports_preserve_reasoning: true` | **同一台 router、两个模型能力位不同** → 能力是 per-model 的铁证 |
| `tokenrouter.glm5_3`（`z-ai/glm-5.3-free`） | 开（reasoning_tokens 82） | ❌ **HTTP 400**：`GLM-5.3 does not support disabling thinking` | ⚠️ **此通路不可信**：`reasoning_effort:"bogus"` → **HTTP 200 正常返回**（不校验），此前据 82→36→28 判"档位生效"是**误判，已撤回** | `thinking{enabled,clear_thinking:false}` → 200（不报错，也无法证明生效） | `chat_template_kwargs` 被吞（200、无效果） |
| `sensenova.deepseek_v4_flash` | 开（reason 53 / 16 tok） | ✅ `thinking:{type:"disabled"}` → reason **0** / reasoning_tokens 0 | 单发 `reasoning_effort:"low"` → 200（reason 194，反而更多 → 抖动，未证） | 未测 | 非法组合有精确规则：`thinking{disabled}` + `effort:"low"` → **400** `only be disabled when reasoning effort is none` |
| `modelscope.deepseek-v4-flash` | **不可知** | — | — | — | 所有变体 **HTTP 200 + 零内容**（`completion_tokens:0`、无 `finish_reason`、0.1s），偶尔 429 → 见下文对 guard 的影响 |
| `agnes-cn.agnes2_5_flash` | **关**（基线 reason 0 / completion 2 tok） | ✅ 语义上 `none` 即关 | ✅ **唯一有效方言**是 `reasoning_effort`；`bogus` → 400 并直接吐出合法集 **`none\|low\|medium\|high\|max`**；`high` → reason 101 / 24 tok | 未测 | 顶层/嵌套 `enable_thinking`、`thinking:{type:"enabled"}` 三者**全部 200 但零效果**（wiki 文档写的 `chat_template_kwargs` 在这家网关被静默忽略） |

### 三条结构性结论

1. **「关」是少数能力，且表达形态互不相同**：本地靠嵌套 bool、DeepSeek 靠 `thinking.type`、agnes 靠 `reasoning_effort:none`、GLM **明确拒绝**（400）。任何单一 bool 或单一方言都覆盖不了。
2. **「什么都不发 = 开着」这个隐含假设是错的**：agnes 基线不思考，且它的 `on` 只能靠发 `reasoning_effort`。框架必须知道**每个模型的默认态**，否则连"开"都做不到。
3. **能力挂在 (provider, model) 粒度**：同一个 `deepseek-v4-flash` 在 modelscope 静默空响应、在 sensenova 正常；同一个 GLM-5.3 官方支持 `reasoning_effort`、经 tokenrouter 则不校验。`[provider.<id>.<model_alias>]` 正好是这个粒度。模型名只能当默认值（agnes 一例即证伪直觉：名字陌生，方言却恰好是 `reasoning_effort`；反过来它文档写的 `enable_thinking` 实测无效）。

### 追加：跨家族对照与一处撤回

`ornith-1.5-35b`（非 Qwen 家族，`Ornith-1.5-35B-A3B-APEX-MTP-I-Quality.gguf`）与 `qwen-3.6-35b-MTP` **能力面完全一致**：同 build（b10645）、caps 同为 `supports_preserve_reasoning:true` + `supports_reasoning_effort:false`、模板同样含 `enable_thinking` 与 `reasoning_content` 分支（7764 vs 8057 字符）。行为逐项对齐：基线 reason 122 / 1.0s，嵌套 kwargs 关 → reason 0 / 2 tok / 0.3s（3.4×），顶层 `enable_thinking` 无效，`reasoning_effort:"low"` 反而多想（317，content 被 max_tokens 截空），`reasoning_effort:"bogus"` **200 正常返回**。

两条推论：一是方言与能力位**跨家族稳定**（本地 llama.cpp 的形态由 server + GGUF 模板决定，不由模型家族决定），所以 D4 探 caps 是通用正解；二是 bogus 不报错说明 **HTTP 状态码在本机也不能当"参数被支持"的证据**，可信来源只有 caps。

> **撤回**：本文 v1 曾据「回传 `reasoning_content` 后 prompt 57→87」判定本地会消费回传的思考。补控制组后推翻：同一三段消息、**只差该字段时 prompt 30 vs 30 一字不差**，那 +30 全是两条额外消息的开销。真实机制是 llama.cpp 的 OpenAI 兼容层在**请求解析阶段丢弃这个未知 per-message 字段**，到不了模板分支（`/models` 里各实例 argv 无任何 `--*think*`/`--jinja` 透传参数）。连带修正对 caps 的解读：`supports_preserve_reasoning` 指"模板会渲染思考块"，**不是**"服务端接受你把思考送回来"。

### 本地 llama.cpp 新 build 的完整开关面（help 实测，e:/apps/llamacpp-cuda13-win-x64，build 10867 / v0.4.0-dev）

注意版本差：**正在跑的远端 router 是 b10645**（argv 里的 `C:\Program Files\llamacpp-cuda133-win-x64\` 是那台机器的路径，本机不存在，读不到它的 help）。以下属 10867。

| 开关 | 取值 | 默认 | 对 LLAIA 的意义 |
| --- | --- | --- | --- |
| `--jinja` / `--no-jinja` | bool | **enabled** | 解释为何 argv 里没有 `--jinja` 而嵌套 kwargs 照样生效（旧版要显式开） |
| `--reasoning [on\|off\|auto]` | 三态 | auto（从模板探测） | 部署级总开关，服务端已替我们做了"这模型能不能想"的判定 |
| `--reasoning-effort LEVEL` | minimal/low/medium/high/xhigh/max | default | 部署级档位，"given to the chat template" → 与 caps `supports_reasoning_effort` 配对 |
| `--reasoning-budget N` | -1 不限 / 0 立即结束 / N>0 | -1 | **token 级连续预算**，比云端所有档位都细；本地反而是控制面最强的一端 |
| `--reasoning-budget-message MSG` | 文本 | none | 预算耗尽时注入到 end-of-thinking 前的收束语 |
| `--reasoning-preserve` / `--no-reasoning-preserve` | bool | **enabled** | P2 的前置在此：保留"全部历史"而非仅最后一条 assistant 的思考，且 help 明说"compatible with certain templates having `supports_preserve_reasoning` capability" → caps 位的真实语义 = 模板配合此开关 |
| `--reasoning-format` | none / deepseek / deepseek-legacy | auto | `deepseek-legacy` = 在 content 里**保留** `<think>` 标签，**同时**填 `reasoning_content` → **ADR-0026 当年二选一的前提没了** |
| `--chat-template-kwargs STRING` | json 对象 | — | 部署级注入任意模板参数（per-request 同名能力已实测可用） |

三点改判。**一**，P2 的前置从「客户端要不要发这个字段」变成「服务端有没有开 `--reasoning-preserve`」，而它默认是开的 → 远端 b10645 那次丢弃更可能是版本差异而非机制缺失（待验，见待实测 6）。**二**，`--reasoning-format` 三个取值把 ADR-0026 当年的二选一变成了共存：`deepseek-legacy` 同时填 `content`（带 `<think>` 标签）与 `reasoning_content`，所以「折回可见文本」与「分离字段」不必赌一个默认值。**三**，`--reasoning-budget` + `--reasoning-budget-message` 给了比 `guard_thinking_cap` 优雅得多的路径：框架侧「超了就掐掉重来」，服务端「到预算就收束并给一句收束语」。由此 LLAIA 侧的设计改动是：`guard_thinking_cap` 应优先映射为**请求级预算**，只在不支持的端点上退化成 abort + 重试。

### 方法论：哪些断言能单发判，哪些不能

- **能判（二值信号）**：参数被接受还是 400；思考能否归零（reason_len 0 vs >0）；字段被吞还是被校验。**本 plan 的结论只建立在这类断言上。**
- **不能判（抖动主导）**：档位是否真的"少想"。`reasoning_effort:low` 在本地、GLM、sensenova 三处都出现"比基线想得多"的反常，GLM 的 reasoning_tokens 在同参数下 82/36/28 乱跳。**判档位需固定 seed + N 次取中位数**，或直接放弃断言、在声明里把该通路的 `levels` 留空（对齐 goose 的 `ThinkingEffortSupport::Unsupported`：没有旋钮是一等状态，UI 就不给旋钮）。
- **必须带控制组**：判"某字段是否被消费"时，要发**只差该字段**的成对请求。本 plan 的 `preserve` 结论就因缺这一对照组而误判一次（见上节撤回）。消息条数、role、system 注入任一变化都会让 token 差异失去解释力。
- **400 有方向性**：400 ⇒ 该参数被校验 ⇒ 支持（Ollama / agnes / sensenova 三次均成立）；但 **200 反之不成立**——tokenrouter 对 `reasoning_effort:"bogus"` 照返 200。探针不报错永远不能当能力声明。
- **别为探测加载模型**：本轮 Ollama 首发耗时 114.8s 是冷加载。探测面只能走不加载的元数据端点（`/api/show`、`/props`、`/models`），任何 chat 端点探测都不允许进入交互关键路径。

### 本地 Ollama 实测（ollama 0.33.3，`qwen3.5:4b`）

OpenAI 兼容层（`/v1/chat/completions`）与原生 `/api/chat` **各认不同参数**：

| 形态 | 结果 | 判定 |
| --- | --- | --- |
| 基线 / `reasoning_effort:"low"` / `chat_template_kwargs.enable_thinking=false` / `thinking:{type:"disabled"}` / 顶层 `think:false` | 五发行为**逐字节相同**：content 空、`finish_reason=length`、compl 48 tok、`reasoning_content` 长度 0 | 后三种方言 **Ollama 全不认**（直传 `think` 也不透传，AstrBot 的注释得到复现）；且兼容层**不把 thinking 映射到 `reasoning_content`** |
| `reasoning_effort:"none"` | content `ok`、compl 2 tok、2.3s、prompt 15→17 | **唯一有效的关** |
| `reasoning_effort:"bogus"` | **HTTP 400** + 合法集 `minimal,low,medium,high,xhigh,ultra,max,none`（8 档，含 `ultra`） | 服务端校验 ⇒ 档位存在（正向量测） |
| 原生 `think:false` / `true` / `"low"` | thinking 0 / 211 / 198（基线 193） | 原生通路可关；档位降幅仍判不了（抖动） |
| 回传 `reasoning_content`（兼容层）/ `thinking`（原生） | 控制组 prompt **30 vs 30**（两条通路各测一对） | **都不消费**。Ollama 侧无 `--reasoning-preserve` 类开关，这是硬结论 |

旁证与连带结论：`fredrezones55/Qwopus3.5:4b`（qwen3.5 衍生，parent blob 指向基座）capabilities 与基座**完全一致**——Ollama 的能力由 family 注册表（`RENDERER/PARSER qwen3.5`，`template` 仅 13 字符 `{{ .Prompt }}`）决定，不像 llama.cpp 那样取决于 GGUF 内嵌模板。所以「衍生变体能力不可信」这条只在 llama.cpp 侧成立，Ollama 侧反而稳。而 `reasoning_effort` 一条方言同时命中 **Ollama + agnes + DeepSeek** 三家（agnes 的 `none\|low\|medium\|high\|max` 是 Ollama 8 档的子集），实现两条方言即可覆盖全部有效控制面。

**Ollama 上的 guard 死循环（实测机理）**：思考吃满 `max_tokens` → content 空 + `finish_reason=length` → 被空输出判定当作退化 → 强制 `disable_thinking` 重试 → 重试发 `chat_template_kwargs` → Ollama 忽略 → 二次仍空 → 抛误导性诊断。即今天在 Ollama 上「输出被思考吃满」这个最常见的形态，重试是**完全无效**的。

### 探测面（本地，只读）

| 端点 | 可探到的东西 | 判定 |
| --- | --- | --- |
| `8080/props`（llama.cpp **router 模式**根） | stub：`role:"router"`、`model_path:"none"`、`n_ctx:0`，**无 chat_template** | 根路径探测对本机主力部署无效（也解释了 context_size 为何一直靠显式配置） |
| `8080/props?model=<loaded>` | `chat_template`（8057 字符）+ **`chat_template_caps`** 结构化能力位 + `modalities` | **优先读 caps，正则扫模板只是降级路径** |
| `localhost:11434/api/show` | `capabilities:["completion","vision","tools","thinking"]`；`template` 只有 13 字符 `{{ .Prompt }}`（RENDERER/PARSER 接管） | 扫模板在 Ollama 上**必然失效**，只能读 capabilities（衍生变体 `Qwopus3.5:4b` 的 capabilities 与基座一致，能力来自 family 注册表） |

> **探测 hazard（实测踩到）**：该 router `max_instances:1`，对未加载模型发 `/props?model=<id>` 会**触发换入换出**（实测把 3.8-27b 装上了、3.6 被卸载）。探测必须限定已加载实例，或走只读的 `/models` → `data[].status.args`（server argv 直接可读，本机无 `--jinja`/`--chat-template` → 模板来自 GGUF 内嵌）。

## 决策

**D1 规范档位：`none | low | medium | high | max`，`auto` 为默认（= 不发任何参数）。**
`/reasoning [auto|none|low|medium|high|max]`，参数照 `cmd_lc` 归一小写。`auto` 是新的默认值——它取代今天 `on` 的"什么都不发"语义，并明确区别于 `none`（后者是"要求关"）。
`none` 落在不支持关的模型上 → **报错并列出该模型可用档**，不静默降级成 `low`（那会把"以为关了其实没关"藏得更深）。GLM 尤其必须报错：实测发 `disabled` 是 400，静默降级至少得先探测过。

**D2 两个正交字段替代方言矩阵。**
`level_wire`（怎么表达档位）× `off_wire`（怎么表达"关"），恰好覆盖实测五家：

```toml
[provider.llamacpp.qwen3_8_27b.thinking]
default = "unknown"                    # 实测只能断言「非 none」（基线 reason 117 字符），测不出精确档
level_wire = "none"                    # caps supports_reasoning_effort=true，但未经重复采样确认前保守留 none
off_wire = "enable_thinking_false"     # 嵌套 chat_template_kwargs（顶层无效，实测）
preserve = false                       # 远端 b10645 实测无效（控制组 prompt 30 vs 30）；新 build 有 --reasoning-preserve 且默认 enabled，升级后应转 true

[provider.ollama-local.qwen3_5-4b.thinking]
default = "high"                       # 基线确在思考（冷加载后 compl 吃满 48 tok）
level_wire = "reasoning_effort"        # 8 档含 ultra，服务端校验非法值（400）
off_wire = "reasoning_effort_none"     # 实测唯一有效的关；chat_template_kwargs/thinking/顶层 think 三种全被忽略
preserve = false                       # 双通路控制组各测一对：prompt 30 vs 30、prompt_eval 30 vs 30

[provider.agnes-cn.agnes2_5_flash.thinking]
default = "none"                       # 实测：基线 reason 0，确定
level_wire = "reasoning_effort"
off_wire = "reasoning_effort_none"
preserve = false

[provider.tokenrouter.glm5_3.thinking]
default = "max"                        # 官方文档口径；实测基线确在思考
level_wire = "none"                    # 此通路不校验 effort（bogus 也 200）→ 不给旋钮
off_wire = "unsupported"               # 实测 400
```

映射规则（provider 层唯一收口）：`level_wire=reasoning_effort` → 发顶层 `reasoning_effort`；`off_wire=enable_thinking_false` → 发 `chat_template_kwargs:{enable_thinking:false}`、非 none → 发 `{enable_thinking:true}`；`off_wire=thinking_disabled` → 发 `thinking:{type:"disabled"}` 且**同时抑制 effort 字段**（DeepSeek 的 400 规则直接来自实测错误消息）；`off_wire=unsupported` → 任何 `none` 意图整个短路。

**D3 能力门禁前置于注入。**
`ThinkingCapability{default, levels, off_wire, preserve}` 先求值再决定发不发。这一步的保护对象很具体，且有两个方向相反的实例：GLM 上 guard 的 `attempt > 0` 强制关思考（`mod.rs:1101`）若照发 `disabled` 会每次吃 400，把"重试一次"变成"整回合失败"；Ollama 上它发的是被忽略的 `chat_template_kwargs`，于是"输出被思考吃满"这一最常见形态的重试**完全无效**、白烧一次。所以关思考必须走**该端点被实测有效的方言**（Ollama = `reasoning_effort:"none"`）。另一条硬要求：`content` 为空但 `finish_reason=length` 时**不得判退化**——那是输出预算问题，正解是调大上限或给思考设预算（本地 llama.cpp 的 `--reasoning-budget` 即此思路），而不是掐掉重来。

**D4 探测只填默认值，且只读。**
`Provider::detect_thinking()` 与 `detect_context_size` 同构（`provider/mod.rs:203`），**懒执行、不进启动路径**（沿用 `channels/cli.rs:632` 的教训）：llama.cpp 读 `/props?model=` 的 caps 位、Ollama 读 `/api/show` 的 capabilities、云端一律不探（花真 token、未必幂等、忽略未知键的探不出来）。`llaia doctor` 与 WebUI Doctor 加一行「思考能力：configured / detected / unknown（按 unknown 处理）」，与 context_size 三态同形。

**D5 期望态 ≠ 生效态，必须可见。**
命令按能力求值回显三态（生效 / 该模型不支持 / 该通路被忽略）；生效档位另在**尾部** Runtime Context 注入（`status_bar` 之后那一区，`context.rs:57-98`）。硬约束：不进 `system_prompt_base`（缓存前缀字节稳定是 KV 前提）。

**D6 留存先行，且它比"回传"更值钱。**
留存 = provider 收集思考文本 → `ChatMessage.reasoning_content` → `append_message` 写进已有的列 → WebUI 渲染。这一步**不改任何出站请求**，零协议风险，且同时兑现「记进 session」「UI 可看」「为回传备好原料」三件事。实现面比预想小：`chat()` 是 `chat_stream` 的折叠（`openai_compat.rs:207-229`），所以**只需给 `StreamEvent` 加 `ReasoningDelta` 并在 SSE 循环处收集**，两条通路自动覆盖；`ChatResponse`/`append_message` 各加一参。
回传（`preserve`）单独开一档，默认关，红线是**逐字**：本轮没有思考就保持字段缺失，框架**绝不**补空或代写（GLM/DeepSeek 文档与 sensenova 的 400 措辞同向）。

**D7 guard 分流。**
`off_wire=unsupported` 或 `unknown` 时，退化重试不再"假装关思考"，改走「追加 `[guard]` 提示 + 收紧 `guard_thinking_cap`」。另记一个相邻缺陷：modelscope 那类**200 + 零内容**会被空输出判定当成退化，重试又被端点再次吞掉 → 白烧重试并抛误导性诊断。区分"端点静默空"与"模型退化"不属本 plan，登记为后续项。

## 实现分期

**P0 · 留存（新序位，用户诉求的主体）**

1. `StreamEvent::ReasoningDelta(String)`；`openai_compat.rs:420-455` 在丢弃分支改为边计数边收集；`ChatResponse.reasoning: Option<String>`；`chat()` 折叠处同步。
2. `ChatMessage.reasoning_content: Option<String>`（`skip_serializing_if`），`context.push` 与 `append_message`（`sqlite.rs:618` 扩到五列）带上。
3. `reasoning_to_content=true` 的旧行为保持兼容（折进 content 时不再重复存 reasoning，二选一，避免同一段文本双份吃预算）。
4. 预算：`Context::estimate_tokens` 计入该字段（现不含 tool definitions，教训同型）；`hard_truncate_to_fit`/`drop_oldest_unit`（`context.rs:142-190`）丢 assistant(tool_calls) 时连思考一起丢，不留孤儿；`cheap_normalize`（`:298`）**不得**截断/改写 reasoning。
5. WebUI：历史接口带出该字段并按「思考」样式渲染（与 `/btw` 的 Side 样式同族）。注意改 `src/web/static/*` 必须重编（rust-embed）。

**P1 · 档位与生效可见（零协议风险）**

6. `ChatRequest.disable_thinking: bool` → `thinking: Option<ThinkingIntent>`（`Auto`/`None`/`Level`），`ModelConfig` 加 `thinking: Option<ThinkingConfig>`（serde 方向照 `enabled` 教训：**None 时省略**，否则 provider 子树 replace 会把声明静默删掉）。
7. `/reasoning` 按能力回显三态 + 小写归一；`/provider` 切换时重估并提示；尾部注入生效档位。
8. guard 重试按 `off_wire` 分流（D7）。
9. sidecar 单发调用（`context.rs:280`、`memory/trim.rs:49`、`memory/markdown.rs:138`、`slash.rs:1008`、`web/mod.rs:2488`，现全为 `disable_thinking:false`；只 `reminder.rs:129` 是 true）统一带 `None` 意图——能关的省一大段时间，关不动的被 P1-7/门禁短路。
10. 测试：能力矩阵单测（`unsupported` 时注入被短路）、回显文案快照、`put_config` 后能力即时生效（`put_config` 不走 `Config::load`，WebUI 侧须重算）。

**P2 · 回传（`preserve`）**

11. 出站序列化仅在 `preserve=true` 时带 `reasoning_content`。**前置已在「本地新 build 开关面」一节解决**：远端 b10645 实测该字段被丢弃（控制组 prompt 30 vs 30），而新 build 10867 有 `--reasoning-preserve`（默认 enabled）。两条可走的路——(a) 查这版 llama.cpp（b10645）是否已有透传开关或更新版本行为；(b) 走模板的另一兜底分支 `content.split('</think>')`，即把思考按 "<think>...</think>回答" 的形态内联进 content，这条确定会被渲染，代价是可见文本与 token 计费里混入思考。后者正是 ADR-0026 把 `reasoning_to_content` 默认值改成 false 时否定的形态——当时判它"把思考混进回答"，现在要回看：对某些端点那可能不是显示偏好而是**功能必需**。GLM/DeepSeek 侧回传不报错，但生效未证。
12. 前端 Config 页 model 行加 thinking 面板（`app.js:700-712` 的 compat 归一化旁边）：档位 `<select>` + 显式 `:value`，bool 走 checkbox（compat 面板已踩过字符串化坑）。

## 不可破坏的性质

1. 存量配置零回归：不写 `[thinking]` 段时不落盘、请求体不发任何新键（`auto` 与今天完全同字节）。
2. 能力门禁在注入之前（否则 guard 自动重试给必 400 的端点投禁用参数）。
3. 探测绝不触发模型换入/卸载，且不在启动路径。
4. 生效态走尾部注入，不进 system 前缀。
5. 回传内容逐字，缺就缺，框架不生成不摘要。
6. `none` 不静默降级为最低档。
7. 留存不得成为回传的隐含前提之外的第二份上下文负担——折回 content 与独立存 reasoning 二选一。

## 明确否决

- **pi-ai 式全矩阵**（7 档 × 11 方言 + per-model 全量 pin）：实测五家只用 3 种表达，且主力本地模型没有可信档位。
- **按模型名 substring 猜方言**：只当默认值（agnes 双向证伪：文档写的方言无效、文档没写的方言有效）。
- **云端启动期探测** / **靠单发 reason_len 判档位**：前者花真 token 且不幂等，后者被采样抖动支配。
- **新增 `/think` 命令**：命令面已够挤，`/reasoning` 保名改语义。

## 待实测（P2 与部分 P1 的前置）

1. ~~tokenrouter 是否转发 GLM 的 `thinking` / `reasoning_effort`~~ → **已验**：校验 `thinking.type`（400），**不校验** `reasoning_effort`（bogus 也 200）。
2. ~~GLM `disabled` 是 400 还是忽略~~ → **已验**：400，D3 是必须项。
3. ~~agnes 默认态与有效方言~~ → **已验**：默认不思考，唯一有效方言 `reasoning_effort`（`none|low|medium|high|max`）。
4. ~~`qwen-3.8-27b` caps~~ → **已验**：`preserve_reasoning:true`、`reasoning_effort:true`（但档位待重复采样确认）。
4b. ~~ornith-1.5-35b 是否与 qwen-3.6-35b 一致~~ → **已验**：caps 与模板形态逐项一致（effort 位均 false），方言行为一致 → 本地能力面由 server+GGUF 决定，跨家族稳定。
5. **未决**：bigmodel 官方域名上 GLM-5.3 的 `reasoning_effort` 是否真降思考量（需固定 seed + N 次中位数）；`glm-5.3-flash` 的取值/默认与 `clear_thinking` 在 5.3 系的语义（目前从 5.1/5.2 页推断）。
6. ~~llama.cpp 有无透传开关~~ → **部分已答**：新 build 10867 有 `--reasoning-preserve`（默认 enabled）与 `--reasoning-format` 三值，见「本地新 build 开关面」节。**仍待验**：正在跑的远端 router 是 b10645，是否已有这些开关（本机读不到它的 binary）；以及 per-request 是否有 `reasoning_budget`/`reasoning_effort` 对等字段（caps `supports_reasoning_effort` 暗示有，但单发判不了）。
7. **未决**：modelscope 的「200 + 空内容」是限流形态还是模型名失效（`DeepSeek-V4-Flash-0731`），决定 guard 空输出判定要不要先做一次连通性复核。

## 文档面同步

`AGENTS.md`（Provider 小节加「思考能力」段 + compat 表补 thinking 字段）、`docs/adr/0026-provider-compat.md`（追加修订节，含本 plan 的实测表）、`docs/guide/slash-commands.md:19`（改语义 + 写明「进程级、跨频道共享、重启失效」——现文案「仅当前会话有效」会被读成 per-session）、`docs/guide/configuration.md`（`[provider.<id>.<model>.thinking]`）、`src/commands/mod.rs` 的 `CONFIG_TEMPLATE` 注释、`docs/glossary.md`（档位 / 生效态 / 留存 / 回传）、`docs/CHANGELOG.md`（先查有无既有条目）。
