# ADR-0033: 安全模型——从补丁堆到资产中心设计

- 状态：Accepted（2026-10-02 评审定案；实施条目见 [docs/plan.md](../plan.md) P9，Phase 0–4 落地后逐项回填）
- 日期：2026-10-02
- 关联：修订 [ADR-0020](0020-permission-and-approval.md)（审批体系，本 ADR 为其上位框架）；落地 [ADR-0032](0032-instance-architecture.md) 的 forbidden_home 守卫思想；承接 [docs/guide/security-hardening.md](../guide/security-hardening.md)（T2 部署规范）；起因是 2026-10-01 dream cron 首跑被 T3 拦截的事故复盘（见 commit 2ec9b13 讨论）；防泄露与闸门取舍设计参考 `.ref` 四仓库调研（codex / zeroclaw / nanobot / astrbot，2026-10-02），出站面盘点见 §2.1。2026-10-02 评审定案修订四项：资产层级改 **A 编号**（避免与既有闸门 T2/T3/T6 撞车）、出站面补 search 通道、web_fetch 闸拆为「SSRF 常开 + 域名清单仅非交互」、A0 特许写入口（记忆压缩工具）一并定案——详见 §1 注与 Phase 0

## 背景 / Context

当前安全体系是历次事故后补丁堆出来的：P4-d 审批档位 → 命令黑名单 → 路径提取校验 → T3 内联解释器闸门（9-07）→ Windows shell 包装拦截（9-15 高危漏洞）→ Delete Guard（9-17）→ T6 人格主权守卫 → cron T3 豁免（10-01）。复盘发现两条规律：

1. **所有补丁都立在同一个位置——命令字符串层。** 拦截依据是"命令行长什么样"：黑名单匹配、路径提取、内联形态识别。而命令形态是无穷的（`powershell -Command`、`python -c`、heredoc、管道喂、写脚本文件再执行……），每发现一个新形态就多一块补丁，这个过程不会收敛。补丁感来自防线位置站错，不是补丁打得不认真。
2. **目的从未被显式定义。** 现有机制混杂了防篡改（人格守卫、Delete Guard）、防误伤（界外审批）、防泄露（几乎为零）三种目标，但没有任何文档说清"我们到底在保护什么、防谁、防到什么程度"。每次事故只能就事论事。

显式化安全目的，收敛为两条（按优先级）：

1. **防篡改与删除**：敏感文件不被覆盖、破坏、误删——尤其是人格文件（SOUL/USER/MEMORY）与凭据；
2. **防泄露**：凭据与隐私内容（sessions.db 历史、uploads）不流出到外部。

**威胁源修正**：现有体系全部针对"模型犯错"（无心之失），但 agent 安全的主要泄露威胁是"**模型被骗**"——web_fetch 抓取的网页、邮件正文、会话历史里藏注入指令，诱导模型外传数据。"模型作恶"（对抗性）概率最低，且字符串层从来防不住（脚本文件绕行 T3 已证），真正承接它的是 OS 权限边界。

## 决策 / Decision

### 1. 资产分级（守护对象清单，安全体系的起点）

| 层级 | 资产 | 首要威胁 | 手段基调 |
| --- | --- | --- | --- |
| **A0 人格主权** | `workspace/SOUL.md`、`USER.md`、`MEMORY.md`（main）、`instances/*/` | 篡改 | 守卫直接拒（不审批）+ 快照兜底；唯一特许写入见下注 |
| **A1 凭据** | `config.toml`、`.env`、`mcp.toml`、`trusted_dirs.json` | 泄露 + 篡改 | 读取出受控名单、写入守卫 |
| **A2 隐私历史** | `workspace/sessions.db`、`uploads/`、`logs/` | 泄露 + 删除 | 快照 + 出站把关 |
| **A3 全盘机器** | agent 家目录之外的一切 | 误删/误改（不可恢复） | 审批（交互）/ fail-fast 拒（cron）+ trash |

