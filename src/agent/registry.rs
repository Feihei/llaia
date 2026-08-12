use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::Mutex as AsyncMutex;

use crate::agent::Agent;
use crate::channels::cli::build_single_agent;
use crate::config::Config;
use crate::skill::SkillManifest;
use crate::tools::Tool;

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
}

impl AgentRegistry {
    pub fn new(main: Arc<AsyncMutex<Agent>>, main_workspace: PathBuf) -> Self {
        Self {
            main,
            main_workspace,
            sub_agents: Mutex::new(HashMap::new()),
        }
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
            .ok_or_else(|| anyhow::anyhow!("未知子 Agent: {}", alias))
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
