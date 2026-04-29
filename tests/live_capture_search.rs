//! Capture Google Chat's search wire format by hooking window.fetch
//! in the live web client. Auth via Chrome → inject fetch interceptor →
//! poll the captured-calls buffer every 2s → print each /api/* call's
//! endpoint name, request size, and base64-encoded body.
//!
//! Drive it: type a search query in the Chrome window. Whatever the web
//! client sends to the API will be captured and printed here.
//!
//! Run:
//!   cargo test --test live_capture_search -- --ignored --nocapture

use std::time::{Duration, Instant};

use tchat::platform::googlechat::{auth, session::Session};

#[test]
#[ignore]
fn capture_search_endpoint() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let secs: u64 = std::env::var("TCHAT_CAPTURE_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);

    eprintln!("\n=========================================================");
    eprintln!("  Google Chat search-endpoint capture");
    eprintln!("  duration: {secs}s");
    eprintln!("  → type a search query in the Chrome window to capture");
    eprintln!("=========================================================\n");

    let tokens = auth::authenticate(None).expect("auth failed");
    let session = Session::new(tokens);
    let tab = session.tokens.get_tab().expect("no tab");
    eprintln!("[auth] ✓ tab on {}", tab.get_url());

    // Inject hooks for both fetch() AND XMLHttpRequest. The Chat SPA uses
    // XHR for internal API calls (its service worker intercepts fetch).
    // Both hooks push captured calls into `__tchat_captured__`.
    let install_js = r#"
    (() => {
        if (window.__tchat_capture_installed__) return "already installed";
        window.__tchat_capture_installed__ = true;
        window.__tchat_captured__ = [];

        const push = (rec) => {
            window.__tchat_captured__.push(rec);
            if (window.__tchat_captured__.length > 200) {
                window.__tchat_captured__.shift();
            }
        };

        const encode = (body) => {
            try {
                if (body instanceof ArrayBuffer) {
                    const bytes = new Uint8Array(body);
                    let bin = '';
                    for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
                    return { size: bytes.length, b64: btoa(bin) };
                }
                if (body instanceof Uint8Array) {
                    let bin = '';
                    for (let i = 0; i < body.length; i++) bin += String.fromCharCode(body[i]);
                    return { size: body.length, b64: btoa(bin) };
                }
                if (typeof body === 'string') {
                    return { size: body.length, b64: btoa(unescape(encodeURIComponent(body.substring(0, 4096)))) };
                }
                if (body && body.constructor && body.constructor.name === 'Blob') {
                    return { size: body.size, b64: '(blob)' };
                }
            } catch(e) {}
            return { size: 0, b64: '' };
        };

        // fetch hook
        const origFetch = window.fetch.bind(window);
        window.fetch = async function(input, init) {
            const url = typeof input === 'string' ? input : input.url;
            const body = init && init.body ? init.body : null;
            const enc = encode(body);
            const resp = await origFetch(input, init);
            // Capture every same-origin request — search may use a path
            // outside /api/ (e.g. cloudsearch). Filter out high-volume
            // noise from the BC streaming endpoint.
            if (url && !url.includes('/webchannel/events')
                    && !url.includes('beacons.gcp.gvt2')
                    && (url.startsWith('https://chat.google.com')
                        || url.startsWith('/'))) {
                push({
                    t: Date.now(),
                    transport: 'fetch',
                    url: url,
                    method: (init && init.method) || 'GET',
                    body_size: enc.size,
                    body_b64: enc.b64,
                    status: resp.status,
                });
            }
            return resp;
        };

        // XHR hook
        const origOpen = XMLHttpRequest.prototype.open;
        const origSend = XMLHttpRequest.prototype.send;
        XMLHttpRequest.prototype.open = function(method, url) {
            this.__tchat_method__ = method;
            this.__tchat_url__ = url;
            return origOpen.apply(this, arguments);
        };
        XMLHttpRequest.prototype.send = function(body) {
            const xhr = this;
            const url = xhr.__tchat_url__ || '';
            const method = xhr.__tchat_method__ || 'GET';
            const enc = encode(body);
            xhr.addEventListener('load', function() {
                if (url && !url.includes('/webchannel/events')
                        && (url.startsWith('https://chat.google.com')
                            || url.startsWith('/'))) {
                    push({
                        t: Date.now(),
                        transport: 'xhr',
                        url: url,
                        method: method,
                        body_size: enc.size,
                        body_b64: enc.b64,
                        status: xhr.status,
                    });
                }
            });
            return origSend.apply(this, arguments);
        };

        return "installed (fetch + xhr)";
    })()
    "#;

    let install_result = tab.evaluate(install_js, false).expect("inject hook failed");
    eprintln!(
        "[hook] {}",
        install_result
            .value
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    );
    eprintln!("[hook] now type a search query in the Chrome window");
    eprintln!("       (click the magnifying-glass / search bar)\n");

    let drain_js = r#"
    (() => {
        const out = window.__tchat_captured__ || [];
        window.__tchat_captured__ = [];
        return JSON.stringify(out);
    })()
    "#;

    let mut endpoints_seen = std::collections::HashMap::<String, u32>::new();
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(2));
        let result = match tab.evaluate(drain_js, false) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[drain] error: {e}");
                continue;
            }
        };
        let json = match result.value.and_then(|v| v.as_str().map(str::to_owned)) {
            Some(s) => s,
            None => continue,
        };
        let calls: Vec<serde_json::Value> = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if calls.is_empty() {
            continue;
        }
        for call in calls {
            let url = call.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let method = call.get("method").and_then(|v| v.as_str()).unwrap_or("");
            let size = call.get("body_size").and_then(|v| v.as_u64()).unwrap_or(0);
            let status = call.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
            let body_b64 = call.get("body_b64").and_then(|v| v.as_str()).unwrap_or("");

            // Highlight anything that looks like search/find/query, plus
            // any non-/api/ path (those are the interesting candidates).
            let lower = url.to_lowercase();
            let is_search_keyword =
                lower.contains("search") || lower.contains("find") || lower.contains("query");
            let is_non_api = !url.contains("/api/");
            let path = url.split('?').next().unwrap_or(url);
            let endpoint = path.rsplit('/').next().unwrap_or("?");
            *endpoints_seen.entry(endpoint.to_owned()).or_insert(0) += 1;

            let mark = if is_search_keyword {
                "🔎"
            } else if is_non_api {
                "🆕"
            } else {
                "📡"
            };
            eprintln!("{mark} {method} {path}  status={status}  body={size}B");
            if size > 0 && body_b64.len() > 20 {
                eprintln!("    base64({size}B): {body_b64}");
            }
        }
    }

    eprintln!("\n=========================================================");
    eprintln!("  endpoints seen during capture window");
    eprintln!("=========================================================");
    let mut sorted: Vec<_> = endpoints_seen.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));
    for (ep, count) in sorted {
        eprintln!("  {count:>4}  {ep}");
    }
    eprintln!();
}
