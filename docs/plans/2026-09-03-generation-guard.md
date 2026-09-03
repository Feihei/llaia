# Generation Guard：输出退化防护（重复检测 / 思考限长 / 截断重试 / 熔断）

状态：已定案（2026-09-03，P0-P2 一条龙；阈值采纳推荐值；退化输出不落库）
日期：2026-09-03

## 背景与问题

### 事故复盘

编码任务中使用本地小 MoE 模型（35B-A3B），上下文过长时模型退化：一直重复输出思考内容，始终没有产出正式回答，也没有自行恢复。整轮空转直到被手动终止，会话记录被垃圾撑得很长（已手动删除整个会话）。推理侧（llama.cpp 采样参数）已做调整，但**框架层面对这类失败模式零防御**。

### 现有防线为什么全部错过

逐条对照代码，四层现有保障没有一层能拦住"退化重复"：

| 现有机制 | 位置 | 为什么拦不住 |
|---|---|---|
| 流空闲超时 `STREAM_CHUNK_TIMEOUT_SECS = 120s` | `src/provider/openai_compat.rs:16` | 只防"完全没数据"。退化时模型持续吐 token，计时器不断被重置，永不触发 |
| 单轮时长上限 `max_turn_duration_secs`（默认 3600s） | `src/config.rs:77` + `src/agent/sink.rs` run_turn | 兜底太晚：要空转满 1 小时才掐，且只能掐掉整轮，无重试 |
| fallback 降级链 | `src/provider/fallback.rs:57-76` | 只在"首个事件即 Error"（请求失败）时降级。退化生成在协议上是**成功完成**的流，不触发 |
| think 标签剥离 | `src/tool_call/stream_parser.rs:156-170` | `InThink` 状态**静默丢弃**思考内容（只保留闭标签长度的尾部做检测），框架完全不知道思考流有多长、是否在重复 |

关键盲区有两个：

1. **思考流对框架不可见**。标签模式下 `InThink` 直接丢弃；native 模式下 `reasoning_to_content = false`（llamacpp 预设默认，`src/provider/compat.rs`）时 `reasoning_content` 在 provider SSE 循环里被直接跳过（`src/provider/openai_compat.rs:386-398`）。本次事故恰好是思考流退化——用户看到的是画面冻结、零可见输出，token 空转。
2. **没有任何重复性检测**。已有的重复检测先例是工具调用层面的（`src/agent/mod.rs:1186-1204`，连续 3 次相同 (name, args) 注入警告），文本生成层面完全没有对应物。

### 主路径现状（设计依据）

- 主聊天路径**已是流式**：`src/agent/mod.rs:1080-1149` 消费 `provider.chat_stream`，逐事件处理。检测与中途 abort 有天然的挂载点。
- 用户中止路径（`event_tx.is_closed()`，mod.rs:1089-1100）已验证"中途断流 + 保存部分输出"的模式；guard 的 abort 复用同一形态（`break` 消费循环即 `drop(stream)`，HTTP 连接关闭，本地服务端通常随之停止生成）。
- `ChatRequest.disable_thinking`（`src/provider/mod.rs:155-159`）已贯通到 provider 层注入 `chat_template_kwargs: {enable_thinking: false}`（llama.cpp 等），重试时强制关思考是现成能力。
- `[steer]` 注入（mod.rs:1048-1056）确立了"带前缀标记的消息持久化进 context + sqlite"的先例，重试提示语沿用此模式。

## 目标 / 非目标

**目标**：框架层对"生成退化"（重复循环、思考流失控、空输出）做到——

1. 早发现：流式滑动窗口重复检测 + 思考流长度上限；
2. 早止损：判定退化即中途 abort，不再烧 token；
3. 自愈：丢弃退化输出，带提示 + 关闭思考重试一次（退化有随机性，重试命中率高）；
4. 熔断：连续多轮退化则报警终止，明确提示去调推理配置，而不是无限等。

**非目标**：

