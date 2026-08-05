# ADR-0011: Agent 能力边界 — 按 agent 隔离的 workspace + 命令拦截

- 状态：Proposed
- 日期：2026-08-04
- 关联：[ADR-0002](0002-agent-architecture.md)、[ADR-0009](0009-qq-channel.md)、[ADR-0006](0006-tools-and-cli.md)、[docs/plan.md P3-a](../plan.md)

## 背景

### 现状问题

P1.5 引入 QQ channel 时，因 QQ 下无法弹 stdin 等用户确认，设计了 `confirm_mode` 三档（`always` / `whitelist` / `none`），让 QQ channel 默认拒绝所有 `requires_confirm() == true` 的工具。结果：

- QQ 只能聊天 + 调只读工具（`file_read` / `web_fetch` / `tavily_search`）
- 终端命令在 QQ 下完全不可用
- 用户必须切到 CLI 才能完成"让 agent 干活"的任务

更深层问题：**channel 不应该决定工具权限**。channel 只是 I/O 入口（用户从哪进来），agent 才是执行主体（谁在调工具）。同一 agent 跨 channel 应该有一致的能力边界。

### 现有 workspace 模型

P2-a 子 Agent 委派引入了"子 agent 独立 workspace"概念，但实现散乱：

- 主 agent workspace = `~/.llaia/`（config.toml / SOUL.md / USER.md / MEMORY.md / sessions.db 全混在一起）
- 子 agent workspace = 配置中显式指定（`[agent.<alias>].workspace`），通常是 `~/.llaia/subagents/<alias>/`

问题：

1. 主 agent 的工具能直接读写 `~/.llaia/config.toml`（敏感信息泄露风险）
2. 主 agent workspace 与配置目录混在一起，无清晰边界
3. 子 agent workspace 路径不统一，主 agent 难以统一访问子 agent 产出

### 同类项目参考

- **AstrBot**：每 UMO（会话）独立 workspace（`data/workspaces/{normalized_umo}/`）+ 危险命令黑名单 + 管理员/普通用户区分 + 可选沙箱
- **ZeroClaw**：deny-by-default + 路径白名单 + 命令白名单（具体到 `/usr/bin/curl`）+ OS 沙箱（Landlock / Bubblewrap / Seatbelt / Docker）+ 链式审计回执

## 决策

采用 **按 agent 隔离的 workspace + 命令拦截** 模型。channel 不再决定工具权限，工具权限由 agent 的 workspace 边界 + 命令策略决定。

### 1. 新目录结构

```
~/.llaia/                              # 根目录（敏感配置 + 进程状态）
  config.toml                         # 主配置（含 provider api_key / qq app_secret / web token）
  cron.toml                           # cron 任务定义（P3-c 引入）
  mcp.toml                            # MCP server 配置（P3-d 引入）
  llaia.pid                           # PID 文件
  logs/                               # 日志目录（tracing + audit.log）

  workspace/                          # 主 agent 工作区（主 agent 工具能访问的"根"）
    SOUL.md                           # 主 agent 人格
    USER.md                           # 用户画像
    MEMORY.md                         # 长期记忆
    sessions.db                       # 主 agent 会话历史
    uploads/                          # 用户上传媒体
    ...主 agent 工作文件...            # agent 创建的笔记、代码、临时文件等

    subagent/                         # 子 agent 工作区集合
      <agent-name>/                   # 如 coder / searcher / writer
        SOUL.md                       # 子 agent 人格
        USER.md                       # 子 agent 用户画像（通常继承主 agent）
        MEMORY.md                     # 子 agent 记忆
        sessions.db                   # 子 agent 会话历史（独立）
        ...子 agent 工作文件...
```

**关键规则**：

- `~/.llaia/` 根目录只放配置和敏感信息，**agent 工具不可访问**（file/terminal 都不能读写）
- `~/.llaia/workspace/` 是主 agent 的"挂载根"，主 agent 工具只能在此目录内操作
- `~/.llaia/workspace/subagent/<name>/` 是子 agent 的"挂载根"，子 agent 工具只能在此目录内操作
- **主 agent 可读 `subagent/` 子目录**（层级权限：主 agent 能整合子 agent 产出）
- 子 agent **不可访问** `subagent/` 之外的目录（不能访问主 agent 的文件，也不能访问兄弟子 agent）

类似容器的挂载目录：每个 agent 看到的"文件系统根"是自己 workspace，工具的相对路径都解析到自己 workspace 内。

