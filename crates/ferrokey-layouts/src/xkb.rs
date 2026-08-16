//! The xkbcommon bridge.
//!
//! Ferrokey's layout model is designed so the symbols it stores are exactly
//! what an XKB keymap resolves to: `primary` is XKB level 1, `shifted` is
//! level 2, `altgr` is level 3 and `shift_altgr` is level 4.
//!
//! This module exposes that mapping explicitly (so the rest of Ferrokey talks
//! "levels", matching xkbcommon terminology), and defines the seam where a
//! real `libxkbcommon` link plugs in for live keymap loading.
//!
//! With the `xkb` feature enabled, [`xkbcommon::XkbKeymap::from_names`] loads
//! the desktop's active keymap (or any `layout(variant)` / `layout@variant`
//! spec such as `de@neo` or `us(intl)`) and converts it into a Ferrokey
//! [`Layout`]; without it, Ferrokey uses its own built-in YAML layouts (the
//! default and the behaviour the compatibility courts validate).

#[cfg(feature = "xkb")]
use ferrokey_core::layout::DeadKey;
use ferrokey_core::layout::{KeyDefinition, KeySymbol, Layout};
use ferrokey_core::modifier::ModifierSet;

/// The four XKB levels of a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Levels {
    pub level1: KeySymbol,
    pub level2: KeySymbol,
    pub level3: Option<KeySymbol>,
    pub level4: Option<KeySymbol>,
}

impl From<&KeyDefinition> for Levels {
    fn from(def: &KeyDefinition) -> Self {
        Levels {
            level1: def.primary.clone(),
            level2: def.shifted.clone(),
            level3: def.altgr.clone(),
            level4: def.shift_altgr.clone(),
        }
    }
}

impl Levels {
    /// Resolve the active level for a modifier state, mirroring XKB's
    /// "level" computation: shift → level 2, altgr → level 3, both → level 4.
    pub fn resolve(&self, mods: ModifierSet) -> &KeySymbol {
        let altgr = mods.contains(ModifierSet::ALTGR);
        let shift = mods.contains(ModifierSet::SHIFT);
        match (altgr, shift) {
            (true, true) => self
                .level4
                .as_ref()
                .or(self.level3.as_ref())
                .unwrap_or(&self.level2),
            (true, false) => self.level3.as_ref().unwrap_or(&self.level1),
            (false, true) => &self.level2,
            (false, false) => &self.level1,
        }
    }
}

/// Expose a layout as its per-key XKB levels.
pub fn levels_for(layout: &Layout, key: ferrokey_core::PhysicalKey) -> Option<Levels> {
    layout.get(key).map(Levels::from)
}

/// Validate that a layout satisfies Ferrokey's minimum contract: it must be
/// able to produce the full alphabet (upper and lower case) and all digits.
/// Returns an error message naming the first missing capability.
pub fn validate(layout: &Layout) -> Result<(), &'static str> {
    // Digits are required on the primary level of some key.
    for digit in '0'..='9' {
        if !layout
            .keys
            .values()
            .any(|d| d.primary == KeySymbol::Char(digit))
        {
            return Err("layout cannot produce all digits");
        }
    }
    // Every lowercase letter must be producible either directly or via shift.
    for letter in "abcdefghijklmnopqrstuvwxyz".chars() {
        let ok = layout.keys.values().any(|d| {
            d.primary == KeySymbol::Char(letter)
                || d.shifted == KeySymbol::Char(letter)
                || d.altgr.as_ref() == Some(&KeySymbol::Char(letter))
        });
        if !ok {
            return Err("layout cannot produce a lowercase letter");
        }
        let upper = letter.to_ascii_uppercase();
        let ok = layout.keys.values().any(|d| {
            d.primary == KeySymbol::Char(upper)
                || d.shifted == KeySymbol::Char(upper)
                || d.altgr.as_ref() == Some(&KeySymbol::Char(upper))
        });
        if !ok {
            return Err("layout cannot produce an uppercase letter");
        }
    }
    Ok(())
}

/// Translate a char to the modifier combination that produces it, in XKB
/// level order. Returns the physical key and required modifiers.
pub fn find_key(
    layout: &Layout,
    c: char,
    active: ModifierSet,
) -> Option<(ferrokey_core::PhysicalKey, ModifierSet)> {
    layout.find_char(c, active)
}

// ────────────────────────────────────────────────────────────────────────────
// xkbcommon
// ────────────────────────────────────────────────────────────────────────────

