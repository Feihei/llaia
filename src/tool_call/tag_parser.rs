use crate::provider::ToolCall;
use serde_json::Value;

/// 从模型回复文本中解析工具调用标签，并剥离 think 标签。
/// 返回 (纯文本部分, 工具调用列表)。
/// 支持多种标签别名，以及 JSON 解析失败时的 brace-balancing 恢复。
pub fn parse_tool_calls(text: &str) -> (String, Vec<ToolCall>) {
    // 先 strip think 标签，防止推理内容泄漏
    let stripped = strip_think_tags(text);

    let re_str = concat!(
        r"(?is)<",
        r"(?:tool_call|toolcall|tool-call|invoke)>\s*(.*?)\s*<",
        r"/(?:tool_call|toolcall|tool-call|invoke)>"
    );
    let re = regex::Regex::new(re_str).unwrap();

    let mut clean_text = String::new();
    let mut calls = Vec::new();
    let mut last_end = 0;

    for cap in re.captures_iter(&stripped) {
        let match_start = cap.get(0).unwrap().start();
        clean_text.push_str(&stripped[last_end..match_start]);
        last_end = cap.get(0).unwrap().end();

        let body = cap.get(1).unwrap().as_str().trim();
        let call = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|v| value_to_tool_call(&v))
            .or_else(|| {
                extract_json_with_brace_counting(body)
                    .and_then(|sub| serde_json::from_str::<Value>(sub).ok())
                    .and_then(|v| value_to_tool_call(&v))
            });
        if let Some(call) = call {
            calls.push(call);
            continue;
        }
        // 解析失败且恢复失败，保留原文
        clean_text.push_str(cap.get(0).unwrap().as_str());
    }
    clean_text.push_str(&stripped[last_end..]);

    (clean_text.trim().to_string(), calls)
}

/// 剥离 think 标签：删除完整的 think 块及未闭合的 think 开标签（防泄漏）。
fn strip_think_tags(text: &str) -> String {
    // 删除完整的 think / thinking 块
    let re_closed = concat!(r"(?is)<", r"think(?:ing)?>.*?<", r"/think(?:ing)?>");
    let re = regex::Regex::new(re_closed).unwrap();
    let mut result = re.replace_all(text, "").to_string();
    // 删除未闭合的 think 开标签（防泄漏推理内容）
    let re_unclosed = concat!(r"(?is)<", r"think(?:ing)?>.*$");
    let re2 = regex::Regex::new(re_unclosed).unwrap();
    result = re2.replace_all(&result, "").to_string();
    result
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

    // 标签常量用 concat! 拼接，避免字面量被工具层误解析。
    const TC_OPEN: &str = concat!("<", "tool_call>");
    const TC_CLOSE: &str = concat!("<", "/tool_call>");
    const TC2_OPEN: &str = concat!("<", "toolcall>");
    const TC2_CLOSE: &str = concat!("<", "/toolcall>");
    const INV_OPEN: &str = concat!("<", "invoke>");
    const INV_CLOSE: &str = concat!("<", "/invoke>");
    const TH_OPEN: &str = concat!("<", "think>");
    const TH_CLOSE: &str = concat!("<", "/think>");

    #[test]
    fn test_single_tag() {
        let json = r#"{"name":"file_read","arguments":{"path":"/tmp/x"}}"#;
        let text = format!("我来读文件 {}{}{} 看看", TC_OPEN, json, TC_CLOSE);
        let (clean, calls) = parse_tool_calls(&text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[0].arguments, json!({"path": "/tmp/x"}));
        assert!(clean.contains("我来读文件"));
        assert!(clean.contains("看看"));
        assert!(!clean.contains("tool_call"));
    }

    #[test]
    fn test_multiple_tags() {
        let j1 = r#"{"name":"a","arguments":{}}"#;
        let j2 = r#"{"name":"b","arguments":{}}"#;
        let text = format!("{}{}{}{}{}", TC_OPEN, j1, TC_CLOSE, TC_OPEN, j2);
        let text = format!("{}{}", text, TC_CLOSE);
        let (_, calls) = parse_tool_calls(&text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
    }

    #[test]
    fn test_no_tag() {
        let text = "普通回复";
        let (clean, calls) = parse_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(clean, "普通回复");
    }

    #[test]
    fn test_string_arguments() {
        let json = r#"{"name":"x","arguments":"{\"k\":1}"}"#;
        let text = format!("{}{}{}", TC_OPEN, json, TC_CLOSE);
        let (_, calls) = parse_tool_calls(&text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, json!({"k": 1}));
    }

    #[test]
    fn test_multiline_body() {
        let json = r#"{
  "name": "file_write",
  "arguments": {"path": "/tmp/y", "content": "hello"}
}"#;
        let text = format!("{}{}{}", TC_OPEN, json, TC_CLOSE);
        let (_, calls) = parse_tool_calls(&text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
    }

    #[test]
    fn test_malformed_kept_as_text() {
        let text = format!("{}not json{}", TC_OPEN, TC_CLOSE);
        let (clean, calls) = parse_tool_calls(&text);
        assert!(calls.is_empty());
        assert!(clean.contains("tool_call"));
    }

    // --- 新增测试 ---

    #[test]
    fn test_strip_think_tags() {
        let text = format!("{}secret{}visible", TH_OPEN, TH_CLOSE);
        let (clean, calls) = parse_tool_calls(&text);
        assert_eq!(clean, "visible");
        assert!(calls.is_empty());
    }

    #[test]
    fn test_strip_unclosed_think() {
        let text = format!("visible{}secret", TH_OPEN);
        let (clean, _) = parse_tool_calls(&text);
        assert_eq!(clean, "visible");
    }

    #[test]
    fn test_alias_toolcall() {
        let json = r#"{"name":"x","arguments":{}}"#;
        let text = format!("{}{}{}", TC2_OPEN, json, TC2_CLOSE);
        let (_, calls) = parse_tool_calls(&text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "x");
    }

    #[test]
    fn test_alias_invoke() {
        let json = r#"{"name":"x","arguments":{}}"#;
        let text = format!("{}{}{}", INV_OPEN, json, INV_CLOSE);
        let (_, calls) = parse_tool_calls(&text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "x");
    }

    #[test]
    fn test_cross_alias_close() {
        let json = r#"{"name":"x","arguments":{}}"#;
        let text = format!("{}{}{}", TC2_OPEN, json, TC_CLOSE);
        let (_, calls) = parse_tool_calls(&text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "x");
    }

    #[test]
    fn test_json_brace_recovery() {
        let body = r#"prefix {"name":"x","arguments":{}} suffix"#;
        let text = format!("{}{}{}", TC_OPEN, body, TC_CLOSE);
        let (_, calls) = parse_tool_calls(&text);
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
}
