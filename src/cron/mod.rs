pub mod runner;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// cron.toml 根配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CronConfig {
    #[serde(default)]
    pub task: Vec<CronTask>,
}

/// 单个 cron 任务定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronTask {
    pub id: String,
    /// 5 字段 cron 表达式（分 时 日 月 周），内部转 6 字段喂给调度器
    pub schedule: String,
    pub mode: CronMode,
    /// 推送目标：qq / cli / web
    pub channel: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// mode = "agent" 时：注入主 agent 上下文的提示词
    #[serde(default)]
    pub prompt: Option<String>,
    /// mode = "tools" 时：预定义工具链
    #[serde(default)]
    pub steps: Option<Vec<Step>>,
}

fn default_enabled() -> bool {
    true
}

/// 任务模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CronMode {
    /// 唤醒主 agent 跑一轮对话
    Agent,
    /// 直接按 steps 顺序执行工具链
    Tools,
}

/// tools 模式单步
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub tool: String,
    /// 工具参数；省略时默认空对象 {}（便于 `[[task]] steps = [{tool = "x"}]` 简写）
    #[serde(default = "default_args")]
    pub args: Value,
}

fn default_args() -> Value {
    json!({})
}

impl CronConfig {
    /// 从文件加载 cron.toml；文件不存在返回空配置（无 cron 任务）
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        let cfg: CronConfig = toml::from_str(&content)?;
        Ok(cfg)
    }

    /// 序列化为 TOML 文本
    pub fn to_toml(&self) -> anyhow::Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }
}

/// 主动推送抽象：cron runner 通过此 trait 把结果推送到 channel
#[async_trait::async_trait]
pub trait ProactivePusher: Send + Sync {
    /// 推送一条文本消息到 channel；失败返回 Err（runner 记 log，不重试）
    async fn push(&self, message: &str) -> anyhow::Result<()>;
}
