# LLAIA 项目 Roadmap

> 本文档是 LLAIA 的**前瞻路线图**：顶部是已交付阶段一览（索引），主体是**近期小修（H 系列）**与下一步计划（P9）。
> 各阶段的**完整交付清单**见 [`CHANGELOG.md`](CHANGELOG.md)；详细实现计划见 [`plans/`](plans/)，设计规格见 [`specs/`](specs/)，架构决策见 [`adr/`](adr/)。

**整体目标**：一个单用户、本地优先的私人 AI 助理，跨 CLI/QQ/Web 等多 channel 接入，主 Agent + 可委派子 Agent 协作，持久化记忆与会话。

---

## 状态图例

- ✅ 已完成
- 🚧 进行中
- ⏳ 计划中（未开始）

> 条目勾选框语义：**`[x]` = 代码已落地**，`[ ]` = 尚有未完部分（含「已定案未实现」「部分交付」）。
> 「已定案/已立项」只是决策完成，不算 `[x]`——须在实现后勾上，并在条目标注交付日期与代码位置。

> **编号约定**（一个编号只表示一件事，2026-09-05 起）：**P*** = 阶段性工程（P1…P7）；**H*** = 近期小修（发版窗口内的低风险止血项）。T1–T4（OS/部署级安全防线）与 S1–S2（进程内静态分析）曾是旧 P7「terminal 脚本绕过防护」的子项编号，2026-09-07 随专项收口退役（归档见 CHANGELOG §v0.5.0）；新 P7 于 2026-09-09 重新立项，不再使用 T/S 编号。曾有一版把静态分析三步也写成 T1/T2/T3，与 T1–T4 撞车，已改（后者改用 S 前缀）。

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
| v0.5.0 | ✅ | 发版版本（因含行为不兼容改动由原定的 v0.4.1 直升 minor）：H1–H4 止血 + T3/S1 terminal 安全收口 + 画像模板升级 + /session 改名 + 框架消息英文化 + todo GC + WebUI 归档卫生 + chat 会话线侧栏，2026-09-11 打 tag 发版 | [CHANGELOG.md](CHANGELOG.md)（§v0.5.0）、[release-notes/v0.5.0.md](release-notes/v0.5.0.md) |
| v0.5.1 | ✅ | 补丁版：全新机器启动崩溃修复 + todo 注入弱化（done 项不再回灌）+ path_guard 段首路径程序名校验 + 受信目录持久化 + chat 历史回放/审批结果标注 + 微信登录二维码上卡，2026-09-14 打 tag 发版 | [CHANGELOG.md](CHANGELOG.md)（§v0.5.1）、[release-notes/v0.5.1.md](release-notes/v0.5.1.md) |
| v0.5.2 | ✅ | 补丁版：Delete Guard（破坏性命令进 .trash）+ QQ 审批内联按钮（msg_type=2）+ 思考流 guard 补漏 + path_guard 内联载荷扩面，2026-09-17 打 tag 发版 | [CHANGELOG.md](CHANGELOG.md)（§v0.5.2）、[release-notes/v0.5.2.md](release-notes/v0.5.2.md) |
| P7 | ✅ | 六项 grill 全部收敛：WebUI 审批/问题卡片（AC1–AC3）、MCP stdio 新 spec 兜底（A1–A4）、session-rail 先行交付；其余留观/不做定案（触发条件留档见下节） | [CHANGELOG.md](CHANGELOG.md)（§v0.5.0、§v0.6.0） |
| P8 | ✅ | 模型目录重构：provider 连接注册表 + `[model.<id>]` 目录 + 加载期自动迁移，配置 breaking 随 v0.6.0 | [CHANGELOG.md](CHANGELOG.md)（§v0.6.0）、[plans/2026-09-25-model-catalog.md](plans/2026-09-25-model-catalog.md) |

> **P6 已全部交付并归档**：原 P6 节的完整勾选清单（WebUI W1/W2/W3、会话主题总结、provider 针对性优化、`memory_research`、启动优化 #11、主干代码体检、#A–#J 新增发现、Generation Guard、First-run Bootstrap 等）已随各项实现陆续迁入 [CHANGELOG.md](CHANGELOG.md) §v0.3.1 / §v0.4.0，本文件不再保留已交付明细。注意 CHANGELOG 里**没有独立的 §v0.3.2**：原按 0.3.2 攒的开发内容跨版本号当作 v0.4.0 发布，段标题已一并重定。v0.5.0 的三项同样已迁入 CHANGELOG，本文件只留索引与下一步。

