//! workspace 快照基建（ADR-0033 L1 可恢复性，P9 Phase 1）。
//!
//! A0（SOUL/USER/MEMORY 及实例私有 MEMORY）/ A2（sessions.db、uploads/）写前留底：
//! 篡改可回滚，为全部闸门的放行宽松度供底气。快照存储在 `<config_dir>/snapshots/`，
//! 位于 agent 家目录之外，且 file/terminal 工具对它硬拒（`[snapshot-guard]`），
//! agent 即使被批准/委派绕过作用域也改不到（防「先改本体再改快照」洗白）。
//!
//! 形态：时间戳镜像目录 + hash 去重——`snapshots/<YYYYMMDD-HHMMSS>/<key>`，
//! key 为相对 agent 家目录的 `/` 分隔路径。内容未变（sha1 命中 `.state.json`）
//! 不落新拷贝；sessions.db 经 `VACUUM INTO` 产出一致副本（裸拷 WAL 活库可能损坏），
//! 且至多每 24h 一次。已知留档：A→B→A 的中间态若无任何快照触发点经过则不留底，
//! sweep 节奏（10 分钟）给出损失上界；快照失败 log warn 不阻断主操作。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha1::Digest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 单文件快照体积上限：uploads 的病态大文件防御（QQ 分片上传上限同为 200MB）。
const MAX_FILE_BYTES: u64 = 200 * 1024 * 1024;
/// 兜底 sweep 间隔：A0 被 terminal 等未挂钩路径改写时，留底损失上界。
const SWEEP_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// sessions.db 一致副本的最小间隔：消息每次落库 mtime 都变，按 sweep 频率拷贝会爆盘。
const DB_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(24 * 3600);
const GC_INTERVAL: Duration = Duration::from_secs(24 * 3600);

#[derive(Serialize, Deserialize, Default)]
struct State {
    /// key -> 最近一次留底内容的 sha1 hex（去重依据）
    #[serde(default)]
    files: HashMap<String, String>,
    /// 上次 sessions.db 快照的 unix 秒（24h 节流）
    #[serde(default)]
    last_db: i64,
    /// 上次 GC 的 unix 秒（24h 节流）
    #[serde(default)]
    last_gc: i64,
}

pub struct SnapshotStore {
    /// `<config_dir>/snapshots/`
    root: PathBuf,
    retention_days: u64,
    state: Mutex<State>,
}

/// 工具侧快照上下文（仅 main agent Some）：写前留底 + 快照库只读守卫。
/// 由 build_single_agent 构造，注入 FileWrite / FileEdit / MemoryWrite / Terminal
/// 与 `Agent.snapshot`（slash 命令、ApprovalContext 复用同一 Arc）。
#[derive(Clone)]
pub struct SnapshotCtx {
    pub store: Arc<SnapshotStore>,
    /// agent 家目录：A0 判定基准（目标在其下且文件名为 SOUL/USER/MEMORY 时写前留底）
    pub home: PathBuf,
    /// 快照库根，写入目标命中即拒（`[snapshot-guard]`）
    pub root: PathBuf,
}

impl SnapshotCtx {
    /// 目标落在家目录 A0 人格文件（SOUL.md / USER.md / MEMORY.md，含实例与子
    /// agent 的 MEMORY）上时返回快照 key（相对家目录的 `/` 分隔路径）。
    pub fn persona_write_target(&self, resolved: &Path) -> Option<String> {
        let rel = resolved.strip_prefix(&self.home).ok()?;
        let name = rel.file_name()?.to_str()?;
        if !matches!(name, "SOUL.md" | "USER.md" | "MEMORY.md") {
            return None;
        }
        Some(rel.to_string_lossy().replace('\\', "/"))
    }

    /// 快照库只读守卫：目标（审批解析或范围校验后的规范化路径）命中快照库根。
    pub fn blocks_write(&self, resolved: &Path) -> bool {
        resolved.starts_with(&self.root)
    }

    /// A0 写前留底：目标是家目录人格文件时快照旧内容。失败仅 log warn
    /// 不阻断主操作（快照与目标同盘，快照失败时写入大概率也失败）。
    pub async fn snapshot_target(&self, resolved: &Path, reason: &str) {
        if let Some(key) = self.persona_write_target(resolved) {
            if let Err(e) = self.store.snapshot_file(resolved, &key, reason).await {
                tracing::warn!(error = %e, key, reason, "pre-write snapshot failed");
            }
        }
    }
}