> **命名说明（2026-10-02 评审）**：资产层级用 **A 前缀**，刻意避开既有闸门编号——仓库词汇里 T2 = 部署级无特权账户（[security-hardening.md](../guide/security-hardening.md)）、T3 = 内联解释器闸门、T6 = 人格守卫。本 ADR 中的「T2 受限进程」沿用部署规范旧名，指 L2 的 OS 层落地，与资产层 A2 无关。

> **A0 特许写入例外**：记忆压缩/去重工具是人格文件唯一绕过「直接拒」的写入通路——它不接收自由文本目标/内容参数，输出经结构校验（每行可溯源到输入条目、`- [YYYY-MM-DD] entry` 契约保持、凭空行即失败回退原文件），被骗的压缩器**加不进**任何新内容。这条不只是质量保险：MEMORY.md 是持久化注入潭，早期从外部内容写入的条目可能在压缩时引爆注入，结构校验即此威胁的对症控制。已于 2026-10-06 落地（`src/memory/hygiene.rs` + `markdown.rs::compress_memory` 三段管线，实施见 [docs/plans/2026-10-06-memory-hygiene.md](../plans/2026-10-06-memory-hygiene.md)）。

### 2. 防线分层（按可靠性排序，字符串层降级）

| 层 | 防线 | 特性 | 现状 |
| --- | --- | --- | --- |
| **L1 可恢复性基建** | workspace 快照（git-backed 或 shadow copy），A0/A2 写前留底 | 不依赖"拦得住"，篡改可回滚；**可恢复性越高，闸门可以越少** | 缺位（仅 memory-compact 写前备份） |
| **L2 OS 最小权限** | terminal 子进程跑受限 token / 低权账户（部署级 T2 落地，见 security-hardening.md） | 唯一不依赖命令形态的硬边界；脚本文件、python -c、任何形态同受约束 | 仅有文档规范，未落地 |
| **L3 资产守卫** | 按资产分级直接拒/审（forbidden_home 模式推广） | 与命令形态无关，守在文件系统入口 | forbidden_home 已有；A1/A2 分级缺位 |
| **L4 出站口检查** | 外发通道的分级把关（对象清单见 §2.1 出站面盘点） | 防泄露的直接位置——但出站口有七个且 **LLM provider 本身不可把关**，故 L4 防的是"被骗模型最顺手的显式通道"，根本兜底靠 L3 压缩可偷库存 | **缺位**（web_fetch 视为界内免审；terminal 网络命令零把关） |
| **L5 字符串启发** | 黑名单、路径提取、T3 内联识别 | 永不收敛、可绕行；**降级为审计线索与提示信号，不再当安全边界** | 已有（现有补丁的主体） |

### 2.1 出站面盘点（L4 的对象清单，2026-10-02 基于工具注册表逐一核实；同日评审补充 search 通道，序号相应顺延）

| 出站通道 | 位置 | 数据可控性 | 现状把关 | 风险排序 |
| --- | --- | --- | --- | --- |
| terminal 任意网络命令 | `tools/terminal.rs` | 完全可控（目标/载荷/协议全自由） | 仅 T3 内联启发式 + 界外路径判定，内容零把关 | **1（最大缺口）** |
| send_file / send_image | `tools/send_media.rs` | 模型选文件内容 | 无内容检查 | 2（目标固定，泄的是"主人的数据给主人"，低危） |
| image_gen / image_edit / tts | `tools/image_gen.rs`、`tools/tts.rs` | prompt/文本发往第三方 API | 无内容检查 | 3 |
| search | `tools/search.rs` | query 全文由模型撰写，发往第三方搜索 API（provider 目标 config 固定，模型不可选） | 无内容检查 | 4（与 3 同性质：模型撰写的文本出第三方，暂无动作，留观） |
| mcp_* 工具 | `tools/mcp.rs` | 取决于 MCP server | 审批"一律界外"是入站判定，调用照样出网 | 5 |
| web_fetch | `tools/web.rs` | URL + 隐式带出信息 | 无域名/SSRF 检查 | 6（**最好管**：显式、URL 在 transcript 可见） |
| mail/IM 渠道推送 | `channels/mail.rs` 等 | 内容可控，**目标固定**（config 写死 owner/群） | 结构性已封：模型无法指定收件人 | 7（✅ 无需动作） |
| LLM provider 调用本身 | — | 模型读过的一切都会发出去 | **不可把关**（核心功能） | 0（推论：防泄露第一性原理在**读入侧**） |

