//! Typed Rust client for a running `tchat serve` daemon.
//!
//! Mirrors the HTTP surface in `agent/server.rs` so an external Rust
//! agent harness can `cargo add tchat` and call the daemon over the
//! local network without dealing with `ureq` / JSON / URL encoding
//! itself.
//!
//! For in-process integration (no HTTP at all) the harness can use
//! [`crate::agent::AgentApi`] directly — that path skips the daemon
//! entirely. This client is only for the case where the harness lives
//! in a separate process and the daemon owns the long-lived Chat
//! session.
//!
//! Example:
//!
//! ```no_run
//! use tchat::agent::client::Client;
//!
//! let c = Client::default();          // http://127.0.0.1:7800
//! let health = c.health()?;
//! let spaces = c.list_spaces()?;
//! let q = c.ask(&spaces[0].id, "How do I configure HTTP_PROXY?", None)?;
//! let ctx = c.search(&spaces[0].id, "HTTP_PROXY", 2)?;
//! // ... harness builds an answer using ctx.threads ...
//! c.reply(&spaces[0].id, &q.topic_id, "Here's the answer", None)?;
//! # Ok::<(), tchat::agent::client::ClientError>(())
//! ```

use std::io::Read;
use std::time::Duration;

use crate::agent::events::EventFilter;
use crate::agent::json::{
    AskRequest, AskResponse, EventEnvelope, EventKind, HealthResponse, ReplyRequest, ReplyResponse,
    SearchResponse, SpaceInfo, SpacesResponse,
};

/// Errors surfaced by the client.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("HTTP transport error: {0}")]
    Http(String),
    #[error("HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("response was not valid JSON: {0}")]
    Decode(String),
    #[error("event stream parse error: {0}")]
    EventStream(String),
}

impl From<ureq::Error> for ClientError {
    fn from(e: ureq::Error) -> Self {
        ClientError::Http(e.to_string())
    }
}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        ClientError::Http(e.to_string())
    }
}

/// Rust client for the agent HTTP daemon.
#[derive(Debug, Clone)]
pub struct Client {
    base: String,
}

impl Default for Client {
    fn default() -> Self {
        Self::new("http://127.0.0.1:7800")
    }
}

impl Client {
    /// Build a client targeting `base_url` (e.g. `"http://127.0.0.1:7800"`).
    pub fn new(base_url: impl Into<String>) -> Self {
        let mut base: String = base_url.into();
        if base.ends_with('/') {
            base.pop();
        }
        Self { base }
    }

    pub fn health(&self) -> Result<HealthResponse, ClientError> {
        self.get(&format!("{}/v1/health", self.base))
    }

    pub fn list_spaces(&self) -> Result<Vec<SpaceInfo>, ClientError> {
        let r: SpacesResponse = self.get(&format!("{}/v1/spaces", self.base))?;
        Ok(r.spaces)
    }

    /// Post a top-level question to a space. In a threaded space this
    /// creates a new topic with `topic_id == message_id`.
    pub fn ask(
        &self,
        space_id: &str,
        text: &str,
        idempotency_key: Option<&str>,
    ) -> Result<AskResponse, ClientError> {
        let body = AskRequest {
            text: text.to_owned(),
            idempotency_key: idempotency_key.map(str::to_owned),
        };
        self.post(
            &format!("{}/v1/spaces/{}/questions", self.base, space_id),
            &body,
        )
    }

    /// Server-side scoped search; returns up to `top` ranked threads.
    pub fn search(
        &self,
        space_id: &str,
        query: &str,
        top: usize,
    ) -> Result<SearchResponse, ClientError> {
        let url = format!(
            "{}/v1/spaces/{}/threads/search?q={}&top={top}",
            self.base,
            space_id,
            url_encode(query),
        );
        self.get(&url)
    }

    /// Reply inside an existing topic.
    pub fn reply(
        &self,
        space_id: &str,
        topic_id: &str,
        text: &str,
        idempotency_key: Option<&str>,
    ) -> Result<ReplyResponse, ClientError> {
        let body = ReplyRequest {
            space_id: space_id.to_owned(),
            text: text.to_owned(),
            idempotency_key: idempotency_key.map(str::to_owned),
        };
        self.post(
            &format!("{}/v1/threads/{}/reply", self.base, topic_id),
            &body,
        )
    }

    /// Open a Server-Sent Events stream of filtered events. Yields one
    /// `EventEnvelope` per `next()` call until the connection closes.
    /// Iterator drops cleanly when the stream ends or an error occurs.
    pub fn events(&self, filter: EventFilter) -> Result<EventStream, ClientError> {
        let mut url = format!("{}/v1/events?", self.base);
        if let Some(s) = &filter.space_id {
            url.push_str(&format!("space_id={}&", url_encode(s)));
        }
        if let Some(t) = &filter.topic_id {
            url.push_str(&format!("topic_id={}&", url_encode(t)));
        }
        if !filter.kinds.is_empty() {
            let kinds_str = filter
                .kinds
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(",");
            url.push_str(&format!("kinds={}&", url_encode(&kinds_str)));
        }
        let resp = ureq::get(&url)
            .config()
            .timeout_global(Some(Duration::from_secs(60 * 60)))
            .build()
            .call()?;
        Ok(EventStream {
            reader: Box::new(resp.into_body().into_reader()),
            buf: String::new(),
        })
    }

    // ───── inner helpers ─────