---

## 近期小修（H 系列）

**状态**：H 系列仅剩 H5 未交付｜v0.5.2 已发（2026-09-17 打 tag），H5 未随版交付、顺延至下一窗口（当前开发版本 0.5.3，工作区版本号恒为下一开发版）

> H1（file_edit 自纠错 + CRLF 宽容）、H2（```` ```json ```` 围栏工具调用）、H3（文档一致性扫尾）、H4（毒锁恢复等机械项）、H6（v0.5.0 发版动作，含 v0.4.1 → v0.5.0 升号说明与攒发时序修订）均已交付，完整记录见 [CHANGELOG.md](CHANGELOG.md) §v0.5.0，此处不再保留明细。v0.5.1 窗口内交付的四项代码改动（todo 注入弱化、path_guard 段首程序名、受信目录持久化、chat 回放/审批标注）见 §v0.5.1，它们不属 H 编号序列。

- [ ] **H5 · 入站附件 `uploads/` 无回收通路**（todo GC 的同类项，2026-09-05 顺带查得）：QQ 附件（`channels/qq.rs:872`）与邮件附件（`channels/mail.rs:190`）都落 `<家目录>/uploads/`，全仓库没有任何删除/回收代码，只增不减（本机现 2 个文件 / 220 KB，属还没长起来而不是没问题）。**两条现成通路都不能照搬**：`todos/` 的按会话 uuid GC 在这里无意义（附件不属于会话生命周期，且文件名是 `<msg_id>_<filename>` 不带 uuid），`workspace/tmp/` 的启动期 3 天 mtime 清理又太危险——用户半年前发来的图片可能仍被引用。必要性：**低–中**（单用户场景增长慢，但发版前把它记下来比忘掉便宜）。
      - **定案（grill 2026-09-07）：WebUI 手动清理**——保留全部 + 容量显示 + uploads 文件列表/手动删除，不做任何自动回收（消息历史引用这些路径，自动删/挪都会断链，零自动 = 零断链风险）。原定挂到 v0.5.1，**v0.5.1 未带**（发版只含上述四项代码改动），顺延至下一窗口。

---

## P7 — 已收口归档（2026-09-09 立项，2026-09-23 交付齐）

> **旧 P7 已收口归档**：P7 编号曾用于 terminal 脚本绕过防护专项（T1–T4 / S1–S2），2026-09-07 全部定案——T3（解释器内联载荷强制审批）、S1（命令拆分 + flag 级路径检查）、T2（无特权账户文档化）已交付，S2 / T1 / T4 定案不做；完整记录随 **v0.5.0 攒发**迁入 [CHANGELOG.md](CHANGELOG.md) §v0.5.0。

> **新 P7 六项全部过 grill（2026-09-22）并交付/定案**：已交付明细（WebUI 审批卡片 AC1–AC3、ask_user 问题卡片、MCP stdio `-32601` 兜底 A1–A4、session-rail 先行）随 §v0.5.0 / §v0.6.0 迁入 [CHANGELOG.md](CHANGELOG.md)，本文件不再保留。

**留观 / 不做定案（触发条件留档，触发后重新评估再立项）**：

- **RAG → 留观**：现有记忆栈三层已覆盖（MEMORY + `memory_research` FTS5 + sqlite 留底），RAG 真实增量只有语义召回与非会话语料库；届时嵌入（Ollama 本地）+ sqlite-vec 契合 sqlite-first 架构，纯加法不迟。**触发条件：出现一批反复被查询的本地文档语料，且 `memory_research` 关键词召回不满意。**
- **浏览器自动化 → 不自建**：MCP 外挂完胜（基础设施现成、`McpTool` 默认审批、playwright-mcp 维护中），内嵌 chromiumoxide = 几百行新维护面。**触发动作：需要时改 mcp.toml 接 playwright-mcp，零代码零立项（浏览器工具绝不进 `safe_tools` 白名单）。**
- **搜索增强 → 不做**：多源聚合/rerank 要高频搜索才摊得平成本，tavily/baidu/brave 三路由已够用。**触发条件：搜索质量真实卡住任务且换 provider 解决不了。**
- **原子工具优化增强 → 撤项**：无痛点清单就没有项目；既有 H 系列止血 + 定期主干体检两条通路承接，痛点出现时随用随修。
- **WebUI 增强**：① session-rail 已交付（§v0.5.0）；② bound path 可视化（session-rail + `[scope]` 文本行已覆盖）、④ 运行状态提示 → 留观，**触发条件：日常使用中实际造成困扰**；⑤ 已删除（伪问题）。
- **MCP HTTP 双协议（B）→ 留观**（stdio 兜底 A1–A4 已交付）：**触发条件：真要接 HTTP server，或想用的 server 只发新 spec HTTP。** 届时先拍 auto 判定信号（白名单：400 缺 session / 404 session 不存在 / -32601；**401/403、5xx、超时一律不回退**）。不做项留档：MCP server 模式（纯 client 定位不变）、OAuth/CIMD、Tasks/MRTR/MCP Apps 扩展、ttlMs tools/list 缓存、WebUI 降级状态位（log warn 足够）。

---

## P9 — 安全模型落地（ADR-0033）

**状态**：🚧 进行中（2026-10-02 [ADR-0033](adr/0033-security-model.md) 评审定案转 Accepted；Phase 1 + Phase 2 首刀已于 2026-10-05 落地）

依 ADR-0033：资产分级（**A0 人格主权 / A1 凭据 / A2 隐私历史 / A3 全盘机器**，A 编号刻意避开既有闸门 T2/T3/T6）+ 五层防线（L1 可恢复性基建 → L5 字符串启发）。总体顺序：先建可恢复性基建给全部闸门的放行宽松度供底气，再做防泄露组合刀（L3 读入侧收缩 + L4 出站首闸），字符串层降级为审计线索，最后做闸门减法。

- [x] **Phase 1 · workspace 快照基建**（2026-10-05，`src/snapshot.rs`，[plan](plans/2026-10-05-p9-snapshot-and-env-scrub.md)）：时间戳镜像目录 + sha1 去重（`<config_dir>/snapshots/<时刻>/<key>`，零新依赖，弃 git-backed——外部 git 依赖与轻量单 exe 定位冲突）。A0/A2 写前留底走双通道：写前挂钩（memory_write 双通路 / file 工具家目录人格文件 / `/memory-compact`，经 `ApprovalContext.snapshot` 与工具构造注入）+ 定时 sweep（10 分钟；sessions.db 用 `VACUUM INTO` 一致副本、24h 节流，防 WAL 活拷损坏与爆盘）；保留窗口 `[runtime].snapshot_retention_days`（默认 14 天）日清 GC；`snapshot_enabled` 总开关。**快照库对 agent 只读**：位置在 config_dir 顶层天然界外 + file/terminal 工具持 `snapshot_root` 硬拒（execute 与 execute_approved 同拒，堵 delegate/批准豁免绕行；`bash -s` stdin 内路径不可见属 L5 固有局限，硬边界归 Phase 3 T2）。恢复 = 从快照目录手工拷回
- [ ] **Phase 2 · 防泄露组合刀**（顺序即优先级；env 剔除与 Phase 1 无依赖，可提前单独落地）：
    - [x] **env 变量密钥剔除**（2026-10-05，`src/child_env.rs` + `tools/terminal.rs::run_command` + `mcp/transport.rs::spawn_child`）：spawn terminal/MCP 子进程剔除变量名含 `KEY`/`SECRET`/`TOKEN` 的继承变量（学 codex），config 键 `runtime.scrub_child_env`（默认 true）。MCP server 的 mcp.toml `env` 段显式提供的键在白名单内不剔除；`StdioTransport` 持标志，reset 重连 respawn 同样生效。dotenvy 灌进进程 env 的 .env 凭据不再随一条 `env` 命令倒出
    - [ ] **基础设施文件只读守卫**：config.toml / sessions.db / mcp.toml / trusted_dirs.json 对 agent 只读（file 工具审批→直接拒 + terminal 路径校验收紧），扩展 forbidden_home 守卫
    - [ ] **web_fetch 出站闸门（两道分离）**：SSRF 校验无条件常开（私网/环回/云元数据 IP + DNS 解析后复验，学 zeroclaw domain_guard）；域名允许清单只收紧非交互频道（未配置 = fail-closed），交互频道走 trusted_dirs 同款模式（首访新域名审批一次、批准持久化）
    - [ ] **terminal 网络命令风险标记**：curl/wget/nc/ssh/scp 等进 High 风险类，audit.log 强化记录（不拦截，留痕）
    - [ ] **拒绝话术两原则**：不含救济路径、声明硬边界劝阻绕行（approval 拒绝消息与 path_guard 错误文案）
- [ ] **Phase 3 · 部署级 T2 受限进程落地**：Windows 受限 token / 专用低权账户，含部署文档（T2 指部署规范旧名，与资产层 A2 无关）
- [ ] **Phase 4 · 闸门复审减法**：用 ADR-0033 判据 3 逐个过现有机制，该降级降级、该删除删除（T3 内联闸门为第一候选）
- **记忆卫生三档**（A0 特许写入口的落地点，与 ADR-0033 同日定案；三档独立排期，第 0 档随时可做）：
    - [ ] **第 0 档 · memory_write 写入时防重**：entry 归一化（空白/标点折叠）后与现有行比对，已存在即返回 already remembered 不落盘——多数膨胀是字面重复，无需 LLM
    - [ ] **第 1 档 · compress_memory 结构化升级**（`memory/markdown.rs`，现为裸 LLM 单发）：① 确定性预检——按 `- [YYYY-MM-DD] entry` 契约解析条目，精确/近似重复直接合并不经 LLM；② LLM 只做语义合并（输入输出均为条目列表）；③ 输出结构校验——每行匹配条目正则 + 每行可溯源到至少一条输入 + 凭空行即失败，重试一次仍败则原文件保留并报错。校验即注入防线：被骗压缩器加不进任何新内容（ADR-0033「A0 特许写入例外」的对症控制）
    - **触发**：`trim_memory_to_budget` 实际开始丢弃内容（文件超 ADR-0025 预算）为自动压缩信号——有写前备份，按 ADR-0033 判据 1（可恢复性放行）免交互审批；`/memory-compact` 保留为手动覆盖
    - **SOUL/USER 压缩 → 留观**：人格文件让 sidecar LLM 改写与 A0「直接拒」立场冲突，增长压力远小于 MEMORY。触发条件：实测增长出现；届时只做结构性去重（同节合并重复 bullet、逐字重复行）+ diff 人审，永不丢唯一内容
- **留档不排期**：cron T3 豁免扩大 A2 可读面（`cron_allow_inline_interpreter` 让内联 python 可读 sessions.db，dream 首跑实证）；读入侧收缩若立项，考虑给 cron 走 scoped 会话查询 helper 而非裸文件。详见 ADR-0033 §5 留档项

---

## 遗留 backlog（主干体检·主动不做项，2026-08-26 起留档）

影响小或需结构性前提，暂缓处理，需要时再评估：

- ~~生产路径 `lock().unwrap()`~~ 已交付（**H4**，见 [CHANGELOG.md](CHANGELOG.md) §v0.5.0）。
- ~~常量正则 `unwrap()`~~ 已交付（**H4**，同上；`approval.rs` 核实为立项笔误——该文件没有常量正则）。
- `TRIM_CACHE` 无上限增长（`memory/trim.rs`）：单用户 MEMORY 变更频率低，实际影响极小。
- 图片逐张串行 vision 描述（`agent/mod.rs::maybe_describe_images`）：可 `join_all`，但通常单图。
- tools schema 每次请求重建序列化（`openai_compat.rs`）：~20 工具 × 每迭代，微小。

### 排查提示（留档，不是待办）

- **agent 改自己的持久化，验证必须由外部实例做**（2026-09-05 定）：running 的 llaia 进程锁着自己的 `sessions.db` 与 workspace，让 agent 自测「删会话后清单是否被回收」这类项必然测不到——写验证脚本得另起一个实例、复制一份状态目录。同因：本机跑 `cargo test` 前须停掉运行中的 llaia，否则 `target\debug\llaia.exe` 被锁报拒绝访问（os error 5）。
- 残留数据清理的现行结论（`todos/` 启动期 GC 已落地）见 [CHANGELOG.md](CHANGELOG.md) §v0.5.0。
- **定期主干代码体检**是**例行项**而非一次性交付：需要时手动触发（用户定，2026-08-25），主干模块（agent loop / provider / memory / web）逐次过一遍，产出为检查记录（发现项 → 直接修 / 单独立项 / 搁置留档）。历轮已交付修复见 CHANGELOG §v0.3.1 / §v0.4.0 / §v0.5.0。

---

## 工程约定

- 每个 Task 完成后跑 `cargo test` + `cargo clippy`
- 提交节奏：一个完整功能/修复链路验证通过后提交一次，不要每个 Task 都提交
- 遇到编译错误立即修，不要积累
- 详细实现计划放 `docs/plans/YYYY-MM-DD-<feature>.md`，设计规格放 `docs/specs/YYYY-MM-DD-<feature>-design.md`，架构决策放 `docs/adr/NNNN-<topic>.md`
- 阶段交付后，其完整勾选清单迁入 `docs/CHANGELOG.md`，本文件只保留「已交付阶段一览」索引 + 下一步计划
