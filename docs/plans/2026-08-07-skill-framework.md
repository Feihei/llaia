# P3-e：Skill 技能框架 — 实现计划

- 日期：2026-08-07
- 状态：✅ 已完成（2026-08-07）
- 设计依据：[ADR-0015](../adr/0015-skill-framework.md)

## 目标

在 MCP 工具之上封装"提示词 + 工具推荐"的技能包（SKILL.md + YAML frontmatter，对齐 OpenAI Codex CLI / Anthropic Claude Skills / AstrBot 业界标准）。启动时扫描 `~/.llaia/skills/*/SKILL.md`，通过 Progressive Disclosure 把 active skill 的 name + description + 路径注入 system prompt，LLM 触发时自己 `file_read` 完整 SKILL.md。

## 文件清单

| 文件 | 变更 | 说明 |
|---|---|---|
| `Cargo.toml` | 改 | 新增 `serde_yaml = "0.9"`（解析 frontmatter） |
| `src/skill/mod.rs` | 新增 | `SkillManifest` 结构 + 模块入口 |
| `src/skill/loader.rs` | 新增 | 扫描 skills 目录、解析 frontmatter、skills.json active 开关读写、内置示例 skill 种子 |
| `src/skill/prompt.rs` | 新增 | `build_skills_prompt` 生成 system prompt 的 "## Skills" 段 + name/path/description 安全过滤 |
| `src/lib.rs` | 改 | 挂 `skill` 模块 |
| `src/channels/cli.rs` | 改 | `build_single_agent` 扫描 skills 并在 system prompt 末尾追加 skills 段（主/子 agent 均注入） |
| `src/tools/file.rs` | 改 | `FileRead` 增加 `skills_dir` 特殊放行：`<skills_dir>/<name>/SKILL.md` 可读（workspace 之外） |
| `src/web/mod.rs` | 改 | AppState 加 `skills_dir`；`/api/skills` 系列路由（列表/创建/删除/active 切换/content 读写） |
| `src/channels/web.rs` | 改 | AppState 构造填 `skills_dir`（从 config_path 推导） |
| `src/web/static/index.html` | 改 | Config 面板 Skills section（替换 WIP 占位）：列表 + active 开关 + SKILL.md 编辑器 |
| `src/web/static/app.js` | 改 | Skills CRUD API 封装 |
| `src/commands/mod.rs` | 改 | doctor 增加 skills 检查项 |
| `tests/skill.rs` | 新增 | loader / prompt / file_read 放行的集成测试 |

## 关键设计（对齐 ADR-0015）

1. **Skill 格式**：`~/.llaia/skills/<name>/SKILL.md`，YAML frontmatter 字段 `name`（必需，正则 `^[\w.-]+$`）/ `description`（必需）/ `duration`（`turn` 默认 / `session`，P3-e 仅记录不影响行为）/ `tools`（可选，仅 prompt 提示，不控制挂载——方案 C）。frontmatter 缺失或 name 非法时目录名作为 fallback name，解析失败 log + 跳过。
2. **Progressive Disclosure**：启动时只解析 frontmatter（不读 body），system prompt 追加 "## Skills" 段列出 name + description + SKILL.md 绝对路径 + 规则（用前必须先 file_read）。
3. **active 开关**：`~/.llaia/skills.json`（`{"skills": {"<name>": {"active": bool}}}`）。扫描到的新 skill 默认 active=true 并自动写回 skills.json；skills.json 不存在/损坏时全部默认 active（损坏 log warn）。
4. **内置示例 skill**：code-review / news-digest / todoist 三个 SKILL.md 模板内嵌为常量。**首次扫描时 skills 目录不存在** → 创建目录并写入三个示例（on-demand，`llaia init` 不生成）；目录已存在则不触碰。
5. **file_read 特殊放行**：SKILL.md 路径在 `~/.llaia/skills/`（agent workspace 之外）。FileRead 执行时先做 skill 路径匹配：`~` 展开 + 词法规范化后 canonicalize `starts_with(skills_dir)` 且文件名恰为 `SKILL.md` → 放行；其余 `~/.llaia/` 路径仍拒绝（保护 config.toml / skills.json 等）。file_write / file_edit 不放行。
6. **路径安全**：skill name 注入 prompt/API 前用 `^[\w.-]+$` 校验；description / path 注入 prompt 前过滤控制字符与反引号（防 prompt injection，借鉴 AstrBot `_SAFE_PATH_RE` / `_CONTROL_CHARS_RE`）。
7. **WebUI API**（改后需重启 serve 才注入新 system prompt，与 cron/mcp raw 编辑一致）：
   - `GET /api/skills` → 列表（name / description / duration / tools / active / path）
   - `POST /api/skills` → 创建（body `{name, content?}`，name 正则校验，content 缺省用模板）
   - `DELETE /api/skills/:name` → 删除 skill 目录
   - `PUT /api/skills/:name/active` → 切换 active（写 skills.json）
   - `GET /api/skills/:name/content` → 读 SKILL.md 原文
   - `PUT /api/skills/:name/content` → 写 SKILL.md（先校验 frontmatter 可解析）
