use crate::provider::ToolSpec;

/// 构造标签降级模式下的工具协议说明，注入 system prompt。
/// 加强约束：禁止 think 标签、禁止 markdown 包裹、工具名必须在列表中。
pub fn build_tool_instructions(tools: &[ToolSpec]) -> String {
    let open = concat!("<", "tool_call>");
    let close = concat!("<", "/tool_call>");
    let mut s = String::from("\n\n## Tool Use Protocol\n\n");
    s.push_str(&format!(
        "When you need to use a tool, you MUST wrap a JSON object in {}{} tags. ",
        open, close
    ));
    s.push_str("The tool_call tag MUST be the only content in your response when calling a tool — do not add prose before or after the tag.\n\n");
    s.push_str("Format:\n");
    s.push_str(&format!(
        "{}\n{{\"name\": \"tool_name\", \"arguments\": {{\"param\": \"value\"}}}}\n{}\n\n",
        open, close
    ));
    s.push_str("Rules:\n");
    s.push_str("- Do NOT output think tags — keep reasoning internal.\n");
    s.push_str("- Do NOT wrap tool calls in markdown code blocks.\n");
    s.push_str("- Tool name must be from the Available tools list below.\n");
    s.push_str("- Multiple tool calls can be made in one response, each in its own tool_call tag.\n\n");
    s.push_str("Available tools:\n\n");
    for t in tools {
        s.push_str(&format!("- **{}**: {}\n", t.name, t.description));
        s.push_str(&format!("  parameters: {}\n", t.parameters));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_build_instructions() {
        let tools = vec![ToolSpec {
            name: "file_read".into(),
            description: "Read a file".into(),
            parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        }];
        let s = build_tool_instructions(&tools);
        let open = concat!("<", "tool_call>");
        assert!(s.contains(open));
        assert!(s.contains("file_read"));
        assert!(s.contains("Read a file"));
    }

    #[test]
    fn test_prompt_contains_must() {
        let s = build_tool_instructions(&[]);
        assert!(s.contains("MUST"));
    }

    #[test]
    fn test_prompt_contains_no_think() {
        let s = build_tool_instructions(&[]);
        assert!(s.to_lowercase().contains("think"));
    }

    #[test]
    fn test_prompt_contains_alias_hint() {
        let s = build_tool_instructions(&[]);
        assert!(s.contains("Available tools"));
    }
}
