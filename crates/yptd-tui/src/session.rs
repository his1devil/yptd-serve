//! The live session: sidecar in, domain model out.
//!
//! Everything the TUI knows about OpenIM arrives through here, already
//! translated. `App` never sees SDK JSON, and the sidecar never sees a
//! `Snapshot`. That seam is what lets the mock and the real backend drive the
//! same renderer.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use im_model::translate::{self, Interner};
use im_model::{
    ConversationId, ConversationKind, Message, MessageId, SendState, Snapshot, UserId,
};
use im_sidecar::{Event, Sidecar};
use serde_json::{json, Value};

use crate::app::DraftMention;
use crate::config::{Config, Paths};

/// The SDK's "User has logged in repeatedly". Not a failure when the session
/// it is complaining about is the one being asked for.
const ALREADY_LOGGED_IN: i32 = 10102;

/// Messages per history request. Enough to fill a tall terminal twice over,
/// small enough that reaching the top does not stall.
const PAGE: i64 = 60;

/// Who the sidecar is currently logged in as, if anyone.
fn logged_in_user(sidecar: &Sidecar) -> Option<String> {
    sidecar
        .call("self", json!({}))
        .ok()?
        .get("userID")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub struct Session {
    sidecar: Arc<Sidecar>,
    interner: Interner,
    me: UserId,
    my_name: String,
    /// Conversations whose history has been pulled at least once, so opening
    /// one again does not re-fetch what is already on screen.
    loaded: HashSet<ConversationId>,
    /// Group whose members are currently in the snapshot.
    members_for: Option<String>,
    /// Conversations whose history has been read back to the beginning, so
    /// scrolling to the top does not keep asking for a page that is not there.
    exhausted: HashSet<ConversationId>,
    /// Where the next page starts when the last one held nothing worth
    /// showing: a page of pure notifications adds no message to page from.
    page_cursor: HashMap<ConversationId, String>,
    /// OpenIM's reserved id for "@everyone", read from the SDK rather than
    /// hardcoded, since it travels in `atUserList` like a real user id.
    at_all_tag: String,
    /// True when this session took over a sidecar that was already logged in,
    /// which means the initial sync happened on some earlier run.
    resumed: bool,
}

/// What an incoming event changed, so the caller can decide whether a redraw
/// is worth it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Changed {
    pub messages: bool,
    pub conversations: bool,
    pub connection: bool,
    /// The SDK finished a server sync: its local store may now hold
    /// conversations and messages it never pushed as events, so the caller
    /// should [`Session::resync`].
    pub resync: bool,
    /// Membership of the open group changed; the caller should
    /// [`Session::ensure_members`] again.
    pub members: bool,
}

impl Session {
    /// Spawns the sidecar, initialises the SDK and logs in. Returns the event
    /// receiver so the main loop can merge it with terminal input.
    pub fn start(
        paths: &Paths,
        config: &Config,
        user_id: &str,
        nickname: &str,
        im_token: &str,
    ) -> Result<(Self, Receiver<Event>), Box<dyn std::error::Error>> {
        let (sidecar, events) = Sidecar::spawn(&im_sidecar::Config {
            binary: crate::embedded::sidecar_path(paths),
            socket: paths.socket(),
            data_dir: paths.data_dir(),
        })?;

        sidecar.call(
            "init",
            json!({
                "api_addr": config.api,
                "ws_addr": config.ws,
                "data_dir": paths.data_dir(),
                "platform_id": crate::auth::platform_id(),
            }),
        )?;
        // An adopted sidecar may already be logged in. That is a good thing --
        // no re-sync, instant start -- but the SDK reports it as an error, so
        // the "already there" case has to be recognised rather than obeyed.
        let mut resumed = false;
        match sidecar.call("login", json!({ "user_id": user_id, "token": im_token })) {
            Ok(_) => {}
            Err(im_sidecar::Error::Sdk { code, msg }) if code == ALREADY_LOGGED_IN => {
                match logged_in_user(&sidecar) {
                    Some(who) if who == user_id => resumed = true,
                    // Somebody else's session: end it and take over, rather
                    // than driving a client that speaks as the wrong person.
                    _ => {
                        let _ = sidecar.call("logout", json!({}));
                        sidecar
                            .call("login", json!({ "user_id": user_id, "token": im_token }))
                            .map_err(|_| im_sidecar::Error::Sdk { code, msg })?;
                    }
                }
            }
            Err(e) => return Err(e.into()),
        }

        let at_all_tag = sidecar
            .call("at_all_tag", json!({}))
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .filter(|tag| !tag.is_empty())
            .unwrap_or_else(|| im_model::AT_ALL_TAG.to_owned());

        Ok((
            Self {
                sidecar: Arc::new(sidecar),
                interner: Interner::default(),
                me: UserId(user_id.to_owned()),
                my_name: nickname.to_owned(),
                resumed,
                loaded: HashSet::new(),
                members_for: None,
                exhausted: HashSet::new(),
            page_cursor: HashMap::new(),
                at_all_tag,
            },
            events,
        ))
    }

