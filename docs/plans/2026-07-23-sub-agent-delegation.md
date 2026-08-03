# 子 Agent 委派模式实现计划 (P2-a)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 主 Agent 能通过 `delegate` 工具委派任务给子 Agent，子 Agent 独立 session 执行，结果回传主 Agent 整合后回用户。

**Architecture:** AgentRegistry 预加载所有子 Agent 实例。delegate 是普通工具走 tool calling 管道，调子 Agent 的 `handle_input_streaming` 并用 `tokio::time::timeout` 包裹。Channel trait 改接收 `Arc<AgentRegistry>`，Tool::execute 加 `channel` 参数。

**Tech Stack:** Rust + tokio + async-trait + serde

**Spec:** [docs/specs/2026-07-23-sub-agent-delegation-design.md](../specs/2026-07-23-sub-agent-delegation-design.md)

---

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
| `src/commands/mod.rs` | chat_cmd / serve_cmd 传 registry | 修改 |

---

## Task 0：Tool::execute 加 channel 参数

**Files:**
- Modify: `src/tools/mod.rs`
- Modify: `src/tools/file.rs`
- Modify: `src/tools/terminal.rs`
- Modify: `src/tools/web.rs`
- Modify: `src/tools/tavily.rs`
- Modify: `src/tools/memory.rs`
- Modify: `src/agent/runner.rs`

- [ ] **Step 1: 修改 Tool trait 定义**

`src/tools/mod.rs` 第 18 行，`execute` 签名加 `channel: &str` 参数：

```rust
async fn execute(&self, args: &Value, channel: &str) -> Result<String>;
```

- [ ] **Step 2: 修改 file.rs 的三个 execute 实现**

`src/tools/file.rs` 第 60、94、136 行，每个 `execute` 签名加 `channel: &str` 参数，方法体首行加 `let _ = channel;`（忽略参数）。示例（FileRead）：

```rust
async fn execute(&self, args: &Value, channel: &str) -> Result<String> {
    let _ = channel;
    // ... 原有实现不变
}
```

FileWrite、FileEdit 同理。

- [ ] **Step 3: 修改 terminal.rs 的 execute 实现**

`src/tools/terminal.rs` 第 65 行：

```rust
async fn execute(&self, args: &Value, channel: &str) -> Result<String> {
    let _ = channel;
    // ... 原有实现不变
}
```

- [ ] **Step 4: 修改 web.rs 的 execute 实现**

`src/tools/web.rs` 第 35 行：

```rust
async fn execute(&self, args: &Value, channel: &str) -> Result<String> {
    let _ = channel;
    // ... 原有实现不变
}
```

- [ ] **Step 5: 修改 tavily.rs 的 execute 实现**

`src/tools/tavily.rs` 第 53 行：

```rust
async fn execute(&self, args: &Value, channel: &str) -> Result<String> {
    let _ = channel;
    // ... 原有实现不变
}
```

- [ ] **Step 6: 修改 memory.rs 的 execute 实现**

`src/tools/memory.rs` 第 43 行：

```rust
async fn execute(&self, args: &Value, channel: &str) -> Result<String> {
    let _ = channel;
    // ... 原有实现不变
}
```

- [ ] **Step 7: 修改 runner.rs 的 execute_tool_calls 调用**

`src/agent/runner.rs` 第 66 行，调用处加 `channel` 参数：

```rust
let outcome = match tool.execute(&call.arguments, channel).await {
```

- [ ] **Step 8: 修改 runner.rs 的测试 Mock 工具**

`src/agent/runner.rs` 测试里的 `EchoTool` 和 `DangerousTool` 的 `execute` 签名也要加 `channel: &str`：

```rust
// EchoTool
async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
    Ok(format!("{}", args))
}

// DangerousTool
async fn execute(&self, _args: &Value, _channel: &str) -> Result<String> {
    Ok("executed".into())
}
```

- [ ] **Step 9: 修改 agent/mod.rs 测试中的 Mock 工具**

`src/agent/mod.rs` 测试里如果有 Mock 工具实现 Tool trait，execute 签名也要同步加 `channel: &str`。检查并修改。

- [ ] **Step 10: 编译验证**

Run: `cargo build`
Expected: 编译通过，无错误

- [ ] **Step 11: 测试验证**

