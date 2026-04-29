//! Phase 1 — capture real web client format for catch_up_group and update_reaction.
//!
//! Run:  cargo test --test live_phase1_test -- --ignored --nocapture

use prost::Message;
use tchat::platform::googlechat::{api, auth, convert, pblite, proto, session::Session};

#[test]
#[ignore]
fn live_phase1_capture() {
    eprintln!("\n========== Phase 1: Capture real requests ==========\n");

    eprintln!("[1] Authenticating...");
    let tokens = auth::authenticate(None).expect("Auth failed");
    let mut session = Session::new(tokens);
    let _ = session.fetch_session_tokens();

    let tab = session.tokens.get_tab().expect("No tab");

    // Install XHR hook to capture requests
    let hook = r#"
    window._cap2 = [];
    const _oOpen = XMLHttpRequest.prototype.open;
    const _oSend = XMLHttpRequest.prototype.send;
    const _oSetH = XMLHttpRequest.prototype.setRequestHeader;
    XMLHttpRequest.prototype.open = function(m, u, ...r) {
        this._m = m; this._u = u; this._h = {};
        return _oOpen.call(this, m, u, ...r);
    };
    XMLHttpRequest.prototype.setRequestHeader = function(k, v) {
        if (this._h) this._h[k] = v;
        return _oSetH.call(this, k, v);
    };
    XMLHttpRequest.prototype.send = function(body) {
        const u = this._u || '';
        if (u.includes('/api/')) {
            const entry = {
                url: u,
                body: body ? String(body) : null,
                time: Date.now()
            };
            this.addEventListener('load', function() {
                entry.status = this.status;
            });
            window._cap2.push(entry);
        }
        return _oSend.call(this, body);
    };
    "#;
    let _ = tab.evaluate(hook, false);

    std::thread::sleep(std::time::Duration::from_secs(5));

    // Get a space — retry for SPA load
    let dom_js = r#"(() => {
        const el = document.querySelector('[data-group-id]');
        return el ? el.getAttribute('data-group-id').replace(/^space\//, '').replace(/^dm\//, '') : null;
    })()"#;
    let mut space_id = String::new();
    for attempt in 0..10 {
        if let Ok(r) = tab.evaluate(dom_js, false) {
            if let Some(s) = r.value.and_then(|v| v.as_str().map(|s| s.to_owned())) {
                space_id = s;
                break;
            }
        }
        eprintln!("  Waiting for SPA ({})...", attempt + 1);
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
    assert!(!space_id.is_empty(), "No space found in DOM");
    eprintln!("  Space: {space_id}");

    // ── Click the space to trigger API calls (including catch_up_group if used) ──
    eprintln!("\n[2] Clicking space to trigger real API traffic...");
    let click_js = format!(
        r#"
    (() => {{
        const items = document.querySelectorAll('[data-group-id]');
        for (const item of items) {{
            if (item.getAttribute('data-group-id').includes('{space_id}')) {{
                item.click();
                return 'clicked';
            }}
        }}
        return 'not found';
    }})()
    "#
    );
    let _ = tab.evaluate(&click_js, true);
    std::thread::sleep(std::time::Duration::from_secs(4));

    // ── Pause so user can manually click a reaction to capture format ──
    eprintln!(
        "\n[3] MANUAL STEP: In the Chrome window, click a reaction (👍 etc.) on any message."
    );
    eprintln!("    Waiting 15 seconds for you to click a reaction...");
    std::thread::sleep(std::time::Duration::from_secs(15));
    let react_js = r#"
    (async () => {
        // Reactions are rendered as clickable chips under messages.
        // Look for elements that look like reaction buttons.
        const candidates = [];

        // Method 1: aria-label with emoji
        const allButtons = document.querySelectorAll('[role="button"]');
        for (const btn of allButtons) {
            const label = btn.getAttribute('aria-label') || '';
            const text = btn.textContent || '';
            // Emoji reactions often have aria-labels like "👍 2 reactions"
            if ((label.includes('reaction') || label.match(/[👍❤️😀🎉👀🚀]/))
                && text.length < 30) {
                candidates.push({label, text});
                // Try clicking
                btn.click();
                break;
            }
        }
        await new Promise(r => setTimeout(r, 2000));
        return JSON.stringify({candidates});
    })()
    "#;
    if let Ok(r) = tab.evaluate(react_js, true) {
        if let Some(text) = r.value.and_then(|v| v.as_str().map(|s| s.to_owned())) {
            eprintln!("  {}", &text[..text.len().min(300)]);
        }
    }
    std::thread::sleep(std::time::Duration::from_secs(3));

    // ── Collect captured calls ──
    let collect = r#"JSON.stringify(window._cap2 || [])"#;
    let caps_text = tab
        .evaluate(collect, false)
        .ok()
        .and_then(|r| r.value)
        .and_then(|v| v.as_str().map(|s| s.to_owned()))
        .unwrap_or("[]".to_string());
    let caps: Vec<serde_json::Value> = serde_json::from_str(&caps_text).unwrap_or_default();
    eprintln!("  {} API calls captured", caps.len());

    // Find catch_up_group if present
    for cap in caps.iter() {
        let url = cap.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let status = cap.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
        if url.contains("catch_up_group") {
            eprintln!("\n  Found catch_up_group (status={status}):");
            if let Some(body) = cap.get("body").and_then(|v| v.as_str()) {
                eprintln!(
                    "    Body ({} chars): {}",
                    body.len(),
                    &body[..body.len().min(800)]
                );
            }
        }
        if url.contains("update_reaction") {
            eprintln!("\n  Found update_reaction (status={status}):");
            if let Some(body) = cap.get("body").and_then(|v| v.as_str()) {
                eprintln!("    Body ({} chars): {}", body.len(), body);
            }
        }
    }

    // ── Build our catch_up_group and try variants ──
    eprintln!("\n[4] catch_up_group — trying multiple formats...");
    let gid = proto::GroupId {
        space_id: Some(proto::SpaceId {
            space_id: Some(space_id.clone()),
        }),
        dm_id: None,
    };

    // Variant A: no range
    let req_a = proto::CatchUpGroupRequest {
        request_header: Some(convert::tests_make_header()),
        group_id: Some(gid.clone()),
        range: None,
        page_size: Some(20),
        cutoff_size: None,
    };
    eprintln!("  A: no range, page_size=20");
    match api::call_proto::<_, proto::CatchUpResponse>(&mut session, "catch_up_group", &req_a) {
        Ok(r) => eprintln!("    OK — {} events", r.events.len()),
        Err(e) => eprintln!(
            "    FAILED: {}",
            &e.to_string()[..e.to_string().len().min(120)]
        ),
    }

    // Variant B: with range (0 to now)
    let now_usec = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0);
    let req_b = proto::CatchUpGroupRequest {
        request_header: Some(convert::tests_make_header()),
        group_id: Some(gid.clone()),
        range: Some(proto::CatchUpRange {
            from_revision_timestamp: Some(0),
            to_revision_timestamp: Some(now_usec),
        }),
        page_size: Some(20),
        cutoff_size: Some(100),
    };
    eprintln!("  B: with full range and cutoff");
    match api::call_proto::<_, proto::CatchUpResponse>(&mut session, "catch_up_group", &req_b) {
        Ok(r) => eprintln!("    OK — {} events", r.events.len()),
        Err(e) => eprintln!(
            "    FAILED: {}",
            &e.to_string()[..e.to_string().len().min(120)]
        ),
    }

    // Variant C: only group_id (no page_size)
    let req_c = proto::CatchUpGroupRequest {
        request_header: Some(convert::tests_make_header()),
        group_id: Some(gid.clone()),
        range: None,
        page_size: None,
        cutoff_size: None,
    };
    eprintln!("  C: group_id only");
    match api::call_proto::<_, proto::CatchUpResponse>(&mut session, "catch_up_group", &req_c) {
        Ok(r) => eprintln!("    OK — {} events", r.events.len()),
        Err(e) => eprintln!(
            "    FAILED: {}",
            &e.to_string()[..e.to_string().len().min(120)]
        ),
    }

    // ── Send a message first so we can test update_reaction ──
    eprintln!("\n[5] Sending test message...");
    let parent_id = proto::MessageParentId {
        topic_id: Some(proto::TopicId {
            group_id: Some(gid.clone()),
            topic_id: None,
        }),
    };

    // Debug: show the pblite we're about to send
    let probe = proto::CreateMessageRequest {
        request_header: Some(convert::tests_make_header()),
        parent_id: Some(parent_id.clone()),
        text_body: Some("test".into()),
        annotations: Vec::new(),
        local_id: Some("local".into()),
        message_id: None,
        message_info: Some(proto::MessageInfo {
            accept_format_annotations: Some(true),
            reply_to: None,
        }),
    };
    let probe_wire = probe.encode_to_vec();
    let probe_pb = pblite::wire_to_pblite(&probe_wire).unwrap();
    let probe_str = serde_json::to_string(&probe_pb).unwrap();
    eprintln!(
        "  Pblite for create_message: {}",
        &probe_str[..probe_str.len().min(300)]
    );
    let send_resp = api::call_proto::<_, proto::CreateMessageResponse>(
        &mut session,
        "create_message",
        &proto::CreateMessageRequest {
            request_header: Some(convert::tests_make_header()),
            parent_id: Some(parent_id.clone()),
            text_body: Some("tchat reaction test (will be deleted)".into()),
            annotations: Vec::new(),
            local_id: Some(format!("tchat-react-{}", std::process::id())),
            message_id: None,
            message_info: Some(proto::MessageInfo {
                accept_format_annotations: Some(true),
                reply_to: None,
            }),
        },
    );

    let (msg_id_full, msg_id_str, topic_id_str) = match send_resp {
        Ok(resp) => {
            // Get the FULL MessageId from the response (server-canonical form)
            let full_mid = resp
                .message
                .as_ref()
                .and_then(|m| m.id.clone())
                .expect("message should have id");
            let mid_str = full_mid.message_id.clone().unwrap_or_default();
            let tid_str = full_mid
                .parent_id
                .as_ref()
                .and_then(|p| p.topic_id.as_ref())
                .and_then(|t| t.topic_id.clone())
                .unwrap_or_default();
            eprintln!("  Sent: msg={mid_str}, topic={tid_str}");
            eprintln!("  Full MessageId from server: {:?}", full_mid);
            (full_mid, mid_str, tid_str)
        }
        Err(e) => {
            eprintln!("  Send FAILED: {e}");
            return;
        }
    };

    // ── Show what wire format the server-returned MessageId encodes to ──
    let mid_wire = msg_id_full.encode_to_vec();
    let mid_pblite = pblite::wire_to_pblite(&mid_wire).unwrap();
    eprintln!(
        "\n[6a] Server MessageId as pblite: {}",
        serde_json::to_string(&mid_pblite).unwrap()
    );

    // ── Our update_reaction pblite ──
    eprintln!("\n[6b] Our update_reaction request...");
    let react_req = proto::UpdateReactionRequest {
        request_header: Some(convert::tests_make_header()),
        message_id: Some(msg_id_full.clone()),
        emoji: Some(proto::Emoji {
            unicode: Some("👍".into()),
            custom_emoji: None,
        }),
        option: Some(1),
    };
    let wire2 = react_req.encode_to_vec();
    let pb2 = pblite::wire_to_pblite(&wire2).unwrap();
    let our_body2 = serde_json::to_string(&pb2).unwrap();
    eprintln!(
        "  ({} chars): {}",
        our_body2.len(),
        &our_body2[..our_body2.len().min(400)]
    );

    // ── Attempt the reaction with the server-canonical MessageId ──
    eprintln!("\n[7] Attempting update_reaction (server-canonical MessageId)...");
    match api::call_proto::<_, proto::UpdateReactionResponse>(
        &mut session,
        "update_reaction",
        &react_req,
    ) {
        Ok(_) => eprintln!("  OK"),
        Err(e) => eprintln!("  FAILED: {e}"),
    }

    // ── Search for reaction API URLs in loaded scripts ──
    eprintln!("\n[7a-search] Searching for reaction endpoint in loaded JS...");
    let search_js = r#"
    (() => {
        const found = new Set();
        // Check all script src attributes
        const scripts = document.querySelectorAll('script');
        for (const s of scripts) {
            const src = s.src || '';
            if (src.includes('chat') || src.includes('dynamite')) found.add(src);
        }

        // Look at performance entries
        const urls = performance.getEntriesByType('resource').map(e => e.name);
        const apiCalls = urls.filter(u => u.includes('/api/'));
        const endpoints = new Set();
        for (const u of apiCalls) {
            const m = u.match(/\/api\/([a-z_]+)/);
            if (m) endpoints.add(m[1]);
        }

        return JSON.stringify({
            scripts_seen: Array.from(found).slice(0, 3),
            all_endpoints: Array.from(endpoints).sort()
        });
    })()
    "#;
    if let Ok(r) = tab.evaluate(search_js, false) {
        if let Some(s) = r.value.and_then(|v| v.as_str().map(|s| s.to_owned())) {
            eprintln!("  {}", &s[..s.len().min(1500)]);
        }
    }

    // ── Try different endpoint names ──
    eprintln!("\n[7b] Trying different endpoint names...");
    for endpoint in &[
        "react",
        "add_reaction",
        "create_reaction",
        "update_reactions",
        "create_topic_reaction",
        "create_message_reaction",
        "reaction",
        "set_reaction",
        "update_reaction_v2",
    ] {
        match api::call_proto::<_, proto::UpdateReactionResponse>(
            &mut session,
            endpoint,
            &react_req,
        ) {
            Ok(_) => {
                eprintln!("  /api/{endpoint}: OK ✓");
                break;
            }
            Err(e) => {
                let s = e.to_string();
                let brief = if s.len() > 100 { &s[..100] } else { &s };
                eprintln!("  /api/{endpoint}: {}", brief);
            }
        }
    }

    // ── Try via direct JS fetch with the exact same body (sanity check) ──
    eprintln!("\n[7c] Attempting via direct JS fetch...");
    let xsrf = session.xsrf_token.clone().unwrap_or_default();
    let counter = session.next_api_counter();
    let body_b64 = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(our_body2.as_bytes())
    };
    let js = format!(
        r#"(async () => {{
        try {{
            const body = atob("{body_b64}");
            const resp = await fetch("https://chat.google.com/u/0/api/update_reaction?c={counter}", {{
                method: 'POST', credentials: 'include',
                headers: {{
                    'Content-Type': 'application/json',
                    'X-Goog-AuthUser': '0',
                    'X-Framework-Xsrf-Token': '{xsrf}',
                    'Accept-Language': 'en'
                }},
                body: body
            }});
            const text = await resp.text();
            return JSON.stringify({{status: resp.status, body: text.substring(0, 300)}});
        }} catch(e) {{ return JSON.stringify({{error: e.message}}); }}
    }})()"#
    );
    if let Ok(r) = tab.evaluate(&js, true) {
        if let Some(s) = r.value.and_then(|v| v.as_str().map(|s| s.to_owned())) {
            eprintln!("  {}", s);
        }
    }

    // Cleanup
    eprintln!("\n[8] Cleanup: delete the test message");
    let _ = api::call_proto::<_, proto::DeleteMessageResponse>(
        &mut session,
        "delete_message",
        &proto::DeleteMessageRequest {
            request_header: Some(convert::tests_make_header()),
            message_id: Some(msg_id_full),
        },
    );
    eprintln!("  done (msg={msg_id_str}, topic={topic_id_str})");

    // ── Look at all captured update_reaction requests from the real web ──
    // Wait a bit and re-collect (maybe the web client made calls during our test)
    std::thread::sleep(std::time::Duration::from_secs(2));
    if let Ok(r) = tab.evaluate(collect, false) {
        if let Some(text) = r.value.and_then(|v| v.as_str().map(|s| s.to_owned())) {
            let all: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
            eprintln!("\n[9] All captured update_reaction / catch_up_group calls:");
            for cap in all.iter() {
                let url = cap.get("url").and_then(|v| v.as_str()).unwrap_or("");
                if url.contains("update_reaction") || url.contains("catch_up_group") {
                    let status = cap.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
                    let endpoint = url
                        .split("/api/")
                        .nth(1)
                        .and_then(|s| s.split('?').next())
                        .unwrap_or("?");
                    eprintln!("  {endpoint} status={status}");
                    if let Some(body) = cap.get("body").and_then(|v| v.as_str()) {
                        eprintln!("    body: {}", &body[..body.len().min(500)]);
                    }
                }
            }
        }
    }

    eprintln!("\n========== capture complete ==========");
}
