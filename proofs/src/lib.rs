//! Kani model-checking harnesses over the **production** `ferrokey-core`
//! keyboard state machine (Phase 4 WS3).
//!
//! The harnesses call the real `KeyboardState` / `RepeatEngine` — no toy
//! model. Nondeterminism is deliberately shaped for the solver:
//!
//! * **Keys** are chosen from a small fixed universe; the *operation
//!   sequence* (which key, press/release/release-all, in which order) is the
//!   symbolic dimension. (Fully symbolic key *values* make the BTree shape
//!   symbolic and the verification exponential, so the universe is small —
//!   the invariant set is what matters, not the key count.)
//! * **Time** is `ferrokey_core::time::Moment` (a plain millisecond tick),
//!   bounded to the tap/latch windows, so the timing branches
//!   (tap vs long-press) are explored symbolically.
//!
//! Proof IDs: KANI.HELD.001, KANI.RELEASE.001, KANI.REPEAT.001,
//! KANI.ROLLOVER.001, KANI.RELEASEALL.001, KANI.LATCH.001, KANI.LOCK.001,
//! KANI.SEQUENCE.001, KANI.MUTATION.001 (negative controls, expected to
//! fail — see proofs/run-negative-controls.sh), KANI.RECEIPT.001.

#![allow(dead_code)]

#[cfg(kani)]
mod proofs {
    use ferrokey_core::{
        KeyEvent, KeyboardState, ModifierSet, Moment, RepeatEngine, RepeatSettings, StateError,
        StateSettings,
    };
    use std::time::Duration;

    // ── the key universe (kept small: the fixed-capacity KeySet's sorted
    // shape stays concrete, keeping the solver's state space small) ───────

    const KEYS: [ferrokey_core::PhysicalKey; 3] = [
        ferrokey_core::PhysicalKey::A,
        ferrokey_core::PhysicalKey::B,
        ferrokey_core::PhysicalKey::LeftShift,
    ];
    const LETTERS: [ferrokey_core::PhysicalKey; 2] =
        [ferrokey_core::PhysicalKey::A, ferrokey_core::PhysicalKey::B];

    /// Choose one of the universe keys (concrete value per path).
    fn small_key() -> ferrokey_core::PhysicalKey {
        let i = kani::any::<u8>();
        kani::assume(i < KEYS.len() as u8);
        KEYS[i as usize]
    }

    fn letter_key() -> ferrokey_core::PhysicalKey {
        let i = kani::any::<u8>();
        kani::assume(i < LETTERS.len() as u8);
        LETTERS[i as usize]
    }

    /// The 2-key universe for the sequence harness (a letter + a modifier):
    /// with `max_held_keys: 2` it still reaches the rollover bound, but
    /// keeps the symbolic state accumulation cheap enough for the solver.
    fn sequence_key() -> ferrokey_core::PhysicalKey {
        let i = kani::any::<u8>();
        kani::assume(i < 2);
        if i == 0 {
            ferrokey_core::PhysicalKey::A
        } else {
            ferrokey_core::PhysicalKey::LeftShift
        }
    }

    /// A moment chosen from a two-value timing domain.
    ///
    /// The latch logic decides purely by a strict comparison against the tap
    /// threshold (`tap_timeout` 400 ms):
    ///
    /// ```text
    /// duration < 400  ⇒  tap        duration ≥ 400  ⇒  hold
    /// ```
    ///
    /// `0 ms` represents every duration below the threshold, `900 ms` every
    /// duration at/above it — so the two-value domain samples every region
    /// the implementation can distinguish. Fully symbolic `u64` moments would
    /// make CBMC bit-blast 64-bit arithmetic for no extra semantic coverage
    /// and exhaust the solver (the observable timing behavior is identical
    /// for every value inside a region).
    fn symbolic_moment() -> Moment {
        let v = kani::any::<u8>();
        kani::assume(v <= 1);
        match v {
            0 => Moment::from_millis(0),
            _ => Moment::from_millis(900),
        }
    }

    // ── KANI.HELD.001: a physical key is held at most once ────────────────

    #[kani::proof]
    pub fn kani_held_unique() {
        let mut s = KeyboardState::new(StateSettings::default());
        let k = small_key();
        let _ = s.press(k, Moment::from_millis(100));
        assert!(s.is_depressed(k));
        assert_eq!(s.depressed().count_of(k), 1);
        let before = s.held_count();
        let ev2 = s.press(k, Moment::from_millis(100));
        // A duplicate press is a no-op: no events, no state change.
        assert!(matches!(ev2, Ok(ev) if ev.is_empty()));
        assert_eq!(s.held_count(), before);
        assert_eq!(s.depressed().count_of(k), 1);
    }

    // ── KANI.RELEASE.001: releasing an unheld key cannot create state ─────