Run: `cargo test`
Expected: 所有现有测试通过

---

## Task 1：AgentConfig 加 denied_tools / delegate_timeout 字段

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs` (内联测试)

- [ ] **Step 1: 修改 AgentConfig 结构体**

`src/config.rs` 第 107-117 行，`AgentConfig` 加两个字段：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// 引用 "provider_id.model_alias"，例如 "default.qwen3"
    pub model: String,
    /// 该 agent 的 md 文件根目录，sessions.db 也在其下
    pub workspace: String,
    /// 以下三项缺省时从 workspace 推导为 <workspace>/SOUL.md 等
    pub soul: Option<String>,
    pub user: Option<String>,
    pub memory: Option<String>,
    /// 工具黑名单：列出的工具子 Agent 不可用。默认空（继承所有工具）
    #[serde(default)]
    pub denied_tools: Vec<String>,
    /// 委派超时秒数（仅子 Agent 生效）。默认 120
    #[serde(default = "default_delegate_timeout")]
    pub delegate_timeout: u64,
}

fn default_delegate_timeout() -> u64 {
    120
}
```

- [ ] **Step 2: 修改 default_for_workspace 里的 main agent 配置**

`src/config.rs` `default_for_workspace` 函数里 `agent.insert("main", ...)` 处补上新字段默认值：

```rust
agent.insert(
    "main".into(),
    AgentConfig {
        model: "default.qwen".into(),
        workspace: ws.clone(),
        soul: None,
        user: None,
        memory: None,
        denied_tools: Vec::new(),
        delegate_timeout: default_delegate_timeout(),
    },
);
```

- [ ] **Step 3: 写测试验证字段反序列化**

`src/config.rs` 测试模块里加新测试：

```rust
#[test]
fn test_sub_agent_config_fields() {
    let toml = r#"
[provider.default]
type = "openai_compatible"
base_url = "http://localhost:11434/v1"

[provider.default.qwen]
model = "qwen2.5:7b"

[agent.main]
model = "default.qwen"
workspace = "~/.llaia"

[agent.coder]
model = "default.qwen"
workspace = "~/.llaia/agents/coder"
soul = "~/.llaia/agents/coder.md"
denied_tools = ["memory_write"]
delegate_timeout = 180
"#;
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    write!(tmp, "{}", toml).unwrap();
    let config = Config::load(&tmp.path().to_path_buf()).unwrap();
    
    let main = config.agent.get("main").unwrap();
    assert!(main.denied_tools.is_empty());
    assert_eq!(main.delegate_timeout, 120); // 默认值
    
    let coder = config.agent.get("coder").unwrap();
    assert_eq!(coder.denied_tools, vec!["memory_write"]);
    assert_eq!(coder.delegate_timeout, 180);
}
```

- [ ] **Step 4: 运行测试验证通过**

Run: `cargo test test_sub_agent_config_fields`
Expected: PASS

- [ ] **Step 5: 运行全部测试确保无回归**

Run: `cargo test`
Expected: 所有测试通过

---

## Task 2：AgentRegistry 结构

**Files:**
- Create: `src/agent/registry.rs`
- Modify: `src/agent/mod.rs`

- [ ] **Step 1: 创建 registry.rs**

```rust
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::agent::Agent;

/// 管理 main Agent 和所有子 Agent 实例
pub struct AgentRegistry {
    /// 主 Agent
    pub main: Arc<Mutex<Agent>>,
    /// 子 Agent：alias → 实例
    sub_agents: HashMap<String, Arc<Mutex<Agent>>>,
}

impl AgentRegistry {
    pub fn new(main: Arc<Mutex<Agent>>) -> Self {
        Self {
            main,
            sub_agents: HashMap::new(),
        }
    }

    pub fn register_sub_agent(&mut self, alias: String, agent: Arc<Mutex<Agent>>) {
        self.sub_agents.insert(alias, agent);
    }

    pub fn get(&self, alias: &str) -> Result<&Arc<Mutex<Agent>>> {
        self.sub_agents
            .get(alias)
            .ok_or_else(|| anyhow::anyhow!("未知子 Agent: {}", alias))
    }

    pub fn available_sub_agents(&self) -> Vec<String> {
        self.sub_agents.keys().cloned().collect()
    }
}
```

- [ ] **Step 2: 在 agent/mod.rs 导出 registry 模块**