8. **触发机制**：纯 LLM 判断（name + description），无关键词匹配、无特殊语法、无 `activate_skill` 工具。

## Task 拆分

- Task 1：skill 模块（manifest + loader + prompt + 安全过滤）+ 单测
- Task 2：cli.rs system prompt 注入 + FileRead 特殊放行 + 内置示例 skill
- Task 3：WebUI API + 前端 Skills section
- Task 4：doctor 检查项 + 集成测试 + fmt/clippy/test 全绿
- Task 5：更新 plan.md 状态

## 实现补记

- **新增依赖**：`serde_yaml 0.9`（frontmatter 解析）；dev 依赖 `tower 0.4 + util`（WebAPI oneshot 集成测试）。
- **按需种子**：ADR 要求 init 不生成 skills/ 目录，但需随附 3 个示例——采用 on-demand 策略：首次 `load_skills` 发现目录不存在才创建并种子 code-review / news-digest / todoist；`doctor` 只 `scan_skills` 不种子。
- **skills.json 按目录名存取** active 状态（目录名是磁盘唯一标识，frontmatter name 可能与目录名不一致）；新 skill 默认 `active=true` 并自动写回；删除 skill 时同步清理对应条目。
- **生效模型**：skill 在启动时注入 system prompt，WebUI 改动需重启 serve/chat 生效（与 cron / mcp raw 编辑行为一致），不做热重载。
- **file_read 特殊放行**：FileRead 新增可选 `skills_dir` 参数，execute 前置调用 `resolve_skill_path`（`~` 展开 + 相对路径按 workspace 解析 + 词法规范化 + canonicalize 落在 skills_dir 内且文件名恰为 SKILL.md）；复用 path_guard 的 `normalize_lexical` / `strip_verbatim_prefix`（改 `pub(crate)`）。file_write / file_edit 不放行。
- **注入范围**：主 agent 与子 agent 均注入 Skills 段；prompt 注入前经 `sanitize_prompt_text` 过滤控制字符与反引号（反引号→单引号）。
- **安全校验**：`is_valid_skill_name` 手写实现（仅 ASCII 字母数字 `_-.`，拒绝 `.` / `..` / 路径分隔符），WebUI 创建 / 删除 / 切换 / 读写 content 全链路校验；写 content 前 `validate_skill_md` 校验 frontmatter 完整性。
- **前端**：Skill 编辑器用普通 monospace textarea（vendor CodeMirror 只有 toml mode，不引入新 mode）。
- **clippy 处理**：`build_single_agent` 加第 8 个参数后附 `#[allow(clippy::too_many_arguments)]`（与 Agent::new 先例一致）；`skill_name_or_err` Err 变体改 `Box<Response>`；skills.json 新条目写回改 Entry API。
- **测试**：模块内单测 + `tests/skill.rs` 集成测试 3 个（种子一次性、prompt 含 SKILL.md 绝对路径、Skills API 全生命周期 11 步）。CI 三道门（fmt/clippy/test）全绿：233 单元测试 + 全部集成测试套件。
