//! OpenIM SDK JSON → domain model.
//!
//! The SDK hands over its own shapes: `textElem`, `pictureElem`, numeric
//! `contentType`s, `roleLevel`s. Nothing outside this module should know those
//! names. Every field read here is optional, because the SDK omits what it
//! does not have and a missing `senderNickname` is not a reason to drop a
//! message.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    Attachment, AttachmentKind, Body, Conversation, ConversationId, ConversationKind, Member,
    Mention, Message, MessageId, Quote, Role, SendState, UserId, Visibility,
};

/// OpenIM `contentType` values this client understands.
mod content {
    pub const TEXT: i64 = 101;
    pub const PICTURE: i64 = 102;
    pub const VOICE: i64 = 103;
    pub const VIDEO: i64 = 104;
    pub const FILE: i64 = 105;
    pub const AT_TEXT: i64 = 106;
    pub const QUOTE: i64 = 114;
    pub const ADVANCED_TEXT: i64 = 117;
    pub const MARKDOWN: i64 = 118;
    pub const NOTIFICATION_FROM: i64 = 1000;
    pub const NOTIFICATION_TO: i64 = 5000;
    /// A message was withdrawn. The SDK also raises an event carrying which
    /// one, and that is what removes it -- drawing the notice as well would
    /// leave a "系统通知" line behind every revoke.
    pub const REVOKE_NOTIFICATION: i64 = 2101;
    /// Messages were deleted; same reasoning.
    pub const DELETE_NOTIFICATION: i64 = 2102;
}

/// Assigns stable, time-ordered [`MessageId`]s to SDK messages.
///
/// The SDK identifies a message by `clientMsgID`. The same message can arrive
/// twice -- once from history, once as a live push -- and both must map to the
/// same id or the list shows it twice. Once assigned, an id never changes,
/// even if a later copy carries a slightly different `sendTime`; the id is a
/// map key and re-keying is how ordering bugs are born.
#[derive(Default)]
pub struct Interner {
    by_client_id: HashMap<String, MessageId>,
    /// The way back, for the operations the SDK keys by `clientMsgID`:
    /// quoting a message means handing the SDK the original, and all the UI
    /// has at that point is the id it is drawing.
    by_id: HashMap<MessageId, String>,
    /// Per-millisecond counter so two messages in the same instant differ.
    last_ms: i64,
    counter: u64,
}

impl Interner {
    pub fn intern(&mut self, client_msg_id: &str, sent_at_ms: i64) -> MessageId {
        if let Some(&id) = self.by_client_id.get(client_msg_id) {
            return id;
        }
        // A send time before the id epoch (a missing `sendTime`, a clock set
        // to 1970) would lose the counter bits and let ids collide, so such
        // messages sort at the very start instead.
        let sent_at_ms = sent_at_ms.max(crate::EPOCH_MS + 1);
        if sent_at_ms == self.last_ms {
            self.counter += 1;
        } else {
            self.last_ms = sent_at_ms;
            self.counter = 0;
        }
        let id = MessageId::from_send_time(sent_at_ms, self.counter);
        self.by_client_id.insert(client_msg_id.to_owned(), id);
        self.by_id.insert(id, client_msg_id.to_owned());
        id
    }

    pub fn get(&self, client_msg_id: &str) -> Option<MessageId> {
        self.by_client_id.get(client_msg_id).copied()
    }

    pub fn client_msg_id(&self, id: MessageId) -> Option<&str> {
        self.by_id.get(&id).map(String::as_str)
    }
}

