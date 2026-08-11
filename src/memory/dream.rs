//! 做梦（Dream）的记忆文件辅助：备份 / 回滚 / diff。
//!
//! 设计（见 ADR-0016）：做梦最终只改 MEMORY.md，dream_draft.md 只是离线草稿。
//! 安全兜底 = 事前时间戳 .bak 备份 → 事后 diff 摘要推送 → 可 /dream-rollback 回滚。
//! 版本化用轻量手写 .bak，不引入 git（本地单用户够用）。

use anyhow::{Context, Result};
use regex::Regex;
use std::path::{Path, PathBuf};

/// 单条记忆条目的日期前缀，如 `- [2026-08-11] `。
/// 用于 diff 时按「条目正文」而非整行比对（日期不同但正文相同的条目不算新增）。
fn entry_body(line: &str) -> String {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^\s*-\s*\[\d{4}-\d{2}-\d{2}\]\s*").unwrap());
    re.replace(line, "").trim().to_string()
}

/// 把 MEMORY.md 内容拆成「条目正文」集合（跳过空行/标题/注释）。
fn entry_bodies(content: &str) -> Vec<String> {
    content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("<!--"))
        .map(entry_body)
        .filter(|b| !b.is_empty())
        .collect()
}

/// 备份 MEMORY.md 为带时间戳的 .bak，保留最近 `keep` 份（超出删最旧）。
/// 返回备份路径；备份前文件不存在时仍生成一个（空内容），便于首次回滚语义一致。
pub async fn backup_memory(memory_path: &Path, backup_dir: &Path, keep: usize) -> Result<PathBuf> {
    tokio::fs::create_dir_all(backup_dir)
        .await
        .with_context(|| format!("create backup dir {:?}", backup_dir))?;
    let ts = crate::time::now(&None)
        .naive
        .format("%Y%m%d-%H%M%S")
        .to_string();
    let backup_path = backup_dir.join(format!("MEMORY.{}.md.bak", ts));
    let content = tokio::fs::read_to_string(memory_path)
        .await
        .unwrap_or_default();
    tokio::fs::write(&backup_path, &content)
        .await
        .with_context(|| format!("write backup {:?}", backup_path))?;

    // 保留最近 keep 份
    let mut backups = list_backups(backup_dir).await;
    backups.sort();
    while backups.len() > keep {
        if let Some(oldest) = backups.first() {
            let _ = tokio::fs::remove_file(oldest).await;
            backups.remove(0);
        }
    }
    Ok(backup_path)
}

/// 列出所有 .bak 绝对路径（不含目录），按文件名升序（旧→新）。
pub async fn list_backups(backup_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut entries = match tokio::fs::read_dir(backup_dir).await {
        Ok(e) => e,
        Err(_) => return out,
    };
    while let Some(entry) = entries.next_entry().await.ok().flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("bak") {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// 回滚：用最近一份（或指定）.bak 覆盖 MEMORY.md。
/// `which` 为 None 时用最新一份；返回实际还原的备份路径。
pub async fn restore_memory(
    memory_path: &Path,
    backup_dir: &Path,
    which: Option<usize>,
) -> Result<PathBuf> {
    let mut backups = list_backups(backup_dir).await;
    backups.sort();
    if backups.is_empty() {
        anyhow::bail!("no MEMORY backup found, cannot rollback");
    }
    let idx = which.unwrap_or(backups.len() - 1).min(backups.len() - 1);
    let src = &backups[idx];
    let content = tokio::fs::read_to_string(src)
        .await
        .with_context(|| format!("read backup {:?}", src))?;
    tokio::fs::write(memory_path, &content)
        .await
        .with_context(|| format!("restore MEMORY {:?}", memory_path))?;
    Ok(src.clone())
}

/// 计算「备份(旧) vs 新内容」的 diff 摘要：新增 / 删除 / 共保留条目数。
pub fn diff_memory(old_content: &str, new_content: &str) -> String {
    let old = entry_bodies(old_content);
    let new = entry_bodies(new_content);
    let old_set: std::collections::HashSet<&String> = old.iter().collect();
    let new_set: std::collections::HashSet<&String> = new.iter().collect();

    let added: Vec<&String> = new.iter().filter(|b| !old_set.contains(b)).collect();
    let removed: Vec<&String> = old.iter().filter(|b| !new_set.contains(b)).collect();

    if added.is_empty() && removed.is_empty() {
        return format!("[dream] 记忆无变化（共 {} 条条目，已是最新）", new.len());
    }
    let mut s = String::from("[dream] 记忆整理完成：\n");
    s.push_str(&format!(
        "· 当前条目数：{}（整理前 {}）\n",
        new.len(),
        old.len()
    ));
    if !added.is_empty() {
        s.push_str(&format!("· 新增 {} 条：\n", added.len()));
        for a in added {
            s.push_str(&format!("  + {}\n", a));
        }
    }
    if !removed.is_empty() {
        s.push_str(&format!(
            "· 删除 {} 条（过期/重复/矛盾）：\n",
            removed.len()
        ));
        for r in removed {
            s.push_str(&format!("  - {}\n", r));
        }
    }
    s
}

/// dream_draft.md 路径（与 MEMORY.md 同目录，即 workspace 下）。
pub fn draft_path(workspace: &Path) -> PathBuf {
    workspace.join("dream_draft.md")
}

/// 写出 dream_draft.md（覆盖式）。
pub async fn write_draft(draft_path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = draft_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create draft dir {:?}", parent))?;
    }
    tokio::fs::write(draft_path, content)
        .await
        .with_context(|| format!("write draft {:?}", draft_path))
}

/// 读取 dream_draft.md（不存在返回空）。
pub async fn read_draft(draft_path: &Path) -> Result<String> {
    Ok(tokio::fs::read_to_string(draft_path)
        .await
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn test_entry_body_strips_date() {
        assert_eq!(entry_body("- [2026-08-11] likes rust"), "likes rust");
        assert_eq!(entry_body("  - [2026-01-01] foo bar "), "foo bar");
    }

    #[tokio::test]
    async fn test_backup_and_rollback() {
        let dir = tempdir().unwrap();
        let mem = dir.path().join("MEMORY.md");
        tokio::fs::write(&mem, "- [2026-08-11] original\n")
            .await
            .unwrap();
        let bak_dir = dir.path().join("backups");
        let b1 = backup_memory(&mem, &bak_dir, 3).await.unwrap();
        assert!(b1.exists());
        tokio::fs::write(&mem, "- [2026-08-12] changed\n")
            .await
            .unwrap();
        let restored = restore_memory(&mem, &bak_dir, None).await.unwrap();
        assert_eq!(restored, b1);
        let back = tokio::fs::read_to_string(&mem).await.unwrap();
        assert!(back.contains("original"));
    }

    #[test]
    fn test_diff_memory_added_and_removed() {
        let old = "- [2026-08-11] a\n- [2026-08-11] b\n";
        let new = "- [2026-08-11] a\n- [2026-08-12] c\n";
        let d = diff_memory(old, new);
        assert!(d.contains("删除 1 条"));
        assert!(d.contains("新增 1 条"));
        assert!(d.contains("+ c"));
        assert!(d.contains("- b"));
    }

    #[test]
    fn test_diff_memory_no_change() {
        let c = "- [2026-08-11] a\n";
        assert!(diff_memory(c, c).contains("无变化"));
    }

    #[test]
    fn test_draft_path() {
        let p = draft_path(Path::new("/x/workspace"));
        assert_eq!(p, Path::new("/x/workspace/dream_draft.md"));
    }
}
