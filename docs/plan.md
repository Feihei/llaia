# LLAIA 项目 Roadmap

> 本文档是 LLAIA 的**前瞻路线图**：顶部是已交付阶段一览（索引），主体是下一步计划（P5）。
> 各阶段的**完整交付清单**见 [`CHANGELOG.md`](CHANGELOG.md)；详细实现计划见 [`plans/`](plans/)，设计规格见 [`specs/`](specs/)，架构决策见 [`adr/`](adr/)。

**整体目标**：一个单用户、本地优先的私人 AI 助理，跨 CLI/QQ/Web 等多 channel 接入，主 Agent + 可委派子 Agent 协作，持久化记忆与会话。

---

## 状态图例

- ✅ 已完成
- 🚧 进行中
- ⏳ 计划中（未开始）

---

## 已交付阶段一览

| 阶段 | 状态 | 一句话目标 | 交付清单 |
|---|---|---|---|
| P1 | ✅ | MVP：CLI 单 channel，REPL + 基础工具 + 持久化 | [CHANGELOG.md](CHANGELOG.md)（§P1） |
| P1.5 | ✅ | QQ channel + 全 channel 流式输出 + 稳定性补丁 | [CHANGELOG.md](CHANGELOG.md)（§P1.5） |
| P2 | ✅ | 子 Agent 委派 + 交互增强 + Web channel | [CHANGELOG.md](CHANGELOG.md)（§P2） |
| P3 | ✅ | 能力扩展与生态接入（边界/init/cron/MCP/Skill） | [CHANGELOG.md](CHANGELOG.md)（§P3） |
| P3+ | ✅ | 交互增强与生态扩展（快赢/Anthropic/Telegram/钉钉/微信） | [CHANGELOG.md](CHANGELOG.md)（§P3+） |
| P4 | ✅ | 基础能力增强（时区/做梦/压缩/权限/shutdown/Gemini/飞书…） | [CHANGELOG.md](CHANGELOG.md)（§P4） |

---

## P5 — 未来计划（下一步）

**状态**：⏳ 计划中

> 来自 `docs/issues/` 反馈与扩展评估的候选池。下方条目按主题分组，并标注**必要性**（高/中/低，不做会持续踩坑或已影响正确性→高；明显改善体验→中；锦上添花→低）与**难度**（★☆☆ 半天内单点 / ★★☆ 一到数天跨模块 / ★★★ 结构性改造，动手前先出 ADR），便于排期。

### 模型与 Provider

- [ ] provider 接入优化：参考 zeroclaw、goose 等，针对 Ollama / Llama.cpp 等 OpenAI 兼容端点的格式与行为做专项适配（必要性：**中** / 难度：★★☆）
- [ ] 系统提示词优化：言简意赅、占更少 token，参考 pi 等项目（必要性：**中** / 难度：★☆☆）

### WebUI 增强

- [ ] session.db 会话历史在 WebUI 中可查询/修改，参考 AstrBot（必要性：**中** / 难度：★★☆）
- [ ] WebUI provider API 探测可用模型，点击添加到 models；添加按钮检查可用性，参考 AstrBot（必要性：**中** / 难度：★★☆）

### 安全

- [ ] 敏感信息存储：api-key 等自动写入 `.env`，config 只保留环境引用；探讨二进制（如 db）存储避免明文（必要性：**高** / 难度：★★☆）

### 生态与工具

- [ ] 环境探测：本地 shell / python / node / rust / go 等环境探测，据情况提示 agent 优化行为（必要性：**中** / 难度：★☆☆）
- [ ] skill 增强：在现有 skill 工具基础上针对 llaia 优化；npx skills 工具的 rust 实现；claude 创建 skill / hermes curator 等"管理 skill 的元 skill"的 llaia 化（必要性：**中** / 难度：★★☆）
- [ ] 自然对话给主 agent 添加 MCP 工具（必要性：**中** / 难度：★★☆）

### 语音

- [ ] TTS 服务接入、发语音（必要性：**低** / 难度：★★☆）

### 目标系统

- [ ] `/goal` 长期目标，参考 zeroclaw、hermes（必要性：**中** / 难度：★★☆）

---

## 工程约定

- 每个 Task 完成后跑 `cargo test` + `cargo clippy`
- 提交节奏：一个完整功能/修复链路验证通过后提交一次，不要每个 Task 都提交
- 遇到编译错误立即修，不要积累
- 详细实现计划放 `docs/plans/YYYY-MM-DD-<feature>.md`，设计规格放 `docs/specs/YYYY-MM-DD-<feature>-design.md`，架构决策放 `docs/adr/NNNN-<topic>.md`
- 阶段交付后，其完整勾选清单迁入 `docs/CHANGELOG.md`，本文件只保留「已交付阶段一览」索引 + 下一步计划
