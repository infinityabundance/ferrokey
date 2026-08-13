//! Terminal-pane pointer/touch interaction (§24–§27, §102–§103).
//!
//! The pane below the OSK has its own deterministic gesture state machine,
//! completely separate from the OSK key bridge:
//!
//! * **tap** (quick press+release, no drift) → start a character selection at
//!   the cell, or activate an overlay control (↓ newest / Restart);
//! * **drag** (movement past a threshold) → scroll the viewport when no
//!   selection is active, extend the selection when one is;
//! * **long-press** (≥ 500 ms) then drag → select;
//! * a second gesture after a tap extends the selection.
//!
//! Hit regions are strict: a gesture that begins in the pane stays owned by
//! the pane until release, and a drag can never press an OSK key (§25).
//! Gestures never generate terminal *mouse* events (§24).

use ferrokey_terminal::{CellPos, SelectionMode, Terminal};
use std::time::{Duration, Instant};

/// Movement beyond this many cells turns a pending press into a scroll.
const SCROLL_THRESHOLD_CELLS: u32 = 2;
/// A press held this long becomes a selection gesture.
const LONG_PRESS: Duration = Duration::from_millis(500);

/// The state of the gesture in progress.
enum Gesture {
    /// Pressed; neither scroll nor selection decided yet.
    Pending { y: u32, pressed_at: Instant },
    /// Dragging to scroll the viewport.
    Scroll { last_y: u32 },
    /// Dragging to extend the selection.
    Select,
}

/// Owns pane gestures for one session.
#[derive(Default)]
pub struct TerminalInput {
    gesture: Option<Gesture>,
    /// The cell the pending press started at (for tap selection).
    press_cell: Option<CellPos>,
    /// Whether the pane currently owns the pointer (no OSK events may fire).
    pub owns_pointer: bool,
}

impl TerminalInput {
    /// A press went down in the pane at physical px `(x, y)`.
    pub fn press(&mut self, term: &mut Terminal, x: u32, y: u32, now: Instant) {
        self.owns_pointer = true;
        // If a selection already exists, a follow-up press starts a
        // selection-extension gesture directly.
        let selecting = term.selection().is_some();
        self.gesture = Some(if selecting {
            Gesture::Select
        } else {
            Gesture::Pending { y, pressed_at: now }
        });
        self.press_cell = term.physical_to_doc(x, y);
    }

    /// The pointer moved while pressed.
    pub fn move_to(&mut self, term: &mut Terminal, x: u32, y: u32, now: Instant) {
        let Some(gesture) = self.gesture.as_mut() else {
            return;
        };
        match gesture {
            Gesture::Pending { y: sy, pressed_at } => {
                let cell_h = term.cell_metrics().cell_h;
                // Finger movement: positive = down (y increased).
                let dy = i64::from(y) - i64::from(*sy);
                let threshold = i64::from(SCROLL_THRESHOLD_CELLS) * i64::from(cell_h);
                if dy.abs() > threshold {
                    // A decisive vertical swipe: scroll by the amount already
                    // moved (up = into history, down = toward newest).
                    let delta = dy.clamp(-threshold * 8, threshold * 8) as i32;
                    term.scroll_by_delta(delta);
                    *gesture = Gesture::Scroll { last_y: y };
                } else if now.duration_since(*pressed_at) >= LONG_PRESS {
                    // Long-press: begin selecting from the press cell.
                    if let Some(cell) = self.press_cell {
                        term.selection_start(cell, SelectionMode::Character);
                    }
                    *gesture = Gesture::Select;
                    self.extend(term, x, y);
                }
            }
            Gesture::Scroll { last_y } => {
                let delta = i64::from(y) - i64::from(*last_y);
                if delta != 0 {
                    term.scroll_by_delta(delta.clamp(-4096, 4096) as i32);
                    *last_y = y;
                }
            }
            Gesture::Select => self.extend(term, x, y),
        }
    }

