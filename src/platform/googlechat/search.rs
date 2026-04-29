//! Google Chat search via the batchexecute RPC framework.
//!
//! Backed by the batchexecute RPC framework (`/_/DynamiteWebUi/data/batchexecute`),
//! NOT the protobuf `/api/*` endpoints. Two RPCs are exposed:
//!
//! - `wxhTDd` — autocomplete / typeahead (per-keystroke; light payload)
//! - `SBNmJb` — full search submit (returns ranked space hits)
//!
//! See `tests/live_capture_search.rs` for the capture tool used to derive
//! the wire format. Auth uses the page-extracted `at` token in
//! `DirectSession::at_token` (see `direct::DirectSession::fetch_xsrf_token`).
//!
//! Wire format of `SBNmJb` payload (positional JSON array):
//!
//! ```text
//! [
//!   null, null, null,                  // 0,1,2: padding
//!   "<query>",                         // 3: search string
//!   null,                              // 4
//!   "<UUID>",                          // 5: search-session UUID
//!   [[],null,null,null,"<UUID>",null,0,
//!    [[[[[[1]]]]],[[[1]]]]],           // 6: options
//!   null,                              // 7
//!   [3],                               // 8: scope marker
//!   [97]                               // 9: page-size / limit
//! ]
//! ```
//!
//! Response (after `)]}'` strip and `wrb.fr/SBNmJb` frame extraction):
//!
//! ```text
//! [
//!   "<continuation_token>",            // 0
//!   <int>,                             // 1: result-cap (e.g. 20)
//!   null,                              // 2
//!   "<request_uuid>",                  // 3
//!   [ <hit>, <hit>, ... ]              // 4: hits — see SearchHit
//! ]
//! ```

use crate::error::AuthError;
use crate::platform::googlechat::direct::DirectSession;

/// One hit returned by `search_messages`. Currently surfaces the matching
/// space; deeper message-snippet extraction lives at higher hit-array
/// indices and can be added when needed.
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// Bare space ID (e.g. `"AAAAPptFat4"`), no `space/` prefix.
    pub space_id: String,
    /// Space display name (e.g. `"BI Layer Internal Team"`). Empty for
    /// unnamed DMs / group DMs.
    pub name: String,
    /// Group kind from the wire: 1 = DM, 2 = Room (best-effort guess —
    /// matches the `["space/ID", "ID", N]` triple at hit[0]).
    pub kind: i64,
}

/// One matching message inside a thread.
#[derive(Debug, Clone)]
pub struct MessageHit {
    /// Topic / thread ID this message belongs to.
    pub topic_id: String,
    /// User ID of the message author.
    pub author_id: String,
    /// Full message text body.
    pub text: String,
    /// Message timestamp in microseconds since UNIX epoch (0 if missing).
    pub timestamp_usec: u64,
}

/// A group of message hits that share the same topic / thread.
/// Threads in `SearchResults::threads` preserve the server's relevance
/// ranking — `threads[0]` is the most relevant thread for the query.
#[derive(Debug, Clone)]
pub struct ThreadHit {
    pub topic_id: String,
    pub messages: Vec<MessageHit>,
}

/// Search results from the `SBNmJb` RPC.
#[derive(Debug, Clone, Default)]
pub struct SearchResults {
    /// Space-level hits (1 per matching space when scoped, N when global).
    pub hits: Vec<SearchHit>,
    /// Message-level hits grouped by topic. Server-ranked.
    pub threads: Vec<ThreadHit>,
    /// Opaque cursor for paging. Pass back as `query_continuation` in a
    /// future request to fetch the next page (paging not yet implemented).
    pub continuation: Option<String>,
}

impl SearchResults {
    /// First `n` threads in server-ranked order.
    pub fn top_threads(&self, n: usize) -> &[ThreadHit] {
        let take = n.min(self.threads.len());
        &self.threads[..take]
    }
}

/// Run a global (cross-space) search via direct (`ureq`) batchexecute.
pub fn search_messages(session: &DirectSession, query: &str) -> Result<SearchResults, AuthError> {
    let payload = build_search_payload(query);
    let payload_str = serde_json::to_string(&payload)
        .map_err(|e| AuthError::SessionFetch(format!("encode payload: {e}")))?;
    let resp_str = session.batchexecute("SBNmJb", &payload_str)?;
    parse_search_response(&resp_str)
}

