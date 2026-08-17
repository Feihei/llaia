//! Skill 加载器：扫描 `<skills_dir>/<name>/SKILL.md`、解析 frontmatter、
//! 管理 skills.json active 开关、种子内置示例 skill。

use crate::skill::{is_valid_skill_name, SkillManifest};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// skills.json 相对 skills_dir 的位置：`<config_dir>/skills.json`
/// （skills_dir = `<config_dir>/skills`，with_file_name 平移到父目录）
pub fn skills_json_path(skills_dir: &Path) -> PathBuf {
    skills_dir.with_file_name("skills.json")
}

// ───────────────────────── frontmatter 解析 ─────────────────────────

/// SKILL.md frontmatter 字段（全部可选，缺失时用回退值）
#[derive(Debug, Clone, Deserialize)]
pub struct Frontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub duration: Option<String>,
    #[serde(default, deserialize_with = "deserialize_tools")]
    pub tools: Vec<String>,
}

/// tools 字段宽松解析：接受 YAML 列表或逗号分隔字符串
fn deserialize_tools<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ToolsField {
        List(Vec<String>),
        Single(String),
    }
    match Option::<ToolsField>::deserialize(deserializer)? {
        None => Ok(Vec::new()),
        Some(ToolsField::List(v)) => Ok(v),
        Some(ToolsField::Single(s)) => Ok(s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect()),
    }
}

/// 拆分 SKILL.md：返回 (frontmatter YAML, body)。
/// frontmatter 必须以 `---` 独占一行开头，以下一个 `---` 独占行结束。
pub fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut lines = content.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim() != "---" {
        return None;
    }
    let rest = content[first.len()..].to_string();
    // 找结束的 --- 行
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim() == "---" {
            let yaml = &content[first.len()..first.len() + offset];
            let body = &content[first.len() + offset + line.len()..];
            return Some((yaml, body));
        }
        offset += line.len();
    }
    None
}

/// 解析 frontmatter YAML。解析失败返回 Err（调用方决定回退策略）。
pub fn parse_frontmatter(yaml: &str) -> Result<Frontmatter> {
    serde_yaml::from_str(yaml).map_err(|e| anyhow!("parse frontmatter: {}", e))
}

/// 校验 SKILL.md 内容可用作 skill 定义：frontmatter 存在且可解析，name/description 非空。
/// WebUI 保存 content 前调用。
pub fn validate_skill_md(content: &str) -> Result<()> {
    let (yaml, _body) = split_frontmatter(content)
        .ok_or_else(|| anyhow!("SKILL.md 缺少 YAML frontmatter（--- 包裹的头部元数据）"))?;
    let fm = parse_frontmatter(yaml)?;
    if fm
        .name
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        anyhow::bail!("frontmatter 缺少 name 字段");
    }
    if fm
        .description
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        anyhow::bail!("frontmatter 缺少 description 字段");
    }
    Ok(())
}

// ───────────────────────── skills.json ─────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
struct SkillsJson {
    #[serde(default)]
    skills: HashMap<String, SkillEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SkillEntry {
    active: bool,
}

fn load_skills_json(path: &Path) -> SkillsJson {
    match std::fs::read_to_string(path) {
        Ok(s) => match serde_json::from_str(&s) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "skills.json corrupted, treating all skills as active");
                SkillsJson::default()
            }
        },
        Err(_) => SkillsJson::default(),
    }
}

fn save_skills_json(path: &Path, json: &SkillsJson) -> Result<()> {
    let s = serde_json::to_string_pretty(json)?;
    std::fs::write(path, s).map_err(|e| anyhow!("write {}: {}", path.display(), e))
}

/// 设置某个 skill 的 active 状态（写 skills.json）
pub fn set_active(skills_dir: &Path, name: &str, active: bool) -> Result<()> {
    if !is_valid_skill_name(name) {
        anyhow::bail!("invalid skill name: {}", name);
    }
    let path = skills_json_path(skills_dir);
    let mut json = load_skills_json(&path);
    json.skills.insert(name.to_string(), SkillEntry { active });
    save_skills_json(&path, &json)
}

/// 从 skills.json 删除条目（删除 skill 目录时一并清理）
pub fn remove_entry(skills_dir: &Path, name: &str) -> Result<()> {
    let path = skills_json_path(skills_dir);
    let mut json = load_skills_json(&path);
    if json.skills.remove(name).is_some() {
        save_skills_json(&path, &json)?;
    }
    Ok(())
}

// ───────────────────────── 扫描与种子 ─────────────────────────

