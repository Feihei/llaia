# 命令策略引擎（execpolicy）：三值裁决 + 声明式规则文件

状态：**plan 待拍板，未实施**。行号以 2026-09-13 12:35 的工作区为准；实现前需等用户当前未提交改动（`path_guard.rs` / `approval.rs` / `slash.rs` / `trusted_store.rs` 等）落库后重新对线号。
日期：2026-09-13
关联：ADR-0020（/ok 审批豁免）、plan.md T3（解释器内联闸门）、trusted_dirs 持久化（2026-09-13，`src/trusted_store.rs`）、`.ref/codex` 架构调研（2026-09-13）

## 背景与问题

terminal 命令的策略判定目前散在**三套互不相识的机制**里，语义各缺一块：

1. **硬编码字符串黑名单**：`COMMAND_BLACKLIST`（`src/path_guard.rs:560-573`，12 条）+ `hits_command_blacklist`（`:576-579`）——整行 `lower.contains(bl)` 的**子串匹配**。两头都漏：`echo "please sudo install"` 误报；`rm  -rf  /`（双空格）、`/usr/bin/sudo`（带路径）漏报。**不可配**，用户想放行一条被误杀的常规命令只能改代码——这是 MEMORY 里记了很久的老怨念。
2. **首 token 白名单**：`command_policy=whitelist` 模式只拿 `split_whitespace().next()` 与 `command_whitelist` 精确比对（`src/tools/terminal.rs:47-65`）。和黑名单是两套形态，互斥三选一（`blacklist`/`whitelist`/`none`）。
3. **审批层按路径范围裁决**：`approval_decision`（`src/agent/approval.rs:250-305`）只看 permission profile + 路径是否落在 workspace∪受信（`:273-278`）+ T3 内联闸门（`:282-288`），**完全不认识命令内容**。`git` 与 `curl | bash` 在 workspace 内的审批待遇一样（都免审，除后者被 T3 拦）。

另有**两个悬空死键**：`[tools.terminal].confirm` 与 `.whitelist`（`src/config.rs:727-731`）全仓库零消费者（仅 `config.rs:1330/1384` 测试自引用），`Terminal` 结构体根本不带它们（`src/tools/terminal.rs:13-25`，接线只有 `command_policy`/`command_whitelist`，`src/channels/cli.rs:512-513`）。属"没有用例的 config key"，应一并清除。

codex 用同一个收口解决这一族问题：**声明式前缀规则 + 三值裁决**。规则引擎只回答 Allow / Prompt / Forbidden，审批档位（AskForApproval）回答"Prompt 能不能问到人"，两层正交。本 plan 移植其**语义与形态**，不搬其实现依赖。

## codex 机制速览（调研结论，file:line 相对 `.ref/codex/codex-rs/`）

- DSL：Starlark 语法 `prefix_rule(pattern=[...], decision?, justification?, match?, not_match?)`，`decision` 默认 `allow`，值域 `allow|prompt|forbidden`；pattern 元素可为数组表**备选**（`execpolicy/README.md:7-24`）。
- `match`/`not_match` 是**加载时校验的示例命令**——规则文件自带单测，写错规则当场失败（README `:21`）。
- 多规则同时命中取**最严**：`forbidden > prompt > allow`（README `:95`），因此文件间、层间无优先级设计负担。
- 装载：各配置层的 `rules/` 目录合并 `.rules` 文件；用户批准的命令以 allow 前缀**追加**进 `default.rules`（amendment，`core/src/exec_policy.rs:54-56,464-499`）。
- 护栏：shell/解释器/`rm`/`sudo` 等前缀在 `BANNED_PREFIX_SUGGESTIONS`（`core/src/exec_policy.rs:57-146`），**永不作为持久放行建议**提出。
- 裁决→审批的映射（`core/src/exec_policy.rs:394-461`）：`Forbidden`→带理由拒绝；`Prompt`→人审，但档位不允许（如 `Never`）时**自动拒绝而非放行**（`:216-238`）；`Allow`→跳过审批，且规则显式 allow 时连沙箱也豁免。
- 调试通路：独立子命令 `codex execpolicy check --rules f.rules <cmd...>` 打印裁决 JSON（README `:48-68`）。

## 目标

