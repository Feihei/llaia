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

> **P6 已全部交付并归档**：原 P6 节的完整勾选清单（WebUI W1/W2/W3、会话主题总结、provider 针对性优化、`memory_research`、启动优化 #11、主干代码体检、#A–#J 新增发现、Generation Guard、First-run Bootstrap 等）已随各项实现陆续迁入 [CHANGELOG.md](CHANGELOG.md) §v0.3.1 / §v0.4.0，本文件不再保留已交付明细。注意 CHANGELOG 里**没有独立的 §v0.3.2**：原按 0.3.2 攒的开发内容跨版本号当作 v0.4.0 发布，段标题已一并重定。v0.5.0 的三项同样已迁入 CHANGELOG，本文件只留索引与下一步。

---

## 近期小修（H 系列）

**状态**：H 系列仅剩 H5 未交付｜v0.5.2 已发（2026-09-17 打 tag），H5 未随版交付、顺延至下一窗口（当前开发版本 0.5.3，工作区版本号恒为下一开发版）

> H1（file_edit 自纠错 + CRLF 宽容）、H2（```` ```json ```` 围栏工具调用）、H3（文档一致性扫尾）、H4（毒锁恢复等机械项）、H6（v0.5.0 发版动作，含 v0.4.1 → v0.5.0 升号说明与攒发时序修订）均已交付，完整记录见 [CHANGELOG.md](CHANGELOG.md) §v0.5.0，此处不再保留明细。v0.5.1 窗口内交付的四项代码改动（todo 注入弱化、path_guard 段首程序名、受信目录持久化、chat 回放/审批标注）见 §v0.5.1，它们不属 H 编号序列。

- [ ] **H5 · 入站附件 `uploads/` 无回收通路**（todo GC 的同类项，2026-09-05 顺带查得）：QQ 附件（`channels/qq.rs:872`）与邮件附件（`channels/mail.rs:190`）都落 `<家目录>/uploads/`，全仓库没有任何删除/回收代码，只增不减（本机现 2 个文件 / 220 KB，属还没长起来而不是没问题）。**两条现成通路都不能照搬**：`todos/` 的按会话 uuid GC 在这里无意义（附件不属于会话生命周期，且文件名是 `<msg_id>_<filename>` 不带 uuid），`workspace/tmp/` 的启动期 3 天 mtime 清理又太危险——用户半年前发来的图片可能仍被引用。必要性：**低–中**（单用户场景增长慢，但发版前把它记下来比忘掉便宜）。
      - **定案（grill 2026-09-07）：WebUI 手动清理**——保留全部 + 容量显示 + uploads 文件列表/手动删除，不做任何自动回收（消息历史引用这些路径，自动删/挪都会断链，零自动 = 零断链风险）。原定挂到 v0.5.1，**v0.5.1 未带**（发版只含上述四项代码改动），顺延至下一窗口。

---

## P7 — 下一步计划

**状态**：🚧 进行中（2026-09-09 立项；2026-09-22 grill 六项全部定案：MCP stdio 兜底已交付、WebUI 审批卡片立项展开，其余四项不做/留观/撤项，触发条件留档；2026-09-23 审批卡片 AC1–AC3 代码落地，待随版交付）

> **旧 P7 已收口归档**：P7 编号曾用于 terminal 脚本绕过防护专项（T1–T4 / S1–S2），2026-09-07 全部定案——T3（解释器内联载荷强制审批）、S1（命令拆分 + flag 级路径检查）、T2（无特权账户文档化）已交付，S2 / T1 / T4 定案不做；完整记录（含不做项评估）随 **v0.5.0 攒发**迁入 [CHANGELOG.md](CHANGELOG.md) §v0.5.0，本文件不再保留。

> 六项已全部过 grill（2026-09-22）：一项立项展开（WebUI 审批卡片），三项不做/留观（触发条件留档，触发后重新评估再立项），一项撤项并入既有流程；MCP 项早前同日定案并已交付。「想法落袋防忘」阶段结束，本节不再有未拷问条目。

- **RAG → 留观**（2026-09-22 grill 定案）：现有记忆栈三层已覆盖——MEMORY.md（事实 + 预算裁剪 + 压缩）、`memory_research`（P5，FTS5 跨会话关键词召回）、sqlite 留底。RAG 的真实增量只有语义召回（关键词失配时）与非会话语料库（文档目录），单用户痛点未出现；届时嵌入（Ollama 本地）+ sqlite-vec 契合 sqlite-first 架构，纯加法不迟。**触发条件：出现一批反复被查询的本地文档语料，且 `memory_research` 关键词召回不满意。**
- **浏览器自动化 → 不自建**（2026-09-22 grill 定案）：原三选项（内嵌 chromiumoxide / 驱动外部实例 / MCP 外挂）裁决 MCP 外挂完胜——MCP 基础设施已建成在用（blender/sketchup-mcp），`McpTool` 默认 `requires_confirm=true`（仅 `safe_tools` 白名单免确认，浏览器工具**绝不进白名单**——能碰登录态/内网），playwright-mcp 是现成维护中 server。内嵌方案 = 几百行新维护面 + 沙箱/审批从零设计。**触发动作：需要时改 mcp.toml 接 playwright-mcp，零代码零立项。**
- **搜索增强 → 不做**（2026-09-22 grill 定案）：多源聚合/rerank 要高频搜索才摊得平成本（每查多倍 API 调用 + 去重逻辑），单用户频率撑不起；tavily/baidu/brave 三路由已交付够用。**触发条件：搜索质量真实卡住任务且换 provider 解决不了。**
- **原子工具优化增强 → 撤项**（2026-09-22 grill 定案）：方向本身是「从实际使用痛点出发逐工具盘点」——没有痛点清单就没有项目，预先立项违背「没有具体用例就不写」的编码约定。既有两条通路承接：H 系列止血窗口（file_edit 自纠错即 H1 打样）+ 定期主干体检（例行项）；diff/patch、json/yaml 查询等新原子工具无使用证据不写，痛点出现时随用随修走 H 系列。
- **WebUI chat 界面增强 → ③ 立项，②④ 留观，⑤ 删除**（2026-09-09 记录，2026-09-22 grill 定案）：① session-rail 已先行交付（2026-09-11，见 [CHANGELOG.md](CHANGELOG.md) §v0.5.0「chat 左侧活跃会话线列表」）；⑤ 与 P6 WebUI 批次的关系是伪问题（P6 已交付归档），删除；② bound path/任务线可视化——session-rail + `[scope]` 文本行已覆盖，留观（触发条件：图形化需求实际出现）；④ 运行状态提示（busy/工具调用/steer/todo 进度呈现）——UX 润色，留观（触发条件：日常使用中实际造成困扰）。③ 审批卡片立项展开（唯一实候选：QQ 侧 v0.5.2 已有内联审批按钮先例，WebUI 主界面还在聊天流打 `/ok`）：
  - [x] **AC1 · 后端审批状态端点**（2026-09-23，`web/mod.rs::get_approvals` + 路由 `GET /api/approvals`）：列出待审批（id、工具名、参数摘要、workspace 内外、频道、注册时间）。**形态按 AC3 实况收窄为只读列表**：动作不走 HTTP——`run_turn` 的续跑输出必须回到发起连接（`current_turn` / `stop` / outbox 全在 WS handler 手里），POST 就得另造一套广播 sink + busy/stop 协调，等于把已有通路复制一遍
  - [x] **AC2 · 前端审批卡片**（2026-09-23，`channels/web.rs` + `web/static/{index.html,app.js,theme.css}`）：聊天流内渲染待审批项为卡片（工具名 + 范围 + 参数摘要 + 批准/拒绝按钮），结果回灌聊天流并与 `/ok` `/deny` **严格同源**——按钮只发 `{type:"approval",id,approve}` 帧，后端 `web.rs::approval_frame_command` 把它翻成 `/ok <id>` / `/deny <id>` 走原 `SlashOutcome::Resume` 续跑；`/ok` 文本命令保留为兜底（两处共用 `ApprovalGate`，互不冲突）。刷新/换设备后靠 AC1 端点补回卡片，已在别处解析的卡片标记为失效。事件侧 `TurnEvent::ApprovalRequested` 扩为带 `tool_name/summary/within_workspace`（sink 回调改收 `ApprovalRequest`），QQ 只用 id 不受影响
  - [x] **AC3 · 实现前过一遍 approval 通路**（2026-09-23）：核过 `ApprovalContext` / `approval_decision` / `resolve_approval` / QQ 内联按钮（`msg_type=2` 键盘 + `ap:ok:<id>` 回调解码）——两条既有事实决定了上面两处形态收窄：① 审批是 **deferred 语义**（pending 注册后本轮即结束，非阻塞等待），所以卡片天然可以事后点击，不依赖 turn 存活；② 卡片动作**不该新增解析逻辑**，翻译成 `/ok` `/deny` 即为最大复用。回写修订已落进 AC1/AC2 条目
- **MCP 新 spec 兼容——已拆 A/B**（2026-09-12 调研，2026-09-22 grill 定案）：新 spec（2026-07-28）删 initialize 握手 + `Mcp-Session-Id`（SEP-2567/2575），每请求自带 `_meta`（protocolVersion/clientInfo/capabilities），Streamable HTTP 须带 `Mcp-Method`/`Mcp-Name` 头（SEP-2243），另增可选 `server/discover`、list 响应 `ttlMs` 缓存提示（SEP-2549）。**背景修正（grill 2026-09-22）**：原「HTTP session 管理被删对 llaia 是利好」前提不成立——`HttpTransport` 已是 Streamable HTTP（2025-06-18 风格），session 管理仅 ~40 行且工作正常；真实暴露面在 **stdio**：新 SDK（Python mcp 2.0 起）上的 server 可能对 `initialize` 回 -32601，现 `client.rs::handshake` 失败即 server dead。在用 server（blender-mcp / sketchup-mcp）均为 stdio；sketchup-mcp 已遇 SDK 2.0 破碎（`--with mcp==1.3.0` pin 缓解——属 server 侧 Python API 破碎，非协议层，llama 无感）。**定案：以旧协议为主，只做 stdio 兜底（A）；HTTP 双协议（B）留观**。
  - [x] **A1 · stdio `-32601` 容忍降级**（2026-09-22，`mcp/client.rs::handshake` + `mcp/transport.rs::TransportError::JsonRpc`）：`initialize` 收 -32601 → log warn（一行）→ 跳过握手直接发 `tools/list`；降级后所有 JSON-RPC 请求带 `_meta`（protocolVersion = 新 spec 档 + clientInfo，`McpServer::with_stateless_meta`）；`notifications/initialized` 不发；旧协议握手主路径不动
  - [x] **A2 · 协商版本记录 + 校验**（2026-09-22，`McpServer::record_negotiated_version` + `negotiated_version` 字段）：initialize 成功路径不再丢弃响应——记录 server 协商的 `protocolVersion`，不在支持集合 → warn，不静默
  - [x] **A3 · 版本集合**（2026-09-22，`mcp/protocol.rs`）：`MCP_PROTOCOL_VERSION`（2024-11-05）保留为握手默认档，`MCP_SUPPORTED_PROTOCOL_VERSIONS` 五档（含 `MCP_STATELESS_PROTOCOL_VERSION = "2026-07-28"`）
  - [x] **A4 · 测试**（2026-09-22，`mcp::client::tests`）：mock stdio transport（不 spawn 真实子进程）两态（完整握手 / initialize 回 -32601）+ 降级后 tools/list 与 tools/call 的 `_meta` 断言 + 协商版本越界 warn 不拒连 + 非 -32601 错误仍失败
  - **B 留观（不定案、无勾选）**：HTTP 无状态/auto/legacy 双协议协商、四档版本集合、SSE transport 标记 deprecated。触发条件：真要接 HTTP server，或想用的 server 只发新 spec HTTP。届时先拍 auto 判定信号（白名单：400 缺 session / 404 session 不存在 / -32601；**401/403、5xx、超时一律不回退**——auth 配错与 server 挂了都不是协议不匹配）再实施。
  - **不做项（留档）**：MCP server 模式（纯 client 定位不变）、OAuth/CIMD、Tasks/MRTR/MCP Apps 扩展、ttlMs tools/list 缓存（工具列表握手后缓存到重连即可，中途变化极罕见，过期刷新的时序问题配不上收益）、WebUI 降级状态位（用户定 2026-09-22：log warn 足够）。

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
