//! Direct HTTP session — makes API calls via `ureq` with extracted cookies.
//!
//! This is the Chrome-free alternative to `session.rs`. After cookies are
//! extracted from Chrome's database, all API calls go through standard HTTP.
//!
//! Benefits:
//! - No Chrome process needed (saves ~500MB RAM)
//! - No tab contention / CDP timeouts
//! - Much faster API calls (~50ms vs ~500ms through Chrome)
//!
//! The trade-off: cookies expire (~2 weeks) and require re-extraction.

use std::io::Read;

use crate::error::AuthError;
use crate::types::IdInterner;

use super::cookies::{compute_sapisidhash, ChatCookies};

const CHAT_BASE: &str = "https://chat.google.com/u/0";
const ORIGIN: &str = "https://chat.google.com";

/// Direct HTTP session using extracted cookies.
pub struct DirectSession {
    pub cookies: ChatCookies,
    pub xsrf_token: Option<String>,
    /// `at` token used by the batchexecute RPC framework (search etc).
    /// Extracted from `WIZ_global_data["SNlM0e"]` on the chat page.
    pub at_token: Option<String>,
    pub sid: Option<String>,
    pub interner: IdInterner,
    api_counter: u32,
}

impl DirectSession {
    pub fn new(cookies: ChatCookies) -> Self {
        Self {
            cookies,
            xsrf_token: None,
            at_token: None,
            sid: None,
            interner: IdInterner::new(),
            api_counter: 0,
        }
    }

    pub fn next_api_counter(&mut self) -> u32 {
        self.api_counter += 1;
        self.api_counter
    }

    /// Build common headers for requests.
    fn headers(&self) -> Vec<(String, String)> {
        let mut h = vec![
            ("Cookie".into(), self.cookies.cookie_header.clone()),
            ("X-Goog-AuthUser".into(), "0".into()),
            (
                "User-Agent".into(),
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36".into(),
            ),
        ];
        if let Some(ref xsrf) = self.xsrf_token {
            h.push(("X-Framework-Xsrf-Token".into(), xsrf.clone()));
        }
        if let Some(ref sapisid) = self.cookies.sapisid {
            h.push(("Authorization".into(), compute_sapisidhash(sapisid, ORIGIN)));
        }
        h
    }

    /// Fetch the XSRF token AND the batchexecute `at` token by loading
    /// the Chat page once. Both come from the same inline WIZ_global_data
    /// blob so we extract them together.
    pub fn fetch_xsrf_token(&mut self) -> Result<(), AuthError> {
        let url = format!("{CHAT_BASE}/");
        let body = self.fetch_get(&url)?;

        let xsrf = extract_xsrf_from_html(&body);
        let at = extract_at_token_from_html(&body);

        if let Some(token) = xsrf {
            self.xsrf_token = Some(token);
            self.at_token = at;
            Ok(())
        } else {
            Err(AuthError::SessionFetch(
                "Could not find XSRF token in page. Cookies may be expired.".into(),
            ))
        }
    }

    /// POST a batchexecute RPC. The Chat web client uses this for search
    /// (RPC `SBNmJb` for full search, `wxhTDd` for autocomplete) and a
    /// handful of other non-proto endpoints. `payload_json` is the inner
    /// JSON-as-string the RPC expects; this method handles wrapping it in
    /// the `f.req=[[[ID,payload,null,"generic"]]]` envelope and stripping
    /// the `)]}'` XSSI prefix from the response.
    pub fn batchexecute(&self, rpc_id: &str, payload_json: &str) -> Result<String, AuthError> {
        let at = self.at_token.as_deref().ok_or_else(|| {
            AuthError::SessionFetch("no at token; call fetch_xsrf_token first".into())
        })?;

        let f_req = serde_json::to_string(&serde_json::json!([[[
            rpc_id,
            payload_json,
            null,
            "generic"
        ]]]))
        .map_err(|e| AuthError::SessionFetch(format!("encode f.req: {e}")))?;

        let body = format!("f.req={}&at={}", urlencode(&f_req), urlencode(at),);

        let reqid = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let url = format!(
            "{CHAT_BASE}/_/DynamiteWebUi/data/batchexecute\
             ?rpcids={rpc_id}&source-path=%2F&f.sid=-1\
             &bl=boq_dynamite-frontend&hl=en&_reqid={reqid}"
        );

        // batchexecute auth differs from /api/*:
        //   - the at-token is in the BODY (not a header)
        //   - Origin and Referer are required
        //   - the X-Framework-Xsrf-Token header isn't expected here
        let mut req = ureq::post(&url)
            .header(
                "Content-Type",
                "application/x-www-form-urlencoded;charset=UTF-8",
            )
            .header("Origin", ORIGIN)
            .header("Referer", &format!("{ORIGIN}/"))
            .header("Cookie", self.cookies.cookie_header.as_str())
            .header("X-Goog-AuthUser", "0")
            .header(
                "User-Agent",
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
            );
        if let Some(ref sapisid) = self.cookies.sapisid {
            let auth = compute_sapisidhash(sapisid, ORIGIN);
            req = req.header("Authorization", auth.as_str());
        }

        let resp = req
            .send(body.as_bytes())
            .map_err(|e| AuthError::SessionFetch(format!("batchexecute {rpc_id}: {e}")))?;

        let status = resp.status();
        let mut text = String::new();
        resp.into_body()
            .into_reader()
            .read_to_string(&mut text)
            .map_err(|e| AuthError::SessionFetch(format!("read: {e}")))?;
        if status != 200 {
            return Err(AuthError::SessionFetch(format!(
                "batchexecute {rpc_id} HTTP {status}: {}",
                &text[..text.len().min(200)]
            )));
        }

        // Strip the )]}' XSSI prefix and find the wrb.fr frame for our RPC.
        let json_part = text.trim_start_matches(")]}'").trim();
        let outer: serde_json::Value = serde_json::from_str(json_part)
            .map_err(|e| AuthError::SessionFetch(format!("parse outer: {e}")))?;
        let payload = outer
            .as_array()
            .and_then(|frames| {
                frames.iter().find(|f| {
                    f.get(0).and_then(|v| v.as_str()) == Some("wrb.fr")
                        && f.get(1).and_then(|v| v.as_str()) == Some(rpc_id)
                })
            })
            .and_then(|f| f.get(2).and_then(|v| v.as_str()))
            .ok_or_else(|| {
                AuthError::SessionFetch(format!(
                    "batchexecute {rpc_id}: no wrb.fr frame in response"
                ))
            })?;
        Ok(payload.to_owned())
    }
}

