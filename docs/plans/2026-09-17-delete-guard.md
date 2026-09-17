# Delete Guard：workspace 内删除命令转 .trash（rm → 回收站）

- 日期：2026-09-17
- 状态：已实现（grill 三问：(a) 免审批 ✅ (b) 只管 bound path ✅ (c) /trash 命令后置）
- 门禁：fmt / clippy -D warnings / cargo test 全绿（2026-09-17）
- 上游背景：2026-09-17 实测 `rm .trash-guard-test.txt`（workspace 内）无审批硬删（audit.log 09:12），暴露 default 档审批空洞。

## 问题

default 档的三层闸门（作用域审批 / T3 内联载荷 / 灾难命令黑名单）均不覆盖
**workspace 内的破坏性命令**：裸 `rm <workspace内路径>` 自动放行且真删，
SOUL/USER/MEMORY/sessions.db 等家目录命根子无防护。

## 定案语义（三档）

| 删除目标位置 | 行为 |
| --- | --- |
| 当前 bound path（workspace_root）内 | **转 `.trash/`，免审批**（可恢复所以不打扰） |
| bound path 外、受信目录内 | **强制审批**（不沿用受信=免审；审批后真删） |
| bound path 外、非受信 | 强制审批（现状不变） |

- 受信目录**不**获得 trash 保护——trash 只属于当前 bound path（用户定案 b）。
- 审批后（/ok）执行真删，trash 不再接管（人已批准）。
- yolo 档显式弃权，全部直通（与 T3 同语义）；delegate 通道不受审批拦截（现状）。

## 拦截设计

- **拦截点**：`approval_decision`（审批判定）+ `Terminal::run`（执行层，非 approved 路径）。
  纯 Rust 解析，不注入 shell wrapper；cron/delegate/全频道统一。
- **识别**：`split_command_segments` 引号感知切段，段首词（`normalize_prog` 归一化）命中
  `DESTRUCTIVE_COMMANDS`：`rm` / `unlink` / `rmdir`（POSIX）、`del` / `erase` / `rd`（cmd）、
  `Remove-Item`（PowerShell）。
- **目标提取**：段内引号感知 tokenize，跳过 `-` 旗标与 `/x` 短开关（cmd 形态）、`--`；
  `-path` / `-literalpath` 的值算目标。
- **界内判定**：复用 `validate_path(workspace, target)`——Ok 即 bound path 内（与执行层
  同一套语义）；含 `$` 未展开变量的 token 由 validate_path 词法路径自然判外。
- **已知边界（刻意接受，注释留档）**：
  - 只识别顶层段；`xargs rm`、`find -delete`、反引号/$() 内的 rm 不识别
    （后者本就被 `check_shell_wrappers` 拒绝；前者属对抗性构造，单用户威胁模型不设防）；
  - `mv` 覆盖、`>` 截断、`tee`、`git clean` 等间接破坏不在本期范围；
  - heredoc 正文不逐段分析（与 T3 同口径）。

## trash 落盘

- 目录：`<workspace>/.trash/<UTC时间戳-毫秒>/<原相对路径>`；同盘 rename，毫秒级。
- manifest：`.trash/manifest.jsonl` 追加一行 `{"ts","channel","targets","trash_dir"}`。
- glob（`rm *.log`）：无新依赖，自实现 `*`/`?` 组件匹配展开；展开为空按 rm `-f`
  语义静默跳过（无 `-f` 则报错，与 shell 行为一致）。
- 拒绝移动：workspace 根自身、`.trash` 自身。
- 失败不降级真删：rename 失败（占用/权限）→ 整条命令报错返回（已移动部分留档 manifest）。
- 复合命令（`echo hi && rm x`）：trash 接管 rm 目标后**仍执行原命令**——非破坏段不受影响，
  rm 段对已移走文件自然 no-op（`-f` 静默 / 无 `-f` 报 No such file，诚实可见）。
- 工具输出前缀：`[delete-guard] moved N path(s) to .trash/<ts>/ ...`，agent 明确知道文件没死。

## 配置

`[tools.terminal] delete_guard = "trash" | "off"`（默认 `"trash"`；off = 旧行为，
受信目录内 rm 静默真删——不建议）。

## 后续迭代（不在本期）

- `/trash` 斜杠命令：list / restore <n> / empty。
- 间接破坏向量（mv 覆盖、`>` 截断）评估。

## Checkpoints

- [x] C1 plan 文档
- [x] C2 config：`delete_guard` 字段 + 模板注释
- [x] C3 path_guard：`DESTRUCTIVE_COMMANDS` / `is_destructive_command` /
      `destructive_targets` / `destructive_all_within_bound` + 单测
- [x] C4 approval：`terminal_delete_guard` 三档判定 + 单测
- [x] C5 terminal：trash 落盘执行（含 glob、manifest、拒绝清单）+ 单测
- [x] C6 接线（mod.rs / cli.rs）+ runner 测试桩补字段
- [x] C7 门禁：fmt / clippy -D warnings / cargo test（清代理）
