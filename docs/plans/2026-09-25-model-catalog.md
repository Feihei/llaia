# 模型目录重构（Model Catalog）：provider 解耦 + model kind 分型

状态：**已定案，未实施**（2026-09-25，三轮 grill 定案）
日期：2026-09-25
关联：`docs/adr/0008-config-schema-v1.1.md`（provider/model 配置模式，本重构将其再次演进）、`docs/adr/0026-provider-compat.md`（compat 覆盖层）、`docs/plans/2026-09-09-model-enabled-toggle.md`（serde 序列化方向教训）、`docs/plans/2026-09-14-provider-compat.md`

## 背景与问题

模型能力正在从 chat 单一形态扩展：`[tools.tts]`、`[tools.image_gen]` 已各自 inline 了 `base_url/api_key/model`，未来 embedding（RAG 留观，触发即需）还会再来一份。inline 配置的代价已经显性化：

- **api_key 四件套重复**：每加一个带模型的工具就要过 `expand()`（`config.rs`）→ `secrets.rs` 注册 → `web/mod.rs` mask+merge → `index.html` 手写表单，已重复两轮。
- **无统一模型目录**：模型散落在 provider 子表与各 tools 段里，WebUI 没有统一视图，agent 与 tools 的选模体验不一致。
- **职责混杂**：provider 表同时承担端点连接与 chat 模型定义（flatten 子表），非 chat 能力没有自然归宿。

目标：`[provider]` 收窄为**端点 + 凭据**，模型独立成 `[model.<id>]` 目录，按 kind 分型；agents 与 tools 一律从目录引用，选模体验对齐。

## 设计前提（用户已定，本文不推翻）

1. **kind 在 model 上，不在 provider 上**。OpenAI 兼容端点天然多能力共存（同一 base_url 下 chat/embedding/tts/image），provider 级 kind 会逼用户为同一端点建 N 条重复条目、key 复制 N 份。
2. **不做兼容过渡，一步到位**。用户极少（主要自用），不保留旧 `[provider.<id>.<alias>]` 双读；但必须配**加载期检测报错安全网**（见下）。
3. **批量添加直接删除**，无保留草稿态的折中。添加一律走 probe → 选 kind → 填字段单条流程。
4. **capabilities v1 仅 `multimodal`**，但结构用可扩展数组，未来能力位直接追加。
5. **probe 未选 kind 时默认 chat**：SiliconFlow 等提供商的模型列表混入大量非 LLM 模型，缺省归 chat 符合大多数情况，用户可改。
6. **搜索类服务进 provider 注册表，统一凭据**（2026-09-25 二轮修订，推翻初版"留 tools"）：tavily/baidu/brave 收进 `[provider]`，api_key 与模型 provider 走同一条 expand/secrets/mask 管线；**不产生 model 条目、不进添加模型的 probe 列表**。tools 侧从目录引用。
7. **provider 不新增 kind 字段，`type` 即判别器**：llm family `{openai_compatible, anthropic, gemini}` 可挂 model、进 probe；service family `{tavily, baidu, brave, …}` 纯凭据。缺省（未写 type）仍回退 `openai_compatible`；**显式未知 type 从静默回退改为报错**（防 `"tavily"` 打错字变出一个 LLM provider 蹲进 probe 列表）。
8. **搜索商走 trait + adapter，配置同形**（2026-09-25 三轮补定）：结果列表族搜索商（tavily/brave/tinyfish/…）输出语义一致（ranked hits）但 wire 协议各异——统一为 `SearchProvider` trait + `SearchHit` IR + per-type adapter；配置层全部同形（type + api_key + 可选 base_url 覆盖默认端点），**新增一家 = adapter ~百行 + type 枚举一项，schema/WebUI/引用机制零改动**。两个边界裁决：① Perplexity 类 answer engine（返回合成答案非结果列表）v1 不接，不为想象需求造 IR 变体；② service family 内部有 search / extract 两个能力维度（jina reader、tinyfish Fetch、tavily /extract 同属 extract），能力由 type 决定（adapter 声明 `supports_extract()`），不给配置加旋钮。

## 新配置 schema

