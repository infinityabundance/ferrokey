//! Keyboard layouts.
//!
//! Ferrokey keeps three concepts strictly separated:
//!
//! * [`crate::key::PhysicalKey`] — *which* key on the board (a Linux `KEY_*`
//!   code). `KEY_Q` is not the character `q`.
//! * `LogicalKey` — what a physical key *means* under the active modifier
//!   state (e.g. `KEY_Q` + shift ⇒ `'Q'`).
//! * [`KeySymbol`] — the visible text symbol / named function a key produces.
//!
//! Layouts are **data files** (`layouts/*.yaml`), not `.slint` code, so they
//! can be extended without touching the UI. (Eventually layout processing may
//! move to `xkbcommon` semantics; the `Layout` model below is designed so the
//! symbols it stores are exactly what an XKB keymap resolves to.)

use crate::key::PhysicalKey;
use crate::modifier::ModifierSet;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

/// A dead key (combining) accent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeadKey {
    Grave,
    Acute,
    Circumflex,
    Tilde,
    Diaeresis,
    Cedilla,
    Ring,
    Caron,
    Breve,
    DoubleAcute,
    Ogonek,
    Macron,
    Horn,
    HookAbove,
    Abovedot,
    Belowdot,
    Stroke,
}

impl DeadKey {
    pub const fn name(self) -> &'static str {
        use DeadKey::*;
        match self {
            Grave => "grave",
            Acute => "acute",
            Circumflex => "circumflex",
            Tilde => "tilde",
            Diaeresis => "diaeresis",
            Cedilla => "cedilla",
            Ring => "ring",
            Caron => "caron",
            Breve => "breve",
            DoubleAcute => "double-acute",
            Ogonek => "ogonek",
            Macron => "macron",
            Horn => "horn",
            HookAbove => "hook-above",
            Abovedot => "abovedot",
            Belowdot => "belowdot",
            Stroke => "stroke",
        }
    }
}

/// The symbol a key produces.
///
/// `Char(c)` is a literal character; `Name(s)` is a named, non-text function
/// (e.g. `"enter"`, `"shift"`, `"left"`); `Dead(d)` is a dead accent;
/// [`KeySymbol::Compose`] is the compose key; [`KeySymbol::None`] is a
/// placeholder key with no output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KeySymbol {
    /// No symbol (spacer / unused key).
    None,
    /// A literal character.
    Char(char),
    /// A named non-text function ("enter", "backspace", "shift", ...).
    Name(String),
    /// A dead (combining) accent.
    Dead(DeadKey),
    /// The compose key.
    Compose,
}

impl KeySymbol {
    /// Parse a layout-file symbol string.
    ///
    /// * single character → `Char`
    /// * `"none"` / empty → `None`
    /// * `"dead:<name>"` or `"dead_<name>"` → `Dead`
    /// * `"compose"` / `"multi_key"` → `Compose`
    /// * anything else → `Name`
    pub fn parse(s: &str) -> Self {
        match s {
            "" | "none" | "None" => KeySymbol::None,
            "compose" | "multi_key" => KeySymbol::Compose,
            _ => {
                if let Some(rest) = s.strip_prefix("dead:").or_else(|| s.strip_prefix("dead_")) {
                    let name = rest.to_ascii_lowercase();
                    for dead in [
                        DeadKey::Grave,
                        DeadKey::Acute,
                        DeadKey::Circumflex,
                        DeadKey::Tilde,
                        DeadKey::Diaeresis,
                        DeadKey::Cedilla,
                        DeadKey::Ring,
                        DeadKey::Caron,
                        DeadKey::Breve,
                        DeadKey::DoubleAcute,
                        DeadKey::Ogonek,
                        DeadKey::Macron,
                        DeadKey::Horn,
                        DeadKey::HookAbove,
                        DeadKey::Abovedot,
                        DeadKey::Belowdot,
                        DeadKey::Stroke,
                    ] {
                        if dead.name() == name {
                            return KeySymbol::Dead(dead);
                        }
                    }
                    // Fall through to Name for unknown dead keys.
                }
                let mut chars = s.chars();
                let first = chars.next();
                if let Some(c) = first {
                    if chars.next().is_none() {
                        return KeySymbol::Char(c);
                    }
                }
                KeySymbol::Name(s.to_string())
            }
        }
    }

    /// The visible label for this symbol.
    pub fn label(&self) -> String {
        match self {
            KeySymbol::None => String::new(),
            KeySymbol::Char(c) => c.to_string(),
            KeySymbol::Name(n) => n.clone(),
            KeySymbol::Dead(d) => format!("◌{}", d.name()),
            KeySymbol::Compose => "⏽".to_string(),
        }
    }
}

/// The full definition of one physical key in a layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyDefinition {
    /// The physical key this definition describes.
    pub physical: PhysicalKey,
    /// Base (unmodified) symbol.
    pub primary: KeySymbol,
    /// Shifted symbol.
    pub shifted: KeySymbol,
    /// AltGr symbol, if any.
    #[serde(default)]
    pub altgr: Option<KeySymbol>,
    /// Shift+AltGr symbol, if any.
    #[serde(default)]
    pub shift_altgr: Option<KeySymbol>,
    /// Fn-layer symbol, if any (media keys on a laptop-style Fn layer).
    #[serde(default)]
    pub fn_layer: Option<KeySymbol>,
    /// Whether holding this key should auto-repeat.
    #[serde(default = "default_repeatable")]
    pub repeatable: bool,
}