    /// Loads what the sidebar needs: every conversation, then the groups so
    /// member counts and names are right.
    pub fn bootstrap(&mut self, snapshot: &mut Snapshot) -> Result<(), im_sidecar::Error> {
        snapshot.me = Some(self.me.clone());
        snapshot.my_name = self.my_name.clone();
        snapshot.connected = true;

        let convs = self.sidecar.call("conversations", json!({}))?;
        for raw in convs.as_array().into_iter().flatten() {
            if let Some(c) = translate::conversation(raw, &self.interner) {
                snapshot.upsert_conversation(c);
            }
        }

        // Groups the SDK knows about but that have no conversation row yet
        // (freshly created, nothing said) still belong in the sidebar.
        let groups = self.sidecar.call("groups", json!({}))?;
        for g in groups.as_array().into_iter().flatten() {
            let id = g.get("groupID").and_then(Value::as_str).unwrap_or("");
            if id.is_empty() {
                continue;
            }
            let conv_id = ConversationId(format!("sg_{id}"));
            let name = g
                .get("groupName")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .to_owned();
            let count = g.get("memberCount").and_then(Value::as_u64).unwrap_or(0) as u32;
            match snapshot.conversation_mut(&conv_id) {
                Some(c) => {
                    if c.name.is_empty() || c.name == id {
                        c.name = name;
                    }
                    c.member_count = count;
                }
                None => snapshot.upsert_conversation(im_model::Conversation {
                    id: conv_id,
                    name,
                    kind: ConversationKind::Group,
                    category: Some(translate::category_for(ConversationKind::Group).to_owned()),
                    unread: 0,
                    mentions: 0,
                    muted: false,
                    member_count: count,
                    last_activity_ms: g.get("createTime").and_then(Value::as_i64).unwrap_or(0),
                    read_up_to: None,
                }),
            }
        }
        Ok(())
    }

    /// Waits for the SDK's post-login sync so the first frame is not empty.
    ///
    /// `Login` returns as soon as the socket is up; conversations and groups
    /// are pulled into the SDK's local store afterwards and only that store
    /// is what `conversations` / `groups` read. Events seen while waiting
    /// are folded into `snapshot`, not dropped. Gives up after `limit`, and
    /// earlier if the connection is up and nothing has arrived for a while.
    pub fn wait_for_sync(
        &mut self,
        events: &Receiver<Event>,
        snapshot: &mut Snapshot,
        limit: Duration,
    ) -> bool {
        let deadline = Instant::now() + limit;
        // Short: this only decides whether the first frame has yesterday's
        // conversations or today's. A late sync is caught by the
        // OnSyncServerFinish event and folded in when it lands.
        let quiet = Duration::from_millis(600);
        let mut connected = false;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            match events.recv_timeout(remaining.min(quiet)) {
                Ok(ev) => {
                    let changed = self.apply(&ev, snapshot);
                    connected |= snapshot.connected;
                    if changed.resync {
                        return true;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if connected => return false,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return false,
            }
        }
    }

    /// Re-reads the SDK store after a sync. History pages are forgotten so
    /// the open conversation refetches on its next frame.
    pub fn resync(&mut self, snapshot: &mut Snapshot) -> Result<(), im_sidecar::Error> {
        self.loaded.clear();
        self.members_for = None;
        self.bootstrap(snapshot)
    }

    /// Pulls the newest page of a conversation's history, once.
    pub fn ensure_history(
        &mut self,
        conv: &ConversationId,
        snapshot: &mut Snapshot,
    ) -> Result<bool, im_sidecar::Error> {
        if self.loaded.contains(conv) {
            return Ok(false);
        }
        let reply = self.sidecar.call(
            "history",
            json!({
                "conversationID": conv.0,
                "startClientMsgID": "",
                "count": PAGE,
            }),
        )?;
        let mut added = 0;
        for raw in reply
            .get("messageList")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(m) = translate::message(raw, &self.me, &mut self.interner) {
                snapshot.upsert_message(m);
                added += 1;
            }
        }
        self.loaded.insert(conv.clone());
        if reply.get("isEnd").and_then(Value::as_bool).unwrap_or(false) {
            self.exhausted.insert(conv.clone());
        }
        Ok(added > 0)
    }

