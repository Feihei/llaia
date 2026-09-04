# First-run Bootstrap：首次启动引导 agent 采集并落盘 SOUL / USER

状态：已定案（2026-09-04，方案 B；无新常驻文件 / 无新配置项 / 无新工具）
日期：2026-09-04

## 背景与问题

LLAIA 定位是单用户私人助理，但**全新安装的 agent 既不知道用户是谁，也没有任何机制去学习**。

`init_scaffold` 与 `build_single_agent` 里的 `ensure_template` 写的是占位符模板
（`src/memory/markdown.rs:28-56`）：`SOUL.md` 是 `<Describe LLAIA's personality>` /
`<conversation style>`，`USER.md` 是空 `- name:` 与默认的 `language: Chinese`。随后
`build_single_agent` 把三个文件原样拼进 `system_prompt_base`（`src/channels/cli.rs:585-588`）。

关键缺口：**没有任何一行提示告诉 agent"这些还是待填空的模板，你去问用户"**。所以

1. agent 不会主动问，用户也不知道可以问；`/remember` 与 `memory_write` 都只写 MEMORY.md，
   画像文件不在任何写入路径上；
2. 现状的"引导"是文档里一句人肉编辑——`docs/guide/memory-and-context.md:11-12` 把
   SOUL.md 的维护者写成"你（初始化模板生成，可自由改）"。这与"私人助理"的前提不符；
3. 唯一声明回复语言的 `USER.md` `language:` 字段一直是模板默认值。

### 顺带发现的存量 bug

Tail Reminder 的门禁是 `if !soul.is_empty() || !user.is_empty()`
（`src/agent/mod.rs:1013`）。模板文本**永远非空**，于是全新 agent 的第一个回合就会跑一次
隔离 LLM turn，让模型从 `<Describe LLAIA's personality>` 里"提炼抗漂移要点"——纯浪费，
且 `reminder.md` 里落的是无意义内容。修复需要同一个"仍是模板 = 未填写"的判定，故并入本 plan。

### 现有可复用机制（设计依据）

| 机制 | 位置 | 复用方式 |
|---|---|---|
| 尾部 Runtime Context 注入（状态栏 / todo / env / task / reminder 逐条 `ChatMessage::user` 追加在 history 之后，**不进 system 前缀**，KV 缓存友好） | `src/agent/context.rs:51-88` | bootstrap 文案走同一槽位族，逐轮现算、不落 history/sqlite |
| SOUL+USER 每回合起点读取（reminder 刷新时已读入内存） | `src/agent/mod.rs:1010-1027` | 同一次读取直接复用，**零额外文件 I/O** |
| 带前缀标记的注入消息（`[steer]` / `[guard]` / `[Previous conversation summary]`） | `src/agent/mod.rs:1066` 等 | 新前缀 `[bootstrap]` 沿用同一约定 |
| 文件工具作用域 | `src/tools/file.rs:160-183` + `src/channels/cli.rs:356-359` | 启动时 `workspace_root == workspace`（家目录），`file_edit("USER.md")` 相对路径**本就可写**，无需扩作用域 |

## 目标 / 非目标

**目标**

1. 首次启动时，agent 主动、**一次性**地问用户补齐人格与画像，并把答案写回 `SOUL.md` / `USER.md`；
2. 自我终止：文件一旦填写，提示自然消失，不需要任何新的状态位或斜杠命令；
3. 跨频道一致（WebUI / CLI / QQ / Telegram 同一注入点，因为都在 `run_turn` 起点）；
4. 零 LLM 额外开销、零新配置项、零新工具、零常驻上下文膨胀；
5. 修掉占位符误触发 Tail Reminder 的存量 bug。

**非目标**