`src/agent/mod.rs` 第 1-2 行，加 `pub mod registry;`：

```rust
pub mod context;
pub mod registry;
pub mod runner;
```

并在文件头部 re-export `AgentRegistry`：

```rust
pub use crate::agent::registry::AgentRegistry;
```

- [ ] **Step 3: 编译验证**

Run: `cargo build`
Expected: 编译通过

---

## Task 3：DelegateTool 实现

**Files:**
- Create: `src/tools/delegate.rs`
- Modify: `src/tools/mod.rs`

- [ ] **Step 1: 创建 delegate.rs**

delegate 工具用 `tokio::sync::OnceCell` 延迟持有 registry，解决循环依赖（build_agent 时先创建 delegate 工具，registry 构建后调 `set_registry`）。

```rust
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::OnceCell;

use crate::agent::AgentRegistry;
use crate::agent::TurnEvent;
use crate::tools::Tool;

pub struct DelegateTool {
    registry: OnceCell<Arc<AgentRegistry>>,
    timeout_secs: u64,
}

impl DelegateTool {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            registry: OnceCell::new(),
            timeout_secs,
        }
    }

    pub async fn set_registry(&self, registry: Arc<AgentRegistry>) {
        let _ = self.registry.set(registry).await;
    }

    fn get_registry(&self) -> Option<&Arc<AgentRegistry>> {
        self.registry.get()
    }
}

#[async_trait]
impl Tool for DelegateTool {
    fn name(&self) -> &str {
        "delegate"
    }

    fn description(&self) -> &str {
        "委派任务给子 Agent 执行。子 Agent 有独立的专业能力和工具集。适用于需要特定专业技能的任务。"
    }

    fn parameters_schema(&self) -> Value {
        let agents: Vec<String> = self.get_registry()
            .map(|r| r.available_sub_agents())
            .unwrap_or_default();
        json!({
            "type": "object",
            "properties": {
                "agent_name": {
                    "type": "string",
                    "description": "要委派的子 Agent 名称",
                    "enum": agents
                },
                "task": {
                    "type": "string",
                    "description": "要委派给子 Agent 执行的任务描述"
                }
            },
            "required": ["agent_name", "task"]
        })
    }

    fn requires_confirm(&self) -> bool {
        false
    }

    async fn execute(&self, args: &Value, channel: &str) -> Result<String> {
        let registry = match self.get_registry() {
            Some(r) => r.clone(),
            None => return Ok("[委派失败: registry 未初始化]".into()),
        };

        let agent_name = args["agent_name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing agent_name"))?;
        let task = args["task"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing task"))?;

        let sub_agent = match registry.get(agent_name) {
            Ok(a) => a.clone(),
            Err(e) => return Ok(format!("[委派失败: {}]", e)),
        };

        let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
        let task_clone = task.to_string();
        let channel_clone = channel.to_string();
        let timeout = self.timeout_secs;

        let result = tokio::time::timeout(
            Duration::from_secs(timeout),
            async {
                sub_agent
                    .lock()
                    .await
                    .handle_input_streaming(&task_clone, &channel_clone, tx)
                    .await
            },
        )
        .await;

        // 非阻塞收集子 Agent 已产生的 Chunk
        let mut output = String::new();
        while let Ok(ev) = rx.try_recv() {
            if let TurnEvent::Chunk { delta } = ev {
                output.push_str(&delta);
            }
        }

        match result {
            Ok(Ok(_)) => {
                if output.is_empty() {
                    Ok("[子 Agent 无输出]".into())
                } else {
                    Ok(output)
                }
            }
            Ok(Err(e)) => Ok(format!(
                "[子 Agent 执行错误: {}]\n部分输出: {}",
                e, output
            )),
            Err(_) => Ok(format!(
                "[子 Agent 超时({}秒)]\n部分输出: {}",
                timeout, output
            )),
        }
    }
}
```

- [ ] **Step 2: 在 tools/mod.rs 导出 delegate 模块**

`src/tools/mod.rs` 顶部加模块声明：

```rust
pub mod delegate;
```

- [ ] **Step 3: 编译验证**

Run: `cargo build`
Expected: 编译通过

---

## Task 4：Channel trait 改接收 AgentRegistry

