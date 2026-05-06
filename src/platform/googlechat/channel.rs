//! BrowserChannel long-poll implementation through Chrome's fetch().
//!
//! The web client uses a streaming long-poll to `/u/0/webchannel/events`
//! to receive real-time events (new messages, reactions, typing indicators).
//!
//! Implementation notes:
//! - Cookies are encrypted at the browser level so we MUST proxy through Chrome.
//! - Chrome's `fetch()` with ReadableStream + AbortController gives us
//!   streaming semantics: the call returns after idle-timeout (2s) with
//!   whatever data has been received.
//! - The protocol is a UTF-16-length-prefixed framed stream of JSON arrays.

use std::sync::Arc;
use std::time::Duration;

use crossbeam::channel::Sender;

use crate::error::ChannelError;
use crate::event::InboundEvent;
use crate::types::PlatformId;

use super::chunk::ChunkParser;
use super::pblite;
use super::session::Session;

/// Context passed to the long-poll thread. Owns a Chrome tab for
/// executing fetch() calls independently of other tabs.
pub struct StreamingContext {
    pub tab: Arc<headless_chrome::Tab>,
    pub sid: String,
}

/// Long-poll loop that can be spawned as a separate thread.
///
/// When the Chrome tab drops (connection closed), creates a new tab
/// and continues polling. Only exits on session expiry or max retries.
pub fn long_poll_loop_threaded(ctx: StreamingContext, inbound_tx: Sender<InboundEvent>) {
    let mut aid: u64 = 0;
    let mut retry_count: u32 = 0;
    let max_retries: u32 = 20;
    let mut interner = crate::types::IdInterner::new();

    loop {
        if retry_count >= max_retries {
            tracing::warn!("Long-poll: max retries exceeded ({max_retries})");
            let _ = inbound_tx.send(InboundEvent::Disconnected {
                platform: PlatformId::GoogleChat,
                reason: crate::event::DisconnectReason::MaxRetriesExceeded,
            });
            return;
        }

        match long_poll_once(&ctx, aid, &inbound_tx, &mut interner) {
            Ok(new_aid) => {
                aid = new_aid;
                retry_count = 0;
                // CDP keepalive: evaluate a trivial expression between poll
                // cycles to keep the headless_chrome WebSocket connection alive.
                // Without this, the connection dies after ~5 minutes of the
                // WebSocket seeing only outbound evaluate() calls.
                let _ = ctx.tab.evaluate("1", false);
            }
            Err(ChannelError::SessionExpired) => {
                tracing::warn!("Long-poll: session expired");
                let _ = inbound_tx.send(InboundEvent::Disconnected {
                    platform: PlatformId::GoogleChat,
                    reason: crate::event::DisconnectReason::SessionExpired,
                });
                return;
            }
            Err(e) => {
                let err_str = e.to_string();
                retry_count += 1;

                // Chrome tab connection died — exit thread so main loop
                // can recreate the entire BrowserChannel setup.
                if err_str.contains("connection is closed") || err_str.contains("not connected") {
                    tracing::warn!("Long-poll: Chrome tab died, requesting restart");
                    let _ = inbound_tx.send(InboundEvent::Reconnecting {
                        platform: PlatformId::GoogleChat,
                        attempt: retry_count,
                    });
                    // Return so the main loop can call setup_browserchannel again
                    return;
                }

                let backoff = Duration::from_secs(2u64.pow(retry_count.min(6)));
                tracing::warn!(
                    "Long-poll error (attempt {retry_count}/{max_retries}): {e}, \
                     backing off {backoff:?}"
                );
                let _ = inbound_tx.send(InboundEvent::Reconnecting {
                    platform: PlatformId::GoogleChat,
                    attempt: retry_count,
                });
                std::thread::sleep(backoff);
            }
        }
    }
}

/// One long-poll cycle using the threaded context. Returns the new AID.
fn long_poll_once(
    ctx: &StreamingContext,
    aid: u64,
    inbound_tx: &Sender<InboundEvent>,
    interner: &mut crate::types::IdInterner,
) -> Result<u64, ChannelError> {
    let zx = Session::random_zx();
    let url = format!(
        "https://chat.google.com/u/0/webchannel/events?\
         VER=8&RID=rpc&SID={}&AID={aid}&TYPE=xmlhttp&CI=0&t=1&zx={zx}",
        ctx.sid
    );

    // We need a Tokens-like interface but only have Arc<Tab>. Use the
    // Tab's call_method / evaluate directly.
    let js = format!(
        r#"(async () => {{
            try {{
                const ctrl = new AbortController();
                const timeoutId = setTimeout(() => ctrl.abort(), 30000);
                let bytes = new Uint8Array(0);
                let status = 0;
                try {{
                    const resp = await fetch("{url}", {{
                        credentials: 'include',
                        headers: {{ 'X-Goog-AuthUser': '0' }},
                        signal: ctrl.signal
                    }});
                    status = resp.status;
                    const reader = resp.body.getReader();
                    const chunks = [];
                    let total = 0;
                    let idleTimer = null;
                    const done = new Promise((resolve) => {{
                        const resetIdle = () => {{
                            if (idleTimer) clearTimeout(idleTimer);
                            idleTimer = setTimeout(() => {{
                                try {{ reader.cancel(); }} catch(e) {{}}
                                resolve();
                            }}, 3000);
                        }};
                        resetIdle();
                        (async () => {{
                            try {{
                                while (true) {{
                                    const {{done, value}} = await reader.read();
                                    if (done) break;
                                    chunks.push(value);
                                    total += value.length;
                                    resetIdle();
                                }}
                            }} catch(e) {{}}
                            resolve();
                        }})();
                    }});
                    await done;
                    clearTimeout(timeoutId);
                    bytes = new Uint8Array(total);
                    let off = 0;
                    for (const c of chunks) {{ bytes.set(c, off); off += c.length; }}
                }} catch(e) {{
                    clearTimeout(timeoutId);
                }}
                let bin = '';
                for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
                return JSON.stringify({{status: status, size: bytes.length, data: btoa(bin)}});
            }} catch(e) {{ return JSON.stringify({{error: e.message}}); }}
        }})()"#
    );

    let result = ctx
        .tab
        .evaluate(&js, true)
        .map_err(|e| ChannelError::Http(e.to_string()))?;
    let text = result
        .value
        .and_then(|v| v.as_str().map(|s| s.to_owned()))
        .ok_or(ChannelError::Http("empty js response".into()))?;
    let resp: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| ChannelError::Http(format!("bad js response: {e}")))?;

    let status = resp.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
    if status == 400 || status == 401 || status == 403 {
        return Err(ChannelError::SessionExpired);
    }

    let data_b64 = resp.get("data").and_then(|v| v.as_str()).unwrap_or("");
    let body_bytes = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(data_b64)
            .unwrap_or_default()
    };

    if body_bytes.is_empty() {
        return Ok(aid);
    }

    if body_bytes.len() > 30 {
        tracing::warn!("long-poll: {} body bytes (event data)", body_bytes.len());
    } else {
        tracing::debug!("long-poll: {} body bytes (keepalive)", body_bytes.len());
    }

    let mut parser = ChunkParser::new();
    parser.feed(&body_bytes);
    let mut last_aid = aid;
    while let Some(chunk_bytes) = parser.next_chunk() {
        if let Err(e) =
            process_chunk_with_interner(&chunk_bytes, &mut last_aid, inbound_tx, interner)
        {
            tracing::warn!("Failed to process chunk: {e}");
        }
    }
    Ok(last_aid)
}

