# LLAIA 项目 Roadmap

> 本文档是 LLAIA 的**前瞻路线图**：顶部是已交付阶段一览（索引），主体是下一步计划（P7）。
> 各阶段的**完整交付清单**见 [`CHANGELOG.md`](CHANGELOG.md)；详细实现计划见 [`plans/`](plans/)，设计规格见 [`specs/`](specs/)，架构决策见 [`adr/`](adr/)。

**整体目标**：一个单用户、本地优先的私人 AI 助理，跨 CLI/QQ/Web 等多 channel 接入，主 Agent + 可委派子 Agent 协作，持久化记忆与会话。

---

## 状态图例

- ✅ 已完成
- 🚧 进行中
- ⏳ 计划中（未开始）

> 条目勾选框语义：**`[x]` = 代码已落地**，`[ ]` = 尚有未完部分（含「已定案未实现」「部分交付」）。
> 「已定案/已立项」只是决策完成，不算 `[x]`——须在实现后勾上，并在条目标注交付日期与代码位置。

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
| P6 | ✅ | 稳定性修复 + 快赢 + WebUI 批次 + 任务线/侧问/插话/媒体作用域 + Generation Guard/首运行引导（代码已全部落地，待发布） | [CHANGELOG.md](CHANGELOG.md)（§v0.3.1、§v0.3.2） |

> **P6 已全部交付并归档**：原 P6 节的完整勾选清单（WebUI W1/W2/W3、会话主题总结、provider 针对性优化、`memory_research`、启动优化 #11、主干代码体检、#A–#J 新增发现、Generation Guard、First-run Bootstrap 等）已随各项实现陆续迁入 [CHANGELOG.md](CHANGELOG.md) §v0.3.1 / §v0.3.2，本文件不再保留已交付明细。其中 `v0.3.2` 尚未打 tag 发布，发布节奏见 AGENTS.md「发版」。

---

## P7 — 下一步计划

**状态**：⏳ 计划中（起点 2026-09-04）

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

> 定案方向预判（待 grill 确认）：T3 作为**近期代码改动**先行，T1/T2 作为**部署规范**写进文档（安全/权限相关 guide），T4 作为推荐实践。是否引入 OS 沙箱取决于「是否愿意给 terminal 加运行时依赖」——需与「轻量、可移植、单 crate」的产品定位一并权衡。

### 🧩 待 grill 明确后立项

- （暂无新增需求项；terminal 安全项见上 T1–T4）

---

## 遗留 backlog（主干体检·主动不做项，2026-08-26 起留档）

影响小或需结构性前提，暂缓处理，需要时再评估：

- 生产路径 `lock().unwrap()`：`slash.rs`（background_tasks）与 `sqlite.rs` 全文件（conn，约 20 处）。锁内均同步调用、无 await，正确性无问题，仅 poisoning-panic 与编码约定冲突。机械替换 `unwrap_or_else(|e| e.into_inner())` 可解，diff 大。
- `TRIM_CACHE` 无上限增长（`memory/trim.rs`）：单用户 MEMORY 变更频率低，实际影响极小。
- 图片逐张串行 vision 描述（`agent/mod.rs::maybe_describe_images`）：可 `join_all`，但通常单图。
- tools schema 每次请求重建序列化（`openai_compat.rs`）：~20 工具 × 每迭代，微小。
- 常量正则 `unwrap()`（`secrets.rs` / `config.rs` / `approval.rs`）：逻辑上不可 panic，按约定补注释即可。
- **定期主干代码体检**为**例行项**而非一次性交付：需要时手动触发（用户定，2026-08-25），主干模块（agent loop / provider / memory / web）逐次过一遍，产出为检查记录（发现项 → 直接修 / 单独立项 / 搁置留档）。历轮已交付修复见 CHANGELOG §v0.3.1 / §v0.3.2。

---

## 工程约定

- 每个 Task 完成后跑 `cargo test` + `cargo clippy`
- 提交节奏：一个完整功能/修复链路验证通过后提交一次，不要每个 Task 都提交
- 遇到编译错误立即修，不要积累
- 详细实现计划放 `docs/plans/YYYY-MM-DD-<feature>.md`，设计规格放 `docs/specs/YYYY-MM-DD-<feature>-design.md`，架构决策放 `docs/adr/NNNN-<topic>.md`
- 阶段交付后，其完整勾选清单迁入 `docs/CHANGELOG.md`，本文件只保留「已交付阶段一览」索引 + 下一步计划
