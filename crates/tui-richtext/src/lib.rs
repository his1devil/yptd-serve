//! Plain text plus byte-range annotations, wrapped to a terminal width.
//!
//! The whole crate rests on one decision: **text and meaning are stored
//! apart**. A [`RichText`] is a `String` with a list of byte ranges saying what
//! parts of it are. Nothing here knows about colors -- a range says
//! `SpanKind::MentionSelf`, and the theme decides what that looks like.
//!
//! That split pays off twice. Width measurement only ever looks at the string,
//! so it cannot be confused by markup. And when text is rewritten -- an OpenIM
//! `@` token expanded to a nickname, a timestamp formatted for the local zone --
//! [`Rewriter`] *moves* the existing annotations to their new offsets instead of
//! re-parsing the result. Re-parsing generated output is how mention
//! highlighting quietly drifts one character to the left.

pub mod markdown;
pub mod wrap;

pub use markdown::{Block, parse_markdown};
pub use wrap::{Line, LineStyle, Prefix, PrefixKind, layout_blocks, wrap};

/// What a byte range of text means. Never what it looks like.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpanKind {
    /// A mention that notifies the local user.
    MentionSelf,
    /// A mention of somebody else.
    MentionOther,
    /// `@全体成员`.
    MentionAll,
    Url,
    InlineCode,
    Bold,
    Italic,
    Strikethrough,
    /// Text produced from a timestamp token.
    Timestamp,
    /// A literal 0xRRGGBB foreground. The single exception to "spans carry
    /// meaning, not appearance": syntect owns code coloring, and its themes
    /// express relationships between token classes that a fixed set of
    /// highlight groups cannot. Everything else still names a group.
    Syntax(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub kind: SpanKind,
}

impl Span {
    pub fn new(start: usize, end: usize, kind: SpanKind) -> Self {
        Self { start, end, kind }
    }

    fn is_empty(self) -> bool {
        self.end <= self.start
    }
}

/// A string plus what its byte ranges mean.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RichText {
    pub text: String,
    pub spans: Vec<Span>,
}

impl RichText {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            spans: Vec::new(),
        }
    }

    pub fn with_spans(text: impl Into<String>, spans: Vec<Span>) -> Self {
        let mut rich = Self {
            text: text.into(),
            spans,
        };
        rich.normalize();
        rich
    }

    pub fn push_span(&mut self, span: Span) {
        if !span.is_empty() && span.end <= self.text.len() {
            self.spans.push(span);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Drops empty and out-of-bounds spans, then orders them so a renderer can
    /// walk spans and text together in one pass.
    pub fn normalize(&mut self) {
        let len = self.text.len();
        self.spans.retain(|span| !span.is_empty() && span.start < len);
        for span in &mut self.spans {
            span.end = span.end.min(len);
        }
        self.spans.sort_by_key(|span| (span.start, span.end));
    }
}

impl From<&str> for RichText {
    fn from(value: &str) -> Self {
        Self::plain(value)
    }
}

/// One rewritten byte range, recorded so annotations can follow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Replacement {
    input_start: usize,
    input_end: usize,
    output_start: usize,
    output_len: usize,
}

/// Rewrites byte ranges of a [`RichText`] while keeping its spans aligned.
///
/// Replacements must be supplied in ascending, non-overlapping order -- the
/// same order a left-to-right scanner finds them in, so callers never have to
/// sort. Out-of-order input is ignored rather than silently corrupting offsets.
#[derive(Debug)]
pub struct Rewriter {
    source: String,
    spans: Vec<Span>,
    output: String,
    replacements: Vec<Replacement>,
    cursor: usize,
    added: Vec<Span>,
}

impl Rewriter {
    pub fn new(rich: RichText) -> Self {
        Self {
            source: rich.text,
            spans: rich.spans,
            output: String::new(),
            replacements: Vec::new(),
            cursor: 0,
            added: Vec::new(),
        }
    }

    /// Replaces `start..end` of the source with `text`.
    ///
    /// When `kind` is given the replacement itself becomes a span, which is how
    /// an expanded mention gets highlighted without a second parse.
    pub fn replace(&mut self, start: usize, end: usize, text: &str, kind: Option<SpanKind>) {
        if start < self.cursor || end < start || end > self.source.len() {
            return;
        }
        if !self.source.is_char_boundary(start) || !self.source.is_char_boundary(end) {
            return;
        }

        self.output.push_str(&self.source[self.cursor..start]);
        let output_start = self.output.len();
        self.output.push_str(text);
        let output_len = self.output.len() - output_start;

        self.replacements.push(Replacement {
            input_start: start,
            input_end: end,
            output_start,
            output_len,
        });
        if let Some(kind) = kind {
            self.added
                .push(Span::new(output_start, output_start + output_len, kind));
        }
        self.cursor = end;
    }

