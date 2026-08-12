//! Ferrokey — the on-screen keyboard that preserves target focus via
//! kernel-level input injection.
//!
//! This is the umbrella crate for the Ferrokey workspace. It re-exports the
//! reusable crates under short, stable module names so an application can
//! pull in the whole stack with a single dependency:
//!
//! ```
//! use ferrokey::{core::PhysicalKey, uinput::VirtualKeyboard};
//! ```
//!
//! The umbrella contains no code of its own; each module is the
//! corresponding workspace crate:
//!
//! * [`core`](crate::core) — keyboard state machine: modifiers, sticky/locked
//!   keys, repeat, layouts, actions.
//! * [`layouts`](crate::layouts) — layout data files and loaders.
//! * [`protocol`](crate::protocol) — the binary wire protocol between the UI
//!   and the daemon.
//! * [`surface`](crate::surface) — window-system integration (Wayland/X11)
//!   and the custom Slint platform adapter.
//! * [`uinput`](crate::uinput) — the uinput virtual keyboard with its
//!   defensive held-key ledger.

pub use ferrokey_core as core;
pub use ferrokey_layouts as layouts;
pub use ferrokey_protocol as protocol;
pub use ferrokey_surface as surface;
pub use ferrokey_uinput as uinput;
