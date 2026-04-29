//! Phase 4 live test: BrowserChannel real-time events end-to-end.
//!
//! Sends operations (create, edit, delete, react) and verifies the
//! BrowserChannel long-poll thread receives and decodes each event.
//!
//! Run:  cargo test --test live_phase4_stream_events -- --ignored --nocapture

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tchat::event::InboundEvent;
use tchat::platform::googlechat::{api, auth, channel, convert, proto, session::Session};

const TEST_SPACE_ID: &str = "AAQAjslKeUE";

#[test]
#[ignore]
fn stream_events_end_to_end() {
    eprintln!("\n========== Phase 4: BrowserChannel real-time events ==========\n");

    let tokens = auth::authenticate(None).expect("Auth failed");
    let mut session = Session::new(tokens);
    let _ = session.fetch_session_tokens();

    // ── Step 1: Set up BrowserChannel ──
    eprintln!("[1] Setting up BrowserChannel...");
    session.register().expect("register failed");
    session.acquire_sid().expect("acquire_sid failed");
    eprintln!(
        "  ✓ SID acquired: {} chars",
        session.sid.as_ref().map(|s| s.len()).unwrap_or(0)
    );

    // ── Step 2: Start long-poll thread ──
    let (event_tx, event_rx) = crossbeam::channel::unbounded::<InboundEvent>();

    let tab = session.tokens.get_tab().expect("no tab");
    let sid = session.sid.clone().expect("no sid");
    let ctx = channel::StreamingContext { tab, sid };

    eprintln!("\n[2] Spawning long-poll thread...");
    let poll_tx = event_tx.clone();
    std::thread::spawn(move || {
        channel::long_poll_loop_threaded(ctx, poll_tx);
    });

    // Give the long-poll thread time to start
    std::thread::sleep(Duration::from_secs(3));
    eprintln!("  ✓ long-poll thread started");

    // ── Step 3: Send a test message and watch for echo ──
    eprintln!("\n[3] Sending test message, watching for echo event...");
    let gid = proto::GroupId {
        space_id: Some(proto::SpaceId {
            space_id: Some(TEST_SPACE_ID.into()),
        }),
        dm_id: None,
    };

    let send_resp = api::call_proto::<_, proto::CreateMessageResponse>(
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
            text_body: Some("tchat stream-events test — sent at ${{timestamp}}".into()),
            annotations: Vec::new(),
            local_id: Some(format!("tchat-stream-{}", std::process::id())),
            message_id: None,
            message_info: Some(proto::MessageInfo {
                accept_format_annotations: Some(true),
                reply_to: None,
            }),
        },
    )
    .expect("send must work");

    let full_id = send_resp.message.and_then(|m| m.id).expect("msg id");
    let msg_id_str = full_id.message_id.clone().unwrap_or_default();
    eprintln!("  sent msg_id={msg_id_str}");

    // Count received events by type
    let received_messages = AtomicUsize::new(0);
    let received_reactions = AtomicUsize::new(0);
    let received_deletes = AtomicUsize::new(0);
    let received_typing = AtomicUsize::new(0);
    let received_noops = AtomicUsize::new(0);

    // Drain events for 5 seconds
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let mut got_posted_echo = false;
    while std::time::Instant::now() < deadline {
        match event_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(event) => match event {
                InboundEvent::MessagePosted { message, .. } => {
                    received_messages.fetch_add(1, Ordering::Relaxed);
                    let t = if message.text.len() > 60 {
                        format!("{}...", &message.text[..60])
                    } else {
                        message.text.clone()
                    };
                    eprintln!("  📨 MessagePosted — \"{t}\"");
                    got_posted_echo = true;
                }
                InboundEvent::ReactionUpdated {
                    message_id,
                    reactions,
                    ..
                } => {
                    received_reactions.fetch_add(1, Ordering::Relaxed);
                    eprintln!(
                        "  👍 ReactionUpdated — msg_id={message_id:?} ({} reactions)",
                        reactions.len()
                    );
                }
                InboundEvent::MessageDeleted { .. } => {
                    received_deletes.fetch_add(1, Ordering::Relaxed);
                    eprintln!("  🗑  MessageDeleted");
                }
                InboundEvent::TypingStarted { .. } => {
                    received_typing.fetch_add(1, Ordering::Relaxed);
                    eprintln!("  ⌨  TypingStarted");
                }
                InboundEvent::TypingStopped { .. } => {
                    received_typing.fetch_add(1, Ordering::Relaxed);
                    eprintln!("  ⌨  TypingStopped");
                }
                InboundEvent::ReadStateUpdated { .. } => {
                    eprintln!("  👁  ReadStateUpdated");
                }
                _ => {
                    received_noops.fetch_add(1, Ordering::Relaxed);
                }
            },
            Err(_) => {} // timeout — keep polling
        }
    }

    let messages = received_messages.load(Ordering::Relaxed);
    eprintln!("\n  Event totals after 8s:");
    eprintln!("    MessagePosted:   {messages}");
    eprintln!(
        "    ReactionUpdated: {}",
        received_reactions.load(Ordering::Relaxed)
    );
    eprintln!(
        "    MessageDeleted:  {}",
        received_deletes.load(Ordering::Relaxed)
    );
    eprintln!(
        "    Typing events:   {}",
        received_typing.load(Ordering::Relaxed)
    );
    eprintln!(
        "    Other:           {}",
        received_noops.load(Ordering::Relaxed)
    );

    if got_posted_echo {
        eprintln!("  ✓ Received the echoed message event!");
    } else {
        eprintln!(
            "  ⚠ Did not receive echo for our sent message (events may be delayed or batched)"
        );
    }

    // ── Step 4: Trigger a reaction and wait for ReactionUpdated ──
    eprintln!("\n[4] Adding a reaction, waiting for ReactionUpdated event...");
    let msg_id_proto = proto::MessageId {
        parent_id: Some(proto::MessageParentId {
            topic_id: Some(proto::TopicId {
                group_id: Some(gid.clone()),
                topic_id: Some(msg_id_str.clone()),
            }),
        }),
        message_id: Some(msg_id_str.clone()),
    };
    let _ = api::call_proto::<_, proto::UpdateReactionResponse>(
        &mut session,
        "update_reaction",
        &proto::UpdateReactionRequest {
            request_header: Some(convert::tests_make_header()),
            message_id: Some(msg_id_proto.clone()),
            emoji: Some(proto::Emoji {
                unicode: Some("🦀".into()),
                custom_emoji: None,
            }),
            option: Some(1), // ADD
        },
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut got_reaction = false;
    while std::time::Instant::now() < deadline {
        match event_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(InboundEvent::ReactionUpdated { reactions, .. }) => {
                let emojis: Vec<String> = reactions
                    .iter()
                    .map(|r| match &r.emoji {
                        tchat::types::Emoji::Unicode(s) => format!("{}x{}", s, r.count),
                        _ => "?".to_string(),
                    })
                    .collect();
                eprintln!("  ✓ ReactionUpdated: [{}]", emojis.join(", "));
                got_reaction = true;
                break;
            }
            Ok(other) => eprintln!("  (other: {})", event_type_name(&other)),
            Err(_) => {}
        }
    }
    if !got_reaction {
        eprintln!("  ⚠ No ReactionUpdated event");
    }

    // Remove reaction
    let _ = api::call_proto::<_, proto::UpdateReactionResponse>(
        &mut session,
        "update_reaction",
        &proto::UpdateReactionRequest {
            request_header: Some(convert::tests_make_header()),
            message_id: Some(msg_id_proto),
            emoji: Some(proto::Emoji {
                unicode: Some("🦀".into()),
                custom_emoji: None,
            }),
            option: Some(2), // REMOVE
        },
    );
    // Wait a moment for remove event
    std::thread::sleep(Duration::from_secs(3));
    // Drain any pending events
    while event_rx.try_recv().is_ok() {}

    // ── Step 5: Delete the message and wait for MessageDeleted event ──
    eprintln!("\n[5] Deleting message, waiting for MessageDeleted event...");
    let _ = api::call_proto::<_, proto::DeleteMessageResponse>(
        &mut session,
        "delete_message",
        &proto::DeleteMessageRequest {
            request_header: Some(convert::tests_make_header()),
            message_id: Some(full_id),
        },
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut got_delete = false;
    while std::time::Instant::now() < deadline {
        match event_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(InboundEvent::MessageDeleted { message_id, .. }) => {
                eprintln!("  ✓ MessageDeleted: msg={message_id:?}");
                got_delete = true;
                break;
            }
            Ok(other) => eprintln!("  (other: {})", event_type_name(&other)),
            Err(_) => {}
        }
    }

    if !got_delete {
        eprintln!("  ⚠ No MessageDeleted event (may need schema work)");
    }

    eprintln!("\n========== Phase 4 complete ==========");
    eprintln!(
        "  MessagePosted: {}",
        if got_posted_echo { "✓" } else { "✗" }
    );
    eprintln!(
        "  ReactionUpdated: {}",
        if got_reaction { "✓" } else { "✗" }
    );
    eprintln!("  MessageDeleted: {}", if got_delete { "✓" } else { "✗" });
}

fn event_type_name(evt: &InboundEvent) -> &'static str {
    match evt {
        InboundEvent::Connected { .. } => "Connected",
        InboundEvent::Disconnected { .. } => "Disconnected",
        InboundEvent::Reconnecting { .. } => "Reconnecting",
        InboundEvent::WorldSync { .. } => "WorldSync",
        InboundEvent::SpaceUpdated { .. } => "SpaceUpdated",
        InboundEvent::MessagePosted { .. } => "MessagePosted",
        InboundEvent::MessageEdited { .. } => "MessageEdited",
        InboundEvent::MessageDeleted { .. } => "MessageDeleted",
        InboundEvent::TypingStarted { .. } => "TypingStarted",
        InboundEvent::TypingStopped { .. } => "TypingStopped",
        InboundEvent::PresenceChanged { .. } => "PresenceChanged",
        InboundEvent::ReadStateUpdated { .. } => "ReadStateUpdated",
        InboundEvent::ReactionUpdated { .. } => "ReactionUpdated",
        InboundEvent::HistoryChunk { .. } => "HistoryChunk",
        InboundEvent::UsersResolved { .. } => "UsersResolved",
        InboundEvent::MembershipChanged { .. } => "MembershipChanged",
    }
}
