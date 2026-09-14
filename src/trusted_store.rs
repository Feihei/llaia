//! 持久化受信目录存储（#B 持久化，2026-09-13）。
//!
//! /move 批准过的受信目录此前仅存内存（Agent 生命周期），serve 重启即清空，
//! 同一目录（如项目仓库）每次重启后都要重新弹审批。现持久化到
//! `<config_dir>/trusted_dirs.json`：启动时加载进共享 trusted_dirs Arc，
//! `Agent::add_trusted_dir` 时回写。
//!
//! agent 家目录 workspace 默认在列（每次启动重新种子，语义上等价于基线
//! 白名单的显式化）：/move 切走后，home 内操作依旧免审。
//!
//! 安全边界：文件位于 config_dir（agent 家目录之外）；目录内容在 /move 批准
//! 时已过 `validate_move_target`（存在性 + 危险黑名单）校验，此处不做重复
//! 校验——失效目录（已删除）在 `validate_path_in_scope` 的 canonicalize
//! 回溯下自然放行为其现存祖先的判定，与内存版行为一致。

use std::path::{Path, PathBuf};

/// 持久化文件路径：`<config_dir>/trusted_dirs.json`
fn trusted_file(config_dir: &Path) -> PathBuf {
    config_dir.join("trusted_dirs.json")
}

/// 从 config_dir 加载持久化受信目录。文件缺失或损坏时返回空集（行为同旧版）。
pub fn load(config_dir: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(trusted_file(config_dir)) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<String>>(&text) {
        Ok(paths) => paths.into_iter().map(PathBuf::from).collect(),
        Err(_) => Vec::new(),
    }
}

/// 把受信目录集合写入 config_dir。写失败静默降级（受信退化为会话级，不阻断流程）。
pub fn save(config_dir: &Path, dirs: &[PathBuf]) {
    let paths: Vec<String> = dirs
        .iter()
        .map(|d| d.to_string_lossy().into_owned())
        .collect();
    if let Ok(json) = serde_json::to_string_pretty(&paths) {
        if std::fs::create_dir_all(config_dir).is_ok() {
            let _ = std::fs::write(trusted_file(config_dir), json + "\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_missing_file_loads_empty() {
        let dir = tempdir().unwrap();
        assert!(load(dir.path()).is_empty());
    }

    #[test]
    fn test_save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let dirs = vec![
            PathBuf::from(r"C:\Users\me\proj"),
            PathBuf::from("/tmp/other"),
        ];
        save(dir.path(), &dirs);
        assert_eq!(load(dir.path()), dirs);
    }

    #[test]
    fn test_corrupt_file_loads_empty() {
        let dir = tempdir().unwrap();
        std::fs::write(trusted_file(dir.path()), "not json {{{").unwrap();
        assert!(load(dir.path()).is_empty());
    }
}
