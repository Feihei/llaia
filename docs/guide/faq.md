# 常见问题（FAQ）

## `llaia doctor` 都查什么？

检查项：config 目录、provider 连通性（请求 `/models` 探活）、`runtime` 参数、`timezone` / `permission` 合法性、`cron.toml` / `mcp.toml` 解析与启用数、skills 扫描、`sessions.db` 存在性。排错第一步先跑它。

## 启动报 "No provider configured, cannot start chat"

`chat` 模式必须有 provider。两种解法：

- 跑 `llaia serve`，在 Web UI（http://127.0.0.1:51217）里配好 provider；或
- 编辑 `~/.llaia/config.toml`，取消注释 `[provider.default]` 与 `[agent.main]` 的 `model` 引用。

`serve` 模式更宽松：无 provider 也能启动，只是聊天降级、Web UI 配置可用。

## serve 启动了但聊不了天

多半是没配 provider（降级模式）。`llaia doctor` 会提示 `[warn] No provider configured`。去 Web UI 补上 provider，下一轮对话即生效（热更新）。

## Web UI 的 token 找不到

`webui.token` 留空时，随机 token 打印在**启动日志**里（`docker logs llaia | grep -i token` 或本地日志文件 `<config_dir>/logs/`）。要稳定，就在 `[webui]` 里写死 `token = "你的固定token"`。

## 上下文越聊越长，没自动压缩？

- 压缩在上下文占用超过 `[runtime].context_threshold`（默认 0.7）时触发；可手动 `/compact`。
- 确认 `context_size` 合理：本地模型不配 `context_size` 时启动会探测，探测失败回退 8192。可在 model 下显式设 `context_size`。
- 想用更便宜的模型压缩：`[runtime].compact_model`。

## 旧配置不认 `[channels.web]` / `confirm_mode = "whitelist"`

- 旧 `[channels.web]` 自动迁移到 `[webui]`（向后兼容）。
- `confirm_mode = "whitelist"` 已废弃，自动回退 `none`，新请用权限档位（见 [权限与安全](permissions.md)）。
- `[agent.main]` 的 `workspace` / `soul` / `user` / `memory` 已废弃，自动推导到 `workspace/`，显式设置会告警。

## 我的数据都在哪？

默认 `~/.llaia/`：`config.toml`、`.env`、`cron.toml`、`mcp.toml`、`logs/`、`skills/`、`workspace/`（含 SOUL/USER/MEMORY、uploads、subagent、sessions.db）。用 `--config-dir` 可整体换地方。

## 怎么备份 / 回滚记忆？

- `sessions.db` 是会话历史 source of truth，直接备份文件即可。
- `MEMORY.md` 被「做梦」改写前会存 `workspace/MEMORY.backups/`；`/dream-rollback` 一键回滚到最近备份。

## 想从头重来

删掉（或移走）数据目录即可重置，再 `llaia init` 重建。涉及删除请先备份重要数据。

## 微信频道登录失效

登录态在 `<config_dir>/wechat_state.json`，不是 config。失效就重新 `llaia serve` 扫码。

## 相关

- 配置问题：[配置参考](configuration.md)
- 连通性诊断：[CLI 参考](cli.md)
- 权限边界：[权限与安全](permissions.md)