    /// The client id of the oldest message loaded for `conv`, which is where
    /// the next page of history starts.
    pub fn oldest_client_id(&self, conv: &ConversationId, snapshot: &Snapshot) -> Option<String> {
        if let Some(cursor) = self.page_cursor.get(conv) {
            return Some(cursor.clone());
        }
        snapshot
            .messages_in(conv)
            .first()
            .and_then(|m| self.interner.client_msg_id(m.id))
            .map(str::to_owned)
    }

    /// A worker that fetches history pages without blocking the interface.
    /// The SDK may have to go to the server for a page, and a stall right when
    /// somebody reaches the top of what they have is the worst place for one.
    pub fn pager<T, F>(&self, out: Sender<T>, wake: F) -> Pager
    where
        T: Send + 'static,
        F: Fn(Page) -> T + Send + 'static,
    {
        Pager::start(Arc::clone(&self.sidecar), out, wake)
    }

    /// Folds a fetched page into the snapshot. Returns how many were new.
    pub fn absorb_page(&mut self, page: &Page, snapshot: &mut Snapshot) -> usize {
        let Ok(reply) = &page.reply else {
            return 0;
        };
        let conv = &page.conversation;
        let list = reply.get("messageList").and_then(Value::as_array);
        let mut added = 0;
        for raw in list.into_iter().flatten() {
            if let Some(m) = translate::message(raw, &self.me, &mut self.interner) {
                snapshot.upsert_message(m);
                added += 1;
            }
        }
        // The SDK says so itself; "fewer than asked for" would stop early
        // whenever a page happened to be all notifications.
        let end = reply.get("isEnd").and_then(Value::as_bool).unwrap_or(false);
        if end {
            self.exhausted.insert(conv.clone());
            self.page_cursor.remove(conv);
        } else if added > 0 {
            self.page_cursor.remove(conv);
        } else {
            // Nothing to show but not the end either: page on from the oldest
            // of what came back, or the same request would repeat forever.
            match list.and_then(|raw| oldest_raw_client_id(raw)) {
                Some(id) => {
                    self.page_cursor.insert(conv.clone(), id);
                }
                None => {
                    self.exhausted.insert(conv.clone());
                }
            }
        }
        added
    }

    pub fn has_older(&self, conv: &ConversationId) -> bool {
        !self.exhausted.contains(conv)
    }

    /// Marks a conversation read.
    ///
    /// Separate from [`Self::ensure_history`] on purpose: history is fetched
    /// once, but reading happens every time somebody opens the conversation.
    /// Folding the two together left the badge lit on the second visit.
    pub fn mark_read(&mut self, conv: &ConversationId, snapshot: &mut Snapshot) -> bool {
        let had_unread = snapshot
            .conversation_mut(conv)
            .map(|c| {
                let was = c.unread > 0 || c.mentions > 0;
                c.unread = 0;
                c.mentions = 0;
                was
            })
            .unwrap_or(false);
        // Best effort: a failure here costs a badge, not a message.
        let _ = self
            .sidecar
            .call("mark_read", json!({ "conversation_id": conv.0 }));
        had_unread
    }