/// Process a chunk using the thread's own interner for ID resolution.
fn process_chunk_with_interner(
    chunk_bytes: &[u8],
    last_aid: &mut u64,
    inbound_tx: &Sender<InboundEvent>,
    interner: &mut crate::types::IdInterner,
) -> Result<(), ChannelError> {
    let arrays: Vec<serde_json::Value> = serde_json::from_slice(chunk_bytes)?;

    if dump_raw_frames() {
        if let Ok(s) = std::str::from_utf8(chunk_bytes) {
            eprintln!("[BC RAW] {s}");
        }
    }

    for array in &arrays {
        let arr = array.as_array().ok_or(ChannelError::MalformedFrame)?;
        if arr.len() < 2 {
            continue;
        }

        let array_id = arr[0].as_u64().ok_or(ChannelError::MalformedFrame)?;

        if let Some(events) = parse_browserchannel_event_with_interner(&arr[1], interner) {
            for event in events {
                let _ = inbound_tx.send(event);
            }
        }

        *last_aid = array_id;
    }

    Ok(())
}

/// Whether to dump raw BC chunk JSON to stderr. Enabled by setting
/// `TCHAT_BC_DUMP=1` in the environment. Used by the wire-format capture
/// harness to capture the wire format of unhandled event bodies.
fn dump_raw_frames() -> bool {
    std::env::var("TCHAT_BC_DUMP")
        .map(|v| v == "1")
        .unwrap_or(false)
}

const CHAT_BASE: &str = "https://chat.google.com/u/0";

/// Run the BrowserChannel long-poll loop using a [`DirectSession`]
/// (no Chrome process). The DirectSession's cookies are used directly via
/// `ureq`. Returns when the SID can no longer be re-acquired.
pub fn long_poll_loop_direct(
    mut session: super::direct::DirectSession,
    inbound_tx: Sender<InboundEvent>,
) {
    let mut retry_count: u32 = 0;
    let max_retries: u32 = 10;
    let mut interner = crate::types::IdInterner::new();

    if let Err(e) = session.register() {
        tracing::warn!("Direct BC: register failed: {e}");
        let _ = inbound_tx.send(InboundEvent::Disconnected {
            platform: PlatformId::GoogleChat,
            reason: crate::event::DisconnectReason::ServerError(e.to_string()),
        });
        return;
    }
    if let Err(e) = session.acquire_sid() {
        tracing::warn!("Direct BC: SID acquisition failed: {e}");
        let _ = inbound_tx.send(InboundEvent::Disconnected {
            platform: PlatformId::GoogleChat,
            reason: crate::event::DisconnectReason::ServerError(e.to_string()),
        });
        return;
    }

    let mut aid: u64 = 0;
    loop {
        if retry_count >= max_retries {
            let _ = inbound_tx.send(InboundEvent::Disconnected {
                platform: PlatformId::GoogleChat,
                reason: crate::event::DisconnectReason::MaxRetriesExceeded,
            });
            return;
        }

        let sid = match session.sid.clone() {
            Some(s) => s,
            None => {
                let _ = inbound_tx.send(InboundEvent::Disconnected {
                    platform: PlatformId::GoogleChat,
                    reason: crate::event::DisconnectReason::SessionExpired,
                });
                return;
            }
        };
        let zx = Session::random_zx();
        let url = format!(
            "{CHAT_BASE}/webchannel/events?\
             VER=8&RID=rpc&SID={sid}&AID={aid}&TYPE=xmlhttp&CI=0&t=1&zx={zx}"
        );

        match session.fetch_get_binary(&url) {
            Ok(body_bytes) => {
                if body_bytes.is_empty() {
                    retry_count = 0;
                    continue;
                }
                let mut parser = ChunkParser::new();
                parser.feed(&body_bytes);
                let mut last_aid = aid;
                while let Some(chunk) = parser.next_chunk() {
                    if let Err(e) = process_chunk_with_interner(
                        &chunk,
                        &mut last_aid,
                        &inbound_tx,
                        &mut interner,
                    ) {
                        tracing::warn!("Direct BC: chunk failed: {e}");
                    }
                }
                aid = last_aid;
                retry_count = 0;
            }
            Err(e) => {
                let s = e.to_string();
                if s.contains("HTTP 401") || s.contains("HTTP 403") || s.contains("HTTP 400") {
                    tracing::warn!("Direct BC: session expired, re-registering");
                    let re_ok = session.register().and_then(|_| session.acquire_sid());
                    if let Err(e) = re_ok {
                        tracing::warn!("Direct BC: re-register failed: {e}");
                        let _ = inbound_tx.send(InboundEvent::Disconnected {
                            platform: PlatformId::GoogleChat,
                            reason: crate::event::DisconnectReason::SessionExpired,
                        });
                        return;
                    }
                    aid = 0;
                    continue;
                }
                retry_count += 1;
                let backoff = Duration::from_secs(2u64.pow(retry_count.min(6)));
                tracing::warn!(
                    "Direct BC: HTTP error (attempt {retry_count}/{max_retries}): {e}, backoff {backoff:?}"
                );
                let _ = inbound_tx.send(InboundEvent::Reconnecting {
                    platform: PlatformId::GoogleChat,
                    attempt: retry_count,
                });
                std::thread::sleep(backoff);
            }
        }
    }
}

