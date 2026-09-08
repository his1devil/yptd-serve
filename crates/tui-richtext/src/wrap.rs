//! Width-aware wrapping that carries annotations across line breaks.
//!
//! Terminal width is measured in cells, not characters: a CJK ideograph
//! occupies two. Wrapping on character count is the single most common way a
//! Chinese chat client ends up with a ragged right edge, so every measurement
//! here goes through [`unicode_width`], over grapheme clusters rather than
//! `char`s so a combining mark never gets stranded on its own line.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{Block, RichText, Span};

/// Decoration drawn in the gutter, ahead of the line's own text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Prefix {
    pub text: String,
    pub kind: PrefixKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrefixKind {
    Quote,
    Bullet,
    /// Continuation gutter under a bullet, so wrapped text stays indented.
    Indent,
    CodeBorder,
    CodeBody,
}

/// The base role of a whole line. Inline [`Span`]s layer on top of it.
///
/// Two layers, not three: a line has one base role, and ranges inside it carry
/// inline meaning. Anything a renderer needs is reachable from those two.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineStyle {
    Body,
    Heading(u8),
    Quote,
    Code,
    CodeBorder,
}

/// One rendered line: gutter, text, and the annotations that fall inside it.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub prefix: Option<Prefix>,
    /// Right-hand gutter. A framed block has two edges, not one.
    pub suffix: Option<Prefix>,
    pub text: String,
    pub spans: Vec<Span>,
    pub style: LineStyle,
    /// Byte range of the source this line came from, for hit-testing and for
    /// mapping a click back to the original message text.
    pub source_start: usize,
    pub source_end: usize,
}

impl Line {
    fn blank(style: LineStyle, at: usize) -> Self {
        Self {
            prefix: None,
            suffix: None,
            text: String::new(),
            spans: Vec::new(),
            style,
            source_start: at,
            source_end: at,
        }
    }

    /// Display width of gutter plus text.
    pub fn width(&self) -> usize {
        let gutter = |edge: &Option<Prefix>| edge.as_ref().map_or(0, |e| e.text.width());
        gutter(&self.prefix) + self.text.width() + gutter(&self.suffix)
    }
}

/// Wraps `rich` to `width` cells.
pub fn wrap(rich: &RichText, width: usize) -> Vec<Line> {
    wrap_styled(rich, width, LineStyle::Body, None, None)
}

fn wrap_styled(
    rich: &RichText,
    width: usize,
    style: LineStyle,
    first_prefix: Option<Prefix>,
    rest_prefix: Option<Prefix>,
) -> Vec<Line> {
    let gutter = first_prefix
        .as_ref()
        .or(rest_prefix.as_ref())
        .map_or(0, |prefix| prefix.text.width());
    let content_width = width.saturating_sub(gutter).max(1);

    if rich.text.is_empty() {
        let mut line = Line::blank(style, 0);
        line.prefix = first_prefix;
        return vec![line];
    }

    let mut lines = Vec::new();
    let mut logical_start = 0usize;

    for logical in rich.text.split('\n') {
        let logical_end = logical_start + logical.len();
        if logical.is_empty() {
            lines.push(Line::blank(style, logical_start));
        } else {
            for (start, end) in break_points(logical, content_width) {
                let absolute_start = logical_start + start;
                let absolute_end = logical_start + end;
                let visible_end = absolute_start + trimmed_len(&rich.text[absolute_start..absolute_end]);
                lines.push(Line {
                    prefix: None,
                    suffix: None,
                    text: rich.text[absolute_start..visible_end].to_owned(),
                    spans: slice_spans(&rich.spans, absolute_start, visible_end),
                    style,
                    source_start: absolute_start,
                    source_end: absolute_end,
                });
            }
        }
        // `+ 1` steps over the newline that `split` consumed.
        logical_start = logical_end + 1;
    }

    for (index, line) in lines.iter_mut().enumerate() {
        line.prefix = if index == 0 {
            first_prefix.clone().or_else(|| rest_prefix.clone())
        } else {
            rest_prefix.clone()
        };
    }
    lines
}

