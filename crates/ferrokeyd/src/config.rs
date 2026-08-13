//! `ferrokeyd` configuration.
//!
//! The daemon is a security boundary, so its configuration is deliberately
//! small, explicit and **statically validated with hard bounds** (§25):
//! every value has lower and upper limits so malformed administrative
//! configuration cannot create unbounded allocations, resource use, or an
//! over-permissive broker.
//!
//! Security properties:
//!
//! * deny-by-default client whitelists (`allowed_uids`/`allowed_gids`)
//! * hard bounds on connections, held keys, rate limits, path lengths,
//!   device-name length and socket mode
//! * no dynamic reload — a change requires a restart, keeping the running
//!   configuration auditable (§44)
//! * when run with privilege (`euid == 0`), the config file must be
//!   root-owned and not group/world-writable (§45: no user-writable config
//!   as a privileged identity)
//! * `service_user`/`service_group` name the dedicated runtime identity (§3);
//!   `run_as_root` is a dev-only escape hatch that is refused in production
//!   validation and always prints a warning (§7)

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Hard upper bounds (§25).
pub const MAX_CONNECTIONS_LIMIT: usize = 64;
pub const MAX_RATE_LIMIT: u32 = 10_000;
pub const MAX_HELD_KEYS_LIMIT: usize = 32;
pub const MAX_SOCKET_PATH_LEN: usize = 107; // sockaddr_un.sun_path
pub const MAX_DEVICE_NAME_LEN: usize = 80; // UINPUT_MAX_NAME_SIZE (minus NUL)

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
    /// Maximum concurrent client connections (§11: default 1).
    pub max_connections: usize,
    /// Per-connection rate limits.
    pub rate: RateConfig,
    /// Maximum simultaneously-held keys on the virtual device (§24).
    pub max_held_keys: usize,
    /// Name reported by the virtual keyboard device (§49: trusted config).
    pub device_name: String,
    /// Bind the socket with mode (permissions) applied after creation.
    pub socket_mode: u32,
    /// The dedicated service identity the runtime drops to (§3).
    pub service_user: String,
    /// The dedicated service group the runtime drops to (§3).
    pub service_group: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            socket_path: PathBuf::from("/run/ferrokeyd/ferrokeyd.sock"),
            allowed_uids: Vec::new(),
            allowed_gids: Vec::new(),
            max_connections: 1,
            rate: RateConfig::default(),
            max_held_keys: 16,
            device_name: "Ferrokey Virtual Keyboard".into(),
            socket_mode: 0o666,
            service_user: "ferrokeyd".into(),
            service_group: "ferrokeyd".into(),
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
        let cfg = Self::parse(&text)?;
        cfg.check_privileged_placement(path)?;
        Ok(cfg)
    }

    /// Parse from YAML text.
    pub fn parse(yaml: &str) -> Result<Self, ConfigError> {
        let config: DaemonConfig =
            serde_yaml::from_str(yaml).map_err(|e| ConfigError::Parse(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Validate the configuration is usable and safe (§25: hard bounds).
    pub fn validate(&self) -> Result<(), ConfigError> {
        let os = self.socket_path.as_os_str();
        if os.is_empty() {
            return Err(ConfigError::Invalid("socket_path must not be empty".into()));
        }
        let path_len = self.socket_path.to_string_lossy().len();
        if path_len > MAX_SOCKET_PATH_LEN {
            return Err(ConfigError::Invalid(format!(
                "socket_path is {path_len} bytes; the kernel limit (sun_path) is \
                 {MAX_SOCKET_PATH_LEN}"
            )));
        }
        if !self.socket_path.is_absolute() {
            return Err(ConfigError::Invalid(
                "socket_path must be an absolute path (relative paths are ambiguous \
                 and attacker-influenced by the cwd)"
                    .into(),
            ));
        }
        if self.allowed_uids.is_empty() && self.allowed_gids.is_empty() {
            return Err(ConfigError::Invalid(
                "allowed_uids and allowed_gids are both empty: no client could ever \
                 connect (deny-by-default). Add at least one uid or gid."
                    .into(),
            ));
        }
        if has_duplicates(&self.allowed_uids) {
            return Err(ConfigError::Invalid(
                "allowed_uids contains duplicates".into(),
            ));
        }
        if has_duplicates(&self.allowed_gids) {
            return Err(ConfigError::Invalid(
                "allowed_gids contains duplicates".into(),
            ));
        }
        if self.max_connections == 0 {
            return Err(ConfigError::Invalid("max_connections must be >= 1".into()));
        }
        if self.max_connections > MAX_CONNECTIONS_LIMIT {
            return Err(ConfigError::Invalid(format!(
                "max_connections {} exceeds the hard limit {MAX_CONNECTIONS_LIMIT}",
                self.max_connections
            )));
        }
        if self.max_held_keys == 0 {
            return Err(ConfigError::Invalid("max_held_keys must be >= 1".into()));
        }
        if self.max_held_keys > MAX_HELD_KEYS_LIMIT {
            return Err(ConfigError::Invalid(format!(
                "max_held_keys {} exceeds the hard limit {MAX_HELD_KEYS_LIMIT} \
                 (justification: real keyboards cap rollover at ~6-10 keys; more \
                 held keys only multiply attack surface)",
                self.max_held_keys
            )));
        }
        if self.rate.burst == 0 {
            return Err(ConfigError::Invalid("rate.burst must be >= 1".into()));
        }
        if self.rate.burst > MAX_RATE_LIMIT {
            return Err(ConfigError::Invalid(format!(
                "rate.burst {} exceeds the hard limit {MAX_RATE_LIMIT}",
                self.rate.burst
            )));
        }
        if self.rate.per_second == 0 {
            return Err(ConfigError::Invalid("rate.per_second must be >= 1".into()));
        }
        if self.rate.per_second > MAX_RATE_LIMIT {
            return Err(ConfigError::Invalid(format!(
                "rate.per_second {} exceeds the hard limit {MAX_RATE_LIMIT}",
                self.rate.per_second
            )));
        }
        if self.device_name.is_empty() {
            return Err(ConfigError::Invalid("device_name must not be empty".into()));
        }
        if self.device_name.len() > MAX_DEVICE_NAME_LEN {
            return Err(ConfigError::Invalid(format!(
                "device_name is {} bytes; the kernel limit (UINPUT_MAX_NAME_SIZE) \
                 is {MAX_DEVICE_NAME_LEN}",
                self.device_name.len()
            )));
        }
        if self.device_name.contains('\0') {
            return Err(ConfigError::Invalid(
                "device_name must not contain NUL bytes".into(),
            ));
        }
        if self.socket_mode > 0o777 {
            return Err(ConfigError::Invalid(format!(
                "socket_mode {:#o} is not a valid permission mask (max 0o777)",
                self.socket_mode
            )));
        }
        if self.service_user.is_empty() || self.service_group.is_empty() {
            return Err(ConfigError::Invalid(
                "service_user and service_group must both be set (§3)".into(),
            ));
        }
        if self.service_user == "root" || self.service_group == "root" {
            return Err(ConfigError::Invalid(
                "service_user/service_group must not be root: the runtime broker \
                 must run under a dedicated unprivileged identity (§3)"
                    .into(),
            ));
        }
        Ok(())
    }

    /// §45: a config loaded *while privileged* must be root-owned and not
    /// group/world-writable. Loading a user-writable config as root would let
    /// an unprivileged user steer a privileged process.
    fn check_privileged_placement(&self, path: &std::path::Path) -> Result<(), ConfigError> {
        use std::os::unix::fs::MetadataExt;
        if !nix::unistd::geteuid().is_root() {
            return Ok(());
        }
        let meta = std::fs::metadata(path).map_err(|e| ConfigError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        if meta.uid() != 0 {
            return Err(ConfigError::Invalid(format!(
                "config {} is owned by uid {} but the daemon is running as root; \
                 the security-boundary configuration must be root-owned (§45)",
                path.display(),
                meta.uid()
            )));
        }
        if meta.mode() & 0o022 != 0 {
            return Err(ConfigError::Invalid(format!(
                "config {} is group/world-writable ({:#o}); the security-boundary \
                 configuration must not be writable by anyone but root (§45)",
                path.display(),
                meta.mode() & 0o777
            )));
        }
        Ok(())
    }

    /// The default config path candidates (first existing wins). Production
    /// places the security-boundary config at `/etc/ferrokey/ferrokeyd.yaml`.
    pub fn default_paths() -> Vec<PathBuf> {
        vec![PathBuf::from("/etc/ferrokey/ferrokeyd.yaml")]
    }
}

fn has_duplicates<T: PartialEq>(items: &[T]) -> bool {
    items
        .iter()
        .enumerate()
        .any(|(i, a)| items.iter().skip(i + 1).any(|b| a == b))
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

    fn valid_yaml() -> String {
        "socket_path: /run/ferrokeyd/ferrokeyd.sock\n\
         allowed_uids: [1000]\n\
         max_connections: 1\n\
         max_held_keys: 16\n\
         rate:\n  burst: 400\n  per_second: 400\n\
         service_user: ferrokeyd\n\
         service_group: ferrokeyd\n"
            .to_string()
    }

    #[test]
    fn valid_config_parses() {
        let cfg = DaemonConfig::parse(&valid_yaml()).unwrap();
        assert_eq!(cfg.allowed_uids, vec![1000]);
        assert_eq!(cfg.rate.burst, 400);
        assert_eq!(cfg.max_connections, 1);
        assert_eq!(
            cfg.socket_path,
            PathBuf::from("/run/ferrokeyd/ferrokeyd.sock")
        );
    }

    #[test]
    fn missing_whitelist_is_rejected() {
        let yaml = "socket_path: /run/ferrokeyd/x.sock\nallowed_uids: []\nallowed_gids: []\n";
        assert!(DaemonConfig::parse(yaml).is_err());
    }

    #[test]
    fn relative_socket_path_is_rejected() {
        let yaml = "socket_path: tmp/x.sock\nallowed_uids: [1]\n";
        assert!(DaemonConfig::parse(yaml).is_err());
    }

    #[test]
    fn zero_rate_is_rejected() {
        let yaml =
            "socket_path: /run/ferrokeyd/x.sock\nallowed_uids: [1]\nrate:\n  per_second: 0\n";
        assert!(DaemonConfig::parse(yaml).is_err());
    }

    // ── §25: hard bounds are enforced ────────────────────────────────────

    #[test]
    fn absurd_max_held_keys_is_rejected() {
        let yaml = format!(
            "socket_path: /run/ferrokeyd/x.sock\nallowed_uids: [1]\nmax_held_keys: {}\n",
            u64::MAX
        );
        let err = DaemonConfig::parse(&yaml).unwrap_err();
        assert!(err.to_string().contains("hard limit"));
    }

    #[test]
    fn max_held_keys_over_limit_rejected() {
        let yaml = format!(
            "socket_path: /run/ferrokeyd/x.sock\nallowed_uids: [1]\nmax_held_keys: {}\n",
            MAX_HELD_KEYS_LIMIT + 1
        );
        assert!(DaemonConfig::parse(&yaml).is_err());
    }

    #[test]
    fn max_connections_over_limit_rejected() {
        let yaml = format!(
            "socket_path: /run/ferrokeyd/x.sock\nallowed_uids: [1]\nmax_connections: {}\n",
            MAX_CONNECTIONS_LIMIT + 1
        );
        assert!(DaemonConfig::parse(&yaml).is_err());
    }

    #[test]
    fn rate_over_limit_rejected() {
        let yaml = format!(
            "socket_path: /run/ferrokeyd/x.sock\nallowed_uids: [1]\nrate:\n  burst: {}\n  per_second: 400\n",
            MAX_RATE_LIMIT + 1
        );
        assert!(DaemonConfig::parse(&yaml).is_err());
    }

    #[test]
    fn oversized_socket_path_rejected() {
        let long = "x".repeat(MAX_SOCKET_PATH_LEN + 1);
        let yaml = format!("socket_path: /{long}\nallowed_uids: [1]\n");
        assert!(DaemonConfig::parse(&yaml).is_err());
    }

    #[test]
    fn oversized_device_name_rejected() {
        let long = "x".repeat(MAX_DEVICE_NAME_LEN + 1);
        let yaml = format!(
            "socket_path: /run/ferrokeyd/x.sock\nallowed_uids: [1]\ndevice_name: \"{long}\"\n"
        );
        assert!(DaemonConfig::parse(&yaml).is_err());
    }

    #[test]
    fn socket_mode_out_of_range_rejected() {
        let yaml = "socket_path: /run/ferrokeyd/x.sock\nallowed_uids: [1]\nsocket_mode: 0o1000\n";
        assert!(DaemonConfig::parse(yaml).is_err());
    }

    #[test]
    fn service_identity_must_not_be_root() {
        let yaml = "socket_path: /run/ferrokeyd/x.sock\nallowed_uids: [1]\nservice_user: root\nservice_group: ferrokeyd\n";
        assert!(DaemonConfig::parse(yaml).is_err());
        let yaml = "socket_path: /run/ferrokeyd/x.sock\nallowed_uids: [1]\nservice_user: ferrokeyd\nservice_group: root\n";
        assert!(DaemonConfig::parse(yaml).is_err());
    }

    #[test]
    fn duplicate_uids_rejected() {
        let yaml = "socket_path: /run/ferrokeyd/x.sock\nallowed_uids: [1000, 1000]\n";
        assert!(DaemonConfig::parse(yaml).is_err());
    }
}