    /// Loads the member list for the group behind `conv`, if it is a group.
    pub fn ensure_members(
        &mut self,
        conv: &ConversationId,
        snapshot: &mut Snapshot,
    ) -> Result<bool, im_sidecar::Error> {
        let Some(group_id) = conv.0.strip_prefix("sg_") else {
            snapshot.set_members(Vec::new());
            self.members_for = None;
            return Ok(true);
        };
        if self.members_for.as_deref() == Some(group_id) {
            return Ok(false);
        }
        let reply = self
            .sidecar
            .call("group_members", json!({ "group_id": group_id, "count": 500 }))?;
        let members = reply
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(translate::member)
            .collect();
        snapshot.set_members(members);
        self.members_for = Some(group_id.to_owned());
        Ok(true)
    }

    /// Whether an already-synced session was taken over, in which case there
    /// is no first sync to wait for.
    pub fn resumed(&self) -> bool {
        self.resumed
    }

    pub fn at_all_tag(&self) -> &str {
        &self.at_all_tag
    }

    /// The SDK's own JSON for a message, which is what its quote primitives
    /// take -- they want the whole original, not an id. Read from the local
    /// store, so it works for anything on screen however long ago it loaded.
    fn raw_message(
        &self,
        conv: &ConversationId,
        id: MessageId,
    ) -> Result<Option<Value>, im_sidecar::Error> {
        let Some(client_id) = self.interner.client_msg_id(id) else {
            return Ok(None);
        };
        let reply = self.sidecar.call(
            "find_message",
            json!({ "conversation_id": conv.0, "client_msg_ids": [client_id] }),
        )?;
        Ok(reply
            .get("findResultItems")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("messageList"))
            .and_then(Value::as_array)
            .and_then(|list| list.first())
            .cloned())
    }

    /// Sends a picture and waits for it. Uploading blocks, so the interactive
    /// client uses [`Uploads`] instead; this is for the one-shot diagnostics.
    pub fn send_image(
        &mut self,
        conv: &ConversationId,
        path: &std::path::Path,
        snapshot: &mut Snapshot,
    ) -> Result<(), im_sidecar::Error> {
        let sent = upload(&self.sidecar, &self.me, conv, path)?;
        self.absorb(sent, snapshot);
        Ok(())
    }

    /// A queue for pictures, so the interface keeps painting while they go
    /// up. Every job it finishes comes back to the caller's channel.
    pub fn uploads<T, F>(&self, out: Sender<T>, wake: F) -> Uploads
    where
        T: Send + 'static,
        F: Fn(Result<Value, im_sidecar::Error>) -> T + Send + 'static,
    {
        Uploads::start(Arc::clone(&self.sidecar), self.me.clone(), out, wake)
    }

    /// Folds the SDK's echo of a message we sent into the snapshot.
    pub fn absorb(&mut self, sent: Value, snapshot: &mut Snapshot) -> bool {
        match translate::message(&sent, &self.me, &mut self.interner) {
            Some(mut m) => {
                m.send_state = SendState::Sent;
                snapshot.upsert_message(m);
                true
            }
            None => false,
        }
    }

    /// Sends a message, optionally quoting one and mentioning people. The SDK
    /// echoes it back with its final ids, and that echo is what goes into the
    /// snapshot -- never a local guess.
    pub fn send_text(
        &mut self,
        conv: &ConversationId,
        text: &str,
        reply_to: Option<MessageId>,
        mentions: &[DraftMention],
        snapshot: &mut Snapshot,
    ) -> Result<(), im_sidecar::Error> {
        let quote = match reply_to {
            Some(id) => self.raw_message(conv, id)?,
            None => None,
        };
        // The SDK nests a quote inside the at-text element, so a reply that
        // also mentions somebody is one message, not two.
        let created = if mentions.is_empty() {
            match &quote {
                Some(q) => self.sidecar.call("create_quote", json!({ "text": text, "quote": q }))?,
                None => self.sidecar.call("create_text", json!({ "text": text }))?,
            }
        } else {
            let ids: Vec<&str> = mentions.iter().map(|m| m.user_id.as_str()).collect();
            let info: Vec<Value> = mentions
                .iter()
                .map(|m| json!({ "atUserID": m.user_id, "groupNickname": m.nickname }))
                .collect();
            self.sidecar.call(
                "create_at_text",
                json!({
                    "text": text,
                    "at_user_ids": ids,
                    "at_users_info": info,
                    "quote": quote,
                }),
            )?
        };

        let (recv_id, group_id) = match conv.0.strip_prefix("sg_") {
            Some(g) => (String::new(), g.to_owned()),
            None => (peer_of(conv, &self.me), String::new()),
        };
        let sent = self.sidecar.call(
            "send",
            json!({ "message": created, "recv_id": recv_id, "group_id": group_id }),
        )?;
        if let Some(mut m) = translate::message(&sent, &self.me, &mut self.interner) {
            m.send_state = SendState::Sent;
            snapshot.upsert_message(m);
        }
        Ok(())
    }

    /// Creates a group with the local user as owner and puts it in the
    /// sidebar at once, so the person can start inviting before the SDK's
    /// own conversation row arrives.
    pub fn create_group(
        &mut self,
        name: &str,
        snapshot: &mut Snapshot,
    ) -> Result<ConversationId, im_sidecar::Error> {
        let reply = self.sidecar.call(
            "create_group",
            json!({
                "memberUserIDs": [],
                "adminUserIDs": [],
                "ownerUserID": "",
                "groupInfo": { "groupName": name, "groupType": 2 },
            }),
        )?;
        let info = reply.get("groupInfo").unwrap_or(&reply);
        let group_id = info.get("groupID").and_then(Value::as_str).unwrap_or("");
        if group_id.is_empty() {
            return Err(im_sidecar::Error::Sdk {
                code: 0,
                msg: "服务端没有返回 groupID".into(),
            });
        }
        let conv = ConversationId(format!("sg_{group_id}"));
        snapshot.upsert_conversation(im_model::Conversation {
            id: conv.clone(),
            name: info
                .get("groupName")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty())
                .unwrap_or(name)
                .to_owned(),
            kind: ConversationKind::Group,
            category: Some(translate::category_for(ConversationKind::Group).to_owned()),
            unread: 0,
            mentions: 0,
            muted: false,
            member_count: info.get("memberCount").and_then(Value::as_u64).unwrap_or(1) as u32,
            last_activity_ms: info
                .get("createTime")
                .and_then(Value::as_i64)
                .filter(|t| *t > 0)
                .unwrap_or_else(now_ms),
            read_up_to: None,
        });
        Ok(conv)
    }

    /// Invites people into a group. The member list is forgotten so the next
    /// frame refetches it rather than waiting on the SDK's callback.
    pub fn invite(&mut self, group_id: &str, user_ids: &[String]) -> Result<(), im_sidecar::Error> {
        self.sidecar.call(
            "invite",
            json!({ "group_id": group_id, "reason": "", "user_ids": user_ids }),
        )?;
        self.members_for = None;
        Ok(())
    }

    /// Opens a direct conversation; see [`local_direct`].
    pub fn open_direct(&mut self, user_id: &str, nickname: &str, snapshot: &mut Snapshot) -> ConversationId {
        local_direct(snapshot, &self.me.0, user_id, nickname)
    }

    /// Folds one SDK event into the snapshot.
    pub fn apply(&mut self, event: &Event, snapshot: &mut Snapshot) -> Changed {
        let mut changed = Changed::default();
        match event.name.as_str() {
            "OnRecvNewMessage" | "OnRecvOfflineNewMessage" | "OnRecvOnlineOnlyMessage" => {
                if let Some(m) = translate::message(&event.data, &self.me, &mut self.interner) {
                    self.bump_unread(&m, snapshot);
                    snapshot.upsert_message(m);
                    changed.messages = true;
                }
            }
            "OnRecvNewMessages" | "OnRecvOfflineNewMessages" => {
                for raw in event.data.as_array().into_iter().flatten() {
                    if let Some(m) = translate::message(raw, &self.me, &mut self.interner) {
                        self.bump_unread(&m, snapshot);
                        snapshot.upsert_message(m);
                        changed.messages = true;
                    }
                }
            }
            "OnNewConversation" | "OnConversationChanged" => {
                for raw in event.data.as_array().into_iter().flatten() {
                    if let Some(c) = translate::conversation(raw, &self.interner) {
                        // Keep a name we already resolved from the group list
                        // if the SDK's row has a worse one.
                        let existing_name = snapshot
                            .conversation(&c.id)
                            .map(|e| e.name.clone())
                            .filter(|n| !n.is_empty());
                        let mut c = c;
                        if c.name.is_empty()
                            && let Some(n) = existing_name
                        {
                            c.name = n;
                        }
                        snapshot.upsert_conversation(c);
                        changed.conversations = true;
                    }
                }
            }
            "OnNewRecvMessageRevoked" => {
                // The event names the withdrawn message by its client id, so
                // it can be dropped from the list rather than left on screen
                // with a notice beside it.
                let client_id = event.data.get("clientMsgID").and_then(Value::as_str);
                if let Some(id) = client_id.and_then(|c| self.interner.get(c))
                    && snapshot.remove_message(id)
                {
                    changed.messages = true;
                }
            }
            "OnConnectSuccess" => {
                snapshot.connected = true;
                changed.connection = true;
            }
            "OnConnecting" | "OnConnectFailed" | "OnKickedOffline" | "OnUserTokenExpired"
            | "OnUserTokenInvalid" => {
                snapshot.connected = false;
                changed.connection = true;
            }
            "OnSyncServerStart" => {
                snapshot.sync_percent = Some(0);
                changed.connection = true;
            }
            "OnSyncServerProgress" => {
                snapshot.sync_percent = event
                    .data
                    .get("progress")
                    .and_then(Value::as_u64)
                    .map(|p| p.min(100) as u8);
                changed.connection = true;
            }
            "OnSyncServerFinish" => {
                snapshot.sync_percent = None;
                changed.connection = true;
                changed.resync = true;
            }
            "OnSyncServerFailed" => {
                snapshot.sync_percent = None;
                changed.connection = true;
            }
            "OnJoinedGroupAdded" | "OnJoinedGroupDeleted" | "OnGroupInfoChanged" => {
                changed.resync = true;
            }
            "OnGroupMemberAdded" | "OnGroupMemberDeleted" | "OnGroupMemberInfoChanged" => {
                let group = event.data.get("groupID").and_then(Value::as_str).unwrap_or("");
                if self.members_for.as_deref() == Some(group) {
                    self.members_for = None;
                    changed.members = true;
                }
            }
            _ => {}
        }
        changed
    }

    /// A message in a conversation that is not open counts as unread until
    /// the SDK's own conversation update says otherwise.
    fn bump_unread(&self, m: &Message, snapshot: &mut Snapshot) {
        if m.sender == self.me || m.is_system() {
            return;
        }
        if let Some(c) = snapshot.conversation_mut(&m.conversation) {
            c.last_activity_ms = m.sent_at_ms().max(c.last_activity_ms);
        }
    }
}

