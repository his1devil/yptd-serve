//! A deliberately small Markdown subset, matched to what people actually type
//! in chat: emphasis, inline code, fenced code, headings, quotes, bullets, and
//! bare URLs.
//!
//! Markers are *removed* from the text and recorded as [`Span`]s instead of
//! being left in place. That is what keeps wrapping honest -- `**紧急**` is
//! four cells wide on screen, not eight, and only a stripped string measures
//! it correctly.

use crate::{RichText, Span, SpanKind};

/// A block-level element. Blocks never wrap into each other.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Paragraph(RichText),
    Heading { level: u8, content: RichText },
    Quote(RichText),
    Bullet(RichText),
    Code {
        language: Option<String>,
        lines: Vec<String>,
        /// Per-source-line syntax spans, filled in by the renderer before
        /// layout. Empty means "draw it plain", which is what an unknown
        /// language gets.
        highlights: Vec<Vec<Span>>,
    },
}

/// Splits `input` into blocks, parsing inline markup inside each.
pub fn parse_markdown(input: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut quote: Vec<&str> = Vec::new();
    let mut lines = input.split('\n').peekable();

    fn flush<'a>(buffer: &mut Vec<&'a str>, blocks: &mut Vec<Block>, wrap: fn(RichText) -> Block) {
        if !buffer.is_empty() {
            blocks.push(wrap(parse_inline(&buffer.join("\n"))));
            buffer.clear();
        }
    }

    while let Some(line) = lines.next() {
        let trimmed = line.trim_end();

        if let Some(language) = fence_language(trimmed) {
            flush(&mut paragraph, &mut blocks, Block::Paragraph);
            flush(&mut quote, &mut blocks, Block::Quote);
            let mut code = Vec::new();
            for body in lines.by_ref() {
                if fence_language(body.trim_end()).is_some() {
                    break;
                }
                code.push(body.to_owned());
            }
            blocks.push(Block::Code {
                language,
                lines: code,
                highlights: Vec::new(),
            });
            continue;
        }

        if let Some((level, rest)) = heading(trimmed) {
            flush(&mut paragraph, &mut blocks, Block::Paragraph);
            flush(&mut quote, &mut blocks, Block::Quote);
            blocks.push(Block::Heading {
                level,
                content: parse_inline(rest),
            });
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("> ").or_else(|| trimmed.strip_prefix(">")) {
            flush(&mut paragraph, &mut blocks, Block::Paragraph);
            quote.push(rest);
            continue;
        }

        if let Some(rest) = bullet(trimmed) {
            flush(&mut paragraph, &mut blocks, Block::Paragraph);
            flush(&mut quote, &mut blocks, Block::Quote);
            blocks.push(Block::Bullet(parse_inline(rest)));
            continue;
        }

        flush(&mut quote, &mut blocks, Block::Quote);
        paragraph.push(trimmed);
    }

    flush(&mut paragraph, &mut blocks, Block::Paragraph);
    flush(&mut quote, &mut blocks, Block::Quote);
    blocks
}

fn fence_language(line: &str) -> Option<Option<String>> {
    let rest = line.trim_start().strip_prefix("```")?;
    let label = rest.trim();
    Some((!label.is_empty()).then(|| label.to_owned()))
}

fn heading(line: &str) -> Option<(u8, &str)> {
    for level in 1..=3u8 {
        let marker = format!("{} ", "#".repeat(level as usize));
        if let Some(rest) = line.strip_prefix(&marker) {
            return Some((level, rest));
        }
    }
    None
}

fn bullet(line: &str) -> Option<&str> {
    line.strip_prefix("- ").or_else(|| line.strip_prefix("* "))
}

/// Strips inline markers, recording what they meant as spans.
pub fn parse_inline(input: &str) -> RichText {
    let bytes = input.as_bytes();
    let mut text = String::with_capacity(input.len());
    let mut spans = Vec::new();
    let mut index = 0usize;

    while index < bytes.len() {
        // Inline code wins over everything: markers inside it are literal.
        if bytes[index] == b'`'
            && let Some(close) = find(bytes, index + 1, b"`")
        {
            let start = text.len();
            text.push_str(&input[index + 1..close]);
            spans.push(Span::new(start, text.len(), SpanKind::InlineCode));
            index = close + 1;
            continue;
        }

        let mut matched_marker = false;
        for (marker, kind) in [
            ("**", SpanKind::Bold),
            ("~~", SpanKind::Strikethrough),
            ("*", SpanKind::Italic),
        ] {
            if input[index..].starts_with(marker)
                && let Some(close) = find(bytes, index + marker.len(), marker.as_bytes())
                && close > index + marker.len()
            {
                let inner = parse_inline(&input[index + marker.len()..close]);
                let start = text.len();
                text.push_str(&inner.text);
                spans.extend(inner.spans.into_iter().map(|span| Span {
                    start: span.start + start,
                    end: span.end + start,
                    kind: span.kind,
                }));
                spans.push(Span::new(start, text.len(), kind));
                index = close + marker.len();
                matched_marker = true;
                break;
            }
        }
        if matched_marker {
            continue;
        }

        if let Some(end) = url_end(input, index) {
            let start = text.len();
            text.push_str(&input[index..end]);
            spans.push(Span::new(start, text.len(), SpanKind::Url));
            index = end;
            continue;
        }

        let char_len = next_char_len(input, index);
        text.push_str(&input[index..index + char_len]);
        index += char_len;
    }

    RichText::with_spans(text, spans)
}

