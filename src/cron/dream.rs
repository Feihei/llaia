//! 做梦（Dream）编排：两阶段管线（见 ADR-0016）。
//!
//! stage1 蒸馏：阶段专用 dream 会话读增量历史 → 产出草稿文本 → coordinator 写 dream_draft.md（不进上下文）。
//! stage2 整理：基于 draft + 当前 MEMORY.md → 产出完整新 MEMORY.md 内容 → coordinator 备份后覆盖（进上下文）。
//!
//! 两阶段都跑在独立 cron 会话（run_isolated_turn），主会话历史零污染；
//! 两阶段共用稳定 channel（`cron:<id>:dream`）复用同一会话，不会每次触发都新建孤儿会话。
//! 游标增量（messages.id > last_dream_message_id）保证只消化新内容、可续跑、不重放老历史。

use crate::agent::Agent;
use crate::memory::dream as dream_fs;
use crate::memory::sqlite::{MessageRow, SessionStore};
use anyhow::{Context, Result};

/// 每轮做梦最多消化的新消息条数（防单次过大）。
const DREAM_BATCH_LIMIT: i64 = 300;
/// 保留最近几份 .bak 备份。
const DREAM_BACKUP_KEEP: usize = 10;
/// dream 隔离 turn 用的极简 system：不暴露工具、不夹带主 agent 的 SOUL/MEMORY/指令。
/// 推理模型（qwen 深度思考版等）在超大 system + 全套 tools 下会爆量推理撑过顶层超时，
/// 或误调 web_fetch 卡死整轮；dream 是纯文本合成，用最小 system + 关工具可稳定 ~30s 出结果。
const DREAM_SYSTEM_PROMPT: &str =
    "You are LLAIA's memory consolidation engine. Distill conversation history into durable facts. You never call tools. Output only the requested text.";

/// 去掉 LLM 输出可能夹带的 ```markdown / ``` 代码围栏，返回纯内容。
/// 同时裁掉常见的「这是更新后的记忆」之类前导/尾巴说明，尽量只留记忆条目本身。
fn extract_fenced(text: &str) -> String {
    let trimmed = text.trim();
    // 尝试提取最后一个代码围栏块
    if let Some(start) = trimmed.find("```") {
        let after_open = &trimmed[start + 3..];
        // 跳过语言标记行
        let rest = match after_open.find('\n') {
            Some(i) => &after_open[i + 1..],
            None => after_open,
        };
        if let Some(end) = rest.rfind("```") {
            let body = rest[..end].trim();
            if !body.is_empty() {
                return body.to_string();
            }
        }
    }
    trimmed.to_string()
}

/// 把增量消息格式化为喂给 stage1 的文本。
fn format_messages(rows: &[MessageRow]) -> String {
    let mut s = String::new();
    for m in rows {
        let role = match m.role.as_str() {
            "user" => "User",
            "assistant" => "Assistant",
            "tool" => "Tool",
            _ => &m.role,
        };
        s.push_str(&format!("[{}] {}: {}\n", m.created_at, role, m.content));
    }
    s
}

/// stage1 蒸馏 prompt（基于 nanobot dream.md 改造，蒸馏阶段）。
fn stage1_prompt(history_text: &str, existing_draft: &str) -> String {
    format!(
        r#"You are LLAIA's memory consolidation stage 1 (DISTILL). Your job is to read recent conversation history and extract a DRAFT of facts worth remembering long-term. You do NOT edit the user's actual memory file yet — you only produce a candidate draft.

Existing draft so far (carry it forward, do not drop prior good items; merge and dedupe):
```
{existing_draft}
```

Recent new conversation history (messages after the last consolidation cursor):
```
{history_text}
```

Rules:
- Extract only durable, cross-session-useful facts: user preferences, project decisions, conventions, commitments, corrections, environment facts.
- Skip transient chatter, one-off task outputs, and anything already in the existing draft.
- One fact per line. Be concrete and self-contained (a future session must understand it without this conversation).
- Do NOT include dates or bullet prefixes here — just the fact text, one per line.
- If there is genuinely nothing worth remembering, output exactly: NONE

Output ONLY the draft content (the updated list of fact lines). No commentary, no code fences."#,
        existing_draft = if existing_draft.trim().is_empty() {
            "(empty)"
        } else {
            existing_draft
        },
        history_text = if history_text.trim().is_empty() {
            "(no new messages)"
        } else {
            history_text
        },
    )
}

