use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "laia", version = "0.1.0", about = "Lightweight AI Assistant")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 进入交互式对话（默认）
    Chat,
    /// 打印当前配置
    Config,
    /// 诊断 provider 连通性、文件完整性
    Doctor,
    /// 写一条记忆
    Remember { text: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Commands::Chat);
    match command {
        Commands::Chat => laia::commands::chat_cmd().await,
        Commands::Config => laia::commands::config_cmd(),
        Commands::Doctor => laia::commands::doctor_cmd().await,
        Commands::Remember { text } => laia::commands::remember_cmd(&text).await,
    }
}