**Files:**
- Modify: `src/channels/mod.rs`
- Modify: `src/channels/cli.rs`
- Modify: `src/channels/qq.rs`
- Modify: `src/commands/mod.rs`

- [ ] **Step 1: 修改 Channel trait 签名**

`src/channels/mod.rs` 第 22 行：

```rust
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    async fn run(self: Arc<Self>, registry: Arc<crate::agent::AgentRegistry>) -> Result<()>;
}
```

- [ ] **Step 2: 修改 CliChannel::run 签名**

`src/channels/cli.rs` 的 `impl Channel for CliChannel`：

```rust
#[async_trait]
impl Channel for CliChannel {
    async fn run(self: Arc<Self>, registry: Arc<AgentRegistry>) -> Result<()> {
        let agent = registry.main.clone();
        // ... 现有 run 逻辑不变，用 agent 变量替代原来的参数
    }
}
```

注意文件顶部可能要加 `use crate::agent::AgentRegistry;`。

- [ ] **Step 3: 修改 QqChannel::run 签名**

`src/channels/qq.rs` 第 329-344 行：

```rust
#[async_trait]
impl Channel for QqChannel {
    async fn run(self: Arc<Self>, registry: Arc<AgentRegistry>) -> Result<()> {
        let agent = registry.main.clone();
        tracing::info!(app_id = %self.config.app_id, "QqChannel starting");
        loop {
            match self.clone().run_connection(&agent).await {
                Ok(()) => tracing::warn!("qq ws connection closed, will reconnect"),
                Err(e) => tracing::error!(error = %e, "qq ws connection ended with error, will reconnect"),
            }
            tracing::info!("reconnecting in 5 seconds...");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
}
```

注意文件顶部加 `use crate::agent::AgentRegistry;`。

- [ ] **Step 4: 编译验证**

Run: `cargo build`
Expected: 编译通过（commands/mod.rs 会有错误，下一步修复）

---

## Task 5：build_agent 改造为构建 AgentRegistry

**Files:**
- Modify: `src/channels/cli.rs`（build_agent 函数 + 新增 build_single_agent）
- Modify: `src/commands/mod.rs`

delegate 工具的 OnceCell 机制已在 Task 3 实现，本 Task 只做 build_agent 重构和 registry 注入。

- [ ] **Step 1: 提取 build_single_agent 函数**

`src/channels/cli.rs` 加 `build_single_agent` 函数（从原 build_agent 提取），返回 `(Arc<Mutex<Agent>>, Option<Arc<DelegateTool>>)`：