/// Parse an XKB layout spec into `(layout, variant)`.
///
/// * `"us(intl)"` → `("us", "intl")`
/// * `"de@neo"` → `("de", "neo")`
/// * `"us"` → `("us", "")`
/// * empty / malformed → `None`
pub fn parse_xkb_spec(spec: &str) -> Option<(String, String)> {
    if let Some(open) = spec.find('(') {
        let close = spec.rfind(')')?;
        if close < open {
            return None;
        }
        let layout = &spec[..open];
        let variant = &spec[open + 1..close];
        if layout.is_empty() || variant.contains('(') || variant.contains(')') {
            return None;
        }
        return Some((layout.to_string(), variant.to_string()));
    }
    if let Some(at) = spec.find('@') {
        let (layout, variant) = (&spec[..at], &spec[at + 1..]);
        if layout.is_empty() {
            return None;
        }
        return Some((layout.to_string(), variant.to_string()));
    }
    if spec.is_empty() {
        None
    } else {
        Some((spec.to_string(), String::new()))
    }
}

/// Load a system layout through real xkbcommon (feature `xkb`).
///
/// The spec may be a plain layout (`"us"`), an XKB variant
/// (`"us(intl)"`, `"de@neo"`), or anything `parse_xkb_spec` accepts.
/// Returns `None` when the `xkb` feature is disabled or the keymap could
/// not be loaded (missing rules data, unknown layout, …) — callers fall
/// back to the built-in YAML layouts.
pub fn load_system_layout(spec: &str) -> Option<Layout> {
    #[cfg(feature = "xkb")]
    {
        let (layout, variant) = parse_xkb_spec(spec)?;
        let keymap = xkbcommon::XkbKeymap::from_names("evdev", &layout, &variant, "")?;
        let l = keymap.to_layout();
        // System keymaps are validated too: a loaded-but-broken keymap must
        // not silently produce garbage.
        validate(&l).ok()?;
        Some(l)
    }
    #[cfg(not(feature = "xkb"))]
    {
        let _ = spec;
        None
    }
}

/// The xkbcommon state object: wraps a live `libxkbcommon` keymap.
///
/// With the `xkb` feature enabled this is the real thing:
/// `XkbKeymap::from_names` calls `xkb_keymap_new_from_names` and reads the
/// system rules/layout/variant data. Without the feature it degrades to a
/// documented stub returning `None` (built-in layouts are used instead).
#[cfg(feature = "xkb")]
pub mod xkbcommon {
    use super::*;
    use ::xkbcommon::xkb::keysyms;
    use ::xkbcommon::xkb::{Context, Keymap, Keysym, KEYMAP_COMPILE_NO_FLAGS};
    use ferrokey_core::PhysicalKey;
    use std::collections::HashMap;

    /// A live xkbcommon keymap plus the spec it was created from.
    pub struct XkbKeymap {
        // The context is a deliberate lifetime anchor: a future `State` (for
        // live modifier/level resolution) needs the same context that created
        // the keymap. Kept even though `to_layout` alone does not read it.
        #[allow(dead_code)]
        context: Context,
        keymap: Keymap,
        layout: String,
        variant: String,
    }

    impl XkbKeymap {
        /// Build a keymap from xkb names (rules / layout / variant /
        /// options), exactly like `xkb_keymap_new_from_names`. `rules` is
        /// normally `"evdev"`; layout/variant follow XKB notation
        /// (`"us"`, `"intl"`, `"neo"`, …).
        pub fn from_names(
            rules: &str,
            layout: &str,
            variant: &str,
            options: &str,
        ) -> Option<XkbKeymap> {
            let context = Context::new(::xkbcommon::xkb::CONTEXT_NO_FLAGS);
            let keymap = Keymap::new_from_names(
                &context,
                rules,
                "pc105",
                layout,
                variant,
                Some(options.to_string()),
                KEYMAP_COMPILE_NO_FLAGS,
            )?;
            Some(XkbKeymap {
                context,
                keymap,
                layout: layout.to_string(),
                variant: variant.to_string(),
            })
        }

        /// The resolved layout name (id), e.g. `"us(intl)"`.
        pub fn layout_id(&self) -> String {
            if self.variant.is_empty() {
                self.layout.clone()
            } else {
                format!("{}({})", self.layout, self.variant)
            }
        }

