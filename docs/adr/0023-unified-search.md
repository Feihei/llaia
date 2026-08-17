# ADR-0023: 统一搜索抽象（unified search）

- 状态：提议（P5 实施）
- 日期：2026-08-14
- 关联：P5 搜索提供方扩展；现有 `src/tools/tavily.rs`

## 背景

现有 `tavily_search` 是单一 provider 的内置工具。用户希望增加豆包（Doubao）、百度（Baidu）、Brave 等搜索 API。

两个极端方案各有问题：
- **N 个独立工具**（tavily_search / baidu_search / ...）：污染工具列表，且要求 agent 自己判断"该用哪个 provider"，增加决策负担与出错概率。
- **纯 MCP**：接外部 search MCP server，需额外进程/网络，对"高频刚需的搜索"增加不必要的依赖与延迟。

用户已确认方向：**统一 `search` 抽象 + 内置实现**。

## 决策

### 1. 单一 `search` 工具

对外只暴露一个 `search(query, top_k?)` 工具（仿 `tavily.rs` 实现 `Tool` trait）。agent 无需关心底层 provider。

### 2. Provider 抽象与归一化

```rust
pub trait SearchProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn search(&self, query: &str, top_k: usize) -> anyhow::Result<Vec<SearchResult>>;
}

pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}
```

豆包 / 百度 / Brave 都是简单 REST（GET/POST + JSON），各自实现 `SearchProvider`，内部把各家响应归一化成 `SearchResult`。

### 3. 路由 / 兜底（单 provider + 缺 key 回落）

经参考 zeroclaw / nanobot 复核，二者均**不**做顺序串试或聚合，而是"单一 provider 由配置选定，缺 key / 未知时回落到内置默认"。本 ADR 采用同策略：

```toml
[tools.search]
provider = "tavily"          # 选定单一 primary provider；可选 tavily/baidu/doubao/brave
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

- 单 `search` 调用只走 `provider` 指定的那一个；**不顺序串试、不聚合合并**。
- **无内置无 key 兜底**：若所选 provider 缺 key / 未知，直接报错"请先配置 search provider"（不引入在中国可能连不上的 DuckDuckGo 类隐形依赖）。如希望开箱即用，可将 `provider` 设为需 key 的中文源（如 bocha / volcengine 类）并配 key。
- **agent 不可指定 provider**：`search(query, top_k?)` 对 agent 只暴露查询与数量，provider 选择完全由配置决定（与两大参考一致，保持工具简单）。
- key 缺失的 provider 在注册阶段跳过（条件注册，与 tavily 现有 `if !api_key.is_empty()` 一致）。

### 4. 内置而非 MCP

搜索是高频刚需，内置实现更稳、零额外进程、符合 local-first  ethos。如用户后续想接社区 search MCP server，仍可通过现有 `src/tools/mcp.rs` 叠加，不冲突。

### 5. 与 web_fetch 的关系

`search` 只返回链接+摘要；agent 需要正文时自行再调 `web_fetch` 抓取（不强制串联，保持工具单一职责）。

## 后果

- 工具列表从"N 个 search"收敛为 1 个，agent 提示更干净。
- 需新增：3 个 provider 实现 + 归一化 + 路由/兜底 + 配置解析 + 测试。改动集中、风险低。
- `tavily` 现状：保留 `TavilySearch` 作为 `search` 的一个 provider（迁移而非删除），保持向后兼容，老配置 `[tools.tavily]` 仍生效。
- 不引入新依赖（均用现有 `reqwest` / `serde` / `tokio`）。

## 实施记录

- **doubao（豆包）provider 暂未实现**：其公开接入只有 MCP/Skill 或 Volcengine SigV4 SDK（access_key+secret_key），没有干净的"单 api_key REST"端点；本 ADR 明确"内置而非 MCP"，手搓 SigV4 不可测、风险高。故 `provider = "doubao"` 当前不注册 `search` 工具（给清晰报错），待后续单独补 `DoubaoConfig` + provider。tavily/baidu/brave 已实现。
- 工具名由旧 `tavily_search` 收敛为 `search`；cron 任务若引用旧名需改为 `search`。
