# 安全加固（部署级防线）

> 本文是 llaia 的**部署/运维层**安全指南，与进程内的命令黑名单、路径校验、[解释器内联载荷闸门](configuration.md#toolsterminal)（T3）互补。
> 背景：terminal 的字符串层防御拦不住解释器内部的文件操作（`python -c` / `node -e` 的载荷对框架不可见），T3 审批闸门只覆盖内联形态。**进程级的真正防线是无特权账户**（T2）；OS 级沙箱（T1）经评估不采纳。两者均出自 2026-09-07 的 grill 定案（docs/plan.md P7 节）。
> 2026-10-08 起 T2 节按 ADR-0033 Phase 3 扩为完整部署手册，并新增 `llaia doctor` 自检项。

## 威胁模型小结

| 防线 | 层级 | 覆盖 | 不覆盖 |
|---|---|---|---|
| 命令黑名单 / 路径校验 | 进程内，命令行字符串 | 直接路径越界、危险命令 | 解释器载荷、引号内内容 |
| T3 内联载荷审批 | 进程内，审批闸门 | `python -c` / `node -e` / `curl \| bash` 等内联执行 | `python script.py`（跑文件）、delegate 通道。**非安全边界**（ADR-0033 Phase 4 复审定性）：无意识操作的强制人审点 + 审计信号，T2 部署后可摘（见纵深组合建议） |
| **T2 无特权账户** | **OS 权限** | workspace 外的一切写/删（内核强制 `EACCES`） | 读敏感文件（需配合 ACL / HOME 隔离） |
| T1 OS 沙箱 | OS 隔离 | 一切未知载荷 | ——（经评估**不采纳**，见下） |

## T2 · 无特权账户运行（推荐）

**原理**：整个 llaia 进程（含它 fork 出的解释器子进程）以专用低权限账户运行，该账户对 workspace 外的目录无写权限——脚本再聪明也绕不过文件系统权限，破坏面被压缩到「该账户可写的范围」。降权做在**部署层**（进程整体）而非 per-command 包装：包装层（`runas /trustlevel` 等）会断 terminal 的 piped-stdin 通路，且 Basic User 受限 token 对用户自身 profile 的读写仍然放行——进程级专用账户才是 ADR-0033 L2 定案的落地形态。

### Windows · 专用账户部署手册

**第 1 步 · 创建专用账户**（不加入 Administrators，密码走强口令）：

```powershell
net user llaia-agent <强密码> /add
wmic useraccount where "name='llaia-agent'" set PasswordExpires=false   # 可选：服务账户免密到期
```

**第 2 步 · ACL 授予**——只对 llaia 需要的目录授予完全控制，其余保持默认拒绝：

```powershell
# config_dir（含 workspace/、sessions.db、snapshots/ 等全部家当）
icacls "C:\Users\llaia-agent" /grant "llaia-agent:(OI)(CI)F"
# workspace_root 若被 /move 指到别处，对那一处同样授权
icacls "D:\llaia-workspaces"  /grant "llaia-agent:(OI)(CI)F"
```

敏感目录（其它账户的 Documents / Desktop / `.ssh` / 浏览器凭据 / 云盘同步目录）确认默认 ACL 未对该账户开放——**T2 挡写不挡读**，可读面的收缩要靠 ACL 单独收紧。

**第 3 步 · 服务化运行**（任务计划程序「不管用户是否登录都运行」，或 `sc.exe`）：

```powershell
schtasks /create /tn "llaia-serve" /sc onlogon /ru llaia-agent /rp <强密码> ^
  /tr "E:\path\to\llaia.exe serve --config C:\Users\llaia-agent\.llaia"
# 或服务方式：
sc.exe create llaia binPath= "E:\path\to\llaia.exe serve" obj= .\llaia-agent password= <强密码> start= auto
```

注意：专用账户首次登录前需至少完成一次交互登录（或 `net user llaia-agent /logonpasswordchg:no`），否则「作为服务登录」权限缺失会导致任务启动失败（事件查看器 7038/7041）。

**第 4 步 · 验证**：

```powershell
# 任务/服务运行后，确认进程身份与完整性级别（应看到 Medium Mandatory Level）
whoami                          # → machine-name\llaia-agent
whoami /groups | findstr /i "mandatory"
```

### Linux · systemd 加固部署手册

```bash
# 第 1 步：专用系统账户（无 shell、无 sudo）
sudo useradd -r -m -d /var/lib/llaia -s /usr/sbin/nologin llaia-agent

# 第 2 步：config_dir 归属该账户
sudo chown -R llaia-agent:llaia-agent /var/lib/llaia
```

systemd unit（`/etc/systemd/system/llaia.service`）——除 `User=` 降权外，叠加内核层加固指令：

```ini
[Unit]
Description=llaia personal assistant
After=network-online.target

[Service]
User=llaia-agent
Group=llaia-agent
ExecStart=/usr/local/bin/llaia serve --config /var/lib/llaia
Restart=on-failure

# 内核层加固（systemd 内建，零成本叠加）
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/llaia
PrivateTmp=yes

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload && sudo systemctl enable --now llaia
```

**验证**：`systemctl status llaia` 显示 `User=llaia-agent`；`ps -o user= -C llaia` 确认进程身份。`ProtectSystem=strict` 使全盘只读、仅 `ReadWritePaths` 可写，比单纯账户降权更进一层。

### 通用验证清单

| 检查 | Windows | Linux |
|---|---|---|
| 进程身份是专用账户 | `whoami` → `llaia-agent` | `ps -o user= -C llaia` |
| 非提权运行 | `whoami /groups` 含 Medium Mandatory Level | `id -u` ≠ 0 |
| workspace 外写入被拒 | 试试对 `C:\Users\<你自己>\Desktop` 写文件 → 拒绝访问 | 对 `/home/<你>` 写 → Permission denied |
| 敏感文件不可读 | `.ssh` / 浏览器凭据目录 ACL 不含 llaia-agent | `chmod 750 /home/<你>` |
| doctor 自检通过 | `llaia doctor` → `security.privilege: not elevated` | 同左 |

### `llaia doctor` 自检

`llaia doctor`（CLI）与 WebUI Diagnostics 页（`/api/doctor`）自带 T2 自检项 `security.privilege`：

- **not elevated**（Medium integrity / 非 root）→ ok。已满足 T2 的底线形态；完整形态（专用账户）见本手册。
- **elevated**（High/System integrity 或 root）→ **warn**。日常以管理员/root 跑 serve 是最差实践——agent 失误与被注入指令的破坏面等于全机。
- **unknown**（探测失败）→ ok 带 unknown 标注，不阻断其余诊断。

### 已知边界（2026-09-07 定案接受）

- T2 挡写不挡读：低权账户读不到的才是真安全，敏感文件 ACL 需单独收紧（上表第 4 行）。
- delegate 子 agent 不走审批（P2-a 既有性质）：T2 落地后 delegate 的执行同样受 OS 权限约束——这是 L2 对 delegate 通道的兜底价值所在。
- 误删场景无自动备份兜底（T4 归档不做）；P9 Phase 1 的 workspace 快照基建已提供 A0/A2 资产的恢复通路。

## T1 · OS 级沙箱：评估结论（不采纳）

2026-09-07 评估：把 terminal 及子进程关进 jail（Windows Sandbox / AppContainer；Linux bubblewrap + landlock/seccomp；或容器化）是唯一能覆盖未知载荷的根治方案，但需要给 terminal 加运行时依赖或强约束运行环境，与 llaia「轻量、可移植、单 crate」的产品定位正面冲突。

**结论：不做**，本节留作评估记录。若未来真实发生 T2 + T3 组合拦不住的安全事故，再重启评估（候选：容器化运行 llaia serve，零代码改动、纯部署选择）。

## 纵深组合建议

1. 权限档位保持 `default`（workspace 内自动放行、外审批），T3 闸门保持默认 `approval`；
2. 日常以专用低权限账户跑 `serve`（T2，见上方部署手册），`llaia doctor` 确认 `security.privilege` 非 elevated；
3. **T2 部署完成后摘除 T3**（ADR-0033 Phase 4 复审定案）：OS 权限接管「界外内联写」这最后一格后，T3 的审批摩擦失去兜底依据——`config.toml` 设 `[tools.terminal] interpret_inline = "off"`，闸门关闭（cron 豁免键随之不需要）。T3 届时只剩审计价值，audit.log 留痕不中断；
4. 长任务离机跑时注意 delegate 通道不受审批约束（见下）——不在 delegate 任务里放未审查的执行类指令。

> **已知边界（2026-09-07 定案接受）**：delegate 子 agent 不走审批（P2-a 既有性质），主 agent 可借 delegate 绕过 T3；误删场景无自动备份兜底（T4 归档不做）。这些边界的代价已在立项时明确权衡。
