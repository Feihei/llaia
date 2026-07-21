use crate::provider::ToolSpec;

/// 构造标签降级模式下的工具协议说明，注入 system prompt。
pub fn build_tool_instructions(tools: &[ToolSpec]) -> String {
    let mut s = String::from("\n\n## Tool Use Protocol\n\n");
    s.push_str("To use a tool, wrap a JSON object in <tool_call></tool_call> tags:\n\n");
    s.push_str("<tool_call>\n{\"name\": \"tool_name\", \"arguments\": {\"param\": \"value\"}}\n</tool_call>\n\n");
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
        assert!(s.contains("<tool_call>"));
        assert!(s.contains("file_read"));
        assert!(s.contains("Read a file"));
    }
}
