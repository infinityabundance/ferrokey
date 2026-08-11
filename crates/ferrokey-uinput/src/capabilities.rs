//! Deterministic device capability construction.
//!
//! The virtual keyboard advertises exactly [`ferrokey_core::key::CAPABILITY_SET`]
//! — an explicit, auditable list — rather than a raw `1..240` scan of Linux
//! input codes. This module is the single place that translates
//! `ferrokey-core`'s [`PhysicalKey`] space into evdev `KeyCode`s.

use evdev::KeyCode;
use ferrokey_core::{PhysicalKey, CAPABILITY_SET};

/// Translate one Ferrokey physical key into its evdev key code.
///
/// `PhysicalKey::linux_code()` returns the same values as the kernel's
/// `KEY_*` constants, so this is a straight wrap — but keeping the
/// translation here means core stays free of any evdev dependency.
pub fn to_evdev_key(key: PhysicalKey) -> KeyCode {
    KeyCode::new(key.linux_code() as u16)
}

/// The full explicit key capability list, in deterministic order.
pub fn capability_keys() -> Vec<KeyCode> {
    CAPABILITY_SET.iter().map(|&k| to_evdev_key(k)).collect()
}

/// The set of linux key codes Ferrokey may emit (as `u32`), for protocol
/// validation and ledger checks.
pub fn capability_codes() -> Vec<u32> {
    CAPABILITY_SET.iter().map(|k| k.linux_code()).collect()
}

/// Whether a raw linux key code is part of the explicit capability set.
pub fn is_capable(code: u32) -> bool {
    CAPABILITY_SET.iter().any(|k| k.linux_code() == code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn capability_set_maps_1_to_1() {
        let keys = capability_keys();
        assert_eq!(keys.len(), CAPABILITY_SET.len());
        let unique: BTreeSet<u32> = keys.iter().map(|k| u32::from(k.code())).collect();
        assert_eq!(unique.len(), keys.len());
    }

    #[test]
    fn is_capable_matches_capability_set() {
        assert!(is_capable(PhysicalKey::A.linux_code()));
        assert!(is_capable(PhysicalKey::F24.linux_code()));
        assert!(!is_capable(0));
        assert!(!is_capable(255));
        assert!(!is_capable(0x100));
    }

    #[test]
    fn known_mappings() {
        assert_eq!(to_evdev_key(PhysicalKey::A), KeyCode::KEY_A);
        assert_eq!(to_evdev_key(PhysicalKey::LeftCtrl), KeyCode::KEY_LEFTCTRL);
        assert_eq!(to_evdev_key(PhysicalKey::KpEnter), KeyCode::KEY_KPENTER);
    }
}
