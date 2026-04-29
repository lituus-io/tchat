use std::collections::{BTreeMap, HashMap};

use crate::event::InboundEvent;
use crate::types::{
    IdInterner, Message, MessageId, PlatformId, Space, SpaceId, Timestamp, User, UserId,
};

/// GAT-based trait for read access to the store.
///
/// The associated iterator types borrow from `&'a self`, enabling zero-copy
/// rendering: the TUI borrows message text directly from the store without
/// allocation.
///
/// Note: `BTreeMap::Range` already produces `&'a` references naturally via
/// the standard `Iterator` trait. This GAT trait adds value as an abstraction
/// boundary for testing and future alternate implementations (e.g. SQLite).
pub trait StoreRead {
    type MsgIter<'a>: Iterator<Item = &'a Message>
    where
        Self: 'a;
    type SpaceIter<'a>: Iterator<Item = &'a Space>
    where
        Self: 'a;

    fn messages_in_space<'a>(&'a self, space: SpaceId) -> Self::MsgIter<'a>;
    fn spaces_sorted<'a>(&'a self) -> Self::SpaceIter<'a>;
    fn user(&self, id: UserId) -> Option<&User>;
    fn space(&self, id: SpaceId) -> Option<&Space>;
    fn self_user(&self, platform: PlatformId) -> Option<UserId>;
}

/// Owns all application state: messages, spaces, users.
///
/// All data enters via [`ingest`](Store::ingest) (move semantics, no cloning).
/// All reads are borrows via [`StoreRead`].
pub struct Store {
    spaces: HashMap<SpaceId, Space>,
    /// Messages keyed by (space, timestamp, message_id) for time-ordered
    /// range queries per space. All key components are `Copy` (interned).
    messages: BTreeMap<(SpaceId, Timestamp, MessageId), Message>,
    users: HashMap<UserId, User>,
    pub interner: IdInterner,
    self_users: HashMap<PlatformId, UserId>,
    /// Cached space ordering by last activity (rebuilt on space updates).
    space_order: Vec<SpaceId>,
}

impl Store {
    pub fn new() -> Self {
        Self {
            spaces: HashMap::new(),
            messages: BTreeMap::new(),
            users: HashMap::new(),
            interner: IdInterner::new(),
            self_users: HashMap::new(),
            space_order: Vec::new(),
        }
    }

