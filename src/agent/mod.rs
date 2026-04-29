//! Agent-facing programmatic surface for tchat.
//!
//! The harness flow:
//!   1. `bootstrap()` — auth via the existing chain (saved cookies →
//!      Chrome DB → interactive Chrome). Brings up a BC long-poll
//!      supervisor that publishes message events to an `EventBus`.
//!   2. `post_question` — top-level `create_message` (no `topic_id`),
//!      which the threaded space turns into a new topic. Returns the
//!      assigned `topic_id` / `message_id`.
//!   3. `search_threads` — scoped `search_messages_in_space` →
//!      `top_threads(n)`.
//!   4. `reply_in_thread` — `create_message` with the trigger's
//!      `topic_id` populated.
//!   5. `subscribe_events` — register on the `EventBus` and stream
//!      filtered envelopes (the HTTP server bridges to SSE).
//!
//! Concurrency: `AgentApi` is `!Sync`. The HTTP server wraps it in a
//! `parking_lot::Mutex<AgentApi>` so write paths (which mutate
//! `Session`'s API counter) are serialized. The `EventBus` is held by
//! `Arc` because the BC bridge thread and the SSE handler threads need
//! independent ownership of it.

pub mod cli;
pub mod client;
pub mod events;
pub mod json;
pub mod server;

use std::sync::Arc;
use std::time::Instant;

use crossbeam::channel::{unbounded, Sender};
use prost::Message as _;

use crate::error::{AppError, AuthError};
use crate::event::InboundEvent;
use crate::platform::googlechat::{
    api, auth, channel, convert, cookies, proto, search, session::Session, setup_browserchannel,
};
use crate::types::SpaceKind as InternalSpaceKind;

use events::{envelope_from_inbound, EventBus, EventFilter};
use json::{
    AskResponse, EventEnvelope, MessageJson, ReplyResponse, SearchResponse, SpaceInfo, ThreadJson,
};

/// Top-level handle held by the daemon.
pub struct AgentApi {
    session: Session,
    bus: Arc<EventBus>,
    started_at: Instant,
    self_user_id: Option<String>,
}

impl AgentApi {
    /// Auth + start BC supervisor. Blocks until the auth chain
    /// completes; may pop a Chrome window for interactive sign-in if
    /// saved cookies are absent or expired.
    pub fn bootstrap() -> Result<Self, AppError> {
        let tokens = auth::authenticate(None).map_err(AppError::Auth)?;
        let mut session = Session::new(tokens);
        let _ = session.fetch_session_tokens();

        // Persist cookies for next run (best-effort).
        if let Ok(tab) = session.tokens.get_tab() {
            if let Ok(extracted) = cookies::extract_from_chrome_session(&tab) {
                let _ = cookies::save_cookies(&extracted);
            }
        }

        // Resolve self user id (best-effort; UI doesn't fail if absent).
        let self_user_id = fetch_self_user_id(&mut session);

        let bus = Arc::new(EventBus::new());
        spawn_bc_supervisor(&session, Arc::clone(&bus));

        Ok(Self {
            session,
            bus,
            started_at: Instant::now(),
            self_user_id,
        })
    }

    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    pub fn self_user_id(&self) -> Option<&str> {
        self.self_user_id.as_deref()
    }

    pub fn bus(&self) -> Arc<EventBus> {
        Arc::clone(&self.bus)
    }

    /// Subscribe to filtered events. Returned receiver is independent
    /// of `AgentApi`'s lifetime (held by `Arc<EventBus>`).
    pub fn subscribe_events(
        &self,
        _filter: EventFilter,
    ) -> crossbeam::channel::Receiver<EventEnvelope> {
        // Filter is applied in the SSE writer (drive_sse), not here, so
        // every subscriber gets the full firehose and decides per-frame.
        // This keeps the bus simpler at the cost of a tiny redundancy.
        self.bus.subscribe()
    }