/// Byte ranges of `value` that each fit in `width` cells.
fn break_points(value: &str, width: usize) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut line_start = 0usize;
    let mut line_width = 0usize;
    // The most recent point we would rather break at than mid-word.
    let mut opportunity: Option<usize> = None;

    for (offset, grapheme) in value.grapheme_indices(true) {
        let cell_width = grapheme.width().max(1);

        if line_width + cell_width > width && offset > line_start {
            let cut = opportunity.filter(|cut| *cut > line_start).unwrap_or(offset);
            ranges.push((line_start, cut));
            line_start = cut;
            line_width = value[cut..offset].width();
            opportunity = None;
        }

        line_width += cell_width;
        if breaks_after(grapheme, cell_width) {
            opportunity = Some(offset + grapheme.len());
        }
    }

    if line_start < value.len() {
        ranges.push((line_start, value.len()));
    }
    ranges
}

/// Whether a line may break immediately after this grapheme.
///
/// Latin text breaks at spaces. CJK has no spaces, so a wide grapheme is its
/// own break opportunity -- without this, one long Chinese sentence would never
/// find a break point and would hard-cut mid-measurement instead.
fn breaks_after(grapheme: &str, cell_width: usize) -> bool {
    grapheme.chars().all(char::is_whitespace) || cell_width >= 2
}

/// Length of `value` with trailing spaces removed, so a break after a space
/// does not leave it painted at the end of the line.
fn trimmed_len(value: &str) -> usize {
    value.trim_end_matches(' ').len()
}

/// Clips spans to `[start, end)` and rebases them to line-local offsets.
fn slice_spans(spans: &[Span], start: usize, end: usize) -> Vec<Span> {
    spans
        .iter()
        .filter_map(|span| {
            let clipped_start = span.start.max(start);
            let clipped_end = span.end.min(end);
            (clipped_start < clipped_end).then(|| Span {
                start: clipped_start - start,
                end: clipped_end - start,
                kind: span.kind,
            })
        })
        .collect()
}

/// Lays out parsed markdown blocks into rendered lines.
pub fn layout_blocks(blocks: &[Block], width: usize) -> Vec<Line> {
    let mut lines = Vec::new();
    for block in blocks {
        match block {
            Block::Paragraph(rich) => lines.extend(wrap(rich, width)),
            Block::Heading { level, content } => {
                lines.extend(wrap_styled(
                    content,
                    width,
                    LineStyle::Heading(*level),
                    None,
                    None,
                ));
            }
            Block::Quote(rich) => {
                let bar = Prefix {
                    text: "▏".to_owned(),
                    kind: PrefixKind::Quote,
                };
                lines.extend(wrap_styled(
                    rich,
                    width,
                    LineStyle::Quote,
                    Some(bar.clone()),
                    Some(bar),
                ));
            }
            Block::Bullet(rich) => {
                lines.extend(wrap_styled(
                    rich,
                    width,
                    LineStyle::Body,
                    Some(Prefix {
                        text: "• ".to_owned(),
                        kind: PrefixKind::Bullet,
                    }),
                    Some(Prefix {
                        text: "  ".to_owned(),
                        kind: PrefixKind::Indent,
                    }),
                ));
            }
            Block::Code {
                language,
                lines: code,
                highlights,
            } => {
                lines.extend(code_block(language.as_deref(), code, highlights, width));
            }
        }
    }
    lines
}

