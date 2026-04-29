//! Live "command bot" test — reacts to user messages on a threaded space.
//!
//! Listens for trigger messages on AAQAJuwMi-4. On `!hello` replies in the
//! same thread; on `!thread` opens a brand-new thread. Runs for
//! TCHAT_BOT_SECONDS (default 90) then reports how many triggers fired.
//!
//! Drive it from the Google Chat web client: post "!hello" or "!thread" in
//! the test space and watch this terminal.
//!
//! Run:
//!   cargo test --test live_command_bot -- --ignored --nocapture

use std::time::{Duration, Instant};

use tchat::event::InboundEvent;
use tchat::platform::googlechat::{
    api, auth, channel, convert, proto, session::Session, setup_browserchannel,
};

const TEST_SPACE_ID: &str = "AAQAJuwMi-4";
const BOT_PREFIX: &str = "[tchat-bot]";

#[test]
#[ignore]
fn live_command_bot() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let secs: u64 = std::env::var("TCHAT_BOT_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(90);

    eprintln!("\n=========================================================");
    eprintln!("  tchat command-bot live test");
    eprintln!("  space: {TEST_SPACE_ID} (threaded)");
    eprintln!("  triggers: !hello (reply in thread), !thread (open new)");
    eprintln!("  duration: {secs}s");
    eprintln!("=========================================================\n");

    eprintln!("[auth] launching Chrome...");
    let tokens = auth::authenticate(None).expect("auth failed");
    let mut session = Session::new(tokens);
    let _ = session.fetch_session_tokens();
    if let Ok(tab) = session.tokens.get_tab() {
        if let Ok(cookies) = tchat::platform::googlechat::cookies::extract_from_chrome_session(&tab)
        {
            let _ = tchat::platform::googlechat::cookies::save_cookies(&cookies);
        }
    }
    eprintln!("[auth] ✓ ready\n");

    let (event_tx, event_rx) = crossbeam::channel::unbounded::<InboundEvent>();
    spawn_bc(&session, &event_tx);
    std::thread::sleep(Duration::from_secs(3));

    let gid = proto::GroupId {
        space_id: Some(proto::SpaceId {
            space_id: Some(TEST_SPACE_ID.into()),
        }),
        dm_id: None,
    };

    eprintln!("[bot] listening — post \"!hello\" or \"!thread\" in {TEST_SPACE_ID}\n");

    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut hello_count = 0u32;
    let mut thread_count = 0u32;

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = remaining.min(Duration::from_millis(500));
        let event = match event_rx.recv_timeout(timeout) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Auto-reconnect on BC disconnect — keeps the bot listening
        // through session-expired / tab-death / max-retry exits.
        if let InboundEvent::Disconnected { reason, .. } = &event {
            eprintln!("    ⟲ BC disconnected ({reason:?}), reconnecting...");
            spawn_bc(&session, &event_tx);
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        if let InboundEvent::Reconnecting { attempt, .. } = &event {
            eprintln!("    ⟲ BC reconnecting (attempt {attempt})");
            continue;
        }

        let InboundEvent::MessagePosted {
            message,
            space_id_raw,
            topic_id_raw,
            message_id_raw,
        } = event
        else {
            continue;
        };

        // Skip messages we sent ourselves to avoid feedback loops.
        if message.text.starts_with(BOT_PREFIX) {
            continue;
        }

        // Filter by raw space ID — interner-independent string compare.
        if space_id_raw != TEST_SPACE_ID {
            continue;
        }

        let text = message.text.trim();

        if text.starts_with("!hello") {
            eprintln!(
                "    🤖 trigger: !hello in topic={} msg={message_id_raw}",
                topic_id_raw.as_deref().unwrap_or("(none)")
            );
            if reply_in_thread(
                &mut session,
                &gid,
                topic_id_raw.as_deref(),
                &message_id_raw,
                "hi! reacting to !hello inside this thread.",
            ) {
                hello_count += 1;
            }
        } else if text.starts_with("!thread") {
            eprintln!("    🤖 trigger: !thread (will open new thread)");
            if open_new_thread(
                &mut session,
                &gid,
                "hello — opened by tchat command-bot from !thread",
            ) {
                thread_count += 1;
            }
        }
    }

    eprintln!("\n=========================================================");
    eprintln!("  bot summary");
    eprintln!("=========================================================");
    eprintln!("  !hello   triggers handled: {hello_count}");
    eprintln!("  !thread  triggers handled: {thread_count}");
    eprintln!();
}

