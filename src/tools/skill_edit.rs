//! `skill_edit` 工具（ADR-0027）：修改已存在的 skill 的 SKILL.md。
//!
//! 与 `skill_create` 同目录（ADR-0027 决策 #2）。`skill_create` 复用本文件的
//! `resolve_skills_dir` 做 scope → skills 目录解析。

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::skill::is_valid_skill_name;
use crate::skill::loader::{split_frontmatter, validate_skill_md};

/// 工具名常量，供 runner / 文档复用，避免字符串散落。
pub const SKILL_EDIT_TOOL_NAME: &str = "skill_edit";

/// 解析 scope → skills 目录：
/// - `user`（默认）：`<config_dir>/skills`
/// - `project`：`<workspace>/.workbuddy/skills`
pub fn resolve_skills_dir(config_dir: &Path, workspace: &Path, scope: &str) -> Result<PathBuf> {
    match scope {
        "user" | "" => Ok(config_dir.join("skills")),
        "project" => Ok(workspace.join(".workbuddy").join("skills")),
        other => anyhow::bail!("invalid scope {:?} (allowed: \"user\", \"project\")", other),
    }
}

/// 确保最终目录落在 skills_dir 内（词法防穿越）。
/// `name` 已通过 `is_valid_skill_name` 保证不含 `/` 与 `..`，此处为防御性兜底。
fn ensure_within_skills_dir(skills_dir: &Path, name: &str) -> Result<PathBuf> {
    let dir = skills_dir.join(name);
    let norm = crate::path_guard::normalize_lexical(&dir);
    let norm_root = crate::path_guard::normalize_lexical(skills_dir);
    if !norm.starts_with(&norm_root) {
        anyhow::bail!("skill path escapes skills dir: {:?}", norm);
    }
    Ok(dir)
}

/// 应用 patch：
/// - 字符串 → 追加到 body（保留 frontmatter）。
/// - 对象 `{ find, replace }` → 在全文做单次精确替换（找不到则报错）。
fn apply_patch(existing: &str, patch: &Value) -> Result<String> {
    match patch {
        Value::String(s) if !s.trim().is_empty() => {
            let (yaml, body) = split_frontmatter(existing).unwrap_or(("", existing));
            let mut out = String::new();
            if !yaml.is_empty() {
                // 保留 frontmatter：开头 --- + yaml + 结尾 ---
                out.push_str("---\n");
                out.push_str(yaml);
                out.push_str("---\n");
            }
            let trimmed_body = body.trim_end();
            out.push_str(trimmed_body);
            if !trimmed_body.is_empty() {
                out.push('\n');
            }
            out.push('\n');
            out.push_str(s.trim());
            out.push('\n');
            Ok(out)
        }
        Value::Object(map) => {
            let find = map
                .get("find")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("skill_edit: patch.find required (non-empty string)"))?;
            let replace = map.get("replace").and_then(|v| v.as_str()).unwrap_or("");
            if !existing.contains(find) {
                anyhow::bail!("skill_edit: patch.find not found in SKILL.md");
            }
            Ok(existing.replacen(find, replace, 1))
        }
        _ => anyhow::bail!(
            "skill_edit: `patch` must be a string (append to body) or an object {{\"find\", \"replace\"}}"
        ),
    }
}

pub struct SkillEditTool {
    config_dir: PathBuf,
    workspace: PathBuf,
}

impl SkillEditTool {
    pub fn new(config_dir: PathBuf, workspace: PathBuf) -> Self {
        Self {
            config_dir,
            workspace,
        }
    }
}