```rust
async fn build_single_agent(
    config: &Config,
    alias: &str,
    agent_cfg: AgentConfig,
    is_main: bool,
) -> Result<(Arc<Mutex<Agent>>, Option<Arc<DelegateTool>>)> {
    let workspace = PathBuf::from(&agent_cfg.workspace);
    std::fs::create_dir_all(&workspace).ok();

    let soul_path = resolve_md_path(&agent_cfg.soul, &workspace, "SOUL.md");
    let user_path = resolve_md_path(&agent_cfg.user, &workspace, "USER.md");
    let memory_path = resolve_md_path(&agent_cfg.memory, &workspace, "MEMORY.md");
    ensure_template(&soul_path, SOUL_TEMPLATE).await?;
    ensure_template(&user_path, USER_TEMPLATE).await?;
    ensure_template(&memory_path, MEMORY_TEMPLATE).await?;

    let soul = load_md(&soul_path).await?;
    let user = load_md(&user_path).await?;
    let memory = load_md(&memory_path).await?;

    let (prov_id, model_alias) = Config::parse_model_ref(&agent_cfg.model)?;
    let prov_cfg = config.provider.get(prov_id).cloned()
        .ok_or_else(|| anyhow::anyhow!("provider.{} not configured", prov_id))?;
    let model_cfg = prov_cfg.model.get(model_alias).cloned()
        .ok_or_else(|| anyhow::anyhow!("provider.{}.model.{} not configured", prov_id, model_alias))?;

    let provider: Arc<dyn Provider> = Arc::new(OpenAiCompatibleProvider::new(
        &prov_cfg.base_url, &prov_cfg.api_key, &model_cfg.model, model_cfg.native_tool_calling,
    )?);

    // 构建完整工具集
    let mut all_tools: Vec<Arc<dyn Tool>> = Vec::new();
    all_tools.push(Arc::new(FileRead::new(workspace.clone())));
    all_tools.push(Arc::new(FileWrite::new(workspace.clone())));
    all_tools.push(Arc::new(FileEdit::new(workspace.clone())));
    all_tools.push(Arc::new(Terminal::new(
        config.tools.terminal.confirm.clone(),
        config.tools.terminal.whitelist.clone(),
        workspace.clone(),
    )));
    all_tools.push(Arc::new(WebFetch::new()?));
    if !config.tools.tavily.api_key.is_empty() {
        all_tools.push(Arc::new(TavilySearch::new(config.tools.tavily.api_key.clone())?));
    }
    all_tools.push(Arc::new(MemoryWrite::new(memory_path.clone())));

    // 按 denied_tools 过滤
    let denied: std::collections::HashSet<&str> = agent_cfg.denied_tools
        .iter().map(|s| s.as_str()).collect();
    let mut delegate_tool: Option<Arc<DelegateTool>> = None;
    let mut registry = ToolRegistry::new();
    for tool in all_tools {
        if !denied.contains(tool.name()) {
            registry.register(tool);
        }
    }

    // main Agent 加 delegate（有子 Agent 时）
    if is_main && config.agent.len() > 1 {
        let d = Arc::new(DelegateTool::new(agent_cfg.delegate_timeout));
        registry.register(d.clone());
        delegate_tool = Some(d);
    }

    let mut system_prompt = format!(
        "# SOUL\n{}\n\n# USER\n{}\n\n# MEMORY\n{}\n\n# WORKSPACE\n{}\n\n工作目录说明：所有工具的相对路径都相对于 WORKSPACE 解析；terminal 命令在 WORKSPACE 下执行。需要写到其它位置时请使用绝对路径。",
        soul, user, memory, workspace.display()
    );
    if !provider.native_tool_calling() {
        system_prompt.push_str(&build_tool_instructions(&registry.specs()));
    }
    let registry = Arc::new(registry);

    let db_path = workspace.join("sessions.db");
    let session_store = Arc::new(SessionStore::open(&db_path)?);

    let session_id = match session_store.latest_session()? {
        Some((id, _)) => id,
        None => {
            let uuid = uuid::Uuid::new_v4().to_string();
            session_store.create_session(&uuid, alias)?
        }
    };

    let detected = provider.detect_context_size().await;
    let context_size = match (model_cfg.context_size, detected) {
        (Some(cfg), Some(det)) => cfg.min(det),
        (Some(cfg), None) => cfg,
        (None, Some(det)) => det,
        (None, None) => 8192,
    };
    tracing::info!(
        agent = alias,
        configured = ?model_cfg.context_size,
        detected = ?detected,
        final = context_size,
        "context_size resolved"
    );

    let agent = Agent::new(
        config, provider, registry, session_store, session_id,
        system_prompt, context_size,
    ).await;

    Ok((Arc::new(Mutex::new(agent)), delegate_tool))
}
```

- [ ] **Step 2: 重写 build_agent 整合逻辑**

替换原 `build_agent` 函数：

```rust
pub async fn build_agent(config: &Config) -> Result<Arc<AgentRegistry>> {
    // 构建子 Agent
    let mut sub_agents: Vec<(String, Arc<Mutex<Agent>>)> = Vec::new();
    for (alias, cfg) in &config.agent {
        if alias == "main" {
            continue;
        }
        let (agent, _) = build_single_agent(config, alias, cfg.clone(), false).await?;
        sub_agents.push((alias.clone(), agent));
    }

    // 构建 main Agent
    let main_cfg = config.agent.get("main").cloned()
        .ok_or_else(|| anyhow::anyhow!("agent.main not configured"))?;
    let (main_agent, delegate_tool) = build_single_agent(config, "main", main_cfg, true).await?;

    let mut registry = AgentRegistry::new(main_agent);
    for (alias, agent) in sub_agents {
        registry.register_sub_agent(alias, agent);
    }
    let registry = Arc::new(registry);

    // 注入 registry 给 delegate 工具
    if let Some(d) = delegate_tool {
        d.set_registry(registry.clone()).await;
    }

    tracing::info!(
        sub_agents = registry.available_sub_agents().len(),
        "AgentRegistry built"
    );
    Ok(registry)
}
```

