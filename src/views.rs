//! Keyboard views: visual arrangements over the *same* physical-key engine.
//!
//! Ferrokey's key semantics live in `ferrokey-core` and are view-independent.
//! A view only decides which physical keys are visible, in which order, at
//! what width, and with which label — exactly the "presentation is adaptive,
//! input is universal" split from the architecture spec.
//!
//! Two views ship today:
//!
//! * `compact` — the classic mobile-style 6-row OSK (the default).
//! * `full` — a complete desktop keyboard: function row, navigation cluster,
//!   numeric keypad, media and brightness keys. This is the view the
//!   compatibility courts use to exercise the parts of the key state engine
//!   a compact layout never touches.
//!
//! The geometry constants and [`key_center`] computation are mirrored by
//! `testing/courts/osk-geometry.py` (the courts click by coordinates); the
//! unit tests below pin exact positions so the mirror can never drift
//! silently.

/// Horizontal padding around each row (logical px).
pub const VIEW_PAD: f32 = 6.0;
/// Gap between keys (logical px).
pub const VIEW_SPACING: f32 = 6.0;
/// Key height (logical px).
pub const VIEW_KEY_HEIGHT: f32 = 52.0;
/// Minimum rendered key width (logical px). Never reached at the shipped
/// base widths; guards degenerate custom widths.
pub const VIEW_MIN_KEY_WIDTH: f32 = 24.0;
/// The title/status strip height (logical px). The OSK window is this strip
/// PLUS the keyboard view: the strip is the drag handle and shows the daemon
/// link state. The key geometry in this module is the KEYBOARD space only
/// (no strip offset); the window/pointer layer maps through the strip.
pub const TITLE_H: f32 = 22.0;

/// One key in a view row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewKey {
    /// Physical key name (see [`PhysicalKey::name`]). For chord keys this is
    /// a display placeholder (the chord is what matters); for the logo key it
    /// is a decorative placeholder that maps to no physical key.
    pub name: &'static str,
    /// Width factor relative to the view's base key width.
    pub width: f32,
    /// Display-label override, used when the active layout has no symbol for
    /// the key (e.g. Print Screen is physically `KEY_SYSRQ`; the cap says
    /// "print").
    pub label: Option<&'static str>,
    /// Optional key chord (terminal shortcut row, §55): pressing this key
    /// plays the listed physical keys in order (e.g. `["left-ctrl", "c"]`
    /// produces a genuine Ctrl+C through the core state machine — never an
    /// internal shell-command macro, §57).
    pub chord: Option<&'static [&'static str]>,
    /// Decorative logo button: renders the embedded brand image instead of a
    /// label and maps to no physical key (the pointer bridge ignores it).
    pub logo: bool,
}

impl ViewKey {
    pub const fn new(name: &'static str, width: f32) -> Self {
        ViewKey {
            name,
            width,
            label: None,
            chord: None,
            logo: false,
        }
    }

    pub const fn with_label(name: &'static str, width: f32, label: &'static str) -> Self {
        ViewKey {
            name,
            width,
            label: Some(label),
            chord: None,
            logo: false,
        }
    }

    pub const fn chord(name: &'static str, width: f32, chord: &'static [&'static str]) -> Self {
        ViewKey {
            name,
            width,
            label: Some(name),
            chord: Some(chord),
            logo: false,
        }
    }

    /// A decorative logo button (brand mark, e.g. next to F12). Maps to no
    /// physical key: the pointer bridge never turns it into a key event and
    /// it is excluded from the adaptive-geometry interaction model.
    pub const fn logo(name: &'static str, width: f32) -> Self {
        ViewKey {
            name,
            width,
            label: None,
            chord: None,
            logo: true,
        }
    }
}

/// One row of the view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewRow {
    pub keys: &'static [ViewKey],
}

/// A keyboard view: an arrangement of physical keys.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyboardView {
    /// Stable view id (`"compact"`, `"full"`, `"terminal"`).
    pub id: &'static str,
    /// Human-readable name.
    pub name: &'static str,
    /// Preferred window size in physical pixels.
    pub width: u32,
    pub height: u32,
    /// Base key width in logical px (widths above are factors of this).
    pub base_width: f32,
    /// The rows, top to bottom.
    pub rows: &'static [ViewRow],
}

impl KeyboardView {
    /// The chord attached to the key `name`, if any (terminal shortcut row,
    /// §55). Chord keys are played as real key sequences by the pointer
    /// bridge, never as internal shell-command macros (§57).
    pub fn chord_for(&self, name: &str) -> Option<&'static [&'static str]> {
        for row in self.rows {
            for key in row.keys {
                if key.name == name {
                    return key.chord;
                }
            }
        }
        None
    }
}

