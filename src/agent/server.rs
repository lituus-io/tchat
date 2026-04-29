//! HTTP daemon for the agent surface. Single-threaded request
//! dispatcher (tiny_http) over `Mutex<AgentApi>`. SSE handlers spawn
//! their own threads and hold an independent `Receiver<EventEnvelope>`
//! so they don't block the API mutex.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::agent::events::{drive_sse, EventBus, EventFilter};
use crate::agent::json::{
    AskRequest, ErrorResponse, EventKind, HealthResponse, ReplyRequest, SearchResponse,
    SpacesResponse,
};
use crate::agent::AgentApi;
use crate::error::AppError;

/// Shared state across handlers.
pub struct ServerState {
    pub api: Mutex<AgentApi>,
    pub bus: Arc<EventBus>,
    pub idempotency: Mutex<HashMap<String, (u16, String)>>,
}

impl ServerState {
    pub fn new(api: AgentApi) -> Self {
        let bus = api.bus();
        Self {
            api: Mutex::new(api),
            bus,
            idempotency: Mutex::new(HashMap::new()),
        }
    }
}

/// Run the HTTP daemon, blocking until the OS terminates the process or
/// the listener errors out.
pub fn run(addr: &str, state: Arc<ServerState>) -> Result<(), AppError> {
    let server = Server::http(addr)
        .map_err(|e| AppError::Auth(crate::error::AuthError::Http(format!("bind {addr}: {e}"))))?;
    eprintln!("[serve] tchat agent daemon listening on http://{addr}");

    for request in server.incoming_requests() {
        let state = Arc::clone(&state);
        // Each request runs on its own thread — SSE holds the connection
        // open for a long time, so we don't want to block the accept loop.
        std::thread::spawn(move || {
            if let Err(e) = dispatch(request, state) {
                tracing::warn!("[serve] handler error: {e}");
            }
        });
    }
    Ok(())
}

fn dispatch(request: Request, state: Arc<ServerState>) -> std::io::Result<()> {
    let path = request.url().to_owned();
    let method = request.method().clone();
    let method_str = method.as_str().to_owned();
    let (path_only, query) = match path.split_once('?') {
        Some((p, q)) => (p, q),
        None => (path.as_str(), ""),
    };
    let segs: Vec<&str> = path_only.trim_start_matches('/').split('/').collect();

    match (method, segs.as_slice()) {
        (Method::Get, ["v1", "health"]) => handle_health(request, &state),
        (Method::Get, ["v1", "spaces"]) => handle_list_spaces(request, &state),
        (Method::Post, ["v1", "spaces", space_id, "questions"]) => {
            let s = (*space_id).to_owned();
            handle_ask(request, &state, &s)
        }
        (Method::Get, ["v1", "spaces", space_id, "threads", "search"]) => {
            let s = (*space_id).to_owned();
            handle_search(request, &state, &s, query)
        }
        (Method::Post, ["v1", "threads", topic_id, "reply"]) => {
            let t = (*topic_id).to_owned();
            handle_reply(request, &state, &t)
        }
        (Method::Get, ["v1", "events"]) => handle_events(request, &state, query),
        _ => respond_json(
            request,
            404,
            &ErrorResponse {
                error: "not_found".into(),
                detail: Some(format!("no route for {method_str} {path_only}")),
            },
        ),
    }
}

// ───────── handlers ─────────

fn handle_health(request: Request, state: &ServerState) -> std::io::Result<()> {
    let api = state.api.lock();
    let resp = HealthResponse {
        status: "ok".into(),
        auth: "valid".into(),
        self_user_id: api.self_user_id().map(str::to_owned),
        uptime_secs: api.started_at().elapsed().as_secs(),
    };
    respond_json(request, 200, &resp)
}

fn handle_list_spaces(request: Request, state: &ServerState) -> std::io::Result<()> {
    let mut api = state.api.lock();
    match api.list_spaces() {
        Ok(spaces) => respond_json(request, 200, &SpacesResponse { spaces }),
        Err(e) => respond_error(request, e),
    }
}

