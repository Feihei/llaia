# 技能（Skills）

技能用可复用的提示词 + 元数据，给 agent 扩充工作流、领域知识或工具集成。一个技能就是一个带 `SKILL.md` 的文件夹。

> 技能框架设计与加载机制见开发文档 [ADR-0015](../adr/0015-skill-framework.md)。

## 存放位置

- **用户级**：`~/.llaia/skills/<skill-name>/`（对所有工作区生效）
- **项目级**：`.workbuddy/skills/<skill-name>/`（仅当前项目）

```
~/.llaia/skills/
 └── my-skill/
     └── SKILL.md       # 技能定义（prompt + 元数据）
```

## 加载

- agent 启动时扫描并加载技能，**无需重启**（项目级技能改完下次对话即生效）。
- 内置示例技能在首次 `chat` / `serve` 启动时种子（seed）到 `skills/`。

## 管理

- **Web UI**：`/api/skills/:name` 系列接口查看 / 删除（见 [Web UI](webui.md)）。
- **CLI**：`llaia doctor` 会扫描 `skills/` 并列出已激活的技能。
- **agent 自管（ADR-0027）**：agent 可直接创建 / 修改 skill，无需你手动写文件。
  - `skill_create { name, description, content, scope? }`：在 `<scope>` 对应 skills 目录建 `<name>/SKILL.md`。`name` 为 kebab-case 目录名；`content` 是 skill body（markdown），frontmatter 自动从 `name`/`description` 生成；`scope` 为 `user`（默认，`~/.workbuddy/skills/`，对所有工作区生效）或 `project`（`<workspace>/.workbuddy/skills/`，仅当前项目）。已存在则拒绝覆盖。
  - `skill_edit { name, content | patch, scope? }`：改已存在 skill。`content` 整文件替换；`patch` 为字符串（追加到正文）或对象 `{ "find": "...", "replace": "..." }`（单次精确替换）。
  - 这两个工具只注册在 main agent，写盘前做路径安全校验，且 frontmatter 经 `validate_skill_md` 校验（name + description 必填、name ≤ 64 / description ≤ 1024 字符）。
  - 想按规范自管 skill 时，先让 agent 加载内置元 skill **`skill-authoring`**（随 llaia 发布，首次启动即确保存在），它引导何时该建 skill、frontmatter 约束与路径安全。

> 注意：项目级 skill（`scope="project"`）写出后不会自动注入 system prompt——当前只有用户级 skills 目录参与启动扫描。用户级 skill 创建后即可被 agent 发现。

## 编写技能

技能核心是 `SKILL.md`：用 frontmatter 写元数据（名称、描述、触发条件等），正文写给 agent 的工作流提示。具体格式以仓库内已有技能与 [ADR-0015](../adr/0015-skill-framework.md) 为准。
