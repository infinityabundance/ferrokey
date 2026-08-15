//! The Ferrokey keyboard state machine.
//!
//! Owns everything that cannot be represented by `send_key(u16)`:
//!
//! * physically depressed keys (chords / rollover with a hard cap)
//! * latched (sticky) modifiers — tap Shift ⇒ next key is shifted
//! * locked modifiers — double-tap Shift ⇒ Caps Lock
//! * Caps Lock / Num Lock mirror state
//! * active layer resolution (Base / Shift / AltGr / Fn)
//!
//! The machine is deliberately **pure and deterministic**: every mutating
//! method takes an explicit `now: Moment` so tests can drive time exactly
//! and reproduce any sequence.

use crate::key::PhysicalKey;
use crate::keyset::{KeySet, MAX_HELD_KEYS};
use crate::modifier::{ModifierKind, ModifierSet};
use crate::time::Moment;
use std::time::Duration;

/// The virtual press duration used when the UI reports an explicit `Tap`.
/// Chosen well below the default [`StateSettings::tap_timeout`] so a tap is
/// always recognised as a tap.
pub const TAP_GRACE: Duration = Duration::from_millis(20);

/// The maximum number of physical modifier keys Ferrokey injects for one
/// pressed key (one per modifier kind) — the per-entry capacity of
/// [`KeyboardState::injected_mods`].
const MAX_INJECTED_MODS: usize = ModifierKind::COUNT;

/// A single key event the state machine decides to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    Down(PhysicalKey),
    Up(PhysicalKey),
}

impl KeyEvent {
    pub fn key(&self) -> PhysicalKey {
        match self {
            KeyEvent::Down(k) | KeyEvent::Up(k) => *k,
        }
    }
}

/// Errors produced by the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StateError {
    #[error("keyboard rollover limit ({limit} simultaneously held keys) exceeded")]
    Rollover { limit: usize },
}

/// The active keyboard layer.
///
/// Resolved from the effective modifier state. `Fn` takes precedence over
/// `AltGr`, which takes precedence over `Shift`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layer {
    #[default]
    Base,
    Shift,
    AltGr,
    Fn,
}

impl Layer {
    pub fn from_modifiers(mods: ModifierSet) -> Layer {
        if mods.contains(ModifierSet::FN) {
            Layer::Fn
        } else if mods.contains(ModifierSet::ALTGR) {
            Layer::AltGr
        } else if mods.contains(ModifierSet::SHIFT) {
            Layer::Shift
        } else {
            Layer::Base
        }
    }
}

/// Tunable state-machine behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateSettings {
    /// Allow sticky (latched) modifiers via quick taps.
    pub latch_enabled: bool,
    /// Allow locking modifiers via double-taps (e.g. Shift ⇒ Caps Lock).
    pub lock_enabled: bool,
    /// Maximum press duration for a release to count as a "tap".
    pub tap_timeout: Duration,
    /// Window within which two taps of the same modifier count as a double-tap.
    pub double_tap_timeout: Duration,
    /// Maximum number of simultaneously depressed keys (rollover cap).
    pub max_held_keys: usize,
}

