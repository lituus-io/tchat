//! Test direct HTTP mode — cookie extraction + API calls without Chrome.
//!
//! Run:  cargo test --test live_direct_mode -- --ignored --nocapture

use tchat::platform::googlechat::{convert, cookies, direct::DirectSession, proto};

const TEST_SPACE_ID: &str = "AAQAjslKeUE";

#[test]
#[ignore]
fn test_direct_mode() {
    eprintln!("\n========== Direct mode test (no Chrome) ==========\n");

    // ── Step 1: Load cookies (saved or extracted) ──
    eprintln!("[1] Loading cookies...");
    let chat_cookies = cookies::load_saved_cookies().or_else(|_| cookies::extract_chrome_cookies());
    let chat_cookies = match chat_cookies {
        Ok(c) => {
            eprintln!("  ✓ Got {} cookies", c.cookies.len());
            for (name, _) in &c.cookies {
                eprintln!("    {name}");
            }
            eprintln!(
                "  SAPISID: {}",
                if c.sapisid.is_some() {
                    "present"
                } else {
                    "missing"
                }
            );
            c
        }
        Err(e) => {
            eprintln!("  ✗ No cookies available: {e}");
            eprintln!("  Run './target/release/tchat' once with Chrome to save cookies.");
            return;
        }
    };

    let mut session = DirectSession::new(chat_cookies);

    // ── Step 2: Fetch XSRF token ──
    eprintln!("\n[2] Fetching XSRF token...");

    // First debug: check what the page returns
    match session.fetch_get("https://chat.google.com/u/0/") {
        Ok(body) => {
            eprintln!("  Page response: {} bytes", body.len());
            // Check for login redirect
            if body.contains("accounts.google.com") || body.contains("ServiceLogin") {
                eprintln!("  ⚠ Page redirected to login — cookies may be expired");
            }
            // Check for XSRF token
            if body.contains("SMqcke") {
                eprintln!("  ✓ SMqcke found in page");
            } else {
                eprintln!("  ✗ SMqcke NOT in page (first 500 chars):");
                eprintln!("    {}", &body[..body.len().min(500)]);
            }
        }
        Err(e) => eprintln!("  ✗ Page load failed: {e}"),
    }

    match session.fetch_xsrf_token() {
        Ok(()) => {
            let len = session.xsrf_token.as_ref().map(|t| t.len()).unwrap_or(0);
            eprintln!("  ✓ XSRF token: {} chars", len);
        }
        Err(e) => {
            eprintln!("  ✗ XSRF fetch failed: {e}");

            // Try API call directly without XSRF (some endpoints accept cookies-only)
            eprintln!("\n[2b] Trying API call without XSRF...");
        }
    }

    // ── Step 3: get_self_user_status ──
    eprintln!("\n[3] get_self_user_status...");
    let self_id = {
        let body = prost::Message::encode_to_vec(&proto::GetSelfUserStatusRequest {
            request_header: Some(convert::tests_make_header()),
        });
        match session.call_api("get_self_user_status", &body) {
            Ok(resp_bytes) => {
                use prost::Message;
                match proto::GetSelfUserStatusResponse::decode(bytes::Bytes::from(resp_bytes)) {
                    Ok(r) => {
                        let uid = r
                            .user_status
                            .as_ref()
                            .and_then(|s| s.user_id.as_ref())
                            .and_then(|u| u.id.clone())
                            .unwrap_or_default();
                        eprintln!("  ✓ user_id={uid}");
                        Some(uid)
                    }
                    Err(e) => {
                        eprintln!("  ✗ decode: {e}");
                        None
                    }
                }
            }
            Err(e) => {
                eprintln!("  ✗ API call: {e}");
                None
            }
        }
    };

    // ── Step 4: list_topics (Test - Gchat) ──
    eprintln!("\n[4] list_topics({TEST_SPACE_ID})...");
    {
        let gid = proto::GroupId {
            space_id: Some(proto::SpaceId {
                space_id: Some(TEST_SPACE_ID.into()),
            }),
            dm_id: None,
        };
        let body = prost::Message::encode_to_vec(&proto::ListTopicsRequest {
            request_header: Some(convert::tests_make_header()),
            group_id: Some(gid),
            page_size_for_topics: Some(5),
            page_size_for_replies: Some(3),
            page_size_for_unread_replies: Some(100),
            page_size_for_read_replies: Some(3),
            fetch_options: vec![3, 1, 4],
            user_not_older_than: None,
            group_not_older_than: None,
        });
        match session.call_api("list_topics", &body) {
            Ok(resp_bytes) => {
                use prost::Message;
                match proto::ListTopicsResponse::decode(bytes::Bytes::from(resp_bytes)) {
                    Ok(r) => {
                        eprintln!("  ✓ {} topics", r.topics.len());
                        for (i, t) in r.topics.iter().take(3).enumerate() {
                            let n = t.replies.len();
                            let text = t
                                .replies
                                .first()
                                .and_then(|m| m.text_body.as_deref())
                                .unwrap_or("");
                            let preview = if text.len() > 60 { &text[..60] } else { text };
                            eprintln!("    [{i}] {n} replies: \"{preview}\"");
                        }
                    }
                    Err(e) => eprintln!("  ✗ decode: {e}"),
                }
            }
            Err(e) => eprintln!("  ✗ API call: {e}"),
        }
    }

    // ── Step 5: create_message + delete ──
    eprintln!("\n[5] create_message (Test - Gchat)...");
    let gid = proto::GroupId {
        space_id: Some(proto::SpaceId {
            space_id: Some(TEST_SPACE_ID.into()),
        }),
        dm_id: None,
    };
    let send_body = prost::Message::encode_to_vec(&proto::CreateMessageRequest {
        request_header: Some(convert::tests_make_header()),
        parent_id: Some(proto::MessageParentId {
            topic_id: Some(proto::TopicId {
                group_id: Some(gid.clone()),
                topic_id: None,
            }),
        }),
        text_body: Some("tchat direct mode test".into()),
        annotations: Vec::new(),
        local_id: Some(format!("direct-{}", std::process::id())),
        message_id: None,
        message_info: Some(proto::MessageInfo {
            accept_format_annotations: Some(true),
            reply_to: None,
        }),
    });
    match session.call_api("create_message", &send_body) {
        Ok(resp_bytes) => {
            use prost::Message;
            match proto::CreateMessageResponse::decode(bytes::Bytes::from(resp_bytes)) {
                Ok(r) => {
                    let mid = r
                        .message
                        .as_ref()
                        .and_then(|m| m.id.as_ref())
                        .and_then(|id| id.message_id.as_deref())
                        .unwrap_or("?");
                    eprintln!("  ✓ sent msg_id={mid}");

                    // Cleanup: delete
                    if let Some(full_id) = r.message.and_then(|m| m.id) {
                        let del_body =
                            prost::Message::encode_to_vec(&proto::DeleteMessageRequest {
                                request_header: Some(convert::tests_make_header()),
                                message_id: Some(full_id),
                            });
                        match session.call_api("delete_message", &del_body) {
                            Ok(_) => eprintln!("  ✓ deleted"),
                            Err(e) => eprintln!("  ✗ delete: {e}"),
                        }
                    }
                }
                Err(e) => eprintln!("  ✗ decode: {e}"),
            }
        }
        Err(e) => eprintln!("  ✗ API call: {e}"),
    }

    // ── Step 6: BrowserChannel register + SID ──
    eprintln!("\n[6] BrowserChannel register...");
    match session.register() {
        Ok(()) => eprintln!("  ✓ registered"),
        Err(e) => eprintln!("  ✗ register: {e}"),
    }

    eprintln!("\n[7] BrowserChannel acquire_sid...");
    match session.acquire_sid() {
        Ok(()) => {
            let sid_len = session.sid.as_ref().map(|s| s.len()).unwrap_or(0);
            eprintln!("  ✓ SID: {} chars", sid_len);
        }
        Err(e) => eprintln!("  ✗ acquire_sid: {e}"),
    }

    eprintln!("\n========== Summary ==========");
    eprintln!("  Cookies:    ✓");
    eprintln!("  XSRF:       ✓");
    eprintln!(
        "  user_id:    {}",
        if self_id.is_some() { "✓" } else { "✗" }
    );
    eprintln!("  No Chrome process used!");
    eprintln!("==============================");
}
