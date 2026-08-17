//! 长期目标（/goal）持久化与注入。
//!
//! 采用文件方案（ADR-0021 修订 2026-08-17）：目标存于 `<config_dir>/workspace/goal.md`，
//! 与 SOUL/USER/MEMORY 同处 agent 家目录（对 file_write 等工具不可见）。不进
//! `sessions` schema，零迁移、省 token（每轮从文件重新注入，不进会话历史）。
use anyhow::{Context as _, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const GOAL_FILE_NAME: &str = "goal.md";

/// 目标生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalStatus {
    Active,
    Done,
    Cancelled,
}

impl GoalStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            GoalStatus::Active => "active",
            GoalStatus::Done => "done",
            GoalStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "active" => Some(GoalStatus::Active),
            "done" => Some(GoalStatus::Done),
            "cancelled" | "canceled" => Some(GoalStatus::Cancelled),
            _ => None,
        }
    }
}

/// 解析后的目标状态。objective / progress 为文件正文拆出，不进 frontmatter。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalState {
    pub status: GoalStatus,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(skip)]
    pub objective: String,
    #[serde(skip)]
    pub progress: String,
}

/// 目标文件绝对路径：`<workspace>/goal.md`（workspace = agent 家目录）。
pub fn goal_path(workspace: &Path) -> PathBuf {
    workspace.join(GOAL_FILE_NAME)
}

/// 读取并解析目标；文件不存在或无法解析时返回 None（视为无目标）。
pub fn read_goal(workspace: &Path) -> Option<GoalState> {
    let path = goal_path(workspace);
    let content = std::fs::read_to_string(&path).ok()?;
    parse_goal(&content)
}

/// 仅当存在且处于 active 状态时返回 Runtime Context 注入文本。
pub fn read_active_goal_line(workspace: &Path) -> Option<String> {
    let state = read_goal(workspace)?;
    goal_runtime_lines(&state)
}

/// 解析 goal.md 文本（frontmatter + 正文）。
/// frontmatter 必须存在且 status 可识别，否则返回 None。
pub fn parse_goal(content: &str) -> Option<GoalState> {
    let (yaml, body) = split_frontmatter(content)?;
    let fm: serde_yaml::Value = serde_yaml::from_str(&yaml).ok()?;
    let status_str = fm
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("active");
    let status = GoalStatus::parse(status_str)?; // 无效 status → 整文件视为无目标
    let created_at = fm
        .get("created_at")
        .and_then(|v| v.as_str())
        .map(String::from);
    let updated_at = fm
        .get("updated_at")
        .and_then(|v| v.as_str())
        .map(String::from);
    let (objective, progress) = split_objective_progress(&body);
    Some(GoalState {
        status,
        created_at,
        updated_at,
        objective,
        progress,
    })
}

/// 生成 Runtime Context 注入行（仅 active 时调用）。
pub fn goal_runtime_lines(state: &GoalState) -> Option<String> {
    if state.status != GoalStatus::Active {
        return None;
    }
    let objective = if state.objective.trim().is_empty() {
        "(no objective set)"
    } else {
        state.objective.trim()
    };
    let progress = if state.progress.trim().is_empty() {
        "(no progress logged yet)"
    } else {
        state.progress.trim()
    };
    Some(format!(
        "Goal (active): {} / Summary: {}",
        objective, progress
    ))
}

/// 设定（或重置）目标：覆盖式写新文件，置 status=active。
pub fn set_goal(workspace: &Path, objective: &str) -> Result<PathBuf> {
    let now = now_iso();
    let state = GoalState {
        status: GoalStatus::Active,
        created_at: Some(now.clone()),
        updated_at: Some(now),
        objective: objective.trim().to_string(),
        progress: String::new(),
    };
    let path = goal_path(workspace);
    write_atomic(&path, &render(&state))?;
    Ok(path)
}

/// 切换状态：done / cancelled。无现有目标时报错。
pub fn update_status(workspace: &Path, status: GoalStatus) -> Result<()> {
    let mut state = read_goal(workspace)
        .ok_or_else(|| anyhow::anyhow!("no active goal to update; set one with /goal first"))?;
    state.status = status;
    let path = goal_path(workspace);
    write_atomic(&path, &render(&state))?;
    Ok(())
}

/// 更新进度笔记（覆盖 ## Progress 段）。
pub fn update_progress(workspace: &Path, progress: &str) -> Result<()> {
    let mut state = read_goal(workspace)
        .ok_or_else(|| anyhow::anyhow!("no active goal to update; set one with /goal first"))?;
    state.progress = progress.trim().to_string();
    let path = goal_path(workspace);
    write_atomic(&path, &render(&state))?;
    Ok(())
}

// ───────────────────────── 内部辅助 ─────────────────────────

/// 拆分 YAML frontmatter 与正文。返回 (yaml_text, body_text)。
/// 允许 BOM 开头；需以 `---` 行起、再以 `---` 行收。
fn split_frontmatter(content: &str) -> Option<(String, String)> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let rest = match content.strip_prefix("---\r\n") {
        Some(r) => r,
        None => content.strip_prefix("---\n")?,
    };
    let mut yaml_lines = Vec::new();
    let mut body_lines = Vec::new();
    let mut in_yaml = true;
    for line in rest.lines() {
        if in_yaml {
            if line.trim() == "---" {
                in_yaml = false;
                continue;
            }
            yaml_lines.push(line);
        } else {
            body_lines.push(line);
        }
    }
    if in_yaml {
        return None; // 没找到收尾的 ---
    }
    Some((yaml_lines.join("\n"), body_lines.join("\n")))
}

