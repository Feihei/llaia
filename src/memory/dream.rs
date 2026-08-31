//! 做梦（Dream）的记忆文件辅助：备份 / 回滚 / diff。
//!
//! 设计（见 ADR-0016）：做梦最终只改 MEMORY.md，dream_draft.md 只是离线草稿。
//! 安全兜底 = 写盘前形状校验（`validate_memory_candidate`，不合规直接拒绝写入）→ 事前
//! 时间戳 .bak 备份 → 事后 diff 摘要推送 → 可 /dream-rollback 回滚。
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

/// stage2 产物写盘前的形状校验。MEMORY.md 的契约是「`# MEMORY` 标题 + 注释 + 若干
/// `- [YYYY-MM-DD] <fact>` 条目」，而 stage2 是让 LLM **重写整份文件**——记忆条目里
/// 本身就写着「幽默、讽刺、自嘲、不对就直说、称呼 Boss」这类人格指令，小模型读完极易
/// 入戏，回一段反问用户的散文。非空不等于合法，故设三重门：
/// 1. 首个非空行必须是 `# MEMORY` 标题（散文第一行就不是，直接挡掉）；
/// 2. 每个内容行都必须是 `- [YYYY-MM-DD] <fact>`，且日期在合理范围；
/// 3. 条目数不得低于旧文件的 60%（挡住「整份塌成一条」这种静默丢记忆）。
///
/// `Err(String)` 为人类可读的拒绝理由，由调用方推送给用户。
pub fn validate_memory_candidate(old: &str, new: &str) -> Result<(), String> {
    static ENTRY: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = ENTRY.get_or_init(|| {
        Regex::new(r"^-\s*\[(\d{4})-(\d{2})-(\d{2})\]\s+\S.*$").expect("static entry regex")
    });

    if new.trim().is_empty() {
        return Err("consolidated memory is empty".to_string());
    }

    let first_line = new.lines().map(str::trim).find(|l| !l.is_empty());
    match first_line {
        Some(l) if l.starts_with("# MEMORY") => {}
        Some(l) => {
            return Err(format!(
                "missing `# MEMORY` title line (output starts with {:?})",
                l.chars().take(60).collect::<String>()
            ))
        }
        None => return Err("consolidated memory has no content".to_string()),
    }

    let mut entries = 0usize;
    for (idx, line) in new.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with("<!--") {
            continue;
        }
        let Some(caps) = re.captures(l) else {
            return Err(format!(
                "line {} is not a `- [YYYY-MM-DD] <fact>` entry: {:?}",
                idx + 1,
                l.chars().take(60).collect::<String>()
            ));
        };
        let year = caps[1].parse::<i32>().unwrap_or(0);
        let month = caps[2].parse::<u32>().unwrap_or(0);
        let day = caps[3].parse::<u32>().unwrap_or(0);
        if !(2000..=2100).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day)
        {
            return Err(format!(
                "line {} has an out-of-range date [{}-{}-{}]",
                idx + 1,
                year,
                month,
                day
            ));
        }
        entries += 1;
    }

    let old_entries = entry_bodies(old).len();
    let floor = old_entries * 6 / 10;
    if entries < floor {
        return Err(format!(
            "entry count collapsed: {} → {} (must keep at least {} entries)",
            old_entries, entries, floor
        ));
    }
    Ok(())
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

