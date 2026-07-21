use crate::provider::ToolSpec;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

pub mod file;
pub mod memory;
pub mod tavily;
pub mod terminal;
pub mod web;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;

    async fn execute(&self, args: &Value) -> Result<String>;

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }
}