impl SnapshotStore {
    pub fn open(config_dir: &Path, retention_days: u64) -> Result<Self> {
        let root = config_dir.join("snapshots");
        std::fs::create_dir_all(&root)
            .with_context(|| format!("create snapshot dir {:?}", root))?;
        let state = std::fs::read(root.join(".state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<State>(&b).ok())
            .unwrap_or_default();
        Ok(Self {
            root,
            retention_days,
            state: Mutex::new(state),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 留底单个文件（key 为相对家目录路径）。文件不存在 → Ok(false)；内容未变 → Ok(false)。
    /// 快照失败不应阻断主操作，调用方（工具/挂钩）对 Err 只 log warn。
    pub async fn snapshot_file(&self, abs: &Path, key: &str, reason: &str) -> Result<bool> {
        let meta = match tokio::fs::metadata(abs).await {
            Ok(m) if m.is_file() => m,
            Ok(_) => return Ok(false),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e).with_context(|| format!("stat {:?}", abs)),
        };
        if meta.len() > MAX_FILE_BYTES {
            tracing::warn!(key, size = meta.len(), "snapshot skipped: file too large");
            return Ok(false);
        }
        let bytes = tokio::fs::read(abs)
            .await
            .with_context(|| format!("read {:?}", abs))?;
        let hash = sha1_hex(&bytes);

        let changed = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.files.get(key).map(|h| h.as_str()) != Some(hash.as_str())
        };
        if !changed {
            return Ok(false);
        }

        let dir = self.timestamp_dir();
        let dest = dir.join(key);
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create snapshot parent {:?}", parent))?;
        }
        // 先写文件再更新状态：写失败时状态不前进，下次重试照常落底
        tokio::fs::write(&dest, &bytes)
            .await
            .with_context(|| format!("write snapshot {:?}", dest))?;
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.files.insert(key.to_string(), hash);
            self.persist_state(&state)?;
        }
        tracing::info!(key, reason, dir = %dir.display(), "snapshot saved");
        Ok(true)
    }

    /// sessions.db 一致副本（VACUUM INTO），24h 节流。dest 由本方法落在新的时刻目录。
    pub async fn snapshot_session_db(
        &self,
        store: &Arc<crate::memory::sqlite::SessionStore>,
    ) -> Result<bool> {
        let now = crate::time::unix_now();
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if now - state.last_db < DB_SNAPSHOT_INTERVAL.as_secs() as i64 {
                return Ok(false);
            }
        }
        let dest = self.timestamp_dir().join("sessions.db");
        let store = store.clone();
        let dest_for_log = dest.clone();
        tokio::task::spawn_blocking(move || store.snapshot_into(&dest))
            .await
            .with_context(|| "snapshot task join")?
            .with_context(|| format!("VACUUM INTO {:?}", dest_for_log))?;
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.last_db = now;
            self.persist_state(&state)?;
        }
        tracing::info!(dest = %dest_for_log.display(), "sessions.db snapshot saved");
        Ok(true)
    }

    /// 删除超过保留窗口的时刻目录。返回删除数。
    pub async fn gc(&self) -> Result<usize> {
        let cutoff = crate::time::now(&None).naive.date()
            - chrono::Duration::days(self.retention_days as i64);
        let mut removed = 0usize;
        let mut rd = tokio::fs::read_dir(&self.root)
            .await
            .with_context(|| format!("read snapshot dir {:?}", self.root))?;
        while let Some(entry) = rd.next_entry().await? {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(date) = parse_snapshot_dir_date(name) else {
                continue;
            };
            if date < cutoff && tokio::fs::remove_dir_all(entry.path()).await.is_ok() {
                removed += 1;
            }
        }
        let now = crate::time::unix_now();
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.last_gc = now;
            self.persist_state(&state)?;
        }
        if removed > 0 {
            tracing::info!(removed, retention_days = self.retention_days, "snapshot gc");
        }
        Ok(removed)
    }

    /// 某个 key 最近一次留底的路径（供恢复与测试）。
    pub async fn latest(&self, key: &str) -> Result<Option<PathBuf>> {
        let mut dirs: Vec<String> = Vec::new();
        let mut rd = tokio::fs::read_dir(&self.root).await?;
        while let Some(entry) = rd.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    if parse_snapshot_dir_date(name).is_some() {
                        dirs.push(name.to_string());
                    }
                }
            }
        }
        dirs.sort();
        for name in dirs.into_iter().rev() {
            let candidate = self.root.join(&name).join(key);
            if tokio::fs::metadata(&candidate).await.is_ok() {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    fn timestamp_dir(&self) -> PathBuf {
        let ts = crate::time::now(&None)
            .naive
            .format("%Y%m%d-%H%M%S-%3f")
            .to_string();
        self.root.join(ts)
    }

    fn persist_state(&self, state: &State) -> Result<()> {
        let path = self.root.join(".state.json");
        let tmp = self.root.join(".state.json.tmp");
        let body = serde_json::to_vec(state).context("serialize snapshot state")?;
        std::fs::write(&tmp, body).with_context(|| format!("write {:?}", tmp))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("rename {:?}", path))?;
        Ok(())
    }

    /// 启动周期任务：兜底 sweep（A0 + uploads + sessions.db 24h 节奏）+ GC。
    /// 持 Weak——reload 重建 registry 后旧 store/SessionStore 随之释放，任务自动退出。
    pub fn spawn_periodic(
        store: &std::sync::Arc<Self>,
        session_store: &std::sync::Arc<crate::memory::sqlite::SessionStore>,
        home: PathBuf,
    ) {
        let weak = std::sync::Arc::downgrade(store);
        let weak_ss = std::sync::Arc::downgrade(session_store);
        tokio::spawn(async move {
            let mut sweep = tokio::time::interval(SWEEP_INTERVAL);
            let mut gc = tokio::time::interval(GC_INTERVAL);
            sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            gc.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // 首个 tick 立即到期：启动即做一轮 GC（内容未变的 sweep 会被去重跳过，无成本）
            loop {
                let (Some(store), Some(ss)) = (weak.upgrade(), weak_ss.upgrade()) else {
                    return;
                };
                tokio::select! {
                    _ = sweep.tick() => {
                        let items = default_sweep_items(&home);
                        let mut ok = 0;
                        for (path, key) in items {
                            if store.snapshot_file(&path, &key, "sweep").await.unwrap_or(false) {
                                ok += 1;
                            }
                        }
                        if let Err(e) = store.snapshot_session_db(&ss).await {
                            tracing::warn!(error = %e, "sessions.db snapshot failed");
                        }
                        if ok > 0 {
                            tracing::debug!(stored = ok, "periodic snapshot sweep");
                        }
                    }
                    _ = gc.tick() => {
                        if let Err(e) = store.gc().await {
                            tracing::warn!(error = %e, "snapshot gc failed");
                        }
                    }
                }
            }
        });
    }
}