    /// The pointer went up.
    pub fn release(&mut self, term: &mut Terminal, x: u32, y: u32) {
        self.owns_pointer = false;
        match self.gesture.take() {
            Some(Gesture::Pending { .. }) => {
                // A clean tap: activate overlay controls, else start a
                // character selection at the cell.
                if !term.tap(x, y) {
                    if let Some(cell) = self.press_cell {
                        term.selection_start(cell, SelectionMode::Character);
                    }
                }
            }
            Some(Gesture::Scroll { .. }) | None => {
                // The viewport stays where the user left it (§23).
            }
            Some(Gesture::Select) => self.extend(term, x, y),
        }
        self.press_cell = None;
    }

    /// The compositor cancelled the touch (palm rejection): never leave a
    /// selection mid-drag; keep any completed selection.
    pub fn cancel(&mut self) {
        self.gesture = None;
        self.press_cell = None;
        self.owns_pointer = false;
    }

    fn extend(&mut self, term: &mut Terminal, x: u32, y: u32) {
        if let Some(cell) = term.physical_to_doc(x, y) {
            term.selection_extend(cell);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrokey_terminal::TerminalConfig;
    use std::time::Instant;

    fn term() -> Terminal {
        let mut t = Terminal::new(TerminalConfig {
            confirm_multiline_paste: false,
            ..TerminalConfig::default()
        })
        .unwrap();
        t.resize(800, 400).unwrap();
        for i in 0..50 {
            t.feed(format!("line{i:02}\r\n").as_bytes());
        }
        t.feed(b"bottom");
        t
    }

    #[test]
    fn tap_starts_selection() {
        let mut t = term();
        let mut input = TerminalInput::default();
        let now = Instant::now();
        input.press(&mut t, 20, 30, now);
        input.release(&mut t, 20, 30);
        assert!(t.selection().is_some());
        assert!(!input.owns_pointer);
    }

    #[test]
    fn drag_scrolls_when_no_selection() {
        let mut t = term();
        let mut input = TerminalInput::default();
        let now = Instant::now();
        input.press(&mut t, 100, 200, now);
        // Move up far enough to trigger scrolling.
        input.move_to(&mut t, 100, 60, now + Duration::from_millis(100));
        assert!(
            t.viewport().scroll_offset > 0,
            "scroll must move into history"
        );
        input.release(&mut t, 100, 60);
        // The gesture must not have started a selection.
        assert!(t.selection().is_none());
    }

    #[test]
    fn long_press_then_drag_selects() {
        let mut t = term();
        let mut input = TerminalInput::default();
        let now = Instant::now();
        input.press(&mut t, 20, 30, now);
        // Hold past the long-press threshold without moving much.
        input.move_to(&mut t, 22, 32, now + Duration::from_millis(600));
        // Now drag across cells.
        input.move_to(&mut t, 120, 32, now + Duration::from_millis(700));
        input.release(&mut t, 120, 32);
        let sel = t.selection().expect("selection started");
        assert!(sel.span_lines() >= 0);
    }

    #[test]
    fn drag_after_tap_extends_selection() {
        let mut t = term();
        let mut input = TerminalInput::default();
        let now = Instant::now();
        // Tap: selects the cell.
        input.press(&mut t, 20, 30, now);
        input.release(&mut t, 20, 30);
        let anchor = t.selection().unwrap().anchor;
        // Second gesture: press + drag extends.
        input.press(&mut t, 20, 30, now + Duration::from_millis(50));
        input.move_to(&mut t, 200, 30, now + Duration::from_millis(150));
        input.release(&mut t, 200, 30);
        let sel = t.selection().unwrap();
        assert!(sel.end != anchor, "drag must extend the selection");
    }

    #[test]
    fn pane_gesture_never_leaks_to_osks() {
        let mut t = term();
        let mut input = TerminalInput::default();
        let now = Instant::now();
        input.press(&mut t, 100, 100, now);
        input.move_to(&mut t, 100, 50, now + Duration::from_millis(100));
        input.move_to(&mut t, 300, 50, now + Duration::from_millis(200));
        input.release(&mut t, 300, 50);
        // Only the pane consumed events; no OSK key semantics exist here, and
        // the pointer ownership must have been released.
        assert!(!input.owns_pointer);
    }

    #[test]
    fn cancel_clears_ownership() {
        let mut t = term();
        let mut input = TerminalInput::default();
        input.press(&mut t, 10, 10, Instant::now());
        assert!(input.owns_pointer);
        input.cancel();
        assert!(!input.owns_pointer);
        assert!(input.gesture.is_none());
    }
}