    /// Ingest an event, taking ownership of all data within it.
    pub fn ingest(&mut self, event: InboundEvent) {
        match event {
            InboundEvent::Connected { .. } | InboundEvent::Reconnecting { .. } => {}

            InboundEvent::Disconnected { .. } => {}

            InboundEvent::WorldSync {
                spaces,
                self_user,
                platform,
                ..
            } => {
                let self_id = self_user.id;
                self.users.insert(self_user.id, self_user);
                self.self_users.insert(platform, self_id);
                for space in spaces {
                    self.spaces.insert(space.id, space);
                }
                self.rebuild_space_order();
            }

            InboundEvent::SpaceUpdated { space } => {
                self.spaces.insert(space.id, space);
                self.rebuild_space_order();
            }

            InboundEvent::MessagePosted { message, .. } => {
                let key = (message.space_id, message.timestamp, message.id);
                // Update space's last activity
                if let Some(space) = self.spaces.get_mut(&message.space_id) {
                    if message.timestamp > space.last_activity {
                        space.last_activity = message.timestamp;
                        space.sort_timestamp = message.timestamp;
                    }
                }
                self.messages.insert(key, message);
                self.rebuild_space_order();
            }

            InboundEvent::MessageEdited { message, .. } => {
                // Remove old entry (timestamp may differ), insert new
                let space = message.space_id;
                self.messages
                    .retain(|k, _| !(k.0 == space && k.2 == message.id));
                let key = (message.space_id, message.timestamp, message.id);
                self.messages.insert(key, message);
            }

            InboundEvent::MessageDeleted {
                space_id,
                message_id,
            } => {
                self.messages
                    .retain(|k, _| !(k.0 == space_id && k.2 == message_id));
            }

            InboundEvent::TypingStarted {
                space_id, user_id, ..
            } => {
                if let Some(space) = self.spaces.get_mut(&space_id) {
                    if !space.typing_users.contains(&user_id) {
                        space.typing_users.push(user_id);
                    }
                }
            }

            InboundEvent::TypingStopped { space_id, user_id } => {
                if let Some(space) = self.spaces.get_mut(&space_id) {
                    space.typing_users.retain(|u| *u != user_id);
                }
            }

            InboundEvent::PresenceChanged {
                user_id, presence, ..
            } => {
                if let Some(user) = self.users.get_mut(&user_id) {
                    user.presence = presence;
                }
            }

            InboundEvent::ReadStateUpdated {
                space_id,
                unread_count,
                ..
            } => {
                if let Some(space) = self.spaces.get_mut(&space_id) {
                    space.unread_count = unread_count;
                }
            }

            InboundEvent::ReactionUpdated {
                space_id,
                message_id,
                reactions,
            } => {
                // Find the message and replace its reactions
                for (key, msg) in self.messages.iter_mut() {
                    if key.0 == space_id && key.2 == message_id {
                        msg.reactions = reactions;
                        break;
                    }
                }
            }

            InboundEvent::HistoryChunk {
                space_id, messages, ..
            } => {
                for message in messages {
                    let key = (space_id, message.timestamp, message.id);
                    self.messages.entry(key).or_insert(message);
                }
            }

            InboundEvent::UsersResolved { users } => {
                for user in users {
                    self.users.insert(user.id, user);
                }
            }

            InboundEvent::MembershipChanged { .. } => {
                // Membership state is currently observed via re-fetch
                // (ListMembers/GetMembers). The event itself is a signal
                // that the caller should refresh; the TUI consumes this
                // for display and we have no per-space membership index
                // in the store yet.
            }
        }
    }

    fn rebuild_space_order(&mut self) {
        self.space_order.clear();
        self.space_order.extend(self.spaces.keys().copied());
        // Sort by last activity descending (most recent first)
        let spaces = &self.spaces;
        self.space_order.sort_by(|a, b| {
            let ts_a = spaces
                .get(a)
                .map(|s| s.sort_timestamp)
                .unwrap_or(Timestamp::ZERO);
            let ts_b = spaces
                .get(b)
                .map(|s| s.sort_timestamp)
                .unwrap_or(Timestamp::ZERO);
            ts_b.cmp(&ts_a)
        });
    }
}

/// Iterator over messages in a space, yielding `&Message` in time order.
pub struct SpaceMessageIter<'a> {
    inner: std::collections::btree_map::Range<'a, (SpaceId, Timestamp, MessageId), Message>,
}

impl<'a> Iterator for SpaceMessageIter<'a> {
    type Item = &'a Message;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(_, msg)| msg)
    }
}

/// Iterator over spaces in activity order.
pub struct SortedSpaceIter<'a> {
    store: &'a Store,
    index: usize,
}

impl<'a> Iterator for SortedSpaceIter<'a> {
    type Item = &'a Space;

    fn next(&mut self) -> Option<Self::Item> {
        let id = self.store.space_order.get(self.index)?;
        self.index += 1;
        self.store.spaces.get(id)
    }
}

impl StoreRead for Store {
    type MsgIter<'a> = SpaceMessageIter<'a>;
    type SpaceIter<'a> = SortedSpaceIter<'a>;

