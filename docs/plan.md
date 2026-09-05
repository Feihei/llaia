# LLAIA 项目 Roadmap

> 本文档是 LLAIA 的**前瞻路线图**：顶部是已交付阶段一览（索引），主体是**近期小修（H 系列）**与下一步计划（P7）。
> 各阶段的**完整交付清单**见 [`CHANGELOG.md`](CHANGELOG.md)；详细实现计划见 [`plans/`](plans/)，设计规格见 [`specs/`](specs/)，架构决策见 [`adr/`](adr/)。

**整体目标**：一个单用户、本地优先的私人 AI 助理，跨 CLI/QQ/Web 等多 channel 接入，主 Agent + 可委派子 Agent 协作，持久化记忆与会话。

---

## 状态图例

- ✅ 已完成
- 🚧 进行中
- ⏳ 计划中（未开始）

> 条目勾选框语义：**`[x]` = 代码已落地**，`[ ]` = 尚有未完部分（含「已定案未实现」「部分交付」）。
> 「已定案/已立项」只是决策完成，不算 `[x]`——须在实现后勾上，并在条目标注交付日期与代码位置。

> **编号约定**（一个编号只表示一件事，2026-09-05 起）：**P*** = 阶段性工程（P1…P7）；**T*** = P7 里 OS/部署级安全防线（T1–T4）；**S*** = 进程内静态分析层（S1–S2，P7 的前置）；**H*** = 近期小修（发版窗口内的低风险止血项）。曾有一版把静态分析三步也写成 T1/T2/T3，与 T1–T4 撞车，已改。

---

## 已交付阶段一览

| 阶段 | 状态 | 一句话目标 | 交付清单 |
|---|---|---|---|
| P1 | ✅ | MVP：CLI 单 channel，REPL + 基础工具 + 持久化 | [CHANGELOG.md](CHANGELOG.md)（§P1） |
| P1.5 | ✅ | QQ channel + 全 channel 流式输出 + 稳定性补丁 | [CHANGELOG.md](CHANGELOG.md)（§P1.5） |
| P2 | ✅ | 子 Agent 委派 + 交互增强 + Web channel | [CHANGELOG.md](CHANGELOG.md)（§P2） |
| P3 | ✅ | 能力扩展与生态接入（边界/init/cron/MCP/Skill） | [CHANGELOG.md](CHANGELOG.md)（§P3） |
| P3+ | ✅ | 交互增强与生态扩展（快赢/Anthropic/Telegram/钉钉/微信） | [CHANGELOG.md](CHANGELOG.md)（§P3+） |
| P4 | ✅ | 基础能力增强（时区/做梦/压缩/权限/shutdown/Gemini/飞书…） | [CHANGELOG.md](CHANGELOG.md)（§P4） |
| P5 | ✅ | Provider Compat / 记忆预算 / 统一搜索 / todo / ask_user / skill 自管 / goal / 剩余项 | [CHANGELOG.md](CHANGELOG.md)（§P5） |
| P6 | ✅ | 稳定性修复 + 快赢 + WebUI 批次 + 任务线/侧问/插话/媒体作用域 + Generation Guard/首运行引导 | [CHANGELOG.md](CHANGELOG.md)（§v0.3.1、§v0.4.0） |
| v0.4.0 | ✅ | P6 全量 + provider/compat 收口，2026-09-04 打 tag 发版 | [CHANGELOG.md](CHANGELOG.md)（§v0.4.0）、[release-notes/v0.4.0.md](release-notes/v0.4.0.md) |
| v0.4.1 | 🚧 | 发版后小版本：画像模板升级 + 框架消息英文化收尾 + todo 孤儿清单 GC（三项已落地，**未打 tag**） | [CHANGELOG.md](CHANGELOG.md)（§v0.4.1） |

