//! Wire-format capture harness for Google Chat real-time events.
//!
//! Runs against the dedicated test space (AAAAz6E4W_g) via Chrome.
//! Uses saved cookies if present (skips login). Drives every event type,
//! reports observed events. Set TCHAT_BC_DUMP=1 to print raw chunk JSON.
//!
//! Run:
//!   cargo test --test live_event_capture -- --ignored --nocapture

use std::time::{Duration, Instant};

use crossbeam::channel::Receiver;

use tchat::event::InboundEvent;
use tchat::platform::googlechat::{api, auth, channel, convert, proto, session::Session};
use tchat::types::{MembershipState, PresenceStatus};

const TEST_SPACE_ID: &str = "AAAAz6E4W_g";

#[derive(Default)]
struct Observed {
    message_posted: bool,
    message_edited: bool,
    message_deleted: bool,
    reaction_added: bool,
    reaction_removed: bool,
    typing_started: bool,
    typing_stopped: bool,
    space_updated: bool,
    presence_changed: bool,
    membership_changed: bool,
    read_state: bool,
    other_count: usize,
}

#[test]
#[ignore]
fn reverse_engineer_realtime_events() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::WARN)
        .try_init();

    eprintln!("\n=========================================================");
    eprintln!("  tchat real-time event capture harness");
    eprintln!("  Test space: {TEST_SPACE_ID}");
    eprintln!(
        "  Raw frame dump: {}",
        std::env::var("TCHAT_BC_DUMP").unwrap_or_default() == "1"
    );
    eprintln!("=========================================================\n");

    eprintln!("[auth] launching Chrome / loading cookies...");
    let tokens = auth::authenticate(None).expect("auth failed");
    let mut session = Session::new(tokens);
    let _ = session.fetch_session_tokens();
    let _ = session.ensure_clean_api_tab();

    if let Ok(tab) = session.tokens.get_tab() {
        if let Ok(cookies) = tchat::platform::googlechat::cookies::extract_from_chrome_session(&tab)
        {
            let _ = tchat::platform::googlechat::cookies::save_cookies(&cookies);
        }
    }
    eprintln!("[auth] ✓ session ready\n");

    eprintln!("[bc] registering BrowserChannel...");
    session.register().expect("register failed");
    session.acquire_sid().expect("acquire_sid failed");
    eprintln!(
        "[bc] ✓ SID acquired ({} chars)",
        session.sid.as_ref().map(|s| s.len()).unwrap_or(0)
    );

    let (event_tx, event_rx) = crossbeam::channel::unbounded::<InboundEvent>();
    let tab = session.tokens.get_tab().expect("no tab");
    let sid = session.sid.clone().expect("no sid");
    let ctx = channel::StreamingContext { tab, sid };
    std::thread::spawn(move || channel::long_poll_loop_threaded(ctx, event_tx));
    std::thread::sleep(Duration::from_secs(4));
    eprintln!("[bc] ✓ long-poll thread up\n");

    let gid = proto::GroupId {
        space_id: Some(proto::SpaceId {
            space_id: Some(TEST_SPACE_ID.into()),
        }),
        dm_id: None,
    };
    let mut observed = Observed::default();

    eprintln!("[run] send_message");
    let msg_id = send_message(&mut session, &gid);
    drain(&event_rx, Duration::from_secs(8), &mut observed);

    if let Some(mid) = msg_id.clone() {
        eprintln!("[run] edit_message");
        edit_message(&mut session, &mid);
        drain(&event_rx, Duration::from_secs(8), &mut observed);

        eprintln!("[run] add_reaction 🦀");
        update_reaction(&mut session, &mid, "🦀", 1);
        drain(&event_rx, Duration::from_secs(8), &mut observed);

        eprintln!("[run] remove_reaction 🦀");
        update_reaction(&mut session, &mid, "🦀", 2);
        drain(&event_rx, Duration::from_secs(6), &mut observed);
    }

    eprintln!("[run] set_typing(STARTED)");
    set_typing(&mut session, &gid, 1);
    drain(&event_rx, Duration::from_secs(5), &mut observed);
    eprintln!("[run] set_typing(STOPPED)");
    set_typing(&mut session, &gid, 2);
    drain(&event_rx, Duration::from_secs(5), &mut observed);

    if let Some(mid) = msg_id {
        eprintln!("[run] delete_message");
        delete_message(&mut session, &mid);
        drain(&event_rx, Duration::from_secs(8), &mut observed);
    }

    eprintln!("[run] set_dnd(DND, 60s)");
    set_dnd(&mut session, true);
    drain(&event_rx, Duration::from_secs(5), &mut observed);
    eprintln!("[run] set_dnd(AVAILABLE)");
    set_dnd(&mut session, false);
    drain(&event_rx, Duration::from_secs(5), &mut observed);

    eprintln!("[run] rename_space → probed");
    let probed = format!("test - google chat api (probed {})", short_timestamp());
    rename_space(&mut session, &gid, &probed);
    drain(&event_rx, Duration::from_secs(8), &mut observed);
    eprintln!("[run] rename_space → restore");
    rename_space(&mut session, &gid, "test - google chat api");
    drain(&event_rx, Duration::from_secs(8), &mut observed);

    drain(&event_rx, Duration::from_secs(3), &mut observed);

    eprintln!("\n=========================================================");
    eprintln!("  Observed events");
    eprintln!("=========================================================");
    report("MessagePosted", observed.message_posted);
    report("MessageEdited", observed.message_edited);
    report("MessageDeleted", observed.message_deleted);
    report("ReactionUpdated +", observed.reaction_added);
    report("ReactionUpdated -", observed.reaction_removed);
    report("TypingStarted", observed.typing_started);
    report("TypingStopped", observed.typing_stopped);
    report("SpaceUpdated", observed.space_updated);
    report("PresenceChanged", observed.presence_changed);
    report("MembershipChanged", observed.membership_changed);
    report("ReadStateUpdated", observed.read_state);
    eprintln!(
        "  Other (Connected/Reconnecting/etc): {}",
        observed.other_count
    );
    eprintln!("=========================================================");

    if !observed.membership_changed {
        eprintln!("\nNote: MembershipChanged needs a 2nd user joining/leaving");
        eprintln!("      {TEST_SPACE_ID} during the test — won't fire from self.");
    }

    let uncovered = channel::uncovered_event_types();
    if !uncovered.is_empty() {
        eprintln!("\nUncovered body event_types seen but not dispatched:");
        for et in &uncovered {
            eprintln!("  • event_type={et} ({})", event_type_name(*et));
        }
        eprintln!("  (Bodies with these types arrived but no recognized field");
        eprintln!("   was set. Either the proto definition omits the field, or");
        eprintln!("   we have no dispatch handler for it.)");
    }
    eprintln!();
}

