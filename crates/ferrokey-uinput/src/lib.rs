//! # ferrokey-uinput
//!
//! The Linux `/dev/uinput` backend: creating the virtual keyboard device once
//! (bootstrap), verifying it, and emitting validated `EV_KEY` events through
//! the pre-created fd.
//!
//! # Phase 3 security architecture
//!
//! * [`device::UinputDevice::create`] — used **only** by the tiny bootstrap
//!   component (`ferrokeyd init`), before any hostile input is accepted
//!   (§8, §15, §16). It performs the configuration ioctls.
//! * [`device::UinputDevice::adopt`] — used by the runtime broker
//!   (`ferrokeyd serve`) to wrap the fd transferred via SCM_RIGHTS, after
//!   verifying device identity and the immutable capability set (§10, §13).
//! * [`emit`] — the runtime event path: pure safe `write(2)` of `EV_KEY`
//!   batches. No `ioctl` is reachable at runtime (§14, §19, §61).
//! * [`ledger::HeldLedger`] — the authoritative held-key ledger (§22).
//!
//! # Unsafe discipline (§82)
//!
//! All `unsafe` code lives in [`ffi`], an isolated, fully-documented module
//! containing only the uinput configuration ioctls.

#![deny(unsafe_code)]

pub mod capabilities;
pub mod device;
pub mod emit;
#[allow(unsafe_code)] // §82: every unsafe block in the crate lives here, documented
pub mod ffi;
pub mod ledger;

pub use capabilities::{capability_codes, capability_count, is_capable, to_linux_code};
pub use device::{
    DeviceIdentity, DeviceOptions, SinkError, UinputDevice, UinputError, BUS_VIRTUAL, DEVICE_NAME,
    PRODUCT_ID, VENDOR_ID, VERSION_ID,
};
pub use ledger::{HeldLedger, LedgerError, DEFAULT_MAX_HELD_KEYS, MAX_HELD_KEYS_LIMIT};
