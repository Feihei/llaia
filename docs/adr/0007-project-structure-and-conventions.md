# ADR-0007: 项目结构、工作区与工程约定

- 状态：Accepted
- 日期：2026-07-21

## 背景

需要确定 Rust 项目结构、工作区目录布局、错误处理风格、日志库选择等工程基线。

## 决策

### 工作区目录

默认 `~/.laia/`，用户可在配置中改到别处：

```
~/.laia/
  config.toml
  SOUL.md
  USER.md
  MEMORY.md
  sessions.db
  logs/
```

### Rust 项目结构（v1 单 crate）

```
laia/
├── Cargo.toml
├── src/
│   ├── main.rs              # CLI 入口、子命令分发
│   ├── config.rs            # toml 配置加载
│   ├── provider/
│   │   ├── mod.rs           # Provider trait
│   │   └── openai_compat.rs # OpenAI 兼容实现
│   ├── agent/
│   │   ├── mod.rs           # 主 Agent 循环
│   │   ├── context.rs       # 上下文管理、压缩
│   │   └── tool_call.rs     # 工具调用解析（原生+标签降级）
│   ├── tools/
│   │   ├── mod.rs           # Tool trait
│   │   ├── file.rs          # 文件读写
│   │   ├── terminal.rs      # 终端命令
│   │   ├── web.rs           # 网页获取
│   │   └── tavily.rs        # 搜索
│   ├── memory/
│   │   ├── mod.rs           # SOUL/USER/MEMORY 加载
│   │   └── sqlite.rs        # 会话持久化
│   ├── channels/
│   │   ├── mod.rs           # Channel trait
│   │   └── cli.rs           # CLI REPL
│   └── commands.rs          # 斜杠命令
└── docs/
    └── adr/                 # 决策记录
```

v1 单 crate，v2 视复杂度再考虑拆分（参考 zeroclaw 的多 crate 布局）。

### 错误处理

`anyhow::Result` 全局兜底，v1 简单优先。
v2 视需要对外 API 引入 `thiserror` 自定义错误类型。

### 日志库

tracing，v1 只配一个 fmt layer 输出到文件（`~/.laia/logs/`）+ stderr。
不做复杂订阅链。

### 配置文件完整 schema

```toml
[provider.default]
type = "openai_compatible"   # v1 只支持这一种
base_url = "http://localhost:11434/v1"
api_key = ""
model = "qwen2.5:7b"
native_tool_calling = true    # false 则走 <tool_call> 标签降级

[agent.main]
context_threshold = 0.7       # 压缩阈值
soul = "~/.laia/SOUL.md"
user = "~/.laia/USER.md"
memory = "~/.laia/MEMORY.md"

[channels.cli]
enabled = true

[tools.terminal]
confirm = "whitelist"         # none / whitelist / always
whitelist = ["ls", "cat", "grep", "pwd", "dir"]

[tools.tavily]
api_key = ""

[workspace]
dir = "~/.laia"

[log]
level = "info"                # debug / info / warn / error
dir = "~/.laia/logs"
```

### 命名式 section 的扩展性

采用 `[provider.<id>]` / `[agent.<alias>]` 命名式结构，v1 代码只认 `default` 和 `main`，
v2 加多 provider/多 agent 时只增 section 不改 schema。

## 影响

- v1 工程量可控，无多 crate 协调成本
- 配置 schema 为 v2 扩展留好口子
- 日志和错误处理简单，便于快速迭代

## 参考

- grilling 第五轮 Q33、第六轮 Q35–Q37
- zeroclaw 项目结构（参考但简化）