```toml
[provider.openai]
type = "openai_compatible"        # anthropic/gemini 仅 chat 可用（见约束）
base_url = "https://api.openai.com/v1"
api_key = "..."
[provider.openai.compat]          # 保留在 provider：wire 方言是端点属性，
native_tool_calling = true        # 但语义上仅作用于 chat 请求（表单加注）

[provider.my-tavily]              # service family：纯凭据，无 model 条目、不进 probe
type = "tavily"
api_key = "..."

[provider.my-brave]
type = "brave"
api_key = "..."

[model.qwen3]                     # model_id 全局唯一
provider = "openai"               # 基础属性：指向 provider
kind = "chat"                     # chat | tts | image | embedding | ...（缺省 chat）
model = "qwen3-32b"               # 服务端模型名
enabled = true
context_size = 32768              # chat 专属
thinking = { level_wire = "reasoning_effort" }   # chat 专属
capabilities = ["multimodal"]     # 可扩展能力位数组，空省略

[model.tts1]
provider = "openai"
kind = "tts"
model = "tts-1"

[model.sdxl]
provider = "sd"                   # 本地 sd-server，允许 allow_no_key 语义（沿 image_gen 先例）
kind = "image"
model = "stable-diffusion-xl"
size = "512x512"                  # kind 专属属性
```

消费端引用全部改为一元 model_id：

```toml
[agent.main]
model = "qwen3"                   # 原 "provider.alias" 两段式退役
fallback = ["local-small"]

[tools.tts]
enabled = true
model = "tts1"                    # 引用目录条目；base_url/api_key/voice 归属见下

[tools.image_gen]
enabled = true
model = "sdxl"

[tools.search]                    # 统一搜索改引用 service family provider
provider = "my-tavily"            # 原 tavily|baidu|brave 枚举 → provider id 引用
top_k = 8

[tools.web_fetch]
max_chars = 20000
extract_provider = "my-tavily"    # 原 use_tavily_extract 布尔 → 引用（空 = 本地抽取）
```

### 结构决策：单 struct + kind 校验，不用 serde 内部 tag

`ModelEntry` 用单一 struct（`kind: ModelKind` 缺省 Chat + Option 字段按 kind 归属），**不用** `#[serde(tag = "kind")]` enum——内部 tag 与 serde_json/toml 的 flatten、HashMap 混用坑多（非自明 tag 在多个序列化路径行为不一致），而 kind 专属字段的正确性用注册期校验兜住更稳：`image.size` 写在 chat 模型上 → load/put_config 时 warn + 忽略。

### kind × provider type 约束（添加时校验，不留运行时）

| provider type | 允许的 kind |
|---|---|
| `openai_compatible` | 全部 |
| `anthropic` / `gemini` | 仅 `chat` |

在 WebUI 添加流与 `put_config` 双侧校验，配出 "anthropic + image" 直接 400，别等请求报错。

### 引用过滤

- `agent.model` / `fallback` / `runtime.compact_model` / `runtime.vision_model` 下拉与校验：**只列 kind=chat**；`vision_model` 顺手过滤 `capabilities ∋ multimodal`（有目录后这是白捡的一致性）。
- `tools.tts` 引用：只接受 kind=tts；`tools.image_gen`：只接受 kind=image。
- 引用缺失或 kind 不符 → **工具不注册 + 启动 warn**（沿 `TtsTool::build` 返 None 先例），不 panic 不静默。

### `enabled` 语义原样平移

沿用 2026-09-09 定案：只管可发现性（下拉过滤），`model_from_ref` 不拦显式引用，避免 brick 当前模型。

### 加载期安全网（一步到位的代价控制）

`ProviderConfig` 移除 flatten 后开 `deny_unknown_fields`：旧配置里的 `[provider.<id>.<alias>]` 子表成为 unknown key → **启动即报错**，错误消息指引人工迁移（示例：`unknown field 'qwen3' in [provider.openai] — model tables moved to top-level [model.<id>], see docs`）。这不是兼容层，是把"静默吞掉所有模型"变成"显式失败"。量小（一两个人），改一遍配置五分钟。

## 改动面

