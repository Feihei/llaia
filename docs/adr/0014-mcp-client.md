# ADR-0014: MCP Client 接入

- 状态：Proposed
- 日期：2026-08-04
- 关联：[ADR-0006](0006-tools-and-cli.md)、[docs/plan.md P3-d](../plan.md)

## 背景

MCP（Model Context Protocol）是 Anthropic 提出的开放协议，用于 LLM 应用与外部工具/数据源之间的标准化通信。生态已有大量 MCP server 实现（GitHub / 文件系统 / 数据库 / Slack / Notion 等），LLAIA 接入 MCP 可以：

- 不重复造轮子，直接复用社区 MCP server 的工具能力
- 给用户提供标准化扩展接口（用户配 MCP server 就能加工具，不用改 LLAIA 代码）
- 与其他 Agent 框架（如 Claude Desktop / Cursor）共享 MCP server 配置

需要决策：

1. LLAIA 作为 client（消费外部 MCP server）还是 server（暴露自身能力为 MCP）？
2. 支持哪些 transport（stdio / HTTP / SSE）？
3. MCP 工具如何与现有 `Tool` trait 整合？
4. 配置形态？
5. 错误处理（MCP server 启动失败 / 调用超时 / 崩溃）？

## 决策

**纯 client 模式**：LLAIA 仅消费外部 MCP server 提供的工具，不把自身能力暴露为 MCP server。

理由：
- LLAIA 是私人助理，主要价值是"调度其他能力"，不是"被其他 Agent 调度"
- 作为 server 暴露能力会引入并发 / 鉴权 / 多租户问题，与单用户定位冲突
- 若未来有需求，可作为独立 P4 阶段加（参考 ZeroClaw 的 server 模式）

### 1. 协议层自实现（不引入 MCP SDK）

P3-d **不引入 `rmcp` 或任何外部 MCP SDK**，完全自实现协议层。理由：
- MCP 协议层薄（JSON-RPC 2.0 + initialize 握手 + tools/list + tools/call），自实现 < 500 行
- 零外部依赖风险，不受 SDK 维护节奏制约
- 直接借鉴 zeroclaw 的实现模式（Rust + tokio + serde_json），代码结构清晰可参考

实现参考 zeroclaw 的四个模块：
- `src/mcp/protocol.rs`：JSON-RPC 2.0 类型（`JsonRpcRequest`/`JsonRpcResponse`/`McpToolDef`）+ 常量（`MCP_PROTOCOL_VERSION = "2024-11-05"`）
- `src/mcp/client.rs`：`McpServer` + `McpRegistry`
- `src/mcp/transport.rs`：三种 transport 实现
- `src/tools/mcp.rs`：`McpTool` adapter，实现 `Tool` trait

### 2. 支持的 transport

P3-d 阶段支持三种（对齐 MCP 2025-06-18 spec）：

- **stdio**（优先）：LLAIA 启动子进程，通过 stdin/stdout 通信（JSON-RPC over stdio）。最常见，本地 MCP server 多用此方式（如 `mcp-server-filesystem`、`mcp-server-github`）。`kill_on_drop(true)` 确保子进程随 registry 退出而被 kill
- **HTTP**（streamable）：POST JSON-RPC，支持 `Mcp-Session-Id` header 会话管理（MCP 2025-06-18 spec）
- **SSE**：GET 长连接读 + POST 写，支持 `endpoint` 事件发现 message URL。兼容旧版 SSE transport 的 MCP server

不支持：
- WebSocket transport（MCP 规范未稳定）
- 旧版 HTTP streaming（已被 streamable HTTP + SSE 取代）

### 3. 配置形态

`~/.llaia/mcp.toml`（独立文件，与 cron.toml 一致策略）：

```toml
# ~/.llaia/mcp.toml

[[server]]
id = "filesystem"
enabled = true
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed/dir"]
env = { }
# tool_timeout_secs = 180  # 可选，per-server 工具调用超时覆盖默认值

# HTTP transport（streamable）
# [[server]]
# id = "remote-git"
# enabled = true
# transport = "http"
# url = "https://internal-mcp.corp/mcp"
# headers = { Authorization = "Bearer ${MCP_TOKEN}" }
# tool_timeout_secs = 300

# SSE transport（旧版兼容）
# [[server]]
# id = "legacy-sse"
# enabled = true
# transport = "sse"
# url = "https://legacy-mcp.corp/sse"
# headers = { Authorization = "Bearer ${MCP_TOKEN}" }
```

