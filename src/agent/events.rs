//! Event bus + SSE writer for the agent HTTP surface.
//!
//! The BC long-poll thread converts `InboundEvent` → `EventEnvelope` and
//! publishes to the bus. SSE handlers each `subscribe()` to get a
//! `Receiver<EventEnvelope>`, filter via `EventFilter`, and write
//! `event: <kind>\ndata: <json>\n\n` frames to the TCP stream with a 15-
//! second `: keepalive` heartbeat to prevent intermediaries from closing
//! idle connections.
//!
//! The bus is `parking_lot::Mutex<Vec<Sender>>`. Dead subscribers are
//! pruned on the next publish (their `send()` returns `Err`). No `Arc`
//! beyond what `crossbeam::channel` already requires internally; the bus
//! itself is held by an outer `Arc<EventBus>` shared between the BC
//! producer thread and the SSE handler threads (publisher + subscribers
//! must outlive arbitrary connection lifetimes).

use std::io::Write;
use std::time::{Duration, Instant};

use crossbeam::channel::{unbounded, Receiver, Sender, TrySendError};
use parking_lot::Mutex;

use crate::agent::json::{EventEnvelope, EventKind};
use crate::event::InboundEvent;

/// Per-subscriber filter. Empty fields = "match all".
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    pub space_id: Option<String>,
    pub topic_id: Option<String>,
    pub kinds: Vec<EventKind>,
}

impl EventFilter {
    pub fn matches(&self, e: &EventEnvelope) -> bool {
        if let Some(ref s) = self.space_id {
            if &e.space_id != s {
                return false;
            }
        }
        if let Some(ref t) = self.topic_id {
            if e.topic_id.as_deref() != Some(t.as_str()) {
                return false;
            }
        }
        if !self.kinds.is_empty() && !self.kinds.contains(&e.kind) {
            return false;
        }
        true
    }
}

