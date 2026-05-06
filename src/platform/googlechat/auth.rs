//! Authentication for Google Chat via Chrome browser automation.
//!
//! Architecture:
//! 1. Launch Chrome (visible) → user logs in interactively
//! 2. Wait for chat.google.com to fully load
//! 3. Extract session data by evaluating JavaScript in the page context
//! 4. Use Chrome's `Runtime.evaluate` to make authenticated fetch() calls
//!    from within the page — Chrome handles all cookie/auth encryption
//!
//! The key insight: Google encrypts cookies at the browser level (macOS
//! Keychain binding). Cookie values from CDP are NOT usable in raw HTTP
//! clients. Instead, we proxy all API calls through Chrome's page context.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::AuthError;

const CHAT_URL: &str = "https://chat.google.com";
const LOGIN_INDICATOR_COOKIE: &str = "SID";

// ─────────────────── Token Types ───────────────────

/// Authentication state wrapping a live Chrome browser.
/// API calls are made by evaluating fetch() inside the page context.
pub struct Tokens {
    /// The Chrome browser reference — kept alive for the lifetime of tchat.
    pub browser: Option<headless_chrome::Browser>,
    /// XSRF token extracted from the page.
    pub xsrf_token: Option<String>,
    /// Cached cookie header (for session display, not for API calls).
    pub cookie_header: String,
    /// SAPISID for SAPISIDHASH (display only).
    pub sapisid: Option<String>,
    pub dynamite_token: Option<String>,
    pub dynamite_expiry: Instant,
    pub raw_cookies: String,
}

impl Tokens {
    pub fn is_expired(&self) -> bool {
        self.browser.is_none()
    }

    pub fn refresh(&mut self) -> Result<(), AuthError> {
        Err(AuthError::RefreshFailed(
            "browser session expired — restart tchat to re-authenticate".into(),
        ))
    }

    pub fn auth_header(&self) -> Option<String> {
        None // Auth is handled by Chrome internally
    }

    /// Get a fresh tab reference from the browser.
    /// The tab that has chat.google.com loaded.
    pub fn get_tab(&self) -> Result<Arc<headless_chrome::Tab>, AuthError> {
        let browser = self
            .browser
            .as_ref()
            .ok_or(AuthError::SessionFetch("browser not available".into()))?;

        let tabs = browser
            .get_tabs()
            .lock()
            .map_err(|e| AuthError::SessionFetch(format!("failed to get tabs: {e}")))?;

        // Find the tab with chat.google.com
        for tab in tabs.iter() {
            let url = tab.get_url();
            if url.contains("chat.google.com") {
                return Ok(Arc::clone(tab));
            }
        }

        // Fallback: return the first tab
        tabs.first()
            .cloned()
            .ok_or(AuthError::SessionFetch("no tabs available".into()))
    }