> **P6 已全部交付并归档**：原 P6 节的完整勾选清单（WebUI W1/W2/W3、会话主题总结、provider 针对性优化、`memory_research`、启动优化 #11、主干代码体检、#A–#J 新增发现、Generation Guard、First-run Bootstrap 等）已随各项实现陆续迁入 [CHANGELOG.md](CHANGELOG.md) §v0.3.1 / §v0.4.0，本文件不再保留已交付明细。注意 CHANGELOG 里**没有独立的 §v0.3.2**：原按 0.3.2 攒的开发内容跨版本号当作 v0.4.0 发布，段标题已一并重定。v0.4.1 的三项同样已迁入 CHANGELOG，本文件只留索引与下一步。

---

## 近期小修（H 系列，v0.4.1 → v0.4.2 窗口）

**状态**：⏳ 待拍板（起点 2026-09-05）｜与 P7 解耦，每项独立可提交、不阻塞发版

抽出来的理由：这些都不是「阶段性工程」，而是散在 P7 / 主干体检 backlog 里的低成本止血项——原混在前瞻计划里，既不会被顺手做掉，也看不清发版窗口里还剩什么。

- [ ] **H1 · file_edit 失败时不给下一步（+ 零 CRLF 容忍）**（2026-09-05 实锤排查）
      - **先说结论：判定逻辑没坏，四次失败全是模型侧文本不符**。对照 `22302d8^` 的原文逐行验过：阿来改 `todo.rs` 的两次（`tool_calls` 1260/1265）把 `/// 禁用落盘（测试 / 无 workspace 场景）。` 记成了 `/// 禁用落盘的降级实例（测试用）。`；改 `plan.md` 的两次（09-04，calls 1088/1091）引用了一条从没进过仓库正文的行（`git log -S "N+1"` 全历史无命中）。
      - **但工具确实在帮倒忙**：`tools/file.rs:239` 只回 `old_string not found in <path>`，不提示「先 `file_read` 拿准原文再重试」，模型于是拿同一份脑内快照连撞四五次，一次编辑烧掉五个回合。改法：0 命中的错误文本追加一句可执行指引，并回显文件里最接近的一行帮助定位。
      - **潜在坑（本机未复现，Windows 用户会踩）**：匹配是字节级 `content.matches(old)`，零归一化。本仓库工作树恰好纯 LF，但 `core.autocrlf = true` 且**无 `.gitattributes`** → 新克隆到 Windows 得到 CRLF 工作树，模型按 LF 给 `old_string` 必然 0 命中，`file_edit` 直接废掉。改法：0 命中且文件含 `\r\n` 时，把 `old`/`new` 的 `\n` 归一为 `\r\n` 重试一次（命中则按原样写回，不混行尾）。约 20 行 + 2 单测。
- [ ] **H2 · 标签模式下 `` ```json `` 围栏的工具调用不被解析**（`88e96b7` commit message 里记的 follow-up issue，至今未立案）
      - 现象：首运行引导实测中，模型唯一一次尝试写 SOUL/USER 是把工具调用包在 `` ```json … ``` `` 里，静默不执行，用户侧什么都没发生。
      - 根因（生产路径）：`tool_call/stream_parser.rs:63` 的 `FENCE_LANGS = ["tool_call","toolcall","tool-call","invoke"]` 不含 `json` → 不进入 `InFence`，整块按普通文本透传。仅影响 `native_tool_calling = false` 的标签降级模式（本地小模型主路径）。
      - **陷阱（本项最大的坑）**：`tool_call/tag_parser.rs::parse_tool_calls` 里有同一套 tag + fence 规则、还带完整单测，但**全仓库没有任何生产调用点**（`grep` 实锤：只有自身 tests 与 `tool_call/mod.rs:7` 的 re-export）。改它零效果，且两份规则会持续漂移——违反本仓库「不留 `dead_code`，要么接入要么删」的约定。处理方向二选一：非流式路径接入它，或删掉、把有用的用例迁到 `stream_parser.rs`。
      - 改法：`FENCE_LANGS` 加 `"json"`。风险比看上去低——`value_to_tool_call` 本来就要求同时含 `name`(string) 与 `arguments` 才成调用，且 `InFence` 收口时解析失败会把原块当文本吐回（`stream_parser.rs:235-255`），给用户看的 JSON 示例不会被吞。真要再收紧一层，可把已注册工具名集合传进 parser 做白名单校验（签名要动，代价明显）。补两条单测：`` ```json `` 被识别为调用；`` ```json `` 里是普通配置示例时原文保留。
      - 顺手（可选）：`tool_call/prompt.rs` 明确写「工具调用只能用 `<tool_call>` 标签或 `` ```tool_call `` 围栏，不要包在 `` ```json `` 里」。
- [ ] **H3 · 文档一致性欠账**
      - `AGENTS.md:101` 仍写「chat 主路径当前仍整块返回，未启用流式」——过时，主路径早已 `chat_stream` 流式（`agent/mod.rs` 消费侧）；同节其余 v0.4.x 增补（guard/bootstrap）都已同步。
      - 用户文档没跟上 v0.4.1 的画像模板改动（`88e96b7` 只动了 `src/`）：`docs/guide/` 未提 SOUL 默认 `# Name` = LLAIA、USER 的 `language` 改为留空由引导去问、以及启动时自动升级逐字节未改动的旧占位文件（`migrate::refresh_placeholder_templates`）。
      - 本文件自身的编号冲突（**本轮已修**）：P7 的 T1–T4 与 2026-09-05 那条笔记里另套 T1/T2/T3 含义互相矛盾，现已把「静态分析层」改称 S1/S2，见 P7 节末。