    /// List the user's spaces (rooms + DMs) via `paginated_world`.
    pub fn list_spaces(&mut self) -> Result<Vec<SpaceInfo>, AppError> {
        let req = proto::PaginatedWorldRequest {
            request_header: Some(convert::tests_make_header()),
            world_section_requests: vec![
                proto::WorldSectionRequest {
                    page_size: Some(100),
                    world_section: Some(proto::WorldSection {
                        world_section_type: Some(14),
                    }), // ALL_DIRECT_MESSAGE_EVERYONE
                    ..Default::default()
                },
                proto::WorldSectionRequest {
                    page_size: Some(100),
                    world_section: Some(proto::WorldSection {
                        world_section_type: Some(8),
                    }), // ALL_ROOMS
                    ..Default::default()
                },
            ],
            fetch_from_user_spaces: Some(true),
            fetch_snippets_for_unnamed_rooms: Some(true),
            ..Default::default()
        };
        let bytes = self
            .session
            .call_api("paginated_world", &req.encode_to_vec())
            .map_err(map_auth_err)?;
        let resp = proto::PaginatedWorldResponse::decode(bytes::Bytes::from(bytes))
            .map_err(|e| AppError::Api(crate::error::ApiError::ProtoDecode(e)))?;

        let mut items: Vec<&proto::WorldItemLite> = Vec::new();
        for section in &resp.world_section_responses {
            items.extend(section.world_items.iter());
        }
        items.extend(resp.world_items.iter());

        let mut spaces = Vec::new();
        for item in items {
            let gid = match item.group_id.as_ref() {
                Some(g) => g,
                None => continue,
            };
            let (id_str, kind) = if let Some(sid) = gid.space_id.as_ref() {
                let id = sid.space_id.clone().unwrap_or_default();
                (id, classify_room_kind(item))
            } else if let Some(did) = gid.dm_id.as_ref() {
                let raw = did.dm_id.clone().unwrap_or_default();
                (raw, InternalSpaceKind::DirectMessage)
            } else {
                continue;
            };
            if id_str.is_empty() {
                continue;
            }
            let name = item.room_name.clone().unwrap_or_else(|| id_str.clone());
            spaces.push(SpaceInfo {
                id: id_str,
                name,
                kind: kind.into(),
            });
        }
        Ok(spaces)
    }

    /// Post a top-level message. In a threaded space this creates a
    /// new topic; the returned `topic_id == message_id`. In a flat
    /// space the topic_id is server-assigned and unrelated to the
    /// message_id.
    pub fn post_question(
        &mut self,
        space_id: &str,
        text: &str,
        idempotency_key: Option<&str>,
    ) -> Result<AskResponse, AppError> {
        let local_id = idempotency_key
            .map(|k| format!("agent-ask-{k}"))
            .unwrap_or_else(|| format!("agent-ask-{}-{}", std::process::id(), now_secs()));

        let req = proto::CreateMessageRequest {
            request_header: Some(convert::tests_make_header()),
            parent_id: Some(proto::MessageParentId {
                topic_id: Some(proto::TopicId {
                    group_id: Some(group_id_for(space_id)),
                    topic_id: None,
                }),
            }),
            text_body: Some(text.to_owned()),
            annotations: Vec::new(),
            local_id: Some(local_id),
            message_id: None,
            message_info: Some(proto::MessageInfo {
                accept_format_annotations: Some(true),
                reply_to: None,
            }),
        };
        let resp = api::call_proto::<_, proto::CreateMessageResponse>(
            &mut self.session,
            "create_message",
            &req,
        )
        .map_err(map_api_err)?;

        let id = resp.message.and_then(|m| m.id).ok_or_else(|| {
            AppError::Api(crate::error::ApiError::Http(
                "create_message: no message id in response".into(),
            ))
        })?;
        let message_id = id.message_id.unwrap_or_default();
        let topic_id = id
            .parent_id
            .and_then(|p| p.topic_id)
            .and_then(|t| t.topic_id)
            .unwrap_or_else(|| message_id.clone());
        Ok(AskResponse {
            topic_id,
            message_id,
            space_id: space_id.to_owned(),
        })
    }

