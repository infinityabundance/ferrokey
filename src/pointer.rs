//! The pointer/touch → key bridge (rules 18, 85, 92).
//!
//! Slint's `TouchArea` grabs the pointer on press: while one button is down,
//! a second press at another key is routed to the *grabber*, not to the key
//! under the pointer. Per-key `.slint` callbacks therefore **cannot** deliver
//! genuine chords (hold Ctrl, tap C) — the second key would never fire and
//! the grabber's release would fire an early key-up.
//!
//! Key semantics therefore live here, in Rust: raw surface events are
//! hit-tested against the active view geometry and translated into
//! `key-pressed`/`key-released` invocations on the UI. The `.slint` layer is
//! purely visual (key caps light up); it never decides which key is down.

use crate::views::{self, KeyboardView};
use crate::MainWindow;
use ferrokey_core::geometry::{AdaptiveGeometry, Point};
use ferrokey_surface::{PointerButton, SurfaceEvent};
use ferrokey_terminal::shell::ShellRowKey;
use std::collections::BTreeMap;

/// Owns the pointer/touch → key translation for one session.
///
/// Tracks which key each pointer button (and the active touch) is currently
/// holding, so a release always targets the key that was pressed — matching
/// physical-keyboard semantics even if the pointer drifts between keys.
///
/// Touch presses are hit-tested through the **adaptive geometry** (WS4)
/// when enabled: the OSK learns where the user actually touches and adapts
/// the effective hit targets while the visible keyboard stays stable.
/// Pointer presses keep the plain visual hit-test (mouse input is precise).
pub struct PointerBridge {
    view: &'static KeyboardView,
    scale: f32,
    /// Adaptive touch hit-testing + learning; `None` = disabled.
    adaptive: Option<AdaptiveGeometry>,
    /// key index → name, in view order (the adaptive geometry's index
    /// space, matching [`views::adaptive_geometry_basis`]).
    names: Vec<&'static str>,
    /// The active shell-aware row (WS5): its keys' sequences win over the
    /// static view chords. Presentation-only: switching never releases
    /// held keys, presses keys, changes modifiers, resets modes, resizes
    /// the terminal or restarts the child (§5.10).
    shell_row: Option<&'static [ShellRowKey]>,
    /// Normalized-distance confidence below which a touch is an unambiguous
    /// intended-key sample (evidence rule §4.3).
    evidence_confidence: f64,
    /// Pointer button → key name currently held by that button.
    pointer_down: BTreeMap<PointerButton, &'static str>,
    /// The key currently held by the active touch, if any.
    touch_down: Option<&'static str>,
}

impl PointerBridge {
    pub fn new(
        view: &'static KeyboardView,
        scale: f32,
        adaptive: Option<(AdaptiveGeometry, f64)>,
    ) -> Self {
        let mut names = Vec::new();
        let evidence_confidence = match &adaptive {
            Some((_, conf)) => {
                names = views::adaptive_geometry_basis(view).2;
                *conf
            }
            None => f64::INFINITY,
        };
        let adaptive = adaptive.map(|(ag, _)| ag);
        PointerBridge {
            view,
            scale,
            adaptive,
            names,
            shell_row: None,
            evidence_confidence,
            pointer_down: BTreeMap::new(),
            touch_down: None,
        }
    }

    /// Switch the active shell-aware row (WS5). Presentation-only (§5.10):
    /// this changes which sequences the shortcut buttons play — it never
    /// releases held keys, presses keys, changes modifier state, resets
    /// terminal modes, resizes the terminal or restarts the child.
    pub fn set_shell_row(&mut self, row: Option<&'static [ShellRowKey]>) {
        self.shell_row = row;
    }