    /// Finishes the rewrite, remapping every original span onto the new text.
    pub fn finish(mut self) -> RichText {
        self.output.push_str(&self.source[self.cursor..]);

        let mut spans: Vec<Span> = self
            .spans
            .iter()
            .map(|span| Span {
                start: remap(&self.replacements, span.start),
                end: remap(&self.replacements, span.end),
                kind: span.kind,
            })
            .collect();
        spans.extend(self.added);

        RichText::with_spans(self.output, spans)
    }
}

/// Maps a source byte offset onto the rewritten text.
///
/// A position inside a replaced range lands at the closest point inside the
/// replacement rather than jumping past it, so a span that overlapped the
/// rewritten text still covers it afterwards.
fn remap(replacements: &[Replacement], position: usize) -> usize {
    let mut delta: isize = 0;
    for replacement in replacements {
        if position < replacement.input_start {
            break;
        }
        if position < replacement.input_end {
            let inside = position - replacement.input_start;
            return replacement.output_start + inside.min(replacement.output_len);
        }
        delta += replacement.output_len as isize
            - (replacement.input_end - replacement.input_start) as isize;
    }
    position.saturating_add_signed(delta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_survive_a_rewrite_that_lengthens_the_text() {
        // "同意，@u_lina 出稿" -- the token is replaced by a nickname.
        let source = "同意，@u_lina 今天出稿吗";
        let tail_start = source.find("今天").expect("fixture");
        let rich = RichText::with_spans(
            source,
            vec![Span::new(tail_start, source.len(), SpanKind::Bold)],
        );

        let token_start = source.find('@').expect("fixture");
        let token_end = token_start + "@u_lina".len();
        let mut rewriter = Rewriter::new(rich);
        rewriter.replace(
            token_start,
            token_end,
            "@李娜",
            Some(SpanKind::MentionOther),
        );
        let out = rewriter.finish();

        assert_eq!(out.text, "同意，@李娜 今天出稿吗");
        let bold = out
            .spans
            .iter()
            .find(|span| span.kind == SpanKind::Bold)
            .expect("bold span survived");
        assert_eq!(&out.text[bold.start..bold.end], "今天出稿吗");

        let mention = out
            .spans
            .iter()
            .find(|span| span.kind == SpanKind::MentionOther)
            .expect("mention span added");
        assert_eq!(&out.text[mention.start..mention.end], "@李娜");
    }

    #[test]
    fn spans_survive_a_rewrite_that_shortens_the_text() {
        let source = "开始 <t:1788760800:f> 结束";
        let tail = source.find("结束").expect("fixture");
        let rich = RichText::with_spans(source, vec![Span::new(tail, source.len(), SpanKind::Bold)]);

        let start = source.find('<').expect("fixture");
        let end = source.find('>').expect("fixture") + 1;
        let mut rewriter = Rewriter::new(rich);
        rewriter.replace(start, end, "14:00", Some(SpanKind::Timestamp));
        let out = rewriter.finish();

        assert_eq!(out.text, "开始 14:00 结束");
        let bold = out
            .spans
            .iter()
            .find(|span| span.kind == SpanKind::Bold)
            .expect("bold survived");
        assert_eq!(&out.text[bold.start..bold.end], "结束");
    }

    #[test]
    fn several_replacements_compose() {
        let source = "@a 和 @b 都要看";
        let mut rewriter = Rewriter::new(RichText::plain(source));
        rewriter.replace(0, 2, "@李娜", Some(SpanKind::MentionOther));
        let second = source.find("@b").expect("fixture");
        rewriter.replace(second, second + 2, "@陈明", Some(SpanKind::MentionSelf));
        let out = rewriter.finish();

        assert_eq!(out.text, "@李娜 和 @陈明 都要看");
        for span in &out.spans {
            let slice = &out.text[span.start..span.end];
            assert!(slice == "@李娜" || slice == "@陈明", "got {slice}");
        }
    }

    #[test]
    fn out_of_order_or_ragged_replacements_are_refused_not_misapplied() {
        let source = "abcdef";
        let mut rewriter = Rewriter::new(RichText::plain(source));
        rewriter.replace(3, 5, "XY", None);
        rewriter.replace(0, 2, "ZZ", None); // backwards -- ignored
        rewriter.replace(5, 99, "!!", None); // out of bounds -- ignored
        assert_eq!(rewriter.finish().text, "abcXYf");
    }

    #[test]
    fn a_replacement_never_splits_a_multibyte_character() {
        let source = "中文";
        let mut rewriter = Rewriter::new(RichText::plain(source));
        rewriter.replace(1, 2, "!", None); // inside the first char -- ignored
        assert_eq!(rewriter.finish().text, "中文");
    }

    #[test]
    fn normalize_drops_empty_and_clamps_overlong_spans() {
        let rich = RichText::with_spans(
            "abc",
            vec![
                Span::new(1, 1, SpanKind::Bold),
                Span::new(2, 99, SpanKind::Url),
                Span::new(9, 12, SpanKind::Italic),
            ],
        );
        assert_eq!(rich.spans, vec![Span::new(2, 3, SpanKind::Url)]);
    }
}
