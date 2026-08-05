use anyhow::Result;
use std::path::Path;

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
