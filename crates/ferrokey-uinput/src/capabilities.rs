//! Deterministic device capability construction.
//!
//! The virtual keyboard advertises exactly [`ferrokey_core::key::CAPABILITY_SET`]
//! — an explicit, auditable list — rather than a raw `1..240` scan of Linux
//! input codes. This module is the single place that derives the kernel
//! capability bitmap from `ferrokey-core`'s [`PhysicalKey`] space.
//!
//! # Phase 3 invariants (§21)
//!
//! * Every advertised code maps to a known `PhysicalKey`.
//! * Every supported `PhysicalKey` is advertised.
//! * No duplicate codes, no invalid codes, no non-key event classes.
//!
//! These are enforced by tests below and by the kernel-security courts, which
//! hash the guest's advertised capability bitmap before and after hostile
//! protocol fuzzing and require byte-for-byte equality (§65).

use ferrokey_core::{PhysicalKey, CAPABILITY_SET};

/// The complete list of linux key codes Ferrokey may emit, in the
/// deterministic order of `CAPABILITY_SET`.
///
/// # Panics
/// Panics if any `PhysicalKey` in `CAPABILITY_SET` has a linux code that
/// does not fit in `u16` (all current codes are `u32` values below 0x300,
/// so this is a static invariant kept as a runtime guard).
pub fn capability_codes() -> Vec<u16> {
    CAPABILITY_SET
        .iter()
        .map(|k| u16::try_from(k.linux_code()).expect("linux key codes fit in u16"))
        .collect()
}

/// Whether a raw linux key code belongs to the explicit capability set.
pub fn is_capable(code: u16) -> bool {
    CAPABILITY_SET
        .iter()
        .any(|k| u32::from(code) == k.linux_code())
}

/// The size of the explicit capability set (used by tests and courts).
pub fn capability_count() -> usize {
    CAPABILITY_SET.len()
}

/// Translate one Ferrokey physical key into its linux input code.
///
/// `PhysicalKey::linux_code()` returns the same values as the kernel's
/// `KEY_*` constants, so this is a straight narrowing — kept here so the
/// translation is auditable in one place.
///
/// # Panics
/// Panics if `key.linux_code()` does not fit in `u16` (a static invariant
/// for the current `PhysicalKey` space).
pub fn to_linux_code(key: PhysicalKey) -> u16 {
    u16::try_from(key.linux_code()).expect("linux key codes fit in u16")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn capability_set_maps_1_to_1() {
        let codes = capability_codes();
        assert_eq!(codes.len(), CAPABILITY_SET.len());
        let unique: BTreeSet<u16> = codes.iter().copied().collect();
        assert_eq!(
            unique.len(),
            codes.len(),
            "duplicate codes in capability set"
        );
    }

    #[test]
    fn every_supported_key_is_advertised() {
        for &key in CAPABILITY_SET {
            let code = to_linux_code(key);
            assert!(
                is_capable(code),
                "supported key {key:?} (code {code}) not advertised"
            );
            assert!(
                capability_codes().contains(&code),
                "capability list missing code {code}"
            );
        }
    }

    #[test]
    fn every_advertised_code_maps_to_a_known_key() {
        for code in capability_codes() {
            assert!(
                PhysicalKey::from_linux_code(u32::from(code)).is_some(),
                "advertised code {code} maps to no PhysicalKey"
            );
        }
    }

    #[test]
    fn no_invalid_or_non_key_codes() {
        // Linux input reserves KEY_MAX = 0x2ff; Ferrokey must never advertise
        // out-of-range codes, and the only event class is EV_KEY (handled in
        // the device builder, which enables exactly EV_SYN + EV_KEY).
        for code in capability_codes() {
            assert!(code <= 0x2ff, "code {code} exceeds KEY_MAX");
        }
        assert!(!is_capable(0));
        assert!(!is_capable(0x300));
        assert!(!is_capable(u16::MAX));
    }

    #[test]
    fn is_capable_matches_capability_set() {
        assert!(is_capable(to_linux_code(PhysicalKey::A)));
        assert!(is_capable(to_linux_code(PhysicalKey::F24)));
        assert!(!is_capable(0));
        assert!(!is_capable(255));
        assert!(!is_capable(0x100));
    }

    #[test]
    fn known_mappings() {
        assert_eq!(to_linux_code(PhysicalKey::A), 30);
        assert_eq!(to_linux_code(PhysicalKey::LeftCtrl), 29);
        assert_eq!(to_linux_code(PhysicalKey::KpEnter), 96);
    }
}
