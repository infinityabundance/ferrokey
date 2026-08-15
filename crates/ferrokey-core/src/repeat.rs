//! Deterministic key-repeat engine.
//!
//! Ferrokey does **not** rely on Slint's `clicked` callback as the repeat
//! primitive. The UI reports raw `pointer-down` / `pointer-up`, and this
//! engine re-emits `KeyDown` for held, repeatable keys at a fixed cadence:
//!
//! ```text
//! pointer down ──► immediate KeyDown
//!                   │
//!                   └── repeat delay (e.g. 500 ms)
//!                           │
//!                           └── repeat cadence (e.g. 30 ms) ──► KeyDown …
//! pointer up ──► repeat stops immediately, KeyUp emitted
//! ```
//!
//! The engine is pure and deterministic: it never touches a clock itself, it
//! is driven by explicit `now` values so tests can script time precisely.

use crate::key::PhysicalKey;
use crate::keyset::MAX_HELD_KEYS;
use crate::time::Moment;
use std::time::Duration;

/// Repeat timing configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepeatSettings {
    /// Master switch: when disabled, held keys never repeat.
    pub enabled: bool,
    /// Delay before the first repeat fires while a key is held.
    pub delay: Duration,
    /// Interval between subsequent repeats.
    pub cadence: Duration,
}

impl Default for RepeatSettings {
    fn default() -> Self {
        RepeatSettings {
            enabled: true,
            delay: Duration::from_millis(500),
            cadence: Duration::from_millis(30),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RepeatState {
    /// When the next repeat event is due.
    next_repeat_at: Moment,
    /// Whether the initial delay has already elapsed (cadence mode).
    repeating: bool,
}

/// Tracks held repeatable keys and produces the repeat stream.
///
/// Linear fixed-capacity bookkeeping (`[Option<..>; MAX_HELD_KEYS]`, bounded
/// by the held-key cap): the engine is allocation-free in the hot path and
/// model-checkable. Every scan is a constant-bound loop over the fixed array
/// (stale slots are `None` and skipped), so CBMC derives exact trip bounds.
#[derive(Debug, Clone)]
pub struct RepeatEngine {
    settings: RepeatSettings,
    held: [Option<(PhysicalKey, RepeatState)>; MAX_HELD_KEYS],
    len: usize,
}

impl RepeatEngine {
    pub fn new(settings: RepeatSettings) -> Self {
        RepeatEngine {
            settings,
            held: [None; MAX_HELD_KEYS],
            len: 0,
        }
    }

    /// Start tracking a held key. `repeatable` comes from the active layout
    /// (non-repeatable keys such as modifiers are tracked but never fire).
    pub fn key_down(&mut self, key: PhysicalKey, now: Moment, repeatable: bool) {
        // Replace an existing entry for `key` (idempotent).
        let mut i = 0;
        while i < MAX_HELD_KEYS {
            if let Some((k, _)) = self.held[i] {
                if k == key {
                    self.remove_at(i);
                    break;
                }
            }
            i += 1;
        }
        if !repeatable {
            // Track nothing: this key will never repeat.
            return;
        }
        let next = if self.settings.enabled {
            now + self.settings.delay
        } else {
            // Repeat disabled: keep a marker far in the future so `held` is
            // accurate but no event can fire.
            now + Duration::from_hours(24)
        };
        if self.len >= MAX_HELD_KEYS {
            // The rollover guard upstream caps held keys below this; never
            // panic.
            return;
        }
        self.held[self.len] = Some((
            key,
            RepeatState {
                next_repeat_at: next,
                repeating: false,
            },
        ));
        self.len += 1;
    }

    /// Stop tracking a key (pointer up).
    pub fn key_up(&mut self, key: PhysicalKey) {
        let mut i = 0;
        while i < MAX_HELD_KEYS {
            if let Some((k, _)) = self.held[i] {
                if k == key {
                    self.remove_at(i);
                    break;
                }
            }
            i += 1;
        }
    }

    /// Remove the entry at `pos`, shifting the tail left (constant bound).
    fn remove_at(&mut self, pos: usize) {
        let mut k = 0;
        while k < MAX_HELD_KEYS {
            let j = k;
            if j >= pos && j + 1 < self.len {
                self.held[j] = self.held[j + 1];
            }
            k += 1;
        }
        self.held[self.len - 1] = None;
        self.len -= 1;
    }

    /// Advance the clock. Returns the keys that must be re-emitted as
    /// `KeyDown` at this instant, in deterministic order.
    pub fn tick(&mut self, now: Moment) -> Vec<PhysicalKey> {
        if !self.settings.enabled {
            return Vec::new();
        }
        let mut due = Vec::new();
        let cadence = self.settings.cadence;
        let mut i = 0;
        while i < MAX_HELD_KEYS {
            if let Some((key, state)) = self.held[i].as_mut() {
                if now >= state.next_repeat_at {
                    due.push(*key);
                    state.next_repeat_at = now + cadence;
                    state.repeating = true;
                }
            }
            i += 1;
        }
        due
    }

    /// Whether the engine is currently repeating `key` (already past the
    /// initial delay).
    pub fn is_repeating(&self, key: PhysicalKey) -> bool {
        let mut i = 0;
        while i < MAX_HELD_KEYS {
            if let Some((k, s)) = self.held[i] {
                if k == key && s.repeating {
                    return true;
                }
            }
            i += 1;
        }
        false
    }

    /// Stop all repeats (release-all / hide / disconnect).
    pub fn release_all(&mut self) {
        self.held = [None; MAX_HELD_KEYS];
        self.len = 0;
    }

    /// Keys currently tracked as held (repeatable or not).
    pub fn held_keys(&self) -> HeldKeysIter<'_> {
        HeldKeysIter {
            held: &self.held,
            idx: 0,
        }
    }

    /// Number of keys currently tracked (flat scan — the model-checkable
    /// query primitive; see `keyset::KeySet::copy_into`).
    pub fn held_len(&self) -> usize {
        let mut n = 0;
        let mut i = 0;
        while i < MAX_HELD_KEYS {
            if self.held[i].is_some() {
                n += 1;
            }
            i += 1;
        }
        n
    }

    /// Whether `key` is currently tracked (flat scan, no iterator nesting).
    pub fn contains_held(&self, key: PhysicalKey) -> bool {
        let mut found = false;
        let mut i = 0;
        while i < MAX_HELD_KEYS {
            if let Some((k, _)) = self.held[i] {
                if k == key {
                    found = true;
                }
            }
            i += 1;
        }
        found
    }

    pub fn settings(&self) -> &RepeatSettings {
        &self.settings
    }
}

/// The concrete iterator over a [`RepeatEngine`]'s tracked keys.
///
/// Index-based with a constant trip bound — no std adapter fusion (see the
/// `keyset::KeyIter` notes).
#[derive(Debug, Clone)]
pub struct HeldKeysIter<'a> {
    held: &'a [Option<(PhysicalKey, RepeatState)>; MAX_HELD_KEYS],
    idx: usize,
}

impl Iterator for HeldKeysIter<'_> {
    type Item = PhysicalKey;