/// Run a search restricted to one space (server-side scope). The hits
/// returned are message-level matches inside that space rather than the
/// space-level digest produced by `search_messages`.
pub fn search_messages_in_space(
    session: &DirectSession,
    space_id: &str,
    group_type: i64,
    query: &str,
) -> Result<SearchResults, AuthError> {
    let payload = build_search_in_space_payload(query, space_id, group_type);
    let payload_str = serde_json::to_string(&payload)
        .map_err(|e| AuthError::SessionFetch(format!("encode payload: {e}")))?;
    let resp_str = session.batchexecute("SBNmJb", &payload_str)?;
    parse_search_response(&resp_str)
}

/// Build the SBNmJb request payload for a global (cross-space) query.
/// Exposed so callers using a non-DirectSession transport (e.g. Chrome
/// `tab.evaluate`) can build the same JSON Rust uses.
pub fn build_search_payload(query: &str) -> serde_json::Value {
    let uuid = generate_uuid();
    serde_json::json!([
        null,
        null,
        null,
        query,
        null,
        uuid,
        [[], null, null, null, uuid, null, 0, [[[[[[1]]]]], [[[1]]]]],
        null,
        [3],
        [97]
    ])
}

/// Build the SBNmJb request payload for a search restricted to a single
/// space. Derived from a captured browser submission via the
/// in-conversation search UI; the scope filter appears in two slots
/// inside the options struct (field 6).
///
/// `space_id` is the bare space ID (e.g. `"AAAAz6E4W_g"`), no `space/`
/// prefix; `group_type` is 2 for Rooms and 1 for DMs (matches the
/// triple's third element in `[\"space/ID\", \"ID\", N]`).
pub fn build_search_in_space_payload(
    query: &str,
    space_id: &str,
    group_type: i64,
) -> serde_json::Value {
    let uuid = generate_uuid();
    let group_path = format!("space/{space_id}");
    serde_json::json!([
        null,
        null,
        null,
        query,
        null,
        uuid,
        [
            [null, [group_path, space_id, group_type]],
            null,
            null,
            null,
            uuid,
            null,
            0,
            [null, null, null, [[[[space_id]], 1]]]
        ],
        null,
        [3],
        [257]
    ])
}

/// Parse a complete batchexecute response (the raw text from the HTTP
/// body, with the `)]}'` XSSI prefix and outer wrb.fr framing) into
/// typed results. Use this when the HTTP transport is Chrome (or any
/// non-DirectSession path).
pub fn parse_batchexecute_response(body: &str, rpc_id: &str) -> Result<SearchResults, AuthError> {
    let json_part = body.trim_start_matches(")]}'").trim();
    let outer: serde_json::Value = serde_json::from_str(json_part)
        .map_err(|e| AuthError::SessionFetch(format!("parse outer: {e}")))?;
    let payload = outer
        .as_array()
        .and_then(|frames| {
            frames.iter().find(|f| {
                f.get(0).and_then(|v| v.as_str()) == Some("wrb.fr")
                    && f.get(1).and_then(|v| v.as_str()) == Some(rpc_id)
            })
        })
        .and_then(|f| f.get(2).and_then(|v| v.as_str()))
        .ok_or_else(|| AuthError::SessionFetch(format!("no wrb.fr/{rpc_id} frame in response")))?;
    parse_search_response(payload)
}

/// Parse the inner JSON-string payload of an `SBNmJb` response into
/// typed hits. Failure-tolerant: malformed hits are skipped rather than
/// failing the whole call.
///
/// Top-level shape (decoded from captured response frames):
/// ```text
///   [continuation_token, total_or_cap, ?, request_uuid,
///    space_hits[],                 // index 4
///    ?, query_echo, ?,             // indices 5-7
///    ?,                            // index 8
///    message_hits[]]               // index 9: only present in scoped responses
/// ```
fn parse_search_response(payload_str: &str) -> Result<SearchResults, AuthError> {
    let payload: serde_json::Value = serde_json::from_str(payload_str)
        .map_err(|e| AuthError::SessionFetch(format!("parse SBNmJb payload: {e}")))?;
    let arr = payload
        .as_array()
        .ok_or_else(|| AuthError::SessionFetch("SBNmJb payload not array".into()))?;

    let continuation = arr
        .first()
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned());

    let hits: Vec<SearchHit> = arr
        .get(4)
        .and_then(|v| v.as_array())
        .map(|hs| hs.iter().filter_map(parse_hit).collect())
        .unwrap_or_default();

    // Message-level hits live at index 9 in the in-space response shape;
    // global responses don't include them. Server-ranked.
    let raw_messages: Vec<MessageHit> = arr
        .get(9)
        .and_then(|v| v.as_array())
        .map(|ms| ms.iter().filter_map(parse_message_hit).collect())
        .unwrap_or_default();

    let threads = group_into_threads(raw_messages);

    Ok(SearchResults {
        hits,
        threads,
        continuation,
    })
}

