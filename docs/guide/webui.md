# Web UI

Web UI 是 LLAIA 最完整的图形入口，随 `llaia serve` 启动。

## 访问

```bash
llaia serve
# 打开 http://127.0.0.1:51217
```

- 地址/端口由 `[webui]` 配置（`host` 默认 `127.0.0.1`，`port` 默认 `51217`）。
- 鉴权 token：若 `webui.token` 留空，启动时**随机生成并打印在日志**里；设了就用你设的。
- 想让同网其他设备访问，把 `host` 改成 `0.0.0.0` 并自行负责安全（建议配合固定 token + 反向代理）。

## 配置（热更新）

Web UI 的设置页可填 provider / model / runtime / channels 等，保存后写入 `config.toml` 并**热加载**——下一轮对话即生效，无需重启。例如改 `[runtime].timezone` 后状态栏时区立即更新（见 [ADR-0017](../adr/0017-timezone-injection.md)）。

无 provider 时 `serve` 仍会启动，Web UI 配置功能可用，聊天降级提示——先把 provider 配好即可开聊。

## 对话

对话通过 WebSocket（`/ws`）进行，支持流式输出（见 [ADR-0010](../adr/0010-streaming-output.md)）。

## 文件

- 上传：`POST /upload` → 落到 `workspace/uploads/`，可在对话里引用。
- 取回：`GET /file` 从工作区提供文件。

## 管理界面

- **定时任务**：查看 / 新增 / 修改 / 触发 / 看执行历史（见 [定时任务](cron.md)）。
- **MCP**：查看 / 增删 server（见 [MCP](mcp.md)）。
- **技能**：查看 / 删除（见 [技能](skills.md)）。

## REST API 一览

| 方法 & 路径 | 作用 |
|---|---|
| `GET /` | SPA 首页 |
| `GET /static/*path` | 静态资源 |
| `POST /upload` | 上传文件到 `workspace/uploads/` |
| `GET /file` | 从工作区取文件 |
| `GET /api/config` · `PUT /api/config` | 读 / 写配置 |
| `POST /api/config/validate` | 校验配置 |
| `POST /api/restart` | 重启服务 |
| `POST /api/shutdown` | 优雅停止 serve（等价 `Ctrl+C`，见 [ADR-0018](../adr/0018-shutdown.md)） |
| `GET /api/status` | 运行状态 |
| `GET /api/cron` · `POST /api/cron` | 列出 / 增改定时任务 |
| `GET /api/cron/history` | 定时任务执行历史 |
| `POST /api/cron/:id/trigger` | 手动触发某个任务 |
| `GET /api/mcp` · `POST /api/mcp` · `DELETE /api/mcp` | 列出 / 增 / 删 MCP server |
| `GET|POST|PUT|DELETE /api/skills/:name` | 技能管理 |

> 这些接口需要 Web UI 的 token 鉴权（与 `webui.token` 一致）。

## 相关

- 启动与停止：[CLI 参考](cli.md)
- 配置字段：[配置参考](configuration.md)