/// Passive watch mode — listens to BrowserChannel for `TCHAT_WATCH_SECONDS`
/// (default 120) without issuing any API actions. Prints every dispatched
/// event plus an uncovered summary at the end.
///
/// Run alongside organic Chat activity (you typing in the web client,
/// coworkers messaging, etc.) to catch self-suppressed events like
/// TypingStarted/Stopped and MembershipChanged from a 2nd participant.
///
/// Run:
///   TCHAT_WATCH_SECONDS=300 cargo test --test live_event_capture passive_watch -- --ignored --nocapture
#[test]
#[ignore]
fn passive_watch() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let secs: u64 = std::env::var("TCHAT_WATCH_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);

    eprintln!("\n=========================================================");
    eprintln!("  passive watch mode — listening for {secs}s");
    eprintln!("  drive activity in Chat (web/mobile) to populate events");
    eprintln!(
        "  raw frame dump: {}",
        std::env::var("TCHAT_BC_DUMP").unwrap_or_default() == "1"
    );
    eprintln!("=========================================================\n");

    eprintln!("[auth] launching Chrome / loading cookies...");
    let tokens = auth::authenticate(None).expect("auth failed");
    let mut session = Session::new(tokens);
    let _ = session.fetch_session_tokens();
    let _ = session.ensure_clean_api_tab();
    if let Ok(tab) = session.tokens.get_tab() {
        if let Ok(cookies) = tchat::platform::googlechat::cookies::extract_from_chrome_session(&tab)
        {
            let _ = tchat::platform::googlechat::cookies::save_cookies(&cookies);
        }
    }
    eprintln!("[auth] ✓ ready\n");

    eprintln!("[bc] registering...");
    session.register().expect("register failed");
    session.acquire_sid().expect("acquire_sid failed");
    eprintln!("[bc] ✓ SID acquired");

    let (event_tx, event_rx) = crossbeam::channel::unbounded::<InboundEvent>();
    let tab = session.tokens.get_tab().expect("no tab");
    let sid = session.sid.clone().expect("no sid");
    let ctx = channel::StreamingContext { tab, sid };
    std::thread::spawn(move || channel::long_poll_loop_threaded(ctx, event_tx));
    std::thread::sleep(Duration::from_secs(3));
    eprintln!("[bc] ✓ listening — go drive activity in Chat now\n");

    let mut observed = Observed::default();
    drain(&event_rx, Duration::from_secs(secs), &mut observed);

    eprintln!("\n=========================================================");
    eprintln!("  passive watch summary ({secs}s)");
    eprintln!("=========================================================");
    report("MessagePosted", observed.message_posted);
    report("MessageEdited", observed.message_edited);
    report("MessageDeleted", observed.message_deleted);
    report(
        "ReactionUpdated",
        observed.reaction_added || observed.reaction_removed,
    );
    report("TypingStarted", observed.typing_started);
    report("TypingStopped", observed.typing_stopped);
    report("SpaceUpdated", observed.space_updated);
    report("PresenceChanged", observed.presence_changed);
    report("MembershipChanged", observed.membership_changed);
    report("ReadStateUpdated", observed.read_state);
    eprintln!("  Other: {}", observed.other_count);

    let uncovered = channel::uncovered_event_types();
    if !uncovered.is_empty() {
        eprintln!("\nUncovered body event_types observed:");
        for et in &uncovered {
            eprintln!("  • event_type={et} ({})", event_type_name(*et));
        }
    }
    eprintln!();
}

