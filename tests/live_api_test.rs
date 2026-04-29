//! Live API test — all endpoints with schema-aware pblite decoder.
//!
//! Run:  cargo test --test live_api_test -- --ignored --nocapture

use tchat::platform::googlechat::{api, auth, convert, proto, session::Session};

#[test]
#[ignore]
fn live_all_apis() {
    eprintln!("\n========== tchat full API test (schema-aware) ==========\n");

    eprintln!("[1/6] Authenticating...");
    let tokens = auth::authenticate(None).expect("Auth failed");
    let mut session = Session::new(tokens);
    let _ = session.fetch_session_tokens();
    eprintln!(
        "  XSRF: {} chars",
        session.xsrf_token.as_ref().map(|t| t.len()).unwrap_or(0)
    );

    let tab = session.tokens.get_tab().expect("No tab");

    // Give the SPA more time to load the sidebar
    eprintln!("  Waiting for SPA to load spaces...");
    std::thread::sleep(std::time::Duration::from_secs(5));

    // Get bare space IDs from DOM — retry until we find some
    let dom_js = r#"(() => {
        const items = document.querySelectorAll('[data-group-id]');
        const out = [];
        const seen = new Set();
        for (const item of items) {
            let id = item.getAttribute('data-group-id') || '';
            if (id && !seen.has(id)) {
                seen.add(id);
                const bare = id.replace(/^space\//, '');
                const name = item.getAttribute('aria-label') || item.textContent.trim().substring(0, 50);
                out.push({bare, name});
            }
        }
        return JSON.stringify(out.slice(0, 5));
    })()"#;

    let mut spaces: Vec<serde_json::Value> = Vec::new();
    for attempt in 0..5 {
        spaces = tab
            .evaluate(dom_js, false)
            .ok()
            .and_then(|r| r.value)
            .and_then(|v| v.as_str().map(|s| s.to_owned()))
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if !spaces.is_empty() {
            break;
        }
        eprintln!("  Attempt {}: no spaces yet, waiting...", attempt + 1);
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
    eprintln!("  {} spaces from DOM:", spaces.len());
    for (i, s) in spaces.iter().enumerate() {
        eprintln!(
            "    [{i}] {} — \"{}\"",
            s.get("bare").and_then(|v| v.as_str()).unwrap_or("?"),
            s.get("name").and_then(|v| v.as_str()).unwrap_or("?")
        );
    }

    // Use the second space — the first has our deleted test messages
    let test_space = spaces
        .get(1)
        .or_else(|| spaces.first())
        .and_then(|s| s.get("bare"))
        .and_then(|v| v.as_str())
        .expect("Need at least one space");
    let gid = proto::GroupId {
        space_id: Some(proto::SpaceId {
            space_id: Some(test_space.to_string()),
        }),
        dm_id: None,
    };
    eprintln!("\n  Testing with: {test_space}");

    // ── get_self_user_status ──
    eprintln!("\n[2/6] get_self_user_status...");
    match api::call_proto::<_, proto::GetSelfUserStatusResponse>(
        &mut session,
        "get_self_user_status",
        &proto::GetSelfUserStatusRequest {
            request_header: Some(convert::tests_make_header()),
        },
    ) {
        Ok(r) => {
            let uid = r
                .user_status
                .as_ref()
                .and_then(|s| s.user_id.as_ref())
                .and_then(|u| u.id.as_deref())
                .unwrap_or("?");
            let dnd = r
                .user_status
                .as_ref()
                .and_then(|s| s.dnd_settings.as_ref())
                .and_then(|d| d.dnd_state)
                .unwrap_or(0);
            eprintln!("  OK — user_id={uid}, dnd_state={dnd}");
        }
        Err(e) => eprintln!("  FAILED: {e}"),
    }

    // ── get_group ──
    eprintln!("\n[3/6] get_group({test_space})...");
    match api::call_proto::<_, proto::GetGroupResponse>(
        &mut session,
        "get_group",
        &proto::GetGroupRequest {
            request_header: Some(convert::tests_make_header()),
            group_id: Some(gid.clone()),
            fetch_options: vec![5, 9, 8, 7, 1, 4],
            user_not_older_than: None,
            group_not_older_than: None,
            include_invite_dms: Some(true),
        },
    ) {
        Ok(resp) => {
            let name = resp
                .group
                .as_ref()
                .and_then(|g| g.name.as_deref())
                .unwrap_or("?");
            let gtype = resp.group.as_ref().and_then(|g| g.group_type).unwrap_or(0);
            let flat = resp.group.as_ref().and_then(|g| g.is_flat);
            let n_members = resp.memberships.len();
            eprintln!("  OK — \"{name}\", type={gtype}, flat={flat:?}, {n_members} memberships");
            if let Some(g) = &resp.group {
                let creator = g
                    .creator
                    .as_ref()
                    .and_then(|u| u.user_id.as_ref())
                    .and_then(|uid| uid.id.as_deref())
                    .unwrap_or("?");
                eprintln!("  Creator: {creator}");
                eprintln!("  Created: {}", g.create_time.unwrap_or(0));
            }
        }
        Err(e) => eprintln!("  FAILED: {e}"),
    }

    // ── list_topics ──
    eprintln!("\n[4/6] list_topics({test_space})...");
    match api::call_proto::<_, proto::ListTopicsResponse>(
        &mut session,
        "list_topics",
        &proto::ListTopicsRequest {
            request_header: Some(convert::tests_make_header()),
            group_id: Some(gid.clone()),
            page_size_for_topics: Some(5),
            page_size_for_replies: Some(3),
            page_size_for_unread_replies: Some(100),
            page_size_for_read_replies: Some(3),
            fetch_options: vec![3, 1, 4],
            user_not_older_than: None,
            group_not_older_than: None,
        },
    ) {
        Ok(resp) => {
            let n_topics = resp.topics.len();
            let first = resp.contains_first_topic.unwrap_or(false);
            let last = resp.contains_last_topic.unwrap_or(false);
            eprintln!("  OK — {n_topics} topics (first={first}, last={last})");

            for (i, topic) in resp.topics.iter().take(3).enumerate() {
                let tid = topic
                    .id
                    .as_ref()
                    .and_then(|t| t.topic_id.as_deref())
                    .unwrap_or("?");
                let n_replies = topic.replies.len();
                eprintln!("    [{i}] topic={tid}, {n_replies} replies");
                for (j, reply) in topic.replies.iter().take(2).enumerate() {
                    let sender = reply
                        .creator
                        .as_ref()
                        .and_then(|u| u.name.as_deref())
                        .unwrap_or("?");
                    let text = reply.text_body.as_deref().unwrap_or("");
                    let preview = if text.len() > 80 {
                        format!("{}...", &text[..80])
                    } else {
                        text.to_string()
                    };
                    eprintln!("      [{j}] {sender}: \"{preview}\"");
                }
            }

            // Also test convert path
            let sid = convert::group_id_to_space_id(&gid, &mut session.interner);
            let event = convert::list_topics_response_to_event(resp, sid, &mut session);
            if let tchat::event::InboundEvent::HistoryChunk {
                messages, has_more, ..
            } = &event
            {
                eprintln!(
                    "  Converted: {} messages (has_more={has_more})",
                    messages.len()
                );
            }
        }
        Err(e) => eprintln!("  FAILED: {e}"),
    }

    // ── set_typing_state ──
    eprintln!("\n[5/6] set_typing_state({test_space})...");
    match api::call_proto::<_, proto::SetTypingStateResponse>(
        &mut session,
        "set_typing_state",
        &proto::SetTypingStateRequest {
            request_header: Some(convert::tests_make_header()),
            state: Some(1),
            context: Some(proto::TypingContext {
                group_id: Some(gid.clone()),
                topic_id: None,
            }),
        },
    ) {
        Ok(r) => eprintln!("  OK — start_ts={}", r.start_timestamp_usec.unwrap_or(0)),
        Err(e) => eprintln!("  FAILED: {e}"),
    }
    let _ = api::call_proto::<_, proto::SetTypingStateResponse>(
        &mut session,
        "set_typing_state",
        &proto::SetTypingStateRequest {
            request_header: Some(convert::tests_make_header()),
            state: Some(2),
            context: Some(proto::TypingContext {
                group_id: Some(gid.clone()),
                topic_id: None,
            }),
        },
    );
    eprintln!("  (stopped)");

    // ── mark_group_readstate ──
    eprintln!("\n[6/6] mark_group_readstate({test_space})...");
    // Current timestamp in microseconds
    let now_usec = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0);
    match api::call_proto::<_, proto::MarkGroupReadstateResponse>(
        &mut session,
        "mark_group_readstate",
        &proto::MarkGroupReadstateRequest {
            request_header: Some(convert::tests_make_header()),
            id: Some(gid),
            last_read_time: Some(now_usec),
        },
    ) {
        Ok(r) => {
            let lr = r
                .read_state
                .as_ref()
                .and_then(|rs| rs.last_read_time)
                .unwrap_or(0);
            let unread = r
                .read_state
                .as_ref()
                .and_then(|rs| rs.unread_message_count)
                .unwrap_or(0);
            eprintln!("  OK — last_read={lr}, unread={unread}");
        }
        Err(e) => eprintln!("  FAILED: {e}"),
    }

    eprintln!("\n========== complete ==========");
}