字段说明：
- `id`：server 标识，用于工具前缀（`<id>__<tool_name>`）
- `transport`：`stdio` / `http` / `sse`
- `command` + `args` + `env`：stdio 专用
- `url` + `headers`：HTTP/SSE 专用，`headers` 支持 `${ENV_VAR}` 环境变量插值（复用 P2-d 现有机制）
- `tool_timeout_secs`：可选，per-server 工具调用超时（秒），覆盖默认 180s，硬上限 600s

### 4. MCP 工具适配

MCP server 通过 `tools/list` 返回工具元数据，每个工具有 `name` / `description` / `inputSchema`（JSON Schema）。LLAIA 包装为 `Tool` trait 实现：

```rust
// src/tools/mcp.rs
pub struct McpTool {
    /// 双下划线前缀名：`<server_id>__<tool_name>`，如 `filesystem__read_file`
    prefixed_name: String,
    description: String,
    /// input_schema 用 Arc 共享，避免每次 spec 组装时深拷贝（MCP schema 可能数十 KB）
    input_schema: Arc<serde_json::Value>,
    /// 共享的 registry，调用时路由到正确 server
    registry: Arc<McpRegistry>,
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str { &self.prefixed_name }
    fn description(&self) -> &str { &self.description }
    fn parameters_schema(&self) -> serde_json::Value { (*self.input_schema).clone() }
    fn requires_confirm(&self) -> bool { true }  // 默认 true，受 P3-a 边界约束

    async fn execute(&self, args: &Value, channel: &str) -> Result<String> {
        let _ = channel;
        // registry 负责路由到正确 server，返回序列化后的 JSON 字符串
        self.registry.call_tool(&self.prefixed_name, args.clone()).await
    }
}
```

**工具命名**：所有 MCP 工具加 `<server_id>__` 前缀（双下划线），避免与内置工具冲突（如 `filesystem__read_file` vs 内置 `file_read`）。双下划线让 server name 和 tool name 分界明确，`split_once("__")` 即可拆分。

**McpRegistry 中央路由**（借鉴 zeroclaw）：
```rust
pub struct McpRegistry {
    servers: Vec<McpServer>,
    /// prefixed_name → (server_idx, original_tool_name)
    tool_index: HashMap<String, (usize, String)>,
    server_index: HashMap<String, usize>,
}
```
所有 MCP 工具调用走 `registry.call_tool(prefixed_name, args)`，registry 负责路由到正确 server。

**isError envelope 处理**：MCP spec 规定工具失败用 HTTP 200 + `result.isError: true` 表达（不是 JSON-RPC error）。`call_tool` 内部检查 `result.isError`，提取 `content[].text` 作为错误详情，映射为 `Err`。错误详情做 secret scrubbing + 长度截断（500 字符上限 + 省略号），避免泄漏 secret 到日志或 LLM 上下文。

**LLM 可见性**：MCP 工具默认挂载到主 agent。子 Agent 通过 `denied_tools` 黑名单过滤（沿用 P2-a 机制）。

### 5. 生命周期与错误处理

**启动**：`serve_cmd` / `chat_cmd` 启动时初始化所有 enabled MCP server：
- stdio：spawn 子进程（`kill_on_drop(true)`），建立 JSON-RPC 通道，发 `initialize` + `notifications/initialized` + `tools/list`
- HTTP：POST `initialize`，记录 `Mcp-Session-Id`，发 `tools/list`
- SSE：GET 长连接 + POST `initialize` + `tools/list`

**超时分层**（借鉴 zeroclaw）：
- `RECV_TIMEOUT_SECS = 30`：initialize / tools/list 握手
- `DEFAULT_TOOL_TIMEOUT_SECS = 180`：工具调用默认
- `MAX_TOOL_TIMEOUT_SECS = 600`：硬上限，per-server `tool_timeout_secs` 不超过此值
- per-server `tool_timeout_secs` 覆盖默认 180s

**失败处理**：
- 单个 server 初始化失败：不阻塞启动，`tracing::error!` + 跳过该 server 的所有工具
- 工具调用失败：返回错误给 LLM（非致命，LLM 可自行决定重试或换路径）

**bounded reconnect**（借鉴 zeroclaw）：
- `MAX_RECONNECT_ATTEMPTS = 2`，`RECONNECT_BACKOFF_MS = 500`
- 只在 `StaleSession`（HTTP 404/410 且携带 session id）或 `TransportClosed`（SSE stream EOF / stdio stdout 关闭）时触发重连
- 重连流程：`transport.reset()` → 重新 `handshake` → 重试原调用
- genuine tool error（包括 `isError: true`）和 timeout **不重连**，直接 surface 给 caller

