//! The virtual keyboard device.
//!
//! # Phase 3 architecture
//!
//! ```text
//! ferrokeyd init   ──open/configure/create/verify──▶  UinputDevice
//!        │                                                │
//!        └────────── SCM_RIGHTS fd transfer ──────────────┘
//!        │                                                │
//! ferrokeyd serve  ──adopt(fd)──▶  write-only event path (no ioctl)
//! ```
//!
//! [`UinputDevice::create`] is used only by the tiny bootstrap component
//! (`ferrokeyd init`) and performs the configuration ioctls (§8, §15, §16).
//! [`UinputDevice::adopt`] wraps an already-created fd received via
//! SCM_RIGHTS: it **verifies** the device identity and the advertised
//! capability bitmap (read-only ioctls + sysfs reads, all before the seccomp
//! freeze) and then exposes only the typed, closed event API (§18, §19).
//!
//! After the freeze the runtime holds exactly this fd and can only
//! `write(2)` validated `EV_KEY` events through it (§10, §14).
//!
//! # Unsafe discipline (§82)
//!
//! This module contains **no** `unsafe` code: the only `unsafe` in the crate
//! lives in [`crate::ffi`]. The fd is held as a `std::fs::File` and all
//! writes go through safe [`crate::emit`] wrappers.

use crate::capabilities::{capability_codes, is_capable, to_linux_code};
use crate::emit;
use crate::ffi;
use crate::ledger::{HeldLedger, LedgerError};
use ferrokey_core::{PhysicalKey, CAPABILITY_SET};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

/// Stable kernel identifiers for the virtual device (§49, §50).
///
/// These are ABI-like product properties: compositors, udev policies and
/// security policies key on them. Any change must be deliberate and tested.
pub const BUS_VIRTUAL: u16 = 0x03; // BUS_VIRTUAL
pub const VENDOR_ID: u16 = 0xFE20; // "Ferrokey"
pub const PRODUCT_ID: u16 = 0xFE21;
pub const VERSION_ID: u16 = 0x0001;
pub const DEVICE_NAME: &str = "Ferrokey Virtual Keyboard";

/// The identity Ferrokey's virtual keyboard reports to the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub bus: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

impl DeviceIdentity {
    pub const fn ferrokey() -> Self {
        DeviceIdentity {
            bus: BUS_VIRTUAL,
            vendor: VENDOR_ID,
            product: PRODUCT_ID,
            version: VERSION_ID,
        }
    }
}

/// Device creation parameters.
#[derive(Debug, Clone)]
pub struct DeviceOptions {
    /// Device name reported to the kernel (visible in `/proc/bus/input/devices`).
    /// Fixed from trusted configuration; never client-controlled (§49).
    pub name: String,
    /// Maximum simultaneously-held keys enforced by the ledger (§24).
    pub max_held_keys: usize,
}

impl Default for DeviceOptions {
    fn default() -> Self {
        DeviceOptions {
            name: DEVICE_NAME.to_string(),
            max_held_keys: crate::ledger::DEFAULT_MAX_HELD_KEYS,
        }
    }
}

/// The registered virtual keyboard, owned by the broker.
pub struct UinputDevice {
    fd: File,
    ledger: HeldLedger,
    name: String,
    /// The immutable capability set (linux key codes), in deterministic order.
    capabilities: Vec<u16>,
}

impl UinputDevice {
    /// Open `/dev/uinput` and register the virtual keyboard with the explicit
    /// capability set (bootstrap path).
    ///
    /// # Phase 3 contract (§8, §10)
    /// This is called **once** per broker instance, by the bootstrap
    /// component, before any hostile input is accepted. It performs the
    /// configuration ioctls that are forbidden at runtime.
    pub fn create(options: DeviceOptions) -> Result<Self, UinputError> {
        let max_held = options.max_held_keys;
        let fd = OpenOptions::new()
            .read(false)
            .write(true)
            .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOCTTY)
            .open("/dev/uinput")
            .map_err(UinputError::Open)?;
        let raw = fd.as_raw_fd();