/// The rendered width of a key with `factor` in `view`.
pub fn key_width(view: &KeyboardView, factor: f32) -> f32 {
    (factor * view.base_width).max(VIEW_MIN_KEY_WIDTH)
}

/// The center of the key at (`row_index`, `key_index`), in physical pixels
/// relative to the window's top-left. Mirrors `osk-geometry.py`.
pub fn key_center(view: &KeyboardView, row_index: usize, key_index: usize) -> (f32, f32) {
    let row = &view.rows[row_index];
    let key = &row.keys[key_index];
    let mut x = VIEW_PAD;
    for k in row.keys.iter().take(key_index) {
        x += key_width(view, k.width) + VIEW_SPACING;
    }
    x += key_width(view, key.width) / 2.0;
    let y = VIEW_PAD + row_index as f32 * (VIEW_KEY_HEIGHT + VIEW_SPACING) + VIEW_KEY_HEIGHT / 2.0;
    (x, y)
}

/// The physical key under a point, or `None` if it falls in a gap.
///
/// Mirrors `osk-geometry.py` (the same row/width math as [`key_center`]);
/// the pointer bridge uses this to translate raw pointer/touch surface
/// events into key actions (rules 18, 85). Coordinates are the same space
/// as [`key_center`] (physical px at scale 1).
pub fn key_at(view: &KeyboardView, x: f32, y: f32) -> Option<&'static str> {
    for (row_index, row) in view.rows.iter().enumerate() {
        let mut x0 = VIEW_PAD;
        for key in row.keys {
            let w = key_width(view, key.width);
            let y0 = VIEW_PAD + row_index as f32 * (VIEW_KEY_HEIGHT + VIEW_SPACING);
            if x >= x0 && x <= x0 + w && y >= y0 && y <= y0 + VIEW_KEY_HEIGHT {
                return Some(key.name);
            }
            x0 += w + VIEW_SPACING;
        }
    }
    None
}

/// Sanity-check a view at startup: every key's *center* must be clickable
/// within the view's window. The courts click centers (see `osk-geometry.py`),
/// so a mis-tuned view is caught here with a clear warning instead of
/// silently breaking clicks.
pub fn check_geometry(view: &KeyboardView) {
    for (r, row) in view.rows.iter().enumerate() {
        for (c, key) in row.keys.iter().enumerate() {
            let (x, y) = key_center(view, r, c);
            let out_x = x < 0.0 || x > view.width as f32;
            let out_y = y < 0.0 || y > view.height as f32;
            if out_x || out_y {
                log::warn!(
                    "view {}: key {:?} center ({x:.1},{y:.1}) is outside the {}x{} window",
                    view.id,
                    key.name,
                    view.width,
                    view.height
                );
            }
        }
    }
}

/// Resolve a view by id.
pub fn view(id: &str) -> Option<&'static KeyboardView> {
    VIEWS.iter().find(|v| v.id == id)
}

/// The adaptive-geometry basis over a view: the visual rect of every key in
/// view order, the neighbor graph, and the key-name lookup (`index → name`).
///
/// The rects mirror [`key_center`] / [`key_at`] exactly (same row/width
/// math), so the initial adaptive hit regions are identical to the visual
/// rects and the OSK behaves byte-for-byte the same until it has learned.
/// Neighbors: left/right in a row plus any key in an adjacent row whose
/// horizontal interval overlaps (the competition graph, WS4 §4.13).
pub fn adaptive_geometry_basis(
    view: &'static KeyboardView,
) -> (
    Vec<ferrokey_core::geometry::Rect>,
    Vec<Vec<usize>>,
    Vec<&'static str>,
) {
    use ferrokey_core::geometry::Rect;

    let mut rects = Vec::new();
    let mut names = Vec::new();
    let mut row_of = Vec::new();
    for (r, row) in view.rows.iter().enumerate() {
        let mut x = VIEW_PAD;
        for key in row.keys {
            // Decorative keys (the logo) are not interaction targets: they
            // must never accumulate touch evidence or compete for space.
            let interactive = !key.logo
                && (key.chord.is_some()
                    || ferrokey_core::PhysicalKey::from_name(key.name).is_some());
            let w = key_width(view, key.width);
            if interactive {
                let y = VIEW_PAD + r as f32 * (VIEW_KEY_HEIGHT + VIEW_SPACING);
                rects.push(Rect::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(w),
                    f64::from(VIEW_KEY_HEIGHT),
                ));
                names.push(key.name);
                row_of.push(r);
            }
            x += w + VIEW_SPACING;
        }
    }

    let n = rects.len();
    let mut neighbors = vec![Vec::new(); n];
    for i in 0..n {
        // horizontal neighbours: previous / next index in the same row
        if i > 0 && row_of[i - 1] == row_of[i] {
            neighbors[i].push(i - 1);
        }
        if i + 1 < n && row_of[i + 1] == row_of[i] {
            neighbors[i].push(i + 1);
        }
        // vertical neighbours: any key in an adjacent row whose horizontal
        // interval overlaps this key's interval
        let (x0, x1) = (rects[i].x, rects[i].x + rects[i].w);
        for j in 0..n {
            if i == j || row_of[i].abs_diff(row_of[j]) != 1 {
                continue;
            }
            let (jx0, jx1) = (rects[j].x, rects[j].x + rects[j].w);
            if x0 < jx1 && jx0 < x1 {
                neighbors[i].push(j);
            }
        }
        neighbors[i].sort_unstable();
        neighbors[i].dedup();
    }
    (rects, neighbors, names)
}

