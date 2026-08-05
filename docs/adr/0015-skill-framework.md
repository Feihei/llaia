# ADR-0015：Skill 技能框架

**日期**：2026-08-04
**状态**：Accepted
**阶段**：P3-e（依赖 P3-d MCP client 完成）

## 背景

P3-d 引入 MCP client 后，LLAIA 能消费外部工具。但用户仍需一种方式封装"提示词 + 工具推荐 + 触发条件"的技能包，让 agent 快速获得特定领域能力（代码审查、新闻摘要、todoist 提醒等），无需改代码。

## 决策

### 1. Skill 格式：SKILL.md（markdown + YAML frontmatter）

对齐 OpenAI Codex CLI / Anthropic Claude Skills / AstrBot 的业界标准格式，不使用 `skill.toml`。

```markdown
---
name: code-review
description: 审查 Git 仓库的代码变更，给出标准化 review 意见。当用户请求代码审查时使用。
duration: turn  # turn / session，默认 turn
tools: ["file_read", "terminal", "filesystem__git_log"]  # 可选，prompt 提示推荐工具
---

# Code Review Skill

## 工作流程
1. 用 terminal 跑 `git diff` 看变更
2. ...

## 输出格式
...
```

**frontmatter 字段**：
- `name`：skill 唯一标识（正则 `^[\w.-]+$`）
- `description`：做什么 + 何时用（注入 system prompt，必须精炼）
- `duration`：`turn`（仅当前 turn 注入，默认）/ `session`（整个会话注入直到关闭）
- `tools`：可选，prompt 提示 LLM 推荐用的工具列表（**不实际控制工具挂载**，见 §4）

**body**：给 LLM 的详细指令（markdown），LLM 触发 skill 时 file_read 读取。

理由：
- markdown 比 toml 更适合写 prompt 指令（支持标题、列表、代码块）
- frontmatter 描述元数据，与 body 分离
- 与 OpenAI Codex CLI / Anthropic Claude Skills / AstrBot 生态接轨，未来可复用社区 skill 包

### 2. Progressive Disclosure（渐进式披露）

借鉴 AstrBot 的核心设计哲学：**启动时只注入轻量元数据，详细指令按需读取**。

- 启动时扫描 `~/.llaia/skills/*/SKILL.md`
- 解析 frontmatter 拿 `name` + `description`（不读 body）
- 在 system prompt 追加"## Skills"段，列出所有 active skill 的 name + description + SKILL.md 路径
- 规则提示 LLM："用 skill 前必须先 file_read 它的 SKILL.md"

**System prompt 注入示例**（借鉴 AstrBot `build_skills_prompt`）：
```
## Skills

You have specialized skills — reusable instruction bundles stored in SKILL.md files.

### Available skills
- **code-review**: 审查 Git 仓库的代码变更，给出标准化 review 意见。
  File: `~/.llaia/skills/code-review/SKILL.md`
- **news-digest**: 摘要当日新闻。当用户问"今天有什么新闻"时使用。
  File: `~/.llaia/skills/news-digest/SKILL.md`

### Skill rules
1. Discovery — 上面的列表是当前会话可用的完整 skill 清单
2. When to trigger — 用户显式提到 skill 名，或任务明确匹配 skill 的 description 时使用
3. Mandatory grounding — 执行 skill 前必须先 file_read 它的 SKILL.md（用绝对路径）
4. Progressive disclosure — 只读 SKILL.md 直接引用的文件，不要深度追引用
5. Failure handling — skill 无法应用时清楚说明问题，继续用最佳替代方案
```

好处：
- 100 个 skill 也只占 system prompt 几 KB（name + description）
- 不需要专门的 skill 触发机制（注入 prompt / 挂载工具）
- LLM 自主判断，符合 agent 范式

### 3. 触发机制：agent 判断为主

放弃之前 grill 的"关键词/agent 判断/显式调用混合"方案，简化为：