/// Puts a direct conversation in the sidebar if the two have never talked.
/// The server-side row appears with the first message; until then history is
/// simply empty. Returns the id either way.
pub fn local_direct(snapshot: &mut Snapshot, me: &str, user_id: &str, nickname: &str) -> ConversationId {
    let id = translate::direct_conversation_id(me, user_id);
    if snapshot.conversation(&id).is_none() {
        snapshot.upsert_conversation(im_model::Conversation {
            id: id.clone(),
            name: if nickname.is_empty() { user_id } else { nickname }.to_owned(),
            kind: ConversationKind::Direct,
            category: Some(translate::category_for(ConversationKind::Direct).to_owned()),
            unread: 0,
            mentions: 0,
            muted: false,
            member_count: 2,
            last_activity_ms: now_ms(),
            read_up_to: None,
        });
    }
    id
}

/// One picture, start to finish: create the message, hand it to the SDK,
/// return what came back. Free of `Session` so it can run off the main
/// thread; the translation that needs `&mut Session` happens back home.
fn upload(
    sidecar: &Sidecar,
    me: &UserId,
    conv: &ConversationId,
    path: &std::path::Path,
) -> Result<Value, im_sidecar::Error> {
    let (recv_id, group_id) = target_of(conv, me)?;
    let created = sidecar.call("create_image", json!({ "path": path.to_string_lossy() }))?;
    sidecar.call(
        "send",
        json!({ "message": created, "recv_id": recv_id, "group_id": group_id }),
    )
}

