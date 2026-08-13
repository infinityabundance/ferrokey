//! The authoritative held-key ledger.
//!
//! Ferrokey must never leave a stuck modifier behind, and Phase 3 §22 makes
//! the broker's ledger the *authoritative* record of depressed keys: the
//! kernel is never trusted to track Ferrokey's logical state.
//!
//! # Transition rules
//!
//! * `key_down` of an already-held key is **rejected** (`KeyBusy`). In the
//!   phase-3 protocol the repeat path is the explicit `KEY_REPEAT` message
//!   (`EV_KEY` value=2), which does **not** touch the ledger — so duplicate
//!   `KEY_DOWN` is always a protocol error or cross-session ownership
//!   violation (§12, §22).
//! * `key_up` without a matching `key_down` is rejected (`KeyUpWithoutDown`).
//! * The simultaneous-keys cap is enforced (`Rollover`) with a bounded
//!   maximum (§24).
//! * `release_all`/`drain` empties the ledger; the caller must emit `Up` for
//!   every returned key.
//!
//! # Recovery contract
//!
//! * Client disconnect → release exactly that session's keys.
//! * Daemon SIGKILL → the kernel unregisters the uinput device on fd close
//!   and releases every key, so no stuck keys survive a killed broker.

use std::collections::BTreeSet;

/// The maximum number of simultaneously depressed keys Ferrokey will accept.
///
/// Phase 3 §24: hard-bounded with an explicit sane constant. 16 matches
/// typical multi-key hardware; the config validator additionally clamps
/// `max_held_keys` to `1..=MAX_HELD_KEYS_LIMIT`.
pub const DEFAULT_MAX_HELD_KEYS: usize = 16;

/// The hard upper bound for `max_held_keys` (config validation limit).
///
/// Justification: a virtual keyboard with more simultaneously-held keys than
/// this provides no legitimate typing benefit (real keyboards cap at ~6-10
/// rollover), while each additional held key multiplies ledger, rate-limit
/// and kernel-event surface. The constant is explicit and testable.
pub const MAX_HELD_KEYS_LIMIT: usize = 32;

/// Errors produced by the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LedgerError {
    #[error("key_up for key {0} without a matching key_down (impossible transition)")]
    KeyUpWithoutDown(u32),
    #[error("key_down for key {0} exceeds the simultaneous-keys cap of {1}")]
    Rollover(u32, usize),
    #[error("key code 0x{0:x} is outside the explicit capability set")]
    NotCapable(u32),
    #[error("key_down for key {0} while already held (duplicate down / cross-session ownership)")]
    KeyBusy(u32),
}

/// A set of held key codes with strict transition rules.
#[derive(Debug, Clone, Default)]
pub struct HeldLedger {
    held: BTreeSet<u32>,
    max_held: usize,
}

impl HeldLedger {
    /// Create a ledger with a bounded cap.
    ///
    /// # Preconditions
    /// * `max_held` must satisfy `1 <= max_held <= MAX_HELD_KEYS_LIMIT`
    ///   (enforced by config validation; clamped defensively here).
    pub fn new(max_held: usize) -> Self {
        let max_held = max_held.clamp(1, MAX_HELD_KEYS_LIMIT);
        HeldLedger {
            held: BTreeSet::new(),
            max_held,
        }
    }

