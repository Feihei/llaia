# P4-b 实现计划 — 工具调用格式优化 + image 描述模型

- 日期：2026-08-11
- 状态：🔄 实施中
- 依据：[specs/2026-08-11-p4b-tool-call-cleanup-design.md](../specs/2026-08-11-p4b-tool-call-cleanup-design.md)

## Task 1：统一走 ToolCallStreamParser（核心）

**目标**：去掉 native 模式下 TextDelta 绕过 parser 的分支，让 think/工具调用标签泄露被始终清洗。

**改动**（`src/agent/mod.rs` `handle_message_streaming` 内层流式循环）：

1. 去掉 `StreamEvent::TextDelta` 的 `if provider.native_tool_calling()` 分支，统一走 `parser.feed(&d)`
2. 去掉流结束后的 `if !provider.native_tool_calling() { parser.finish() }` 门控，统一调用 `parser.finish()`
3. `StreamEvent::ToolCall(tc)` 仍直接 `calls.push(tc)`（native 结构化工具调用与 parser 提取的合并）

**验证**：
- 新增测试：native 模式下 TextDelta 含 `<think>` → 用户 chunk 不含 think 内容
- 新增测试：native 模式下 TextDelta 含 `<tool_call>` 标签 → 标签被剥离、工具调用被执行
- 回归：现有 `test_streaming_native_tool_call`、`test_streaming_tag_mode_filters_tags` 仍通过

## Task 2：补充 markdown fence 格式解析

**目标**：支持 `` ```tool_call\n{...}\n``` `` 和 `` ```invoke\n{...}\n``` `` 格式的工具调用泄露。

**改动**：

1. `src/tool_call/stream_parser.rs`：状态机加 `MaybeFence` 状态
   - `Outside` 下遇到 `` ` `` → 进入 `MaybeFence`，缓冲判断是否 `` ``` `` + 已知 fence 语言（tool_call/invoke/toolcall）
   - 匹配成功 → `InFence`（行为同 `InToolCall`，累积到 `` ``` `` 闭合）
   - 不匹配 → 透传缓冲内容回 `Outside`
2. `src/tool_call/tag_parser.rs`：正则补充 fence 格式
   - `` `​``tool_call\n(.*?)\n`​`` ``（is 标志，dotall）
   - fence 语言别名同标签别名

**验证**：
- 单测：`` ```tool_call\n{"name":"x","arguments":{}}\n``` `` 被解析为 1 个 ToolCall
- 单测：`` ```invoke\n{...}\n``` `` 同上
- 单测：普通 `` ```python\ncode\n``` `` 不被误解析（透传）
- 流式单测：fence 跨 chunk 边界仍正确解析

## Task 3：image 描述模型 — 配置 + provider 构建

**目标**：新增 `vision_model` 配置，Agent 持有 vision_provider，照搬 compact_provider 模式。

**改动**：

1. `src/config.rs`：`RuntimeConfig` 加 `#[serde(default)] pub vision_model: Option<String>`
2. `src/agent/mod.rs`：
   - `Agent` 加 `pub vision_provider: Arc<RwLock<Option<Arc<dyn Provider>>>>`
   - `Agent::new` 加参数 `vision_provider: Option<Arc<dyn Provider>>`
   - 加 `vision_provider_snapshot()` / `reload_vision_provider()`（照搬 compact）
3. `src/channels/cli.rs` `build_agent`：构建 vision_provider（照搬 `build_compact_provider`）
4. `src/channels/web.rs` `build_agent`（serve 模式）：同步构建
5. `src/web/mod.rs` `hot_reload_providers`：加 vision_provider 热替换

**验证**：
- config 序列化/反序列化测试（vision_model 存在/缺失）
- `build_agent` 返回的 Agent vision_provider 正确注入

## Task 4：image 描述模型 — 图片描述流程

**目标**：`handle_message_streaming` 入口拦截多模态消息，用 vision_provider 描述图片。

**改动**（`src/agent/mod.rs`）：

1. 新增私有方法 `describe_images(&self, parts: &[ContentPart]) -> Vec<String>`
   - 遍历 `ImageUrl` part，逐张调 `vision_provider.chat`（非流式）
   - prompt 固定："请详细描述这张图片的内容，包括文字、物体、场景等关键信息"
   - 失败 → "[图片描述失败]" 占位
   - 返回描述文本列表
2. `handle_message_streaming` 入口：消息 `has_image()` 且 vision_provider 存在时
   - 提取 `Multimodal` parts
   - 调 `describe_images` 获取描述
   - 改写消息为 `MessageContent::Text`：`"[图片1描述] desc1\n[图片2描述] desc2\n<原始文本>"`
   - 改写后的消息进 context + sqlite（纯文本）

**验证**：
- 单测：含图片消息 + vision_provider 配置 → 主模型收到纯文本（含描述）
- 单测：vision_provider 描述失败 → 降级占位，不 panic
- 单测：无 vision_provider → 消息原样发给主模型（回归）

## Task 5：质量门验收

```bash
cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test
```

全绿后更新 `docs/plan.md` P4-b 状态。
