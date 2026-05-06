use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),

    #[error("terminal error: {0}")]
    Terminal(#[from] std::io::Error),

    #[error("all platforms disconnected")]
    AllDisconnected,

    #[error("authentication: {0}")]
    Auth(#[from] AuthError),

    #[error("api: {0}")]
    Api(#[from] ApiError),
}

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("config file not found: {path}")]
    NotFound { path: String },

    #[error("invalid TOML: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("missing required field: {field}")]
    MissingField { field: &'static str },
}

#[derive(Error, Debug)]
pub enum AuthError {
    #[error("OAuth2 flow failed: {0}")]
    OAuthFailed(String),

    #[error("Dynamite token exchange failed: {0}")]
    DynamiteExchange(String),

    #[error("session token fetch failed: {0}")]
    SessionFetch(String),

    #[error("token refresh failed: {0}")]
    RefreshFailed(String),

    #[error("missing field in auth response: {0}")]
    MissingField(&'static str),

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("credential storage error: {0}")]
    CredentialStorage(String),
}

#[derive(Error, Debug)]
pub enum ChannelError {
    #[error("session expired (SID invalid)")]
    SessionExpired,

    #[error("malformed BrowserChannel frame")]
    MalformedFrame,

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Error, Debug)]
pub enum ApiError {
    #[error("HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },

    #[error("protobuf decode error: {0}")]
    ProtoDecode(#[from] prost::DecodeError),

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("authentication expired")]
    AuthExpired,
}

impl ApiError {
    pub fn is_auth_expired(&self) -> bool {
        matches!(
            self,
            ApiError::AuthExpired | ApiError::HttpStatus { status: 401, .. }
        )
    }
}

#[derive(Error, Debug)]
pub enum PbliteError {
    #[error("expected JSON array")]
    ExpectedArray,

    #[error("invalid field key in sparse map: {0}")]
    InvalidFieldKey(String),

    #[error("unsupported wire type: {0}")]
    UnsupportedWireType(u32),

    #[error("unexpected JSON type at field {field}: {detail}")]
    UnexpectedType { field: u32, detail: String },

    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("varint overflow")]
    VarintOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_error_display() {
        let e = AppError::Config(ConfigError::NotFound {
            path: "/tmp/cfg".into(),
        });
        assert!(format!("{e}").contains("configuration error"));
        assert!(format!("{e}").contains("/tmp/cfg"));

        let e = AppError::Terminal(std::io::Error::other("boom"));
        assert!(format!("{e}").contains("terminal error"));
        assert!(format!("{e}").contains("boom"));

        let e = AppError::AllDisconnected;
        assert!(format!("{e}").contains("all platforms disconnected"));
    }

    #[test]
    fn config_error_display() {
        let e = ConfigError::NotFound {
            path: "/etc/app.toml".into(),
        };
        assert!(format!("{e}").contains("config file not found"));
        assert!(format!("{e}").contains("/etc/app.toml"));

        let e = ConfigError::MissingField { field: "token" };
        assert!(format!("{e}").contains("missing required field"));
        assert!(format!("{e}").contains("token"));
    }

    #[test]
    fn config_error_from_toml() {
        let bad = "not valid [[[ toml";
        let toml_err = toml::from_str::<toml::Value>(bad).unwrap_err();
        let e = ConfigError::from(toml_err);
        assert!(format!("{e}").contains("invalid TOML"));
    }

    #[test]
    fn auth_error_display() {
        let cases: Vec<(AuthError, &str)> = vec![
            (
                AuthError::OAuthFailed("timeout".into()),
                "OAuth2 flow failed",
            ),
            (
                AuthError::DynamiteExchange("bad".into()),
                "Dynamite token exchange failed",
            ),
            (
                AuthError::SessionFetch("nope".into()),
                "session token fetch failed",
            ),
            (
                AuthError::RefreshFailed("expired".into()),
                "token refresh failed",
            ),
            (
                AuthError::MissingField("id_token"),
                "missing field in auth response",
            ),
            (AuthError::Http("502".into()), "HTTP error"),
            (
                AuthError::CredentialStorage("locked".into()),
                "credential storage error",
            ),
        ];
        for (e, expected) in cases {
            let msg = format!("{e}");
            assert!(msg.contains(expected), "expected {expected:?} in {msg:?}");
        }
    }

    #[test]
    fn channel_error_display_and_from_impls() {
        let e = ChannelError::SessionExpired;
        assert!(format!("{e}").contains("session expired"));

        let e = ChannelError::MalformedFrame;
        assert!(format!("{e}").contains("malformed BrowserChannel frame"));

        let e = ChannelError::Http("503".into());
        assert!(format!("{e}").contains("HTTP error"));

        // From<serde_json::Error>
        let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let e = ChannelError::from(json_err);
        assert!(format!("{e}").contains("JSON parse error"));

        // From<std::io::Error>
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe");
        let e = ChannelError::from(io_err);
        assert!(format!("{e}").contains("IO error"));
    }

    #[test]
    fn api_error_display_and_is_auth_expired() {
        let e = ApiError::HttpStatus {
            status: 404,
            body: "not found".into(),
        };
        assert!(format!("{e}").contains("HTTP 404"));
        assert!(format!("{e}").contains("not found"));
        assert!(!e.is_auth_expired());

        let e = ApiError::Http("conn refused".into());
        assert!(format!("{e}").contains("HTTP error"));
        assert!(!e.is_auth_expired());

        let e = ApiError::AuthExpired;
        assert!(format!("{e}").contains("authentication expired"));
        assert!(e.is_auth_expired());

        // 401 should also be treated as auth expired
        let e = ApiError::HttpStatus {
            status: 401,
            body: "unauthorized".into(),
        };
        assert!(e.is_auth_expired());
    }

    #[test]
    fn api_error_from_prost() {
        let prost_err = prost::DecodeError::new("truncated");
        let e = ApiError::from(prost_err);
        assert!(format!("{e}").contains("protobuf decode error"));
    }

    #[test]
    fn pblite_error_display() {
        let cases: Vec<(PbliteError, &str)> = vec![
            (PbliteError::ExpectedArray, "expected JSON array"),
            (
                PbliteError::InvalidFieldKey("abc".into()),
                "invalid field key",
            ),
            (
                PbliteError::UnsupportedWireType(7),
                "unsupported wire type: 7",
            ),
            (
                PbliteError::UnexpectedType {
                    field: 3,
                    detail: "got string".into(),
                },
                "unexpected JSON type at field 3",
            ),
            (PbliteError::VarintOverflow, "varint overflow"),
        ];
        for (e, expected) in cases {
            let msg = format!("{e}");
            assert!(msg.contains(expected), "expected {expected:?} in {msg:?}");
        }
    }

    #[test]
    fn pblite_error_from_base64() {
        let b64_err = base64::DecodeError::InvalidByte(0, b'!');
        let e = PbliteError::from(b64_err);
        assert!(format!("{e}").contains("base64 decode error"));
    }

    #[test]
    fn app_error_from_config_error() {
        let ce = ConfigError::MissingField { field: "key" };
        let ae = AppError::from(ce);
        assert!(format!("{ae}").contains("configuration error"));
    }

    #[test]
    fn app_error_from_io_error() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let ae = AppError::from(io);
        assert!(format!("{ae}").contains("terminal error"));
    }
}