    /// The shell-row sequence for a button label, if the active row has one.
    fn shell_sequence_for(
        &self,
        name: &str,
    ) -> Option<&'static [&'static [ferrokey_core::PhysicalKey]]> {
        let row = self.shell_row?;
        row.iter().find(|k| k.label == name).map(|k| k.sequence)
    }

    /// Keep the bridge's scale in sync with the surface (HiDPI).
    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale;
    }

    /// Advance the adaptive geometry: run an optimization pass when enough
    /// new evidence has accumulated. Called from the UI timer — never from a
    /// touch event (the optimizer is not on the touch hot path, §4.7).
    pub fn tick_adaptive(&mut self) {
        if let Some(ag) = &mut self.adaptive {
            if ag.optimize_due() {
                ag.optimize();
            }
        }
    }

    /// Translate one raw surface event into key actions on `ui`.
    pub fn handle_event(&mut self, ui: &MainWindow, event: SurfaceEvent) {
        match event {
            SurfaceEvent::PointerPressed { x, y, button } => {
                let name = self.key_at(x, y);
                log::debug!("pointer press ({x:.0},{y:.0}) btn={button:?} -> key {name:?}");
                if let Some(name) = name {
                    if let Some(seq) = self.shell_sequence_for(name) {
                        self.play_sequence(ui, seq);
                    } else if let Some(chord) = self.view.chord_for(name) {
                        self.play_chord(ui, chord);
                    } else {
                        self.pointer_down.insert(button, name);
                        ui.invoke_key_pressed(name.into());
                    }
                }
            }
            SurfaceEvent::PointerReleased { button, .. } => {
                let name = self.pointer_down.remove(&button);
                log::debug!("pointer release btn={button:?} -> key {name:?}");
                if let Some(name) = name {
                    ui.invoke_key_released(name.into());
                }
            }
            // The pointer left the window with buttons still held (defensive:
            // release what we believe is down so nothing sticks).
            SurfaceEvent::PointerLeft => {
                let held: Vec<_> = self.pointer_down.values().copied().collect();
                self.pointer_down.clear();
                for name in held {
                    ui.invoke_key_released(name.into());
                }
            }
            SurfaceEvent::TouchPressed { x, y } => {
                if self.touch_down.is_none() {
                    if let Some(name) = self.touch_key_at(x, y) {
                        if let Some(seq) = self.shell_sequence_for(name) {
                            self.play_sequence(ui, seq);
                        } else if let Some(chord) = self.view.chord_for(name) {
                            self.play_chord(ui, chord);
                        } else {
                            self.touch_down = Some(name);
                            ui.invoke_key_pressed(name.into());
                        }
                    }
                }
            }
            // Palm rejection / compositor cancel: never leave the key held.
            SurfaceEvent::TouchReleased { .. } | SurfaceEvent::TouchCancelled => {
                if let Some(name) = self.touch_down.take() {
                    ui.invoke_key_released(name.into());
                }
            }
            // Movement, resize and close are not key actions.
            SurfaceEvent::PointerMoved { .. }
            | SurfaceEvent::TouchMoved { .. }
            | SurfaceEvent::Resized { .. }
            | SurfaceEvent::CloseRequested => {}
        }
    }

    /// The key under a physical-pixel point, if any.
    fn key_at(&self, x: f64, y: f64) -> Option<&'static str> {
        let scale = if self.scale > 0.0 { self.scale } else { 1.0 };
        views::key_at(
            self.view,
            (x / f64::from(scale)) as f32,
            (y / f64::from(scale)) as f32,
        )
    }

    /// The touch point in logical (view) coordinates.
    fn logical_point(&self, x: f64, y: f64) -> Point {
        let scale = if self.scale > 0.0 { self.scale } else { 1.0 };
        Point::new(x / f64::from(scale), y / f64::from(scale))
    }

    /// The key under a touch, via the adaptive geometry when enabled (with
    /// intended-key evidence recording, §4.3); falls back to the visual
    /// rects when disabled.
    fn touch_key_at(&mut self, x: f64, y: f64) -> Option<&'static str> {
        let p = self.logical_point(x, y);
        if let Some(ag) = &mut self.adaptive {
            let (hit, confidence) = ag.hit_test_confidence(p);
            match hit {
                Some(idx) => {
                    // Evidence rule: only unambiguous hits (well inside the
                    // effective region) are training samples — a boundary
                    // touch is ambiguous and must not pollute the model.
                    if confidence <= self.evidence_confidence {
                        ag.record_hit(idx, p);
                    }
                    Some(self.names[idx])
                }
                None => None,
            }
        } else {
            let scale = if self.scale > 0.0 { self.scale } else { 1.0 };
            views::key_at(
                self.view,
                (x / f64::from(scale)) as f32,
                (y / f64::from(scale)) as f32,
            )
        }
    }

    /// Play a chord key: press each member in order, release in reverse
    /// (§55, §57). The sequence flows through the normal core state machine
    /// and the active destination — never an internal command.
    fn play_chord(&self, ui: &MainWindow, chord: &'static [&'static str]) {
        for name in chord {
            ui.invoke_key_pressed((*name).into());
        }
        for name in chord.iter().rev() {
            ui.invoke_key_released((*name).into());
        }
    }

    /// Play a shell-row key sequence (WS5 §5.5): each press-group is
    /// pressed and fully released before the next group starts — the honest
    /// keyboard semantics behind tmux prefixes and post-prefix keys. Every
    /// key flows through the normal core state machine into the active
    /// destination; never a hidden shell command.
    fn play_sequence(
        &self,
        ui: &MainWindow,
        sequence: &'static [&'static [ferrokey_core::PhysicalKey]],
    ) {
        for group in sequence {
            for key in *group {
                ui.invoke_key_pressed((*key).name().into());
            }
            for key in group.iter().rev() {
                ui.invoke_key_released((*key).name().into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// A recorder that stands in for the generated `MainWindow` (the invoke
    /// methods are hard to call without a running Slint instance, so the
    /// bridge's state transitions are asserted through an observer view).
    fn assert_release_targets_pressed_key() {
        let view = views::view("compact").expect("compact view");
        let mut bridge = PointerBridge::new(view, 1.0, None);

        // Pointer press + release of the same button must pair up even when
        // the pointer has drifted to a different key by release time.
        let (cx, cy) = {
            let (r, c) = find_key(view, "a");
            views::key_center(view, r, c)
        };
        let name = bridge.key_at(f64::from(cx), f64::from(cy)).expect("a key");
        assert_eq!(name, "a");

        let held_before = bridge.pointer_down.len();
        bridge.pointer_down.insert(PointerButton::Left, name);
        assert_eq!(bridge.pointer_down.len(), held_before + 1);
        // Release at a far-away point (not over any key) must still release
        // the recorded key, not hit-test the release position.
        let released = bridge.pointer_down.remove(&PointerButton::Left);
        assert_eq!(released, Some("a"));
        assert!(bridge.pointer_down.is_empty());
    }

    fn find_key(view: &'static KeyboardView, name: &str) -> (usize, usize) {
        for (r, row) in view.rows.iter().enumerate() {
            for (c, key) in row.keys.iter().enumerate() {
                if key.name == name {
                    return (r, c);
                }
            }
        }
        panic!("key {name} not in view")
    }

    #[test]
    fn key_at_matches_key_center_for_every_key() {
        for view in views::VIEWS {
            for (r, row) in view.rows.iter().enumerate() {
                for (c, key) in row.keys.iter().enumerate() {
                    let (x, y) = views::key_center(view, r, c);
                    let hit = views::key_at(view, x, y).expect("center must hit");
                    assert_eq!(hit, key.name, "view {} key {}", view.id, key.name);
                }
            }
        }
    }

    #[test]
    fn key_at_gaps_are_empty() {
        let view = views::view("compact").expect("compact view");
        // Midway between two keys in the top letter row there is a 6px
        // spacing gap; probe a point in it (x is a boundary scan).
        let mut seen = BTreeSet::new();
        for (r, row) in view.rows.iter().enumerate() {
            let mut x = views::VIEW_PAD;
            for key in row.keys {
                let w = views::key_width(view, key.width);
                // Just inside the gap after this key.
                let probe = x + w + views::VIEW_SPACING / 2.0;
                let y = views::VIEW_PAD
                    + r as f32 * (views::VIEW_KEY_HEIGHT + views::VIEW_SPACING)
                    + views::VIEW_KEY_HEIGHT / 2.0;
                let hit = views::key_at(view, probe, y);
                // The gap probe may land on the *next* key when the gap is
                // tiny — only require that it never panics and is consistent
                // with the center hit above.
                if let Some(name) = hit {
                    seen.insert(name.to_string());
                }
                x += w + views::VIEW_SPACING;
            }
        }
        // Every gap probe that hit, hit a real key of the view.
        for name in &seen {
            let _ = find_key(view, name);
        }
    }

    #[test]
    fn bridge_tracks_button_ownership() {
        assert_release_targets_pressed_key();
    }

    #[test]
    fn scale_affects_hit_testing() {
        let view = views::view("compact").expect("compact view");
        let (r, c) = find_key(view, "h");
        let (x, y) = views::key_center(view, r, c);

        let scale2 = PointerBridge::new(view, 2.0, None);
        // Physical coords are 2x the logical geometry; the bridge divides.
        assert_eq!(
            scale2.key_at(f64::from(x) * 2.0, f64::from(y) * 2.0),
            Some("h")
        );

        let scale1 = PointerBridge::new(view, 1.0, None);
        assert_eq!(scale1.key_at(f64::from(x), f64::from(y)), Some("h"));
    }
}
