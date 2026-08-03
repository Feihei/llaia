use anyhow::Result;
use std::path::PathBuf;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// 初始化 tracing：输出到 stderr + 文件。
pub fn init(level: &str, log_dir: &PathBuf) -> Result<()> {
    std::fs::create_dir_all(log_dir)?;
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("llaia.log"))?;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

    let stderr_layer = fmt::layer().with_writer(std::io::stderr);
    let file_layer = fmt::layer()
        .with_writer(std::sync::Mutex::new(log_file))
        .with_ansi(false);

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();
    Ok(())
}
