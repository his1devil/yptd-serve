//! The live session: sidecar in, domain model out.
//!
//! Everything the TUI knows about OpenIM arrives through here, already
//! translated. `App` never sees SDK JSON, and the sidecar never sees a
//! `Snapshot`. That seam is what lets the mock and the real backend drive the
//! same renderer.

use std::collections::HashSet;
use std::sync::mpsc::Receiver;

use im_model::translate::{self, Interner};
use im_model::{ConversationId, ConversationKind, Message, SendState, Snapshot, UserId};
use im_sidecar::{Event, Sidecar};
use serde_json::{json, Value};

use crate::config::{Config, Paths};

pub struct Session {
    sidecar: Sidecar,
    interner: Interner,
    me: UserId,
    /// Conversations whose history has been pulled at least once, so opening
    /// one again does not re-fetch what is already on screen.
    loaded: HashSet<ConversationId>,
    /// Group whose members are currently in the snapshot.
    members_for: Option<String>,
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
}

impl Session {
    /// Spawns the sidecar, initialises the SDK and logs in. Returns the event
    /// receiver so the main loop can merge it with terminal input.
    pub fn start(
        paths: &Paths,
        config: &Config,
        user_id: &str,
        im_token: &str,
    ) -> Result<(Self, Receiver<Event>), Box<dyn std::error::Error>> {
        let (sidecar, events) = Sidecar::spawn(&im_sidecar::Config {
            binary: std::env::var_os("YPTD_SIDECAR").map(Into::into),
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
        sidecar.call("login", json!({ "user_id": user_id, "token": im_token }))?;

        Ok((
            Self {
                sidecar,
                interner: Interner::default(),
                me: UserId(user_id.to_owned()),
                loaded: HashSet::new(),
                members_for: None,
            },
            events,
        ))
    }

    /// Loads what the sidebar needs: every conversation, then the groups so
    /// member counts and names are right.
    pub fn bootstrap(&mut self, snapshot: &mut Snapshot) -> Result<(), im_sidecar::Error> {
        snapshot.me = Some(self.me.clone());
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
        limit: std::time::Duration,
    ) -> bool {
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + limit;
        let quiet = Duration::from_millis(1500);
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
                "count": 60,
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
        // Opening a conversation reads it; tell the server so other devices
        // and the unread badge agree.
        if let Some(c) = snapshot.conversation_mut(conv) {
            c.unread = 0;
        }
        let _ = self.sidecar.call("mark_read", json!({ "conversation_id": conv.0 }));
        Ok(added > 0)
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

    /// Sends a text message. The SDK echoes the message back with its final
    /// ids, which is what goes into the snapshot -- not a local guess.
    pub fn send_text(
        &mut self,
        conv: &ConversationId,
        text: &str,
        snapshot: &mut Snapshot,
    ) -> Result<(), im_sidecar::Error> {
        let created = self.sidecar.call("create_text", json!({ "text": text }))?;
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
    fn the_peer_of_a_direct_conversation_is_the_one_who_is_not_me() {
        let me = UserId("alice".into());
        assert_eq!(peer_of(&ConversationId("si_alice_zed".into()), &me), "zed");
        assert_eq!(peer_of(&ConversationId("si_bob_alice".into()), &me), "bob");
        assert_eq!(peer_of(&ConversationId("sg_1".into()), &me), "");
    }
}
