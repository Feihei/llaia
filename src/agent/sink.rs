use crate::agent::MediaKind;
use async_trait::async_trait;

/// channel 输出抽象：`run_turn` 按 `TurnEvent` 回调 sink 的方法。
/// channel 只实现"如何输出"，不关心 agent task 调度和中断。
#[async_trait]
pub trait OutputSink: Send {
    /// 文本增量
    async fn on_chunk(&mut self, delta: &str);
    /// 工具调用开始
    async fn on_tool_start(&mut self, name: &str);
    /// 工具执行结果（默认忽略，CLI override 打印预览）
    async fn on_tool_result(&mut self, _output: &str) {}
    /// Agent 请求发送媒体给用户
    async fn on_media(&mut self, path: &str, kind: MediaKind);
    /// 整轮正常结束
    async fn on_done(&mut self);
    /// 错误（已生成的文本保留，错误追加）
    async fn on_error(&mut self, message: &str);
    /// 被 /stop 或 Ctrl+C 中断
    async fn on_interrupted(&mut self);
}
