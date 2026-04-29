//! Chrome cookie extraction for macOS.
//!
//! Reads Google Chat session cookies directly from Chrome's SQLite database,
//! decrypts them using the key stored in the macOS Keychain, and returns
//! them for use in direct HTTP requests (eliminating the Chrome process).
//!
//! Flow:
//! 1. Read encryption key from macOS Keychain ("Chrome Safe Storage")
//! 2. Derive AES-128-CBC key via PBKDF2 (salt="saltysalt", iterations=1003)
//! 3. Open Chrome's Cookies SQLite database
//! 4. Query for google.com cookies (SID, HSID, SSID, OSID, SAPISID, etc.)
//! 5. Decrypt each cookie value using AES-128-CBC
//!
//! Reference: Chromium source `os_crypt_mac.mm`

use std::path::PathBuf;

use crate::error::AuthError;

/// The cookies needed for Google Chat API calls.
#[derive(Debug, Clone)]
pub struct ChatCookies {
    /// All cookies as a formatted Cookie header string.
    pub cookie_header: String,
    /// SAPISID cookie value (needed for SAPISIDHASH auth).
    pub sapisid: Option<String>,
    /// Individual cookie map for reference.
    pub cookies: Vec<(String, String)>,
}

/// File where we persist cookies from Chrome auth for direct mode reuse.
const SAVED_COOKIES_FILE: &str = "chat_cookies.json";

/// Required cookie names for Google Chat.
const REQUIRED_COOKIES: &[&str] = &["SID", "HSID", "SSID"];