/// Best-effort label for proto Event::EventType values, for diagnostics.
fn event_type_name(et: i32) -> &'static str {
    match et {
        0 => "UNKNOWN",
        1 => "USER_ADDED_TO_GROUP",
        2 => "USER_REMOVED_FROM_GROUP",
        3 => "GROUP_VIEWED",
        4 => "TOPIC_VIEWED",
        5 => "GROUP_UPDATED",
        6 => "MESSAGE_POSTED",
        7 => "MESSAGE_UPDATED",
        8 => "MESSAGE_DELETED",
        9 => "TOPIC_MUTE_CHANGED",
        10 => "USER_SETTINGS_CHANGED",
        11 => "GROUP_STARRED",
        12 => "WEB_PUSH_NOTIFICATION",
        14 => "INVITE_COUNT_UPDATED",
        15 => "MEMBERSHIP_CHANGED",
        20 => "TOPIC_CREATED",
        24 => "MESSAGE_REACTED",
        25 => "USER_STATUS_UPDATED_EVENT",
        29 => "TYPING_STATE_CHANGED",
        33 => "SESSION_READY",
        34 => "GROUP_SORT_TIMESTAMP_CHANGED",
        36 => "READ_RECEIPT_CHANGED",
        43 => "USER_PRESENCE_SHARED_UPDATED_EVENT",
        58 => "BATCH_REACTIONS_UPDATED",
        64 => "GROUP_DEFAULT_SORT_ORDER_UPDATED",
        82 => "TOPIC_METADATA_UPDATED",
        83 => "GROUP_READ_STATE_UPDATED",
        _ => "(see proto)",
    }
}