        // Event classes: EV_SYN (required by uinput) + EV_KEY only (§19).
        ffi::ui_set_evbit(raw, i32::from(emit::EV_SYN))
            .map_err(|e| UinputError::Configure(format!("UI_SET_EVBIT(EV_SYN): {e}")))?;
        ffi::ui_set_evbit(raw, i32::from(emit::EV_KEY))
            .map_err(|e| UinputError::Configure(format!("UI_SET_EVBIT(EV_KEY): {e}")))?;
        // The explicit, immutable capability set — never a 0..KEY_MAX range.
        for &key in CAPABILITY_SET {
            let code = to_linux_code(key);
            ffi::ui_set_keybit(raw, i32::from(code))
                .map_err(|e| UinputError::Configure(format!("UI_SET_KEYBIT({code}): {e}")))?;
        }

        // Identity/name from trusted configuration (§49, §50).
        let identity = DeviceIdentity::ferrokey();
        let setup = ffi::UinputSetup::new(
            ffi::InputId {
                bustype: identity.bus,
                vendor: identity.vendor,
                product: identity.product,
                version: identity.version,
            },
            &options.name,
        );
        ffi::ui_dev_setup(raw, &setup)
            .map_err(|e| UinputError::Configure(format!("UI_DEV_SETUP: {e}")))?;
        ffi::ui_dev_create(raw).map_err(|e| UinputError::Create(format!("UI_DEV_CREATE: {e}")))?;

