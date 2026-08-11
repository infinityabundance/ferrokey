//! The xkbcommon bridge.
//!
//! Ferrokey's layout model is designed so the symbols it stores are exactly
//! what an XKB keymap resolves to: `primary` is XKB level 1, `shifted` is
//! level 2, `altgr` is level 3 and `shift_altgr` is level 4.
//!
//! This module exposes that mapping explicitly (so the rest of Ferrokey talks
//! "levels", matching xkbcommon terminology), and defines the seam where a
//! real `libxkbcommon` link would plug in for live keymap loading. The
//! built-in layouts never require the C library; enabling the `xkb` feature
//! (which needs `libxkbcommon-dev`) would let Ferrokey consume the desktop's
//! active keymap instead of its own data files.

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

/// A no-op placeholder for the real xkbcommon state object, kept so the
/// integration seam is explicit. The feature-gated real implementation would
/// wrap `xkb_context`/`xkb_keymap` and translate `xkb_keycode_t` symbols
/// into Ferrokey `KeyDefinition`s.
///
/// With the `xkb` feature enabled, `XkbKeymap::from_names` would call
/// `xkb_keymap_new_from_names` and read the desktop's active keymap; without
/// it, Ferrokey uses its own built-in YAML layouts (the default and the
/// behaviour the compatibility courts validate).
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

/// Translate a char to the modifier combination that produces it, in XKB
/// level order. Returns the physical key and required modifiers.
pub fn find_key(
    layout: &Layout,
    c: char,
    active: ModifierSet,
) -> Option<(ferrokey_core::PhysicalKey, ModifierSet)> {
    layout.find_char(c, active)
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
}
