use crate::types::ids::{PlatformId, SpaceId, UserId};
use crate::types::timestamp::Timestamp;

/// A chat space (room, DM, channel). Owned by the [`Store`](crate::store::Store).
pub struct Space {
    pub id: SpaceId,
    pub name: String,
    pub kind: SpaceKind,
    pub platform: PlatformId,
    pub unread_count: u32,
    pub last_activity: Timestamp,
    pub sort_timestamp: Timestamp,
    pub typing_users: Vec<UserId>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SpaceKind {
    DirectMessage,
    GroupDm,
    /// Google Chat flat room.
    Room,
    /// Google Chat threaded space.
    ThreadedRoom,
    /// Slack channel (future).
    Channel,
}

/// A user profile. Owned by the [`Store`](crate::store::Store).
pub struct User {
    pub id: UserId,
    pub display_name: String,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    pub presence: PresenceStatus,
    pub is_bot: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PresenceStatus {
    Active,
    Inactive,
    Dnd,
    Unknown,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MemberRole {
    Owner,
    Manager,
    Member,
    Invitee,
    Unknown,
}

/// Per-user membership state in a space. Mirrors the Google Chat
/// `MembershipState` proto enum, narrowed to states tchat acts on.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MembershipState {
    Joined,
    Invited,
    Left,
    Unknown,
}
