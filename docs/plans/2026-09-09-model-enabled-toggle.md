# Provider model 启用开关（`enabled`）：记录参数但暂不启用

状态：**已实施**（2026-09-09，语义按下方四条决策落地；`enabled` 只管可发现性，硬拦明确否决）
日期：2026-09-09
关联：`docs/adr/0008-config-schema-v1.1.md`（provider/model 配置模式）、`docs/adr/0026-provider-compat.md`（compat 覆盖层）

## 背景与问题

`[provider.<id>.<model_alias>]` 里的每个 model 目前**只要能写进配置就是可用的**：出现在 `/provider` 列表、WebUI 的 agent model 下拉与 fallback 下拉里。实际使用里有一类稳定需求——**把模型的参数先记在配置里，但暂时不希望它被选中**（例如本地 ollama 上拉了一堆模型、只想让 agent 在少数几个里挑；或某云端模型这一阵不打算烧 token，但 `context_size`/`max_tokens` 想留着备查）。

现在绕这个需求只有两条脏路：删掉整个 model 表（参数丢失，下次要重写）或留着但自己在脑子里排除（`/provider <序号>` 会数到它）。

目标：给每个 model 加一个 `enabled` 布尔开关，默认启用、存量配置零迁移。

## 设计前提（用户已定，本文不推翻）

1. `enabled` **只管可发现性**，不管可用性——`provider_from_ref` 不拦显式引用。
2. `agent.<alias>.fallback` 引用了 disabled model → **剔除**。
3. `runtime.compact_model` / `runtime.vision_model` 引用了 disabled model → **warn + 置 None（回退主模型）**。
4. UI 采 astrbot 式：**行常驻列表 + 灰化 + 开关**，不从列表里抹掉。

## 为什么第 1 条不能选「硬拦」

`provider_from_ref`（`src/provider/mod.rs:218-229`）是**唯一**的 model ref 解析收口，agent init（`channels/cli.rs:401-421`）、fallback 链（`provider/mod.rs:294-321`）、compact/vision 构建（`web/mod.rs:742-765`、`channels/cli.rs:426-445`）、`/provider` 切换全走它。若在此处拒绝 disabled：

- 用户把**当前** `agent.model` 指向的模型关掉 → 下次启动 agent init 直接 `Err` → 自己 brick 自己，且 WebUI 保存路径（`put_config` 会先 `build_provider_from_config` 试构建、失败即 400 不落盘，`web/mod.rs:520-525`）连"改回来"都要先绕 raw TOML。
- 收益为零：关掉的意义是"别在我选模型时烦我"，不是"我已经配好的立刻失效"。

选「只管可发现性」后，最坏情况只是该 ref 从下拉里消失而当前值仍生效——而这个兜底 UI **早就存在**：`index.html` 的 agent model 下拉在 `a.model` 不在候选集时会额外渲染一项 `xxx (current)`。零额外代码。

## 实测：序列化通路（本 plan 的主要证据）

结构化保存的落盘走 `merge_config_preserving_comments`（`src/web/mod.rs:560-580`）：对**整个 merged `Config`** 做 `toml::to_string`，再按 replace/preserve 两种模式合并回磁盘文档；`provider`/`agent` 子树是 **replace**（`:571`，盘上缺失的 key 会被删，`:606-615`）。

用 `toml = 0.8.23` 复刻 `ProviderConfig`/`ModelConfig` 形状跑四种写法：

| 字段写法 | `toml::to_string` 输出 | 回读 |
|---|---|---|
| `Option` 为 `None`（现状 `context_size`/`max_tokens`/`native_tool_calling`） | 静默省略该键，**不报错** | ✅ 顺带证明现有保存通路无潜在 serialize 失败 |
| 裸 `bool` + `#[serde(default)]`（无 skip） | **每个 model 表多写一行 `enabled = true`** | ✅ 正确，但脏 diff |
| `skip_serializing_if = "std::ops::Not::not"`（false 时跳过） | `[ollama.off]` 只剩 `model = "gpt-4o"` | ❌ **disabled 蒸发**：false 不落盘，回读变 true |
| `skip_serializing_if = "is_true"`（true 时跳过） | 只写 `enabled = false` | ✅ `off=false, default=true` |

结论：字段必须写成 `#[serde(default = "default_true", skip_serializing_if = "is_true")]`，**方向不能反**。反了的症状是"关掉再开页面又变回开着"，且因为发生在 serde 层、前后端都看不出异常，属于最难查的一类静默失效。

### 连带后果：GET 也省略该键

`skip_serializing_if` 对 `serde_json` 同样生效，所以 `GET /api/config` 里启用的 model **不带** `enabled` 键 → 前端拿到 `undefined` → checkbox 显示未勾选，而实际是启用态。**这就是 WebUI 与 config 真正会失同步的地方**，对策是装载时归一化 `enabled ??= true`（`app.js` 的 flatten 适配循环内）。

### 前端为什么必须用 checkbox 而不是 select

`x-model` 绑 `<input type="checkbox">` 产出真 boolean；绑 `<select>` 产出字符串 `"true"` → serde `bool` 反序列化失败 → 整个 PUT 返 400。既有代码里 `compat.native_tool_calling` 的三态下拉之所以要写 `:value="true"` / `:value="false"`，正是在绕这个坑（`index.html` compat 面板）。本开关只需要二态，直接用 checkbox（复用 `.switch` 形态）。

