# P5 剩余项评估与实施计划

> 日期：2026-08-17
> 范围：`docs/plan.md` P5 中 6 个未实施条目（WebUI 会话历史、WebUI 模型探测、敏感信息 .env、环境探测、自然对话 MCP、TTS）
> 状态：⏳ 待评审（决策点见文末 §5）

---

## 0. 结论先行

- **6 项中 1 项现状已实质完成**（自然对话 MCP，ADR-0014 交付时已把 MCP 工具接入主 agent 工具集），剩余可选项价值有限，建议**勾选 + 收尾**而非新建工程。
- **5 项真正待做**，按风险分层推荐顺序：**环境探测 → WebUI 会话历史 → WebUI 模型探测 → 敏感信息 .env → TTS**。
- 最值得投入的是 **敏感信息 .env 自动化**（必要性「高」，安全债）与 **WebUI 会话历史**（日常使用高频诉求）；两者恰好分别是最难（动配置保存核心路径）与次难（动 sqlite 只读/写边界）的项，都放到环境探测与模型探测之后，先积累信心。
- 每项均为**自包含模块**，不依赖其他项，可独立排期、独立 commit。

---

## 1. 未实施项总览

| # | 条目 | 必要性 | 难度 | 风险面 | 依赖 | 推荐排期 |
|---|---|---|---|---|---|---|
| E1 | 环境探测 | 中 | ★☆☆ | 极低（只读 + timeout） | Runtime Context 机制（已有） | **第 1** |
| W1 | WebUI 会话历史查询/修改 | 中 | ★★★ | 中（sqlite 读写边界 + 破坏性删除） | SessionStore（已有） | **第 2** |
| W2 | WebUI provider 模型探测 | 中 | ★★☆ | 低（只读网络请求） | Provider 层 / reqwest | **第 3** |
| S1 | 敏感信息自动写 .env | **高** | ★★☆ | **中高（动配置保存核心路径）** | ${VAR} 展开（已有） | **第 4** |
| M1 | 自然对话 MCP | 中 | ★★☆ | 视决策（见 §3.5） | MCP 框架（已完整） | 收尾 |
| T1 | TTS 服务接入、发语音 | 低 | ★★☆ | 低-中（外部服务 + 格式兼容） | MediaOutput 链路（已有） | **第 5** |

> 难度修正说明：W1 从 plan.md 的 ★★☆ 上调为 ★★★——「修改」语义涉及 sqlite（source of truth）与内存 Context（运行时窗口）不一致的边界问题，须先定只读/可编辑策略（§3.1 决策 1）。

---

## 2. 推荐实施顺序（风险分层）

| 序 | 项 | 理由 |
|---|---|---|
| 1 | E1 环境探测 | 自包含、只读、零回滚成本，收益即时（agent 少说错话） |
| 2 | W1 会话历史 | 独立模块，不动 agent 主循环；先只读后编辑，删除有兜底 |
| 3 | W2 模型探测 | 只读网络请求 + 前端表单，不碰配置保存路径 |
| 4 | S1 .env 自动化 | **风险最高**（改 PUT /api/config 核心路径 + 回显脱敏 + 存量迁移），放最后动核心 |
| 5 | T1 TTS | 必要性低，独立小模块，按需排期 |
| — | M1 自然对话 MCP | 现状核实后勾选，剩余可选项并入评审 |

---

## 3. 逐项细化设计

### E1 环境探测（★☆☆）

**现状**：完全不存在。Runtime Context 注入机制已成熟——`Context.todo_state` / `Context.goal_state` 在 `to_messages` 尾部追加 user 消息（`src/agent/context.rs`），不进 system 前缀，KV 缓存友好，逐轮字节稳定。

**设计**：

- 探测内容（进程启动时一次 + `/env` 手动刷新）：
  - `shell`：`$SHELL`（Unix）；Windows 下探测 `powershell.exe` / `cmd.exe` / git-bash
  - 工具链：`python` / `node` / `npm` / `rustc` / `cargo` / `go` / `git` / `docker`，逐个 `<cmd> --version` 探存在性与版本
  - 每命令带 timeout（2s），只列出**存在且版本可解析**的项，控制注入体积 ≤ 2 行