fn handle_ask(mut request: Request, state: &ServerState, space_id: &str) -> std::io::Result<()> {
    let body = match read_body(&mut request) {
        Ok(b) => b,
        Err(e) => return respond_bad_request(request, &format!("read body: {e}")),
    };
    let req: AskRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return respond_bad_request(request, &format!("parse JSON: {e}")),
    };

    let idem_key = req
        .idempotency_key
        .as_ref()
        .map(|k| format!("ask:{space_id}:{k}"));
    if let Some(key) = idem_key.as_ref() {
        if let Some((code, body)) = state.idempotency.lock().get(key).cloned() {
            return respond_raw(request, code, body);
        }
    }

    let mut api = state.api.lock();
    match api.post_question(space_id, &req.text, req.idempotency_key.as_deref()) {
        Ok(resp) => {
            let body = serde_json::to_string(&resp).unwrap_or_default();
            if let Some(key) = idem_key {
                state.idempotency.lock().insert(key, (201, body.clone()));
            }
            respond_raw(request, 201, body)
        }
        Err(e) => respond_error(request, e),
    }
}

fn handle_search(
    request: Request,
    state: &ServerState,
    space_id: &str,
    query_str: &str,
) -> std::io::Result<()> {
    let qs = parse_query(query_str);
    let q = match qs.get("q") {
        Some(v) if !v.is_empty() => v.clone(),
        _ => return respond_bad_request(request, "missing required query param `q`"),
    };
    let top: usize = qs.get("top").and_then(|v| v.parse().ok()).unwrap_or(2);

    let mut api = state.api.lock();
    match api.search_threads(space_id, &q, top) {
        Ok(resp) => respond_json::<SearchResponse>(request, 200, &resp),
        Err(e) => respond_error(request, e),
    }
}

fn handle_reply(mut request: Request, state: &ServerState, topic_id: &str) -> std::io::Result<()> {
    let body = match read_body(&mut request) {
        Ok(b) => b,
        Err(e) => return respond_bad_request(request, &format!("read body: {e}")),
    };
    let req: ReplyRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return respond_bad_request(request, &format!("parse JSON: {e}")),
    };
    if req.space_id.is_empty() {
        return respond_bad_request(request, "missing `space_id`");
    }

    let idem_key = req
        .idempotency_key
        .as_ref()
        .map(|k| format!("reply:{topic_id}:{k}"));
    if let Some(key) = idem_key.as_ref() {
        if let Some((code, body)) = state.idempotency.lock().get(key).cloned() {
            return respond_raw(request, code, body);
        }
    }

    let mut api = state.api.lock();
    match api.reply_in_thread(
        &req.space_id,
        topic_id,
        &req.text,
        req.idempotency_key.as_deref(),
    ) {
        Ok(resp) => {
            let body = serde_json::to_string(&resp).unwrap_or_default();
            if let Some(key) = idem_key {
                state.idempotency.lock().insert(key, (201, body.clone()));
            }
            respond_raw(request, 201, body)
        }
        Err(e) => respond_error(request, e),
    }
}

fn handle_events(request: Request, state: &ServerState, query_str: &str) -> std::io::Result<()> {
    let qs = parse_query(query_str);
    let mut filter = EventFilter::default();
    if let Some(v) = qs.get("space_id") {
        if !v.is_empty() {
            filter.space_id = Some(v.clone());
        }
    }
    if let Some(v) = qs.get("topic_id") {
        if !v.is_empty() {
            filter.topic_id = Some(v.clone());
        }
    }
    if let Some(v) = qs.get("kinds") {
        for k in v.split(',') {
            if let Some(kind) = EventKind::from_str(k.trim()) {
                filter.kinds.push(kind);
            }
        }
    }

    // Subscribe BEFORE we hijack the writer so any events that arrive
    // mid-handshake are queued, not dropped.
    let rx = state.bus.subscribe();

    // tiny_http: hijack the underlying socket and stream SSE chunks.
    // `into_writer()` returns the boxed write half directly (not a
    // Result), so we just take it.
    let writer = request.into_writer();
    let mut buffered = std::io::BufWriter::new(writer);
    write!(
        buffered,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nAccess-Control-Allow-Origin: *\r\n\r\n"
    )?;
    buffered.flush()?;

    drive_sse(rx, filter, buffered, None)
}

// ───────── responders ─────────

fn respond_json<T: serde::Serialize>(
    request: Request,
    code: u16,
    payload: &T,
) -> std::io::Result<()> {
    let body = serde_json::to_string(payload).unwrap_or_default();
    respond_raw(request, code, body)
}