盘点得出的三条结论：

1. **terminal 是真正的最大缺口**，但内容级把关短期做不到（命令形态无穷）——近期处置是风险标记 + audit 强化留痕，硬边界归 L2（OS 沙箱网络限制）。
2. **web_fetch 排第 6 危险但仍是最好管的格子**（与 search 同为显式参数、transcript 可见），先做不是因为最危险，而是最便宜的第一个格子——且被骗模型被注入指令指名"访问这个 URL"时最顺手的就是它。
3. **出站管不完，读入侧才是根本**：LLM provider 这条出站永远开着，所以每压缩一分可偷库存（env 密钥、config、sessions.db 的可读性），所有出站通道风险同时下降。这决定了 Phase 2 是"L3 读入侧收缩 + L4 首闸"的组合刀，不是单独给 web_fetch 加闸。


### 3. 「度」的三判据（闸门准入与退役规则）

安全强度的把握不再拍脑袋，每道闸门（无论新旧）过三问：

1. **可恢复性放行**：出错后能撤销的操作不拦（放行底气来自可恢复，不来自"判定它安全"）；不可撤销才进入拦/审分支。
2. **资产等级选手段**：A0/A1 → 直接拒（不给人审被哄骗的机会）；A3 界外 → 审批；泄露风险 → 查出站口而非入站。
3. **兜底检验**：「它拦下来的东西，别的层有没有兜底？」有兜底 → 删掉或降级此闸门（做减法）；没有兜底 → 允许存在。**新事故补丁必须先归位到本矩阵，矩阵内已有的加强对应层，矩阵外的新格子才允许新机制**——补丁从此不再自由生长。

### 4. 现有机制归位审计（按判据 3 复审的初表）

| 机制 | 归位 | 处置 |
| --- | --- | --- |
| forbidden_home（T6 人格守卫） | L3 范本 | **保留强化**：扩展覆盖 A1 凭据文件写入；A0 保留唯一特许写入口 = 结构校验通过的记忆压缩工具（P9 记忆卫生三档落地） |
| Delete Guard（rm → .trash） | L1 思想萌芽 + L3 | **保留**；界内免审的底气正是"可恢复"，判据 1 的活例 |
| 审批档位（P4-d）/ trusted dirs | L3/L4 之间 | 保留；权限档位继续作为交互频道的总开关 |
| T3 内联闸门（含 cron 豁免） | L5 | **降级候选**：定位为"无意识操作的强制人审点 + 审计信号"，非边界；待 L1/L2 落地后复审是否进一步降级 |
| 命令黑名单 / 路径提取 | L5 | 保留为提示与审计，不再承担边界叙事 |
| 出站口 | L4 | **缺位**——按 §2.1 盘点：terminal 网络命令是最大缺口（近期标记+留痕，远期归 L2），web_fetch 是首刀；渠道推送已结构性封死无需动作 |
| 子进程环境变量 | L3/A1 | **缺位**——terminal 子进程继承完整环境，模型一条 `env` 即可读出全部 API key；学 codex 默认剔除 `*KEY*`/`*SECRET*`/`*TOKEN*` |
| config.toml / sessions.db / mcp.toml | L3/A1+A2 | **缺位**——现状分层不齐：config.toml / mcp.toml 在 config_dir，file 工具默认界外走审批（但 /move 可将其纳入作用域），sessions.db 在 workspace 内自由可写；共同的无边界口是 terminal（L5 路径提取不可靠）。被篡改即同时丢凭据与审计；学 zeroclaw"agent 禁改自身基础设施"，推广 forbidden_home 为只读守卫（file 工具审批→直接拒 + terminal 路径校验收紧） |
| 拒绝反馈话术 | L5 话术 | **缺位**——现有拒绝信息未做两件事：不含"如何放开权限"的救济指引（防模型游说扩权，学 zeroclaw）、声明"这是策略硬边界，别用 shell 技巧绕行"（学 nanobot） |
| workspace 快照 | L1 | **缺位**，第二优先 |
| T2 受限进程 | L2 | **缺位**，第三优先（部署复杂度最高，收益最硬） |

