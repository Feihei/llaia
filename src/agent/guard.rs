//! Generation Guard：输出退化防护（docs/plans/2026-09-03-generation-guard.md）。
//!
//! 针对小参数本地模型在长上下文下的退化（重复循环、思考流失控、空输出）：
//! - 流式判定退化 → 中途 abort（drop 流即断连，本地服务端随之停止生成）→ 丢弃产物
//! - 重试：注入 `[guard]` 提示（持久化）+ 强制 `disable_thinking`（退化几乎总是
//!   思考流失控，从根上掐）
//! - 重试耗尽 → 诊断收尾；连续失败附加醒目警告（熔断只报警不拒服）
//!
//! 检测全部字符级，引擎无关；`output_guard = false` 时全链路短路，行为与旧版一致。
//! 与现有防线的关系：流空闲超时（连接层）/ 本模块（内容层）/ max_turn_duration
//! （回合层）/ FallbackProvider（请求层）四层互补。

use crate::config::RuntimeConfig;

/// Guard 配置快照：Agent 构建期从 `[runtime]` 取值，`reload_runtime` 热加载。
#[derive(Debug, Clone)]
pub struct GuardConfig {
    pub enabled: bool,
    /// 重复检测滑动窗口（字符）
    pub repeat_window: usize,
    /// 重复检测 n-gram 长度（字符）
    pub repeat_gram: usize,
    /// 窗口内同一 n-gram 出现次数达到该值即判退化
    pub repeat_threshold: u32,
    /// 思考流字符上限（`<think>` 块与 reasoning_content 累计），0 = 不限
    pub thinking_cap: usize,
    /// 判退化后的重试次数
    pub max_retries: u32,
    /// 连续退化回合报警阈值（达到即在诊断消息附加醒目警告）
    pub breaker_threshold: u32,
}

impl GuardConfig {
    pub fn from_runtime(rt: &RuntimeConfig) -> Self {
        Self {
            enabled: rt.output_guard,
            repeat_window: rt.guard_repeat_window,
            repeat_gram: rt.guard_repeat_gram,
            repeat_threshold: rt.guard_repeat_threshold,
            thinking_cap: rt.guard_thinking_cap,
            max_retries: rt.guard_max_retries,
            breaker_threshold: rt.guard_breaker_threshold,
        }
    }
}

/// 重试时注入给模型的提示（英文，与内部提示语约定一致；持久化进 sqlite/context，
/// 前缀标记同 `[steer]` 模式，会话日志可追溯，WebUI 可事后删除）。
pub const RETRY_HINT: &str = "[guard] Your previous response was discarded because it degenerated into repetition or produced no answer. Reply again, directly and concisely. Do not repeat what you already wrote.";

/// 首次重试前发给用户的可见通知（退化产物不落库，用户需要知道刚才看到的
/// 部分输出已被丢弃）。
pub const RETRY_NOTICE: &str = "\n\n[检测到输出退化（重复/思考失控/空回复），已中止并重新生成…]\n";

/// 滑动窗口字符 n-gram 重复检测器（Generation Guard P1）。
///
/// 窗口内任一 `gram` 字符的片段出现 ≥ `threshold` 次即判退化（命中后粘滞）。
/// 字符级而非 token 级：框架无 tokenizer，引擎无关。窗口 ≤ 千字符量级，
/// 每 `CHECK_INTERVAL` 字符重建一次窗口内 gram 计数，开销可忽略。
/// `threshold = 0` 表示禁用：`feed` 短路，`is_degenerate` 恒 false。
pub struct RepetitionDetector {
    window: usize,
    gram: usize,
    threshold: u32,
    buf: std::collections::VecDeque<char>,
    /// 自上次扫描以来新喂入的字符数
    since_scan: usize,
    degenerate: bool,
}

/// 每累计多少字符做一次窗口扫描
const CHECK_INTERVAL: usize = 16;

impl RepetitionDetector {
    pub fn new(window: usize, gram: usize, threshold: u32) -> Self {
        Self {
            window: window.max(gram),
            gram,
            threshold,
            buf: std::collections::VecDeque::new(),
            since_scan: 0,
            degenerate: false,
        }
    }

    /// 喂入一段文本
    pub fn feed(&mut self, text: &str) {
        for ch in text.chars() {
            self.feed_char(ch);
        }
    }