    fn get<R: for<'de> serde::Deserialize<'de>>(&self, url: &str) -> Result<R, ClientError> {
        let resp = ureq::get(url).call()?;
        let status: u16 = resp.status().into();
        let mut text = String::new();
        resp.into_body().into_reader().read_to_string(&mut text)?;
        if !(200..300).contains(&status) {
            return Err(ClientError::Status { status, body: text });
        }
        serde_json::from_str(&text).map_err(|e| ClientError::Decode(e.to_string()))
    }

    fn post<T: serde::Serialize, R: for<'de> serde::Deserialize<'de>>(
        &self,
        url: &str,
        body: &T,
    ) -> Result<R, ClientError> {
        let body_str =
            serde_json::to_string(body).map_err(|e| ClientError::Decode(e.to_string()))?;
        let resp = ureq::post(url)
            .header("Content-Type", "application/json")
            .send(body_str.as_bytes())?;
        let status: u16 = resp.status().into();
        let mut text = String::new();
        resp.into_body().into_reader().read_to_string(&mut text)?;
        if !(200..300).contains(&status) {
            return Err(ClientError::Status { status, body: text });
        }
        serde_json::from_str(&text).map_err(|e| ClientError::Decode(e.to_string()))
    }
}

/// Iterator over SSE events from `/v1/events`. Each call to `next()`
/// reads one full event (`event: ... \n data: {...} \n\n`) and returns
/// the parsed envelope. Heartbeats (`: keepalive`) are skipped silently.
pub struct EventStream {
    reader: Box<dyn Read + Send>,
    buf: String,
}

impl Iterator for EventStream {
    type Item = Result<EventEnvelope, ClientError>;

    fn next(&mut self) -> Option<Self::Item> {
        // Pull bytes until we have a complete `\n\n`-terminated frame
        // OR the upstream closes.
        let mut tmp = [0u8; 4096];
        loop {
            if let Some(idx) = self.buf.find("\n\n") {
                let frame = self.buf[..idx].to_owned();
                self.buf.drain(..idx + 2);
                if let Some(parsed) = parse_sse_frame(&frame) {
                    return Some(parsed);
                }
                // Heartbeat or unknown frame — keep going.
                continue;
            }
            match self.reader.read(&mut tmp) {
                Ok(0) => return None,
                Ok(n) => self.buf.push_str(&String::from_utf8_lossy(&tmp[..n])),
                Err(e) => return Some(Err(ClientError::Http(e.to_string()))),
            }
        }
    }
}

/// Parse one SSE frame. Returns `None` for keepalives / unrecognized
/// frames; `Some(Err)` if the data line is malformed JSON.
fn parse_sse_frame(frame: &str) -> Option<Result<EventEnvelope, ClientError>> {
    let mut data: Option<&str> = None;
    let mut event_kind: Option<&str> = None;
    for line in frame.lines() {
        if line.starts_with(':') {
            return None; // comment / keepalive
        }
        if let Some(rest) = line.strip_prefix("event: ") {
            event_kind = Some(rest);
        } else if let Some(rest) = line.strip_prefix("data: ") {
            data = Some(rest);
        }
    }
    let data = data?;
    let env: EventEnvelope = match serde_json::from_str(data) {
        Ok(e) => e,
        Err(e) => return Some(Err(ClientError::EventStream(e.to_string()))),
    };
    // Sanity: kind in envelope should match `event:` line if both set.
    if let Some(k) = event_kind {
        if let Some(parsed) = EventKind::from_str(k) {
            if parsed != env.kind {
                return Some(Err(ClientError::EventStream(format!(
                    "kind mismatch: header={k}, body={:?}",
                    env.kind
                ))));
            }
        }
    }
    Some(Ok(env))
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            other => {
                out.push('%');
                out.push_str(&format!("{:02X}", other));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_strips_trailing_slash() {
        assert_eq!(Client::new("http://x:7/").base, "http://x:7");
        assert_eq!(Client::new("http://x:7").base, "http://x:7");
    }

    #[test]
    fn parse_sse_frame_returns_envelope() {
        let frame = "event: message_posted\n\
                     data: {\"kind\":\"message_posted\",\"space_id\":\"S\",\"topic_id\":\"T\"}";
        let parsed = parse_sse_frame(frame).expect("frame").expect("ok");
        assert_eq!(parsed.kind, EventKind::MessagePosted);
        assert_eq!(parsed.space_id, "S");
        assert_eq!(parsed.topic_id.as_deref(), Some("T"));
    }

    #[test]
    fn parse_sse_frame_skips_keepalive() {
        let frame = ": keepalive";
        assert!(parse_sse_frame(frame).is_none());
    }

    #[test]
    fn parse_sse_frame_returns_err_on_bad_json() {
        let frame = "event: message_posted\ndata: not-json";
        let r = parse_sse_frame(frame).expect("frame");
        assert!(matches!(r, Err(ClientError::EventStream(_))));
    }

    #[test]
    fn parse_sse_frame_detects_kind_mismatch() {
        let frame = "event: message_posted\n\
                     data: {\"kind\":\"message_edited\",\"space_id\":\"S\"}";
        let r = parse_sse_frame(frame).expect("frame");
        assert!(matches!(r, Err(ClientError::EventStream(_))));
    }

    #[test]
    fn url_encode_matches_server_side() {
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("HTTP_PROXY"), "HTTP_PROXY");
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
    }
}
