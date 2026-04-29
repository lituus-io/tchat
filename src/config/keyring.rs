//! Credential persistence via file storage.
//!
//! Stores tokens/cookies as individual files in the tchat data directory
//! with user-only permissions (0600). Simple, no platform-specific keyring
//! dependencies, works everywhere.
//!
//! File location: `~/.local/share/com.tchat.tchat/credentials/<key>`
//! (macOS: `~/Library/Application Support/com.tchat.tchat/credentials/<key>`)

use std::fs;
use std::path::PathBuf;

/// Store a credential value to disk.
pub fn save_token(key: &str, value: &str) -> Result<(), String> {
    let path = credential_path(key)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("failed to create credentials dir: {e}"))?;
    }

    fs::write(&path, value).map_err(|e| format!("failed to write credential: {e}"))?;

    // Set file permissions to owner-only (Unix)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&path, perms).map_err(|e| format!("failed to set permissions: {e}"))?;
    }

    Ok(())
}

/// Load a credential value from disk. Returns `None` if not found.
pub fn load_token(key: &str) -> Option<String> {
    let path = credential_path(key).ok()?;
    fs::read_to_string(path).ok()
}

/// Delete a credential from disk.
pub fn delete_token(key: &str) -> Result<(), String> {
    let path = credential_path(key)?;
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("failed to delete credential: {e}"))?;
    }
    Ok(())
}

fn credential_path(key: &str) -> Result<PathBuf, String> {
    let dirs = directories::ProjectDirs::from("com", "tchat", "tchat")
        .ok_or("could not determine data directory")?;
    // Sanitize key: replace non-alphanumeric chars with underscores
    let safe_key: String = key
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(dirs.data_dir().join("credentials").join(safe_key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_path_sanitizes_key() {
        let path = credential_path("googlechat_cookies:user@example.com").unwrap();
        let filename = path.file_name().unwrap().to_str().unwrap();
        assert!(!filename.contains(':'));
        assert!(!filename.contains('@'));
        assert!(filename.contains("googlechat_cookies_user_example_com"));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let key = "test_roundtrip_token";
        let value = "secret_cookie_value_123";

        // Clean up any previous test data
        let _ = delete_token(key);

        save_token(key, value).unwrap();
        let loaded = load_token(key);
        assert_eq!(loaded, Some(value.to_owned()));

        // Clean up
        delete_token(key).unwrap();
        assert_eq!(load_token(key), None);
    }

    #[test]
    fn load_missing_returns_none() {
        assert_eq!(load_token("nonexistent_key_xyz"), None);
    }
}
