# 子 Agent 委派模式设计 (P2-a)

> 日期：2026-07-23
> 状态：Spec
> 关联：[ADR-0002](../adr/0002-agent-architecture.md)、[Roadmap](../plan.md)

## 背景

P1 + P1.5 完成了单 Agent 架构的 LLAIA MVP（CLI + QQ channel + 流式输出）。主 Agent 单干所有任务，所有工具直接挂载。

P2-a 引入主从 Agent 委派模式：主 Agent 通过 `delegate` 工具把特定任务整体甩给专用子 Agent 独立完成，子 Agent 结果回传主 Agent 整合后再回用户。用户只跟主 Agent 接触。

ADR-0002 已确定采用**委派模式（C）**——非编排者模式、非人格切换模式。本 spec 细化实现设计。

## 目标

- 主 Agent 能通过 `delegate` 工具委派任务给子 Agent
- 子 Agent 有独立的 SOUL、session、工具集（黑名单过滤）
- 子 Agent 持久 session：多次委派会话接续
- 委派超时机制：超时保留部分结果
- 不改动现有 Channel 层（CLI/QQ 无感知）

## 非目标

- 热重载配置（未来独立特性）
- 子 Agent 并行执行（单用户场景不需要）
- 子 Agent 再委派子 Agent（物理禁止递归）
- Web 面板创建/编辑子 Agent（P2-b）

## 架构

### 整体数据流

```
启动时：
  Config → 遍历 [agent.<alias>]
    → alias="main" → 主 Agent（现有逻辑不变）
    → alias≠"main" → 子 Agent 实例（各自独立 workspace/session/SOUL/tools）
  → AgentRegistry { main: Arc<Mutex<Agent>>, sub_agents: HashMap<String, Arc<Mutex<Agent>>> }

委派流程：
  用户消息 → 主 Agent handle_input_streaming
    → 主 Agent LLM 决定调 delegate 工具（参数：agent_name + task_description）
    → delegate 工具从 AgentRegistry 取子 Agent 实例
    → 子 Agent .handle_input_streaming(task_description, "delegate", inner_tx)
       ↳ 子 Agent 独立 session，只接收 task_description（无主会话上下文）
       ↳ tokio::time::timeout 包裹执行
    → 子 Agent 最终文本回传给主 Agent（作为 delegate 工具的 tool result）
    → 主 Agent 拿到结果，继续生成最终回复给用户
```

### 关键设计决策

1. **AgentRegistry 预加载**：启动时实例化所有子 Agent，常驻内存。单用户场景子 Agent 数量少（个位数），内存不是瓶颈，持久 session 天然实现。

2. **delegate 是普通工具**：走现有 tool calling 管道（native 或标签降级均可）。LLM 自主决定何时委派、委派给谁。`delegate` 工具的 `agent_name` 参数用 `enum` 列出可用子 Agent，LLM 直接知道能委派给谁。

3. **子 Agent 不直接推 Channel**：子 Agent 调 `handle_input_streaming` 时传的是 inner mpsc，主 Agent 收集子 Agent 的所有 Chunk 拼成最终文本作为 tool result。子 Agent 的 ToolStart/ToolResult 事件在主 Agent 视角不可见。

4. **Channel 层无感知**：CLI/QQ 完全不知道委派发生。它们只看到主 Agent 的 TurnEvent 流。子 Agent 执行发生在主 Agent 的 tool execution 阶段内部。

## Config Schema 扩展

`AgentConfig` 新增两个字段：

```rust
pub struct AgentConfig {
    pub model: String,              // 现有：引用 "provider_id.model_alias"
    pub workspace: String,          // 现有：工作区目录
    pub soul: Option<String>,       // 现有：SOUL 文件路径（子 Agent 用独立 soul 文件）
    pub user: Option<String>,       // 现有
    pub memory: Option<String>,     // 现有
    // 新增 ↓
    pub denied_tools: Vec<String>,  // 工具黑名单，默认空（继承主 Agent 所有工具）
    pub delegate_timeout: u64,      // 委派超时秒数，默认 120
}
```