/// The client id of the earliest message in a raw page, by sequence number.
fn oldest_raw_client_id(list: &[Value]) -> Option<String> {
    list.iter()
        .min_by_key(|raw| {
            (
                raw.get("seq").and_then(Value::as_i64).unwrap_or(i64::MAX),
                raw.get("sendTime").and_then(Value::as_i64).unwrap_or(i64::MAX),
            )
        })
        .and_then(|raw| raw.get("clientMsgID").and_then(Value::as_str))
        .map(str::to_owned)
}

/// One fetched page of history, raw, for the main thread to translate.
pub struct Page {
    pub conversation: ConversationId,
    pub reply: Result<Value, im_sidecar::Error>,
}

/// Fetches history pages on its own thread.
pub struct Pager {
    jobs: Option<Sender<(ConversationId, String)>>,
    loading: HashSet<ConversationId>,
    /// When a page last failed, so a broken connection is retried at a
    /// walking pace rather than on every frame.
    failed: HashMap<ConversationId, Instant>,
}

/// How long after a failed page the next attempt waits.
const RETRY_AFTER: Duration = Duration::from_secs(5);

impl Pager {
    fn start<T, F>(sidecar: Arc<Sidecar>, out: Sender<T>, wake: F) -> Self
    where
        T: Send + 'static,
        F: Fn(Page) -> T + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel::<(ConversationId, String)>();
        let spawned = std::thread::Builder::new()
            .name("history-pager".into())
            .spawn(move || {
                for (conversation, oldest) in rx {
                    let reply = sidecar.call(
                        "history",
                        json!({
                            "conversationID": conversation.0,
                            "startClientMsgID": oldest,
                            "count": PAGE,
                        }),
                    );
                    if out.send(wake(Page { conversation, reply })).is_err() {
                        break;
                    }
                }
            });
        Self {
            jobs: spawned.is_ok().then_some(tx),
            loading: HashSet::new(),
            failed: HashMap::new(),
        }
    }

    pub fn disabled() -> Self {
        Self { jobs: None, loading: HashSet::new(), failed: HashMap::new() }
    }

    /// Asks for the page before `oldest`, unless one is already on its way or
    /// the last one failed a moment ago.
    pub fn request(&mut self, conv: &ConversationId, oldest: String) -> bool {
        if self.loading.contains(conv) {
            return false;
        }
        if self.failed.get(conv).is_some_and(|at| at.elapsed() < RETRY_AFTER) {
            return false;
        }
        let Some(jobs) = self.jobs.as_ref() else {
            return false;
        };
        if jobs.send((conv.clone(), oldest)).is_err() {
            return false;
        }
        self.loading.insert(conv.clone());
        true
    }

    pub fn finished(&mut self, conv: &ConversationId) {
        self.loading.remove(conv);
        self.failed.remove(conv);
    }

    pub fn failed(&mut self, conv: &ConversationId) {
        self.loading.remove(conv);
        self.failed.insert(conv.clone(), Instant::now());
    }

    pub fn is_loading(&self, conv: &ConversationId) -> bool {
        self.loading.contains(conv)
    }
}