| # | 位置 | 动作 |
|---|---|---|
| 1 | `config.rs` `ProviderConfig` | 收窄为 type/base_url/api_key/compat；移除 flatten model 表；开 `deny_unknown_fields`；`type` 扩为双 family 判别器（llm family 缺省回退保留，**显式未知 type 报错**——防打错字的 service type 变成 LLM provider 蹲进 probe 列表） |
| 2 | `config.rs` | 新增 `ModelKind`（parse + 缺省 Chat）、`ModelEntry`（provider/kind/model/enabled/context_size/thinking/max_tokens/native_tool_calling/size/capabilities）、`Config.models: BTreeMap<String, ModelEntry>`（BTreeMap 保证落盘顺序确定，HashMap 会在 replace 合并时产生无意义 diff） |
| 3 | `config.rs` 引用校验 | agent.model/fallback/compact/vision、tools.tts/image_gen 引用存在性 + kind 匹配校验；`tools.search.provider` / `tools.web_fetch.extract_provider` 引用存在性 + service family 校验；per-provider-type 字段校验（tavily/brave 只要 key 不要 base_url，llm family 必须有 base_url）与 `ModelEntry` kind 校验同一套机制 |
| 4 | `src/provider` | `provider_from_ref` → `model_from_ref`（唯一解析收口，语义沿旧）：由 model 条目取 provider 端点构建 chat client；compat 探测仍按 provider type |
| 5 | `tools/tts.rs`、`tools/image_gen.rs` | build 改读 `model` 引用；api_key 经 provider 展开；`voice`（tts）与 `size/timeout_secs`（image_gen）留 tools 段（调用参数，非模型属性）|
| 6 | `tools/search/mod.rs` | 搜索收口为 `SearchProvider` trait + 统一 `SearchHit` IR + per-type adapter（前提 8）：`[tools.search].provider` 引用解析出对应 adapter；`[tools.tavily]/[tools.baidu]/[tools.brave]` 三段删除；`tools/web.rs` 的 extract 改读 `web_fetch.extract_provider` 引用（按 adapter `supports_extract()` 校验） |
| 7 | `config/secrets.rs` + `web/mod.rs` mask/merge | 删除 tts/image_gen 与搜索 key 在 tools 层的全部 api_key 分支，凭据管线只在 provider 层一份 |
| 8 | `web/mod.rs` | probe 端点 per provider type，**只对 llm family provider 开放**（OpenAI `/v1/models`、Ollama `/api/tags`、llama.cpp `/v1/models`、Gemini ListModels、Anthropic `/v1/models`）；返回结果标注推测 kind（缺省 chat，用户可改）；models 与 provider 子树 put_config 走 replace |
| 9 | `web/static`（index.html/app.js/theme.css） | 新 **Models 选项卡**（交互稿见下节）；Provider 表单瘦身（compat 加 chat 专属标注；service family 条目只渲染 api_key 字段）；agent 表单 model/fallback 改一元下拉（kind=chat 过滤）；tts/image_gen 卡片 model 变下拉（kind 过滤），base_url/api_key 输入框删除；search 卡片 provider 变下拉（service family 过滤） |
| 10 | `slash.rs` | 命令拆分：`/models` 列模型目录（按 provider 分组，kind 徽章 + enabled 标记，`/models <id>` 切换 agent 主模型）；`/providers` 列连接注册表（llm/service 两族分列）；`/provider` 保留为 `/models` 别名转发，避免肌肉记忆断裂 |
| 11 | `commands/mod.rs` | init CONFIG_TEMPLATE 重写（provider 双 family + model 示例段）；doctor 按目录遍历，service family provider 只探 key 有效性（或跳过网络探测） |
| 12 | `channels/cli.rs` | agent init 构建链改 `model_from_ref`；compact/vision 构建同步 |
| 13 | 测试 | `config.rs` serde 往返、`provider` 解析、`slash.rs` 字面量、工具 build 引用解析（含 search/web_fetch 的 service family 引用）、显式未知 type 报错等全面跟改 |
| 14 | 文档 | `AGENTS.md`、`docs/guide/configuration.md`、`CHANGELOG`；ADR-0008 补演进记录（或新开 ADR） |
| 15 | 版本 | 配置格式不兼容 → 按惯例直升 minor，目标 **v0.6.0**（当前开发版 0.5.3 的 H5 顺延不受影响） |

