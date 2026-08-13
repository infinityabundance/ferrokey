//! Text-mode input: the second input channel.
//!
//! Keyboard mode sends raw key events; **text mode** types *characters* by
//! resolving each character against the active layout (base / shift / altgr
//! levels) and emitting the corresponding chord. Characters the layout cannot
//! produce are reported as errors — Ferrokey **never silently substitutes
//! clipboard paste** for keyboard injection.
//!
//! Text mode also hosts the compose engine ([`ferrokey_core::ComposeEngine`]):
//! dead-key accents and the compose key are handled here, so `' + e → é` works
//! even when the OSK layout declares the accent but the caller only expected
//! plain characters.

use ferrokey_core::{
    ComposeEngine, FeedOutcome, KeyAction, KeySymbol, KeyboardDriver, ModifierSet, PhysicalKey,
    TextError, VirtualKey,
};
use std::sync::Arc;
use std::time::Instant;

/// What the caller should do after a text-mode key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextModeOutcome {
    /// The key was handled by text mode (consumed by the compose engine or
    /// typed as text).
    Handled,
    /// The key is not text input (modifier, navigation, …): the caller should
    /// process it through the normal keyboard path.
    FallThrough,
}

/// Stateful text-mode handler: the compose engine plus layout-driven character
/// typing over a [`KeyboardDriver`].
///
/// Text-mode character presses are typed on the pointer-down (tap semantics);
/// the OSK's *keyboard* mode is the autorepeat path (hold-to-repeat flows
/// through the repeat engine). Navigation keys (Backspace, arrows, …) fall
/// through to the keyboard path, so they repeat normally in text mode too.
/// This is the documented Phase-1 behaviour: full hold-to-repeat for typed
/// characters in text mode is a later milestone.
pub struct TextComposer {
    engine: ComposeEngine,
    layout: Arc<ferrokey_core::Layout>,
}

impl TextComposer {
    pub fn new(layout: Arc<ferrokey_core::Layout>) -> Self {
        TextComposer {
            engine: ComposeEngine::new(),
            layout,
        }
    }

    /// The current compose-state label ("" when idle), for the UI hint.
    pub fn pending_label(&self) -> String {
        self.engine.pending_label()
    }

    /// Cancel any pending compose sequence (hide, disconnect, release-all, …).
    pub fn reset(&mut self) {
        self.engine.reset();
    }

    /// Handle one *text-mode* key press.
    ///
    /// `effective_mods` must be the driver's current effective modifier set:
    /// it is used both to interpret the pressed symbol and to find the
    /// physical key that produces the composed text, so a latched shift is
    /// consumed exactly once instead of being re-engaged.
    pub fn key_down(
        &mut self,
        driver: &mut KeyboardDriver,
        symbol: &KeySymbol,
        effective_mods: ModifierSet,
    ) -> Result<TextModeOutcome, TextError> {
        match symbol {
            KeySymbol::Char(_) | KeySymbol::Dead(_) | KeySymbol::Compose => {
                let layout = self.layout.clone();
                let mut reprocess: Option<KeySymbol> = None;
                // Bounded reprocessing loop: a Reset with `reprocess` re-enters
                // the engine (e.g. compose q w → emit 'q', reprocess 'w'). The
                // tables guarantee at most one re-entry; the bound is defensive.
                for _ in 0..4 {
                    let symbol = reprocess.take().unwrap_or_else(|| symbol.clone());
                    match self.engine.feed(&symbol) {
                        FeedOutcome::Pass => {
                            if let KeySymbol::Char(c) = symbol {
                                type_text(driver, &c.to_string(), |c| {
                                    layout.find_char(c, effective_mods)
                                })?;
                            }
                            return Ok(TextModeOutcome::Handled);
                        }
                        FeedOutcome::Consumed | FeedOutcome::Cancelled => {
                            return Ok(TextModeOutcome::Handled)
                        }
                        FeedOutcome::Emit(chars) => {
                            let text: String = chars.iter().collect();
                            type_text(driver, &text, |c| layout.find_char(c, effective_mods))?;
                            return Ok(TextModeOutcome::Handled);
                        }
                        FeedOutcome::Reset {
                            standalone,
                            reprocess: next,
                        } => {
                            if !standalone.is_empty() {
                                let text: String = standalone.iter().collect();
                                type_text(driver, &text, |c| layout.find_char(c, effective_mods))?;
                            }
                            reprocess = next;
                            if reprocess.is_none() {
                                return Ok(TextModeOutcome::Handled);
                            }
                        }
                    }
                }
                // Unreachable with the current tables; be safe.
                Ok(TextModeOutcome::Handled)
            }
            KeySymbol::Name(_) | KeySymbol::None => {
                // Non-text keys cancel any pending compose state and let the
                // caller process them as real keys (documented simplification:
                // dead + BackSpace behaves like the accent was never pressed).
                self.engine.reset();
                Ok(TextModeOutcome::FallThrough)
            }
        }
    }
}

