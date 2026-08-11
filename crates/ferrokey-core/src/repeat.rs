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
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

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
    next_repeat_at: Instant,
    /// Whether the initial delay has already elapsed (cadence mode).
    repeating: bool,
}

/// Tracks held repeatable keys and produces the repeat stream.
#[derive(Debug, Clone)]
pub struct RepeatEngine {
    settings: RepeatSettings,
    held: BTreeMap<PhysicalKey, RepeatState>,
}

impl RepeatEngine {
    pub fn new(settings: RepeatSettings) -> Self {
        RepeatEngine {
            settings,
            held: BTreeMap::new(),
        }
    }

    /// Start tracking a held key. `repeatable` comes from the active layout
    /// (non-repeatable keys such as modifiers are tracked but never fire).
    pub fn key_down(&mut self, key: PhysicalKey, now: Instant, repeatable: bool) {
        if !repeatable {
            // Track nothing: this key will never repeat.
            self.held.remove(&key);
            return;
        }
        let next = if self.settings.enabled {
            now + self.settings.delay
        } else {
            // Repeat disabled: keep a marker far in the future so `held` is
            // accurate but no event can fire.
            now + Duration::from_secs(24 * 60 * 60)
        };
        self.held.insert(
            key,
            RepeatState {
                next_repeat_at: next,
                repeating: false,
            },
        );
    }

    /// Stop tracking a key (pointer up).
    pub fn key_up(&mut self, key: PhysicalKey) {
        self.held.remove(&key);
    }

    /// Advance the clock. Returns the keys that must be re-emitted as
    /// `KeyDown` at this instant, in deterministic order.
    pub fn tick(&mut self, now: Instant) -> Vec<PhysicalKey> {
        if !self.settings.enabled {
            return Vec::new();
        }
        let mut due = Vec::new();
        let cadence = self.settings.cadence;
        for (&key, state) in self.held.iter_mut() {
            if now >= state.next_repeat_at {
                due.push(key);
                state.next_repeat_at = now + cadence;
                state.repeating = true;
            }
        }
        due
    }

    /// Whether the engine is currently repeating `key` (already past the
    /// initial delay).
    pub fn is_repeating(&self, key: PhysicalKey) -> bool {
        self.held.get(&key).is_some_and(|s| s.repeating)
    }

    /// Stop all repeats (release-all / hide / disconnect).
    pub fn release_all(&mut self) {
        self.held.clear();
    }

    /// Keys currently tracked as held (repeatable or not).
    pub fn held_keys(&self) -> impl Iterator<Item = PhysicalKey> + '_ {
        self.held.keys().copied()
    }

    pub fn settings(&self) -> &RepeatSettings {
        &self.settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
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
        assert!(eng.tick(now + Duration::from_millis(1000)).is_empty());
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
        assert!(eng.tick(now + Duration::from_secs(60)).is_empty());
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