        let device = UinputDevice {
            fd,
            ledger: HeldLedger::new(max_held),
            name: options.name,
            capabilities: capability_codes(),
        };
        device.verify()?;
        Ok(device)
    }

    /// Wrap an already-created device fd received via SCM_RIGHTS (runtime
    /// path). Verifies identity and capability bitmap before use.
    ///
    /// # Preconditions
    /// * `fd` must be the fd transferred by our own bootstrap component (or a
    ///   court-instrumented equivalent); it is a character device backing a
    ///   uinput virtual device.
    ///
    /// # Postconditions
    /// * On `Ok`, the device has been positively identified as a Ferrokey
    ///   virtual keyboard advertising exactly the immutable capability set.
    /// * No further configuration ioctl is permitted; after the seccomp
    ///   freeze none will succeed anyway.
    pub fn adopt(fd: OwnedFd, name: &str, max_held_keys: usize) -> Result<Self, UinputError> {
        let file = File::from(fd);
        // fstat: must be a character device (a uinput fd is one).
        let stat = nix::sys::stat::fstat(file.as_fd())
            .map_err(|e| UinputError::Adopt(io::Error::from(e)))?;
        let mode = stat.st_mode;
        if mode & nix::libc::S_IFMT != nix::libc::S_IFCHR {
            return Err(UinputError::Adopt(io::Error::new(
                io::ErrorKind::InvalidData,
                "transferred fd is not a character device",
            )));
        }
        let device = UinputDevice {
            fd: file,
            ledger: HeldLedger::new(max_held_keys),
            name: name.to_string(),
            capabilities: capability_codes(),
        };
        device.verify()?;
        Ok(device)
    }

    /// Verify the device identity and capability bitmap against expectations.
    ///
    /// Read-only: uses `UI_GET_SYSNAME` (ioctl) plus sysfs reads. Must be
    /// called before the seccomp freeze (ioctl is forbidden after it).
    pub fn verify(&self) -> Result<(), UinputError> {
        let sysname = ffi::ui_get_sysname(self.fd.as_raw_fd()).map_err(UinputError::Sysname)?;
        let sysname = String::from_utf8_lossy(&sysname);
        if !sysname.starts_with("input") {
            return Err(UinputError::Verify(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected uinput sysname {sysname:?}"),
            )));
        }
        let syspath = PathBuf::from("/sys/devices/virtual/input").join(&*sysname);
        if !syspath.exists() {
            return Err(UinputError::Verify(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "sysfs path {} does not exist for created device",
                    syspath.display()
                ),
            )));
        }

        // Device identity must match the stable Ferrokey constants (§50).
        let (bus, vendor, product, version) = read_device_id(&syspath.join("id")).map_err(|e| {
            UinputError::Verify(io::Error::new(
                e.kind(),
                format!("reading device id from {}", syspath.join("id").display()),
            ))
        })?;
        let expected = DeviceIdentity::ferrokey();
        if (bus, vendor, product, version)
            != (
                expected.bus,
                expected.vendor,
                expected.product,
                expected.version,
            )
        {
            return Err(UinputError::Verify(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "device identity mismatch: bus={bus:x} vendor={vendor:x} \
                     product={product:x} version={version:x}, expected \
                     bus={:x} vendor={:x} product={:x} version={:x}",
                    expected.bus, expected.vendor, expected.product, expected.version
                ),
            )));
        }

        // The advertised capability bitmap must equal the immutable set (§13,
        // §21, §65): EV_KEY only (plus EV_SYN, always present), and exactly
        // the codes Ferrokey supports.
        let ev = read_cap_bitmap(&syspath.join("capabilities/ev")).map_err(|e| {
            UinputError::Verify(io::Error::new(
                e.kind(),
                format!(
                    "reading capability bitmap from {}",
                    syspath.join("capabilities/ev").display()
                ),
            ))
        })?;
        let key = read_cap_bitmap(&syspath.join("capabilities/key")).map_err(|e| {
            UinputError::Verify(io::Error::new(
                e.kind(),
                format!(
                    "reading capability bitmap from {}",
                    syspath.join("capabilities/key").display()
                ),
            ))
        })?;
        let expect_key = capability_codes();
        let mut expect_key = expect_key;
        expect_key.sort_unstable();
        let mut expect_ev = vec![emit::EV_KEY, emit::EV_SYN];
        expect_ev.sort_unstable();
        if ev != expect_ev {
            return Err(UinputError::Verify(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("device advertises unexpected event classes: {ev:?}"),
            )));
        }
        if key != expect_key {
            return Err(UinputError::Verify(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "device capability bitmap does not match the immutable set \
                     ({} codes advertised, {} expected)",
                    key.len(),
                    expect_key.len()
                ),
            )));
        }
        Ok(())
    }

    /// The immutable capability set (linux key codes).
    pub fn capability_codes(&self) -> &[u16] {
        &self.capabilities
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The raw fd (for SCM_RIGHTS transfer by the bootstrap component).
    pub fn raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Unwrap the fd (used by `init` to transfer it via SCM_RIGHTS).
    pub fn into_file(self) -> File {
        self.fd
    }

    /// The current number of keys the device believes are held.
    pub fn held_count(&self) -> usize {
        self.ledger.len()
    }

    /// Whether the device ledger currently holds `code`.
    pub fn is_held(&self, code: u16) -> bool {
        self.ledger.is_held(u32::from(code))
    }

    // ------------------------------------------------------------------
    // Typed, closed runtime operations (§18, §19, §20, §22)
    // ------------------------------------------------------------------

    /// Press a key. Validates at the device boundary (capability + ledger).
    pub fn key_down(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        let code = to_linux_code(key);
        self.validate_capable(code)?;
        self.ledger
            .key_down(u32::from(code))
            .map_err(sink_from_ledger)?;
        emit::write_one(self.fd.as_fd(), code, 1).map_err(sink_from_io)
    }

    /// Release a key.
    pub fn key_up(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        let code = to_linux_code(key);
        self.validate_capable(code)?;
        self.ledger
            .key_up(u32::from(code))
            .map_err(sink_from_ledger)?;
        emit::write_one(self.fd.as_fd(), code, 0).map_err(sink_from_io)
    }

    /// Autorepeat a held key (`EV_KEY` value=2; not a state transition).
    pub fn key_repeat(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        let code = to_linux_code(key);
        self.validate_capable(code)?;
        emit::write_one(self.fd.as_fd(), code, 2).map_err(sink_from_io)
    }

    /// Release exactly the given keys. **Fail-safe (§23):** every key is
    /// attempted even if earlier releases fail; all errors are collected.
    pub fn release_keys(&mut self, codes: &[u16]) -> Vec<SinkError> {
        let mut errors = Vec::new();
        for &code in codes {
            if let Err(e) = self.ledger.key_up(u32::from(code)) {
                errors.push(SinkError(e.to_string()));
                continue;
            }
            if let Err(e) = emit::write_one(self.fd.as_fd(), code, 0) {
                errors.push(SinkError(e.to_string()));
            }
        }
        errors
    }

    /// Release every key the device ledger holds, fail-safe (§23).
    pub fn release_all(&mut self) -> Vec<SinkError> {
        let codes: Vec<u16> = self
            .ledger
            .drain()
            .iter()
            .map(|c| u16::try_from(*c).unwrap_or(0))
            .collect();
        self.release_keys(&codes)
    }

    /// Protocol-boundary + device-boundary validation (§20): the code must be
    /// in the immutable capability set.
    fn validate_capable(&self, code: u16) -> Result<(), SinkError> {
        if is_capable(code) {
            Ok(())
        } else {
            Err(SinkError(format!(
                "key code {code} outside the immutable capability set"
            )))
        }
    }
}

