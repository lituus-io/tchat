use crate::types::{Emoji, MessageId, SpaceId, Timestamp, TopicId, UserId};

/// Commands the main thread sends to platform IO threads.
/// Moved through channels — no shared state.
pub enum OutboundCommand {
    // ─── Message operations ───
    SendMessage {
        space_id: SpaceId,
        text: String,
        thread_id: Option<TopicId>,
    },
    EditMessage {
        space_id: SpaceId,
        message_id: MessageId,
        new_text: String,
    },
    DeleteMessage {
        space_id: SpaceId,
        message_id: MessageId,
    },
    SetTyping {
        space_id: SpaceId,
        typing: bool,
    },
    MarkRead {
        space_id: SpaceId,
        up_to: MessageId,
    },
    AddReaction {
        space_id: SpaceId,
        message_id: MessageId,
        emoji: Emoji,
    },
    RemoveReaction {
        space_id: SpaceId,
        message_id: MessageId,
        emoji: Emoji,
    },
    FetchHistory {
        space_id: SpaceId,
        before: Timestamp,
        count: u32,
    },
    /// Start a new top-level thread in a threaded space
    CreateTopic {
        space_id: SpaceId,
        text: String,
    },

    // ─── User operations ───
    /// Resolve user IDs to display names/profiles
    GetUsers {
        user_ids: Vec<UserId>,
    },
    /// Get presence (online/offline) for users
    GetUserPresence {
        user_ids: Vec<UserId>,
    },
    /// Refresh self user info
    RefreshSelf,

    // ─── Space management ───
    /// Create a new room/space
    CreateRoom {
        name: String,
        invite_user_ids: Vec<UserId>,
    },
    /// Create a new DM with one or more users
    CreateDm {
        user_ids: Vec<UserId>,
    },
    /// Add members to a space
    AddMembers {
        space_id: SpaceId,
        user_ids: Vec<UserId>,
    },
    /// Remove members from a space (or yourself)
    RemoveMembers {
        space_id: SpaceId,
        user_ids: Vec<UserId>,
    },
    /// Rename a space
    RenameSpace {
        space_id: SpaceId,
        new_name: String,
    },
    /// Hide/archive a space without leaving
    HideSpace {
        space_id: SpaceId,
        hide: bool,
    },
    /// List members of a space
    ListSpaceMembers {
        space_id: SpaceId,
    },

    // ─── User settings ───
    /// Set DND duration (microseconds from now). 0 = disable DND.
    SetDndDuration {
        duration_usec: u64,
    },
    /// Set custom status text and emoji
    SetCustomStatus {
        text: String,
        emoji: Option<Emoji>,
        expiry_usec: Option<i64>,
    },
    /// Block a user or space
    BlockEntity {
        user_id: Option<UserId>,
        space_id: Option<SpaceId>,
        blocked: bool,
        reported: bool,
    },
    /// Enable/disable presence sharing
    SetPresenceShared {
        shared: bool,
    },

    // ─── Advanced features ───
    /// Create a custom emoji (shortcode only; upload not implemented)
    CreateCustomEmoji {
        shortcode: String,
    },
    /// Autocomplete slash commands in a space
    AutocompleteSlashCommands {
        query: String,
        space_id: SpaceId,
        max_results: u32,
    },
    /// Create a video call / Meet link in a space
    CreateVideoCall {
        space_id: SpaceId,
    },

    Disconnect,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbound_command_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<OutboundCommand>();
    }
}
