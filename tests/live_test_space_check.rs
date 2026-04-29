//! Quick check of the Test - Gchat space — verify we can access and send.
//!
//! Run:  cargo test --test live_test_space_check -- --ignored --nocapture

use tchat::platform::googlechat::{api, auth, convert, proto, session::Session};

const TEST_SPACE_ID: &str = "AAQAjslKeUE";

#[test]
#[ignore]
fn check_test_space() {
    eprintln!("\n========== Verify Test - Gchat access ==========\n");

    let tokens = auth::authenticate(None).expect("Auth failed");
    let mut session = Session::new(tokens);
    let _ = session.fetch_session_tokens();

    let gid = proto::GroupId {
        space_id: Some(proto::SpaceId {
            space_id: Some(TEST_SPACE_ID.into()),
        }),
        dm_id: None,
    };

    // get_group — should tell us the space name & if we're a member
    eprintln!("[1] get_group({TEST_SPACE_ID})...");
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
            let ms = resp.membership_state.unwrap_or(0);
            eprintln!("  OK — name=\"{name}\", membership_state={ms}");
            eprintln!("  {} memberships", resp.memberships.len());
        }
        Err(e) => eprintln!("  FAILED: {e}"),
    }

    // list_topics — should show existing messages
    eprintln!("\n[2] list_topics({TEST_SPACE_ID})...");
    match api::call_proto::<_, proto::ListTopicsResponse>(
        &mut session,
        "list_topics",
        &proto::ListTopicsRequest {
            request_header: Some(convert::tests_make_header()),
            group_id: Some(gid.clone()),
            page_size_for_topics: Some(10),
            page_size_for_replies: Some(3),
            page_size_for_unread_replies: Some(100),
            page_size_for_read_replies: Some(3),
            fetch_options: vec![3, 1, 4],
            user_not_older_than: None,
            group_not_older_than: None,
        },
    ) {
        Ok(resp) => {
            eprintln!("  OK — {} topics", resp.topics.len());
            for topic in resp.topics.iter().take(5) {
                let tid = topic
                    .id
                    .as_ref()
                    .and_then(|t| t.topic_id.as_deref())
                    .unwrap_or("?");
                let n = topic.replies.len();
                eprintln!("    topic={tid}, {n} replies");
                for r in topic.replies.iter().take(2) {
                    let sender = r
                        .creator
                        .as_ref()
                        .and_then(|u| u.name.as_deref())
                        .unwrap_or("?");
                    let text = r.text_body.as_deref().unwrap_or("");
                    let t = if text.len() > 60 {
                        format!("{}...", &text[..60])
                    } else {
                        text.to_string()
                    };
                    eprintln!("      {sender}: \"{t}\"");
                    // Check if message has reactions!
                    for rx in &r.reactions {
                        let emo = rx
                            .emoji
                            .as_ref()
                            .and_then(|e| e.unicode.as_deref())
                            .unwrap_or("?");
                        let count = rx.count.unwrap_or(0);
                        eprintln!("        reaction: {emo} ({count})");
                    }
                }
            }
        }
        Err(e) => eprintln!("  FAILED: {e}"),
    }

    // Try sending a test message
    eprintln!("\n[3] create_message in test space...");
    let send_req = proto::CreateMessageRequest {
        request_header: Some(convert::tests_make_header()),
        parent_id: Some(proto::MessageParentId {
            topic_id: Some(proto::TopicId {
                group_id: Some(gid.clone()),
                topic_id: None,
            }),
        }),
        text_body: Some("tchat test".into()),
        annotations: Vec::new(),
        local_id: Some(format!("tchat-{}", std::process::id())),
        message_id: None,
        message_info: Some(proto::MessageInfo {
            accept_format_annotations: Some(true),
            reply_to: None,
        }),
    };
    match api::call_proto::<_, proto::CreateMessageResponse>(
        &mut session,
        "create_message",
        &send_req,
    ) {
        Ok(resp) => {
            let mid = resp
                .message
                .as_ref()
                .and_then(|m| m.id.as_ref())
                .and_then(|id| id.message_id.as_deref())
                .unwrap_or("?");
            eprintln!("  OK — sent {mid}");

            // Cleanup
            if let Some(full_id) = resp.message.and_then(|m| m.id) {
                let _ = api::call_proto::<_, proto::DeleteMessageResponse>(
                    &mut session,
                    "delete_message",
                    &proto::DeleteMessageRequest {
                        request_header: Some(convert::tests_make_header()),
                        message_id: Some(full_id),
                    },
                );
                eprintln!("  cleaned up");
            }
        }
        Err(e) => eprintln!("  FAILED: {e}"),
    }
}