- **主触发**：agent 判断（LLM 看 name + description 自行决定）
- **显式触发**：用户说"用 code-review skill"也能触发（LLM 自然语言理解，无特殊语法）
- **不做关键词匹配**：误触发率高，且 progressive disclosure 已足够

理由：
- AstrBot 验证过此模式，体验良好
- 关键词匹配需在 channel 层或 agent 入口层做字符串扫描，增加复杂度
- LLM 自主判断符合 agent 范式，无需硬编码触发逻辑

### 4. 工具挂载：方案 C（skill 的 tools 不控制挂载）

**skill 的 `tools` 字段只是 prompt 提示**，告诉 LLM "这个 skill 推荐用这些工具"，不实际控制工具挂载。

- **内置工具**（file_read / file_write / file_edit / terminal / web_fetch / tavily_search / memory_write / send_image / send_file）：始终全挂载，无成本
- **MCP 工具**：按 server 挂载（P3-d 已设计），与 skill 解耦
- **skill 的 `tools` 字段**：SKILL.md body 里提示 LLM "本 skill 推荐用 file_read + terminal + filesystem__git_log"，LLM 读 SKILL.md 后知道用哪些工具

理由：
- 避免 50 个 skill 各声明 3 个工具导致 150 个工具全挂载（浪费 tool slot）
- 避免 `activate_skill` 系统调用（增加复杂度）
- 内置工具数量固定，MCP 工具按 server 挂载，skill 只是指令层

### 5. active 开关

`~/.llaia/skills.json` 控制 每个 skill 是否激活（借鉴 AstrBot）：

```json
{
  "skills": {
    "code-review": { "active": true },
    "news-digest": { "active": false },
    "todoist": { "active": true }
  }
}
```

- 新增 skill（`~/.llaia/skills/<name>/SKILL.md` 存在但 skills.json 无记录）：默认 active = true，自动写入 skills.json
- 用户可通过 WebUI 或手动编辑 skills.json 禁用 skill

### 6. 路径安全

借鉴 AstrBot 的 `_SAFE_PATH_RE` + `_CONTROL_CHARS_RE`：skill name / path 注入到 system prompt 时过滤危险字符，防 prompt injection。

- skill name 校验：正则 `^[\w.-]+$`，不合法的名字不注入 prompt
- path 注入时过滤控制字符 + 反引号 + 危险 unicode
- description 注入时过滤控制字符 + 反引号

### 7. 目录结构

```
~/.llaia/
  skills/                             # Skill 技能包根目录
    code-review/
      SKILL.md                        # skill 定义
      assets/                         # 可选，skill 引用的资源文件
      scripts/                        # 可选，skill 引用的脚本
    news-digest/
      SKILL.md
    todoist/
      SKILL.md
  skills.json                         # active 开关
```

### 8. WebUI 管理

配置面板加 Skill tab：
- 列出所有 skill（name + description + active 开关）
- 可视化编辑 SKILL.md（CodeMirror markdown 编辑器 + frontmatter 表单）
- 新建/删除 skill
- 测试 skill（触发一次 agent 对话验证）

### 9. 内置示例 Skill

P3-e 随附 3 个示例 skill，放在 `~/.llaia/skills/` 下：
- `code-review`：代码审查
- `news-digest`：新闻摘要（依赖 tavily_search）
- `todoist`：提醒（依赖 cron 或 web_fetch）

## 不做

- **Skill bundle**（zeroclaw 的分组机制）：单用户私人助理场景不需要
- **Skill install/audit/test 工具链**（zeroclaw 的 git/registry 安装 + 安全审计 + 测试）：P3-e 先不做，用户手动创建或复制 skill 目录
- **skillforge**（zeroclaw 的自动发现 GitHub skill）：不做
- **Sandbox skill**（AstrBot 的远程沙箱 skill）：不做，单用户场景用本地 skill 即可
- **Plugin skill**（AstrBot 的插件提供 skill）：不做，LLAIA 无插件系统
- **关键词匹配触发**：误触发率高，progressive disclosure 已足够
- **`activate_skill` 系统工具**：工具挂载走方案 C，不需要系统调用