/// stage2 整理 prompt（基于 nanobot dream.md 改造，consolidate 阶段）。
fn stage2_prompt(draft: &str, current_memory: &str) -> String {
    format!(
        r#"You are LLAIA's memory consolidation stage 2 (CONSOLIDATE). Your job is to produce the COMPLETE, updated long-term memory file content, merging the candidate draft into the existing memory while deduplicating and removing stale/contradicted entries.

The memory file format is strict: each entry on its own line as `- [YYYY-MM-DD] <fact>`. Keep the `# MEMORY` title line and the HTML comment line at the top.

Existing memory:
```
{current_memory}
```

Candidate draft extracted from recent conversations (merge these in if they are not already present and not contradicted):
```
{draft}
```

Rules:
- Output the ENTIRE updated memory file content (all surviving old entries + merged new ones).
- Deduplicate: if a draft fact is already covered (even with different wording), do NOT add a near-duplicate.
- Remove entries that are outdated, superseded, or contradicted by newer facts.
- Prefer newer/authoritative info when entries conflict; keep the more specific one.
- Preserve original dates on surviving old entries; date new entries with the current date.
- Keep it concise and MECE. Fewer, higher-quality entries beat a long pile.
- Output ONLY the memory file content. No preamble, no trailing commentary. You may wrap it in a ```markdown fence or not — either is fine."#,
        draft = if draft.trim().is_empty() {
            "(empty)"
        } else {
            draft
        },
        current_memory = current_memory,
    )
}

/// 取得（复用或新建）dream 的稳定 cron 会话。
///
/// 以 `cron:<id>:dream` 作为 channel 锚点，保证每个 dream 任务只有一个持久会话、跨多次
/// 触发复用，而不是每次跑都新建孤儿会话（之前每次触发会新建 uuid 会话，历史碎片化且
/// WebUI 会话列表无限增长）。stage1/stage2 共用同一会话：两阶段各自单轮、prompt 全量
/// 自包含（中间态经 `dream_draft.md` 文件传递），不依赖会话历史，故无需分阶段建会话。
fn acquire_dream_session(store: &SessionStore, task_id: &str) -> Result<i64> {
    let channel = format!("cron:{}:dream", task_id);
    if let Some(id) = store.session_by_channel(&channel)? {
        return Ok(id);
    }
    let uuid = uuid::Uuid::new_v4().to_string();
    store.create_session(&uuid, &channel)
}

/// 执行一次做梦：两阶段 + 游标推进 + 备份 + diff。
///
/// 直接借用 `&mut Agent`（调用方负责持锁：cron 分支 lock 后调用，slash 已持有 &mut）。
/// `manual`=true 时跳过空闲门控（/dream 手动触发）。
/// 返回用户可见摘要（diff 或跳过原因），由调用方决定推送 / 显示。
pub async fn run_dream(
    agent: &mut Agent,
    task_id: &str,
    idle_minutes: u64,
    manual: bool,
) -> Result<String> {
    let workspace = agent.workspace.clone();
    let memory_path = workspace.join("MEMORY.md");
    let backup_dir = workspace.join("MEMORY.backups");
    let store = agent.session_store.clone();
    let draft_path = dream_fs::draft_path(&workspace);

    // 1) 空闲门控（手动触发跳过）
    if !manual && idle_minutes > 0 {
        if let Some(last) = store.last_user_message_time()? {
            if let Ok(last_dt) = chrono::DateTime::parse_from_rfc3339(&last) {
                let elapsed_min = chrono::Utc::now()
                    .signed_duration_since(last_dt.with_timezone(&chrono::Utc))
                    .num_minutes();
                if elapsed_min < idle_minutes as i64 {
                    tracing::info!(
                        task = task_id,
                        elapsed_min,
                        idle_minutes,
                        "dream skipped: not idle long enough"
                    );
                    return Ok(format!(
                        "[dream] 距上次对话仅 {} 分钟（需 ≥{} 分钟空闲），跳过",
                        elapsed_min, idle_minutes
                    ));
                }
            }
        }
    }

    // 2) 增量历史（按游标）
    let cursor = store.get_last_dream_message_id()?;
    let new_rows = store.messages_after(cursor, DREAM_BATCH_LIMIT)?;
    if new_rows.is_empty() {
        tracing::info!(
            task = task_id,
            "dream skipped: no new messages since last consolidation"
        );
        return Ok("[dream] 没有新对话需要整理".into());
    }
    let history_text = format_messages(&new_rows);
    let max_processed_id = new_rows.iter().map(|m| m.id).max().unwrap_or(cursor);

    // 3) stage1 蒸馏 → dream_draft.md
    let dream_session = acquire_dream_session(&store, task_id)?;
    let existing_draft = dream_fs::read_draft(&draft_path).await?;
    let stage1_reply = agent
        .run_isolated_turn_with(
            &stage1_prompt(&history_text, &existing_draft),
            "cron",
            dream_session,
            Some(DREAM_SYSTEM_PROMPT),
            true,
            true,
        )
        .await
        .context("dream stage1 failed")?;
    let draft_content = if stage1_reply.trim() == "NONE" {
        existing_draft
    } else {
        extract_fenced(&stage1_reply)
    };
    dream_fs::write_draft(&draft_path, &draft_content)
        .await
        .context("write dream_draft failed")?;

    // 4) stage2 整理 → MEMORY.md（先备份）
    let current_memory = tokio::fs::read_to_string(&memory_path)
        .await
        .unwrap_or_default();
    let _backup = dream_fs::backup_memory(&memory_path, &backup_dir, DREAM_BACKUP_KEEP)
        .await
        .context("backup MEMORY failed")?;

    let stage2_reply = agent
        .run_isolated_turn_with(
            &stage2_prompt(&draft_content, &current_memory),
            "cron",
            dream_session,
            Some(DREAM_SYSTEM_PROMPT),
            true,
            true,
        )
        .await
        .context("dream stage2 failed")?;
    let new_memory = extract_fenced(&stage2_reply);
    if new_memory.trim().is_empty() {
        tracing::warn!(
            task = task_id,
            "dream stage2 produced empty content, skip write"
        );
        return Ok("[dream] 整理结果为空，已跳过写入（MEMORY 未改动）".into());
    }
    tokio::fs::write(&memory_path, &new_memory)
        .await
        .with_context(|| format!("write MEMORY {:?}", memory_path))?;

    // 5) 推进游标 + 生成 diff 摘要
    store.set_last_dream_message_id(max_processed_id)?;
    let diff = dream_fs::diff_memory(&current_memory, &new_memory);
    tracing::info!(task = task_id, "dream completed");
    Ok(diff)
}
