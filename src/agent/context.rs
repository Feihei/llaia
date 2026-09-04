use crate::provider::{
    ChatMessage, ChatRequest, ChatResponse, ContentPart, MessageContent, Provider, Role,
};
use anyhow::Result;

/// 压缩时工具消息内容保留的最大字符数（超出截断，完整内容已在 sqlite 留底）。
const TOOL_TRIM_CAP: usize = 500;

/// 当前会话的上下文窗口。
pub struct Context {
    pub system: String,
    pub history: Vec<ChatMessage>,
    pub summary: Option<String>,
    /// 规划后执行（ADR-0024）的当前 todo 清单文本，每轮由 agent 在 turn 起点写入；
    /// 作为 Runtime Context 追加到尾部（与 status_bar 同区，不进 system 前缀，KV 缓存友好）。
    pub todo_state: Option<String>,
    /// 环境探测（P5 E1）的注入文本：进程启动时对 main agent 探测一次、
    /// `/env` 命令手动刷新。与 todo 同区，不进 system 前缀（KV 缓存友好）。
    pub env_state: Option<String>,
    /// 当前 active 任务线信息（ADR-0031）：任务线内注入「任务名 + 绑定目录」，
    /// 让模型知道自己身在哪个任务；通用线为 None。与 todo 同区（KV 缓存友好）。
    pub task_state: Option<String>,
    /// 自动生成的 Tail Reminder（P6）：LLM 从 SOUL+USER 提炼的抗漂移要点，
    /// 存 `workspace/reminder.md`，hash 失配时后台重生成。排在 todo/env/task 尾部
    /// 消息之后，仅 bootstrap 指令在其后（离生成点最近，注意力最强）。
    pub reminder: Option<String>,
    /// First-run Bootstrap：画像文件仍是 init 模板时的一次性引导指令（问用户要信息并写回
    /// SOUL.md / USER.md）。排在 reminder 之后成为最后一条消息——t=0 的关键行为指令要离
    /// 生成点最近。逐轮现算、不进 history/sqlite，agent 写盘后指纹改变即自动消失；
    /// 绝不进 system 前缀（KV 缓存友好）。
    pub bootstrap: Option<String>,
}

impl Context {
    pub fn new(system: String) -> Self {
        Self {
            system,
            history: Vec::new(),
            summary: None,
            todo_state: None,
            env_state: None,
            task_state: None,
            reminder: None,
            bootstrap: None,
        }
    }

    pub fn push(&mut self, msg: ChatMessage) {
        self.history.push(msg);
    }

    /// 组装送给 provider 的消息列表。
    ///
    /// `tz` 为 `[runtime].timezone`，用于生成末尾的运行时状态栏（ADR-0017）。
    /// 状态栏不写入 `history`：每轮现算、只挂在尾部，system 前缀
    /// （SOUL/USER/MEMORY/Skills）逐轮字节一致，KV cache 才能命中。
    pub fn to_messages(&self, tz: &Option<String>) -> Vec<ChatMessage> {
        let mut msgs = vec![ChatMessage::system(&self.system)];
        if let Some(s) = &self.summary {
            msgs.push(ChatMessage::system(format!(
                "[Previous conversation summary]\n{}",
                s
            )));
        }
        msgs.extend(self.history.iter().cloned());
        msgs.push(ChatMessage::user(crate::time::status_bar(tz)));
        // 规划后执行（ADR-0024）：当前 todo 清单作为 Runtime Context 追加在尾部，
        // 让模型每轮都知道"还差哪几步"。无清单时跳过，不影响 system 前缀稳定性。
        if let Some(todo) = &self.todo_state {
            if !todo.is_empty() {
                msgs.push(ChatMessage::user(todo.clone()));
            }
        }
        // 环境探测（P5 E1）：本机工具链快照，让模型知道环境里有什么、避免建议不存在的工具。
        // 启动探测一次 + /env 手动刷新；不进 system 前缀（KV 缓存友好）。
        if let Some(env) = &self.env_state {
            if !env.is_empty() {
                msgs.push(ChatMessage::user(env.clone()));
            }
        }
        // 当前任务线（ADR-0031）：任务线内让模型知道自己身在哪个任务、绑定哪个目录。
        if let Some(task) = &self.task_state {
            if !task.is_empty() {
                msgs.push(ChatMessage::user(task.clone()));
            }
        }
        // Tail Reminder（P6）：抗风格/身份漂移的要点重申。
        if let Some(rem) = &self.reminder {
            if !rem.is_empty() {
                msgs.push(ChatMessage::user(rem.clone()));
            }
        }
        // First-run Bootstrap：画像仍是 init 模板时的引导指令，放最末——t=0 的行为指令
        // 优先级最高，要离生成点最近。仅在 agent 写盘 SOUL.md / USER.md 后消失。
        if let Some(bs) = &self.bootstrap {
            if !bs.is_empty() {
                msgs.push(ChatMessage::user(bs.clone()));
            }
        }
        msgs
    }