    fn next(&mut self) -> Option<PhysicalKey> {
        while self.idx < MAX_HELD_KEYS {
            let e = self.held[self.idx];
            self.idx += 1;
            if let Some((k, _)) = e {
                return Some(k);
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(MAX_HELD_KEYS - self.idx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Moment {
        Moment::from_millis(1_000_000)
    }

    #[test]
    fn first_repeat_after_delay_then_cadence() {
        let mut eng = RepeatEngine::new(RepeatSettings::default());
        let now = t0();
        eng.key_down(PhysicalKey::A, now, true);

        // No repeats before the delay elapses.
        assert!(eng.tick(now + Duration::from_millis(499)).is_empty());
        // Exactly at the delay: first repeat.
        assert_eq!(
            eng.tick(now + Duration::from_millis(500)),
            vec![PhysicalKey::A]
        );
        // Cadence intervals from then on.
        assert!(eng.tick(now + Duration::from_millis(529)).is_empty());
        assert_eq!(
            eng.tick(now + Duration::from_millis(530)),
            vec![PhysicalKey::A]
        );
    }

    #[test]
    fn repeat_stops_on_key_up() {
        let mut eng = RepeatEngine::new(RepeatSettings::default());
        let now = t0();
        eng.key_down(PhysicalKey::Backspace, now, true);
        eng.tick(now + Duration::from_millis(500));
        eng.key_up(PhysicalKey::Backspace);
        assert!(eng.tick(now + Duration::from_secs(1)).is_empty());
        assert!(eng.held_keys().next().is_none());
    }

    #[test]
    fn non_repeatable_keys_never_fire() {
        let mut eng = RepeatEngine::new(RepeatSettings::default());
        let now = t0();
        eng.key_down(PhysicalKey::LeftShift, now, false);
        assert!(eng.tick(now + Duration::from_secs(10)).is_empty());
        assert!(eng.held_keys().next().is_none());
    }

    #[test]
    fn disabled_engine_never_repeats() {
        let settings = RepeatSettings {
            enabled: false,
            ..Default::default()
        };
        let mut eng = RepeatEngine::new(settings);
        let now = t0();
        eng.key_down(PhysicalKey::A, now, true);
        assert!(eng.tick(now + Duration::from_mins(1)).is_empty());
    }

    #[test]
    fn release_all_clears() {
        let mut eng = RepeatEngine::new(RepeatSettings::default());
        let now = t0();
        eng.key_down(PhysicalKey::A, now, true);
        eng.key_down(PhysicalKey::Space, now, true);
        eng.release_all();
        assert!(eng.tick(now + Duration::from_secs(5)).is_empty());
    }

    #[test]
    fn multiple_keys_repeat_in_key_order() {
        let mut eng = RepeatEngine::new(RepeatSettings::default());
        let now = t0();
        eng.key_down(PhysicalKey::A, now, true);
        eng.key_down(PhysicalKey::Space, now, true);
        // Sorted by key: A before Space.
        assert_eq!(
            eng.tick(now + Duration::from_millis(500)),
            vec![PhysicalKey::A, PhysicalKey::Space]
        );
    }
}
