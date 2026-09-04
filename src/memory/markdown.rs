use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::provider::{ChatMessage, ChatRequest, ChatResponse, Provider};

/// 加载 Markdown 文件内容。文件不存在时返回空字符串（不报错）。
pub async fn load_md(path: &PathBuf) -> Result<String> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => Ok(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("read {:?}", path)),
    }
}

/// 当文件不存在时，写入默认模板。
pub async fn ensure_template(path: &PathBuf, template: &str) -> Result<()> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        tokio::fs::write(path, template)
            .await
            .with_context(|| format!("write template {:?}", path))?;
    }
    Ok(())
}

pub const SOUL_TEMPLATE: &str = r#"# Personality

<Describe LLAIA's personality>

# Behavior Guidelines

- Be concise and direct, no fluff
- Ask proactively when unsure
- Use relative paths when working; files land under WORKSPACE. Use absolute paths only when writing elsewhere

# Tone

<conversation style>
"#;

pub const USER_TEMPLATE: &str = r#"# Basic Info

- name:

# Identity Binding

- qq:
- email:
- web:

# Preferences

- language: Chinese
"#;

pub const MEMORY_TEMPLATE: &str = r#"# MEMORY

<!-- format: - [YYYY-MM-DD] <entry> -->
"#;

/// 画像文件（SOUL.md / USER.md）是否**尚未填写**：内容为空，或仍是 init 模板原文。
///
/// 忽略首尾空白——模板常量与落盘内容可能只差一个尾换行。用字符串比较而非 md5
/// （reminder 用 md5 是要比任意两次内容差异并当缓存键），这里只与一个已知常量比对。
/// 供 first-run bootstrap 注入判定与 Tail Reminder 门禁共用。
pub fn is_unfilled(content: &str, template: &str) -> bool {
    let c = content.trim();
    c.is_empty() || c == template.trim()
}

/// MEMORY.md 压缩：先备份，再调 LLM 去重压缩，覆写。
pub async fn compress_memory(
    memory_path: &PathBuf,
    provider: &dyn Provider,
    backup_dir: &PathBuf,
    tz: &Option<String>,
) -> Result<()> {
    let content = tokio::fs::read_to_string(memory_path)
        .await
        .with_context(|| format!("read {:?}", memory_path))?;

    tokio::fs::create_dir_all(backup_dir).await.ok();
    let ts = crate::time::now(tz)
        .naive
        .format("%Y%m%d-%H%M%S")
        .to_string();
    let backup_path = backup_dir.join(format!("MEMORY.{}.md", ts));
    tokio::fs::write(&backup_path, &content).await?;

    let system = "You are a memory compactor. Given a list of memory entries, output a deduplicated, compressed version. Keep the same format: '- [YYYY-MM-DD] <entry>'. Remove duplicates and merge related entries. Preserve dates. Output only the list, no commentary.";
    let user = format!("Compress this memory:\n\n{}", content);
    let messages = vec![ChatMessage::system(system), ChatMessage::user(user)];
    let req = ChatRequest {
        messages: &messages,
        tools: None,
        disable_thinking: false,
    };
    let resp: ChatResponse = provider.chat(&req).await?;
    let new_content = resp.text.unwrap_or_default();

    if !new_content.trim().is_empty() {
        tokio::fs::write(memory_path, &new_content).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_load_md_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.md");
        let content = load_md(&path).await.unwrap();
        assert_eq!(content, "");
    }

    #[tokio::test]
    async fn test_load_md_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("x.md");
        tokio::fs::write(&path, "hello").await.unwrap();
        let content = load_md(&path).await.unwrap();
        assert_eq!(content, "hello");
    }

    #[tokio::test]
    async fn test_ensure_template_creates() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("SOUL.md");
        ensure_template(&path, SOUL_TEMPLATE).await.unwrap();
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(content.contains("Personality"));
    }

    #[tokio::test]
    async fn test_ensure_template_no_overwrite() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("SOUL.md");
        tokio::fs::write(&path, "existing").await.unwrap();
        ensure_template(&path, SOUL_TEMPLATE).await.unwrap();
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content, "existing");
    }

    #[test]
    fn test_is_unfilled_covers_template_blank_and_empty() {
        // 模板原文（init 落盘形态）→ 未填写
        assert!(is_unfilled(SOUL_TEMPLATE, SOUL_TEMPLATE));
        // 差一个尾换行/缩进 → 仍判未填写（模板常量与落盘内容可能不逐字节相同）
        assert!(is_unfilled(
            &format!("\n\n{}\n   ", SOUL_TEMPLATE),
            SOUL_TEMPLATE
        ));
        // 空内容（文件缺失时 read_to_string 降级为空串）→ 未填写
        assert!(is_unfilled("", SOUL_TEMPLATE));
        assert!(is_unfilled("   \n ", SOUL_TEMPLATE));
        // 填过任何一处 → 已填写
        assert!(!is_unfilled(
            "# Personality\n\n干活利落的私人助理\n",
            SOUL_TEMPLATE
        ));
    }

    /// 两份画像文件各用各的模板比对，不串台
    #[test]
    fn test_is_unfilled_uses_matching_template() {
        assert!(is_unfilled(USER_TEMPLATE, USER_TEMPLATE));
        assert!(!is_unfilled(USER_TEMPLATE, SOUL_TEMPLATE));
        assert!(!is_unfilled(SOUL_TEMPLATE, USER_TEMPLATE));
    }
}