/// Run the BrowserChannel long-poll loop.
///
/// Blocks the calling thread. Continuously reconnects to the backwards channel,
/// parsing events and sending them to the main thread.
pub fn long_poll_loop(mut session: Session, inbound_tx: Sender<InboundEvent>) {
    let mut retry_count: u32 = 0;
    let max_retries: u32 = 10;

    // Step 1: Register to set cookies, then acquire a SID
    if let Err(e) = session.register() {
        tracing::warn!("Register failed: {e}");
        let _ = inbound_tx.send(InboundEvent::Disconnected {
            platform: PlatformId::GoogleChat,
            reason: crate::event::DisconnectReason::ServerError(e.to_string()),
        });
        return;
    }
    if let Err(e) = session.acquire_sid() {
        tracing::warn!("SID acquisition failed: {e}");
        let _ = inbound_tx.send(InboundEvent::Disconnected {
            platform: PlatformId::GoogleChat,
            reason: crate::event::DisconnectReason::ServerError(e.to_string()),
        });
        return;
    }

    let mut aid: u64 = 0;

    loop {
        if retry_count >= max_retries {
            let _ = inbound_tx.send(InboundEvent::Disconnected {
                platform: PlatformId::GoogleChat,
                reason: crate::event::DisconnectReason::MaxRetriesExceeded,
            });
            return;
        }

        match long_poll_request(&mut session, aid, &inbound_tx) {
            Ok(new_aid) => {
                aid = new_aid;
                retry_count = 0;
                tracing::debug!("Long-poll cycle complete (aid={aid}), continuing");
            }
            Err(ChannelError::SessionExpired) => {
                tracing::warn!("Session expired, re-registering");
                let re_ok = session.register().and_then(|_| session.acquire_sid());
                if let Err(e) = re_ok {
                    tracing::warn!("Re-register failed: {e}");
                    let _ = inbound_tx.send(InboundEvent::Disconnected {
                        platform: PlatformId::GoogleChat,
                        reason: crate::event::DisconnectReason::SessionExpired,
                    });
                    return;
                }
                aid = 0;
            }
            Err(e) => {
                retry_count += 1;
                let backoff = Duration::from_secs(2u64.pow(retry_count.min(6)));
                tracing::warn!(
                    "Long-poll error (attempt {retry_count}/{max_retries}): {e}, \
                     backing off {backoff:?}"
                );
                let _ = inbound_tx.send(InboundEvent::Reconnecting {
                    platform: PlatformId::GoogleChat,
                    attempt: retry_count,
                });
                std::thread::sleep(backoff);
            }
        }
    }
}

/// Execute one long-poll HTTP request through Chrome.
///
/// Returns the last acknowledged array ID (AID) on success.
fn long_poll_request(
    session: &mut Session,
    aid: u64,
    inbound_tx: &Sender<InboundEvent>,
) -> Result<u64, ChannelError> {
    let sid = session.sid.clone().ok_or(ChannelError::SessionExpired)?;
    let zx = Session::random_zx();
    let url = format!(
        "{CHAT_BASE}/webchannel/events?\
         VER=8&RID=rpc&SID={sid}&AID={aid}&TYPE=xmlhttp&CI=0&t=1&zx={zx}"
    );

    let body_bytes = session.tokens.fetch_get_binary(&url).map_err(|e| {
        let s = e.to_string();
        if s.contains("HTTP 401") || s.contains("HTTP 403") || s.contains("HTTP 400") {
            ChannelError::SessionExpired
        } else {
            ChannelError::Http(s)
        }
    })?;

    if body_bytes.is_empty() {
        // Empty response — server timeout, just reconnect
        return Ok(aid);
    }

    // Parse framed chunks
    let mut parser = ChunkParser::new();
    parser.feed(&body_bytes);

    let mut last_aid = aid;
    while let Some(chunk_bytes) = parser.next_chunk() {
        if let Err(e) = process_chunk(&chunk_bytes, &mut last_aid, inbound_tx) {
            tracing::warn!("Failed to process chunk: {e}");
            // One bad chunk shouldn't kill the connection
        }
    }

    Ok(last_aid)
}

/// Process a single BrowserChannel chunk (a JSON array of event arrays).
fn process_chunk(
    chunk_bytes: &[u8],
    last_aid: &mut u64,
    inbound_tx: &Sender<InboundEvent>,
) -> Result<(), ChannelError> {
    let arrays: Vec<serde_json::Value> = serde_json::from_slice(chunk_bytes)?;

    for array in &arrays {
        let arr = array.as_array().ok_or(ChannelError::MalformedFrame)?;
        if arr.len() < 2 {
            continue;
        }

        let array_id = arr[0].as_u64().ok_or(ChannelError::MalformedFrame)?;

        if let Some(events) = parse_browserchannel_event(&arr[1]) {
            for event in events {
                let _ = inbound_tx.send(event);
            }
        }

        *last_aid = array_id;
    }

    Ok(())
}

/// Parse a BrowserChannel inner event into InboundEvents. Uses the thread's
/// own interner to produce properly-interned IDs.
fn parse_browserchannel_event_with_interner(
    data: &serde_json::Value,
    interner: &mut crate::types::IdInterner,
) -> Option<Vec<InboundEvent>> {
    let arr = data.as_array()?;
    if arr.is_empty() {
        return None;
    }

    if arr.first().and_then(|v| v.as_str()) == Some("noop") {
        return None;
    }

    // Per mautrix: `data_array[0]` holds the StreamEventsResponse pblite.
    let event_data = arr.first()?;

    // Trim Event's trailing fields (latency_data, etc.) that have encoding
    // edge cases.
    let trimmed = trim_event_trailing_fields(event_data);

    // Schema-aware encode: lets digit-only-string timestamps decode as
    // int64, and resolves nested-message ambiguity for repeated fields.
    let schema = pblite::load_schema();
    let wire = match pblite::pblite_to_wire_typed(&trimmed, &schema, "StreamEventsResponse") {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("BC pblite_to_wire_typed failed: {e}");
            // Fall back to untyped — gives MessagePosted etc. even
            // without the schema.
            match pblite::pblite_to_wire(&trimmed) {
                Ok(w) => w,
                Err(e2) => {
                    tracing::warn!("BC pblite_to_wire fallback failed: {e2}");
                    return None;
                }
            }
        }
    };

    let stream_resp = match <super::proto::StreamEventsResponse as prost::Message>::decode(wire) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("BC StreamEventsResponse decode failed: {e}");
            return None;
        }
    };

    let mut events = Vec::new();
    if let Some(event) = stream_resp.event {
        extract_events(event, interner, &mut events);
    }

    if events.is_empty() {
        None
    } else {
        Some(events)
    }
}

