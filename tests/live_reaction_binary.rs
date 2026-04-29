//! Test update_reaction using binary protobuf (mautrix's approach).
//!
//! Run:  cargo test --test live_reaction_binary -- --ignored --nocapture

use tchat::platform::googlechat::{api, auth, convert, proto, session::Session};

const TEST_SPACE_ID: &str = "AAQAjslKeUE";

#[test]
#[ignore]
fn binary_reaction() {
    eprintln!("\n========== update_reaction via binary protobuf ==========\n");

    let tokens = auth::authenticate(None).expect("Auth failed");
    let mut session = Session::new(tokens);
    let _ = session.fetch_session_tokens();

    let gid = proto::GroupId {
        space_id: Some(proto::SpaceId {
            space_id: Some(TEST_SPACE_ID.into()),
        }),
        dm_id: None,
    };

    // Send a test message
    eprintln!("[1] create_message...");
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
            text_body: Some("tchat binary-reaction test".into()),
            annotations: Vec::new(),
            local_id: Some(format!("br-{}", std::process::id())),
            message_id: None,
            message_info: Some(proto::MessageInfo {
                accept_format_annotations: Some(true),
                reply_to: None,
            }),
        },
    )
    .expect("send must work");

    let full_id = send.message.and_then(|m| m.id).expect("msg id");
    let msg_id = full_id.message_id.clone().unwrap_or_default();
    let topic_id = full_id
        .parent_id
        .as_ref()
        .and_then(|p| p.topic_id.as_ref())
        .and_then(|t| t.topic_id.clone())
        .unwrap_or_default();
    eprintln!("  Sent msg={msg_id} topic={topic_id}");

    // ── Add reaction with full mautrix-style MessageId via binary ──
    eprintln!("\n[2] update_reaction (ADD) via binary protobuf...");
    let req = proto::UpdateReactionRequest {
        request_header: Some(convert::tests_make_header()),
        message_id: Some(proto::MessageId {
            parent_id: Some(proto::MessageParentId {
                topic_id: Some(proto::TopicId {
                    group_id: Some(gid.clone()),
                    topic_id: Some(topic_id.clone()),
                }),
            }),
            message_id: Some(msg_id.clone()),
        }),
        emoji: Some(proto::Emoji {
            unicode: Some("👍".into()),
            custom_emoji: None,
        }),
        option: Some(1), // ADD
    };
    match api::call_proto::<_, proto::UpdateReactionResponse>(&mut session, "update_reaction", &req)
    {
        Ok(r) => {
            let rev = r
                .group_revision
                .as_ref()
                .and_then(|r| r.timestamp)
                .unwrap_or(0);
            eprintln!("  ✓✓✓ OK — group_revision={rev}");
        }
        Err(e) => {
            let s = e.to_string();
            let brief = if s.len() > 200 { &s[..200] } else { &s };
            eprintln!("  FAILED: {brief}");
        }
    }

    // ── Remove the reaction ──
    eprintln!("\n[3] update_reaction (REMOVE) via binary protobuf...");
    let req_rm = proto::UpdateReactionRequest {
        request_header: Some(convert::tests_make_header()),
        message_id: Some(proto::MessageId {
            parent_id: Some(proto::MessageParentId {
                topic_id: Some(proto::TopicId {
                    group_id: Some(gid.clone()),
                    topic_id: Some(topic_id.clone()),
                }),
            }),
            message_id: Some(msg_id.clone()),
        }),
        emoji: Some(proto::Emoji {
            unicode: Some("👍".into()),
            custom_emoji: None,
        }),
        option: Some(2), // REMOVE
    };
    match api::call_proto::<_, proto::UpdateReactionResponse>(
        &mut session,
        "update_reaction",
        &req_rm,
    ) {
        Ok(r) => {
            let rev = r
                .group_revision
                .as_ref()
                .and_then(|r| r.timestamp)
                .unwrap_or(0);
            eprintln!("  ✓ OK — group_revision={rev}");
        }
        Err(e) => {
            let s = e.to_string();
            let brief = if s.len() > 200 { &s[..200] } else { &s };
            eprintln!("  FAILED: {brief}");
        }
    }

    // Cleanup
    eprintln!("\n[4] Cleanup: delete test message");
    let _ = api::call_proto::<_, proto::DeleteMessageResponse>(
        &mut session,
        "delete_message",
        &proto::DeleteMessageRequest {
            request_header: Some(convert::tests_make_header()),
            message_id: Some(full_id),
        },
    );
    eprintln!("  done");
}
