# ADR-0028: Skill 目录文件访问边界（file_read 放行全部配套文件）

- 状态：Accepted
- 日期：2026-08-27
- 关联：ADR-0015（skill 框架）、ADR-0027（skill 自管）；`src/skill/loader.rs`、`src/tools/file.rs`

## 背景 / Context

ADR-0015 的 progressive disclosure 假设每个 skill = **单个 `SKILL.md`**，file_read 只对它特殊放行：
- file_read 的 path canonicalize 后 `starts_with(skills_dir)` **且文件名恰为 `SKILL.md`** → 放行
- 其余 `~/.llaia/` 路径 → 拒绝

但实际使用的"工具型 skill"天然带配套文件（`comfy_img_gen` 带 `generate_image.py` / `flux2_config.json`，officecli 系带 PPT 模板等）。实测时 agent 在读完 `SKILL.md` 后去读同目录的 `generate_image.py` / `flux2_config.json`，被文件名校验拦下，被迫改走 `terminal cat` 硬读——这不仅体验差，还**让读路径绕开了 file_read 的工具级路径校验**（terminal 不受 skill 目录白名单约束，理论上可读任意文件），反而违背"防 LLM 误操作"的安全边界。

## 决策 / Decision

1. **file_read 对 `skills/<name>/` 下任意文件放行**：`resolve_skill_path` 去掉「文件名必须为 `SKILL.md`」的限制，只保留 canonicalize 落在 `skills_dir` 内这一条。安全校验链不变——词法规范化 + canonicalize 消解符号链接与 `..` 穿越，`starts_with(skills_dir)` 保证不越出。
2. **file_write / file_edit 保持不放行** skill 目录。skill 增改仍走专用 `skill_create` / `skill_edit`（frontmatter 校验 + 路径安全校验，ADR-0027），避免绕过校验直接改知识资产。
3. **terminal 对 skill 目录放行读/执行**：`validate_command_paths` 新增 `extra_readable` 参数，terminal 把 `skills_dir` 传入，命令引用 skill 目录内脚本/资产不再被判越界。写防线由 file_write / file_edit 保持拒绝兜底；terminal 内写 skill（`cd skills && > file` 技巧）在无内核沙箱下无法从 token 可靠封死，属已知边界（见「后果」）。审批：`tool_within_workspace` 仍对 terminal 引用 skill 目录判为 workspace 外 → 执行 skill 代码需人肉确认。
4. **send_media 保持 workspace 约束**：skill 产出（生成图等）先落到 workspace 再发送，不扩大 send_media 暴露面。

## 备选 / Alternatives

- **保持严格、只放行 SKILL.md**：安全但绑死工具型 skill，逼 agent 用 terminal 绕过，实际更不安全。
- **放行到 `~/.llaia/` 整个根目录**：过度——会暴露 config.toml / mcp.toml / cron.toml / skills.json，不可接受。
- **给 skill 引入独立文件工具**：过度设计，file_read 放行 + terminal 执行已覆盖真实需求。

## 后果 / Consequences

- 正向：agent 能直接 `file_read` skill 的脚本 / 配置 / 模板并运行；读路径回到受校验的 `file_read`；`terminal` 引用 skill 目录时不再误判越界（`python …/generate_image.py` 直接可跑），同时执行仍走审批。
- 负向：skill 目录内文件对 agent 可读（多读面）。鉴于 skill 是用户自甄选/自管理的知识资产，且仅读不写，风险可接受。
- 已知边界：`terminal` 放行对读与写一视同仁（命令无法从 token 区分），`cd skills && > file` 等写技巧在无内核沙箱下无法封死。写防线由 file_write / file_edit 对 skill 目录保持拒绝 + 审批兜底；单用户本地场景下可接受。

## 实现

- `src/skill/loader.rs::resolve_skill_path`：删除文件名 `=== "SKILL.md"` 判断。
- 测试：`test_resolve_skill_path_allows_skill_md`、`test_file_read_skill_md_special_allow` 更新为非 SKILL.md 配套文件放行 + 目录外穿越仍拒。
- `src/path_guard.rs::validate_command_paths`：新增 `extra_readable: Option<&Path>` 透传给 `validate_path`（默认 None 保持旧行为）。
- `src/tools/terminal.rs`：`Terminal` 新增 `skills_dir` 字段（`Terminal::new` 第 4 参），`check_path_safety` 传入 `validate_command_paths`。
- `src/channels/cli.rs`：构造 `Terminal` 时传 `Some(config_dir.join("skills"))`。
- `src/agent/approval.rs::tool_within_workspace`：terminal 分支仍传 `None`——引用 skill 目录判为 workspace 外走审批（执行需人肉确认）。
- 测试：`test_skills_dir_allowed_when_configured`（配了 skills_dir 放行目录内脚本、未配仍拒）。