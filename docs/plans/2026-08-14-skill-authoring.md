# 实现计划：Skill 自管（skill_create / skill_edit + 元 skill）

> 关联 ADR：[0027-skill-authoring.md](../adr/0027-skill-authoring.md)
> 日期：2026-08-14

## Goal

让 agent 通过 `skill_create` / `skill_edit` 工具直接写/改 SKILL.md（落用户级默认、可选项目级），路径安全校验；加内置 `skill-authoring` 元 skill 引导方法论。不做 npx 式搜索 / 自动安装。

## Architecture

- `src/tools/skill_create.rs`（新）：参数 `{ name, description, content, scope? }`，建 `<skills_dir>/<name>/SKILL.md`，校验 name kebab-case、description 非空、路径经 `resolve_skill_path`；写完跑 `validate_skill_md`。
- `src/tools/skill_edit.rs`（新）：参数 `{ name, content | patch, scope? }`，改已存在 SKILL.md。
- skills 目录解析：`scope="user"` → `~/.workbuddy/skills/`；`scope="project"` → `{workspace}/.workbuddy/skills/`。
- 内置元 skill：`src/.../skills/skill-authoring/SKILL.md`（随 agent 发布），引导创建/审查/整理。
- 复用 `src/skill/loader.rs` 的 `resolve_skill_path` / `validate_skill_md`（确保已 `pub`）。

## Tech Stack

Rust（llaia 单 crate）。复用 skill loader 现有校验。

## 文件结构

- `src/tools/skill_create.rs`（新）
- `src/tools/skill_edit.rs`（新）
- `src/tools/mod.rs`：注册（仅 main agent）
- `src/skill/loader.rs`：导出 `resolve_skill_path` / `validate_skill_md`（若未 pub）
- `src/skill/.../skill-authoring/SKILL.md`（新，内置元 skill）
- `src/config.rs`：skills 目录解析辅助（user / project）

## 分步 Task

1. [ ] 确认 `resolve_skill_path` / `validate_skill_md` 为 `pub` 且可复用。
2. [ ] `skill_create` 工具：建目录 + 写 SKILL.md + 校验 + 路径安全。
3. [ ] `skill_edit` 工具：改已存在 SKILL.md + 校验。
4. [ ] 注册工具（main agent）。
5. [ ] 内置 `skill-authoring` 元 skill（SKILL.md，含 frontmatter 约束 / progressive disclosure / 路径安全 / 何时建 skill）。
6. [ ] 单测：路径越权被拒、name 校验、scope 两值、内容写入正确。
7. [ ] 文档：AGENTS.md 补 skill 自管说明。

## 自查

- [ ] `cargo test` + `cargo clippy` 绿
- [ ] `skill_create` 写到 `~/.workbuddy/skills/`，可被 loader 发现并 progressive disclosure 加载
- [ ] 越权路径（如 `/etc/passwd`）被 `resolve_skill_path` 拒绝
- [ ] 元 skill 加载后 agent 能按引导创建 skill
- [ ] 不自动安装任何外部 skill（用户甄选权保留）