/// One message-level hit (from `payload[9]`) is a 2-element wrapper:
///   [hit_data, kind]
/// where `hit_data` carries 48+ positional fields. The ones we surface:
///   hit_data[4][0] → author user ID
///   hit_data[5]    → full message text body
///   hit_data[11]   → timestamp in microseconds (string)
///   hit_data[17][0]→ topic_id (thread ID)
fn parse_message_hit(hit: &serde_json::Value) -> Option<MessageHit> {
    let outer = hit.as_array()?;
    let data = outer.first()?.as_array()?;
    let topic_id = data
        .get(17)
        .and_then(|v| v.as_array())
        .and_then(|t| t.first())
        .and_then(|v| v.as_str())?
        .to_owned();
    let author_id = data
        .get(4)
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let text = data
        .get(5)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let timestamp_usec = data
        .get(11)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    if topic_id.is_empty() {
        return None;
    }
    Some(MessageHit {
        topic_id,
        author_id,
        text,
        timestamp_usec,
    })
}

/// Group message hits by `topic_id`, preserving server-ranking order.
/// First-seen topic_id wins the slot; later duplicates append to that
/// thread's `messages`.
fn group_into_threads(messages: Vec<MessageHit>) -> Vec<ThreadHit> {
    let mut order: Vec<String> = Vec::new();
    let mut by_topic: std::collections::HashMap<String, Vec<MessageHit>> =
        std::collections::HashMap::new();
    for m in messages {
        let key = m.topic_id.clone();
        if !by_topic.contains_key(&key) {
            order.push(key.clone());
        }
        by_topic.entry(key).or_default().push(m);
    }
    order
        .into_iter()
        .map(|topic_id| ThreadHit {
            messages: by_topic.remove(&topic_id).unwrap_or_default(),
            topic_id,
        })
        .collect()
}

/// One hit looks like:
///   [["space/ID","ID",kind], null, "Space Name", ...]
fn parse_hit(hit: &serde_json::Value) -> Option<SearchHit> {
    let arr = hit.as_array()?;
    let group = arr.first()?.as_array()?;
    let space_id = group.get(1)?.as_str()?.to_owned();
    let kind = group.get(2)?.as_i64().unwrap_or(0);
    let name = arr.get(2).and_then(|v| v.as_str()).unwrap_or("").to_owned();
    if space_id.is_empty() {
        return None;
    }
    Some(SearchHit {
        space_id,
        name,
        kind,
    })
}

