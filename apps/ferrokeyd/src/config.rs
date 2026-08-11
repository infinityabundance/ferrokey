//! `ferrokeyd` configuration.
//!
//! The daemon is a security boundary, so its configuration is deliberately
//! small and explicit: who may connect, how fast, where the socket lives,
//! and the device parameters. There is no dynamic config reload — a change
//! requires a daemon restart, which makes the running configuration
//! auditable.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Rate limiting parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RateConfig {
    /// Maximum burst of messages allowed instantly.
    pub burst: u32,
    /// Sustained messages per second.
    pub per_second: u32,
}

impl Default for RateConfig {
    fn default() -> Self {
        RateConfig {
            burst: 200,
            per_second: 200,
        }
    }
}

/// Full daemon configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Unix socket path the daemon listens on.
    pub socket_path: PathBuf,
    /// UIDs allowed to connect. Empty = nobody (deny-all by default).
    pub allowed_uids: Vec<u32>,
    /// GIDs allowed to connect. Empty = nobody.
    pub allowed_gids: Vec<u32>,
    /// Maximum concurrent client connections.
    pub max_connections: usize,
    /// Per-connection rate limits.
    pub rate: RateConfig,
    /// Maximum simultaneously-held keys on the virtual device.
    pub max_held_keys: usize,
    /// Name reported by the virtual keyboard device.
    pub device_name: String,
    /// Bind the socket with mode (permissions) applied after creation.
    pub socket_mode: u32,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
        DaemonConfig {
            socket_path: PathBuf::from(runtime).join("ferrokeyd.sock"),
            allowed_uids: Vec::new(),
            allowed_gids: Vec::new(),
            max_connections: 4,
            rate: RateConfig::default(),
            max_held_keys: 16,
            device_name: "Ferrokey Virtual Keyboard".into(),
            socket_mode: 0o660,
        }
    }
}

impl DaemonConfig {
    /// Load from a YAML file; missing fields fall back to defaults.
    pub fn load(path: &std::path::Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        Self::parse(&text)
    }

    /// Parse from YAML text.
    pub fn parse(yaml: &str) -> Result<Self, ConfigError> {
        let mut config: DaemonConfig =
            serde_yaml::from_str(yaml).map_err(|e| ConfigError::Parse(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Validate the configuration is usable and safe.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.socket_path.as_os_str().is_empty() {
            return Err(ConfigError::Invalid("socket_path must not be empty".into()));
        }
        if self.allowed_uids.is_empty() && self.allowed_gids.is_empty() {
            return Err(ConfigError::Invalid(
                "allowed_uids and allowed_gids are both empty: no client could ever connect \
                 (deny-by-default). Add at least one uid or gid."
                    .into(),
            ));
        }
        if self.max_connections == 0 {
            return Err(ConfigError::Invalid("max_connections must be >= 1".into()));
        }
        if self.rate.per_second == 0 {
            return Err(ConfigError::Invalid("rate.per_second must be >= 1".into()));
        }
        Ok(())
    }

    /// The default config path candidates (first existing wins).
    pub fn default_paths() -> Vec<PathBuf> {
        let mut paths = vec![PathBuf::from("/etc/ferrokey/ferrokeyd.yaml")];
        if let Ok(config_dir) = std::env::var("XDG_CONFIG_HOME") {
            paths.push(PathBuf::from(config_dir).join("ferrokey/ferrokeyd.yaml"));
        } else if let Ok(home) = std::env::var("HOME") {
            paths.push(PathBuf::from(home).join(".config/ferrokey/ferrokeyd.yaml"));
        }
        paths
    }
}

/// Configuration errors.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read config {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("cannot parse config: {0}")]
    Parse(String),
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_deny_by_default() {
        // The default config has empty whitelists, which must fail validation
        // — nobody can connect until an operator configures it.
        let cfg = DaemonConfig::default();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn valid_config_parses() {
        let yaml = r#"
socket_path: /tmp/ferrokeyd.sock
allowed_uids: [1000]
rate:
  burst: 300
  per_second: 250
"#;
        let cfg = DaemonConfig::parse(yaml).unwrap();
        assert_eq!(cfg.allowed_uids, vec![1000]);
        assert_eq!(cfg.rate.burst, 300);
        assert_eq!(cfg.socket_path, PathBuf::from("/tmp/ferrokeyd.sock"));
    }

    #[test]
    fn missing_whitelist_is_rejected() {
        let yaml = "socket_path: /tmp/x.sock\nallowed_uids: []\nallowed_gids: []\n";
        assert!(DaemonConfig::parse(yaml).is_err());
    }

    #[test]
    fn zero_rate_is_rejected() {
        let yaml = "socket_path: /tmp/x.sock\nallowed_uids: [1]\nrate:\n  per_second: 0\n";
        assert!(DaemonConfig::parse(yaml).is_err());
    }
}
