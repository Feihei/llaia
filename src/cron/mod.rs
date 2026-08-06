pub mod runner;

use crate::agent::AgentRegistry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

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

/// 把 5 字段 cron 表达式（分 时 日 月 周）转成 6 字段（秒 分 时 日 月 周），
/// 供 tokio_cron_scheduler 使用。秒固定为 0。
///
/// 已是 6 字段的输入原样返回（便于用户直接写 6 字段）。
fn to_six_field(schedule: &str) -> String {
    let trimmed = schedule.trim();
    // 5 字段 → 6 字段；其他情况原样返回交给调度器校验
    let field_count = trimmed.split_whitespace().count();
    if field_count == 5 {
        format!("0 {}", trimmed)
    } else {
        trimmed.to_string()
    }
}

/// cron 调度器：加载 cron.toml，注册任务，到点执行。
pub struct CronScheduler {
    scheduler: tokio_cron_scheduler::JobScheduler,
    /// 任务定义缓存（供 list/trigger 用，含 disabled 任务）
    tasks: tokio::sync::Mutex<HashMap<String, CronTask>>,
    /// pusher 注册表：channel 名 → pusher（qq / web / cli）
    pushers: HashMap<String, Arc<dyn ProactivePusher>>,
    /// 主 agent registry（共享）
    registry: Arc<AgentRegistry>,
}

impl CronScheduler {
    /// 启动调度器：加载 cron.toml，注册所有 enabled 任务。
    /// cron.toml 不存在时返回空调度器（无任务，不报错）。
    pub async fn start(
        cron_path: &Path,
        registry: Arc<AgentRegistry>,
        pushers: HashMap<String, Arc<dyn ProactivePusher>>,
    ) -> anyhow::Result<Self> {
        let cfg = CronConfig::load(cron_path)?;
        let scheduler = tokio_cron_scheduler::JobScheduler::new()
            .await
            .map_err(|e| anyhow::anyhow!("init cron scheduler: {}", e))?;

        let mut tasks_map = HashMap::new();
        for task in &cfg.task {
            tasks_map.insert(task.id.clone(), task.clone());
            if !task.enabled {
                tracing::info!(task = %task.id, "cron task disabled, skip");
                continue;
            }
            let pusher = pushers.get(&task.channel).cloned();
            let agent = registry.main.clone();
            let task_clone = task.clone();
            let six_field = to_six_field(&task.schedule);
            scheduler
                .add(
                    tokio_cron_scheduler::Job::new_async(six_field.as_str(), move |_uuid, _l| {
                        let agent = agent.clone();
                        let task = task_clone.clone();
                        let pusher = pusher.clone();
                        Box::pin(async move {
                            let noop = NoopPusher;
                            let pusher_ref: &dyn ProactivePusher = match &pusher {
                                Some(p) => p.as_ref(),
                                None => {
                                    tracing::warn!(
                                        task = %task.id,
                                        channel = %task.channel,
                                        "no pusher for channel, cron result will be lost"
                                    );
                                    &noop
                                }
                            };
                            tracing::info!(task = %task.id, "cron task triggered");
                            runner::run_task(agent, &task, pusher_ref).await;
                        })
                    })
                    .map_err(|e| anyhow::anyhow!("parse cron expr '{}': {}", task.schedule, e))?,
                )
                .await
                .map_err(|e| anyhow::anyhow!("add cron job '{}': {}", task.id, e))?;
        }

        scheduler
            .start()
            .await
            .map_err(|e| anyhow::anyhow!("start cron scheduler: {}", e))?;
        tracing::info!(tasks = cfg.task.len(), "CronScheduler started");

        Ok(Self {
            scheduler,
            tasks: tokio::sync::Mutex::new(tasks_map),
            pushers,
            registry,
        })
    }

    /// 列出所有任务定义（供 WebUI 展示，含 disabled）
    pub async fn list_tasks(&self) -> Vec<CronTask> {
        self.tasks.lock().await.values().cloned().collect()
    }

    /// 手动触发一个任务（供 WebUI "立即执行" 按钮）。
    /// 任务在后台 spawn 执行，本方法立即返回。
    pub async fn trigger(&self, task_id: &str) -> anyhow::Result<()> {
        let task = self.tasks.lock().await.get(task_id).cloned();
        let task = match task {
            Some(t) => t,
            None => anyhow::bail!("cron task not found: {}", task_id),
        };
        let pusher = self.pushers.get(&task.channel).cloned();
        let noop = Arc::new(NoopPusher) as Arc<dyn ProactivePusher>;
        let pusher = match pusher {
            Some(p) => p,
            None => {
                tracing::warn!(
                    task = %task.id,
                    channel = %task.channel,
                    "no pusher for channel on manual trigger, result will be lost"
                );
                noop
            }
        };
        let agent = self.registry.main.clone();
        tokio::spawn(async move {
            tracing::info!(task = %task.id, "cron task manually triggered");
            runner::run_task(agent, &task, pusher.as_ref()).await;
        });
        Ok(())
    }

    /// 显式停止调度器（drop 时也会停，本方法用于优雅退出场景）。
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        let mut s = self.scheduler.clone();
        s.shutdown()
            .await
            .map_err(|e| anyhow::anyhow!("shutdown cron scheduler: {}", e))
    }
}

/// 空 pusher：channel 不可用时的占位，丢弃所有推送。
struct NoopPusher;

#[async_trait::async_trait]
impl ProactivePusher for NoopPusher {
    async fn push(&self, _message: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod scheduler_tests {
    use super::*;

    #[test]
    fn test_to_six_field_5_fields_prepended() {
        assert_eq!(to_six_field("0 8 * * *"), "0 0 8 * * *");
        assert_eq!(to_six_field("*/30 * * * *"), "0 */30 * * * *");
        assert_eq!(to_six_field("0 0 1 * 0"), "0 0 0 1 * 0");
    }

    #[test]
    fn test_to_six_field_6_fields_unchanged() {
        assert_eq!(to_six_field("0 0 8 * * *"), "0 0 8 * * *");
        assert_eq!(to_six_field("30 */5 * * * *"), "30 */5 * * * *");
    }

    #[test]
    fn test_to_six_field_trims_whitespace() {
        assert_eq!(to_six_field("  0 8 * * *  "), "0 0 8 * * *");
    }

    #[test]
    fn test_to_six_field_other_lengths_unchanged() {
        // 1 字段、7 字段等不常见情况原样返回，交给调度器校验
        assert_eq!(to_six_field("*"), "*");
        assert_eq!(to_six_field("0 0 0 * * * *"), "0 0 0 * * * *");
    }

    #[tokio::test]
    async fn test_noop_pusher_swallows_message() {
        let p = NoopPusher;
        assert!(p.push("anything").await.is_ok());
    }
}
