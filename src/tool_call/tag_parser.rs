use crate::provider::ToolCall;
use serde_json::Value;

/// 从模型回复文本中解析 `<tool_call>{...}</tool_call>` 标签。
/// 返回 (纯文本部分, 工具调用列表)。
pub fn parse_tool_calls(text: &str) -> (String, Vec<ToolCall>) {
    let mut clean_text = String::new();
    let mut calls = Vec::new();
    let mut last_end = 0;

    let re = regex::Regex::new(r"(?is)<tool_call>\s*(.*?)\s*</tool_call>").unwrap();

    for cap in re.captures_iter(text) {
        let match_start = cap.get(0).unwrap().start();
        clean_text.push_str(&text[last_end..match_start]);
        last_end = cap.get(0).unwrap().end();

        let body = cap.get(1).unwrap().as_str().trim();
        if let Ok(value) = serde_json::from_str::<Value>(body) {
            if let Some(call) = value_to_tool_call(&value) {
                calls.push(call);
                continue;
            }
        }
        clean_text.push_str(cap.get(0).unwrap().as_str());
    }
    clean_text.push_str(&text[last_end..]);

    (clean_text.trim().to_string(), calls)
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
    use serde_json::json;

    #[test]
    fn test_single_tag() {
        let text = r#"我来读文件 <tool_call>{"name":"file_read","arguments":{"path":"/tmp/x"}}</tool_call> 看看"#;
        let (clean, calls) = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[0].arguments, json!({"path": "/tmp/x"}));
        assert!(clean.contains("我来读文件"));
        assert!(clean.contains("看看"));
        assert!(!clean.contains("tool_call"));
    }

    #[test]
    fn test_multiple_tags() {
        let text = r#"<tool_call>{"name":"a","arguments":{}}</tool_call><tool_call>{"name":"b","arguments":{}}</tool_call>"#;
        let (_, calls) = parse_tool_calls(text);
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
        let text = r#"<tool_call>{"name":"x","arguments":"{\"k\":1}"}</tool_call>"#;
        let (_, calls) = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, json!({"k": 1}));
    }

    #[test]
    fn test_multiline_body() {
        let text = r#"<tool_call>
{
  "name": "file_write",
  "arguments": {"path": "/tmp/y", "content": "hello"}
}
</tool_call>"#;
        let (_, calls) = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
    }

    #[test]
    fn test_malformed_kept_as_text() {
        let text = r#"<tool_call>not json</tool_call>"#;
        let (clean, calls) = parse_tool_calls(text);
        assert!(calls.is_empty());
        assert!(clean.contains("tool_call"));
    }
}
