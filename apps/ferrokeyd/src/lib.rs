//! # ferrokeyd
//!
//! The privileged Ferrokey broker.
//!
//! `ferrokeyd` owns `/dev/uinput`; the desktop UI is fully unprivileged and
//! talks to the daemon over a Unix socket authenticated with `SO_PEERCRED`.
//! The daemon deliberately contains **no Slint, no image decoding, no layout
//! parsers, no networking, no plugins and no scripting** — it is a small,
//! auditable security boundary.
//!
//! Recovery contract:
//!
//! * client disconnect → `release_all` + device close (kernel releases keys)
//! * daemon SIGKILL → kernel releases all keys on device close
//! * malformed protocol → connection torn down
//! * rate limits → floods rejected
//! * unknown key codes → rejected against the explicit capability set

#![forbid(unsafe_code)]

pub mod config;
pub mod device;
pub mod rate_limit;
pub mod server;

pub use config::{ConfigError, DaemonConfig, RateConfig};
pub use device::{DeviceError, KeyboardDevice, MockKeyboard, UinputKeyboard};
pub use rate_limit::TokenBucket;
pub use server::{ConnectionError, Server, ServerError};