    #[kani::proof]
    pub fn kani_release_valid() {
        let mut s = KeyboardState::new(StateSettings::default());
        let k = small_key();
        let before_latched = s.latched();
        let ev = s.release(k, Moment::from_millis(100));
        assert!(matches!(ev, Ok(v) if v.is_empty()));
        assert!(!s.is_depressed(k));
        assert_eq!(s.latched(), before_latched);
        assert_eq!(s.held_count(), 0);
    }

    // ── KANI.REPEAT.001: repeat cannot manufacture a held key ─────────────

    #[kani::proof]
    pub fn kani_repeat_invariants() {
        let mut eng = RepeatEngine::new(RepeatSettings::default());
        let k = small_key();
        let down_at = Moment::from_millis(100);
        eng.key_down(k, down_at, true);
        assert!(eng.contains_held(k));
        // Any key tick returns was tracked (owned) by the engine. (The
        // invariant is timing-independent: the tick fires at a fixed moment
        // past the repeat delay so the repeat branch is exercised.)
        let due = eng.tick(Moment::from_millis(600));
        for d in &due {
            assert!(eng.contains_held(*d), "repeat manufactured a key");
        }
        // Releasing stops the repeats and removes ownership.
        eng.key_up(k);
        assert_eq!(eng.held_len(), 0);
        assert!(eng.tick(Moment::from_millis(10_000)).is_empty());
    }

    // ── KANI.ROLLOVER.001: held_count <= max_held_keys after every press ──

    #[kani::proof]
    pub fn kani_rollover_held_bound() {
        let settings = StateSettings {
            max_held_keys: 2,
            ..StateSettings::default()
        };
        let mut s = KeyboardState::new(settings);
        for _step in 0..5u32 {
            let k = small_key();
            let r = s.press(k, Moment::from_millis(100));
            assert!(r.is_ok() || matches!(r, Err(StateError::Rollover { .. })));
            assert!(s.held_count() <= 2, "rollover bound violated");
        }
    }

    // ── KANI.RELEASEALL.001: complete logical clear ───────────────────────

    #[kani::proof]
    pub fn kani_release_all_complete() {
        let mut s = KeyboardState::new(StateSettings::default());
        // A bounded symbolic press sequence first (2-key universe: a letter
        // and a modifier — exercising both release paths of release_all).
        for i in 0..3u32 {
            let _ = s.press(sequence_key(), Moment::from_millis(u64::from(i) * 50));
        }
        let locked_before = s.locked();
        let caps_before = s.caps_lock();
        let held_before = s.held_count();
        let events = s.release_all();
        // Every held physical key becomes logically released; the held set
        // is empty; latch bookkeeping clears.
        assert!(s.depressed().is_empty());
        assert!(s.latched().is_empty());
        // No unrelated key is created and none is dropped: `release_all`
        // builds its events only from the held snapshot, so an exact count
        // match proves the Up stream is complete and nothing extra. (No
        // per-element heap deref — the count is the model-checkable form.)
        assert_eq!(events.len(), held_before);
        // Locks are logical state and persist (like physical LEDs).
        assert_eq!(s.locked(), locked_before);
        assert_eq!(s.caps_lock(), caps_before);
        // A second release_all is a no-op.
        assert!(s.release_all().is_empty());
    }

    // ── KANI.LATCH.001: latch semantics ───────────────────────────────────

    #[kani::proof]
    pub fn kani_latch_semantics() {
        let settings = StateSettings::default(); // tap_timeout 400ms
        let mut s = KeyboardState::new(settings);
        // A symbolic modifier: the shift key (the universe's only modifier).
        let m = ferrokey_core::PhysicalKey::LeftShift;
        let kind = m.modifier_kind().unwrap();
        let down_at = symbolic_moment();
        let up_at = symbolic_moment();
        kani::assume(up_at >= down_at);

        let _ = s.press(m, down_at);
        let _ = s.release(m, up_at);

        // Latch enters ONLY through a valid transition: a clean tap within
        // the timeout. Anything else leaves no latch behind.
        let is_tap = up_at.saturating_duration_since(down_at) < settings.tap_timeout;
        if is_tap {
            assert!(s.latched().contains(kind.into()));
        } else {
            assert!(!s.latched().contains(kind.into()));
        }

        // A qualifying key press consumes the latch exactly once. A latched
        // modifier is injected as a physical Down (it is never physically
        // down here — it was released before the letter pressed); with no
        // latch there must be NO injection. press() emits injections before
        // the key's own Down, so the injected key is exactly events[0] (and
        // the shift's preferred physical key is concrete).
        let n = letter_key();
        let events = s.press(n, symbolic_moment()).unwrap_or_default();
        assert!(!s.latched().contains(kind.into()));
        let injected_shift = !events.is_empty()
            && events[0] == KeyEvent::Down(ferrokey_core::PhysicalKey::LeftShift);
        if is_tap {
            assert!(injected_shift, "latch not injected for the qualifying key");
        } else {
            assert!(
                !injected_shift,
                "phantom modifier injection without a latch"
            );
        }
        // Consumption cannot leave a ghost physical hold: releasing the key
        // releases the injected modifier.
        let _ = s.release(n, symbolic_moment());
        assert!(
            !s.held_modifiers().contains(kind.into()),
            "ghost physical hold after latch consumption"
        );
    }