    /// Reply inside an existing topic.
    pub fn reply_in_thread(
        &mut self,
        space_id: &str,
        topic_id: &str,
        text: &str,
        idempotency_key: Option<&str>,
    ) -> Result<ReplyResponse, AppError> {
        let local_id = idempotency_key
            .map(|k| format!("agent-reply-{k}"))
            .unwrap_or_else(|| format!("agent-reply-{}-{}", std::process::id(), now_secs()));

        let req = proto::CreateMessageRequest {
            request_header: Some(convert::tests_make_header()),
            parent_id: Some(proto::MessageParentId {
                topic_id: Some(proto::TopicId {
                    group_id: Some(group_id_for(space_id)),
                    topic_id: Some(topic_id.to_owned()),
                }),
            }),
            text_body: Some(text.to_owned()),
            annotations: Vec::new(),
            local_id: Some(local_id),
            message_id: None,
            message_info: Some(proto::MessageInfo {
                accept_format_annotations: Some(true),
                reply_to: None,
            }),
        };
        let resp = api::call_proto::<_, proto::CreateMessageResponse>(
            &mut self.session,
            "create_message",
            &req,
        )
        .map_err(map_api_err)?;
        let message_id = resp
            .message
            .and_then(|m| m.id)
            .and_then(|i| i.message_id)
            .unwrap_or_default();
        Ok(ReplyResponse { message_id })
    }

    /// Search messages in a single space, server-side scope. Returns
    /// the top `top` threads (server-ranked).
    pub fn search_threads(
        &mut self,
        space_id: &str,
        query: &str,
        top: usize,
    ) -> Result<SearchResponse, AppError> {
        // Build payload + run via Chrome's authenticated tab (the only
        // batchexecute path that works with our cookie set).
        let payload_value = search::build_search_in_space_payload(query, space_id, 2);
        let payload_string = serde_json::to_string(&payload_value)
            .map_err(|e| AppError::Api(crate::error::ApiError::Http(format!("encode: {e}"))))?;
        let payload_js_lit = serde_json::to_string(&payload_string)
            .map_err(|e| AppError::Api(crate::error::ApiError::Http(format!("encode: {e}"))))?;

        let js = format!(
            r#"
        (async () => {{
            try {{
                const w = window.WIZ_global_data || {{}};
                const at = w['SNlM0e'] || '';
                if (!at) return JSON.stringify({{error:'no at token'}});
                const payload = {payload_js_lit};
                const fReq = JSON.stringify([[["SBNmJb", payload, null, "generic"]]]);
                const body = 'f.req=' + encodeURIComponent(fReq) + '&at=' + encodeURIComponent(at);
                const resp = await fetch('/_/DynamiteWebUi/data/batchexecute?rpcids=SBNmJb&source-path=%2F&f.sid=-1&bl=boq_dynamite-frontend&hl=en&_reqid=' + Date.now(), {{
                    method: 'POST',
                    body: body,
                    headers: {{'Content-Type':'application/x-www-form-urlencoded;charset=UTF-8'}},
                    credentials: 'include',
                }});
                const text = await resp.text();
                return JSON.stringify({{status: resp.status, body: text}});
            }} catch (e) {{ return JSON.stringify({{error: e.message}}); }}
        }})()
        "#
        );

        let tab = self.session.tokens.get_tab().map_err(AppError::Auth)?;
        let result = tab
            .evaluate(&js, true)
            .map_err(|e| AppError::Api(crate::error::ApiError::Http(format!("eval: {e}"))))?;
        let raw = result
            .value
            .and_then(|v| v.as_str().map(str::to_owned))
            .ok_or_else(|| {
                AppError::Api(crate::error::ApiError::Http("empty eval result".into()))
            })?;
        let outer: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
            AppError::Api(crate::error::ApiError::Http(format!("parse outer: {e}")))
        })?;
        if let Some(err) = outer.get("error").and_then(|v| v.as_str()) {
            return Err(AppError::Api(crate::error::ApiError::Http(err.to_owned())));
        }
        let status = outer.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
        if status != 200 {
            return Err(AppError::Api(crate::error::ApiError::Http(format!(
                "batchexecute HTTP {status}"
            ))));
        }
        let body = outer.get("body").and_then(|v| v.as_str()).unwrap_or("");
        let results =
            search::parse_batchexecute_response(body, "SBNmJb").map_err(AppError::Auth)?;

        let threads: Vec<ThreadJson> = results
            .top_threads(top)
            .iter()
            .map(|t| ThreadJson {
                topic_id: t.topic_id.clone(),
                messages: t
                    .messages
                    .iter()
                    .map(|m| MessageJson {
                        author_id: m.author_id.clone(),
                        text: m.text.clone(),
                        timestamp_usec: m.timestamp_usec,
                    })
                    .collect(),
            })
            .collect();

        Ok(SearchResponse {
            query: query.to_owned(),
            threads,
            continuation: results.continuation,
        })
    }
}

