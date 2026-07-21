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

    /// 是否需要确认（有副作用）。默认 false（只读工具）。
    /// 有副作用的工具（file_write, terminal, memory_write 等）应 override 返回 true。
    fn requires_confirm(&self) -> bool {
        false
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }
}
