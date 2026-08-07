//! Skill 技能框架（P3-e）
//!
//! Skill = "提示词 + 工具推荐"的技能包，定义为 `<skills_dir>/<name>/SKILL.md`
//! （markdown + YAML frontmatter，对齐 OpenAI Codex CLI / Claude Skills / AstrBot）。
//!
//! Progressive Disclosure：启动时只解析 frontmatter（name + description），
//! 注入 system prompt 的 "## Skills" 段；LLM 触发 skill 时自己 file_read 完整 SKILL.md。
//! 详见 ADR-0015。

pub mod loader;
pub mod prompt;

use serde::Serialize;
use std::path::PathBuf;

/// Skill 元数据（SKILL.md frontmatter 解析结果 + active 开关）
#[derive(Debug, Clone, Serialize)]
pub struct SkillManifest {
    /// skill 唯一标识（frontmatter `name`，非法/缺失时回退为目录名）
    pub name: String,
    /// 做什么 + 何时用（注入 system prompt）
    pub description: String,
    /// turn（默认）/ session；P3-e 仅记录，不影响行为
    pub duration: String,
    /// 推荐工具列表（仅 prompt 提示，不控制工具挂载 —— 方案 C）
    pub tools: Vec<String>,
    /// SKILL.md 绝对路径
    pub path: PathBuf,
    /// 是否激活（skills.json 控制，缺省 true）
    pub active: bool,
}

/// skill name 合法性校验：`^[\w.-]+$` 且不为 "." / ".."
/// 用于 prompt 注入前过滤与 Web API 路径参数校验（防路径穿越 + prompt injection）
pub fn is_valid_skill_name(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_skill_name() {
        assert!(is_valid_skill_name("code-review"));
        assert!(is_valid_skill_name("news_digest.v2"));
        assert!(!is_valid_skill_name(""));
        assert!(!is_valid_skill_name("."));
        assert!(!is_valid_skill_name(".."));
        assert!(!is_valid_skill_name("../evil"));
        assert!(!is_valid_skill_name("a/b"));
        assert!(!is_valid_skill_name("a b"));
        assert!(!is_valid_skill_name("反引号`"));
    }
}
