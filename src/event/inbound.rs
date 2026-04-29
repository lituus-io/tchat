use crate::types::{
    MemberRole, MembershipState, Message, MessageId, PlatformId, PresenceStatus, Reaction, Space,
    SpaceId, Timestamp, User, UserId,
};

/// Events produced by platform IO threads. Moved through channels to the main
/// thread. This enum is the platform abstraction boundary — all platforms
/// produce the same event types regardless of their wire protocol.
pub enum InboundEvent {
    // Connection lifecycle
    Connected {
        platform: PlatformId,
    },
    Disconnected {
        platform: PlatformId,
        reason: DisconnectReason,
    },
    Reconnecting {
        platform: PlatformId,
        attempt: u32,
    },

    // Initial sync
    WorldSync {
        platform: PlatformId,
        spaces: Vec<Space>,
        self_user: User,
    },

    // Space events
    SpaceUpdated {
        space: Space,
    },

    // Message events. The raw_* strings carry the wire-format IDs that
    // produced the InternedIds in `message`. They're populated by the BC
    // dispatch path and consumed by code (e.g. bots) that needs to make
    // outbound API calls referencing the same space/topic/message — without
    // cross-interner resolution.
    MessagePosted {
        message: Message,
        space_id_raw: String,
        topic_id_raw: Option<String>,
        message_id_raw: String,
    },
    MessageEdited {
        message: Message,
        space_id_raw: String,
        topic_id_raw: Option<String>,
        message_id_raw: String,
    },
    MessageDeleted {
        space_id: SpaceId,
        message_id: MessageId,
    },

    // Presence / typing
    TypingStarted {
        space_id: SpaceId,
        user_id: UserId,
        timestamp: Timestamp,
    },
    TypingStopped {
        space_id: SpaceId,
        user_id: UserId,
    },
    PresenceChanged {
        user_id: UserId,
        presence: PresenceStatus,
    },

    // Read state
    ReadStateUpdated {
        space_id: SpaceId,
        last_read: Timestamp,
        unread_count: u32,
    },

    // Reactions
    ReactionUpdated {
        space_id: SpaceId,
        message_id: MessageId,
        reactions: Vec<Reaction>,
    },

    // History
    HistoryChunk {
        space_id: SpaceId,
        messages: Vec<Message>,
        has_more: bool,
    },

    // User profile resolution
    UsersResolved {
        users: Vec<User>,
    },

    // Membership change in a space (join/leave/invite/role change).
    // `state` and `role` reflect the new membership; if `state` is `Left`,
    // the user has been removed and should be dropped from the space's
    // membership list.
    MembershipChanged {
        space_id: SpaceId,
        user_id: UserId,
        state: MembershipState,
        role: MemberRole,
    },
}

#[derive(Debug)]
pub enum DisconnectReason {
    AuthFailed(String),
    SessionExpired,
    MaxRetriesExceeded,
    ServerError(String),
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbound_event_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<InboundEvent>();
    }
}