1. 命令维度策略**收口成一套**：可声明、可持久、可诊断的三值裁决引擎，替换上面机制 1、2 与两个死键。
2. 审批提示从"只认路径范围"升级为"也认命令是什么"，同时内置 allow 规则给常见安全命令**降审批噪音**。
3. 与既有权限档位 / trusted_dirs / T3 内联闸门语义正交，不互相削弱。

## 非目标（调研中评估后明确否决）

- **不引入 Starlark**：codex 用完整 `starlark` crate 做规则求值器（`execpolicy/Cargo.toml` 依赖表），对 LLAIA 是一个解释器级依赖换 20 行的 pattern 匹配语义，不成比例。
- **不引入 tree-sitter-bash**：codex 的真实 AST 解析（`shell-command/src/bash.rs:3-19`）服务其多前端产品；LLAIA 是单人助理，已有引号感知切分器（`split_command_segments` `path_guard.rs:627`、`tokenize_command` `:242`、`normalize_prog` `:613`），升级为结构化匹配够用。且本机 Rust 工具链内存吃紧（clippy 需 `-j 1`），新增 C 语法库的构建风险不值当。
- **不做 OS 沙箱**（Seatbelt/Landlock/Windows token）、**不做 hooks 生命周期**、**不做 submission/event 协议重构**——均见调研报告，代价与威胁模型不匹配。
- 不碰**路径维度**：`dangerous_prefixes`、`validate_path*`、trusted_dirs 保持原样。命令策略管"这个程序允不允许跑"，路径守卫管"引用了哪些文件"，两维继续正交。

## 设计

### D1 裁决引擎：`src/execpolicy.rs` 新模块

```rust
pub enum Decision { Allow, Prompt, Forbidden }   // 严重度 Forbidden > Prompt > Allow

pub struct PrefixRule {
    pattern: Vec<Token>,        // Token = 单值 | 备选组
    decision: Decision,         // 缺省 Allow
    justification: Option<String>,
}

pub struct ExecPolicy { rules: Vec<SourceRule> }  // SourceRule 带来源文件标注
impl ExecPolicy {
    pub fn check(&self, command: &str) -> Evaluation;  // 收集全部命中规则，取最严
}
```

匹配算法（关键：**替换掉子串匹配**）：

1. `split_command_segments` 切段（现有，复用）；**每段都要过**，全段最严者为命令裁决。
2. 段内 token 化用 `tokenize_command`（现有，复用；去引号），程序名过 `normalize_prog`（现有，复用：剥路径前缀与 `.exe/.cmd/.bat`，故 `/usr/bin/sudo`、`C:\Windows\System32\sudo.exe` 与 `sudo` 同判——修掉现黑名单的漏报面）。
3. 段首变量赋值前缀跳过（`is_env_assignment`，现有复用）。
4. pattern 与段 token 序列做**前缀匹配**（大小写不敏感，Windows 惯例），元素为备选组时任一相等即中。

误报对照：`echo "please sudo install"` 段首是 echo → 不再命中（今天 `contains("sudo")` 误杀）；`rm -rf  /`（双空格）今天 `contains("rm -rf /")` 漏 → token 前缀 `["rm","-rf","/"]` 需容忍多空格已由 tokenize 解决；子串匹配彻底退役。

T3 关系：内联闸门继续独立存在（heredoc/管道/裸解释器是**结构形态**判定，前缀规则表达不了）。规则与 T3 叠加取严——命令被规则判 Allow 也照被 T3 拉去人审，两机制正交不重复实现 flag 形态。

### D2 规则来源与文件格式

三层，全部装载、合并取最严（无优先级设计，同 codex）：

| 层 | 位置 | 形态 |
|---|---|---|
| 内置默认 | `src/rules/default.rules.toml`，`include_str!` 编译进二进制 | forbidden（现 `COMMAND_BLACKLIST` 12 条的 token 级转写）+ 保守 allow 集（只读查询类：`ls/cat/head/tail/grep/pwd/which`、`git` 的只读子命令 `status/log/diff/show/branch`、`cargo --version` 这类无副作用形态） |
| 用户规则 | `<config_dir>/rules/*.toml`（可多文件） | 用户手写，allow/prompt/forbidden 任意 |
| 持久放行 | `<config_dir>/rules/amendments.toml` | **机器管理**，`/allow` 追加（见 D5），无保注释需求 |