    fn messages_in_space<'a>(&'a self, space: SpaceId) -> Self::MsgIter<'a> {
        let start = (space, Timestamp::ZERO, MessageId::MIN);
        let end = (space, Timestamp::MAX, MessageId::MAX);
        SpaceMessageIter {
            inner: self.messages.range(start..=end),
        }
    }

    fn spaces_sorted<'a>(&'a self) -> Self::SpaceIter<'a> {
        SortedSpaceIter {
            store: self,
            index: 0,
        }
    }

    fn user(&self, id: UserId) -> Option<&User> {
        self.users.get(&id)
    }

    fn space(&self, id: SpaceId) -> Option<&Space> {
        self.spaces.get(&id)
    }

    fn self_user(&self, platform: PlatformId) -> Option<UserId> {
        self.self_users.get(&platform).copied()
    }
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MessageType, PresenceStatus, SpaceKind};

    fn make_store_with_interner() -> Store {
        Store::new()
    }

    fn make_space(store: &mut Store, name: &str) -> SpaceId {
        let id = store.interner.intern(name);
        let space_id = SpaceId {
            platform: PlatformId::GoogleChat,
            id,
        };
        let space = Space {
            id: space_id,
            name: name.to_owned(),
            kind: SpaceKind::Room,
            platform: PlatformId::GoogleChat,
            unread_count: 0,
            last_activity: Timestamp::ZERO,
            sort_timestamp: Timestamp::ZERO,
            typing_users: Vec::new(),
        };
        store.ingest(InboundEvent::SpaceUpdated { space });
        space_id
    }

    fn make_message(store: &mut Store, space: SpaceId, text: &str, ts: u64) -> Message {
        let msg_id = MessageId(store.interner.intern(&format!("msg_{ts}")));
        let sender_id = store.interner.intern("user_1");
        Message {
            id: msg_id,
            space_id: space,
            sender: UserId {
                platform: PlatformId::GoogleChat,
                id: sender_id,
            },
            timestamp: Timestamp(ts),
            edit_timestamp: None,
            text: text.to_owned(),
            annotations: Vec::new(),
            reactions: Vec::new(),
            thread_id: None,
            message_type: MessageType::User,
            platform: PlatformId::GoogleChat,
        }
    }

    #[test]
    fn ingest_message_posted_stores_message() {
        let mut store = make_store_with_interner();
        let space = make_space(&mut store, "space_1");
        let msg = make_message(&mut store, space, "hello", 1000);
        let msg_id = msg.id;
        store.ingest(InboundEvent::MessagePosted {
            message: msg,
            space_id_raw: String::new(),
            topic_id_raw: None,
            message_id_raw: String::new(),
        });

        let msgs: Vec<_> = store.messages_in_space(space).collect();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, msg_id);
        assert_eq!(msgs[0].text, "hello");
    }

    #[test]
    fn ingest_message_deleted_removes_message() {
        let mut store = make_store_with_interner();
        let space = make_space(&mut store, "space_1");
        let msg = make_message(&mut store, space, "delete me", 1000);
        let msg_id = msg.id;
        store.ingest(InboundEvent::MessagePosted {
            message: msg,
            space_id_raw: String::new(),
            topic_id_raw: None,
            message_id_raw: String::new(),
        });
        assert_eq!(store.messages_in_space(space).count(), 1);

        store.ingest(InboundEvent::MessageDeleted {
            space_id: space,
            message_id: msg_id,
        });
        assert_eq!(store.messages_in_space(space).count(), 0);
    }

    #[test]
    fn messages_in_space_returns_time_ordered() {
        let mut store = make_store_with_interner();
        let space = make_space(&mut store, "space_1");

        // Insert out of order
        let msg3 = make_message(&mut store, space, "third", 3000);
        let msg1 = make_message(&mut store, space, "first", 1000);
        let msg2 = make_message(&mut store, space, "second", 2000);

        store.ingest(InboundEvent::MessagePosted {
            message: msg3,
            space_id_raw: String::new(),
            topic_id_raw: None,
            message_id_raw: String::new(),
        });
        store.ingest(InboundEvent::MessagePosted {
            message: msg1,
            space_id_raw: String::new(),
            topic_id_raw: None,
            message_id_raw: String::new(),
        });
        store.ingest(InboundEvent::MessagePosted {
            message: msg2,
            space_id_raw: String::new(),
            topic_id_raw: None,
            message_id_raw: String::new(),
        });

        let texts: Vec<&str> = store
            .messages_in_space(space)
            .map(|m| m.text.as_str())
            .collect();
        assert_eq!(texts, vec!["first", "second", "third"]);
    }

    #[test]
    fn messages_in_space_empty_for_unknown_space() {
        let mut store = make_store_with_interner();
        let id = store.interner.intern("nonexistent");
        let space = SpaceId {
            platform: PlatformId::GoogleChat,
            id,
        };
        assert_eq!(store.messages_in_space(space).count(), 0);
    }

    #[test]
    fn messages_in_space_does_not_leak_across_spaces() {
        let mut store = make_store_with_interner();
        let space_a = make_space(&mut store, "space_a");
        let space_b = make_space(&mut store, "space_b");

        let msg_a = make_message(&mut store, space_a, "in A", 1000);
        let msg_b = make_message(&mut store, space_b, "in B", 2000);

        store.ingest(InboundEvent::MessagePosted {
            message: msg_a,
            space_id_raw: String::new(),
            topic_id_raw: None,
            message_id_raw: String::new(),
        });
        store.ingest(InboundEvent::MessagePosted {
            message: msg_b,
            space_id_raw: String::new(),
            topic_id_raw: None,
            message_id_raw: String::new(),
        });

        let a_msgs: Vec<_> = store.messages_in_space(space_a).collect();
        let b_msgs: Vec<_> = store.messages_in_space(space_b).collect();

        assert_eq!(a_msgs.len(), 1);
        assert_eq!(a_msgs[0].text, "in A");
        assert_eq!(b_msgs.len(), 1);
        assert_eq!(b_msgs[0].text, "in B");
    }

    #[test]
    fn spaces_sorted_by_last_activity_descending() {
        let mut store = make_store_with_interner();
        let _old = make_space(&mut store, "old_space");
        let _new = make_space(&mut store, "new_space");

        // Update timestamps via space updates
        if let Some(s) = store.spaces.get_mut(&_old) {
            s.sort_timestamp = Timestamp(100);
        }
        if let Some(s) = store.spaces.get_mut(&_new) {
            s.sort_timestamp = Timestamp(200);
        }
        store.rebuild_space_order();

        let names: Vec<&str> = store.spaces_sorted().map(|s| s.name.as_str()).collect();
        assert_eq!(names[0], "new_space");
        assert_eq!(names[1], "old_space");
    }

    #[test]
    fn ingest_typing_started_adds_user() {
        let mut store = make_store_with_interner();
        let space = make_space(&mut store, "space_1");
        let uid = store.interner.intern("typer");
        let user_id = UserId {
            platform: PlatformId::GoogleChat,
            id: uid,
        };

        store.ingest(InboundEvent::TypingStarted {
            space_id: space,
            user_id,
            timestamp: Timestamp(100),
        });

        let s = store.space(space).unwrap();
        assert!(s.typing_users.contains(&user_id));
    }

    #[test]
    fn ingest_typing_stopped_removes_user() {
        let mut store = make_store_with_interner();
        let space = make_space(&mut store, "space_1");
        let uid = store.interner.intern("typer");
        let user_id = UserId {
            platform: PlatformId::GoogleChat,
            id: uid,
        };

        store.ingest(InboundEvent::TypingStarted {
            space_id: space,
            user_id,
            timestamp: Timestamp(100),
        });
        store.ingest(InboundEvent::TypingStopped {
            space_id: space,
            user_id,
        });

        let s = store.space(space).unwrap();
        assert!(!s.typing_users.contains(&user_id));
    }

    #[test]
    fn ingest_read_state_updates_unread_count() {
        let mut store = make_store_with_interner();
        let space = make_space(&mut store, "space_1");

        store.ingest(InboundEvent::ReadStateUpdated {
            space_id: space,
            last_read: Timestamp(100),
            unread_count: 5,
        });

        assert_eq!(store.space(space).unwrap().unread_count, 5);
    }

    #[test]
    fn ingest_world_sync_populates_spaces_and_users() {
        let mut store = make_store_with_interner();
        let sid = store.interner.intern("space_sync");
        let uid = store.interner.intern("self_user");

        let space_id = SpaceId {
            platform: PlatformId::GoogleChat,
            id: sid,
        };
        let user_id = UserId {
            platform: PlatformId::GoogleChat,
            id: uid,
        };

        store.ingest(InboundEvent::WorldSync {
            platform: PlatformId::GoogleChat,
            spaces: vec![Space {
                id: space_id,
                name: "synced".to_owned(),
                kind: SpaceKind::Room,
                platform: PlatformId::GoogleChat,
                unread_count: 0,
                last_activity: Timestamp::ZERO,
                sort_timestamp: Timestamp::ZERO,
                typing_users: Vec::new(),
            }],
            self_user: crate::types::User {
                id: user_id,
                display_name: "Me".to_owned(),
                email: None,
                avatar_url: None,
                presence: PresenceStatus::Active,
                is_bot: false,
            },
        });

        assert!(store.space(space_id).is_some());
        assert!(store.user(user_id).is_some());
        assert_eq!(store.self_user(PlatformId::GoogleChat), Some(user_id));
    }

    #[test]
    fn ingest_presence_changed_updates_user() {
        let mut store = make_store_with_interner();
        let uid = store.interner.intern("presence_user");
        let user_id = UserId {
            platform: PlatformId::GoogleChat,
            id: uid,
        };
        let user = crate::types::User {
            id: user_id,
            display_name: "Alice".to_owned(),
            email: None,
            avatar_url: None,
            presence: PresenceStatus::Active,
            is_bot: false,
        };
        store.users.insert(user_id, user);

        store.ingest(InboundEvent::PresenceChanged {
            user_id,
            presence: PresenceStatus::Dnd,
        });

        assert_eq!(store.user(user_id).unwrap().presence, PresenceStatus::Dnd);
    }

    #[test]
    fn ingest_reaction_updated_replaces_reactions() {
        use crate::types::{Emoji, Reaction};

        let mut store = make_store_with_interner();
        let space = make_space(&mut store, "space_1");
        let msg = make_message(&mut store, space, "react to me", 1000);
        let msg_id = msg.id;
        store.ingest(InboundEvent::MessagePosted {
            message: msg,
            space_id_raw: String::new(),
            topic_id_raw: None,
            message_id_raw: String::new(),
        });

        let reactions = vec![Reaction {
            emoji: Emoji::Unicode("👍".to_owned()),
            count: 3,
            includes_self: true,
        }];

        store.ingest(InboundEvent::ReactionUpdated {
            space_id: space,
            message_id: msg_id,
            reactions,
        });

        let msgs: Vec<_> = store.messages_in_space(space).collect();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].reactions.len(), 1);
        assert_eq!(msgs[0].reactions[0].count, 3);
        assert!(msgs[0].reactions[0].includes_self);
    }

    #[test]
    fn ingest_history_chunk_adds_messages() {
        let mut store = make_store_with_interner();
        let space = make_space(&mut store, "space_1");

        let msg1 = make_message(&mut store, space, "old msg", 500);
        let msg2 = make_message(&mut store, space, "older msg", 400);

        store.ingest(InboundEvent::HistoryChunk {
            space_id: space,
            messages: vec![msg1, msg2],
            has_more: false,
        });

        let texts: Vec<&str> = store
            .messages_in_space(space)
            .map(|m| m.text.as_str())
            .collect();
        assert_eq!(texts, vec!["older msg", "old msg"]);
    }

    #[test]
    fn ingest_history_chunk_does_not_overwrite_existing() {
        let mut store = make_store_with_interner();
        let space = make_space(&mut store, "space_1");

        let msg = make_message(&mut store, space, "live version", 1000);
        let msg_id = msg.id;
        store.ingest(InboundEvent::MessagePosted {
            message: msg,
            space_id_raw: String::new(),
            topic_id_raw: None,
            message_id_raw: String::new(),
        });

        // History chunk carries a stale copy of the same message
        let stale = Message {
            id: msg_id,
            space_id: space,
            sender: UserId {
                platform: PlatformId::GoogleChat,
                id: store.interner.intern("user_1"),
            },
            timestamp: Timestamp(1000),
            edit_timestamp: None,
            text: "stale version".to_owned(),
            annotations: Vec::new(),
            reactions: Vec::new(),
            thread_id: None,
            message_type: MessageType::User,
            platform: PlatformId::GoogleChat,
        };

        store.ingest(InboundEvent::HistoryChunk {
            space_id: space,
            messages: vec![stale],
            has_more: true,
        });

        let msgs: Vec<_> = store.messages_in_space(space).collect();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "live version");
    }

    #[test]
    fn ingest_space_updated_replaces_space_and_rebuilds_order() {
        let mut store = make_store_with_interner();
        let space_a = make_space(&mut store, "alpha");
        let _space_b = make_space(&mut store, "beta");

        // alpha has older activity, beta has newer
        let updated_alpha = Space {
            id: space_a,
            name: "alpha_renamed".to_owned(),
            kind: SpaceKind::Room,
            platform: PlatformId::GoogleChat,
            unread_count: 2,
            last_activity: Timestamp(300),
            sort_timestamp: Timestamp(300),
            typing_users: Vec::new(),
        };
        store.ingest(InboundEvent::SpaceUpdated {
            space: updated_alpha,
        });

        // alpha (300) should now sort before beta (0)
        let s = store.space(space_a).unwrap();
        assert_eq!(s.name, "alpha_renamed");
        assert_eq!(s.unread_count, 2);

        let names: Vec<&str> = store.spaces_sorted().map(|s| s.name.as_str()).collect();
        assert_eq!(names[0], "alpha_renamed");
        assert_eq!(names[1], "beta");
    }

    #[test]
    fn ingest_message_edited_replaces_message() {
        let mut store = make_store_with_interner();
        let space = make_space(&mut store, "space_1");
        let msg = make_message(&mut store, space, "original", 1000);
        let msg_id = msg.id;
        store.ingest(InboundEvent::MessagePosted {
            message: msg,
            space_id_raw: String::new(),
            topic_id_raw: None,
            message_id_raw: String::new(),
        });

        let edited = Message {
            id: msg_id,
            space_id: space,
            sender: UserId {
                platform: PlatformId::GoogleChat,
                id: store.interner.intern("user_1"),
            },
            timestamp: Timestamp(1000),
            edit_timestamp: Some(Timestamp(2000)),
            text: "edited text".to_owned(),
            annotations: Vec::new(),
            reactions: Vec::new(),
            thread_id: None,
            message_type: MessageType::User,
            platform: PlatformId::GoogleChat,
        };
        store.ingest(InboundEvent::MessageEdited {
            message: edited,
            space_id_raw: String::new(),
            topic_id_raw: None,
            message_id_raw: String::new(),
        });

        let msgs: Vec<_> = store.messages_in_space(space).collect();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "edited text");
        assert_eq!(msgs[0].edit_timestamp, Some(Timestamp(2000)));
    }

    #[test]
    fn lending_iterator_borrows_from_store() {
        let mut store = make_store_with_interner();
        let space = make_space(&mut store, "space_1");
        let msg = make_message(&mut store, space, "borrowed", 1000);
        store.ingest(InboundEvent::MessagePosted {
            message: msg,
            space_id_raw: String::new(),
            topic_id_raw: None,
            message_id_raw: String::new(),
        });

        // This compiles only if the iterator's Item lifetime is tied to &store
        let text: &str = {
            let iter = store.messages_in_space(space);
            let first = iter.into_iter().next().unwrap();
            first.text.as_str()
        };
        assert_eq!(text, "borrowed");
    }
}
