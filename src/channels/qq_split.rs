/// 将长文本按 QQ 单条消息上限分片。
///
/// 规则：
/// 1. 优先按段落（`\n\n`）切
/// 2. 单段超 max 时按行（`\n`）切
/// 3. 单行超 max 时按字符硬切
/// 4. 代码块跨片时闭合后再开，下一片以 ``` 同语言标记开始
pub fn split_reply(text: &str, max: usize) -> Vec<String> {
    if text.len() <= max {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut in_code_block = false;
    let mut code_lang = String::new();

    /// 把 current 推到 chunks。如果在代码块里，先闭合。
    /// 推完后，如果还在代码块里，current 重置为 ```{lang}\n 准备接续。
    fn flush(
        current: &mut String,
        chunks: &mut Vec<String>,
        in_code_block: &mut bool,
        code_lang: &str,
    ) {
        if current.is_empty() {
            return;
        }
        let was_in_code = *in_code_block;
        if was_in_code {
            current.push_str("\n```");
        }
        chunks.push(std::mem::take(current));
        if was_in_code {
            // 仍在代码块内，下一片以代码块开头续接
            *current = format!("```{}\n", code_lang);
        }
    }

    let paragraphs: Vec<&str> = text.split("\n\n").collect();
    for para in paragraphs {
        // 检测代码块状态变化
        let trimmed = para.trim_start();
        if trimmed.starts_with("```") {
            if !in_code_block {
                in_code_block = true;
                code_lang = trimmed.trim_start_matches("```").trim_end().to_string();
            } else if trimmed == "```" {
                in_code_block = false;
            }
        }

        let candidate = if current.is_empty() {
            para.to_string()
        } else {
            format!("{}\n\n{}", current, para)
        };

        if candidate.len() <= max {
            current = candidate;
        } else {
            // 段落加不进去，先把 current 推走
            flush(&mut current, &mut chunks, &mut in_code_block, &code_lang);

            if para.len() <= max {
                // 段落本身不超 max，直接放到 current（current 可能已有代码块开头）
                if current.is_empty() {
                    current = para.to_string();
                } else {
                    // current 是 ```{lang}\n，追加段落
                    current.push_str(para);
                }
            } else {
                // 段落本身超 max，按行切
                let lines: Vec<&str> = para.split('\n').collect();
                for line in lines {
                    let candidate = if current.is_empty() {
                        line.to_string()
                    } else if current.ends_with('\n') {
                        format!("{}{}", current, line)
                    } else {
                        format!("{}\n{}", current, line)
                    };

                    if candidate.len() <= max {
                        current = candidate;
                    } else {
                        // 当前行加不进去
                        flush(&mut current, &mut chunks, &mut in_code_block, &code_lang);

                        if line.len() > max {
                            // 单行也超 max，按字符硬切
                            // current 可能是 ```{lang}\n，先把这部分作为前缀
                            let prefix = if !current.is_empty() {
                                current.clone()
                            } else {
                                String::new()
                            };
                            let prefix_len = prefix.len();
                            let avail = max.saturating_sub(prefix_len);

                            if prefix_len >= max {
                                // 前缀本身就超 max（极端情况），先推走
                                chunks.push(std::mem::take(&mut current));
                                let mut remaining = line;
                                while remaining.len() > max {
                                    let (chunk, rest) = remaining.split_at(max);
                                    chunks.push(chunk.to_string());
                                    remaining = rest;
                                }
                                current = remaining.to_string();
                            } else {
                                // 第一片带前缀
                                let mut remaining = line;
                                // 先把能装进第一片的装进去
                                let (chunk, rest) = remaining.split_at(avail);
                                current.push_str(chunk);
                                chunks.push(std::mem::take(&mut current));
                                remaining = rest;
                                // 后续片不带前缀，纯字符切
                                while remaining.len() > max {
                                    let (chunk, rest) = remaining.split_at(max);
                                    chunks.push(chunk.to_string());
                                    remaining = rest;
                                }
                                current = remaining.to_string();
                            }
                        } else {
                            // 单行不超 max，直接放入 current
                            if current.is_empty() {
                                current = line.to_string();
                            } else if current.ends_with('\n') {
                                current.push_str(line);
                            } else {
                                current.push('\n');
                                current.push_str(line);
                            }
                        }
                    }
                }
            }
        }
    }

    if !current.is_empty() {
        if in_code_block {
            current.push_str("\n```");
        }
        chunks.push(current);
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_no_split() {
        assert_eq!(split_reply("hi", 100), vec!["hi"]);
    }

    #[test]
    fn test_paragraph_split() {
        let text = "p1\n\np2\n\np3";
        assert_eq!(split_reply(text, 4), vec!["p1", "p2", "p3"]);
    }

    #[test]
    fn test_long_line_char_split() {
        let text = "a".repeat(250);
        let parts = split_reply(&text, 100);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 100);
        assert_eq!(parts[1].len(), 100);
        assert_eq!(parts[2].len(), 50);
    }

    #[test]
    fn test_line_split_when_paragraph_too_long() {
        let text = "aaaaa\nbbbbb\nccccc\nddddd";
        let parts = split_reply(text, 12);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].len() <= 12);
        assert!(parts[1].len() <= 12);
    }

    #[test]
    fn test_code_block_preserved_within_chunk() {
        let text = "前文\n\n```rust\nfn main() {}\n```\n\n后文";
        let parts = split_reply(text, 100);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].contains("```rust"));
    }

    #[test]
    fn test_code_block_split_closes_and_reopens() {
        let long_line = "    println!(\"x\");\n".repeat(100);
        let text = format!("```rust\nfn main() {{\n{}}}\n```", long_line);
        let parts = split_reply(&text, 1800);
        assert!(parts.len() > 1, "expected multiple chunks, got {}", parts.len());
        assert!(parts[0].ends_with("```"), "first chunk should end with ```");
        assert!(parts[1].starts_with("```rust"), "second chunk should start with ```rust");
    }
}
