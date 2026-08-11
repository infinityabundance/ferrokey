//! The action protocol between the UI and the keyboard engine.
//!
//! The UI must **not** bury key semantics in Slint callbacks. It reports raw
//! intents — `Action::Press(VirtualKey::Shift)`, `Action::Press(VirtualKey::A)`
//! — and [`KeyboardDriver`] decides what Linux events result (modifier
//! injection, sticky/locked modifiers, chords, repeat, …).
//!
//! Two input channels exist from day one (see `InputRequest`):
//!
//! * `Key(...)` — always kernel-level key events (uinput).
//! * `Text(String)` — best-effort text/IME path (compose sequences etc.).
//!   Text is **never** silently substituted with clipboard paste.

use crate::key::PhysicalKey;
use crate::layout::Layout;
use crate::repeat::RepeatEngine;
use crate::state::{KeyEvent, KeyboardState, StateError, StateSettings};
use std::sync::Arc;
use std::time::Instant;

/// A key the UI can address.
///
/// Currently only physical keys; the wrapper exists so a future logical-key
/// space (layout-independent "the key that produces `q`") can be added
/// without churning every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VirtualKey {
    Physical(PhysicalKey),
}

impl From<PhysicalKey> for VirtualKey {
    fn from(key: PhysicalKey) -> Self {
        VirtualKey::Physical(key)
    }
}

/// A raw key intent from the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Pointer/touch went down on the key.
    Down,
    /// Pointer/touch went up on the key.
    Up,
    /// A short press+release pair (tap).
    Tap,
    /// Emergency release of everything held.
    ReleaseAll,
}

/// The two input channels (keyboard mode and text mode).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputRequest {
    Key(KeyAction),
    Text(String),
}

/// The sink for kernel-level key events.
///
/// Implemented by the uinput device (`ferrokey-uinput`) and by the protocol
/// client (`ferrokey-protocol` → `ferrokeyd`).
pub trait KeySink {
    fn key_down(&mut self, key: PhysicalKey) -> Result<(), SinkError>;
    fn key_up(&mut self, key: PhysicalKey) -> Result<(), SinkError>;

    /// Press and release a key.
    fn tap(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        self.key_down(key)?;
        self.key_up(key)
    }

    /// Release every key this sink currently holds down.
    fn release_all(&mut self) -> Result<(), SinkError>;
}

/// A failure while delivering key events to the sink (I/O, ledger, …).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SinkError(pub String);

impl From<&str> for SinkError {
    fn from(s: &str) -> Self {
        SinkError(s.to_string())
    }
}

impl From<String> for SinkError {
    fn from(s: String) -> Self {
        SinkError(s)
    }
}

/// The sink for text-mode input.
///
/// The implementation chooses the best available text backend (compose
/// sequences, IME, …). A clipboard fallback, if ever offered, must be
/// opt-in and explicit — never silent.
pub trait TextSink {
    fn type_text(&mut self, text: &str) -> Result<(), TextError>;
}

/// Errors from the text input path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TextError {
    #[error("no text input backend is available")]
    Unsupported,
    #[error("character {0:?} cannot be produced by the active layout")]
    Unmappable(char),
    #[error("text backend failed: {0}")]
    Backend(String),
}

/// The complete keyboard engine: state machine + repeat + layout policy,
/// driving a [`KeySink`].
pub struct KeyboardDriver {
    state: KeyboardState,
    repeat: RepeatEngine,
    layout: Arc<Layout>,
    sink: Box<dyn KeySink>,
}

impl KeyboardDriver {
    pub fn new(
        settings: StateSettings,
        repeat_settings: crate::repeat::RepeatSettings,
        layout: Arc<Layout>,
        sink: Box<dyn KeySink>,
    ) -> Self {
        KeyboardDriver {
            state: KeyboardState::new(settings),
            repeat: RepeatEngine::new(repeat_settings),
            layout,
            sink,
        }
    }

    pub fn state(&self) -> &KeyboardState {
        &self.state
    }

