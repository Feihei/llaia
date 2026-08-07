# P3-d：MCP Client 接入 — 实现计划

- 日期：2026-08-07
- 状态：✅ 已完成（2026-08-07）
- 设计依据：[ADR-0014](../adr/0014-mcp-client.md)

## 目标

LLAIA 作为 MCP client 消费外部 MCP server 的工具，支持 stdio / HTTP（streamable）/ SSE 三种 transport，协议层自实现（不引入 rmcp），MCP 工具通过 adapter 包装成 `Tool` trait 注册到主 agent。

## 文件清单

| 文件 | 变更 | 说明 |
|---|---|---|
| `src/mcp/mod.rs` | 新增 | `McpConfig` / `McpServerConfig` + mcp.toml 加载校验 + `${VAR}` 插值 |
| `src/mcp/protocol.rs` | 新增 | JSON-RPC 2.0 类型 + MCP 协议常量 |
| `src/mcp/transport.rs` | 新增 | `McpTransport` trait + Stdio/Http/Sse 三实现 |
| `src/mcp/client.rs` | 新增 | `McpServer` + `McpRegistry`（handshake / call_tool / bounded reconnect / isError / scrub） |
| `src/tools/mcp.rs` | 新增 | `McpTool` adapter（`<server_id>__<tool_name>` 前缀，`requires_confirm` 默认 true） |
| `src/lib.rs` | 改 | 挂 `mcp` 模块 |
| `src/config.rs` | 改 | `expand_string` 提升为 `pub(crate)` 供 mcp 复用 |
| `src/channels/cli.rs` | 改 | `build_agent` 连接 MCP registry + 主 agent 注册 MCP 工具（受 denied_tools 过滤） |
| `src/commands/mod.rs` | 改 | init 生成 mcp.toml 模板；doctor 增加 mcp.toml 检查 |
| `src/web/mod.rs` | 改 | AppState 加 mcp 字段；`GET /api/mcp`、`GET/PUT /api/mcp/raw`、`POST /api/mcp/:id/test` |
| `src/channels/web.rs` | 改 | WebChannel 加 mcp_registry 注入槽 |
| `src/web/static/*` | 改 | MCP tab（server 列表 + 工具展开 + raw 编辑器） |
| `tests/mcp_http.rs` | 新增 | mockito 模拟 streamable HTTP server 的集成测试 |

## 关键设计（对齐 ADR-0014）

1. **串行请求模型**：每个 server 一把 `tokio::sync::Mutex` 保证请求串行，transport 内部按 id 匹配响应、跳过 server 通知，无需后台分发任务（SSE 例外：GET 长连接需后台 reader + pending map）。
2. **超时分层**：握手 30s；工具调用默认 180s，per-server `tool_timeout_secs` 覆盖，硬上限 600s。
3. **bounded reconnect**：`MAX_RECONNECT_ATTEMPTS = 2`，仅 `StaleSession`（HTTP 404/410 带 session）/ `TransportClosed`（流 EOF / 子进程退出）触发；重连 = transport.reset() + 重新 handshake + 重试原调用。
4. **isError envelope**：`result.isError == true` → 提取 `content[].text` 映射为 Err，错误信息 secret scrub + 500 字符截断。
5. **工具命名**：`<server_id>__<tool_name>`，registry 维护 prefixed_name → (server_idx, tool_name) 索引路由。
6. **安全**：MCP 工具默认 `requires_confirm = true`（走现有 confirm_mode + audit.log）；per-server `safe_tools` 白名单可标记只读工具免确认。stdio 子进程 `kill_on_drop(true)`。
7. **失败隔离**：单个 server 初始化失败 log + 跳过，不阻塞启动。
8. **WebUI**：MCP tab 展示 server 状态与工具列表，raw 编辑 mcp.toml（保存前校验可解析，改后需重启生效），`POST /api/mcp/:id/test` 现场连接验证。

## Task 拆分

- Task 1：protocol.rs + mod.rs（config）+ 单测
- Task 2：transport.rs（stdio/http/sse）
- Task 3：client.rs（McpServer/McpRegistry/handshake/call_tool/reconnect）+ tools/mcp.rs
- Task 4：cli.rs 整合 + init/doctor
- Task 5：WebUI API + 前端 MCP tab
- Task 6：mockito 集成测试 + fmt/clippy/test 全绿
- Task 7：更新 plan.md 状态

## 实现补记

- **系统代理问题**：reqwest 默认读取 Windows 注册表系统代理（如 Clash），其 bypass 列表常不含 loopback，导致对 127.0.0.1 的 HTTP 请求被代理截断（"connection closed before message completed"）。修复：MCP transport 的 reqwest client 统一 `.no_proxy()`（MCP server 多为本地/内网服务）；既有 mockito 测试（provider_http/provider_stream/qq_http）加 `NO_PROXY` 环境变量旁路。
- **scrub 正则**：命中敏感 key 后把值整段掩掉直到逗号/换行，避免 `Authorization: Bearer xxx` 这类带前缀的值漏网。
