use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "llaia",
    version = "0.1.0",
    about = "Lightweight Local AI Assistant"
)]
struct Cli {
    /// 配置目录，默认 ~/.llaia
    #[arg(long, global = true)]
    config_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 进入终端交互模式（默认）
    Chat,
    /// 启动后台服务（QQ 频道、未来 WebUI 等），不启动终端交互
    Serve,
    /// 初始化配置目录：生成目录骨架 + 默认模板
    Init {
        /// 覆盖已存在的文件
        #[arg(long)]
        force: bool,
    },
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
    let config_dir = cli.config_dir.unwrap_or_else(|| {
        dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("no home dir"))
            .expect("no home dir")
            .join(".llaia")
    });
    let command = cli.command.unwrap_or(Commands::Chat);
    match command {
        Commands::Chat => llaia::commands::chat_cmd(&config_dir).await,
        Commands::Serve => llaia::commands::serve_cmd(&config_dir).await,
        Commands::Init { force } => llaia::commands::init_cmd(&config_dir, force),
        Commands::Config => llaia::commands::config_cmd(&config_dir),
        Commands::Doctor => llaia::commands::doctor_cmd(&config_dir).await,
        Commands::Remember { text } => llaia::commands::remember_cmd(&text, &config_dir).await,
    }
}