/// 原子写 MEMORY.md：先写同目录 `.tmp`，再 rename 覆盖目标。
///
/// 两处写盘（dream 写回、回滚还原）都是**整份覆盖**，就地 `fs::write` 若被进程中断会
/// 留下半截文件，而这份文件没有第二份拷贝可拼回来。rename 在同目录内是原子替换
/// （POSIX 与 Windows 语义一致），读者只会看到旧内容或新内容。
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
    write_memory_atomic(memory_path, &content)
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
        return format!(
            "[dream] no memory changes ({} entries, already up to date)",
            new.len()
        );
    }
    let mut s = String::from("[dream] memory consolidated:\n");
    s.push_str(&format!(
        "  Current entries: {} (before: {})\n",
        new.len(),
        old.len()
    ));
    if !added.is_empty() {
        s.push_str(&format!("  Added {} entries:\n", added.len()));
        for a in added {
            s.push_str(&format!("  + {}\n", a));
        }
    }
    if !removed.is_empty() {
        s.push_str(&format!(
            "  Removed {} entries (stale/duplicate/contradicted):\n",
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
        assert!(d.contains("Removed 1 entries"));
        assert!(d.contains("Added 1 entries"));
        assert!(d.contains("+ c"));
        assert!(d.contains("- b"));
    }

    #[test]
    fn test_diff_memory_no_change() {
        let c = "- [2026-08-11] a\n";
        assert!(diff_memory(c, c).contains("no memory changes"));
    }

    #[test]
    fn test_draft_path() {
        let p = draft_path(Path::new("/x/workspace"));
        assert_eq!(p, Path::new("/x/workspace/dream_draft.md"));
    }

    /// 旧文件：3 条合法条目（校验的 60% 下限据此算出 = 1）。
    const OLD_THREE: &str = "# MEMORY\n\n<!-- 格式：- [YYYY-MM-DD] <条目> -->\n\
        - [2026-08-10] 用户要求每天早上 8:00 推送 AI 简讯\n\
        - [2026-08-28] 用户希望改 skill 时用 skill_edit 工具\n\
        - [2026-08-30] 用户要求中文回复、称呼其为 Boss\n";

    #[test]
    fn test_validate_rejects_persona_prose_regression() {
        // 2026-08-31 真实事故：模型读了记忆里的人格条目（"自嘲、不对就直说、称呼 Boss"）
        // 后入戏，把 stage2 当成跟 Boss 说话，回了一段反问用户「morning_news 到底是 7:30
        // 还是 8:00」的散文。它既没有 ``` 围栏也没有 `# MEMORY` 标题，非空于是直接过闸
        // 写盘，MEMORY.md 就此变成散文。缺标题这条规则必须把它挡下。
        let prose = "Boss，早（说\"早\"其实快午睡了 😏）。你要的 consolidation 我干了。\n\
             \n\
             那条 `morning_news` 细节没往 MEMORY 里塞——原因很实在：\n\
             \n\
             - 旧条目 `[2026-08-10]` 已经记了\"每天 **8:00** 推\"，新草稿说 cron 是 `0 30 7 * * *`（**7:30**）。时间对不上。\n\
             \n\
             所以问你一句：**morning_news 到底是 7:30 还是 8:00 跑？**";
        let err = validate_memory_candidate(OLD_THREE, prose).unwrap_err();
        assert!(err.contains("missing `# MEMORY` title"), "got: {}", err);
    }

    #[test]
    fn test_validate_rejects_prose_with_title_kept() {
        // 更隐蔽的一档：模型保住了 `# MEMORY` 标题，但正文夹带对话散文——靠逐行形状挡下
        let new = "# MEMORY\n\n- [2026-08-30] 用户要求中文回复\n\n我干完了，有问题再喊我。\n";
        let err = validate_memory_candidate(OLD_THREE, new).unwrap_err();
        assert!(
            err.contains("not a `- [YYYY-MM-DD] <fact>` entry"),
            "got: {}",
            err
        );
    }

    #[test]
    fn test_validate_rejects_prose_without_title() {
        let err = validate_memory_candidate(OLD_THREE, "我干完了，Boss 觉得如何？").unwrap_err();
        assert!(err.contains("missing `# MEMORY` title"), "got: {}", err);
    }

    #[test]
    fn test_validate_accepts_normal_rewrite() {
        let new = "# MEMORY\n\n<!-- 格式 -->\n\
            - [2026-08-30] 用户要求每天早上 7:30 推送 AI 简讯\n\
            - [2026-08-28] 用户希望改 skill 时用 skill_edit 工具\n";
        assert!(validate_memory_candidate(OLD_THREE, new).is_ok());
    }

    #[test]
    fn test_validate_rejects_entry_count_collapse() {
        let mut old = String::from("# MEMORY\n\n<!-- 格式 -->\n");
        for i in 1..=10 {
            old.push_str(&format!("- [2026-08-{:02}] fact {}\n", i, i));
        }
        // 10 条 → 1 条，低于 60% 下限（6 条）
        let new = "# MEMORY\n\n- [2026-08-20] only one left\n";
        let err = validate_memory_candidate(&old, new).unwrap_err();
        assert!(err.contains("entry count collapsed"), "got: {}", err);
    }

    #[test]
    fn test_validate_allows_first_write_on_empty_memory() {
        // 旧文件为空/散文（尚无条目）时不设条目数下限，只要形状合法就放行
        assert!(validate_memory_candidate("", "# MEMORY\n\n- [2026-08-31] first fact\n").is_ok());
        assert!(validate_memory_candidate(
            "not a memory file at all",
            "# MEMORY\n\n- [2026-08-31] first fact\n"
        )
        .is_ok());
    }

    #[test]
    fn test_validate_rejects_unstripped_fence() {
        // 围栏没剥净时，```markdown 占了首行 → 由「缺标题」这条先挡下（比逐行检查更早）。
        // 断言只关心「被拒 + 理由点名了 offending 行」，不绑定哪条规则命中。
        let err = validate_memory_candidate(
            OLD_THREE,
            "```markdown\n# MEMORY\n\n- [2026-08-30] a\n- [2026-08-28] b\n```",
        )
        .unwrap_err();
        assert!(err.contains("```markdown"), "got: {}", err);
    }

    #[test]
    fn test_validate_rejects_bad_date() {
        let err = validate_memory_candidate(OLD_THREE, "# MEMORY\n\n- [2026-13-45] impossible\n")
            .unwrap_err();
        assert!(err.contains("out-of-range date"), "got: {}", err);
    }

    #[test]
    fn test_validate_rejects_empty() {
        assert!(validate_memory_candidate(OLD_THREE, "   \n  ").is_err());
    }
}
