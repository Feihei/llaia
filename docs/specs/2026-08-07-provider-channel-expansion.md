# Provider 与 Channel 扩展 — 评估与设计

- 日期：2026-08-07
- 状态：✅ 第一批（快赢）+ 第二批（Telegram / Anthropic / 钉钉 / 微信 ClawBot）已实现（2026-08-07）；第三批及后续项见 plan.md P4
- 背景：P3 主线完成后，下一步主线是横向扩展 provider（厂商 API 直连）与 channel（接入平台）。本文基于 `.ref/`（zeroclaw / AstrBot / goose）调研给出可行性评估、技术路线与排期建议。

## 一、Provider 扩展

### 现状

`src/provider/mod.rs` 的 `Provider` trait 很干净：`chat` / `chat_stream` / `native_tool_calling` / `detect_context_size` 四方法，数据模型（`ChatMessage` / `ToolCall` / `StreamEvent`）是标准 OpenAI 形态。新增厂商 = 新增一个 trait 实现 + 消息格式转换层，**不需要动 Agent 层**。

### 目标厂商与难度

| Provider | 协议要点 | 难度 | 估算 |
|---|---|---|---|
| Anthropic（Messages API） | system 提到顶层；tool_use / tool_result content blocks；SSE 事件流（`message_start` / `content_block_delta` 等）；原生 tool calling | ★★☆ | 600-900 行 |
| Google Gemini（REST） | `generateContent` + `functionDeclarations`；REST 直连免 SDK | ★★☆ | 600-900 行 |
| OpenAI Responses API | 与 chat completions 不同的端点 / 事件模型（reasoning items、structured output） | ★★★ | 1000+ 行，缓做 |

**关键判断**：

1. OpenRouter 及多数聚合网关用 OpenAI 兼容协议即可路由到 Claude / Gemini，所以三家直连的实际收益排序：**Anthropic（官方订阅用户多）> Gemini > Responses**。
2. **不整体引入 zeroclaw-providers**：单文件虽成熟（anthropic.rs 3192 行），但耦合 `zeroclaw-api` / `zeroclaw-config` / `zeroclaw-log` / `zeroclaw-spawn` 四个内部 crate，且自带 OAuth / 计费 / 路由层，违背 LLAIA 轻量原则。
3. **推荐"参考移植"**：zeroclaw 为 Apache-2.0 / MIT 双许可，允许复制。参考其 payload 构造与 SSE 解析逻辑，按 llaia 的 `StreamEvent` 模型自写精简版（注明出处）。

### 设计要点

- `[provider.<id>]` 增加 `type` 枚举：`openai_compatible`（现有）/ `anthropic` / `gemini`，config.rs 的 ProviderType 按 type 分发构造
- Anthropic / Gemini 均支持 native tool calling，标签降级模式不适用（`native_tool_calling` 恒 true）
- 流式统一映射到现有 `StreamEvent`（TextDelta / ToolCall / Done / Error）
- WebUI provider 表单按 type 动态显示字段（anthropic 需 api_key + base_url 默认 `https://api.anthropic.com`；gemini 需 api_key + 可选 base_url）

## 二、Channel 扩展

### 现状

`src/channels/mod.rs` 的 `Channel` trait 极简：`run(Arc<Self>, registry)` 阻塞循环，每个 channel 自管 I/O。新增 channel = 一个实现 + config section + `chat_cmd`/`serve_cmd` 的 spawn 分支。

### 复用结论（对应 P3+ "评估借用 zeroclaw 代码"）

**值得借鉴、不值得依赖**：

- zeroclaw-channels 覆盖面广（telegram / slack / discord / line / dingtalk / lark / wechat / wecom / whatsapp / email…），许可（Apache-2.0 / MIT）允许复制
- 但直接引 crate 会拖进 `zeroclaw-api`（ChannelMessage / SendMessage / allowlist）+ `zeroclaw-config` schema 依赖树（channels Cargo.toml 含 matrix-sdk / nostr-sdk / lapin 等重依赖，虽 feature 可选）
- 且 zeroclaw 实现是全功能的（群聊 / slash commands / 语音 / 审批按钮），telegram 7.2k / slack 9.1k / discord 12.9k 行，**单用户私人助理场景可砍 70% 以上**
- 正确姿势：**单文件 vendor + 裁剪适配** llaia 的 Channel trait；AstrBot（Python）当协议行为参考文档用
- 例外：dingtalk 仅 554 行（Stream Mode WS），可直接移植改造

### 各平台难度

