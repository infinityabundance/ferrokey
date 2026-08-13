//! The terminal key encoder (§11–§16): physical keys + modifiers + terminal
//! modes → the exact byte stream a real terminal would emit.
//!
//! This is *not* a desktop-keyboard emulation. `A` produces `a`, `Shift+A`
//! produces `A`, `Enter` produces CR, `Ctrl+C` produces `0x03`, `Alt+X`
//! produces `ESC x`, arrows produce mode-dependent CSI/SS3 sequences, and the
//! keypad respects application-keypad mode.
//!
//! The encoder is a pure function of its inputs — no mutable state. Modifier
//! tracking lives in [`crate::sink::TerminalKeySink`], which observes the
//! same physical-key event stream the ferrokey-core state machine emits (so
//! latched/locked/sticky modifiers all flow through unchanged, §53).
//!
//! Symbol resolution (which character a key produces) is delegated to the
//! active `ferrokey-core` layout; the encoder never hard-codes a keyboard
//! layout for printable keys (§49).

use crate::modes::TerminalModes;
use ferrokey_core::{KeySymbol, Layout, ModifierKind, ModifierSet, PhysicalKey};
use std::sync::Arc;

/// The encoded bytes for one key action, or nothing to send.
pub type EncodedInput = Option<Vec<u8>>;

/// Maps a physical key to the *base* (unshifted) ASCII character on the
/// standard US-position keyboard. Control characters are defined against
/// these base characters regardless of the active layout (that is how the
/// terminal protocol works: Ctrl+C is 0x03 on every layout).
pub fn base_ascii(key: PhysicalKey) -> Option<char> {
    use PhysicalKey::*;
    Some(match key {
        Grave => '`',
        D1 | Kp1 => '1',
        D2 | Kp2 => '2',
        D3 | Kp3 => '3',
        D4 | Kp4 => '4',
        D5 | Kp5 => '5',
        D6 | Kp6 => '6',
        D7 | Kp7 => '7',
        D8 | Kp8 => '8',
        D9 | Kp9 => '9',
        D0 | Kp0 => '0',
        Minus | KpSubtract => '-',
        Equal | KpEqual => '=',
        Q => 'q',
        W => 'w',
        E => 'e',
        R => 'r',
        T => 't',
        Y => 'y',
        U => 'u',
        I => 'i',
        O => 'o',
        P => 'p',
        LeftBracket => '[',
        RightBracket => ']',
        A => 'a',
        S => 's',
        D => 'd',
        F => 'f',
        G => 'g',
        H => 'h',
        J => 'j',
        K => 'k',
        L => 'l',
        Semicolon => ';',
        Apostrophe => '\'',
        Z => 'z',
        X => 'x',
        C => 'c',
        V => 'v',
        B => 'b',
        N => 'n',
        M => 'm',
        Comma | KpComma => ',',
        Dot | KpDecimal => '.',
        Slash | KpDivide => '/',
        Backslash | IntlBackslash => '\\',
        Space => ' ',
        KpAdd => '+',
        KpMultiply => '*',
        _ => return None,
    })
}

/// The xterm modifier parameter for a modifier set (2=Shift, 3=Alt,
/// 4=Shift+Alt, 5=Ctrl, 6=Shift+Ctrl, 7=Alt+Ctrl, 8=Shift+Alt+Ctrl).
fn modifier_param(modifier_set: ModifierSet) -> Option<u8> {
    let mut m = 0u8;
    if modifier_set.contains(ModifierSet::SHIFT) {
        m |= 1;
    }
    if modifier_set.contains(ModifierSet::ALT) || modifier_set.contains(ModifierSet::META) {
        m |= 2;
    }
    if modifier_set.contains(ModifierSet::CTRL) {
        m |= 4;
    }
    if m == 0 {
        None
    } else {
        Some(m + 1)
    }
}