### 2.terminal 工具的 cwd

`terminal` 工具执行命令时，cwd 固定为**当前 agent 的 workspace 根**：

- 主 agent 调 terminal → cwd = `~/.llaia/workspace/`
- 子 agent `coder` 调 terminal → cwd = `~/.llaia/workspace/subagent/coder/`

命令中的相对路径自然解析到 workspace 内。绝对路径 / `..` 逃逸由路径校验拦截（沿用 `resolve_within` 实现）。

### 3. terminal 工具的拦截策略

terminal 工具有两个**正交**的拦截维度：**命令策略**（哪些命令允许执行）和**路径防御**（命令能访问哪些路径）。两者叠加生效，所有 agent（主 + 子）统一适用，不区分 channel。

#### 3.1 命令策略（command_policy）

`[tools.terminal].command_policy` 三档：

- `blacklist`（默认）：内置命令黑名单拦截，其余放行
- `whitelist`：仅白名单内命令放行
- `none`：全放行（CLI 交互场景默认，向后兼容）

**命令黑名单**（内置，不可配）：`rm -rf /` / `rm -rf ~` / `sudo` / `su` / `shutdown` / `reboot` / `kill -9 1` / `dd if=` / `mkfs` / `:(){:|:&};:` / `> /dev/sda` / `chmod -R 777 /` / `curl|wget ... | sh`（管道执行远程脚本）

**命令白名单**（用户配置）：`[tools.terminal].command_whitelist = ["ls", "cat", "grep", "git", "cargo"]`

#### 3.2 路径防御（三层深度防御）

cwd 固定为当前 agent workspace 根（见 §2），相对路径自然解析到 workspace 内。但 shell 命令行内仍可能含绝对路径 / `..` 逃逸 / `cd /` 等，单纯固定 cwd 不够。采用三层防御挡住 LLM 误操作（**不防恶意用户**，用户即 owner；防的是模型失控误删 `~/Documents` 这类）：

**第一层 — shell 包装拒绝**：用 shell 词法解析器拆 token，命中以下模式一律拒绝：

- 首 token 为 `bash` / `sh` / `zsh` / `fish` 且参数含 `-c`
- 命令行含 `eval` / `exec` / `source` / `$()` / 反引号 / `>(...)` `<(...)`（进程替换）

堵住任意代码执行逃逸——`bash -c "rm -rf /"` 这类无法被后续路径检查解析。

**第二层 — 路径白名单（主防御）**：对所有"看起来像路径"的 token（`/` 或 `~` 开头、Windows 盘符开头、含路径分隔符），canonicalize 后必须 `starts_with` 当前 agent workspace 根：

- 主 agent：路径必须落在 `~/.llaia/workspace/` 内（含 `subagent/` 子目录）
- 子 agent `<name>`：路径必须落在 `~/.llaia/workspace/subagent/<name>/` 内

canonicalize 失败（路径不存在）时回溯父目录直到存在的祖先，检查祖先 `starts_with` workspace。

**第三层 — 路径黑名单（兜底）**：canonicalize 失败或启发式漏判时，字符串前缀匹配危险目录：

| 平台 | 危险路径 |
|---|---|
| Linux | `/root` `/usr` `/bin` `/sbin` `/etc` `/var` `/boot` `/proc` `/sys` `/dev` `/lib` `/lib64` |
| macOS | `/System` `/Library` `/usr` `/private` `/bin` `/sbin` `/etc` `/var` `/dev` |
| Windows | `C:\Windows` `C:\Program Files` `C:\Program Files (x86)` `C:\ProgramData` `C:\System Volume Information` |

命中黑名单前缀的路径一律拒绝（即使后续 canonicalize 想放行也拒）。

#### 3.3 与 file 工具的关系

file 工具的路径参数是结构化的（单一 `path` 字段），直接走第二层（canonicalize `starts_with` workspace）+ 第三层黑名单兜底，不需要第一层 shell 解析。详见 §5。

### 4. confirm_mode 语义简化

按 agent 隔离后，channel 不再决定工具权限。`confirm_mode` 角色弱化，重定义为**全局开关**：

- `none`（新默认）：不弹确认，所有 agent 工具受 workspace 边界 + 命令策略约束即可
- `always`：所有有副作用工具调用前弹确认（CLI 弹 stdin y/n，QQ/Web 拒绝并提示"该操作需在 CLI 确认"）
- `session`：首次确认后 N 分钟内放行同类工具（CLI 弹一次，QQ/Web 拒绝并提示）