/// 解析单个 SKILL.md 为 manifest。frontmatter 缺失/损坏时用目录名回退（description 留空）。
fn parse_skill_md(path: &Path, dir_name: &str) -> SkillManifest {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = split_frontmatter(&content).and_then(|(yaml, _)| parse_frontmatter(yaml).ok());
    let fm = parsed.unwrap_or(Frontmatter {
        name: None,
        description: None,
        duration: None,
        tools: Vec::new(),
    });
    // name：frontmatter 优先；非法或缺失时回退目录名
    let name = fm
        .name
        .filter(|n| is_valid_skill_name(n.trim()))
        .map(|n| n.trim().to_string())
        .unwrap_or_else(|| dir_name.to_string());
    let duration = fm.duration.unwrap_or_else(|| "turn".to_string());
    SkillManifest {
        name,
        description: fm.description.unwrap_or_default(),
        duration,
        tools: fm.tools,
        path: path.to_path_buf(),
        active: true, // 由调用方按 skills.json 覆盖
    }
}

/// 扫描 skills 目录（不种子示例）：目录不存在返回空列表。
/// 目录名非法（不满足 name 校验）的 skill 跳过，防 prompt injection。
pub fn scan_skills(skills_dir: &Path) -> Vec<SkillManifest> {
    let entries = match std::fs::read_dir(skills_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let active_map = load_skills_json(&skills_json_path(skills_dir)).skills;
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        if !is_valid_skill_name(&dir_name) {
            tracing::warn!(dir = %dir_name, "skip skill dir with invalid name");
            continue;
        }
        let skill_md = entry.path().join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let mut manifest = parse_skill_md(&skill_md, &dir_name);
        manifest.active = active_map.get(&dir_name).map(|e| e.active).unwrap_or(true);
        out.push(manifest);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 扫描 + 种子：skills 目录不存在时创建并写入内置示例 skill（on-demand，init 不生成）。
/// 扫描到的新 skill（skills.json 无记录）默认 active=true 并自动写回。
pub fn load_skills(skills_dir: &Path) -> Vec<SkillManifest> {
    if !skills_dir.exists() {
        if let Err(e) = seed_examples(skills_dir) {
            tracing::warn!(error = %e, dir = %skills_dir.display(), "seed example skills failed");
        }
    }
    let skills = scan_skills(skills_dir);
    // 新 skill 写回 skills.json（默认 active=true）
    let json_path = skills_json_path(skills_dir);
    let mut json = load_skills_json(&json_path);
    let mut changed = false;
    for s in &skills {
        // active 状态按目录名存取（目录名是磁盘上的唯一标识）
        let dir_name = s
            .path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| s.name.clone());
        if let std::collections::hash_map::Entry::Vacant(e) = json.skills.entry(dir_name) {
            e.insert(SkillEntry { active: true });
            changed = true;
        }
    }
    if changed {
        if let Err(e) = save_skills_json(&json_path, &json) {
            tracing::warn!(error = %e, "save skills.json failed");
        }
    }
    skills
}

// ───────────────────────── file_read 特殊放行 ─────────────────────────

/// 尝试把用户传入的路径解析为 skills 目录内的 SKILL.md（file_read 特殊放行规则）。
/// 命中返回可读路径；未命中返回 None（调用方继续走 workspace 校验）。
///
/// 规则（ADR-0015）：`~` 展开 + 相对路径按 workspace 解析 + 词法规范化后，
/// canonicalize 落在 skills_dir 内且文件名恰为 `SKILL.md` → 放行。
pub fn resolve_skill_path(skills_dir: &Path, workspace: &Path, raw: &str) -> Option<PathBuf> {
    let expanded = shellexpand::tilde(raw).into_owned();
    let p = PathBuf::from(&expanded);
    let joined = if p.is_absolute() {
        p
    } else {
        workspace.join(&expanded)
    };
    let normalized = crate::path_guard::normalize_lexical(&joined);
    // 必须真实存在才能读；canonicalize 同时消解符号链接
    let canon = match std::fs::canonicalize(&normalized) {
        Ok(c) => crate::path_guard::strip_verbatim_prefix(&c),
        Err(_) => return None,
    };
    let canon_skills = match std::fs::canonicalize(skills_dir) {
        Ok(c) => crate::path_guard::strip_verbatim_prefix(&c),
        Err(_) => return None,
    };
    if !canon.starts_with(&canon_skills) {
        return None;
    }
    if canon.file_name().map(|n| n.to_string_lossy()) != Some("SKILL.md".into()) {
        return None;
    }
    Some(normalized)
}

// ───────────────────────── built-in example skills ─────────────────────────

/// Default template for a new skill (used as the default content when creating via WebUI)
pub fn default_skill_template(name: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: TODO: what this skill does and when to use it\nduration: turn\n---\n\n# {name}\n\n## Workflow\n1. \n\n## Output Format\n\n## Notes\n"
    )
}

