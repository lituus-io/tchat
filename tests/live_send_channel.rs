//! Test sending via command channel (same as io_loop) but WITHOUT io_loop.
//! This isolates whether the command channel pattern itself causes issues.
//!
//! Run:  cargo test --test live_send_channel -- --ignored --nocapture

use crossbeam::channel;
use std::time::Duration;
use tchat::platform::googlechat::{api, auth, convert, proto, session::Session};

const TEST_SPACE: &str = "AAAAz6E4W_g";

#[test]
#[ignore]
fn send_via_channel() {
    eprintln!("\n========== Send via channel test ==========\n");

    let tokens = auth::authenticate(None).expect("Auth failed");
    let mut session = Session::new(tokens);
    let _ = session.fetch_session_tokens();

    let gid = proto::GroupId {
        space_id: Some(proto::SpaceId {
            space_id: Some(TEST_SPACE.into()),
        }),
        dm_id: None,
    };

    // Spawn a thread that receives commands and processes them
    // (same pattern as io_loop_with_tokens)
    let (cmd_tx, cmd_rx) = channel::unbounded::<proto::CreateMessageRequest>();

    let handle = std::thread::spawn(move || {
        for req in cmd_rx {
            match api::call_proto::<_, proto::CreateMessageResponse>(
                &mut session,
                "create_message",
                &req,
            ) {
                Ok(r) => {
                    let mid = r
                        .message
                        .as_ref()
                        .and_then(|m| m.id.as_ref())
                        .and_then(|id| id.message_id.as_deref())
                        .unwrap_or("?");
                    eprintln!("  ✓ sent {mid}");
                }
                Err(e) => eprintln!("  ✗ {e}"),
            }
        }
    });

    // Send 5 messages
    for i in 1..=5 {
        let text = format!("channel test message #{i}");
        eprintln!("[{i}/5] Sending: \"{text}\"");

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
            local_id: Some(format!("chan-{i}-{}", std::process::id())),
            message_id: None,
            message_info: Some(proto::MessageInfo {
                accept_format_annotations: Some(true),
                reply_to: None,
            }),
        };
        cmd_tx.send(req).unwrap();
        std::thread::sleep(Duration::from_secs(3));
    }

    drop(cmd_tx);
    let _ = handle.join();
    eprintln!("\n========== done ==========");
}
