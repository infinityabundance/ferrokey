//! X11 touch tracking.
//!
//! Slint's pointer model has a single pointer; a touchscreen can produce many
//! concurrent touch points. [`TouchTracker`] implements the single-pointer
//! fallback: only the *first* active touch is forwarded as touch events, and
//! when it lifts, the next remaining touch takes over with a fresh press. This
//! keeps the OSK usable with two thumbs while staying deterministic and
//! unit-testable (no X server needed).

use crate::SurfaceEvent;
use std::collections::{BTreeMap, BTreeSet};

/// Pure touch-sequence state machine for the X11 backend.
#[derive(Debug, Default)]
pub struct TouchTracker {
    /// Active XI2 touch ids, ordered by touch id.
    active: BTreeSet<u32>,
    /// Last known position per touch id.
    positions: BTreeMap<u32, (f64, f64)>,
}

impl TouchTracker {
    pub fn new() -> Self {
        TouchTracker::default()
    }

    /// A touch went down. Emits a `TouchPressed` only for the first touch.
    pub fn down(&mut self, id: u32, x: f64, y: f64) -> Option<SurfaceEvent> {
        if self.active.is_empty() {
            self.active.insert(id);
            self.positions.insert(id, (x, y));
            return Some(SurfaceEvent::TouchPressed { x, y });
        }
        self.active.insert(id);
        self.positions.insert(id, (x, y));
        None
    }

    /// A touch moved. Emits a `TouchMoved` only for the tracked touch.
    pub fn move_to(&mut self, id: u32, x: f64, y: f64) -> Option<SurfaceEvent> {
        self.positions.insert(id, (x, y));
        (self.active.first() == Some(&id)).then_some(SurfaceEvent::TouchMoved { x, y })
    }

    /// A touch was lifted. Emits a `TouchReleased`, and — when another touch
    /// is still active — a follow-up `TouchPressed` at its last position so
    /// the pointer state hands over cleanly.
    pub fn up(&mut self, id: u32, x: f64, y: f64) -> Vec<SurfaceEvent> {
        self.positions.remove(&id);
        if !self.active.remove(&id) {
            return Vec::new();
        }
        let mut events = vec![SurfaceEvent::TouchReleased { x, y }];
        if let Some(&next) = self.active.first() {
            if let Some(&(nx, ny)) = self.positions.get(&next) {
                events.push(SurfaceEvent::TouchPressed { x: nx, y: ny });
            }
        }
        events
    }

    /// Drop all tracking (window destroyed, etc.).
    pub fn reset(&mut self) {
        self.active.clear();
        self.positions.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_touch_round_trip() {
        let mut t = TouchTracker::new();
        assert_eq!(
            t.down(0, 10.0, 20.0),
            Some(SurfaceEvent::TouchPressed { x: 10.0, y: 20.0 })
        );
        assert_eq!(
            t.move_to(0, 15.0, 25.0),
            Some(SurfaceEvent::TouchMoved { x: 15.0, y: 25.0 })
        );
        assert_eq!(
            t.up(0, 16.0, 26.0),
            vec![SurfaceEvent::TouchReleased { x: 16.0, y: 26.0 }]
        );
        assert!(t.is_empty());
    }

    #[test]
    fn second_touch_is_suppressed_until_first_lifts() {
        let mut t = TouchTracker::new();
        t.down(0, 10.0, 20.0);
        // The second concurrent touch is not forwarded…
        assert_eq!(t.down(1, 100.0, 200.0), None);
        assert_eq!(t.move_to(1, 110.0, 210.0), None);
        // …until the tracked touch lifts, then it takes over with a press.
        assert_eq!(
            t.up(0, 16.0, 26.0),
            vec![
                SurfaceEvent::TouchReleased { x: 16.0, y: 26.0 },
                SurfaceEvent::TouchPressed { x: 110.0, y: 210.0 },
            ]
        );
        assert_eq!(
            t.move_to(1, 120.0, 220.0),
            Some(SurfaceEvent::TouchMoved { x: 120.0, y: 220.0 })
        );
        assert_eq!(
            t.up(1, 121.0, 221.0),
            vec![SurfaceEvent::TouchReleased { x: 121.0, y: 221.0 }]
        );
        assert!(t.is_empty());
    }

    #[test]
    fn up_of_unknown_touch_is_ignored() {
        let mut t = TouchTracker::new();
        assert_eq!(t.up(7, 0.0, 0.0), Vec::new());
        t.down(3, 1.0, 2.0);
        assert_eq!(t.up(9, 0.0, 0.0), Vec::new());
        assert!(!t.is_empty());
        t.reset();
        assert!(t.is_empty());
    }
}