## WebUI Models 选项卡交互稿

- **卡片网格**：每模型一卡——model_id、服务端模型名、kind 徽章、provider 名、`multimodal` 徽章（有则显示）、enabled switch、编辑/删除按钮。行常驻列表 + 灰化（沿 enabled toggle 的 astrbot 式约定，不从列表抹掉）。
- **过滤条**：provider 下拉、kind 下拉、multimodal checkbox。
- **添加流**（modal）：选 provider（下拉，**仅 llm family**）→ Probe（按 provider type 调 listing，展示模型列表）→ 点选一条 → **必选 kind**（默认 chat，可改；anthropic/gemini provider 时锁定 chat）→ 按 kind 渲染专属字段表单 → Add。service family provider 在 Provider 表单里直接加（只需 api_key），与本流程无关。
- **编辑**：卡片进编辑态，复用添加表单（provider 不可改——换端点=删了重建，避免引用漂移）。
- **无批量添加**：Probe 列表仅支持单条点选添加（决策 3）。

## Serde / merge 陷阱清单（沿 2026-09-09 实测教训）

- `enabled`：`default_true` + `skip_serializing_if = "is_true"`，**方向不能反**（反了 disabled 蒸发）；前端装载 `??= true` 归一化。
- `kind`：`skip_serializing_if` 等于 chat 时省略（盘面干净），回读缺省即 chat；同理 `capabilities` 空数组省略。
- models 子树 put_config 走 replace：盘上缺失的键即删除（支持表单删模型）；删除前确认 GET/PUT 往返不丢 `enabled = false` 之类的显式键。
- `ModelKind` 若用 `skip_serializing_if`，比较函数必须与 Default 判定**同源**，避免"写出来是 chat、判省略却按别的标准"的分裂。

## 明确否决

- **provider 级 kind**：多能力端点逼重复建 provider，见设计前提 1。
- **兼容双读 / 自动迁移脚本**：用户定一步到位；`deny_unknown_fields` 报错即安全网。
- **批量导入 + 未分类草稿态**：用户定直接删，无保留。
- **搜索 key 进模型目录**（即 `[model.<id>]` 条目）：搜索服务进的是 **provider 注册表**（凭据统一），但永远不产生 model 条目、不进 probe——初版"搜索留 tools"已按 2026-09-25 二轮讨论推翻并修订进设计前提 6/7。
- **provider 新增 kind 字段**：`type` 本身就是 llm family / service family 的判别器，再加一层 kind 是冗余旋钮。
- **serde 内部 tag enum 分型**：与 flatten/BTreeMap/serde_json 多路径混用坑多，注册期校验更稳。

## 验证

- 单测：`ModelEntry` TOML/JSON serde 往返（enabled=false、capabilities 空、kind 缺省三个方向各一例，防 skip 方向回归）；`deny_unknown_fields` 拦旧配置并出友好错误；引用校验（缺失/kind 不符 → warn + 不注册）；`model_from_ref` 三态（命中/disabled 显式引用仍可用/不存在报错）；tts/image_gen/search/web_fetch 的引用解析（含 service family 过滤、`supports_extract()` 拦截、引用了 llm family provider 的报错）；显式未知 provider type 报错、缺省 type 回退 openai_compatible；搜索 adapter 的 mock 响应解析（每 type 至少一例 fixture 往返，保证 `SearchHit` IR 映射稳定）。
- 手工：WebUI 全流程（建 provider → probe → 选 kind → 落盘 → 卡片过滤 → 编辑 → 删除）；service family 添加与 search 实际搜一次、web_fetch 走 tavily extract；agent 切模型与 fallback；tts/image_gen 实际出声/出图（api_key 走 provider 层展开）；`/models` 列表与切换、`/providers` 列表、`/provider` 别名转发；旧配置启动报错消息可读性。
- 质量门：`cargo fmt --all` → `cargo clippy --all-targets -- -D warnings` → `cargo test`（清代理 env；跑 test 前停掉运行中的 llaia 实例；本机内存限制须 `-j 1`）。
