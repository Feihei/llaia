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
