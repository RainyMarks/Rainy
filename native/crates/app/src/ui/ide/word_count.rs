//! Word and character counts of a Markdown document as a reader sees it (3.x `word-count.ts`): syntax, link targets and
//! front matter are left out.

use std::sync::OnceLock;

use regex::Regex;

/// Counts the status bar shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WordCount {
    /// Chinese, Japanese and Korean characters each count as a word, like in Word; other scripts count words.
    pub words: usize,
    /// Characters other than spaces.
    pub characters: usize,
    /// Minutes a reader needs: 300 CJK characters or 200 other words a minute, at least one for any text.
    pub minutes: usize,
}

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("a valid pattern"))
}

/// The lines of front matter at the start, if any (`splitFrontMatter`).
fn front_matter_lines(lines: &[&str]) -> usize {
    let opening = lines.first().map(|line| line.trim_end_matches([' ', '\t']));
    let closing: &[&str] = match opening {
        Some("---") => &["---", "..."],
        Some("+++") => &["+++"],
        _ => return 0,
    };
    lines.iter().skip(1).position(|line| closing.contains(&line.trim_end_matches([' ', '\t']))).map_or(0, |at| at + 2)
}

/// The text without Markdown syntax, link targets, HTML tags and front matter (`readableText`).
pub fn readable_text(markdown: &str) -> String {
    static DROPPED: OnceLock<Regex> = OnceLock::new();
    static MARKERS: OnceLock<Regex> = OnceLock::new();
    static IMAGE: OnceLock<Regex> = OnceLock::new();
    static LINK: OnceLock<Regex> = OnceLock::new();
    static TAG: OnceLock<Regex> = OnceLock::new();
    static SYNTAX: OnceLock<Regex> = OnceLock::new();
    // Fences, `$$` lines, reference definitions, thematic breaks and table delimiter rows.
    let dropped = regex(
        &DROPPED,
        r"^(?: {0,3}(?:`{3,}|~{3,}).*| {0,3}\$\$[ \t]*| {0,3}\[[^\]]+\]:.*| {0,3}(?:[-=_*][ \t]*){3,}|[ \t]*\|?(?:[ \t]*:?-+:?[ \t]*\|)+[ \t]*:?-*:?[ \t]*)$",
    );
    // Quote, list, heading and task markers at the start of a line; the space after them is only a space.
    let markers = regex(&MARKERS, r"^(?:[ \t]*(?:>|[-*+][ \t]|\d+[.)][ \t]|#{1,6}(?:[ \t]|$)|\[[ xX]\][ \t]))+");
    let image = regex(&IMAGE, r"!\[([^\]]*)\]\([^)]*\)");
    let link = regex(&LINK, r"\[([^\]]*)\]\([^)]*\)");
    let tag = regex(&TAG, r"<[^>]+>");
    let syntax = regex(&SYNTAX, r"[*_~`|#>\\]");
    let lines: Vec<&str> = markdown.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line)).collect();
    let mut text = String::with_capacity(markdown.len());
    for line in &lines[front_matter_lines(&lines)..] {
        if dropped.is_match(line) {
            text.push('\n');
            continue;
        }
        let line = image.replace_all(line, "$1");
        let line = link.replace_all(&line, "$1");
        let line = tag.replace_all(&line, " ");
        let line = markers.replace(&line, "");
        text.push_str(&syntax.replace_all(&line, " "));
        text.push('\n');
    }
    text
}

/// The counts of a Markdown document (`countWords`).
pub fn count_words(markdown: &str) -> WordCount {
    static CJK: OnceLock<Regex> = OnceLock::new();
    static WORD: OnceLock<Regex> = OnceLock::new();
    let text = readable_text(markdown);
    let cjk_pattern = regex(&CJK, r"[\p{Han}\p{Hiragana}\p{Katakana}\p{Hangul}]");
    let cjk = cjk_pattern.find_iter(&text).count();
    let other = regex(&WORD, r"[\p{L}\p{N}][\p{L}\p{N}\p{M}'’_.-]*").find_iter(&cjk_pattern.replace_all(&text, " ")).count();
    let characters = text.chars().filter(|character| !character.is_whitespace()).count();
    let words = cjk + other;
    let minutes = if words == 0 { 0 } else { ((cjk as f64 / 300.0 + other as f64 / 200.0).round() as usize).max(1) };
    WordCount { words, characters, minutes }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_chinese_characters_and_other_words() {
        assert_eq!(count_words(""), WordCount::default());
        assert_eq!(count_words("你好，世界 hello world"), WordCount { words: 6, characters: 15, minutes: 1 });
        assert_eq!(count_words("Rainy 4.1 is out").words, 4);
    }

    #[test]
    fn leaves_out_syntax_targets_and_front_matter() {
        let markdown = "---\ntitle: 标题\ntags: [a, b]\n---\n# 一级标题\n\n- [x] 完成 **加粗**\n> 引用 [链接](https://example.com/very/long)\n\n![图](a.png)\n\n```rust\nfn main() {}\n```\n\n| 表 | 头 |\n| --- | :-: |\n| 1 | 2 |\n\n***\n[ref]: https://example.com\n<br>";
        let text = readable_text(markdown);
        assert!(!text.contains("title") && !text.contains("https") && !text.contains("```") && !text.contains("---"), "{text}");
        // 一级标题 完成 加粗 引用 链接 图 表 头 → 15 CJK; fn main 1 2 → 4 other words.
        assert_eq!(count_words(markdown).words, 19);
    }

    #[test]
    fn reads_crlf_like_lf() {
        assert_eq!(count_words("# 标题\r\n\r\n正文\r\n"), count_words("# 标题\n\n正文\n"));
    }

    #[test]
    fn needs_a_minute_per_300_chinese_characters() {
        assert_eq!(count_words(&"字".repeat(900)).minutes, 3);
        assert_eq!(count_words(&"word ".repeat(100)).minutes, 1);
    }
}