impl Default for StateSettings {
    fn default() -> Self {
        StateSettings {
            latch_enabled: true,
            lock_enabled: true,
            tap_timeout: Duration::from_millis(400),
            double_tap_timeout: Duration::from_millis(500),
            max_held_keys: 16,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TapTrack {
    down_at: Moment,
    /// Set when a non-modifier key was pressed while this modifier was held:
    /// the release is then a chord release, never a tap.
    interleaved: bool,
}

/// The physical modifier keys injected for one pressed key (latch/lock
/// consumption). At most one per modifier kind — `ModifierKind::COUNT`.
#[derive(Debug, Clone, Copy)]
struct InjectedMods {
    keys: [PhysicalKey; MAX_INJECTED_MODS],
    len: usize,
}

impl InjectedMods {
    const fn new() -> Self {
        InjectedMods {
            keys: [PhysicalKey::Escape; MAX_INJECTED_MODS],
            len: 0,
        }
    }

    fn push(&mut self, key: PhysicalKey) {
        if self.len < MAX_INJECTED_MODS {
            self.keys[self.len] = key;
            self.len += 1;
        }
    }
}

/// The full keyboard state.
///
/// `caps_lock` / `num_lock` are the canonical lock flags; the `Shift` bit of
/// `locked` mirrors `caps_lock` so symbol resolution can use the modifier set
/// directly. A debug assertion keeps the two views consistent.
///
/// All collections are fixed-capacity / linear (see `keyset`): bounded by the
/// hard rollover cap, allocation-free in the hot path, and model-checkable.
/// Every scan is a constant-bound loop over the fixed arrays (slots at/after
/// the tracked length are `None` and skipped), so CBMC derives exact trip
/// bounds instead of unrolling a symbolic length.
#[derive(Debug, Clone)]
pub struct KeyboardState {
    settings: StateSettings,
    depressed: KeySet,
    latched: ModifierSet,
    locked: ModifierSet,
    caps_lock: bool,
    num_lock: bool,
    active_layer: Layer,
    /// Modifier physical keys injected on behalf of a pressed key (latch/lock
    /// consumption). Keyed by the non-modifier key they were injected for.
    injected_mods: [Option<(PhysicalKey, InjectedMods)>; MAX_HELD_KEYS],
    injected_mods_len: usize,
    tap_track: [Option<(PhysicalKey, TapTrack)>; MAX_HELD_KEYS],
    tap_track_len: usize,
    last_tap: [Option<Moment>; ModifierKind::COUNT],
}

impl KeyboardState {
    pub fn new(settings: StateSettings) -> Self {
        KeyboardState {
            settings,
            depressed: KeySet::new(),
            latched: ModifierSet::empty(),
            locked: ModifierSet::empty(),
            caps_lock: false,
            num_lock: false,
            active_layer: Layer::Base,
            injected_mods: [None; MAX_HELD_KEYS],
            injected_mods_len: 0,
            tap_track: [None; MAX_HELD_KEYS],
            tap_track_len: 0,
            last_tap: [None; ModifierKind::COUNT],
        }
    }

    // ── Accessors ────────────────────────────────────────────────────────

    pub fn depressed(&self) -> &KeySet {
        &self.depressed
    }

    pub fn latched(&self) -> ModifierSet {
        self.latched
    }

    pub fn locked(&self) -> ModifierSet {
        self.locked
    }

    pub fn caps_lock(&self) -> bool {
        self.caps_lock
    }

    pub fn num_lock(&self) -> bool {
        self.num_lock
    }

    pub fn active_layer(&self) -> Layer {
        self.active_layer
    }

    pub fn is_depressed(&self, key: PhysicalKey) -> bool {
        self.depressed.contains(key)
    }

    /// Modifiers currently physically held down.
    pub fn held_modifiers(&self) -> ModifierSet {
        let mut mods = ModifierSet::empty();
        let mut keys = [PhysicalKey::Escape; MAX_HELD_KEYS];
        let n = self.depressed.copy_into(&mut keys);
        let mut i = 0;
        while i < MAX_HELD_KEYS {
            if i < n {
                if let Some(kind) = keys[i].modifier_kind() {
                    mods.insert(kind.into());
                }
            }
            i += 1;
        }
        mods
    }

    /// Modifiers that currently apply to key presses: held + latched + locked.
    pub fn effective_modifiers(&self) -> ModifierSet {
        let mut mods = self.held_modifiers();
        mods = mods.union(self.latched);
        mods = mods.union(self.locked);
        mods
    }

    /// Number of keys currently held (for rollover accounting / UI display).
    pub fn held_count(&self) -> usize {
        self.depressed.len()
    }

    // ── Mutating operations ─────────────────────────────────────────────

    /// Press a key. Returns the events to deliver to the sink, in order.
    pub fn press(&mut self, key: PhysicalKey, now: Moment) -> Result<Vec<KeyEvent>, StateError> {
        if self.depressed.contains(key) {
            return Ok(Vec::new());
        }
        if self.depressed.len() >= self.settings.max_held_keys {
            return Err(StateError::Rollover {
                limit: self.settings.max_held_keys,
            });
        }

        let mut events = Vec::new();

        if let Some(kind) = key.modifier_kind() {
            // Pressing a modifier while any other modifier is held is a chord:
            // its release must not count as a tap.
            let chord = {
                let mut keys = [PhysicalKey::Escape; MAX_HELD_KEYS];
                let n = self.depressed.copy_into(&mut keys);
                let mut others_held = false;
                let mut i = 0;
                while i < MAX_HELD_KEYS {
                    if i < n {
                        if let Some(h) = keys[i].modifier_kind() {
                            if h != kind {
                                others_held = true;
                            }
                        }
                    }
                    i += 1;
                }
                others_held
            };
            self.depressed.insert(key);
            self.tap_track_push(
                key,
                TapTrack {
                    down_at: now,
                    interleaved: chord,
                },
            );
            events.push(KeyEvent::Down(key));
        } else if key.is_lock_key() {
            // Lock keys are tapped: toggle the mirror state on release.
            self.depressed.insert(key);
            events.push(KeyEvent::Down(key));
        } else {
            // A normal key press. Resolve the modifiers that must be engaged
            // (held ∪ latched ∪ locked), injecting the physical modifier keys
            // that are not already down.
            let effective = self.effective_modifiers();
            let held_mods = self.held_modifiers();
            let mut injected = InjectedMods::new();
            for kind in [
                ModifierKind::Shift,
                ModifierKind::Ctrl,
                ModifierKind::Alt,
                ModifierKind::AltGr,
                ModifierKind::Meta,
                ModifierKind::Fn,
            ] {
                if effective.contains(kind.into()) && !held_mods.contains(kind.into()) {
                    let phys = kind.preferred_key();
                    if !self.depressed.contains(phys) {
                        self.depressed.insert(phys);
                        injected.push(phys);
                        events.push(KeyEvent::Down(phys));
                    }
                }
            }
            if injected.len > 0 {
                self.injected_mods_push(key, injected);
            }
            self.depressed.insert(key);
            events.push(KeyEvent::Down(key));
            // The latch is consumed by the first key pressed after it.
            self.latched = ModifierSet::empty();
            // Any modifier still held now becomes part of a chord, so its
            // release must not count as a tap. Every tap-track entry belongs
            // to a currently held modifier (tracks are created at press and
            // removed at release), so marking them all is exactly the
            // interleave rule — one flat pass, no per-key lookup nesting.
            let mut i = 0;
            while i < MAX_HELD_KEYS {
                if let Some((_, t)) = &mut self.tap_track[i] {
                    t.interleaved = true;
                }
                i += 1;
            }
        }

        self.update_layer();
        Ok(events)
    }

    /// Release a key. Returns the events to deliver to the sink, in order.
    pub fn release(&mut self, key: PhysicalKey, now: Moment) -> Result<Vec<KeyEvent>, StateError> {
        if !self.depressed.contains(key) {
            return Ok(Vec::new());
        }

        let mut events = Vec::new();

        if let Some(kind) = key.modifier_kind() {
            let track = self.tap_track_remove(key);
            events.push(KeyEvent::Up(key));
            self.depressed.remove(key);

            let is_tap = match track {
                Some(t) => {
                    !t.interleaved
                        && now.saturating_duration_since(t.down_at) < self.settings.tap_timeout
                }
                None => false,
            };
            if is_tap && self.settings.latch_enabled {
                let double = self.settings.lock_enabled
                    && self.last_tap[kind.index()].is_some_and(|last| {
                        now.saturating_duration_since(last) < self.settings.double_tap_timeout
                    });
                if double {
                    self.toggle_lock(kind);
                    self.latched.remove(kind.into());
                    self.last_tap[kind.index()] = None;
                } else {
                    self.latched.insert(kind.into());
                    self.last_tap[kind.index()] = Some(now);
                }
            } else {
                self.last_tap[kind.index()] = None;
            }
        } else if key.is_lock_key() {
            events.push(KeyEvent::Up(key));
            self.depressed.remove(key);
            match key {
                PhysicalKey::CapsLock => self.toggle_caps_lock(),
                PhysicalKey::NumLock => self.num_lock = !self.num_lock,
                _ => {}
            }
        } else {
            events.push(KeyEvent::Up(key));
            self.depressed.remove(key);
            // Release any modifier keys that were injected for this key.
            if let Some(injected) = self.injected_mods_remove(key) {
                let mut i = 0;
                while i < MAX_INJECTED_MODS {
                    if i < injected.len && self.depressed.remove(injected.keys[i]) {
                        events.push(KeyEvent::Up(injected.keys[i]));
                    }
                    i += 1;
                }
            }
        }

        self.update_layer();
        Ok(events)
    }

    /// Release every depressed key (crash recovery, hide, disconnect, …).
    ///
    /// Locks (Caps Lock / Num Lock) intentionally persist: they are logical
    /// state, like a physical keyboard's LEDs.
    pub fn release_all(&mut self) -> Vec<KeyEvent> {
        let mut events = Vec::new();
        let mut keys = [PhysicalKey::Escape; MAX_HELD_KEYS];
        let n = self.depressed.copy_into(&mut keys);
        // Release non-modifiers first, modifiers last (deterministic order).
        let mut i = 0;
        while i < MAX_HELD_KEYS {
            let idx = MAX_HELD_KEYS - 1 - i;
            if idx < n && !keys[idx].is_modifier() {
                events.push(KeyEvent::Up(keys[idx]));
            }
            i += 1;
        }
        let mut i = 0;
        while i < MAX_HELD_KEYS {
            let idx = MAX_HELD_KEYS - 1 - i;
            if idx < n && keys[idx].is_modifier() {
                events.push(KeyEvent::Up(keys[idx]));
            }
            i += 1;
        }
        self.depressed.clear();
        self.injected_mods = [None; MAX_HELD_KEYS];
        self.injected_mods_len = 0;
        self.tap_track = [None; MAX_HELD_KEYS];
        self.tap_track_len = 0;
        self.latched = ModifierSet::empty();
        self.update_layer();
        events
    }

    // ── Internals ────────────────────────────────────────────────────────

    // ── linear fixed-capacity table helpers (constant-bound, allocation-free)
    //
    // Slots at/after the tracked length are always `None`, so every scan is a
    // constant-bound loop over the fixed array: CBMC derives the exact trip
    // bound and stale slots are skipped by the `Option` pattern.

    /// Append a tap-track entry (dropped at capacity — the state machine's
    /// rollover guard prevents reaching it).
    fn tap_track_push(&mut self, key: PhysicalKey, track: TapTrack) {
        if self.tap_track_len < MAX_HELD_KEYS {
            self.tap_track[self.tap_track_len] = Some((key, track));
            self.tap_track_len += 1;
        }
    }

    /// Find and remove the tap track for `key`, returning it if present.
    fn tap_track_remove(&mut self, key: PhysicalKey) -> Option<TapTrack> {
        let mut pos = None;
        let mut i = 0;
        while i < MAX_HELD_KEYS {
            if let Some((k, _)) = self.tap_track[i] {
                if k == key {
                    pos = Some(i);
                    break;
                }
            }
            i += 1;
        }
        let pos = pos?;
        let removed = self.tap_track[pos].unwrap().1;
        // Shift the tail left, then clear the vacated slot (constant bound).
        let mut k = 0;
        while k < MAX_HELD_KEYS {
            let j = k;
            if j >= pos && j + 1 < self.tap_track_len {
                self.tap_track[j] = self.tap_track[j + 1];
            }
            k += 1;
        }
        self.tap_track[self.tap_track_len - 1] = None;
        self.tap_track_len -= 1;
        Some(removed)
    }

    /// Find and remove the tap track for `key`, returning it if present.
    /// Append an injected-modifier record (dropped at capacity — the rollover
    /// guard prevents reaching it).
    fn injected_mods_push(&mut self, key: PhysicalKey, injected: InjectedMods) {
        if self.injected_mods_len < MAX_HELD_KEYS {
            self.injected_mods[self.injected_mods_len] = Some((key, injected));
            self.injected_mods_len += 1;
        }
    }

    /// Remove and return the injected-modifier record for `key`, if any.
    fn injected_mods_remove(&mut self, key: PhysicalKey) -> Option<InjectedMods> {
        let mut pos = None;
        let mut i = 0;
        while i < MAX_HELD_KEYS {
            if let Some((k, _)) = self.injected_mods[i] {
                if k == key {
                    pos = Some(i);
                    break;
                }
            }
            i += 1;
        }
        let pos = pos?;
        let removed = self.injected_mods[pos].unwrap().1;
        // Shift the tail left, then clear the vacated slot (constant bound).
        let mut k = 0;
        while k < MAX_HELD_KEYS {
            let j = k;
            if j >= pos && j + 1 < self.injected_mods_len {
                self.injected_mods[j] = self.injected_mods[j + 1];
            }
            k += 1;
        }
        self.injected_mods[self.injected_mods_len - 1] = None;
        self.injected_mods_len -= 1;
        Some(removed)
    }

    fn toggle_caps_lock(&mut self) {
        self.caps_lock = !self.caps_lock;
        if self.caps_lock {
            self.locked.insert(ModifierSet::SHIFT);
        } else {
            self.locked.remove(ModifierSet::SHIFT);
        }
    }

    fn toggle_lock(&mut self, kind: ModifierKind) {
        match kind {
            ModifierKind::Shift => self.toggle_caps_lock(),
            other => {
                let bit: ModifierSet = other.into();
                if self.locked.contains(bit) {
                    self.locked.remove(bit);
                } else {
                    self.locked.insert(bit);
                }
            }
        }
    }

    fn update_layer(&mut self) {
        self.active_layer = Layer::from_modifiers(self.effective_modifiers());
        debug_assert_eq!(self.locked.contains(ModifierSet::SHIFT), self.caps_lock);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Moment {
        Moment::from_millis(1_000_000)
    }

    fn settings() -> StateSettings {
        StateSettings::default()
    }

    #[test]
    fn simple_tap_emits_down_up() {
        let mut s = KeyboardState::new(settings());
        let now = t0();
        assert_eq!(
            s.press(PhysicalKey::A, now).unwrap(),
            vec![KeyEvent::Down(PhysicalKey::A)]
        );
        assert_eq!(
            s.release(PhysicalKey::A, now + Duration::from_millis(50))
                .unwrap(),
            vec![KeyEvent::Up(PhysicalKey::A)]
        );
        assert!(s.depressed().is_empty());
    }

    #[test]
    fn chord_shift_a() {
        let mut s = KeyboardState::new(settings());
        let now = t0();
        let ev = s.press(PhysicalKey::LeftShift, now).unwrap();
        assert_eq!(ev, vec![KeyEvent::Down(PhysicalKey::LeftShift)]);

        let ev = s
            .press(PhysicalKey::A, now + Duration::from_millis(30))
            .unwrap();
        // Shift is already down: no injection needed.
        assert_eq!(ev, vec![KeyEvent::Down(PhysicalKey::A)]);

        let ev = s
            .release(PhysicalKey::A, now + Duration::from_millis(60))
            .unwrap();
        assert_eq!(ev, vec![KeyEvent::Up(PhysicalKey::A)]);

        // Shift was interleaved (A pressed while held) ⇒ not a tap, no latch.
        let ev = s
            .release(PhysicalKey::LeftShift, now + Duration::from_millis(90))
            .unwrap();
        assert_eq!(ev, vec![KeyEvent::Up(PhysicalKey::LeftShift)]);
        assert!(s.latched().is_empty());
    }

    #[test]
    fn tap_shift_latches_next_key() {
        let mut s = KeyboardState::new(settings());
        let now = t0();
        // Tap shift (fast press+release, nothing in between).
        s.press(PhysicalKey::LeftShift, now).unwrap();
        s.release(PhysicalKey::LeftShift, now + Duration::from_millis(80))
            .unwrap();
        assert!(s.latched().contains(ModifierSet::SHIFT));

        // Next key press injects the shift and consumes the latch.
        let ev = s
            .press(PhysicalKey::A, now + Duration::from_millis(200))
            .unwrap();
        assert_eq!(
            ev,
            vec![
                KeyEvent::Down(PhysicalKey::LeftShift),
                KeyEvent::Down(PhysicalKey::A),
            ]
        );
        assert!(s.latched().is_empty(), "latch consumed on first key press");

        // Releasing A releases the injected shift too.
        let ev = s
            .release(PhysicalKey::A, now + Duration::from_millis(300))
            .unwrap();
        assert_eq!(
            ev,
            vec![
                KeyEvent::Up(PhysicalKey::A),
                KeyEvent::Up(PhysicalKey::LeftShift)
            ]
        );
        assert!(s.depressed().is_empty());
    }

    #[test]
    fn double_tap_shift_toggles_caps_lock() {
        let mut s = KeyboardState::new(settings());
        let now = t0();
        // Tap 1
        s.press(PhysicalKey::RightShift, now).unwrap();
        s.release(PhysicalKey::RightShift, now + Duration::from_millis(50))
            .unwrap();
        assert!(s.latched().contains(ModifierSet::SHIFT));
        // Tap 2 within window
        s.press(PhysicalKey::RightShift, now + Duration::from_millis(100))
            .unwrap();
        s.release(PhysicalKey::RightShift, now + Duration::from_millis(150))
            .unwrap();
        assert!(s.caps_lock(), "double-tap should lock shift");
        assert!(s.latched().is_empty(), "lock replaces latch");

        // Caps lock now applies shift to keys.
        let ev = s
            .press(PhysicalKey::A, now + Duration::from_millis(200))
            .unwrap();
        assert_eq!(
            ev,
            vec![
                KeyEvent::Down(PhysicalKey::LeftShift),
                KeyEvent::Down(PhysicalKey::A)
            ]
        );

        // Release A (releases the injected shift) so the next taps are clean.
        s.release(PhysicalKey::A, now + Duration::from_millis(250))
            .unwrap();
        assert!(s.depressed().is_empty());

        // Toggle off again with another double-tap.
        s.press(PhysicalKey::LeftShift, now + Duration::from_millis(300))
            .unwrap();
        s.release(PhysicalKey::LeftShift, now + Duration::from_millis(340))
            .unwrap();
        s.press(PhysicalKey::LeftShift, now + Duration::from_millis(380))
            .unwrap();
        s.release(PhysicalKey::LeftShift, now + Duration::from_millis(420))
            .unwrap();
        assert!(!s.caps_lock());
    }

    #[test]
    fn slow_release_is_not_a_tap() {
        let mut s = KeyboardState::new(settings());
        let now = t0();
        s.press(PhysicalKey::LeftShift, now).unwrap();
        // Hold much longer than tap_timeout.
        s.release(PhysicalKey::LeftShift, now + Duration::from_secs(1))
            .unwrap();
        assert!(s.latched().is_empty());
    }

    #[test]
    fn caps_lock_key_toggles_mirror() {
        let mut s = KeyboardState::new(settings());
        let now = t0();
        s.press(PhysicalKey::CapsLock, now).unwrap();
        let ev = s
            .release(PhysicalKey::CapsLock, now + Duration::from_millis(40))
            .unwrap();
        assert_eq!(ev, vec![KeyEvent::Up(PhysicalKey::CapsLock)]);
        assert!(s.caps_lock());
        assert!(s.locked().contains(ModifierSet::SHIFT));
        assert_eq!(s.active_layer(), Layer::Shift);
    }

    #[test]
    fn altgr_layer_resolution() {
        let mut s = KeyboardState::new(settings());
        let now = t0();
        s.press(PhysicalKey::RightAlt, now).unwrap();
        assert_eq!(s.active_layer(), Layer::AltGr);
        assert!(s.effective_modifiers().contains(ModifierSet::ALTGR));
        // Release after tap_timeout ⇒ not a tap ⇒ no latch.
        s.release(PhysicalKey::RightAlt, now + Duration::from_secs(1))
            .unwrap();
        assert_eq!(s.active_layer(), Layer::Base);
        assert!(s.latched().is_empty());
    }

    #[test]
    fn quick_altgr_tap_latches_like_shift() {
        let mut s = KeyboardState::new(settings());
        let now = t0();
        s.press(PhysicalKey::RightAlt, now).unwrap();
        s.release(PhysicalKey::RightAlt, now + Duration::from_millis(60))
            .unwrap();
        assert!(s.latched().contains(ModifierSet::ALTGR));
    }

    #[test]
    fn modifier_pressed_while_other_modifier_held_is_chord_not_tap() {
        let mut s = KeyboardState::new(settings());
        let now = t0();
        s.press(PhysicalKey::LeftShift, now).unwrap();
        // Tap Ctrl while Shift is held ⇒ release must not latch Ctrl.
        s.press(PhysicalKey::LeftCtrl, now + Duration::from_millis(20))
            .unwrap();
        s.release(PhysicalKey::LeftCtrl, now + Duration::from_millis(60))
            .unwrap();
        assert!(!s.latched().contains(ModifierSet::CTRL));
        s.release_all();
    }

    #[test]
    fn shift_tapped_while_letter_held_still_latches() {
        let mut s = KeyboardState::new(settings());
        let now = t0();
        s.press(PhysicalKey::A, now).unwrap();
        // Tapping shift while a letter is held should still latch (the letter
        // was pressed before the modifier, so it isn't a chord).
        s.press(PhysicalKey::LeftShift, now + Duration::from_millis(20))
            .unwrap();
        s.release(PhysicalKey::LeftShift, now + Duration::from_millis(60))
            .unwrap();
        assert!(s.latched().contains(ModifierSet::SHIFT));
        s.release_all();
    }

    #[test]
    fn rollover_cap_is_enforced() {
        let mut settings = settings();
        settings.max_held_keys = 3;
        let mut s = KeyboardState::new(settings);
        let now = t0();
        s.press(PhysicalKey::A, now).unwrap();
        s.press(PhysicalKey::S, now + Duration::from_millis(10))
            .unwrap();
        s.press(PhysicalKey::D, now + Duration::from_millis(20))
            .unwrap();
        assert_eq!(
            s.press(PhysicalKey::F, now + Duration::from_millis(30)),
            Err(StateError::Rollover { limit: 3 })
        );
    }

    #[test]
    fn duplicate_press_is_ignored() {
        let mut s = KeyboardState::new(settings());
        let now = t0();
        s.press(PhysicalKey::A, now).unwrap();
        assert_eq!(
            s.press(PhysicalKey::A, now + Duration::from_millis(10))
                .unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn release_all_clears_everything() {
        let mut s = KeyboardState::new(settings());
        let now = t0();
        s.press(PhysicalKey::LeftCtrl, now).unwrap();
        s.press(PhysicalKey::A, now + Duration::from_millis(10))
            .unwrap();
        let ev = s.release_all();
        // Non-modifiers first, then modifiers.
        assert_eq!(
            ev,
            vec![
                KeyEvent::Up(PhysicalKey::A),
                KeyEvent::Up(PhysicalKey::LeftCtrl)
            ]
        );
        assert!(s.depressed().is_empty());
        assert!(s.latched().is_empty());
    }

    #[test]
    fn latch_consumed_by_first_key() {
        let mut s = KeyboardState::new(settings());
        let now = t0();
        s.press(PhysicalKey::LeftShift, now).unwrap();
        s.release(PhysicalKey::LeftShift, now + Duration::from_millis(50))
            .unwrap();
        // Press a, then (still holding) b: only a consumes the latch.
        s.press(PhysicalKey::A, now + Duration::from_millis(100))
            .unwrap();
        let ev_b = s
            .press(PhysicalKey::B, now + Duration::from_millis(120))
            .unwrap();
        // Shift physically down (injected for a) ⇒ no new injection for b.
        assert_eq!(ev_b, vec![KeyEvent::Down(PhysicalKey::B)]);
        s.release_all();
    }

    #[test]
    fn held_shift_with_latch_does_not_double_inject() {
        let mut s = KeyboardState::new(settings());
        let now = t0();
        // Tap to latch…
        s.press(PhysicalKey::LeftShift, now).unwrap();
        s.release(PhysicalKey::LeftShift, now + Duration::from_millis(50))
            .unwrap();
        // …then hold shift physically and press A.
        s.press(PhysicalKey::LeftShift, now + Duration::from_millis(80))
            .unwrap();
        let ev = s
            .press(PhysicalKey::A, now + Duration::from_millis(90))
            .unwrap();
        // Shift already down ⇒ no injection.
        assert_eq!(ev, vec![KeyEvent::Down(PhysicalKey::A)]);
        // Release A: injected list empty ⇒ nothing extra released.
        let ev = s
            .release(PhysicalKey::A, now + Duration::from_millis(120))
            .unwrap();
        assert_eq!(ev, vec![KeyEvent::Up(PhysicalKey::A)]);
        s.release_all();
    }
}