/// Save cookies to disk for reuse without Chrome on the next run.
pub fn save_cookies(cookies: &ChatCookies) -> Result<(), AuthError> {
    let dirs = directories::ProjectDirs::from("com", "tchat", "tchat")
        .ok_or(AuthError::CredentialStorage("no data dir".into()))?;
    let dir = dirs.data_dir();
    std::fs::create_dir_all(dir)
        .map_err(|e| AuthError::CredentialStorage(format!("mkdir: {e}")))?;
    let path = dir.join(SAVED_COOKIES_FILE);
    let json = serde_json::to_string(&cookies.cookies)
        .map_err(|e| AuthError::CredentialStorage(format!("json: {e}")))?;
    std::fs::write(&path, json).map_err(|e| AuthError::CredentialStorage(format!("write: {e}")))?;

    // Restrict permissions (Unix)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Load previously saved cookies from disk.
pub fn load_saved_cookies() -> Result<ChatCookies, AuthError> {
    let dirs = directories::ProjectDirs::from("com", "tchat", "tchat")
        .ok_or(AuthError::CredentialStorage("no data dir".into()))?;
    let path = dirs.data_dir().join(SAVED_COOKIES_FILE);
    if !path.exists() {
        return Err(AuthError::CredentialStorage("no saved cookies".into()));
    }
    let json = std::fs::read_to_string(&path)
        .map_err(|e| AuthError::CredentialStorage(format!("read: {e}")))?;
    let cookies: Vec<(String, String)> = serde_json::from_str(&json)
        .map_err(|e| AuthError::CredentialStorage(format!("parse: {e}")))?;

    // Verify required cookies exist
    for req in REQUIRED_COOKIES {
        if !cookies.iter().any(|(n, _)| n == req) {
            return Err(AuthError::CredentialStorage(format!(
                "Saved cookies missing {req}. Re-authenticate with Chrome."
            )));
        }
    }

    let sapisid = cookies
        .iter()
        .find(|(n, _)| n == "SAPISID")
        .map(|(_, v)| v.clone());
    let cookie_header = cookies
        .iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; ");

    Ok(ChatCookies {
        cookie_header,
        sapisid,
        cookies,
    })
}

/// Extract cookies from a live headless_chrome session (via CDP) and build
/// a ChatCookies struct. Called after successful Chrome auth.
pub fn extract_from_chrome_session(
    tab: &std::sync::Arc<headless_chrome::Tab>,
) -> Result<ChatCookies, AuthError> {
    // Use CDP Network.getAllCookies to get all cookies from the browser
    #[derive(serde::Serialize, Debug)]
    struct GetAllCookies;
    #[derive(serde::Deserialize, Debug)]
    struct CdpCookie {
        name: String,
        value: String,
        domain: String,
    }
    #[derive(serde::Deserialize, Debug)]
    struct GetAllCookiesResponse {
        cookies: Vec<CdpCookie>,
    }
    impl headless_chrome::protocol::cdp::types::Method for GetAllCookies {
        const NAME: &'static str = "Network.getAllCookies";
        type ReturnObject = GetAllCookiesResponse;
    }

    let resp = tab
        .call_method(GetAllCookies)
        .map_err(|e| AuthError::SessionFetch(format!("getAllCookies: {e}")))?;

    let mut cookies = Vec::new();
    for c in &resp.cookies {
        if c.domain.contains("google.com") || c.domain.contains("google.ca") {
            cookies.push((c.name.clone(), c.value.clone()));
        }
    }

    let sapisid = cookies
        .iter()
        .find(|(n, _)| n == "SAPISID")
        .map(|(_, v)| v.clone());
    let cookie_header = cookies
        .iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; ");

    Ok(ChatCookies {
        cookie_header,
        sapisid,
        cookies,
    })
}
const DESIRED_COOKIES: &[&str] = &[
    "SID",
    "HSID",
    "SSID",
    "OSID",
    "SAPISID",
    "APISID",
    "SIDCC",
    "__Secure-1PSIDCC",
    "__Secure-3PSIDCC",
    "__Secure-1PSID",
    "__Secure-3PSID",
    "__Secure-1PSIDTS",
    "__Secure-3PSIDTS",
    "__Secure-1PAPISID",
    "__Secure-3PAPISID",
    "__Secure-OSID",
    "COMPASS",
    "OGPC",
    "NID",
    "LSID",
];

/// Extract Google Chat cookies from Chrome's database.
pub fn extract_chrome_cookies() -> Result<ChatCookies, AuthError> {
    let db_path = chrome_cookies_db_path()?;
    let key = get_chrome_encryption_key()?;
    let derived_key = derive_aes_key(&key);

    let cookies = read_and_decrypt_cookies(&db_path, &derived_key)?;

    // Verify we have the minimum required cookies
    for required in REQUIRED_COOKIES {
        if !cookies.iter().any(|(n, _)| n == required) {
            return Err(AuthError::CredentialStorage(format!(
                "Missing required cookie: {required}. Please log into Google Chat in Chrome first."
            )));
        }
    }

    let sapisid = cookies
        .iter()
        .find(|(n, _)| n == "SAPISID")
        .map(|(_, v)| v.clone());

    let cookie_header = cookies
        .iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; ");

    Ok(ChatCookies {
        cookie_header,
        sapisid,
        cookies,
    })
}

/// Path to Chrome's Cookies database — checks tchat's persistent profile first,
/// then the user's regular Chrome.
fn chrome_cookies_db_path() -> Result<PathBuf, AuthError> {
    // First: tchat's own persistent Chrome profile (has Chat cookies)
    if let Some(dirs) = directories::ProjectDirs::from("com", "tchat", "tchat") {
        let tchat_cookies = dirs.data_dir().join("chrome-profile/Default/Cookies");
        if tchat_cookies.exists() {
            return Ok(tchat_cookies);
        }
    }

    // Fallback: user's regular Chrome
    let home =
        std::env::var("HOME").map_err(|_| AuthError::CredentialStorage("HOME not set".into()))?;
    let candidates = [
        format!("{home}/Library/Application Support/Google/Chrome/Default/Cookies"),
        format!("{home}/Library/Application Support/Google/Chrome/Profile 1/Cookies"),
    ];
    for path in &candidates {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
    }

    Err(AuthError::CredentialStorage(
        "No Chrome Cookies database found. Run tchat with Chrome first.".into(),
    ))
}

/// Read Chrome's encryption key from macOS Keychain.
fn get_chrome_encryption_key() -> Result<Vec<u8>, AuthError> {
    let output = std::process::Command::new("security")
        .args(["find-generic-password", "-w", "-s", "Chrome Safe Storage"])
        .output()
        .map_err(|e| AuthError::CredentialStorage(format!("security command failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AuthError::CredentialStorage(format!(
            "Could not read Chrome Safe Storage from Keychain: {stderr}\n\
                 You may need to allow access when prompted by macOS."
        )));
    }

    let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(key.into_bytes())
}

