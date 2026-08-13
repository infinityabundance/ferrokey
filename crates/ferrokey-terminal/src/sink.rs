//! Input sinks (§50–§51): how keyboard semantics become PTY bytes.
//!
//! The system-mode sink (uinput) and the terminal-mode sink are kept
//! strictly separate. [`TerminalKeySink`] implements
//! `ferrokey_core::KeySink` — the *same* trait the uinput path uses — so the
//! identical ferrokey-core state machine drives both destinations with no
//! duplicated modifier/sticky/repeat logic (§2, §49, §53). The sink observes
//! physical modifier key transitions and encodes every other key with the
//! current modifier set through the [`crate::key_encoder::TerminalKeyEncoder`].

use crate::key_encoder::TerminalKeyEncoder;
use crate::modes::TerminalModes;
use crate::terminal::TerminalError;
use ferrokey_core::{KeySink, ModifierSet, PhysicalKey, SinkError};
use std::cell::RefCell;
use std::rc::Rc;

/// Anything that can receive encoded terminal input.
pub trait TerminalInputSink {
    fn send(&mut self, encoded: &[u8]) -> Result<(), TerminalSinkError>;
}

/// Errors from the terminal input path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TerminalSinkError {
    #[error("terminal write failed: {0}")]
    Io(String),
    #[error("terminal is not running")]
    NotRunning,
}

impl From<nix::errno::Errno> for TerminalSinkError {
    fn from(e: nix::errno::Errno) -> Self {
        TerminalSinkError::Io(e.to_string())
    }
}

/// A [`TerminalInputSink`] that writes bytes to a PTY master fd.
///
/// The fd is borrowed (the [`crate::terminal::Terminal`] owns the PTY pair);
/// writes are buffered and flushed by the terminal's event loop so the sink
/// stays cheap and the PTY never blocks the key path.
#[derive(Debug, Clone)]
pub struct PtySink {
    write: Rc<RefCell<Vec<u8>>>,
}

