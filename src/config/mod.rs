pub mod keyring;

use crate::error::ConfigError;
use crate::types::PlatformId;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default, rename = "platform")]
    pub platforms: Vec<PlatformConfig>,
    #[serde(default)]
    pub theme: ThemeConfig,
}

#[derive(Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_tick_rate")]
    pub tick_rate_ms: u64,
    #[serde(default = "default_layout")]
    pub default_layout: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            tick_rate_ms: default_tick_rate(),
            default_layout: default_layout(),
            log_level: default_log_level(),
        }
    }
}

fn default_tick_rate() -> u64 {
    100
}
fn default_layout() -> String {
    "standard".into()
}
fn default_log_level() -> String {
    "info".into()
}

#[derive(Deserialize)]
pub struct PlatformConfig {
    pub kind: PlatformKind,
    pub account: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}

#[derive(Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlatformKind {
    GoogleChat,
    Slack,
}

impl PlatformKind {
    pub fn id(self) -> PlatformId {
        match self {
            PlatformKind::GoogleChat => PlatformId::GoogleChat,
            PlatformKind::Slack => PlatformId::Slack,
        }
    }
}

#[derive(Deserialize, Default)]
pub struct ThemeConfig {
    pub space_list_bg: Option<String>,
    pub active_space_fg: Option<String>,
    pub message_name_fg: Option<String>,
    pub timestamp_fg: Option<String>,
}

impl Config {
    /// Load configuration from `~/.config/tchat/config.toml`.
    ///
    /// Returns a default config with no platforms if the file doesn't exist.
    pub fn load() -> Result<Self, ConfigError> {
        let dirs = directories::ProjectDirs::from("com", "tchat", "tchat");
        let Some(dirs) = dirs else {
            return Ok(Self::default_config());
        };
        let config_path = dirs.config_dir().join("config.toml");
        if !config_path.exists() {
            return Ok(Self::default_config());
        }
        let content = std::fs::read_to_string(&config_path).map_err(|_| ConfigError::NotFound {
            path: config_path.display().to_string(),
        })?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    fn default_config() -> Self {
        Self {
            general: GeneralConfig::default(),
            platforms: Vec::new(),
            theme: ThemeConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_minimal_config() {
        let toml_str = r#"
            [[platform]]
            kind = "google_chat"
            account = "user@company.com"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.platforms.len(), 1);
        assert_eq!(config.platforms[0].kind, PlatformKind::GoogleChat);
        assert_eq!(
            config.platforms[0].account.as_deref(),
            Some("user@company.com")
        );
    }

    #[test]
    fn deserialize_empty_config() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.platforms.is_empty());
        assert_eq!(config.general.tick_rate_ms, 100);
    }

    #[test]
    fn platform_kind_maps_to_platform_id() {
        assert_eq!(PlatformKind::GoogleChat.id(), PlatformId::GoogleChat);
        assert_eq!(PlatformKind::Slack.id(), PlatformId::Slack);
    }
}
