//! Stable JSON shapes for the agent-facing HTTP surface.
//!
//! These types are the public boundary between tchat and any external
//! agent harness. They deliberately do NOT re-export internal Rust enum
//! names (e.g. `SpaceKind`) — instead they emit string discriminators
//! the harness can rely on across versions.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HealthResponse {
    /// "ok" — kept as String for round-trip via serde.
    pub status: String,
    /// "valid" | "expired"
    pub auth: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_user_id: Option<String>,
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpaceInfo {
    pub id: String,
    pub name: String,
    pub kind: SpaceKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum SpaceKind {
    Room,
    ThreadedRoom,
    DirectMessage,
    GroupDm,
    Channel,
    Unknown,
}

impl From<crate::types::SpaceKind> for SpaceKind {
    fn from(k: crate::types::SpaceKind) -> Self {
        match k {
            crate::types::SpaceKind::Room => SpaceKind::Room,
            crate::types::SpaceKind::ThreadedRoom => SpaceKind::ThreadedRoom,
            crate::types::SpaceKind::DirectMessage => SpaceKind::DirectMessage,
            crate::types::SpaceKind::GroupDm => SpaceKind::GroupDm,
            crate::types::SpaceKind::Channel => SpaceKind::Channel,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpacesResponse {
    pub spaces: Vec<SpaceInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AskRequest {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AskResponse {
    pub topic_id: String,
    pub message_id: String,
    pub space_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchResponse {
    pub query: String,
    pub threads: Vec<ThreadJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThreadJson {
    pub topic_id: String,
    pub messages: Vec<MessageJson>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessageJson {
    pub author_id: String,
    pub text: String,
    pub timestamp_usec: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReplyRequest {
    pub space_id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReplyResponse {
    pub message_id: String,
}

/// SSE event payload. The HTTP framing prepends `event: <kind>\n` and the
/// JSON serialization of this struct as the `data:` line.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventEnvelope {
    pub kind: EventKind,
    pub space_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp_usec: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    MessagePosted,
    MessageEdited,
    MessageDeleted,
    ReactionUpdated,
    TypingStarted,
    TypingStopped,
    SpaceUpdated,
    PresenceChanged,
    MembershipChanged,
    ReadStateUpdated,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::MessagePosted => "message_posted",
            EventKind::MessageEdited => "message_edited",
            EventKind::MessageDeleted => "message_deleted",
            EventKind::ReactionUpdated => "reaction_updated",
            EventKind::TypingStarted => "typing_started",
            EventKind::TypingStopped => "typing_stopped",
            EventKind::SpaceUpdated => "space_updated",
            EventKind::PresenceChanged => "presence_changed",
            EventKind::MembershipChanged => "membership_changed",
            EventKind::ReadStateUpdated => "read_state_updated",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "message_posted" => EventKind::MessagePosted,
            "message_edited" => EventKind::MessageEdited,
            "message_deleted" => EventKind::MessageDeleted,
            "reaction_updated" => EventKind::ReactionUpdated,
            "typing_started" => EventKind::TypingStarted,
            "typing_stopped" => EventKind::TypingStopped,
            "space_updated" => EventKind::SpaceUpdated,
            "presence_changed" => EventKind::PresenceChanged,
            "membership_changed" => EventKind::MembershipChanged,
            "read_state_updated" => EventKind::ReadStateUpdated,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(v: T) {
        let json = serde_json::to_string(&v).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(v, back);
    }

    #[test]
    fn space_info_roundtrip() {
        roundtrip(SpaceInfo {
            id: "AAQAJuwMi-4".into(),
            name: "test space".into(),
            kind: SpaceKind::ThreadedRoom,
        });
    }

    #[test]
    fn ask_response_roundtrip() {
        roundtrip(AskResponse {
            topic_id: "Gg4xRoNE7w".into(),
            message_id: "Gg4xRoNE7w".into(),
            space_id: "AAQAJuwMi-4".into(),
        });
    }

    #[test]
    fn search_response_roundtrip() {
        roundtrip(SearchResponse {
            query: "https_proxy".into(),
            threads: vec![ThreadJson {
                topic_id: "T1".into(),
                messages: vec![MessageJson {
                    author_id: "U1".into(),
                    text: "hello".into(),
                    timestamp_usec: 1770000000000000,
                }],
            }],
            continuation: Some("token".into()),
        });
    }

    #[test]
    fn search_response_omits_null_continuation() {
        let r = SearchResponse {
            query: "q".into(),
            threads: vec![],
            continuation: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("continuation"));
    }

    #[test]
    fn event_envelope_kind_serializes_snake_case() {
        let e = EventEnvelope {
            kind: EventKind::MessagePosted,
            space_id: "S".into(),
            topic_id: Some("T".into()),
            message_id: Some("M".into()),
            author_id: Some("U".into()),
            text: Some("hi".into()),
            timestamp_usec: Some(123),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"message_posted\""));
        roundtrip(e);
    }

    #[test]
    fn event_kind_as_str_round_trips_via_from_str() {
        for k in [
            EventKind::MessagePosted,
            EventKind::MessageEdited,
            EventKind::MessageDeleted,
            EventKind::ReactionUpdated,
            EventKind::TypingStarted,
            EventKind::TypingStopped,
            EventKind::SpaceUpdated,
            EventKind::PresenceChanged,
            EventKind::MembershipChanged,
            EventKind::ReadStateUpdated,
        ] {
            assert_eq!(EventKind::from_str(k.as_str()), Some(k));
        }
        assert_eq!(EventKind::from_str("nonexistent"), None);
    }

    #[test]
    fn ask_request_idempotency_key_optional() {
        let with: AskRequest =
            serde_json::from_str(r#"{"text":"hi","idempotency_key":"k"}"#).unwrap();
        assert_eq!(with.idempotency_key.as_deref(), Some("k"));
        let without: AskRequest = serde_json::from_str(r#"{"text":"hi"}"#).unwrap();
        assert!(without.idempotency_key.is_none());
    }

    #[test]
    fn space_kind_serializes_pascal_case() {
        let json = serde_json::to_string(&SpaceKind::ThreadedRoom).unwrap();
        assert_eq!(json, "\"ThreadedRoom\"");
    }
}