// ───────────── helpers ─────────────

fn group_id_for(space_id: &str) -> proto::GroupId {
    if let Some(rest) = space_id.strip_prefix("dm/") {
        proto::GroupId {
            space_id: None,
            dm_id: Some(proto::DmId {
                dm_id: Some(rest.to_owned()),
            }),
        }
    } else {
        proto::GroupId {
            space_id: Some(proto::SpaceId {
                space_id: Some(space_id.to_owned()),
            }),
            dm_id: None,
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn classify_room_kind(item: &proto::WorldItemLite) -> InternalSpaceKind {
    // WorldItemLite doesn't expose group_type cleanly here; default to
    // ThreadedRoom for spaces (most modern rooms support threading).
    // Best-effort — agent harness can call `get_group` if it needs the
    // exact kind.
    let _ = item;
    InternalSpaceKind::Room
}

fn fetch_self_user_id(session: &mut Session) -> Option<String> {
    let req = proto::GetSelfUserStatusRequest {
        request_header: Some(convert::tests_make_header()),
    };
    let bytes = session
        .call_api("get_self_user_status", &req.encode_to_vec())
        .ok()?;
    let resp = proto::GetSelfUserStatusResponse::decode(bytes::Bytes::from(bytes)).ok()?;
    resp.user_status?.user_id?.id
}

fn map_auth_err(e: AuthError) -> AppError {
    AppError::Auth(e)
}

fn map_api_err(e: crate::error::ApiError) -> AppError {
    AppError::Api(e)
}

/// Spawn a BC supervisor that auto-reconnects on disconnect, converting
/// each `InboundEvent` to an `EventEnvelope` and publishing it on the
/// shared bus. Mirrors the supervisor pattern in `live_command_bot.rs`.
fn spawn_bc_supervisor(session: &Session, bus: Arc<EventBus>) {
    let bus_for_thread = Arc::clone(&bus);
    let (event_tx, event_rx) = unbounded::<InboundEvent>();
    spawn_bc_once(session, &event_tx);

    // Bridge thread: read InboundEvents, convert to envelopes, publish.
    // On Disconnected, we'd ideally re-spawn — but spawn_bc_once needs
    // &Session and we can't move the session into this thread. The
    // server holds the AgentApi behind a Mutex; it can call
    // `respawn_bc()` if it observes a Disconnected envelope. For v1 the
    // BC long-poll's own internal reconnect (in channel.rs) handles
    // most cases.
    std::thread::spawn(move || {
        while let Ok(ev) = event_rx.recv() {
            if let Some(env) = envelope_from_inbound(&ev) {
                bus_for_thread.publish(env);
            }
        }
    });
}

fn spawn_bc_once(session: &Session, tx: &Sender<InboundEvent>) {
    match setup_browserchannel(session) {
        Ok(ctx) => {
            let tx_clone = tx.clone();
            std::thread::spawn(move || {
                channel::long_poll_loop_threaded(ctx, tx_clone);
            });
        }
        Err(e) => {
            tracing::warn!("BC setup failed: {e}; events disabled");
        }
    }
}