/// All view ids, in deterministic order.
pub const VIEW_IDS: &[&str] = &["compact", "full", "terminal"];

// ────────────────────────────────────────────────────────────────────────────
// View data
// ────────────────────────────────────────────────────────────────────────────

/// `compact`: the mobile-style 6-row OSK. Kept byte-for-byte equivalent to
/// the original hard-coded rows so existing courts keep their geometry.
static COMPACT: KeyboardView = KeyboardView {
    id: "compact",
    name: "Compact",
    width: 936,
    height: 354,
    base_width: 58.0,
    rows: &[
        ViewRow {
            keys: &[
                ViewKey::new("escape", 1.6),
                ViewKey::new("f1", 1.0),
                ViewKey::new("f2", 1.0),
                ViewKey::new("f3", 1.0),
                ViewKey::new("f4", 1.0),
                ViewKey::new("f5", 1.0),
                ViewKey::new("f6", 1.0),
                ViewKey::new("f7", 1.0),
                ViewKey::new("f8", 1.0),
                ViewKey::new("f9", 1.0),
                ViewKey::new("f10", 1.0),
                ViewKey::new("f11", 1.0),
                ViewKey::new("f12", 1.0),
                // The brand mark sits next to F12: launcher identity on the
                // board itself. Decorative: no physical key, ignored by the
                // pointer bridge, excluded from adaptive geometry.
                ViewKey::logo("logo", 0.9),
            ],
        },
        ViewRow {
            keys: &[
                ViewKey::new("grave", 1.0),
                ViewKey::new("1", 1.0),
                ViewKey::new("2", 1.0),
                ViewKey::new("3", 1.0),
                ViewKey::new("4", 1.0),
                ViewKey::new("5", 1.0),
                ViewKey::new("6", 1.0),
                ViewKey::new("7", 1.0),
                ViewKey::new("8", 1.0),
                ViewKey::new("9", 1.0),
                ViewKey::new("0", 1.0),
                ViewKey::new("minus", 1.0),
                ViewKey::new("equal", 1.0),
                ViewKey::new("backspace", 1.6),
            ],
        },
        ViewRow {
            keys: &[
                ViewKey::new("tab", 1.6),
                ViewKey::new("q", 1.0),
                ViewKey::new("w", 1.0),
                ViewKey::new("e", 1.0),
                ViewKey::new("r", 1.0),
                ViewKey::new("t", 1.0),
                ViewKey::new("y", 1.0),
                ViewKey::new("u", 1.0),
                ViewKey::new("i", 1.0),
                ViewKey::new("o", 1.0),
                ViewKey::new("p", 1.0),
                ViewKey::new("left-bracket", 1.0),
                ViewKey::new("right-bracket", 1.0),
                ViewKey::new("backslash", 1.0),
            ],
        },
        ViewRow {
            keys: &[
                ViewKey::new("caps-lock", 1.6),
                ViewKey::new("a", 1.0),
                ViewKey::new("s", 1.0),
                ViewKey::new("d", 1.0),
                ViewKey::new("f", 1.0),
                ViewKey::new("g", 1.0),
                ViewKey::new("h", 1.0),
                ViewKey::new("j", 1.0),
                ViewKey::new("k", 1.0),
                ViewKey::new("l", 1.0),
                ViewKey::new("semicolon", 1.0),
                ViewKey::new("apostrophe", 1.0),
                ViewKey::new("enter", 1.6),
            ],
        },
        ViewRow {
            keys: &[
                ViewKey::new("left-shift", 1.6),
                ViewKey::new("z", 1.0),
                ViewKey::new("x", 1.0),
                ViewKey::new("c", 1.0),
                ViewKey::new("v", 1.0),
                ViewKey::new("b", 1.0),
                ViewKey::new("n", 1.0),
                ViewKey::new("m", 1.0),
                ViewKey::new("comma", 1.0),
                ViewKey::new("dot", 1.0),
                ViewKey::new("slash", 1.0),
                // Arrow cluster (compact): the up arrow sits directly above
                // down, same size (the space bar is 5.0 so the bottom-row
                // cluster aligns with the up key above it). left/down/right
                // replace right-shift/right-ctrl — the compact OSK keeps a
                // single shift and ctrl, each on the left.
                ViewKey::new("up", 1.0),
            ],
        },
        ViewRow {
            keys: &[
                ViewKey::new("left-ctrl", 1.0),
                ViewKey::new("left-meta", 1.0),
                ViewKey::new("left-alt", 1.0),
                // 4.9 (the compact default): the bottom-row arrow cluster
                // then sits EXACTLY under the up key on the row above
                // (same-size keys; row-5 has two more keys/gaps than row-6,
                // which the shorter space bar compensates).
                ViewKey::new("space", 4.9),
                ViewKey::new("right-alt", 1.0),
                ViewKey::new("compose", 1.0),
                ViewKey::new("menu", 1.0),
                // Arrow cluster (compact): left/down/right replace right-ctrl.
                ViewKey::new("left", 1.0),
                ViewKey::new("down", 1.0),
                ViewKey::new("right", 1.0),
            ],
        },
    ],
};

