//! Interaction state: which pane has focus, which conversation is open, and
//! where the two message cursors sit.

use im_model::{
    Conversation, ConversationId, ConversationKind, Member, Message, MessageId, Role, Snapshot,
};
use tui_textedit::TextArea;

use std::path::PathBuf;

use crate::browser::Browser;
use crate::picker::Picker;

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

/// Somebody the draft mentions: who to notify, and the text that stands for
/// them in the message. The SDK wants both -- the id to route the
/// notification, the nickname to find the mention inside the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftMention {
    pub user_id: String,
    pub nickname: String,
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
    pub composer: TextArea,
    /// Which message each visible row of the message pane belongs to, written
    /// by the renderer each frame. Mouse events are handled against the frame
    /// the user actually clicked on, so last frame's map is the right one.
    pub message_line_map: Vec<Option<usize>>,
    pub show_members: bool,
    pub show_conversations: bool,
    /// A one-line status message: a send failure, a lost sidecar. Cleared on
    /// the next keypress so it never lingers past its relevance.
    pub notice: Option<String>,
    /// What the clock says, so "今天" means today. Fixed for the mock and the
    /// off-screen captures, which must render the same on any day.
    pub now_ms: i64,
    /// Added to every timestamp before display. Zero for the mock and the
    /// off-screen captures so a test renders the same in every time zone;
    /// the live client sets it from the machine's zone at startup.
    pub utc_offset_ms: i64,
    /// A modal list over everything else. While it is open, keys go to it
    /// and the panes underneath are inert.
    pub picker: Option<Picker>,
    /// The message the draft is a reply to. Belongs to [`Self::open`], so it
    /// is dropped whenever the conversation changes.
    pub reply_to: Option<MessageId>,
    /// Who the draft mentions so far. Recorded when `@` picks somebody, then
    /// filtered at send time against what is actually still in the text.
    pub draft_mentions: Vec<DraftMention>,
    /// Pictures staged for the next send. Dragging files in or picking them
    /// from the browser puts them here rather than sending at once, so a
    /// caption can be typed and several pictures can go together.
    pub pending: Vec<PathBuf>,
    /// The file browser, when it is open. Like the picker it owns the
    /// keyboard while it is up.
    pub browser: Option<Browser>,
    /// Where the browser was last time, so it reopens where it was left.
    pub last_dir: Option<PathBuf>,
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
            composer: TextArea::new(),
            message_line_map: Vec::new(),
            show_members: true,
            show_conversations: true,
            notice: None,
            now_ms: im_model::mock::BASE_MS,
            utc_offset_ms: 0,
            picker: None,
            reply_to: None,
            draft_mentions: Vec::new(),
            pending: Vec::new(),
            browser: None,
            last_dir: None,
        };
        app.jump_to_latest();
        app
    }

    /// A shallow copy for off-screen rendering, where the caller only has a
    /// shared reference but `draw` needs to record its line map somewhere.
    pub fn clone_for_render(&self) -> App {
        App {
            snapshot: self.snapshot.clone(),
            pane: self.pane,
            mode: self.mode,
            open: self.open.clone(),
            message_cursor: self.message_cursor,
            message_scroll: self.message_scroll,
            follow_latest: self.follow_latest,
            nav_cursor: self.nav_cursor,
            member_cursor: self.member_cursor,
            collapsed: self.collapsed.clone(),
            composer: self.composer.clone(),
            message_line_map: Vec::new(),
            show_members: self.show_members,
            show_conversations: self.show_conversations,
            notice: self.notice.clone(),
            now_ms: self.now_ms,
            utc_offset_ms: self.utc_offset_ms,
            picker: self.picker.clone(),
            reply_to: self.reply_to,
            draft_mentions: self.draft_mentions.clone(),
            pending: self.pending.clone(),
            browser: self.browser.clone(),
            last_dir: self.last_dir.clone(),
        }
    }

    pub fn conversation(&self) -> Option<&Conversation> {
        self.snapshot.conversation(&self.open)
    }

    /// Whether there is somewhere to send to.
    ///
    /// A brand-new account has no conversations at all, and `open` is then
    /// the empty id. Handing that to the SDK produces "invalid input
    /// arguments" and nothing else, so every send checks this first.
    pub fn has_open_conversation(&self) -> bool {
        !self.open.0.is_empty() && self.conversation().is_some()
    }

    /// How many rows the composer needs: its text, plus one for the strip
    /// naming what the draft is replying to and one for staged pictures.
    pub fn composer_rows(&self) -> usize {
        self.composer.lines().count().max(1)
            + usize::from(self.reply_to.is_some())
            + usize::from(!self.pending.is_empty())
    }

    /// Stages pictures for the next send, ignoring ones already staged.
    ///
    /// Returns how many were added. The cap is not a protocol limit -- each
    /// picture is its own message -- but sending twenty at once is almost
    /// always a mistake, and the album only draws four anyway.
    pub fn attach(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> usize {
        const MAX_PENDING: usize = 10;
        let mut added = 0;
        for path in paths {
            if self.pending.len() >= MAX_PENDING {
                break;
            }
            if self.pending.contains(&path) {
                continue;
            }
            self.pending.push(path);
            added += 1;
        }
        added
    }

    /// Whether the reader has scrolled to the oldest message on screen, which
    /// is when the next page of history is worth fetching.
    pub fn at_oldest(&self) -> bool {
        !self.follow_latest
            && self
                .message_line_map
                .iter()
                .flatten()
                .min()
                .is_none_or(|first| *first <= 1)
    }

    /// The message under the message-pane cursor.
    pub fn selected_message(&self) -> Option<&Message> {
        self.messages().get(self.message_cursor).copied()
    }

    /// The message the draft is replying to, if it is still in the snapshot.
    pub fn reply_target(&self) -> Option<&Message> {
        self.reply_to.and_then(|id| self.snapshot.message(id))
    }

    /// Starts a reply to the selected message. Refuses system notices: there
    /// is no author to reply to and the SDK has nothing to quote.
    pub fn begin_reply(&mut self) -> bool {
        match self.selected_message() {
            Some(message) if !message.is_system() => {
                self.reply_to = Some(message.id);
                self.mode = Mode::Insert;
                true
            }
            _ => false,
        }
    }

    /// Whether anything besides the typed text is attached to this draft.
    pub fn has_draft_context(&self) -> bool {
        self.reply_to.is_some() || !self.pending.is_empty() || !self.draft_mentions.is_empty()
    }

    /// Drops the reply, the mentions and the staged pictures, leaving the
    /// typed text alone.
    pub fn clear_draft_context(&mut self) {
        self.reply_to = None;
        self.draft_mentions.clear();
        self.pending.clear();
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
    ///
    /// Grouped by category, then by activity inside each one. The snapshot is
    /// sorted by activity alone, so walking it straight through emits a
    /// heading every time the category changes -- and a group, a direct chat
    /// and another group produce "群聊 / 私聊 / 群聊", three headings for two
    /// categories.
    pub fn nav_rows(&self) -> Vec<NavRow> {
        let mut order: Vec<String> = Vec::new();
        for index in 0..self.snapshot.conversations.len() {
            let category = self.category_of(index);
            if !order.contains(&category) {
                order.push(category);
            }
        }

        let mut rows = Vec::new();
        for category in order {
            let members: Vec<usize> = (0..self.snapshot.conversations.len())
                .filter(|index| self.category_of(*index) == category)
                .collect();
            let collapsed = self.is_collapsed(&category);
            rows.push(NavRow::Category {
                collapsed,
                count: members.len(),
                name: category,
            });
            if !collapsed {
                rows.extend(members.into_iter().map(|index| NavRow::Conversation { index }));
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

    /// Selects the entry drawn at row `index` of `pane`'s interior.
    ///
    /// Returns whether a real entry was hit: clicking past the end of a short
    /// list should leave the selection alone rather than clamping onto the
    /// last row, which would silently open the wrong conversation.
    pub fn select_row(&mut self, pane: Pane, index: usize) -> bool {
        match pane {
            Pane::Conversations => {
                if index >= self.nav_rows().len() {
                    return false;
                }
                self.nav_cursor = index;
                true
            }
            Pane::Members => {
                if index >= self.members().len() + self.members_by_role().len() {
                    return false;
                }
                // Role headings occupy rows too; walk the rendered order to map
                // a screen row back to a member.
                let mut row = 0usize;
                let mut member_index = 0usize;
                for (_, bucket) in self.members_by_role() {
                    if row == index {
                        return false; // a heading, not a member
                    }
                    row += 1;
                    for _ in bucket {
                        if row == index {
                            self.member_cursor = member_index;
                            return true;
                        }
                        row += 1;
                        member_index += 1;
                    }
                }
                false
            }
            Pane::Messages => {
                match self.message_line_map.get(index).copied().flatten() {
                    Some(message) => {
                        self.message_cursor = message;
                        self.follow_latest = message + 1 >= self.messages().len();
                        true
                    }
                    None => false,
                }
            }
        }
    }

    /// Scrolls one pane without changing focus.
    pub fn scroll_pane(&mut self, pane: Pane, delta: isize) {
        match pane {
            Pane::Messages => {
                self.leave_follow();
                let count = self.messages().len();
                self.message_scroll = step(self.message_scroll, delta, count);
            }
            Pane::Conversations => {
                let rows = self.nav_rows().len();
                self.nav_cursor = step(self.nav_cursor, delta, rows);
            }
            Pane::Members => {
                let count = self.members().len();
                self.member_cursor = step(self.member_cursor, delta, count);
            }
        }
    }

    /// Test helper: a single number that moves whenever the message pane does.
    #[cfg(test)]
    pub fn message_scroll_or_cursor(&self) -> usize {
        self.message_scroll * 1000 + self.message_cursor
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
        self.scroll_pane(Pane::Messages, delta);
    }

    /// Switches from bottom-anchored following to manual scrolling.
    ///
    /// While following, `message_scroll` is meaningless -- the viewport is
    /// pinned to the newest message and the top is derived from the height.
    /// Handing that stale zero to the scroll arithmetic is what made one wheel
    /// click jump to the very start of the conversation. Anchor it to what is
    /// actually on screen first.
    fn leave_follow(&mut self) {
        if !self.follow_latest {
            return;
        }
        self.follow_latest = false;
        self.message_scroll = self
            .message_line_map
            .iter()
            .flatten()
            .next()
            .copied()
            .unwrap_or(self.message_cursor);
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

    /// Switches to a conversation and lands at its newest message.
    pub fn open_conversation(&mut self, id: ConversationId) {
        if self.open != id {
            // The reply target and the mentioned people belong to the
            // conversation being left; carrying them over would quote a
            // message nobody here can see.
            self.clear_draft_context();
        }
        self.open = id;
        self.message_scroll = 0;
        self.jump_to_latest();
        self.pane = Pane::Messages;
        if let Some(index) = self.nav_rows().iter().position(
            |row| matches!(row, NavRow::Conversation { index } if self.snapshot.conversations.get(*index).is_some_and(|c| c.id == self.open)),
        ) {
            self.nav_cursor = index;
        }
    }

    pub fn open_selected(&mut self) {
        if self.pane != Pane::Conversations {
            return;
        }
        let rows = self.nav_rows();
        match rows.get(self.nav_cursor) {
            Some(NavRow::Conversation { index }) => {
                if let Some(id) = self.snapshot.conversations.get(*index).map(|c| c.id.clone()) {
                    self.open_conversation(id);
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

/// The recorded mentions that survive to the text actually being sent.
///
/// Somebody can pick a name from the `@` list and then delete it again, or
/// pick the same person twice. What goes on the wire has to match what is
/// written, or people get notified for a message that never names them.
pub fn live_mentions(text: &str, draft: &[DraftMention]) -> Vec<DraftMention> {
    let mut out: Vec<DraftMention> = Vec::new();
    for mention in draft {
        if out.iter().any(|kept| kept.user_id == mention.user_id) {
            continue;
        }
        if text.contains(&format!("@{}", mention.nickname)) {
            out.push(mention.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(user_id: &str, nickname: &str) -> DraftMention {
        DraftMention { user_id: user_id.into(), nickname: nickname.into() }
    }

    #[test]
    fn each_category_gets_exactly_one_heading() {
        // Conversations arrive sorted by activity, so a group, a direct chat
        // and another group would otherwise print three headings.
        let mut app = App::new(im_model::mock::snapshot());
        app.snapshot.conversations.sort_by(|a, b| a.name.cmp(&b.name));
        let headings: Vec<String> = app
            .nav_rows()
            .into_iter()
            .filter_map(|row| match row {
                NavRow::Category { name, .. } => Some(name),
                NavRow::Conversation { .. } => None,
            })
            .collect();
        let mut unique = headings.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(headings.len(), unique.len(), "repeated headings: {headings:?}");
    }

    #[test]
    fn reaching_the_top_is_what_asks_for_older_messages() {
        let mut app = App::new(im_model::mock::snapshot());
        assert!(!app.at_oldest(), "following the newest is not the top");
        app.follow_latest = false;
        app.message_line_map = vec![Some(4), Some(4), Some(5)];
        assert!(!app.at_oldest(), "still some way down");
        app.message_line_map = vec![Some(1), Some(2)];
        assert!(app.at_oldest(), "the oldest message is on screen");
    }

    #[test]
    fn a_fresh_account_has_nowhere_to_send() {
        let app = App::new(Snapshot::default());
        assert!(app.open.0.is_empty());
        assert!(!app.has_open_conversation());
        assert!(App::new(im_model::mock::snapshot()).has_open_conversation());
    }

    #[test]
    fn a_mention_deleted_from_the_draft_does_not_notify() {
        let draft = [draft("lina", "李娜"), draft("chenming", "陈明")];
        let kept = live_mentions("@陈明 你看下", &draft);
        assert_eq!(kept, vec![draft[1].clone()], "李娜 was removed from the text");
        assert!(live_mentions("什么都没写", &draft).is_empty());
    }

    #[test]
    fn picking_the_same_person_twice_notifies_them_once() {
        let draft = [draft("lina", "李娜"), draft("lina", "李娜")];
        assert_eq!(live_mentions("@李娜 @李娜 在吗", &draft).len(), 1);
    }

    #[test]
    fn a_reply_is_dropped_when_the_conversation_changes() {
        let mut app = App::new(im_model::mock::snapshot());
        app.focus(Pane::Messages);
        assert!(app.begin_reply(), "a normal message can be replied to");
        assert_eq!(app.mode, Mode::Insert);
        app.draft_mentions.push(draft("lina", "李娜"));

        let elsewhere = app.snapshot.conversations[1].id.clone();
        app.open_conversation(elsewhere);
        assert!(app.reply_to.is_none(), "the quoted message is not in this conversation");
        assert!(app.draft_mentions.is_empty());
    }

    #[test]
    fn a_system_notice_cannot_be_replied_to() {
        let mut app = App::new(im_model::mock::snapshot());
        app.jump_to_top();
        app.message_cursor = app
            .messages()
            .iter()
            .position(|m| m.is_system())
            .expect("the fixture has a system message");
        assert!(!app.begin_reply(), "there is nothing to quote");
        assert!(app.reply_to.is_none());
    }

    #[test]
    fn the_composer_grows_by_one_row_while_replying() {
        let mut app = App::new(im_model::mock::snapshot());
        let plain = app.composer_rows();
        app.focus(Pane::Messages);
        app.begin_reply();
        assert_eq!(app.composer_rows(), plain + 1);
    }

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
    fn scrolling_up_from_the_live_edge_does_not_jump_to_the_beginning() {
        let mut app = app();
        assert!(app.follow_latest, "fixture opens at the live edge");
        // Pretend a frame was drawn showing the last four messages.
        let last = app.messages().len() - 1;
        app.message_line_map = (last - 3..=last).map(Some).collect();

        app.scroll_pane(Pane::Messages, -3);
        assert!(!app.follow_latest);
        assert!(
            app.message_scroll >= last - 6,
            "scroll landed at {}, expected near the live edge ({last})",
            app.message_scroll
        );
    }

    #[test]
    fn leaving_follow_without_a_rendered_frame_anchors_to_the_cursor() {
        let mut app = app();
        let cursor = app.message_cursor;
        app.scroll_pane(Pane::Messages, -1);
        assert!(app.message_scroll <= cursor && app.message_scroll + 2 >= cursor);
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
