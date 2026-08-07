use crate::agent::TurnEvent;
use crate::provider::ToolSpec;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

pub mod cron;
pub mod delegate;
pub mod file;
pub mod memory;
pub mod send_media;
pub mod tavily;
pub mod terminal;
pub mod web;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;

    async fn execute(&self, args: &Value, channel: &str) -> Result<String>;

    /// 带事件转发的 execute：默认实现忽略 event_tx，直接调用 execute。
    /// 需要向 channel 转发进度的工具（如 delegate）override 此方法。
    async fn execute_with_events(
        &self,
        args: &Value,
        channel: &str,
        event_tx: Option<&mpsc::Sender<TurnEvent>>,
    ) -> Result<String> {
        let _ = event_tx;
        self.execute(args, channel).await
    }

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
