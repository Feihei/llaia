# LLAIA 用户指南

本目录是按**功能模块**组织的用户文档（任务导向、面向使用）。开发者向的架构决策、设计规格、实现计划分别在 `../adr/`、`../specs/`、`../plans/`、`../issues/`，不在本指南范围。

> 想直接用起来？从 [快速开始](quick-start.md) 走一遍。

## 入门

| 文档 | 内容 |
|---|---|
| [安装](installation.md) | 二进制 / Docker / 从源码编译 |
| [快速开始](quick-start.md) | init → 配 provider → 启动 → 第一次对话 |
| [CLI 参考](cli.md) | `chat` / `serve` / `init` / `config` / `doctor` / `remember` 与全局 `--config-dir` |
| [配置参考](configuration.md) | `config.toml` 全部字段（runtime / provider / agent / webui / tools / channels） |

## 接入方式

| 文档 | 内容 |
|---|---|
| [Web UI](webui.md) | 浏览器入口、token、设置热更新、文件上传、管理接口 |
| [频道](channels.md) | QQ / Telegram / 钉钉 / 微信 / 邮箱 / 飞书，单用户安全锁 |

## 扩展能力

| 文档 | 内容 |
|---|---|
| [定时任务](cron.md) | `cron.toml`：agent 模式 / 工具链模式、推送频道 |
| [MCP](mcp.md) | 接入外部工具与数据源，server 配置与工具命名 |
| [技能](skills.md) | 可复用工作流，`SKILL.md`，用户级 / 项目级 |

## 使用与运维

| 文档 | 内容 |
|---|---|
| [记忆与上下文](memory-and-context.md) | SOUL/USER/MEMORY、sessions.db、压缩、做梦 |
| [斜杠命令](slash-commands.md) | 会话内全部 `/` 命令 |
| [内置工具](tools.md) | file / terminal / web / search / memory / delegate / cron / mcp |
| [权限与安全](permissions.md) | 权限档位、交互式审批、硬边界 |
| [常见问题](faq.md) | 排错与高频疑问 |

## 文档约定

- 本指南内相互引用用同目录相对路径（如 `configuration.md`）。
- 指向开发者文档用 `../adr/xxxx.md` 等。
- 顶层 README 只保留安装 / 快速启动 / 核心功能入口的简介，详情都在本指南。
