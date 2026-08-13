//! Input routing (§61–§63): one destination decision for every key action.
//!
//! The `KeyboardDriver`'s sink is an [`InputRouter`] — a single point that
//! forwards every emitted physical-key event to either the **system**
//! destination (the daemon link → `/dev/uinput` → compositor → focused app)
//! or the **terminal** destination (the terminal key encoder → PTY → shell).
//! There are no scattered `if terminal_mode` checks across UI callbacks.
//!
//! Destination switching is explicit, safe and deterministic: the driver is
//! `emergency_release`d first (held keys and repeat state are cleared on the
//! old destination), *then* the active destination flips (§62–§63). A
//! key-down never crosses destinations (§59–§60).

use crate::daemon::DaemonLink;
use ferrokey_core::{KeySink, PhysicalKey, SinkError};
use std::cell::RefCell;
use std::rc::Rc;

/// Which destination owns keyboard actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    /// The system-wide focused application (via ferrokeyd/uinput).
    System,
    /// The embedded terminal workspace (via the PTY).
    Terminal,
}

impl Destination {
    pub const fn label(self) -> &'static str {
        match self {
            Destination::System => "SYSTEM",
            Destination::Terminal => "TERMINAL",
        }
    }
}

/// A [`KeySink`] that routes to exactly one destination at a time.
pub struct InputRouter {
    active: Destination,
    /// The system sink (the daemon link); `None` in terminal-only mode.
    system: Option<Rc<RefCell<DaemonLink>>>,
    /// The terminal sink (encoder → PTY).
    terminal: Option<Box<dyn KeySink>>,
}

impl InputRouter {
    pub fn new(
        system: Option<Rc<RefCell<DaemonLink>>>,
        terminal: Option<Box<dyn KeySink>>,
        initial: Destination,
    ) -> Self {
        InputRouter {
            active: initial,
            system,
            terminal,
        }
    }

    pub fn active(&self) -> Destination {
        self.active
    }

    /// Switch destination. Callers must `emergency_release` the driver first
    /// so no held key or repeat state crosses the boundary (§62).
    pub fn set_active(&mut self, dest: Destination) {
        self.active = dest;
    }
}

impl KeySink for InputRouter {
    fn key_down(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        match self.active {
            Destination::System => {
                if let Some(link) = &self.system {
                    let mut link = link.borrow_mut();
                    return link.key_down(key);
                }
                Ok(())
            }
            Destination::Terminal => {
                if let Some(sink) = self.terminal.as_mut() {
                    return sink.key_down(key);
                }
                Ok(())
            }
        }
    }

    fn key_up(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        match self.active {
            Destination::System => {
                if let Some(link) = &self.system {
                    let mut link = link.borrow_mut();
                    return link.key_up(key);
                }
                Ok(())
            }
            Destination::Terminal => {
                if let Some(sink) = self.terminal.as_mut() {
                    return sink.key_up(key);
                }
                Ok(())
            }
        }
    }

    fn key_repeat(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        match self.active {
            Destination::System => {
                if let Some(link) = &self.system {
                    let mut link = link.borrow_mut();
                    return link.key_repeat(key);
                }
                Ok(())
            }
            Destination::Terminal => {
                if let Some(sink) = self.terminal.as_mut() {
                    return sink.key_repeat(key);
                }
                Ok(())
            }
        }
    }

    fn release_all(&mut self) -> Result<(), SinkError> {
        // Release on the ACTIVE destination only (the inactive one holds
        // nothing — held state was cleared when it was deactivated).
        match self.active {
            Destination::System => {
                if let Some(link) = &self.system {
                    let mut link = link.borrow_mut();
                    return link.release_all();
                }
                Ok(())
            }
            Destination::Terminal => {
                if let Some(sink) = self.terminal.as_mut() {
                    return sink.release_all();
                }
                Ok(())
            }
        }
    }
}

/// A [`KeySink`] over a shared [`InputRouter`] (newtype: the orphan rule
/// forbids implementing the foreign trait for `Rc<RefCell<_>>` directly).
pub struct RouterSink(pub Rc<RefCell<InputRouter>>);

impl KeySink for RouterSink {
    fn key_down(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        self.0.borrow_mut().key_down(key)
    }
    fn key_up(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        self.0.borrow_mut().key_up(key)
    }
    fn key_repeat(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        self.0.borrow_mut().key_repeat(key)
    }
    fn release_all(&mut self) -> Result<(), SinkError> {
        self.0.borrow_mut().release_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Recorder(Rc<RefCell<Vec<String>>>);

    impl KeySink for Recorder {
        fn key_down(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
            self.0.borrow_mut().push(format!("down:{key:?}"));
            Ok(())
        }
        fn key_up(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
            self.0.borrow_mut().push(format!("up:{key:?}"));
            Ok(())
        }
        fn key_repeat(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
            self.0.borrow_mut().push(format!("repeat:{key:?}"));
            Ok(())
        }
        fn release_all(&mut self) -> Result<(), SinkError> {
            self.0.borrow_mut().push("release-all".into());
            Ok(())
        }
    }

    #[test]
    fn routes_to_the_active_destination_only() {
        let term = Rc::new(RefCell::new(Vec::<String>::new()));
        let mut router = InputRouter::new(
            None,
            Some(Box::new(Recorder(term.clone()))),
            Destination::Terminal,
        );
        // Terminal-only: system events are dropped, terminal events routed.
        router.key_down(PhysicalKey::A).unwrap();
        assert_eq!(*term.borrow(), vec!["down:A".to_string()]);
        router.key_up(PhysicalKey::A).unwrap();
        assert_eq!(*term.borrow(), vec!["down:A", "up:A"]);
    }

    #[test]
    fn terminal_only_router_drops_system_routing() {
        let mut router = InputRouter::new(None, None, Destination::System);
        // No system sink: events are dropped, never panic.
        router.key_down(PhysicalKey::A).unwrap();
        router.release_all().unwrap();
    }
}