fn send_message(session: &mut Session, gid: &proto::GroupId) -> Option<proto::MessageId> {
    match api::call_proto::<_, proto::CreateMessageResponse>(
        session,
        "create_message",
        &proto::CreateMessageRequest {
            request_header: Some(convert::tests_make_header()),
            parent_id: Some(proto::MessageParentId {
                topic_id: Some(proto::TopicId {
                    group_id: Some(gid.clone()),
                    topic_id: None,
                }),
            }),
            text_body: Some(format!("rev-eng harness — t={}", short_timestamp())),
            annotations: Vec::new(),
            local_id: Some(format!("rev-eng-{}", std::process::id())),
            message_id: None,
            message_info: Some(proto::MessageInfo {
                accept_format_annotations: Some(true),
                reply_to: None,
            }),
        },
    ) {
        Ok(resp) => {
            let id = resp.message.and_then(|m| m.id);
            eprintln!(
                "    ✓ sent, msg_id={:?}",
                id.as_ref().and_then(|i| i.message_id.as_deref())
            );
            id
        }
        Err(e) => {
            eprintln!("    ✗ create_message FAILED: {e}");
            None
        }
    }
}

fn edit_message(session: &mut Session, mid: &proto::MessageId) {
    match api::call_proto::<_, proto::EditMessageResponse>(
        session,
        "edit_message",
        &proto::EditMessageRequest {
            request_header: Some(convert::tests_make_header()),
            message_id: Some(mid.clone()),
            text_body: Some(format!("rev-eng EDITED — t={}", short_timestamp())),
            annotations: Vec::new(),
            message_info: Some(proto::MessageInfo {
                accept_format_annotations: Some(true),
                reply_to: None,
            }),
        },
    ) {
        Ok(_) => eprintln!("    ✓ edited"),
        Err(e) => eprintln!("    ✗ edit_message FAILED: {e}"),
    }
}

fn update_reaction(session: &mut Session, mid: &proto::MessageId, emoji: &str, option: i32) {
    match api::call_proto::<_, proto::UpdateReactionResponse>(
        session,
        "update_reaction",
        &proto::UpdateReactionRequest {
            request_header: Some(convert::tests_make_header()),
            message_id: Some(mid.clone()),
            emoji: Some(proto::Emoji {
                unicode: Some(emoji.into()),
                custom_emoji: None,
            }),
            option: Some(option),
        },
    ) {
        Ok(_) => eprintln!("    ✓ reaction updated"),
        Err(e) => eprintln!("    ✗ update_reaction FAILED: {e}"),
    }
}

fn delete_message(session: &mut Session, mid: &proto::MessageId) {
    match api::call_proto::<_, proto::DeleteMessageResponse>(
        session,
        "delete_message",
        &proto::DeleteMessageRequest {
            request_header: Some(convert::tests_make_header()),
            message_id: Some(mid.clone()),
        },
    ) {
        Ok(_) => eprintln!("    ✓ deleted"),
        Err(e) => eprintln!("    ✗ delete_message FAILED: {e}"),
    }
}

fn set_typing(session: &mut Session, gid: &proto::GroupId, state: i32) {
    if let Err(e) = api::call_proto::<_, proto::SetTypingStateResponse>(
        session,
        "set_typing_state",
        &proto::SetTypingStateRequest {
            request_header: Some(convert::tests_make_header()),
            context: Some(proto::TypingContext {
                group_id: Some(gid.clone()),
                topic_id: None,
            }),
            state: Some(state),
        },
    ) {
        eprintln!("    ✗ set_typing FAILED: {e}");
    }
}

fn set_dnd(session: &mut Session, dnd: bool) {
    // current_dnd_state is the user's CURRENT state (before the call).
    // new_dnd_duration_usec >0 enters DND; 0 exits.
    if let Err(e) = api::call_proto::<_, proto::SetDndDurationResponse>(
        session,
        "set_dnd_duration",
        &proto::SetDndDurationRequest {
            request_header: Some(convert::tests_make_header()),
            current_dnd_state: Some(if dnd { 1 } else { 2 }),
            new_dnd_duration_usec: Some(if dnd { 60_000_000 } else { 0 }),
            dnd_expiry_timestamp_usec: None,
        },
    ) {
        eprintln!("    ✗ set_dnd FAILED: {e}");
    }
}

