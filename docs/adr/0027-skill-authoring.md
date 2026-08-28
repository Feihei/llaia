# ADR-0027: Skill 自管（skill_create / skill_edit 工具 + 内置元 skill）

- 状态：Accepted
- 日期：2026-08-14
- 关联：plan.md §P5「skill 增强」；参考 deepseek `packages/skill` 元技能、pi frontmatter 校验；`src/skill/loader.rs`、`src/skill/mod.rs`、`src/skill/prompt.rs`

## 背景 / Context

llaia 已有扎实的 skill 系统：`src/skill/loader.rs` + `prompt.rs` 已实现 **progressive disclosure**（name+desc+path 进 prompt，全文 `file_read`）；`skills.json` 管 active 开关；目录名 = 标识；frontmatter name/description 校验；`resolve_skill_path` 防越权；WebUI 可创建（`default_skill_template`）。

但 agent **无法自己创建/修改 skill**：skill 目录（`~/.workbuddy/skills/` 用户级、`{workspace}/.workbuddy/skills/` 项目级）在主 agent 文件作用域外，文件工具够不到。用户明确**不想要** npx-skills 式「搜索 + 自动安装」（怕 hermes 式繁杂 skill 集），更愿自己甄选。deepseek 有「管理 skill 的元 skill」（`dsh-*` 系列）思路可借鉴——但两参考都未内置让 agent 自管 skill 的现成机制，需 llaia 自建。

## 决策 / Decision

1. **不做** npx-skills 的搜索 + 自动安装。
2. 加 `skill_create` / `skill_edit` **工具**：
   - `skill_create { name, description, content, scope? }`：在 `scope`（默认 `"user"`，可选 `"project"`）对应 skills 目录建 `<name>/SKILL.md`；目录名 = name（kebab-case 校验）；`content` 写 SKILL.md。
   - `skill_edit { name, content | old_string + new_string | append, scope? }`：改已存在 skill 的 SKILL.md。三种编辑模式互斥单选（2026-08-28 修订：原 `patch` 的 string|object union 分派诱发弱模型把 `{find,replace}` 对象二次序列化成字符串、JSON 原文被追加进文件，改为与 `file_edit` 对齐的扁平命名参数；替换要求 old_string 唯一命中）。
   - 路径经 `resolve_skill_path` 校验，禁止越权写到 skills 目录外；`scope` 只允许 `user` / `project` 两值。
   - 创建/编辑后自动跑现有 `validate_skill_md`（frontmatter 校验：name 匹配 dirname、description 必填非空、长度上限）。
3. 加一个**内置元 skill**（如 `skill-authoring`，随 llaia 发布）：引导 agent 如何按 llaia 约定创建/审查/整理 skill（frontmatter 约束、progressive disclosure、路径安全、`validate_skill_md` 规则、何时该建 skill vs 直接做）。agent 想自管 skill 时先加载此元 skill。
4. 补 frontmatter 长度/字符约束（对齐 pi：description ≤ 1024、name kebab-case）。
5. **不**引入引擎级 skill import（两参考都只靠 markdown 链接复用）。

## 备选 / Alternatives

- **元技能以「内置 skill」还是「工具」暴露**：决定为**工具**（`skill_create`/`skill_edit` 负责写盘，因涉及越权/路径安全）+ **内置元 skill**（负责方法论引导）。工具保证路径安全，元 skill 保证规范一致。
- **抽 `loader.rs` 为独立「npx skills rust 实现」**：不单独做——现有 loader 已够用，`skill_create` 直接复用其扫描/校验逻辑（`resolve_skill_path` / `validate_skill_md`）。

## 后果 / Consequences

- 正向：agent 能自管 skill（创建/改/审），复用既有约束；用户保留甄选权（不自动装一堆繁杂 skill）。
- 负向：新增两工具 + 一元 skill；需路径安全测试（`/etc/passwd` 类越权必须被拒）。

## 待办（实现计划）

见 [`plans/2026-08-14-skill-authoring.md`](../plans/2026-08-14-skill-authoring.md)。