TOML 形态（贴 LLAIA 全家桶现有栈，serde 直读；`Token` 用 `untagged` 的 `String | Vec<String>`）：

```toml
[[rule]]
pattern = ["git", ["commit", "push"]]
decision = "prompt"
justification = "改动要推出去前确认一次"
match = ["git push origin main", "git commit -m x"]     # 加载时校验，规则自带用例
not_match = ["git status"]
```

`match`/`not_match` 加载时求值，**不中即该文件判无效**——规则文件不可静默坏掉。用户规则文件解析/校验失败的行为是待拍板项（Q5）：推荐 warn + 跳过该文件继续启动（对齐 `load_config_for_doctor` 的"绝不在最需要它的时刻哑火"哲学 + trusted_store 写失败降级的先例），由 doctor 显式报出；内置默认规则的用例失败则是**单测期就必挂**的编译不变式。

内置 allow 集刻意保守：allow 在 D4 语义下会豁免到"路径守卫也放行"的程度，所以只收无写副作用或写入面极窄的查询命令，其余留给用户 amendment 自己长出来。

### D3 装载与共享

`ExecPolicy` 在 serve/chat 启动时构建一次，`Arc<ExecPolicy>` 注入 `Agent` 与 `Terminal` 工具（与 trusted_dirs 同一批共享 Arc，`src/agent/mod.rs:1422` 附近构造点）。**不做热加载**（低频显式：改规则文件后重启/`restartService()` 生效；与 channel 不热启动是同一既有性质）。若 P1 落地 amendment，写入方（slash.rs）同时更新内存态 Arc，重启后从文件重建。

### D4 与审批流、权限档的接线（核心变更）

`approval_decision`（`approval.rs:250`）内 terminal 分支升级为：

```
先：policy.check(command) → 最严裁决（附命中规则与来源）
Forbidden → ApprovalAction::Denied{reason: justification 或默认文案}   // 任何档位都不问，含 yolo（Q3）
Prompt    → required = true（现有 NeedsApproval 通路，非交互频道照旧自动拒绝）
Allow     → required = false 且执行层路径校验豁免（下方②），但 T3 与 dangerous_prefixes 硬地板不豁免
无命中    → 现有语义原样（default 档按 within 判定 / read-only 全审）
```

两处必须钉死的语义：

1. **`AskForApproval::Never` 映射**（codex `core/src/exec_policy.rs:216-238` 的教训）：Prompt 撞上不可问的频道（cron/delegate 之外的非交互 IM 等）→ **拒绝并说明**，绝不降级为放行。LLAIA 现通路已如此（`approval.rs:296-304`），规则接入后不得改道。
2. **Allow × 路径守卫的相交**（待拍板 Q2，影响 `Terminal::check_path_safety`，`terminal.rs:68`）：
   - **选项 A（推荐，对齐 codex）**：规则显式 Allow → 审批豁免 + 执行层 `validate_command_paths_in_scope` 对该命令豁免——语义是"用户为该前缀整体背书"（`git -C /outside` 可用）。硬地板仍在：`dangerous_prefixes` 路径黑名单、T3、`check_shell_wrappers` 不受任何规则影响。
   - 选项 B（保守）：Allow 只免审批、路径守卫照挡。缺点：审批层放行了、执行层再 Err，用户看到的是"批了也跑不了"，且拒绝原因绕出审批通路之外，可见性差。

   推荐 A 的根据：codex 正是 "explicit rule allow → `Skip{bypass_sandbox}`"（`exec_policy.rs:440-454`，且要求**每一段**都被显式 allow 才豁免）；LLAIA 对应实现同样逐段检查，`cd ws && git -C /other` 这种混合段不因一段命中就整体豁免。

**yolo 与 delegate**：yolo 保持"显式弃权审批"但**禁不掉 Forbidden**（Q3 推荐：forbidden 对 yolo 仍生效——现 `COMMAND_BLACKLIST` 在执行层本来就是档位无关硬挡，`terminal.rs:225`，语义等价迁移、无回归）；delegate 频道现状全放行（`approval.rs:263-265`），推荐同样让 Forbidden 生效（Q4：子 agent 拿到的正是主 agent 背书过的命令面，挡灾难指令无损耗）。