/// `full`: the complete desktop keyboard. Rows pack left; the navigation
/// cluster and keypad form a fixed right-hand block. The keypad is drawn as a
/// flat 4-column grid (no 2-row-tall keys) — visually simplified, functionally
/// complete: every keypad key is a real physical key.
static FULL: KeyboardView = KeyboardView {
    id: "full",
    name: "Full Desktop",
    width: 1160,
    height: 460,
    base_width: 40.0,
    rows: &[
        // Media / system row.
        ViewRow {
            keys: &[
                ViewKey::new("mute", 1.0),
                ViewKey::new("volume-down", 1.0),
                ViewKey::new("volume-up", 1.0),
                ViewKey::new("play-pause", 1.0),
                ViewKey::new("previous-song", 1.0),
                ViewKey::new("next-song", 1.0),
                ViewKey::new("brightness-down", 1.0),
                ViewKey::new("brightness-up", 1.0),
            ],
        },
        // Function row + extended keys (Print Screen is physically KEY_SYSRQ).
        ViewRow {
            keys: &[
                ViewKey::new("escape", 1.0),
                ViewKey::new("f1", 1.0),
                ViewKey::new("f2", 1.0),
                ViewKey::new("f3", 1.0),
                ViewKey::new("f4", 1.0),
                ViewKey::new("f5", 1.0),
                ViewKey::new("f6", 1.0),
                ViewKey::new("f7", 1.0),
                ViewKey::new("f8", 1.0),
                ViewKey::new("f9", 1.0),
                ViewKey::new("f10", 1.0),
                ViewKey::new("f11", 1.0),
                ViewKey::new("f12", 1.0),
                ViewKey::with_label("sysrq", 1.0, "print"),
                ViewKey::new("scroll-lock", 1.0),
                ViewKey::new("pause", 1.0),
            ],
        },
        // Number row + nav cluster + keypad operators.
        ViewRow {
            keys: &[
                ViewKey::new("grave", 1.0),
                ViewKey::new("1", 1.0),
                ViewKey::new("2", 1.0),
                ViewKey::new("3", 1.0),
                ViewKey::new("4", 1.0),
                ViewKey::new("5", 1.0),
                ViewKey::new("6", 1.0),
                ViewKey::new("7", 1.0),
                ViewKey::new("8", 1.0),
                ViewKey::new("9", 1.0),
                ViewKey::new("0", 1.0),
                ViewKey::new("minus", 1.0),
                ViewKey::new("equal", 1.0),
                ViewKey::new("backspace", 1.7),
                ViewKey::new("insert", 1.0),
                ViewKey::new("home", 1.0),
                ViewKey::new("page-up", 1.0),
                ViewKey::new("num-lock", 1.0),
                ViewKey::new("kp-divide", 1.0),
                ViewKey::new("kp-multiply", 1.0),
                ViewKey::new("kp-subtract", 1.0),
            ],
        },
        // Top letter row + nav cluster + keypad digits.
        ViewRow {
            keys: &[
                ViewKey::new("tab", 1.4),
                ViewKey::new("q", 1.0),
                ViewKey::new("w", 1.0),
                ViewKey::new("e", 1.0),
                ViewKey::new("r", 1.0),
                ViewKey::new("t", 1.0),
                ViewKey::new("y", 1.0),
                ViewKey::new("u", 1.0),
                ViewKey::new("i", 1.0),
                ViewKey::new("o", 1.0),
                ViewKey::new("p", 1.0),
                ViewKey::new("left-bracket", 1.0),
                ViewKey::new("right-bracket", 1.0),
                ViewKey::new("backslash", 1.0),
                ViewKey::new("delete", 1.0),
                ViewKey::new("end", 1.0),
                ViewKey::new("page-down", 1.0),
                ViewKey::new("kp7", 1.0),
                ViewKey::new("kp8", 1.0),
                ViewKey::new("kp9", 1.0),
                ViewKey::new("kp-add", 1.0),
            ],
        },
        // Home row + up arrow + keypad middle block.
        ViewRow {
            keys: &[
                ViewKey::new("caps-lock", 1.6),
                ViewKey::new("a", 1.0),
                ViewKey::new("s", 1.0),
                ViewKey::new("d", 1.0),
                ViewKey::new("f", 1.0),
                ViewKey::new("g", 1.0),
                ViewKey::new("h", 1.0),
                ViewKey::new("j", 1.0),
                ViewKey::new("k", 1.0),
                ViewKey::new("l", 1.0),
                ViewKey::new("semicolon", 1.0),
                ViewKey::new("apostrophe", 1.0),
                ViewKey::new("enter", 1.6),
                ViewKey::new("up", 1.0),
                ViewKey::new("kp4", 1.0),
                ViewKey::new("kp5", 1.0),
                ViewKey::new("kp6", 1.0),
                ViewKey::new("kp-enter", 1.0),
            ],
        },
        // Bottom letter row + arrow cluster + keypad digits.
        ViewRow {
            keys: &[
                ViewKey::new("left-shift", 2.2),
                ViewKey::new("z", 1.0),
                ViewKey::new("x", 1.0),
                ViewKey::new("c", 1.0),
                ViewKey::new("v", 1.0),
                ViewKey::new("b", 1.0),
                ViewKey::new("n", 1.0),
                ViewKey::new("m", 1.0),
                ViewKey::new("comma", 1.0),
                ViewKey::new("dot", 1.0),
                ViewKey::new("slash", 1.0),
                ViewKey::new("right-shift", 2.2),
                ViewKey::new("left", 1.0),
                ViewKey::new("down", 1.0),
                ViewKey::new("right", 1.0),
                ViewKey::new("kp1", 1.0),
                ViewKey::new("kp2", 1.0),
                ViewKey::new("kp3", 1.0),
                ViewKey::new("kp-decimal", 1.0),
            ],
        },
        // Modifier row + wide zero on the keypad.
        ViewRow {
            keys: &[
                ViewKey::new("left-ctrl", 1.0),
                ViewKey::new("left-meta", 1.0),
                ViewKey::new("left-alt", 1.0),
                ViewKey::new("space", 6.0),
                ViewKey::new("right-alt", 1.0),
                ViewKey::new("compose", 1.0),
                ViewKey::new("menu", 1.0),
                ViewKey::new("right-ctrl", 1.0),
                ViewKey::new("kp0", 1.6),
            ],
        },
    ],
};