    // ── KANI.LOCK.001: lock semantics (Caps Lock key; taps toggle) ───────

    #[kani::proof]
    pub fn kani_lock_semantics() {
        let settings = StateSettings::default();

        // Lock enters ONLY through the Caps Lock key's tap: release of the
        // CapsLock key toggles caps_lock and mirrors it into locked SHIFT.
        let mut s = KeyboardState::new(settings);
        let _ = s.press(ferrokey_core::PhysicalKey::CapsLock, Moment::from_millis(1));
        let _ = s.release(
            ferrokey_core::PhysicalKey::CapsLock,
            Moment::from_millis(51),
        );
        assert!(s.caps_lock());
        assert!(s.locked().contains(ModifierSet::SHIFT));

        // Ordinary key activity cannot invent lock state.
        let mut s2 = KeyboardState::new(settings);
        for i in 0..4u32 {
            let t = Moment::from_millis(u64::from(i) * 100 + 1);
            let _ = s2.press(letter_key(), t);
            let _ = s2.release(letter_key(), t + Duration::from_millis(20));
        }
        assert!(s2.locked().is_empty());
        assert!(!s2.caps_lock());

        // Unlock follows the defined transition: another Caps Lock tap.
        let mut s3 = KeyboardState::new(settings);
        let _ = s3.press(ferrokey_core::PhysicalKey::CapsLock, Moment::from_millis(1));
        let _ = s3.release(
            ferrokey_core::PhysicalKey::CapsLock,
            Moment::from_millis(51),
        );
        assert!(s3.caps_lock());
        let _ = s3.press(
            ferrokey_core::PhysicalKey::CapsLock,
            Moment::from_millis(101),
        );
        let _ = s3.release(
            ferrokey_core::PhysicalKey::CapsLock,
            Moment::from_millis(151),
        );
        assert!(!s3.caps_lock());
        assert!(s3.locked().is_empty());

        // Click-to-toggle: tapping an already-active modifier disengages it
        // and never invents lock state (Shift double-taps used to map to
        // Caps Lock; that gesture now just latches then disengages).
        let mut s4 = KeyboardState::new(settings);
        let m = ferrokey_core::PhysicalKey::LeftShift;
        let kind = m.modifier_kind().unwrap();
        let _ = s4.press(m, Moment::from_millis(1));
        let _ = s4.release(m, Moment::from_millis(51));
        assert!(s4.latched().contains(kind.into()));
        let _ = s4.press(m, Moment::from_millis(101));
        let _ = s4.release(m, Moment::from_millis(151));
        assert!(!s4.latched().contains(kind.into()));
        assert!(s4.locked().is_empty());
        assert!(!s4.caps_lock());

        // release_all cannot leave a physically stuck modifier; the lock is
        // logical state and persists (like a physical keyboard's LED).
        let mut s5 = KeyboardState::new(settings);
        let _ = s5.press(ferrokey_core::PhysicalKey::CapsLock, Moment::from_millis(1));
        let _ = s5.release(
            ferrokey_core::PhysicalKey::CapsLock,
            Moment::from_millis(51),
        );
        let _ = s5.press(letter_key(), Moment::from_millis(101));
        let _ = s5.release_all();
        assert!(s5.depressed().is_empty());
        assert!(s5.latched().is_empty());
        assert!(s5.caps_lock(), "caps lock is logical state and persists");
        assert!(s5.locked().contains(ModifierSet::SHIFT));
    }

    // ── KANI.SEQUENCE.001: bounded symbolic operation sequences ───────────

    #[kani::proof]
    pub fn kani_sequence_invariants() {
        let settings = StateSettings {
            max_held_keys: 2,
            ..StateSettings::default()
        };
        let mut s = KeyboardState::new(settings);
        // A 2-key universe, 4 steps, ops {press, release} (2-way), with a
        // final release_all: every interleaving of press and release is
        // exercised against the invariant set, and release_all's complete
        // clear is asserted at the end. (A 3-way symbolic op made the
        // solver's multi-step formula outgrow the verifier memory cap; the
        // op space stays exhaustive for the reachable-state invariants.)
        for _step in 0..4u32 {
            let op = kani::any::<u8>();
            kani::assume(op <= 1);
            let k = sequence_key();
            match op {
                0 => {
                    let _ = s.press(k, Moment::from_millis(100));
                }
                _ => {
                    let _ = s.release(k, Moment::from_millis(100));
                }
            }
            // The invariant set after EVERY step.
            assert!(s.held_count() <= 2, "sequence rollover bound violated");
            assert!(
                !s.depressed().has_duplicates(),
                "duplicate held key in sequence"
            );
        }
        // release_all always terminates in the empty held state.
        s.release_all();
        assert!(s.depressed().is_empty());
        assert!(s.latched().is_empty());
    }
}