- [ ] **H4 · 主干体检 backlog 里的机械项提升**（从下方「遗留 backlog」上提，改动面小、零设计）
      - 常量正则 `unwrap()`（`secrets.rs` / `config.rs` / `approval.rs`）：逻辑上不可 panic，按约定补注释即可。
      - `slash.rs`（background_tasks）与 `sqlite.rs` 全文件约 20 处 `lock().unwrap()`：锁内均同步调用、无 await，正确性无问题，仅与「生产路径不用 unwrap」的约定冲突，机械替换 `unwrap_or_else(|e| e.into_inner())`，diff 大但零风险。
- [ ] **H5 · 入站附件 `uploads/` 无回收通路**（todo GC 的同类项，2026-09-05 顺带查得）：QQ 附件（`channels/qq.rs:872`）与邮件附件（`channels/mail.rs:190`）都落 `<家目录>/uploads/`，全仓库没有任何删除/回收代码，只增不减（本机现 2 个文件 / 220 KB，属还没长起来而不是没问题）。**两条现成通路都不能照搬**：`todos/` 的按会话 uuid GC 在这里无意义（附件不属于会话生命周期，且文件名是 `<msg_id>_<filename>` 不带 uuid），`workspace/tmp/` 的启动期 3 天 mtime 清理又太危险——用户半年前发来的图片可能仍被引用。要先拍板策略（例如「保留全部，只加容量上限 + WebUI 手动清理」或「N 天后移到 `backups/` 而非删除」），再动手。必要性：**低–中**（单用户场景增长慢，但发版前把它记下来比忘掉便宜）。
- [ ] **H6 · v0.4.1 发版动作**：写好 `docs/release-notes/v0.4.1.md`（简短英文 changelog，`release.yml` 的 release-notes job 会据此填 GitHub release body）→ `git tag -a v0.4.1` → push 分支与 tag。发版节奏与「跨版本号需先改 `Cargo.toml` 再打 tag」的既有约定见 AGENTS.md「发版」。

---

## P7 — 下一步计划

**状态**：⏳ 计划中（起点 2026-09-04）

> **v0.4.1 不含 P7 任何子项**（用户 2026-09-05 拍板）：T1/T2 是 OS 级防线，要给 terminal 加运行时依赖、与「轻量、可移植、单 crate」的产品定位正面冲突，动手前必须先出 ADR，整体推到 v0.4.2 之后评估。T3（解释器/内联执行强制审批）不依赖沙箱、只是几十行，本轮同样不单独塞进来——它和静态分析层 S1/S2 的去留要一起在 grill 时定，见本节末「建议实现顺序」。近期窗口能做的都已在 **H 系列**。

### 🛡️ terminal 脚本绕过防护（2026-09-04 立项，待 grill）