### D5 amendment：`/allow <id>` 持久放行（P1）

`/ok <id>` 语义不变（本次批准）。新增 `/allow <id>`：批准本次 **且** 把该命令的段首程序前缀（经 `normalize_prog` 归一，取首 token）追加为 allow 规则写进 `amendments.toml`——即 trusted_dirs 在命令维度的同款交互（批准一次 → 该前缀后续免审），概念复用、文案复用。

护栏（对齐 `BANNED_PREFIX_SUGGESTIONS`，codex `exec_policy.rs:57-146`）：派生前缀命中内置表（shell：bash/sh/zsh/fish/cmd/powershell 等；解释器：`INLINE_INTERPRETERS` 全集；`rm/sudo/su/dd/mkfs/shutdown/reboot`）→ **拒绝持久化**、仅允许一次性 `/ok`，回复说明原因。此表是"永不可背书"清单，与"可被规则改判"的普通 forbidden 区分开。

幂等：同前缀已存在则不重复追加（现 trusted_store 幂等 upsert 同款处理）。

### D6 诊断面

- `llaia policy check <command...>` 子命令（`src/main.rs` clap + `commands/`）：打印 `{decision, matched_rules:[{matched_prefix, decision, justification, source}], segments:[...]}` JSON——规则写错了当场验证，对齐 codex `execpolicy check`。
- `llaia doctor` 两层都加项：文件层报各规则文件解析结果与用例校验失败明细；运行层报内置规则版本 + 用户规则计数。沿用"doctor 纯只读、永不 Err 退出"契约。
- 审批提示升级：`format_approval_prompt`（`approval.rs:349`）追加命中规则的 `justification`（"policy: {why}"行），用户批的时候知道**为什么被问**。

## 退役与兼容

一次性删净（Q1 推荐）：`command_policy`、`command_whitelist`、`confirm`、`whitelist` 四个键从 `TerminalToolConfig`（`config.rs:726-742`）移除；`Config::load` 对残留 `command_policy`/`command_whitelist` 键 warn 一行"已由命令规则取代"+ 忽略（`[channels.*].confirm_mode` 的 whitelist 废弃迁移有同款先例可循）；`commands/mod.rs:152` CONFIG_TEMPLATE 与 `docs/guide/configuration.md`、`docs/guide/security-hardening.md` 相应段落改写。单人产品，不搞双轨兼容期。

## 实施分期与文件清单

**P0（引擎 + 接线 + 退役 + 诊断）**

- 新 `src/execpolicy.rs`（类型/装载/匹配，目标 <500 行）+ 新 `src/rules/default.rules.toml`
- `src/path_guard.rs`：删 `COMMAND_BLACKLIST`/`hits_command_blacklist`（T3 三常量与切分器保留，供 D1/D5 复用）
- `src/agent/approval.rs`：`approval_decision` 接 `Evaluation`；`ApprovalContext` 加 Arc
- `src/tools/terminal.rs`：`check_command_policy` 删除；`check_path_safety` 按 D4-A 接 allow 豁免；构造点 `src/channels/cli.rs:510-515` 传 Arc
- `src/config.rs`：四键退役 + load warn
- `src/agent/mod.rs`：策略构建与注入（`:1422` 区）；`src/lib.rs` 挂模块
- `src/main.rs` + `src/commands/mod.rs`：`policy check` 子命令；doctor 检查项；CONFIG_TEMPLATE
- 测试：引擎单测（备选组/逐段最严/归一化命中/用例校验拒载）、`approval_decision` 矩阵扩展（含 yolo 遇 Forbidden、非交互遇 Prompt）、MSYS 路径程序名、平台门控 `#[cfg(target_os)]`
- 文档：AGENTS.md 工具集与安全段、guide（configuration/security-hardening）、glossary 新词条（裁决三值/前缀规则/amendment）；CHANGELOG 先查再记

**P1**：`/allow` + amendments.toml + banned 护栏 + 幂等；WebUI Config 页规则只读清单（列出生效规则与来源，编辑仍走文件）。
**P2（可选，倾向不做）**：审批提示的 per-segment 人话摘要（codex `ParsedCommand::{Read,ListFiles,Search}` 思路，`shell-command/src/parse_command.rs:54`）；当前 220 字符截断（`approval.rs:362`）够用再说。