/// Derive the AES-128-CBC key from Chrome's password using PBKDF2.
///
/// Chromium uses: PBKDF2-HMAC-SHA1, salt="saltysalt", iterations=1003, key_len=16
fn derive_aes_key(password: &[u8]) -> [u8; 16] {
    use pbkdf2::pbkdf2_hmac;
    use sha1::Sha1;

    let mut key = [0u8; 16];
    pbkdf2_hmac::<Sha1>(password, b"saltysalt", 1003, &mut key);
    key
}

/// Read cookies from the SQLite database and decrypt them.
fn read_and_decrypt_cookies(
    db_path: &std::path::Path,
    aes_key: &[u8; 16],
) -> Result<Vec<(String, String)>, AuthError> {
    // Copy the database to a temp location to avoid locking issues
    // (Chrome holds a WAL lock on the database while running)
    let temp_dir = std::env::temp_dir();
    let temp_db = temp_dir.join(format!("tchat-cookies-{}.db", std::process::id()));
    std::fs::copy(db_path, &temp_db).map_err(|e| {
        AuthError::CredentialStorage(format!(
            "Could not copy Cookies database (is Chrome running?): {e}"
        ))
    })?;

    // Also copy WAL/SHM if they exist
    let wal_path = db_path.with_extension("Cookies-wal");
    let shm_path = db_path.with_extension("Cookies-shm");
    if wal_path.exists() {
        let _ = std::fs::copy(&wal_path, temp_db.with_extension("db-wal"));
    }
    if shm_path.exists() {
        let _ = std::fs::copy(&shm_path, temp_db.with_extension("db-shm"));
    }

    let conn = rusqlite::Connection::open_with_flags(
        &temp_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| AuthError::CredentialStorage(format!("SQLite open failed: {e}")))?;

    let mut result = Vec::new();

    // Query cookies for domains that chat.google.com needs:
    // - .google.com (main session: SID, HSID, SSID, SIDCC, SAPISID)
    // - chat.google.com (COMPASS, OSID)
    // - accounts.google.com (LSID, GAPS)
    // DO NOT include .google.ca or other TLDs — duplicate cookies cause 401.
    let mut stmt = conn
        .prepare(
            "SELECT name, encrypted_value, value, host_key FROM cookies \
             WHERE (host_key = '.google.com' \
                    OR host_key = 'chat.google.com' \
                    OR host_key = 'accounts.google.com' \
                    OR host_key LIKE '%.clients6.google.com') \
             ORDER BY host_key, name",
        )
        .map_err(|e| AuthError::CredentialStorage(format!("SQL prepare: {e}")))?;

    let rows = stmt
        .query_map([], |row| {
            let name: String = row.get(0)?;
            let encrypted: Vec<u8> = row.get(1)?;
            let plain: String = row.get(2)?;
            let host: String = row.get(3)?;
            Ok((name, encrypted, plain, host))
        })
        .map_err(|e| AuthError::CredentialStorage(format!("SQL query: {e}")))?;

    // Track which cookies we've already added (prefer .google.com over .google.ca)
    let mut seen = std::collections::HashSet::new();

    for row in rows {
        let (name, encrypted, plain, _host) =
            row.map_err(|e| AuthError::CredentialStorage(format!("row read: {e}")))?;

        // Only extract cookies we care about
        if !DESIRED_COOKIES.iter().any(|&c| c == name) {
            continue;
        }

        // Skip duplicates (first occurrence wins)
        if seen.contains(&name) {
            continue;
        }

        // Prefer plaintext if available (older Chrome versions)
        if !plain.is_empty() {
            seen.insert(name.clone());
            result.push((name, plain));
            continue;
        }

        // Decrypt the cookie value
        if encrypted.len() < 4 {
            continue;
        }

        // Chrome v80+ on macOS: encrypted_value starts with "v10" (3 bytes)
        // followed by AES-128-CBC encrypted data with IV = 16 bytes of 0x20
        let prefix = &encrypted[..encrypted.len().min(3)];
        if encrypted.starts_with(b"v10") || encrypted.starts_with(b"v11") {
            match decrypt_cookie_value(&encrypted[3..], aes_key) {
                Ok(value) => {
                    seen.insert(name.clone());
                    result.push((name, value));
                }
                Err(e) => {
                    eprintln!(
                        "  decrypt {name} ({} bytes, prefix={prefix:?}): {e}",
                        encrypted.len()
                    );
                }
            }
        } else if !encrypted.is_empty() {
            eprintln!(
                "  unknown prefix for {name}: {:?} ({} bytes)",
                prefix,
                encrypted.len()
            );
        }
    }

    // Cleanup temp file
    let _ = std::fs::remove_file(&temp_db);
    let _ = std::fs::remove_file(temp_db.with_extension("db-wal"));
    let _ = std::fs::remove_file(temp_db.with_extension("db-shm"));

    Ok(result)
}

