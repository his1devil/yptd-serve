//! Domain model for the yptd terminal client.
//!
//! Shaped by OpenIM, not by Discord: a conversation is a group or a direct
//! chat, membership carries a numeric role level, and read state is a count of
//! who has seen a message rather than a per-user acknowledgement.

pub mod mock;
pub mod translate;

/// Epoch for synthetic message ids: 2020-01-01T00:00:00Z.
pub const EPOCH_MS: i64 = 1_577_836_800_000;

/// The reserved user id OpenIM uses for "@everyone" (`constant.AtAllString`).
/// It travels in `atUserList` like any other id, so it needs no special case
/// anywhere except when deciding how to draw it.
pub const AT_ALL_TAG: &str = "AtAllTag";

const TIMESTAMP_SHIFT: u32 = 22;
const COUNTER_MASK: u64 = (1 << TIMESTAMP_SHIFT) - 1;

/// A message id that sorts by send time and carries it in the high bits.
///
/// OpenIM identifies messages by `clientMsgID` (a UUID) plus a per-conversation
/// `seq`, neither of which orders across conversations. Packing the send time
/// into the high 42 bits buys three things at once: a total order that matches
/// wall-clock, a timestamp we never have to store separately, and a `Copy` key
/// cheap enough for a `BTreeMap`. The low 22 bits are a per-millisecond
/// counter, so two messages in the same millisecond still get distinct ids.
///
/// The mapping back to `clientMsgID` lives in an interning table beside the
/// store; this type deliberately knows nothing about it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MessageId(u64);

impl MessageId {
    pub fn new(raw: u64) -> Self {
        Self(raw.max(1))
    }

    /// Packs a send time and an intra-millisecond counter into an id.
    pub fn from_send_time(sent_at_ms: i64, counter: u64) -> Self {
        let since_epoch = sent_at_ms.saturating_sub(EPOCH_MS).max(0) as u64;
        Self::new((since_epoch << TIMESTAMP_SHIFT) | (counter & COUNTER_MASK))
    }

    pub fn raw(self) -> u64 {
        self.0
    }

