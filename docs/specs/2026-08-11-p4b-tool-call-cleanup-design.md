# P4-b 设计规格 — 工具调用格式优化 + image 描述模型

- 日期：2026-08-11
- 状态：🔄 实施中
- 依据：[docs/plan.md](../plan.md) §P4-b
- 参考：zeroclaw `zeroclaw-tool-call-parser`、goose `toolshim`、AstrBot `core/agent/tool.py`

---

## 一、问题分析

### 1.1 工具调用格式泄露（核心痛点）

**现象**：agen 系模型（DeepSeek-R1 蒸馏、GLM-4 思考版等）偶发把 `<think>...</think>` 推理内容或 `<tool_call>...</tool_call>` 标签泄露到用户可见回复中。

**根因**：`src/agent/mod.rs:351-365` 的流式消费逻辑按 `provider.native_tool_calling()` 分流：

```rust
StreamEvent::TextDelta(d) => {
    if provider.native_tool_calling() {
        // native 模式：直接发给用户，不经过 parser
        let _ = event_tx.send(TurnEvent::Chunk { delta: d.clone() }).await;
        iter_text.push_str(&d);
    } else {
        // 标签降级模式：经过 ToolCallStreamParser 过滤
        let user_text = parser.feed(&d);
        ...
    }
}
```

native 模式下 `TextDelta` **完全绕过 `ToolCallStreamParser`**，直接推给用户。模型本应通过 `StreamEvent::ToolCall` 发结构化工具调用、通过非标签文本发可见回复，但偶发行为下它把 think / tool_call 标签写进了文本流，于是原样泄露。

**对比参考项目**：

| 项目 | native 模式是否跑文本解析 | 文本解析器 |
|---|---|---|
| zeroclaw | ✅ 始终跑 | `zeroclaw-tool-call-parser`（~2000 行，10+ 格式） |
| goose | ✅ 始终跑（toolshim 内含） | tokenized marker + inline JSON |
| astrbot | ✅ 走原生 schema，无独立兜底 | — |
| **LLAIA 现状** | ❌ native 绕过 | `ToolCallStreamParser`（仅标签降级模式用） |

结论：三个参考项目即使走 native function calling，也都保留文本解析作为**始终启用的兜底**。LLAIA 的 `ToolCallStreamParser` 已具备 think 剥离 + tool_call 标签解析能力，但只在标签降级模式启用——P4-b 的核心改造是**让它始终启用**。

### 1.2 image 描述模型缺失

**现象**：主模型无多模态能力时（如本地 Ollama 跑纯文本模型），用户发图片会被忽略或触发 provider 报错。

**现状**：`image_utils.rs` 已有 `prepare_image_for_vision`（缩放 + base64 编码），多模态消息通过 `MessageContent::Multimodal` + `ContentPart::ImageUrl` 支持。但缺少"主模型不支持 vision 时用独立模型描述图片"的降级路径。

**参考**：LLAIA 已有 `compact_model` 机制（用独立 provider 跑上下文压缩），vision_model 可完全照搬该模式。

---

## 二、方案设计

### 2.1 工具调用格式优化

#### 2.1.1 统一走 ToolCallStreamParser

**核心改动**：去掉 `agent/mod.rs` 中 `if provider.native_tool_calling()` 分支，所有 `TextDelta` 统一经过 `ToolCallStreamParser`。

改造后逻辑：

```rust
StreamEvent::TextDelta(d) => {
    // 统一走 parser：剥离 think + 提取 tool_call 标签
    let user_text = parser.feed(&d);
    if !user_text.is_empty() {
        let _ = event_tx.send(TurnEvent::Chunk { delta: user_text }).await;
    }
    iter_text.push_str(&d);  // 仍存原始文本（见 2.1.3 说明）
    let new_calls = parser.take_tool_calls();
    calls.extend(new_calls);
}
```

**为什么安全**：
- `ToolCallStreamParser` 对无标签文本是**透传**的（`State::Outside` 下非 `<` 字符直接 push 到 out）。native 模式下正常文本不受影响。
- think 标签被剥离（native 模式也受益）。
- 偶发泄露的 `<tool_call>` 标签被提取为 `ToolCall` 并合并到 `calls` 执行（与 native 的 `StreamEvent::ToolCall` 合并，不重复——模型不会同时通过两种方式发同一个调用）。
- `parser.finish()` 统一在流结束后调用（去掉 `if !provider.native_tool_calling()` 门控）。

#### 2.1.2 补充 markdown fence 格式

**动机**：部分模型（尤其本地小模型）会用 markdown 代码块包裹工具调用：

~~~
```tool_call
{"name":"file_read","arguments":{"path":"/tmp/x"}}
```
~~~

或：

~~~
```invoke
{"name":"x","arguments":{}}
```
~~~

这是除 `<tool_call>` 标签外最常见的泄露形式。zeroclaw parser 覆盖了此格式，LLAIA 补充。

