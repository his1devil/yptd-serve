//! Interaction state: which pane has focus, which conversation is open, and
//! where the two message cursors sit.

use im_model::{Conversation, ConversationId, ConversationKind, Member, Message, Role, Snapshot};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pane {
    Conversations,
    Messages,
    Members,
}

impl Pane {
    pub const ORDER: [Self; 3] = [Self::Conversations, Self::Messages, Self::Members];

    pub fn index(self) -> usize {
        Self::ORDER.iter().position(|p| *p == self).unwrap_or(0)
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Conversations => "会话",
            Self::Messages => "消息",
            Self::Members => "成员",
        }
    }

    fn cycle(self, forward: bool) -> Self {
        let count = Self::ORDER.len();
        let step = if forward { 1 } else { count - 1 };
        Self::ORDER[(self.index() + step) % count]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Normal,
    Insert,
    Command,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::Command => "COMMAND",
        }
    }
}

/// A row in the conversation pane: either a collapsible category or a
/// conversation under one.
#[derive(Clone, Debug)]
pub enum NavRow {
    Category {
        name: String,
        collapsed: bool,
        /// Shown while collapsed, so folding a category does not hide how much
        /// was folded away.
        count: usize,
    },
    Conversation {
        index: usize,
    },
}

pub struct App {
    pub snapshot: Snapshot,
    pub pane: Pane,
    pub mode: Mode,
    pub open: ConversationId,
    /// Selection cursor in the message pane.
    pub message_cursor: usize,
    /// Topmost visible message. Separate from the cursor on purpose: `j`/`k`
    /// move the cursor and drag the viewport along, `J`/`K` move the viewport
    /// and leave the cursor where it is.
    pub message_scroll: usize,
    /// New messages scroll into view only while the cursor is already at the
    /// bottom, so reading history is never yanked away.
    pub follow_latest: bool,
    pub nav_cursor: usize,
    pub member_cursor: usize,
    pub collapsed: Vec<String>,
    pub composer: String,
    pub show_members: bool,
    pub show_conversations: bool,
}

impl App {
    pub fn new(snapshot: Snapshot) -> Self {
        let open = snapshot
            .conversations
            .first()
            .map(|conversation| conversation.id.clone())
            .unwrap_or(ConversationId(String::new()));
        let mut app = Self {
            snapshot,
            pane: Pane::Messages,
            mode: Mode::Normal,
            open,
            message_cursor: 0,
            message_scroll: 0,
            follow_latest: true,
            nav_cursor: 0,
            member_cursor: 0,
            collapsed: Vec::new(),
            composer: String::new(),
            show_members: true,
            show_conversations: true,
        };
        app.jump_to_latest();
        app
    }

    pub fn conversation(&self) -> Option<&Conversation> {
        self.snapshot.conversation(&self.open)
    }

    pub fn messages(&self) -> Vec<&Message> {
        self.snapshot.messages_in(&self.open)
    }

    pub fn members(&self) -> Vec<&Member> {
        let mut members: Vec<&Member> = self.snapshot.members.iter().collect();
        // Owner first, then admins, then members; online before offline inside
        // each tier so the people who can answer are at the top.
        members.sort_by(|a, b| {
            b.role
                .cmp(&a.role)
                .then(b.online.cmp(&a.online))
                .then(a.name.cmp(&b.name))
        });
        members
    }

    pub fn members_by_role(&self) -> Vec<(Role, Vec<&Member>)> {
        let mut groups: Vec<(Role, Vec<&Member>)> = Vec::new();
        for member in self.members() {
            match groups.last_mut() {
                Some((role, bucket)) if *role == member.role => bucket.push(member),
                _ => groups.push((member.role, vec![member])),
            }
        }
        groups
    }