/// A single worker that sends queued pictures in order.
///
/// In order, and one at a time, on purpose: four pictures sent at once would
/// race into the conversation in whatever order the uploads finished, and
/// the album that groups them would shuffle every time it redrew.
pub struct Uploads {
    jobs: Option<Sender<(ConversationId, PathBuf)>>,
    /// Pictures queued but not yet answered for, for the status line.
    outstanding: usize,
}

impl Uploads {
    fn start<T, F>(sidecar: Arc<Sidecar>, me: UserId, out: Sender<T>, wake: F) -> Self
    where
        T: Send + 'static,
        F: Fn(Result<Value, im_sidecar::Error>) -> T + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel::<(ConversationId, PathBuf)>();
        let spawned = std::thread::Builder::new()
            .name("image-uploads".into())
            .spawn(move || {
                for (conv, path) in rx {
                    let outcome = upload(&sidecar, &me, &conv, &path);
                    if out.send(wake(outcome)).is_err() {
                        break;
                    }
                }
            });
        Self {
            jobs: spawned.is_ok().then_some(tx),
            outstanding: 0,
        }
    }

    /// An `Uploads` that accepts nothing, for the mock.
    pub fn disabled() -> Self {
        Self { jobs: None, outstanding: 0 }
    }

    pub fn queue(&mut self, conv: &ConversationId, path: PathBuf) -> bool {
        let Some(jobs) = self.jobs.as_ref() else {
            return false;
        };
        if jobs.send((conv.clone(), path)).is_err() {
            return false;
        }
        self.outstanding += 1;
        true
    }

    /// Records that one job answered. Returns how many are still out.
    pub fn finished(&mut self) -> usize {
        self.outstanding = self.outstanding.saturating_sub(1);
        self.outstanding
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Splits a conversation id into the pair the SDK's send takes: a recipient
/// for a direct chat, a group for a group chat, never both and never neither.
///
/// The SDK answers an empty pair with nothing but "invalid input arguments",
/// so this refuses first and says which conversation was wrong.
fn target_of(
    conv: &ConversationId,
    me: &UserId,
) -> Result<(String, String), im_sidecar::Error> {
    if let Some(group) = conv.0.strip_prefix("sg_") {
        if !group.is_empty() {
            return Ok((String::new(), group.to_owned()));
        }
    } else {
        let peer = peer_of(conv, me);
        if !peer.is_empty() {
            return Ok((peer, String::new()));
        }
    }
    Err(im_sidecar::Error::Sdk {
        code: 0,
        msg: if conv.0.is_empty() {
            "没有打开任何会话".into()
        } else {
            format!("会话 {} 不是可以发送的对象", conv.0)
        },
    })
}

/// The other party in a direct conversation id `si_<a>_<b>`.
fn peer_of(conv: &ConversationId, me: &UserId) -> String {
    conv.0
        .strip_prefix("si_")
        .map(|pair| {
            pair.split('_')
                .find(|p| *p != me.0)
                .unwrap_or("")
                .to_owned()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_send_with_no_conversation_is_refused_before_it_reaches_the_sdk() {
        // The SDK's own answer here is "invalid input arguments" and nothing
        // more, which is what a brand-new account used to see on its first
        // keystroke.
        let me = UserId("alice".into());
        let err = target_of(&ConversationId(String::new()), &me).unwrap_err();
        assert!(err.to_string().contains("没有打开任何会话"), "{err}");
        assert!(target_of(&ConversationId("sg_".into()), &me).is_err(), "群号为空");
        assert!(target_of(&ConversationId("垃圾".into()), &me).is_err());
        assert!(target_of(&ConversationId("si_alice_alice".into()), &me).is_err(), "只有我自己");
    }

    #[test]
    fn a_send_target_is_a_group_or_a_person_never_both() {
        let me = UserId("alice".into());
        assert_eq!(
            target_of(&ConversationId("sg_42".into()), &me).unwrap(),
            (String::new(), "42".to_owned())
        );
        assert_eq!(
            target_of(&ConversationId("si_alice_zed".into()), &me).unwrap(),
            ("zed".to_owned(), String::new())
        );
    }

    #[test]
    fn the_peer_of_a_direct_conversation_is_the_one_who_is_not_me() {
        let me = UserId("alice".into());
        assert_eq!(peer_of(&ConversationId("si_alice_zed".into()), &me), "zed");
        assert_eq!(peer_of(&ConversationId("si_bob_alice".into()), &me), "bob");
        assert_eq!(peer_of(&ConversationId("sg_1".into()), &me), "");
    }
}
