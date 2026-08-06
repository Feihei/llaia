use crate::provider::ToolCall;
use serde_json::Value;

/// 流式 tool_call 标签解析器（状态机）。
///
/// 喂入文本 chunk，输出应发给用户的纯文本增量；
/// 完整的工具调用标签被解析为 ToolCall，不输出。
/// 同时剥离 think 标签，丢弃其中的推理内容，避免泄漏给用户。
/// 支持标签跨 chunk 边界，以及多种标签别名。
pub struct ToolCallStreamParser {
    state: State,
    /// InToolCall / InThink 状态下累积的标签内容
    buffer: String,
    /// MaybeTag / MaybeThink 状态下缓冲的可能是标签开头的内容
    pending: String,
    /// 已解析的 ToolCall 列表
    completed: Vec<ToolCall>,
    /// 进入 InToolCall 时匹配到的开标签，finish() 还原用
    open_tag: &'static str,
}

#[derive(PartialEq)]
enum State {
    Outside,
    /// 可能是 think 开标签
    MaybeThink,
    /// 可能是 tool_call 开标签
    MaybeTag,
    /// 处于 think 块内，丢弃内容直到遇到 think 闭标签
    InThink,
    /// 处于 tool_call 标签内，累积 JSON
    InToolCall,
}

// 所有标签常量用 concat! 拼接，避免字面量被工具层误解析。
const OPEN_TAGS: &[&str] = &[
    concat!("<", "tool_call>"),
    concat!("<", "toolcall>"),
    concat!("<", "tool-call>"),
    concat!("<", "invoke>"),
];
const CLOSE_TAGS: &[&str] = &[
    concat!("<", "/tool_call>"),
    concat!("<", "/toolcall>"),
    concat!("<", "/tool-call>"),
    concat!("<", "/invoke>"),
];
const THINK_OPEN_TAGS: &[&str] = &[concat!("<", "think>"), concat!("<", "thinking>")];
const THINK_CLOSE_TAGS: &[&str] = &[concat!("<", "/think>"), concat!("<", "/thinking>")];

impl Default for ToolCallStreamParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolCallStreamParser {
    pub fn new() -> Self {
        Self {
            state: State::Outside,
            buffer: String::new(),
            pending: String::new(),
            completed: Vec::new(),
            open_tag: "",
        }
    }