/// Built-in example skills: (directory name, SKILL.md content)
pub fn example_skills() -> &'static [(&'static str, &'static str)] {
    &[
        ("code-review", EXAMPLE_CODE_REVIEW),
        ("news-digest", EXAMPLE_NEWS_DIGEST),
        ("todoist", EXAMPLE_TODOIST),
    ]
}

/// Seed built-in examples: create the skills dir and write example SKILL.md files
fn seed_examples(skills_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(skills_dir)?;
    for (name, content) in example_skills() {
        let dir = skills_dir.join(name);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("SKILL.md"), content)?;
    }
    tracing::info!(dir = %skills_dir.display(), "seeded example skills (code-review / news-digest / todoist)");
    Ok(())
}

const EXAMPLE_CODE_REVIEW: &str = r#"---
name: code-review
description: Review code changes in a Git repo and give structured review comments. Use when the user asks for a code review or to review changes.
duration: turn
tools: ["file_read", "terminal"]
---

# Code Review

## Workflow
1. Run `git status` and `git diff HEAD` via terminal to see uncommitted changes (use the user-specified commit range if given)
2. Analyze changes file by file, focusing on:
   - Logic bugs and edge cases
   - Error handling and resource leaks
   - Security issues (injection, path traversal, sensitive info leakage)
   - Readability problems that clearly violate project conventions
3. Use file_read on suspicious spots to confirm context and avoid false positives

## Output Format
Group by severity:
- 🔴 Blocker: bugs / security issues that must be fixed
- 🟡 Suggestion: recommended improvements
- 🟢 Nitpick: minor style issues

Each comment points to the file and approximate location with a fix suggestion. If nothing is found, say so explicitly.

## Notes
- Review only; don't modify code unless explicitly asked
- If the workspace is not a Git repo, tell the user and ask for a file list
"#;

const EXAMPLE_NEWS_DIGEST: &str = r#"---
name: news-digest
description: Search and summarize today's hot news / tech briefs. Use when the user asks "what's the news today" or "any recent highlights".
duration: turn
tools: ["search", "web_fetch"]
---

# News Digest