fn find(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

fn url_end(input: &str, index: usize) -> Option<usize> {
    let rest = &input[index..];
    if !rest.starts_with("http://") && !rest.starts_with("https://") {
        return None;
    }
    // Chinese runs straight into a URL with no space, so whitespace is not a
    // reliable terminator: stop at the first character a URL cannot contain,
    // which any CJK character is.
    let stop = rest
        .find(|c: char| !is_url_char(c))
        .unwrap_or(rest.len());
    // Whatever is left may still end in sentence punctuation that is legal in
    // a URL but almost never meant as part of one.
    let trimmed = rest[..stop].trim_end_matches(['.', ',', ')', ';', ':', '!', '?', '\'']);
    (trimmed.len() > "https://".len()).then(|| index + trimmed.len())
}

fn is_url_char(value: char) -> bool {
    value.is_ascii_alphanumeric() || "-._~:/?#[]@!$&'()*+,;=%".contains(value)
}

fn next_char_len(input: &str, index: usize) -> usize {
    input[index..]
        .chars()
        .next()
        .map_or(1, |value| value.len_utf8())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inline(source: &str) -> RichText {
        parse_inline(source)
    }

    #[test]
    fn markers_are_removed_so_width_reflects_what_is_drawn() {
        let rich = inline("**紧急**修复");
        assert_eq!(rich.text, "紧急修复");
        assert_eq!(rich.spans, vec![Span::new(0, "紧急".len(), SpanKind::Bold)]);
    }

    #[test]
    fn inline_code_is_literal_inside() {
        let rich = inline("跑 `./run --dry-run **x**` 看看");
        assert_eq!(rich.text, "跑 ./run --dry-run **x** 看看");
        let code = rich
            .spans
            .iter()
            .find(|span| span.kind == SpanKind::InlineCode)
            .expect("code span");
        assert_eq!(&rich.text[code.start..code.end], "./run --dry-run **x**");
        assert!(!rich.spans.iter().any(|span| span.kind == SpanKind::Bold));
    }

    #[test]
    fn emphasis_nests() {
        let rich = inline("**外 *内* 层**");
        assert_eq!(rich.text, "外 内 层");
        assert!(rich.spans.iter().any(|s| s.kind == SpanKind::Bold));
        let italic = rich
            .spans
            .iter()
            .find(|s| s.kind == SpanKind::Italic)
            .expect("italic span");
        assert_eq!(&rich.text[italic.start..italic.end], "内");
    }

    #[test]
    fn unclosed_markers_stay_literal() {
        let rich = inline("**没闭合");
        assert_eq!(rich.text, "**没闭合");
        assert!(rich.spans.is_empty());
    }

    #[test]
    fn bare_urls_become_spans_without_trailing_punctuation() {
        let rich = inline("见 https://im.yptd.cn/docs，谢谢");
        let url = rich
            .spans
            .iter()
            .find(|s| s.kind == SpanKind::Url)
            .expect("url span");
        assert_eq!(&rich.text[url.start..url.end], "https://im.yptd.cn/docs");
    }

    #[test]
    fn spans_always_land_on_character_boundaries() {
        for source in [
            "**紧急**修复",
            "跑 `代码` 看看",
            "见 https://im.yptd.cn/docs，谢谢",
            "**外 *内* 层**",
        ] {
            let rich = inline(source);
            for span in &rich.spans {
                assert!(
                    rich.text.is_char_boundary(span.start) && rich.text.is_char_boundary(span.end),
                    "{source}: span {span:?} splits a character"
                );
            }
        }
    }

    #[test]
    fn blocks_split_on_structure() {
        let blocks = parse_markdown(
            "# 标题\n正文一行\n> 引用\n> 第二行\n- 条目\n```bash\n./run.sh\n```\n收尾",
        );
        assert!(matches!(blocks[0], Block::Heading { level: 1, .. }));
        assert!(matches!(blocks[1], Block::Paragraph(_)));
        assert!(matches!(blocks[2], Block::Quote(_)));
        assert!(matches!(blocks[3], Block::Bullet(_)));
        assert!(matches!(blocks[4], Block::Code { .. }));
        assert!(matches!(blocks[5], Block::Paragraph(_)));
    }

    #[test]
    fn a_fence_keeps_its_language_and_body_verbatim() {
        let blocks = parse_markdown("```bash\n./scripts/rollback.sh --dry-run\n```");
        let Block::Code { language, lines, .. } = &blocks[0] else {
            panic!("expected a code block, got {:?}", blocks[0]);
        };
        assert_eq!(language.as_deref(), Some("bash"));
        assert_eq!(lines, &["./scripts/rollback.sh --dry-run".to_owned()]);
    }

    #[test]
    fn consecutive_quote_lines_merge_into_one_block() {
        let blocks = parse_markdown("> 第一行\n> 第二行");
        assert_eq!(blocks.len(), 1);
        let Block::Quote(rich) = &blocks[0] else {
            panic!("expected a quote");
        };
        assert_eq!(rich.text, "第一行\n第二行");
    }

    #[test]
    fn a_bullet_line_is_not_read_as_italics() {
        let blocks = parse_markdown("* 待办一项");
        assert!(matches!(blocks[0], Block::Bullet(_)));
    }

    #[test]
    fn an_unterminated_fence_still_produces_a_code_block() {
        let blocks = parse_markdown("```\nrun me\n没有闭合");
        let Block::Code { lines, .. } = &blocks[0] else {
            panic!("expected a code block");
        };
        assert_eq!(lines, &["run me".to_owned(), "没有闭合".to_owned()]);
    }
}