    /// 喂一个 chunk，返回应发给用户的文本增量
    pub fn feed(&mut self, chunk: &str) -> String {
        let mut out = String::new();
        for ch in chunk.chars() {
            match self.state {
                State::Outside => {
                    if ch == '<' {
                        // 所有 think / tool_call 开标签都以 '<' 开头，先走 MaybeThink
                        self.state = State::MaybeThink;
                        self.pending.push(ch);
                    } else {
                        out.push(ch);
                    }
                }
                State::MaybeThink => {
                    self.pending.push(ch);
                    if exact_match(&self.pending, THINK_OPEN_TAGS).is_some() {
                        self.state = State::InThink;
                        self.pending.clear();
                        self.buffer.clear();
                    } else if matches_prefix(&self.pending, THINK_OPEN_TAGS) {
                        // 仍是 think 开标签前缀，继续累积
                    } else if matches_prefix(&self.pending, OPEN_TAGS) {
                        // 不是 think 前缀，但可能是 tool_call 开标签
                        self.state = State::MaybeTag;
                    } else {
                        out.push_str(&self.pending);
                        self.pending.clear();
                        self.state = State::Outside;
                    }
                }
                State::MaybeTag => {
                    self.pending.push(ch);
                    if let Some(tag) = exact_match(&self.pending, OPEN_TAGS) {
                        self.open_tag = tag;
                        self.state = State::InToolCall;
                        self.pending.clear();
                        self.buffer.clear();
                    } else if matches_prefix(&self.pending, OPEN_TAGS) {
                        // 仍是 tool_call 开标签前缀，继续累积
                    } else if matches_prefix(&self.pending, THINK_OPEN_TAGS) {
                        // 不是 tool_call 前缀，但可能是 think 开标签
                        self.state = State::MaybeThink;
                    } else {
                        out.push_str(&self.pending);
                        self.pending.clear();
                        self.state = State::Outside;
                    }
                }
                State::InThink => {
                    // 丢弃所有内容，仅检查 think 闭标签
                    self.buffer.push(ch);
                    if ends_with_any(&self.buffer, THINK_CLOSE_TAGS).is_some() {
                        self.buffer.clear();
                        self.state = State::Outside;
                    } else {
                        // 限制 buffer 仅保留最长闭标签长度的尾部，避免无限增长
                        let max_close = max_tag_len(THINK_CLOSE_TAGS);
                        if self.buffer.len() > max_close {
                            let keep = self.buffer.len() - max_close;
                            self.buffer = self.buffer.split_at(keep).1.to_string();
                        }
                    }
                }
                State::InToolCall => {
                    self.buffer.push(ch);
                    if let Some(close_len) = ends_with_any(&self.buffer, CLOSE_TAGS) {
                        let body = &self.buffer[..self.buffer.len() - close_len];
                        let body_trimmed = body.trim();
                        let call = serde_json::from_str::<Value>(body_trimmed)
                            .ok()
                            .and_then(|v| value_to_tool_call(&v))
                            .or_else(|| {
                                extract_json_with_brace_counting(body_trimmed)
                                    .and_then(|sub| serde_json::from_str::<Value>(sub).ok())
                                    .and_then(|v| value_to_tool_call(&v))
                            });
                        if let Some(call) = call {
                            self.completed.push(call);
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

    /// 流结束时调用，返回残留内容作为普通文本。
    /// InThink 状态丢弃未闭合的思考内容（防泄漏）；InToolCall 还原开标签+buffer。
    pub fn finish(self) -> String {
        let mut out = String::new();
        match self.state {
            State::InThink => {}
            State::InToolCall => {
                out.push_str(self.open_tag);
                out.push_str(&self.buffer);
            }
            _ => {
                out.push_str(&self.pending);
            }
        }
        out
    }
}

fn matches_prefix(s: &str, tags: &[&str]) -> bool {
    tags.iter().any(|t| t.starts_with(s))
}

fn exact_match<'a>(s: &str, tags: &'a [&'a str]) -> Option<&'a str> {
    tags.iter().find(|t| **t == s).copied()
}

fn ends_with_any(s: &str, tags: &[&str]) -> Option<usize> {
    for tag in tags {
        if s.ends_with(tag) {
            return Some(tag.len());
        }
    }
    None
}

fn max_tag_len(tags: &[&str]) -> usize {
    tags.iter().map(|t| t.len()).max().unwrap_or(0)
}

/// 从文本中提取第一个完整的 JSON 对象（基于大括号配对）。
/// 跳过字符串字面量内的括号，处理转义。用于 JSON 解析失败时的恢复。
fn extract_json_with_brace_counting(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = text.find('{')?;
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if escape {
            escape = false;
        } else if in_string {
            match c {
                b'\\' => escape = true,
                b'"' => in_string = false,
                _ => {}
            }
        } else {
            match c {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&text[start..=i]);
                    }
                    if depth < 0 {
                        return None;
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn value_to_tool_call(value: &Value) -> Option<ToolCall> {
    let name = value.get("name")?.as_str()?.to_string();
    let arguments = match value.get("arguments") {
        Some(Value::Object(_)) => value.get("arguments").cloned().unwrap(),
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        _ => Value::Null,
    };
    let id = format!("tag_{}", uuid::Uuid::new_v4().simple());
    Some(ToolCall {
        id,
        name,
        arguments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_smoke() {
        let mut p = ToolCallStreamParser::new();
        assert_eq!(p.feed("hi"), "hi");
    }

    #[test]
    fn test_think_tag_stripped() {
        let mut p = ToolCallStreamParser::new();
        let input = format!("{}secret{}hello", THINK_OPEN_TAGS[0], THINK_CLOSE_TAGS[0]);
        assert_eq!(p.feed(&input), "hello");
        assert_eq!(p.take_tool_calls().len(), 0);
    }

    #[test]
    fn test_thinking_tag_stripped() {
        let mut p = ToolCallStreamParser::new();
        let input = format!("{}thoughts{}hello", THINK_OPEN_TAGS[1], THINK_CLOSE_TAGS[1]);
        assert_eq!(p.feed(&input), "hello");
        assert_eq!(p.take_tool_calls().len(), 0);
    }

    #[test]
    fn test_unclosed_think_discarded() {
        let mut p = ToolCallStreamParser::new();
        let input = format!("{}secret thoughts", THINK_OPEN_TAGS[0]);
        let out = p.feed(&input);
        assert_eq!(out, "");
        assert_eq!(p.finish(), "");
    }

    #[test]
    fn test_tool_call_alias_toolcall() {
        let mut p = ToolCallStreamParser::new();
        let body = r#"{"name":"x","arguments":{}}"#;
        let input = format!("{}{}{}", OPEN_TAGS[1], body, CLOSE_TAGS[1]);
        assert_eq!(p.feed(&input), "");
        let calls = p.take_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "x");
    }

    #[test]
    fn test_tool_call_alias_invoke() {
        let mut p = ToolCallStreamParser::new();
        let body = r#"{"name":"x","arguments":{}}"#;
        let input = format!("{}{}{}", OPEN_TAGS[3], body, CLOSE_TAGS[3]);
        assert_eq!(p.feed(&input), "");
        let calls = p.take_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "x");
    }

    #[test]
    fn test_cross_alias_close() {
        let mut p = ToolCallStreamParser::new();
        let body = r#"{"name":"x","arguments":{}}"#;
        let input = format!("{}{}{}", OPEN_TAGS[1], body, CLOSE_TAGS[3]);
        assert_eq!(p.feed(&input), "");
        let calls = p.take_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "x");
    }

    #[test]
    fn test_think_before_tool_call() {
        let mut p = ToolCallStreamParser::new();
        let think = format!("{}reasoning{}", THINK_OPEN_TAGS[0], THINK_CLOSE_TAGS[0]);
        assert_eq!(p.feed(&think), "");
        let body = r#"{"name":"x","arguments":{}}"#;
        let call = format!("{}{}{}", OPEN_TAGS[0], body, CLOSE_TAGS[0]);
        assert_eq!(p.feed(&call), "");
        assert_eq!(p.take_tool_calls().len(), 1);
    }

    #[test]
    fn test_json_brace_recovery() {
        let mut p = ToolCallStreamParser::new();
        let body = r#"prefix {"name":"x","arguments":{}} suffix"#;
        let input = format!("{}{}{}", OPEN_TAGS[0], body, CLOSE_TAGS[0]);
        assert_eq!(p.feed(&input), "");
        let calls = p.take_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "x");
    }

    #[test]
    fn test_brace_counting_string_with_braces() {
        let sub = extract_json_with_brace_counting(
            r#"prefix {"name":"x","arguments":{"s":"{]}"}} suffix"#,
        )
        .unwrap();
        let v: Value = serde_json::from_str(sub).unwrap();
        assert_eq!(v["name"], "x");
        assert_eq!(v["arguments"]["s"], "{]}");
    }

    #[test]
    fn test_string_arguments_streamed() {
        let mut p = ToolCallStreamParser::new();
        let body = r#"{"name":"x","arguments":"{\"k\":1}"}"#;
        let input = format!("{}{}{}", OPEN_TAGS[0], body, CLOSE_TAGS[0]);
        assert_eq!(p.feed(&input), "");
        let calls = p.take_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, json!({"k": 1}));
    }
}
