//! Test the BrowserChannel long-poll for real-time events.
//!
//! Run:  cargo test --test live_streamevents -- --ignored --nocapture

use tchat::platform::googlechat::{auth, session::Session};

#[test]
#[ignore]
fn test_browserchannel_register() {
    eprintln!("\n========== BrowserChannel register ==========\n");

    let tokens = auth::authenticate(None).expect("Auth failed");
    let mut session = Session::new(tokens);
    let _ = session.fetch_session_tokens();

    eprintln!("[1] Registering BrowserChannel (sets cookies)...");
    match session.register() {
        Ok(()) => eprintln!("  ✓ registered"),
        Err(e) => {
            eprintln!("  ✗ Register failed: {e}");
            return;
        }
    }

    eprintln!("\n[1b] Acquiring SID via bootstrap long-poll...");
    match session.acquire_sid() {
        Ok(()) => eprintln!("  ✓ SID: {:?}", session.sid),
        Err(e) => {
            eprintln!("  ✗ SID acquisition failed: {e}");
            return;
        }
    }

    // Try one long-poll request directly via Chrome
    eprintln!("\n[2] Attempting one long-poll request (10s timeout)...");
    let sid = session.sid.clone().unwrap();
    let zx = Session::random_zx();
    let url = format!(
        "https://chat.google.com/u/0/webchannel/events?\
         VER=8&RID=rpc&SID={sid}&AID=0&TYPE=xmlhttp&CI=0&t=1&zx={zx}"
    );
    eprintln!("  URL: {url}");

    let start = std::time::Instant::now();
    match session.tokens.fetch_get_binary(&url) {
        Ok(bytes) => {
            let elapsed = start.elapsed().as_secs_f64();
            eprintln!("  ✓ Got {} bytes in {:.1}s", bytes.len(), elapsed);
            if !bytes.is_empty() {
                let preview =
                    std::str::from_utf8(&bytes[..bytes.len().min(300)]).unwrap_or("(non-UTF8)");
                eprintln!("  Preview: {preview}");
            }
        }
        Err(e) => {
            eprintln!("  ✗ Long-poll failed: {e}");
        }
    }
}
