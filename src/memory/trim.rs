//! MEMORY.md token 预算裁剪（ADR-0025，hermes 式）。
//!
//! MEMORY.md 全量加载进 system prompt（不懒加载），但受可配置 token 预算约束；
//! 超限时把最旧溢出段经 `compact_provider` 摘要压缩（保留近期条目原文），
//! 无 `compact_provider` 时降级为硬截断、保留末尾预算内条目。
//! SOUL/USER 永留全量、不计入预算（由调用方负责，不在本模块处理）。

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex};

use crate::provider::{ChatMessage, ChatRequest, Provider};

/// 动态 provider 引用类型（与 cli.rs / agent 内 `Option<Arc<dyn Provider>>` 一致）。
type DynProvider = Arc<dyn Provider>;

/// 默认 MEMORY token 预算（与 `[agent.<alias>].memory_token_budget` 一致）。
pub const DEFAULT_MEMORY_TOKEN_BUDGET: usize = 4000;

/// 复用 llaia 全局 token 启发式：字符数 / 4。无真实 tokenizer。
pub fn estimate_tokens(s: &str) -> usize {
    s.chars().count() / 4
}

/// 把 MEMORY.md 文本按空行分段为条目（兼容 `- [date]` 列表与 `# ` 标题）。
/// 段内换行保留，段间空行作为分隔。返回各条目的切片（不拷贝）。
fn split_entries(memory: &str) -> Vec<&str> {
    let mut entries: Vec<&str> = memory
        .split("\n\n")
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .collect();
    // 整段一大块（无空行分隔）时整体作为单条目
    if entries.is_empty() && !memory.trim().is_empty() {
        entries.push(memory.trim());
    }
    entries
}

/// 调用 compact_provider 把最旧溢出段摘要成一段压缩文本。
/// 失败时返回空串（调用方据此降级为硬截断）。
async fn summarize_chunk(provider: &DynProvider, chunk: &str) -> String {
    let system = "You are a memory compactor. Compress the following memory entries into a single concise paragraph preserving key facts, dates, and decisions. Output only the compressed text, no commentary.";
    let user = format!("Compress these older memory entries:\n\n{}", chunk);
    let messages = vec![ChatMessage::system(system), ChatMessage::user(user)];
    let req = ChatRequest {
        messages: &messages,
        tools: None,
        disable_thinking: false,
    };
    match provider.chat(&req).await {
        Ok(resp) => resp.text.unwrap_or_default(),
        Err(e) => {
            tracing::warn!(error = %e, "memory trim summarize failed, falling back to hard truncation");
            String::new()
        }
    }
}

/// 硬截断到预算内：丢弃开头部分，仅保留末尾（预算×4）字符。
fn hard_truncate_tail(text: &str, budget: usize) -> String {
    let max_chars = budget.saturating_mul(4);
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let skip = total - max_chars;
    let tail: String = text.chars().skip(skip).collect();
    format!("… (earlier content truncated)\n{}", tail)
}

/// 实际裁剪逻辑（不含缓存）。
async fn compute_trim(
    memory: &str,
    budget: usize,
    compact_provider: Option<&DynProvider>,
) -> String {
    let entries = split_entries(memory);

    // 从后往前贪心累加近期条目；首条（末尾）总是先加入（即便单条巨长也保留、稍后截断），
    // 之后每条在「加入后仍不超预算」时才保留，否则停止（旧段交给摘要/丢弃）。
    let mut recent: Vec<&str> = Vec::new();
    let mut recent_tokens = 0usize;
    for &entry in entries.iter().rev() {
        let t = estimate_tokens(entry) + 1; // +1 近似段间空行
        if !recent.is_empty() && recent_tokens + t > budget {
            break;
        }
        recent_tokens += t;
        recent.push(entry);
    }
    // recent 是「从后往前」的顺序，反转回原文顺序
    recent.reverse();
    let recent_start = entries.len().saturating_sub(recent.len());
    let oldest: Vec<&str> = entries[..recent_start].to_vec();
    let oldest_text = oldest.join("\n\n");

    // recent 可能本身仍超预算（单条巨长条目），硬截断到预算内（保留末尾）
    let mut recent_text = recent.join("\n\n");
    if estimate_tokens(&recent_text) > budget {
        recent_text = hard_truncate_tail(&recent_text, budget);
    }

    // 没有可压缩的旧段（仅单条巨长条目）→ 直接返回（已截断的）recent
    if oldest.is_empty() {
        return recent_text;
    }

    match compact_provider {
        Some(provider) => {
            let summary = summarize_chunk(provider, &oldest_text).await;
            if summary.trim().is_empty() {
                // 摘要失败 → 丢弃旧段，仅保留近期
                return recent_text;
            }
            // 压缩前缀 + 近期原文（不重复 # MEMORY 标题，外层已有包裹）
            format!(
                "> early memory compressed: {}\n>\n{}",
                summary.trim(),
                recent_text
            )
        }
        None => {
            // 无 compact_provider → 硬截断：丢弃旧段，保留末尾预算内条目
            recent_text
        }
    }
}

// 内容哈希缓存：相同 MEMORY 内容 + 预算 + 是否含 provider → 免重复 LLM 摘要，
// 避免 system prompt 抖动（同内容每次返回一致结果）。
type CacheKey = (u64, usize, bool);