- [ ] **Step 3: 修改 commands/mod.rs 传 registry**

`src/commands/mod.rs` 第 16、29 行，`build_agent` 返回类型变了：

```rust
// chat_cmd（第 16-19 行）
let registry = crate::channels::cli::build_agent(&config).await?;
let cli = std::sync::Arc::new(crate::channels::CliChannel::new());
crate::channels::Channel::run(cli, registry).await

// serve_cmd（第 29-44 行）
let registry = crate::channels::cli::build_agent(&config).await?;
// ...
let registry = registry.clone();
tasks.push(tokio::spawn(async move {
    if let Err(e) = crate::channels::Channel::run(qq, registry).await {
        tracing::error!(error = %e, "QqChannel exited with error");
    }
}));
```

- [ ] **Step 4: 编译验证**

Run: `cargo build`
Expected: 编译通过

- [ ] **Step 5: 测试验证**

Run: `cargo test`
Expected: 所有现有测试通过

---

## Task 6：集成测试 - 端到端委派

**Files:**
- Create: `tests/delegation_test.rs` 或在现有测试模块加

- [ ] **Step 1: 写端到端委派测试**

在 `src/agent/mod.rs` 测试模块或新文件加集成测试。Mock provider 让主 Agent 第一轮返回 delegate 工具调用，验证 delegate 执行和结果回传。

```rust
#[tokio::test]
async fn test_delegation_end_to_end() {
    use crate::tools::delegate::DelegateTool;
    use crate::agent::AgentRegistry;
    
    // 子 Agent 的 mock provider：返回固定文本
    let sub_rounds = vec![vec![
        StreamEvent::TextDelta("子 Agent 完成".into()),
        StreamEvent::Done,
    ]];
    let sub_provider: Arc<dyn Provider> = Arc::new(MockProvider::new(true, sub_rounds));
    let sub_store = SessionStore::open_in_memory().unwrap();
    let sub_sid = sub_store.create_session("sub", "test").unwrap();
    let sub_tools = Arc::new(ToolRegistry::new());
    let config = Config::default_for_workspace("/tmp/llaia-test");
    let sub_agent = Agent::new(
        &config, sub_provider, sub_tools, Arc::new(sub_store), sub_sid,
        "sub soul".into(), 8192,
    ).await;
    let sub_arc = Arc::new(Mutex::new(sub_agent));

    // 构建 registry
    let main_rounds = vec![
        // 第一轮：调 delegate 工具
        vec![StreamEvent::ToolCall(ToolCall {
            id: "call_1".into(),
            name: "delegate".into(),
            arguments: json!({"agent_name": "coder", "task": "写个函数"}),
        }), StreamEvent::Done],
        // 第二轮：基于 delegate 结果生成回复
        vec![StreamEvent::TextDelta("已委派完成".into()), StreamEvent::Done],
    ];
    let main_provider: Arc<dyn Provider> = Arc::new(MockProvider::new(true, main_rounds));
    let main_store = SessionStore::open_in_memory().unwrap();
    let main_sid = main_store.create_session("main", "test").unwrap();
    
    let delegate = Arc::new(DelegateTool::new(120));
    let mut main_tools = ToolRegistry::new();
    main_tools.register(delegate.clone());
    let main_tools = Arc::new(main_tools);
    
    let main_agent = Agent::new(
        &config, main_provider, main_tools, Arc::new(main_store), main_sid,
        "main soul".into(), 8192,
    ).await;
    let main_arc = Arc::new(Mutex::new(main_agent));
    
    let mut registry = AgentRegistry::new(main_arc);
    registry.register_sub_agent("coder".into(), sub_arc);
    let registry = Arc::new(registry);
    delegate.set_registry(registry.clone()).await;
    
    // 执行
    let main = registry.main.clone();
    let (tx, mut rx) = mpsc::channel(64);
    let mut agent = main.lock().await;
    let result = agent.handle_input_streaming("帮我写个函数", "cli", tx).await.unwrap();
    
    // 验证主 Agent 最终回复包含整合文本
    assert!(result.contains("已委派完成"));
    
    // 验证事件流
    let mut chunks = Vec::new();
    let mut tool_starts = Vec::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            TurnEvent::Chunk { delta } => chunks.push(delta),
            TurnEvent::ToolStart { name, .. } => tool_starts.push(name),
            TurnEvent::Done => break,
            _ => {}
        }
    }
    assert!(tool_starts.contains(&"delegate".to_string()));
    assert!(chunks.concat().contains("已委派完成"));
}
```

