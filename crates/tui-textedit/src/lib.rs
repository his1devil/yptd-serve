//! Text editing primitives for a terminal composer.
//!
//! Two rules run through everything here:
//!
//! 1. **The cursor sits on a grapheme boundary, never inside one.** A byte
//!    offset is the storage form, but every move steps whole clusters, so
//!    backspace after an emoji removes the emoji rather than half its
//!    codepoints.
//! 2. **Position and column are different questions.** The cursor's byte
//!    offset says *where in the string*; its column says *where on screen*,
//!    and a CJK character advances the column by two. Conflating them is why
//!    a cursor drifts away from the text it is supposed to be under.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// A multi-line editable buffer with one cursor.
#[derive(Clone, Debug, Default)]
pub struct TextArea {
    text: String,
    cursor: usize,
    /// Column the user was aiming for before vertical movement started, so a
    /// run of up/down keys through short lines returns to the original column
    /// instead of collapsing toward the left edge.
    goal_column: Option<usize>,
}

impl TextArea {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            goal_column: None,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Character count, which is what a message length limit means to a person
    /// -- not bytes, and not display cells.
    pub fn char_count(&self) -> usize {
        self.text.chars().count()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.goal_column = None;
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.goal_column = None;
    }

    /// Line index and display column of the cursor, for placing the terminal
    /// caret.
    pub fn cursor_position(&self) -> (usize, usize) {
        let line_start = line_start_before(&self.text, self.cursor);
        let line = self.text[..self.cursor].matches('\n').count();
        (line, self.text[line_start..self.cursor].width())
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.text.split('\n')
    }

    // ------------------------------------------------------------ editing ---

    pub fn insert_char(&mut self, value: char) {
        self.text.insert(self.cursor, value);
        self.cursor += value.len_utf8();
        self.goal_column = None;
    }

    pub fn insert_str(&mut self, value: &str) {
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
        self.goal_column = None;
    }

    pub fn newline(&mut self) {
        self.insert_char('\n');
    }

    /// Deletes the grapheme before the cursor. Returns whether anything went.
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let start = previous_boundary(&self.text, self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.goal_column = None;
        true
    }

    /// Deletes the grapheme after the cursor.
    pub fn delete(&mut self) -> bool {
        let end = next_boundary(&self.text, self.cursor);
        if end == self.cursor {
            return false;
        }
        self.text.replace_range(self.cursor..end, "");
        self.goal_column = None;
        true
    }

