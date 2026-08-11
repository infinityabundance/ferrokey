//! The virtual keyboard device.
//!
//! `VirtualKeyboard` owns an `evdev::uinput::VirtualDevice` and implements
//! [`ferrokey_core::action::KeySink`] so the core state machine can drive it
//! directly. All event emission funnels through [`crate::emit`], which relies
//! on `VirtualDevice::emit()`'s automatic `SYN_REPORT` appending.

use crate::capabilities::{capability_keys, to_evdev_key};
use crate::emit;
use crate::ledger::HeldLedger;
use evdev::uinput::VirtualDevice;
use evdev::{BusType, InputId, KeyCode};
use ferrokey_core::{KeySink, PhysicalKey, SinkError, CAPABILITY_SET};
use std::io;

/// Kernel identifiers for the virtual device.
pub const VENDOR_ID: u16 = 0xFE20; // Ferrokey
pub const PRODUCT_ID: u16 = 0xFE21;
pub const VERSION_ID: u16 = 0x0001;
pub const BUS: BusType = BusType(0x03); // BUS_VIRTUAL
pub const DEVICE_NAME: &str = "Ferrokey Virtual Keyboard";

/// Device creation parameters.
#[derive(Debug, Clone)]
pub struct DeviceOptions {
    /// Device name reported to the kernel (visible in `/proc/bus/input/devices`).
    pub name: String,
    /// Maximum simultaneously-held keys enforced by the ledger.
    pub max_held_keys: usize,
    /// Optional `phys` string (e.g. a stable identifier for udev matching).
    pub phys: Option<String>,
}

impl Default for DeviceOptions {
    fn default() -> Self {
        DeviceOptions {
            name: DEVICE_NAME.to_string(),
            max_held_keys: crate::ledger::DEFAULT_MAX_HELD_KEYS,
            phys: Some("ferrokey/virtual".to_string()),
        }
    }
}

/// A registered virtual keyboard.
pub struct VirtualKeyboard {
    device: VirtualDevice,
    pub ledger: HeldLedger,
    options: DeviceOptions,
}

impl VirtualKeyboard {
    /// Open `/dev/uinput` and register the virtual keyboard with the explicit
    /// capability set.
    pub fn create(options: DeviceOptions) -> Result<Self, UinputError> {
        let keys = capability_keys();
        let mut builder = VirtualDevice::builder().map_err(UinputError::Open)?;
        builder = builder
            .name(&options.name)
            .input_id(InputId::new(BUS, VENDOR_ID, PRODUCT_ID, VERSION_ID));
        if let Some(phys) = &options.phys {
            // `with_phys` requires a CStr; constructing it from a known-safe
            // byte string. phys strings with interior NULs are invalid.
            let c =
                std::ffi::CString::new(phys.as_bytes()).map_err(|_| UinputError::InvalidPhys)?;
            builder = builder.with_phys(&c).map_err(UinputError::Build)?;
        }
        let mut key_set = evdev::AttributeSet::new();
        for key in &keys {
            key_set.insert(*key);
        }
        builder = builder.with_keys(&key_set).map_err(UinputError::Build)?;

        let device = builder.build().map_err(UinputError::Build)?;
        Ok(VirtualKeyboard {
            device,
            ledger: HeldLedger::new(options.max_held_keys),
            options,
        })
    }

    /// The sysfs path of the created device (`/sys/devices/virtual/input/...`).
    pub fn syspath(&mut self) -> Result<std::path::PathBuf, UinputError> {
        self.device.get_syspath().map_err(UinputError::Syspath)
    }

    /// Enumerate the `/dev/input/event*` nodes for this device.
    pub fn dev_nodes(&mut self) -> Result<Vec<std::path::PathBuf>, UinputError> {
        let mut nodes = self
            .device
            .enumerate_dev_nodes_blocking()
            .map_err(UinputError::DevNodes)?;
        let mut out = Vec::new();
        while let Some(res) = nodes.next() {
            let path = res.map_err(UinputError::DevNodes)?;
            out.push(path);
        }
        Ok(out)
    }

    pub fn name(&self) -> &str {
        &self.options.name
    }

    /// The raw evdev key code for a Ferrokey physical key.
    pub fn key_code(key: PhysicalKey) -> KeyCode {
        to_evdev_key(key)
    }
}

impl KeySink for VirtualKeyboard {
    fn key_down(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        let code = to_evdev_key(key);
        self.ledger
            .key_down(u32::from(code.code()))
            .map_err(|e| SinkError(e.to_string()))?;
        emit::emit_key_down(&mut self.device, code).map_err(|e| SinkError(e.to_string()))
    }

    fn key_up(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        let code = to_evdev_key(key);
        self.ledger
            .key_up(u32::from(code.code()))
            .map_err(|e| SinkError(e.to_string()))?;
        emit::emit_key_up(&mut self.device, code).map_err(|e| SinkError(e.to_string()))
    }

    fn release_all(&mut self) -> Result<(), SinkError> {
        let codes = self.ledger.drain();
        let keys: Vec<KeyCode> = codes.iter().map(|c| KeyCode::new(*c as u16)).collect();
        emit::emit_release_many(&mut self.device, keys.into_iter())
            .map_err(|e| SinkError(e.to_string()))
    }
}

/// Errors from device creation and operation.
#[derive(Debug, thiserror::Error)]
pub enum UinputError {
    #[error("cannot open /dev/uinput: {0}")]
    Open(io::Error),
    #[error("cannot configure/register the virtual device: {0}")]
    Build(io::Error),
    #[error("invalid device phys string")]
    InvalidPhys,
    #[error("cannot read device syspath: {0}")]
    Syspath(io::Error),
    #[error("cannot enumerate device nodes: {0}")]
    DevNodes(io::Error),
}

/// The size of the explicit capability set (used by tests and courts).
pub fn capability_count() -> usize {
    CAPABILITY_SET.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_defaults_are_sane() {
        let opts = DeviceOptions::default();
        assert_eq!(opts.name, DEVICE_NAME);
        assert!(opts.max_held_keys > 0);
    }

    #[test]
    fn key_code_mapping_matches_capability() {
        for &key in CAPABILITY_SET {
            let code = VirtualKeyboard::key_code(key);
            assert_eq!(u32::from(code.code()), key.linux_code());
        }
    }
}
