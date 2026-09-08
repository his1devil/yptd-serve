//! A modal list picker: type to filter, arrows to move, Space to tick, Enter
//! to confirm. It serves "invite whom" and "message whom" alike and knows
//! nothing about groups or users beyond ids and labels, so the same widget
//! can front any list later.

use std::collections::BTreeSet;

use crossterm::event::{KeyCode, KeyModifiers};
use im_model::ConversationId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickItem {
    pub id: String,
    pub label: String,
    /// Shown dimmed after the label; the user id, usually.
    pub detail: String,
}

/// What to do with the chosen ids once the popup closes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Purpose {
    Invite {
        group_id: String,
        conversation: ConversationId,
    },
    DirectMessage,
    /// Insert `@somebody` into the draft. The `@` is already typed, so
    /// cancelling leaves it behind as ordinary text.
    Mention,
}

#[derive(Clone, Debug)]
pub struct Picker {
    pub title: String,
    pub purpose: Purpose,
    pub items: Vec<PickItem>,
    pub filter: String,
    /// Position within the *visible* (filtered) list, not `items`.
    pub cursor: usize,
    pub selected: BTreeSet<String>,
    pub multi: bool,
}

pub enum Verdict {
    Continue,
    Cancel,
    Confirm(Vec<String>),
}

impl Picker {
    /// Rows are ordered by id, not label: ids are ASCII and read in a
    /// predictable order, whereas sorting Chinese nicknames by code point
    /// scatters them in a way nobody can scan.
    pub fn new(title: impl Into<String>, purpose: Purpose, mut items: Vec<PickItem>, multi: bool) -> Self {
        items.sort_by(|a, b| a.id.cmp(&b.id).then(a.label.cmp(&b.label)));
        Self::ordered(title, purpose, items, multi)
    }

    /// Keeps the caller's order, for lists where position carries meaning --
    /// "everyone" belongs at the top, not under the Z's.
    pub fn ordered(title: impl Into<String>, purpose: Purpose, items: Vec<PickItem>, multi: bool) -> Self {
        Self {
            title: title.into(),
            purpose,
            items,
            filter: String::new(),
            cursor: 0,
            selected: BTreeSet::new(),
            multi,
        }
    }

    /// Indices into `items` that match the filter, in display order. Matches
    /// label or id, case-insensitively, so "li" finds both 李娜 (lina) and
    /// anyone whose nickname is Lily.
    pub fn visible(&self) -> Vec<usize> {
        let needle = self.filter.to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                needle.is_empty()
                    || item.label.to_lowercase().contains(&needle)
                    || item.id.to_lowercase().contains(&needle)
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub fn current(&self) -> Option<&PickItem> {
        self.visible().get(self.cursor).map(|&index| &self.items[index])
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let count = self.visible().len();
        if count == 0 {
            self.cursor = 0;
            return;
        }
        let next = self.cursor as isize + delta;
        self.cursor = next.clamp(0, count as isize - 1) as usize;
    }

    pub fn toggle(&mut self) {
        let Some(id) = self.current().map(|item| item.id.clone()) else {
            return;
        };
        if !self.selected.remove(&id) {
            self.selected.insert(id);
        }
    }

    pub fn push_char(&mut self, value: char) {
        if value == ' ' {
            return;
        }
        self.filter.push(value);
        self.cursor = 0;
    }

    pub fn pop_char(&mut self) {
        self.filter.pop();
        self.cursor = 0;
    }

    /// The ticked ids, or the item under the cursor when nothing is ticked --
    /// so Enter on a single name does what it looks like it does.
    pub fn chosen(&self) -> Vec<String> {
        if self.multi && !self.selected.is_empty() {
            return self.selected.iter().cloned().collect();
        }
        self.current().map(|item| vec![item.id.clone()]).unwrap_or_default()
    }

    pub fn label_of(&self, id: &str) -> String {
        self.items
            .iter()
            .find(|item| item.id == id)
            .map(|item| item.label.clone())
            .unwrap_or_else(|| id.to_owned())
    }

    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Verdict {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Esc => return Verdict::Cancel,
            KeyCode::Enter => {
                let chosen = self.chosen();
                if !chosen.is_empty() {
                    return Verdict::Confirm(chosen);
                }
            }
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('p' | 'k') if ctrl => self.move_cursor(-1),
            KeyCode::Char('n' | 'j') if ctrl => self.move_cursor(1),
            KeyCode::Char('u') if ctrl => {
                self.filter.clear();
                self.cursor = 0;
            }
            KeyCode::Tab => self.toggle(),
            KeyCode::Char(' ') => {
                if self.multi {
                    self.toggle();
                } else {
                    let chosen = self.chosen();
                    if !chosen.is_empty() {
                        return Verdict::Confirm(chosen);
                    }
                }
            }
            KeyCode::Backspace => self.pop_char(),
            KeyCode::Char(value) if !ctrl => self.push_char(value),
            _ => {}
        }
        Verdict::Continue
    }
}

