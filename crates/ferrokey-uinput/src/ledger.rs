//! The defensive held-key ledger.
//!
//! Ferrokey must never leave a stuck modifier behind. This ledger tracks
//! every linux key code currently reported down to the device, enforces the
//! simultaneous-keys cap, rejects impossible transitions (`key_up` without a
//! matching `key_down`), and can atomically release everything.
//!
//! Recovery contract:
//!
//! * `key_down` is **idempotent** — the repeat engine legitimately re-emits
//!   `KeyDown` for already-held keys (kernel repeat semantics), so a second
//!   down for a held key is not an error.
//! * `key_up` for a key the ledger does not hold is rejected as an impossible
//!   transition (hostile or corrupt input).
//! * On UI disconnect / SIGTERM / device teardown, [`HeldLedger::release_all`]
//!   emits `Up` for every held key. If the daemon itself is killed, the kernel
//!   unregisters the uinput device and releases all keys as part of close —
//!   so a `SIGKILL`ed daemon cannot leave stuck keys either.

use std::collections::BTreeSet;

/// The maximum number of simultaneously depressed keys Ferrokey will accept.
/// Real keyboards typically allow 6+ keys; Ferrokey's virtual device applies
/// a deliberate, configurable cap to bound the attack surface.
pub const DEFAULT_MAX_HELD_KEYS: usize = 16;

/// Errors produced by the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LedgerError {
    #[error("key_up for key {0} without a matching key_down (impossible transition)")]
    KeyUpWithoutDown(u32),
    #[error("key_down for key {0} exceeds the simultaneous-keys cap of {1}")]
    Rollover(u32, usize),
    #[error("key code 0x{0:x} is outside the explicit capability set")]
    NotCapable(u32),
}

/// A set of held key codes with strict transition rules.
#[derive(Debug, Clone, Default)]
pub struct HeldLedger {
    held: BTreeSet<u32>,
    max_held: usize,
}

impl HeldLedger {
    pub fn new(max_held: usize) -> Self {
        HeldLedger {
            held: BTreeSet::new(),
            max_held,
        }
    }

    /// Register a key-down. Idempotent for already-held keys (repeat path).
    pub fn key_down(&mut self, code: u32) -> Result<(), LedgerError> {
        if self.held.contains(&code) {
            // Repeat press of a held key: legal (kernel repeat semantics).
            return Ok(());
        }
        if self.held.len() >= self.max_held {
            return Err(LedgerError::Rollover(code, self.max_held));
        }
        self.held.insert(code);
        Ok(())
    }

    /// Register a key-up. Rejects release of keys that are not held.
    pub fn key_up(&mut self, code: u32) -> Result<(), LedgerError> {
        if !self.held.remove(&code) {
            return Err(LedgerError::KeyUpWithoutDown(code));
        }
        Ok(())
    }

    /// The set of keys currently held (deterministic order).
    pub fn held(&self) -> &BTreeSet<u32> {
        &self.held
    }

    pub fn is_held(&self, code: u32) -> bool {
        self.held.contains(&code)
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    pub fn len(&self) -> usize {
        self.held.len()
    }

    /// Take all held keys and return them, clearing the ledger. The caller
    /// must emit `Up` for each returned key.
    pub fn drain(&mut self) -> Vec<u32> {
        let keys: Vec<u32> = self.held.iter().copied().collect();
        self.held.clear();
        keys
    }

    pub fn max_held(&self) -> usize {
        self.max_held
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn down_up_round_trip() {
        let mut ledger = HeldLedger::new(16);
        ledger.key_down(30).unwrap();
        assert!(ledger.is_held(30));
        ledger.key_up(30).unwrap();
        assert!(!ledger.is_held(30));
        assert!(ledger.is_empty());
    }

    #[test]
    fn repeat_down_is_idempotent() {
        let mut ledger = HeldLedger::new(16);
        ledger.key_down(30).unwrap();
        // The repeat engine re-emits KeyDown for held keys: must be accepted.
        ledger.key_down(30).unwrap();
        assert_eq!(ledger.len(), 1);
        ledger.key_up(30).unwrap();
        assert!(ledger.is_empty());
    }

    #[test]
    fn key_up_without_down_is_rejected() {
        let mut ledger = HeldLedger::new(16);
        assert_eq!(ledger.key_up(30), Err(LedgerError::KeyUpWithoutDown(30)));
    }

    #[test]
    fn rollover_cap_is_enforced() {
        let mut ledger = HeldLedger::new(3);
        ledger.key_down(30).unwrap();
        ledger.key_down(31).unwrap();
        ledger.key_down(32).unwrap();
        assert_eq!(ledger.key_down(33), Err(LedgerError::Rollover(33, 3)));
    }

    #[test]
    fn drain_returns_all_and_clears() {
        let mut ledger = HeldLedger::new(16);
        ledger.key_down(30).unwrap();
        ledger.key_down(42).unwrap();
        let keys = ledger.drain();
        assert_eq!(keys, vec![30, 42]);
        assert!(ledger.is_empty());
    }

    #[test]
    fn not_capable_is_surfaceable() {
        // The ledger itself does not enforce capability (the daemon validates
        // protocol input against `is_capable`), but the error variant exists
        // for that call path.
        let err = LedgerError::NotCapable(0x300);
        assert!(err.to_string().contains("0x300"));
    }
}
