//! First-run Bootstrap（plan `docs/plans/2026-09-04-first-run-bootstrap.md`）
//!
//! 全新 agent 的 `SOUL.md` / `USER.md` 是 `init` 落盘的占位符模板：人格没定义、用户是谁
//! 也不知道，而框架层面没有任何提示让 agent 去补——它不会主动问，用户也不知道可以问。
//!
//! 本模块在**回合起点**做一件事：画像仍是模板时，构造一条 `[bootstrap]` 指令注入到请求
//! 尾部（`Context.bootstrap`），引导 agent 向用户提问并用 `file_edit` 把答案写回画像文件。
//!
//! 设计约束：
//!
//! - **零新状态、自我终止**：判定是「内容 == 模板常量 or 空」的纯字符串比较。agent 一写盘，
//!   指纹即改变、提示自然消失；用户拒答时按指令写一行偏好说明，同样终止。不需要配置项、
//!   斜杠命令或"已引导过"标志位。
//! - **不进 system 前缀**：`system_prompt_base` 由 `init_system_meta` 缓存、全频道共享，
//!   且逐轮字节一致是 KV 缓存命中的前提。bootstrap 生命周期只有几个回合，挂尾部最经济。
//! - **纯函数、无 I/O、无 LLM 调用**：文案构造与门禁全部可单测。

use crate::memory::{is_unfilled, SOUL_TEMPLATE, USER_TEMPLATE};

/// 尚未填写的画像文件清单（空内容或仍是 init 模板原文）。两份都填好 → 空 Vec。
pub fn unfilled_profile_files(soul: &str, user: &str) -> Vec<&'static str> {
    let mut files = Vec::new();
    if is_unfilled(soul, SOUL_TEMPLATE) {
        files.push("SOUL.md");
    }
    if is_unfilled(user, USER_TEMPLATE) {
        files.push("USER.md");
    }
    files
}

/// Tail Reminder 生成门禁：两份画像**都**还是模板/空时不生成。
///
/// 存量 bug 修复：旧门禁是 `!soul.is_empty() || !user.is_empty()`，而模板文本永远非空，
/// 于是全新 agent 的第一回合就会烧一个隔离 LLM turn，让模型从
/// `<Describe LLAIA's personality>` 里"提炼抗漂移要点"。任一侧真填过即照常生成。
pub fn should_generate_reminder(soul: &str, user: &str) -> bool {
    unfilled_profile_files(soul, user).len() < 2
}

/// 构造 `[bootstrap]` 注入文案；画像均已填写时返回 None（此时提示永不残留）。
pub fn bootstrap_note(soul: &str, user: &str) -> Option<String> {
    let files = unfilled_profile_files(soul, user);
    if files.is_empty() {
        return None;
    }
    let mut asks: Vec<&str> = Vec::new();
    if files.contains(&"USER.md") {
        asks.push(
            "who you are talking to (how to address them, which language to reply in, what they mainly work on)",
        );
    }
    if files.contains(&"SOUL.md") {
        asks.push("what personality and speaking style you should have");
    }
    let targets = files.join(" and ");
    let question_hint = asks.join("; and ");

    Some(format!(
        "[bootstrap] Your own setup is incomplete: {targets} still hold the untouched init \
         template, so you do not know who this user is or how to behave. Finish answering the \
         user's actual request first, then append your own questions for: {question_hint}. Ask \
         at most 5 short questions, in the language the user writes in. When the answers come, \
         record them with file_edit on the relative path(s) {targets}, keeping the existing \
         headings. Never re-ask what you already asked in this conversation, and if the user \
         declines to answer, write one line in USER.md saying so instead of pressing them. If a \
         write is refused as outside your scope, tell the user to run /move home."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MEMORY_TEMPLATE;

    fn filled(template: &str, extra: &str) -> String {
        let mut s = template.to_string();
        s.push_str(extra);
        s
    }

    #[test]
    fn test_unfilled_lists_both_on_fresh_install() {
        assert_eq!(
            unfilled_profile_files(SOUL_TEMPLATE, USER_TEMPLATE),
            vec!["SOUL.md", "USER.md"]
        );
        // 文件缺失（读盘降级为空串）同样视为未填写
        assert_eq!(unfilled_profile_files("", ""), vec!["SOUL.md", "USER.md"]);
    }

    #[test]
    fn test_unfilled_is_per_file_and_terminates_once_written() {
        let soul = filled(SOUL_TEMPLATE, "\n# Personality\n\n干活利落\n");
        // 只填了 SOUL → 仅剩 USER.md，且文案点名催 USER.md
        assert_eq!(
            unfilled_profile_files(&soul, USER_TEMPLATE),
            vec!["USER.md"]
        );
        let user = filled(USER_TEMPLATE, "\n- name: feihei\n");
        assert!(unfilled_profile_files(&soul, &user).is_empty());
        assert!(bootstrap_note(&soul, &user).is_none());
    }

    #[test]
    fn test_bootstrap_note_names_files_and_asks_once() {
        let note = bootstrap_note(SOUL_TEMPLATE, USER_TEMPLATE).unwrap();
        assert!(note.starts_with("[bootstrap] "), "前缀约定：{}", note);
        assert!(note.contains("SOUL.md") && note.contains("USER.md"));
        assert!(note.contains("file_edit"));
        // 反唠叨：只问一次 / 拒答即终止 / 越界降级指引
        assert!(note.contains("Never re-ask"));
        assert!(note.contains("declines"));
        assert!(note.contains("/move home"));
        // 只缺 USER.md 时不应把 SOUL.md 也扯进来
        let only_user = bootstrap_note(&filled(SOUL_TEMPLATE, "\nx\n"), USER_TEMPLATE).unwrap();
        assert!(!only_user.contains("SOUL.md"));
        assert!(only_user.contains("USER.md"));
    }

    #[test]
    fn test_reminder_gate_skips_only_when_both_still_templates() {
        // 双模板 / 双空 → 跳过（修掉的存量空转）
        assert!(!should_generate_reminder(SOUL_TEMPLATE, USER_TEMPLATE));
        assert!(!should_generate_reminder("", ""));
        // 任一侧真填过 → 照常生成
        assert!(should_generate_reminder(
            &filled(SOUL_TEMPLATE, "\n# Personality\n\n简洁\n"),
            USER_TEMPLATE
        ));
        assert!(should_generate_reminder(
            SOUL_TEMPLATE,
            &filled(USER_TEMPLATE, "\n- name: feihei\n")
        ));
    }

    /// MEMORY.md 不是画像文件：永不进入待填清单与引导文案
    #[test]
    fn test_memory_template_not_part_of_profile() {
        // 把 MEMORY 模板当 user 传入：它与 USER_TEMPLATE 不同 → 判为"已填写"，清单只剩 SOUL.md
        assert_eq!(
            unfilled_profile_files(SOUL_TEMPLATE, MEMORY_TEMPLATE),
            vec!["SOUL.md"]
        );
        // 内容与各自主模板都不同 → 无提示，MEMORY.md 不会被误催
        assert!(bootstrap_note(MEMORY_TEMPLATE, MEMORY_TEMPLATE).is_none());
    }
}