/// Ctrl+letter → control character (§12).
pub fn ctrl_byte(ch: char) -> Option<u8> {
    let upper = ch.to_ascii_uppercase();
    if upper.is_ascii_uppercase() {
        Some(upper as u8 & 0x1F)
    } else {
        None
    }
}

/// The control character for Ctrl+{@, [, \, ], ^, _, space} and the
/// xterm digit conventions (Ctrl+2 = NUL … Ctrl+8 = DEL).
fn ctrl_punctuation(base: char) -> Option<u8> {
    Some(match base {
        '@' | '2' | ' ' => 0x00,
        '3' | '[' => 0x1B,
        '4' | '\\' => 0x1C,
        '5' | ']' => 0x1D,
        '6' => 0x1E,
        '7' => 0x1F,
        '8' => 0x7F,
        _ => return None,
    })
}

/// Whether a physical key is a keypad key (encoded by `special_bytes` only,
/// never through the layout symbol path, so application-keypad mode works).
fn is_keypad(key: PhysicalKey) -> bool {
    use PhysicalKey::*;
    matches!(
        key,
        Kp0 | Kp1
            | Kp2
            | Kp3
            | Kp4
            | Kp5
            | Kp6
            | Kp7
            | Kp8
            | Kp9
            | KpDecimal
            | KpEnter
            | KpAdd
            | KpSubtract
            | KpMultiply
            | KpDivide
            | KpEqual
            | KpComma
            | KpLeftParen
            | KpRightParen
    )
}

/// The terminal key encoder. Stateless and deterministic.
#[derive(Debug, Clone)]
pub struct TerminalKeyEncoder {
    layout: Arc<Layout>,
}

impl TerminalKeyEncoder {
    pub fn new(layout: Arc<Layout>) -> Self {
        TerminalKeyEncoder { layout }
    }

    pub fn layout(&self) -> &Arc<Layout> {
        &self.layout
    }

    /// Encode a key press. `modifier_set` must be the *current* held/latched/locked
    /// modifier set as observed by the sink. Returns `None` for keys that
    /// produce no terminal output (modifiers, lock keys, media keys).
    pub fn encode(
        &self,
        key: PhysicalKey,
        modifier_set: ModifierSet,
        modes: &TerminalModes,
    ) -> EncodedInput {
        // Modifiers and lock keys only affect other keys.
        if key.is_modifier() || key.is_lock_key() {
            return None;
        }

        let ctrl = modifier_set.contains(ModifierSet::CTRL);
        let alt =
            modifier_set.contains(ModifierSet::ALT) || modifier_set.contains(ModifierSet::META);

        // Control characters take precedence over everything: with Ctrl held
        // (and no Alt) the key produces its control char, or — when there is
        // no plain control char (navigation keys, keypad) — the modified
        // special sequence (Ctrl+Up → `CSI 1;5A`). The layout symbol path is
        // never consulted: Ctrl+6 must never emit "6".
        if ctrl && !alt {
            if let Some(byte) = self.ctrl_sequence(key) {
                return Some(vec![byte]);
            }
            return self.special_bytes(key, modifier_set, modes);
        }

        // Alt (and Alt+Ctrl) prefix with ESC.
        if alt {
            let inner: Vec<u8> = if ctrl {
                self.ctrl_sequence(key)
                    .map(|b| vec![b])
                    .or_else(|| self.special_bytes(key, modifier_set, modes))
                    .unwrap_or_default()
            } else {
                self.symbol_or_special(key, modifier_set, modes)
            };
            if inner.is_empty() {
                return None;
            }
            let mut out = Vec::with_capacity(inner.len() + 1);
            out.push(0x1B);
            out.extend_from_slice(&inner);
            return Some(out);
        }

        // Plain keys: layout symbol first (except the keypad, which is
        // mode-dependent), then special-key handling.
        if !is_keypad(key) {
            if let Some(bytes) = self.symbol_bytes(key, modifier_set) {
                return Some(bytes);
            }
        }
        self.special_bytes(key, modifier_set, modes)
    }