    /// 估算上下文 token 用量（chars/4 启发式，与 `memory/trim.rs` 口径一致）。
    ///
    /// 覆盖 `to_messages` 实际发送给 provider 的全部文本：system + summary +
    /// history + 尾部状态栏 + todo/env Runtime Context，另加每条消息的
    /// JSON 结构开销（role/header 等，近似 8 token/条）。tool definitions 是
    /// 常量不随对话增长，由调用方按需追加（`/stats` 用 `tools.specs()` 序列化
    /// 估算；压缩判定不依赖它——相对变化不受影响）。
    pub fn estimate_tokens(&self) -> usize {
        let msgs = self.to_messages(&None);
        let text_tokens: usize = msgs
            .iter()
            .map(|m| m.content.as_text().chars().count() / 4)
            .sum();
        text_tokens + msgs.len() * 8
    }

    pub fn clear(&mut self) {
        self.history.clear();
    }

    /// 是否需要压缩。`extra_tokens` 为 estimate 未计入但同样占用窗口的部分（tool definitions
    /// 序列化估算，见 `Agent::tool_def_tokens`），无工具时传 0。加入它才能让「是否触发压缩」
    /// 的判断口径与实际请求（messages + tools）一致，消除小窗口模型上的低估错配。
    pub fn needs_compaction(
        &self,
        context_size: usize,
        threshold: f64,
        extra_tokens: usize,
    ) -> bool {
        let current = self.estimate_tokens() + extra_tokens;
        (current as f64 / context_size as f64) > threshold
    }

    /// 丢掉 `history` 最前面的一个「回合单元」，保证不产生孤儿 tool 消息。
    ///
    /// 单元定义：若首条为带 `tool_calls` 的 assistant，则连同其后连续的 tool 结果
    /// 一起丢（原生工具调用必须成对，否则严格端点直接 400）；否则只丢首条。丢完若
    /// 头部露出孤儿 tool 消息，一并剔除。返回 `true` 表示确实丢了内容且仍留有消息；
    /// 返回 `false` 表示无法再丢（只剩一个单元、丢了就空，或本就为空）。
    fn drop_oldest_unit(&mut self) -> bool {
        if self.history.len() <= 1 {
            return false;
        }
        let mut unit = 1;
        if self.history[0].tool_calls.is_some() {
            while unit < self.history.len() && self.history[unit].role == Role::Tool {
                unit += 1;
            }
        }
        // 首单元即整个 history：丢了就空，保留它交给上层截断兜底。
        if unit >= self.history.len() {
            return false;
        }
        self.history.drain(..unit);
        while self
            .history
            .first()
            .map(|m| m.role == Role::Tool)
            .unwrap_or(false)
        {
            self.history.remove(0);
        }
        !self.history.is_empty()
    }