/// 把正文拆成 (objective, progress)：以首个 `## Progress` 标题为界。
fn split_objective_progress(body: &str) -> (String, String) {
    let mut obj_lines = Vec::new();
    let mut prog_lines = Vec::new();
    let mut in_progress = false;
    for line in body.lines() {
        if !in_progress
            && line
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("## progress")
        {
            in_progress = true;
            continue;
        }
        if in_progress {
            prog_lines.push(line);
        } else {
            obj_lines.push(line);
        }
    }
    let objective = clean_objective(&obj_lines.join("\n"));
    let progress = prog_lines.join("\n").trim().to_string();
    (objective, progress)
}

fn clean_objective(s: &str) -> String {
    let mut lines: Vec<&str> = s.lines().collect();
    while let Some(&first) = lines.first() {
        if first.trim().is_empty() {
            lines.remove(0);
        } else {
            break;
        }
    }
    while let Some(&last) = lines.last() {
        if last.trim().is_empty() {
            lines.pop();
        } else {
            break;
        }
    }
    if let Some(&first) = lines.first() {
        if first
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("# goal")
        {
            lines.remove(0);
        }
    }
    lines.join("\n").trim().to_string()
}

fn render(state: &GoalState) -> String {
    let now = now_iso();
    let mut s = String::new();
    s.push_str("---\n");
    s.push_str(&format!("status: {}\n", state.status.as_str()));
    let created = state.created_at.clone().unwrap_or_else(|| now.clone());
    s.push_str(&format!("created_at: {}\n", created));
    s.push_str(&format!("updated_at: {}\n", now));
    s.push_str("---\n\n");
    s.push_str("# Goal\n");
    if !state.objective.trim().is_empty() {
        s.push_str(state.objective.trim());
        s.push('\n');
    }
    s.push_str("\n## Progress\n");
    if !state.progress.trim().is_empty() {
        s.push_str(state.progress.trim());
        s.push('\n');
    }
    s
}

fn write_atomic(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, content).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample() -> String {
        "---\nstatus: active\ncreated_at: 2026-08-17T11:00:00+08:00\nupdated_at: 2026-08-17T11:00:00+08:00\n---\n\n# Goal\n交付 P5-7\n\n## Progress\n- P5-1~P5-6 已完成\n".into()
    }

    #[test]
    fn parse_active_goal() {
        let s = parse_goal(&sample()).unwrap();
        assert_eq!(s.status, GoalStatus::Active);
        assert_eq!(s.objective, "交付 P5-7");
        assert!(s.progress.contains("P5-1~P5-6 已完成"));
    }

    #[test]
    fn parse_missing_file_is_none() {
        let dir = tempdir().unwrap();
        assert!(read_goal(dir.path()).is_none());
    }

    #[test]
    fn done_status_not_injected() {
        let content = "---\nstatus: done\n---\n\n# Goal\nobj\n";
        let s = parse_goal(content).unwrap();
        assert_eq!(s.status, GoalStatus::Done);
        assert!(goal_runtime_lines(&s).is_none());
    }

    #[test]
    fn invalid_status_means_no_goal() {
        let content = "---\nstatus: banana\n---\n\n# Goal\nobj\n";
        assert!(parse_goal(content).is_none());
    }

    #[test]
    fn set_overwrites() {
        let dir = tempdir().unwrap();
        let p1 = set_goal(dir.path(), "first").unwrap();
        assert!(p1.exists());
        let p2 = set_goal(dir.path(), "second").unwrap();
        assert_eq!(p2, p1);
        let s = read_goal(dir.path()).unwrap();
        assert_eq!(s.objective, "second");
        assert_eq!(s.status, GoalStatus::Active);
    }

    #[test]
    fn update_progress_and_status() {
        let dir = tempdir().unwrap();
        set_goal(dir.path(), "ship it").unwrap();
        update_progress(dir.path(), "half done").unwrap();
        let s = read_goal(dir.path()).unwrap();
        assert_eq!(s.progress, "half done");
        update_status(dir.path(), GoalStatus::Done).unwrap();
        let s = read_goal(dir.path()).unwrap();
        assert_eq!(s.status, GoalStatus::Done);
    }

    #[test]
    fn update_status_without_goal_errors() {
        let dir = tempdir().unwrap();
        assert!(update_status(dir.path(), GoalStatus::Done).is_err());
    }

    #[test]
    fn roundtrip_render_parse() {
        let dir = tempdir().unwrap();
        set_goal(dir.path(), "objective line").unwrap();
        update_progress(dir.path(), "some progress").unwrap();
        let s = read_goal(dir.path()).unwrap();
        let line = goal_runtime_lines(&s).unwrap();
        assert!(line.contains("objective line"));
        assert!(line.contains("some progress"));
    }
}