**问题**：现有安全模型对「误删大量文件 / 改系统关键位置」的防护建立在**命令行字符串**上——命令黑名单（`path_guard.rs::COMMAND_BLACKLIST`，硬编码）、路径 token 提取（`extract_path_tokens` → `validate_command_paths_in_scope`）、shell 套壳拦截（`check_shell_wrappers`）。但 `python` / `node` / `perl` / `ruby` 等解释器一旦启动，其真正的文件操作发生在解释器内部，框架对子进程的 syscall / 文件系统效果**零感知**。实测三层全部绕过：

- 命令黑名单只做子串匹配，`python -c "..."` 不命中任何条目；
- `check_shell_wrappers` 只拦 `bash/sh/zsh/fish` + `-c` 和 `eval/exec/source/$()/反引号`，`python` 不在 shell 名单、`-c` 非被拦构造；
- 路径校验从命令行抠 token，`python evil.py` 只看到 `evil.py`（workspace 内合法）；即便 `shutil.rmtree('C:/Windows')` 被抠成含 `/` 的 token，`validate_path` 的黑名单是「危险前缀**开头**」匹配，token 实际以 `shutil.rmtree(` 开头 → 漏判。

**结论**：字符串匹配层无法可靠覆盖「执行任意代码」的载荷，往黑名单里堆关键词是补不完的。真正的防线需要从「检测命令」转向「约束进程」。**候选方案（按可靠度 / 成本排序，待 grill 选型）**：

- [ ] **T1 · OS 级沙箱（根治，★★★）**：把 terminal 及子进程关进 jail，让「碰不到系统目录」成为内核强制事实而非事后判断。Linux `bubblewrap`/`firejail` + namespace + `landlock`/`seccomp`（workspace 外只读 bind 或隐藏）；Windows `Windows Sandbox` / AppContainer / 低权限账户 + ACL；重任务可容器化（Docker）。结构性改造，动手前出 ADR。必要性：**高**（唯一能覆盖未知 payload 的方案）。
- [ ] **T2 · 无特权账户运行（最省的强防线，★★☆）**：整个 llaia 进程（含 fork 出的解释器）以专用低权限账户运行，该账户对 workspace 外无写权限——OS 直接 `EACCES`，脚本再聪明也绕不过文件系统权限。缺点：主要挡写/删，挡不住读敏感文件（配合 HOME 隔离 + ACL 缓解）。必要性：**高**，性价比最高。
- [ ] **T3 · 解释器 / 内联执行强制审批（当天可落地的止血，★☆☆）**：既然静态分析 `-c` 与 `.py` 内容不可靠，就不假装能分析，直接把常见解释器首词（`python/python3/node/perl/ruby/php/deno/bun`）与任意 `-c`/`-e` 内联执行标记为高危——命中即强制 `/ok` 审批（或 `deny`）。在 `check_shell_wrappers` 旁加 `check_high_risk_interpreter(command)`，同步 `[tools.terminal]` 可配开关与 CONFIG_TEMPLATE / `docs/guide/configuration.md` / AGENTS.md 四处（新增 runtime/terminal key 的既有约定）。不挡 `python evil.py` 的实际破坏，但把「跑任意代码」升级到人审这一现有唯一能覆盖未知载荷的闸门。必要性：**中**（真防线归 T1/T2，本项是当下无沙箱环境里的务实收敛）。
- [ ] **T4 · 缩小爆炸半径（兜底，★☆☆）**：workspace git 跟踪 / 定期备份（误删可回滚）；terminal 默认 `read-only` 权限档、需要写时临时提档；考虑 terminal 断网（多数破坏脚本先下载载荷）。多为运维/配置约定而非进程内逻辑。必要性：**低–中**。

> 定案方向预判（2026-09-04 原议）：T3 作为**近期代码改动**先行，T1/T2 作为**部署规范**写进文档（安全/权限相关 guide），T4 作为推荐实践。是否引入 OS 沙箱取决于「是否愿意给 terminal 加运行时依赖」——需与「轻量、可移植、单 crate」的产品定位一并权衡。
>
> **2026-09-05 修订**：用户拍板 v0.4.1 不动 P7，T1/T2 连 ADR 一起推到 v0.4.2 之后评估；T3 不再是「近期先行」，改为等 S1 落地后一并评估（下面第 3 条）。

