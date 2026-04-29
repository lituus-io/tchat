//! Google Chat session state.
//!
//! All API calls are proxied through Chrome's page context via
//! `Tokens.fetch_post()` / `Tokens.fetch_get()`. This is necessary
//! because Google encrypts cookies at the browser level — raw cookie
//! values from CDP are not usable in external HTTP clients.

use crate::error::AuthError;
use crate::types::IdInterner;

use super::auth::Tokens;

const CHAT_BASE: &str = "https://chat.google.com/u/0";

/// Complete session state for a Google Chat connection.
pub struct Session {
    pub tokens: Tokens,
    pub sid: Option<String>,
    pub aid: u64,
    rid_counter: u32,
    api_counter: u32,
    pub xsrf_token: Option<String>,
    pub interner: IdInterner,
    /// Dedicated tab for API calls — separate from the SPA tab to avoid
    /// the SPA's service worker/JS interfering with our fetch() calls.
    api_tab: Option<std::sync::Arc<headless_chrome::Tab>>,
}

impl Session {
    pub fn new(tokens: Tokens) -> Self {
        let xsrf = tokens.xsrf_token.clone();
        Self {
            tokens,
            sid: None,
            aid: 0,
            rid_counter: 0,
            api_counter: 0,
            xsrf_token: xsrf,
            interner: IdInterner::new(),
            api_tab: None,
        }
    }

    pub fn next_rid(&mut self) -> u32 {
        self.rid_counter += 1;
        self.rid_counter
    }

    pub fn next_api_counter(&mut self) -> u32 {
        self.api_counter += 1;
        self.api_counter
    }

