//! `skill_create` / `skill_edit` 工具（ADR-0027）：让 agent 自管 skill。
//!
//! skill 目录（用户级 `~/.workbuddy/skills/`、项目级 `<workspace>/.workbuddy/skills/`）
//! 在主 agent 文件作用域外，file_write 够不到，故提供专用工具写盘 + 路径安全校验。
//! 本文件是 `skill_create`；`skill_edit` 见同目录 `skill_edit.rs`。

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::skill::is_valid_skill_name;
use crate::skill::loader::{split_frontmatter, validate_skill_md};
use crate::tools::skill_edit::resolve_skills_dir;

/// 工具名常量，供 runner / 文档复用，避免字符串散落。
pub const SKILL_CREATE_TOOL_NAME: &str = "skill_create";

/// 组装 SKILL.md：若 `content` 已含 frontmatter（以 `---` 开头且能正常 split），则原样采用；
/// 否则自动生成 frontmatter（`name` / `description` / `duration: turn`）+ body。
fn assemble_skill_md(name: &str, description: &str, content: &str) -> String {
    if split_frontmatter(content).is_some() {
        // content 自带 frontmatter，原样采用（合法性交给 validate_skill_md）
        return content.to_string();
    }
    format!("---\nname: {name}\ndescription: {description}\nduration: turn\n---\n\n{content}\n")
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

pub struct SkillCreateTool {
    config_dir: PathBuf,
    workspace: PathBuf,
}

impl SkillCreateTool {
    pub fn new(config_dir: PathBuf, workspace: PathBuf) -> Self {
        Self {
            config_dir,
            workspace,
        }
    }
}

#[async_trait]
impl crate::tools::Tool for SkillCreateTool {
    fn name(&self) -> &str {
        SKILL_CREATE_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Create a new skill by writing its SKILL.md. \
        Use this (NOT file_write) because skill directories live outside the agent workspace. \
        Provide `name` (kebab-case dir name, <=64 chars, no slashes), `description` (what it does and when to use it, <=1024 chars, required), and `content` (the skill body in markdown; frontmatter is auto-generated from name/description unless content already includes its own `---` frontmatter). \
        Optional `scope`: \"user\" (default, all workspaces) or \"project\" (current project only). \
        Refuses to overwrite an existing skill — use skill_edit to modify one. \
        After writing, frontmatter is validated (name + description required, length limits)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill directory name (kebab-case, <=64 chars, no '/' or '..'). Must match frontmatter name."
                },
                "description": {
                    "type": "string",
                    "description": "What the skill does and when to use it. Injected into the system prompt. <=1024 chars, required."
                },
                "content": {
                    "type": "string",
                    "description": "The skill body in markdown (workflow / output format / notes). Frontmatter is auto-generated. If you include your own `---` frontmatter block, it is used as-is."
                },
                "scope": {
                    "type": "string",
                    "enum": ["user", "project"],
                    "description": "Where to create the skill: \"user\" (default, ~/.workbuddy/skills/) or \"project\" (<workspace>/.workbuddy/skills/)."
                }
            },
            "required": ["name", "description", "content"]
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
            .ok_or_else(|| anyhow!("skill_create: missing required `name`"))?
            .to_string();
        if !is_valid_skill_name(&name) {
            anyhow::bail!(
                "skill_create: invalid skill name {:?} (use kebab-case, no slashes or '..')",
                name
            );
        }
        let description = args
            .get("description")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("skill_create: missing required `description`"))?
            .to_string();
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("skill_create: missing required `content`"))?
            .to_string();
        let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("user");

        let skills_dir = resolve_skills_dir(&self.config_dir, &self.workspace, scope)?;
        let dir = ensure_within_skills_dir(&skills_dir, &name)?;
        if dir.join("SKILL.md").exists() {
            anyhow::bail!(
                "skill_create: skill {:?} already exists (scope={:?}); use skill_edit to modify it",
                name,
                scope
            );
        }

        let full = assemble_skill_md(&name, &description, &content);
        validate_skill_md(&full).map_err(|e| anyhow!("skill_create: invalid SKILL.md: {}", e))?;

        std::fs::create_dir_all(&dir)
            .map_err(|e| anyhow!("skill_create: create dir {}: {}", dir.display(), e))?;
        let skill_md = dir.join("SKILL.md");
        std::fs::write(&skill_md, &full)
            .map_err(|e| anyhow!("skill_create: write {}: {}", skill_md.display(), e))?;

        Ok(format!(
            "Created skill {:?} (scope={:?}) at {}\n\n{}",
            name,
            scope,
            skill_md.display(),
            full
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use tempfile::tempdir;

    fn tool() -> SkillCreateTool {
        let tmp = tempdir().unwrap();
        SkillCreateTool::new(tmp.path().join("config"), tmp.path().join("workspace"))
    }

    #[tokio::test]
    async fn rejects_invalid_name() {
        let t = tool();
        let r = t
            .execute(
                &json!({ "name": "../evil", "description": "d", "content": "body" }),
                "cli",
            )
            .await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn writes_user_scope_and_validates() {
        let t = tool();
        let out = t
            .execute(
                &json!({
                    "name": "my-skill",
                    "description": "does a thing",
                    "content": "# My Skill\n\n1. step one"
                }),
                "cli",
            )
            .await
            .unwrap();
        assert!(out.contains("Created skill"));
        let p = t
            .config_dir
            .join("skills")
            .join("my-skill")
            .join("SKILL.md");
        assert!(p.exists());
        let written = std::fs::read_to_string(&p).unwrap();
        assert!(written.contains("name: my-skill"));
        assert!(written.contains("description: does a thing"));
        assert!(written.contains("# My Skill"));
    }

    #[tokio::test]
    async fn writes_project_scope() {
        let t = tool();
        let out = t
            .execute(
                &json!({
                    "name": "proj-skill",
                    "description": "project scoped",
                    "content": "body",
                    "scope": "project"
                }),
                "cli",
            )
            .await
            .unwrap();
        assert!(out.contains("scope=\"project\""));
        let p = t
            .workspace
            .join(".workbuddy")
            .join("skills")
            .join("proj-skill")
            .join("SKILL.md");
        assert!(p.exists());
    }

    #[tokio::test]
    async fn rejects_unknown_scope() {
        let t = tool();
        let r = t
            .execute(
                &json!({ "name": "x", "description": "d", "content": "b", "scope": "global" }),
                "cli",
            )
            .await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn rejects_overwrite() {
        let t = tool();
        let args = json!({ "name": "dup", "description": "d", "content": "b" });
        t.execute(&args, "cli").await.unwrap();
        let r = t.execute(&args, "cli").await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn rejects_missing_description() {
        let t = tool();
        let r = t
            .execute(&json!({ "name": "x", "content": "b" }), "cli")
            .await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn escapes_path_rejected_by_name_check() {
        // 即使 name 通过校验，目录也必须落在 skills_dir 内（防御性）。
        let t = tool();
        let r = t
            .execute(
                &json!({ "name": "ok", "description": "d", "content": "b" }),
                "cli",
            )
            .await;
        assert!(r.is_ok());
        // 校验写入位置确实在 skills_dir 内
        let p = t.config_dir.join("skills").join("ok").join("SKILL.md");
        assert!(p.exists());
    }
}