### 建议实现顺序（2026-09-05 重排编号）

原笔记（同日）把这三步也写成 T1/T2/T3，与本节上方 T1–T4 的 OS 级语义撞车，现改用 **S 前缀**区分「进程内静态分析层」与「OS/部署级防线」：

1. **S1 · 命令拆分 + flag 级路径检查**：不再从命令行里抠 token 猜路径，先做真正的 shell 词法拆分，再按 flag 语义判定参数是不是路径（含 `--interpreter` / `-e` / `-c` 与裸执行）。**目前只停在决策状态**：`docs/plans/` 下没有对应计划文件，代码零改动——`git log` 里 `c647c9d` 的 8 条 path 规则已是全部现状，`python -c "open(...).write(...)"` 实测过防线。
2. **S2 · `sh -c` 载荷递归解析**：对套壳的 `-c` 字符串递归跑 S1。依赖 S1，不能先行。
3. **T3 · 解释器 / 内联执行强制审批**：不依赖 S1/S2，但也不再假装能静态分析 `-c` 与 `.py` 内容——只做「命中即人审」的闸门，等 S1 有结论后一并评估要不要留。

### 🧩 待 grill 明确后立项

- （暂无新增需求项；terminal 安全项见上 T1–T4 与 S1/S2）

---

## 遗留 backlog（主干体检·主动不做项，2026-08-26 起留档）

影响小或需结构性前提，暂缓处理，需要时再评估：

- ~~生产路径 `lock().unwrap()`~~：已提升为 **H4**（`slash.rs` background_tasks 与 `sqlite.rs` 全文件约 20 处，锁内均同步调用、无 await，机械替换 `unwrap_or_else(|e| e.into_inner())`）。
- ~~常量正则 `unwrap()`~~：已提升为 **H4**（`secrets.rs` / `config.rs` / `approval.rs`，逻辑上不可 panic，按约定补注释即可）。
- `TRIM_CACHE` 无上限增长（`memory/trim.rs`）：单用户 MEMORY 变更频率低，实际影响极小。
- 图片逐张串行 vision 描述（`agent/mod.rs::maybe_describe_images`）：可 `join_all`，但通常单图。
- tools schema 每次请求重建序列化（`openai_compat.rs`）：~20 工具 × 每迭代，微小。

### 排查提示（留档，不是待办）

- **agent 改自己的持久化，验证必须由外部实例做**（2026-09-05 定）：running 的 llaia 进程锁着自己的 `sessions.db` 与 workspace，让 agent 自测「删会话后清单是否被回收」这类项必然测不到——写验证脚本得另起一个实例、复制一份状态目录。同因：本机跑 `cargo test` 前须停掉运行中的 llaia，否则 `target\debug\llaia.exe` 被锁报拒绝访问（os error 5）。
- 残留数据清理的现行结论（`todos/` 启动期 GC 已落地）见 [CHANGELOG.md](CHANGELOG.md) §v0.4.1。
- **定期主干代码体检**是**例行项**而非一次性交付：需要时手动触发（用户定，2026-08-25），主干模块（agent loop / provider / memory / web）逐次过一遍，产出为检查记录（发现项 → 直接修 / 单独立项 / 搁置留档）。历轮已交付修复见 CHANGELOG §v0.3.1 / §v0.4.0 / §v0.4.1。

---

## 工程约定

- 每个 Task 完成后跑 `cargo test` + `cargo clippy`
- 提交节奏：一个完整功能/修复链路验证通过后提交一次，不要每个 Task 都提交
- 遇到编译错误立即修，不要积累
- 详细实现计划放 `docs/plans/YYYY-MM-DD-<feature>.md`，设计规格放 `docs/specs/YYYY-MM-DD-<feature>-design.md`，架构决策放 `docs/adr/NNNN-<topic>.md`
- 阶段交付后，其完整勾选清单迁入 `docs/CHANGELOG.md`，本文件只保留「已交付阶段一览」索引 + 下一步计划