/// 兜底 sweep 的默认清单：家目录根的 SOUL/USER/MEMORY、实例与子 agent 的 MEMORY、
/// uploads/ 全部文件。key = 相对 home 的 `/` 分隔路径（快照目录内保持同构）。
pub fn default_sweep_items(home: &Path) -> Vec<(PathBuf, String)> {
    let mut items = Vec::new();
    for name in ["SOUL.md", "USER.md", "MEMORY.md"] {
        let p = home.join(name);
        if p.is_file() {
            items.push((p, name.to_string()));
        }
    }
    for sub in ["instances", "subagent"] {
        let base = home.join(sub);
        let Ok(rd) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in rd.flatten() {
            let mem = entry.path().join("MEMORY.md");
            if mem.is_file() {
                let name = entry.file_name().to_string_lossy().into_owned();
                items.push((mem, format!("{sub}/{name}/MEMORY.md")));
            }
        }
    }
    collect_files(&home.join("uploads"), "uploads", 0, &mut items);
    items
}

/// 递归收集目录下文件（限深 4，防符号链接环与异常嵌套）。
fn collect_files(dir: &Path, key_prefix: &str, depth: usize, out: &mut Vec<(PathBuf, String)>) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let key = format!("{key_prefix}/{}", entry.file_name().to_string_lossy());
        if path.is_file() {
            out.push((path, key));
        } else if path.is_dir() {
            collect_files(&path, &key, depth + 1, out);
        }
    }
}