## 现状矩阵

| 对象 | 现状 | 锚点 |
|---|---|---|
| wire 层 | 无 DTO，GET/PUT 直接复用真实 `Config` | `web/mod.rs:423`（`Json(mask_sensitive(cfg))`）、`:449` |
| 前端 flatten 适配 | 装载把非 `type/base_url/api_key/model/compat` 的 key 折进 `p.model[k]`；保存反向摊平 | `app.js:528-537`、`:566-573` |
| 落盘合并 | provider/agent 走 replace（支持表单删模型），其余 preserve | `web/mod.rs:571`、`:606-615` |
| model 枚举（CLI） | `flatten_model_refs` 同时是 `/provider <序号>` 的索引基准 | `commands/slash.rs:1056-1069` |
| model 枚举（WebUI） | `modelRefs()` 喂 agent model 下拉 + fallback 下拉 | `app.js:754-762` |
| 引用校验 | fallback / compact_model **只校验格式**（`split_once('.')`），不校验存在性 | `config.rs:876-886`、`:934-954` |
| vision_model | **完全没有**引用校验（顺手补漏） | `config.rs:53` |
| doctor | 只 **per provider** 发一个 `GET /models`；模型级只有 `detect_context_size`（仅 main model） | `commands/mod.rs:1078-1104`、`:1123` |
| 单模型真实探测 | 手动按钮，发 `max_tokens=1` 最小 chat，非自动 | `web/mod.rs:2396+` |

> doctor 的**探测**侧零改动：它不遍历 model 发请求（连通性是 per provider 一个 `GET /models`，模型级只有 `detect_context_size` 且仅探 main model），`enabled` 与它正交。曾考虑"disabled 是否跳过连通性探测"，因粒度不匹配而撤销。**输出侧改了一处**：CLI doctor 的 model 清单给 disabled 模型加 `[disabled]` 标记——它本来就遍历所有 model 打印，不加的话 doctor 列出而 `/provider` 不列，看起来像两处视图不一致的 bug。

## 改动面

| # | 位置 | 动作 |
|---|---|---|
| 1 | `config.rs` `ModelConfig` | `enabled: bool`，`default_true` + `skip_serializing_if = "is_true"`（`default_true` 已存在于 `:246`） |
| 2 | `config.rs` | 新增 `reconcile_disabled_models(&mut self)` + 私有 `model_is_disabled`；供 load / put_config 共用 |
| 3 | `config.rs:876-886` 区 | compact_model / vision_model 引用 disabled → warn + 置 None |
| 4 | `config.rs:934-954` 区 | fallback retain 加「存在且 enabled」 |
| 5 | `web/mod.rs` `put_config` | 保存前调 `reconcile_disabled_models`——否则 WebUI 里刚关掉的 compact_model 要重启才生效（`put_config` 不走 `Config::load`，`:517` 的注释已承认这层双路分叉） |
| 6 | `slash.rs:1056` | `flatten_model_refs` 过滤 disabled（`/provider <id.alias>` 显式路径仍可用，符合决策 1） |
| 7 | `app.js` | 装载归一化 `??= true`；`addModel`/`addProbedModels` 初始 `enabled: true`；`modelRefs()` 过滤 `!== false` |
| 8 | `index.html` + `theme.css` | alias label 变 switch checkbox；行加 `.model-row--off` 灰化；`.switch` 选择器从 `.compat-fields .switch` 放宽为 `.switch` |
| 9 | `config.rs:1011-1018`、`slash.rs` 两处测试字面量 | 补 `enabled: true`（非 Option 字段的编译连带） |
| 10 | `slash.rs` `list_providers` | 当前模型被隐藏时补一行说明（否则列表里没有 `*`，像模型丢了） |
| 11 | `commands/mod.rs` doctor 的 model 清单 | disabled 加 `[disabled]` 标记，与 `/provider` 视图对齐 |
| 12 | 文档 | CONFIG_TEMPLATE 注释、`docs/guide/configuration.md`、`AGENTS.md`、CHANGELOG |

无迁移脚本、无新命令、无新 `[runtime]` key。

## 明确否决

- **`enabled` 参与可用性判定**（`provider_from_ref` 拦）：见上，会 brick 当前模型。
- **disabled 模型从 WebUI 列表隐藏**：会变成"关掉就找不回来"，且 `+ Add model`/`× Delete` 语义被搅浑。
- **`skip_serializing_if` 反向**（false 时跳过）：实测静默失效，见上表。
- **把 fallback 剔除挪到 `provider_from_ref`**：备用链是容错手段，在构建期静默少一个备用模型比在配置期报错更难发现。

## 验证

- 单测：fallback 剔除、compact/vision 置空、`enabled=false` 的 TOML 序列化往返（防住反向 skip 回归）、存量无 `enabled` 键配置读出 `true`。
- 手工：WebUI 关掉一个模型 → Save → 读盘确认只写 `enabled = false`、重新打开页面确认行灰化且下拉里消失 → 再打开确认键被 remove（replace 模式）而非留 `= true`。
- 质量门：`cargo fmt --all` → `cargo clippy --all-targets -j 1 -- -D warnings` → `cargo test`（本机内存限制须 `-j 1`；跑 test 前须停掉运行中的 llaia 实例，否则 `target\debug\llaia.exe` 被锁）。
