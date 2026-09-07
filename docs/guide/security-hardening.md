# 安全加固（部署级防线）

> 本文是 llaia 的**部署/运维层**安全指南，与进程内的命令黑名单、路径校验、[解释器内联载荷闸门](configuration.md#toolsterminal)（T3）互补。
> 背景：terminal 的字符串层防御拦不住解释器内部的文件操作（`python -c` / `node -e` 的载荷对框架不可见），T3 审批闸门只覆盖内联形态。**进程级的真正防线是无特权账户**（T2）；OS 级沙箱（T1）经评估不采纳。两者均出自 2026-09-07 的 grill 定案（docs/plan.md P7 节）。

## 威胁模型小结

| 防线 | 层级 | 覆盖 | 不覆盖 |
|---|---|---|---|
| 命令黑名单 / 路径校验 | 进程内，命令行字符串 | 直接路径越界、危险命令 | 解释器载荷、引号内内容 |
| T3 内联载荷审批 | 进程内，审批闸门 | `python -c` / `node -e` / `curl \| bash` 等内联执行 | `python script.py`（跑文件）、delegate 通道 |
| **T2 无特权账户** | **OS 权限** | workspace 外的一切写/删（内核强制 `EACCES`） | 读敏感文件（需配合 ACL / HOME 隔离） |
| T1 OS 沙箱 | OS 隔离 | 一切未知载荷 | ——（经评估**不采纳**，见下） |

## T2 · 无特权账户运行（推荐）

**原理**：整个 llaia 进程（含它 fork 出的解释器子进程）以专用低权限账户运行，该账户对 workspace 外的目录无写权限——脚本再聪明也绕不过文件系统权限，破坏面被压缩到「该账户可写的范围」。

### Windows

1. 创建专用本地账户（不加入 Administrators）：

   ```powershell
   net user llaia-agent <强密码> /add
   ```

2. 对 llaia 的工作目录授予完全控制，对其余用户目录保持默认拒绝：

   ```powershell
   icacls "C:\Users\llaia-agent" /grant "llaia-agent:(OI)(CI)F"
   icacls "D:\llaia-workspaces" /grant "llaia-agent:(OI)(CI)F"
   ```

3. 以该账户运行 llaia（服务化可用 `sc.exe create` + `nsi` 服务包装，或任务计划程序「不管用户是否登录都运行」）。
4. 敏感目录（其它账户的 Documents / Desktop / SSH 目录等）确认默认 ACL 未对该账户开放。

**挡住**：workspace 外的写/删直接 `EACCES`。**挡不住**：读——敏感文件的 ACL 需单独收紧（尤其是 SSH 私钥、浏览器凭据、云盘同步目录）。

### Linux

```bash
sudo useradd -m -s /usr/sbin/nologin llaia-agent   # 专用账户，无 sudo
sudo -u llaia-agent llaia serve                     # 以该身份运行
```

配合 `chmod 750 /home/<你的用户>` 阻止其读取你的家目录；workspace 放在专用目录（如 `/srv/llaia`）并 chown。

## T1 · OS 级沙箱：评估结论（不采纳）

2026-09-07 评估：把 terminal 及子进程关进 jail（Windows Sandbox / AppContainer；Linux bubblewrap + landlock/seccomp；或容器化）是唯一能覆盖未知载荷的根治方案，但需要给 terminal 加运行时依赖或强约束运行环境，与 llaia「轻量、可移植、单 crate」的产品定位正面冲突。

**结论：不做**，本节留作评估记录。若未来真实发生 T2 + T3 组合拦不住的安全事故，再重启评估（候选：容器化运行 llaia serve，零代码改动、纯部署选择）。

## 纵深组合建议

1. 权限档位保持 `default`（workspace 内自动放行、外审批），T3 闸门保持默认 `approval`；
2. 日常以专用低权限账户跑 `serve`（T2）；
3. 长任务离机跑时注意 delegate 通道不受审批约束（见下）——不在 delegate 任务里放未审查的执行类指令。

> **已知边界（2026-09-07 定案接受）**：delegate 子 agent 不走审批（P2-a 既有性质），主 agent 可借 delegate 绕过 T3；误删场景无自动备份兜底（T4 归档不做）。这些边界的代价已在立项时明确权衡。