    /// 廉价硬截断：把上下文（system + summary + history + 尾部注入）压进 `budget` 以内。
    ///
    /// 防御性末级手段——LLM 压缩（`compact`）只保证收敛到 `keep_recent` 条，不保证这截
    /// 近期尾部本身塞得下极小窗口（例如 /provider 切到小 context_size 模型）。两步：
    /// (1) 从最旧开始成对丢回合单元，绝不留孤儿 tool 消息，至少保留最新一个单元；
    /// (2) 若单个超长消息仍超预算，截断最长的那条正文补足。均为纯字符串操作，不调 LLM。
    ///
    /// `budget` 应为「模型窗口 − tool definitions」的实际 messages 额度（由调用方扣除，
    /// 见 `Agent::maybe_auto_compact`）。当 system 前缀 + summary 自身即超预算（固定地板，
    /// 非本方法职责）时就此收手，不会清空历史。
    pub fn hard_truncate_to_fit(&mut self, budget: usize) {
        while self.estimate_tokens() > budget {
            if !self.drop_oldest_unit() {
                break;
            }
        }
        if self.estimate_tokens() <= budget {
            return;
        }
        // 兜底：截断最长的单条文本消息。estimate 用 chars/4，故每减 1 token ≈ 砍 4 字符。
        const MIN_KEEP_CHARS: usize = 40;
        let total = self.estimate_tokens();
        let longest = self
            .history
            .iter()
            .enumerate()
            .max_by_key(|(_, m)| m.content.as_text().chars().count())
            .map(|(i, _)| i);
        if let Some(i) = longest {
            let text = self.history[i].content.as_text();
            let cur = text.chars().count();
            // 仅当「去掉这条正文后其余能塞进预算」时才动手——否则是固定地板（system + tools
            // + summary 独占窗口），截历史无济于事，反而毁掉最后的信息，故保留原文。
            if total.saturating_sub(cur / 4) >= budget {
                return;
            }
            let over = total - budget;
            let remove = over.saturating_mul(4).saturating_add(64);
            let keep = cur.saturating_sub(remove).max(MIN_KEEP_CHARS);
            if keep < cur {
                let head: String = text.chars().take(keep).collect();
                self.history[i].content =
                    MessageContent::Text(format!("{}…[truncated to fit context window]", head));
            }
        }
    }

    /// 压缩上下文：先跑廉价抽取式归一化（不调 LLM），若仍在预算内则跳过 LLM 摘要；
    /// 否则摘要旧消息、保留最近 `keep_recent` 条 + 首条用户消息锚点（ADR-0004）。
    ///
    /// 返回是否真的调了 LLM（cheap-first 命中时为 `false`，供调用方日志/统计）。
    /// `token_budget` 为压缩目标 token 上限，通常用 `context_size`。
    pub async fn compact(
        &mut self,
        provider: &dyn Provider,
        keep_recent: usize,
        token_budget: usize,
    ) -> Result<bool> {
        // 1) 廉价抽取式归一化（不调 LLM，每次必跑）
        self.cheap_normalize();

        // 2) cheap-first：归一化后已回到预算内则跳过 LLM，
        //    保留 summary 前缀稳定（KV cache 友好）也省一次调用
        if self.history.len() <= keep_recent || self.estimate_tokens() <= token_budget {
            return Ok(false);
        }

        // 3) 重要性锚点（ADR-0004「首条用户消息留」）：落在 to_compress 区的首条用户消息
        //    提出来前置到 to_keep，且不进摘要 dump
        let first_user_idx = self.history.iter().position(|m| m.role == Role::User);
        let anchor = first_user_idx
            .filter(|&i| i < self.history.len() - keep_recent)
            .map(|i| self.history[i].clone());

        let split = self.history.len() - keep_recent;
        let mut to_compress: Vec<ChatMessage> = self.history[..split].to_vec();
        let mut to_keep: Vec<ChatMessage> = self.history[split..].to_vec();

        if let Some(a) = anchor {
            if let Some(i) = first_user_idx {
                to_compress.remove(i);
            }
            to_keep.insert(0, a);
        }

        // 4) 摘要 dump：工具消息只给一行归档注记，不让大段工具输出进 LLM
        let mut dump = String::new();
        for m in &to_compress {
            if m.role == Role::Tool {
                let t = m.content.as_text();
                let note = if t.chars().count() > 80 {
                    format!("{}…", t.chars().take(80).collect::<String>())
                } else {
                    t
                };
                dump.push_str(&format!("[tool] (result archived) {}\n", note));
                continue;
            }
            let role_str = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool", // 不可达，上面已 continue
            };
            dump.push_str(&format!("[{}] {}\n", role_str, m.content.as_text()));
        }