/// The sidebar heading a conversation sits under. OpenIM has no folders;
/// grouping by kind is the least surprising default.
pub fn category_for(kind: ConversationKind) -> &'static str {
    match kind {
        ConversationKind::Group => "群聊",
        ConversationKind::Direct => "私聊",
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn i64_of(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// The conversation an SDK message belongs to, derived the way the SDK does:
/// `sg_<groupID>` for groups, `si_<a>_<b>` (sorted) for direct chats.
pub fn conversation_id_of(msg: &Value, me: &UserId) -> ConversationId {
    let group = str_of(msg, "groupID");
    if !group.is_empty() {
        return ConversationId(format!("sg_{group}"));
    }
    let send = str_of(msg, "sendID");
    let recv = str_of(msg, "recvID");
    let other = if send == me.0 { recv } else { send };
    direct_conversation_id(&me.0, other)
}

/// `si_<a>_<b>` with the pair sorted, exactly as the SDK derives it, so a
/// conversation opened locally before any message exists lands on the same
/// id the server will later push.
pub fn direct_conversation_id(a: &str, b: &str) -> ConversationId {
    let mut pair = [a, b];
    pair.sort_unstable();
    ConversationId(format!("si_{}_{}", pair[0], pair[1]))
}

/// Converts one SDK message. Returns `None` for kinds we do not render.
pub fn message(msg: &Value, me: &UserId, interner: &mut Interner) -> Option<Message> {
    let client_id = str_of(msg, "clientMsgID");
    if client_id.is_empty() {
        return None;
    }
    let sent_at = match i64_of(msg, "sendTime") {
        0 => i64_of(msg, "createTime"),
        t => t,
    };
    let id = interner.intern(client_id, sent_at);
    let content_type = i64_of(msg, "contentType");
    let sender = UserId(str_of(msg, "sendID").to_owned());
    let mut mentions = Vec::new();

    let body = match content_type {
        content::TEXT => Body::Text(str_of(msg.get("textElem")?, "content").to_owned()),
        content::MARKDOWN => {
            let text = msg
                .get("markdownTextElem")
                .map(|e| str_of(e, "content"))
                .or_else(|| msg.get("textElem").map(|e| str_of(e, "content")))
                .unwrap_or("")
                .to_owned();
            Body::Markdown(text)
        }
        content::AT_TEXT | content::ADVANCED_TEXT => {
            let elem = msg
                .get("atTextElem")
                .or_else(|| msg.get("advancedTextElem"))?;
            let text = str_of(elem, "text").to_owned();
            let is_at_self = elem.get("isAtSelf").and_then(Value::as_bool).unwrap_or(false);
            // The SDK marks who was mentioned but not where in the text; the
            // nickname is what appears inline, so that is what we locate.
            if let Some(users) = elem.get("atUsersInfo").and_then(Value::as_array) {
                for u in users {
                    let uid = str_of(u, "atUserID");
                    let nick = str_of(u, "groupNickname");
                    if nick.is_empty() {
                        continue;
                    }
                    let needle = format!("@{nick}");
                    if let Some(start) = text.find(&needle) {
                        mentions.push(Mention {
                            start,
                            end: start + needle.len(),
                            user: UserId(uid.to_owned()),
                            // A mention of everyone reaches me too, whether or
                            // not the sender's client set `isAtSelf`.
                            notifies_me: uid == crate::AT_ALL_TAG || (is_at_self && uid == me.0),
                        });
                    }
                }
            }
            Body::Text(text)
        }
        content::PICTURE => {
            let elem = msg.get("pictureElem")?;
            let source = elem.get("sourcePicture").cloned().unwrap_or(Value::Null);
            Body::Attachment {
                caption: None,
                attachment: Attachment {
                    name: file_name(str_of(elem, "sourcePath"), str_of(&source, "uuid"), "png"),
                    // Whichever size the server actually filled in: on the
                    // echo of a message we just sent, `sourcePicture.size`
                    // comes back as 0.
                    bytes: [
                        elem.get("sourcePicture"),
                        elem.get("bigPicture"),
                        elem.get("snapshotPicture"),
                    ]
                    .iter()
                    .flatten()
                    .map(|v| i64_of(v, "size"))
                    .find(|size| *size > 0)
                    .unwrap_or(0) as u64,
                    kind: AttachmentKind::Image,
                    // `sourcePicture` is the original. The snapshot is a
                    // thumbnail the server may not have made, so it is the
                    // fallback rather than the first choice.
                    url: first_url(&[
                        elem.get("sourcePicture"),
                        elem.get("bigPicture"),
                        elem.get("snapshotPicture"),
                    ]),
                },
            }
        }
        content::FILE => {
            let elem = msg.get("fileElem")?;
            Body::Attachment {
                caption: None,
                attachment: Attachment {
                    name: str_of(elem, "fileName").to_owned(),
                    bytes: i64_of(elem, "fileSize").max(0) as u64,
                    kind: AttachmentKind::File,
                    url: str_of(elem, "sourceUrl").to_owned(),
                },
            }
        }
        content::VIDEO => {
            let elem = msg.get("videoElem")?;
            Body::Attachment {
                caption: None,
                attachment: Attachment {
                    name: file_name(str_of(elem, "videoPath"), str_of(elem, "videoUUID"), "mp4"),
                    bytes: i64_of(elem, "videoSize").max(0) as u64,
                    kind: AttachmentKind::Video,
                    url: str_of(elem, "videoUrl").to_owned(),
                },
            }
        }
        content::VOICE => {
            let elem = msg.get("soundElem")?;
            Body::Attachment {
                caption: None,
                attachment: Attachment {
                    name: file_name(str_of(elem, "soundPath"), str_of(elem, "uuid"), "m4a"),
                    bytes: i64_of(elem, "dataSize").max(0) as u64,
                    kind: AttachmentKind::Audio,
                    url: str_of(elem, "sourceUrl").to_owned(),
                },
            }
        }
        content::QUOTE => Body::Text(str_of(msg.get("quoteElem")?, "text").to_owned()),
        content::REVOKE_NOTIFICATION | content::DELETE_NOTIFICATION => return None,
        t if (content::NOTIFICATION_FROM..content::NOTIFICATION_TO).contains(&t) => {
            Body::System(notification_text(msg, t))
        }
        _ => return None,
    };

    // A quote rides in `quoteElem` on its own, and inside `atTextElem` when
    // the reply also mentions somebody. Both are replies and both draw the
    // same strip above the message.
    let quote = match content_type {
        content::QUOTE => msg.get("quoteElem").and_then(|q| q.get("quoteMessage")),
        content::AT_TEXT | content::ADVANCED_TEXT => msg
            .get("atTextElem")
            .or_else(|| msg.get("advancedTextElem"))
            .and_then(|e| e.get("quoteMessage")),
        _ => None,
    }
        .and_then(|q| {
            let qid = str_of(q, "clientMsgID");
            (!qid.is_empty()).then(|| Quote {
                message_id: interner.intern(qid, i64_of(q, "sendTime")),
                sender_name: str_of(q, "senderNickname").to_owned(),
                summary: summary_of(q),
            })
        });

    let send_state = match i64_of(msg, "status") {
        1 => SendState::Sending,
        3 => SendState::Failed,
        _ => SendState::Sent,
    };

    Some(Message {
        id,
        conversation: conversation_id_of(msg, me),
        sender: sender.clone(),
        sender_name: {
            let n = str_of(msg, "senderNickname");
            if n.is_empty() { sender.0.clone() } else { n.to_owned() }
        },
        sender_avatar: {
            let url = str_of(msg, "senderFaceURL");
            (!url.is_empty()).then(|| url.to_owned())
        },
        body,
        quote,
        reactions: Vec::new(),
        send_state,
        visibility: Visibility::Broadcast,
        read_by: None,
        edited: false,
        mentions,
        transient: is_transient(str_of(msg, "ex")),
    })
}

/// Whether the sender marked this message as a placeholder it will replace.
///
/// The marker travels in OpenIM's free-form `ex` field, so no content type
/// has to be invented and any other client simply sees an ordinary message.
fn is_transient(ex: &str) -> bool {
    if ex.is_empty() {
        return false;
    }
    serde_json::from_str::<Value>(ex)
        .ok()
        .and_then(|v| v.get("yptd").and_then(Value::as_str).map(str::to_owned))
        .is_some_and(|marker| marker == "pending")
}

/// The first element that actually carries a URL. The SDK fills in whichever
/// sizes the server produced and leaves the rest null.
fn first_url(candidates: &[Option<&Value>]) -> String {
    candidates
        .iter()
        .flatten()
        .map(|v| str_of(v, "url"))
        .find(|url| !url.is_empty())
        .unwrap_or("")
        .to_owned()
}

fn file_name(path: &str, uuid: &str, ext: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or("");
    if !base.is_empty() {
        return base.to_owned();
    }
    if !uuid.is_empty() {
        return format!("{uuid}.{ext}");
    }
    format!("attachment.{ext}")
}

/// Short text for a quoted message, whatever its kind.
fn summary_of(msg: &Value) -> String {
    let t = i64_of(msg, "contentType");
    match t {
        content::TEXT => str_of(msg.get("textElem").unwrap_or(&Value::Null), "content").to_owned(),
        content::AT_TEXT => str_of(msg.get("atTextElem").unwrap_or(&Value::Null), "text").to_owned(),
        content::PICTURE => "[图片]".to_owned(),
        content::FILE => "[文件]".to_owned(),
        content::VIDEO => "[视频]".to_owned(),
        content::VOICE => "[语音]".to_owned(),
        _ => "[消息]".to_owned(),
    }
}

/// Human text for a group notification, from its `notificationElem.detail`
/// when present, otherwise a label for the type.
fn notification_text(msg: &Value, content_type: i64) -> String {
    let detail = msg
        .get("notificationElem")
        .map(|e| str_of(e, "detail"))
        .unwrap_or("");
    let parsed: Option<Value> = serde_json::from_str(detail).ok();
    let who = |v: &Value, key: &str| -> String {
        v.get(key)
            .map(|u| {
                let n = str_of(u, "nickname");
                if n.is_empty() { str_of(u, "userID").to_owned() } else { n.to_owned() }
            })
            .unwrap_or_default()
    };
    match (content_type, parsed) {
        (1501, Some(v)) => format!("{} 创建了群聊", who(&v, "opUser")),
        (1509, Some(v)) => {
            let invited: Vec<String> = v
                .get("invitedUserList")
                .and_then(Value::as_array)
                .map(|l| l.iter().map(|u| who(u, "")).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default();
            if invited.is_empty() {
                format!("{} 邀请了新成员", who(&v, "opUser"))
            } else {
                format!("{} 邀请 {} 加入群聊", who(&v, "opUser"), invited.join("、"))
            }
        }
        (1510, Some(v)) => format!("{} 加入了群聊", who(&v, "entrantUser")),
        (1504, Some(v)) => format!("{} 退出了群聊", who(&v, "quitUser")),
        (1508, Some(v)) => format!("{} 被移出群聊", who(&v, "kickedUser")),
        (1520, Some(v)) => format!("群名改为「{}」", v.get("group").map(|g| str_of(g, "groupName")).unwrap_or("")),
        (1519, _) => "群公告已更新".to_owned(),
        (1507, Some(v)) => format!("群主转让给 {}", who(&v, "newGroupOwner")),
        (1511, _) => "群已解散".to_owned(),
        (t, _) => format!("系统通知 {t}"),
    }
}

/// Converts an SDK conversation record.
pub fn conversation(v: &Value, interner: &Interner) -> Option<Conversation> {
    let id = str_of(v, "conversationID");
    if id.is_empty() {
        return None;
    }
    let kind = match i64_of(v, "conversationType") {
        1 => ConversationKind::Direct,
        _ => ConversationKind::Group,
    };
    let name = {
        let n = str_of(v, "showName");
        if n.is_empty() { str_of(v, "groupID").to_owned() } else { n.to_owned() }
    };
    // The SDK's read cursor is a seq; map it back to the newest message we
    // have seen at or below it once messages are loaded.
    let _ = interner;
    Some(Conversation {
        id: ConversationId(id.to_owned()),
        name,
        kind,
        category: Some(category_for(kind).to_owned()),
        unread: i64_of(v, "unreadCount").max(0) as u32,
        // The SDK reports whether the unread run mentions me at all, not how
        // many times. One badge either way is what the sidebar shows.
        mentions: match i64_of(v, "groupAtType") {
            1 | 3 => 1, // at me, at everyone and me
            2 => 1,     // at everyone
            _ => 0,
        },
        muted: i64_of(v, "recvMsgOpt") == 2,
        member_count: 0,
        last_activity_ms: i64_of(v, "latestMsgSendTime"),
        read_up_to: None,
    })
}

/// Converts an SDK group member record.
pub fn member(v: &Value) -> Option<Member> {
    let id = str_of(v, "userID");
    if id.is_empty() {
        return None;
    }
    let nick = str_of(v, "nickname");
    let face = str_of(v, "faceURL");
    Some(Member {
        id: UserId(id.to_owned()),
        name: if nick.is_empty() { id.to_owned() } else { nick.to_owned() },
        avatar: (!face.is_empty()).then(|| face.to_owned()),
        role: Role::from_level(i64_of(v, "roleLevel").max(0) as u32),
        online: false,
        is_bot: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> UserId {
        UserId("alice".into())
    }

    #[test]
    fn interning_the_same_client_id_twice_yields_one_id() {
        let mut i = Interner::default();
        let a = i.intern("c1", 1_700_000_000_000);
        let again = i.intern("c1", 1_700_000_000_999); // history copy, different time
        assert_eq!(a, again, "a message seen twice must not get two ids");
    }

    #[test]
    fn two_messages_in_one_millisecond_stay_distinct_and_ordered() {
        let mut i = Interner::default();
        let a = i.intern("c1", crate::EPOCH_MS + 5_000);
        let b = i.intern("c2", crate::EPOCH_MS + 5_000);
        assert_ne!(a, b);
        assert!(a < b);
    }

    #[test]
    fn a_garbage_send_time_still_yields_distinct_ids() {
        let mut i = Interner::default();
        let a = i.intern("c1", 0);
        let b = i.intern("c2", 0);
        assert_ne!(a, b);
    }

    #[test]
    fn a_reply_that_also_mentions_someone_carries_both() {
        // The SDK nests the quoted message inside the at-text element rather
        // than sending contentType 114, so a reply-with-@ has to be read from
        // there or the quote strip silently disappears.
        let raw = serde_json::json!({
            "clientMsgID": "m2", "sendID": "bob", "senderNickname": "Bob",
            "groupID": "g", "contentType": 106, "sendTime": 1_788_861_400_000i64,
            "atTextElem": {
                "text": "@李娜 看下这个", "isAtSelf": false,
                "atUsersInfo": [{"atUserID": "lina", "groupNickname": "李娜"}],
                "quoteMessage": {
                    "clientMsgID": "orig", "sendTime": 1_788_861_300_000i64,
                    "senderNickname": "陈明", "contentType": 101,
                    "textElem": {"content": "灰度那一周 buffer 太少"}
                }
            }
        });
        let mut i = Interner::default();
        let m = message(&raw, &me(), &mut i).expect("translates");
        assert_eq!(m.mentions.len(), 1);
        let quote = m.quote.expect("the nested quote is read");
        assert_eq!(quote.sender_name, "陈明");
        assert_eq!(quote.summary, "灰度那一周 buffer 太少");
        assert!(quote.message_id < m.id, "the quoted message is older");
    }

    #[test]
    fn a_mention_of_everyone_notifies_me_without_is_at_self() {
        let raw = serde_json::json!({
            "clientMsgID": "m3", "sendID": "bob", "groupID": "g",
            "contentType": 106, "sendTime": 1_788_861_500_000i64,
            "atTextElem": {
                "text": "@全体成员 发版了", "isAtSelf": false,
                "atUsersInfo": [{"atUserID": crate::AT_ALL_TAG, "groupNickname": "全体成员"}]
            }
        });
        let mut i = Interner::default();
        let m = message(&raw, &me(), &mut i).expect("translates");
        let mention = &m.mentions[0];
        assert!(mention.mentions_everyone());
        assert!(mention.notifies_me);
    }

    #[test]
    fn a_conversation_that_mentions_me_gets_a_badge() {
        let i = Interner::default();
        let row = |at: i64| {
            serde_json::json!({
                "conversationID": "sg_1", "conversationType": 3, "showName": "群",
                "unreadCount": 4, "groupAtType": at
            })
        };
        assert_eq!(conversation(&row(0), &i).unwrap().mentions, 0);
        assert_eq!(conversation(&row(1), &i).unwrap().mentions, 1, "at me");
        assert_eq!(conversation(&row(2), &i).unwrap().mentions, 1, "at everyone");
        assert_eq!(conversation(&row(3), &i).unwrap().mentions, 1, "both");
        assert_eq!(conversation(&row(4), &i).unwrap().mentions, 0, "group notice");
    }

    #[test]
    fn the_pending_marker_is_read_from_ex() {
        assert!(is_transient(r#"{"yptd":"pending"}"#));
        assert!(!is_transient(r#"{"yptd":"other"}"#));
        assert!(!is_transient("not json"));
        assert!(!is_transient(""));
    }

    #[test]
    fn a_revoke_notice_is_not_a_message_of_its_own() {
        // The event that carries which message was withdrawn is what removes
        // it; rendering the notice too would leave a stray line behind.
        let raw = serde_json::json!({
            "clientMsgID": "rv", "sendID": "bob", "groupID": "g",
            "contentType": 2101, "sendTime": 1_788_861_600_000i64,
            "notificationElem": {"detail": "{}"}
        });
        let mut i = Interner::default();
        assert!(message(&raw, &me(), &mut i).is_none());
    }

    #[test]
    fn interning_maps_both_ways() {
        let mut i = Interner::default();
        let id = i.intern("abc", crate::EPOCH_MS + 1);
        assert_eq!(i.client_msg_id(id), Some("abc"));
        assert_eq!(i.get("abc"), Some(id));
        assert_eq!(i.client_msg_id(MessageId::new(999)), None);
    }

    #[test]
    fn a_text_message_translates_with_its_conversation() {
        let raw = serde_json::json!({
            "clientMsgID": "abc", "sendID": "bob", "senderNickname": "Bob",
            "groupID": "193332560", "contentType": 101, "sendTime": 1_788_861_348_604i64,
            "textElem": {"content": "你好"}
        });
        let mut i = Interner::default();
        let m = message(&raw, &me(), &mut i).expect("translates");
        assert_eq!(m.conversation.0, "sg_193332560");
        assert_eq!(m.sender_name, "Bob");
        assert!(matches!(&m.body, Body::Text(t) if t == "你好"));
        assert_eq!(m.sent_at_ms(), 1_788_861_348_604);
    }

    #[test]
    fn a_direct_message_derives_a_sorted_pair_id() {
        let raw = serde_json::json!({
            "clientMsgID": "d1", "sendID": "zed", "recvID": "alice",
            "contentType": 101, "sendTime": 1, "textElem": {"content": "hi"}
        });
        let m = message(&raw, &me(), &mut Interner::default()).unwrap();
        assert_eq!(m.conversation.0, "si_alice_zed", "must not depend on who sent it");
    }

    #[test]
    fn at_text_locates_mentions_by_nickname() {
        let raw = serde_json::json!({
            "clientMsgID": "m1", "sendID": "bob", "groupID": "g", "contentType": 106, "sendTime": 1,
            "atTextElem": {
                "text": "同意，@Alice 那你今天出稿吗",
                "isAtSelf": true,
                "atUsersInfo": [{"atUserID": "alice", "groupNickname": "Alice"}]
            }
        });
        let m = message(&raw, &me(), &mut Interner::default()).unwrap();
        assert_eq!(m.mentions.len(), 1);
        let men = &m.mentions[0];
        assert_eq!(&m.text()[men.start..men.end], "@Alice");
        assert!(men.notifies_me);
    }

    #[test]
    fn a_picture_becomes_an_image_attachment() {
        let raw = serde_json::json!({
            "clientMsgID": "p1", "sendID": "bob", "groupID": "g", "contentType": 102, "sendTime": 1,
            "pictureElem": {"sourcePath": "/tmp/排期草稿.png",
                            "sourcePicture": {"uuid": "u", "size": 1_258_291}}
        });
        let m = message(&raw, &me(), &mut Interner::default()).unwrap();
        let a = m.attachment().expect("attachment");
        assert_eq!(a.kind, AttachmentKind::Image);
        assert_eq!(a.name, "排期草稿.png");
        assert_eq!(a.bytes, 1_258_291);
    }

    #[test]
    fn a_quote_carries_the_quoted_summary_and_a_stable_id() {
        let raw = serde_json::json!({
            "clientMsgID": "q1", "sendID": "bob", "groupID": "g", "contentType": 114, "sendTime": 9,
            "quoteElem": {"text": "补充一下",
                          "quoteMessage": {"clientMsgID": "orig", "sendTime": 3,
                                           "senderNickname": "李娜", "contentType": 101,
                                           "textElem": {"content": "原文"}}}
        });
        let mut i = Interner::default();
        let m = message(&raw, &me(), &mut i).unwrap();
        let q = m.quote.expect("quote");
        assert_eq!(q.sender_name, "李娜");
        assert_eq!(q.summary, "原文");
        assert_eq!(q.message_id, i.get("orig").unwrap(), "quote id must match the original");
    }

    #[test]
    fn a_group_created_notification_reads_as_a_sentence() {
        let raw = serde_json::json!({
            "clientMsgID": "n1", "sendID": "sys", "groupID": "g", "contentType": 1501, "sendTime": 1,
            "notificationElem": {"detail": "{\"opUser\":{\"userID\":\"alice\",\"nickname\":\"Alice\"}}"}
        });
        let m = message(&raw, &me(), &mut Interner::default()).unwrap();
        assert!(m.is_system());
        assert_eq!(m.text(), "Alice 创建了群聊");
    }

    #[test]
    fn unknown_content_types_are_skipped_not_mangled() {
        let raw = serde_json::json!({
            "clientMsgID": "x", "sendID": "bob", "groupID": "g", "contentType": 143, "sendTime": 1
        });
        assert!(message(&raw, &me(), &mut Interner::default()).is_none());
    }

    #[test]
    fn a_missing_client_id_is_not_a_message() {
        let raw = serde_json::json!({"contentType": 101, "textElem": {"content": "x"}});
        assert!(message(&raw, &me(), &mut Interner::default()).is_none());
    }

    #[test]
    fn conversations_and_members_translate() {
        let c = conversation(
            &serde_json::json!({"conversationID": "sg_1", "conversationType": 3,
                                "showName": "排期讨论", "unreadCount": 4, "recvMsgOpt": 2,
                                "latestMsgSendTime": 77}),
            &Interner::default(),
        )
        .unwrap();
        assert_eq!(c.name, "排期讨论");
        assert_eq!(c.kind, ConversationKind::Group);
        assert_eq!(c.unread, 4);
        assert!(c.muted);

        let m = member(&serde_json::json!({"userID": "alice", "nickname": "Alice", "roleLevel": 100}))
            .unwrap();
        assert_eq!(m.role, Role::Owner);
        assert_eq!(m.name, "Alice");
    }
}