        /// Convert the whole keymap into a Ferrokey [`Layout`].
        ///
        /// For every physical key in Ferrokey's explicit capability set, the
        /// xkb keycode is `linux_code + 8` (xkbcommon's keycode base), and the
        /// keysyms of levels 1–4 are read from layout group 0:
        ///
        /// ```text
        /// level 1 → primary, level 2 → shifted,
        /// level 3 → altgr,   level 4 → shift_altgr
        /// ```
        ///
        /// Levels the key does not define stay `None` —
        /// [`KeyDefinition::symbol_for`] and [`Levels::resolve`] already fall
        /// back the same way XKB does (a single-level key produces the same
        /// symbol with Shift).
        pub fn to_layout(&self) -> Layout {
            let mut keys = HashMap::new();
            for &physical in ferrokey_core::CAPABILITY_SET {
                let keycode = physical.linux_code() + 8;
                let Some(primary) = self.sym_at(keycode, 0) else {
                    continue;
                };
                let shifted = self.sym_at(keycode, 1).unwrap_or_else(|| primary.clone());
                let altgr = self.sym_at(keycode, 2);
                let shift_altgr = self.sym_at(keycode, 3);
                keys.insert(
                    physical,
                    KeyDefinition {
                        physical,
                        primary,
                        shifted,
                        altgr,
                        shift_altgr,
                        fn_layer: None,
                        repeatable: default_repeatable(physical),
                    },
                );
            }
            Layout {
                id: self.layout_id(),
                name: format!(
                    "XKB {} {}",
                    self.layout,
                    if self.variant.is_empty() {
                        ""
                    } else {
                        &self.variant
                    }
                )
                .trim()
                .to_string(),
                keys,
            }
        }

        /// The first keysym of `level` (0-based) for `keycode`, or `None`
        /// when the key has no such level.
        fn sym_at(&self, keycode: u32, level: u32) -> Option<KeySymbol> {
            let syms = self.keymap.key_get_syms_by_level(
                ::xkbcommon::xkb::Keycode::new(keycode),
                0,
                level,
            );
            syms.first().map(|s| keysym_to_symbol(*s))
        }
    }

    /// Repeatability for converted keys: modifiers and lock keys never repeat;
    /// everything else follows the layout default (repeatable).
    fn default_repeatable(key: PhysicalKey) -> bool {
        !key.is_modifier() && !key.is_lock_key()
    }

    /// Translate an xkbcommon keysym into a Ferrokey [`KeySymbol`].
    pub fn keysym_to_symbol(keysym: Keysym) -> KeySymbol {
        // 1. Unicode / Latin-1 keysyms → literal characters.
        if let Some(c) = keysym.key_char() {
            return KeySymbol::Char(c);
        }
        // 2. Dead accents.
        for (sym, dead) in DEAD_KEY_KEYSYMS.iter().copied() {
            if keysym == sym {
                return KeySymbol::Dead(dead);
            }
        }
        // 3. The compose (multi-key) key.
        if keysym == Keysym::new(keysyms::KEY_Multi_key) {
            return KeySymbol::Compose;
        }
        // 4. Named non-text keys (mapped to Ferrokey labels). `name()`
        // returns X11-style names with an "XK_" prefix ("XK_Return").
        if let Some(name) = keysym.name() {
            let name = name.strip_prefix("XK_").unwrap_or(name);
            if let Some(label) = NAMED_KEYS
                .iter()
                .find_map(|(k, v)| (*k == name).then_some(*v))
            {
                return KeySymbol::Name(label.to_string());
            }
            // Function keys F1–F24.
            if let Some(f) = function_key_number(name) {
                return KeySymbol::Name(format!("f{f}"));
            }
            // Generic fallback: "KP_Enter" → "kp-enter", "Shift_L" → "shift-l".
            return KeySymbol::Name(name.to_ascii_lowercase().replace('_', "-"));
        }
        KeySymbol::None
    }

    /// `XK_F1` (0xffbe) .. `XK_F35` (0xffe0) → 1..=35.
    fn function_key_number(name: &str) -> Option<u8> {
        let n = name.strip_prefix('F')?;
        let n: u8 = n.parse().ok()?;
        (1..=35).contains(&n).then_some(n)
    }

