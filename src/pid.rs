use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use sysinfo::{Pid, ProcessRefreshKind, System};

/// PID 文件管理：用于检测是否有另一个 laia 实例正在运行。
///
/// 行为：
/// - `acquire`：检查现有 PID 文件，若对应进程还活着则警告（不阻止启动）。
///   然后写入当前 PID。
/// - `release`：删除 PID 文件。进程退出时调用。
///
/// 注意：不阻止重复启动——用户可能上次崩溃没清理，或确实想同时跑 chat + serve。
/// 仅警告，让用户自己判断。
pub struct PidFile {
    path: PathBuf,
}

impl PidFile {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            path: config_dir.join("laia.pid"),
        }
    }

    /// 检查并写入 PID 文件。若已有实例在运行，记录警告。
    pub fn acquire(&self) -> Result<()> {
        if let Some(existing_pid) = self.read_pid()? {
            if self.is_process_alive(existing_pid) {
                tracing::warn!(
                    pid = existing_pid,
                    pid_file = %self.path.display(),
                    "another laia instance may be running; proceeding anyway"
                );
            } else {
                tracing::debug!(
                    pid = existing_pid,
                    "stale pid file found, process not alive, overwriting"
                );
            }
        }
        let current_pid = std::process::id();
        fs::write(&self.path, current_pid.to_string())?;
        tracing::debug!(pid = current_pid, pid_file = %self.path.display(), "pid file acquired");
        Ok(())
    }

    /// 删除 PID 文件。进程退出时调用。
    pub fn release(&self) {
        if let Err(e) = fs::remove_file(&self.path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(error = %e, "failed to remove pid file");
            }
        }
    }

    fn read_pid(&self) -> Result<Option<u32>> {
        match fs::read_to_string(&self.path) {
            Ok(content) => match content.trim().parse::<u32>() {
                Ok(pid) => Ok(Some(pid)),
                Err(_) => {
                    tracing::warn!(pid_file = %self.path.display(), "pid file content invalid, ignoring");
                    Ok(None)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(anyhow::anyhow!("read pid file {:?}: {}", self.path, e)),
        }
    }

    fn is_process_alive(&self, pid: u32) -> bool {
        let mut sys = System::new();
        // 只刷新目标进程，避免全量扫描
        sys.refresh_process_specifics(Pid::from_u32(pid), ProcessRefreshKind::new());
        sys.process(Pid::from_u32(pid)).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_acquire_writes_current_pid() {
        let dir = tempdir().unwrap();
        let pid_file = PidFile::new(dir.path());
        pid_file.acquire().unwrap();

        let content = fs::read_to_string(dir.path().join("laia.pid")).unwrap();
        let stored: u32 = content.trim().parse().unwrap();
        assert_eq!(stored, std::process::id());
    }

    #[test]
    fn test_release_removes_pid_file() {
        let dir = tempdir().unwrap();
        let pid_file = PidFile::new(dir.path());
        pid_file.acquire().unwrap();
        assert!(dir.path().join("laia.pid").exists());

        pid_file.release();
        assert!(!dir.path().join("laia.pid").exists());
    }

    #[test]
    fn test_release_without_acquire_is_noop() {
        let dir = tempdir().unwrap();
        let pid_file = PidFile::new(dir.path());
        // release 未 acquire 的 pid 文件不报错
        pid_file.release();
        assert!(!dir.path().join("laia.pid").exists());
    }

    #[test]
    fn test_acquire_overwrites_stale_pid() {
        let dir = tempdir().unwrap();
        let pid_file = PidFile::new(dir.path());

        // 写一个几乎肯定不存在的 PID
        fs::write(dir.path().join("laia.pid"), "99999999").unwrap();

        pid_file.acquire().unwrap();
        let content = fs::read_to_string(dir.path().join("laia.pid")).unwrap();
        let stored: u32 = content.trim().parse().unwrap();
        assert_eq!(stored, std::process::id());
    }

    #[test]
    fn test_is_process_alive_for_current() {
        let dir = tempdir().unwrap();
        let pid_file = PidFile::new(dir.path());
        // 当前进程肯定活着
        assert!(pid_file.is_process_alive(std::process::id()));
    }

    #[test]
    fn test_is_process_alive_for_dead_pid() {
        let dir = tempdir().unwrap();
        let pid_file = PidFile::new(dir.path());
        // PID 99999999 几乎肯定不存在
        assert!(!pid_file.is_process_alive(99999999));
    }
}
