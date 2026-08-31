pub mod runner;

use crate::agent::{Agent, AgentRegistry};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
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
    /// 调度时区：cron 表达式按此时区解释（默认 UTC → 需配置 [runtime].timezone）。
    /// None 时退化为 UTC（保持历史行为，但会与本地时间偏差 8h，如 Asia/Shanghai）。
    timezone: Option<Tz>,
}

impl CronScheduler {
    /// 启动调度器：加载 cron.toml，注册所有 enabled 任务。
    /// cron.toml 不存在时返回空调度器（无任务，不报错）。
    pub async fn start(
        cron_path: &Path,
        registry: Arc<AgentRegistry>,
        pushers: HashMap<String, Arc<dyn ProactivePusher>>,
        timezone: Option<Tz>,
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
            let job = build_job(task, &pushers, &registry, timezone)?;
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
            timezone,
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
        let pusher = build_fanout_pusher(&self.pushers, &task.channel);
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
            let job = build_job(&task, &self.pushers, &self.registry, self.timezone)?;
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
            let job = build_job(&task, &self.pushers, &self.registry, self.timezone)?;
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

    /// 克隆当前已注册的 pusher 表（热重载 cron 时复用，避免重新构造）。
    pub fn pushers_clone(&self) -> HashMap<String, Arc<dyn ProactivePusher>> {
        self.pushers.clone()
    }

    /// 热重载任务集：复用**已运行**的调度器实例（不重启后台 ticker）。
    ///
    /// 与 [`CronScheduler::start`] 不同，本方法不会调用 `JobScheduler::start`
    /// （其 future 是 `!Send`，无法在 axum handler 内 await）。它只移除全部旧
    /// job 并按最新 cron.toml 重新 `add`，因此返回的 future 是 `Send`，可直接在
    /// WebUI 配置保存的热加载路径里 await。
    pub async fn reload(&self, cron_path: &Path) -> anyhow::Result<()> {
        // 1. 清除旧 job
        {
            let mut job_uuids = self.job_uuids.lock().await;
            let ids: Vec<uuid::Uuid> = job_uuids.drain().map(|(_, u)| u).collect();
            for u in ids {
                if let Err(e) = self.scheduler.remove(&u).await {
                    tracing::warn!(uuid = %u, error = %e, "remove old cron job failed");
                }
            }
        }

        // 2. 重新加载配置并注册 enabled 任务
        let cfg = CronConfig::load(cron_path)?;
        let mut tasks_map: HashMap<String, CronTask> = HashMap::new();
        let mut job_uuids_map: HashMap<String, uuid::Uuid> = HashMap::new();
        let pushers = self.pushers.clone();
        for task in &cfg.task {
            tasks_map.insert(task.id.clone(), task.clone());
            if !task.enabled {
                tracing::info!(task = %task.id, "cron task disabled, skip");
                continue;
            }
            let job = build_job(task, &pushers, &self.registry, self.timezone)?;
            let uuid = self
                .scheduler
                .add(job)
                .await
                .map_err(|e| anyhow::anyhow!("add cron job '{}': {}", task.id, e))?;
            job_uuids_map.insert(task.id.clone(), uuid);
        }

        // 3. 更新缓存
        *self.tasks.lock().await = tasks_map;
        *self.job_uuids.lock().await = job_uuids_map;
        tracing::info!("CronScheduler reloaded (hot)");
        Ok(())
    }
}

/// 对 `!Send` 的 tokio-cron-scheduler 的封装。
///
/// `tokio_cron_scheduler` 的全部异步 API（`start` / `add` / `remove`）返回的
/// future 都是 `!Send`，因为它们在 await 点持有 `RwLock` guard。这使得无法在
/// axum handler（要求 future: `Send`）内直接 await。本 handle 在**专属的
/// 单线程 tokio runtime**（一个 `std::thread` + `current_thread` runtime）上
/// 驱动调度器及其全部 `!Send` 操作，对外只暴露 `Send` 的接口：
///
/// - `scheduler: Arc<CronScheduler>` —— 供 handler 调用 `list_tasks` / `trigger`
///   这类本身 `Send` 的方法（只读共享存储，不触碰 `!Send` 调度器 API）。
/// - `reload(...)` —— 通过 channel 把请求发给专属线程，线程内 await `!Send` 的
///   `CronScheduler::reload`（仅 `remove`/`add`，不重启 ticker），再用 oneshot
///   把结果回传，整个对外 future 是 `Send`。
pub struct CronHandle {
    /// 已启动的调度器（共享存储由内部 ticker 持续读取）
    pub scheduler: Arc<CronScheduler>,
    /// 发往专属线程的命令通道
    tx: tokio::sync::mpsc::UnboundedSender<CronCommand>,
}

enum CronCommand {
    /// 热重载任务集（path 指向最新 cron.toml）；resp 回传结果
    Reload(
        std::path::PathBuf,
        tokio::sync::oneshot::Sender<anyhow::Result<()>>,
    ),
    /// 停止专属线程的驱动循环（drop 时也会发）
    Stop,
}

impl Drop for CronHandle {
    fn drop(&mut self) {
        // 通知专属线程退出驱动循环；通道已关闭时静默忽略
        let _ = self.tx.send(CronCommand::Stop);
    }
}

impl CronHandle {
    /// 在专属单线程 runtime 上启动 cron 调度器，返回可跨线程安全使用的 handle。
    ///
    /// 调度器内部的 ticker / 监听器均 spawn 在该专属 runtime 上，因此会随该
    /// runtime（即专属线程）持续存活，直至收到 `Stop` 或 handle 被 drop。
    pub async fn start(
        cron_path: &Path,
        registry: Arc<AgentRegistry>,
        pushers: HashMap<String, Arc<dyn ProactivePusher>>,
        timezone: Option<Tz>,
    ) -> anyhow::Result<Arc<Self>> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CronCommand>();
        let (sched_tx, sched_rx) = tokio::sync::oneshot::channel::<Arc<CronScheduler>>();
        let cron_path = cron_path.to_path_buf();

        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!(error = %e, "failed to build cron runtime");
                    return;
                }
            };
            rt.block_on(async move {
                let sched =
                    match CronScheduler::start(&cron_path, registry, pushers, timezone).await {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!(error = %e, "failed to start cron scheduler");
                            return;
                        }
                    };
                let sched = Arc::new(sched);
                if sched_tx.send(sched.clone()).is_err() {
                    tracing::error!("cron scheduler handle channel closed before start");
                    return;
                }
                while let Some(cmd) = rx.recv().await {
                    match cmd {
                        CronCommand::Reload(path, resp) => {
                            let r = sched.reload(&path).await;
                            let _ = resp.send(r);
                        }
                        CronCommand::Stop => break,
                    }
                }
            });
        });

        let scheduler = sched_rx
            .await
            .map_err(|_| anyhow::anyhow!("cron scheduler thread terminated before start"))?;
        Ok(Arc::new(Self { scheduler, tx }))
    }

    /// 热重载 cron 任务集（复用已运行的调度器，Send 安全，可在 axum handler 内 await）。
    pub async fn reload(&self, cron_path: &Path) -> anyhow::Result<()> {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        if self
            .tx
            .send(CronCommand::Reload(cron_path.to_path_buf(), resp_tx))
            .is_err()
        {
            anyhow::bail!("cron scheduler thread is not running");
        }
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("cron reload response channel closed"))?
    }

    /// 请求专属线程停止（随后线程退出，runtime 释放）。
    pub fn request_stop(&self) {
        let _ = self.tx.send(CronCommand::Stop);
    }
}

