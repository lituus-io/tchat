//! Minimal probe: confirm AAAAz6E4W_g is reachable for this account.
//!
//! Sends a single message and prints the API response or error. Decoupled
//! from BrowserChannel so we can isolate "send" failures from "receive"
//! failures.
//!
//! Run:
//!   cargo test --test live_probe_space -- --ignored --nocapture

use tchat::platform::googlechat::{api, auth, convert, proto, session::Session};

const TEST_SPACE_ID: &str = "AAAAz6E4W_g";

#[test]
#[ignore]
fn probe_test_space_reachable() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::WARN)
        .try_init();

    eprintln!("\n[probe] target space: {TEST_SPACE_ID}");
    eprintln!("[probe] launching Chrome / loading cookies...");
    let tokens = auth::authenticate(None).expect("auth failed");
    let mut session = Session::new(tokens);
    let _ = session.fetch_session_tokens();
    let _ = session.ensure_clean_api_tab();

    // Save cookies for subsequent runs
    if let Ok(tab) = session.tokens.get_tab() {
        if let Ok(cookies) = tchat::platform::googlechat::cookies::extract_from_chrome_session(&tab)
        {
            let _ = tchat::platform::googlechat::cookies::save_cookies(&cookies);
            eprintln!("[probe] cookies persisted");
        }
    }
    eprintln!("[probe] session ready\n");

    // Step 1: get_group on the test space — does it exist for this account?
    let gid = proto::GroupId {
        space_id: Some(proto::SpaceId {
            space_id: Some(TEST_SPACE_ID.into()),
        }),
        dm_id: None,
    };

    eprintln!("[probe] get_group({TEST_SPACE_ID})");
    match api::call_proto::<_, proto::GetGroupResponse>(
        &mut session,
        "get_group",
        &proto::GetGroupRequest {
            request_header: Some(convert::tests_make_header()),
            group_id: Some(proto::GroupId {
                space_id: gid.space_id.clone(),
                dm_id: None,
            }),
            fetch_options: vec![1, 4],
            user_not_older_than: None,
            group_not_older_than: None,
            include_invite_dms: None,
        },
    ) {
        Ok(resp) => {
            let name = resp
                .group
                .as_ref()
                .and_then(|g| g.name.clone())
                .unwrap_or_default();
            let gtype = resp.group.as_ref().and_then(|g| g.group_type);
            eprintln!("    ✓ group exists: name=\"{name}\" type={gtype:?}");
        }
        Err(e) => {
            eprintln!("    ✗ get_group FAILED: {e}");
            eprintln!("\n  → Most likely the user is not a member of {TEST_SPACE_ID},");
            eprintln!("    or the ID is wrong. Verify by opening:");
            eprintln!("    https://chat.google.com/room/{TEST_SPACE_ID}");
            return;
        }
    }

    // Step 2: try sending a message
    eprintln!("\n[probe] create_message → \"probe ping\"");
    match api::call_proto::<_, proto::CreateMessageResponse>(
        &mut session,
        "create_message",
        &proto::CreateMessageRequest {
            request_header: Some(convert::tests_make_header()),
            parent_id: Some(proto::MessageParentId {
                topic_id: Some(proto::TopicId {
                    group_id: Some(proto::GroupId {
                        space_id: gid.space_id.clone(),
                        dm_id: None,
                    }),
                    topic_id: None,
                }),
            }),
            text_body: Some("probe ping".into()),
            annotations: Vec::new(),
            local_id: Some(format!("probe-{}", std::process::id())),
            message_id: None,
            message_info: Some(proto::MessageInfo {
                accept_format_annotations: Some(true),
                reply_to: None,
            }),
        },
    ) {
        Ok(resp) => {
            let mid = resp
                .message
                .and_then(|m| m.id)
                .and_then(|i| i.message_id)
                .unwrap_or_default();
            eprintln!("    ✓ create_message OK, message_id={mid}");
            eprintln!("\n  → Space is reachable. The full harness should work.");
        }
        Err(e) => {
            eprintln!("    ✗ create_message FAILED: {e}");
        }
    }
}
