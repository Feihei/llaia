use llaia::tool_call::stream_parser::ToolCallStreamParser;

#[test]
fn test_plain_text_passthrough() {
    let mut p = ToolCallStreamParser::new();
    let out = p.feed("hello world");
    assert_eq!(out, "hello world");
    let out = p.feed(" more text");
    assert_eq!(out, " more text");
    let calls = p.take_tool_calls();
    assert!(calls.is_empty());
    assert_eq!(p.finish(), "");
}

#[test]
fn test_single_tag_in_one_chunk() {
    let mut p = ToolCallStreamParser::new();
    let out = p.feed(
        r#"before <tool_call>{"name":"file_read","arguments":{"path":"/tmp/x"}}</tool_call> after"#,
    );
    assert_eq!(out, "before  after");
    let calls = p.take_tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "file_read");
}

#[test]
fn test_tag_split_across_chunks() {
    let mut p = ToolCallStreamParser::new();
    let out1 = p.feed("before <tool_");
    assert_eq!(out1, "before ");
    let out2 = p.feed(r#"call>{"name":"x","arguments":{}}</tool_call> after"#);
    assert_eq!(out2, " after");
    let calls = p.take_tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "x");
}

#[test]
fn test_multiple_tags_consecutive() {
    let mut p = ToolCallStreamParser::new();
    let out = p.feed(r#"<tool_call>{"name":"a","arguments":{}}</tool_call><tool_call>{"name":"b","arguments":{}}</tool_call>"#);
    assert_eq!(out, "");
    let calls = p.take_tool_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "a");
    assert_eq!(calls[1].name, "b");
}

#[test]
fn test_lt_char_not_tag() {
    let mut p = ToolCallStreamParser::new();
    let out = p.feed("a < b and c > d");
    assert_eq!(out, "a < b and c > d");
    let calls = p.take_tool_calls();
    assert!(calls.is_empty());
    assert_eq!(p.finish(), "");
}

#[test]
fn test_unclosed_tag_finish_returns_buffer_as_text() {
    let mut p = ToolCallStreamParser::new();
    let _ = p.feed("text <tool_call>not closed");
    let calls = p.take_tool_calls();
    assert!(calls.is_empty());
    let rest = p.finish();
    assert!(rest.contains("not closed"));
}

#[test]
fn test_partial_tag_at_chunk_end_finish_returns_as_text() {
    let mut p = ToolCallStreamParser::new();
    let _ = p.feed("text <tool");
    let calls = p.take_tool_calls();
    assert!(calls.is_empty());
    let rest = p.finish();
    assert!(rest.contains("<tool"));
}

#[test]
fn test_malformed_json_kept_as_text() {
    let mut p = ToolCallStreamParser::new();
    let out = p.feed("<tool_call>not json</tool_call>");
    assert!(out.contains("not json"));
    let calls = p.take_tool_calls();
    assert!(calls.is_empty());
}