/// `terminal`: the embedded terminal-workspace board (Phase 3 addendum #2,
/// §54–§56): a shortcut row of genuine key chords (Ctrl+C etc. — §55, §57),
/// the full letter/digit rows, the coding punctuation keys, navigation keys
/// and the terminal-critical modifiers. Arrows are on the home row so shell
/// history and editors are reachable without a layer switch.
static TERMINAL: KeyboardView = KeyboardView {
    id: "terminal",
    name: "Terminal",
    width: 936,
    height: 354,
    base_width: 58.0,
    rows: &[
        // Shortcut row: chords, never shell-command macros (§55, §57). The
        // first five are the static chords; the shell-aware rows (WS5) swap
        // this row for the detected shell's row (bash/zsh/fish/nushell/
        // tmux/ssh) — the swap is presentation-only and keeps the logo.
        ViewRow {
            keys: &[
                ViewKey::chord("Ctrl+C", 1.3, &["left-ctrl", "c"]),
                ViewKey::chord("Ctrl+D", 1.3, &["left-ctrl", "d"]),
                ViewKey::chord("Ctrl+Z", 1.3, &["left-ctrl", "z"]),
                ViewKey::chord("Ctrl+L", 1.3, &["left-ctrl", "l"]),
                ViewKey::chord("Ctrl+A", 1.3, &["left-ctrl", "a"]),
                ViewKey::new("escape", 1.3),
                ViewKey::new("home", 1.0),
                ViewKey::new("end", 1.0),
                // The brand mark (same decorative key as the compact view).
                ViewKey::logo("logo", 0.9),
            ],
        },
        // Rows 2–6 mirror the compact view EXACTLY (the main keyboard's
        // design language: the number/tab/home letter rows, the arrow
        // cluster at the bottom-right with up directly above down, compose
        // + menu, a single left-side shift/ctrl and the 4.9-unit space bar).
        // The terminal differs only in row 1 (the shortcut row).
        ViewRow {
            keys: &[
                ViewKey::new("grave", 1.0),
                ViewKey::new("1", 1.0),
                ViewKey::new("2", 1.0),
                ViewKey::new("3", 1.0),
                ViewKey::new("4", 1.0),
                ViewKey::new("5", 1.0),
                ViewKey::new("6", 1.0),
                ViewKey::new("7", 1.0),
                ViewKey::new("8", 1.0),
                ViewKey::new("9", 1.0),
                ViewKey::new("0", 1.0),
                ViewKey::new("minus", 1.0),
                ViewKey::new("equal", 1.0),
                ViewKey::new("backspace", 1.6),
            ],
        },
        ViewRow {
            keys: &[
                ViewKey::new("tab", 1.6),
                ViewKey::new("q", 1.0),
                ViewKey::new("w", 1.0),
                ViewKey::new("e", 1.0),
                ViewKey::new("r", 1.0),
                ViewKey::new("t", 1.0),
                ViewKey::new("y", 1.0),
                ViewKey::new("u", 1.0),
                ViewKey::new("i", 1.0),
                ViewKey::new("o", 1.0),
                ViewKey::new("p", 1.0),
                ViewKey::new("left-bracket", 1.0),
                ViewKey::new("right-bracket", 1.0),
                ViewKey::new("backslash", 1.0),
            ],
        },
        ViewRow {
            keys: &[
                ViewKey::new("caps-lock", 1.6),
                ViewKey::new("a", 1.0),
                ViewKey::new("s", 1.0),
                ViewKey::new("d", 1.0),
                ViewKey::new("f", 1.0),
                ViewKey::new("g", 1.0),
                ViewKey::new("h", 1.0),
                ViewKey::new("j", 1.0),
                ViewKey::new("k", 1.0),
                ViewKey::new("l", 1.0),
                ViewKey::new("semicolon", 1.0),
                ViewKey::new("apostrophe", 1.0),
                ViewKey::new("enter", 1.6),
            ],
        },
        ViewRow {
            keys: &[
                ViewKey::new("left-shift", 1.6),
                ViewKey::new("z", 1.0),
                ViewKey::new("x", 1.0),
                ViewKey::new("c", 1.0),
                ViewKey::new("v", 1.0),
                ViewKey::new("b", 1.0),
                ViewKey::new("n", 1.0),
                ViewKey::new("m", 1.0),
                ViewKey::new("comma", 1.0),
                ViewKey::new("dot", 1.0),
                ViewKey::new("slash", 1.0),
                ViewKey::new("up", 1.0),
            ],
        },
        ViewRow {
            keys: &[
                ViewKey::new("left-ctrl", 1.0),
                ViewKey::new("left-meta", 1.0),
                ViewKey::new("left-alt", 1.0),
                ViewKey::new("space", 4.9),
                ViewKey::new("right-alt", 1.0),
                ViewKey::new("compose", 1.0),
                ViewKey::new("menu", 1.0),
                ViewKey::new("left", 1.0),
                ViewKey::new("down", 1.0),
                ViewKey::new("right", 1.0),
            ],
        },
    ],
};