fn hash_content(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

static TRIM_CACHE: LazyLock<Mutex<HashMap<CacheKey, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 在 system prompt 预算内裁剪 MEMORY.md 内容。
///
/// - 不超限 → 原样返回（最常见路径，无摘要、无抖动）。
/// - 超限且 `compact_provider` 存在 → 旧段经 LLM 摘要成前缀，近期原文保留。
/// - 超限且无 `compact_provider` → 硬截断，仅保留末尾预算内条目。
///
/// 结果按 (内容 hash, 预算, 是否含 provider) 缓存，避免重复摘要。
pub async fn trim_memory_to_budget(
    memory: &str,
    budget: usize,
    compact_provider: Option<&DynProvider>,
) -> String {
    // 不超限直接原样返回（最常见路径，无需摘要）
    if estimate_tokens(memory) <= budget {
        return memory.to_string();
    }

    let key = (hash_content(memory), budget, compact_provider.is_some());
    if let Some(cached) = TRIM_CACHE.lock().unwrap().get(&key) {
        return cached.clone();
    }

    // 计算（可能调 LLM）在锁外完成，仅把结果放回缓存时加锁
    let trimmed = compute_trim(memory, budget, compact_provider).await;
    TRIM_CACHE.lock().unwrap().insert(key, trimmed.clone());
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatResponse, Provider};
    use async_trait::async_trait;

    struct MockSummarizeProvider;
    #[async_trait]
    impl Provider for MockSummarizeProvider {
        async fn chat(
            &self,
            req: &crate::provider::ChatRequest<'_>,
        ) -> anyhow::Result<crate::provider::ChatResponse> {
            // 简单回显：把 user 消息首行截成摘要
            let text = req.messages[1].content.as_text();
            let summary = text.lines().take(2).collect::<Vec<_>>().join(" ");
            Ok(ChatResponse {
                text: Some(format!("SUMMARY[{}]", summary)),
                tool_calls: vec![],
                usage: None,
                finish_reason: None,
            })
        }
        async fn chat_stream(
            &self,
            _req: &crate::provider::ChatRequest<'_>,
        ) -> futures_util::stream::BoxStream<'_, anyhow::Result<crate::provider::StreamEvent>>
        {
            unreachable!()
        }
        fn native_tool_calling(&self) -> bool {
            true
        }
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(&"a".repeat(40)), 10);
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn test_split_entries() {
        let mem = "# MEMORY\n\n- [2024-01-01] a\n\n- [2024-01-02] b\n\n- [2024-01-03] c";
        let entries = split_entries(mem);
        assert_eq!(entries.len(), 4); // 标题 + 3 条
        assert!(entries[0].contains("# MEMORY"));
        assert!(entries[3].contains("c"));
    }

    #[test]
    fn test_split_entries_single_block() {
        let mem = "this is one big block without blank lines";
        let entries = split_entries(mem);
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn test_no_overflow_returns_original() {
        let mem = "short memory";
        let out = trim_memory_to_budget(mem, 4000, None).await;
        assert_eq!(out, mem);
    }

    #[tokio::test]
    async fn test_overflow_no_provider_hard_truncates_old() {
        // 构造远超预算的 MEMORY：标题 + 多条长条目
        let mut mem = String::from("# MEMORY\n\n");
        for i in 0..50 {
            mem.push_str(&format!(
                "- [2024-01-{:02}] old entry number {} with padding text to grow size\n\n",
                i + 1,
                i
            ));
        }
        mem.push_str("- [2024-06-01] RECENT IMPORTANT FACT that must be kept\n");
        // 预算很小（约 50 token = 200 字符），必然超限
        let out = trim_memory_to_budget(&mem, 50, None).await;
        // 无 provider → 丢弃旧段，仅保留末尾预算内条目
        assert!(!out.contains("old entry number 1"));
        assert!(out.contains("RECENT IMPORTANT FACT"));
        // 不引入压缩前缀标记（无 provider）
        assert!(!out.contains("early memory compressed"));
    }

    #[tokio::test]
    async fn test_overflow_with_provider_summarizes_old() {
        let mut mem = String::from("# MEMORY\n\n");
        for i in 0..50 {
            mem.push_str(&format!(
                "- [2024-01-{:02}] old entry number {} with padding text to grow size\n\n",
                i + 1,
                i
            ));
        }
        mem.push_str("- [2024-06-01] RECENT IMPORTANT FACT that must be kept\n");
        let provider: DynProvider = Arc::new(MockSummarizeProvider);
        let out = trim_memory_to_budget(&mem, 50, Some(&provider)).await;
        // 有 provider → 旧段被摘要，压缩前缀存在
        assert!(out.contains("early memory compressed"));
        assert!(out.contains("SUMMARY["));
        // 近期条目原文保留
        assert!(out.contains("RECENT IMPORTANT FACT"));
        // 旧条目原文已被压缩掉（不再逐条出现）
        assert!(!out.contains("- [2024-01-01]"));
    }

    #[tokio::test]
    async fn test_single_giant_entry_hard_truncates_tail() {
        let mem = format!("# MEMORY\n\n{}", "x".repeat(10000));
        let out = trim_memory_to_budget(&mem, 100, None).await;
        // 单条巨长 → 硬截断保留末尾
        assert!(out.contains("earlier content truncated"));
        assert!(out.len() < mem.len());
        // 末尾字符保留
        assert!(out.ends_with('x'));
    }

    #[tokio::test]
    async fn test_cache_stable_for_same_content() {
        let mem = "# MEMORY\n\n- [2024-01-01] a very long entry to exceed budget ".repeat(40);
        let provider: DynProvider = Arc::new(MockSummarizeProvider);
        let first = trim_memory_to_budget(&mem, 50, Some(&provider)).await;
        let second = trim_memory_to_budget(&mem, 50, Some(&provider)).await;
        // 同内容 → 同一缓存结果（不重复摘要、无抖动）
        assert_eq!(first, second);
    }
}
