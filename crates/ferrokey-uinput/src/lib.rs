//! # ferrokey-uinput
//!
//! The Linux `/dev/uinput` backend: creating and owning the virtual keyboard
//! device, emitting `EV_KEY` events, and keeping a defensive held-key ledger
//! so Ferrokey can never leave a stuck modifier behind.
//!
//! The device itself is owned by **`ferrokeyd`**, never by the GUI — the UI
//! is fully unprivileged and talks to the daemon over a Unix socket.

#![forbid(unsafe_code)]

pub mod capabilities;
pub mod device;
pub mod emit;
pub mod ledger;

pub use capabilities::{capability_codes, capability_keys, is_capable, to_evdev_key};
pub use device::{DeviceOptions, VirtualKeyboard, DEVICE_NAME};
pub use ledger::{HeldLedger, LedgerError, DEFAULT_MAX_HELD_KEYS};
