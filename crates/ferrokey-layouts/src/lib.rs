//! # ferrokey-layouts
//!
//! Keyboard layout data and loaders.
//!
//! Layouts are **data files** (`layouts/*.yaml`) — never `.slint` code and
//! never hard-coded `KEY_X == 'q'` assumptions. Each layout maps a
//! [`ferrokey_core::key::PhysicalKey`] to its [`KeyDefinition`]: primary /
//! shifted / altgr / shift+altgr symbols, an optional Fn-layer symbol and a
//! repeat policy. `ferrokey-core` decides *what the key means under the
//! active modifier state*; this crate decides *what symbols exist*.

#![forbid(unsafe_code)]

pub mod builtin;
pub mod xkb;

pub use builtin::{
    builtin, builtin_index, load_from_path, parse_layout, validate_layout, LayoutError, BUILTIN_IDS,
};
pub use xkb::{find_key, levels_for, Levels};