`whitelist` 模式废弃，加载时 warn + fallback 到 `none`。

**默认从 `always` 改为 `none`** 的理由：workspace 边界 + 命令黑名单已经提供了基本安全保障，单用户私人助理场景不需要每次确认。用户觉得不安全可改 `always`。

### 5. file 工具路径策略

所有 agent 的 `file_read` / `file_write` / `file_edit` 路径校验走 §3.2 的第二层（canonicalize `starts_with` workspace）+ 第三层（路径黑名单兜底），不需要第一层 shell 词法解析：

- 相对路径：解析到当前 agent workspace 根
- 绝对路径：canonicalize 后必须 `starts_with` workspace，否则拒绝（不再一刀切拒绝绝对路径，落在 workspace 内的绝对路径放行）
- `..` 逃逸：canonicalize 后必落 workspace 内，自动拦截
- 路径黑名单兜底：命中 §3.2 第三层危险路径前缀的拒绝

主 agent 特殊层级权限：

- `file_read` 可读 `subagent/` 子目录（整合子 agent 产出）
- `file_write` / `file_edit` 不可写 `subagent/`（避免主 agent 污染子 agent workspace），唯一例外见 §7 的 `.inbox/`

### 7. 跨 workspace 协作

按 agent 隔离后，子 agent 看不到主 agent workspace 的文件。三个协作机制：

#### 7.1 主 → 子：delegate 工具的 file_paths 参数（复制到 .inbox/）

`delegate` 工具加 `file_paths: Vec<String>` 参数（可选）。委派时系统层把主 agent workspace 内的指定文件复制到子 agent workspace 的 `.inbox/` 目录：

- 主 agent 调 `delegate(agent_name="coder", task="...", file_paths=["notes.txt", "uploads/photo.jpg"])`
- 系统在 `subagent/coder/.inbox/` 下创建对应文件（保留原文件名，同名加数字后缀）
- 子 agent 收到的 task 文本末尾由系统追加一行：`[输入文件已放在 .inbox/: notes.txt, uploads/photo.jpg]`
- 子 agent 用相对路径 `.inbox/notes.txt` 读取
- `.inbox/` 是子 agent workspace 内的普通目录，受同样的路径边界约束

`uploads/` 不搞特殊权限——主 agent 委派时如需让子 agent 看图，就在 `file_paths` 里带 `uploads/xxx.jpg`，系统照样复制到 `.inbox/`。

`.inbox/` 生命周期：不自动清空，子 agent 可自行 `file_edit` / 删除。建议下次委派前系统清空 `.inbox/`（避免上次残留污染本次任务）——具体策略为：**delegate 调用时先清空 `.inbox/` 再复制新文件**，保证每次委派输入干净。

#### 7.2 子 → 主：delegate 返回值含产出文件清单

delegate 工具的返回值结构：

```json
{
  "text": "<子 agent 最终文本回复>",
  "output_files": ["result.md", "summary.txt"]
}
```

`output_files` 由系统层收集：从子 agent 本次 turn 的工具调用记录里，提取所有 `file_write` / `file_edit` 的 `path` 参数，去重后得到产出文件相对路径列表（相对子 agent workspace 根）。

主 agent 整合产出时：
- 文本直接用
- 需要文件内容时 `file_read subagent/<name>/<path>`（§5 已允许主 agent 读 `subagent/`）
- 路径形式：`subagent/coder/result.md`（主 agent workspace 视角的相对路径）

#### 7.3 USER.md 启动时同步覆盖

USER.md 是用户身份和偏好的权威来源，应单点真值。子 agent 的 USER.md：

- **启动时**：从主 agent workspace 的 USER.md 复制覆盖到 `subagent/<name>/USER.md`
- **运行时**：子 agent 的 `memory_write` 工具拒绝写 USER.md（身份绑定统一在主 agent 管理）；主 agent 的 `memory_write` 可写主 agent 的 USER.md
- **SOUL.md**：各 agent 独立，无同步

这样主 agent 更新偏好后，下次启动子 agent 自动同步；同一进程内若主 agent 改了 USER.md，已加载的子 agent 不会热更新（可接受，子 agent 通常短生命周期）。

### 8. 危险动作审计

新增 `~/.llaia/logs/audit.log`，记录所有有副作用工具的调用：

