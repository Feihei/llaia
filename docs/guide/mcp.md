# MCP（Model Context Protocol）

通过 MCP 把外部工具与数据源接进 LLAIA。server 定义在 `mcp.toml`，修改后**重启 `serve`/`chat` 生效**。`llaia init` 会生成全注释模板。

> 客户端实现与协议细节见开发文档 [ADR-0014](../adr/0014-mcp-client.md)。

## Server 字段

每个 server 写在 `[[server]]` 表里：

| 字段 | 说明 |
|---|---|
| `id` | server 唯一 ID，工具名前缀。 |
| `enabled` | 是否启用（默认建议 `true`）。 |
| `transport` | `stdio` / `http` / `sse`。 |
| `command` + `args` | `transport=stdio` 时启动本地子进程的命令与参数。 |
| `url` | `transport=http` / `sse` 时的远程地址。 |
| `headers` | `transport=http` 时的请求头（如 `Authorization = "Bearer ${MCP_TOKEN}"`，secret 放 `.env`）。 |
| `safe_tools` | 免确认的工具名列表（默认 MCP 工具都需确认）。 |
| `tool_timeout_secs` | 单工具超时（秒）。 |

## 工具命名

MCP 工具注册为 `<server>__<tool_name>`，例如 `filesystem__read_file`。agent 像调用内置工具一样调用它们。

## 示例

```toml
# stdio 本地子进程
[[server]]
id = "filesystem"
enabled = true
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed/dir"]
safe_tools = ["read_file", "list_directory"]

# streamable HTTP 远程 server
[[server]]
id = "remote"
enabled = true
transport = "http"
url = "https://internal-mcp.corp/mcp"

[server.headers]
Authorization = "Bearer ${MCP_TOKEN}"

# 旧版 SSE
[[server]]
id = "legacy-sse"
enabled = true
transport = "sse"
url = "https://legacy-mcp.corp/sse"
```

## 安全

- MCP 工具默认 `requires_confirm = true`（有副作用）；`safe_tools` 里的工具免确认。
- 按[权限档位](permissions.md)，MCP 工具一律视为「workspace 外」，安全默认——即便 `yolo` 档，硬边界仍生效。

## 管理

- **Web UI**：`/api/mcp` 系列接口查看 / 增删（见 [Web UI](webui.md)）。
- **CLI**：`llaia doctor` 会列出 `mcp.toml` 中的 server 与启用状态。
