//! Ferrokey UI configuration.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Sticky/latch settings (mirrors ferrokey-core `StateSettings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StickyConfig {
    pub latch_enabled: bool,
    pub lock_enabled: bool,
    pub tap_timeout_ms: u64,
    pub double_tap_timeout_ms: u64,
}

impl Default for StickyConfig {
    fn default() -> Self {
        StickyConfig {
            latch_enabled: true,
            lock_enabled: true,
            tap_timeout_ms: 400,
            double_tap_timeout_ms: 500,
        }
    }
}

/// Key repeat settings (mirrors ferrokey-core `RepeatSettings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepeatConfig {
    pub enabled: bool,
    pub delay_ms: u64,
    pub cadence_ms: u64,
}

impl Default for RepeatConfig {
    fn default() -> Self {
        RepeatConfig {
            enabled: true,
            delay_ms: 500,
            cadence_ms: 30,
        }
    }
}

/// UI configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// Active layout id (see ferrokey-layouts builtins).
    pub layout: String,
    /// Daemon socket path.
    pub socket_path: PathBuf,
    /// Initial OSK size (physical pixels).
    pub width: u32,
    pub height: u32,
    /// Override the X11 display (for tests); `None` = auto.
    pub x11_display: Option<String>,
    pub sticky: StickyConfig,
    pub repeat: RepeatConfig,
    /// Show the degraded-mode banner even when the backend preserves focus
    /// (diagnostics).
    pub force_degraded_banner: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
        UiConfig {
            layout: "us".into(),
            socket_path: PathBuf::from(runtime).join("ferrokeyd.sock"),
            width: 920,
            height: 342,
            x11_display: None,
            sticky: StickyConfig::default(),
            repeat: RepeatConfig::default(),
            force_degraded_banner: false,
        }
    }
}

impl UiConfig {
    pub fn load(path: &std::path::Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        Self::parse(&text)
    }

    pub fn parse(yaml: &str) -> Result<Self, ConfigError> {
        let config: UiConfig =
            serde_yaml::from_str(yaml).map_err(|e| ConfigError::Parse(e.to_string()))?;
        if config.width == 0 || config.height == 0 {
            return Err(ConfigError::Invalid("width and height must be > 0".into()));
        }
        Ok(config)
    }

    pub fn default_paths() -> Vec<PathBuf> {
        let mut paths = vec![PathBuf::from("/etc/ferrokey/ferrokey.yaml")];
        if let Ok(config_dir) = std::env::var("XDG_CONFIG_HOME") {
            paths.push(PathBuf::from(config_dir).join("ferrokey/ferrokey.yaml"));
        } else if let Ok(home) = std::env::var("HOME") {
            paths.push(PathBuf::from(home).join(".config/ferrokey/ferrokey.yaml"));
        }
        paths
    }
}

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
    fn defaults_parse() {
        let cfg = UiConfig::default();
        assert_eq!(cfg.layout, "us");
        assert!(cfg.width > 0 && cfg.height > 0);
        assert!(cfg.sticky.latch_enabled);
    }

    #[test]
    fn yaml_round_trip() {
        let yaml = "layout: de\nwidth: 800\nheight: 300\nrepeat:\n  cadence_ms: 40\n";
        let cfg = UiConfig::parse(yaml).unwrap();
        assert_eq!(cfg.layout, "de");
        assert_eq!(cfg.width, 800);
        assert_eq!(cfg.repeat.cadence_ms, 40);
    }

    #[test]
    fn zero_size_rejected() {
        assert!(UiConfig::parse("width: 0\nheight: 100\n").is_err());
    }
}
