pub mod markdown;
pub mod sqlite;
pub mod trim;

pub use markdown::{
    ensure_template, is_unfilled, load_md, MEMORY_TEMPLATE, SOUL_TEMPLATE, USER_TEMPLATE,
};

use anyhow::{Context, Result};
use std::path::Path;

/// 原子写 MEMORY.md：先写同目录 `.tmp`，再 rename 覆盖目标。
///
/// 就地 `fs::write` 若被进程中断会留下半截文件，而这份文件没有第二份拷贝可拼回来。
/// rename 在同目录内是原子替换（POSIX 与 Windows 语义一致），读者只会看到旧内容或新内容。
/// 顺带补齐尾部换行：缺尾换行会让 `memory_write` 的追加把新条目粘到最后一条上。
pub async fn write_memory_atomic(memory_path: &Path, content: &str) -> Result<()> {
    let tmp = memory_path.with_file_name(format!(
        "{}.tmp",
        memory_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("MEMORY.md")
    ));
    let body = if content.ends_with('\n') {
        content.to_string()
    } else {
        format!("{}\n", content)
    };
    tokio::fs::write(&tmp, &body)
        .await
        .with_context(|| format!("write MEMORY temp {:?}", tmp))?;
    if let Err(e) = tokio::fs::rename(&tmp, memory_path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e).with_context(|| format!("rename MEMORY {:?} -> {:?}", tmp, memory_path));
    }
    Ok(())
}