    /// 喂入单个字符（parser 的 InThink 状态逐字符喂）
    pub fn feed_char(&mut self, ch: char) {
        if self.threshold == 0 || self.degenerate {
            return; // 禁用或已命中：短路
        }
        self.buf.push_back(ch);
        while self.buf.len() > self.window {
            self.buf.pop_front();
        }
        self.since_scan += 1;
        if self.since_scan >= CHECK_INTERVAL {
            self.scan();
        }
    }

    /// 是否已判定退化（命中后粘滞，不随后续内容回退）
    pub fn is_degenerate(&self) -> bool {
        self.degenerate
    }

    /// 触发时日志用的窗口尾部摘要
    pub fn tail_summary(&self) -> String {
        self.buf.iter().rev().take(60).rev().collect()
    }

    /// 重建窗口内 gram 计数并判定
    fn scan(&mut self) {
        self.since_scan = 0;
        let n = self.buf.len();
        if self.gram == 0 || n < self.gram {
            return;
        }
        let mut counts: std::collections::HashMap<String, u32> =
            std::collections::HashMap::with_capacity(n - self.gram + 1);
        for start in 0..=(n - self.gram) {
            let g: String = self.buf.iter().skip(start).take(self.gram).collect();
            let c = counts.entry(g).or_insert(0);
            *c += 1;
            if *c >= self.threshold {
                self.degenerate = true;
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RuntimeConfig;

    #[test]
    fn test_from_runtime_defaults() {
        let rt = RuntimeConfig::default();
        let g = GuardConfig::from_runtime(&rt);
        assert!(g.enabled);
        assert_eq!(g.repeat_window, 512);
        assert_eq!(g.repeat_gram, 24);
        assert_eq!(g.repeat_threshold, 4);
        assert_eq!(g.thinking_cap, 32_000);
        assert_eq!(g.max_retries, 1);
        assert_eq!(g.breaker_threshold, 2);
    }

    fn detector() -> RepetitionDetector {
        RepetitionDetector::new(512, 24, 4)
    }

    #[test]
    fn test_degenerate_loop_hits() {
        // 退化循环：同一句子反复出现 → 24 字符 gram 远超阈值
        let mut d = detector();
        d.feed(&"让我重新整理一下思路。".repeat(60));
        assert!(d.is_degenerate());
        assert!(!d.tail_summary().is_empty());
    }

    #[test]
    fn test_normal_code_repetition_does_not_hit() {
        // 误报底线：真实风格的测试代码重复行 ×3（24 字符以上的行）不得命中
        let mut d = detector();
        let line = "    assert_eq!(compute(input), expected_output);\n";
        let code = format!("fn t() {{\n{line}{line}{line}}}\n");
        d.feed(&code);
        assert!(
            !d.is_degenerate(),
            "code with 3 identical lines must not hit"
        );
    }

    #[test]
    fn test_natural_text_does_not_hit() {
        // 正常中英混排长文本：无高频重复片段
        let mut d = detector();
        let text = "Generation Guard 在流式路径上逐字符检测退化模式。窗口内的 n-gram \
                    计数超过阈值才会触发中止与重试。For English text the same rule \
                    applies: occasional repeats are fine, sustained loops are not. \
                    检测全部在框架层完成，不依赖推理引擎。";
        d.feed(text);
        d.feed(text); // 整段重复一次也应容忍（两次远低于阈值 4）
        assert!(!d.is_degenerate());
    }

    #[test]
    fn test_short_input_no_verdict() {
        // 窗口未积累足够字符不判定
        let mut d = detector();
        d.feed("短文本");
        assert!(!d.is_degenerate());
    }

    #[test]
    fn test_disabled_threshold_zero_never_hits() {
        let mut d = RepetitionDetector::new(512, 24, 0);
        d.feed(&"循环循环循环。".repeat(200));
        assert!(!d.is_degenerate());
    }

    #[test]
    fn test_hit_is_sticky() {
        let mut d = detector();
        d.feed(&"重复片段循环输出。".repeat(80));
        assert!(d.is_degenerate());
        d.feed("后续正常内容");
        assert!(d.is_degenerate(), "sticky once triggered");
    }
}
