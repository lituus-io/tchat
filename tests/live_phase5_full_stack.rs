//! Phase 5: Full stack integration test.
//!
//! Exercises the same pipeline main.rs uses — `io_loop_with_tokens` running
//! in one thread, commands sent via an outbound channel, events received via
//! an inbound channel. Verifies the store ingests events correctly and the
//! end-to-end flow works without a real TUI.
//!
//! Run:  cargo test --test live_phase5_full_stack -- --ignored --nocapture

use std::time::Duration;

use crossbeam::channel;

use tchat::event::{InboundEvent, OutboundCommand};
use tchat::platform::googlechat::auth;
use tchat::platform::googlechat::io_loop_with_tokens;
use tchat::store::{Store, StoreRead};
use tchat::types::SpaceId;

const TEST_SPACE_ID: &str = "AAQAjslKeUE";

#[test]
#[ignore]
fn full_stack_integration() {
    eprintln!("\n========== Phase 5: Full-stack integration ==========\n");

    // ── Step 1: Authenticate ──
    eprintln!("[1] Authenticating...");
    let tokens = auth::authenticate(None).expect("Auth failed");

    // ── Step 2: Spawn the real io_loop (same as main.rs) ──
    let (inbound_tx, inbound_rx) = channel::unbounded::<InboundEvent>();
    let (cmd_tx, cmd_rx) = channel::unbounded::<OutboundCommand>();

    eprintln!("\n[2] Spawning io_loop_with_tokens (same as main.rs)...");
    let io_thread = std::thread::spawn(move || {
        io_loop_with_tokens(tokens, inbound_tx, cmd_rx);
    });

    let mut store = Store::new();

    // ── Step 3: Drain initial events (WorldSync, Connected, etc.) ──
    eprintln!("\n[3] Draining startup events (30s timeout)...");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut connected = false;
    let mut world_synced_spaces = 0;
    while std::time::Instant::now() < deadline && !(connected && world_synced_spaces > 0) {
        match inbound_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(event) => {
                match &event {
                    InboundEvent::Connected { .. } => {
                        eprintln!("  ✓ Connected");
                        connected = true;
                    }
                    InboundEvent::WorldSync { spaces, .. } => {
                        world_synced_spaces = spaces.len();
                        eprintln!("  ✓ WorldSync — {} spaces", spaces.len());
                        for (i, s) in spaces.iter().take(3).enumerate() {
                            eprintln!("      [{i}] \"{}\"", s.name);
                        }
                    }
                    InboundEvent::Disconnected { reason, .. } => {
                        eprintln!("  ✗ Disconnected: {reason:?}");
                        break;
                    }
                    _ => {}
                }
                store.ingest(event);
            }
            Err(_) => {}
        }
    }

    assert!(connected, "never received Connected event");
    assert!(world_synced_spaces > 0, "WorldSync had no spaces");

    // ── Step 4: Find our test space ──
    eprintln!("\n[4] Locating Test - Gchat space in store...");
    // Look up by interner ID first (bare space ID), then fall back to name
    let target_space: Option<SpaceId> = store
        .spaces_sorted()
        .find(|s| store.interner.resolve(s.id.id) == Some(TEST_SPACE_ID))
        .map(|s| s.id);

    let target_space = target_space.or_else(|| {
        store
            .spaces_sorted()
            .find(|s| s.name.contains("Test") && s.name.contains("Gchat"))
            .map(|s| s.id)
    });

    let target_space = target_space.expect("Test - Gchat space not in store");
    eprintln!("  ✓ Found target space: {:?}", target_space);

    // ── Step 5: Simulate pressing Enter — FetchHistory ──
    eprintln!("\n[5] Dispatching FetchHistory for target space...");
    cmd_tx
        .send(OutboundCommand::FetchHistory {
            space_id: target_space,
            before: tchat::types::Timestamp::MAX,
            count: 20,
        })
        .expect("send FetchHistory");

    // Wait for HistoryChunk
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut got_history = false;
    while std::time::Instant::now() < deadline {
        match inbound_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(event) => {
                if let InboundEvent::HistoryChunk {
                    messages, space_id, ..
                } = &event
                {
                    if *space_id == target_space {
                        eprintln!("  ✓ HistoryChunk — {} messages", messages.len());
                        got_history = true;
                        store.ingest(event);
                        break;
                    }
                }
                store.ingest(event);
            }
            Err(_) => {}
        }
    }
    assert!(got_history, "never received HistoryChunk");

    let history_count = store.messages_in_space(target_space).count();
    eprintln!("  Messages in store for target space: {history_count}");

    // ── Step 6: Send a message ──
    eprintln!("\n[6] Sending message (expecting BrowserChannel echo)...");
    let before_count = store.messages_in_space(target_space).count();

    cmd_tx
        .send(OutboundCommand::SendMessage {
            space_id: target_space,
            text: "tchat full-stack integration test".into(),
            thread_id: None,
        })
        .expect("send SendMessage");

    // Watch for MessagePosted event via BrowserChannel
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut got_message_posted = false;
    while std::time::Instant::now() < deadline {
        match inbound_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(event) => {
                match &event {
                    InboundEvent::MessagePosted { message, .. } => {
                        if message.space_id == target_space
                            && message.text.contains("full-stack integration test")
                        {
                            eprintln!("  ✓ MessagePosted echoed: \"{}\"", message.text);
                            got_message_posted = true;
                        }
                    }
                    _ => {}
                }
                store.ingest(event);
                if got_message_posted {
                    break;
                }
            }
            Err(_) => {}
        }
    }

    let after_count = store.messages_in_space(target_space).count();
    eprintln!("  Messages in store: {before_count} → {after_count}");

    if !got_message_posted {
        eprintln!("  ⚠ Did not receive MessagePosted echo (BrowserChannel may not be wired)");
    }

    // ── Step 7: Cleanup — disconnect ──
    eprintln!("\n[7] Sending Disconnect...");
    let _ = cmd_tx.send(OutboundCommand::Disconnect);
    // Give io_loop 2s to drain
    std::thread::sleep(Duration::from_secs(2));

    eprintln!("\n========== Summary ==========");
    eprintln!(
        "  Connected:          {}",
        if connected { "✓" } else { "✗" }
    );
    eprintln!("  WorldSync (spaces): {}", world_synced_spaces);
    eprintln!(
        "  HistoryChunk:       {}",
        if got_history { "✓" } else { "✗" }
    );
    eprintln!(
        "  MessagePosted echo: {}",
        if got_message_posted { "✓" } else { "✗" }
    );
    eprintln!("  Store messages:     {before_count} → {after_count}");

    drop(cmd_tx);
    let _ = io_thread.join();
}