    /// Make an authenticated POST returning text (for debugging).
    pub fn fetch_post_text(
        &self,
        url: &str,
        body_base64: &str,
        content_type: &str,
    ) -> Result<String, AuthError> {
        let tab = self.get_tab()?;
        let xsrf = self.xsrf_token.as_deref().unwrap_or("");

        let js = format!(
            r#"(async () => {{
                try {{
                    const bodyBytes = Uint8Array.from(atob("{body_base64}"), c => c.charCodeAt(0));
                    const resp = await fetch("{url}", {{
                        method: 'POST',
                        credentials: 'include',
                        headers: {{
                            'Content-Type': '{content_type}',
                            'X-Goog-AuthUser': '0',
                            'X-Framework-Xsrf-Token': '{xsrf}'
                        }},
                        body: bodyBytes
                    }});
                    const text = await resp.text();
                    return JSON.stringify({{status: resp.status, body: text.substring(0, 2000)}});
                }} catch(e) {{
                    return JSON.stringify({{error: e.message}});
                }}
            }})()"#
        );

        let result = tab
            .evaluate(&js, true)
            .map_err(|e| AuthError::SessionFetch(e.to_string()))?;

        result
            .value
            .and_then(|v| v.as_str().map(|s| s.to_owned()))
            .ok_or(AuthError::SessionFetch("empty response".into()))
    }

    /// Make an authenticated POST with a string body (for pblite JSON requests).
    /// Unlike `fetch_post` which sends binary bytes, this sends the body as a text string.
    /// Uses base64 to safely transport the body into JavaScript without escaping issues.
    pub fn fetch_post_string_body(
        &self,
        url: &str,
        body: &str,
        content_type: &str,
    ) -> Result<Vec<u8>, AuthError> {
        let tab = self.get_tab()?;
        let xsrf = self.xsrf_token.as_deref().unwrap_or("");

        // Base64-encode the body to safely embed in JS, then atob() to recover
        use base64::Engine;
        let body_b64 = base64::engine::general_purpose::STANDARD.encode(body.as_bytes());

        let js = format!(
            r#"(async () => {{
                try {{
                    const bodyText = atob("{body_b64}");
                    const resp = await fetch("{url}", {{
                        method: 'POST',
                        credentials: 'include',
                        headers: {{
                            'Content-Type': '{content_type}',
                            'X-Goog-AuthUser': '0',
                            'X-Framework-Xsrf-Token': '{xsrf}',
                            'Accept-Language': 'en'
                        }},
                        body: bodyText
                    }});
                    const status = resp.status;
                    const contentType = resp.headers.get('content-type') || 'unknown';
                    const buf = await resp.arrayBuffer();
                    const bytes = new Uint8Array(buf);
                    if (!resp.ok) {{
                        const text = new TextDecoder().decode(bytes);
                        return JSON.stringify({{error: status, body: text.substring(0, 200), ct: contentType}});
                    }}
                    let binary = '';
                    for (let i = 0; i < bytes.length; i++) {{
                        binary += String.fromCharCode(bytes[i]);
                    }}
                    return JSON.stringify({{ok: true, status: status, ct: contentType, size: bytes.length, data: btoa(binary)}});
                }} catch(e) {{
                    return JSON.stringify({{error: e.message}});
                }}
            }})()"#
        );

        let result = tab
            .evaluate(&js, true)
            .map_err(|e| AuthError::SessionFetch(e.to_string()))?;

        let text = result
            .value
            .and_then(|v| v.as_str().map(|s| s.to_owned()))
            .ok_or(AuthError::SessionFetch("empty response".into()))?;

        let resp: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
            AuthError::SessionFetch(format!("bad response: {}", &text[..text.len().min(100)]))
        })?;

        if let Some(error) = resp.get("error") {
            let body = resp.get("body").and_then(|v| v.as_str()).unwrap_or("");
            return Err(AuthError::SessionFetch(format!(
                "HTTP {error}: {}",
                &body[..body.len().min(150)]
            )));
        }

        let data_b64 = resp
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or(AuthError::SessionFetch("no data field in response".into()))?;

        base64::engine::general_purpose::STANDARD
            .decode(data_b64)
            .map_err(|e| AuthError::SessionFetch(format!("base64 decode: {e}")))
    }

    /// Make an authenticated GET request returning binary bytes through Chrome's fetch.
    ///
    /// Uses streaming reads with a timeout so long-polling endpoints return
    /// as soon as they have data. Defaults: 15-second timeout, abort on idle.
    pub fn fetch_get_binary(&self, url: &str) -> Result<Vec<u8>, AuthError> {
        self.fetch_get_binary_timed(url, 15000)
    }

    /// GET request that reads only the `X-HTTP-Initial-Response` header,
    /// used by BrowserChannel to get the SID without reading the streaming body.
    pub fn fetch_get_header(&self, url: &str, header_name: &str) -> Result<String, AuthError> {
        let tab = self.get_tab()?;
        let js = format!(
            r#"(async () => {{
                try {{
                    const ctrl = new AbortController();
                    const timeoutId = setTimeout(() => ctrl.abort(), 10000);
                    const resp = await fetch("{url}", {{
                        credentials: 'include',
                        headers: {{ 'X-Goog-AuthUser': '0' }},
                        signal: ctrl.signal
                    }});
                    const hdr = resp.headers.get("{header_name}");
                    clearTimeout(timeoutId);
                    try {{ ctrl.abort(); }} catch(e) {{}}
                    return JSON.stringify({{status: resp.status, header: hdr}});
                }} catch(e) {{
                    return JSON.stringify({{error: e.message}});
                }}
            }})()"#
        );
        let result = tab
            .evaluate(&js, true)
            .map_err(|e| AuthError::SessionFetch(e.to_string()))?;
        let text = result
            .value
            .and_then(|v| v.as_str().map(|s| s.to_owned()))
            .ok_or(AuthError::SessionFetch("empty response".into()))?;
        let resp: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
            AuthError::SessionFetch(format!("bad response: {}", &text[..text.len().min(100)]))
        })?;
        if let Some(error) = resp.get("error") {
            return Err(AuthError::SessionFetch(format!(
                "fetch_get_header: {error}"
            )));
        }
        let header = resp
            .get("header")
            .and_then(|v| v.as_str())
            .ok_or(AuthError::SessionFetch(format!(
                "header {header_name} not present"
            )))?;
        Ok(header.to_string())
    }

    /// GET with an explicit timeout (milliseconds). Aborts after `timeout_ms`
    /// and returns whatever bytes have been received so far.
    pub fn fetch_get_binary_timed(&self, url: &str, timeout_ms: u32) -> Result<Vec<u8>, AuthError> {
        let tab = self.get_tab()?;

        let js = format!(
            r#"(async () => {{
                try {{
                    const ctrl = new AbortController();
                    const timeoutId = setTimeout(() => ctrl.abort(), {timeout_ms});
                    let bytes = new Uint8Array(0);
                    let status = 0;
                    try {{
                        const resp = await fetch("{url}", {{
                            credentials: 'include',
                            headers: {{
                                'X-Goog-AuthUser': '0'
                            }},
                            signal: ctrl.signal
                        }});
                        status = resp.status;

                        // Use reader for streaming so we can get data as it
                        // arrives and abort when idle. For bootstrap long-poll,
                        // the SID arrives in the first chunk quickly.
                        const reader = resp.body.getReader();
                        const chunks = [];
                        let total = 0;
                        let idleTimer = null;
                        const idleMs = 2000; // stop reading after 2s idle
                        const done = new Promise((resolve) => {{
                            const resetIdle = () => {{
                                if (idleTimer) clearTimeout(idleTimer);
                                idleTimer = setTimeout(() => {{
                                    try {{ reader.cancel(); }} catch(e) {{}}
                                    resolve();
                                }}, idleMs);
                            }};
                            resetIdle();
                            (async () => {{
                                try {{
                                    while (true) {{
                                        const {{done, value}} = await reader.read();
                                        if (done) break;
                                        chunks.push(value);
                                        total += value.length;
                                        resetIdle();
                                    }}
                                }} catch(e) {{}}
                                resolve();
                            }})();
                        }});
                        await done;
                        clearTimeout(timeoutId);

                        bytes = new Uint8Array(total);
                        let off = 0;
                        for (const c of chunks) {{
                            bytes.set(c, off);
                            off += c.length;
                        }}
                    }} catch(e) {{
                        clearTimeout(timeoutId);
                        if (bytes.length === 0) {{
                            return JSON.stringify({{error: e.message, aborted: true}});
                        }}
                    }}

                    let bin = '';
                    for (let i = 0; i < bytes.length; i++) {{
                        bin += String.fromCharCode(bytes[i]);
                    }}
                    return JSON.stringify({{status: status, size: bytes.length, data: btoa(bin)}});
                }} catch(e) {{
                    return JSON.stringify({{error: e.message}});
                }}
            }})()"#
        );

        let result = tab
            .evaluate(&js, true)
            .map_err(|e| AuthError::SessionFetch(e.to_string()))?;

        let text = result
            .value
            .and_then(|v| v.as_str().map(|s| s.to_owned()))
            .ok_or(AuthError::SessionFetch("empty response".into()))?;

        let resp: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
            AuthError::SessionFetch(format!("bad response: {}", &text[..text.len().min(100)]))
        })?;

        if let Some(error) = resp.get("error") {
            return Err(AuthError::SessionFetch(format!(
                "fetch_get_binary: {error}"
            )));
        }

        let status = resp.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
        if status != 200 {
            return Err(AuthError::SessionFetch(format!("HTTP {status}")));
        }

        let data_b64 = resp
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or(AuthError::SessionFetch("no data field".into()))?;

        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(data_b64)
            .map_err(|e| AuthError::SessionFetch(format!("base64 decode: {e}")))
    }

    /// Make an authenticated GET request through Chrome's page context.
    pub fn fetch_get(&self, url: &str) -> Result<String, AuthError> {
        let tab = self.get_tab()?;

        let js = format!(
            r#"(async () => {{
                try {{
                    const resp = await fetch("{url}", {{
                        credentials: 'include',
                        headers: {{
                            'X-Goog-AuthUser': '0'
                        }}
                    }});
                    if (!resp.ok) return JSON.stringify({{error: resp.status}});
                    const text = await resp.text();
                    return text;
                }} catch(e) {{
                    return JSON.stringify({{error: e.message}});
                }}
            }})()"#
        );

        let result = tab
            .evaluate(&js, true)
            .map_err(|e| AuthError::SessionFetch(e.to_string()))?;

        result
            .value
            .and_then(|v| v.as_str().map(|s| s.to_owned()))
            .ok_or(AuthError::SessionFetch("empty response from fetch".into()))
    }

    /// Make an authenticated POST request through Chrome's page context.
    pub fn fetch_post(
        &self,
        url: &str,
        body_base64: &str,
        content_type: &str,
    ) -> Result<Vec<u8>, AuthError> {
        let tab = self.get_tab()?;

        let xsrf = self.xsrf_token.as_deref().unwrap_or("");

        let js = format!(
            r#"(async () => {{
                try {{
                    const bodyBytes = Uint8Array.from(atob("{body_base64}"), c => c.charCodeAt(0));
                    const resp = await fetch("{url}", {{
                        method: 'POST',
                        credentials: 'include',
                        headers: {{
                            'Content-Type': '{content_type}',
                            'X-Goog-AuthUser': '0',
                            'X-Framework-Xsrf-Token': '{xsrf}'
                        }},
                        body: bodyBytes
                    }});
                    const status = resp.status;
                    const contentType = resp.headers.get('content-type') || 'unknown';
                    const buf = await resp.arrayBuffer();
                    const bytes = new Uint8Array(buf);
                    if (!resp.ok) {{
                        const text = new TextDecoder().decode(bytes);
                        return JSON.stringify({{error: status, body: text.substring(0, 200), ct: contentType}});
                    }}
                    let binary = '';
                    for (let i = 0; i < bytes.length; i++) {{
                        binary += String.fromCharCode(bytes[i]);
                    }}
                    return JSON.stringify({{ok: true, status: status, ct: contentType, size: bytes.length, data: btoa(binary)}});
                }} catch(e) {{
                    return JSON.stringify({{error: e.message}});
                }}
            }})()"#
        );

        let result = tab
            .evaluate(&js, true)
            .map_err(|e| AuthError::SessionFetch(e.to_string()))?;

        let text = result
            .value
            .and_then(|v| v.as_str().map(|s| s.to_owned()))
            .ok_or(AuthError::SessionFetch("empty response".into()))?;

        // Parse the JSON wrapper
        let resp: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
            AuthError::SessionFetch(format!("bad response: {}", &text[..text.len().min(100)]))
        })?;

        let status = resp.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
        let ct = resp.get("ct").and_then(|v| v.as_str()).unwrap_or("?");
        let size = resp.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
        tracing::warn!("fetch_post: status={status}, ct={ct}, size={size}");

        // Check for error — could be HTTP error or JS exception
        if let Some(error) = resp.get("error") {
            let body = resp.get("body").and_then(|v| v.as_str()).unwrap_or("");
            let err_name = resp.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let stack = resp.get("stack").and_then(|v| v.as_str()).unwrap_or("");
            // If it's a JS exception (not an HTTP error), log the details
            if !err_name.is_empty() || error.is_string() {
                eprintln!("fetch_post error detail: err={error} name={err_name} stack={stack}");
            }
            return Err(AuthError::SessionFetch(format!(
                "HTTP {error}: {}",
                &body[..body.len().min(150)]
            )));
        }

        // Extract base64-encoded data
        let data_b64 = resp
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or(AuthError::SessionFetch("no data field in response".into()))?;

        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(data_b64)
            .map_err(|e| AuthError::SessionFetch(format!("base64 decode: {e}")))
    }
}