    /// Symbol bytes, or the special-key encoding, or nothing.
    fn symbol_or_special(
        &self,
        key: PhysicalKey,
        modifier_set: ModifierSet,
        modes: &TerminalModes,
    ) -> Vec<u8> {
        if !is_keypad(key) {
            if let Some(bytes) = self.symbol_bytes(key, modifier_set) {
                return bytes;
            }
        }
        self.special_bytes(key, modifier_set, modes)
            .unwrap_or_default()
    }

    /// The control byte for a key with Ctrl held, if one is defined.
    fn ctrl_sequence(&self, key: PhysicalKey) -> Option<u8> {
        if let Some(ch) = base_ascii(key) {
            if let Some(b) = ctrl_byte(ch) {
                return Some(b);
            }
            if let Some(b) = ctrl_punctuation(ch) {
                return Some(b);
            }
            // Ctrl+digits (except the defined ones) produce nothing, like
            // xterm without modifyOtherKeys.
            return None;
        }
        match key {
            PhysicalKey::Backspace => Some(0x7F),
            PhysicalKey::Tab => Some(0x09),
            PhysicalKey::Enter | PhysicalKey::KpEnter => Some(0x0D),
            PhysicalKey::Escape => Some(0x1B),
            PhysicalKey::Space => Some(0x00),
            _ => None,
        }
    }

    /// The UTF-8 bytes for a printable key via the active layout. Only
    /// printable-position keys (letters, digits, punctuation, space) resolve
    /// through the layout; navigation/function/editing keys are encoded by
    /// [`TerminalKeyEncoder::special_bytes`] — the layout's label glyphs for
    /// arrows (↑↓←→) are display-only and must never become terminal bytes.
    /// Dead keys and the compose key fall back to the base ASCII character so
    /// typing remains usable on intl layouts.
    fn symbol_bytes(&self, key: PhysicalKey, modifier_set: ModifierSet) -> Option<Vec<u8>> {
        // Printable-position gate: no base character → not a printable key.
        let base = base_ascii(key)?;
        let layout_mods = modifier_set.intersection(
            ModifierSet::SHIFT
                .union(ModifierSet::ALTGR)
                .union(ModifierSet::FN),
        );
        let symbol = self.layout.symbol_for(key, layout_mods).cloned();
        let ch = match symbol {
            Some(KeySymbol::Char(c)) => c,
            Some(KeySymbol::Dead(_) | KeySymbol::Compose | KeySymbol::None) | None => base,
            Some(KeySymbol::Name(_)) => return None,
        };
        if ch.is_control() {
            return None;
        }
        let mut buf = [0u8; 4];
        Some(ch.encode_utf8(&mut buf).as_bytes().to_vec())
    }

