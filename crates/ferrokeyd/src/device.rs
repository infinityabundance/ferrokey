//! The daemon's keyboard device abstraction.
//!
//! The broker owns the single pre-created `/dev/uinput` device (transferred
//! by the bootstrap component). The [`KeyDevice`] trait exists so the
//! session/protocol logic (auth, state machine, rate limiting, recovery) can
//! be unit-tested without a kernel — the real implementation wraps
//! [`ferrokey_uinput::UinputDevice`], which enforces the capability set and
//! the authoritative held-key ledger (§20, §22).

use ferrokey_core::PhysicalKey;
use ferrokey_uinput::UinputDevice;

/// Errors from device operations, mapped onto protocol error codes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeviceError {
    #[error("key code {0} is not in the explicit capability set")]
    UnknownKey(u16),
    #[error("key_up for key {0} without a matching key_down")]
    KeyUpWithoutDown(u16),
    #[error("rollover limit exceeded while pressing {0}")]
    Rollover(u16),
    #[error("key {0} is already held (duplicate down / cross-session ownership)")]
    KeyBusy(u16),
    #[error("device I/O failed: {0}")]
    Io(String),
}

/// A keyboard the broker can command — a deliberately *closed* operation set
/// (§18): only key presses, releases, autorepeats and release-all. No generic
/// `emit(event_type, code, value)` is exposed to protocol data.
pub trait KeyDevice {
    fn key_down(&mut self, key: PhysicalKey) -> Result<(), DeviceError>;
    fn key_up(&mut self, key: PhysicalKey) -> Result<(), DeviceError>;
    /// Autorepeat a held key (`EV_KEY` value=2; not a state transition).
    fn key_repeat(&mut self, key: PhysicalKey) -> Result<(), DeviceError>;
    /// Release exactly `codes`, fail-safe (§23): every key is attempted even
    /// if earlier releases fail; all errors are returned.
    fn release_keys(&mut self, codes: &[u16]) -> Vec<DeviceError>;
    /// Release every key the device ledger holds, fail-safe.
    fn release_all(&mut self) -> Vec<DeviceError>;
    /// The linux key codes this device supports (immutable capability set).
    fn capability_codes(&self) -> &[u16];
    /// Whether the code belongs to the immutable capability set (§21).
    fn is_capable(&self, code: u16) -> bool;
    /// Whether the device ledger currently holds `code` (§22).
    fn is_held(&self, code: u16) -> bool;
    /// Number of keys currently held.
    fn held_count(&self) -> usize;
}

fn map_sink(code: u16, e: &ferrokey_uinput::SinkError) -> DeviceError {
    let msg = e.0.as_str();
    if msg.contains("KeyUpWithoutDown") {
        DeviceError::KeyUpWithoutDown(code)
    } else if msg.contains("Rollover") {
        DeviceError::Rollover(code)
    } else if msg.contains("KeyBusy") {
        DeviceError::KeyBusy(code)
    } else if msg.contains("capability") {
        DeviceError::UnknownKey(code)
    } else {
        DeviceError::Io(e.0.clone())
    }
}

/// The real uinput-backed device (adopted from the bootstrap component).
pub struct RealDevice(pub UinputDevice);

impl KeyDevice for RealDevice {
    fn key_down(&mut self, key: PhysicalKey) -> Result<(), DeviceError> {
        let code = u16::try_from(key.linux_code()).unwrap_or(0);
        self.0.key_down(key).map_err(|e| map_sink(code, &e))
    }

    fn key_up(&mut self, key: PhysicalKey) -> Result<(), DeviceError> {
        let code = u16::try_from(key.linux_code()).unwrap_or(0);
        self.0.key_up(key).map_err(|e| map_sink(code, &e))
    }

    fn key_repeat(&mut self, key: PhysicalKey) -> Result<(), DeviceError> {
        let code = u16::try_from(key.linux_code()).unwrap_or(0);
        self.0.key_repeat(key).map_err(|e| map_sink(code, &e))
    }

    fn release_keys(&mut self, codes: &[u16]) -> Vec<DeviceError> {
        self.0
            .release_keys(codes)
            .into_iter()
            .map(|e| {
                let code = codes.first().copied().unwrap_or(0);
                map_sink(code, &e)
            })
            .collect()
    }

    fn release_all(&mut self) -> Vec<DeviceError> {
        self.0
            .release_all()
            .into_iter()
            .map(|e| DeviceError::Io(e.0.clone()))
            .collect()
    }

    fn capability_codes(&self) -> &[u16] {
        self.0.capability_codes()
    }

    fn is_capable(&self, code: u16) -> bool {
        ferrokey_uinput::is_capable(code)
    }

    fn is_held(&self, code: u16) -> bool {
        self.0.is_held(code)
    }

    fn held_count(&self) -> usize {
        self.0.held_count()
    }
}

