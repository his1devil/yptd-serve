//! A deterministic snapshot that stands in for the OpenIM store.
//!
//! Not a demo: this is the fixture the renderer is developed and snapshot-
//! tested against, so it deliberately contains the awkward cases -- CJK width,
//! author grouping, an unread divider, a failed send, a ghost reply, a code
//! fence, a quote, an attachment, and a system notification.

use crate::*;

/// Fixed base time so snapshot output never changes between runs.
/// 2026-09-08T06:00:00Z.
const BASE_MS: i64 = 1_788_847_200_000;

fn minutes(n: i64) -> i64 {
    BASE_MS + n * 60_000
}

fn user(id: &str) -> UserId {
    UserId(id.to_owned())
}

struct Builder {
    conversation: ConversationId,
    counter: u64,
    messages: Vec<Message>,
}

impl Builder {
    fn new(conversation: &str) -> Self {
        Self {
            conversation: ConversationId(conversation.to_owned()),
            counter: 0,
            messages: Vec::new(),
        }
    }

    fn push(&mut self, at: i64, sender: &str, name: &str, body: Body) -> &mut Message {
        self.counter += 1;
        self.messages.push(Message {
            id: MessageId::from_send_time(at, self.counter),
            conversation: self.conversation.clone(),
            sender: user(sender),
            sender_name: name.to_owned(),
            body,
            quote: None,
            reactions: Vec::new(),
            send_state: SendState::Sent,
            visibility: Visibility::Broadcast,
            read_by: None,
            edited: false,
            mentions: Vec::new(),
            transient: false,
        });
        self.messages.last_mut().expect("just pushed")
    }
}

fn text(value: &str) -> Body {
    Body::Text(value.to_owned())
}