// ─────────────────── Public API ───────────────────

/// Launch Chrome, authenticate, and return Tokens with a live browser tab.
/// MUST be called before ratatui::init() — Chrome opens a visible window.
/// Launch Chrome and authenticate. Uses a persistent profile so subsequent
/// runs auto-authenticate. Tries headless first (if profile has saved login),
/// falls back to visible window if interactive sign-in is needed.
pub fn authenticate(_account: Option<&str>) -> Result<Tokens, AuthError> {
    let chrome_path = find_chrome()?;
    let profile_dir = tchat_chrome_profile_dir();

    // Check if profile has actual Google cookies (not just directory existence).
    // The Cookies SQLite DB in the profile must have a SID cookie.
    let has_prior_session = {
        let cookies_db = profile_dir.join("Default").join("Cookies");
        if cookies_db.exists() {
            // Quick check: does the DB have a SID cookie for google?
            rusqlite::Connection::open_with_flags(
                &cookies_db,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .ok()
            .and_then(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM cookies WHERE name='SID' AND host_key LIKE '%google%'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .ok()
            })
            .map(|count| count > 0)
            .unwrap_or(false)
        } else {
            false
        }
    };

    if has_prior_session {
        eprintln!("  Using saved Chrome session (headless)...");
    } else {
        eprintln!();
        eprintln!("  \x1b[36m╭─────────────────────────────────────╮\x1b[0m");
        eprintln!("  \x1b[36m│\x1b[0m  \x1b[1mtchat\x1b[0m — Google Chat authentication  \x1b[36m│\x1b[0m");
        eprintln!("  \x1b[36m╰─────────────────────────────────────╯\x1b[0m");
        eprintln!();
        eprintln!("  A Chrome window will open for Google sign-in.");
        eprintln!("  Complete the login in the browser window.");
        eprintln!();
    }

    eprintln!("  Launching Chrome...");

    let browser = headless_chrome::Browser::new(headless_chrome::LaunchOptions {
        headless: has_prior_session, // headless if we have a saved session
        path: Some(chrome_path.clone()),
        user_data_dir: Some(profile_dir.clone()),
        // Keep Chrome alive for the entire tchat session (default 30s kills it)
        idle_browser_timeout: std::time::Duration::from_secs(86400),
        args: vec![
            std::ffi::OsStr::new("--disable-gpu"),
            std::ffi::OsStr::new("--no-first-run"),
            std::ffi::OsStr::new("--no-default-browser-check"),
            std::ffi::OsStr::new("--window-size=800,600"),
            // Reduce memory and prevent SPA background tasks from crashing
            std::ffi::OsStr::new("--disable-background-networking"),
            std::ffi::OsStr::new("--disable-background-timer-throttling"),
            std::ffi::OsStr::new("--disable-renderer-backgrounding"),
            std::ffi::OsStr::new("--disable-backgrounding-occluded-windows"),
            std::ffi::OsStr::new("--js-flags=--max-old-space-size=256"),
        ],
        ..Default::default()
    })
    .map_err(|e| AuthError::OAuthFailed(format!("failed to launch Chrome: {e}")))?;

    let tab = browser
        .new_tab()
        .map_err(|e| AuthError::OAuthFailed(format!("failed to open tab: {e}")))?;

    tab.navigate_to(CHAT_URL)
        .map_err(|e| AuthError::OAuthFailed(format!("failed to navigate: {e}")))?;

    eprintln!("  Waiting for login (complete sign-in in the Chrome window)...");

    // Wait for login to complete — poll for SID cookie AND page reaching chat.google.com.
    // With corporate SSO (SAML), the SID cookie may exist from a previous session
    // but the IdP session can expire, causing the page to stall on the IdP login.
    // We need BOTH the cookie AND the page on chat.google.com.
    let deadline = Instant::now() + Duration::from_secs(if has_prior_session { 20 } else { 300 });
    let mut page_on_chat = false;
    loop {
        if Instant::now() > deadline {
            if has_prior_session && !page_on_chat {
                // Headless mode failed — SSO session expired.
                // Drop this browser and retry with a visible window.
                eprintln!("  \x1b[33m!\x1b[0m SSO session expired, relaunching Chrome with UI...");
                drop(tab);
                drop(browser);
                return authenticate_visible(&chrome_path, &profile_dir);
            }
            return Err(AuthError::OAuthFailed("login timed out".into()));
        }
        std::thread::sleep(Duration::from_secs(2));

        // Check if page reached chat.google.com
        let url = tab.get_url();
        if url.contains("chat.google.com") {
            page_on_chat = true;
        }

        let cookies = match get_cookies_raw(&tab) {
            Ok(c) => c,
            Err(_) => continue,
        };

        if page_on_chat && cookies.iter().any(|c| c.name == LOGIN_INDICATOR_COOKIE) {
            break;
        }
    }

    // Wait for SPA to fully initialize
    std::thread::sleep(Duration::from_secs(3));

    // Extract XSRF token from the loaded page
    let xsrf = tab.evaluate(
        "(() => { try { return (window.WIZ_global_data && window.WIZ_global_data.SMqcke) || ''; } catch(e) { return ''; } })()",
        false,
    ).ok().and_then(|v| {
        let s = v.value?.as_str()?.to_owned();
        if s.is_empty() { None } else { Some(s) }
    });

    if xsrf.is_none() {
        eprintln!("  \x1b[33m!\x1b[0m XSRF token not found, page may not be fully loaded");
    }

    Ok(Tokens {
        browser: Some(browser),
        xsrf_token: xsrf,
        cookie_header: String::new(),
        sapisid: None,
        dynamite_token: None,
        dynamite_expiry: Instant::now() + Duration::from_secs(86400),
        raw_cookies: String::new(),
    })
}