### 5. 分步路线

- [x] Phase 0：本 ADR 评审定稿（2026-10-02 转 Accepted。评审修订四项：资产层级 T→A 改号防与闸门 T2/T3/T6 撞车、出站面补 search 通道、web_fetch 闸拆为「SSRF 常开 + 域名清单仅非交互」、A0 特许写入口随记忆卫生三档一并定案——实施条目见 [docs/plan.md](../plan.md) P9）
- [x] Phase 1：workspace 快照基建（2026-10-05 落地，`src/snapshot.rs`；时间戳镜像目录 + sha1 去重，弃 git-backed——外部 git 依赖与轻量单 exe 定位冲突。A0/A2 写前挂钩 + 10 分钟定时 sweep 双通道；sessions.db 走 `VACUUM INTO` 一致副本 24h 节流；快照库 `<config_dir>/snapshots/` 对 agent 只读——file/terminal 工具持 `snapshot_root` 硬拒，execute 与 execute_approved 同拒。实施细节见 [docs/plans/2026-10-05-p9-snapshot-and-env-scrub.md](../plans/2026-10-05-p9-snapshot-and-env-scrub.md)）
- [ ] Phase 2：防泄露组合刀（L3 读入侧收缩 + L4 出站首闸，顺序即优先级）：
  - [x] env 变量密钥剔除：spawn terminal/MCP 子进程时剔除 `*KEY*`/`*SECRET*`/`*TOKEN*`（学 codex config_types.rs:232-246），config 键 `runtime.scrub_child_env`（默认 true）。2026-10-05 落地（`src/child_env.rs`；MCP server env 段显式提供的键在白名单内；`StdioTransport` 重连 respawn 同样生效）。**本项与 Phase 1 无依赖、实现零风险，可提前单独落地**（评审注：dotenvy 已把 .env 灌进进程 env，terminal 一条 `env` 即倒出全部 API key——现状已消解）
  - [x] 基础设施文件只读守卫：config.toml / sessions.db / mcp.toml / trusted_dirs.json 对 agent（含 terminal 路径校验）只读，扩展 forbidden_home 守卫。2026-10-06 落地（`path_guard::infra_readonly_target` + `GuardCtx` 全 agent 注入，file/terminal execute 与 execute_approved 同拒 `[infra-guard]`；`.env` 按 A1 资产表补入；cron.toml 不守卫——cron_task 工具是特许通路）
  - [x] web_fetch 出站闸门，两道分离（评审修订）：**SSRF 校验无条件常开**（私网/环回/云元数据 IP、DNS 解析后复验，学 zeroclaw domain_guard——不涉可用性取舍）；**域名允许清单只收紧非交互频道**（cron/delegate：未配置 = 拒绝 fail-closed），交互频道走 trusted_dirs 同款模式（首访新域名审批一次、批准持久化），不把单用户助理最高频的「帮我查这个」闸死——cron T3 已证明「闸门堆砌 = 功能死亡」。2026-10-06 落地（`tools/web.rs::ssrf_check` 无条件 + `WebFetchGate`/`allowed_domains` 非交互 fail-closed + 交互首访审批持久化 web_domains.json；闸门判定在 delegate/yolo 早退之前）
  - [x] terminal 网络命令风险标记：curl/wget/nc/ssh/scp 等进 High 风险类，audit.log 强化记录（不拦截，留痕）。2026-10-06 落地（`path_guard::network_command_hit`，审计条目附 `reason=network=<prog>`；/ok 批准路径补审计）
  - [x] 拒绝话术两原则：不含救济路径、声明硬边界劝阻绕行（进 approval 拒绝消息与 path_guard 错误文案）。2026-10-06 落地（`path_guard::HARD_BOUNDARY_NOTICE` 统一追加）