/// The full mock snapshot.
pub fn snapshot() -> Snapshot {
    let me = user("7036948217");
    let mut build = Builder::new("sg_paiqi");

    build.push(
        minutes(0),
        "sys",
        "",
        Body::System("李娜 邀请 王强 加入了群聊".to_owned()),
    );

    build.push(
        minutes(2),
        "u_lina",
        "李娜",
        text("这版排期我看了下，核心路径没问题，主要是后半段的节奏偏紧"),
    );
    build.push(
        minutes(3),
        "u_lina",
        "李娜",
        text("尤其是灰度那一周，留的 buffer 太少了"),
    );

    let mention = build.push(
        minutes(5),
        "7036948217",
        "张伟",
        text("同意，@李娜 那你今天能出个修订版吗"),
    );
    mention.mentions.push(Mention {
        start: "同意，".len(),
        end: "同意，@李娜".len(),
        user: user("u_lina"),
        notifies_me: false,
    });
    mention.reactions = vec![
        Reaction {
            emoji: "👍".to_owned(),
            count: 3,
            by_me: true,
        },
        Reaction {
            emoji: "✅".to_owned(),
            count: 1,
            by_me: false,
        },
    ];
    mention.read_by = Some(8);

    let quoted = build.push(
        minutes(9),
        "u_chen",
        "陈明",
        text("灰度这块我补一下：上次那个回滚脚本还没合，先别按一周算"),
    );
    quoted.quote = Some(Quote {
        message_id: MessageId::from_send_time(minutes(3), 3),
        sender_name: "李娜".to_owned(),
        summary: "尤其是灰度那一周，留的 buffer 太少了".to_owned(),
    });

    build.push(
        minutes(12),
        "u_chen",
        "陈明",
        Body::Markdown(
            "回滚入口在这，**记得先跑 dry-run**：\n\
             ```bash\n\
             ./scripts/rollback.sh --dry-run --to v2.4.1\n\
             ```\n\
             确认没问题再去掉 `--dry-run`"
                .to_owned(),
        ),
    );

    // Everything below here is unread.
    build.push(
        minutes(31),
        "u_lina",
        "李娜",
        text("能，草稿在这，主要改了后半段"),
    );
    build.push(
        minutes(31),
        "u_lina",
        "李娜",
        Body::Attachment {
            caption: None,
            attachment: Attachment {
                name: "排期草稿-v3.png".to_owned(),
                bytes: 1_258_291,
                kind: AttachmentKind::Image,
                // The fixture's picture is generated at runtime, so there is
                // nothing to fetch.
                url: String::new(),
            },
        },
    );

    // Three pictures in a row from one person: the case an album exists for.
    for (offset, name) in [(0, "现场-1.png"), (2, "现场-2.png"), (4, "现场-3.png")] {
        build.push(
            minutes(32) + offset * 1_000,
            "u_chen",
            "陈明",
            Body::Attachment {
                caption: None,
                attachment: Attachment {
                    name: name.to_owned(),
                    bytes: 0,
                    kind: AttachmentKind::Image,
                    url: String::new(),
                },
            },
        );
    }

    let failed = build.push(
        minutes(33),
        "7036948217",
        "张伟",
        text("收到，我这边同步给测试"),
    );
    failed.send_state = SendState::Failed;

    let ghost = build.push(
        minutes(34),
        "bot_openclaw",
        "AI 助手",
        text("这条会话的未读要点：灰度周 buffer 不足、回滚脚本未合、李娜已出修订草稿待确认。"),
    );
    ghost.visibility = Visibility::Ghost;

    let read_up_to = build
        .messages
        .iter()
        .find(|message| message.sent_at_ms() == minutes(12))
        .map(|message| message.id);

    let conversations = vec![
        Conversation {
            id: ConversationId("sg_paiqi".to_owned()),
            name: "排期讨论".to_owned(),
            kind: ConversationKind::Group,
            category: Some("产品".to_owned()),
            unread: 7,
            mentions: 0,
            muted: false,
            member_count: 12,
            last_activity_ms: minutes(34),
            read_up_to,
        },
        Conversation {
            id: ConversationId("sg_sheji".to_owned()),
            name: "设计评审".to_owned(),
            kind: ConversationKind::Group,
            category: Some("产品".to_owned()),
            unread: 0,
            mentions: 0,
            muted: false,
            member_count: 7,
            last_activity_ms: minutes(-120),
            read_up_to: None,
        },
        Conversation {
            id: ConversationId("sg_guidang".to_owned()),
            name: "归档-Q2".to_owned(),
            kind: ConversationKind::Group,
            category: Some("产品".to_owned()),
            unread: 0,
            mentions: 0,
            muted: true,
            member_count: 21,
            last_activity_ms: minutes(-8_600),
            read_up_to: None,
        },
        Conversation {
            id: ConversationId("sg_core".to_owned()),
            name: "core".to_owned(),
            kind: ConversationKind::Group,
            category: Some("研发".to_owned()),
            unread: 9,
            mentions: 2,
            muted: false,
            member_count: 16,
            last_activity_ms: minutes(-15),
            read_up_to: None,
        },
        Conversation {
            id: ConversationId("sg_infra".to_owned()),
            name: "infra".to_owned(),
            kind: ConversationKind::Group,
            category: Some("研发".to_owned()),
            unread: 0,
            mentions: 0,
            muted: false,
            member_count: 9,
            last_activity_ms: minutes(-1_500),
            read_up_to: None,
        },
        Conversation {
            id: ConversationId("dm_lina".to_owned()),
            name: "李娜".to_owned(),
            kind: ConversationKind::Direct,
            category: None,
            unread: 7,
            mentions: 0,
            muted: false,
            member_count: 2,
            last_activity_ms: minutes(-6),
            read_up_to: None,
        },
        Conversation {
            id: ConversationId("dm_wang".to_owned()),
            name: "王强".to_owned(),
            kind: ConversationKind::Direct,
            category: None,
            unread: 0,
            mentions: 0,
            muted: false,
            member_count: 2,
            last_activity_ms: minutes(-900),
            read_up_to: None,
        },
    ];

    let members = vec![
        Member {
            id: user("7036948217"),
            name: "张伟".to_owned(),
            role: Role::Owner,
            online: true,
            is_bot: false,
        },
        Member {
            id: user("u_lina"),
            name: "李娜".to_owned(),
            role: Role::Admin,
            online: true,
            is_bot: false,
        },
        Member {
            id: user("u_chen"),
            name: "陈明".to_owned(),
            role: Role::Admin,
            online: false,
            is_bot: false,
        },
        Member {
            id: user("bot_openclaw"),
            name: "AI 助手".to_owned(),
            role: Role::Member,
            online: true,
            is_bot: true,
        },
        Member {
            id: user("u_wang"),
            name: "王强".to_owned(),
            role: Role::Member,
            online: true,
            is_bot: false,
        },
        Member {
            id: user("u_zhao"),
            name: "赵敏".to_owned(),
            role: Role::Member,
            online: false,
            is_bot: false,
        },
    ];

    Snapshot {
        me: Some(me),
        my_name: "张伟".to_owned(),
        conversations,
        messages: build.messages,
        members,
        connected: true,
        sync_percent: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixture_covers_the_cases_the_renderer_must_handle() {
        let snapshot = snapshot();
        let messages = snapshot.messages_in(&ConversationId("sg_paiqi".to_owned()));

        assert!(messages.iter().any(|m| m.is_system()), "system notification");
        assert!(messages.iter().any(|m| m.quote.is_some()), "quote");
        assert!(messages.iter().any(|m| !m.reactions.is_empty()), "reactions");
        assert!(messages.iter().any(|m| m.attachment().is_some()), "attachment");
        assert!(messages.iter().any(|m| m.read_by.is_some()), "read receipt");
        assert!(
            messages.iter().any(|m| m.send_state == SendState::Failed),
            "failed send"
        );
        assert!(
            messages.iter().any(|m| m.visibility == Visibility::Ghost),
            "ghost reply"
        );
        assert!(
            messages
                .iter()
                .any(|m| matches!(m.body, Body::Markdown(_))),
            "markdown with a code fence"
        );
        assert!(
            messages.iter().any(|m| !m.mentions.is_empty()),
            "mention range"
        );
    }

    #[test]
    fn consecutive_messages_from_one_sender_exist_for_author_grouping() {
        let snapshot = snapshot();
        let messages = snapshot.messages_in(&ConversationId("sg_paiqi".to_owned()));
        let grouped = messages
            .windows(2)
            .any(|pair| pair[0].sender == pair[1].sender);
        assert!(grouped, "fixture needs a same-sender run");
    }

    #[test]
    fn messages_are_returned_in_send_order() {
        let snapshot = snapshot();
        let messages = snapshot.messages_in(&ConversationId("sg_paiqi".to_owned()));
        assert!(messages.windows(2).all(|pair| pair[0].id < pair[1].id));
    }

    #[test]
    fn the_read_cursor_leaves_unread_messages_above_the_newest() {
        let snapshot = snapshot();
        let conversation = snapshot
            .conversation(&ConversationId("sg_paiqi".to_owned()))
            .expect("fixture conversation");
        let read_up_to = conversation.read_up_to.expect("fixture sets a read cursor");
        let unread = snapshot
            .messages_in(&conversation.id)
            .into_iter()
            .filter(|message| message.id > read_up_to)
            .count();
        assert_eq!(unread, conversation.unread as usize);
    }
}