/// The roster the mock uses when there is no server to ask.
pub fn mock_roster() -> Vec<PickItem> {
    [
        ("zhangwei", "张伟"),
        ("lina", "李娜"),
        ("chenming", "陈明"),
        ("wangqiang", "王强"),
        ("zhaomin", "赵敏"),
        ("sunli", "孙丽"),
        ("zhouyu", "周宇"),
        ("wuhao", "吴昊"),
    ]
    .into_iter()
    .map(|(id, label)| PickItem {
        id: id.to_owned(),
        label: label.to_owned(),
        detail: id.to_owned(),
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker(multi: bool) -> Picker {
        Picker::new("测试", Purpose::DirectMessage, mock_roster(), multi)
    }

    #[test]
    fn typing_filters_by_label_or_id_and_resets_the_cursor() {
        let mut p = picker(true);
        p.cursor = 3;
        p.push_char('l');
        p.push_char('i');
        let names: Vec<&str> = p.visible().iter().map(|&i| p.items[i].id.as_str()).collect();
        assert_eq!(names, vec!["lina", "sunli"], "matches the id substring");
        assert_eq!(p.cursor, 0);
        p.pop_char();
        p.pop_char();
        p.push_char('张');
        assert_eq!(p.current().map(|i| i.id.as_str()), Some("zhangwei"), "matches the label");
    }

    #[test]
    fn space_ticks_in_multi_mode_and_enter_returns_the_ticked_ids() {
        let mut p = picker(true);
        p.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        p.handle_key(KeyCode::Down, KeyModifiers::NONE);
        p.handle_key(KeyCode::Down, KeyModifiers::NONE);
        p.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        match p.handle_key(KeyCode::Enter, KeyModifiers::NONE) {
            Verdict::Confirm(ids) => assert_eq!(ids.len(), 2),
            _ => panic!("enter with ticks must confirm"),
        }
    }

    #[test]
    fn enter_with_nothing_ticked_takes_the_cursor_item() {
        let mut p = picker(true);
        p.handle_key(KeyCode::Down, KeyModifiers::NONE);
        let expected = p.current().map(|i| i.id.clone()).expect("an item");
        match p.handle_key(KeyCode::Enter, KeyModifiers::NONE) {
            Verdict::Confirm(ids) => assert_eq!(ids, vec![expected]),
            _ => panic!("enter must confirm the cursor item"),
        }
    }

    #[test]
    fn single_mode_confirms_on_space_and_escape_cancels() {
        let mut p = picker(false);
        assert!(matches!(p.handle_key(KeyCode::Char(' '), KeyModifiers::NONE), Verdict::Confirm(_)));
        assert!(matches!(p.handle_key(KeyCode::Esc, KeyModifiers::NONE), Verdict::Cancel));
    }

    #[test]
    fn the_cursor_clamps_instead_of_wrapping_or_overflowing() {
        let mut p = picker(true);
        p.move_cursor(-5);
        assert_eq!(p.cursor, 0);
        p.move_cursor(100);
        assert_eq!(p.cursor, p.items.len() - 1);
        p.push_char('z');
        p.push_char('z');
        p.push_char('z');
        assert!(p.visible().is_empty());
        assert!(p.chosen().is_empty(), "no match, nothing to confirm");
        assert!(matches!(p.handle_key(KeyCode::Enter, KeyModifiers::NONE), Verdict::Continue));
    }

    #[test]
    fn a_space_never_lands_in_the_filter() {
        let mut p = picker(false);
        p.push_char(' ');
        assert!(p.filter.is_empty());
    }
}