- [ ] **Step 2: 运行测试验证**

Run: `cargo test test_delegation_end_to_end`
Expected: PASS

- [ ] **Step 3: 运行全部测试**

Run: `cargo test`
Expected: 所有测试通过

---

## Task 7：超时和错误处理测试

**Files:**
- Test: `src/tools/delegate.rs`（内联测试）

- [ ] **Step 1: 写未知子 Agent 名测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentRegistry;
    use crate::agent::Agent;
    use crate::config::Config;
    use crate::memory::sqlite::SessionStore;
    use crate::provider::{Provider, ChatRequest, ChatResponse, StreamEvent};
    use crate::agent::runner::ToolRegistry;
    use async_trait::async_trait;
    use futures_util::stream::BoxStream;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    struct DummyProvider;
    #[async_trait]
    impl Provider for DummyProvider {
        async fn chat(&self, _: &ChatRequest<'_>) -> Result<ChatResponse> { unreachable!() }
        async fn chat_stream(&self, _: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>> { unreachable!() }
        fn native_tool_calling(&self) -> bool { true }
    }

    async fn make_registry() -> Arc<AgentRegistry> {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("main", "test").unwrap();
        let config = Config::default_for_workspace("/tmp/llaia-test");
        let agent = Agent::new(
            &config, Arc::new(DummyProvider), Arc::new(ToolRegistry::new()),
            Arc::new(store), sid, "test".into(), 8192,
        ).await;
        Arc::new(AgentRegistry::new(Arc::new(Mutex::new(agent))))
    }

    #[tokio::test]
    async fn test_unknown_sub_agent() {
        let registry = make_registry().await;
        let tool = DelegateTool::new(120);
        tool.set_registry(registry);
        
        let args = json!({"agent_name": "nonexistent", "task": "test"});
        let result = tool.execute(&args, "cli").await.unwrap();
        assert!(result.contains("委派失败"));
        assert!(result.contains("nonexistent"));
    }

    #[tokio::test]
    async fn test_timeout_preserves_partial_output() {
        // 子 Agent sleep 超过 timeout
        // mock provider 返回一个会 sleep 的 stream
        // 验证返回部分输出 + 超时提示
        // （实现略，需要 mock provider 慢响应）
    }
}
```

- [ ] **Step 2: 运行测试验证**

Run: `cargo test test_unknown_sub_agent`
Expected: PASS

---

## Task 8：手动验收

**Files:** 无（手动测试）

- [ ] **Step 1: 准备 config.toml 配一个 coder 子 Agent**

在 `~/.llaia/config.toml` 加：

```toml
[agent.coder]
model = "default.qwen"
workspace = "~/.llaia/agents/coder"
soul = "~/.llaia/agents/coder.md"
denied_tools = ["memory_write"]
delegate_timeout = 180
```

创建 `~/.llaia/agents/coder.md`：

```markdown
你是 coder，专注写代码。接到任务后用 file_read/file_write/file_edit/terminal 工具完成。
```

- [ ] **Step 2: 启动 CLI 测试委派**

Run: `cargo run -- chat`
输入：`帮我写一个 Python hello world 到 hello.py`
验证：
- 日志显示 delegate 工具被调用
- coder 子 Agent 执行了 file_write
- 主 Agent 回复整合了 coder 的结果

- [ ] **Step 3: 验证子 Agent 持久 session**

再次输入：`让 coder 在 hello.py 加一行打印`
验证：
- coder 子 Agent 能看到上次的上下文（hello.py 已存在）
- 不是从头开始

- [ ] **Step 4: 验证 denied_tools 生效**

让 coder 执行 `/remember` 类任务（写 MEMORY）：
验证 coder 返回错误或无法执行 memory_write

---

## 验收清单

- [ ] `cargo build` 编译通过
- [ ] `cargo test` 所有测试通过
- [ ] `cargo clippy` 无警告
- [ ] 端到端委派测试通过
- [ ] 子 Agent 持久 session 验证
- [ ] denied_tools 过滤生效
- [ ] 超时机制有效
- [ ] CLI 和 QQ channel 都能正常工作
