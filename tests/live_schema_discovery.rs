//! Discover the real update_reaction schema by inspecting the web client's
//! loaded JS bundle and installing strategic debugger hooks.
//!
//! Run:  cargo test --test live_schema_discovery -- --ignored --nocapture

use tchat::platform::googlechat::{auth, session::Session};

#[test]
#[ignore]
fn discover_reaction_schema() {
    eprintln!("\n========== Reaction schema discovery ==========\n");

    let tokens = auth::authenticate(None).expect("Auth failed");
    let mut session = Session::new(tokens);
    let _ = session.fetch_session_tokens();

    let tab = session.tokens.get_tab().expect("No tab");

    eprintln!("[1] Navigating to chat...");
    let _ = tab.evaluate(
        "(() => { window.location.href = 'https://chat.google.com/room/AAQAjslKeUE?cls=7'; return 'ok'; })()",
        true,
    );
    std::thread::sleep(std::time::Duration::from_secs(8));

    // ── Install a hook that captures FULL detail on any update_reaction call,
    // INCLUDING a JavaScript stack trace. This reveals what code is sending
    // the request and we can use that to find the object structure.
    eprintln!("\n[2] Installing deep capture hook with stack traces...");
    let deep_hook = r#"
    (() => {
        window._deep_cap = [];
        const _oOpen = XMLHttpRequest.prototype.open;
        const _oSend = XMLHttpRequest.prototype.send;
        const _oSetH = XMLHttpRequest.prototype.setRequestHeader;
        XMLHttpRequest.prototype.open = function(m, u, ...r) {
            this._u = u; this._m = m; this._h = {};
            return _oOpen.call(this, m, u, ...r);
        };
        XMLHttpRequest.prototype.setRequestHeader = function(k, v) {
            if (this._h) this._h[k] = v;
            return _oSetH.call(this, k, v);
        };
        XMLHttpRequest.prototype.send = function(body) {
            const u = this._u || '';
            if (u.includes('/api/')) {
                const endpoint = (u.match(/\/api\/([a-z_]+)/) || [])[1] || '?';
                const entry = {
                    url: u,
                    endpoint: endpoint,
                    headers: Object.assign({}, this._h),
                    body: body ? String(body) : null,
                    time: Date.now()
                };
                // For reaction calls, capture a stack trace
                if (endpoint.includes('react') || endpoint === 'update_reaction') {
                    try {
                        throw new Error('_stack_');
                    } catch (e) {
                        entry.stack = e.stack;
                    }
                }
                this.addEventListener('load', function() {
                    entry.status = this.status;
                });
                window._deep_cap.push(entry);
            }
            return _oSend.call(this, body);
        };
        return 'hook installed';
    })()
    "#;
    if let Ok(r) = tab.evaluate(deep_hook, false) {
        eprintln!("  {:?}", r.value);
    }

    // ── Fetch the Chat JS bundle and search for update_reaction patterns ──
    eprintln!("\n[3] Searching loaded JS bundles for 'update_reaction'...");
    let grep_js = r#"
    (async () => {
        try {
            // Find all <script src=...> tags
            const scripts = Array.from(document.querySelectorAll('script[src]'))
                .map(s => s.src)
                .filter(src => src.includes('gstatic') || src.includes('google.com'));

            // Filter to the main JS bundles
            const bundles = scripts.filter(s => s.includes('dynamite') || s.includes('DynamiteWebUi'));

            const results = [];
            for (const url of bundles.slice(0, 3)) {
                try {
                    const resp = await fetch(url);
                    const text = await resp.text();
                    // Search for "update_reaction" and capture surrounding bytes
                    const idx = text.indexOf('update_reaction');
                    if (idx >= 0) {
                        // Get 800 chars around the match
                        const ctx = text.substring(Math.max(0, idx - 400), idx + 400);
                        results.push({
                            url: url.substring(url.length - 60),
                            matchIdx: idx,
                            context: ctx
                        });
                    }
                } catch (e) {
                    results.push({url, error: e.message});
                }
            }

            return JSON.stringify({
                bundlesScanned: bundles.length,
                totalScripts: scripts.length,
                hits: results
            });
        } catch (e) { return JSON.stringify({fatal: e.message}); }
    })()
    "#;
    if let Ok(r) = tab.evaluate(grep_js, true) {
        if let Some(s) = r.value.and_then(|v| v.as_str().map(|s| s.to_owned())) {
            eprintln!("  Bundle search (first 4000): {}", &s[..s.len().min(4000)]);
        }
    }

    // ── Also search for the proto field numbers the web client uses for reactions ──
    eprintln!("\n[4] Looking for reaction proto field patterns in bundle...");
    let proto_search_js = r#"
    (async () => {
        try {
            const scripts = Array.from(document.querySelectorAll('script[src]'))
                .map(s => s.src)
                .filter(src => src.includes('gstatic') || src.includes('google.com'))
                .filter(s => s.includes('dynamite') || s.includes('DynamiteWebUi'));

            const results = [];
            for (const url of scripts.slice(0, 3)) {
                try {
                    const resp = await fetch(url);
                    const text = await resp.text();

                    // Look for Reaction-related class names and their field numbers
                    // Google's code has patterns like "UpdateReactionRequest" or "zBd"
                    // near proto definitions with .setField calls
                    const patterns = [
                        /UpdateReaction(?:Request|Response)/g,
                        /update_reaction"[^"]{0,200}/g,
                        // Look for common proto builder patterns near reaction
                        /react[a-zA-Z_]{0,30}/gi,
                    ];

                    const hits = [];
                    for (const p of patterns) {
                        const matches = text.match(p);
                        if (matches) {
                            const unique = [...new Set(matches)].slice(0, 20);
                            hits.push({pattern: p.toString(), matches: unique});
                        }
                    }
                    results.push({url: url.substring(url.length - 60), hits});
                } catch (e) {
                    results.push({url, error: e.message});
                }
            }
            return JSON.stringify(results);
        } catch (e) { return JSON.stringify({fatal: e.message}); }
    })()
    "#;
    if let Ok(r) = tab.evaluate(proto_search_js, true) {
        if let Some(s) = r.value.and_then(|v| v.as_str().map(|s| s.to_owned())) {
            eprintln!("  Proto search (first 4000): {}", &s[..s.len().min(4000)]);
        }
    }

    // ── Install CDP-based hover + click on a message to trigger reaction UI ──
    eprintln!("\n[5] Looking for message elements to simulate a reaction...");
    let find_msg_js = r#"
    (() => {
        // Messages in Google Chat are usually rendered with data-message-id or similar
        const selectors = [
            '[data-message-id]',
            '[data-topic-id]',
            '[data-id]',
            '[role="listitem"]',
            '[role="article"]',
            '.DKlmJd',  // common Gchat message class prefix
        ];
        for (const sel of selectors) {
            const elts = document.querySelectorAll(sel);
            if (elts.length > 0) {
                const first = elts[0];
                const rect = first.getBoundingClientRect();
                return JSON.stringify({
                    selector: sel,
                    count: elts.length,
                    firstAttrs: Array.from(first.attributes).map(a => `${a.name}=${a.value.substring(0,40)}`).slice(0,10),
                    firstRect: [rect.left, rect.top, rect.width, rect.height]
                });
            }
        }
        return JSON.stringify({found: false, tried: selectors});
    })()
    "#;
    if let Ok(r) = tab.evaluate(find_msg_js, false) {
        if let Some(s) = r.value.and_then(|v| v.as_str().map(|s| s.to_owned())) {
            eprintln!("  {}", s);
        }
    }

    // ── Try to use CDP Input.dispatchMouseEvent to hover over a message ──
    // First, get the center point of the first message, then hover, then look for react button
    eprintln!("\n[6] Trying DOM mouse events on first message...");
    let dom_hover_js = r#"
    (async () => {
        const msgs = document.querySelectorAll('[data-message-id], [role="listitem"], [data-topic-id]');
        if (msgs.length === 0) return JSON.stringify({error: 'no messages'});

        const msg = msgs[msgs.length - 1]; // last message (most likely our test msg)
        const rect = msg.getBoundingClientRect();

        // Dispatch synthetic mouse events on the message
        for (const evt of ['mouseenter', 'mouseover', 'mousemove']) {
            msg.dispatchEvent(new MouseEvent(evt, {
                bubbles: true, cancelable: true,
                clientX: rect.left + rect.width / 2,
                clientY: rect.top + rect.height / 2
            }));
        }

        await new Promise(r => setTimeout(r, 500));

        // Look for buttons that appeared
        const allButtons = Array.from(document.querySelectorAll('[role="button"], button'));
        const reactBtns = [];
        for (const b of allButtons) {
            const label = b.getAttribute('aria-label') || '';
            const title = b.getAttribute('data-tooltip') || b.title || '';
            if ((label + ' ' + title).toLowerCase().match(/react|emoji|smile|👍/)) {
                reactBtns.push({
                    label: label.substring(0, 60),
                    title: title.substring(0, 60),
                    tag: b.tagName,
                    visible: b.getBoundingClientRect().width > 0
                });
                if (reactBtns.length >= 5) break;
            }
        }

        return JSON.stringify({
            numMsgs: msgs.length,
            lastMsgRect: [rect.left, rect.top, rect.width, rect.height],
            reactBtns
        });
    })()
    "#;
    if let Ok(r) = tab.evaluate(dom_hover_js, true) {
        if let Some(s) = r.value.and_then(|v| v.as_str().map(|s| s.to_owned())) {
            eprintln!("  {}", s);
        }
    }

    // Wait for any reactions captured
    eprintln!("\n[7] Waiting 60s — please hover a message in Chrome and click the smiley+ button");
    eprintln!("    (alternatively: click any existing reaction chip on a message)\n");
    for sec in 0..60 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if let Ok(r) = tab.evaluate("JSON.stringify(window._deep_cap || [])", false) {
            if let Some(t) = r.value.and_then(|v| v.as_str().map(|s| s.to_owned())) {
                let caps: Vec<serde_json::Value> = serde_json::from_str(&t).unwrap_or_default();
                // Check for any reaction capture
                let reaction = caps.iter().find(|c| {
                    c.get("endpoint")
                        .and_then(|v| v.as_str())
                        .map(|e| e.contains("react"))
                        .unwrap_or(false)
                });
                if let Some(r) = reaction {
                    eprintln!("\n  🎯 CAPTURED A REACTION CALL at {sec}s:");
                    let url = r.get("url").and_then(|v| v.as_str()).unwrap_or("?");
                    let status = r.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
                    eprintln!("     URL: {url}");
                    eprintln!("     STATUS: {status}");
                    if let Some(body) = r.get("body").and_then(|v| v.as_str()) {
                        eprintln!("     BODY ({} chars):", body.len());
                        eprintln!("     {}", body);
                    }
                    if let Some(stack) = r.get("stack").and_then(|v| v.as_str()) {
                        eprintln!("     STACK:");
                        eprintln!("     {}", &stack[..stack.len().min(500)]);
                    }
                    if let Some(headers) = r.get("headers").and_then(|v| v.as_object()) {
                        eprintln!("     HEADERS:");
                        for (k, v) in headers {
                            eprintln!("       {k}: {}", v.as_str().unwrap_or("?"));
                        }
                    }
                    break;
                }
            }
        }
    }

    eprintln!("\n========== discovery complete ==========");
}