### TOML 示例

```toml
[agent.main]
model = "default.qwen"
workspace = "~/.llaia"
soul = "~/.llaia/SOUL.md"

[agent.coder]
model = "default.qwen"
workspace = "~/.llaia/agents/coder"
soul = "~/.llaia/agents/coder.md"
denied_tools = ["memory_write"]
delegate_timeout = 180

[agent.searcher]
model = "default.qwen"
workspace = "~/.llaia/agents/searcher"
soul = "~/.llaia/agents/searcher.md"
denied_tools = ["terminal", "file_write", "file_edit", "memory_write"]
delegate_timeout = 60
```

### 规则

- `alias = "main"` 是主 Agent，必有且只有一个
- 其它 alias 都是子 Agent，不配也不会报错（只是没有子 Agent 可委派）
- `denied_tools` 默认空 = 继承所有工具；显式列出 = 排除这些
- `delegate_timeout` 默认 120 秒
- 子 Agent 的 `workspace` 独立（各自的 sessions.db）
- 子 Agent 的 `soul` 指向各自的人格文件
- USER.md 默认共享（从主 Agent 继承），MEMORY.md 通过 `denied_tools` 控制写权限

## Trait 签名改动

### Tool::execute 加 channel 参数

现有 `Tool::execute` 签名没有 channel 信息，delegate 工具需要 channel 来透传给子 Agent 的 confirm 策略。改 Tool trait：

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;

    // 改动：加 channel: &str 参数
    async fn execute(&self, args: &Value, channel: &str) -> Result<String>;

    fn requires_confirm(&self) -> bool { false }
    // ...
}
```

现有工具（file/terminal/web/tavily/memory）的 execute 签名加 `channel: &str` 参数但忽略它（`let _ = channel;`）。只有 delegate 工具使用 channel。

`execute_tool_calls` 调 `tool.execute(args, channel)` 时把已有的 channel 传进去。

### Channel trait 改接收 AgentRegistry

现有 `Channel::run` 接收 `Arc<Mutex<Agent>>`，但 delegate 工具需要 AgentRegistry。改 Channel trait：

```rust
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    // 改动：接收 Arc<AgentRegistry> 替代 Arc<Mutex<Agent>>
    async fn run(self: Arc<Self>, registry: Arc<AgentRegistry>) -> Result<()>;
}
```

各 channel 实现从 `registry.main` 取主 Agent（`registry.main.clone()`），现有逻辑基本不变。

## AgentRegistry

```rust
pub struct AgentRegistry {
    /// 主 Agent（现有逻辑的入口）
    pub main: Arc<Mutex<Agent>>,
    /// 子 Agent：alias → 实例
    sub_agents: HashMap<String, Arc<Mutex<Agent>>>,
}

impl AgentRegistry {
    pub fn get(&self, alias: &str) -> Result<&Arc<Mutex<Agent>>>;
    pub fn available_sub_agents(&self) -> Vec<String>;
}
```

启动时在 `build_agent`（现 cli.rs 里）改造：遍历 `config.agent`，`main` 走现有逻辑，其它 alias 各自 build 一个 Agent 实例（各自 SessionStore、各自 SOUL、各自 ToolRegistry 按 denied_tools 过滤）。build 完后把 registry 的 `Arc` 注入主 Agent 的 delegate 工具。

## delegate 工具

```rust
pub struct DelegateTool {
    registry: Arc<AgentRegistry>,
    timeout_secs: u64,  // 从 config.agent.<alias>.delegate_timeout 读
}