    pub fn random_zx() -> String {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        use std::time::{SystemTime, UNIX_EPOCH};
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
        );
        format!("{:012x}", hasher.finish() & 0xFFFFFFFFFFFF)
    }

    /// Register a BrowserChannel session.
    ///
    /// The register endpoint just sets session cookies; it does NOT return
    /// the SID. The SID is obtained later from the first long-poll response
    /// (see `acquire_sid`).
    pub fn register(&mut self) -> Result<(), AuthError> {
        let url = format!("{CHAT_BASE}/webchannel/register?ignore_compass_cookie=1");

        let body = self.tokens.fetch_get(&url)?;
        tracing::warn!("register response ({} bytes)", body.len());
        Ok(())
    }

    /// Acquire a SID by making the first long-poll request with `SID=null`.
    ///
    /// The response body format is `[[0,["c","SID_HERE","",8,12,0,...]]]`
    /// (a JSON array). The SID is at path `[0][1][1]`.
    pub fn acquire_sid(&mut self) -> Result<(), AuthError> {
        let zx = Session::random_zx();
        let url = format!(
            "{CHAT_BASE}/webchannel/events?\
             VER=8&RID=1&CVER=22&zx={zx}&t=1&SID=null&\
             %24req=count%3D1%26ofs%3D0%26req0_data%3D%255B%255D"
        );

        // Strategy: try to read X-HTTP-Initial-Response header (matches mautrix).
        // If that isn't exposed by CORS, fall back to reading the body.
        if let Ok(header) = self
            .tokens
            .fetch_get_header(&url, "X-HTTP-Initial-Response")
        {
            tracing::warn!("X-HTTP-Initial-Response: {header}");
            self.sid = Some(parse_sid_from_register(&header)?);
            return Ok(());
        }

        // Fallback: read the body and search for "c","SID" pattern
        let body_bytes = self.tokens.fetch_get_binary(&url)?;
        let body = std::str::from_utf8(&body_bytes)
            .map_err(|e| AuthError::SessionFetch(format!("non-UTF8: {e}")))?;
        tracing::warn!("SID body fallback ({} bytes)", body.len());

        // Search for the first ["c","SID"...] pattern anywhere in the body
        if let Some(sid) = extract_sid_from_stream(body) {
            self.sid = Some(sid);
            return Ok(());
        }

        Err(AuthError::MissingField("SID"))
    }

    /// Fetch the XSRF token by evaluating JS in the chat page.
    pub fn fetch_session_tokens(&mut self) -> Result<(), AuthError> {
        // The XSRF token was already extracted during auth
        if self.xsrf_token.is_some() {
            return Ok(());
        }

        // Try to extract from the page
        if let Ok(tab) = self.tokens.get_tab() {
            let result = tab.evaluate(
                "(() => { try { return (window.WIZ_global_data && window.WIZ_global_data.SMqcke) || ''; } catch(e) { return ''; } })()",
                false,
            ).ok().and_then(|v| {
                let s = v.value?.as_str()?.to_owned();
                if s.is_empty() { None } else { Some(s) }
            });
            if let Some(xsrf) = result {
                tracing::warn!("XSRF token obtained ({} chars)", xsrf.len());
                self.xsrf_token = Some(xsrf);
                self.tokens.xsrf_token = self.xsrf_token.clone();
            }
        }
        Ok(())
    }

    pub fn ensure_fresh(&mut self) -> Result<(), AuthError> {
        Ok(()) // Browser session manages its own auth
    }

    /// Open a clean API tab — same origin as chat.google.com but
    /// without SPA JavaScript.
    pub fn ensure_clean_api_tab(&mut self) -> Result<(), AuthError> {
        if self.api_tab.is_some() {
            return Ok(());
        }
        let browser = self
            .tokens
            .browser
            .as_ref()
            .ok_or(AuthError::SessionFetch("no browser".into()))?;
        let tab = browser
            .new_tab()
            .map_err(|e| AuthError::SessionFetch(format!("new tab: {e}")))?;
        // Navigate to chat.google.com/u/0/ so cookies scoped to /u/0/ are sent.
        // Immediately stop the page to prevent SPA JS from loading.
        tab.navigate_to("https://chat.google.com/u/0/")
            .map_err(|e| AuthError::SessionFetch(format!("navigate: {e}")))?;
        // Stop quickly — before SPA framework can initialize.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let _ = tab.evaluate("window.stop()", false);
        std::thread::sleep(std::time::Duration::from_millis(200));
        eprintln!("  Clean API tab opened");
        self.api_tab = Some(tab);
        Ok(())
    }

    /// Make an API call via XHR on a given Chrome tab.
    ///
    /// Uses XMLHttpRequest instead of fetch — the SPA's service worker
    /// intercepts fetch() and returns "Failed to fetch" (status 0).
    /// XHR on the SPA tab works because Chrome's service worker fetch
    /// event handler typically only processes navigation/fetch requests.
    fn xhr_api_call(
        tab: &headless_chrome::Tab,
        url: &str,
        body: &[u8],
        xsrf: &str,
    ) -> Result<Vec<u8>, AuthError> {
        use base64::Engine;
        let body_b64 = base64::engine::general_purpose::STANDARD.encode(body);

        let js = format!(
            r#"(new Promise((resolve) => {{
                try {{
                    const bodyBytes = Uint8Array.from(atob("{body_b64}"), c => c.charCodeAt(0));
                    const xhr = new XMLHttpRequest();
                    xhr.open('POST', "{url}", true);
                    xhr.responseType = 'arraybuffer';
                    xhr.setRequestHeader('Content-Type', 'application/x-protobuf');
                    xhr.setRequestHeader('X-Goog-AuthUser', '0');
                    xhr.setRequestHeader('X-Framework-Xsrf-Token', '{xsrf}');
                    xhr.onload = function() {{
                        const status = xhr.status;
                        const ct = xhr.getResponseHeader('content-type') || '?';
                        const bytes = new Uint8Array(xhr.response || new ArrayBuffer(0));
                        if (status < 200 || status >= 300) {{
                            const text = new TextDecoder().decode(bytes);
                            resolve(JSON.stringify({{error: status, body: text.substring(0, 200), ct: ct}}));
                        }} else {{
                            let bin = '';
                            for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
                            resolve(JSON.stringify({{ok: true, status: status, ct: ct, size: bytes.length, data: btoa(bin)}}));
                        }}
                    }};
                    xhr.onerror = function() {{
                        resolve(JSON.stringify({{error: 'XHR network error', name: 'NetworkError'}}));
                    }};
                    xhr.send(bodyBytes);
                }} catch(e) {{
                    resolve(JSON.stringify({{error: e.message, name: e.name}}));
                }}
            }}))
            "#
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

        if let Some(err) = resp.get("error") {
            let body = resp.get("body").and_then(|v| v.as_str()).unwrap_or("");
            return Err(AuthError::SessionFetch(format!(
                "HTTP {err}: {}",
                &body[..body.len().min(150)]
            )));
        }

        let data_b64 = resp
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or(AuthError::SessionFetch("no data".into()))?;

        base64::engine::general_purpose::STANDARD
            .decode(data_b64)
            .map_err(|e| AuthError::SessionFetch(format!("base64: {e}")))
    }

    /// Make an API call via XHR on the dedicated API tab.
    pub fn call_api_binary(&mut self, endpoint: &str, body: &[u8]) -> Result<Vec<u8>, AuthError> {
        self.ensure_clean_api_tab()?;
        let counter = self.next_api_counter();
        let url = format!("{CHAT_BASE}/api/{endpoint}?c={counter}&rt=b");
        let xsrf = self.xsrf_token.clone().unwrap_or_default();
        let tab = self
            .api_tab
            .as_ref()
            .ok_or(AuthError::SessionFetch("no API tab".into()))?;
        Self::xhr_api_call(tab, &url, body, &xsrf)
    }

    /// Make an API call through Chrome.
    ///
    /// Uses XHR on the dedicated API tab if available (for write commands
    /// where SPA interference is a problem). Falls back to fetch() on the
    /// SPA tab for startup reads where the page is freshly loaded.
    pub fn call_api(&mut self, endpoint: &str, body: &[u8]) -> Result<Vec<u8>, AuthError> {
        if self.api_tab.is_some() {
            self.call_api_binary(endpoint, body)
        } else {
            let counter = self.next_api_counter();
            let url = format!("{CHAT_BASE}/api/{endpoint}?c={counter}&rt=b");

            use base64::Engine;
            let body_b64 = base64::engine::general_purpose::STANDARD.encode(body);

            let resp_bytes = self
                .tokens
                .fetch_post(&url, &body_b64, "application/x-protobuf")?;
            tracing::warn!("{endpoint}: got {} response bytes", resp_bytes.len());
            Ok(resp_bytes)
        }
    }

    /// Legacy: send via pblite JSON encoding. Kept as a fallback.
    #[allow(dead_code)]
    pub fn call_api_pblite(&mut self, endpoint: &str, body: &[u8]) -> Result<Vec<u8>, AuthError> {
        let counter = self.next_api_counter();
        let url = format!("{CHAT_BASE}/api/{endpoint}?c={counter}");

        // Convert binary protobuf request → pblite JSON string
        let pblite_req = super::pblite::wire_to_pblite(body)
            .map_err(|e| AuthError::SessionFetch(format!("wire_to_pblite: {e}")))?;
        let pblite_json = serde_json::to_string(&pblite_req)
            .map_err(|e| AuthError::SessionFetch(format!("json serialize: {e}")))?;

        tracing::warn!("{endpoint}: sending pblite ({} chars)", pblite_json.len());

        // Send as application/json (the key insight from wire-format observation)
        let resp_bytes =
            self.tokens
                .fetch_post_string_body(&url, &pblite_json, "application/json")?;

        // The response is pblite JSON text
        let resp_text = std::str::from_utf8(&resp_bytes)
            .map_err(|e| AuthError::SessionFetch(format!("response not UTF-8: {e}")))?;

        tracing::warn!("{endpoint}: got {} byte response", resp_text.len());

        if resp_text.is_empty() {
            return Ok(Vec::new());
        }

        // Strip XSSI prefix: )]}'\n
        let json_str = if resp_text.starts_with(")]}'") {
            resp_text
                .find('\n')
                .map(|i| &resp_text[i + 1..])
                .unwrap_or(resp_text)
                .trim()
        } else {
            resp_text.trim()
        };

        if json_str.is_empty() {
            return Ok(Vec::new());
        }

        // Parse the pblite JSON response
        let pblite_value: serde_json::Value = serde_json::from_str(json_str)
            .map_err(|e| AuthError::SessionFetch(format!("pblite parse: {e}")))?;

        // Response is wrapped: [["method.name", field1, field2, ...]]
        // Skip the method name at index 0
        let inner = pblite_value
            .as_array()
            .and_then(|outer| outer.first())
            .and_then(|v| v.as_array())
            .ok_or(AuthError::SessionFetch("empty pblite response".into()))?;

        let proto_fields: Vec<serde_json::Value> =
            if inner.first().map(|v| v.is_string()).unwrap_or(false) {
                inner[1..].to_vec()
            } else {
                inner.clone()
            };

        tracing::warn!("{endpoint}: pblite has {} fields", proto_fields.len());

        let pblite_array = serde_json::Value::Array(proto_fields);

        // Convert pblite JSON to protobuf wire bytes.
        // Use schema-aware encoder for known response types to correctly
        // distinguish digit-only string fields (e.g., user IDs) from
        // integer fields encoded as JSON strings.
        let schema = super::pblite::load_schema();
        let response_msg = match endpoint {
            "paginated_world" => "PaginatedWorldResponse",
            "catch_up_group" => "CatchUpResponse",
            "get_group" => "GetGroupResponse",
            "list_topics" => "ListTopicsResponse",
            "list_messages" => "ListMessagesResponse",
            "create_message" => "CreateMessageResponse",
            "edit_message" => "EditMessageResponse",
            "delete_message" => "DeleteMessageResponse",
            "set_typing_state" => "SetTypingStateResponse",
            "mark_group_readstate" => "MarkGroupReadstateResponse",
            "update_reaction" => "UpdateReactionResponse",
            _ => "", // fallback to heuristic
        };

        let wire = if !response_msg.is_empty() {
            super::pblite::pblite_to_wire_typed(&pblite_array, &schema, response_msg)
                .map_err(|e| AuthError::SessionFetch(format!("pblite_to_wire_typed: {e}")))?
        } else {
            super::pblite::pblite_to_wire(&pblite_array)
                .map_err(|e| AuthError::SessionFetch(format!("pblite_to_wire: {e}")))?
        };

        tracing::warn!("{endpoint}: converted to {} wire bytes", wire.len());

        Ok(wire.to_vec())
    }

    /// Make a pblite API call returning the raw pblite JSON response text (for debugging).
    pub fn call_api_pblite_raw(
        &mut self,
        endpoint: &str,
        body: &[u8],
    ) -> Result<String, AuthError> {
        let counter = self.next_api_counter();
        let url = format!("{CHAT_BASE}/api/{endpoint}?c={counter}");

        let pblite_req = super::pblite::wire_to_pblite(body)
            .map_err(|e| AuthError::SessionFetch(format!("wire_to_pblite: {e}")))?;
        let pblite_json = serde_json::to_string(&pblite_req)
            .map_err(|e| AuthError::SessionFetch(format!("json serialize: {e}")))?;

        let resp_bytes =
            self.tokens
                .fetch_post_string_body(&url, &pblite_json, "application/json")?;

        let resp_text = std::str::from_utf8(&resp_bytes)
            .map_err(|e| AuthError::SessionFetch(format!("response not UTF-8: {e}")))?;

        // Strip XSSI prefix and method wrapper
        let json_str = if resp_text.starts_with(")]}'") {
            resp_text
                .find('\n')
                .map(|i| &resp_text[i + 1..])
                .unwrap_or(resp_text)
                .trim()
        } else {
            resp_text.trim()
        };

        // Unwrap [["method", ...fields...]] → [...fields...]
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
            if let Some(inner) = parsed
                .as_array()
                .and_then(|outer| outer.first())
                .and_then(|v| v.as_array())
            {
                let fields: Vec<serde_json::Value> =
                    if inner.first().map(|v| v.is_string()).unwrap_or(false) {
                        inner[1..].to_vec()
                    } else {
                        inner.clone()
                    };
                return Ok(
                    serde_json::to_string(&serde_json::Value::Array(fields)).unwrap_or_default()
                );
            }
        }

        Ok(json_str.to_string())
    }

    /// Make a raw text API call through Chrome's fetch (for debugging).
    pub fn call_api_text(&mut self, endpoint: &str, body: &[u8]) -> Result<String, AuthError> {
        let counter = self.next_api_counter();
        // No rt=b — get pblite/JSON response for debugging
        let url = format!("{CHAT_BASE}/api/{endpoint}?c={counter}");

        use base64::Engine;
        let body_b64 = base64::engine::general_purpose::STANDARD.encode(body);

        self.tokens
            .fetch_post_text(&url, &body_b64, "application/x-protobuf")
    }

    /// Configuration for the long-poll reader thread.
    /// Contains the Chrome tab reference for making fetch() calls.
    pub fn read_config(&self) -> ReadConfig {
        ReadConfig {
            sid: self.sid.clone().unwrap_or_default(),
            tab: None, // Long-poll not yet implemented via Chrome
            xsrf_token: self.xsrf_token.clone().unwrap_or_default(),
        }
    }
}

