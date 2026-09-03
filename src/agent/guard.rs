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
}
