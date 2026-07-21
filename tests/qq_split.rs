use laia::channels::qq_split::split_reply;

#[test]
fn test_short_reply_no_split() {
    let text = "短回复";
    assert_eq!(split_reply(text, 1800), vec!["短回复"]);
}

#[test]
fn test_split_by_paragraph() {
    let text = "段落一\n\n段落二\n\n段落三";
    let parts = split_reply(text, 10);
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], "段落一");
    assert_eq!(parts[1], "段落二");
    assert_eq!(parts[2], "段落三");
}

#[test]
fn test_split_by_line_when_paragraph_too_long() {
    let text = "aaaaa\nbbbbb\nccccc\nddddd";
    let parts = split_reply(text, 12);
    assert_eq!(parts.len(), 2);
    assert!(parts[0].len() <= 12);
    assert!(parts[1].len() <= 12);
}

#[test]
fn test_split_by_char_when_line_too_long() {
    let text = "a".repeat(2500);
    let parts = split_reply(&text, 1800);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].len(), 1800);
    assert_eq!(parts[1].len(), 700);
}

#[test]
fn test_code_block_preserved_within_chunk() {
    let text = "前文\n\n```rust\nfn main() {}\n```\n\n后文";
    let parts = split_reply(text, 100);
    assert_eq!(parts.len(), 1);
    assert!(parts[0].contains("```rust"));
    assert!(parts[0].contains("```"));
}

#[test]
fn test_code_block_split_closes_and_reopens() {
    let long_code = "fn main() {\n".to_string() + &"    println!(\"x\");\n".repeat(200) + "}\n";
    let text = format!("```rust\n{}```", long_code);
    let parts = split_reply(&text, 1800);
    assert!(parts.len() > 1);
    assert!(parts[0].ends_with("```"));
    assert!(parts[1].starts_with("```rust"));
}
