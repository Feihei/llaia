use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;
use tokio::sync::Mutex as AsyncMutex;

use crate::agent::Agent;
use crate::channels::cli::build_single_agent;
use crate::config::Config;
use crate::skill::SkillManifest;
use crate::tools::delegate::DeliveryTarget;
use crate::tools::Tool;

/// 后台委派任务记录（供 /delegate-list / /delegate-cancel 管理）
pub struct BackgroundTask {
    pub id: String,
    pub agent_name: String,
    pub started: Instant,
    pub handle: tokio::task::JoinHandle<()>,
}

/// 管理 main Agent 和所有子 Agent 实例
pub struct AgentRegistry {
    /// 主 Agent
    pub main: Arc<AsyncMutex<Agent>>,
    /// 主 Agent 工作区根（缓存，避免 delegate 在持有 main 锁的调用链中再次 lock 导致死锁）
    pub main_workspace: PathBuf,
    /// 子 Agent：alias → 实例。
    /// 用 std::sync::Mutex 包一层以支持热加载时原地替换（rebuild_sub_agents 经 `&self` 调用）。
    /// 读取时只 clone 出 Arc、不持有锁跨 await。
    sub_agents: Mutex<HashMap<String, Arc<AsyncMutex<Agent>>>>,
    /// 后台委派任务注册表（异步委派用，CLI 与 serve 共用）。
    pub background_tasks: Arc<Mutex<HashMap<String, BackgroundTask>>>,
    /// 当前轮结果投递目标：channel 在 run_turn 前注入（serve→pusher，CLI→stdout）。
    /// 异步委派 spawn 时克隆此值，完成后主动推送结果。
    pub delivery: Arc<Mutex<Option<DeliveryTarget>>>,
    /// 主线 /steer 插话缓冲（plan.md #I）：与 main Agent 持同一 Arc，
    /// channel 在 turn 持锁期间经它投递（不取 Agent 锁）。
    pub steer_buffer: Arc<Mutex<std::collections::VecDeque<String>>>,
}

impl AgentRegistry {
    pub fn new(main: Arc<AsyncMutex<Agent>>, main_workspace: PathBuf) -> Self {
        Self {
            main,
            main_workspace,
            sub_agents: Mutex::new(HashMap::new()),
            background_tasks: Arc::new(Mutex::new(HashMap::new())),
            delivery: Arc::new(Mutex::new(None)),
            steer_buffer: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        }
    }

    /// 把 registry 的 steer 缓冲接到 main Agent 的那一份上（build_agent 构建后调用）。
    /// 不接也不影响功能正确性，只是 /steer 投递落不到正在跑的 turn。
    pub fn attach_steer_buffer(&mut self, buf: Arc<Mutex<std::collections::VecDeque<String>>>) {
        self.steer_buffer = buf;
    }

    /// 注入本轮的结果投递目标（各 channel 在 run_turn 前调用）。
    pub fn set_delivery(&self, d: Option<DeliveryTarget>) {
        *self.delivery.lock().unwrap() = d;
    }

    /// 克隆当前投递目标（异步委派 spawn 前调用，拿到独立副本）。
    pub fn clone_delivery(&self) -> Option<DeliveryTarget> {
        self.delivery.lock().unwrap().clone()
    }

    pub fn register_sub_agent(&self, alias: String, agent: Arc<AsyncMutex<Agent>>) {
        self.sub_agents.lock().unwrap().insert(alias, agent);
    }

    pub fn get(&self, alias: &str) -> Result<Arc<AsyncMutex<Agent>>> {
        self.sub_agents
            .lock()
            .unwrap()
            .get(alias)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown sub-agent: {}", alias))
    }

    pub fn available_sub_agents(&self) -> Vec<String> {
        self.sub_agents.lock().unwrap().keys().cloned().collect()
    }

    /// 热重载所有子 Agent：按最新 config 重建（SOUL/USER/MEMORY/工具/skills 全刷新）。
    /// 失败的单条跳过并 warn，不阻塞其它子 Agent。
    pub async fn rebuild_sub_agents(
        &self,
        config: &Config,
        config_dir: &Path,
        mcp_tools: Vec<Arc<dyn Tool>>,
        skills: &[SkillManifest],
    ) {
        let mut rebuilt = HashMap::new();
        for (alias, cfg) in &config.agent {
            if alias == "main" {
                continue;
            }
            match build_single_agent(
                config,
                config_dir,
                alias,
                cfg.clone(),
                false,
                None,
                mcp_tools.clone(),
                skills,
            )
            .await
            {
                Ok((agent, _, _, _)) => {
                    rebuilt.insert(alias.clone(), agent);
                }
                Err(e) => tracing::warn!(
                    agent = %alias,
                    error = %e,
                    "rebuild sub-agent failed, skipped"
                ),
            }
        }
        *self.sub_agents.lock().unwrap() = rebuilt;
    }
}