        let system = "You are a conversation summarizer. Summarize the following conversation into a concise paragraph preserving key facts, decisions, and context. Output only the summary.";
        let messages = vec![ChatMessage::system(system), ChatMessage::user(dump)];
        let req = ChatRequest {
            messages: &messages,
            tools: None,
            disable_thinking: false,
        };
        let resp: ChatResponse = provider.chat(&req).await?;
        let summary = resp.text.unwrap_or_default();

        let new_summary = match &self.summary {
            Some(old) => format!("{}\n\n[Later]\n{}", old, summary),
            None => summary,
        };
        self.summary = Some(new_summary);

        // to_keep 已是 cheap_normalize 后的内容（图片已降级、工具已截断），直接采用
        self.history = to_keep;
        Ok(true)
    }

    /// 廉价抽取式归一化（不调 LLM）：丢弃空消息、图片降级、工具消息截断、连续重复去重。
    /// 在 `compact` 每轮开头对整段 history 跑一次，幂等。
    fn cheap_normalize(&mut self) {
        let mut out: Vec<ChatMessage> = Vec::with_capacity(self.history.len());
        for mut m in self.history.drain(..) {
            let text = m.content.as_text();
            // 丢弃空消息。两个例外：tool 结果（需与 tool_call_id 配对）与带
            // tool_calls 的 assistant 消息——原生工具调用常见「零文本 + tool_calls」
            // 形态，丢掉会产生孤儿 tool 消息，违反 OpenAI 消息结构（严格端点 400）。
            if text.trim().is_empty() && m.role != Role::Tool && m.tool_calls.is_none() {
                continue;
            }
            // 多模态图片降级为文本占位（省 token，图片信息已由 vision 模型描述进入上下文）
            if m.content.has_image() {
                if let MessageContent::Multimodal(parts) = &m.content {
                    let t = parts
                        .iter()
                        .map(|p| match p {
                            ContentPart::Text { text } => text.clone(),
                            ContentPart::ImageUrl { .. } => "[image]".to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    m.content = MessageContent::Text(t);
                }
            }
            // 工具消息截断到上限（完整内容已在 sqlite 留底，ADR-0004「工具调用结果可丢」）
            if m.role == Role::Tool {
                let t = m.content.as_text();
                if t.chars().count() > TOOL_TRIM_CAP {
                    let head: String = t.chars().take(TOOL_TRIM_CAP).collect();
                    m.content = MessageContent::Text(format!(
                        "{}…[truncated; full result is in the session history]",
                        head
                    ));
                }
            }
            // 连续重复用户消息去重（只留一条）
            if m.role == Role::User {
                if let Some(last) = out.last() {
                    if last.role == Role::User && last.content.as_text() == m.content.as_text() {
                        continue;
                    }
                }
            }
            out.push(m);
        }
        self.history = out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Role;

    #[test]
    fn test_to_messages_includes_system() {
        let ctx = Context::new("SOUL".into());
        let msgs = ctx.to_messages(&None);
        // system + 状态栏
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::System);
    }

    #[test]
    fn test_summary_inserted() {
        let mut ctx = Context::new("SOUL".into());
        ctx.summary = Some("old stuff".into());
        ctx.push(ChatMessage::user("hi"));
        let msgs = ctx.to_messages(&None);
        // system + summary + history + 状态栏
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[1].role, Role::System);
    }

    #[test]
    fn test_status_bar_is_last_and_not_persisted() {
        let mut ctx = Context::new("SOUL".into());
        ctx.push(ChatMessage::user("hi"));
        let msgs = ctx.to_messages(&Some("Asia/Shanghai".into()));
        let last = msgs.last().unwrap();
        assert_eq!(last.role, Role::User);
        assert!(last.content.as_text().contains("Asia/Shanghai"));
        // 状态栏只在 to_messages 里现算，不落进 history
        assert_eq!(ctx.history.len(), 1);
    }

    #[test]
    fn test_reminder_injected_last() {
        // 回归：Tail Reminder（P6）必须是最后一条消息（离生成点最近，注意力最强）
        let mut ctx = Context::new("SOUL".into());
        ctx.push(ChatMessage::user("hi"));
        ctx.todo_state = Some("[todo]".into());
        ctx.env_state = Some("[env]".into());
        ctx.reminder = Some("简洁口语化".into());
        let msgs = ctx.to_messages(&None);
        assert_eq!(
            msgs.last().unwrap().content.as_text(),
            "简洁口语化",
            "reminder should be placed after status bar/todo/env"
        );
    }

    /// 尾部注入次序：状态栏 → todo → env → task → reminder → bootstrap（t=0 指令最末）
    #[test]
    fn test_bootstrap_injected_after_reminder() {
        let mut ctx = Context::new("SOUL".into());
        ctx.push(ChatMessage::user("hi"));
        ctx.todo_state = Some("[todo]".into());
        ctx.env_state = Some("[env]".into());
        ctx.task_state = Some("[task]".into());
        ctx.reminder = Some("[reminder] 简洁".into());
        ctx.bootstrap = Some("[bootstrap] 填写画像".into());
        let msgs = ctx.to_messages(&None);
        let tail: Vec<String> = msgs[msgs.len() - 5..]
            .iter()
            .map(|m| m.content.as_text())
            .collect();
        assert_eq!(
            tail,
            vec![
                "[todo]",
                "[env]",
                "[task]",
                "[reminder] 简洁",
                "[bootstrap] 填写画像"
            ],
            "bootstrap 必须排在 reminder 之后（末 5 条之前的那条是状态栏）"
        );
    }

    #[test]
    fn test_bootstrap_skipped_when_none_or_empty() {
        let mut ctx = Context::new("SOUL".into());
        ctx.push(ChatMessage::user("hi"));
        ctx.reminder = Some("[reminder] 简洁".into());
        ctx.bootstrap = None;
        assert_eq!(
            ctx.to_messages(&None).last().unwrap().content.as_text(),
            "[reminder] 简洁",
            "无 bootstrap 时 reminder 回到末位"
        );
        ctx.bootstrap = Some(String::new());
        assert_eq!(
            ctx.to_messages(&None).last().unwrap().content.as_text(),
            "[reminder] 简洁"
        );
    }

    #[test]
    fn test_reminder_skipped_when_none_or_empty() {
        let mut ctx = Context::new("SOUL".into());
        ctx.push(ChatMessage::user("hi"));
        ctx.reminder = None;
        assert_eq!(ctx.to_messages(&None).last().unwrap().role, Role::User);
        ctx.reminder = Some(String::new());
        assert_eq!(ctx.to_messages(&None).last().unwrap().role, Role::User);
    }

    #[test]
    fn test_env_state_injected_when_set() {
        let mut ctx = Context::new("SOUL".into());
        ctx.push(ChatMessage::user("hi"));
        ctx.env_state = Some("[env] python 3.13.2 · node 22.22.2".into());
        let msgs = ctx.to_messages(&None);
        let env_msg = msgs
            .iter()
            .find(|m| m.content.as_text().contains("[env] python 3.13.2"));
        assert!(env_msg.is_some(), "env line should be injected");
    }

    #[test]
    fn test_env_state_skipped_when_none() {
        let mut ctx = Context::new("SOUL".into());
        ctx.env_state = None;
        let msgs = ctx.to_messages(&None);
        assert!(!msgs.iter().any(|m| m.content.as_text().contains("[env]")));
    }

    #[test]
    fn test_system_prefix_stable_across_turns() {
        // 缓存友好性回归：前缀部分逐轮必须字节一致，只有尾部状态栏会变
        let mut ctx = Context::new("SOUL".into());
        ctx.push(ChatMessage::user("hi"));
        let a = ctx.to_messages(&None);
        let b = ctx.to_messages(&None);
        assert_eq!(
            a[..a.len() - 1]
                .iter()
                .map(|m| m.content.as_text())
                .collect::<Vec<_>>(),
            b[..b.len() - 1]
                .iter()
                .map(|m| m.content.as_text())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_token_estimate() {
        let mut ctx = Context::new("a".repeat(40));
        ctx.push(ChatMessage::user("b".repeat(40)));
        // system(10) + history(10) + 尾部状态栏 + 每条消息结构开销(8/条)
        let est = ctx.estimate_tokens();
        assert!(
            est >= 20,
            "at least system+history text tokens should be included"
        );
        assert!(
            est > 20,
            "status bar and per-message overhead should be counted in the estimate"
        );
    }

    #[test]
    fn test_token_estimate_includes_runtime_context() {
        // 回归：todo/env 等 Runtime Context 必须计入估算（曾漏掉导致 /stats 低估）
        let mut ctx = Context::new("a".repeat(40));
        ctx.push(ChatMessage::user("b".repeat(40)));
        let base = ctx.estimate_tokens();
        ctx.todo_state = Some("c".repeat(400));
        let with_todo = ctx.estimate_tokens();
        // 400 chars/4 = 100 tokens + 1 条消息结构开销 8
        assert!(
            with_todo > base + 90,
            "todo list should be counted in the token estimate"
        );
    }

    #[test]
    fn test_needs_compaction() {
        let mut ctx = Context::new("a".repeat(80));
        ctx.push(ChatMessage::user("b".repeat(80)));
        // 估算含尾部状态栏 + 结构开销，用相对值断言避免依赖具体文本长度
        let est = ctx.estimate_tokens();
        assert!(ctx.needs_compaction(est * 2, 0.3, 0));
        assert!(!ctx.needs_compaction(est * 2, 0.9, 0));
    }
}

#[cfg(test)]
mod compact_tests {
    use super::*;
    use crate::provider::{ChatRequest, ChatResponse, Provider, StreamEvent, ToolCall};
    use async_trait::async_trait;
    use serde_json::json;

    struct MockProvider;
    #[async_trait]
    impl Provider for MockProvider {
        async fn chat(&self, _req: &ChatRequest<'_>) -> Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some("summary of old".into()),
                tool_calls: vec![],
                usage: None,
                finish_reason: None,
            })
        }
        async fn chat_stream(
            &self,
            _req: &ChatRequest<'_>,
        ) -> futures_util::stream::BoxStream<'_, Result<StreamEvent>> {
            let s = async_stream::try_stream! {
                yield StreamEvent::TextDelta("summary of old".into());
                yield StreamEvent::Done;
            };
            Box::pin(s)
        }
        fn native_tool_calling(&self) -> bool {
            true
        }
    }

    /// 超出预算 → 真调 LLM：history 收敛到 keep_recent，summary 生成。
    #[tokio::test]
    async fn test_compact_runs_llm_when_over_budget() {
        let mut ctx = Context::new("SOUL".into());
        for i in 0..10 {
            ctx.push(ChatMessage::user(format!("msg {}", i).repeat(40)));
        }
        // 10 条 × ~160 字符 ≈ 400 token，budget=100 → 必超
        let used_llm = ctx.compact(&MockProvider, 3, 100).await.unwrap();
        assert!(used_llm);
        // 首条用户消息作为锚点被前置保留 → 长度 = keep_recent + 1
        assert_eq!(ctx.history.len(), 4);
        assert!(ctx.history[0].content.as_text().contains("msg 0"));
        assert!(ctx.summary.is_some());
        assert!(ctx.summary.as_ref().unwrap().contains("summary of old"));
    }

    /// cheap-first：归一化后已回到预算内 → 不调 LLM（返回 false），history 不动。
    #[tokio::test]
    async fn test_compact_skips_llm_under_budget() {
        let mut ctx = Context::new("SOUL".into());
        for i in 0..10 {
            ctx.push(ChatMessage::user(format!("msg {}", i))); // 小消息
        }
        let used_llm = ctx.compact(&MockProvider, 3, 10_000).await.unwrap();
        assert!(!used_llm);
        assert_eq!(ctx.history.len(), 10);
        assert!(ctx.summary.is_none());
    }

    /// ADR-0004「首条用户消息留」：首条用户消息永不被摘要掉。
    #[tokio::test]
    async fn test_compact_preserves_first_user_message() {
        let mut ctx = Context::new("SOUL".into());
        ctx.push(ChatMessage::user(
            "IMPORTANT initial instruction".repeat(20),
        ));
        for i in 0..10 {
            ctx.push(ChatMessage::user(format!("msg {}", i).repeat(40)));
        }
        ctx.compact(&MockProvider, 3, 100).await.unwrap();
        let first = ctx.history.first().expect("history non-empty");
        assert_eq!(first.role, Role::User);
        assert!(first
            .content
            .as_text()
            .contains("IMPORTANT initial instruction"));
    }

    /// 工具消息截断：大段工具输出被砍到 TOOL_TRIM_CAP 内。
    #[tokio::test]
    async fn test_compact_trims_tool_messages() {
        let mut ctx = Context::new("SOUL".into());
        ctx.push(ChatMessage::assistant("let me check"));
        ctx.push(ChatMessage::tool("x".repeat(5000), "call_1"));
        ctx.push(ChatMessage::user("result?"));
        // budget 极小 → 走 LLM 路径也会先归一化截断工具消息
        ctx.compact(&MockProvider, 3, 1).await.unwrap();
        let tool_msg = ctx
            .history
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("tool message kept");
        assert!(tool_msg.content.as_text().chars().count() <= TOOL_TRIM_CAP + 60);
        assert!(tool_msg.content.as_text().contains("truncated"));
    }

    /// 回归：原生工具调用常见「零文本 + tool_calls」的 assistant 消息，
    /// cheap_normalize 不得丢弃——否则 tool 消息成孤儿，违反 OpenAI 消息结构。
    #[tokio::test]
    async fn test_compact_keeps_empty_assistant_with_tool_calls() {
        let mut ctx = Context::new("SOUL".into());
        ctx.push(ChatMessage::user("do something"));
        ctx.push(ChatMessage::assistant_with_tools(
            "",
            vec![ToolCall {
                id: "c1".into(),
                name: "file_read".into(),
                arguments: json!({"path": "/tmp"}),
            }],
        ));
        ctx.push(ChatMessage::tool("result", "c1"));
        // budget 充裕 → 不进 LLM，但 cheap_normalize 仍跑
        ctx.compact(&MockProvider, 3, 10_000).await.unwrap();
        let assistant = ctx
            .history
            .iter()
            .find(|m| m.role == Role::Assistant)
            .expect("empty assistant(tool_calls) must survive cheap_normalize");
        assert!(assistant.tool_calls.is_some());
    }

    /// 廉价去重：连续重复用户消息只留一条。
    #[tokio::test]
    async fn test_compact_dedup_consecutive_user() {
        let mut ctx = Context::new("SOUL".into());
        ctx.push(ChatMessage::user("same"));
        ctx.push(ChatMessage::user("same"));
        ctx.push(ChatMessage::user("same"));
        ctx.push(ChatMessage::user("different"));
        // budget 足够大 → 不进 LLM，但 cheap_normalize 仍去重
        ctx.compact(&MockProvider, 3, 10_000).await.unwrap();
        let users: Vec<&ChatMessage> = ctx
            .history
            .iter()
            .filter(|m| m.role == Role::User)
            .collect();
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].content.as_text(), "same");
        assert_eq!(users[1].content.as_text(), "different");
    }

    /// A：极小预算下从最旧开始丢回合单元，保留最新一条，且绝不产生孤儿 tool 消息。
    #[test]
    fn test_hard_truncate_bounds_keeps_newest_and_no_orphan_tool() {
        let mut ctx = Context::new("S".into());
        ctx.push(ChatMessage::user("u0 ".repeat(120)));
        ctx.push(ChatMessage::assistant_with_tools(
            "",
            vec![ToolCall {
                id: "c1".into(),
                name: "t".into(),
                arguments: json!({}),
            }],
        ));
        ctx.push(ChatMessage::tool("tr ".repeat(200), "c1"));
        ctx.push(ChatMessage::user("keep me"));

        ctx.hard_truncate_to_fit(40);

        assert!(!ctx.history.is_empty());
        assert_eq!(ctx.history.last().unwrap().content.as_text(), "keep me");
        if let Some(first) = ctx.history.first() {
            assert_ne!(
                first.role,
                Role::Tool,
                "head must not be an orphan tool msg"
            );
        }
        // 每条 tool 消息前必有携带其 id 的 assistant —— 无孤儿
        for (i, m) in ctx.history.iter().enumerate() {
            if m.role == Role::Tool {
                let id = m.tool_call_id.clone().unwrap();
                let parent_ok = ctx.history[..i].iter().any(|p| {
                    p.role == Role::Assistant
                        && p.tool_calls
                            .as_ref()
                            .map(|v| v.iter().any(|c| c.id == id))
                            .unwrap_or(false)
                });
                assert!(parent_ok, "orphan tool message at index {i}");
            }
        }
    }

    /// A：只剩一个单元、且该单元正文超长（预算高于固定地板）时，走兜底截断（丢不掉了才砍内容）。
    #[test]
    fn test_hard_truncate_truncates_lone_oversized_message() {
        let mut ctx = Context::new("S".into());
        ctx.push(ChatMessage::user("x".repeat(5000)));
        ctx.hard_truncate_to_fit(600);
        assert_eq!(ctx.history.len(), 1, "唯一单元不应被丢空");
        assert!(
            ctx.history[0]
                .content
                .as_text()
                .contains("truncated to fit"),
            "超长单条应被兜底截断"
        );
        assert!(
            ctx.estimate_tokens() <= 600,
            "截断后应落回预算内，实为 {}",
            ctx.estimate_tokens()
        );
    }

    /// A：预算充裕时是 no-op，不动 history。
    #[test]
    fn test_hard_truncate_noop_when_within_budget() {
        let mut ctx = Context::new("S".into());
        ctx.push(ChatMessage::user("hello"));
        ctx.push(ChatMessage::assistant("hi there"));
        ctx.hard_truncate_to_fit(10_000);
        assert_eq!(ctx.history.len(), 2);
        assert_eq!(ctx.history[0].content.as_text(), "hello");
    }

    /// A：固定地板（system 自身即超预算）时就此收手，不清空历史、不 panic。
    #[test]
    fn test_hard_truncate_respects_fixed_floor() {
        let mut ctx = Context::new("S".repeat(4000)); // system 地板远超预算
        ctx.push(ChatMessage::user("a".repeat(500)));
        ctx.push(ChatMessage::user("b".repeat(500)));
        ctx.hard_truncate_to_fit(10);
        assert!(!ctx.history.is_empty(), "固定地板下不得清空近期尾部");
        assert_eq!(
            ctx.history.last().unwrap().content.as_text(),
            "b".repeat(500)
        );
    }
}