    /// xkb keysyms for the dead accents Ferrokey knows.
    const DEAD_KEY_KEYSYMS: &[(Keysym, DeadKey)] = &[
        (Keysym::new(keysyms::KEY_dead_grave), DeadKey::Grave),
        (Keysym::new(keysyms::KEY_dead_acute), DeadKey::Acute),
        (
            Keysym::new(keysyms::KEY_dead_circumflex),
            DeadKey::Circumflex,
        ),
        (Keysym::new(keysyms::KEY_dead_tilde), DeadKey::Tilde),
        (Keysym::new(keysyms::KEY_dead_diaeresis), DeadKey::Diaeresis),
        (Keysym::new(keysyms::KEY_dead_cedilla), DeadKey::Cedilla),
        (Keysym::new(keysyms::KEY_dead_abovering), DeadKey::Ring),
        (Keysym::new(keysyms::KEY_dead_caron), DeadKey::Caron),
        (Keysym::new(keysyms::KEY_dead_breve), DeadKey::Breve),
        (
            Keysym::new(keysyms::KEY_dead_doubleacute),
            DeadKey::DoubleAcute,
        ),
        (Keysym::new(keysyms::KEY_dead_ogonek), DeadKey::Ogonek),
        (Keysym::new(keysyms::KEY_dead_macron), DeadKey::Macron),
        (Keysym::new(keysyms::KEY_dead_horn), DeadKey::Horn),
        (Keysym::new(keysyms::KEY_dead_hook), DeadKey::HookAbove),
        (Keysym::new(keysyms::KEY_dead_abovedot), DeadKey::Abovedot),
        (Keysym::new(keysyms::KEY_dead_belowdot), DeadKey::Belowdot),
        (Keysym::new(keysyms::KEY_dead_stroke), DeadKey::Stroke),
    ];

    /// Well-known named keysyms → Ferrokey display labels (names without the
    /// `XK_` prefix).
    const NAMED_KEYS: &[(&str, &str)] = &[
        ("Return", "enter"),
        ("BackSpace", "backspace"),
        ("Tab", "tab"),
        ("Escape", "esc"),
        ("Delete", "del"),
        ("Insert", "ins"),
        ("Home", "home"),
        ("End", "end"),
        ("Page_Up", "pgup"),
        ("Page_Down", "pgdn"),
        ("Prior", "pgup"),
        ("Next", "pgdn"),
        ("Left", "left"),
        ("Right", "right"),
        ("Up", "up"),
        ("Down", "down"),
        ("Shift_L", "shift"),
        ("Shift_R", "shift"),
        ("Control_L", "ctrl"),
        ("Control_R", "ctrl"),
        ("Alt_L", "alt"),
        ("Alt_R", "altgr"),
        ("ISO_Level3_Shift", "altgr"),
        ("ISO_Level5_Shift", "fn"),
        ("Super_L", "super"),
        ("Super_R", "super"),
        ("Meta_L", "super"),
        ("Meta_R", "super"),
        ("Caps_Lock", "caps"),
        ("Num_Lock", "num"),
        ("Scroll_Lock", "scroll"),
        ("Menu", "menu"),
        ("Multi_key", "compose"),
        ("Pause", "pause"),
        ("Print", "print"),
        ("Sys_Req", "sysrq"),
        ("space", "space"),
        ("KP_Enter", "enter"),
        ("KP_Add", "kp-add"),
        ("KP_Subtract", "kp-subtract"),
        ("KP_Multiply", "kp-multiply"),
        ("KP_Divide", "kp-divide"),
        ("KP_Decimal", "kp-decimal"),
        ("KP_Separator", "kp-comma"),
        ("KP_Equal", "kp-equal"),
        ("AudioMute", "mute"),
        ("AudioLowerVolume", "vol-"),
        ("AudioRaiseVolume", "vol+"),
        ("AudioPlay", "play"),
        ("AudioStop", "stop"),
        ("AudioNext", "next"),
        ("AudioPrev", "prev"),
        ("XF86AudioMute", "mute"),
        ("XF86AudioLowerVolume", "vol-"),
        ("XF86AudioRaiseVolume", "vol+"),
        ("XF86AudioPlay", "play"),
        ("XF86AudioStop", "stop"),
        ("XF86AudioNext", "next"),
        ("XF86AudioPrev", "prev"),
        ("XF86MonBrightnessDown", "dim"),
        ("XF86MonBrightnessUp", "bright"),
        ("XF86PowerOff", "power"),
        ("XF86Sleep", "sleep"),
        ("XF86WakeUp", "wakeup"),
        ("XF86Back", "back"),
        ("XF86Forward", "forward"),
        ("XF86Refresh", "refresh"),
        ("XF86HomePage", "homepage"),
        ("XF86Mail", "mail"),
        ("XF86Calculator", "calculator"),
        ("XF86Book", "bookmarks"),
        ("XF86Explorer", "computer"),
        ("XF86Search", "search"),
        ("Help", "help"),
    ];
}

/// A no-op placeholder for the real xkbcommon state object, kept so the
/// integration seam is explicit when the `xkb` feature is disabled.
#[cfg(not(feature = "xkb"))]
pub mod xkbcommon {
    /// Marker type documenting the intended xkbcommon integration point.
    pub struct XkbKeymap;