    pub fn delete_word_before(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let start = previous_word_boundary(&self.text, self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.goal_column = None;
        true
    }

    pub fn delete_word_after(&mut self) -> bool {
        let end = next_word_boundary(&self.text, self.cursor);
        if end == self.cursor {
            return false;
        }
        self.text.replace_range(self.cursor..end, "");
        self.goal_column = None;
        true
    }

    /// Deletes from the cursor to the start of the line, like readline's C-u.
    pub fn delete_to_line_start(&mut self) -> bool {
        let start = line_start_before(&self.text, self.cursor);
        if start == self.cursor {
            return false;
        }
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.goal_column = None;
        true
    }

    /// Deletes from the cursor to the end of the line, like readline's C-k.
    pub fn delete_to_line_end(&mut self) -> bool {
        let end = line_end_after(&self.text, self.cursor);
        if end == self.cursor {
            return false;
        }
        self.text.replace_range(self.cursor..end, "");
        self.goal_column = None;
        true
    }

    // ---------------------------------------------------------- movement ---

    pub fn move_left(&mut self) {
        self.cursor = previous_boundary(&self.text, self.cursor);
        self.goal_column = None;
    }

    pub fn move_right(&mut self) {
        self.cursor = next_boundary(&self.text, self.cursor);
        self.goal_column = None;
    }

    pub fn move_word_left(&mut self) {
        self.cursor = previous_word_boundary(&self.text, self.cursor);
        self.goal_column = None;
    }

    pub fn move_word_right(&mut self) {
        self.cursor = next_word_boundary(&self.text, self.cursor);
        self.goal_column = None;
    }

    pub fn move_line_start(&mut self) {
        self.cursor = line_start_before(&self.text, self.cursor);
        self.goal_column = None;
    }

    pub fn move_line_end(&mut self) {
        self.cursor = line_end_after(&self.text, self.cursor);
        self.goal_column = None;
    }

    pub fn move_start(&mut self) {
        self.cursor = 0;
        self.goal_column = None;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.text.len();
        self.goal_column = None;
    }

    /// Moves one line up or down, keeping the display column where possible.
    /// Returns false when there is no line in that direction, which lets a
    /// single-line composer hand the key back to the caller.
    pub fn move_vertical(&mut self, direction: i32) -> bool {
        let line_start = line_start_before(&self.text, self.cursor);
        let goal = self
            .goal_column
            .unwrap_or_else(|| self.text[line_start..self.cursor].width());

        let target_start = match direction {
            d if d < 0 => {
                if line_start == 0 {
                    return false;
                }
                line_start_before(&self.text, line_start - 1)
            }
            d if d > 0 => {
                let end = line_end_after(&self.text, self.cursor);
                if end >= self.text.len() {
                    return false;
                }
                end + 1
            }
            _ => return false,
        };

        let target_end = line_end_after(&self.text, target_start);
        self.cursor = column_to_offset(&self.text[target_start..target_end], goal) + target_start;
        self.goal_column = Some(goal);
        true
    }
}

// ------------------------------------------------------------- boundaries ---

/// Clamps an arbitrary byte offset onto a character boundary.
pub fn clamp(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

pub fn previous_boundary(text: &str, index: usize) -> usize {
    let index = clamp(text, index);
    text[..index]
        .grapheme_indices(true)
        .next_back()
        .map(|(start, _)| start)
        .unwrap_or(0)
}

pub fn next_boundary(text: &str, index: usize) -> usize {
    let index = clamp(text, index);
    text[index..]
        .grapheme_indices(true)
        .next()
        .map(|(_, g)| index + g.len())
        .unwrap_or(text.len())
}

/// Start of the word before `index`: skips any whitespace immediately behind
/// the cursor, then the word itself.
pub fn previous_word_boundary(text: &str, index: usize) -> usize {
    let index = clamp(text, index);
    let mut chars = text[..index].char_indices().rev().peekable();
    while matches!(chars.peek(), Some((_, c)) if c.is_whitespace()) {
        chars.next();
    }
    let mut start = None;
    while let Some(&(offset, c)) = chars.peek() {
        if c.is_whitespace() {
            break;
        }
        start = Some(offset);
        chars.next();
    }
    start.unwrap_or(0)
}

pub fn next_word_boundary(text: &str, index: usize) -> usize {
    let index = clamp(text, index);
    let mut chars = text[index..].char_indices().peekable();
    while matches!(chars.peek(), Some((_, c)) if !c.is_whitespace()) {
        chars.next();
    }
    while matches!(chars.peek(), Some((_, c)) if c.is_whitespace()) {
        chars.next();
    }
    match chars.peek() {
        Some(&(offset, _)) => index + offset,
        None => text.len(),
    }
}

pub fn line_start_before(text: &str, index: usize) -> usize {
    let index = clamp(text, index);
    text[..index].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

pub fn line_end_after(text: &str, index: usize) -> usize {
    let index = clamp(text, index);
    text[index..]
        .find('\n')
        .map(|i| index + i)
        .unwrap_or(text.len())
}

/// Byte offset within `line` closest to display column `column`.
///
/// Lands *before* a wide character whose cell the column falls into: a cursor
/// aimed at the right half of a CJK glyph belongs to its left edge, because
/// there is no position between its two cells.
pub fn column_to_offset(line: &str, column: usize) -> usize {
    let mut width = 0usize;
    for (offset, grapheme) in line.grapheme_indices(true) {
        let cell = grapheme.width().max(1);
        if width + cell > column {
            return offset;
        }
        width += cell;
    }
    line.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backspace_removes_a_whole_grapheme_not_a_byte() {
        let mut a = TextArea::from_text("排期讨论");
        assert!(a.backspace());
        assert_eq!(a.text(), "排期讨");

        // A flag is several codepoints in one cluster.
        let mut a = TextArea::from_text("hi🇨🇳");
        assert!(a.backspace());
        assert_eq!(a.text(), "hi");
    }

    #[test]
    fn backspace_on_an_empty_buffer_is_a_no_op() {
        let mut a = TextArea::new();
        assert!(!a.backspace());
        assert_eq!(a.cursor(), 0);
    }

    #[test]
    fn arrows_step_clusters_in_both_directions() {
        let mut a = TextArea::from_text("a中b");
        a.move_start();
        a.move_right();
        assert_eq!(a.cursor(), 1);
        a.move_right();
        assert_eq!(a.cursor(), 4, "中 is three bytes");
        a.move_left();
        assert_eq!(a.cursor(), 1);
    }

    #[test]
    fn the_cursor_never_lands_mid_character() {
        let text = "同意，@李娜 今天出稿吗";
        let mut a = TextArea::from_text(text);
        a.move_start();
        for _ in 0..40 {
            a.move_right();
            assert!(a.text().is_char_boundary(a.cursor()), "cursor {}", a.cursor());
        }
    }

    #[test]
    fn word_movement_skips_whitespace_then_the_word() {
        let mut a = TextArea::from_text("hello   brave world");
        a.move_word_left();
        assert_eq!(&a.text()[a.cursor()..], "world");
        a.move_word_left();
        assert_eq!(&a.text()[a.cursor()..], "brave world");
    }

    #[test]
    fn delete_word_before_matches_word_movement() {
        let mut a = TextArea::from_text("先跑 dry-run 再确认");
        assert!(a.delete_word_before());
        assert_eq!(a.text(), "先跑 dry-run ");
    }

    #[test]
    fn readline_line_kills() {
        let mut a = TextArea::from_text("first\nsecond half");
        a.move_line_start();
        a.move_word_right();
        assert!(a.delete_to_line_end());
        assert_eq!(a.text(), "first\nsecond ");

        let mut a = TextArea::from_text("abc");
        assert!(a.delete_to_line_start());
        assert_eq!(a.text(), "");
    }

    #[test]
    fn cursor_position_counts_display_cells_not_characters() {
        let mut a = TextArea::from_text("排期");
        assert_eq!(a.cursor_position(), (0, 4), "two CJK chars are four cells");
        a.move_left();
        assert_eq!(a.cursor_position(), (0, 2));
    }

    #[test]
    fn vertical_movement_keeps_the_column_across_a_short_line() {
        let mut a = TextArea::from_text("aaaaaaaa\nbb\ncccccccc");
        a.move_start();
        a.move_line_end(); // column 8 on line 0
        assert_eq!(a.cursor_position(), (0, 8));
        assert!(a.move_vertical(1));
        assert_eq!(a.cursor_position(), (1, 2), "clamped to the short line");
        assert!(a.move_vertical(1));
        assert_eq!(
            a.cursor_position(),
            (2, 8),
            "the original column is restored, not the clamped one"
        );
    }

    #[test]
    fn vertical_movement_reports_when_there_is_nowhere_to_go() {
        let mut a = TextArea::from_text("only one line");
        assert!(!a.move_vertical(-1));
        assert!(!a.move_vertical(1));
    }

    #[test]
    fn a_column_inside_a_wide_glyph_resolves_to_its_left_edge() {
        // "中" occupies columns 0..2; aiming at column 1 is still its start.
        assert_eq!(column_to_offset("中文", 0), 0);
        assert_eq!(column_to_offset("中文", 1), 0);
        assert_eq!(column_to_offset("中文", 2), 3);
        assert_eq!(column_to_offset("中文", 99), 6);
    }

    #[test]
    fn char_count_is_what_a_length_limit_means() {
        let a = TextArea::from_text("排期 ok");
        assert_eq!(a.char_count(), 5);
        assert_eq!(a.text().len(), 9, "bytes differ from characters");
    }

    #[test]
    fn insert_and_newline_track_the_cursor() {
        let mut a = TextArea::new();
        a.insert_str("排期");
        a.newline();
        a.insert_char('x');
        assert_eq!(a.text(), "排期\nx");
        assert_eq!(a.cursor_position(), (1, 1));
    }
}