**运行时崩溃**：
- stdio 子进程退出：相关工具调用返回错误，触发 bounded reconnect（重连即重新 spawn 子进程）
- HTTP/SSE 连接断开：同上

**关闭**：进程退出时 MCP registry 被 drop，`kill_on_drop(true)` 自动 kill stdio 子进程；HTTP/SSE 连接自动关闭

### 6. 与现有架构的整合

- **Tool trait 不变**：MCP 工具通过 adapter 实现 `Tool` trait，注册到 `AgentRegistry`
- **Agent 启动流程**：
  1. 加载 config.toml + mcp.toml
  2. `McpRegistry::connect_all(&mcp_configs)` 初始化所有 MCP client，拉取工具列表
  3. 每个 MCP 工具包装成 `McpTool`，加入 `AgentRegistry::main` 的工具列表
  4. 子 Agent 复用同一组 MCP registry（通过 `Arc<McpRegistry>` 共享），但可用工具受 `denied_tools` 过滤
- **channel 边界**：MCP 工具默认 `requires_confirm = true`，受 ADR-0011 的 agent workspace 边界 + confirm_mode 约束（与内置工具一致，不区分 channel）。某些只读 MCP 工具（如 `filesystem__read_file`）可手动配置 `requires_confirm = false`（在 mcp.toml 加 `safe_tools = ["read_file"]` 白名单）

### 7. WebUI 管理

在 WebUI 配置面板加 MCP tab：

- 列表展示所有 MCP server（id / transport / enabled / 状态：connected / dead / starting）
- 表单增删改查（编辑 mcp.toml）
- 工具列表：展开看每个 server 提供的工具（name / description / requires_confirm）
- "测试连接"按钮：手动触发 initialize + tools/list，显示结果
- 直接编辑 mcp.toml 原始文本（CodeMirror）

## 不做

- **MCP server 模式**：不把 LLAIA 的 file/terminal/memory 等工具暴露为 MCP server。理由见决策段
- **MCP resources/prompts**：P3-d 只支持 MCP tools，不支持 resources（只读数据源）和 prompts（提示词模板）。这两个是 MCP 规范的可选能力，生态支持较少，先不实现
- **MCP server 自动发现**：不支持零配置发现（如 mDNS），用户必须显式在 mcp.toml 配置
- **动态启停**：进程启动后不能动态添加 MCP server（必须重启）。WebUI 的"启用/禁用"是改 mcp.toml + 提示重启
- **外部 MCP SDK**：不引入 `rmcp` 等外部 SDK，完全自实现协议层（借鉴 zeroclaw）

## 影响

### 新增依赖

- 无新 crate 依赖（复用 `tokio` + `serde_json` + `reqwest` + `tokio-util` 等已有依赖）
- stdio transport 用 `tokio::process::Child`
- HTTP/SSE transport 用 `reqwest`（P2 WebUI 已引入）+ `tokio_util::io::StreamReader`（bytes_stream → AsyncBufRead）
- `kill_on_drop(true)` 是 `tokio::process::Command` 的内置方法，无新依赖

### 配置文件

- 新增 `~/.llaia/mcp.toml`（可选，不存在时无 MCP 工具）
- `llaia init` 生成空的 mcp.toml 模板

### 代码变更

- 新增 `src/mcp/mod.rs`：模块入口 + `McpServerConfig` 结构
- 新增 `src/mcp/protocol.rs`：JSON-RPC 2.0 类型 + MCP 协议常量（借鉴 zeroclaw `mcp_protocol.rs`）
- 新增 `src/mcp/transport.rs`：`McpTransportConn` trait + `StdioTransport` / `HttpTransport` / `SseTransport` 实现（借鉴 zeroclaw `mcp_transport.rs`）
- 新增 `src/mcp/client.rs`：`McpServer` + `McpRegistry`，含 `handshake` / `connect_all` / `call_tool` / `dispatch_method` / `check_result_is_error`（借鉴 zeroclaw `mcp_client.rs`）
- 新增 `src/tools/mcp.rs`：`McpTool` adapter，实现 `Tool` trait（借鉴 zeroclaw `mcp_tool.rs`，input_schema 用 `Arc<serde_json::Value>` 共享避免深拷贝）
- `src/agent/registry.rs`：`AgentRegistry` 启动时初始化 `McpRegistry`，把 MCP 工具注册到主 agent 的工具列表
- `src/commands/mod.rs`：`serve_cmd` + `chat_cmd` 启动 MCP 初始化（两种模式都启动，方便 CLI 调试 MCP 工具）
- `src/web/mod.rs`：加 `/api/mcp` 路由（GET 列表 / POST 创建 / PUT 更新 / DELETE 删除 / POST `/api/mcp/<id>/test` 测试连接）
- `src/web/static/app.js`：加 MCP tab UI

