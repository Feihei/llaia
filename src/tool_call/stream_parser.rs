use crate::provider::ToolCall;
use serde_json::Value;

/// 流式 tool_call 标签解析器（状态机）。
///
/// 喂入文本 chunk，输出应发给用户的纯文本增量；
/// 完整的 `<tool_call>...</tool_call>` 标签被解析为 ToolCall，不输出。
/// 支持标签跨 chunk 边界。
pub struct ToolCallStreamParser {
    state: State,
    /// InToolCall 状态下累积的标签内容
    buffer: String,
    /// MaybeTag 状态下缓冲的可能是标签开头的内容（如 "<tool"）
    pending: String,
    /// 已解析的 ToolCall 列表
    completed: Vec<ToolCall>,
}

#[derive(PartialEq)]
enum State {
    Outside,
    MaybeTag,
    InToolCall,
}

const OPEN_TAG: &str = "<tool_call>";
const CLOSE_TAG: &str = "</tool_call>";

impl ToolCallStreamParser {
    pub fn new() -> Self {
        Self {
            state: State::Outside,
            buffer: String::new(),
            pending: String::new(),
            completed: Vec::new(),
        }
    }

    /// 喂一个 chunk，返回应发给用户的文本增量
    pub fn feed(&mut self, chunk: &str) -> String {
        let mut out = String::new();
        for ch in chunk.chars() {
            match self.state {
                State::Outside => {
                    if ch == '<' {
                        self.state = State::MaybeTag;
                        self.pending.push(ch);
                    } else {
                        out.push(ch);
                    }
                }
                State::MaybeTag => {
                    self.pending.push(ch);
                    if OPEN_TAG.starts_with(&self.pending) {
                        if self.pending == OPEN_TAG {
                            self.state = State::InToolCall;
                            self.pending.clear();
                            self.buffer.clear();
                        }
                    } else {
                        out.push_str(&self.pending);
                        self.pending.clear();
                        self.state = State::Outside;
                    }
                }
                State::InToolCall => {
                    self.buffer.push(ch);
                    if self.buffer.ends_with(CLOSE_TAG) {
                        let body = &self.buffer[..self.buffer.len() - CLOSE_TAG.len()];
                        let body_trimmed = body.trim();
                        if let Ok(value) = serde_json::from_str::<Value>(body_trimmed) {
                            if let Some(call) = value_to_tool_call(&value) {
                                self.completed.push(call);
                            } else {
                                out.push_str(&self.buffer);
                            }
                        } else {
                            out.push_str(&self.buffer);
                        }
                        self.buffer.clear();
                        self.state = State::Outside;
                    }
                }
            }
        }
        out
    }

    /// 取出已解析的 ToolCall（清空内部列表）
    pub fn take_tool_calls(&mut self) -> Vec<ToolCall> {
        std::mem::take(&mut self.completed)
    }

    /// 流结束时调用，返回残留的 pending/buffer 作为普通文本
    pub fn finish(self) -> String {
        let mut out = String::new();
        out.push_str(&self.pending);
        if self.state == State::InToolCall {
            out.push_str(OPEN_TAG);
            out.push_str(&self.buffer);
        }
        out
    }
}

fn value_to_tool_call(value: &Value) -> Option<ToolCall> {
    let name = value.get("name")?.as_str()?.to_string();
    let arguments = match value.get("arguments") {
        Some(Value::Object(_)) => value.get("arguments").cloned().unwrap(),
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        _ => Value::Null,
    };
    let id = format!("tag_{}", uuid::Uuid::new_v4().simple());
    Some(ToolCall { id, name, arguments })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smoke() {
        let mut p = ToolCallStreamParser::new();
        assert_eq!(p.feed("hi"), "hi");
    }
}
