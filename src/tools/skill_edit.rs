//! `skill_edit` 工具（ADR-0027）：修改已存在的 skill 的 SKILL.md。
//!
//! 与 `skill_create` 同目录（ADR-0027 决策 #2）。`skill_create` 复用本文件的
//! `resolve_skills_dir` 做 scope → skills 目录解析。
//!
//! 参数设计与 `file_edit` 对齐：三种编辑模式互斥单选，全部是扁平命名字符串
//! 参数、无 union 类型——早期 `patch`（string=追加 / object=替换）的 union
//! 分派曾诱发弱模型把对象二次序列化成字符串（2026-08-27 事故）。

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

/// 追加模式：保留 frontmatter，把 `text` 追加到正文末尾。
fn append_to_body(existing: &str, text: &str) -> String {
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
    out.push_str(text.trim());
    out.push('\n');
    out
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
        Exactly one mode per call: \
        `content` (full replacement SKILL.md text), OR \
        `old_string` + `new_string` (single targeted replacement — old_string must match exactly once; same model as file_edit), OR \
        `append` (text appended to the end of the body). \
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
                    "description": "Mode 1 — full replacement SKILL.md text (overwrites the existing file)."
                },
                "old_string": {
                    "type": "string",
                    "description": "Mode 2 (with `new_string`) — exact existing text to replace; must occur exactly once in SKILL.md."
                },
                "new_string": {
                    "type": "string",
                    "description": "Mode 2 (with `old_string`) — replacement text; may be empty to delete `old_string`."
                },
                "append": {
                    "type": "string",
                    "description": "Mode 3 — text appended to the end of the body (frontmatter preserved). Never replaces anything."
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

        // 三种编辑模式互斥单选；成功消息带 op_desc 明示本次写操作类型。
        let content = args.get("content").and_then(|v| v.as_str());
        let old = args.get("old_string").and_then(|v| v.as_str());
        let new = args.get("new_string").and_then(|v| v.as_str());
        let append = args.get("append").and_then(|v| v.as_str());
        if old.is_some() != new.is_some() {
            anyhow::bail!("skill_edit: `old_string` and `new_string` must be provided together");
        }
        let given = [content.is_some(), old.is_some(), append.is_some()];
        if given.iter().filter(|g| **g).count() > 1 {
            anyhow::bail!(
                "skill_edit: modes are mutually exclusive; provide exactly one of `content` (full replace), `old_string`+`new_string` (targeted replace), `append`"
            );
        }

        let (new_content, op_desc) = if let Some(c) = content {
            if c.trim().is_empty() {
                anyhow::bail!("skill_edit: `content` must be non-empty");
            }
            (c.to_string(), "replaced the whole file with `content`")
        } else if let (Some(o), Some(n)) = (old, new) {
            if o.is_empty() {
                anyhow::bail!("skill_edit: `old_string` must be non-empty (to rewrite the whole file use `content`)");
            }
            // 对齐 file_edit：要求唯一命中，避免静默替换到错误位置。
            let count = existing.matches(o).count();
            match count {
                0 => anyhow::bail!("skill_edit: old_string not found in SKILL.md"),
                1 => (
                    existing.replacen(o, n, 1),
                    "replaced the unique occurrence of `old_string` with `new_string`",
                ),
                _ => anyhow::bail!(
                    "skill_edit: old_string appears {count} times in SKILL.md; include more surrounding text so it matches exactly once (or use `content` to replace the whole file)"
                ),
            }
        } else if let Some(a) = append {
            if a.trim().is_empty() {
                anyhow::bail!("skill_edit: `append` must be non-empty");
            }
            (
                append_to_body(&existing, a),
                "appended `append` to the end of the body",
            )
        } else {
            anyhow::bail!(
                "skill_edit: provide exactly one of `content` (full replace), `old_string`+`new_string` (targeted replace), `append`"
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
            "Updated skill {:?} (scope={:?}, {}) at {}\n\n{}",
            name,
            scope,
            op_desc,
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
        assert!(out.contains("replaced the whole file"));
        let written = std::fs::read_to_string(t.config_dir.join("skills/demo/SKILL.md")).unwrap();
        assert!(written.contains("new body"));
        assert!(!written.contains("old body"));
    }

    #[tokio::test]
    async fn append_adds_to_body_and_keeps_frontmatter() {
        let t = tool();
        make_skill(&t, "demo", "step one");
        let out = t
            .execute(&json!({ "name": "demo", "append": "step two" }), "cli")
            .await
            .unwrap();
        assert!(out.contains("appended `append`"));
        let written = std::fs::read_to_string(t.config_dir.join("skills/demo/SKILL.md")).unwrap();
        assert!(written.starts_with("---\nname: demo\n"));
        assert!(written.contains("step one"));
        assert!(written.contains("step two"));
    }

    #[tokio::test]
    async fn replace_unique_occurrence() {
        let t = tool();
        make_skill(&t, "demo", "use FOO here");
        let out = t
            .execute(
                &json!({ "name": "demo", "old_string": "FOO", "new_string": "BAR" }),
                "cli",
            )
            .await
            .unwrap();
        assert!(out.contains("replaced the unique occurrence"));
        let written = std::fs::read_to_string(t.config_dir.join("skills/demo/SKILL.md")).unwrap();
        assert!(written.contains("use BAR here"));
    }

    #[tokio::test]
    async fn replace_can_delete_old_string() {
        let t = tool();
        make_skill(&t, "demo", "keep REMOVE me end");
        t.execute(
            &json!({ "name": "demo", "old_string": "REMOVE me ", "new_string": "" }),
            "cli",
        )
        .await
        .unwrap();
        let written = std::fs::read_to_string(t.config_dir.join("skills/demo/SKILL.md")).unwrap();
        assert_eq!(
            written,
            "---\nname: demo\ndescription: d\n---\n\nkeep end\n"
        );
    }

    #[tokio::test]
    async fn replace_missing_old_string_errors() {
        let t = tool();
        make_skill(&t, "demo", "body");
        let r = t
            .execute(
                &json!({ "name": "demo", "old_string": "NOPE", "new_string": "x" }),
                "cli",
            )
            .await;
        assert!(r.is_err());
    }

    // 对齐 file_edit：old_string 多处命中必须报错，不允许静默替换第一处。
    #[tokio::test]
    async fn replace_ambiguous_old_string_errors() {
        let t = tool();
        make_skill(&t, "demo", "same same different");
        let r = t
            .execute(
                &json!({ "name": "demo", "old_string": "same", "new_string": "x" }),
                "cli",
            )
            .await;
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("2 times"));
        // 原文件未被改动
        let written = std::fs::read_to_string(t.config_dir.join("skills/demo/SKILL.md")).unwrap();
        assert!(written.contains("same same"));
    }

    #[tokio::test]
    async fn replace_empty_old_string_errors() {
        let t = tool();
        make_skill(&t, "demo", "body");
        let r = t
            .execute(
                &json!({ "name": "demo", "old_string": "", "new_string": "x" }),
                "cli",
            )
            .await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn half_old_new_pair_errors() {
        let t = tool();
        make_skill(&t, "demo", "body");
        let r = t
            .execute(&json!({ "name": "demo", "old_string": "body" }), "cli")
            .await;
        assert!(r.unwrap_err().to_string().contains("together"));
    }

    #[tokio::test]
    async fn multiple_modes_error() {
        let t = tool();
        make_skill(&t, "demo", "body");
        let r = t
            .execute(
                &json!({ "name": "demo", "content": "x", "append": "y" }),
                "cli",
            )
            .await;
        assert!(r.unwrap_err().to_string().contains("mutually exclusive"));
    }

    #[tokio::test]
    async fn no_mode_errors() {
        let t = tool();
        make_skill(&t, "demo", "body");
        let r = t.execute(&json!({ "name": "demo" }), "cli").await;
        assert!(r.unwrap_err().to_string().contains("exactly one"));
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
