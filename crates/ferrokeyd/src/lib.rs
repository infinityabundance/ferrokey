//! # ferrokeyd
//!
//! The Ferrokey constrained broker.
//!
//! # Phase 3 architecture (kernel attack-surface hardening)
//!
//! ```text
//! UNTRUSTED UI
//!      │
//!      │ tiny authenticated bounded protocol (FK01 v2, AF_UNIX, SO_PEERCRED)
//!      ▼
//! FERROKEYD RUNTIME BROKER  (ferrokeyd serve)
//!      │  non-root, zero capabilities, NO_NEW_PRIVS, seccomp allowlist
//!      │  no network, no arbitrary open(), no runtime ioctl
//!      │  one pre-created uinput keyboard (verified identity + capability set)
//!      ▼
//! write(input_event)  ──▶  Linux uinput  ──▶  Linux input subsystem
//! ```
//!
//! The untrusted UI never touches `/dev/uinput`. Kernel device creation is
//! the responsibility of the tiny bootstrap component (`ferrokeyd init`),
//! which configures exactly one keyboard *before* any hostile input is
//! accepted and transfers the fd to the runtime via SCM_RIGHTS (§8, §10,
//! §15, §16).
//!
//! Security properties (proved by the VM kernel-security courts):
//!
//! * runtime euid != 0 (§3, §7) — the supervisor pre-drops the service
//!   identity; `serve` refuses to run as root without `--allow-root`
//! * capabilities are empty at runtime (§5)
//! * `PR_SET_NO_NEW_PRIVS` is active (§6)
//! * a strict architecture-aware seccomp allowlist is enforced — `ioctl`,
//!   socket families, arbitrary opens and high-risk subsystems are denied
//!   at the kernel level (§14, §31-§35)
//! * the uinput capability bitmap is immutable and matches the explicit
//!   Ferrokey capability set (§13, §21)
//! * the held-key ledger is authoritative; disconnect and crash release
//!   exactly the affected keys (§12, §22, §74)
//! * protocol frames are tiny and bounded; hostile input is rejected (§51,
//!   §52) and continuously fuzzed (§53)

#![deny(unsafe_code)]

pub mod bootstrap;
pub mod config;
pub mod device;
#[allow(unsafe_code)] // §82: fd passing (SCM_RIGHTS) — documented pre/postconditions
pub mod fds;
pub mod init;
pub mod phase;
pub mod rate_limit;
#[allow(unsafe_code)] // §82: seccomp BPF install + enforcement probes — documented
pub mod sandbox;
#[allow(unsafe_code)] // §82: capability/credential manipulation — documented
pub mod security;
pub mod serve;
pub mod session;
#[allow(unsafe_code)] // §82: async-signal-safe signal handling — documented
pub mod signals;
pub mod socket_path;

pub use config::{ConfigError, DaemonConfig, RateConfig};
pub use device::{DeviceError, KeyDevice, MockKeyDevice, RealDevice};
pub use phase::{BrokerPhase, PhaseError, PhaseGuard};
pub use rate_limit::TokenBucket;
pub use serve::{ServeArgs, ServeError};
pub use session::{ClientSession, Outcome};

// Re-export the sandbox unsafe shims for the isolated unsafe modules.
#[doc(hidden)]
pub use sandbox::raw_syscall;