## 影响

### 新增依赖

- `yaml-rust2` 或类似 YAML 解析 crate（解析 frontmatter；P1 未引入 YAML，需新增）
- 其余复用现有依赖（serde_json / tokio / axum 等）

### 配置文件

- 新增 `~/.llaia/skills.json`（可选，不存在时所有 skill 默认 active）
- `llaia init` 不生成 skills/（按需创建）

### 代码变更

- 新增 `src/skill/mod.rs`：模块入口 + `SkillManifest` 结构
- 新增 `src/skill/loader.rs`：扫描 `~/.llaia/skills/*/SKILL.md`，解析 frontmatter，构建 skill 清单
- 新增 `src/skill/prompt.rs`：`build_skills_prompt(skills: &[SkillManifest]) -> String`，生成 system prompt 的 "## Skills" 段（借鉴 AstrBot `build_skills_prompt`）
- `src/agent/mod.rs`：`Agent::system_prompt` 末尾追加 skills 段
- `src/agent/runner.rs`：LLM 调 `file_read ~/.llaia/skills/<name>/SKILL.md` 时，路径校验走 P3-a 的路径防御（SKILL.md 路径在 `~/.llaia/` 根，agent workspace 外，需特殊放行规则）
- `src/web/mod.rs`：加 `/api/skills` 路由（GET 列表 / POST 创建 / PUT 更新 / DELETE 删除 / GET/PUT `/api/skills/<name>/content` 读写 SKILL.md）
- `src/web/static/app.js`：加 Skill tab UI

### 路径校验特殊规则

SKILL.md 路径在 `~/.llaia/skills/<name>/SKILL.md`，属于 `~/.llaia/` 根目录（agent workspace 之外）。file_read 工具需特殊放行：
- file_read 的 path 参数 canonicalize 后 `starts_with(~/.llaia/skills/)` 且文件名以 `SKILL.md` 结尾 → 放行
- 其他 `~/.llaia/` 根目录路径 → 拒绝（保护 config.toml / mcp.toml / cron.toml / skills.json）

## 与 P3-a 的依赖

- SKILL.md 的 file_read 走特殊放行规则（见上"路径校验特殊规则"）
- skill 的 `tools` 字段推荐的工具调用受 agent workspace 边界约束
- skill 不引入额外 confirm_mode 规则

## 与 P3-d 的依赖

- skill 的 `tools` 字段可推荐 MCP 工具（如 `filesystem__git_log`）
- MCP 工具挂载与 skill 解耦（skill 只是 prompt 提示，不控制挂载）

## 参考

- [OpenAI Codex CLI Skills](https://github.com/openai/codex)
- [Anthropic Claude Skills](https://docs.anthropic.com/en/docs/agents-and-tools/claude-code/skills)
- AstrBot `core/skills/skill_manager.py`：
  - `build_skills_prompt`：progressive disclosure 的 system prompt 生成
  - `_SAFE_PATH_RE` / `_CONTROL_CHARS_RE`：路径注入安全过滤
  - `_SKILL_NAME_RE`：skill name 正则校验
  - `skills.json`：active 开关管理
  - SKILL.md + YAML frontmatter 格式
- zeroclaw `src/skills/`：skill bundle / install / audit / test 工具链（LLAIA 不借鉴，过重）
- grilling 第十轮（P3-e 细化）：
  - Q1 格式改为 SKILL.md（markdown + frontmatter，对齐业界标准）
  - Q2 触发简化为 agent 判断（放弃关键词匹配 + 显式调用特殊语法）
  - Q3 Progressive Disclosure（system prompt 只注 name+description，LLM 用时自己 file_read 完整 SKILL.md）
  - Q4 工具挂载方案 C（skill 的 tools 只是 prompt 提示，不实际控制挂载）