    /// Conversation rows in display order, with collapsed categories folded.
    pub fn nav_rows(&self) -> Vec<NavRow> {
        let mut rows = Vec::new();
        let mut current: Option<String> = None;

        for index in 0..self.snapshot.conversations.len() {
            let category = self.category_of(index);
            if current.as_deref() != Some(category.as_str()) {
                rows.push(NavRow::Category {
                    collapsed: self.is_collapsed(&category),
                    count: self
                        .snapshot
                        .conversations
                        .iter()
                        .enumerate()
                        .filter(|(other, _)| self.category_of(*other) == category)
                        .count(),
                    name: category.clone(),
                });
                current = Some(category.clone());
            }
            if !self.is_collapsed(&category) {
                rows.push(NavRow::Conversation { index });
            }
        }
        rows
    }

    fn category_of(&self, index: usize) -> String {
        self.snapshot
            .conversations
            .get(index)
            .and_then(|conversation| conversation.category.clone())
            .unwrap_or_else(|| "私聊".to_owned())
    }

    fn is_collapsed(&self, category: &str) -> bool {
        self.collapsed.iter().any(|name| name == category)
    }

    pub fn toggle_collapsed(&mut self) {
        let rows = self.nav_rows();
        let Some(row) = rows.get(self.nav_cursor) else {
            return;
        };
        let category = match row {
            NavRow::Category { name, .. } => name.clone(),
            NavRow::Conversation { index } => self.category_of(*index),
        };
        if let Some(position) = self.collapsed.iter().position(|name| *name == category) {
            self.collapsed.remove(position);
        } else {
            self.collapsed.push(category);
        }
        self.nav_cursor = self.nav_cursor.min(self.nav_rows().len().saturating_sub(1));
    }

    pub fn focus(&mut self, pane: Pane) {
        self.pane = pane;
    }

    pub fn cycle_focus(&mut self, forward: bool) {
        let mut next = self.pane.cycle(forward);
        // Skip panes the user has hidden rather than focusing an invisible one.
        for _ in 0..Pane::ORDER.len() {
            if self.pane_visible(next) {
                break;
            }
            next = next.cycle(forward);
        }
        self.pane = next;
    }

    pub fn pane_visible(&self, pane: Pane) -> bool {
        match pane {
            Pane::Conversations => self.show_conversations,
            Pane::Messages => true,
            Pane::Members => self.show_members,
        }
    }

    /// `j` / `k`: move the selection, letting the viewport follow.
    pub fn select(&mut self, delta: isize) {
        match self.pane {
            Pane::Messages => {
                let count = self.messages().len();
                self.message_cursor = step(self.message_cursor, delta, count);
                self.follow_latest = self.message_cursor + 1 >= count;
            }
            Pane::Conversations => {
                let rows = self.nav_rows();
                self.nav_cursor = step(self.nav_cursor, delta, rows.len());
            }
            Pane::Members => {
                let count = self.members().len();
                self.member_cursor = step(self.member_cursor, delta, count);
            }
        }
    }

    /// `J` / `K`: move the viewport, leaving the selection alone.
    pub fn scroll(&mut self, delta: isize) {
        if self.pane != Pane::Messages {
            self.select(delta);
            return;
        }
        let count = self.messages().len();
        self.message_scroll = step(self.message_scroll, delta, count);
        self.follow_latest = false;
    }

    pub fn jump_to_latest(&mut self) {
        let count = self.messages().len();
        self.message_cursor = count.saturating_sub(1);
        self.follow_latest = true;
    }

    pub fn jump_to_top(&mut self) {
        self.message_cursor = 0;
        self.message_scroll = 0;
        self.follow_latest = false;
    }

    pub fn open_selected(&mut self) {
        if self.pane != Pane::Conversations {
            return;
        }
        let rows = self.nav_rows();
        match rows.get(self.nav_cursor) {
            Some(NavRow::Conversation { index }) => {
                if let Some(conversation) = self.snapshot.conversations.get(*index) {
                    self.open = conversation.id.clone();
                    self.message_scroll = 0;
                    self.jump_to_latest();
                    self.pane = Pane::Messages;
                }
            }
            Some(NavRow::Category { .. }) => self.toggle_collapsed(),
            None => {}
        }
    }