- 不做 WebUI 首启 onboarding 向导（表单采集画像要新增一套 UI 与写盘路径，且只覆盖 web 入口）；
- 不新增 `BOOTSTRAP.md` 之类常驻文件（内容与 USER.md 重叠，且填完后仍在上下文里）；
- 不给 `memory_write` 加 `target = soul|user` 参数，也不给文件工具加"家目录恒可写"例外
  ——见下方「方案 A/C 评估」，两条都为极窄边界付代价；
- 不加 `[runtime] bootstrap` 开关（无具体用例；用户故意保留模板时，提示里已含"拒答即写一行说明"的消解路径）；
- 不改子 agent 的 USER.md 复制时机（现为主 agent 内容在 `build_single_agent` 时快照，属存量行为）。

## 方案评估

| 提案 | 结论 | 要点 |
|---|---|---|
| A. 常驻 `BOOTSTRAP.md` 第四份文件 | ❌ 否决 | 多一个持久化对象与注入段，填完后仍是死重；内容与 USER.md 重叠 |
| B. 模板指纹检测 + 尾部一次性注入 | ✅ 采纳 | 零新文件/工具/配置；判定纯字符串比较；靠"写盘即改指纹"自然终止 |
| C. `llaia init` 终端交互式采集 | ❌ 否决 | 唯一真零 LLM 方案，但与主路径冲突：`quick-start.md:7` 明确全新机器直接 `serve` 无需 init，且首启 config 全注释、没有 provider；还漏掉 QQ/Telegram 入口 |

### B 的四个设计细节（逐条定案）

**① 判定：什么叫"尚未填写"。** 字符串比较而非 md5——模板常量已在内存（`SOUL_TEMPLATE` /
`USER_TEMPLATE` 是 `pub const`），`content.trim() == template.trim()` 即可，成本低于求哈希。
空文件（`read_to_string` 失败 → `unwrap_or_default()`）同样判为未填写。逐文件独立判定：
只填了 SOUL 时提示只催 USER。

> reminder 侧沿用 md5 是因为它要比**任意两次内容差异**并做缓存键；这里只比**一个已知常量**，
> 同一次判定不需要哈希。

**② 注入位置：`Context.bootstrap`，排在 `reminder` 之后（最后一条消息）。** 绝不进
`system_prompt_base`——那个值由 `init_system_meta` 缓存、全频道共享、skill 热重载时复用
（`src/channels/cli.rs:651`），且 system 前缀逐轮字节一致是 KV 缓存命中的前提
（`context.rs:48-50` 注释已写明）。bootstrap 生命周期只有几个回合，放在尾部最经济：
出现与消失都只动尾部，前缀不受影响。

**③ 反唠叨：靠模型自检历史，不设回合数阈值。** 提示逐轮注入直到文件被填写。若只投"第一
回合"（`history.len() <= 1`），用户回完答案的那一轮已经没有提示了，agent 可能问完就忘、
不写盘——那是更糟的失败模式。改为在文案里给两条自约束：本轮**把问题附在回复末尾**（不打断
用户真实请求）、**若对话中已经问过或用户明确拒答则不再重复**。t=0 的对话极短，小模型也能
可靠地做这个自检。同时给出拒答的消解路径：往 USER.md 写一行"用户暂不填写"——一次写入即永久
终止提示，不需要框架侧状态位。

**④ 写盘：直接用 `file_edit`，不加任何通路。** 常规路径（首启、未 `/move`）本就可写。
唯一边界是用户在 agent 填写画像前先 `/move` 到别的项目目录：此时家目录既不在
`workspace_root` 也不在受信集合（`src/agent/mod.rs:303-316`，`trusted_dirs` 初始为空、
`/move` 只登记目标目录），写 USER.md 会被拒。该边界**自愈**——`workspace_root` 每次启动由
`derive_workspace` 重推（`cli.rs:356`），`/move` 不持久化，重启或 `/move home`
（`slash.rs:143-160`）即恢复。代价换取是不引入"家目录恒可写"这一层作用域例外，符合最小实现。
文案里补一句降级指引即可。

