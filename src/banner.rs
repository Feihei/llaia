//! 终端展示文案（欢迎 billboard / 退出语）集中管理。
//!
//! chat、serve 等命令都从这里取欢迎/退出文案，保证观感一致。
//! 想改欢迎语、slogan 或退出语，只改这一处，不要再散落到各 channel / command 里。

/// 版本号，编译期取自 Cargo.toml 的 package.version。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 退出语：chat 与 serve 共用，保证两处退出体验一致。
pub const GOODBYE: &str = "(੭aᴗa)੭  Bye~";

/// 横幅中央标语：上下两道横线包裹这一行。
const SLOGAN: &str = "LLAIA  (੭aᴗa)੭  Come On~";

/// 简介副标题，展示在横幅下方。
const SUBTITLE: &str = "Lightweight Local AI Assistant";

/// 生成欢迎横幅：上下两道横线 + 空行包裹中央标语，下方附版本与提示。
///
/// chat 与 serve 都用它作为启动横幅，保证观感统一。返回的字符串已带首尾换行，
/// 直接 `print!` 即可。
pub fn billboard() -> String {
    // 内容区宽度 = 标语显示列宽 + 左右留白，横线与标语均按显示列宽对齐
    let inner = display_width(SLOGAN) + 8;
    let rule: String = "─".repeat(inner);

    let mut s = String::new();
    s.push('\n');
    s.push_str(&rule);
    s.push('\n');
    s.push('\n');
    s.push_str(&center(SLOGAN, inner));
    s.push('\n');
    s.push('\n');
    s.push_str(&rule);
    s.push('\n');
    s.push_str(&format!("llaia v{VERSION}\n"));
    s.push_str(SUBTITLE);
    s.push('\n');
    s.push_str("  /help for commands · /exit to quit · /stop to interrupt · Ctrl+C to abort\n");
    s.push_str("  type while generating to queue; press Enter to send\n");
    s
}

/// 在给定显示宽度内居中：不足则左右补空格（左侧略多），超长原样返回。
/// 补白用宽 1 的 ASCII 空格，保证居中后整体显示列宽恰好等于 `width`。
fn center(content: &str, width: usize) -> String {
    let w = display_width(content);
    if w >= width {
        return content.to_string();
    }
    let pad = width - w;
    let left = pad / 2;
    let right = pad - left;
    format!("{}{}{}", " ".repeat(left), content, " ".repeat(right))
}

/// 字符串的「显示列宽」。等宽终端里大部分字符占 1 列；CJK / 全角 / emoji 等占 2 列。
///
/// 用于横线长度与文字居中，避免直接用字节数导致非 ASCII 字符（如颜文字 `(੭aᴗa)੭`）
/// 错位——`str::len()` 算的是字节数，会让横线比文字短或居中偏移。
fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// 单字符显示列宽：宽字符算 2，其余算 1（覆盖等宽终端常见情况）。
fn char_width(c: char) -> usize {
    let cp = c as u32;
    let wide = (0x1100..=0x115F).contains(&cp)      // Hangul Jamo
        || (0x2E80..=0x303E).contains(&cp)          // CJK 部首
        || (0x3041..=0x33FF).contains(&cp)          // 日文假名 / 括注
        || (0x3400..=0x4DBF).contains(&cp)          // CJK 扩展 A
        || (0x4E00..=0x9FFF).contains(&cp)          // CJK 统一表意
        || (0xA000..=0xA4CF).contains(&cp)          // 彝文
        || (0xAC00..=0xD7A3).contains(&cp)          // Hangul 音节
        || (0xF900..=0xFAFF).contains(&cp)          // CJK 兼容
        || (0xFE30..=0xFE4F).contains(&cp)          // CJK 兼容形式
        || (0xFF00..=0xFF60).contains(&cp)          // 全角 ASCII
        || (0xFFE0..=0xFFE6).contains(&cp)          // 全角符号
        || (0x1F300..=0x1FAFF).contains(&cp)        // emoji 符号
        || (0x20000..=0x3FFFD).contains(&cp); // CJK 扩展 B+
    if wide {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slogan_centered_to_full_width() {
        let inner = display_width(SLOGAN) + 8;
        let line = center(SLOGAN, inner);
        assert_eq!(
            display_width(&line),
            inner,
            "centered display width should equal content area width"
        );
    }

    #[test]
    fn billboard_contains_version_and_slogan() {
        let b = billboard();
        assert!(b.contains(VERSION));
        assert!(b.contains("LLAIA"));
        assert!(b.contains("Come On~"));
    }

    #[test]
    fn non_ascii_emoticon_counts_as_one() {
        // 颜文字字符在等宽终端里按 1 列渲染，不应被算成 2 列
        assert_eq!(char_width('੭'), 1);
        assert_eq!(char_width('ᴗ'), 1);
        assert_eq!(display_width("(੭aᴗa)੭"), 7);
    }
}
