pub mod cli;
pub mod qq;

// 重新导出，方便外部使用
pub use cli::CliChannel;
pub use qq::QqChannel;

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

/// 抽象一个用户接入通道（CLI / QQ / 未来邮箱、web 等）。
/// 每个实现负责自己的 I/O 循环（读用户输入、写回复），
/// 共享同一个 AgentRegistry（main + sub_agents，通过 Arc<Mutex> 串行化访问）。
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    /// 启动 channel，阻塞运行直到退出。
    async fn run(self: Arc<Self>, registry: Arc<crate::agent::AgentRegistry>) -> Result<()>;
}