    /// Recovers the send time. Free, because it was never stored twice.
    pub fn sent_at_ms(self) -> i64 {
        (self.0 >> TIMESTAMP_SHIFT) as i64 + EPOCH_MS
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UserId(pub String);

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ConversationId(pub String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConversationKind {
    /// OpenIM `sessionType = 3`.
    Group,
    /// OpenIM `sessionType = 1`.
    Direct,
}

#[derive(Clone, Debug)]
pub struct Conversation {
    pub id: ConversationId,
    pub name: String,
    pub kind: ConversationKind,
    /// yptd groups conversations client-side; OpenIM has no such entity.
    pub category: Option<String>,
    pub unread: u32,
    pub mentions: u32,
    pub muted: bool,
    pub member_count: u32,
    pub last_activity_ms: i64,
    /// Highest seq the local user has read, mapped from OpenIM `hasReadSeq`.
    /// Messages above it sit under the unread divider.
    pub read_up_to: Option<MessageId>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Role {
    Member,
    Admin,
    Owner,
}

impl Role {
    /// OpenIM encodes membership as `roleLevel`: 100 owner, 60 admin, 20 member.
    pub fn from_level(level: u32) -> Self {
        match level {
            100.. => Self::Owner,
            60..100 => Self::Admin,
            _ => Self::Member,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Owner => "群主",
            Self::Admin => "管理员",
            Self::Member => "成员",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Member {
    pub id: UserId,
    pub name: String,
    /// The picture this person has set, when they have. Preferred over the
    /// copy stamped on each message: somebody who changes their picture
    /// should change it on everything they ever said, not only on what they
    /// say next.
    pub avatar: Option<String>,
    pub role: Role,
    pub online: bool,
    pub is_bot: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SendState {
    Sending,
    Sent,
    Failed,
}

/// Whether a message actually exists on the server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Visibility {
    /// A real OpenIM message everyone in the conversation sees.
    Broadcast,
    /// Rendered on this device only -- a private assistant reply that was
    /// never sent. Must be visually distinguishable, or people will quote it
    /// at colleagues who cannot see it.
    Ghost,
}

#[derive(Clone, Debug)]
pub struct Reaction {
    pub emoji: String,
    pub count: u32,
    pub by_me: bool,
}

#[derive(Clone, Debug)]
pub struct Quote {
    pub message_id: MessageId,
    pub sender_name: String,
    pub summary: String,
}

#[derive(Clone, Debug)]
pub struct Attachment {
    pub name: String,
    pub bytes: u64,
    pub kind: AttachmentKind,
    /// Where the file lives on the object store. Empty for an attachment that
    /// has not been uploaded yet, or for the mock.
    pub url: String,
}

impl Attachment {
    /// The cache key for this file.
    ///
    /// The URL, not the name: two people sending `screenshot.png` are sending
    /// two different pictures, and keying by name would show the first one
    /// twice. Falls back to the name only when there is no URL yet.
    pub fn key(&self) -> &str {
        if self.url.is_empty() { &self.name } else { &self.url }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentKind {
    Image,
    File,
    Video,
    Audio,
}

impl AttachmentKind {
    /// A two-cell glyph for the attachment line.
    ///
    /// The picture frame carries a variation selector on purpose: without it
    /// the code point defaults to text presentation, which `unicode-width`
    /// measures as one cell while terminals draw the emoji in two. Every
    /// picture line would then sit one column off. The other three default to
    /// emoji presentation and need nothing.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Image => "🖼\u{FE0F}",
            Self::File => "📄",
            Self::Video => "🎬",
            Self::Audio => "🔊",
        }
    }
}

/// What a message says. Maps onto OpenIM `contentType`.
#[derive(Clone, Debug)]
pub enum Body {
    /// `contentType` 101 / 106 / 117 -- plain, at-text, advanced text.
    Text(String),
    /// `contentType` 118. Rendered through the markdown subset.
    Markdown(String),
    /// 102 / 103 / 104 / 105, optionally with a caption.
    Attachment {
        caption: Option<String>,
        attachment: Attachment,
    },
    /// 1501..=1520 group notifications, rendered without an author header.
    System(String),
    /// 143 / 2500 stream messages, and local LLM streaming.
    Stream { text: String, done: bool },
}

#[derive(Clone, Debug)]
pub struct Message {
    pub id: MessageId,
    pub conversation: ConversationId,
    pub sender: UserId,
    pub sender_name: String,
    /// Where the sender's picture lives, when they have set one. Carried per
    /// message rather than looked up: a direct conversation has no member
    /// list to look anything up in, and OpenIM stamps it onto every message
    /// anyway.
    pub sender_avatar: Option<String>,
    pub body: Body,
    pub quote: Option<Quote>,
    pub reactions: Vec<Reaction>,
    pub send_state: SendState,
    pub visibility: Visibility,
    /// How many members have read it. OpenIM reports this; Discord has no
    /// equivalent, so this is a component concord never needed.
    pub read_by: Option<u32>,
    pub edited: bool,
    /// Byte ranges in the body text that mention someone.
    pub mentions: Vec<Mention>,
    /// A standing-in message, such as the agent's "working on it", which the
    /// sender will follow with the real thing. Kept in the model rather than
    /// withdrawn on the server: taking a message back needs its sequence
    /// number, and the number is not known until after the send has
    /// propagated, so a revoke aimed right after sending hits whatever came
    /// before it.
    pub transient: bool,
}

#[derive(Clone, Debug)]
pub struct Mention {
    pub start: usize,
    pub end: usize,
    pub user: UserId,
    /// Whether this mention notifies the local user.
    pub notifies_me: bool,
}

impl Mention {
    pub fn mentions_everyone(&self) -> bool {
        self.user.0 == AT_ALL_TAG
    }
}

impl Message {
    pub fn sent_at_ms(&self) -> i64 {
        self.id.sent_at_ms()
    }

    pub fn is_system(&self) -> bool {
        matches!(self.body, Body::System(_))
    }

    /// The text a wrapper measures. Attachments contribute their caption only;
    /// the attachment line itself is laid out separately.
    pub fn text(&self) -> &str {
        match &self.body {
            Body::Text(text) | Body::Markdown(text) | Body::System(text) => text,
            Body::Stream { text, .. } => text,
            Body::Attachment { caption, .. } => caption.as_deref().unwrap_or(""),
        }
    }

    pub fn attachment(&self) -> Option<&Attachment> {
        match &self.body {
            Body::Attachment { attachment, .. } => Some(attachment),
            _ => None,
        }
    }
}

/// Everything the UI reads. In production this is a snapshot handed over from
/// the store; for now it is built by [`mock`].
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub me: Option<UserId>,
    /// The local user's display name. Kept here rather than looked up in
    /// `members`, which only ever holds the open group's roster -- in a
    /// direct chat, or before joining anything, one's own name is not in it.
    pub my_name: String,
    pub conversations: Vec<Conversation>,
    pub messages: Vec<Message>,
    pub members: Vec<Member>,
    pub connected: bool,
    /// `None` once the initial sync has finished.
    pub sync_percent: Option<u8>,
    /// Bumped whenever the message list changes.
    ///
    /// Laying messages out means parsing markdown, highlighting code and
    /// wrapping every line; doing that on each frame is what makes scrolling
    /// feel heavy. A counter lets the renderer keep its work and know exactly
    /// when it is stale.
    revision: u64,
}

impl Snapshot {
    pub fn conversation(&self, id: &ConversationId) -> Option<&Conversation> {
        self.conversations.iter().find(|c| &c.id == id)
    }

    pub fn message(&self, id: MessageId) -> Option<&Message> {
        self.messages.iter().find(|m| m.id == id)
    }

    pub fn messages_in(&self, id: &ConversationId) -> Vec<&Message> {
        let mut found: Vec<&Message> = self
            .messages
            .iter()
            .filter(|message| &message.conversation == id)
            .collect();
        found.sort_by_key(|message| message.id);
        found
    }

    pub fn total_unread(&self) -> u32 {
        self.conversations.iter().map(|c| c.unread).sum()
    }

    pub fn total_mentions(&self) -> u32 {
        self.conversations.iter().map(|c| c.mentions).sum()
    }

    /// Inserts or replaces a conversation, keeping the list ordered by most
    /// recent activity so the sidebar reads newest-first without a sort on
    /// every frame.
    pub fn upsert_conversation(&mut self, conversation: Conversation) {
        match self.conversations.iter().position(|c| c.id == conversation.id) {
            Some(i) => self.conversations[i] = conversation,
            None => self.conversations.push(conversation),
        }
        self.conversations
            .sort_by(|a, b| b.last_activity_ms.cmp(&a.last_activity_ms));
    }

    /// Inserts or replaces a message by id. A message that arrives twice --
    /// history and live push -- lands on the same id and overwrites in place.
    pub fn upsert_message(&mut self, message: Message) {
        match self.messages.iter().position(|m| m.id == message.id) {
            Some(i) => self.messages[i] = message,
            None => self.messages.push(message),
        }
        self.revision = self.revision.wrapping_add(1);
    }

    /// A number that changes whenever the messages do.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Removes a message. Returns whether one was there.
    pub fn remove_message(&mut self, id: MessageId) -> bool {
        match self.messages.iter().position(|m| m.id == id) {
            Some(i) => {
                self.messages.remove(i);
                self.revision = self.revision.wrapping_add(1);
                true
            }
            None => false,
        }
    }

    pub fn set_members(&mut self, members: Vec<Member>) {
        self.members = members;
    }

    pub fn conversation_mut(&mut self, id: &ConversationId) -> Option<&mut Conversation> {
        self.conversations.iter_mut().find(|c| &c.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_ids_carry_their_send_time() {
        let sent = EPOCH_MS + 1_234_567_890;
        let id = MessageId::from_send_time(sent, 7);
        assert_eq!(id.sent_at_ms(), sent);
    }

    #[test]
    fn ids_order_by_time_then_counter() {
        let base = EPOCH_MS + 1_000;
        let first = MessageId::from_send_time(base, 0);
        let second = MessageId::from_send_time(base, 1);
        let later = MessageId::from_send_time(base + 1, 0);
        assert!(first < second, "counter must break ties within a millisecond");
        assert!(second < later, "a later millisecond must sort after");
    }

    #[test]
    fn ids_are_never_zero() {
        // Zero would be indistinguishable from "unset" in an Option-free key.
        assert_eq!(MessageId::from_send_time(EPOCH_MS, 0).raw(), 1);
        assert_eq!(MessageId::from_send_time(0, 0).raw(), 1);
    }

    #[test]
    fn a_counter_wider_than_its_field_cannot_bleed_into_the_timestamp() {
        let sent = EPOCH_MS + 5_000;
        let id = MessageId::from_send_time(sent, u64::MAX);
        assert_eq!(id.sent_at_ms(), sent);
    }

    #[test]
    fn two_pictures_with_the_same_file_name_are_cached_apart() {
        let named = |url: &str| Attachment {
            name: "screenshot.png".to_owned(),
            bytes: 0,
            kind: AttachmentKind::Image,
            url: url.to_owned(),
        };
        let mine = named("https://im.example/object/alice/a.png");
        let theirs = named("https://im.example/object/bob/b.png");
        assert_ne!(mine.key(), theirs.key(), "keying by name would show one twice");
    }

    #[test]
    fn an_attachment_with_nowhere_to_fetch_it_from_falls_back_to_its_name() {
        let local = Attachment {
            name: "draft.png".to_owned(),
            bytes: 12,
            kind: AttachmentKind::Image,
            url: String::new(),
        };
        assert_eq!(local.key(), "draft.png");
    }

    #[test]
    fn role_levels_map_to_the_three_openim_tiers() {
        assert_eq!(Role::from_level(100), Role::Owner);
        assert_eq!(Role::from_level(60), Role::Admin);
        assert_eq!(Role::from_level(20), Role::Member);
        assert_eq!(Role::from_level(0), Role::Member);
        assert!(Role::Owner > Role::Admin && Role::Admin > Role::Member);
    }
}