impl std::fmt::Debug for UinputDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UinputDevice")
            .field("name", &self.name)
            .field("capabilities", &self.capabilities)
            .field("held", &self.ledger.len())
            .finish_non_exhaustive()
    }
}

/// Errors from device creation, adoption and operation.
#[derive(Debug, thiserror::Error)]
pub enum UinputError {
    #[error("cannot open /dev/uinput: {0}")]
    Open(io::Error),
    #[error("cannot configure the virtual device: {0}")]
    Configure(String),
    #[error("cannot register the virtual device: {0}")]
    Create(String),
    #[error("cannot adopt the transferred device fd: {0}")]
    Adopt(io::Error),
    #[error("cannot read device sysname: {0}")]
    Sysname(io::Error),
    #[error("device verification failed: {0}")]
    Verify(io::Error),
}

/// Error type for the typed device operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkError(pub String);

impl std::fmt::Display for SinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SinkError {}

fn sink_from_ledger(e: LedgerError) -> SinkError {
    SinkError(e.to_string())
}

fn sink_from_io(e: io::Error) -> SinkError {
    SinkError(e.to_string())
}

// ---------------------------------------------------------------------------
// sysfs helpers (verification only — used before the seccomp freeze)
// ---------------------------------------------------------------------------

/// Read `id/bus`, `id/vendor`, `id/product`, `id/version` from a sysfs input
/// directory.
fn read_device_id(syspath: &std::path::Path) -> io::Result<(u16, u16, u16, u16)> {
    let read = |name: &str| -> io::Result<u16> {
        let text = std::fs::read_to_string(syspath.join(name))?;
        Ok(u16::from_str_radix(text.trim(), 16).unwrap_or(0))
    };
    Ok((
        read("bustype")?,
        read("vendor")?,
        read("product")?,
        read("version")?,
    ))
}

/// Read a sysfs capability bitmap (`capabilities/ev`, `capabilities/key`)
/// as a sorted list of set bits.
///
/// The sysfs format prints the bitmap as native-`unsigned long` words from
/// most-significant word to least-significant word, each word as lowercase
/// hex with bit 0 as the LSB. So the *last* printed word is word 0, and
/// within a word bit `b` is the `b`-th bit.
fn read_cap_bitmap(path: &std::path::Path) -> io::Result<Vec<u16>> {
    let text = std::fs::read_to_string(path)?;
    let mut bits = Vec::new();
    // `split_whitespace().rev()`: the printed word order is high → low, so
    // the reversed order is word 0 first.
    for (word_idx, word) in text.split_whitespace().rev().enumerate() {
        let value = u64::from_str_radix(word, 16).unwrap_or(0);
        for bit in 0..64 {
            if value & (1u64 << bit) != 0 {
                let code = (word_idx * 64 + bit) as u16;
                bits.push(code);
            }
        }
    }
    bits.sort_unstable();
    Ok(bits)
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
    fn identity_constants_are_stable() {
        let id = DeviceIdentity::ferrokey();
        assert_eq!(id.bus, BUS_VIRTUAL);
        assert_eq!(id.vendor, VENDOR_ID);
        assert_eq!(id.product, PRODUCT_ID);
        assert_eq!(id.version, VERSION_ID);
        assert_eq!(id.bus, 0x03);
        assert_eq!(id.vendor, 0xFE20);
        assert_eq!(id.product, 0xFE21);
        assert_eq!(id.version, 0x0001);
    }

    #[test]
    fn capability_mapping_matches_core() {
        for &key in CAPABILITY_SET {
            let code = to_linux_code(key);
            assert_eq!(u32::from(code), key.linux_code());
        }
    }

    #[test]
    fn capability_codes_are_unique_and_bounded() {
        let codes = capability_codes();
        let unique: std::collections::BTreeSet<u16> = codes.iter().copied().collect();
        assert_eq!(unique.len(), codes.len());
        assert!(codes.iter().all(|c| *c <= 0x2ff));
    }
}