```
2026-08-04T10:23:45+08:00 agent=main channel=qq tool=terminal args={"cmd":"ls -la"} result=ok exit=0
2026-08-04T10:24:01+08:00 agent=main channel=qq tool=file_write args={"path":"notes.txt"} result=ok bytes=1234
2026-08-04T10:24:30+08:00 agent=coder channel=delegate tool=terminal args={"cmd":"rm -rf /"} result=blocked reason=blacklist
```

- 记录字段：timestamp / agent / channel / tool / args / result /（失败时）reason
- 仅记录有副作用工具（`requires_confirm == true`）
- 文本追加，不做链式哈希（单用户场景过重）

## 不做

- **OS 沙箱**：不引入 Landlock / Bubblewrap / Seatbelt / Docker。单用户私人助理场景，workspace 边界 + 命令黑名单足够；未来加多用户支持再考虑
- **按 channel 隔离 workspace**：channel 只是 I/O 入口，不应决定工具权限。同一 agent 跨 channel 应共享 workspace
- **按 UMO（会话）隔离**：LLAIA 单用户，QQ 单聊只有一个对端（owner 自己），按会话隔离无意义；未来加群聊时再考虑按群隔离
- **凭据加密存储**：保持 config.toml 明文（已用环境变量插值 `${VAR}` 解耦），不引入 keyring
- **链式审计哈希**：单用户场景审计日志主要供事后排查，不防篡改
- **MCP 工具单独策略**：MCP 工具（P3-d 引入）默认 `requires_confirm = true`，受同一套 workspace 边界 + confirm_mode 约束，不单独开策略

## 影响

### 目录迁移（Breaking Change）

现有 `~/.llaia/` 下的 SOUL.md / USER.md / MEMORY.md / sessions.db 需迁移到 `~/.llaia/workspace/`：

- `llaia` 启动时检测旧结构，自动迁移：
  1. 创建 `~/.llaia/workspace/`
  2. 移动 SOUL.md / USER.md / MEMORY.md / sessions.db / uploads/ 到 `workspace/`
  3. 移动 `~/.llaia/subagents/<name>/` 到 `~/.llaia/workspace/subagent/<name>/`（如果存在）
  4. 备份原 config.toml 到 `config.toml.bak`，更新 `[agent.main].workspace` / `[agent.<alias>].workspace` 路径
  5. 写 `~/.llaia/.migrated_v0.2` 标记，避免重复迁移

- 子 agent workspace 路径变化：
  - 旧：`[agent.<alias>].workspace = "~/.llaia/subagents/<alias>"`（或用户自定义）
  - 新：固定为 `~/.llaia/workspace/subagent/<alias>/`，配置字段废弃（自动推导）

### 配置 schema 变更

```toml
[tools.terminal]
confirm = "none"                    # CLI 默认（向后兼容）
command_policy = "blacklist"        # 新增：blacklist / whitelist / none
command_whitelist = []              # 新增：仅 policy=whitelist 时生效

[channels.qq]
confirm_mode = "none"               # 全局开关（不再 per-channel）：none / always / session
# whitelist 字段废弃

[agent.main]
# workspace 字段废弃（自动为 ~/.llaia/workspace/）
# soul / user / memory 字段废弃（自动为 workspace 下的 SOUL.md / USER.md / MEMORY.md）

[agent.<alias>]
# workspace 字段废弃（自动为 ~/.llaia/workspace/subagent/<alias>/）
# soul / user / memory 字段废弃（自动推导）
# denied_tools 保留
```

### 代码变更

- `src/config.rs`：
  - `TerminalConfig` 加 `command_policy` / `command_whitelist`
  - `QqConfig.confirm_mode` 枚举重定义（`none` / `always` / `session`，废弃 `whitelist`）
  - `AgentConfig.workspace` / `soul` / `user` / `memory` 字段标记 deprecated，加载时自动推导
- `src/tools/terminal.rs`：
  - cwd 固定为当前 agent workspace 根
  - 实现命令策略（blacklist/whitelist/none）
  - 实现三层路径防御：shell 包装拒绝（词法解析）+ 路径白名单（canonicalize `starts_with` workspace）+ 路径黑名单（跨平台危险目录前缀）
- `src/tools/file.rs`：
  - 路径校验改为 agent-aware（canonicalize `starts_with` 当前 agent workspace + 黑名单兜底）
  - 主 agent `file_read` 放宽到 `subagent/`（只读），`file_write` / `file_edit` 拒绝 `subagent/`（`.inbox/` 例外由系统层处理，不经 file 工具）