**改动**：
- `tool_call/stream_parser.rs`：状态机新增 `MaybeFence` 状态，检测 `` ``` `` 开头后跟 `tool_call`/`invoke`/`toolcall` 的 fence，按 fence 块处理（类似 `InToolCall`）。
- `tool_call/tag_parser.rs`：正则补充 fence 格式 `` ```tool_call\n...\n``` ``。

**不补充的格式**（zeroclaw 有但 LLAIA 暂不需要）：
- MiniMax invoke parameter format、perl-style blocks、tool-name-as-fence-language、OpenAI JSON wrapper、XML nested payload、plural `<tool_calls>` wrapper——这些在 LLAIA 单用户场景极罕见，按需后续补充。

#### 2.1.3 iter_text 存储策略

**改造**：`iter_text` 存**清洗后文本**（`parser.feed` 的输出 + `finish` 的残留），不存原始含标签文本。

**理由**：
- think 内容是模型内部推理，不该进 context（模型后续轮次不需要看到它）。
- tool_call 标签是工具调用协议封装，tool_calls 字段已记录结构化调用，文本部分不该重复残留标签。
- context 和 sqlite 存清洗后文本，让模型看到的 assistant 历史是干净的（纯文本 + 结构化 tool_calls），避免标签污染影响后续行为。
- 用户侧通过 `TurnEvent::Chunk` 拿到的也是清洗后文本，与 context/sqlite 一致。

**实现**：`parser.feed(&d)` 返回 `user_text`，同时 push 到 `iter_text` 和 `event_tx`。`finish()` 返回的残留同样 push 到两者。

### 2.2 image 描述模型单独设置

#### 2.2.1 配置

`RuntimeConfig` 新增字段（完全照搬 `compact_model` 模式）：

```toml
[runtime]
vision_model = "default.gpt-4o"  # 可选：主模型无多模态时，用此模型描述图片
```

- `vision_model: Option<String>`，model ref 格式 `"provider_id.model_alias"`。
- 未设置时：图片直接发给主模型（现状行为，主模型不支持则由 provider 决定如何处理）。
- 设置后：所有多模态消息（含图片）的图片部分先经 vision_provider 描述，描述文本替换图片，组合成纯文本消息发给主模型。

#### 2.2.2 Agent 持有 vision_provider

照搬 `compact_provider` 模式：

```rust
pub struct Agent {
    ...
    pub vision_provider: Arc<RwLock<Option<Arc<dyn Provider>>>>,
    ...
}
```

- `Agent::new` 接收 `vision_provider: Option<Arc<dyn Provider>>`。
- `build_agent`（`channels/cli.rs`）构建 vision_provider（照搬 `build_compact_provider` 逻辑）。
- 热替换：`reload_vision_provider`（照搬 `reload_compact_provider`），WebUI 改配置后生效。

#### 2.2.3 图片描述流程

`handle_message_streaming` 入口拦截多模态消息：

```
用户消息 (含图片)
  ├─ vision_provider 未配置 → 直接发给主模型（现状）
  └─ vision_provider 已配置
       ├─ 提取所有 ImageUrl part
       ├─ 逐张调 vision_provider.chat("请详细描述这张图片的内容")
       ├─ 收集描述文本
       └─ 把原始消息改写为纯文本：
            "[图片1描述] ...\n[图片2描述] ...\n<原始文本>"
          发给主模型
```

**实现要点**：
- 描述请求用 `Provider::chat`（非流式），prompt 固定为"请详细描述这张图片的内容，包括文字、物体、场景等关键信息"。
- 描述失败（vision_provider 报错）时：warn + 降级为"[图片描述失败]"占位，不阻塞对话。
- 改写后的消息是纯文本（`MessageContent::Text`），主模型正常处理。
- sqlite 存储：存改写后的纯文本（不含 base64，与现状"多模态只存文本部分"一致）。

---

## 三、改动清单

### 3.1 工具调用格式优化

| 文件 | 改动 |
|---|---|
| `src/agent/mod.rs` | 去掉 `TextDelta` 的 native/标签降级分支，统一走 `parser.feed`；`finish()` 统一调用；补单测（native 模式 think 泄露被剥离） |
| `src/tool_call/stream_parser.rs` | 新增 `MaybeFence` 状态 + fence 解析逻辑；补单测（` ```tool_call ` 格式） |
| `src/tool_call/tag_parser.rs` | 正则补充 markdown fence 格式；补单测 |

### 3.2 image 描述模型

| 文件 | 改动 |
|---|---|
| `src/config.rs` | `RuntimeConfig` 加 `vision_model: Option<String>`；默认模板注释；序列化测试 |
| `src/agent/mod.rs` | `Agent` 加 `vision_provider` 字段 + `reload_vision_provider` + `vision_provider_snapshot`；`handle_message_streaming` 入口拦截多模态消息 |
| `src/channels/cli.rs` | `build_agent` 构建 vision_provider（照搬 compact_provider 逻辑） |
| `src/channels/web.rs` | `build_agent`（serve 模式）同步构建 vision_provider |
| `src/web/mod.rs` | `hot_reload_providers` 加 vision_provider 热替换 |

---

## 四、验证要点

### 4.1 工具调用格式优化

1. native 模式下模型输出含 `<think>推理</think>可见文本` → 用户只看到"可见文本"
2. native 模式下模型输出含 `<tool_call>{...}</tool_call>` → 标签被剥离，工具调用被执行，用户不看到标签
3. 标签降级模式行为不变（回归）
4. markdown fence 格式 ` ```tool_call\n{...}\n``` ` 被正确解析
5. 正常文本（无任何标签）透传不受影响
6. `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` 全绿

### 4.2 image 描述模型

1. 未配 `vision_model`：多模态消息直接发给主模型（回归）
2. 配了 `vision_model`：含图片消息被改写为纯文本（描述 + 原文本），主模型收到纯文本
3. vision_provider 描述失败：降级为"[图片描述失败]"，不阻塞对话
4. sqlite 存改写后纯文本（不含 base64）
5. WebUI 改 `vision_model` 后热生效（无需重启）