/// Spin up a fresh BC long-poll thread. Called once at startup and
/// again on `InboundEvent::Disconnected` to keep the connection live.
fn spawn_bc(session: &Session, tx: &crossbeam::channel::Sender<InboundEvent>) {
    eprintln!("[bc] setting up dedicated BC tab + SID...");
    match setup_browserchannel(session) {
        Ok(ctx) => {
            eprintln!("[bc] ✓ SID acquired");
            let tx_clone = tx.clone();
            std::thread::spawn(move || {
                channel::long_poll_loop_threaded(ctx, tx_clone);
            });
        }
        Err(e) => eprintln!("[bc] ✗ setup failed: {e}"),
    }
}

/// Post a reply inside the same thread/topic as the trigger by setting
/// `parent_id.topic_id.topic_id` to the trigger's topic_id. We don't use
/// SendReplyTarget — that's for quote-style replies to a *specific*
/// message and the server rejects the combination in some configurations.
/// Posting in the same topic is the semantic "reply in thread".
fn reply_in_thread(
    session: &mut Session,
    gid: &proto::GroupId,
    topic_str: Option<&str>,
    _reply_to_msg: &str,
    body: &str,
) -> bool {
    let parent_id = Some(proto::MessageParentId {
        topic_id: Some(proto::TopicId {
            group_id: Some(gid.clone()),
            topic_id: topic_str.map(|s| s.to_owned()),
        }),
    });

    match api::call_proto::<_, proto::CreateMessageResponse>(
        session,
        "create_message",
        &proto::CreateMessageRequest {
            request_header: Some(convert::tests_make_header()),
            parent_id,
            text_body: Some(format!("{BOT_PREFIX} {body}")),
            annotations: Vec::new(),
            local_id: Some(format!(
                "bot-hello-{}-{}",
                std::process::id(),
                short_timestamp()
            )),
            message_id: None,
            message_info: Some(proto::MessageInfo {
                accept_format_annotations: Some(true),
                reply_to: None,
            }),
        },
    ) {
        Ok(_) => {
            eprintln!("    ✓ replied in thread");
            true
        }
        Err(e) => {
            eprintln!("    ✗ reply failed: {e}");
            false
        }
    }
}

/// Open a brand-new thread (topic) by posting a top-level message into
/// the space. In a threaded space every top-level CreateMessage request
/// creates a topic implicitly; explicit create_topic isn't reliable
/// (returns Ok with an empty response on some configurations).
fn open_new_thread(session: &mut Session, gid: &proto::GroupId, body: &str) -> bool {
    match api::call_proto::<_, proto::CreateMessageResponse>(
        session,
        "create_message",
        &proto::CreateMessageRequest {
            request_header: Some(convert::tests_make_header()),
            // No topic_id → server creates a new topic with this message
            // as its anchor (in a threaded space).
            parent_id: Some(proto::MessageParentId {
                topic_id: Some(proto::TopicId {
                    group_id: Some(gid.clone()),
                    topic_id: None,
                }),
            }),
            text_body: Some(format!("{BOT_PREFIX} {body}")),
            annotations: Vec::new(),
            local_id: Some(format!(
                "bot-thread-{}-{}",
                std::process::id(),
                short_timestamp()
            )),
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
            eprintln!("    ✓ new thread opened (msg_id={mid})");
            true
        }
        Err(e) => {
            eprintln!("    ✗ create new thread failed: {e}");
            false
        }
    }
}

fn short_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