fn code_block(
    language: Option<&str>,
    code: &[String],
    highlights: &[Vec<Span>],
    width: usize,
) -> Vec<Line> {
    // Two side bars and a space of padding on each side.
    let inner = width.saturating_sub(4).max(1);
    let bar = |text: &str| Prefix {
        text: text.to_owned(),
        kind: PrefixKind::CodeBody,
    };
    let mut out = Vec::new();

    let label = language.unwrap_or("");
    let label_cells = label.width() + usize::from(!label.is_empty());
    let mut top = String::from("┌");
    if !label.is_empty() {
        top.push_str(label);
        top.push(' ');
    }
    top.push_str(&"─".repeat((inner + 2).saturating_sub(label_cells)));
    top.push('┐');
    out.push(Line {
        prefix: None,
        suffix: None,
        text: top,
        spans: Vec::new(),
        style: LineStyle::CodeBorder,
        source_start: 0,
        source_end: 0,
    });

    for (index, source_line) in code.iter().enumerate() {
        let line_spans: &[Span] = highlights.get(index).map(Vec::as_slice).unwrap_or(&[]);
        // Code is never word-wrapped: a break inside an identifier is worse
        // than a hard cut at the edge, which at least stays column-aligned.
        let pieces = break_points(source_line, inner);
        let pieces = if pieces.is_empty() { vec![(0, 0)] } else { pieces };
        for (start, end) in pieces {
            let text = &source_line[start..end];
            out.push(Line {
                prefix: Some(bar("│ ")),
                suffix: Some(bar(" │")),
                // Padded so the right edge lines up under the corner above.
                text: format!("{text}{}", " ".repeat(inner.saturating_sub(text.width()))),
                spans: slice_spans(line_spans, start, end),
                style: LineStyle::Code,
                source_start: start,
                source_end: end,
            });
        }
    }

    out.push(Line {
        prefix: None,
        suffix: None,
        text: format!("└{}┘", "─".repeat(inner + 2)),
        spans: Vec::new(),
        style: LineStyle::CodeBorder,
        source_start: 0,
        source_end: 0,
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SpanKind;

    fn texts(lines: &[Line]) -> Vec<&str> {
        lines.iter().map(|line| line.text.as_str()).collect()
    }

    #[test]
    fn latin_wraps_at_word_boundaries() {
        let rich = RichText::plain("the quick brown fox jumps");
        assert_eq!(
            texts(&wrap(&rich, 11)),
            vec!["the quick", "brown fox", "jumps"]
        );
    }

    #[test]
    fn cjk_is_measured_two_cells_wide() {
        // Six ideographs is twelve cells, so a width of 8 fits four.
        let rich = RichText::plain("排期讨论今天出稿");
        let lines = wrap(&rich, 8);
        assert_eq!(texts(&lines), vec!["排期讨论", "今天出稿"]);
        assert!(lines.iter().all(|line| line.width() <= 8));
    }

    #[test]
    fn mixed_scripts_never_exceed_the_width() {
        let rich = RichText::plain("先跑 dry-run 再去掉 --dry-run 参数确认无误");
        for width in 6..30 {
            let lines = wrap(&rich, width);
            for line in &lines {
                assert!(
                    line.width() <= width,
                    "width {width}: {:?} is {} cells",
                    line.text,
                    line.width()
                );
            }
        }
    }

    #[test]
    fn wrapping_never_loses_or_duplicates_text() {
        let source = "同意，@李娜 那你今天能出个修订版吗 please confirm";
        let rich = RichText::plain(source);
        for width in 4..40 {
            let joined: String = wrap(&rich, width)
                .iter()
                .map(|line| source[line.source_start..line.source_end].to_owned())
                .collect();
            assert_eq!(joined, source, "width {width} did not round-trip");
        }
    }

    #[test]
    fn spans_are_clipped_and_rebased_onto_each_line() {
        let source = "hello @lina goodbye";
        let start = source.find('@').expect("fixture");
        let end = start + "@lina".len();
        let rich = RichText::with_spans(
            source,
            vec![Span::new(start, end, SpanKind::MentionOther)],
        );

        let lines = wrap(&rich, 12);
        let carrying: Vec<_> = lines.iter().filter(|line| !line.spans.is_empty()).collect();
        assert_eq!(carrying.len(), 1, "the mention lives on one line");
        let line = carrying[0];
        let span = line.spans[0];
        assert_eq!(&line.text[span.start..span.end], "@lina");
    }

    #[test]
    fn a_span_split_across_a_break_survives_on_both_lines() {
        let source = "aaaa bbbb cccc";
        let rich = RichText::with_spans(source, vec![Span::new(0, source.len(), SpanKind::Bold)]);
        let lines = wrap(&rich, 9);
        assert!(lines.len() > 1, "fixture must actually wrap");
        for line in &lines {
            assert_eq!(line.spans.len(), 1, "{:?}", line.text);
            let span = line.spans[0];
            assert_eq!(span.start, 0);
            assert_eq!(span.end, line.text.len());
        }
    }

    #[test]
    fn explicit_newlines_become_their_own_lines() {
        let rich = RichText::plain("first\n\nthird");
        assert_eq!(texts(&wrap(&rich, 20)), vec!["first", "", "third"]);
    }

    #[test]
    fn a_word_longer_than_the_width_is_hard_cut_rather_than_dropped() {
        let rich = RichText::plain("supercalifragilistic");
        let lines = wrap(&rich, 6);
        assert_eq!(lines.concat_text(), "supercalifragilistic");
        assert!(lines.iter().all(|line| line.width() <= 6));
    }

    #[test]
    fn quote_and_bullet_gutters_shrink_the_content_width() {
        let blocks = vec![
            Block::Quote(RichText::plain("排期讨论今天出稿")),
            Block::Bullet(RichText::plain("排期讨论今天出稿")),
        ];
        for line in layout_blocks(&blocks, 8) {
            assert!(line.width() <= 8, "{line:?}");
            assert!(line.prefix.is_some(), "gutter must be present");
        }
    }

    #[test]
    fn a_code_block_is_framed_and_never_word_wrapped() {
        let blocks = vec![Block::Code {
            language: Some("bash".to_owned()),
            lines: vec!["./scripts/rollback.sh --dry-run".to_owned()],
            highlights: Vec::new(),
        }];
        let lines = layout_blocks(&blocks, 20);
        assert_eq!(lines.first().map(|l| l.style), Some(LineStyle::CodeBorder));
        assert_eq!(lines.last().map(|l| l.style), Some(LineStyle::CodeBorder));
        assert!(lines.first().expect("top").text.contains("bash"));
        for line in &lines {
            assert!(line.width() <= 20, "{line:?}");
        }
        // Body lines are padded so the right edge lines up under the corner,
        // so compare the code itself rather than the padded cells.
        let body: String = lines
            .iter()
            .filter(|line| line.style == LineStyle::Code)
            .map(|line| line.text.trim_end().to_owned())
            .collect();
        assert_eq!(body, "./scripts/rollback.sh --dry-run");

        let top = lines.first().expect("top border");
        let bottom = lines.last().expect("bottom border");
        assert!(top.text.starts_with('┌') && top.text.ends_with('┐'), "{top:?}");
        assert!(bottom.text.starts_with('└') && bottom.text.ends_with('┘'), "{bottom:?}");
        assert_eq!(top.width(), bottom.width(), "the frame must be square");
        for line in lines.iter().filter(|l| l.style == LineStyle::Code) {
            assert_eq!(line.width(), top.width(), "body must reach both edges");
            assert!(line.suffix.is_some(), "body needs a right gutter");
        }
    }

    #[test]
    fn empty_text_still_produces_one_line() {
        assert_eq!(wrap(&RichText::plain(""), 10).len(), 1);
    }

    trait ConcatText {
        fn concat_text(&self) -> String;
    }

    impl ConcatText for Vec<Line> {
        fn concat_text(&self) -> String {
            self.iter().map(|line| line.text.clone()).collect()
        }
    }
}