/// 构造一个 cron Job：捕获 agent / task / pusher，到点调用 runner::run_task。
/// `tz` 为调度时区：Some 时按该时区解释 cron 表达式（如 Asia/Shanghai），
/// None 时退化为 UTC（与历史行为一致，但会与本地时间偏差）。
fn build_job(
    task: &CronTask,
    pushers: &HashMap<String, Arc<dyn ProactivePusher>>,
    registry: &Arc<AgentRegistry>,
    tz: Option<Tz>,
) -> anyhow::Result<tokio_cron_scheduler::Job> {
    // 组合推送：任务指定 channel + web（WebUI 是主交互界面，
    // 把结果/失败都叠加推到 web，确保用户在 WebUI 也能看到 cron 产出）。
    let pusher = build_fanout_pusher(pushers, &task.channel);
    let agent = registry.main.clone();
    let task_clone = task.clone();
    let six_field = to_six_field(&task.schedule);
    // 闭包必须**内联**传给 new_async / new_async_tz（见 cron_run_future 注释）。
    let job = match tz {
        Some(tz) => {
            let a = agent.clone();
            let t = task_clone.clone();
            let p = pusher.clone();
            tokio_cron_scheduler::Job::new_async_tz(six_field.as_str(), tz, move |_uuid, _l| {
                cron_run_future(a.clone(), t.clone(), p.clone())
            })
            .map_err(|e| anyhow::anyhow!("parse cron expr '{}': {}", task.schedule, e))?
        }
        None => {
            let a = agent.clone();
            let t = task_clone.clone();
            let p = pusher.clone();
            tokio_cron_scheduler::Job::new_async(six_field.as_str(), move |_uuid, _l| {
                cron_run_future(a.clone(), t.clone(), p.clone())
            })
            .map_err(|e| anyhow::anyhow!("parse cron expr '{}': {}", task.schedule, e))?
        }
    };
    Ok(job)
}