fn respond_raw(request: Request, code: u16, body: String) -> std::io::Result<()> {
    let resp = Response::from_string(body)
        .with_status_code(code)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
    request.respond(resp)
}

fn respond_bad_request(request: Request, detail: &str) -> std::io::Result<()> {
    respond_json(
        request,
        400,
        &ErrorResponse {
            error: "bad_request".into(),
            detail: Some(detail.to_owned()),
        },
    )
}

fn respond_error(request: Request, e: AppError) -> std::io::Result<()> {
    use crate::error::AuthError;
    let (code, error_kind) = match &e {
        AppError::Auth(AuthError::SessionFetch(s))
            if s.contains("HTTP 401") || s.contains("expired") =>
        {
            (503, "auth_expired")
        }
        AppError::Auth(_) => (503, "auth_error"),
        AppError::Api(_) => (502, "upstream_error"),
        AppError::Config(_) => (500, "config_error"),
        AppError::Terminal(_) => (500, "io_error"),
        AppError::AllDisconnected => (503, "disconnected"),
    };
    respond_json(
        request,
        code,
        &ErrorResponse {
            error: error_kind.to_owned(),
            detail: Some(e.to_string()),
        },
    )
}

fn read_body(request: &mut Request) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    request.as_reader().read_to_end(&mut buf)?;
    Ok(buf)
}

/// Parse a query string `a=b&c=d` into a `HashMap`. URL-decodes `%xx`
/// escapes and `+` (since browsers form-encode that way).
fn parse_query(s: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in s.split('&').filter(|p| !p.is_empty()) {
        if let Some((k, v)) = pair.split_once('=') {
            map.insert(url_decode(k), url_decode(v));
        } else {
            map.insert(url_decode(pair), String::new());
        }
    }
    map
}

fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        match b {
            b'+' => out.push(' '),
            b'%' => {
                let h1 = bytes.next();
                let h2 = bytes.next();
                match (h1, h2) {
                    (Some(a), Some(b)) => {
                        if let (Some(av), Some(bv)) = (hex_val(a), hex_val(b)) {
                            out.push((av << 4 | bv) as char);
                        } else {
                            out.push('%');
                            out.push(a as char);
                            out.push(b as char);
                        }
                    }
                    (Some(a), None) => {
                        out.push('%');
                        out.push(a as char);
                    }
                    _ => out.push('%'),
                }
            }
            other => out.push(other as char),
        }
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Helper for the start time used by the `health` handler.
#[allow(dead_code)]
pub(crate) fn now() -> Instant {
    Instant::now()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_extracts_pairs() {
        let q = parse_query("q=hello&top=2");
        assert_eq!(q.get("q").map(String::as_str), Some("hello"));
        assert_eq!(q.get("top").map(String::as_str), Some("2"));
    }

    #[test]
    fn parse_query_url_decodes() {
        let q = parse_query("q=hello%20world&kinds=message_posted%2Cmessage_edited");
        assert_eq!(q.get("q").map(String::as_str), Some("hello world"));
        assert_eq!(
            q.get("kinds").map(String::as_str),
            Some("message_posted,message_edited")
        );
    }

    #[test]
    fn parse_query_handles_plus_as_space() {
        let q = parse_query("q=hi+there");
        assert_eq!(q.get("q").map(String::as_str), Some("hi there"));
    }

    #[test]
    fn parse_query_handles_empty_value() {
        let q = parse_query("flag=&q=ok");
        assert_eq!(q.get("flag").map(String::as_str), Some(""));
        assert_eq!(q.get("q").map(String::as_str), Some("ok"));
    }

    #[test]
    fn parse_query_handles_no_value() {
        let q = parse_query("flag&q=ok");
        assert_eq!(q.get("flag").map(String::as_str), Some(""));
        assert_eq!(q.get("q").map(String::as_str), Some("ok"));
    }

    #[test]
    fn parse_query_empty_returns_empty_map() {
        let q = parse_query("");
        assert!(q.is_empty());
    }

    #[test]
    fn url_decode_handles_malformed_percent() {
        assert_eq!(url_decode("a%2"), "a%2");
        assert_eq!(url_decode("%XX"), "%XX");
    }
}