/// Trim the Event pblite to just fields 1-10, dropping `latency_data` (11)
/// and anything after. These trailing fields have encoding edge cases we
/// don't handle perfectly and we don't use them.
///
/// The input is a StreamEventsResponse pblite: `[Event, sample_id, clock_sync]`
/// where Event is at index 0 and is itself a pblite array.
#[allow(dead_code)]
fn trim_event_trailing_fields(data: &serde_json::Value) -> serde_json::Value {
    let Some(outer) = data.as_array() else {
        return data.clone();
    };
    if outer.is_empty() {
        return data.clone();
    }

    // Clone the outer structure
    let mut new_outer = outer.clone();

    // Trim Event (at index 0) to first 10 fields
    if let Some(serde_json::Value::Array(event_arr)) = new_outer.first() {
        let mut trimmed = event_arr.clone();
        trimmed.truncate(10);
        new_outer[0] = serde_json::Value::Array(trimmed);
    }

    serde_json::Value::Array(new_outer)
}

/// Trim a raw Event pblite (positional array IS the Event) to just fields 1-10.
#[allow(dead_code)]
fn trim_event_trailing_fields_raw(data: &serde_json::Value) -> serde_json::Value {
    let Some(arr) = data.as_array() else {
        return data.clone();
    };
    let mut trimmed = arr.clone();
    trimmed.truncate(10);
    serde_json::Value::Array(trimmed)
}