- 注入方式：新增 `Context.env_state: Option<String>`，与 todo/goal 同机制尾部注入。示例：

  ```
  [env] python 3.13.2 · node 22.22.2 · git 2.47.1 · docker 27.1.1
  ```

- 刷新时机：进程启动探测一次；`/env` 斜杠命令手动重探；**不做**每轮自动重探（开销不值）。
- 实现：新模块 `src/envprobe.rs`，`tokio::process::Command` + timeout；Windows 用 `where.exe` 定位命令再跑 `--version`（避免依赖 PATH 猜测）。

**风险**：极低。只读探测 + timeout；命令存在但 `--version` 非零退出 → 不列出。

**工作项**：

- [ ] `src/envprobe.rs`：跨平台探测 + 版本解析 + 单测（格式/超时/缺失命令）
- [ ] `Context.env_state` + `to_messages` 尾部注入（复用 todo/goal 模式）
- [ ] 斜杠命令 `/env` 手动刷新
- [ ] 启动接入：cli.rs / serve 初始化时探测一次，写入 agent context

---

### W1 WebUI 会话历史查询/修改（★★★）

**现状**：

- `SessionStore`（`src/memory/sqlite.rs`）schema 完整：`sessions` / `messages` / `tool_calls` / `kv` 四表，WAL；sessions.db 是会话历史的 **source of truth**（上下文压缩后旧消息只留 sqlite）。
- WebUI 无任何历史 API；前端为原生 JS（`app.js` 656 行，Alpine 风格 `llaiaApp()`），已有 config/chat/about/cron/mcp/skills/todo 等 tab。
- **关键边界**：sqlite 与 agent 内存 `Context.history` 是两套——改 sqlite 只影响留底，**不会**同步到运行时内存上下文。

**决策 1：只读 v1 vs 可编辑 v2**（影响「修改」语义，须先定）

- **A（推荐 v1）：只读浏览 + 删除 + 导出**。会话列表、消息详情、删除会话（cascade）、导出 JSON/Markdown。零一致性风险。
- **B（v2）：消息编辑**——仅落 sqlite，前端明示「修改只影响历史留底，当前对话上下文不受影响」。要同步进内存须触发上下文重建（`/new` + 从 sqlite 回灌），成本高、收益低，**不做**。
- 结论：**v1 只读 + v2 选择性编辑**，删除操作提供二次确认。

**决策 2：删除范围**

- 删除单会话（cascade 删 messages/tool_calls）——允许。
- 清空全部会话——提供二次确认 + 导出提示，默认不做一键清空（防误触）。

**API 设计**：

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/sessions?limit&offset` | 会话列表（含消息数、token 数） |
| GET | `/api/sessions/:uuid` | 单会话完整消息（tool_calls 合并展示） |
| DELETE | `/api/sessions/:uuid` | 删除会话（cascade） |
| GET | `/api/sessions/:uuid/export` | 导出 JSON / Markdown |
| PUT | `/api/sessions/:uuid/messages/:id` | （v2）编辑消息内容，仅落 sqlite |

参考 AstrBot：会话列表（时间/渠道/消息数）+ 展开详情 + 删除/清空管理。

**前端**：新增 `Sessions` tab：左列表（时间倒序）+ 右详情（role 徽标着色、tool_calls 折叠、时间戳）；删除按钮带确认；导出按钮。

**风险**：中。删除是破坏性操作但仅影响 sqlite 留底；列表查询需分页防大库卡顿（limit 默认 50）。

**工作项**：

- [ ] `SessionStore`：`list_sessions` / `get_session_messages` / `delete_session` / `session_count` + 单测
- [ ] `web/mod.rs`：4 个 API 路由（auth 复用现有 `authorize`）
- [ ] `app.js` + `index.html`：Sessions tab（列表/详情/删除/导出）
- [ ] 集成测试（删除 cascade、导出格式）

---

### W2 WebUI provider 模型探测（★★☆）

**现状**：Config 页已有 provider 表单与结构化保存（`PUT /api/config` toml_edit 定点合并）；`Compat::detect` 已能按 base_url 启发式识别 Ollama/Llama.cpp 等；`GET /api/status` 不含 provider 探测。

**决策 1：探测协议覆盖范围**

- **A（推荐 v1）：仅 OpenAI 兼容端点**——`GET {base_url}/models`（Ollama / Llama.cpp / LM Studio / OpenRouter / doubao / 百度 / OpenAI 官方全支持），覆盖 llaia 主力场景。
- **B（v2）：Gemini**——`GET https://generativelanguage.googleapis.com/v1beta/models?key=...`，实现成本低但需单独 key 字段；Anthropic **无公开 models 列表端点**，不做探测（走手输 + 文档示例列表）。
- 结论：v1 只做 OpenAI 兼容，B 项留作后续。