/// Decrypt a single Chrome cookie value using AES-128-CBC.
///
/// IV is 16 bytes of space (0x20) — Chromium's hardcoded IV for macOS.
/// After decryption, skip the first 32 bytes of the plaintext — Chrome
/// prepends a 32-byte nonce/metadata block to the actual cookie value.
fn decrypt_cookie_value(encrypted: &[u8], key: &[u8; 16]) -> Result<String, AuthError> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit};

    type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

    let iv = [0x20u8; 16]; // Chromium uses spaces as IV

    let mut buf = encrypted.to_vec();
    let decrypted = Aes128CbcDec::new(key.into(), &iv.into())
        .decrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(&mut buf)
        .map_err(|e| AuthError::CredentialStorage(format!("AES decrypt: {e}")))?;

    // Skip the 32-byte nonce/metadata prefix Chrome adds to the plaintext.
    // On older Chrome versions this prefix may not exist; if skipping
    // produces non-UTF-8, try without skip.
    let value_bytes = if decrypted.len() > 32 {
        let skipped = &decrypted[32..];
        if std::str::from_utf8(skipped).is_ok() {
            skipped
        } else if std::str::from_utf8(decrypted).is_ok() {
            decrypted
        } else {
            // Try 16-byte skip as a middle ground
            &decrypted[decrypted.len().min(16)..]
        }
    } else {
        decrypted
    };

    String::from_utf8(value_bytes.to_vec())
        .map_err(|e| AuthError::CredentialStorage(format!("cookie not UTF-8: {e}")))
}

/// Compute the SAPISIDHASH authentication header.
///
/// Format: `SAPISIDHASH <timestamp>_<SHA1(timestamp + " " + sapisid + " " + origin)>`
pub fn compute_sapisidhash(sapisid: &str, origin: &str) -> String {
    use sha1::{Digest, Sha1};

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let input = format!("{timestamp} {sapisid} {origin}");
    let mut hasher = Sha1::new();
    hasher.update(input.as_bytes());
    let hash = hasher.finalize();
    let hex = hash.iter().map(|b| format!("{b:02x}")).collect::<String>();

    format!("SAPISIDHASH {timestamp}_{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_aes_key_produces_16_bytes() {
        let key = derive_aes_key(b"test_password");
        assert_eq!(key.len(), 16);
    }

    #[test]
    fn derive_aes_key_matches_python() {
        // Verified against Python: hashlib.pbkdf2_hmac('sha1', b'svT3SvCAAiRbU7nlqBCGUg==', b'saltysalt', 1003, 16)
        let key = derive_aes_key(b"svT3SvCAAiRbU7nlqBCGUg==");
        assert_eq!(
            key.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "3497a799e94fdfb956fbade452f7c8f2"
        );
    }

    #[test]
    fn sapisidhash_format() {
        let hash = compute_sapisidhash("abc123", "https://chat.google.com");
        assert!(hash.starts_with("SAPISIDHASH "));
        let parts: Vec<&str> = hash.split(' ').collect();
        assert_eq!(parts.len(), 2);
        assert!(parts[1].contains('_'));
    }

    #[test]
    fn chrome_db_path_exists_or_not() {
        // Just verify it doesn't panic
        let _ = chrome_cookies_db_path();
    }
}