impl PtySink {
    pub fn new() -> Self {
        PtySink {
            write: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// The shared write buffer the terminal drains into the PTY master.
    pub fn buffer(&self) -> Rc<RefCell<Vec<u8>>> {
        self.write.clone()
    }
}

impl Default for PtySink {
    fn default() -> Self {
        PtySink::new()
    }
}

impl TerminalInputSink for PtySink {
    fn send(&mut self, encoded: &[u8]) -> Result<(), TerminalSinkError> {
        // Bounded queue: refuse beyond a hard cap instead of growing without
        // limit (an attacker holding keys cannot balloon memory).
        const MAX_PENDING: usize = 1 << 20;
        let mut buf = self.write.borrow_mut();
        if buf.len() + encoded.len() > MAX_PENDING {
            return Err(TerminalSinkError::Io(
                "pending terminal input buffer full".into(),
            ));
        }
        buf.extend_from_slice(encoded);
        Ok(())
    }
}

/// A [`ferrokey_core::KeySink`] that encodes physical keys into terminal
/// bytes and forwards them to a [`TerminalInputSink`].
///
/// The encoder needs the *current* terminal modes (application cursor keys
/// etc.), so it borrows the terminal's mode state through a shared cell.
pub struct TerminalKeySink {
    encoder: TerminalKeyEncoder,
    modes: Rc<RefCell<TerminalModes>>,
    sink: Box<dyn TerminalInputSink>,
    held_mods: ModifierSet,
    /// Diagnostics counter (the invariant courts assert on:
    /// `terminal_mode_uinput_events == 0` is asserted by the app, not here).
    pub encoded_events: u64,
    pub rejected_events: u64,
}

impl TerminalKeySink {
    pub fn new(
        encoder: TerminalKeyEncoder,
        modes: Rc<RefCell<TerminalModes>>,
        sink: Box<dyn TerminalInputSink>,
    ) -> Self {
        TerminalKeySink {
            encoder,
            modes,
            sink,
            held_mods: ModifierSet::empty(),
            encoded_events: 0,
            rejected_events: 0,
        }
    }

    pub fn held_modifiers(&self) -> ModifierSet {
        self.held_mods
    }

    fn encode_and_send(&mut self, key: PhysicalKey) -> Result<(), TerminalSinkError> {
        let modes = self.modes.borrow().clone();
        if let Some(bytes) = self.encoder.encode(key, self.held_mods, &modes) {
            self.encoded_events = self.encoded_events.wrapping_add(1);
            log::debug!(
                "terminal encode {key:?} mods={:?} -> {} bytes",
                self.held_mods,
                bytes.len()
            );
            self.sink.send(&bytes)
        } else {
            self.rejected_events = self.rejected_events.wrapping_add(1);
            log::debug!("terminal encode {key:?} -> rejected");
            Ok(())
        }
    }
}

impl KeySink for TerminalKeySink {
    fn key_down(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        if let Some(kind) = key.modifier_kind() {
            self.held_mods.insert(kind.into());
            return Ok(());
        }
        self.encode_and_send(key)
            .map_err(|e| SinkError(e.to_string()))
    }

    fn key_up(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        if let Some(kind) = key.modifier_kind() {
            self.held_mods.remove(kind.into());
        }
        // Key releases produce no terminal bytes.
        Ok(())
    }

    fn key_repeat(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        self.key_down(key)
    }

    fn release_all(&mut self) -> Result<(), SinkError> {
        self.held_mods = ModifierSet::empty();
        Ok(())
    }
}

/// Adapt [`TerminalError`] into the sink error vocabulary.
impl From<TerminalError> for TerminalSinkError {
    fn from(e: TerminalError) -> Self {
        match e {
            TerminalError::Io(io) => TerminalSinkError::Io(io.to_string()),
            other => TerminalSinkError::Io(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_encoder::base_ascii;
    use ferrokey_core::Layout;
    use std::sync::Arc;

    struct RecordingSink {
        bytes: Rc<RefCell<Vec<u8>>>,
    }

    impl TerminalInputSink for RecordingSink {
        fn send(&mut self, encoded: &[u8]) -> Result<(), TerminalSinkError> {
            self.bytes.borrow_mut().extend_from_slice(encoded);
            Ok(())
        }
    }

    #[allow(clippy::type_complexity)]
    fn sink() -> (
        TerminalKeySink,
        Rc<RefCell<Vec<u8>>>,
        Rc<RefCell<TerminalModes>>,
    ) {
        let modes = Rc::new(RefCell::new(TerminalModes::default()));
        let bytes = Rc::new(RefCell::new(Vec::new()));
        let encoder = TerminalKeyEncoder::new(Arc::new(Layout::empty("us", "US")));
        let s = TerminalKeySink::new(
            encoder,
            modes.clone(),
            Box::new(RecordingSink {
                bytes: bytes.clone(),
            }),
        );
        (s, bytes, modes)
    }

    #[test]
    fn plain_keys_encode() {
        let (mut s, bytes, _) = sink();
        s.key_down(PhysicalKey::A).unwrap();
        assert_eq!(*bytes.borrow(), b"a");
    }

    #[test]
    fn shift_held_affects_following_keys() {
        let (mut s, bytes, _) = sink();
        s.key_down(PhysicalKey::LeftShift).unwrap();
        s.key_down(PhysicalKey::A).unwrap();
        // With the empty layout, shifted A falls back to base ASCII; the
        // shift *state* is what matters here.
        let got = bytes.borrow().clone();
        assert!(!got.is_empty());
        s.key_up(PhysicalKey::A).unwrap();
        s.key_up(PhysicalKey::LeftShift).unwrap();
        s.key_down(PhysicalKey::A).unwrap();
        let got2 = bytes.borrow().clone();
        assert_eq!(got2.len(), got.len() + 1);
    }

    #[test]
    fn ctrl_c_is_0x03() {
        let (mut s, bytes, _) = sink();
        s.key_down(PhysicalKey::LeftCtrl).unwrap();
        s.key_down(PhysicalKey::C).unwrap();
        assert_eq!(*bytes.borrow(), vec![0x03]);
    }

    #[test]
    fn application_cursor_mode_changes_arrows() {
        let (mut s, bytes, modes) = sink();
        s.key_down(PhysicalKey::Up).unwrap();
        assert_eq!(*bytes.borrow(), b"\x1b[A");
        modes.borrow_mut().application_cursor_keys = true;
        s.key_down(PhysicalKey::Up).unwrap();
        assert_eq!(*bytes.borrow(), b"\x1b[A\x1bOA");
    }

    #[test]
    fn modifiers_tracked_across_release_all() {
        let (mut s, bytes, _) = sink();
        s.key_down(PhysicalKey::LeftCtrl).unwrap();
        s.release_all().unwrap();
        assert!(s.held_modifiers().is_empty());
        s.key_down(PhysicalKey::C).unwrap();
        assert_eq!(*bytes.borrow(), b"c");
    }

    #[test]
    fn release_all_keeps_sink_usable() {
        let (mut s, bytes, _) = sink();
        s.key_down(PhysicalKey::LeftShift).unwrap();
        s.key_down(PhysicalKey::A).unwrap();
        s.release_all().unwrap();
        assert_eq!(*bytes.borrow(), b"a");
        s.key_down(PhysicalKey::B).unwrap();
        assert_eq!(*bytes.borrow(), b"ab");
    }

    #[test]
    fn base_ascii_used_for_letters() {
        assert_eq!(base_ascii(PhysicalKey::Q), Some('q'));
    }
}