/// All views, in [`VIEW_IDS`] order.
pub static VIEWS: &[KeyboardView] = &[COMPACT, FULL, TERMINAL];

#[cfg(test)]
mod tests {
    use super::*;
    use ferrokey_core::PhysicalKey;

    #[test]
    fn every_view_key_is_a_real_physical_key() {
        for view in VIEWS {
            for row in view.rows {
                for key in row.keys {
                    // Chord keys are played by the bridge as sequences of
                    // physical keys; their own names are display placeholders.
                    // Logo keys are decorative (no physical key by design).
                    if key.chord.is_some() || key.logo {
                        continue;
                    }
                    assert!(
                        PhysicalKey::from_name(key.name).is_some(),
                        "view {}: unknown physical key {:?}",
                        view.id,
                        key.name
                    );
                }
            }
        }
    }

    #[test]
    fn every_logo_key_is_decorative() {
        for view in VIEWS {
            for row in view.rows {
                for key in row.keys {
                    if key.logo {
                        assert_eq!(key.chord, None, "logo keys carry no chord");
                        assert_eq!(key.label, None, "logo keys carry no label");
                        assert!(
                            PhysicalKey::from_name(key.name).is_none(),
                            "logo key {:?} must not shadow a physical key",
                            key.name
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn every_chord_is_physical_keys() {
        for view in VIEWS {
            for row in view.rows {
                for key in row.keys {
                    if let Some(chord) = key.chord {
                        assert!(
                            !chord.is_empty() && chord.len() <= 4,
                            "view {}: chord {:?} length",
                            view.id,
                            chord
                        );
                        for member in chord {
                            assert!(
                                PhysicalKey::from_name(member).is_some(),
                                "view {}: chord member {:?} is not a physical key",
                                view.id,
                                member
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn every_view_key_has_a_positive_width() {
        for view in VIEWS {
            for row in view.rows {
                for key in row.keys {
                    assert!(
                        key.width > 0.0,
                        "view {}: key {} width {}",
                        view.id,
                        key.name,
                        key.width
                    );
                }
            }
        }
    }

    #[test]
    fn no_duplicate_keys_within_a_row() {
        for view in VIEWS {
            for row in view.rows {
                let mut seen = std::collections::BTreeSet::new();
                for key in row.keys {
                    assert!(
                        seen.insert(key.name),
                        "view {}: duplicate key {:?} in one row",
                        view.id,
                        key.name
                    );
                }
            }
        }
    }

    #[test]
    fn every_key_center_is_clickable_within_the_window() {
        // The courts click key centers, so the real invariant is: every
        // center must land inside the window (right edges may clip — the
        // original compact layout already relied on that).
        for view in VIEWS {
            for (r, row) in view.rows.iter().enumerate() {
                for (c, key) in row.keys.iter().enumerate() {
                    let (x, y) = key_center(view, r, c);
                    assert!(
                        x >= 0.0 && x <= view.width as f32,
                        "view {}: key {} center x={x} outside {}px window",
                        view.id,
                        key.name,
                        view.width
                    );
                    assert!(
                        y >= 0.0 && y <= view.height as f32,
                        "view {}: key {} center y={y} outside {}px window",
                        view.id,
                        key.name,
                        view.height
                    );
                }
            }
        }
    }

    #[test]
    fn geometry_check_invariants_hold() {
        // check_geometry warns on violations; the invariants it enforces must
        // hold for both shipped views.
        for view in VIEWS {
            for (r, row) in view.rows.iter().enumerate() {
                for (c, _key) in row.keys.iter().enumerate() {
                    let (x, y) = key_center(view, r, c);
                    assert!(x >= 0.0 && x <= view.width as f32);
                    assert!(y >= 0.0 && y <= view.height as f32);
                }
            }
        }
    }

    #[test]
    fn view_lookup() {
        assert_eq!(view("compact").map(|v| v.id), Some("compact"));
        assert_eq!(view("full").map(|v| v.id), Some("full"));
        assert_eq!(view("terminal").map(|v| v.id), Some("terminal"));
        assert_eq!(view("nope"), None);
    }

    /// Pin the exact click coordinates the courts rely on. These numbers are
    /// mirrored by `testing/courts/osk-geometry.py` (which truncates to int
    /// pixels); a change here without a matching change there breaks every
    /// court silently.
    #[test]
    fn pinned_compact_geometry() {
        let v = view("compact").unwrap();
        let row = |name: &str| -> usize {
            v.rows
                .iter()
                .position(|r| r.keys.iter().any(|k| k.name == name))
                .unwrap_or_else(|| panic!("{name} not in compact view"))
        };
        let col = |name: &str| -> usize {
            let r = row(name);
            v.rows[r].keys.iter().position(|k| k.name == name).unwrap()
        };
        assert_eq!(key_center(v, row("a"), col("a")), (133.8, 206.0));
        assert_eq!(key_center(v, row("e"), col("e")), (261.8, 148.0));
        assert_eq!(key_center(v, row("space"), col("space")), (340.1, 322.0));
        assert_eq!(
            key_center(v, row("apostrophe"), col("apostrophe")),
            (773.8, 206.0)
        );
        // The compact arrow cluster: up sits directly above down, same size
        // (sub-pixel residual comes from the row key-count asymmetry: row 5
        // carries two more keys/gaps before the cluster than row 6).
        let up = key_center(v, row("up"), col("up"));
        let down = key_center(v, row("down"), col("down"));
        assert!(
            (up.0 - down.0).abs() <= 1.0,
            "up ({up:?}) must be centered over down ({down:?})"
        );
        let above = up.1 + VIEW_KEY_HEIGHT + VIEW_SPACING;
        assert!(
            (above - down.1).abs() <= 1.0,
            "up must sit directly above down ({up:?} vs {down:?})"
        );
    }

    #[test]
    fn pinned_full_geometry() {
        let v = view("full").unwrap();
        let find = |name: &str| -> (usize, usize) {
            for (r, row) in v.rows.iter().enumerate() {
                if let Some(c) = row.keys.iter().position(|k| k.name == name) {
                    return (r, c);
                }
            }
            panic!("{name} not in full view");
        };
        for (name, expected) in [
            ("mute", (26.0, 32.0)),
            ("sysrq", (624.0, 90.0)),
            ("kp7", (824.0, 206.0)),
            ("space", (264.0, 380.0)),
            ("kp0", (606.0, 380.0)),
        ] {
            let (r, c) = find(name);
            assert_eq!(key_center(v, r, c), expected, "{name} center mismatch");
        }
    }

    #[test]
    fn pinned_terminal_geometry() {
        // The terminal view shares rows 2-6 with the compact view; row 1 is
        // the shortcut row (chords + the brand mark). Pin the same anchors as
        // the compact test plus the terminal-specific row so the geometry
        // mirror (osk-geometry.py) cannot drift.
        let v = view("terminal").unwrap();
        let find = |name: &str| -> (usize, usize) {
            for (r, row) in v.rows.iter().enumerate() {
                if let Some(c) = row.keys.iter().position(|k| k.name == name) {
                    return (r, c);
                }
            }
            panic!("{name} not in terminal view");
        };
        for (name, expected) in [
            ("Ctrl+C", (43.699_997, 32.0)),
            ("logo", (648.49994, 32.0)),
            ("backspace", (884.4, 90.0)),
            ("a", (133.8, 206.0)),
            ("space", (340.1, 322.0)),
            ("up", (773.8, 264.0)),
            ("down", (773.2, 322.0)),
        ] {
            let (r, c) = find(name);
            assert_eq!(key_center(v, r, c), expected, "{name} center mismatch");
        }
        // The compact design language holds: up sits directly above down,
        // same size (the shared 4.9-unit space bar aligns the bottom-row
        // arrow cluster under it).
        let up = key_center(v, find("up").0, find("up").1);
        let down = key_center(v, find("down").0, find("down").1);
        assert!(
            (up.0 - down.0).abs() <= 1.0,
            "up ({up:?}) must be centered over down ({down:?})"
        );
        let above = up.1 + VIEW_KEY_HEIGHT + VIEW_SPACING;
        assert!(
            (above - down.1).abs() <= 1.0,
            "up must sit directly above down ({up:?} vs {down:?})"
        );
    }

    #[test]
    fn full_view_covers_the_extended_keyboard() {
        let v = view("full").unwrap();
        let names: std::collections::BTreeSet<&str> = v
            .rows
            .iter()
            .flat_map(|r| r.keys.iter().map(|k| k.name))
            .collect();
        // Navigation / editing cluster.
        for n in [
            "insert",
            "delete",
            "home",
            "end",
            "page-up",
            "page-down",
            "up",
            "down",
            "left",
            "right",
        ] {
            assert!(names.contains(n), "full view missing {n}");
        }
        // Complete numeric keypad.
        for n in [
            "num-lock",
            "kp0",
            "kp1",
            "kp2",
            "kp3",
            "kp4",
            "kp5",
            "kp6",
            "kp7",
            "kp8",
            "kp9",
            "kp-decimal",
            "kp-enter",
            "kp-add",
            "kp-subtract",
            "kp-multiply",
            "kp-divide",
        ] {
            assert!(names.contains(n), "full view missing {n}");
        }
        // Extended + media.
        for n in [
            "sysrq",
            "scroll-lock",
            "pause",
            "mute",
            "volume-down",
            "volume-up",
            "play-pause",
            "next-song",
            "previous-song",
            "brightness-down",
            "brightness-up",
        ] {
            assert!(names.contains(n), "full view missing {n}");
        }
    }
}