    pub fn repeat(&self) -> &RepeatEngine {
        &self.repeat
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn set_layout(&mut self, layout: Arc<Layout>) {
        self.layout = layout;
    }

    pub fn sink(&self) -> &dyn KeySink {
        self.sink.as_ref()
    }

    /// Handle one UI action, emitting events to the sink.
    pub fn handle_action(
        &mut self,
        action: KeyAction,
        key: VirtualKey,
        now: Instant,
    ) -> Result<(), DriverError> {
        let VirtualKey::Physical(key) = key;
        let events = match action {
            KeyAction::Down => {
                let events = self.state.press(key, now)?;
                if self.state.is_depressed(key) {
                    let repeatable = self.layout.is_repeatable(key);
                    self.repeat.key_down(key, now, repeatable);
                }
                events
            }
            KeyAction::Up => {
                self.repeat.key_up(key);
                self.state.release(key, now)?
            }
            KeyAction::Tap => {
                self.repeat.key_up(key);
                let mut events = self.state.press(key, now)?;
                events.extend(self.state.release(key, now + crate::state::TAP_GRACE)?);
                events
            }
            KeyAction::ReleaseAll => {
                self.repeat.release_all();
                self.state.release_all()
            }
        };
        self.emit(events)
    }

    /// Advance the repeat engine; emits re-presses for held repeatable keys.
    pub fn tick_repeat(&mut self, now: Instant) -> Result<(), DriverError> {
        let due = self.repeat.tick(now);
        for key in due {
            self.sink.key_down(key)?;
        }
        Ok(())
    }

    /// Emergency release of everything (hide, disconnect, SIGTERM, …).
    pub fn emergency_release(&mut self) -> Result<(), DriverError> {
        self.repeat.release_all();
        let events = self.state.release_all();
        self.emit(events)
    }

    fn emit(&mut self, events: Vec<KeyEvent>) -> Result<(), DriverError> {
        for ev in events {
            match ev {
                KeyEvent::Down(key) => self.sink.key_down(key)?,
                KeyEvent::Up(key) => self.sink.key_up(key)?,
            }
        }
        Ok(())
    }
}

/// Errors surfaced by the driver.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("keyboard state machine error: {0}")]
    State(#[from] StateError),
    #[error("sink error: {0}")]
    Sink(#[from] SinkError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::time::Duration;

    /// Recording sink: an in-memory transcript of emitted events.
    #[derive(Default)]
    pub struct Recorder {
        pub events: RefCell<Vec<KeyEvent>>,
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

    impl KeySink for std::rc::Rc<Recorder> {
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

    fn layout() -> Arc<Layout> {
        Arc::new(crate::layout::Layout::empty("test", "Test"))
    }

    fn driver(sink: Box<dyn KeySink>) -> KeyboardDriver {
        KeyboardDriver::new(
            StateSettings::default(),
            crate::repeat::RepeatSettings::default(),
            layout(),
            sink,
        )
    }

    #[test]
    fn tap_a_emits_down_up() {
        let recorder = std::rc::Rc::new(Recorder::default());
        let sink: Box<dyn KeySink> = Box::new(recorder.clone());
        let mut d = driver(sink);
        let now = Instant::now();
        d.handle_action(KeyAction::Tap, VirtualKey::Physical(PhysicalKey::A), now)
            .unwrap();
        let events = recorder.events.borrow().clone();
        assert!(!d.state().is_depressed(PhysicalKey::A));
        assert_eq!(
            events,
            vec![KeyEvent::Down(PhysicalKey::A), KeyEvent::Up(PhysicalKey::A)]
        );
    }

    #[test]
    fn hold_a_then_repeat_tick() {
        let recorder = std::rc::Rc::new(Recorder::default());
        let sink: Box<dyn KeySink> = Box::new(recorder.clone());
        let mut d = driver(sink);
        let now = Instant::now();
        d.handle_action(KeyAction::Down, VirtualKey::Physical(PhysicalKey::A), now)
            .unwrap();
        // Repeat fires after the delay.
        d.tick_repeat(now + Duration::from_millis(500)).unwrap();
        let events = recorder.events.borrow().clone();
        assert_eq!(
            events,
            vec![
                KeyEvent::Down(PhysicalKey::A),
                KeyEvent::Down(PhysicalKey::A), // repeat
            ]
        );
        // Pointer up stops it.
        d.handle_action(
            KeyAction::Up,
            VirtualKey::Physical(PhysicalKey::A),
            now + Duration::from_millis(600),
        )
        .unwrap();
        d.tick_repeat(now + Duration::from_secs(2)).unwrap();
        let events = recorder.events.borrow().clone();
        assert_eq!(
            events,
            vec![
                KeyEvent::Down(PhysicalKey::A),
                KeyEvent::Down(PhysicalKey::A),
                KeyEvent::Up(PhysicalKey::A),
            ]
        );
    }

    #[test]
    fn sticky_shift_end_to_end() {
        let recorder = std::rc::Rc::new(Recorder::default());
        let sink: Box<dyn KeySink> = Box::new(recorder.clone());
        let mut d = driver(sink);
        let now = Instant::now();
        // Tap shift (fast press+release).
        d.handle_action(
            KeyAction::Down,
            VirtualKey::Physical(PhysicalKey::LeftShift),
            now,
        )
        .unwrap();
        d.handle_action(
            KeyAction::Up,
            VirtualKey::Physical(PhysicalKey::LeftShift),
            now + Duration::from_millis(60),
        )
        .unwrap();
        // Press A: shift should be injected, then released on A's release.
        d.handle_action(
            KeyAction::Down,
            VirtualKey::Physical(PhysicalKey::A),
            now + Duration::from_millis(150),
        )
        .unwrap();
        d.handle_action(
            KeyAction::Up,
            VirtualKey::Physical(PhysicalKey::A),
            now + Duration::from_millis(200),
        )
        .unwrap();
        let events = recorder.events.borrow().clone();
        assert_eq!(
            events,
            vec![
                KeyEvent::Down(PhysicalKey::LeftShift),
                KeyEvent::Up(PhysicalKey::LeftShift),
                KeyEvent::Down(PhysicalKey::LeftShift), // injected
                KeyEvent::Down(PhysicalKey::A),
                KeyEvent::Up(PhysicalKey::A),
                KeyEvent::Up(PhysicalKey::LeftShift), // injected released
            ]
        );
        assert!(d.state().depressed().is_empty());
    }

    #[test]
    fn emergency_release_releases_held_keys() {
        let recorder = std::rc::Rc::new(Recorder::default());
        let sink: Box<dyn KeySink> = Box::new(recorder.clone());
        let mut d = driver(sink);
        let now = Instant::now();
        d.handle_action(
            KeyAction::Down,
            VirtualKey::Physical(PhysicalKey::LeftCtrl),
            now,
        )
        .unwrap();
        d.handle_action(
            KeyAction::Down,
            VirtualKey::Physical(PhysicalKey::C),
            now + Duration::from_millis(20),
        )
        .unwrap();
        d.emergency_release().unwrap();
        let events = recorder.events.borrow().clone();
        assert_eq!(
            events,
            vec![
                KeyEvent::Down(PhysicalKey::LeftCtrl),
                KeyEvent::Down(PhysicalKey::C),
                KeyEvent::Up(PhysicalKey::C),
                KeyEvent::Up(PhysicalKey::LeftCtrl),
            ]
        );
        assert!(d.state().depressed().is_empty());
    }

    #[test]
    fn release_all_action_works() {
        let recorder = std::rc::Rc::new(Recorder::default());
        let sink: Box<dyn KeySink> = Box::new(recorder.clone());
        let mut d = driver(sink);
        let now = Instant::now();
        d.handle_action(KeyAction::Down, VirtualKey::Physical(PhysicalKey::A), now)
            .unwrap();
        d.handle_action(
            KeyAction::ReleaseAll,
            VirtualKey::Physical(PhysicalKey::A),
            now + Duration::from_millis(50),
        )
        .unwrap();
        assert!(d.state().depressed().is_empty());
    }
}
