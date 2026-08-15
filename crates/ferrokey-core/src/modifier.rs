//! Modifier tracking: shift / ctrl / alt / altgr / meta / fn, plus the
//! latched (sticky) and locked state that drives Ferrokey's modifier
//! state machine.

use crate::key::PhysicalKey;
use serde::{Deserialize, Serialize};

/// The distinct modifier *kinds* Ferrokey reasons about.
///
/// A kind may be backed by either of the two physical keys (e.g. both
/// `LeftShift` and `RightShift` map to `Shift`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModifierKind {
    Shift,
    Ctrl,
    Alt,
    AltGr,
    Meta,
    Fn,
}

impl ModifierKind {
    /// The number of modifier kinds (the size of per-kind tables).
    pub const COUNT: usize = 6;

    /// The preferred physical key used to emit this modifier.
    pub const fn preferred_key(self) -> PhysicalKey {
        match self {
            ModifierKind::Shift => PhysicalKey::LeftShift,
            ModifierKind::Ctrl => PhysicalKey::LeftCtrl,
            ModifierKind::Alt => PhysicalKey::LeftAlt,
            ModifierKind::AltGr => PhysicalKey::RightAlt,
            ModifierKind::Meta => PhysicalKey::LeftMeta,
            ModifierKind::Fn => PhysicalKey::Menu,
        }
    }

    /// The 0-based index of this modifier — for fixed-size per-kind tables
    /// (the state machine's `last_tap`), in declaration order.
    pub const fn index(self) -> usize {
        match self {
            ModifierKind::Shift => 0,
            ModifierKind::Ctrl => 1,
            ModifierKind::Alt => 2,
            ModifierKind::AltGr => 3,
            ModifierKind::Meta => 4,
            ModifierKind::Fn => 5,
        }
    }

    /// Bit index used by [`ModifierSet`].
    const fn bit(self) -> u8 {
        match self {
            ModifierKind::Shift => 1 << 0,
            ModifierKind::Ctrl => 1 << 1,
            ModifierKind::Alt => 1 << 2,
            ModifierKind::AltGr => 1 << 3,
            ModifierKind::Meta => 1 << 4,
            ModifierKind::Fn => 1 << 5,
        }
    }
}

/// A bitset of active modifier kinds.
///
/// Used for the latched and locked modifier sets in the keyboard state
/// machine. Kept dependency-free (no `bitflags` crate) so the core stays
/// trivially portable and auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ModifierSet(u8);

impl ModifierSet {
    pub const SHIFT: ModifierSet = ModifierSet(1 << 0);
    pub const CTRL: ModifierSet = ModifierSet(1 << 1);
    pub const ALT: ModifierSet = ModifierSet(1 << 2);
    pub const ALTGR: ModifierSet = ModifierSet(1 << 3);
    pub const META: ModifierSet = ModifierSet(1 << 4);
    pub const FN: ModifierSet = ModifierSet(1 << 5);

    pub const ALL: ModifierSet = ModifierSet(
        Self::SHIFT.0 | Self::CTRL.0 | Self::ALT.0 | Self::ALTGR.0 | Self::META.0 | Self::FN.0,
    );

    pub const fn empty() -> Self {
        ModifierSet(0)
    }

    pub const fn contains(self, other: ModifierSet) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn insert(&mut self, other: ModifierSet) {
        self.0 |= other.0;
    }

    pub const fn remove(&mut self, other: ModifierSet) {
        self.0 &= !other.0;
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn union(self, other: ModifierSet) -> ModifierSet {
        ModifierSet(self.0 | other.0)
    }

    pub const fn intersection(self, other: ModifierSet) -> ModifierSet {
        ModifierSet(self.0 & other.0)
    }

    pub const fn as_u8(self) -> u8 {
        self.0
    }

    pub const fn from_u8(bits: u8) -> Self {
        ModifierSet(bits & Self::ALL.0)
    }

    /// Iterate the contained kinds, lowest bit first.
    pub fn iter(self) -> impl Iterator<Item = ModifierKind> {
        let kinds = [
            ModifierKind::Shift,
            ModifierKind::Ctrl,
            ModifierKind::Alt,
            ModifierKind::AltGr,
            ModifierKind::Meta,
            ModifierKind::Fn,
        ];
        kinds
            .into_iter()
            .filter(move |k| self.contains(ModifierSet(k.bit())))
    }
}

impl From<ModifierKind> for ModifierSet {
    fn from(kind: ModifierKind) -> Self {
        ModifierSet(kind.bit())
    }
}

impl PhysicalKey {
    /// Map a physical modifier key to its modifier kind.
    pub fn modifier_kind(self) -> Option<ModifierKind> {
        use PhysicalKey::*;
        Some(match self {
            LeftShift | RightShift => ModifierKind::Shift,
            LeftCtrl | RightCtrl => ModifierKind::Ctrl,
            LeftAlt => ModifierKind::Alt,
            RightAlt => ModifierKind::AltGr,
            LeftMeta | RightMeta => ModifierKind::Meta,
            Menu => ModifierKind::Fn,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_operations() {
        let mut s = ModifierSet::empty();
        s.insert(ModifierSet::SHIFT);
        s.insert(ModifierSet::CTRL);
        assert!(s.contains(ModifierSet::SHIFT));
        assert!(s.contains(ModifierSet::CTRL));
        s.remove(ModifierSet::SHIFT);
        assert!(!s.contains(ModifierSet::SHIFT));
        assert_eq!(s, ModifierSet::CTRL);
    }

    #[test]
    fn iter_yields_contained_kinds() {
        let s = ModifierSet::SHIFT.union(ModifierSet::ALTGR);
        let got: Vec<_> = s.iter().collect();
        assert_eq!(got, vec![ModifierKind::Shift, ModifierKind::AltGr]);
    }

    #[test]
    fn physical_key_modifier_mapping() {
        assert_eq!(
            PhysicalKey::LeftShift.modifier_kind(),
            Some(ModifierKind::Shift)
        );
        assert_eq!(
            PhysicalKey::RightShift.modifier_kind(),
            Some(ModifierKind::Shift)
        );
        assert_eq!(
            PhysicalKey::RightAlt.modifier_kind(),
            Some(ModifierKind::AltGr)
        );
        assert_eq!(
            PhysicalKey::LeftAlt.modifier_kind(),
            Some(ModifierKind::Alt)
        );
        assert_eq!(
            PhysicalKey::LeftMeta.modifier_kind(),
            Some(ModifierKind::Meta)
        );
        assert_eq!(PhysicalKey::A.modifier_kind(), None);
    }

    #[test]
    fn preferred_keys_are_physical_and_distinct() {
        let mut keys = std::collections::BTreeSet::new();
        for kind in [
            ModifierKind::Shift,
            ModifierKind::Ctrl,
            ModifierKind::Alt,
            ModifierKind::AltGr,
            ModifierKind::Meta,
            ModifierKind::Fn,
        ] {
            let k = kind.preferred_key();
            assert!(k.is_modifier() || kind == ModifierKind::Fn);
            assert!(keys.insert(k), "duplicate preferred key for {kind:?}");
        }
    }
}
