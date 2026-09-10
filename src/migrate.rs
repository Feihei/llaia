use anyhow::Result;
use std::path::Path;

use crate::memory::{SOUL_TEMPLATE, SOUL_TEMPLATE_LEGACY, USER_TEMPLATE, USER_TEMPLATE_LEGACY};

/// 画像模板升级（v0.5.0：SOUL 加 `# Name`、USER 的 `language` 改为留空）。
///
/// 为什么改文件而不是让 `is_unfilled` 认多个版本：模板常量是"用户从未填写"的**指纹**，
/// 文案一改，存量仍是旧占位符的文件就被判成已填写——first-run bootstrap 从此哑火，
/// tail reminder 门禁（两份画像都仍是模板才不生成）反向失效，会为
/// `<Describe LLAIA's personality>` 白烧一个隔离 turn。
///
/// 只有**逐字节等于旧模板**的文件才被覆写，用户填过哪怕一行都不命中比较。返回是否有改动。
pub fn refresh_placeholder_templates(config_dir: &Path) -> Result<bool> {
    let workspace = config_dir.join("workspace");
    let mut dirs = vec![workspace];
    let subagents = config_dir.join("workspace").join("subagent");
    if subagents.is_dir() {
        for entry in std::fs::read_dir(&subagents)? {
            let dir = entry?.path();
            if dir.is_dir() {
                dirs.push(dir);
            }
        }
    }

    let mut changed = false;
    for dir in dirs {
        changed |= refresh_one(&dir.join("SOUL.md"), SOUL_TEMPLATE_LEGACY, SOUL_TEMPLATE)?;
        changed |= refresh_one(&dir.join("USER.md"), USER_TEMPLATE_LEGACY, USER_TEMPLATE)?;
    }
    Ok(changed)
}

/// 单个画像文件：内容（忽略首尾空白）仍是 `legacy` 原文时覆写为 `current`。
fn refresh_one(path: &Path, legacy: &str, current: &str) -> Result<bool> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Ok(false);
    };
    if content.trim() != legacy.trim() {
        return Ok(false);
    }
    std::fs::write(path, current)?;
    tracing::info!(file = ?path.file_name(), "profile placeholder upgraded to current template");
    Ok(true)
}

/// 检测并执行 v0.1 → v0.2 目录结构迁移
///
/// 旧结构：~/.llaia/ 下直接放 SOUL.md / USER.md / MEMORY.md / sessions.db / uploads/
/// 新结构：这些文件移到 ~/.llaia/workspace/ 下
///
/// 返回 true 表示执行了迁移，false 表示无需迁移
pub fn migrate_if_needed(config_dir: &Path) -> Result<bool> {
    let marker = config_dir.join(".migrated_v0.2");
    if marker.exists() {
        return Ok(false);
    }

    let workspace = config_dir.join("workspace");
    let old_soul = config_dir.join("SOUL.md");
    let old_user = config_dir.join("USER.md");
    let old_memory = config_dir.join("MEMORY.md");
    let old_sessions = config_dir.join("sessions.db");
    let old_uploads = config_dir.join("uploads");
    let old_subagents = config_dir.join("subagents");

    // 检测是否有旧结构文件
    let has_old = old_soul.exists()
        || old_user.exists()
        || old_memory.exists()
        || old_sessions.exists()
        || old_uploads.exists()
        || old_subagents.exists();

    if !has_old {
        // 无旧文件，直接写标记
        std::fs::write(&marker, "")?;
        return Ok(false);
    }

    tracing::info!("detected old directory structure, migrating to v0.2 workspace layout");

    // 创建 workspace/
    std::fs::create_dir_all(&workspace)?;

    // 移动文件
    move_if_exists(&old_soul, &workspace.join("SOUL.md"))?;
    move_if_exists(&old_user, &workspace.join("USER.md"))?;
    move_if_exists(&old_memory, &workspace.join("MEMORY.md"))?;
    move_if_exists(&old_sessions, &workspace.join("sessions.db"))?;
    move_dir_if_exists(&old_uploads, &workspace.join("uploads"))?;

    // 移动旧子 agent 目录：~/.llaia/subagents/<name>/ → ~/.llaia/workspace/subagent/<name>/
    if old_subagents.exists() {
        let new_subagent = workspace.join("subagent");
        std::fs::create_dir_all(&new_subagent)?;
        for entry in std::fs::read_dir(&old_subagents)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let name = entry.file_name();
                let src = entry.path();
                let dst = new_subagent.join(&name);
                if !dst.exists() {
                    std::fs::rename(&src, &dst)?;
                    tracing::info!(agent = ?name, "migrated subagent directory");
                }
            }
        }
        // 移动完后删除空 subagents 目录
        std::fs::remove_dir(&old_subagents).ok();
    }

    // 备份 config.toml
    let config_path = config_dir.join("config.toml");
    if config_path.exists() {
        let bak = config_dir.join("config.toml.bak");
        std::fs::copy(&config_path, &bak)?;
        tracing::info!("backed up config.toml to config.toml.bak");
    }

    // 写迁移标记
    std::fs::write(&marker, "")?;
    tracing::info!("migration to v0.2 complete");
    Ok(true)
}

fn move_if_exists(src: &Path, dst: &Path) -> Result<()> {
    if src.exists() {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(src, dst)?;
        tracing::info!(file = ?src.file_name(), "migrated file");
    }
    Ok(())
}