/// Immutable config for the long-poll reader thread.
pub struct ReadConfig {
    pub sid: String,
    pub tab: Option<std::sync::Arc<headless_chrome::Tab>>,
    pub xsrf_token: String,
}

/// Parse the SID from a BrowserChannel register response.
/// Extract a SID from a BrowserChannel streaming body by searching for the
/// literal pattern `["c","SID",` which appears when the server sends the
/// session registration message.
pub fn extract_sid_from_stream(body: &str) -> Option<String> {
    let marker = "[\"c\",\"";
    let start = body.find(marker)?;
    let after = &body[start + marker.len()..];
    let end = after.find('"')?;
    let sid = &after[..end];
    if sid.is_empty() {
        None
    } else {
        Some(sid.to_string())
    }
}

/// Extract the first framed chunk from a BrowserChannel response.
///
/// Format: `<utf16_char_count>\n<json_content>...<utf16_char_count>\n<next>...`
/// We return just the first `<json_content>` section.
#[allow(dead_code)]
fn extract_first_frame(body: &str) -> Option<&str> {
    let newline = body.find('\n')?;
    let len_str = body[..newline].trim();
    let utf16_count: usize = len_str.parse().ok()?;
    let after_newline = &body[newline + 1..];

    // Count UTF-16 units to find the end of the first frame.
    let mut units = 0usize;
    let mut byte_idx = 0usize;
    for ch in after_newline.chars() {
        if units >= utf16_count {
            break;
        }
        units += ch.len_utf16();
        byte_idx += ch.len_utf8();
    }
    Some(&after_newline[..byte_idx])
}