fn rename_space(session: &mut Session, gid: &proto::GroupId, new_name: &str) {
    let space = match &gid.space_id {
        Some(s) => proto::SpaceId {
            space_id: s.space_id.clone(),
        },
        None => return,
    };
    if let Err(e) = api::call_proto::<_, proto::UpdateGroupResponse>(
        session,
        "update_group",
        &proto::UpdateGroupRequest {
            request_header: Some(convert::tests_make_header()),
            space_id: Some(space),
            update_masks: vec![1],
            name: Some(new_name.into()),
            visibility: None,
        },
    ) {
        eprintln!("    ✗ rename_space FAILED: {e}");
    }
}

fn drain(rx: &Receiver<InboundEvent>, max: Duration, obs: &mut Observed) {
    let deadline = Instant::now() + max;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(ev) => classify(&ev, obs),
            Err(_) => {}
        }
    }
}

fn classify(ev: &InboundEvent, obs: &mut Observed) {
    match ev {
        InboundEvent::MessagePosted { message, .. } => {
            eprintln!("    📨 MessagePosted: \"{}\"", short(&message.text, 60));
            obs.message_posted = true;
        }
        InboundEvent::MessageEdited { message, .. } => {
            eprintln!("    ✏  MessageEdited: \"{}\"", short(&message.text, 60));
            obs.message_edited = true;
        }
        InboundEvent::MessageDeleted { .. } => {
            eprintln!("    🗑  MessageDeleted");
            obs.message_deleted = true;
        }
        InboundEvent::ReactionUpdated { reactions, .. } => {
            let total: u32 = reactions.iter().map(|r| r.count).sum();
            eprintln!(
                "    👍 ReactionUpdated ({} kinds, total={total})",
                reactions.len()
            );
            if total > 0 {
                obs.reaction_added = true;
            } else {
                obs.reaction_removed = true;
            }
        }
        InboundEvent::TypingStarted { .. } => {
            eprintln!("    ⌨  TypingStarted");
            obs.typing_started = true;
        }
        InboundEvent::TypingStopped { .. } => {
            eprintln!("    ⌨  TypingStopped");
            obs.typing_stopped = true;
        }
        InboundEvent::SpaceUpdated { space } => {
            eprintln!("    🏠 SpaceUpdated: name=\"{}\"", space.name);
            obs.space_updated = true;
        }
        InboundEvent::PresenceChanged { presence, .. } => {
            let label = match presence {
                PresenceStatus::Active => "Active",
                PresenceStatus::Inactive => "Inactive",
                PresenceStatus::Dnd => "Dnd",
                PresenceStatus::Unknown => "Unknown",
            };
            eprintln!("    🟢 PresenceChanged: {label}");
            obs.presence_changed = true;
        }
        InboundEvent::MembershipChanged { state, .. } => {
            let s = match state {
                MembershipState::Joined => "Joined",
                MembershipState::Invited => "Invited",
                MembershipState::Left => "Left",
                MembershipState::Unknown => "Unknown",
            };
            eprintln!("    👥 MembershipChanged: {s}");
            obs.membership_changed = true;
        }
        InboundEvent::ReadStateUpdated { .. } => {
            eprintln!("    👁  ReadStateUpdated");
            obs.read_state = true;
        }
        _ => {
            obs.other_count += 1;
        }
    }
}

fn report(label: &str, ok: bool) {
    let mark = if ok {
        "\x1b[32m✓\x1b[0m"
    } else {
        "\x1b[31m✗\x1b[0m"
    };
    eprintln!("  {mark}  {label}");
}

fn short(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_owned()
    } else {
        format!("{}...", &s[..n])
    }
}

fn short_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