#[async_trait]
impl crate::tools::Tool for SkillEditTool {
    fn name(&self) -> &str {
        SKILL_EDIT_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Edit an existing skill's SKILL.md (use this, NOT file_edit, since skill dirs are outside the workspace). \
        Provide `name` and either `content` (full replacement SKILL.md text) or `patch`. \
        `patch` is a string appended to the body, OR an object {\"find\": \"...\", \"replace\": \"...\"} for a single targeted edit. \
        Optional `scope`: \"user\" (default) or \"project\". \
        After writing, frontmatter is validated (name + description required, length limits)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill directory name (kebab-case). Must already exist."
                },
                "content": {
                    "type": "string",
                    "description": "Full replacement SKILL.md text (overwrites the existing file)."
                },
                "patch": {
                    "description": "Partial edit: a string appended to the body, OR an object {\"find\": \"...\", \"replace\": \"...\"} for a single targeted replacement.",
                    "oneOf": [
                        { "type": "string" },
                        { "type": "object", "properties": { "find": { "type": "string" }, "replace": { "type": "string" } } }
                    ]
                },
                "scope": {
                    "type": "string",
                    "enum": ["user", "project"],
                    "description": "Which skills dir: \"user\" (default) or \"project\"."
                }
            },
            "required": ["name"]
        })
    }

    fn requires_confirm(&self) -> bool {
        // 写盘到 workspace 之外的 skills 目录，属有副作用操作，走审批门。
        true
    }

    async fn execute(&self, args: &Value, _channel: &str) -> Result<String> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("skill_edit: missing required `name`"))?
            .to_string();
        if !is_valid_skill_name(&name) {
            anyhow::bail!(
                "skill_edit: invalid skill name {:?} (use kebab-case, no slashes or '..')",
                name
            );
        }
        let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("user");

        let skills_dir = resolve_skills_dir(&self.config_dir, &self.workspace, scope)?;
        let dir = ensure_within_skills_dir(&skills_dir, &name)?;
        let skill_md = dir.join("SKILL.md");
        if !skill_md.exists() {
            anyhow::bail!(
                "skill_edit: skill {:?} not found (scope={:?}); create it first with skill_create",
                name,
                scope
            );
        }

        let existing = std::fs::read_to_string(&skill_md)
            .map_err(|e| anyhow!("skill_edit: read {}: {}", skill_md.display(), e))?;
        let new_content = if let Some(c) = args.get("content").and_then(|v| v.as_str()) {
            if c.trim().is_empty() {
                anyhow::bail!("skill_edit: `content` must be non-empty");
            }
            c.to_string()
        } else if let Some(patch) = args.get("patch") {
            apply_patch(&existing, patch)?
        } else {
            anyhow::bail!(
                "skill_edit: provide `content` (full replace) or `patch` (append / find-replace)"
            );
        };

        validate_skill_md(&new_content)
            .map_err(|e| anyhow!("skill_edit: invalid SKILL.md: {}", e))?;

        // 原子写：临时文件 + rename，避免半写损坏。
        let tmp = skill_md.with_extension("SKILL.md.tmp");
        std::fs::write(&tmp, &new_content)
            .map_err(|e| anyhow!("skill_edit: write tmp {}: {}", tmp.display(), e))?;
        std::fs::rename(&tmp, &skill_md)
            .map_err(|e| anyhow!("skill_edit: rename {}: {}", skill_md.display(), e))?;

        Ok(format!(
            "Updated skill {:?} (scope={:?}) at {}\n\n{}",
            name,
            scope,
            skill_md.display(),
            new_content
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use tempfile::tempdir;

    fn tool() -> SkillEditTool {
        let tmp = tempdir().unwrap();
        SkillEditTool::new(tmp.path().join("config"), tmp.path().join("workspace"))
    }

    fn make_skill(t: &SkillEditTool, name: &str, body: &str) {
        let dir = t.config_dir.join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: d\n---\n\n{body}\n"),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn full_replace() {
        let t = tool();
        make_skill(&t, "demo", "old body");
        let out = t
            .execute(
                &json!({ "name": "demo", "content": "---\nname: demo\ndescription: d\n---\n\nnew body\n" }),
                "cli",
            )
            .await
            .unwrap();
        assert!(out.contains("Updated skill"));
        let written = std::fs::read_to_string(t.config_dir.join("skills/demo/SKILL.md")).unwrap();
        assert!(written.contains("new body"));
        assert!(!written.contains("old body"));
    }

    #[tokio::test]
    async fn patch_append() {
        let t = tool();
        make_skill(&t, "demo", "step one");
        t.execute(&json!({ "name": "demo", "patch": "step two" }), "cli")
            .await
            .unwrap();
        let written = std::fs::read_to_string(t.config_dir.join("skills/demo/SKILL.md")).unwrap();
        assert!(written.contains("step one"));
        assert!(written.contains("step two"));
    }

    #[tokio::test]
    async fn patch_find_replace() {
        let t = tool();
        make_skill(&t, "demo", "use FOO here");
        t.execute(
            &json!({ "name": "demo", "patch": { "find": "FOO", "replace": "BAR" } }),
            "cli",
        )
        .await
        .unwrap();
        let written = std::fs::read_to_string(t.config_dir.join("skills/demo/SKILL.md")).unwrap();
        assert!(written.contains("use BAR here"));
    }

    #[tokio::test]
    async fn patch_find_missing_errors() {
        let t = tool();
        make_skill(&t, "demo", "body");
        let r = t
            .execute(
                &json!({ "name": "demo", "patch": { "find": "NOPE", "replace": "x" } }),
                "cli",
            )
            .await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn missing_skill_errors() {
        let t = tool();
        let r = t
            .execute(&json!({ "name": "ghost", "content": "x" }), "cli")
            .await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn project_scope_roundtrip() {
        let t = tool();
        let dir = t.workspace.join(".workbuddy").join("skills").join("p");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: p\ndescription: d\n---\n\nb\n",
        )
        .unwrap();
        let out = t
            .execute(&json!({ "name": "p", "content": "---\nname: p\ndescription: d\n---\n\nupdated\n", "scope": "project" }), "cli")
            .await
            .unwrap();
        assert!(out.contains("scope=\"project\""));
    }

    #[tokio::test]
    async fn rejects_invalid_frontmatter() {
        let t = tool();
        make_skill(&t, "demo", "body");
        // 去掉 description 会导致校验失败
        let r = t
            .execute(
                &json!({ "name": "demo", "content": "---\nname: demo\n---\n\nno description\n" }),
                "cli",
            )
            .await;
        assert!(r.is_err());
        // 原文件不应被改坏
        let written = std::fs::read_to_string(t.config_dir.join("skills/demo/SKILL.md")).unwrap();
        assert!(written.contains("body"));
    }
}