- 不碰推理引擎采样参数（repetition penalty 等，推理侧的事）；
- 不引入 tokenizer——检测一律**字符级**，引擎无关（中文 1 字符≈1 token，英文 ≈4 字符/token，阈值按字符标定）；
- 不做退化后自动切 fallback 模型（尊重"报警让人去调配置"的决定，列为未来可选扩展）；
- 不做 WebUI 思考内容展示（独立话题）。

## 四项提案评估（结论先行）

| 提案 | 结论 | 要点 |
|---|---|---|
| ① 流式 n-gram 重复检测 + abort | ✅ 采纳 | 主路径已流式，可行；须分**可见文本**与**思考流**两条检测线（本次事故在思考线）；字符级 n-gram |
| ② 截断 + 重试 | ✅ 采纳 | 重试落在 **agent 迭代层**而非 provider 包装层（要改 messages、要 `disable_thinking`、要熔断计数）；重试强制 `disable_thinking = true` |
| ③ 思考长度硬限制 | ✅ 采纳 | 本次事故的最小直接修复：parser 加计数器即可，成本极低；作为重复检测的兜底 |
| ④ 异常计数熔断 | ✅ 采纳 | 只报警不拒服（退化有随机性，下轮任务可能正常）；正常完成即清零 |

### ① 流式重复检测

- 算法基线：**滑动窗口字符 n-gram 计数**。窗口内任一 n-gram 出现次数 ≥ 阈值即判退化。用户原始参数（近 200 token / 20-gram / 3 次）换算为字符制并留安全边际后的默认值见配置一节。
- **误报风险正面处理**：编码助手的合法输出天然含重复（重复代码行、表格、测试断言）。单个 24 字符行出现 3 次在测试代码里并不罕见。缓解：阈值取保守值；触发时 tracing 记录命中的 gram 与窗口快照，便于按机型/模型调参；`output_guard` 总开关一键关闭。
- **检测两条线**：
  - 可见文本线：parser 输出的 `user_text`（进 `iter_text` 的内容）；
  - 思考线：`InThink` 丢弃的内容 + native `reasoning_content`——本次事故的形态，目前完全不可见，是本次改造的重点。

### ② 截断 + 重试

- **挂载点在 agent 迭代层**（`src/agent/mod.rs` 的 `for i in 0..max_iters` 循环内），不做 provider 包装。理由：重试需要追加提示消息、强制 `disable_thinking`、累计熔断计数、向用户发通知——全是 agent 层职责；且 `FallbackProvider` 的语义是"请求失败降级"，与"内容退化重试"是两回事，不应混用。
- **退化产物的落库决策：不进 sqlite、不进 context**。理由：
  - 重试紧跟同一迭代发生，退化片段若进 context 会出现在重试请求里，诱导模型自我模仿（退化本身就是自我模仿的产物）；
  - 现有 Error 路径保存部分输出（mod.rs:1130-1140）是因为**没有重试**、turn 就此结束，模型需要知道自己说过什么；guard 路径重试后会有正式回复顶替，语义不同；
  - 事故中用户手动删了整个会话——垃圾留底没有价值。
  - 例外：用户中止（tx closed）与真正的 Error 仍走现有保存路径，不受 guard 影响。
- **重试请求**：同一 `messages` 追加一条 user 消息（英文、与现有内部提示语一致）：
  `[guard] Your previous response was discarded because it degenerated into repetition. Answer again, directly and concisely. Do not repeat content you have already produced.`
  持久化进 sqlite + context，前缀标记同 `[steer]` 模式，会话日志可追溯（WebUI 已有单条删除能力，用户可事后清理）。
- **重试次数默认 1**（共 2 次尝试）。退化有随机性，一次重试命中率高；对彻底坏掉的模型/参数组合，多重试只是多烧几分钟。可配。
- 若 abort 时已有 native `ToolCall` 事件到达：无条件丢弃重试——工具尚未执行，无副作用，语义干净。

### ③ 思考长度硬限制