## 实现分解

| # | 改动 | 文件 |
|---|---|---|
| T1 | `is_unfilled(content, template) -> bool`：空或等于模板原文判为未填写 | `src/memory/markdown.rs` + `src/memory/mod.rs` re-export |
| T2 | `Context.bootstrap: Option<String>`，`to_messages` 中排在 reminder 之后 | `src/agent/context.rs` |
| T3 | `bootstrap_note(soul_unfilled, user_unfilled) -> Option<String>` 构造 `[bootstrap]` 文案；`should_generate_reminder(soul_filled, user_filled) -> bool` 门禁纯函数 | 新增 `src/agent/bootstrap.rs` |
| T4 | 回合起点接线：复用已读的 soul/user 求两布尔 → 设 `context.bootstrap`；把 reminder 门禁换成 T3 纯函数 | `src/agent/mod.rs:1008-1027` |
| T5 | 测试：判定单测（模板/填写/空/带空白差异）、注入顺序、门禁、`context.bootstrap` 随文件填写消失 | 各文件 `#[cfg(test)]` |
| T6 | 文档：agents.md 持久化段 + FAQ/guide（画像如何被填） | `AGENTS.md`、`docs/guide/memory-and-context.md`、`docs/guide/quick-start.md` |

> 无新增 `[runtime]` key，故不触发"四处同步"约定（config.rs / CONFIG_TEMPLATE / configuration.md / AGENTS.md）。

### `[bootstrap]` 文案要素

- 点名是哪一份文件未填（SOUL / USER / 两者）；
- 一次性提问、**问题附在本轮回复末尾**、用用户书写语言、至多 5 问；
- 答案到手后用 `file_edit` 写相对路径 `SOUL.md` / `USER.md`，保留原有小节标题；
- 已问过 / 用户拒答 → 不再重复，拒答时写一行偏好进 USER.md 以终止提示；
- 写盘被作用域拒绝 → 提示用户 `/move home`。

## 测试

- **单元**：`is_unfilled` 对「模板原文」「模板 + 首尾空白」「已填写」「空串」四态；
  `should_generate_reminder` 对「双模板」「双空」「单侧已填」「双侧已填」；
  `to_messages` 断言 bootstrap 是最后一条（镜像 `test_reminder_injected_last`，`context.rs:285`）。
- **回合级**：用 `make_agent_with_rounds_seen`（`mod.rs:1734`，其 `workspace` 指向不存在目录 →
  soul/user 读成空 → 判为未填写）跑一轮，断言 `agent.context.bootstrap.is_some()`；
  随后写入非模板内容再跑一轮，断言变 `None`。
- **回归面**：`/tmp/llaia-test/workspace` 不存在的既有测试会普遍见到 bootstrap 注入，需确认
  无测试断言尾部消息条数或 `estimate_tokens` 精确值（预期只有 `context.rs` 内部测试涉及条数，
  已在其自身构造里显式设字段）。
- **质量门**：`cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` → `cargo test`。
  跑 test 前须停掉本机运行中的 llaia 实例，否则 `target\debug\llaia.exe` 被锁报 os error 5。

## 风险与缓解

| 风险 | 缓解 |
|---|---|
| 小模型把提问当成主要任务、打断用户真实请求 | 文案显式要求"先答用户的事、问题附在末尾"，且只问一次 |
| 用户永远不答 → 提示每轮都在 | 拒答路径写一行进 USER.md 即指纹改变、提示永久消失 |
| 用户故意保持通用助手（无画像） | 同上：一句"暂不填写"就完成消解，无需改代码或加开关 |
| 填写过程中 `/move` 导致写盘失败 | 工具返回错误 → agent 按文案指引提示 `/move home`；重启亦自愈 |
| Tail Reminder 门禁收紧后少生成一次 | 只影响"双模板"这一空转场景；任一侧已填写即照常生成 |