- [x] Phase 3：T2 受限进程落地（2026-10-08，方案 A「部署手册 + doctor 自检」：security-hardening.md T2 节扩为完整部署手册（Windows 专用账户 + 服务化 + ACL 验证清单；Linux systemd 加固 unit）+ `src/privilege.rs` 零依赖提权探测接入 `llaia doctor` / `/api/doctor` 的 `security.privilege` 检查。弃选 per-command 包装与原生受限 token 的理由留档 [docs/plans/2026-10-08-p9-phase3-t2.md](../plans/2026-10-08-p9-phase3-t2.md)——runas 包装断 `bash -s` piped stdin、windows-sys 新依赖与轻量单 crate 定位冲突）
- [ ] Phase 4：闸门复审减法——用判据 3 逐个过现有机制，该降级降级、该删除删除
- [ ] 刻意不做：脚本文件内容的 destructive 模式扫描（`os.remove` / `Remove-Item` 等）——又一个猜不完的黑名单，误报成本高（正常脚本普遍带临时文件清理），且 L1/L2 兜底后无增量收益
- 留档不排期（Phase 3 后评估）：cron T3 豁免扩大 A2 可读面——`cron_allow_inline_interpreter` 让内联 python 可读 sessions.db（dream 首跑实证）；读入侧收缩若立项，考虑给 cron 走 scoped 会话查询 helper 而非裸文件访问

## 后果 / Consequences

- (+) 新事故有了归位规则：补丁必须落入矩阵格子，体系演化从"自由生长"变为"受控填空"。
- (+) 防泄露从零到一：L3 读入侧收缩（env 密钥剔除、基础设施文件只读）+ L4 出站首闸（web_fetch 闸门）组合落地，直接针对注入式威胁；渠道推送目标固定（config 写死收件人）已结构性封死"寄给陌生人"路径，无需额外动作。
- (+) 可恢复性基建提升全部现有闸门的放行底气——"度"的宽松有了结构性来源，不再靠闸门堆砌。
- (−) 快照有存储与性能成本（单用户规模可控，需设保留窗口）。
- (−) T2 受限进程部署复杂度高，可能影响部分合法工具的运行，需要灰度。
- (−) T3 等字符串闸门降级过程中，"心理安全感"短暂下降——需接受"文档上承认防不住"优于"伪边界"。
- 迁移零成本：本 ADR 是框架文档，Phase 1 前不改任何行为；现有机制全部原地保留、仅叙事重新定位。

## 备选 / Alternatives

- **维持补丁式演化**：每次事故就事论事。不可持续已被证明——字符串形态无穷，且防泄露目的下出站口裸奔。
- **全面推翻重写**（推倒现有闸门按新模型重建）：大爆炸式重构风险高，现有机制（审批、trash、人格守卫）与新模型兼容良好，归位即可无需重写。
- **纯收紧**（更多闸门、更严审批）：与单用户私人助理的可用性目标冲突；cron 等非交互场景已被 T3 证明"闸门堆砌 = 功能死亡"。收紧的正确方向是 L1/L2 提供结构安全感，而非 L5 继续加锁。
