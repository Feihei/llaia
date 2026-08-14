# P5: 统一搜索抽象（unified search） 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: 使用 superpowers:executing-plans 按任务顺序实现。步骤用 checkbox (`- [ ]`) 标记追踪。

**Goal:** 用单一 `search` 工具替代/收敛多个搜索 provider，内部按配置路由/兜底到 tavily / 百度 / 豆包 / Brave。各 provider 内置实现（仿 `tavily.rs`），归一化结果，零额外进程。

**Architecture:** `SearchProvider` trait + `SearchResult` 归一化结构；`UnifiedSearch` 工具持有 provider 列表，按 `[tools.search].providers` 顺序请求、任一成功即返回、全失败兜底。tavily 作为其中一个 provider 迁移接入（保留老配置兼容）。

**Tech Stack:** Rust + reqwest + serde + tokio（复用现有依赖）

**参考设计:** [ADR-0023](../adr/0023-unified-search.md)

---

## 文件结构

**新建：**
- `src/tools/search/mod.rs` — `SearchProvider` trait + `SearchResult` + `UnifiedSearch` 工具（路由/兜底）
- `src/tools/search/tavily.rs` — 现有 `TavilySearch` 迁为 provider
- `src/tools/search/baidu.rs` — `BaiduSearch` provider
- `src/tools/search/doubao.rs` — `DoubaoSearch` provider
- `src/tools/search/brave.rs` — `BraveSearch` provider
- `tests/search_providers.rs` — 各 provider 归一化 + 路由单测（mock HTTP）

**修改：**
- `src/tools/mod.rs` — 加 `pub mod search;`
- `src/channels/cli.rs` — 用 `UnifiedSearch` 替换原 `TavilySearch` 注册（条件注册）
- `src/config.rs` — 配置结构：`[tools.search]` + `[tools.<provider>]`
- `src/commands/mod.rs` — `init` 模板加 `[tools.search]` 段

---

## Task 1: SearchProvider trait + 归一化结构

**Files:** Create `src/tools/search/mod.rs`

- [ ] 定义：

```rust
pub struct SearchResult { pub title: String, pub url: String, pub snippet: String }

#[async_trait]
pub trait SearchProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn search(&self, query: &str, top_k: usize) -> anyhow::Result<Vec<SearchResult>>;
}
```

- [ ] `UnifiedSearch`：持有 `Vec<Arc<dyn SearchProvider>>`，`execute` 按序请求，返回首个成功的归一化结果；全失败返回错误（含各 provider 失败原因）。

---

## Task 2: 各 provider 实现

**Files:** Create `src/tools/search/{tavily,baidu,doubao,brave}.rs`

- [ ] `TavilySearch`：把现有 `src/tools/tavily.rs` 逻辑包成 `SearchProvider`（结果已是 `SearchResult` 形态则直接复用）。
- [ ] `BaiduSearch`：百度搜索 API（如 百度 AI 搜索 / 百度搜索开放平台），POST query → 解析 `{"result":[{title,url,content}]}` → 归一化。
- [ ] `DoubaoSearch`：豆包/字节搜索 API（参考其搜索接口文档），归一化。
- [ ] `BraveSearch`：Brave Search API `GET https://api.search.brave.com/res/v1/web/search?q=`，解析 `web.results[]` → 归一化。
- [ ] 每个 provider 实现 `requires_confirm()=false`（只读），并在 api_key 缺失时 `search` 返回 Err（由 `UnifiedSearch` 跳过）。

> 注：百度/豆包的具体 API endpoint 与字段需在实现前查官方文档确认（Task 2 第一步先核对 3 家文档）。

---

## Task 3: 配置与条件注册

**Files:** Modify `src/config.rs`, `src/channels/cli.rs`

- [ ] 配置：

```toml
[tools.search]
providers = ["tavily", "baidu", "doubao", "brave"]
top_k = 8

[tools.tavily]
api_key = "${TAVILY_API_KEY}"
[tools.baidu]
api_key = "${BAIDU_API_KEY}"
[tools.doubao]
api_key = "${DOUBAO_API_KEY}"
[tools.brave]
api_key = "${BRAVE_API_KEY}"
```

- [ ] `config.rs` 加 `SearchConfig { providers: Vec<String>, top_k, tavily/baidu/doubao/brave: ProviderKeyConfig }`，`expand` 时处理 `${VAR}`。
- [ ] cli.rs：遍历 `providers`，key 非空则构造对应 provider 加入 `UnifiedSearch`；至少 1 个可用才注册 `search` 工具（全空则不注册，与现有 tavily 行为一致）。

---

## Task 4: 替换原 tavily 注册

**Files:** Modify `src/channels/cli.rs`, 可选删除 `src/tools/tavily.rs`

- [ ] 移除 cli.rs 中独立的 `TavilySearch` 注册，改由 `UnifiedSearch` 统一承载（tavily 作为其内部 provider）。
- [ ] 老配置 `[tools.tavily].api_key` 仍生效（Task 3 已兼容）。
- [ ] 视情况保留 `src/tools/tavily.rs` 作为 `search/tavily.rs` 或删除，避免重复。

---

## Task 5: 单测 + 集成验证

**Files:** Create `tests/search_providers.rs`

- [ ] mock HTTP（用 `wiremock` 或本地 handler）验证：每家 provider 响应正确归一化为 `SearchResult`；`UnifiedSearch` 路由（首成功即返回）、兜底（全失败报错）。
- [ ] `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test`
- [ ] 手动：`llaia chat` → "搜一下 Rust 异步最佳实践" 验证走统一 `search`；配置仅留 baidu 验证单 provider 也能用。
- [ ] 更新 `docs/plan.md` 本条目状态。

---

## 自查

- 单一 `search` 工具 + provider 路由/兜底 ✅；豆包/百度/Brave 内置（非 MCP）✅
- tavily 兼容迁移 ✅；结果归一化 `SearchResult` ✅
- 类型一致性：SearchProvider/SearchResult/UnifiedSearch 在 mod + 各 provider + cli 一致 ✅
- 待确认：百度/豆包具体 API 文档（Task 2 第一步）✅ 已列为前置动作