## Workflow
1. Use search for hot topics in the user's area of interest (default AI/tech if unspecified)
2. Open the key articles with web_fetch to verify details (2-3 is enough; don't over-fetch)
3. Summarize into 3-5 briefs

## Output Format
Each brief:
- **Title** (one-line summary)
- Key points (2-3 bullets)
- Source link

End with one sentence summing up the day's overall takeaways. Be concise; avoid long verbatim translations.

## Notes
- If search is unavailable (no api_key) → explain and ask the user to configure or provide news URLs
- Note the information freshness ("as of X date"); never fabricate news
"#;

const EXAMPLE_TODOIST: &str = r#"---
name: todoist
description: Set reminders and to-do tasks for the user (via cron schedules). Use when the user says "remind me...", "add a to-do", or "do X every day/week".
duration: turn
tools: ["cron_task", "memory_write"]
---

# Todoist Reminders

## Workflow
1. Parse from the user message: reminder content, time (one-shot moment or recurring rule)
2. Create a scheduled task with the cron_task tool:
   - One-shot reminder → mode = "agent", prompt = "Remind the user: X"
   - Recurring reminder → write the matching cron expression schedule (5 fields: min hour day month weekday)
3. Record this reminder into MEMORY.md with memory_write for later lookup/cancellation
4. Confirm with the user: task id, trigger time, reminder content

## Notes
- If the time is vague (e.g. "tomorrow morning"), confirm with the user first, or take a reasonable default (8:00 AM) and say so
- cron_task is only available in serve mode; on tool failure, explain that `llaia serve` must be running
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_skill(dir: &Path, name: &str, content: &str) {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn test_split_frontmatter() {
        let content = "---\nname: x\ndescription: d\n---\n\n# body\n";
        let (yaml, body) = split_frontmatter(content).unwrap();
        assert!(yaml.contains("name: x"));
        assert!(body.contains("# body"));

        assert!(split_frontmatter("no frontmatter").is_none());
        assert!(split_frontmatter("---\nname: x\n").is_none()); // 未闭合
    }

    #[test]
    fn test_parse_frontmatter_tools_list_and_string() {
        let fm = parse_frontmatter("name: a\ntools: [\"x\", \"y\"]").unwrap();
        assert_eq!(fm.tools, vec!["x", "y"]);
        let fm = parse_frontmatter("name: a\ntools: \"x, y\"").unwrap();
        assert_eq!(fm.tools, vec!["x", "y"]);
        let fm = parse_frontmatter("name: a").unwrap();
        assert!(fm.tools.is_empty());
    }

    #[test]
    fn test_validate_skill_md() {
        assert!(validate_skill_md("---\nname: a\ndescription: d\n---\nbody").is_ok());
        assert!(validate_skill_md("---\ndescription: d\n---\n").is_err()); // 缺 name
        assert!(validate_skill_md("---\nname: a\n---\n").is_err()); // 缺 description
        assert!(validate_skill_md("plain markdown").is_err()); // 无 frontmatter
    }

    #[test]
    fn test_scan_skills_basic() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("skills");
        write_skill(
            &dir,
            "demo",
            "---\nname: demo\ndescription: 测试 skill\nduration: session\n---\nbody",
        );
        // 无 SKILL.md 的目录跳过
        std::fs::create_dir_all(dir.join("empty")).unwrap();
        // 非法目录名跳过
        std::fs::create_dir_all(dir.join("bad name")).unwrap();

        let skills = scan_skills(&dir);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "demo");
        assert_eq!(skills[0].description, "测试 skill");
        assert_eq!(skills[0].duration, "session");
        assert!(skills[0].active);
    }

    #[test]
    fn test_scan_skills_name_fallback_to_dir_name() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("skills");
        // frontmatter 缺失 → 回退目录名
        write_skill(&dir, "fallback", "plain markdown without frontmatter");
        let skills = scan_skills(&dir);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "fallback");
    }

    #[test]
    fn test_load_skills_seeds_examples_and_writes_json() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("skills");
        let skills = load_skills(&dir);
        assert_eq!(skills.len(), 3);
        assert!(skills.iter().any(|s| s.name == "code-review"));
        assert!(skills.iter().any(|s| s.name == "news-digest"));
        assert!(skills.iter().any(|s| s.name == "todoist"));
        // skills.json 已写入全部条目
        let json_path = skills_json_path(&dir);
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(json_path).unwrap()).unwrap();
        assert_eq!(json["skills"]["code-review"]["active"], true);
    }

    #[test]
    fn test_load_skills_respects_skills_json_active() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("skills");
        write_skill(&dir, "demo", "---\nname: demo\ndescription: d\n---\n");
        std::fs::write(
            skills_json_path(&dir),
            r#"{"skills": {"demo": {"active": false}}}"#,
        )
        .unwrap();
        let skills = scan_skills(&dir);
        assert_eq!(skills.len(), 1);
        assert!(!skills[0].active);
    }

    #[test]
    fn test_set_active_and_remove_entry() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("skills");
        std::fs::create_dir_all(&dir).unwrap();
        set_active(&dir, "demo", false).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(skills_json_path(&dir)).unwrap())
                .unwrap();
        assert_eq!(json["skills"]["demo"]["active"], false);
        // 非法 name 拒绝
        assert!(set_active(&dir, "../evil", true).is_err());
        remove_entry(&dir, "demo").unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(skills_json_path(&dir)).unwrap())
                .unwrap();
        assert!(json["skills"].get("demo").is_none());
    }

    #[test]
    fn test_resolve_skill_path_allows_skill_md() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        write_skill(
            &skills_dir,
            "demo",
            "---\nname: demo\ndescription: d\n---\n",
        );

        // 绝对路径
        let abs = skills_dir.join("demo").join("SKILL.md");
        assert!(resolve_skill_path(&skills_dir, &workspace, abs.to_str().unwrap()).is_some());

        // 非 SKILL.md 文件拒绝
        let other = skills_dir.join("demo").join("notes.txt");
        std::fs::write(&other, "x").unwrap();
        assert!(resolve_skill_path(&skills_dir, &workspace, other.to_str().unwrap()).is_none());

        // skills 目录外的文件拒绝
        let outside = tmp.path().join("config.toml");
        std::fs::write(&outside, "x").unwrap();
        assert!(resolve_skill_path(&skills_dir, &workspace, outside.to_str().unwrap()).is_none());

        // 不存在的文件拒绝
        let missing = skills_dir.join("demo2").join("SKILL.md");
        assert!(resolve_skill_path(&skills_dir, &workspace, missing.to_str().unwrap()).is_none());
    }
}