fn default_repeatable() -> bool {
    true
}

impl KeyDefinition {
    /// Resolve the symbol for this key given a modifier state.
    pub fn symbol_for(&self, mods: ModifierSet) -> &KeySymbol {
        if mods.contains(ModifierSet::FN) {
            if let Some(f) = &self.fn_layer {
                return f;
            }
        }
        let altgr = mods.contains(ModifierSet::ALTGR);
        let shift = mods.contains(ModifierSet::SHIFT);
        match (altgr, shift) {
            (true, true) => self
                .shift_altgr
                .as_ref()
                .unwrap_or(self.altgr.as_ref().unwrap_or(&self.shifted)),
            (true, false) => self.altgr.as_ref().unwrap_or(&self.primary),
            (false, true) => &self.shifted,
            (false, false) => &self.primary,
        }
    }
}

/// A full keyboard layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    /// Stable layout id, e.g. `"us"`, `"de"`, `"dvorak"`.
    pub id: String,
    /// Human-readable name, e.g. `"English (US)"`.
    pub name: String,
    /// Physical key → definition. Missing keys keep Ferrokey defaults.
    #[serde(default)]
    pub keys: HashMap<PhysicalKey, KeyDefinition>,
}

impl Layout {
    /// A mostly-empty layout used as fallback when no layout data is found.
    pub fn empty(id: impl Into<String>, name: impl Into<String>) -> Self {
        Layout {
            id: id.into(),
            name: name.into(),
            keys: HashMap::new(),
        }
    }

    pub fn get(&self, key: PhysicalKey) -> Option<&KeyDefinition> {
        self.keys.get(&key)
    }

    /// Resolve the symbol a physical key produces under the given modifier
    /// state (locked modifiers included by the caller).
    pub fn symbol_for(&self, key: PhysicalKey, mods: ModifierSet) -> Option<&KeySymbol> {
        self.keys.get(&key).map(|d| d.symbol_for(mods))
    }

    /// The symbol that should be *displayed* on the key cap, honoring the
    /// current modifier state (e.g. shifted symbols shown when shift is
    /// latched or locked).
    pub fn display_symbol(&self, key: PhysicalKey, mods: ModifierSet) -> Option<&KeySymbol> {
        self.symbol_for(key, mods)
    }

    /// Whether holding the key should auto-repeat (from the layout, falling
    /// back to a sensible default for named keys).
    pub fn is_repeatable(&self, key: PhysicalKey) -> bool {
        match self.keys.get(&key) {
            Some(d) => d.repeatable,
            None => !key.is_modifier() && !key.is_lock_key(),
        }
    }

    /// Find a physical key sequence that produces the given character under
    /// the given modifier state. Used by the text-input path (compose /
    /// text mode) to translate text into key events.
    ///
    /// Returns `(key, required_modifiers, consumed_latch)` style info:
    /// the physical key and the modifiers that must be additionally engaged.
    pub fn find_char(
        &self,
        c: char,
        active_mods: ModifierSet,
    ) -> Option<(PhysicalKey, ModifierSet)> {
        for (key, def) in &self.keys {
            if let KeySymbol::Char(sym) = def.symbol_for(active_mods) {
                if *sym == c {
                    return Some((*key, ModifierSet::empty()));
                }
            }
        }
        // Try each modifier combination in preference order: base, shift,
        // altgr, shift+altgr.
        for (extra, combo) in [
            (ModifierSet::empty(), ModifierSet::empty()),
            (ModifierSet::SHIFT, ModifierSet::SHIFT),
            (ModifierSet::ALTGR, ModifierSet::ALTGR),
            (
                ModifierSet::SHIFT.union(ModifierSet::ALTGR),
                ModifierSet::SHIFT.union(ModifierSet::ALTGR),
            ),
        ] {
            let effective = active_mods.union(combo);
            for (key, def) in &self.keys {
                if let KeySymbol::Char(sym) = def.symbol_for(effective) {
                    if *sym == c && !active_mods.contains(extra.intersection(combo)) {
                        // Only return combos whose *difference* from the
                        // active state is expressible.
                        return Some((*key, combo));
                    }
                }
            }
        }
        None
    }

    /// Ordered list of the layout's physical keys (deterministic ordering).
    pub fn ordered_keys(&self) -> Vec<PhysicalKey> {
        let mut keys: Vec<PhysicalKey> = self.keys.keys().copied().collect();
        keys.sort();
        keys
    }
}

/// A layout that adds nothing to the built-in defaults.
#[derive(Debug, Clone)]
pub struct LayoutIndex {
    layouts: BTreeMap<String, Layout>,
    default_id: String,
}

