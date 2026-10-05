use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "llaia", version, about = "Lightweight Local AI Assistant")]
struct Cli {
    /// 状态目录（config.toml / .env / logs / workspace 的所在）。
    /// 优先级：--config-dir > 环境变量 LLAIA_HOME > 默认 ~/.llaia
    #[arg(long, global = true)]
    config_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 进入终端交互模式（需显式指定；无参数启动默认走 serve）
    Chat,
    /// 启动后台服务（WebUI + 已启用的 IM 频道），不启动终端交互
    Serve {
        /// 覆盖 [webui].host：只影响本次监听的地址，不回写 config.toml
        #[arg(long)]
        host: Option<String>,
        /// 覆盖 [webui].port：只影响本次监听的端口，不回写 config.toml
        #[arg(long)]
        port: Option<u16>,
        /// 服务就绪后用系统浏览器打开 WebUI（无参数启动时默认开启）
        #[arg(long)]
        open: bool,
    },
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

/// state dir（`config.toml` / `.env` / `logs/` / `workspace/` 的父目录）解析优先级：
/// `--config-dir` > `LLAIA_HOME` > `~/.llaia`。
///
/// 提纯成函数有两个理由：进程级环境变量只在 `main` 读一次便于单测；以及**空字符串
/// 视为未设置**——`LLAIA_HOME=` 在 CI 里比缺变量更常见，照收会把状态目录变成相对路径
/// `""`，等于悄悄写进当前工作目录。返回 `None` 表示三条路都走不通，由调用方报错，
/// 不再沿用 `expect("no home dir")`（违反本仓库「生产路径不 expect」的约定）。
fn resolve_state_dir(
    cli: Option<PathBuf>,
    env_home: Option<std::ffi::OsString>,
    user_home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(dir) = cli {
        return Some(dir);
    }
    if let Some(home) = env_home.filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    user_home.map(|h| h.join(".llaia"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_dir = resolve_state_dir(
        cli.config_dir,
        std::env::var_os("LLAIA_HOME"),
        dirs::home_dir(),
    )
    .ok_or_else(|| {
        anyhow::anyhow!("cannot locate state dir: no home directory found — set LLAIA_HOME or pass --config-dir")
    })?;

    // 加载 .env：先 CWD（标准 dotenv 位置），再 config_dir/.env
    // 两者都不存在时静默跳过；config.toml 中的 ${VAR} 引用依赖此处先加载
    let _ = dotenvy::dotenv();
    let env_path = config_dir.join(".env");
    if env_path.exists() {
        let _ = dotenvy::from_path(&env_path);
    }

    // 无参数启动（Windows 双击 exe 同效）默认 serve + 自动打开 WebUI；
    // 终端 REPL 改为显式 `llaia chat`。
    let bare = cli.command.is_none();
    let command = cli.command.unwrap_or(Commands::Serve {
        host: None,
        port: None,
        open: true,
    });

    let result: Result<()> = match command {
        Commands::Chat => llaia::commands::chat_cmd(&config_dir).await,
        Commands::Serve { host, port, open } => {
            llaia::commands::serve_cmd(
                &config_dir,
                llaia::commands::WebBindOverride { host, port },
                open,
            )
            .await
        }
        Commands::Init { force } => llaia::commands::init_cmd(&config_dir, force),
        Commands::Config => llaia::commands::config_cmd(&config_dir),
        Commands::Doctor => llaia::commands::doctor_cmd(&config_dir).await,
        Commands::Remember { text } => llaia::commands::remember_cmd(&text, &config_dir).await,
    };

    // 双击启动的控制台窗口随进程退出立即关闭，报错会一闪而过——无参数路径
    // 失败时自行打印并停住窗口等一次回车，让人来得及读错误（随后返回 Ok，
    // 避免 tokio::main 再打一遍 "Error:"）。
    match result {
        Err(e) if bare => {
            eprintln!("\nError: {e:#}");
            println!("\nPress Enter to exit...");
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            Ok(())
        }
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_state_dir;
    use std::path::PathBuf;

    #[test]
    fn flag_beats_env_and_home() {
        let got = resolve_state_dir(
            Some(PathBuf::from("/by/flag")),
            Some("E:/by/env".into()),
            Some(PathBuf::from("/Users/x")),
        );
        assert_eq!(got, Some(PathBuf::from("/by/flag")));
    }

    #[test]
    fn env_beats_default_home() {
        let got = resolve_state_dir(
            None,
            Some("E:/by/env".into()),
            Some(PathBuf::from("/Users/x")),
        );
        assert_eq!(got, Some(PathBuf::from("E:/by/env")));
    }

    #[test]
    fn falls_back_to_dot_llaia_under_home() {
        let got = resolve_state_dir(None, None, Some(PathBuf::from("/Users/x")));
        assert_eq!(got, Some(PathBuf::from("/Users/x/.llaia")));
    }

    /// `LLAIA_HOME=`（空串）在 CI 与 shell 误操作中比"没设"更常见；照收会把状态目录
    /// 变成相对路径 ""，等于把 config.toml / sessions.db 悄悄写进当前工作目录。
    #[test]
    fn empty_env_is_treated_as_unset() {
        let got = resolve_state_dir(None, Some("".into()), Some(PathBuf::from("/Users/x")));
        assert_eq!(got, Some(PathBuf::from("/Users/x/.llaia")));
    }

    /// 三条路都不通时返回 None 让调用方报错——原实现是 `expect("no home dir")`。
    #[test]
    fn unresolvable_returns_none_instead_of_panicking() {
        assert_eq!(resolve_state_dir(None, None, None), None);
    }
}