fn parse_snapshot_dir_date(name: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(name.get(0..8)?, "%Y%m%d").ok()
}

fn sha1_hex(bytes: &[u8]) -> String {
    let mut h = sha1::Sha1::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn store(dir: &Path) -> SnapshotStore {
        SnapshotStore::open(dir, 14).unwrap()
    }

    #[tokio::test]
    async fn test_snapshot_file_stores_and_dedups() {
        let dir = tempdir().unwrap();
        let s = store(dir.path());
        let src = dir.path().join("MEMORY.md");
        std::fs::write(&src, "v1").unwrap();

        assert!(s.snapshot_file(&src, "MEMORY.md", "test").await.unwrap());
        let first = s.latest("MEMORY.md").await.unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "v1");

        // 内容未变：去重，不落新拷贝
        assert!(!s.snapshot_file(&src, "MEMORY.md", "test").await.unwrap());

        // 内容变化：新拷贝
        std::fs::write(&src, "v2").unwrap();
        assert!(s.snapshot_file(&src, "MEMORY.md", "test").await.unwrap());
        let second = s.latest("MEMORY.md").await.unwrap().unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "v2");

        // 状态跨实例重建（模拟重启）仍去重
        let s2 = store(dir.path());
        assert!(!s2.snapshot_file(&src, "MEMORY.md", "test").await.unwrap());
    }

    #[tokio::test]
    async fn test_snapshot_missing_file_is_noop() {
        let dir = tempdir().unwrap();
        let s = store(dir.path());
        let missing = dir.path().join("nope.md");
        assert!(!s.snapshot_file(&missing, "nope.md", "test").await.unwrap());
        assert!(s.latest("nope.md").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_snapshot_nested_key() {
        let dir = tempdir().unwrap();
        let s = store(dir.path());
        let src = dir.path().join("MEMORY.md");
        std::fs::write(&src, "inst").unwrap();
        assert!(s
            .snapshot_file(&src, "instances/work/MEMORY.md", "test")
            .await
            .unwrap());
        let latest = s.latest("instances/work/MEMORY.md").await.unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(&latest).unwrap(), "inst");
    }

    #[tokio::test]
    async fn test_gc_removes_expired_dirs_only() {
        let dir = tempdir().unwrap();
        let s = store(dir.path());
        // 造一个 30 天前的时刻目录（超出默认 14 天窗口）
        let old = crate::time::now(&None).naive - chrono::Duration::days(30);
        let old_dir = dir
            .path()
            .join("snapshots")
            .join(old.format("%Y%m%d-%H%M%S").to_string());
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::write(old_dir.join("MEMORY.md"), "old").unwrap();
        // 一个今天的新目录
        let new_dir = dir.path().join("snapshots").join(
            crate::time::now(&None)
                .naive
                .format("%Y%m%d-%H%M%S")
                .to_string(),
        );
        std::fs::create_dir_all(&new_dir).unwrap();
        std::fs::write(new_dir.join("MEMORY.md"), "new").unwrap();

        assert_eq!(s.gc().await.unwrap(), 1);
        assert!(!old_dir.exists());
        assert!(new_dir.exists());
    }

    #[tokio::test]
    async fn test_default_sweep_items_shape() {
        let home = tempdir().unwrap();
        std::fs::write(home.path().join("SOUL.md"), "s").unwrap();
        std::fs::create_dir_all(home.path().join("instances/work")).unwrap();
        std::fs::write(home.path().join("instances/work/MEMORY.md"), "m").unwrap();
        std::fs::create_dir_all(home.path().join("uploads")).unwrap();
        std::fs::write(home.path().join("uploads/1_pic.png"), "p").unwrap();

        let items = default_sweep_items(home.path());
        let keys: Vec<&str> = items.iter().map(|(_, k)| k.as_str()).collect();
        assert!(keys.contains(&"SOUL.md"));
        assert!(keys.contains(&"instances/work/MEMORY.md"));
        assert!(keys.contains(&"uploads/1_pic.png"));
        // 不存在的不在列
        assert!(!keys.contains(&"USER.md"));
    }
}
