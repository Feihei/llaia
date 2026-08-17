# ADR-0006: 工具集、终端安全与 CLI 形态

- 状态：Accepted
- 日期：2026-07-21

## 背景

需要确定 P1 主 Agent 挂载哪些工具、终端命令的安全策略、CLI 入口形态和斜杠命令清单。

## 决策

### P1 工具集（最小集）

| 工具 | 用途 | 备注 |
|---|---|---|
| `file_read` | 读文件 | |
| `file_write` | 写文件 | |
| `file_edit` | 改文件（精确替换） | |
| `terminal` | 跑终端命令 | 含 ls/grep 等子命令，不单列 |
| `web_fetch` | 获取网页 | |
| `search` | 搜索 | 需配置对应 provider 的 api_key |
| `todo` | 规划后执行：每会话待办清单（add/list/update/done） | 自动注入 Runtime Context（ADR-0024）|
| `memory_read` | 读 MEMORY.md | 内部实现，不暴露给 LLM |
| `memory_write` | 写 MEMORY.md | 暴露给 LLM 用于自动记忆 |
| `session_*` | 会话 sqlite 读写 | 内部实现，不暴露给 LLM |

不单列 ls/grep——让 LLM 走 `terminal` 工具调系统命令。
不单列 glob/search——P1 用终端命令凑，P2 视需要再加。

### 终端命令安全

配置项控制，默认 whitelist 免确认：

```toml
[tools.terminal]
confirm = "whitelist"    # none / whitelist / always
whitelist = ["ls", "cat", "grep", "pwd", "dir"]
```

- `none`：全部直接执行
- `whitelist`：白名单内免确认，其他每次 y/n
- `always`：全部需确认

### CLI 入口形态

子命令式（参考 zeroclaw）：

- `llaia chat` —— 进入交互式 REPL
- `llaia config` —— 打印当前配置
- `llaia doctor` —— 诊断 provider 连通性、文件完整性
- `llaia remember "<text>"` —— 一次性写 MEMORY.md

默认 `llaia`（无子命令）等价于 `llaia chat`。

### 斜杠命令清单（REPL 内）

| 命令 | 用途 |
|---|---|
| `/new` | 新会话 |
| `/exit` | 退出 |
| `/compact` | 手动压缩上下文 |
| `/clear` | 清空当前内存上下文（sqlite 留底） |
| `/remember <text>` | 手动写 MEMORY.md |
| `/config` | 查看当前配置 |
| `/help` | 帮助 |

不做 `/undo`、`/show`、`/model`（P2 视需要）。

### 自动环境发现

P1 不做（扫 PATH、检测 python/nodejs/rust 版本、生成工具描述——工程量大）。
P1 直接把工具写死。P2 再做。

## 影响

- 工具 trait 设计要支持 P2 的白名单过滤，但 P1 不过滤
- REPL 解析层要区分斜杠命令与普通输入
- `llaia remember` 子命令是 `memory_write` 工具的 CLI 快捷方式

## 参考

- grilling 第三轮 Q12.2、第四轮 Q23、第五轮 Q28–Q29、Q31–Q32
