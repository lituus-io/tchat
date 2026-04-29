//! Debug update_reaction using the dedicated Test - Gchat space.
//!
//! Run:  cargo test --test live_reaction_debug -- --ignored --nocapture

use prost::Message;
use tchat::platform::googlechat::{api, auth, convert, pblite, proto, session::Session};

const TEST_SPACE_ID: &str = "AAQAjslKeUE";
const TEST_SPACE_URL: &str = "https://chat.google.com/room/AAQAjslKeUE?cls=7";

#[test]
#[ignore]
fn debug_update_reaction() {
    eprintln!("\n========== update_reaction debugger (Test - Gchat) ==========\n");

    let tokens = auth::authenticate(None).expect("Auth failed");
    let mut session = Session::new(tokens);
    let _ = session.fetch_session_tokens();

    let tab = session.tokens.get_tab().expect("No tab");

    // Install XHR hook that captures ALL /api/ calls (not just react-URLs)
    // in case the reaction endpoint has a different name than expected.
    let hook = r#"
    window._rxn_cap = [];
    window._rxn_baseline = new Set();
    const _oOpen = XMLHttpRequest.prototype.open;
    const _oSend = XMLHttpRequest.prototype.send;
    const _oSetH = XMLHttpRequest.prototype.setRequestHeader;
    XMLHttpRequest.prototype.open = function(m, u, ...r) {
        this._u = u; this._m = m; this._h = {};
        return _oOpen.call(this, m, u, ...r);
    };
    XMLHttpRequest.prototype.setRequestHeader = function(k, v) {
        if (this._h) this._h[k] = v;
        return _oSetH.call(this, k, v);
    };
    XMLHttpRequest.prototype.send = function(body) {
        const u = this._u || '';
        if (u.includes('/api/')) {
            const endpoint = (u.match(/\/api\/([a-z_]+)/) || [])[1] || '?';
            const entry = {
                url: u,
                endpoint: endpoint,
                headers: Object.assign({}, this._h),
                body: body ? String(body) : null,
                time: Date.now()
            };
            this.addEventListener('load', function() {
                entry.status = this.status;
                entry.resp = String(this.response || '').substring(0, 200);
            });
            window._rxn_cap.push(entry);
        }
        return _oSend.call(this, body);
    };
    "#;
    let _ = tab.evaluate(hook, false);

    // Give time for the space to load and baseline calls to complete
    std::thread::sleep(std::time::Duration::from_secs(3));

    // Mark a baseline so we only report NEW calls made during user interaction
    let baseline_js = r#"
    (() => {
        window._rxn_baseline_count = (window._rxn_cap || []).length;
        return window._rxn_baseline_count;
    })()
    "#;
    let baseline = tab
        .evaluate(baseline_js, false)
        .ok()
        .and_then(|r| r.value)
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    eprintln!("  Baseline: {baseline} API calls before your action");

    // Navigate to the test space directly
    eprintln!("[1] Navigating to Test - Gchat space...");
    let nav_js = format!(
        r#"(() => {{ window.location.href = "{TEST_SPACE_URL}"; return 'navigating'; }})()"#
    );
    let _ = tab.evaluate(&nav_js, true);
    std::thread::sleep(std::time::Duration::from_secs(6));

    // Try to find ALL interactive elements in the page to understand the DOM
    let inventory_js = r#"
    (() => {
        const stats = {};
        const tagCounts = {};
        for (const tag of ['button', 'div', 'span']) {
            const elements = document.querySelectorAll(tag + '[role="button"]');
            tagCounts[tag] = elements.length;
        }
        // Look for any element with aria-label containing common emoji/react patterns
        const patterns = ['react', 'emoji', 'thumb', 'heart', 'Add', '👍', '❤', '😀'];
        const matches = {};
        for (const p of patterns) {
            matches[p] = [];
            const all = document.querySelectorAll('[aria-label]');
            for (const el of all) {
                const label = el.getAttribute('aria-label') || '';
                if (label.toLowerCase().includes(p.toLowerCase()) || label.includes(p)) {
                    matches[p].push({
                        tag: el.tagName,
                        role: el.getAttribute('role'),
                        label: label.substring(0, 80)
                    });
                    if (matches[p].length >= 3) break;
                }
            }
        }
        return JSON.stringify({
            buttonCounts: tagCounts,
            matches: matches
        });
    })()
    "#;
    if let Ok(r) = tab.evaluate(inventory_js, false) {
        if let Some(s) = r.value.and_then(|v| v.as_str().map(|s| s.to_owned())) {
            eprintln!("  DOM inventory: {}", &s[..s.len().min(2000)]);
        }
    }

    // Send a test message FIRST so there's something to react to
    eprintln!("\n  Sending a test message to react to...");
    let gid = proto::GroupId {
        space_id: Some(proto::SpaceId {
            space_id: Some(TEST_SPACE_ID.into()),
        }),
        dm_id: None,
    };
    let send = api::call_proto::<_, proto::CreateMessageResponse>(
        &mut session,
        "create_message",
        &proto::CreateMessageRequest {
            request_header: Some(convert::tests_make_header()),
            parent_id: Some(proto::MessageParentId {
                topic_id: Some(proto::TopicId {
                    group_id: Some(gid.clone()),
                    topic_id: None,
                }),
            }),
            text_body: Some("PLEASE REACT TO THIS WITH ANY EMOJI — tchat debug".into()),
            annotations: Vec::new(),
            local_id: Some(format!("tchat-react-{}", std::process::id())),
            message_id: None,
            message_info: Some(proto::MessageInfo {
                accept_format_annotations: Some(true),
                reply_to: None,
            }),
        },
    )
    .expect("send must work");
    let test_msg_id = send.message.and_then(|m| m.id).expect("id");
    let test_msg_str = test_msg_id.message_id.clone().unwrap_or_default();
    eprintln!("  Sent: {test_msg_str}");
    eprintln!("  In Chrome window, find this message and add a reaction.");

    eprintln!(
        "\n[2] MANUAL STEP: hover over the message above and click 'Add reaction' (smiley+)."
    );
    eprintln!("    Then pick any emoji. Polling for captured calls for up to 90 seconds...\n");

    let mut captured: Vec<serde_json::Value> = Vec::new();
    let mut last_count: usize = baseline as usize;
    for sec in 0..60 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if let Ok(r) = tab.evaluate("JSON.stringify(window._rxn_cap || [])", false) {
            if let Some(t) = r.value.and_then(|v| v.as_str().map(|s| s.to_owned())) {
                let all: Vec<serde_json::Value> = serde_json::from_str(&t).unwrap_or_default();
                if all.len() > last_count {
                    let new_count = all.len() - last_count;
                    eprintln!("  [{sec}s] +{new_count} new calls (total: {})", all.len());
                    // Print what's new
                    for cap in all.iter().skip(last_count) {
                        let endpoint = cap.get("endpoint").and_then(|v| v.as_str()).unwrap_or("?");
                        let status = cap.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
                        eprintln!("    -> {endpoint} (status={status})");
                    }
                    last_count = all.len();
                    captured = all.into_iter().skip(baseline as usize).collect();
                }
                // Stop early if we got a reaction call
                if captured.iter().any(|c| {
                    c.get("endpoint")
                        .and_then(|v| v.as_str())
                        .map(|e| e.contains("react"))
                        .unwrap_or(false)
                }) {
                    eprintln!("  Got reaction call!");
                    break;
                }
            }
        }
    }

    eprintln!("\n[3] Captured {} calls:", captured.len());
    let mut real_reaction_body: Option<String> = None;
    for (i, cap) in captured.iter().enumerate() {
        let url = cap.get("url").and_then(|v| v.as_str()).unwrap_or("?");
        let status = cap.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
        let endpoint = url
            .split("/api/")
            .nth(1)
            .and_then(|s| s.split('?').next())
            .unwrap_or("?");
        eprintln!("  [{i}] {endpoint} status={status}");
        if let Some(body) = cap.get("body").and_then(|v| v.as_str()) {
            eprintln!(
                "       body ({} chars): {}",
                body.len(),
                &body[..body.len().min(800)]
            );
            if url.contains("react") {
                real_reaction_body = Some(body.to_string());
            }
        }
        if let Some(headers) = cap.get("headers").and_then(|v| v.as_object()) {
            for (k, v) in headers {
                eprintln!("       header {k}: {}", v.as_str().unwrap_or("?"));
            }
        }
    }

    // If we got a real reaction body, compare with ours and replay
    if let Some(ref real_body) = real_reaction_body {
        eprintln!("\n[4] Comparing real vs ours...");

        // Parse structure
        if let Ok(real_json) = serde_json::from_str::<serde_json::Value>(real_body) {
            eprintln!("  Real body structure:");
            describe(&real_json, "    ", 0);
        }

        // Build our request with the same message_id
        // (we'll need to parse the real body to get a message_id)
        eprintln!("\n[5] Try replaying the EXACT real body via our code path...");
        let xsrf = session.xsrf_token.clone().unwrap_or_default();
        let counter = session.next_api_counter();
        use base64::Engine;
        let body_b64 = base64::engine::general_purpose::STANDARD.encode(real_body.as_bytes());
        let replay_js = format!(
            r#"(async () => {{
            try {{
                const b = atob("{body_b64}");
                const resp = await fetch("https://chat.google.com/u/0/api/update_reaction?c={counter}", {{
                    method: 'POST', credentials: 'include',
                    headers: {{
                        'Content-Type': 'application/json',
                        'X-Goog-AuthUser': '0',
                        'X-Framework-Xsrf-Token': '{xsrf}',
                        'Accept-Language': 'en'
                    }},
                    body: b
                }});
                const t = await resp.text();
                return JSON.stringify({{status: resp.status, body: t.substring(0, 200)}});
            }} catch(e) {{ return JSON.stringify({{error: e.message}}); }}
        }})()"#
        );
        if let Ok(r) = tab.evaluate(&replay_js, true) {
            if let Some(s) = r.value.and_then(|v| v.as_str().map(|s| s.to_owned())) {
                eprintln!("    Replay: {s}");
            }
        }
    } else {
        eprintln!("\n  No real reaction captured — proceeding with our test.");
    }

    // ── Now try our code against the test space ──
    eprintln!("\n[6] Our update_reaction against Test - Gchat...");
    let req = proto::UpdateReactionRequest {
        request_header: Some(convert::tests_make_header()),
        message_id: Some(test_msg_id.clone()),
        emoji: Some(proto::Emoji {
            unicode: Some("👍".into()),
            custom_emoji: None,
        }),
        option: Some(1),
    };
    let wire = req.encode_to_vec();
    let our_pb = pblite::wire_to_pblite(&wire).unwrap();
    let our_body = serde_json::to_string(&our_pb).unwrap();
    eprintln!(
        "  Our pblite ({} chars): {}",
        our_body.len(),
        &our_body[..our_body.len().min(500)]
    );

    match api::call_proto::<_, proto::UpdateReactionResponse>(&mut session, "update_reaction", &req)
    {
        Ok(r) => {
            let rev = r
                .group_revision
                .as_ref()
                .and_then(|r| r.timestamp)
                .unwrap_or(0);
            eprintln!("  OK ✓ group_revision={rev}");
        }
        Err(e) => {
            let s = e.to_string();
            let brief = if s.len() > 150 { &s[..150] } else { &s };
            eprintln!("  FAILED: {brief}");
        }
    }

    // Cleanup
    let _ = api::call_proto::<_, proto::DeleteMessageResponse>(
        &mut session,
        "delete_message",
        &proto::DeleteMessageRequest {
            request_header: Some(convert::tests_make_header()),
            message_id: Some(test_msg_id),
        },
    );
    eprintln!("\n[7] Cleanup done.");
}

fn describe(value: &serde_json::Value, prefix: &str, depth: usize) {
    if depth > 5 {
        return;
    }
    match value {
        serde_json::Value::Array(arr) => {
            eprintln!("{prefix}Array[{}]", arr.len());
            for (i, item) in arr.iter().enumerate() {
                if item.is_null() {
                    continue;
                }
                eprint!("{prefix}  [{i}]: ");
                describe(item, &format!("{prefix}  "), depth + 1);
            }
        }
        serde_json::Value::String(s) => {
            let shown = if s.len() > 60 {
                format!("{}...", &s[..60])
            } else {
                s.clone()
            };
            eprintln!("\"{shown}\" ({} chars)", s.len());
        }
        serde_json::Value::Number(n) => eprintln!("{n}"),
        serde_json::Value::Bool(b) => eprintln!("{b}"),
        serde_json::Value::Null => eprintln!("null"),
        serde_json::Value::Object(_) => eprintln!("Object"),
    }
}