- 本次事故的最小直接修复：`InThink` 状态本来就逐字符处理，加一个计数器零额外分配。
- 超过 `guard_thinking_cap` → 判退化 → 走 ② 的重试路径（重试带 `disable_thinking`，从根上掐掉思考）。**不做**"截断后放行后续文本"——模型语义上还在未闭合的思考里，放行只会得到语无伦次的可见文本；abort + 关思考重试更干净。
- cap 是兜底（思考重复检测通常会先命中），默认值给得相对宽松，避免误伤长思考的合法编码任务。
- 空输出判定（同属此线）：流正常结束但 `iter_text` 空且无工具调用——无论思考流是被标签剥离还是被 provider 丢弃，这都是退化残余形态（思考到天荒地老然后什么都没产出），同样触发重试。空回复本身就是坏的，无需区分原因。

### ④ 异常计数熔断

- **迭代层**：重试耗尽仍退化 → 本回合以诊断消息终止（用户可见 + tracing::warn），文案明确指向行动：怀疑当前模型/量化/参数撑不住此上下文，建议调整推理配置（重复惩罚、上下文长度）或用 `/provider` 换模型。
- **回合层**：Agent 持有 `guard_streak` 计数——连续以退化收尾的回合数；任一回合正常完成即清零。达到 `guard_breaker_threshold` 时在回合结束的提示里附加醒目警告。
- **只报警、不拒服**：退化有随机性且任务逐轮变化，直接拒绝下轮请求过于激进。用户诉求即"报警让我去调配置，而不是无限等"。

## 总体设计

### 数据流

```
provider.chat_stream
   │
   ├─ TextDelta ──► parser ─┬─► user_text ──► iter_text ──► 可见线 RepetitionDetector ─┐
   │                        └─► InThink 丢弃 ──► think_chars 计数 + 思考线检测 ─────────┤
   │                                                                                     ├─► 判退化？
   ├─ reasoning_content（reasoning_to_content=false 时 provider 计数，超 cap 截流）──────┤    │
   │                                                                                     │    ▼
   └─ Done ──► 空输出判定（iter_text 空 && 无 calls）───────────────────────────────────┘  abort（drop stream）
                                                                                              │
                                                              重试 ≤ guard_max_retries 次 ◄───┘
                                                              （+[guard] 提示, disable_thinking）
                                                                        │ 仍退化
                                                                        ▼
                                                              熔断：诊断消息 + streak+=1 + warn 日志
```

### 新模块：`src/agent/guard.rs`

```rust
/// 来自 [runtime] 的 guard 配置快照
pub struct GuardConfig {
    pub enabled: bool,           // 总开关，默认 true
    pub repeat_window: usize,    // 滑动窗口（字符）
    pub repeat_gram: usize,      // n-gram 长度（字符）
    pub repeat_threshold: u32,   // 窗口内同一 gram 最大出现次数
    pub thinking_cap: usize,     // 思考线字符上限，0 = 不限
    pub max_retries: u32,        // 判退化后重试次数
    pub breaker_threshold: u32,  // 连续退化回合报警阈值
}

/// 滑动窗口重复检测器：纯算法结构，可见线/思考线各持一个实例
pub struct RepetitionDetector { window, gram, threshold, ring buffer }

impl RepetitionDetector {
    pub fn feed(&mut self, text: &str);   // 追加文本
    pub fn is_degenerate(&self) -> bool;  // 窗口内任一 gram 出现 ≥ threshold
    pub fn snapshot(&self) -> String;     // 触发时日志用：窗口内容摘要
}
```

- 实现取舍：窗口 ≤ 1024 字符量级，`feed` 每累计 32 字符重建一次窗口内 gram 计数（`HashMap` ≤ 1000 条），开销纳秒级，无需滚动哈希之类的优化。
- `enabled = false` 或 `repeat_threshold = 0` 时 `feed` 直接短路，零开销。
- guard 关闭时 parser 的计数器也只是整数加法，无感知。

### 模块改动清单

**`src/tool_call/stream_parser.rs`**

- `InThink` 状态增加累计计数；内嵌一个思考线 `RepetitionDetector`（guard 关闭时为空操作实例）。
- 新增查询方法：`think_chars(&self) -> usize`、`think_degenerate(&self) -> bool`。
- 不改 `feed` 的对外契约（仍返回应给用户的文本增量），不泄漏任何思考内容。

