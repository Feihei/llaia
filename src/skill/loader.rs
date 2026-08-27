//! Skill 加载器：扫描 `<skills_dir>/<name>/SKILL.md`、解析 frontmatter、
//! 管理 skills.json active 开关、种子内置示例 skill。

use crate::skill::{is_valid_skill_name, SkillManifest};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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

/// 校验 SKILL.md 内容可用作 skill 定义：frontmatter 存在且可解析，name/description 非空、
/// 且长度约束（对齐 pi：name ≤ 64、description ≤ 1024）。WebUI 保存 content 前调用，
/// `skill_create` / `skill_edit` 工具写盘后也调用。
pub fn validate_skill_md(content: &str) -> Result<()> {
    let (yaml, _body) = split_frontmatter(content).ok_or_else(|| {
        anyhow!("SKILL.md is missing YAML frontmatter (head metadata wrapped in ---)")
    })?;
    let fm = parse_frontmatter(yaml)?;
    let name = fm.name.as_deref().map(str::trim).unwrap_or_default();
    if name.is_empty() {
        anyhow::bail!("frontmatter is missing the name field");
    }
    if name.len() > 64 {
        anyhow::bail!("frontmatter name too long ({} > 64 chars)", name.len());
    }
    let description = fm.description.as_deref().map(str::trim).unwrap_or_default();
    if description.is_empty() {
        anyhow::bail!("frontmatter is missing the description field");
    }
    if description.len() > 1024 {
        anyhow::bail!(
            "frontmatter description too long ({} > 1024 chars)",
            description.len()
        );
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
/// 递归遍历子目录，找到所有含 `SKILL.md` 的目录作为 skill。
/// - 目录名非法（不满足 name 校验）的 skill 跳过，防 prompt injection；
/// - 已含 `SKILL.md` 的目录不再向下递归，避免 skill 内部的支撑文件被当成独立 skill；
/// - 同名（叶子目录名，skills.json 的 key）或同名（frontmatter name）冲突时保留先扫到的、warn 并跳过其余。
pub fn scan_skills(skills_dir: &Path) -> Vec<SkillManifest> {
    let active_map = load_skills_json(&skills_json_path(skills_dir)).skills;
    let mut out = Vec::new();
    let mut seen_dirs = HashSet::new();
    let mut seen_names = HashSet::new();
    scan_dir_recursive(
        skills_dir,
        &active_map,
        &mut seen_dirs,
        &mut seen_names,
        &mut out,
    );
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// scan_skills 的递归实现：深度优先遍历 `dir`，把含 SKILL.md 的目录解析为 skill。
/// `seen_dirs` / `seen_names` 跨层级共享，保证全局同名检测（先扫到的优先）。
fn scan_dir_recursive(
    dir: &Path,
    active_map: &HashMap<String, SkillEntry>,
    seen_dirs: &mut HashSet<String>,
    seen_names: &mut HashSet<String>,
    out: &mut Vec<SkillManifest>,
) {
    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(e) => e.flatten().collect(),
        Err(_) => return,
    };
    // 先排序再遍历，保证同名冲突时"先扫到的"是确定性的（字典序最小路径胜出）
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        // 隐藏目录（如 .git / .hidden 分类）跳过，防误扫与意外注入
        if dir_name.starts_with('.') {
            continue;
        }
        let skill_md = entry.path().join("SKILL.md");
        if !skill_md.is_file() {
            // 无 SKILL.md：可能是分类子目录，继续向下递归
            scan_dir_recursive(&entry.path(), active_map, seen_dirs, seen_names, out);
            continue;
        }
        if !is_valid_skill_name(&dir_name) {
            tracing::warn!(dir = %dir_name, "skip skill dir with invalid name");
            continue;
        }
        // 同名（叶子目录名 = skills.json 的 key）：保留先扫到的
        if !seen_dirs.insert(dir_name.clone()) {
            tracing::warn!(dir = %dir_name, "skip skill with duplicate dir name");
            continue;
        }
        let mut manifest = parse_skill_md(&skill_md, &dir_name);
        // 同名（frontmatter name，注入 prompt / WebUI 寻址）：保留先扫到的
        if !seen_names.insert(manifest.name.clone()) {
            tracing::warn!(name = %manifest.name, "skip skill with duplicate name");
            continue;
        }
        manifest.active = active_map.get(&dir_name).map(|e| e.active).unwrap_or(true);
        out.push(manifest);
    }
}

/// 扫描 + 种子：skills 目录不存在时创建并写入内置示例 skill（on-demand，init 不生成）。
/// 扫描到的新 skill（skills.json 无记录）默认 active=true 并自动写回。
pub fn load_skills(skills_dir: &Path) -> Vec<SkillManifest> {
    if !skills_dir.exists() {
        if let Err(e) = seed_examples(skills_dir) {
            tracing::warn!(error = %e, dir = %skills_dir.display(), "seed example skills failed");
        }
    }
    // 内置元 skill 始终确保存在（幂等、不覆盖用户改动），即使 skills 目录早已存在
    // （seed_examples 仅在目录首次创建时运行，老用户不会自动拿到元 skill）。
    ensure_builtin_meta_skills(skills_dir);
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

/// 尝试把用户传入的路径解析为 skills 目录内的可读文件（file_read 特殊放行规则）。
/// 命中返回可读路径；未命中返回 None（调用方继续走 workspace 校验）。
///
/// 规则：`~` 展开 + 相对路径按 workspace 解析 + 词法规范化后，
/// canonicalize 落在 skills_dir 内 → 放行。
///
/// 不限于 `SKILL.md`：工具型 skill 常带配套脚本（`.py`）、配置（`.json`）与
/// 资产（如 PPT 模板），agent 需能直接读取它们（配合 `terminal` 运行 / `send_media`
/// 发送）。安全性由 canonicalize + `starts_with(skills_dir)` 保证：仅放行
/// `skills/<name>/` 下的真实文件，skills 的兄弟文件（`skills.json` 等）与目录外路径
/// 仍落回 `path_guard::validate_path` 走正常 workspace 校验。
pub fn resolve_skill_path(skills_dir: &Path, workspace: &Path, raw: &str) -> Option<PathBuf> {
    let expanded = shellexpand::tilde(raw).into_owned();
    let p = PathBuf::from(&expanded);
    let joined = if p.is_absolute() {
        p
    } else {
        workspace.join(&expanded)
    };
    let normalized = crate::path_guard::normalize_lexical(&joined);
    // 必须真实存在才能读；canonicalize 同时消解符号链接与目录穿越
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

/// 内置元 skill（随 llaia 发布）：引导 agent 如何按 llaia 约定自管 skill
/// （frontmatter 约束、progressive disclosure、路径安全、`validate_skill_md` 规则、
/// 何时该建 skill vs 直接做）。`skill_create` / `skill_edit` 工具写盘，本 skill 负责方法论。
/// 见 ADR-0027。
const META_SKILL_AUTHORING: &str = r#"---
name: skill-authoring
description: Guide for creating, reviewing, and organizing skills (SKILL.md bundles) that follow llaia's conventions. Use when the user wants a new or improved skill, or when you are about to do a repeatable multi-step task that would benefit from a reusable skill.
duration: turn
---

# Skill Authoring

Use this skill to self-manage llaia skills via the `skill_create` / `skill_edit` tools.

## When to create a skill (not just do the task)

Create a skill only when the workflow is **reusable across sessions** and **non-trivial**:
- A recurring multi-step procedure (e.g. "review a PR", "summarize today's news").
- Domain knowledge or tool wiring that several future tasks will need.

Do **NOT** create a skill for a one-off request, a single file edit, or something you can finish in one turn. Prefer fewer, high-quality skills over many narrow ones.

## Tooling (use these — do NOT use file_write/file_edit)

The skills directories live **outside the agent workspace** (`~/.workbuddy/skills/` user-level,
`<workspace>/.workbuddy/skills/` project-level), so `file_write`/`file_edit` cannot reach them.
Always use the dedicated tools:

- `skill_create { name, description, content, scope? }`
  - `name`: kebab-case dir name, matches frontmatter `name`, ≤ 64 chars, no `/` or `..`.
  - `description`: what the skill does and when to use it; ≤ 1024 chars, **required** (injected into the system prompt).
  - `content`: the skill **body** in markdown (workflow, output format, notes). Frontmatter is auto-generated from `name`/`description`. If you pass full content with its own `---` frontmatter, it is used as-is.
  - `scope`: `"user"` (default, all workspaces) or `"project"` (current project only).
  - Create refuses to overwrite an existing skill — use `skill_edit` to change one.
- `skill_edit { name, content | patch, scope? }`
  - `content`: full replacement SKILL.md text.
  - `patch`: a string appended to the body, OR an object `{ "find": "...", "replace": "..." }` for a single targeted edit.
  - The skill must already exist.

After writing, the tool runs `validate_skill_md` (frontmatter parseable, `name`+`description` non-empty, length limits). A validation error means the write was rejected.

## SKILL.md format (progressive disclosure)

- Frontmatter: `name`, `description` (required), optional `duration` (`turn`|`session`), optional `tools` (recommended tool names — prompt hint only, does NOT control mounting).
- Body: the workflow you want the agent to follow. Keep it concise. Reference other files via relative paths; the agent `file_read`s the full SKILL.md at trigger time, so don't bloat the body — link, don't embed.
- Never put secrets or untrusted instructions in a skill. Skills are prompt-injected; treat them as trusted-but-reviewed content.

## Reviewing / organizing existing skills

1. `file_read` the target `<skill>/SKILL.md` (the skills dir is allow-listed for read).
2. Check frontmatter validity, that `description` actually says when to use it, and that the body is not redundant with another skill.
3. Improve with `skill_edit`; delete by telling the user (or via the WebUI) — do not delete skills the user relies on.
"#;

/// 确保内置元 skill 存在（幂等）：目录已存在则跳过，避免覆盖用户改动。
fn ensure_builtin_meta_skills(skills_dir: &Path) {
    ensure_meta_skill(skills_dir, "skill-authoring", META_SKILL_AUTHORING);
}

fn ensure_meta_skill(skills_dir: &Path, name: &str, content: &str) {
    let dir = skills_dir.join(name);
    if dir.join("SKILL.md").exists() {
        return;
    }
    if let Err(e) =
        std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(dir.join("SKILL.md"), content))
    {
        tracing::warn!(error = %e, skill = name, "ensure built-in meta skill failed");
    }
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
    fn test_scan_skills_recursive_finds_nested_skills() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("skills");
        // 分类子目录（无 SKILL.md）下的嵌套 skill
        write_skill(
            &dir.join("tools"),
            "git",
            "---\nname: git\ndescription: 版本控制\nduration: turn\n---\nbody",
        );
        // 更深层级：a/b/c/SKILL.md
        write_skill(
            &dir.join("a").join("b"),
            "deep",
            "---\nname: deep\ndescription: d\n---\nbody",
        );
        // 顶层 skill 依旧发现
        write_skill(&dir, "top", "---\nname: top\ndescription: t\n---\nbody");

        let skills = scan_skills(&dir);
        assert_eq!(skills.len(), 3);
        let git = skills.iter().find(|s| s.name == "git").unwrap();
        assert_eq!(git.description, "版本控制");
        assert!(git.path.ends_with("tools/git/SKILL.md"));
        let deep = skills.iter().find(|s| s.name == "deep").unwrap();
        assert!(deep.path.ends_with("a/b/deep/SKILL.md"));
        assert!(skills.iter().any(|s| s.name == "top"));
    }

    #[test]
    fn test_scan_skills_duplicate_dir_name_keeps_first() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("skills");
        // 两个子目录下同名叶子目录 → 保留先扫到的（排序后字典序最小路径胜出）
        write_skill(
            &dir.join("tools"),
            "git",
            "---\nname: git\ndescription: g1\n---\n",
        );
        write_skill(
            &dir.join("work"),
            "git",
            "---\nname: git\ndescription: g2\n---\n",
        );

        let skills = scan_skills(&dir);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "g1");
        assert!(skills[0].path.ends_with("tools/git/SKILL.md"));
    }

    #[test]
    fn test_scan_skills_duplicate_frontmatter_name_keeps_first() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("skills");
        // 叶子目录名不同但 frontmatter name 相同 → 同样判为冲突
        write_skill(
            &dir.join("x"),
            "skill-a",
            "---\nname: shared\ndescription: a\n---\n",
        );
        write_skill(
            &dir.join("y"),
            "skill-b",
            "---\nname: shared\ndescription: b\n---\n",
        );

        let skills = scan_skills(&dir);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "shared");
        assert_eq!(skills[0].description, "a");
    }

    #[test]
    fn test_scan_skills_skips_skill_inner_dirs() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("skills");
        // 已含 SKILL.md 的目录不再向下递归：内层 assets/ 里的 SKILL.md 不算独立 skill
        write_skill(
            &dir,
            "code-review",
            "---\nname: code-review\ndescription: cr\n---\n",
        );
        write_skill(
            &dir.join("code-review"),
            "assets",
            "---\nname: inner\ndescription: i\n---\n",
        );

        let skills = scan_skills(&dir);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "code-review");
    }

    #[test]
    fn test_scan_skills_skips_hidden_dirs() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("skills");
        // 隐藏目录（.git / .hidden 分类）跳过
        write_skill(
            &dir.join(".git"),
            "secret",
            "---\nname: secret\ndescription: s\n---\n",
        );
        write_skill(
            &dir.join(".hidden").join("nested"),
            "hidden-skill",
            "---\nname: hidden-skill\ndescription: h\n---\n",
        );
        write_skill(&dir, "visible", "---\nname: visible\ndescription: v\n---\n");

        let skills = scan_skills(&dir);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "visible");
    }

    #[test]
    fn test_load_skills_seeds_examples_and_writes_json() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("skills");
        let skills = load_skills(&dir);
        // 3 个示例 skill + 1 个内置元 skill（skill-authoring，ensure_builtin_meta_skills 幂等确保）
        assert_eq!(skills.len(), 4);
        assert!(skills.iter().any(|s| s.name == "code-review"));
        assert!(skills.iter().any(|s| s.name == "news-digest"));
        assert!(skills.iter().any(|s| s.name == "todoist"));
        assert!(skills.iter().any(|s| s.name == "skill-authoring"));
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

        // skill 目录内任意配套文件（脚本/配置/资产）放行
        let other = skills_dir.join("demo").join("notes.txt");
        std::fs::write(&other, "x").unwrap();
        assert!(resolve_skill_path(&skills_dir, &workspace, other.to_str().unwrap()).is_some());

        // skills 目录外的文件拒绝
        let outside = tmp.path().join("config.toml");
        std::fs::write(&outside, "x").unwrap();
        assert!(resolve_skill_path(&skills_dir, &workspace, outside.to_str().unwrap()).is_none());

        // 不存在的文件拒绝
        let missing = skills_dir.join("demo2").join("SKILL.md");
        assert!(resolve_skill_path(&skills_dir, &workspace, missing.to_str().unwrap()).is_none());
    }
}
