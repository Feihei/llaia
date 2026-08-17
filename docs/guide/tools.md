# 内置工具（Tools）

agent 在对话中可以调用的工具。模型通过「原生 function calling」或「标签协议降级」来触发它们（由 `native_tool_calling` 决定，见 [配置参考](configuration.md)）。

## 工具清单

| 工具 | 用途 | 有副作用？ |
|---|---|---|
| `file_read` | 读文件 | 否（只读） |
| `file_write` | 写文件 | 是 |
| `file_edit` | 精确修改文件 | 是 |
| `terminal` | 跑终端命令（含 ls/grep 等） | 视命令而定 |
| `web_fetch` | 抓取网页 | 否 |
| `search` | 联网搜索（统一工具，按 `[tools.search].provider` 路由到 tavily/baidu/brave，需对应 key） | 否 |
| `memory_write` | 写 `MEMORY.md` 长期记忆 | 是 |
| `send_media` | 发送图片/媒体 | 否 |
| `delegate` | 委派子 agent 异步任务 | 视委派内容 |
| `cron` | 管理定时任务 | 是 |
| `mcp` | 调用已接入的 MCP server 工具 | 视工具而定 |

## 副作用与确认

工具的 `requires_confirm()` 区分「只读」与「有副作用」：

- 文件写/改、终端、记忆写、MCP 工具等**有副作用**的工具默认需要确认。
- 是否真的弹确认、确认范围多大，由[权限档位](permissions.md)（`read-only` / `default` / `yolo`）决定。
- MCP 工具一律视为「workspace 外」，安全默认。

## 终端安全

`[tools.terminal]` 的 `command_policy` 控制命令边界：

- `blacklist`（默认）：命令黑名单拦截危险命令。
- `whitelist`：只允许 `command_whitelist` 里的命令。
- `none`：不限制（谨慎）。

路径黑名单、命令黑名单等**硬边界在所有权限档位下始终生效**，无法绕过。
