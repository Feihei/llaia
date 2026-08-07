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
    /// task_id → job_uuid 映射（动态 add/remove 时跟踪调度器内的 job）
    job_uuids: tokio::sync::Mutex<HashMap<String, uuid::Uuid>>,
    /// pusher 注册表：channel 名 → pusher（qq / web / cli）
    pushers: HashMap<String, Arc<dyn ProactivePusher>>,
    /// 主 agent registry（共享）
    registry: Arc<AgentRegistry>,
    /// cron.toml 路径（add/update/remove 时回写）
    cron_path: std::path::PathBuf,
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
        let mut job_uuids_map = HashMap::new();
        for task in &cfg.task {
            tasks_map.insert(task.id.clone(), task.clone());
            if !task.enabled {
                tracing::info!(task = %task.id, "cron task disabled, skip");
                continue;
            }
            let job = build_job(task, &pushers, &registry)?;
            let uuid = scheduler
                .add(job)
                .await
                .map_err(|e| anyhow::anyhow!("add cron job '{}': {}", task.id, e))?;
            job_uuids_map.insert(task.id.clone(), uuid);
        }

        scheduler
            .start()
            .await
            .map_err(|e| anyhow::anyhow!("start cron scheduler: {}", e))?;
        tracing::info!(tasks = cfg.task.len(), "CronScheduler started");

        Ok(Self {
            scheduler,
            tasks: tokio::sync::Mutex::new(tasks_map),
            job_uuids: tokio::sync::Mutex::new(job_uuids_map),
            pushers,
            registry,
            cron_path: cron_path.to_path_buf(),
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

    /// 添加一个新任务：校验 → 注册到调度器（enabled 时）→ 更新内存 map → 回写 cron.toml。
    /// id 重复时返回错误。
    pub async fn add_task(&self, task: CronTask) -> anyhow::Result<()> {
        validate_task(&task)?;
        let mut tasks = self.tasks.lock().await;
        let mut job_uuids = self.job_uuids.lock().await;
        if tasks.contains_key(&task.id) {
            anyhow::bail!("cron task id already exists: {}", task.id);
        }
        if task.enabled {
            let job = build_job(&task, &self.pushers, &self.registry)?;
            let uuid = self
                .scheduler
                .add(job)
                .await
                .map_err(|e| anyhow::anyhow!("schedule task '{}': {}", task.id, e))?;
            job_uuids.insert(task.id.clone(), uuid);
        }
        tasks.insert(task.id.clone(), task);
        drop(tasks);
        drop(job_uuids);
        self.write_cron_toml().await?;
        let task_count = self.tasks.lock().await.keys().count();
        tracing::info!(task = task_count, "cron task added");
        Ok(())
    }

    /// 更新一个已存在任务：移除旧 job → 注册新 job（enabled 时）→ 更新 map → 回写 cron.toml。
    /// id 不存在时返回错误。
    pub async fn update_task(&self, task: CronTask) -> anyhow::Result<()> {
        validate_task(&task)?;
        let id = task.id.clone();
        let mut tasks = self.tasks.lock().await;
        let mut job_uuids = self.job_uuids.lock().await;
        if !tasks.contains_key(&id) {
            anyhow::bail!("cron task not found: {}", id);
        }
        // 移除旧 job
        if let Some(old_uuid) = job_uuids.remove(&id) {
            self.scheduler
                .remove(&old_uuid)
                .await
                .map_err(|e| anyhow::anyhow!("remove old job '{}': {}", id, e))?;
        }
        // 注册新 job
        if task.enabled {
            let job = build_job(&task, &self.pushers, &self.registry)?;
            let uuid = self
                .scheduler
                .add(job)
                .await
                .map_err(|e| anyhow::anyhow!("schedule updated task '{}': {}", id, e))?;
            job_uuids.insert(id.clone(), uuid);
        }
        tasks.insert(id.clone(), task);
        drop(tasks);
        drop(job_uuids);
        self.write_cron_toml().await?;
        tracing::info!(task = %id, "cron task updated");
        Ok(())
    }

    /// 删除一个任务：移除 job（如已注册）→ 从 map 移除 → 回写 cron.toml。
    /// id 不存在时返回错误。
    pub async fn remove_task(&self, task_id: &str) -> anyhow::Result<()> {
        let mut tasks = self.tasks.lock().await;
        let mut job_uuids = self.job_uuids.lock().await;
        if tasks.remove(task_id).is_none() {
            anyhow::bail!("cron task not found: {}", task_id);
        }
        if let Some(uuid) = job_uuids.remove(task_id) {
            self.scheduler
                .remove(&uuid)
                .await
                .map_err(|e| anyhow::anyhow!("remove job '{}': {}", task_id, e))?;
        }
        drop(tasks);
        drop(job_uuids);
        self.write_cron_toml().await?;
        tracing::info!(task = %task_id, "cron task removed");
        Ok(())
    }

    /// 把当前 tasks map 序列化为 TOML 并原子回写 cron.toml。
    /// 注意：会丢失原文件中的注释（程序化编辑的代价）。
    async fn write_cron_toml(&self) -> anyhow::Result<()> {
        let tasks: Vec<CronTask> = self
            .tasks
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let cfg = CronConfig { task: tasks };
        let toml_str = cfg.to_toml()?;
        let tmp = self.cron_path.with_extension("toml.tmp");
        std::fs::write(&tmp, &toml_str).map_err(|e| anyhow::anyhow!("write cron tmp: {}", e))?;
        std::fs::rename(&tmp, &self.cron_path)
            .map_err(|e| anyhow::anyhow!("rename cron.toml: {}", e))?;
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

/// 构造一个 cron Job：捕获 agent / task / pusher，到点调用 runner::run_task。
fn build_job(
    task: &CronTask,
    pushers: &HashMap<String, Arc<dyn ProactivePusher>>,
    registry: &Arc<AgentRegistry>,
) -> anyhow::Result<tokio_cron_scheduler::Job> {
    let pusher = pushers.get(&task.channel).cloned();
    let agent = registry.main.clone();
    let task_clone = task.clone();
    let six_field = to_six_field(&task.schedule);
    let job = tokio_cron_scheduler::Job::new_async(six_field.as_str(), move |_uuid, _l| {
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
    .map_err(|e| anyhow::anyhow!("parse cron expr '{}': {}", task.schedule, e))?;
    Ok(job)
}

/// 校验任务定义：id 非空、mode/prompt/steps 一致性。
/// schedule 的语法由调度器在 add 时校验，这里不重复。
fn validate_task(task: &CronTask) -> anyhow::Result<()> {
    if task.id.trim().is_empty() {
        anyhow::bail!("cron task id must not be empty");
    }
    if task.id.contains(char::is_whitespace) {
        anyhow::bail!("cron task id must not contain whitespace: {}", task.id);
    }
    match task.mode {
        CronMode::Agent => {
            if task
                .prompt
                .as_ref()
                .map(|p| p.trim().is_empty())
                .unwrap_or(true)
            {
                anyhow::bail!(
                    "cron task '{}' mode=agent requires non-empty prompt",
                    task.id
                );
            }
        }
        CronMode::Tools => {
            if task.steps.as_ref().map(|s| s.is_empty()).unwrap_or(true) {
                anyhow::bail!(
                    "cron task '{}' mode=tools requires non-empty steps",
                    task.id
                );
            }
        }
    }
    Ok(())
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