/// Multi-producer multi-subscriber broadcast bus. Subscribers register a
/// `Sender` on the bus; the producer fans out clones of each envelope to
/// every live subscriber. Dead subscribers (closed `Receiver`) are
/// dropped on publish.
pub struct EventBus {
    subscribers: Mutex<Vec<Sender<EventEnvelope>>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            subscribers: Mutex::new(Vec::new()),
        }
    }

    /// Register a new subscriber. The returned `Receiver` is parked on
    /// the bus's `Sender` list until it disconnects.
    pub fn subscribe(&self) -> Receiver<EventEnvelope> {
        let (tx, rx) = unbounded();
        self.subscribers.lock().push(tx);
        rx
    }

    /// Fan `e` out to every live subscriber. Drops disconnected ones.
    pub fn publish(&self, e: EventEnvelope) {
        let mut subs = self.subscribers.lock();
        subs.retain(|tx| match tx.try_send(e.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => true, // unbounded → never full; defensive
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    /// Number of live subscribers (after the last `publish` pruning).
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.lock().len()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert an `InboundEvent` to a JSON-shaped `EventEnvelope`. v1 only
/// emits envelopes for `MessagePosted` / `MessageEdited` because those
/// are the variants that carry raw wire IDs (`space_id_raw`,
/// `topic_id_raw`, `message_id_raw`) — the BC thread's `InternedId`s
/// are not resolvable from the agent's session interner. Other event
/// kinds return `None` for now; future work would route their raw IDs
/// out of the BC thread the same way.
pub fn envelope_from_inbound(event: &InboundEvent) -> Option<EventEnvelope> {
    use crate::event::InboundEvent as E;
    match event {
        E::MessagePosted {
            message,
            space_id_raw,
            topic_id_raw,
            message_id_raw,
        } => Some(EventEnvelope {
            kind: EventKind::MessagePosted,
            space_id: space_id_raw.clone(),
            topic_id: topic_id_raw.clone(),
            message_id: Some(message_id_raw.clone()),
            author_id: None,
            text: Some(message.text.clone()),
            timestamp_usec: Some(message.timestamp.0),
        }),
        E::MessageEdited {
            message,
            space_id_raw,
            topic_id_raw,
            message_id_raw,
        } => Some(EventEnvelope {
            kind: EventKind::MessageEdited,
            space_id: space_id_raw.clone(),
            topic_id: topic_id_raw.clone(),
            message_id: Some(message_id_raw.clone()),
            author_id: None,
            text: Some(message.text.clone()),
            timestamp_usec: Some(message.timestamp.0),
        }),
        _ => None,
    }
}

/// Drive an SSE response for one subscriber. Writes events to `out`
/// until the receiver disconnects, the writer errors, or `deadline` is
/// reached. Heartbeats every 15 s.
pub fn drive_sse<W: Write>(
    rx: Receiver<EventEnvelope>,
    filter: EventFilter,
    mut out: W,
    deadline: Option<Instant>,
) -> std::io::Result<()> {
    let heartbeat = Duration::from_secs(15);
    loop {
        if let Some(d) = deadline {
            if Instant::now() >= d {
                return Ok(());
            }
        }
        let timeout = match deadline {
            Some(d) => d.saturating_duration_since(Instant::now()).min(heartbeat),
            None => heartbeat,
        };
        match rx.recv_timeout(timeout) {
            Ok(env) if filter.matches(&env) => {
                let json = serde_json::to_string(&env).unwrap_or_default();
                writeln!(out, "event: {}", env.kind.as_str())?;
                writeln!(out, "data: {json}")?;
                writeln!(out)?;
                out.flush()?;
            }
            Ok(_) => continue, // filtered out
            Err(crossbeam::channel::RecvTimeoutError::Timeout) => {
                writeln!(out, ": keepalive")?;
                writeln!(out)?;
                out.flush()?;
            }
            Err(crossbeam::channel::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_msg(space: &str, topic: &str) -> EventEnvelope {
        EventEnvelope {
            kind: EventKind::MessagePosted,
            space_id: space.into(),
            topic_id: Some(topic.into()),
            message_id: Some("M".into()),
            author_id: Some("U".into()),
            text: Some("body".into()),
            timestamp_usec: Some(1),
        }
    }

    #[test]
    fn filter_matches_when_empty() {
        let f = EventFilter::default();
        assert!(f.matches(&env_msg("S", "T")));
    }

    #[test]
    fn filter_rejects_wrong_space() {
        let f = EventFilter {
            space_id: Some("OTHER".into()),
            ..EventFilter::default()
        };
        assert!(!f.matches(&env_msg("S", "T")));
    }

    #[test]
    fn filter_rejects_wrong_topic() {
        let f = EventFilter {
            topic_id: Some("OTHER".into()),
            ..EventFilter::default()
        };
        assert!(!f.matches(&env_msg("S", "T")));
    }

    #[test]
    fn filter_kinds_includes() {
        let f = EventFilter {
            kinds: vec![EventKind::MessagePosted],
            ..EventFilter::default()
        };
        assert!(f.matches(&env_msg("S", "T")));
        let f2 = EventFilter {
            kinds: vec![EventKind::MessageEdited],
            ..EventFilter::default()
        };
        assert!(!f2.matches(&env_msg("S", "T")));
    }

    #[test]
    fn bus_fans_out_to_all_subscribers() {
        let bus = EventBus::new();
        let r1 = bus.subscribe();
        let r2 = bus.subscribe();
        bus.publish(env_msg("S", "T"));
        assert!(r1.try_recv().is_ok());
        assert!(r2.try_recv().is_ok());
    }

    #[test]
    fn bus_drops_disconnected_subscribers() {
        let bus = EventBus::new();
        let r1 = bus.subscribe();
        drop(r1);
        let r2 = bus.subscribe();
        bus.publish(env_msg("S", "T"));
        assert_eq!(bus.subscriber_count(), 1);
        assert!(r2.try_recv().is_ok());
    }

    #[test]
    fn drive_sse_writes_event_then_keepalive() {
        let bus = EventBus::new();
        let rx = bus.subscribe();
        bus.publish(env_msg("S", "T"));
        let mut buf: Vec<u8> = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
        // Drop bus so the receiver eventually disconnects after we drain
        // — but we provide a deadline so it terminates regardless.
        drop(bus);
        let _ = drive_sse(rx, EventFilter::default(), &mut buf, Some(deadline));
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("event: message_posted"));
        assert!(s.contains("\"kind\":\"message_posted\""));
    }

    #[test]
    fn drive_sse_filter_excludes_mismatched() {
        let bus = EventBus::new();
        let rx = bus.subscribe();
        bus.publish(env_msg("OTHER", "T"));
        let mut buf: Vec<u8> = Vec::new();
        let f = EventFilter {
            space_id: Some("S".into()),
            ..Default::default()
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(20);
        drop(bus);
        let _ = drive_sse(rx, f, &mut buf, Some(deadline));
        let s = String::from_utf8(buf).unwrap();
        assert!(!s.contains("event: message_posted"));
    }
}