    /// Non-printable, terminal-meaningful keys.
    fn special_bytes(
        &self,
        key: PhysicalKey,
        modifier_set: ModifierSet,
        modes: &TerminalModes,
    ) -> Option<Vec<u8>> {
        use PhysicalKey::*;
        let mod_param = modifier_param(modifier_set);
        let tilda = |code: u8| {
            mod_param.map_or_else(|| format!("\x1b[{code}~"), |p| format!("\x1b[{code};{p}~"))
        };
        let app_arrows = modes.application_cursor_keys;
        let app_keypad = modes.application_keypad;

        match key {
            Enter | KpEnter => {
                if app_keypad && key == KpEnter {
                    Some(b"\x1bOM".to_vec())
                } else {
                    Some(b"\r".to_vec())
                }
            }
            Tab => {
                if modifier_set.contains(ModifierSet::SHIFT) {
                    Some(b"\x1b[Z".to_vec())
                } else {
                    Some(b"\t".to_vec())
                }
            }
            Backspace => Some(vec![0x7F]),
            Escape => Some(vec![0x1B]),
            Up => {
                if let Some(p) = mod_param {
                    Some(format!("\x1b[1;{p}A").into_bytes())
                } else if app_arrows {
                    Some(b"\x1bOA".to_vec())
                } else {
                    Some(b"\x1b[A".to_vec())
                }
            }
            Down => {
                if let Some(p) = mod_param {
                    Some(format!("\x1b[1;{p}B").into_bytes())
                } else if app_arrows {
                    Some(b"\x1bOB".to_vec())
                } else {
                    Some(b"\x1b[B".to_vec())
                }
            }
            Right => {
                if let Some(p) = mod_param {
                    Some(format!("\x1b[1;{p}C").into_bytes())
                } else if app_arrows {
                    Some(b"\x1bOC".to_vec())
                } else {
                    Some(b"\x1b[C".to_vec())
                }
            }
            Left => {
                if let Some(p) = mod_param {
                    Some(format!("\x1b[1;{p}D").into_bytes())
                } else if app_arrows {
                    Some(b"\x1bOD".to_vec())
                } else {
                    Some(b"\x1b[D".to_vec())
                }
            }
            Home => {
                if let Some(p) = mod_param {
                    Some(format!("\x1b[1;{p}H").into_bytes())
                } else if app_arrows {
                    Some(b"\x1bOH".to_vec())
                } else {
                    Some(b"\x1b[H".to_vec())
                }
            }
            End => {
                if let Some(p) = mod_param {
                    Some(format!("\x1b[1;{p}F").into_bytes())
                } else if app_arrows {
                    Some(b"\x1bOF".to_vec())
                } else {
                    Some(b"\x1b[F".to_vec())
                }
            }
            Insert => Some(tilda(2).into_bytes()),
            Delete => Some(tilda(3).into_bytes()),
            PageUp => Some(tilda(5).into_bytes()),
            PageDown => Some(tilda(6).into_bytes()),
            F1 => Some(
                mod_param
                    .map(|p| format!("\x1b[1;{p}P"))
                    .unwrap_or_else(|| "\x1bOP".into())
                    .into_bytes(),
            ),
            F2 => Some(
                mod_param
                    .map(|p| format!("\x1b[1;{p}Q"))
                    .unwrap_or_else(|| "\x1bOQ".into())
                    .into_bytes(),
            ),
            F3 => Some(
                mod_param
                    .map(|p| format!("\x1b[1;{p}R"))
                    .unwrap_or_else(|| "\x1bOR".into())
                    .into_bytes(),
            ),
            F4 => Some(
                mod_param
                    .map(|p| format!("\x1b[1;{p}S"))
                    .unwrap_or_else(|| "\x1bOS".into())
                    .into_bytes(),
            ),
            F5 => Some(tilda(15).into_bytes()),
            F6 => Some(tilda(17).into_bytes()),
            F7 => Some(tilda(18).into_bytes()),
            F8 => Some(tilda(19).into_bytes()),
            F9 => Some(tilda(20).into_bytes()),
            F10 => Some(tilda(21).into_bytes()),
            F11 => Some(tilda(23).into_bytes()),
            F12 => Some(tilda(24).into_bytes()),
            F13 => Some(tilda(25).into_bytes()),
            F14 => Some(tilda(26).into_bytes()),
            F15 => Some(tilda(28).into_bytes()),
            F16 => Some(tilda(29).into_bytes()),
            F17 => Some(tilda(31).into_bytes()),
            F18 => Some(tilda(32).into_bytes()),
            F19 => Some(tilda(33).into_bytes()),
            F20 => Some(tilda(34).into_bytes()),
            // The keypad: application mode emits SS3 sequences; numeric mode
            // emits the plain character.
            Kp0 | Kp1 | Kp2 | Kp3 | Kp4 | Kp5 | Kp6 | Kp7 | Kp8 | Kp9 | KpDecimal | KpAdd
            | KpSubtract | KpMultiply | KpDivide | KpEqual | KpComma => {
                let ch = base_ascii(key)?;
                if app_keypad {
                    let ss3 = match key {
                        Kp0 => b'p',
                        Kp1 => b'q',
                        Kp2 => b'r',
                        Kp3 => b's',
                        Kp4 => b't',
                        Kp5 => b'u',
                        Kp6 => b'v',
                        Kp7 => b'w',
                        Kp8 => b'x',
                        Kp9 => b'y',
                        KpDecimal => b'n',
                        KpAdd => b'k',
                        KpSubtract => b'm',
                        KpMultiply => b'j',
                        KpDivide => b'o',
                        KpEqual => b'X',
                        KpComma => b'l',
                        _ => return None,
                    };
                    let mut out = vec![0x1B, b'O'];
                    out.push(ss3);
                    Some(out)
                } else {
                    let mut buf = [0u8; 4];
                    Some(ch.encode_utf8(&mut buf).as_bytes().to_vec())
                }
            }
            KpLeftParen => {
                if app_keypad {
                    Some(b"\x1bO(".to_vec())
                } else {
                    Some(b"(".to_vec())
                }
            }
            KpRightParen => {
                if app_keypad {
                    Some(b"\x1bO)".to_vec())
                } else {
                    Some(b")".to_vec())
                }
            }
            // Media/function keys without a terminal meaning.
            _ => None,
        }
    }

