use crate::types::ids::{MessageId, PlatformId, SpaceId, TopicId, UserId};
use crate::types::timestamp::Timestamp;

/// A chat message. Owned by the [`Store`](crate::store::Store) after ingestion.
///
/// All `String` fields are moved (never cloned) through the pipeline:
/// proto parse → event → channel → store.
pub struct Message {
    pub id: MessageId,
    pub space_id: SpaceId,
    pub sender: UserId,
    pub timestamp: Timestamp,
    pub edit_timestamp: Option<Timestamp>,
    pub text: String,
    pub annotations: Vec<Annotation>,
    pub reactions: Vec<Reaction>,
    pub thread_id: Option<TopicId>,
    pub message_type: MessageType,
    pub platform: PlatformId,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MessageType {
    User,
    System,
}

/// A text annotation (formatting, link, mention) with byte-offset span.
pub struct Annotation {
    pub kind: AnnotationKind,
    pub start: u32,
    pub length: u32,
}

pub enum AnnotationKind {
    Bold,
    Italic,
    Strikethrough,
    Code,
    CodeBlock,
    Link { url: String },
    UserMention { user_id: UserId },
}

/// A reaction on a message.
pub struct Reaction {
    pub emoji: Emoji,
    pub count: u32,
    pub includes_self: bool,
}

/// An emoji — either a standard Unicode emoji or a custom one.
pub enum Emoji {
    /// Standard Unicode emoji (e.g. "👍"). Stored inline, typically ≤ 16 bytes.
    Unicode(String),
    /// Custom emoji with platform-specific id and display shortcode.
    Custom { id: String, shortcode: String },
}