- `src/tools/delegate.rs`：
  - `DelegateTool::execute` 加 `file_paths` 参数处理：清空子 agent `.inbox/` → 复制主 agent workspace 内指定文件到 `.inbox/`
  - task 文本末尾追加 `[输入文件已放在 .inbox/: ...]` 提示
  - 返回值改为 JSON `{text, output_files}`：从子 agent 本次 turn 工具调用记录提取 `file_write`/`file_edit` 的 `path` 参数去重
- `src/tools/memory.rs`：
  - 子 agent 调 `memory_write` 写 USER.md 时拒绝（返回错误提示"身份绑定统一在主 agent 管理"）
- 新增 `src/path_guard.rs`：路径防御共享逻辑（canonicalize 回溯、跨平台危险路径黑名单常量、shell 词法解析），供 terminal + file 工具复用
- `src/agent/mod.rs`：
  - `Agent` 加 `workspace_root: PathBuf` 字段（区分 `~/.llaia/` 与 agent workspace）
  - `Agent` 加 `is_main: bool` 字段（决定是否能读 `subagent/`）
  - 子 agent 初始化时从主 agent workspace 复制 USER.md 覆盖到自己的 workspace
- `src/agent/runner.rs`：
  - `execute_tool_calls` 不再按 channel 拦截，按 `confirm_mode` 全局开关 + agent workspace 边界
  - 实现 `session` 模式的授权 token 缓存
  - 记录本次 turn 的工具调用历史（供 delegate 提取产出文件清单）
- 新增 `src/audit.rs`：审计日志写入
- 新增 `src/migrate.rs`：v0.1 → v0.2 目录结构迁移逻辑
- `src/channels/qq.rs`：移除 per-channel confirm 逻辑，改为读全局 `confirm_mode`

### 现有行为兼容

- CLI channel 默认 `command_policy = none`，行为不变
- 现有 `confirm_mode = "whitelist"` 配置加载时 warn + fallback 到 `none`
- 现有 `[tools.terminal].confirm` 字段保留，CLI 下生效
- 现有 `[agent.<alias>].workspace` 字段加载时 warn，自动迁移到新路径
- 启动时自动迁移旧目录结构（写 `.migrated_v0.2` 标记）

### 与其他 P3 子阶段的依赖

- **P3-b init**：`llaia init` 直接生成新目录结构（无需迁移）
- **P3-c cron**：cron agent 模式触发主 agent 时，主 agent 在 `~/.llaia/workspace/` 工作
- **P3-d MCP**：MCP 工具默认 `requires_confirm = true`，受同一套 workspace 边界 + confirm_mode 约束
- **P3-e Skill**：Skill 文件可放在 `~/.llaia/workspace/.skills/` 或 `~/.llaia/skills/`（待定，倾向后者避免污染 workspace）

## 参考

- [AstrBot 电脑能力文档](https://docs.astrbot.app/use/computer.html) — workspace 隔离 + 危险命令黑名单
- ZeroClaw 安全模型：deny-by-default + 路径白名单 + 命令白名单 + OS 沙箱 + 链式审计
- grilling 第四轮：用户明确要求按 agent 隔离（主 agent 在 `~/.llaia/workspace/`，子 agent 在 `~/.llaia/workspace/subagent/<name>/`，主 agent 可读子 agent，config 在 `~/.llaia/` 根，agent 权限限定在自己工作区，终端命令在 workspace 内执行，类似容器挂载目录）
- grilling 第五轮：用户要求 terminal 命令也限定在工作区内不能访问别的目录。确认不引入 OS 沙箱（Windows 无轻量方案），采用三层路径防御（shell 包装拒绝 + 路径白名单 + 路径黑名单兜底），借鉴 zeroclaw 路径黑名单思路但以白名单为主、黑名单为兜底。防 LLM 误操作不防恶意用户。
- grilling 第六轮：决策跨 workspace 协作三个机制——主→子用 delegate 的 `file_paths` 参数复制到子 agent `.inbox/`（每次委派先清空再复制）；子→主用 delegate 返回值 `{text, output_files}`（output_files 从子 agent 工具调用记录提取）；USER.md 启动时从主 agent 同步覆盖到子 agent（子 agent memory_write 拒写 USER.md），SOUL.md 各自独立。
