# 实现计划：Provider Compat 层（Ollama / Llama.cpp）

> 关联 ADR：[0026-provider-compat.md](../adr/0026-provider-compat.md)
> 日期：2026-08-14

## Goal

给 `OpenAiCompatibleProvider` 加精简 `Compat` 结构，按 base_url 自动探测本地端点差异 + 显式覆盖；覆盖 Ollama / Llama.cpp 高频差异，非破坏性（默认 = 当前 bare 行为）。

## Architecture

- 新 `src/provider/compat.rs`：定义 `Compat` struct + `Compat::default()`（= bare 现状）+ `detect_compat(base_url: &str) -> Compat` + 各端点预设（`ollama()`、`llamacpp()`）。
- `OpenAiCompatibleProvider` 持 `compat: Compat`。
- 请求构建：按 compat 决定 developer→system 合并、max_tokens 字段名、是否要求 assistant 占位、reasoning 折回 content。
- 响应解析：按 compat 决定 streaming usage 解析、finish_reason 推断。
- 配置：`[provider.<id>].compat.*` 覆盖（优先级高于探测）。

## Tech Stack

Rust（llaia 单 crate）。复用 `reqwest`、现有 `openai_compat.rs`。

## 文件结构

- `src/provider/compat.rs`（新）
- `src/provider/openai_compat.rs`：接入 compat（请求侧 + 响应侧）
- `src/config.rs`：`ProviderConfig` 加 `compat: Option<CompatConfig>`
- `src/provider/mod.rs`：构建 provider 时合并探测 + 覆盖，传入 compat

## 分步 Task

1. [ ] `compat.rs`：定义 struct + `default()` + `detect_compat`（ollama / llamacpp 子串）+ 预设。
2. [ ] openai_compat 请求侧：developer role 合并、`max_tokens` 字段切换、assistant 占位、reasoning→content。
3. [ ] 响应侧：streaming usage、finish_reason 推断。
4. [ ] config 覆盖：`[provider.X].compat.*` 合并到探测结果。
5. [ ] 单测：Ollama / Llama.cpp mock 响应正确归一；`Compat::default()` 行为不变（回归）。
6. [ ] 文档：AGENTS.md 补 compat 说明 + 例子（Ollama / Llama.cpp）。

## 自查

- [ ] `cargo test` + `cargo clippy` 绿
- [ ] 现有 Ollama 用户无感（default = 原行为）
- [ ] Llama.cpp tool calling / reasoning / usage 实测正确
- [ ] 显式 `[provider.X].compat.*` 能覆盖自动探测