    /// Where the unread divider goes: before the first message the local user
    /// has not read. `None` when everything is read.
    pub fn unread_boundary(&self) -> Option<usize> {
        let read_up_to = self.conversation()?.read_up_to?;
        self.messages()
            .iter()
            .position(|message| message.id > read_up_to)
    }

    pub fn conversation_glyph(kind: ConversationKind) -> &'static str {
        match kind {
            ConversationKind::Group => "#",
            ConversationKind::Direct => "@",
        }
    }
}

fn step(current: usize, delta: isize, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let last = count - 1;
    current.saturating_add_signed(delta).min(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::new(im_model::mock::snapshot())
    }

    #[test]
    fn opens_on_the_newest_message_following_the_live_edge() {
        let app = app();
        assert_eq!(app.message_cursor, app.messages().len() - 1);
        assert!(app.follow_latest);
    }

    #[test]
    fn moving_the_cursor_off_the_bottom_stops_following() {
        let mut app = app();
        app.select(-1);
        assert!(!app.follow_latest, "reading history must not be yanked back");
        app.select(1);
        assert!(app.follow_latest, "returning to the bottom resumes following");
    }

    #[test]
    fn scrolling_the_viewport_leaves_the_cursor_alone() {
        let mut app = app();
        let cursor = app.message_cursor;
        app.scroll(-1);
        assert_eq!(app.message_cursor, cursor, "J/K must not move the selection");
        assert!(!app.follow_latest);
    }

    #[test]
    fn cursors_never_run_off_either_end() {
        let mut app = app();
        for _ in 0..50 {
            app.select(-1);
        }
        assert_eq!(app.message_cursor, 0);
        for _ in 0..50 {
            app.select(1);
        }
        assert_eq!(app.message_cursor, app.messages().len() - 1);
    }

    #[test]
    fn the_unread_divider_sits_above_exactly_the_unread_messages() {
        let app = app();
        let boundary = app.unread_boundary().expect("fixture has unread messages");
        let unread = app.messages().len() - boundary;
        assert_eq!(unread as u32, app.conversation().expect("open").unread);
    }

    #[test]
    fn collapsing_a_category_hides_its_conversations() {
        let mut app = app();
        app.pane = Pane::Conversations;
        let before = app.nav_rows().len();
        app.nav_cursor = 0; // the first category header
        app.toggle_collapsed();
        let after = app.nav_rows().len();
        assert!(after < before, "collapsing must remove rows");
        app.toggle_collapsed();
        assert_eq!(app.nav_rows().len(), before, "expanding restores them");
    }

    #[test]
    fn a_category_row_reports_how_many_conversations_it_holds() {
        let app = app();
        let NavRow::Category { name, count, .. } = &app.nav_rows()[0] else {
            panic!("the first row is a category header");
        };
        let expected = app
            .snapshot
            .conversations
            .iter()
            .filter(|conversation| conversation.category.as_deref() == Some(name.as_str()))
            .count();
        assert_eq!(*count, expected);
    }

    #[test]
    fn focus_cycling_skips_hidden_panes() {
        let mut app = app();
        app.show_members = false;
        app.pane = Pane::Messages;
        app.cycle_focus(true);
        assert_eq!(app.pane, Pane::Conversations, "must skip the hidden members pane");
    }

    #[test]
    fn members_are_grouped_owner_first_then_online_first() {
        let app = app();
        let groups = app.members_by_role();
        assert_eq!(groups.first().map(|(role, _)| *role), Some(Role::Owner));
        for (_, bucket) in &groups {
            let first_offline = bucket.iter().position(|member| !member.online);
            if let Some(cut) = first_offline {
                assert!(
                    bucket[cut..].iter().all(|member| !member.online),
                    "online members must sort ahead of offline ones"
                );
            }
        }
    }

    #[test]
    fn opening_a_conversation_moves_focus_and_resets_the_viewport() {
        let mut app = app();
        app.pane = Pane::Conversations;
        app.nav_cursor = 1; // the first conversation under the first category
        app.open_selected();
        assert_eq!(app.pane, Pane::Messages);
        assert_eq!(app.message_scroll, 0);
        assert!(app.follow_latest);
    }
}