/// Re-authenticate with a visible Chrome window (non-headless).
/// Called when headless mode fails due to expired SSO session.
fn authenticate_visible(chrome_path: &Path, profile_dir: &Path) -> Result<Tokens, AuthError> {
    eprintln!("  Launching Chrome with visible window...");

    let browser = headless_chrome::Browser::new(headless_chrome::LaunchOptions {
        headless: false,
        path: Some(chrome_path.to_path_buf()),
        user_data_dir: Some(profile_dir.to_path_buf()),
        idle_browser_timeout: Duration::from_secs(86400),
        args: vec![
            std::ffi::OsStr::new("--disable-gpu"),
            std::ffi::OsStr::new("--no-first-run"),
            std::ffi::OsStr::new("--no-default-browser-check"),
            std::ffi::OsStr::new("--window-size=800,600"),
            std::ffi::OsStr::new("--disable-background-networking"),
            std::ffi::OsStr::new("--disable-background-timer-throttling"),
            std::ffi::OsStr::new("--disable-renderer-backgrounding"),
            std::ffi::OsStr::new("--disable-backgrounding-occluded-windows"),
            std::ffi::OsStr::new("--js-flags=--max-old-space-size=256"),
        ],
        ..Default::default()
    })
    .map_err(|e| AuthError::OAuthFailed(format!("failed to launch Chrome: {e}")))?;

    let tab = browser
        .new_tab()
        .map_err(|e| AuthError::OAuthFailed(format!("failed to open tab: {e}")))?;

    tab.navigate_to(CHAT_URL)
        .map_err(|e| AuthError::OAuthFailed(format!("failed to navigate: {e}")))?;

    eprintln!("  Complete sign-in in the Chrome window...");

    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        if Instant::now() > deadline {
            return Err(AuthError::OAuthFailed("login timed out".into()));
        }
        std::thread::sleep(Duration::from_secs(2));

        let url = tab.get_url();
        if !url.contains("chat.google.com") {
            continue; // Still on SSO/login page
        }

        let cookies = match get_cookies_raw(&tab) {
            Ok(c) => c,
            Err(_) => continue,
        };

        if cookies.iter().any(|c| c.name == LOGIN_INDICATOR_COOKIE) {
            break;
        }
    }

    std::thread::sleep(Duration::from_secs(3));

    let xsrf = tab.evaluate(
        "(() => { try { return (window.WIZ_global_data && window.WIZ_global_data.SMqcke) || ''; } catch(e) { return ''; } })()",
        false,
    ).ok().and_then(|v| {
        let s = v.value?.as_str()?.to_owned();
        if s.is_empty() { None } else { Some(s) }
    });

    Ok(Tokens {
        browser: Some(browser),
        xsrf_token: xsrf,
        cookie_header: String::new(),
        sapisid: None,
        dynamite_token: None,
        dynamite_expiry: Instant::now() + Duration::from_secs(86400),
        raw_cookies: String::new(),
    })
}

