//! Skills system prompt 生成（Progressive Disclosure）。
//!
//! 启动时只注入 name + description + SKILL.md 路径的轻量清单，
//! 详细指令由 LLM 触发 skill 时自己 file_read 读取。

use crate::skill::{is_valid_skill_name, SkillManifest};

/// 过滤注入 prompt 的文本：去除控制字符（换行折叠为空格）与反引号，防 prompt injection。
/// 借鉴 AstrBot `_CONTROL_CHARS_RE`。
pub fn sanitize_prompt_text(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '`' {
                '\''
            } else if c.is_control() {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// 生成 system prompt 的 "## Skills" 段。无 active skill 时返回空串。
/// 仅注入 name 合法（`^[\w.-]+$`）的 skill；description / path 过滤危险字符。
pub fn build_skills_prompt(skills: &[SkillManifest]) -> String {
    let active: Vec<&SkillManifest> = skills
        .iter()
        .filter(|s| s.active && is_valid_skill_name(&s.name))
        .collect();
    if active.is_empty() {
        return String::new();
    }

    let mut out = String::from(
        "## Skills\n\nYou have specialized skills — reusable instruction bundles stored in SKILL.md files.\n\n### Available skills\n",
    );
    for s in &active {
        let desc = sanitize_prompt_text(&s.description);
        let path = sanitize_prompt_text(&s.path.display().to_string());
        out.push_str(&format!("- **{}**: {}\n  File: {}\n", s.name, desc, path));
        if !s.tools.is_empty() {
            let tools = s
                .tools
                .iter()
                .map(|t| sanitize_prompt_text(t))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("  Suggested tools: {}\n", tools));
        }
    }
    out.push_str(
        "\n### Skill rules\n\
         1. Discovery — 上面的列表是当前会话可用的完整 skill 清单\n\
         2. When to trigger — 用户显式提到 skill 名，或任务明确匹配 skill 的 description 时使用\n\
         3. Mandatory grounding — 执行 skill 前必须先 file_read 它的 SKILL.md（用上面给出的路径）\n\
         4. Progressive disclosure — 只读 SKILL.md 直接引用的文件，不要深度追引用\n\
         5. Failure handling — skill 无法应用时清楚说明问题，继续用最佳替代方案\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn manifest(name: &str, description: &str, active: bool) -> SkillManifest {
        SkillManifest {
            name: name.into(),
            description: description.into(),
            duration: "turn".into(),
            tools: Vec::new(),
            path: PathBuf::from(format!("/tmp/skills/{}/SKILL.md", name)),
            active,
        }
    }

    #[test]
    fn test_build_skills_prompt_lists_active_only() {
        let skills = vec![
            manifest("code-review", "审查代码", true),
            manifest("news-digest", "新闻摘要", false),
        ];
        let prompt = build_skills_prompt(&skills);
        assert!(prompt.contains("## Skills"));
        assert!(prompt.contains("code-review"));
        assert!(prompt.contains("审查代码"));
        assert!(!prompt.contains("news-digest"));
        assert!(prompt.contains("Mandatory grounding"));
    }

    #[test]
    fn test_build_skills_prompt_empty_when_no_active() {
        assert_eq!(build_skills_prompt(&[]), "");
        let skills = vec![manifest("x", "d", false)];
        assert_eq!(build_skills_prompt(&skills), "");
    }

    #[test]
    fn test_build_skills_prompt_skips_invalid_name() {
        let skills = vec![manifest("bad/../name", "evil", true)];
        assert_eq!(build_skills_prompt(&skills), "");
    }

    #[test]
    fn test_sanitize_strips_control_chars_and_backticks() {
        let dirty = "line1\nline2\r`injected` \u{7f}end";
        let clean = sanitize_prompt_text(dirty);
        assert!(!clean.contains('\n'));
        assert!(!clean.contains('`'));
        assert!(!clean.contains('\u{7f}'));
        assert!(clean.contains("injected"));
    }

    #[test]
    fn test_prompt_includes_suggested_tools() {
        let mut m = manifest("demo", "d", true);
        m.tools = vec!["file_read".into(), "terminal".into()];
        let prompt = build_skills_prompt(&[m]);
        assert!(prompt.contains("Suggested tools: file_read, terminal"));
    }
}