#[async_trait]
impl Tool for DelegateTool {
    fn name(&self) -> &str { "delegate" }
    fn description(&self) -> &str {
        "委派任务给子 Agent 执行。子 Agent 有独立的专业能力和工具集。"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "agent_name": {
                    "type": "string",
                    "description": "子 Agent 名称",
                    "enum": self.registry.available_sub_agents()
                },
                "task": {
                    "type": "string",
                    "description": "要委派给子 Agent 执行的任务描述"
                }
            },
            "required": ["agent_name", "task"]
        })
    }
    fn requires_confirm(&self) -> bool { false }
    
    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let agent_name = args["agent_name"].as_str().unwrap();
        let task = args["task"].as_str().unwrap();
        let sub_agent = self.registry.get(agent_name)?;

        let (tx, mut rx) = mpsc::channel(64);
        let sub_clone = sub_agent.clone();
        let task_clone = task.to_string();

        let result = tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            async {
                sub_clone.lock().await
                    .handle_input_streaming(&task_clone, "delegate", tx).await
            }
        ).await;
        
        // 非阻塞收集子 Agent 已产生的 Chunk
        let mut output = String::new();
        while let Ok(Some(ev)) = rx.try_recv() {
            if let TurnEvent::Chunk { delta } = ev {
                output.push_str(&delta);
            }
        }
        
        match result {
            Ok(Ok(_)) => Ok(if output.is_empty() { "[子 Agent 无输出]".into() } else { output }),
            Ok(Err(e)) => Ok(format!("[子 Agent 执行错误: {}]\n部分输出: {}", e, output)),
            Err(_) => Ok(format!("[子 Agent 超时({}秒)]\n部分输出: {}", self.timeout_secs, output)),
        }
    }
}
```

### 关键点

- `delegate` 工具的 `agent_name` 参数用 `enum` 列出可用子 Agent
- 子 Agent 执行时 channel 固定为 `"delegate"`，不透传主 Agent 的 channel（避免子 Agent 被 QQ confirm_mode 拦截）
- 超时后用 `try_recv` 非阻塞收集已产生的 Chunk
- delegate 工具挂在**主 Agent** 上，子 Agent 不挂 delegate（防止递归委派）

### 子 Agent 的工具挂载

- 从主 Agent 的 ToolRegistry 复制一份
- 按 `denied_tools` 过滤掉禁止的工具
- **不加 delegate 工具**（防递归）

## 错误处理与边界情况

### 委派失败场景

1. **子 Agent 名不存在**：delegate 工具返回 `[委派失败: 未知子 Agent "xxx"]`
2. **子 Agent 执行超时**：已收集的部分文本作为结果返回 `[子 Agent 超时(120秒)]\n部分输出: ...`
3. **子 Agent 内部错误**（Provider 调用失败等）：`[子 Agent 执行错误: ...]\n部分输出: ...`

### delegate channel 的 confirm 策略

delegate 工具调子 Agent 时，channel 固定为 `"delegate"`，**不透传**主 Agent 所在 channel：
- 子 Agent 的操作是主 Agent 的内部决策延续，不是用户直接指令
- QQ channel 的 `confirm_mode=always` 是为"用户无法在 stdin 确认"设计，不适用于主 Agent 已决策的委派场景
- 子 Agent 能用的工具由 `denied_tools` 控制（更细粒度的安全边界）

**历史 bug**：早期实现透传了 channel，导致 QQ 下委派时子 Agent 调 `file_write` 等需确认工具被 `confirm_mode=always` 拦截，子 Agent 任务完不成导致超时，主 Agent 收到超时后尝试自己用 `file_write` 也被同一逻辑拦截。回归测试 `test_sub_agent_not_blocked_by_qq_confirm` 防止此 bug 再现。

### 递归委派防护

子 Agent 的 ToolRegistry 不挂 delegate 工具 → 物理上无法再委派。不需要额外的递归深度限制。

### 并发安全与死锁

- 主 Agent 持有 `Arc<Mutex<Agent>>`，委派时主 Agent 锁住自己在执行 delegate 工具
- delegate 工具内部锁子 Agent（不同的 Mutex 实例，不死锁）
- 主 Agent 锁住时 QQ channel 新消息排队等锁（与现有"主 Agent 执行任何工具期间锁住"行为一致，非委派特有）

### 委派期间的用户体验限制

同步委派下，委派期间（子 Agent 执行到完成或超时）：
- 主 Agent 的 Mutex 被持有，无法处理新消息
- 用户不能打断委派、不能跟主 Agent 继续对话
- 只能等子 Agent 完成或超时（`delegate_timeout`，默认 120 秒）

这是 P2-a 的已知限制。用户主动中止生成能力（P2-c）实现后可缓解——用户可中止当前委派，主 Agent 释放锁。

## 文件结构

| 文件 | 责任 | 动作 |
|---|---|---|
| `src/config.rs` | `AgentConfig` 加 `denied_tools` / `delegate_timeout` | 修改 |
| `src/agent/registry.rs` | `AgentRegistry` 结构 | 新建 |
| `src/agent/mod.rs` | 导出 registry 模块 | 修改 |
| `src/tools/mod.rs` | `Tool::execute` 加 `channel` 参数；导出 delegate 模块 | 修改 |
| `src/tools/delegate.rs` | `DelegateTool` 实现 | 新建 |
| `src/tools/file.rs` `terminal.rs` `web.rs` `tavily.rs` `memory.rs` | execute 签名加 `channel` 参数（忽略） | 修改 |
| `src/agent/runner.rs` | `execute_tool_calls` 调 `tool.execute(args, channel)` | 修改 |
| `src/channels/mod.rs` | `Channel::run` 签名改接收 `Arc<AgentRegistry>` | 修改 |
| `src/channels/cli.rs` | `build_agent` 改造为构建 AgentRegistry；run 从 registry.main 取 Agent | 修改 |
| `src/channels/qq.rs` | run 从 registry.main 取 Agent | 修改 |

## 测试策略

### 单元测试

1. **AgentRegistry 构建**：mock config 有 main + coder + searcher，验证实例数、各子 Agent 的 denied_tools 过滤生效
2. **delegate 工具**：
   - 正常委派：mock 子 Agent 返回预设文本，验证 delegate 工具返回该文本
   - 未知子 Agent 名：返回错误文本
   - 超时：mock 子 Agent sleep 超过 timeout，验证返回部分输出 + 超时提示
3. **工具过滤**：coder 的 ToolRegistry 不含 denied 工具，不含 delegate
4. **config 解析**：`denied_tools` 和 `delegate_timeout` 字段正确反序列化

### 集成测试

1. **端到端委派**：mock provider 让主 Agent 第一轮返回 delegate 工具调用，第二轮返回整合文本。验证子 Agent session 有任务记录、主 Agent session 有 tool result
2. **持久 session**：同一子 Agent 两次委派，验证第二次能看到第一次的上下文

### 手动验收

- CLI 配一个 coder 子 Agent，让主 Agent 委派写代码任务
- 验证主 Agent 回复里整合了子 Agent 的结果
- 验证子 Agent 的 terminal 工具按 CLI whitelist 确认

## 范围边界

**做**：
- AgentRegistry 预加载
- delegate 工具
- config schema 扩展（denied_tools / delegate_timeout）
- 子 Agent 持久 session
- 委派超时 + 部分结果保留

**不做**：
- 热重载配置（未来独立特性）
- 子 Agent 并行执行
- 子 Agent 再委派
- Web 面板创建/编辑子 Agent（P2-b）

## 风险

1. **委派期间主 Agent 不可用**：同步委派下，子 Agent 执行期间主 Agent 的 Mutex 被持有，用户无法打断或继续对话。这是现有架构的既有行为（主 Agent 执行任何工具期间都锁住），委派只是可能延长锁持有时间。`delegate_timeout`（默认 120 秒）是上限。P2-c 的用户中止能力可缓解。

2. **子 Agent context 增长**：持久 session 下子 Agent context 会累积。复用现有 compaction 机制（子 Agent 的 `context_size` / `context_threshold` 从 config 读或继承默认）。

3. **delegate 工具的 LLM 调用可靠性**：LLM 可能不调 delegate 而自己硬干。通过 system prompt 引导 + delegate 工具描述明确"专业任务应委派"缓解。