**决策 2：模型添加流程**

- 探测返回 `[{id, name}]` → 前端渲染可勾选列表 → 勾选生成 `[provider.<id>].model.<alias>`（alias 自动取模型名尾段，可改）→ 走已有 `PUT /api/config` 保存（**不新增**保存路径，避免破坏定点合并）。
- 手动输入模型名时的「可用性检查」：直接比对已探测列表或轻量 `GET /models` 匹配，失败提示「未在列表中发现，可强制添加」。

**API 设计**：

- `POST /api/providers/:id/models`，body 可选覆盖 `{ base_url, api_key }`（默认用当前配置）→ `{ ok, models: [{id, name}], error? }`；reqwest 超时 10s；错误归一化（连接失败 / 401 / 无 models 端点）。
- Ollama 特殊：`/v1/models` 与 `/api/tags` 均可用，优先 `/v1/models` 保持兼容路径一致。

**前端**：Config 页 provider 编辑区加 `Probe models` 按钮 → 列表展示（探测到 Compat 自动适配的端点标「auto-compat」徽标）→ 勾选添加。

**风险**：低。只读网络请求，超时兜底；不碰配置保存路径。

**工作项**：

- [ ] `src/provider/probe.rs`：模型探测（OpenAI-compat `GET /models`）+ 单测（mockito mock）
- [ ] `web/mod.rs`：`POST /api/providers/:id/models`
- [ ] `app.js`：探测按钮 + 勾选添加流程
- [ ] 集成测试（超时 / 401 / 空列表）

---

### S1 敏感信息自动写 .env（★★☆）— 必要性「高」

**现状**：

- `${VAR}` 展开机制完备（`expand_string`，`src/config.rs:851`）；main.rs 启动加载 CWD + `config_dir/.env`（dotenvy）；`llaia init` 已生成 .env 模板。
- **缺口**：WebUI 保存配置时敏感字段**明文落入 config.toml**，无自动转存 .env；`GET /api/config` 原样回显明文。

**决策 1：敏感字段清单**（写死的路径集合）

| 字段 | 路径 |
|---|---|
| provider API key | `provider.*.api_key` |
| 频道凭证 | `channels.qq.app_secret`、`channels.telegram.bot_token`、`channels.dingtalk.client_secret`、`channels.mail.imap_pass`、`channels.mail.smtp_pass`、`channels.feishu.app_secret` |
| 搜索 key | `tools.tavily.api_key` / `tools.baidu.api_key` / `tools.brave.api_key` |
| WebUI 访问令牌 | `webui.token` |

（`app_id` / `client_id` / `imap_user` 等非密钥字段可留明文，减少噪音。）

**决策 2：自动转存 vs 显式迁移**

- **保存时自动转存**（推荐）：`PUT /api/config`（结构化）与 `PUT /api/config/raw`（原始 toml）两处拦截——敏感字段值为**非空明文且不匹配 `${[A-Z_][A-Z0-9_]*}`** 时：生成变量名 `LLAIA_{SECTION}_{ID}_{FIELD}`（大写 + 非字母数字 sanitize）→ 幂等写入 `config_dir/.env`（同名 key 覆盖，保留其他行）→ config.toml 写 `${VAR}` 引用。
- **降级策略**：.env 写入失败 → 仍按明文保存 + warn，保证服务可用（不因安全改造卡死配置流程）。
- 存量迁移：启动时扫描 config.toml 明文敏感字段 → 仅 **log warn 提示**（不自动迁移，避免启动失败风险），提供 `/migrate-secrets` 斜杠命令一键迁移。

**决策 3：回显脱敏**

- `GET /api/config` 返回时：非 `${VAR}` 的敏感字段 → 返回**空串 + `masked: true`** 标记；前端显示星号占位，**空输入提交 = 保留原值**（不覆盖）。
- 已配 `${VAR}` 引用的字段：原样返回引用（不泄露值）。

