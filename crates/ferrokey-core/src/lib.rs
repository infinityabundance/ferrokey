//! # ferrokey-core
//!
//! The pure, deterministic heart of Ferrokey. No OS dependencies, no input
//! devices, no GUI toolkit: everything here is a function of explicit input
//! and an explicit clock, which is what makes it fully unit-testable.
//!
//! The UI speaks [`action::InputRequest`]s; [`action::KeyboardDriver`]
//! translates them into kernel-level key events via a [`action::KeySink`]
//! (implemented by `ferrokey-uinput`'s device or by the `ferrokey-protocol`
//! client that talks to `ferrokeyd`).

#![forbid(unsafe_code)]

pub mod action;
pub mod compose;
pub mod geometry;
pub mod key;
pub mod keyset;
pub mod layout;
pub mod modifier;
pub mod repeat;
pub mod state;
pub mod time;

pub use action::{
    DriverError, InputRequest, KeyAction, KeySink, KeyboardDriver, SinkError, TextError, TextSink,
    VirtualKey,
};
pub use compose::{ComposeEngine, FeedOutcome};
pub use key::{PhysicalKey, CAPABILITY_SET};
pub use layout::{DeadKey, KeyDefinition, KeySymbol, Layout};
pub use modifier::{ModifierKind, ModifierSet};
pub use repeat::{RepeatEngine, RepeatSettings};
pub use state::{KeyEvent, KeyboardState, Layer, StateError, StateSettings, TAP_GRACE};
pub use time::Moment;