// ─────────────────── Chrome Helpers ───────────────────

fn find_chrome() -> Result<PathBuf, AuthError> {
    let candidates = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
    ];
    for path in &candidates {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
    }
    Err(AuthError::OAuthFailed("Chrome not found".into()))
}

fn tchat_chrome_profile_dir() -> PathBuf {
    // Use a persistent directory so the Google login session survives across
    // tchat restarts. After the first interactive sign-in, subsequent launches
    // auto-authenticate without user interaction.
    if let Some(dirs) = directories::ProjectDirs::from("com", "tchat", "tchat") {
        let dir = dirs.data_dir().join("chrome-profile");
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    // Fallback to temp if no data dir
    let dir = std::env::temp_dir().join("tchat-chrome-profile");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

// ─────────────────── CDP Cookie Reading ───────────────────

#[derive(serde::Deserialize, Debug)]
struct CdpCookie {
    name: String,
    #[allow(dead_code)]
    value: String,
    #[allow(dead_code)]
    domain: String,
    #[serde(default)]
    #[allow(dead_code)]
    expires: f64,
}

#[derive(serde::Serialize, Debug)]
struct GetAllCookies;

#[derive(serde::Deserialize, Debug)]
struct GetAllCookiesResponse {
    cookies: Vec<CdpCookie>,
}

impl headless_chrome::protocol::cdp::types::Method for GetAllCookies {
    const NAME: &'static str = "Network.getAllCookies";
    type ReturnObject = GetAllCookiesResponse;
}

fn get_cookies_raw(tab: &headless_chrome::Tab) -> Result<Vec<CdpCookie>, String> {
    let resp = tab.call_method(GetAllCookies).map_err(|e| e.to_string())?;
    Ok(resp
        .cookies
        .into_iter()
        .filter(|c| c.domain.contains("google.com"))
        .collect())
}

// ─────────────────── Utility functions used by other modules ───────────────────

pub fn parse_access_token(body: &serde_json::Value) -> Result<String, AuthError> {
    body["access_token"]
        .as_str()
        .map(|s| s.to_owned())
        .ok_or(AuthError::MissingField("access_token"))
}

pub fn parse_dynamite_response(body: &serde_json::Value) -> Result<(String, u64), AuthError> {
    let token = body["token"]
        .as_str()
        .ok_or(AuthError::MissingField("token"))?
        .to_owned();
    let expires_in = body["expiresIn"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| body["expiresIn"].as_u64())
        .unwrap_or(3600);
    Ok((token, expires_in))
}

pub fn exchange_dynamite_token(_access_token: &str) -> Result<(String, u64), AuthError> {
    Err(AuthError::DynamiteExchange(
        "not used in browser mode".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_dynamite_response_extracts() {
        let body = json!({ "token": "t", "expiresIn": "3600" });
        let (tok, exp) = parse_dynamite_response(&body).unwrap();
        assert_eq!(tok, "t");
        assert_eq!(exp, 3600);
    }

    #[test]
    fn parse_dynamite_response_missing_token() {
        assert!(parse_dynamite_response(&json!({})).is_err());
    }

    #[test]
    fn find_chrome_runs() {
        let _ = find_chrome(); // Don't assert — CI may not have Chrome
    }
}
