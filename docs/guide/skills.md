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

## 编写技能

技能核心是 `SKILL.md`：用 frontmatter 写元数据（名称、描述、触发条件等），正文写给 agent 的工作流提示。具体格式以仓库内已有技能与 [ADR-0015](../adr/0015-skill-framework.md) 为准。