pub fn parse_sid_from_register(body: &str) -> Result<String, AuthError> {
    let json_start = body.find('[').ok_or(AuthError::MissingField("SID array"))?;
    let json_str = &body[json_start..];

    let parsed: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| AuthError::SessionFetch(e.to_string()))?;

    let sid = parsed
        .get(0)
        .and_then(|v| v.get(1))
        .and_then(|v| v.get(1))
        .and_then(|v| v.as_str())
        .ok_or(AuthError::MissingField("SID"))?;

    Ok(sid.to_owned())
}

pub fn parse_xsrf_token(body: &str) -> Result<String, AuthError> {
    let marker = "\"SMqcke\":\"";
    let start = body.find(marker).ok_or(AuthError::MissingField("SMqcke"))?;
    let after = &body[start + marker.len()..];
    let end = after.find('"').ok_or(AuthError::MissingField("XSRF end"))?;
    Ok(after[..end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sid_from_register_valid() {
        let body = r#"57
[[0,["c","SESSION_ID_ABC","",8,12,0,null,null,null,null,null,2]]]"#;
        let sid = parse_sid_from_register(body).unwrap();
        assert_eq!(sid, "SESSION_ID_ABC");
    }

    #[test]
    fn parse_sid_from_register_missing() {
        assert!(parse_sid_from_register("invalid").is_err());
    }

    #[test]
    fn parse_xsrf_token_from_html() {
        let body = r#"{"SMqcke":"xsrf_12345","other":"x"}"#;
        assert_eq!(parse_xsrf_token(body).unwrap(), "xsrf_12345");
    }

    #[test]
    fn parse_xsrf_token_missing() {
        assert!(parse_xsrf_token("no token").is_err());
    }

    #[test]
    fn random_zx_is_12_hex() {
        let zx = Session::random_zx();
        assert_eq!(zx.len(), 12);
        assert!(zx.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn random_zx_is_unique() {
        let a = Session::random_zx();
        let b = Session::random_zx();
        assert_ne!(a, b);
    }

    #[test]
    fn parse_sid_from_register_with_prefix_bytes() {
        // Real responses often have a byte count prefix before the JSON
        let body = "123\n[[0,[\"c\",\"REAL_SID_123\",\"\",8]]]";
        let sid = parse_sid_from_register(body).unwrap();
        assert_eq!(sid, "REAL_SID_123");
    }

    #[test]
    fn parse_sid_from_register_empty_json_fails() {
        assert!(parse_sid_from_register("[]").is_err());
    }

    #[test]
    fn parse_sid_from_register_null_sid_fails() {
        let body = "[[0,[\"c\",null]]]";
        assert!(parse_sid_from_register(body).is_err());
    }

    #[test]
    fn parse_xsrf_token_extracts_from_middle() {
        let body = r#"some stuff before "SMqcke":"LONG_TOKEN_VALUE_HERE" and after"#;
        assert_eq!(parse_xsrf_token(body).unwrap(), "LONG_TOKEN_VALUE_HERE");
    }

    #[test]
    fn session_new_initializes_counters() {
        let tokens = crate::platform::googlechat::auth::Tokens {
            browser: None,
            xsrf_token: Some("test_xsrf".into()),
            cookie_header: String::new(),
            sapisid: None,
            dynamite_token: None,
            dynamite_expiry: std::time::Instant::now(),
            raw_cookies: String::new(),
        };
        let mut session = Session::new(tokens);
        assert!(session.sid.is_none());
        assert_eq!(session.aid, 0);
        assert_eq!(session.xsrf_token, Some("test_xsrf".into()));
        assert_eq!(session.next_rid(), 1);
        assert_eq!(session.next_rid(), 2);
        assert_eq!(session.next_api_counter(), 1);
        assert_eq!(session.next_api_counter(), 2);
    }

    #[test]
    fn session_ensure_fresh_is_ok() {
        let tokens = crate::platform::googlechat::auth::Tokens {
            browser: None,
            xsrf_token: None,
            cookie_header: String::new(),
            sapisid: None,
            dynamite_token: None,
            dynamite_expiry: std::time::Instant::now(),
            raw_cookies: String::new(),
        };
        let mut session = Session::new(tokens);
        assert!(session.ensure_fresh().is_ok());
    }

    #[test]
    fn session_read_config_without_sid() {
        let tokens = crate::platform::googlechat::auth::Tokens {
            browser: None,
            xsrf_token: Some("xsrf_abc".into()),
            cookie_header: String::new(),
            sapisid: None,
            dynamite_token: None,
            dynamite_expiry: std::time::Instant::now(),
            raw_cookies: String::new(),
        };
        let session = Session::new(tokens);
        let config = session.read_config();
        assert!(config.sid.is_empty());
        assert_eq!(config.xsrf_token, "xsrf_abc");
        assert!(config.tab.is_none());
    }
}
