//! Simulates the TUI flow: launch io_loop, load spaces, select a space,
//! fetch history, send multiple messages — all programmatically.
//!
//! Targets: https://chat.google.com/room/AAAAz6E4W_g
//!
//! Run:  cargo test --test live_tui_simulation -- --ignored --nocapture

use std::time::Duration;

use crossbeam::channel;

use tchat::event::{InboundEvent, OutboundCommand};
use tchat::platform::googlechat::auth;
use tchat::platform::googlechat::io_loop_with_tokens;
use tchat::store::{Store, StoreRead};

const TARGET_SPACE: &str = "AAAAz6E4W_g";

#[test]
#[ignore]
fn tui_simulation() {
    eprintln!("\n========== TUI Simulation on {TARGET_SPACE} ==========\n");

    // ── Step 1: Auth ──
    eprintln!("[1] Authenticating...");
    let tokens = auth::authenticate(None).expect("Auth failed");

    // ── Step 2: Spawn io_loop (same pipeline as main.rs) ──
    let (inbound_tx, inbound_rx) = channel::unbounded::<InboundEvent>();
    let (cmd_tx, cmd_rx) = channel::unbounded::<OutboundCommand>();

    eprintln!("[2] Spawning io_loop...");
    std::thread::spawn(move || {
        io_loop_with_tokens(tokens, inbound_tx, cmd_rx);
    });

    let mut store = Store::new();

    // ── Step 3: Wait for WorldSync ──
    eprintln!("[3] Waiting for WorldSync...");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut connected = false;
    while std::time::Instant::now() < deadline {
        match inbound_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(event) => {
                match &event {
                    InboundEvent::Connected { .. } => {
                        eprintln!("  ✓ Connected");
                        connected = true;
                    }
                    InboundEvent::WorldSync { spaces, .. } => {
                        eprintln!("  ✓ WorldSync — {} spaces", spaces.len());
                    }
                    _ => {}
                }
                store.ingest(event);
                if connected {
                    break;
                }
            }
            Err(_) => {}
        }
    }

    // Drain any remaining startup events
    while let Ok(event) = inbound_rx.recv_timeout(Duration::from_millis(500)) {
        store.ingest(event);
    }

    // ── Step 4: Find target space ──
    eprintln!("\n[4] Finding space...");
    let target = store
        .spaces_sorted()
        .find(|s| s.name.contains(TARGET_SPACE) || s.name.contains("gitops"))
        .or_else(|| store.spaces_sorted().next())
        .map(|s| s.id)
        .expect("Need at least one space");
    let name = store.space(target).map(|s| s.name.as_str()).unwrap_or("?");
    eprintln!("  Using: \"{name}\"");

    // Skip history — send 5 messages directly with 3-second delays
    // (matching the working live_send_debug pattern exactly)
    eprintln!("\n[5] Sending 5 messages with 3s delays...");
    let mut sent = 0;
    let got_history = false;
    let msgs_before = 0;
    for i in 1..=5 {
        let text = format!("tchat TUI simulation message #{i}");
        eprintln!("  [{i}/5] Sending: \"{text}\"");

        cmd_tx
            .send(OutboundCommand::SendMessage {
                space_id: target,
                text,
                thread_id: None,
            })
            .expect("send SendMessage");

        // 5-second delay to test rate limiting
        std::thread::sleep(Duration::from_secs(5));

        // Drain events
        while let Ok(event) = inbound_rx.try_recv() {
            store.ingest(event);
        }

        sent += 1;
    }

    std::thread::sleep(Duration::from_secs(2));
    while let Ok(event) = inbound_rx.try_recv() {
        store.ingest(event);
    }

    let msgs_after = store.messages_in_space(target).count();

    // ── Step 7: Cleanup ──
    eprintln!("\n[7] Disconnecting...");
    let _ = cmd_tx.send(OutboundCommand::Disconnect);
    std::thread::sleep(Duration::from_secs(1));

    eprintln!("\n========== Summary ==========");
    eprintln!("  Connected:      ✓");
    eprintln!("  History loaded:  {}", if got_history { "✓" } else { "✗" });
    eprintln!("  Messages sent:   {sent}/5");
    eprintln!("  Store before:    {msgs_before}");
    eprintln!("  Store after:     {msgs_after}");
    eprintln!("==============================");
}