/// Minimal URL-encoder for batchexecute form bodies. Encodes everything
/// except [A-Za-z0-9-_.~] per RFC 3986 unreserved set.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            other => {
                out.push('%');
                out.push_str(&format!("{:02X}", other));
            }
        }
    }
    out
}

impl DirectSession {
    /// Make a binary protobuf API call via direct HTTP.
    pub fn call_api(&mut self, endpoint: &str, body: &[u8]) -> Result<Vec<u8>, AuthError> {
        let counter = self.next_api_counter();
        let url = format!("{CHAT_BASE}/api/{endpoint}?c={counter}&rt=b");

        let headers = self.headers();
        let mut req = ureq::post(&url).header("Content-Type", "application/x-protobuf");
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }

        let resp = req
            .send(body)
            .map_err(|e| AuthError::SessionFetch(format!("{endpoint}: {e}")))?;

        let status = resp.status();
        if status != 200 {
            let mut err_body = Vec::new();
            let _ = resp.into_body().into_reader().read_to_end(&mut err_body);
            return Err(AuthError::SessionFetch(format!(
                "HTTP {status}: {} bytes",
                err_body.len()
            )));
        }

        let mut resp_bytes = Vec::new();
        resp.into_body()
            .into_reader()
            .read_to_end(&mut resp_bytes)
            .map_err(|e| AuthError::SessionFetch(format!("{endpoint} read: {e}")))?;

        tracing::debug!("{endpoint}: {} response bytes", resp_bytes.len());
        Ok(resp_bytes)
    }

    /// GET request returning text.
    pub fn fetch_get(&self, url: &str) -> Result<String, AuthError> {
        let headers = self.headers();
        let mut req = ureq::get(url);
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }

        let resp = req
            .call()
            .map_err(|e| AuthError::SessionFetch(format!("GET: {e}")))?;

        let mut body = String::new();
        resp.into_body()
            .into_reader()
            .read_to_string(&mut body)
            .map_err(|e| AuthError::SessionFetch(format!("GET read: {e}")))?;
        Ok(body)
    }

    /// GET request returning bytes.
    pub fn fetch_get_binary(&self, url: &str) -> Result<Vec<u8>, AuthError> {
        let headers = self.headers();
        let mut req = ureq::get(url);
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }

        let resp = req
            .call()
            .map_err(|e| AuthError::SessionFetch(format!("GET binary: {e}")))?;

        let mut body = Vec::new();
        resp.into_body()
            .into_reader()
            .read_to_end(&mut body)
            .map_err(|e| AuthError::SessionFetch(format!("GET binary read: {e}")))?;
        Ok(body)
    }

    /// Register BrowserChannel (just sets cookies).
    pub fn register(&mut self) -> Result<(), AuthError> {
        let url = format!("{CHAT_BASE}/webchannel/register?ignore_compass_cookie=1");
        let _ = self.fetch_get(&url)?;
        Ok(())
    }

    /// Acquire a SID via the bootstrap long-poll.
    pub fn acquire_sid(&mut self) -> Result<(), AuthError> {
        let zx = super::session::Session::random_zx();
        let url = format!(
            "{CHAT_BASE}/webchannel/events?\
             VER=8&RID=1&CVER=22&zx={zx}&t=1&SID=null&\
             %24req=count%3D1%26ofs%3D0%26req0_data%3D%255B%255D"
        );

        let body = self.fetch_get(&url)?;

        if let Some(sid) = super::session::extract_sid_from_stream(&body) {
            self.sid = Some(sid);
            return Ok(());
        }

        Err(AuthError::MissingField("SID"))
    }
}

/// Extract XSRF token from the Chat page HTML.
fn extract_xsrf_from_html(html: &str) -> Option<String> {
    extract_wiz_token(html, "SMqcke")
}

/// Extract the batchexecute `at` token from the Chat page HTML.
fn extract_at_token_from_html(html: &str) -> Option<String> {
    extract_wiz_token(html, "SNlM0e")
}

/// Extract a `WIZ_global_data` field value by key. The page bootstrap has
/// `"KEY":"VALUE"` inline; we read between the quotes.
fn extract_wiz_token(html: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\":\"");
    let start = html.find(&marker)?;
    let after = &html[start + marker.len()..];
    let end = after.find('"')?;
    let token = &after[..end];
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_xsrf_from_html_works() {
        let html = r#"window.WIZ_global_data={"SMqcke":"ALOzU4Rxyz123:1234567890","other":"x"}"#;
        let token = extract_xsrf_from_html(html);
        assert_eq!(token, Some("ALOzU4Rxyz123:1234567890".to_string()));
    }

    #[test]
    fn extract_xsrf_empty_returns_none() {
        let html = r#"{"SMqcke":"","other":"x"}"#;
        assert!(extract_xsrf_from_html(html).is_none());
    }
}