> 规模估计：P0 净增约 500-700 行（含测试），删除约 80 行；P1 约 200 行。

## 验收标准

1. `echo "sudo rm -rf / is scary"` 全程免审可跑（今天必被黑名单杀）。
2. `sudo /usr/bin/id`、带路径形态 `C:\...\sudo.exe` 在 default 档即使命令含 workspace 内路径也被人审（今天 `contains("sudo ")` 对无尾空格变体漏判）。
3. 内置 allow 生效后，workspace 外只读命令（如 `cat /etc/hosts`、`git status` 在受信仓库但 workspace 外）仍走审批但规则 allow 的少数枚举项免审——且 `dangerous_prefixes` 路径（`C:\Windows\...`）在 allow 规则下仍被拒。
4. `llaia policy check 'python -c "print(1)"'` 输出显示 allow 规则命中 + `inline_gate=prompt` 叠加取严（T3 不回退）。
5. 规则文件用例校验失败 → 该文件被跳过 + 启动 warn + doctor 红项；`policy check` 与运行期裁决同一函数出同一结果（CLI 复用装载代码路径，非第二实现）。
6. `cargo fmt --all -- --check && cargo clippy --all-targets -j 1 -- -D warnings && cargo test -j 1` 全绿。
7. 文档四处同步完成（CONFIG_TEMPLATE / configuration.md / AGENTS.md / security-hardening.md），glossary 有条目。

## 风险与已接受边界

- allow × 路径豁免（D4-A）把"免审面"从路径维度扩到命令维度，误写宽 allow 规则（如 `pattern=["python"]`）等于给该程序全域背书——缓解：内置保守集 + amendments 的 banned 护栏 + doctor 列出生效规则 + `policy check` 自验。
- yolo/delegate 语义微调（Forbidden 仍挡）是对既有文档承诺的改动，需在 ADR 写明。
- `is_inline_interpreter_command` 已知边界（组合 flag、`env` 包装）原样保留，本 plan 不扩大解释器检测面。
- delegate 频道审批旁支问题（MEMORY 遗留）不在本单修。

## 待拍板问题（编号，附推荐）

| # | 问题 | 推荐 | 备选 |
|---|---|---|---|
| Q1 | 四键退役一次性删净 vs 保留 `command_policy` 双轨一版 | **删净**（单人产品，whitelist 语义本就无人用） | 双轨过渡 |
| Q2 | Allow 规则是否豁免执行层路径校验 | **豁免（对齐 codex，逐段判定）** | 仅免审批 |
| Q3 | Forbidden 对 yolo 是否生效 | **生效**（等价迁移现黑名单的档位无关性） | yolo 全绕 |
| Q4 | Forbidden 对 delegate 频道是否生效 | **生效**（只挡灾难项，无行为损耗） | 维持全绕 |
| Q5 | 用户规则文件坏掉 | **warn + 跳过 + doctor 报**（不砖启动） | 拒绝启动 |
| Q6 | 持久放行命令形态 | **新 `/allow <id>`**（/ok 语义纯粹不动） | `/ok <id> always` 参数化 |
| Q7 | amendment 前缀粒度 | **首 token（程序名级）** | 两 token（git/cargo 类带子命令，规则可更窄但派生启发式复杂） |
| Q8 | `policy check` 进 P0？ | **进**（是引擎的验证面，成本低） | 推后 |

## 参考（调研取证）

codex（`.ref/codex/codex-rs/`）：`execpolicy/README.md:7-24,40-44,48-68,95`；`execpolicy/src/decision.rs:9-16`；`core/src/exec_policy.rs:54-56,57-146,216-238,356-461,464-499`；`protocol/src/protocol.rs:992-1014,1078-1113`；`shell-command/src/bash.rs:3-19`、`parse_command.rs:54`；`execpolicy/Cargo.toml`（starlark 依赖）。
LLAIA：`src/path_guard.rs:242,560-579,598-734`；`src/agent/approval.rs:191,250-305,349`；`src/tools/terminal.rs:13-65,225`；`src/config.rs:726-742`；`src/channels/cli.rs:510-515`；`src/agent/mod.rs:1422`；`src/commands/mod.rs:152`。
