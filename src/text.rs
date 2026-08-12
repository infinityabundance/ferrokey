//! Text-mode input: the second input channel.
//!
//! Keyboard mode sends raw key events; **text mode** types *characters* by
//! resolving each character against the active layout (base / shift / altgr
//! levels) and emitting the corresponding chord. Characters the layout cannot
//! produce are reported as errors — Ferrokey **never silently substitutes
//! clipboard paste** for keyboard injection.

use ferrokey_core::{KeyAction, KeyboardDriver, ModifierSet, PhysicalKey, TextError, VirtualKey};
use std::time::Instant;

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

/// Type `text` through the driver using the active layout.
#[cfg(test)]
mod tests {
    use super::*;
    use ferrokey_core::{KeyEvent, KeySink, Layout, RepeatSettings, SinkError, StateSettings};
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

    fn driver_with(events: &Rc<RefCell<Vec<KeyEvent>>>) -> KeyboardDriver {
        KeyboardDriver::new(
            StateSettings::default(),
            RepeatSettings::default(),
            std::sync::Arc::new(us_layout()),
            Box::new(Recorder {
                events: events.clone(),
            }),
        )
    }

    fn us_layout() -> Layout {
        ferrokey_layouts::builtin("us").unwrap()
    }

    #[test]
    fn types_plain_ascii() {
        let events = recorder();
        let mut driver = driver_with(&events);
        let us = us_layout();
        type_text(&mut driver, "abc", |c| {
            us.find_char(c, ModifierSet::empty())
        })
        .unwrap();
        let events = events.borrow().clone();
        // a, b, c taps: down+up each.
        assert_eq!(events.len(), 6);
        assert_eq!(events[0], KeyEvent::Down(PhysicalKey::A));
        assert_eq!(events[5], KeyEvent::Up(PhysicalKey::C));
    }

    #[test]
    fn shifted_char_injects_shift() {
        let events = recorder();
        let mut driver = driver_with(&events);
        let us = us_layout();
        type_text(&mut driver, "A", |c| us.find_char(c, ModifierSet::empty())).unwrap();
        let events = events.borrow().clone();
        assert_eq!(
            events,
            vec![
                KeyEvent::Down(PhysicalKey::LeftShift),
                KeyEvent::Down(PhysicalKey::A),
                KeyEvent::Up(PhysicalKey::A),
                KeyEvent::Up(PhysicalKey::LeftShift),
            ]
        );
    }

    #[test]
    fn unmappable_char_is_an_error() {
        let events = recorder();
        let mut driver = driver_with(&events);
        let us = us_layout();
        let err =
            type_text(&mut driver, "λ", |c| us.find_char(c, ModifierSet::empty())).unwrap_err();
        assert_eq!(err, TextError::Unmappable('λ'));
        assert!(events.borrow().is_empty());
    }
}
