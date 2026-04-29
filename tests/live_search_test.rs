//! Live test for the Google Chat search endpoint.
//!
//! Architecture:
//!   - The search transport (HTTP POST to /_/DynamiteWebUi/data/batchexecute)
//!     runs through Chrome via `tab.evaluate(JS)`. Direct ureq is rejected
//!     by Google with HTTP 401 in this environment because the cookies
//!     extracted from headless_chrome don't carry the full browser
//!     fingerprint Google checks for batchexecute (Origin/Referer/UA mix).
//!   - Everything else — payload construction, response parsing, hit
//!     extraction — uses pure Rust via `tchat::platform::googlechat::search`.
//!
//! Run:
//!   cargo test --test live_search_test -- --ignored --nocapture

use tchat::platform::googlechat::{auth, search};

const QUERY: &str = "https_proxy";
/// Space to scope the in-conversation search to.
/// "Data & Analytics Community Chat - go/dseap"
const SCOPED_SPACE_ID: &str = "AAAA2kPVvto";

#[test]
#[ignore]
fn live_search_via_batchexecute() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::WARN)
        .try_init();

    eprintln!("\n=========================================================");
    eprintln!("  Google Chat search via batchexecute (Rust types + parse)");
    eprintln!("  query: \"{QUERY}\"");
    eprintln!("=========================================================\n");

    eprintln!("[auth] launching Chrome / loading cookies...");
    let tokens = auth::authenticate(None).expect("Chrome auth failed");
    let tab = tokens.get_tab().expect("no tab after auth");
    std::thread::sleep(std::time::Duration::from_secs(2));
    eprintln!("[auth] ✓ ready\n");

    // Build the SBNmJb payload as a JSON string in Rust, then pass it
    // through to the JS as a string literal. The batchexecute envelope
    // expects the payload to be a JSON-stringified array (positional 1
    // of each `[[[id, payload_string, null, "generic"]]]` triple).
    let payload_value = search::build_search_payload(QUERY);
    let payload_string = serde_json::to_string(&payload_value).unwrap();
    let payload_js_literal = serde_json::to_string(&payload_string).unwrap();

    let post_js = format!(
        r#"
    (async () => {{
        try {{
            const w = window.WIZ_global_data || {{}};
            const at = w['SNlM0e'] || '';
            if (!at) return JSON.stringify({{error: 'no at token'}});
            const payload = {payload_js_literal};
            const fReq = JSON.stringify([[["SBNmJb", payload, null, "generic"]]]);
            const body = 'f.req=' + encodeURIComponent(fReq) + '&at=' + encodeURIComponent(at);
            const resp = await fetch('/_/DynamiteWebUi/data/batchexecute?rpcids=SBNmJb&source-path=%2F&f.sid=-1&bl=boq_dynamite-frontend&hl=en&_reqid=' + Date.now(), {{
                method: 'POST',
                body: body,
                headers: {{'Content-Type': 'application/x-www-form-urlencoded;charset=UTF-8'}},
                credentials: 'include',
            }});
            const text = await resp.text();
            return JSON.stringify({{status: resp.status, body_len: text.length, body: text}});
        }} catch (e) {{ return JSON.stringify({{error: e.message}}); }}
    }})()
    "#
    );

    eprintln!("[search] POST batchexecute via Chrome fetch...");
    let started = std::time::Instant::now();
    let result = tab.evaluate(&post_js, true).expect("eval failed");
    let raw = result
        .value
        .and_then(|v| v.as_str().map(str::to_owned))
        .expect("empty eval result");
    let outer: serde_json::Value = serde_json::from_str(&raw).expect("parse outer");
    let status = outer.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
    let body = outer.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let body_len = outer.get("body_len").and_then(|v| v.as_u64()).unwrap_or(0);
    if status != 200 {
        panic!(
            "batchexecute HTTP {status}: {}",
            outer.get("error").unwrap_or(&serde_json::Value::Null)
        );
    }
    eprintln!(
        "[search] ✓ HTTP {status}, {body_len} bytes in {:?}",
        started.elapsed()
    );

    // Parse the response with the pure-Rust helper.
    let results = search::parse_batchexecute_response(body, "SBNmJb")
        .expect("parse_batchexecute_response failed");
    eprintln!("[search] ✓ {} hits parsed\n", results.hits.len());

    for hit in &results.hits {
        let kind = match hit.kind {
            1 => "DM ",
            2 => "Room",
            _ => "?   ",
        };
        let name = if hit.name.is_empty() {
            "(unnamed)"
        } else {
            &hit.name
        };
        eprintln!("    {kind}  {name}  [id={}]", hit.space_id);
    }

    if let Some(token) = &results.continuation {
        eprintln!("\n  continuation_token: {}…", &token[..token.len().min(48)]);
    }
    eprintln!();

    assert!(
        !results.hits.is_empty(),
        "expected at least one hit for {QUERY:?}"
    );

    // ── Phase 2: in-space (scoped) search ───────────────────────────
    eprintln!("\n=========================================================");
    eprintln!("  in-space search → space={SCOPED_SPACE_ID}");
    eprintln!("=========================================================\n");

    let scoped_payload = search::build_search_in_space_payload(QUERY, SCOPED_SPACE_ID, 2);
    let scoped_payload_string = serde_json::to_string(&scoped_payload).unwrap();
    let scoped_payload_js_lit = serde_json::to_string(&scoped_payload_string).unwrap();

    let scoped_post_js = format!(
        r#"
    (async () => {{
        try {{
            const w = window.WIZ_global_data || {{}};
            const at = w['SNlM0e'] || '';
            if (!at) return JSON.stringify({{error: 'no at token'}});
            const payload = {scoped_payload_js_lit};
            const fReq = JSON.stringify([[["SBNmJb", payload, null, "generic"]]]);
            const body = 'f.req=' + encodeURIComponent(fReq) + '&at=' + encodeURIComponent(at);
            const resp = await fetch('/_/DynamiteWebUi/data/batchexecute?rpcids=SBNmJb&source-path=%2F&f.sid=-1&bl=boq_dynamite-frontend&hl=en&_reqid=' + Date.now(), {{
                method: 'POST',
                body: body,
                headers: {{'Content-Type': 'application/x-www-form-urlencoded;charset=UTF-8'}},
                credentials: 'include',
            }});
            const text = await resp.text();
            return JSON.stringify({{status: resp.status, body_len: text.length, body: text}});
        }} catch (e) {{ return JSON.stringify({{error: e.message}}); }}
    }})()
    "#
    );

    eprintln!("[search-in-space] POST batchexecute via Chrome fetch...");
    let started2 = std::time::Instant::now();
    let result2 = tab.evaluate(&scoped_post_js, true).expect("eval failed");
    let raw2 = result2
        .value
        .and_then(|v| v.as_str().map(str::to_owned))
        .expect("empty eval result");
    let outer2: serde_json::Value = serde_json::from_str(&raw2).expect("parse outer");
    let status2 = outer2.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
    let body2 = outer2.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let body2_len = outer2.get("body_len").and_then(|v| v.as_u64()).unwrap_or(0);
    if status2 != 200 {
        panic!(
            "in-space batchexecute HTTP {status2}: {}",
            outer2.get("error").unwrap_or(&serde_json::Value::Null)
        );
    }
    eprintln!(
        "[search-in-space] ✓ HTTP {status2}, {body2_len} bytes in {:?}",
        started2.elapsed()
    );

    // Dump the inner payload JSON for offline structure analysis.
    let json_part = body2.trim_start_matches(")]}'").trim();
    if let Ok(outer3) = serde_json::from_str::<serde_json::Value>(json_part) {
        if let Some(payload_str) = outer3
            .as_array()
            .and_then(|fs| {
                fs.iter().find(|f| {
                    f.get(0).and_then(|v| v.as_str()) == Some("wrb.fr")
                        && f.get(1).and_then(|v| v.as_str()) == Some("SBNmJb")
                })
            })
            .and_then(|f| f.get(2).and_then(|v| v.as_str()))
        {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(payload_str) {
                let pretty = serde_json::to_string_pretty(&parsed).unwrap_or_default();
                let path = "/tmp/tchat_inspace_payload.json";
                let _ = std::fs::write(path, &pretty);
                eprintln!(
                    "[search-in-space] dumped inner payload → {path} ({} bytes)",
                    pretty.len()
                );
            }
        }
    }

    let scoped_results =
        search::parse_batchexecute_response(body2, "SBNmJb").expect("parse failed");
    eprintln!(
        "[search-in-space] ✓ {} hits parsed (scoped to {SCOPED_SPACE_ID})\n",
        scoped_results.hits.len()
    );

    for hit in &scoped_results.hits {
        let kind = match hit.kind {
            1 => "DM ",
            2 => "Room",
            _ => "?   ",
        };
        let name = if hit.name.is_empty() {
            "(unnamed)"
        } else {
            &hit.name
        };
        eprintln!("    {kind}  {name}  [id={}]", hit.space_id);
    }
    eprintln!();

    // Server-side scoping should NOT return hits from other spaces. Verify.
    for hit in &scoped_results.hits {
        assert_eq!(
            hit.space_id, SCOPED_SPACE_ID,
            "in-space search returned a hit from a different space: {:?}",
            hit
        );
    }

    eprintln!(
        "[search-in-space] {} message-hits across {} thread(s)",
        scoped_results
            .threads
            .iter()
            .map(|t| t.messages.len())
            .sum::<usize>(),
        scoped_results.threads.len()
    );

    eprintln!("\n  ── top 2 threads ──");
    for (i, t) in scoped_results.top_threads(2).iter().enumerate() {
        eprintln!(
            "\n  Thread #{} (topic_id={}, {} msg(s)):",
            i + 1,
            t.topic_id,
            t.messages.len()
        );
        for m in &t.messages {
            let snippet = if m.text.len() > 100 {
                format!("{}…", &m.text[..100])
            } else {
                m.text.clone()
            };
            eprintln!(
                "    [t={}] author={}  {snippet:?}",
                m.timestamp_usec, m.author_id
            );
        }
    }
    eprintln!();

    let _ = std::process::Command::new("pkill")
        .args(["-f", "Chrome.*tchat"])
        .output();
}