/// cron 任务执行闭包的实际工作体：把 `agent` / `task` / `pusher` 搬进一个
/// `Pin<Box<dyn Future<Output = ()> + Send>>`，供 `Job::new_async(_tz)` 直接消费。
///
/// 之所以抽成独立函数并显式标注返回类型，是因为 `Job::new_async` / `new_async_tz`
/// 要求闭包返回 `Pin<Box<dyn Future + Send>>`。若写成 `let run = move |..| { .. };`
/// 再把 `run` 传进 `match` 的两个分支，闭包返回类型会在独立推断时**缺失 `Send`
/// 义务**，导致无法 coerce 为 `Pin<Box<dyn Future + Send>>`（编译报 E0271）。
/// 把返回类型固定为本函数签名，并在调用点**内联**传闭包，即可让期望类型正确流入。
fn cron_run_future(
    agent: Arc<tokio::sync::Mutex<Agent>>,
    task: CronTask,
    pusher: Arc<dyn ProactivePusher>,
) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(async move {
        tracing::info!(task = %task.id, "cron task triggered");
        runner::run_task(agent, &task, pusher.as_ref()).await;
    })
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

/// 组合推送器：把同一条消息依次推送到内部多个 pusher。
/// 只要有一个 channel 推送成功即视为成功；单个失败只记 warn，不阻断其它 channel。
struct FanoutPusher {
    pushers: Vec<Arc<dyn ProactivePusher>>,
}

#[async_trait::async_trait]
impl ProactivePusher for FanoutPusher {
    async fn push(&self, message: &str) -> anyhow::Result<()> {
        let mut any_ok = false;
        for p in &self.pushers {
            match p.push(message).await {
                Ok(()) => any_ok = true,
                Err(e) => tracing::warn!(error = %e, "fanout push to one channel failed"),
            }
        }
        if any_ok {
            Ok(())
        } else {
            Err(anyhow::anyhow!("all fanout pushers failed"))
        }
    }
}

/// 构造一个推送器：以任务指定 channel 为主，额外叠加 web channel（主交互界面）。
/// 两者为同一 Arc 时去重，避免重复推送；都为空时返回 NoopPusher（结果静默丢弃）。
fn build_fanout_pusher(
    pushers: &HashMap<String, Arc<dyn ProactivePusher>>,
    channel: &str,
) -> Arc<dyn ProactivePusher> {
    let primary = pushers.get(channel).cloned();
    let web = pushers.get("web").cloned();
    let mut list: Vec<Arc<dyn ProactivePusher>> = Vec::new();
    if let Some(p) = &primary {
        list.push(p.clone());
    }
    if let Some(w) = &web {
        let dup = primary.as_ref().map(|p| Arc::ptr_eq(p, w)).unwrap_or(false);
        if !dup {
            list.push(w.clone());
        }
    }
    if list.is_empty() {
        if primary.is_none() {
            tracing::warn!(
                channel = channel,
                "no pusher for cron channel and no web pusher, result will be lost"
            );
        }
        Arc::new(NoopPusher)
    } else {
        Arc::new(FanoutPusher { pushers: list })
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