impl LayoutIndex {
    pub fn new(default_id: impl Into<String>) -> Self {
        LayoutIndex {
            layouts: BTreeMap::new(),
            default_id: default_id.into(),
        }
    }

    pub fn add(&mut self, layout: Layout) {
        self.layouts.insert(layout.id.clone(), layout);
    }

    pub fn get(&self, id: &str) -> Option<&Layout> {
        self.layouts.get(id)
    }

    pub fn default(&self) -> &Layout {
        self.layouts
            .get(&self.default_id)
            .or_else(|| self.layouts.values().next())
            .unwrap_or_else(|| {
                // Fallback: an empty layout — callers treat it as "no layout".
                // The index is constructed with a default; this branch only
                // triggers when the index is empty, which callers avoid.
                &EMPTY_FALLBACK
            })
    }

    pub fn default_id(&self) -> &str {
        &self.default_id
    }

    pub fn ids(&self) -> impl Iterator<Item = &String> {
        self.layouts.keys()
    }

    pub fn is_empty(&self) -> bool {
        self.layouts.is_empty()
    }
}

static EMPTY_FALLBACK: LazyLock<Layout> = LazyLock::new(|| Layout::empty("", ""));

#[cfg(test)]
mod tests {
    use super::*;

    fn us_layout() -> Layout {
        let mut keys = HashMap::new();
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
        keys.insert(
            PhysicalKey::D1,
            KeyDefinition {
                physical: PhysicalKey::D1,
                primary: KeySymbol::Char('1'),
                shifted: KeySymbol::Char('!'),
                altgr: None,
                shift_altgr: None,
                fn_layer: None,
                repeatable: true,
            },
        );
        keys.insert(
            PhysicalKey::D2,
            KeyDefinition {
                physical: PhysicalKey::D2,
                primary: KeySymbol::Char('2'),
                shifted: KeySymbol::Char('@'),
                altgr: Some(KeySymbol::Char('²')),
                shift_altgr: None,
                fn_layer: None,
                repeatable: true,
            },
        );
        keys.insert(
            PhysicalKey::Space,
            KeyDefinition {
                physical: PhysicalKey::Space,
                primary: KeySymbol::Char(' '),
                shifted: KeySymbol::Char(' '),
                altgr: None,
                shift_altgr: None,
                fn_layer: None,
                repeatable: true,
            },
        );
        keys.insert(
            PhysicalKey::Enter,
            KeyDefinition {
                physical: PhysicalKey::Enter,
                primary: KeySymbol::Name("enter".into()),
                shifted: KeySymbol::Name("enter".into()),
                altgr: None,
                shift_altgr: None,
                fn_layer: None,
                repeatable: false,
            },
        );
        Layout {
            id: "us-test".into(),
            name: "US Test".into(),
            keys,
        }
    }

    #[test]
    fn symbol_resolution_base_shift_altgr() {
        let layout = us_layout();
        assert_eq!(
            layout.symbol_for(PhysicalKey::A, ModifierSet::empty()),
            Some(&KeySymbol::Char('a'))
        );
        assert_eq!(
            layout.symbol_for(PhysicalKey::A, ModifierSet::SHIFT),
            Some(&KeySymbol::Char('A'))
        );
        assert_eq!(
            layout.symbol_for(PhysicalKey::D2, ModifierSet::ALTGR),
            Some(&KeySymbol::Char('²'))
        );
        // AltGr fallback when undefined: uses primary.
        assert_eq!(
            layout.symbol_for(PhysicalKey::A, ModifierSet::ALTGR),
            Some(&KeySymbol::Char('a'))
        );
    }

    #[test]
    fn find_char_prefers_base_then_shift() {
        let layout = us_layout();
        assert_eq!(
            layout.find_char('a', ModifierSet::empty()),
            Some((PhysicalKey::A, ModifierSet::empty()))
        );
        assert_eq!(
            layout.find_char('A', ModifierSet::empty()),
            Some((PhysicalKey::A, ModifierSet::SHIFT))
        );
        assert_eq!(
            layout.find_char('²', ModifierSet::empty()),
            Some((PhysicalKey::D2, ModifierSet::ALTGR))
        );
        assert_eq!(layout.find_char('x', ModifierSet::empty()), None);
    }

    #[test]
    fn symbol_parse() {
        assert_eq!(KeySymbol::parse("a"), KeySymbol::Char('a'));
        assert_eq!(KeySymbol::parse(""), KeySymbol::None);
        assert_eq!(KeySymbol::parse("none"), KeySymbol::None);
        assert_eq!(KeySymbol::parse("enter"), KeySymbol::Name("enter".into()));
        assert_eq!(KeySymbol::parse("compose"), KeySymbol::Compose);
        assert_eq!(
            KeySymbol::parse("dead:acute"),
            KeySymbol::Dead(DeadKey::Acute)
        );
    }

    #[test]
    fn repeatability_defaults() {
        let layout = us_layout();
        assert!(layout.is_repeatable(PhysicalKey::A));
        assert!(!layout.is_repeatable(PhysicalKey::Enter)); // layout says no
        assert!(!layout.is_repeatable(PhysicalKey::LeftShift)); // modifier default
    }
}