/// Type `text` through the driver using the active layout.
///
/// `resolve` is called per character and returns the physical key plus the
/// extra modifiers required (matching `Layout::find_char` semantics).
pub fn type_text<F>(
    driver: &mut KeyboardDriver,
    text: &str,
    mut resolve: F,
) -> Result<(), TextError>
where
    F: FnMut(char) -> Option<(PhysicalKey, ModifierSet)>,
{
    let now = Instant::now();
    for c in text.chars() {
        let (key, extra) = resolve(c).ok_or(TextError::Unmappable(c))?;
        // Engage the required modifiers (in a deterministic order).
        let mods = extra.iter().collect::<Vec<_>>();
        for kind in &mods {
            driver
                .handle_action(
                    KeyAction::Down,
                    VirtualKey::Physical(kind.preferred_key()),
                    now,
                )
                .map_err(|e| TextError::Backend(e.to_string()))?;
        }
        driver
            .handle_action(KeyAction::Tap, VirtualKey::Physical(key), now)
            .map_err(|e| TextError::Backend(e.to_string()))?;
        // Release the modifiers again.
        for kind in mods.iter().rev() {
            driver
                .handle_action(
                    KeyAction::Up,
                    VirtualKey::Physical(kind.preferred_key()),
                    now,
                )
                .map_err(|e| TextError::Backend(e.to_string()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrokey_core::{
        DeadKey, KeyEvent, KeySink, Layout, RepeatSettings, SinkError, StateSettings,
    };
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Recorder {
        events: Rc<RefCell<Vec<KeyEvent>>>,
    }

    impl KeySink for Recorder {
        fn key_down(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
            self.events.borrow_mut().push(KeyEvent::Down(key));
            Ok(())
        }
        fn key_up(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
            self.events.borrow_mut().push(KeyEvent::Up(key));
            Ok(())
        }
        fn release_all(&mut self) -> Result<(), SinkError> {
            Ok(())
        }
    }

    fn recorder() -> Rc<RefCell<Vec<KeyEvent>>> {
        Rc::new(RefCell::new(Vec::new()))
    }

    fn driver_with(events: &Rc<RefCell<Vec<KeyEvent>>>, layout: Arc<Layout>) -> KeyboardDriver {
        KeyboardDriver::new(
            StateSettings::default(),
            RepeatSettings::default(),
            layout,
            Box::new(Recorder {
                events: events.clone(),
            }),
        )
    }

    fn us_layout() -> Arc<Layout> {
        Arc::new(ferrokey_layouts::builtin("us").unwrap())
    }

    fn us_intl_layout() -> Arc<Layout> {
        Arc::new(ferrokey_layouts::builtin("us-intl").unwrap())
    }

    #[test]
    fn types_plain_ascii() {
        let events = recorder();
        let layout = us_layout();
        let mut driver = driver_with(&events, layout.clone());
        let mut composer = TextComposer::new(layout.clone());
        for c in ['a', 'b', 'c'] {
            composer
                .key_down(&mut driver, &KeySymbol::Char(c), ModifierSet::empty())
                .unwrap();
        }
        let events = events.borrow().clone();
        // a, b, c taps: down+up each.
        assert_eq!(events.len(), 6);
        assert_eq!(events[0], KeyEvent::Down(PhysicalKey::A));
        assert_eq!(events[5], KeyEvent::Up(PhysicalKey::C));
    }

    #[test]
    fn shifted_char_injects_shift() {
        let events = recorder();
        let layout = us_layout();
        let mut driver = driver_with(&events, layout.clone());
        let mut composer = TextComposer::new(layout.clone());
        let now = Instant::now();
        // Latch shift through the real keyboard path (quick tap), exactly like
        // a user clicking the OSK shift key.
        driver
            .handle_action(
                KeyAction::Down,
                VirtualKey::Physical(PhysicalKey::LeftShift),
                now,
            )
            .unwrap();
        driver
            .handle_action(
                KeyAction::Up,
                VirtualKey::Physical(PhysicalKey::LeftShift),
                now + std::time::Duration::from_millis(60),
            )
            .unwrap();
        let effective = driver.state().effective_modifiers();
        assert!(effective.contains(ModifierSet::SHIFT));
        // The pressed 'a' resolves to 'A' under the latched shift.
        let symbol = layout
            .symbol_for(PhysicalKey::A, effective)
            .unwrap()
            .clone();
        assert_eq!(symbol, KeySymbol::Char('A'));
        events.borrow_mut().clear();
        composer.key_down(&mut driver, &symbol, effective).unwrap();
        let events = events.borrow().clone();
        assert_eq!(
            events,
            vec![
                KeyEvent::Down(PhysicalKey::LeftShift), // injected for the latch
                KeyEvent::Down(PhysicalKey::A),
                KeyEvent::Up(PhysicalKey::A),
                KeyEvent::Up(PhysicalKey::LeftShift), // latch consumed
            ]
        );
        // The latch is consumed: the next key is unshifted.
        assert!(driver.state().latched().is_empty());
    }

    #[test]
    fn altgr_char_uses_held_modifier() {
        let events = recorder();
        let layout = us_intl_layout();
        let mut driver = driver_with(&events, layout.clone());
        let mut composer = TextComposer::new(layout.clone());
        // AltGr is physically held; e resolves to é and is typed with the
        // modifier already down (no re-engagement).
        let effective = ModifierSet::ALTGR;
        let symbol = layout
            .symbol_for(PhysicalKey::E, effective)
            .unwrap()
            .clone();
        assert_eq!(symbol, KeySymbol::Char('é'));
        composer.key_down(&mut driver, &symbol, effective).unwrap();
        let events = events.borrow().clone();
        assert_eq!(
            events,
            vec![KeyEvent::Down(PhysicalKey::E), KeyEvent::Up(PhysicalKey::E)]
        );
    }

    #[test]
    fn dead_key_compose_end_to_end() {
        let events = recorder();
        let layout = us_intl_layout();
        let mut driver = driver_with(&events, layout.clone());
        let mut composer = TextComposer::new(layout.clone());
        // ' (dead acute) then e → é.
        let out = composer
            .key_down(
                &mut driver,
                &KeySymbol::Dead(DeadKey::Acute),
                ModifierSet::empty(),
            )
            .unwrap();
        assert_eq!(out, TextModeOutcome::Handled);
        assert!(composer.pending_label().contains("acute"));
        let out = composer
            .key_down(&mut driver, &KeySymbol::Char('e'), ModifierSet::empty())
            .unwrap();
        assert_eq!(out, TextModeOutcome::Handled);
        assert!(composer.pending_label().is_empty());
        let events = events.borrow().clone();
        // é is AltGr+E on us-intl: RightAlt down, E down/up, RightAlt up.
        assert_eq!(
            events,
            vec![
                KeyEvent::Down(PhysicalKey::RightAlt),
                KeyEvent::Down(PhysicalKey::E),
                KeyEvent::Up(PhysicalKey::E),
                KeyEvent::Up(PhysicalKey::RightAlt),
            ]
        );
    }

    #[test]
    fn compose_key_end_to_end() {
        let events = recorder();
        let layout = us_layout();
        let mut driver = driver_with(&events, layout.clone());
        let mut composer = TextComposer::new(layout.clone());
        composer
            .key_down(&mut driver, &KeySymbol::Compose, ModifierSet::empty())
            .unwrap();
        composer
            .key_down(&mut driver, &KeySymbol::Char('o'), ModifierSet::empty())
            .unwrap();
        // © is not producible by the plain us layout → Unmappable error, but
        // the engine itself completed the sequence (state cleared).
        let err = composer
            .key_down(&mut driver, &KeySymbol::Char('c'), ModifierSet::empty())
            .unwrap_err();
        assert_eq!(err, TextError::Unmappable('©'));
        assert_eq!(composer.pending_label(), "");
        let events = events.borrow().clone();
        assert!(events.is_empty()); // the © error emitted nothing
    }

    #[test]
    fn unmappable_char_is_an_error() {
        let events = recorder();
        let layout = us_layout();
        let mut driver = driver_with(&events, layout.clone());
        let mut composer = TextComposer::new(layout.clone());
        let err = composer
            .key_down(&mut driver, &KeySymbol::Char('λ'), ModifierSet::empty())
            .unwrap_err();
        assert_eq!(err, TextError::Unmappable('λ'));
        assert!(events.borrow().is_empty());
    }

    #[test]
    fn name_key_falls_through_and_cancels_pending() {
        let events = recorder();
        let layout = us_intl_layout();
        let mut driver = driver_with(&events, layout.clone());
        let mut composer = TextComposer::new(layout);
        composer
            .key_down(
                &mut driver,
                &KeySymbol::Dead(DeadKey::Acute),
                ModifierSet::empty(),
            )
            .unwrap();
        let out = composer
            .key_down(
                &mut driver,
                &KeySymbol::Name("backspace".into()),
                ModifierSet::empty(),
            )
            .unwrap();
        assert_eq!(out, TextModeOutcome::FallThrough);
        assert!(composer.pending_label().is_empty());
        // The next character is a plain char again (no residual accent).
        let out = composer
            .key_down(&mut driver, &KeySymbol::Char('e'), ModifierSet::empty())
            .unwrap();
        assert_eq!(out, TextModeOutcome::Handled);
        let events = events.borrow().clone();
        assert_eq!(
            events,
            vec![KeyEvent::Down(PhysicalKey::E), KeyEvent::Up(PhysicalKey::E)]
        );
    }

    #[test]
    fn reset_clears_pending() {
        let events = recorder();
        let layout = us_intl_layout();
        let mut driver = driver_with(&events, layout.clone());
        let mut composer = TextComposer::new(layout);
        composer
            .key_down(
                &mut driver,
                &KeySymbol::Dead(DeadKey::Grave),
                ModifierSet::empty(),
            )
            .unwrap();
        assert!(!composer.pending_label().is_empty());
        composer.reset();
        assert!(composer.pending_label().is_empty());
    }
}