fn move_dir_if_exists(src: &Path, dst: &Path) -> Result<()> {
    if src.exists() && src.is_dir() {
        if dst.exists() {
            // dst 已存在：合并目录（移动子项）
            for entry in std::fs::read_dir(src)? {
                let entry = entry?;
                let name = entry.file_name();
                let src_item = entry.path();
                let dst_item = dst.join(&name);
                if !dst_item.exists() {
                    std::fs::rename(&src_item, &dst_item)?;
                }
            }
            std::fs::remove_dir(src).ok();
        } else {
            std::fs::rename(src, dst)?;
        }
        tracing::info!(dir = ?src.file_name(), "migrated directory");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_no_migration_needed() {
        let dir = tempdir().unwrap();
        // 空 config_dir，无旧文件
        let migrated = migrate_if_needed(dir.path()).unwrap();
        assert!(!migrated);
        // 标记文件存在
        assert!(dir.path().join(".migrated_v0.2").exists());
    }

    #[test]
    fn test_already_migrated() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".migrated_v0.2"), "").unwrap();
        let migrated = migrate_if_needed(dir.path()).unwrap();
        assert!(!migrated);
    }

    /// 纯占位符画像（含 subagent 目录）升级到当前模板，且幂等；
    /// 空文件不改写（`is_unfilled` 已把空判为待填，bootstrap 照常引导）。
    #[test]
    fn test_refresh_upgrades_pure_placeholders() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("workspace");
        let sub = ws.join("subagent").join("coder");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(ws.join("SOUL.md"), SOUL_TEMPLATE_LEGACY).unwrap();
        std::fs::write(sub.join("SOUL.md"), SOUL_TEMPLATE_LEGACY).unwrap();
        std::fs::write(ws.join("USER.md"), "  \n").unwrap();

        assert!(refresh_placeholder_templates(dir.path()).unwrap());
        assert_eq!(
            std::fs::read_to_string(ws.join("SOUL.md")).unwrap(),
            SOUL_TEMPLATE
        );
        assert_eq!(
            std::fs::read_to_string(sub.join("SOUL.md")).unwrap(),
            SOUL_TEMPLATE
        );
        assert_eq!(std::fs::read_to_string(ws.join("USER.md")).unwrap(), "  \n");
        // 幂等：已是当前模板，再跑无改动
        assert!(!refresh_placeholder_templates(dir.path()).unwrap());
    }

    /// 指纹根因：旧模板文本改常量后不再命中 is_unfilled（会被判"已填写"），
    /// 所以必须先升级文件；而用户填过哪怕一行的画像绝不能被改写。
    #[test]
    fn test_refresh_leaves_filled_profiles_alone() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("workspace");
        std::fs::create_dir_all(&ws).unwrap();
        let edited = format!("{}\n- name: feihei\n", USER_TEMPLATE_LEGACY);
        std::fs::write(ws.join("USER.md"), &edited).unwrap();

        assert!(!refresh_placeholder_templates(dir.path()).unwrap());
        assert_eq!(std::fs::read_to_string(ws.join("USER.md")).unwrap(), edited);
        assert!(crate::memory::is_unfilled(SOUL_TEMPLATE, SOUL_TEMPLATE));
        assert!(
            !crate::memory::is_unfilled(SOUL_TEMPLATE_LEGACY, SOUL_TEMPLATE),
            "旧模板文本必须不再命中新指纹，否则升级步骤失去依据"
        );
    }

    #[test]
    fn test_migrate_old_structure() {
        let dir = tempdir().unwrap();
        // 模拟旧结构
        std::fs::write(dir.path().join("SOUL.md"), "soul").unwrap();
        std::fs::write(dir.path().join("USER.md"), "user").unwrap();
        std::fs::write(dir.path().join("MEMORY.md"), "memory").unwrap();
        std::fs::write(dir.path().join("config.toml"), "[test]").unwrap();
        std::fs::create_dir(dir.path().join("uploads")).unwrap();
        std::fs::write(dir.path().join("uploads/img.jpg"), "img").unwrap();

        let migrated = migrate_if_needed(dir.path()).unwrap();
        assert!(migrated);

        // 验证文件移动到 workspace/
        let ws = dir.path().join("workspace");
        assert!(ws.join("SOUL.md").exists());
        assert!(ws.join("USER.md").exists());
        assert!(ws.join("MEMORY.md").exists());
        assert!(ws.join("uploads/img.jpg").exists());

        // 旧位置不存在
        assert!(!dir.path().join("SOUL.md").exists());

        // 标记存在
        assert!(dir.path().join(".migrated_v0.2").exists());

        // config 备份存在
        assert!(dir.path().join("config.toml.bak").exists());
    }

    #[test]
    fn test_migrate_old_subagents() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("SOUL.md"), "soul").unwrap();
        // 旧子 agent 目录
        let old_sub = dir.path().join("subagents").join("coder");
        std::fs::create_dir_all(&old_sub).unwrap();
        std::fs::write(old_sub.join("SOUL.md"), "coder soul").unwrap();

        let migrated = migrate_if_needed(dir.path()).unwrap();
        assert!(migrated);

        // 验证子 agent 目录移动
        let new_sub = dir.path().join("workspace").join("subagent").join("coder");
        assert!(new_sub.exists());
        assert!(new_sub.join("SOUL.md").exists());
    }
}
