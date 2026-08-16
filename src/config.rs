//! Ferrokey UI configuration.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Sticky/latch settings (mirrors ferrokey-core `StateSettings`).
///
/// Sticky semantics: a quick tap on a modifier latches it for the next
/// qualifying key; a tap on an already-active modifier disengages it
/// (click-to-toggle). Locks are reached only through the Caps Lock key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StickyConfig {
    pub latch_enabled: bool,
    pub tap_timeout_ms: u64,
}

impl Default for StickyConfig {
    fn default() -> Self {
        StickyConfig {
            latch_enabled: true,
            tap_timeout_ms: 400,
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
    /// Active layout id (see ferrokey-layouts builtins / xkb specs).
    pub layout: String,
    /// Keyboard view ("compact" or "full"). Views are arrangements over the
    /// same physical-key engine; the view's preferred window size wins over
    /// `width`/`height` below.
    pub view: String,
    /// Daemon socket path.
    pub socket_path: PathBuf,
    /// Initial OSK size (physical pixels).
    pub width: u32,
    pub height: u32,
    /// Initial uniform keyboard scale (1.0 = the view's natural size).
    /// Ships at 0.75 so the OSK is compact out of the box; the window can
    /// be resized at runtime, which rescales the keyboard (0.35–3.0).
    #[serde(default = "UiConfig::default_scale")]
    pub scale: f32,
    /// Override the X11 display (for tests); `None` = auto.
    pub x11_display: Option<String>,
    pub sticky: StickyConfig,
    pub repeat: RepeatConfig,
    /// Show the degraded-mode banner even when the backend preserves focus
    /// (diagnostics).
    pub force_degraded_banner: bool,
    /// Start in text mode (characters are typed through the compose/text
    /// engine instead of raw key events). Courts use this to exercise the
    /// text path deterministically.
    pub text_mode: bool,
    /// Embedded terminal workspace mode (Phase 3 addendum #2).
    pub terminal: TerminalUiConfig,
    /// Adaptive key geometry (Phase 4 WS4): the OSK learns touch placement
    /// and adapts the effective hit targets while the visible keyboard stays
    /// stable.
    pub adaptive: AdaptiveUiConfig,
    /// Initial input destination ("system" or "terminal").
    pub destination: String,
}

/// Adaptive key geometry configuration (WS4 §4.9 user controls).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdaptiveUiConfig {
    /// Master switch: Adaptive Geometry On/Off. When off, touch hit-testing
    /// uses the plain visual rects and no samples are recorded.
    pub enabled: bool,
    /// Freeze the current effective geometry: no learning pass may mutate it
    /// until unfrozen (learning may still accumulate statistics).
    pub frozen: bool,
    /// Minimum samples per key before its hit target may adapt.
    pub min_samples: u32,
    /// Run an optimization pass after this many new samples (the optimizer
    /// never runs on the touch hot path).
    pub optimize_every: u32,
    /// The maximum normalized-distance confidence (0 = dead center) below
    /// which a touch counts as an unambiguous intended-key sample.
    pub evidence_confidence: f64,
}

impl Default for AdaptiveUiConfig {
    fn default() -> Self {
        AdaptiveUiConfig {
            enabled: true,
            frozen: false,
            min_samples: 8,
            optimize_every: 16,
            evidence_confidence: 0.5,
        }
    }
}

/// The embedded terminal workspace configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalUiConfig {
    /// Whether the embedded terminal workspace is active (`ferrokey --terminal`).
    pub enabled: bool,
    /// Terminal pane height in physical pixels (the OSK stays fixed above it).
    pub pane_height: u32,
    /// Terminal font size in physical px.
    pub font_size_px: u32,
    /// Scrollback capacity (lines).
    pub scrollback_lines: usize,
    /// Shell to run (None → `$SHELL` → `/bin/sh`).
    pub shell: Option<String>,
    /// Require confirmation for multiline pastes.
    pub confirm_multiline_paste: bool,
}

impl Default for TerminalUiConfig {
    fn default() -> Self {
        TerminalUiConfig {
            enabled: false,
            pane_height: 420,
            font_size_px: 16,
            scrollback_lines: ferrokey_terminal::limits::DEFAULT_SCROLLBACK,
            shell: None,
            confirm_multiline_paste: true,
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
        UiConfig {
            layout: "us".into(),
            view: "compact".into(),
            socket_path: PathBuf::from(runtime).join("ferrokeyd.sock"),
            width: 920,
            height: 342,
            scale: 0.75,
            x11_display: None,
            sticky: StickyConfig::default(),
            repeat: RepeatConfig::default(),
            force_degraded_banner: false,
            text_mode: false,
            terminal: TerminalUiConfig::default(),
            adaptive: AdaptiveUiConfig::default(),
            destination: "system".into(),
        }
    }
}

impl UiConfig {
    /// The shipped default keyboard scale (25% smaller than the natural
    /// view size).
    pub const fn default_scale() -> f32 {
        0.75
    }

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
        if !(config.scale > 0.0 && config.scale <= 4.0) {
            return Err(ConfigError::Invalid("scale must be in (0, 4]".into()));
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
        assert_eq!(cfg.view, "compact");
        assert!(cfg.width > 0 && cfg.height > 0);
        assert!(cfg.sticky.latch_enabled);
    }

    #[test]
    fn yaml_round_trip() {
        let yaml = "layout: de\nview: full\nwidth: 800\nheight: 300\nrepeat:\n  cadence_ms: 40\n";
        let cfg = UiConfig::parse(yaml).unwrap();
        assert_eq!(cfg.layout, "de");
        assert_eq!(cfg.view, "full");
        assert_eq!(cfg.width, 800);
        assert_eq!(cfg.repeat.cadence_ms, 40);
    }

    #[test]
    fn zero_size_rejected() {
        assert!(UiConfig::parse("width: 0\nheight: 100\n").is_err());
    }
}