    impl XkbKeymap {
        /// Returns `None` when the `xkb` feature is disabled (built-in
        /// layouts are used instead).
        pub fn from_names(
            _rules: &str,
            _layout: &str,
            _variant: &str,
            _options: &str,
        ) -> Option<XkbKeymap> {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::builtin;

    #[test]
    fn us_levels() {
        let us = builtin("us").unwrap();
        let levels = levels_for(&us, ferrokey_core::PhysicalKey::D2).unwrap();
        assert_eq!(levels.level1, KeySymbol::Char('2'));
        assert_eq!(levels.level2, KeySymbol::Char('@'));
        assert_eq!(levels.resolve(ModifierSet::SHIFT), &KeySymbol::Char('@'));
        assert_eq!(levels.resolve(ModifierSet::empty()), &KeySymbol::Char('2'));
    }

    #[test]
    fn all_builtins_validate() {
        for id in crate::builtin::BUILTIN_IDS {
            let layout = builtin(id).unwrap();
            assert_eq!(validate(&layout), Ok(()), "layout {id} failed validation");
        }
    }

    #[test]
    fn xkb_spec_parsing() {
        assert_eq!(
            parse_xkb_spec("us(intl)"),
            Some(("us".into(), "intl".into()))
        );
        assert_eq!(parse_xkb_spec("de@neo"), Some(("de".into(), "neo".into())));
        assert_eq!(parse_xkb_spec("us"), Some(("us".into(), String::new())));
        assert_eq!(parse_xkb_spec(""), None);
        assert_eq!(parse_xkb_spec("()"), None);
        assert_eq!(parse_xkb_spec("(intl)"), None);
        assert_eq!(parse_xkb_spec("us("), None);
        assert_eq!(parse_xkb_spec("us)"), Some(("us)".into(), String::new())));
    }

    #[cfg(feature = "xkb")]
    mod xkb_live {
        use super::*;

        /// A real keymap load requires xkb rules data (installed with
        /// libxkbcommon / xkb-data). The test is skipped on hosts without it.
        fn try_us_intl() -> Option<xkbcommon::XkbKeymap> {
            xkbcommon::XkbKeymap::from_names("evdev", "us", "intl", "")
        }

        #[test]
        fn from_names_loads_us_intl() {
            let Some(keymap) = try_us_intl() else {
                eprintln!("skipping: no xkb rules data on this host");
                return;
            };
            assert_eq!(keymap.layout_id(), "us(intl)");
        }

        #[test]
        fn conversion_preserves_chars_and_dead_keys() {
            let Some(keymap) = try_us_intl() else {
                eprintln!("skipping: no xkb rules data on this host");
                return;
            };
            let layout = keymap.to_layout();
            assert!(validate(&layout).is_ok(), "converted layout must validate");
            // A → 'a', shift → 'A'.
            assert_eq!(
                layout.symbol_for(ferrokey_core::PhysicalKey::A, ModifierSet::empty()),
                Some(&KeySymbol::Char('a'))
            );
            assert_eq!(
                layout.symbol_for(ferrokey_core::PhysicalKey::A, ModifierSet::SHIFT),
                Some(&KeySymbol::Char('A'))
            );
            // us(intl): the apostrophe key is a dead acute.
            assert_eq!(
                layout.symbol_for(ferrokey_core::PhysicalKey::Apostrophe, ModifierSet::empty()),
                Some(&KeySymbol::Dead(DeadKey::Acute))
            );
            // us(intl): AltGr+e is é.
            assert_eq!(
                layout.symbol_for(ferrokey_core::PhysicalKey::E, ModifierSet::ALTGR),
                Some(&KeySymbol::Char('é'))
            );
        }

        #[test]
        fn de_neo_variant_loads() {
            let Some(keymap) = xkbcommon::XkbKeymap::from_names("evdev", "de", "neo", "") else {
                eprintln!("skipping: de(neo) unavailable on this host");
                return;
            };
            let layout = keymap.to_layout();
            assert!(validate(&layout).is_ok(), "de(neo) must validate");
        }

        #[test]
        fn unknown_layout_fails_cleanly() {
            let keymap =
                xkbcommon::XkbKeymap::from_names("evdev", "definitely-not-a-layout", "", "");
            assert!(keymap.is_none());
        }

        #[test]
        fn load_system_layout_round_trip() {
            let Some(layout) = load_system_layout("us(intl)") else {
                eprintln!("skipping: no xkb rules data on this host");
                return;
            };
            assert_eq!(layout.id, "us(intl)");
            assert!(validate(&layout).is_ok());
        }
    }
}