**决策 4：二进制存储——不做**

理由：.env + 文件权限（Unix chmod 600）已满足「避免明文进 config.toml」核心诉求；二进制存储引入 key 管理问题（key 存哪？），单用户本地场景收益低。未来若多用户/服务器部署，再评估 OS keyring（keyring crate）。

**风险**：中高。改配置保存核心路径；重点回归：to ml_edit 定点合并不被破坏、热加载正常、脱敏不回显、降级路径可用。

**工作项**：

- [ ] `src/config/secrets.rs`：字段清单 + 变量名生成 + .env 幂等读写 + 单测
- [ ] `PUT /api/config` / `PUT /api/config/raw` 两处转存拦截
- [ ] `GET /api/config` 脱敏（masked 标记）+ 前端星号占位/空值保留
- [ ] `/migrate-secrets` 斜杠命令 + 启动扫描 warn
- [ ] 集成测试：转存幂等、冲突 key、降级、脱敏

---

### M1 自然对话 MCP（★☆☆ — 现状核实后大幅降级）

**现状核实（2026-08-17）**：

- MCP 框架已完整（ADR-0014：client/protocol/transport/registry）。
- **MCP 工具已实质进入主 agent 工具集**：`src/channels/cli.rs:482` `all_tools.extend(mcp_tools)`；WebUI 改 MCP 配置后经 `replace_mcp_tools` 热加载（`src/web/mod.rs:572`）。
- 即「配置好 MCP server → 主 agent 在自然对话中直接调用其工具」**已成立**。

**决策分支**：

- **A（推荐）：条目勾选为已完成**，并把 plan.md 措辞改为「已交付（ADR-0014）」。
- **B：agent 自主配置 MCP server**（对话中说「接上我的 XX」→ agent 自动写 MCP 配置 + 热加载）——需新增 `mcp_install` 类工具；对单用户私人助理价值有限（MCP server 选型通常需用户判断），且 agent 执行任意 `npx` 命令有供应链风险。**不推荐**。
- **C：MCP 工具描述增强**（把 schema 生成的生硬 description 归一化）——收益低。**不做**。

**工作项**：

- [ ] plan.md 勾选 + 措辞更新（M1 收尾，无代码改动）

---

### T1 TTS 服务接入、发语音（★★☆ — 必要性「低」）

**现状**：`send_image` / `send_file`（`src/tools/send_media.rs`）与 MediaOutput 事件链路完整；无 TTS 合成能力。

**决策 1：TTS provider**

- **A（v1 已实施，2026-08-17 修订）：OpenAI TTS API**——`POST /audio/speech`（OpenAI 兼容端点，可配 base_url），实现简单稳定、mock 可测。原计划拟用 edge-tts，实为 **WebSocket + Sec-MS-GEC 签名协议**（非 HTTP 接口），不可测且接口脆弱易变，**降级为 v2 待研究**。
- **B（v2）：edge-tts**——免费、中文音质好、无需 key；需 WS 握手 + Sec-MS-GEC 签名，留待后续验证。
- 本地引擎（espeak-ng / piper）离线但音质/安装成本差，**不做**。

**决策 2：合成与发送分离**（对齐 send_image 模式）

- 新工具 `tts { text, voice? }`：合成到 `workspace/tts/<uuid>.mp3`，返回路径（`requires_confirm: false`，只写 workspace 内）。
- 发送：复用 `send_file` 走 MediaOutput；WebUI 按扩展名（.mp3/.wav/.ogg/.m4a）渲染 `<audio>` 播放器。
- 默认 voice：`alloy`，`[tools.tts]` 可配（enabled / base_url / api_key / model / voice）。

**决策 3：channel 格式兼容**

- WebUI：`<audio>` 直接播 mp3 ✅
- Telegram：mp3 可发（客户端转码）✅
- QQ：需 silk 转码，**v1 不做**（文档标注不支持，agent 侧提示降级为文字+链接）。

**配置**：`[tools.tts]`（`enabled` 默认 false、`provider` 由 base_url 决定、`voice`、`api_key` 可选）。

**风险**：低-中。外部服务依赖（OpenAI 接口变更 → 降级为提示）；需 api_key（edge-tts v2 可免 key）。

**工作项**：