**`src/agent/mod.rs`**

- 把 1080-1149 的流消费抽成辅助函数（返回枚举）：

```rust
enum IterOutcome {
    Done { text: String, calls: Vec<ToolCall>, usage: Option<Usage> },
    Degenerate { reason: String, visible_chars: usize },
}
```

- 消费循环内：每个 `TextDelta` 的可见文本喂可见线检测器；每个事件后检查 `parser.think_chars()` / `parser.think_degenerate()` / 可见线判定；命中即 `break`（drop stream）返回 `Degenerate`。保留现有 `event_tx.is_closed()` 中止检查（重试期间用户中止仍生效，走现有保存路径）。
- 迭代外层套重试循环 `for attempt in 0..=max_retries`：
  - `attempt > 0`：请求 messages 末尾追加 `[guard]` 提示（持久化），`disable_thinking = true`；
  - 首次重试前向 `event_tx` 发一条 `TurnEvent::Chunk`：`\n\n[检测到输出重复/思考失控，已中止并重新生成…]\n`，让已看到部分垃圾的用户知道发生了什么；
  - 全部耗尽 → 熔断路径：诊断消息作为本回合收尾（进 sqlite，用户可见），`guard_streak += 1`，`tracing::warn`。
- `Agent` 结构体新增 `guard: GuardConfig`（构造时从 `config.runtime` 快照，对齐 `tool_result_cap` 的做法，mod.rs:257-258）与 `guard_streak: u32`。
- `reload_runtime`（mod.rs:467）热加载 guard 配置。

**`src/provider/openai_compat.rs`**

- `reasoning_to_content = false` 分支：对 `reasoning_content` / `thinking` 计数，超过 `thinking_cap` 后停止解析、直接 `yield StreamEvent::Done` 截流（相当于"思考被截断"），`tracing::warn` 记录。后续由 agent 层"空输出判定"接住触发重试。
- 选"截流 + Done"而非新增 `StreamEvent` 变体：**不改 `Provider` trait**，anthropic/gemini/所有测试 mock 零改动。
- `thinking_cap` 传递路径：`provider_from_ref(config, …)` 已持有完整 `Config`（`src/provider/mod.rs:218`），`OpenAiCompatibleProvider::new` 增加参数即可，影响面可控。
- `reasoning_to_content = true` 的场景不用管：思考折进 `TextDelta`，天然被可见线检测器 + 空输出判定覆盖。

**`src/config.rs`**

```toml
[runtime]
output_guard = true             # 总开关
guard_repeat_window = 512       # 滑动窗口（字符）
guard_repeat_gram = 24          # n-gram 长度（字符）
guard_repeat_threshold = 4      # 窗口内同一 gram 出现次数上限
guard_thinking_cap = 32000      # 思考线字符上限（≈8k token），0 = 不限
guard_max_retries = 1           # 判退化后重试次数
guard_breaker_threshold = 2     # 连续退化回合的报警阈值
```

- 全部 `#[serde(default)]`，存量配置零迁移。
- 默认值说明：
  - `repeat_threshold = 4` 而非用户原始的 3——24 字符 gram 在代码输出里出现 3 次并非不可能（重复行），4 次更稳；配合触发日志再按机型调。
  - `thinking_cap = 32000`：编码任务长思考合法存在，重复检测会先于 cap 命中真正的退化循环，cap 只兜"思考很长但没重复"的极端情况。
  - 用户原始参数（200 token / 20-gram / 3 次）作为文档注释保留换算依据。

**文档**

- `agents.md`：工具集/会话模型相关段落补 guard 机制描述（对齐现有「工具结果防护」的写法）。
- `docs/guide/`：配置参考补 `[runtime]` 新 key。

### 与现有防线的关系

改造后形成四层互补，各管一段：

| 层 | 机制 | 覆盖 |
|---|---|---|
| 连接层 | `STREAM_CHUNK_TIMEOUT_SECS`（120s 无真实数据） | 流挂死 |
| 内容层 | **Generation Guard（本计划）** | 重复退化 / 思考失控 / 空输出 |
| 回合层 | `max_turn_duration_secs`（1h）+ keepalive 心跳 | 一切慢速失控的最终兜底 |
| 请求层 | `FallbackProvider` | 请求失败（首事件 Error） |

