# P5: 统一搜索抽象（unified search） 实现计划

> 本文件是 **2026-08-14 初稿**，原草稿按「providers 列表 + 顺序兜底」设计；**后续经 zeroclaw / nanobot 复核，改为 ADR-0023 的单一 provider 方案**，本计划已就地对齐 ADR，旧草稿作废。

**Goal:** 用单一 `search` 工具收敛原 `tavily_search`，内部按 `[tools.search].provider` 选定的**单一** provider（tavily / baidu / brave）执行，各 provider 内置实现、归一化结果，零额外进程。

**设计决策（ADR-0023，用户已确认）**
- 单一 `search` 工具，对 agent 只暴露 `query` + 可选 `top_k`；**provider 选择完全由配置决定**，agent 不可指定。
- 单 `search` 调用只走 `provider` 指定的那一个；**不顺序串试、不聚合合并**。
- 无内置无 key 兜底（不引入 DuckDuckGo 类隐形依赖）；所选 provider 缺 key / 未知时**不注册** `search` 工具（条件注册）。
- 内置而非 MCP；`tavily` 作为其中一个 provider 迁移接入（老配置 `[tools.tavily]` 仍生效）。

**Tech Stack:** Rust + reqwest + serde + tokio（复用现有依赖，不引入新依赖）

**参考设计:** [ADR-0023](../adr/0023-unified-search.md)

---

## 文件结构

**新建：**
- `src/tools/search/mod.rs` — `SearchProvider` trait + `SearchResult` + `UnifiedSearch` 工具 + `build()` 条件注册
- `src/tools/search/tavily.rs` — `TavilyProvider`（原 `src/tools/tavily.rs` 迁移）
- `src/tools/search/baidu.rs` — `BaiduProvider`（百度千帆 AI Search）
- `src/tools/search/brave.rs` — `BraveProvider`（Brave Search API）

**修改：**
- `src/tools/mod.rs` — `pub mod tavily;` → `pub mod search;`
- `src/channels/cli.rs` — 用 `UnifiedSearch::build` 替换原 `TavilySearch` 注册
- `src/config.rs` — `SearchConfig { provider, top_k }` + `BaiduConfig` / `BraveConfig`
- `src/web/mod.rs` — 掩码 baidu/brave key
- `src/commands/mod.rs` — `init` 模板加 `[tools.search]` 段
- 文档与示例：`guide/*`、`adr/0006|0011|0013|0015`、`AGENTS.md`、示例 skill / cron

---

## Provider 实现要点（实现前已核对官方文档）

- **Tavily**：`POST https://api.tavily.com/search`，body 含 `api_key`/`query`/`max_results`，响应 `results[]`（title/url/content）。
- **Baidu（千帆 AI Search）**：`POST https://qianfan.baidubce.com/v2/ai_search/web_search`，头 `Authorization: Bearer <key>`，body 含 `messages`+`search_source=baidu_search_v2`+`resource_type_filter`，响应 `references[]`（title/url/snippet|content）。
- **Brave**：`GET https://api.search.brave.com/res/v1/web/search?q=&count=`，头 `X-Subscription-Token`，响应 `web.results[]`（title/url/description）。

## 未做：doubao（豆包）

豆包公开接入只有 MCP/Skill 或 Volcengine SigV4 SDK（access_key+secret_key），没有干净的"单 api_key REST"端点；ADR 明确"内置而非 MCP"，手搓 SigV4 不可测、风险高。故 `provider = "doubao"` 暂不实现（给清晰报错），待后续单独补 `DoubaoConfig` + provider。

---

## 自查

- 单一 `search` 工具 + 单一 provider 路由 ✅；tavily/百度/Brave 内置（非 MCP）✅
- tavily 兼容迁移（老 `[tools.tavily].api_key` 生效）✅；结果归一化 `SearchResult` ✅
- 条件注册：key 缺失/未知则不注册 `search` 工具 ✅
- 破坏性：cron 任务引用旧名 `tavily_search` 需改为 `search` ✅ 已在文档/示例中同步