    /// Register a key-down. Rejects duplicates and rollover.
    pub fn key_down(&mut self, code: u32) -> Result<(), LedgerError> {
        if self.held.contains(&code) {
            return Err(LedgerError::KeyBusy(code));
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
    fn duplicate_down_is_rejected() {
        let mut ledger = HeldLedger::new(16);
        ledger.key_down(30).unwrap();
        assert_eq!(ledger.key_down(30), Err(LedgerError::KeyBusy(30)));
        assert_eq!(ledger.len(), 1, "duplicate down must not change state");
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
    fn max_held_is_clamped_to_the_sane_limit() {
        // A hostile config value (e.g. 18446744073709551615) must never
        // create an unbounded ledger.
        let ledger = HeldLedger::new(usize::MAX);
        assert_eq!(ledger.max_held(), MAX_HELD_KEYS_LIMIT);
        let ledger = HeldLedger::new(0);
        assert_eq!(ledger.max_held(), 1);
    }

    #[test]
    fn drain_returns_all_and_clears() {
        let mut ledger = HeldLedger::new(16);
        ledger.key_down(30).unwrap();
        ledger.key_down(42).unwrap();
        let keys = ledger.drain();
        assert_eq!(keys, vec![30, 42]);
        assert!(ledger.is_empty());
        // After drain, ups of drained keys are impossible transitions.
        assert_eq!(ledger.key_up(30), Err(LedgerError::KeyUpWithoutDown(30)));
    }

    #[test]
    fn not_capable_is_surfaceable() {
        // The ledger itself does not enforce capability (the daemon validates
        // protocol input against `is_capable`), but the error variant exists
        // for that call path.
        let err = LedgerError::NotCapable(0x300);
        assert!(err.to_string().contains("0x300"));
    }

    // -----------------------------------------------------------------------
    // M9 (§87): randomized property tests with a deterministic seeded PRNG
    // (no external property-test dependency — the broker minimizes its
    // dependency tree, §83).
    // -----------------------------------------------------------------------

    fn next_rand(rng: &mut u64) -> u64 {
        *rng ^= *rng << 13;
        *rng ^= *rng >> 7;
        *rng ^= *rng << 17;
        (*rng).wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Op {
        Down(u32),
        Up(u32),
        Drain,
        UpRandom, // up of a code that may not be held (invalid op)
    }

    /// The reference: an unordered set plus the documented transition rules.
    /// Mirrors the ledger exactly; the property test proves both stay in
    /// lockstep under arbitrary operation sequences.
    #[derive(Debug, Default)]
    struct Model {
        held: std::collections::BTreeSet<u32>,
    }

    fn model_step(model: &mut Model, op: Op, max_held: usize) -> bool {
        match op {
            Op::Down(code) => {
                if model.held.contains(&code) || model.held.len() >= max_held {
                    false
                } else {
                    model.held.insert(code);
                    true
                }
            }
            Op::Up(code) => model.held.remove(&code),
            Op::Drain => {
                model.held.clear();
                true
            }
            Op::UpRandom => false, // up of a possibly-unheld code
        }
    }

    #[test]
    fn randomized_op_sequences_keep_ledger_and_model_in_lockstep() {
        let mut rng: u64 = 0xA11C_E5BA_5EBE_EFCA;
        // Under Miri the interpreter is ~1000× slower; the same invariants
        // are exercised on a smaller sample (§86).
        let rounds: u64 = if cfg!(miri) { 20 } else { 300 };
        for round in 0..rounds {
            let max_held = 1 + (next_rand(&mut rng) % 8) as usize;
            let mut ledger = HeldLedger::new(max_held);
            let mut model = Model::default();
            let space: Vec<u32> = (0..12).map(|i| 30 + 3 * i).collect();

            let steps = 20 + (next_rand(&mut rng) % 60) as usize;
            for _ in 0..steps {
                let op = match next_rand(&mut rng) % 10 {
                    0..=4 => Op::Down(space[(next_rand(&mut rng) as usize) % space.len()]),
                    5..=7 => Op::Up(space[(next_rand(&mut rng) as usize) % space.len()]),
                    8 => Op::Drain,
                    _ => Op::UpRandom,
                };
                let model_ok = model_step(&mut model, op, max_held);
                let ledger_result = match op {
                    Op::Down(c) => ledger.key_down(c),
                    Op::Up(c) => ledger.key_up(c),
                    Op::Drain => {
                        ledger.drain();
                        Ok(())
                    }
                    Op::UpRandom => ledger.key_up(0xFFFF),
                };
                assert_eq!(
                    ledger_result.is_ok(),
                    model_ok,
                    "round {round}: ledger/model disagree on {op:?}"
                );
                assert_eq!(
                    ledger.held().iter().copied().collect::<Vec<_>>(),
                    model.held.iter().copied().collect::<Vec<_>>(),
                    "round {round}: ledger state diverged after {op:?}"
                );
            }

            // §87: release/drain always empties the ledger.
            let drained = ledger.drain();
            assert!(ledger.is_empty(), "drain must empty the ledger");
            assert_eq!(
                drained
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                model.held
            );
            model.held.clear();
            assert!(ledger.is_empty() && model.held.is_empty());
        }
    }

    #[test]
    fn invalid_ops_never_modify_state() {
        // For every invalid op shape, the ledger state (the held set and
        // its len) must be unchanged by the rejected operation.
        let snapshot =
            |ledger: &HeldLedger| -> Vec<u32> { ledger.held().iter().copied().collect() };
        let mut ledger = HeldLedger::new(4);
        ledger.key_down(30).unwrap();
        ledger.key_down(31).unwrap();
        let base = snapshot(&ledger);

        // duplicate down
        let before = snapshot(&ledger);
        assert_eq!(ledger.key_down(30), Err(LedgerError::KeyBusy(30)));
        assert_eq!(
            snapshot(&ledger),
            before,
            "duplicate down changed the ledger"
        );

        // up without down
        let before = snapshot(&ledger);
        assert_eq!(ledger.key_up(99), Err(LedgerError::KeyUpWithoutDown(99)));
        assert_eq!(
            snapshot(&ledger),
            before,
            "up-without-down changed the ledger"
        );

        // rollover (cap is 4; fill the remaining slots, then overflow)
        ledger.key_down(32).unwrap();
        ledger.key_down(33).unwrap();
        let before = snapshot(&ledger);
        assert_eq!(ledger.key_down(34), Err(LedgerError::Rollover(34, 4)));
        assert_eq!(snapshot(&ledger), before, "rollover changed the ledger");

        // The only valid mutations were the two fills.
        assert_eq!(snapshot(&ledger), vec![30, 31, 32, 33]);
        assert_eq!(ledger.len(), 4);
        assert_ne!(base.len(), 4, "test precondition: fills must add keys");
    }
}