### 与 P3-a 的依赖

- MCP 工具默认 `requires_confirm = true`，受 ADR-0011 的 agent workspace 边界 + 命令策略 + confirm_mode 约束（与内置工具一致，不区分 channel）
- MCP 工具调用的副作用（如 `filesystem__write_file`）写 audit.log
- MCP server 自身的进程崩溃不影响 LLAIA 主进程（子进程隔离）
- MCP server 的 stdio 子进程 cwd 不受 agent workspace 约束（MCP server 是独立进程，自己管理自己的工作目录）

### MCP workspace 边界策略（Q2/Q3/Q4 决策）

**Q2 — stdio 子进程 workspace 边界**：不管。MCP server 是用户自己配的，信任用户配置（与"防 LLM 误操作不防恶意用户"原则一致）。mcp.toml 里显式配置 MCP server 允许访问的路径（如 `args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed/dir"]`），责任在用户。LLAIA 不强制 cwd、不校验 args 里的路径参数。

**Q3 — MCP 工具路径参数与路径防御的关系**：完全放行 + audit.log 全记录。理由：
- MCP 工具参数 schema 不统一（有的工具 `path`、有的 `file_path`、有的 `uri`、有的无路径参数），LLAIA 难以识别哪个参数是路径
- 强行解析会破坏工具语义（如 `filesystem__list_dir` 的 `path` 参数可以是目录也可以是 glob 模式）
- MCP 工具是"外部能力"，路径语义由 MCP server 自己定，LLAIA 不介入
- 但所有 MCP 工具调用（含参数和返回值摘要）写 audit.log，用户可事后审查

**只对 LLAIA 内置工具做路径防御**（file_read/file_write/file_edit/terminal 走 ADR-0011 §3.2 三层防御），MCP 工具完全放行。

**Q4 — HTTP transport 鉴权**：仅支持环境变量插值（`${VAR}` 机制），够用。理由：
- `${MCP_TOKEN}` 在 config 加载时从环境变量替换，secret 不落 config.toml
- 不加 mcp.toml 内 `token` / `api_key` 字段（避免 secret 明文落盘）
- 不支持 OAuth 流程（单用户私人助理场景不需要，远程 MCP server 用 Bearer token 足够）

## 参考

- [Model Context Protocol 规范](https://modelcontextprotocol.io/)
- [MCP server 生态列表](https://github.com/modelcontextprotocol/servers)
- ZeroClaw 支持 MCP（client + server 双模式，LLAIA 仅做 client）
  - `crates/zeroclaw-tools/src/mcp_protocol.rs`：JSON-RPC 类型 + 协议常量
  - `crates/zeroclaw-tools/src/mcp_client.rs`：`McpServer` + `McpRegistry` + handshake + bounded reconnect + isError 处理
  - `crates/zeroclaw-tools/src/mcp_transport.rs`：Stdio/Http/Sse 三种 transport + `kill_on_drop` + `Mcp-Session-Id` + SSE endpoint 发现
  - `crates/zeroclaw-tools/src/mcp_tool.rs`：`McpToolWrapper` 适配 `Tool` trait + `Arc<serde_json::Value>` 共享 schema
  - LLAIA 直接借鉴以上四个模块的实现模式
- AstrBot 率先拥抱 MCP 协议
- grilling 第三轮 Q3：用户选择"纯 client"
- grilling 第九轮（P3-d 细化）：
  - Q1 改为自实现（不用 `rmcp`），借鉴 zeroclaw 协议层（< 500 行，零外部依赖风险）
  - transport 改为三种：Stdio + HTTP（streamable）+ SSE
  - 工具命名改双下划线：`<server_id>__<tool_name>`
  - 补 isError envelope 处理（HTTP 200 + `result.isError: true`）+ secret scrubbing + 长度截断
  - 补超时分层：init 30s / tool 默认 180s / 硬上限 600s / per-server 可配
  - 补 `kill_on_drop(true)` 子进程管理
  - 补 `Mcp-Session-Id` header 支持（HTTP transport 会话管理）
  - 补 bounded reconnect（MAX_RECONNECT_ATTEMPTS=2，只在 StaleSession/TransportClosed 时触发）
  - Q2: stdio 子进程 workspace 边界不管，信任用户配置
  - Q3: MCP 工具路径参数完全放行 + audit.log 全记录（只内置工具走路径防御）
  - Q4: HTTP 鉴权仅支持环境变量插值，不支持 OAuth
