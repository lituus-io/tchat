//! End-to-end agent HTTP test.
//!
//! Boots `AgentApi` in-process, runs the HTTP daemon on an ephemeral
//! port, then exercises every endpoint with `ureq`. Asserts the
//! response shapes round-trip through serde and the actual chat
//! actions land on the test space.
//!
//! Run:
//!   cargo test --test agent_http_test -- --ignored --nocapture

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use tchat::agent::client::Client;
use tchat::agent::events::EventFilter;
use tchat::agent::json::{
    AskRequest, AskResponse, EventKind, HealthResponse, ReplyRequest, ReplyResponse,
    SearchResponse, SpacesResponse,
};
use tchat::agent::server::ServerState;
use tchat::agent::AgentApi;

const TEST_SPACE_ID: &str = "AAQAJuwMi-4";

#[test]
#[ignore]
fn agent_http_end_to_end() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::WARN)
        .try_init();

    eprintln!("\n========================================================");
    eprintln!("  agent HTTP end-to-end (space {TEST_SPACE_ID})");
    eprintln!("========================================================\n");

    eprintln!("[setup] bootstrapping AgentApi (Chrome auth chain)...");
    let api = AgentApi::bootstrap().expect("AgentApi::bootstrap");
    let state = Arc::new(ServerState::new(api));

    // Pick a port and start the daemon on a worker thread.
    let port = 17800u16;
    let addr = format!("127.0.0.1:{port}");
    let state_for_thread = Arc::clone(&state);
    let addr_for_thread = addr.clone();
    std::thread::spawn(move || {
        let _ = tchat::agent::server::run(&addr_for_thread, state_for_thread);
    });
    std::thread::sleep(Duration::from_millis(500));
    eprintln!("[setup] ✓ daemon listening on http://{addr}\n");

    let base = format!("http://{addr}");

    // ── /v1/health ──────────────────────────────────────────────────
    eprintln!("[GET ] /v1/health");
    let health: HealthResponse = http_get(&format!("{base}/v1/health"));
    assert_eq!(health.status, "ok");
    eprintln!(
        "       ✓ status=ok auth={} self={:?}",
        health.auth, health.self_user_id
    );

    // ── /v1/spaces ──────────────────────────────────────────────────
    eprintln!("\n[GET ] /v1/spaces");
    let spaces: SpacesResponse = http_get(&format!("{base}/v1/spaces"));
    eprintln!("       ✓ {} spaces", spaces.spaces.len());
    let test_space_present = spaces.spaces.iter().any(|s| s.id == TEST_SPACE_ID);
    if !test_space_present {
        eprintln!("       (note: {TEST_SPACE_ID} not in list — continuing anyway)");
    }

    // ── POST /v1/spaces/{id}/questions ──────────────────────────────
    eprintln!("\n[POST] /v1/spaces/{TEST_SPACE_ID}/questions");
    let q_text = format!("agent-http-test question @ {}", short_ts());
    let ask: AskResponse = http_post(
        &format!("{base}/v1/spaces/{TEST_SPACE_ID}/questions"),
        &AskRequest {
            text: q_text.clone(),
            idempotency_key: None,
        },
    );
    eprintln!(
        "       ✓ topic_id={} message_id={}",
        ask.topic_id, ask.message_id
    );
    assert!(!ask.topic_id.is_empty());
    assert!(!ask.message_id.is_empty());

    // ── GET /v1/spaces/{id}/threads/search ──────────────────────────
    std::thread::sleep(Duration::from_secs(2)); // index propagation
    eprintln!("\n[GET ] /v1/spaces/{TEST_SPACE_ID}/threads/search?q=agent-http-test&top=2");
    let search: SearchResponse = http_get(&format!(
        "{base}/v1/spaces/{TEST_SPACE_ID}/threads/search?q=agent-http-test&top=2"
    ));
    eprintln!("       ✓ {} threads", search.threads.len());
    for t in &search.threads {
        eprintln!("         topic={} ({} msgs)", t.topic_id, t.messages.len());
    }

    // ── POST /v1/threads/{topic}/reply ──────────────────────────────
    eprintln!("\n[POST] /v1/threads/{}/reply", ask.topic_id);
    let reply: ReplyResponse = http_post(
        &format!("{base}/v1/threads/{}/reply", ask.topic_id),
        &ReplyRequest {
            space_id: TEST_SPACE_ID.into(),
            text: format!("agent-http-test reply @ {}", short_ts()),
            idempotency_key: None,
        },
    );
    eprintln!("       ✓ message_id={}", reply.message_id);
    assert!(!reply.message_id.is_empty());

    // ── Idempotency: same key → same response ───────────────────────
    eprintln!("\n[POST] /v1/spaces/{TEST_SPACE_ID}/questions  (idempotency check)");
    let key = format!("test-idem-{}", std::process::id());
    let body = AskRequest {
        text: format!("idempotency-test {key}"),
        idempotency_key: Some(key.clone()),
    };
    let first: AskResponse = http_post(
        &format!("{base}/v1/spaces/{TEST_SPACE_ID}/questions"),
        &body,
    );
    let second: AskResponse = http_post(
        &format!("{base}/v1/spaces/{TEST_SPACE_ID}/questions"),
        &body,
    );
    assert_eq!(first.topic_id, second.topic_id);
    assert_eq!(first.message_id, second.message_id);
    eprintln!("       ✓ both calls returned topic_id={}", first.topic_id);

    // ── SSE: subscribe + observe own message echo ───────────────────
    eprintln!("\n[GET ] /v1/events?space_id={TEST_SPACE_ID}&kinds=message_posted");
    let events_url = format!("{base}/v1/events?space_id={TEST_SPACE_ID}&kinds=message_posted");
    let saw_event = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saw_event_thread = std::sync::Arc::clone(&saw_event);
    let watcher = std::thread::spawn(move || {
        let resp = ureq::get(&events_url)
            .config()
            .timeout_global(Some(Duration::from_secs(20)))
            .build()
            .call()
            .expect("SSE GET");
        let mut buf = [0u8; 1024];
        let mut acc = String::new();
        let mut reader = resp.into_body().into_reader();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    acc.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if acc.contains("event: message_posted") {
                        saw_event_thread.store(true, std::sync::atomic::Ordering::Relaxed);
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Trigger a fresh post that the SSE subscriber should observe.
    std::thread::sleep(Duration::from_secs(1));
    let trigger_text = format!("agent-http-test SSE trigger @ {}", short_ts());
    let _: AskResponse = http_post(
        &format!("{base}/v1/spaces/{TEST_SPACE_ID}/questions"),
        &AskRequest {
            text: trigger_text,
            idempotency_key: None,
        },
    );
    let _ = watcher.join();
    let saw = saw_event.load(std::sync::atomic::Ordering::Relaxed);
    eprintln!(
        "       {} message_posted observed via SSE",
        if saw { "✓" } else { "✗" }
    );
    assert!(saw, "expected SSE to deliver MessagePosted within 15s");

    // ── 404 path ────────────────────────────────────────────────────
    eprintln!("\n[GET ] /v1/nonexistent");
    let url = format!("{base}/v1/nonexistent");
    let resp = ureq::get(&url).call();
    match resp {
        Err(ureq::Error::StatusCode(404)) => eprintln!("       ✓ 404 as expected"),
        Err(e) => panic!("unexpected error: {e}"),
        Ok(r) => panic!("expected 404, got {}", r.status()),
    }

    // ── typed Rust client mirrors the raw HTTP results ─────────────
    eprintln!("\n[client] full flow via tchat::agent::client::Client");
    let c = Client::new(&base);
    let h = c.health().expect("client.health");
    assert_eq!(h.status, "ok");
    let posted = c
        .ask(TEST_SPACE_ID, &format!("client-test {}", short_ts()), None)
        .expect("client.ask");
    eprintln!("       ✓ ask → topic_id={}", posted.topic_id);
    let s = c
        .search(TEST_SPACE_ID, "client-test", 1)
        .expect("client.search");
    eprintln!("       ✓ search → {} threads", s.threads.len());
    let r = c
        .reply(TEST_SPACE_ID, &posted.topic_id, "client-test reply", None)
        .expect("client.reply");
    eprintln!("       ✓ reply → message_id={}", r.message_id);

    // SSE iterator delivers at least one MessagePosted within ~15s.
    let stream = c
        .events(EventFilter {
            space_id: Some(TEST_SPACE_ID.into()),
            topic_id: None,
            kinds: vec![EventKind::MessagePosted],
        })
        .expect("client.events");
    let watcher_saw = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saw_clone = std::sync::Arc::clone(&watcher_saw);
    std::thread::spawn(move || {
        for env in stream.take(5).flatten() {
            if env.kind == EventKind::MessagePosted {
                saw_clone.store(true, std::sync::atomic::Ordering::Relaxed);
                break;
            }
        }
    });
    std::thread::sleep(Duration::from_millis(500));
    let _ = c.ask(
        TEST_SPACE_ID,
        &format!("client SSE trigger {}", short_ts()),
        None,
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        if watcher_saw.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        watcher_saw.load(std::sync::atomic::Ordering::Relaxed),
        "client SSE iterator should observe a MessagePosted within 15s",
    );
    eprintln!("       ✓ events stream delivered MessagePosted");

    eprintln!("\n========================================================");
    eprintln!("  ✓ agent HTTP end-to-end OK (raw + typed client)");
    eprintln!("========================================================\n");

    let _ = std::process::Command::new("pkill")
        .args(["-f", "Chrome.*tchat"])
        .output();
}

// ───────── helpers ─────────

fn http_get<T: for<'de> serde::Deserialize<'de>>(url: &str) -> T {
    let resp = ureq::get(url).call().expect("GET");
    let mut text = String::new();
    resp.into_body()
        .into_reader()
        .read_to_string(&mut text)
        .expect("read");
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("parse {url}: {e}\nbody: {}", &text[..text.len().min(500)]))
}

fn http_post<R, T>(url: &str, body: &T) -> R
where
    T: serde::Serialize,
    R: for<'de> serde::Deserialize<'de>,
{
    let body_str = serde_json::to_string(body).expect("serialize");
    let resp = ureq::post(url)
        .header("Content-Type", "application/json")
        .send(body_str.as_bytes())
        .expect("POST");
    let mut text = String::new();
    resp.into_body()
        .into_reader()
        .read_to_string(&mut text)
        .expect("read");
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("parse {url}: {e}\nbody: {}", &text[..text.len().min(500)]))
}

fn short_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