/// Extract one or more InboundEvents from a proto Event's EventBody list.
/// Takes owned `Event` so each body's string fields can move into the
/// resulting InboundEvent rather than being cloned.
fn extract_events(
    event: super::proto::Event,
    interner: &mut crate::types::IdInterner,
    out: &mut Vec<InboundEvent>,
) {
    let body_count = event.bodies.len() + if event.body.is_some() { 1 } else { 0 };
    tracing::warn!("BC stream: type={:?} bodies={body_count}", event.r#type,);
    if let Some(body) = event.body {
        dispatch_body(body, interner, out);
    }
    for body in event.bodies {
        dispatch_body(body, interner, out);
    }
}

fn describe_body(body: &super::proto::event::EventBody) -> String {
    let mut parts = Vec::new();
    if body.message_posted.is_some() {
        parts.push("message_posted");
    }
    if body.message_deleted.is_some() {
        parts.push("message_deleted");
    }
    if body.message_reaction.is_some() {
        parts.push("message_reaction");
    }
    if body.message_reactions_summary.is_some() {
        parts.push("message_reactions_summary");
    }
    if body.typing_state_changed_event.is_some() {
        parts.push("typing");
    }
    if body.group_read_state_updated_event.is_some() {
        parts.push("read_state_updated");
    }
    if body.topic_created.is_some() {
        parts.push("topic_created");
    }
    if body.membership_changed.is_some() {
        parts.push("membership_changed");
    }
    if body.group_updated.is_some() {
        parts.push("group_updated");
    }
    if body.group_viewed.is_some() {
        parts.push("group_viewed");
    }
    if body.user_status_updated_event.is_some() {
        parts.push("user_status_updated");
    }
    if body.read_receipt_changed.is_some() {
        parts.push("read_receipt_changed");
    }
    if body.web_push_notification.is_some() {
        parts.push("web_push");
    }
    let event_type = body.event_type;
    format!("type={event_type:?} fields=[{}]", parts.join(","))
}

/// Dispatch a single EventBody to the appropriate InboundEvent type.
/// Takes owned body so string fields (text_body, IDs) can move out
/// rather than being cloned.
fn dispatch_body(
    body: super::proto::event::EventBody,
    interner: &mut crate::types::IdInterner,
    out: &mut Vec<InboundEvent>,
) {
    use crate::types::{MessageId, PlatformId, SpaceId, Timestamp, UserId};

    let desc = describe_body(&body);
    let len_before = out.len();
    if desc.contains("message_posted") || desc.contains("typing") || desc.contains("reaction") {
        tracing::warn!("BC event: {desc}");
    } else {
        tracing::debug!("BC event: {desc}");
    }
    let event_type = body.event_type;

    // MESSAGE_POSTED / MESSAGE_UPDATED — the same body field is reused for
    // both. Distinguish via the body's event_type (7 = MESSAGE_UPDATED),
    // falling back to last_edit_time > create_time.
    if let Some(msg_event) = body.message_posted {
        if let Some(proto_msg) = msg_event.message {
            // Pull raw IDs from the proto without cloning text_body.
            let (space_str, topic_str, msg_id_str) = extract_message_ids(&proto_msg);
            let space_id = SpaceId {
                platform: PlatformId::GoogleChat,
                id: interner.intern(&space_str),
            };
            let is_edit = match event_type {
                Some(7) => true,
                Some(6) => false,
                _ => match (proto_msg.last_edit_time, proto_msg.create_time) {
                    (Some(edit), Some(create)) => edit > create,
                    _ => false,
                },
            };
            // Move text_body, reactions, etc. out of proto_msg.
            if let Some(msg) = proto_to_message_owned(proto_msg, space_id, interner) {
                let topic_raw = if topic_str.is_empty() {
                    None
                } else {
                    Some(topic_str)
                };
                if is_edit {
                    out.push(InboundEvent::MessageEdited {
                        message: msg,
                        space_id_raw: space_str,
                        topic_id_raw: topic_raw,
                        message_id_raw: msg_id_str,
                    });
                } else {
                    out.push(InboundEvent::MessagePosted {
                        message: msg,
                        space_id_raw: space_str,
                        topic_id_raw: topic_raw,
                        message_id_raw: msg_id_str,
                    });
                }
            }
        }
    }

    // TYPING_STATE_CHANGED
    if let Some(typing) = body.typing_state_changed_event {
        let state = typing.state.unwrap_or(0);
        let user_str = typing
            .user_id
            .as_ref()
            .and_then(|u| u.id.clone())
            .unwrap_or_default();
        let context_space = typing
            .context
            .as_ref()
            .and_then(|c| c.group_id.as_ref())
            .and_then(|g| {
                g.space_id
                    .as_ref()
                    .and_then(|s| s.space_id.clone())
                    .or_else(|| g.dm_id.as_ref().and_then(|d| d.dm_id.clone()))
            })
            .unwrap_or_default();
        let space_id = SpaceId {
            platform: PlatformId::GoogleChat,
            id: interner.intern(&context_space),
        };
        let user_id = UserId {
            platform: PlatformId::GoogleChat,
            id: interner.intern(&user_str),
        };
        match state {
            1 => out.push(InboundEvent::TypingStarted {
                space_id,
                user_id,
                timestamp: Timestamp(typing.start_timestamp_usec.unwrap_or(0) as u64),
            }),
            2 => out.push(InboundEvent::TypingStopped { space_id, user_id }),
            _ => {}
        }
    }

    // MESSAGE_DELETED
    if let Some(del) = body.message_deleted {
        if let Some(mid) = &del.message_id {
            let msg_str = mid.message_id.clone().unwrap_or_default();
            let space_str = mid
                .parent_id
                .as_ref()
                .and_then(|p| p.topic_id.as_ref())
                .and_then(|t| t.group_id.as_ref())
                .and_then(|g| {
                    g.space_id
                        .as_ref()
                        .and_then(|s| s.space_id.clone())
                        .or_else(|| g.dm_id.as_ref().and_then(|d| d.dm_id.clone()))
                })
                .unwrap_or_default();
            out.push(InboundEvent::MessageDeleted {
                space_id: SpaceId {
                    platform: PlatformId::GoogleChat,
                    id: interner.intern(&space_str),
                },
                message_id: MessageId(interner.intern(&msg_str)),
            });
        }
    }

    // MESSAGE_REACTION (singular — fired when a single user adds/removes a reaction)
    if let Some(rxn) = body.message_reaction {
        if let Some(mid) = &rxn.message_id {
            let msg_str = mid.message_id.clone().unwrap_or_default();
            let space_str = mid
                .parent_id
                .as_ref()
                .and_then(|p| p.topic_id.as_ref())
                .and_then(|t| t.group_id.as_ref())
                .and_then(|g| {
                    g.space_id
                        .as_ref()
                        .and_then(|s| s.space_id.clone())
                        .or_else(|| g.dm_id.as_ref().and_then(|d| d.dm_id.clone()))
                })
                .unwrap_or_default();
            // This event carries the single reaction delta; produce a
            // ReactionUpdated with a minimal set. Stores merge these.
            let emoji_str = rxn
                .emoji
                .as_ref()
                .and_then(|e| e.unicode.clone())
                .unwrap_or_else(|| "?".to_string());
            let option = rxn.option.unwrap_or(0);
            let count = if option == 1 { 1 } else { 0 };
            out.push(InboundEvent::ReactionUpdated {
                space_id: SpaceId {
                    platform: PlatformId::GoogleChat,
                    id: interner.intern(&space_str),
                },
                message_id: MessageId(interner.intern(&msg_str)),
                reactions: vec![crate::types::Reaction {
                    emoji: crate::types::Emoji::Unicode(emoji_str),
                    count,
                    includes_self: option == 1,
                }],
            });
        }
    }

    // MESSAGE_REACTIONS_SUMMARY (reactions on a message changed)
    if let Some(summary) = body.message_reactions_summary {
        if let Some(mid) = &summary.message_id {
            let msg_str = mid.message_id.clone().unwrap_or_default();
            let space_str = mid
                .parent_id
                .as_ref()
                .and_then(|p| p.topic_id.as_ref())
                .and_then(|t| t.group_id.as_ref())
                .and_then(|g| {
                    g.space_id
                        .as_ref()
                        .and_then(|s| s.space_id.clone())
                        .or_else(|| g.dm_id.as_ref().and_then(|d| d.dm_id.clone()))
                })
                .unwrap_or_default();
            let reactions = summary
                .reaction_summary
                .iter()
                .map(|rs| {
                    let emoji = rs
                        .emoji
                        .as_ref()
                        .and_then(|e| e.unicode.clone())
                        .map(crate::types::Emoji::Unicode)
                        .unwrap_or(crate::types::Emoji::Unicode("?".into()));
                    crate::types::Reaction {
                        emoji,
                        count: rs.count.unwrap_or(0) as u32,
                        includes_self: rs.current_user_reacted.unwrap_or(false),
                    }
                })
                .collect();
            out.push(InboundEvent::ReactionUpdated {
                space_id: SpaceId {
                    platform: PlatformId::GoogleChat,
                    id: interner.intern(&space_str),
                },
                message_id: MessageId(interner.intern(&msg_str)),
                reactions,
            });
        }
    }

    // GROUP_READ_STATE_UPDATED_EVENT
    if let Some(grse) = body.group_read_state_updated_event {
        if let Some(gid) = &grse.group_id {
            let space_str = gid
                .space_id
                .as_ref()
                .and_then(|s| s.space_id.clone())
                .or_else(|| gid.dm_id.as_ref().and_then(|d| d.dm_id.clone()))
                .unwrap_or_default();
            out.push(InboundEvent::ReadStateUpdated {
                space_id: SpaceId {
                    platform: PlatformId::GoogleChat,
                    id: interner.intern(&space_str),
                },
                last_read: Timestamp(grse.most_recent_read_time.unwrap_or(0) as u64),
                unread_count: 0, // not provided in this event; store won't change it
            });
        }
    }

    // GROUP_UPDATED — space rename, visibility change, etc.
    if let Some(gu) = body.group_updated {
        if let Some(group) = gu.group {
            if let Some(space) = proto_group_to_space(group, interner) {
                out.push(InboundEvent::SpaceUpdated { space });
            }
        }
    }

    // USER_STATUS_UPDATED_EVENT — primarily DND/custom-status changes.
    // We surface this as a PresenceChanged so the UI can reflect Dnd ↔ Active.
    if let Some(usue) = body.user_status_updated_event {
        if let Some(status) = usue.user_status {
            let user_str = status.user_id.and_then(|u| u.id).unwrap_or_default();
            if !user_str.is_empty() {
                let presence = match status.dnd_settings.and_then(|d| d.dnd_state) {
                    Some(2) => crate::types::PresenceStatus::Dnd,    // DND
                    Some(1) => crate::types::PresenceStatus::Active, // AVAILABLE
                    _ => crate::types::PresenceStatus::Unknown,
                };
                out.push(InboundEvent::PresenceChanged {
                    user_id: UserId {
                        platform: PlatformId::GoogleChat,
                        id: interner.intern(&user_str),
                    },
                    presence,
                });
            }
        }
    }

    // MEMBERSHIP_CHANGED — user joined, left, was invited, or role changed.
    if let Some(mc) = body.membership_changed {
        if let Some(membership) = mc.new_membership {
            let mem_state = membership.membership_state;
            let mem_role = membership.membership_role;
            let (user_str, space_str) = membership
                .id
                .map(|i| {
                    let user_str = i
                        .member_id
                        .and_then(|m| m.user_id)
                        .and_then(|u| u.id)
                        .unwrap_or_default();
                    let space_str = i
                        .group_id
                        .and_then(|g| {
                            g.space_id
                                .and_then(|s| s.space_id)
                                .or_else(|| g.dm_id.and_then(|d| d.dm_id))
                        })
                        .or_else(|| i.space_id.and_then(|s| s.space_id))
                        .unwrap_or_default();
                    (user_str, space_str)
                })
                .unwrap_or_default();
            if !user_str.is_empty() && !space_str.is_empty() {
                let state = match mem_state {
                    Some(2) => crate::types::MembershipState::Joined,
                    Some(1) => crate::types::MembershipState::Invited,
                    Some(3) => crate::types::MembershipState::Left,
                    _ => crate::types::MembershipState::Unknown,
                };
                let role = match mem_role {
                    Some(2) => crate::types::MemberRole::Invitee,
                    Some(3) => crate::types::MemberRole::Member,
                    Some(4) => crate::types::MemberRole::Owner,
                    Some(6) => crate::types::MemberRole::Manager,
                    _ => crate::types::MemberRole::Unknown,
                };
                out.push(InboundEvent::MembershipChanged {
                    space_id: SpaceId {
                        platform: PlatformId::GoogleChat,
                        id: interner.intern(&space_str),
                    },
                    user_id: UserId {
                        platform: PlatformId::GoogleChat,
                        id: interner.intern(&user_str),
                    },
                    state,
                    role,
                });
            }
        }
    }

    // TOPIC_CREATED — a new top-level message starts a new topic. The first
    // reply is the message itself; surface each as MessagePosted.
    if let Some(tc) = body.topic_created {
        if let Some(topic) = tc.topic {
            let space_str = topic
                .id
                .as_ref()
                .and_then(|tid| tid.group_id.as_ref())
                .and_then(|g| {
                    g.space_id
                        .as_ref()
                        .and_then(|s| s.space_id.clone())
                        .or_else(|| g.dm_id.as_ref().and_then(|d| d.dm_id.clone()))
                })
                .unwrap_or_default();
            let space_id = SpaceId {
                platform: PlatformId::GoogleChat,
                id: interner.intern(&space_str),
            };
            for reply in topic.replies {
                let (rspace_str, rtopic_str, rmsg_str) = extract_message_ids(&reply);
                if let Some(msg) = proto_to_message_owned(reply, space_id, interner) {
                    out.push(InboundEvent::MessagePosted {
                        message: msg,
                        space_id_raw: if rspace_str.is_empty() {
                            space_str.clone()
                        } else {
                            rspace_str
                        },
                        topic_id_raw: if rtopic_str.is_empty() {
                            None
                        } else {
                            Some(rtopic_str)
                        },
                        message_id_raw: rmsg_str,
                    });
                }
            }
        }
    }

    // READ_RECEIPT_CHANGED — someone else's last-read pointer moved on a
    // message in this space. tchat has no per-message read-receipt model
    // yet; log for visibility but don't emit an InboundEvent.
    if body.read_receipt_changed.is_some() {
        tracing::debug!("BC: read_receipt_changed (no inbound mapping)");
    }

    // Coverage: a body is "uncovered" only if we recognized no field at
    // all (describe_body found nothing) AND we emitted no InboundEvent.
    let has_recognized_field = !desc.contains("fields=[]");
    if out.len() == len_before && !has_recognized_field {
        let et = event_type.unwrap_or(0);
        tracing::warn!("BC uncovered: event_type={et} ({desc})");
        record_uncovered(et);
    }
}

/// Extract (space_id, topic_id, message_id) raw strings from a proto
/// Message's nested ID hierarchy without consuming the message.
fn extract_message_ids(proto_msg: &super::proto::Message) -> (String, String, String) {
    let space_str = proto_msg
        .id
        .as_ref()
        .and_then(|id| id.parent_id.as_ref())
        .and_then(|p| p.topic_id.as_ref())
        .and_then(|t| t.group_id.as_ref())
        .and_then(|g| {
            g.space_id
                .as_ref()
                .and_then(|s| s.space_id.clone())
                .or_else(|| g.dm_id.as_ref().and_then(|d| d.dm_id.clone()))
        })
        .unwrap_or_default();
    let topic_str = proto_msg
        .id
        .as_ref()
        .and_then(|id| id.parent_id.as_ref())
        .and_then(|p| p.topic_id.as_ref())
        .and_then(|t| t.topic_id.clone())
        .unwrap_or_default();
    let msg_id_str = proto_msg
        .id
        .as_ref()
        .and_then(|id| id.message_id.clone())
        .unwrap_or_default();
    (space_str, topic_str, msg_id_str)
}

// Bitset for body event_type values we've received but not dispatched.
// 128 bits covers the proto's EventType enum range.
static UNCOVERED_LO: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static UNCOVERED_HI: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn record_uncovered(event_type: i32) {
    if !(0..128).contains(&event_type) {
        return;
    }
    let bit = 1u64 << (event_type % 64);
    if event_type < 64 {
        UNCOVERED_LO.fetch_or(bit, std::sync::atomic::Ordering::Relaxed);
    } else {
        UNCOVERED_HI.fetch_or(bit, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Returns the list of body `event_type` values received during this
/// session that did not produce any dispatched InboundEvent. Useful from
/// tests/harness for surfacing what's left to handle.
pub fn uncovered_event_types() -> Vec<i32> {
    let lo = UNCOVERED_LO.load(std::sync::atomic::Ordering::Relaxed);
    let hi = UNCOVERED_HI.load(std::sync::atomic::Ordering::Relaxed);
    let mut out = Vec::new();
    for i in 0..64 {
        if lo & (1u64 << i) != 0 {
            out.push(i);
        }
    }
    for i in 0..64 {
        if hi & (1u64 << i) != 0 {
            out.push(64 + i);
        }
    }
    out
}

/// Build a platform Space from an OWNED proto Group, moving `name` rather
/// than cloning. Returns None if group_id is missing.
fn proto_group_to_space(
    group: super::proto::Group,
    interner: &mut crate::types::IdInterner,
) -> Option<crate::types::Space> {
    use crate::types::{PlatformId, Space, SpaceId, SpaceKind, Timestamp};

    let group_type = group.group_type;
    let is_flat = group.is_flat;
    let sort_ts = Timestamp(group.sort_time.unwrap_or(0) as u64);
    let name_opt = group.name;
    let gid = group.group_id?;
    let (id_str, is_dm) = if let Some(sid) = gid.space_id {
        (sid.space_id.unwrap_or_default(), false)
    } else if let Some(did) = gid.dm_id {
        (format!("dm/{}", did.dm_id.unwrap_or_default()), true)
    } else {
        return None;
    };
    if id_str.is_empty() || id_str == "dm/" {
        return None;
    }

    let kind = if is_dm {
        SpaceKind::DirectMessage
    } else if matches!(group_type, Some(1)) || is_flat == Some(true) {
        SpaceKind::Room
    } else {
        SpaceKind::ThreadedRoom
    };

    let name = name_opt.unwrap_or_else(|| id_str.clone());
    Some(Space {
        id: SpaceId {
            platform: PlatformId::GoogleChat,
            id: interner.intern(&id_str),
        },
        name,
        kind,
        platform: PlatformId::GoogleChat,
        unread_count: 0,
        last_activity: sort_ts,
        sort_timestamp: sort_ts,
        typing_users: Vec::new(),
    })
}

/// Convert an OWNED proto Message into platform Message, moving owned
/// fields (text_body, reactions) instead of cloning. Caller pre-extracts
/// IDs via `extract_message_ids` if it needs them too.
fn proto_to_message_owned(
    proto_msg: super::proto::Message,
    space_id: crate::types::SpaceId,
    interner: &mut crate::types::IdInterner,
) -> Option<crate::types::Message> {
    use crate::types::{
        Emoji, Message, MessageId, MessageType, PlatformId, Reaction, Timestamp, TopicId, UserId,
    };

    // Sender (small: a numeric user-id string we just intern).
    let sender = proto_msg
        .creator
        .and_then(|u| u.user_id)
        .and_then(|uid| uid.id)
        .map(|s| UserId {
            platform: PlatformId::GoogleChat,
            id: interner.intern(&s),
        })
        .unwrap_or(UserId {
            platform: PlatformId::GoogleChat,
            id: interner.intern("unknown"),
        });

    let timestamp = Timestamp(proto_msg.create_time.unwrap_or(0) as u64);
    let edit_timestamp = proto_msg.last_edit_time.map(|t| Timestamp(t as u64));
    let message_type = match proto_msg.message_type {
        Some(2) => MessageType::System,
        _ => MessageType::User,
    };

    // Move IDs out (no clones).
    let id = proto_msg.id?;
    let msg_id_str = id.message_id?;
    let msg_id = MessageId(interner.intern(&msg_id_str));
    let thread_id = id
        .parent_id
        .and_then(|p| p.topic_id)
        .and_then(|t| t.topic_id)
        .map(|s| TopicId(interner.intern(&s)));

    // Move reactions vector (no per-Reaction clone of emoji string).
    let reactions = proto_msg
        .reactions
        .into_iter()
        .map(|r| {
            let emoji = r
                .emoji
                .and_then(|e| e.unicode)
                .map(Emoji::Unicode)
                .unwrap_or(Emoji::Unicode("?".into()));
            Reaction {
                emoji,
                count: r.count.unwrap_or(0) as u32,
                includes_self: r.current_user_participated.unwrap_or(false),
            }
        })
        .collect();

    // Move the text body (potentially the largest string in the proto).
    let text = proto_msg.text_body.unwrap_or_default();

    Some(Message {
        id: msg_id,
        space_id,
        sender,
        timestamp,
        edit_timestamp,
        text,
        annotations: Vec::new(),
        reactions,
        thread_id,
        message_type,
        platform: PlatformId::GoogleChat,
    })
}

/// Parse a BrowserChannel inner event into InboundEvents.
fn parse_browserchannel_event(data: &serde_json::Value) -> Option<Vec<InboundEvent>> {
    let arr = data.as_array()?;
    if arr.is_empty() {
        return None;
    }

    // "noop" events are keepalives
    if arr.first().and_then(|v| v.as_str()) == Some("noop") {
        tracing::trace!("BrowserChannel keepalive");
        return None;
    }

    // Try to decode as pblite-encoded StreamEventsResponse
    let wire = pblite::pblite_to_wire(data).ok()?;
    let stream_resp = <super::proto::StreamEventsResponse as prost::Message>::decode(wire).ok()?;

    let mut events = Vec::new();
    if let Some(event) = &stream_resp.event {
        if let Some(inbound) = proto_event_to_inbound(event) {
            events.push(inbound);
        }
    }

    if events.is_empty() {
        None
    } else {
        Some(events)
    }
}

/// Convert a proto Event into an InboundEvent.
fn proto_event_to_inbound(event: &super::proto::Event) -> Option<InboundEvent> {
    let body = event.body.as_ref()?;

    if body.message_posted.is_some() {
        tracing::debug!("Received message_posted event");
        return None;
    }

    if body.typing_state_changed_event.is_some() {
        tracing::debug!("Received typing_state_changed event");
        return None;
    }

    tracing::trace!("Unhandled event type");
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_noop_event() {
        let data = serde_json::json!(["noop"]);
        let result = parse_browserchannel_event(&data);
        assert!(result.is_none());
    }

    #[test]
    fn parse_empty_array_event() {
        let data = serde_json::json!([]);
        let result = parse_browserchannel_event(&data);
        assert!(result.is_none());
    }

    #[test]
    fn parse_null_event() {
        let result = parse_browserchannel_event(&serde_json::Value::Null);
        assert!(result.is_none());
    }

    #[test]
    fn process_chunk_extracts_array_ids() {
        let chunk =
            serde_json::to_vec(&serde_json::json!([[1, ["noop"]], [2, ["noop"]],])).unwrap();

        let mut last_aid = 0u64;
        let (tx, _rx) = crossbeam::channel::unbounded();
        process_chunk(&chunk, &mut last_aid, &tx).unwrap();
        assert_eq!(last_aid, 2);
    }

    #[test]
    fn process_chunk_malformed_returns_error() {
        let chunk = b"not json";
        let mut last_aid = 0u64;
        let (tx, _rx) = crossbeam::channel::unbounded();
        assert!(process_chunk(chunk, &mut last_aid, &tx).is_err());
    }

    #[test]
    fn process_chunk_skips_short_arrays() {
        let chunk = serde_json::to_vec(&serde_json::json!([[1], [2, ["noop"]],])).unwrap();

        let mut last_aid = 0u64;
        let (tx, _rx) = crossbeam::channel::unbounded();
        process_chunk(&chunk, &mut last_aid, &tx).unwrap();
        assert_eq!(last_aid, 2);
    }

    #[test]
    fn process_chunk_updates_aid_monotonically() {
        let chunk = serde_json::to_vec(&serde_json::json!([
            [5, ["noop"]],
            [10, ["noop"]],
            [15, ["noop"]],
        ]))
        .unwrap();

        let mut last_aid = 0u64;
        let (tx, _rx) = crossbeam::channel::unbounded();
        process_chunk(&chunk, &mut last_aid, &tx).unwrap();
        assert_eq!(last_aid, 15);
    }

    #[test]
    fn process_chunk_empty_array_is_ok() {
        let chunk = serde_json::to_vec(&serde_json::json!([])).unwrap();
        let mut last_aid = 0u64;
        let (tx, _rx) = crossbeam::channel::unbounded();
        process_chunk(&chunk, &mut last_aid, &tx).unwrap();
        assert_eq!(last_aid, 0);
    }

    #[test]
    fn parse_string_event_returns_none() {
        let data = serde_json::json!("just a string");
        assert!(parse_browserchannel_event(&data).is_none());
    }

    #[test]
    fn parse_number_event_returns_none() {
        let data = serde_json::json!(42);
        assert!(parse_browserchannel_event(&data).is_none());
    }

    #[test]
    fn parse_nested_noop_returns_none() {
        let data = serde_json::json!(["noop", "extra_data"]);
        assert!(parse_browserchannel_event(&data).is_none());
    }

    #[test]
    fn process_chunk_non_integer_aid_fails() {
        let chunk = serde_json::to_vec(&serde_json::json!([["not_a_number", ["noop"]],])).unwrap();

        let mut last_aid = 0u64;
        let (tx, _rx) = crossbeam::channel::unbounded();
        assert!(process_chunk(&chunk, &mut last_aid, &tx).is_err());
    }

    /// Real captured frame from a live BrowserChannel session: a message
    /// posted in an external Chat space. Used to verify pblite → Event
    /// → InboundEvent conversion offline.
    const CAPTURED_MESSAGE_POSTED_PAYLOAD: &str = r#"[[[[["AAAA2kPVvto"]],null,null,null,["114193829005257704130"],["1777399199302573","1777399191400711"],null,[[null,null,null,[[null,"AV99MTL00Fg",[["AAAA2kPVvto"]]],"1777399031204721"],null,null,null,null,null,null,null,4]],[null,463544721,null,"1777399199348000",[4,10],1,762098494]],"187ba7d1-b5ee-4f59-80cc-5aa0d1cd1b0e"]]"#;

    /// Sanity check: a real captured BrowserChannel payload decodes
    /// through the wire-format pipeline without panicking, even when
    /// the event body uses fields we don't handle.
    #[test]
    fn captured_frame_decodes_through_pipeline() {
        let data: serde_json::Value =
            serde_json::from_str(CAPTURED_MESSAGE_POSTED_PAYLOAD).unwrap();
        let mut interner = crate::types::IdInterner::new();
        // Should not panic. May or may not produce events depending on
        // which body fields the server sent (this captured frame happens
        // to be a topic_viewed which our proto doesn't include).
        let _ = parse_browserchannel_event_with_interner(&data, &mut interner);
    }

    /// The coverage tracker should flag a body where NO recognized field
    /// is set, but should *not* flag a body where a known field is set
    /// even if our handler didn't emit any InboundEvent (e.g.
    /// topic_created with empty replies — a benign case).
    #[test]
    fn uncovered_tracker_only_flags_unknown_fields() {
        use crate::platform::googlechat::proto::event::EventBody;

        let mut interner = crate::types::IdInterner::new();

        // Case A: body where every recognized field is None. event_type
        // is set but no body field — this is genuinely uncovered.
        let mut out = Vec::new();
        let body_unknown = EventBody {
            event_type: Some(64), // GROUP_DEFAULT_SORT_ORDER_UPDATED
            ..Default::default()
        };
        dispatch_body(body_unknown, &mut interner, &mut out);
        assert!(out.is_empty());
        assert!(uncovered_event_types().contains(&64));

        // Case B: body where topic_created is set but topic.replies is
        // empty — a recognized field, intentional skip, NOT uncovered.
        // Reset the bitset would be ideal but it's static; instead pick
        // an event_type we haven't recorded yet for this test.
        let mut out = Vec::new();
        let body_recognized = EventBody {
            event_type: Some(20), // TOPIC_CREATED
            topic_created: Some(crate::platform::googlechat::proto::TopicCreatedEvent {
                topic: Some(crate::platform::googlechat::proto::Topic {
                    replies: Vec::new(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        dispatch_body(body_recognized, &mut interner, &mut out);
        assert!(out.is_empty(), "empty replies → no dispatch (expected)");
        assert!(
            !uncovered_event_types().contains(&20),
            "recognized field with no payload should NOT be flagged uncovered"
        );
    }

    #[test]
    fn process_chunk_mixed_valid_invalid_events() {
        let chunk = serde_json::to_vec(&serde_json::json!([
            [1, ["noop"]],
            [2, [1, 2, 3]],
            [3, ["noop"]],
        ]))
        .unwrap();

        let mut last_aid = 0u64;
        let (tx, _rx) = crossbeam::channel::unbounded();
        process_chunk(&chunk, &mut last_aid, &tx).unwrap();
        assert_eq!(last_aid, 3);
    }
}