- [ ] `src/tools/tts.rs`：edge-tts 实现（HTTP + 落盘 + 超时）+ 单测（mock 音频响应）
- [ ] `[tools.tts]` 配置接入 + 工具注册
- [ ] WebUI `<audio>` 播放支持（聊天页音频消息渲染）
- [ ] 集成测试（合成→发送链路，mock 服务）

---

## 4. 分阶段实施计划（checkbox）

> 每个 Task 完成后跑 `cargo test` + `cargo clippy`；一个功能链路验证通过后提交一次（项目约定）。

### Stage 1 — E1 环境探测（★☆☆）

- [ ] `src/envprobe.rs` 探测逻辑 + 单测
- [ ] `Context.env_state` + 尾部注入 + 单测
- [ ] `/env` 斜杠命令
- [ ] 启动接入（cli.rs / serve）

### Stage 2 — W1 WebUI 会话历史（★★★）

- [ ] SessionStore 查询/删除方法 + 单测
- [ ] API：list / detail / delete / export
- [ ] 前端 Sessions tab
- [ ] 集成测试（cascade、导出、分页）

### Stage 3 — W2 WebUI 模型探测（★★☆）

- [ ] `src/provider/probe.rs` + 单测（mockito）
- [ ] `POST /api/providers/:id/models`
- [ ] 前端 Probe 按钮 + 勾选添加
- [ ] 集成测试

### Stage 4 — S1 敏感信息 .env 自动化（★★☆）

- [ ] `src/config/secrets.rs` + 单测
- [ ] PUT 两处转存拦截
- [ ] GET 脱敏 + 前端占位
- [ ] `/migrate-secrets` + 启动扫描 warn
- [ ] 集成测试（转存/冲突/降级/脱敏）

### Stage 5 — T1 TTS（★★☆，按需）

- [ ] `src/tools/tts.rs` edge-tts + 单测
- [ ] `[tools.tts]` 配置 + 注册
- [ ] WebUI 音频播放
- [ ] 集成测试

### Stage 6 — M1 自然对话 MCP 收尾（无代码）

- [ ] plan.md 勾选 + 措辞更新

---

## 5. 决策点汇总（供集体评审）

| # | 决策 | 选项 | 推荐 |
|---|---|---|---|
| D1 | W1 会话历史「修改」语义 | 只读 v1 / 可编辑 v2（仅落 sqlite） | **v1 只读 + v2 选择性编辑**；编辑不同步内存 Context |
| D2 | W1 删除范围 | 单会话 / 一键清空 | 单会话可删；清空需二次确认，默认不做 |
| D3 | W2 探测协议覆盖 | 仅 OpenAI-compat / +Gemini / +Anthropic | **v1 仅 OpenAI-compat**；Anthropic 无端点不做 |
| D4 | S1 转存时机 | 保存自动转存 / 显式迁移按钮 | **保存时自动转存 + 降级明文**；存量靠 `/migrate-secrets` 手动迁移 |
| D5 | S1 回显策略 | 明文回显 / 星号+留空不变 | **脱敏**：非 `${VAR}` 敏感字段返回空 + masked，空输入=保留原值 |
| D6 | S1 二进制存储 | .env / sqlite 加密 / OS keyring | **.env**（不做二进制，key 管理无解） |
| D7 | E1 注入方式 | Runtime Context 每轮注入 / system prompt 一次性 | **Runtime Context**（复用 todo/goal 机制，KV 缓存友好） |
| D8 | M1 自然对话 MCP | 勾选完成 / agent 自主配置 / 描述增强 | **勾选完成**（现状已支持）；B/C 不做 |
| D9 | T1 provider | edge-tts / OpenAI TTS / 本地引擎 | **OpenAI TTS v1（已实施，edge-tts 为 WS+签名协议不可测，降级 v2）**；QQ silk 转码不做 |

---

## 附：范围外（不在本次评估内）

plan.md 注中提到的「WebUI 增强、敏感信息存储、环境探测、TTS、自然对话 MCP」之外，尚有：

- 搜索 provider 的 doubao（豆包）——ADR-0023 已明确因仅 MCP/SigV4 接入暂缓，不在本次范围。
- 若有其他 P5 候选（来自 `docs/issues/`），需单独建 ADR/plan 再进队列。