    /// Whether this key has *any* terminal output (used by the UI to decide
    /// whether a tap should go to the terminal).
    pub fn produces_output(
        &self,
        key: PhysicalKey,
        modifier_set: ModifierSet,
        modes: &TerminalModes,
    ) -> bool {
        self.encode(key, modifier_set, modes).is_some()
    }

    /// The modifier kinds this encoder consumes (for diagnostics).
    pub const fn relevant_modifiers() -> &'static [ModifierKind] {
        &[
            ModifierKind::Shift,
            ModifierKind::Ctrl,
            ModifierKind::Alt,
            ModifierKind::Meta,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout_us() -> Arc<Layout> {
        Arc::new(ferrokey_core::Layout::empty("us", "US test"))
    }

    fn enc(modifier_set: ModifierSet, modes: &TerminalModes) -> TerminalKeyEncoder {
        // Deterministic for tests; symbol_for on the empty layout returns None
        // for everything, so printable tests use base_ascii fallback only
        // when the symbol is None — which the empty layout provides.
        let _ = (modifier_set, modes);
        TerminalKeyEncoder::new(layout_us())
    }

    fn encode(key: PhysicalKey, modifier_set: ModifierSet) -> Vec<u8> {
        let e = enc(modifier_set, &TerminalModes::default());
        e.encode(key, modifier_set, &TerminalModes::default())
            .unwrap_or_default()
    }

    #[test]
    fn letters_use_layout() {
        // Empty layout: fall back to base ASCII.
        assert_eq!(encode(PhysicalKey::A, ModifierSet::empty()), b"a");
        assert_eq!(encode(PhysicalKey::Q, ModifierSet::empty()), b"q");
    }

    #[test]
    fn shift_makes_uppercase_via_layout_symbol() {
        // The empty layout has no Shift level; verify the modifier mask path
        // doesn't panic and Shift+letter yields the shifted symbol when the
        // layout provides one.
        let e = TerminalKeyEncoder::new(Arc::new(us_layout_with_shift()));
        let out = e
            .encode(
                PhysicalKey::A,
                ModifierSet::SHIFT,
                &TerminalModes::default(),
            )
            .unwrap();
        assert_eq!(out, b"A");
    }

    fn us_layout_with_shift() -> ferrokey_core::Layout {
        use ferrokey_core::{KeyDefinition, KeySymbol, Layout};
        let mut keys = std::collections::HashMap::new();
        keys.insert(
            PhysicalKey::A,
            KeyDefinition {
                physical: PhysicalKey::A,
                primary: KeySymbol::Char('a'),
                shifted: KeySymbol::Char('A'),
                altgr: None,
                shift_altgr: None,
                fn_layer: None,
                repeatable: true,
            },
        );
        Layout {
            id: "us-shift-test".into(),
            name: "US Shift test".into(),
            keys,
        }
    }

    #[test]
    fn enter_is_cr() {
        assert_eq!(encode(PhysicalKey::Enter, ModifierSet::empty()), b"\r");
        assert_eq!(encode(PhysicalKey::KpEnter, ModifierSet::empty()), b"\r");
    }

    #[test]
    fn tab_and_shift_tab() {
        assert_eq!(encode(PhysicalKey::Tab, ModifierSet::empty()), b"\t");
        assert_eq!(encode(PhysicalKey::Tab, ModifierSet::SHIFT), b"\x1b[Z");
    }

    #[test]
    fn backspace_is_del() {
        assert_eq!(
            encode(PhysicalKey::Backspace, ModifierSet::empty()),
            b"\x7f"
        );
    }

    #[test]
    fn escape_key_is_esc() {
        assert_eq!(encode(PhysicalKey::Escape, ModifierSet::empty()), b"\x1b");
    }

    #[test]
    fn all_ctrl_letters_are_correct() {
        for (ch, expected) in [
            ('a', 0x01),
            ('b', 0x02),
            ('c', 0x03),
            ('d', 0x04),
            ('e', 0x05),
            ('f', 0x06),
            ('g', 0x07),
            ('h', 0x08),
            ('i', 0x09),
            ('j', 0x0A),
            ('k', 0x0B),
            ('l', 0x0C),
            ('m', 0x0D),
            ('n', 0x0E),
            ('o', 0x0F),
            ('p', 0x10),
            ('q', 0x11),
            ('r', 0x12),
            ('s', 0x13),
            ('t', 0x14),
            ('u', 0x15),
            ('v', 0x16),
            ('w', 0x17),
            ('x', 0x18),
            ('y', 0x19),
            ('z', 0x1A),
        ] {
            let key = match ch {
                'a' => PhysicalKey::A,
                'b' => PhysicalKey::B,
                'c' => PhysicalKey::C,
                'd' => PhysicalKey::D,
                'e' => PhysicalKey::E,
                'f' => PhysicalKey::F,
                'g' => PhysicalKey::G,
                'h' => PhysicalKey::H,
                'i' => PhysicalKey::I,
                'j' => PhysicalKey::J,
                'k' => PhysicalKey::K,
                'l' => PhysicalKey::L,
                'm' => PhysicalKey::M,
                'n' => PhysicalKey::N,
                'o' => PhysicalKey::O,
                'p' => PhysicalKey::P,
                'q' => PhysicalKey::Q,
                'r' => PhysicalKey::R,
                's' => PhysicalKey::S,
                't' => PhysicalKey::T,
                'u' => PhysicalKey::U,
                'v' => PhysicalKey::V,
                'w' => PhysicalKey::W,
                'x' => PhysicalKey::X,
                'y' => PhysicalKey::Y,
                'z' => PhysicalKey::Z,
                _ => unreachable!(),
            };
            assert_eq!(encode(key, ModifierSet::CTRL), vec![expected], "Ctrl+{ch}");
        }
    }

    #[test]
    fn ctrl_punctuation() {
        assert_eq!(
            encode(PhysicalKey::LeftBracket, ModifierSet::CTRL),
            vec![0x1B]
        );
        assert_eq!(
            encode(PhysicalKey::Backslash, ModifierSet::CTRL),
            vec![0x1C]
        );
        assert_eq!(
            encode(PhysicalKey::RightBracket, ModifierSet::CTRL),
            vec![0x1D]
        );
        assert_eq!(encode(PhysicalKey::D6, ModifierSet::CTRL), vec![0x1E]);
        assert_eq!(encode(PhysicalKey::D7, ModifierSet::CTRL), vec![0x1F]);
        assert_eq!(encode(PhysicalKey::D8, ModifierSet::CTRL), vec![0x7F]);
        assert_eq!(encode(PhysicalKey::Space, ModifierSet::CTRL), vec![0x00]);
        assert_eq!(encode(PhysicalKey::D2, ModifierSet::CTRL), vec![0x00]);
        // Ctrl+digit otherwise produces nothing (never a plain digit).
        assert!(encode(PhysicalKey::D1, ModifierSet::CTRL).is_empty());
        assert!(encode(PhysicalKey::Minus, ModifierSet::CTRL).is_empty());
    }

    #[test]
    fn alt_prefixes_esc() {
        assert_eq!(encode(PhysicalKey::X, ModifierSet::ALT), b"\x1bx");
        assert_eq!(encode(PhysicalKey::Enter, ModifierSet::ALT), b"\x1b\r");
        assert_eq!(encode(PhysicalKey::Tab, ModifierSet::ALT), b"\x1b\t");
        assert_eq!(
            encode(PhysicalKey::C, ModifierSet::ALT.union(ModifierSet::CTRL)),
            b"\x1b\x03"
        );
    }

    #[test]
    fn arrows_normal_vs_application() {
        let mut modes = TerminalModes::default();
        let e = TerminalKeyEncoder::new(layout_us());
        assert_eq!(
            e.encode(PhysicalKey::Up, ModifierSet::empty(), &modes)
                .unwrap(),
            b"\x1b[A"
        );
        modes.application_cursor_keys = true;
        assert_eq!(
            e.encode(PhysicalKey::Up, ModifierSet::empty(), &modes)
                .unwrap(),
            b"\x1bOA"
        );
        // Modified arrows use the CSI 1;<mod> form regardless of mode.
        assert_eq!(
            e.encode(PhysicalKey::Up, ModifierSet::CTRL, &modes)
                .unwrap(),
            b"\x1b[1;5A"
        );
        assert_eq!(
            e.encode(
                PhysicalKey::Left,
                ModifierSet::SHIFT,
                &TerminalModes::default()
            )
            .unwrap(),
            b"\x1b[1;2D"
        );
    }

    #[test]
    fn home_end_are_mode_dependent() {
        let mut modes = TerminalModes::default();
        let e = TerminalKeyEncoder::new(layout_us());
        assert_eq!(
            e.encode(PhysicalKey::Home, ModifierSet::empty(), &modes)
                .unwrap(),
            b"\x1b[H"
        );
        modes.application_cursor_keys = true;
        assert_eq!(
            e.encode(PhysicalKey::Home, ModifierSet::empty(), &modes)
                .unwrap(),
            b"\x1bOH"
        );
        assert_eq!(
            e.encode(PhysicalKey::End, ModifierSet::CTRL, &modes)
                .unwrap(),
            b"\x1b[1;5F"
        );
    }

    #[test]
    fn tilda_keys() {
        let e = TerminalKeyEncoder::new(layout_us());
        let modes = TerminalModes::default();
        assert_eq!(
            e.encode(PhysicalKey::Insert, ModifierSet::empty(), &modes)
                .unwrap(),
            b"\x1b[2~"
        );
        assert_eq!(
            e.encode(PhysicalKey::Delete, ModifierSet::empty(), &modes)
                .unwrap(),
            b"\x1b[3~"
        );
        assert_eq!(
            e.encode(PhysicalKey::PageUp, ModifierSet::empty(), &modes)
                .unwrap(),
            b"\x1b[5~"
        );
        assert_eq!(
            e.encode(PhysicalKey::PageDown, ModifierSet::empty(), &modes)
                .unwrap(),
            b"\x1b[6~"
        );
        assert_eq!(
            e.encode(PhysicalKey::Delete, ModifierSet::CTRL, &modes)
                .unwrap(),
            b"\x1b[3;5~"
        );
    }

    #[test]
    fn function_keys() {
        let e = TerminalKeyEncoder::new(layout_us());
        let modes = TerminalModes::default();
        assert_eq!(
            e.encode(PhysicalKey::F1, ModifierSet::empty(), &modes)
                .unwrap(),
            b"\x1bOP"
        );
        assert_eq!(
            e.encode(PhysicalKey::F4, ModifierSet::empty(), &modes)
                .unwrap(),
            b"\x1bOS"
        );
        assert_eq!(
            e.encode(PhysicalKey::F5, ModifierSet::empty(), &modes)
                .unwrap(),
            b"\x1b[15~"
        );
        assert_eq!(
            e.encode(PhysicalKey::F12, ModifierSet::empty(), &modes)
                .unwrap(),
            b"\x1b[24~"
        );
        assert_eq!(
            e.encode(PhysicalKey::F1, ModifierSet::SHIFT, &modes)
                .unwrap(),
            b"\x1b[1;2P"
        );
        assert_eq!(
            e.encode(PhysicalKey::F5, ModifierSet::CTRL, &modes)
                .unwrap(),
            b"\x1b[15;5~"
        );
    }

    #[test]
    fn keypad_modes() {
        let e = TerminalKeyEncoder::new(layout_us());
        let mut modes = TerminalModes::default();
        // Numeric: plain digits.
        assert_eq!(
            e.encode(PhysicalKey::Kp1, ModifierSet::empty(), &modes)
                .unwrap(),
            b"1"
        );
        assert_eq!(
            e.encode(PhysicalKey::KpAdd, ModifierSet::empty(), &modes)
                .unwrap(),
            b"+"
        );
        // Application: SS3.
        modes.application_keypad = true;
        assert_eq!(
            e.encode(PhysicalKey::Kp1, ModifierSet::empty(), &modes)
                .unwrap(),
            b"\x1bOq"
        );
        assert_eq!(
            e.encode(PhysicalKey::KpAdd, ModifierSet::empty(), &modes)
                .unwrap(),
            b"\x1bOk"
        );
        assert_eq!(
            e.encode(PhysicalKey::KpEnter, ModifierSet::empty(), &modes)
                .unwrap(),
            b"\x1bOM"
        );
    }

    #[test]
    fn modifiers_lock_media_produce_nothing() {
        let e = TerminalKeyEncoder::new(layout_us());
        let modes = TerminalModes::default();
        for key in [
            PhysicalKey::LeftShift,
            PhysicalKey::RightCtrl,
            PhysicalKey::LeftAlt,
            PhysicalKey::CapsLock,
            PhysicalKey::NumLock,
            PhysicalKey::VolumeUp,
            PhysicalKey::Mute,
        ] {
            assert!(
                e.encode(key, ModifierSet::empty(), &modes).is_none(),
                "{key:?} must produce nothing"
            );
        }
    }

    #[test]
    fn base_ascii_round_trip() {
        // Spot-check the mapping table.
        assert_eq!(base_ascii(PhysicalKey::Grave), Some('`'));
        assert_eq!(base_ascii(PhysicalKey::Minus), Some('-'));
        assert_eq!(base_ascii(PhysicalKey::LeftBracket), Some('['));
        assert_eq!(base_ascii(PhysicalKey::Backslash), Some('\\'));
        assert_eq!(base_ascii(PhysicalKey::KpDecimal), Some('.'));
        assert_eq!(base_ascii(PhysicalKey::Space), Some(' '));
        assert_eq!(base_ascii(PhysicalKey::Up), None);
        assert_eq!(base_ascii(PhysicalKey::Home), None);
        assert_eq!(base_ascii(PhysicalKey::F5), None);
    }

    #[test]
    fn encoding_is_deterministic() {
        let e1 = TerminalKeyEncoder::new(layout_us());
        let e2 = TerminalKeyEncoder::new(layout_us());
        let modes = TerminalModes::default();
        let keys = [
            PhysicalKey::A,
            PhysicalKey::Up,
            PhysicalKey::F5,
            PhysicalKey::Kp2,
            PhysicalKey::Tab,
            PhysicalKey::Home,
        ];
        for key in keys {
            assert_eq!(
                e1.encode(key, ModifierSet::empty(), &modes),
                e2.encode(key, ModifierSet::empty(), &modes)
            );
        }
    }
}