/// A recording device used by the unit courts: no kernel involved.
///
/// Mirrors the real ledger semantics: duplicate downs are rejected, ups
/// without downs are rejected, rollover is capped.
#[derive(Debug, Default)]
pub struct MockKeyDevice {
    /// (code, value) event log — 1 down, 0 up, 2 repeat.
    pub events: Vec<(u16, i32)>,
    pub released_all: u32,
    pub capabilities: Vec<u16>,
    pub max_held: usize,
}

impl MockKeyDevice {
    pub fn new() -> Self {
        MockKeyDevice {
            capabilities: ferrokey_uinput::capability_codes(),
            max_held: 16,
            ..Default::default()
        }
    }

    /// The latest event for this code decides its state (like the ledger).
    /// Repeat events (value 2) do NOT change the held state — mirror the
    /// real ledger (§22): a repeat after a down keeps the key held.
    pub fn is_held(&self, code: u16) -> bool {
        self.events
            .iter()
            .rev()
            .find(|(c, v)| *c == code && *v != 2)
            .map(|(_, v)| *v == 1)
            .unwrap_or(false)
    }
}

impl KeyDevice for MockKeyDevice {
    fn key_down(&mut self, key: PhysicalKey) -> Result<(), DeviceError> {
        let code = u16::try_from(key.linux_code()).unwrap_or(0);
        if !self.is_capable(code) {
            return Err(DeviceError::UnknownKey(code));
        }
        if self.is_held(code) {
            return Err(DeviceError::KeyBusy(code));
        }
        if self.held_count() >= self.max_held {
            return Err(DeviceError::Rollover(code));
        }
        self.events.push((code, 1));
        Ok(())
    }

    fn key_up(&mut self, key: PhysicalKey) -> Result<(), DeviceError> {
        let code = u16::try_from(key.linux_code()).unwrap_or(0);
        if !self.is_capable(code) {
            return Err(DeviceError::UnknownKey(code));
        }
        if !self.is_held(code) {
            return Err(DeviceError::KeyUpWithoutDown(code));
        }
        self.events.push((code, 0));
        Ok(())
    }

    fn key_repeat(&mut self, key: PhysicalKey) -> Result<(), DeviceError> {
        let code = u16::try_from(key.linux_code()).unwrap_or(0);
        if !self.is_held(code) {
            return Err(DeviceError::KeyUpWithoutDown(code));
        }
        self.events.push((code, 2));
        Ok(())
    }

    fn release_keys(&mut self, codes: &[u16]) -> Vec<DeviceError> {
        let mut errors = Vec::new();
        for &code in codes {
            if !self.is_held(code) {
                errors.push(DeviceError::KeyUpWithoutDown(code));
                continue;
            }
            self.events.push((code, 0));
        }
        errors
    }

    fn release_all(&mut self) -> Vec<DeviceError> {
        self.released_all += 1;
        let held: Vec<u16> = self
            .events
            .iter()
            .rev()
            .filter(|(_, v)| *v == 1)
            .map(|(c, _)| *c)
            .collect();
        self.release_keys(&held)
    }

    fn capability_codes(&self) -> &[u16] {
        &self.capabilities
    }

    fn is_capable(&self, code: u16) -> bool {
        ferrokey_uinput::is_capable(code)
    }

    fn is_held(&self, code: u16) -> bool {
        self.is_held(code)
    }

    fn held_count(&self) -> usize {
        self.events
            .iter()
            .filter(|(_, v)| *v == 1)
            .map(|(c, _)| *c)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_tracks_held_state() {
        let mut kb = MockKeyDevice::new();
        kb.key_down(PhysicalKey::A).unwrap();
        assert!(kb.is_held(30));
        kb.key_up(PhysicalKey::A).unwrap();
        assert!(!kb.is_held(30));
        assert!(kb.key_up(PhysicalKey::A).is_err()); // up without down
    }

    #[test]
    fn mock_rejects_duplicate_down() {
        let mut kb = MockKeyDevice::new();
        kb.key_down(PhysicalKey::A).unwrap();
        assert_eq!(kb.key_down(PhysicalKey::A), Err(DeviceError::KeyBusy(30)));
    }

    #[test]
    fn mock_release_all_clears() {
        let mut kb = MockKeyDevice::new();
        kb.key_down(PhysicalKey::A).unwrap();
        kb.key_down(PhysicalKey::B).unwrap();
        assert!(kb.release_all().is_empty());
        assert!(!kb.is_held(30) && !kb.is_held(48));
        assert_eq!(kb.released_all, 1);
    }

    #[test]
    fn capability_codes_are_dense() {
        let kb = MockKeyDevice::new();
        let unique: std::collections::BTreeSet<u16> = kb.capabilities.iter().copied().collect();
        assert_eq!(unique.len(), kb.capabilities.len());
    }
}
