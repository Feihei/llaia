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
}