| Channel | 难度 | 说明 |
|---|---|---|
| **微信 ClawBot** | ★★☆ | 腾讯官方 `openclaw-weixin`（ilink bot）接口，见下文专节；个人绑定、契合定位 |
| **Telegram** | ★☆☆ | 官方 Bot API + long polling，免公网回调，业界最简单接入 |
| **钉钉** | ★☆☆ | Stream Mode WS 免公网，zeroclaw 554 行参考，国内首选 |
| **邮箱** | ★★☆ | IMAP 轮询 + SMTP，`lettre` + `async-imap` 生态成熟（还 P2-e 欠账） |
| **飞书 / Lark** | ★★☆ | 自建应用事件订阅（长连接模式可免公网），zeroclaw 6.2k 行可大幅精简 |
| **Slack** | ★★★ | Socket Mode 免公网，但交互概念重，砍功能后中等偏上 |
| **Discord** | ★★★ | Gateway WS + REST，原生无 SDK |
| **WhatsApp** | ★★★ | Cloud API 需 Meta 企业配置；whatsapp-web 协议自实现风险高，不建议 |
| **LINE** | ★★☆ | zeroclaw 2.5k 行，国内用户少，优先级低 |
| 微信个人号（非官方协议） | — | **不碰**：封号风险（与 ClawBot 官方路线是两回事） |

### 微信 ClawBot 专节（重点）

**调研来源**：AstrBot `astrbot/core/platform/sources/weixin_oc/`（约 2100 行 Python，v4.22.0 引入）+ `docs/zh/platform/weixin_oc.md`。

**协议形态**（非官方逆向，是腾讯官方 ClawBot 插件接口）：

1. **登录**：`GET {base_url}/ilink/bot/get_bot_qrcode?bot_type=3` 拿二维码（`qrcode` + `qrcode_img_content`）→ 轮询 `GET ilink/bot/get_qrcode_status?qrcode=<id>`（长轮询 35s）→ `status=confirmed` 返回 `bot_token` + `ilink_bot_id` + `ilink_user_id`
2. **收消息**：HTTP 长轮询（sync_buf 增量游标），无需公网回调
3. **发消息**：`POST ilink/bot/sendmessage`；另有 `sendtyping`（输入状态）、`getconfig`
4. **媒体**：CDN（`https://novac2c.cdn.weixin.qq.com/c2c`）上传/下载，AES-128-ECB + PKCS7 加解密；响应头 `x-encrypted-param` 作为下载凭据
5. **登录态持久化**：`token` + `account_id` + `sync_buf` + `context_tokens` 落盘，重启免扫码（token 失效才重新扫）
6. 默认 base_url：`https://ilinkai.weixin.qq.com`
7. 前置条件：手机微信含 ClawBot 插件（iOS ≥ 8.0.70 / Android ≥ 8.0.69）

**消息能力**：文本 / 图片 / 视频 / 文件 收发；语音仅接收（微信云端自动转录成文本）。

**LLAIA 移植设计**：

- 新文件 `src/channels/wechat.rs`，估算 600-800 行（client 层照搬 AstrBot 279 行结构，reqwest 实现）
- 依赖：`aes` crate（ECB + PKCS7）；二维码展示走 WebUI（登录流程 API：申请二维码 → 前端展示 → 轮询状态）+ CLI 降级打印二维码 URL
- config：`[channels.wechat]` enabled / token / account_id / sync_buf / base_url（token 等敏感项支持 `${VAR}` 引 `.env`）
- 登录态写回 config.toml（或独立 `wechat_state.json`，避免敏感 token 与配置混写——倾向独立文件）
- 单用户假设简化：不需要 AstrBot 的会话缓存 / 群聊逻辑 / allowlist，只服务绑定的主人账号

## 三、排期建议

**第一批（快赢，各半天到一天）**

1. `/provider` 斜杠命令（运行时切换模型，不写 config）
2. model fallback（provider 链降级）
3. WebUI 重启按钮（serve 自重启）

**第二批（扩展主线）**

4. Telegram channel（最简单，先打通"第二个 channel"完整链路）
5. Anthropic provider
6. 微信 ClawBot channel（国内主场景，官方接口无封号风险）

**第三批**

7. 邮箱 channel（还 P2-e）
8. Gemini provider / 钉钉 / 飞书 / Slack

**明确不做（近期）**：OpenAI Responses（聚合网关可绕过）、WhatsApp 自实现、微信个人号非官方协议。

## 四、参考索引

- zeroclaw-providers：`.ref/zeroclaw/crates/zeroclaw-providers/src/`（anthropic.rs / gemini.rs / openai.rs / compatible.rs）
- zeroclaw-channels：`.ref/zeroclaw/crates/zeroclaw-channels/src/`（dingtalk.rs 554 行 / telegram.rs / slack.rs / email_channel.rs 1717 行）
- zeroclaw 抽象：`zeroclaw-api::channel::Channel`（send/listen 分离式，与 llaia run 模型不同，适配层薄）
- AstrBot weixin_oc：`.ref/AstrBot/astrbot/core/platform/sources/weixin_oc/` + `docs/zh/platform/weixin_oc.md`
- AstrBot platforms：`.ref/AstrBot/astrbot/core/platform/sources/`（telegram / dingtalk / lark / slack / discord / line / wecom…）