/// Generate a v4-shaped UUID without pulling in the `uuid` crate. The
/// server only checks shape (8-4-4-4-12 hex), not RFC 4122 compliance.
fn generate_uuid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // Mix the nanos with a per-process counter for some intra-process
    // entropy without crate-level RNG.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let c = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let a = (nanos as u64) ^ c.wrapping_mul(0x9E3779B97F4A7C15);
    let b = (nanos >> 64) as u64 ^ c.rotate_left(13);
    format!(
        "{:08X}-{:04X}-{:04X}-{:04X}-{:012X}",
        (a & 0xFFFFFFFF) as u32,
        ((a >> 32) & 0xFFFF) as u16,
        ((a >> 48) & 0xFFFF) as u16,
        (b & 0xFFFF) as u16,
        ((b >> 16) & 0xFFFFFFFFFFFF) as u64,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_shape_is_8_4_4_4_12_hex() {
        let id = generate_uuid();
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 4);
        assert_eq!(parts[3].len(), 4);
        assert_eq!(parts[4].len(), 12);
        assert!(id.chars().all(|c| c == '-' || c.is_ascii_hexdigit()));
    }

    #[test]
    fn uuid_is_unique_across_calls() {
        let a = generate_uuid();
        let b = generate_uuid();
        assert_ne!(a, b);
    }

    #[test]
    fn parse_search_response_extracts_hits() {
        // Synthetic minimal SBNmJb payload — captured shape with two hits.
        let payload = r#"["TOKEN",20,null,"REQ-UUID",[
            [["space/AAAAPptFat4","AAAAPptFat4",2],null,"BI Layer Internal Team"],
            [["space/AAAA2kPVvto","AAAA2kPVvto",2],null,"D&A Community"],
            [["space/AAQAF0fV-io","AAQAF0fV-io",1],null,""]
        ]]"#;
        let r = parse_search_response(payload).expect("parse ok");
        assert_eq!(r.hits.len(), 3);
        assert_eq!(r.hits[0].space_id, "AAAAPptFat4");
        assert_eq!(r.hits[0].name, "BI Layer Internal Team");
        assert_eq!(r.hits[0].kind, 2);
        assert_eq!(r.hits[2].kind, 1);
        assert_eq!(r.continuation.as_deref(), Some("TOKEN"));
    }

    #[test]
    fn parse_search_response_handles_empty_hits() {
        let payload = r#"[null,20,null,"REQ-UUID",[]]"#;
        let r = parse_search_response(payload).expect("parse ok");
        assert!(r.hits.is_empty());
        assert!(r.continuation.is_none());
    }

    #[test]
    fn parse_search_response_skips_malformed_hits() {
        let payload = r#"[null,20,null,"REQ-UUID",[
            [["space/OK","OK",2],null,"Good"],
            "garbage",
            null,
            [["space/OK2","OK2",2],null,"Good2"]
        ]]"#;
        let r = parse_search_response(payload).expect("parse ok");
        assert_eq!(r.hits.len(), 2);
        assert_eq!(r.hits[0].space_id, "OK");
        assert_eq!(r.hits[1].space_id, "OK2");
    }

    #[test]
    fn parse_search_response_rejects_non_array() {
        assert!(parse_search_response("\"not an array\"").is_err());
    }

    #[test]
    fn build_search_in_space_payload_embeds_space_id_in_both_slots() {
        let p = build_search_in_space_payload("hello", "AAAA2kPVvto", 2);
        let s = serde_json::to_string(&p).unwrap();
        // The space ID must appear in:
        //   - the path-prefixed group triple at field 6[0][1]
        //   - the bare-id filter array at field 6[7]
        assert!(s.contains("\"space/AAAA2kPVvto\""));
        assert!(s.contains("\"AAAA2kPVvto\""));
        // The page-size marker for in-space search.
        assert!(s.ends_with(",[3],[257]]"));
        // And the global empty-filter slot must NOT remain.
        assert!(!s.contains("[[],null,null,null"));
    }

    #[test]
    fn parse_message_hit_extracts_topic_text_author_timestamp() {
        // Structure: outer = [hit_data, kind]; hit_data has positional fields.
        let hit_json = serde_json::json!([
            [
                null,
                "",
                ["space/X", "X", 2],
                null,
                [
                    "AUTHOR_ID",
                    "",
                    "",
                    "",
                    null,
                    null,
                    null,
                    1,
                    null,
                    ["AUTHOR_ID", "human/AUTHOR_ID", 0]
                ],
                "the message body",
                null,
                null,
                [], // annotations
                [], // segments
                null,
                "1770071003269941", // timestamp_usec as string
                null,
                null,
                null,
                true,
                1770071003269i64,
                ["TOPIC_ABC", null, ["TOPIC_ABC", null, []]]
            ],
            2
        ]);
        let hit = super::parse_message_hit(&hit_json).expect("parse");
        assert_eq!(hit.topic_id, "TOPIC_ABC");
        assert_eq!(hit.author_id, "AUTHOR_ID");
        assert_eq!(hit.text, "the message body");
        assert_eq!(hit.timestamp_usec, 1770071003269941);
    }

    #[test]
    fn group_into_threads_preserves_server_ranking_and_deduplicates() {
        let mk = |topic: &str, text: &str, ts: u64| MessageHit {
            topic_id: topic.into(),
            author_id: "A".into(),
            text: text.into(),
            timestamp_usec: ts,
        };
        let msgs = vec![
            mk("T1", "first hit", 10),
            mk("T2", "second", 20),
            mk("T1", "another in T1", 30),
            mk("T3", "third topic", 40),
            mk("T2", "more T2", 50),
        ];
        let threads = super::group_into_threads(msgs);
        assert_eq!(threads.len(), 3);
        assert_eq!(threads[0].topic_id, "T1");
        assert_eq!(threads[0].messages.len(), 2);
        assert_eq!(threads[1].topic_id, "T2");
        assert_eq!(threads[1].messages.len(), 2);
        assert_eq!(threads[2].topic_id, "T3");
        assert_eq!(threads[2].messages.len(), 1);
    }

    #[test]
    fn top_threads_clamps_to_available() {
        let r = SearchResults {
            hits: vec![],
            threads: vec![
                ThreadHit {
                    topic_id: "A".into(),
                    messages: vec![],
                },
                ThreadHit {
                    topic_id: "B".into(),
                    messages: vec![],
                },
            ],
            continuation: None,
        };
        assert_eq!(r.top_threads(0).len(), 0);
        assert_eq!(r.top_threads(1).len(), 1);
        assert_eq!(r.top_threads(2).len(), 2);
        assert_eq!(r.top_threads(99).len(), 2); // clamped
    }

    #[test]
    fn build_search_payload_uses_global_filter_slot() {
        let p = build_search_payload("hello");
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("[[],null,null,null"));
        assert!(s.ends_with(",[3],[97]]"));
    }
}
