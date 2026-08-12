//! The daemon's keyboard device abstraction.
//!
//! `ferrokeyd` owns the only real `/dev/uinput` device in the system. The
//! `KeyboardDevice` trait exists so the server logic (auth, protocol, rate
//! limiting, recovery) can be unit-tested without a kernel.

use ferrokey_core::{KeySink, PhysicalKey};
use ferrokey_uinput::{DeviceOptions, VirtualKeyboard};

/// A keyboard the daemon can command.
pub trait KeyboardDevice: std::any::Any {
    /// Create the device (idempotent; called once per connection).
    fn create(&mut self) -> Result<(), DeviceError>;
    fn key_down(&mut self, code: u16) -> Result<(), DeviceError>;
    fn key_up(&mut self, code: u16) -> Result<(), DeviceError>;
    /// Autorepeat a held key (`EV_KEY` value=2; not a state transition).
    fn key_repeat(&mut self, code: u16) -> Result<(), DeviceError>;
    /// Release every held key (disconnect / crash recovery).
    fn release_all(&mut self) -> Result<(), DeviceError>;
    /// The linux key codes this device supports.
    fn capability_codes(&self) -> &[u32];
    /// Downcast helper for tests.
    fn as_any(&self) -> &dyn std::any::Any {
        // Deliberately not implemented by default: only test doubles need it.
        panic!("as_any is only implemented on concrete types")
    }
}

/// Errors from device operations, mapped onto protocol error codes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeviceError {
    #[error("key code {0} is not in the explicit capability set")]
    UnknownKey(u16),
    #[error("key_up for key {0} without a matching key_down")]
    KeyUpWithoutDown(u16),
    #[error("rollover limit exceeded while pressing {0}")]
    Rollover(u16),
    #[error("device creation failed: {0}")]
    Create(String),
    #[error("device I/O failed: {0}")]
    Io(String),
}

/// The real uinput-backed device.
pub struct UinputKeyboard {
    inner: Option<VirtualKeyboard>,
    options: DeviceOptions,
    capabilities: Vec<u32>,
}

impl UinputKeyboard {
    pub fn new(device_name: &str, max_held_keys: usize) -> Self {
        UinputKeyboard {
            inner: None,
            options: DeviceOptions {
                name: device_name.to_string(),
                max_held_keys,
            },
            capabilities: ferrokey_uinput::capability_codes(),
        }
    }
}

impl KeyboardDevice for UinputKeyboard {
    fn create(&mut self) -> Result<(), DeviceError> {
        if self.inner.is_some() {
            return Ok(());
        }
        let keyboard = VirtualKeyboard::create(self.options.clone())
            .map_err(|e| DeviceError::Create(e.to_string()))?;
        log::info!("created uinput device {:?}", keyboard.name());
        self.inner = Some(keyboard);
        Ok(())
    }

    fn key_down(&mut self, code: u16) -> Result<(), DeviceError> {
        let key =
            PhysicalKey::from_linux_code(u32::from(code)).ok_or(DeviceError::UnknownKey(code))?;
        let dev = self
            .inner
            .as_mut()
            .ok_or_else(|| DeviceError::Create("device not created".into()))?;
        dev.key_down(key)
            .map_err(|e| classify_sink_error(code, e.to_string()))
    }

    fn key_up(&mut self, code: u16) -> Result<(), DeviceError> {
        let key =
            PhysicalKey::from_linux_code(u32::from(code)).ok_or(DeviceError::UnknownKey(code))?;
        let dev = self
            .inner
            .as_mut()
            .ok_or_else(|| DeviceError::Create("device not created".into()))?;
        dev.key_up(key)
            .map_err(|e| classify_sink_error(code, e.to_string()))
    }

    fn key_repeat(&mut self, code: u16) -> Result<(), DeviceError> {
        let key =
            PhysicalKey::from_linux_code(u32::from(code)).ok_or(DeviceError::UnknownKey(code))?;
        let dev = self
            .inner
            .as_mut()
            .ok_or_else(|| DeviceError::Create("device not created".into()))?;
        dev.key_repeat(key)
            .map_err(|e| classify_sink_error(code, e.to_string()))
    }

    fn release_all(&mut self) -> Result<(), DeviceError> {
        if let Some(dev) = self.inner.as_mut() {
            log::info!("release_all: releasing held keys");
            dev.release_all()
                .map_err(|e| DeviceError::Io(e.to_string()))?;
            log::info!("release_all: done");
        }
        Ok(())
    }

    fn capability_codes(&self) -> &[u32] {
        &self.capabilities
    }
}

fn classify_sink_error(code: u16, message: String) -> DeviceError {
    if message.contains("KeyUpWithoutDown") {
        DeviceError::KeyUpWithoutDown(code)
    } else if message.contains("Rollover") {
        DeviceError::Rollover(code)
    } else if message.contains("NotCapable") {
        DeviceError::UnknownKey(code)
    } else {
        DeviceError::Io(message)
    }
}

/// A recording device used by the unit courts: no kernel involved.
#[derive(Debug, Default)]
pub struct MockKeyboard {
    pub events: Vec<(bool, u16)>, // (is_down, code)
    pub created: bool,
    pub released_all: u32,
    pub capabilities: Vec<u32>,
}

impl MockKeyboard {
    pub fn new() -> Self {
        MockKeyboard {
            capabilities: ferrokey_uinput::capability_codes(),
            ..Default::default()
        }
    }

    pub fn is_held(&self, code: u16) -> bool {
        // The latest event for this code decides its state.
        self.events
            .iter()
            .rev()
            .find(|(_, c)| *c == code)
            .map(|(down, _)| *down)
            .unwrap_or(false)
    }
}

impl KeyboardDevice for MockKeyboard {
    fn create(&mut self) -> Result<(), DeviceError> {
        self.created = true;
        Ok(())
    }

    fn key_down(&mut self, code: u16) -> Result<(), DeviceError> {
        self.events.push((true, code));
        Ok(())
    }

    fn key_up(&mut self, code: u16) -> Result<(), DeviceError> {
        if !self.is_held(code) {
            return Err(DeviceError::KeyUpWithoutDown(code));
        }
        self.events.push((false, code));
        Ok(())
    }

    fn key_repeat(&mut self, code: u16) -> Result<(), DeviceError> {
        // Autorepeat is not a state transition: require the key to be held.
        if !self.is_held(code) {
            return Err(DeviceError::KeyUpWithoutDown(code));
        }
        self.events.push((true, code));
        Ok(())
    }

    fn release_all(&mut self) -> Result<(), DeviceError> {
        self.released_all += 1;
        self.events.retain(|(down, _)| !down);
        Ok(())
    }

    fn capability_codes(&self) -> &[u32] {
        &self.capabilities
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_tracks_held_state() {
        let mut kb = MockKeyboard::new();
        kb.key_down(30).unwrap();
        assert!(kb.is_held(30));
        kb.key_up(30).unwrap();
        assert!(!kb.is_held(30));
        assert!(kb.key_up(30).is_err()); // key_up without down
    }

    #[test]
    fn mock_release_all_clears() {
        let mut kb = MockKeyboard::new();
        kb.key_down(30).unwrap();
        kb.key_down(42).unwrap();
        kb.release_all().unwrap();
        assert!(kb.events.is_empty());
        assert_eq!(kb.released_all, 1);
    }

    #[test]
    fn capability_codes_are_dense() {
        let kb = MockKeyboard::new();
        let unique: std::collections::BTreeSet<u32> = kb.capabilities.iter().copied().collect();
        assert_eq!(unique.len(), kb.capabilities.len());
    }
}