### 频道无关性

检测与重试全部在 agent 层，CLI / WebUI / QQ / Telegram / cron / delegate 全频道自动受益。非交互频道收不到通知 Chunk 也无害（`let _ = event_tx.send`）。

## 测试计划

1. **`RepetitionDetector` 单测**：退化文本（循环段落）命中；正常长文本不命中；**真实风格的代码片段不命中**（重复行 ×3 的测试代码用例，守住误报底线）；窗口未满不判定；阈值/窗口边界。
2. **parser 思考统计单测**：think 字符计数准确（跨 chunk）；思考线重复命中；guard 关闭时行为与现状逐字节一致（回归）。
3. **agent 集成测试**（沿用 `src/agent/mod.rs` 现有 mock provider 模式）：
   - mock 流输出重复文本 → 中途断流 + 重试 → 第二次正常 → 回合成功；断言：context 含 `[guard]` 提示与最终回复、**不含**退化文本、用户收到中止通知；
   - 重试全部退化 → 诊断收尾、`guard_streak` 递增；随后一个正常回合 → 清零；
   - think 标签流超 `thinking_cap` → 触发 abort + 重试，且重试请求 `disable_thinking = true`（复用现有记录请求形态的 mock，mod.rs:1495-1518）；
   - 空输出（流只有 Done）→ 重试；
   - `output_guard = false` → 全部行为与现状一致（零回归门）。
4. **provider 截流测试**：`reasoning_content` 累计超 cap → 流以 Done 结束、后续 delta 不再产出。
5. **质量门**（仓库约定）：`cargo fmt --all` → `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`。跑 full test 前先停掉本机运行中的 llaia 实例（二进制锁），期间可用 `cargo test --lib` 过渡。

## 实现顺序

- **P0（本次事故的直接修复）**：parser `think_chars` 计数 + `thinking_cap` 判定 + 空输出判定 + agent 重试框架（`[guard]` 提示 + `disable_thinking`）+ 用户通知。不引入检测器，改动面最小，先止血。
- **P1（检测增强）**：`RepetitionDetector`（guard.rs），接入思考线与可见线，流式中途 abort。
- **P2（熔断与工程化）**：`guard_streak` + 诊断文案 + 热加载 + provider 层 `reasoning_content` 计数截流 + 文档同步。

每步独立编译 + 测试通过、独立可提交。提交只 `git add` 自己改动的文件（仓库里常有其他会话的 WIP，禁用 `-A`）。

## 风险与缓解

| 风险 | 缓解 |
|---|---|
| 代码类合法输出误报（重复行/表格） | 保守默认阈值（4 次）；触发日志记录 gram + 窗口快照便于调参；`output_guard` 一键关；误报代价可控——只是 abort + 重试一次，重试成功照样交付 |
| 重试风暴烧时间 | `guard_max_retries` 默认 1；熔断阈值 2 回合；外层仍有 `max_turn_duration_secs` 兜底 |
| 本地服务端在客户端断连后继续生成 | abort 的价值主要在框架侧（不再等待、不再入库）；llama.cpp server 通常随连接关闭停止；即使个别后端继续跑也不影响正确性 |
| 提示消息污染会话 | `[guard]` 前缀与 `[steer]` 同模式，真实可追溯；WebUI 已有消息删除能力 |
| 思考线检测性能 | 窗口 ≤ 512 字符、每 32 字符检查一次，O(窗口) 重建，纳秒级；guard 关闭时仅剩整数计数 |

## 未来扩展（本期不做）

- 熔断触发且配置了 `fallback` 链时自动升级到下一模型（本期尊重"报警让人调配置"的决定）。
- WebUI 展示思考流（需要 `StreamEvent` 增加 reasoning 变体，独立话题）。
- 逐 agent 的 guard 配置（`[agent.<alias>]` 覆盖 `[runtime]` 默认）——等多模型差异化需求真实出现再加，遵循"没有用例就不写 config key"。
