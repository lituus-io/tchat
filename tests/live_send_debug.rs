//! Automated send test on a specific space.
//!
//! Run:  cargo test --test live_send_debug -- --ignored --nocapture

use std::time::Duration;
use tchat::platform::googlechat::{api, auth, convert, proto, session::Session};

const TEST_SPACE: &str = "AAAAz6E4W_g";

#[test]
#[ignore]
fn send_multiple_messages() {
    eprintln!("\n========== Automated send test ==========\n");

    let tokens = auth::authenticate(None).expect("Auth failed");
    let mut session = Session::new(tokens);
    let _ = session.fetch_session_tokens();
    eprintln!(
        "  XSRF: {} chars",
        session.xsrf_token.as_ref().map(|t| t.len()).unwrap_or(0)
    );

    let gid = proto::GroupId {
        space_id: Some(proto::SpaceId {
            space_id: Some(TEST_SPACE.into()),
        }),
        dm_id: None,
    };

    // First: make some read calls (like the io_loop does before sending)
    eprintln!("\n[PRE] Making read calls first...");
    let _ = api::call_proto::<_, proto::GetSelfUserStatusResponse>(
        &mut session,
        "get_self_user_status",
        &proto::GetSelfUserStatusRequest {
            request_header: Some(convert::tests_make_header()),
        },
    );
    eprintln!("  get_self_user_status: done");

    let gid2 = proto::GroupId {
        space_id: Some(proto::SpaceId {
            space_id: Some(TEST_SPACE.into()),
        }),
        dm_id: None,
    };
    let _ = api::call_proto::<_, proto::ListTopicsResponse>(
        &mut session,
        "list_topics",
        &proto::ListTopicsRequest {
            request_header: Some(convert::tests_make_header()),
            group_id: Some(gid2),
            page_size_for_topics: Some(10),
            page_size_for_replies: Some(3),
            page_size_for_unread_replies: Some(100),
            page_size_for_read_replies: Some(3),
            fetch_options: vec![3, 1, 4],
            user_not_older_than: None,
            group_not_older_than: None,
        },
    );
    eprintln!("  list_topics: done");

    // Now send 5 messages with delays between them
    for i in 1..=5 {
        let text = format!("tchat automated test message #{i}");
        eprintln!("\n[{i}/5] Sending: \"{text}\"");

        let req = proto::CreateMessageRequest {
            request_header: Some(convert::tests_make_header()),
            parent_id: Some(proto::MessageParentId {
                topic_id: Some(proto::TopicId {
                    group_id: Some(gid.clone()),
                    topic_id: None,
                }),
            }),
            text_body: Some(text),
            annotations: Vec::new(),
            local_id: Some(format!("send-test-{i}-{}", std::process::id())),
            message_id: None,
            message_info: Some(proto::MessageInfo {
                accept_format_annotations: Some(true),
                reply_to: None,
            }),
        };

        let start = std::time::Instant::now();
        match api::call_proto::<_, proto::CreateMessageResponse>(
            &mut session,
            "create_message",
            &req,
        ) {
            Ok(resp) => {
                let mid = resp
                    .message
                    .as_ref()
                    .and_then(|m| m.id.as_ref())
                    .and_then(|id| id.message_id.as_deref())
                    .unwrap_or("?");
                eprintln!("  ✓ sent msg={mid} ({:.1}s)", start.elapsed().as_secs_f64());
            }
            Err(e) => {
                let s = e.to_string();
                eprintln!(
                    "  ✗ FAILED ({:.1}s): {}",
                    start.elapsed().as_secs_f64(),
                    &s[..s.len().min(150)]
                );
            }
        }

        // Delay between sends
        if i < 5 {
            eprintln!("  (waiting 3s...)");
            std::thread::sleep(Duration::from_secs(3));
        }
    }

    eprintln!("\n========== test complete ==========");
}
