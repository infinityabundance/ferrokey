//! Built-in layout loading.
//!
//! Layouts live in `layouts/*.yaml` as **data**, not in `.slint` code. This
//! module embeds them at compile time (`include_str!`), so the built-in set
//! requires no filesystem access at runtime and is deterministic.

use crate::xkb;
use ferrokey_core::layout::{KeyDefinition, KeySymbol};
use ferrokey_core::{Layout, PhysicalKey};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Errors produced while parsing layout data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    #[error("failed to parse layout {id:?}: {msg}")]
    Parse { id: String, msg: String },
    #[error("layout {id:?} is missing the required key {key}")]
    MissingKey { id: String, key: &'static str },
}

/// Intermediate representation of a layout YAML file.
#[derive(Debug, Deserialize)]
struct LayoutFile {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    keys: BTreeMap<PhysicalKey, KeyDefFile>,
}

#[derive(Debug, Default, Deserialize)]
struct KeyDefFile {
    #[serde(default)]
    primary: Option<String>,
    #[serde(default)]
    shifted: Option<String>,
    #[serde(default)]
    altgr: Option<String>,
    #[serde(default)]
    shift_altgr: Option<String>,
    #[serde(default)]
    fn_layer: Option<String>,
    #[serde(default)]
    repeatable: Option<bool>,
}

/// Parse a layout from YAML text.
pub fn parse_layout(yaml: &str) -> Result<Layout, LayoutError> {
    let file: LayoutFile = serde_yaml::from_str(yaml).map_err(|e| LayoutError::Parse {
        id: "(unknown)".into(),
        msg: e.to_string(),
    })?;
    convert(file)
}

fn convert(file: LayoutFile) -> Result<Layout, LayoutError> {
    let mut keys = std::collections::HashMap::new();
    for (physical, def) in file.keys {
        let primary = def
            .primary
            .as_deref()
            .map(KeySymbol::parse)
            .unwrap_or(KeySymbol::None);
        let shifted = def
            .shifted
            .as_deref()
            .map(KeySymbol::parse)
            .unwrap_or_else(|| primary.clone());
        let repeatable = def.repeatable.unwrap_or(true);
        keys.insert(
            physical,
            KeyDefinition {
                physical,
                primary,
                shifted,
                altgr: def.altgr.as_deref().map(KeySymbol::parse),
                shift_altgr: def.shift_altgr.as_deref().map(KeySymbol::parse),
                fn_layer: def.fn_layer.as_deref().map(KeySymbol::parse),
                repeatable,
            },
        );
    }
    let layout = Layout {
        id: file.id,
        name: file.name,
        keys,
    };
    Ok(layout)
}

/// Run the full xkb capability validation on a parsed layout, returning an
/// error naming the first missing capability. Used by the built-in loader;
/// user-provided layouts may legitimately be partial (they extend a base
/// layout), so they skip this gate.
pub fn validate_layout(layout: &Layout) -> Result<(), LayoutError> {
    xkb::validate(layout).map_err(|key| LayoutError::MissingKey {
        id: layout.id.clone(),
        key,
    })
}

/// All built-in layout ids, in deterministic order.
pub const BUILTIN_IDS: &[&str] = &["us", "us-intl", "gb", "de", "fr", "dvorak"];

/// Parse one built-in layout by id.
pub fn builtin(id: &str) -> Result<Layout, LayoutError> {
    let yaml: &str = match id {
        "us" => include_str!("../layouts/us.yaml"),
        "us-intl" => include_str!("../layouts/us-intl.yaml"),
        "gb" => include_str!("../layouts/gb.yaml"),
        "de" => include_str!("../layouts/de.yaml"),
        "fr" => include_str!("../layouts/fr.yaml"),
        "dvorak" => include_str!("../layouts/dvorak.yaml"),
        _ => {
            return Err(LayoutError::Parse {
                id: id.into(),
                msg: "unknown builtin layout id".into(),
            })
        }
    };
    let layout = parse_layout(yaml)?;
    validate_layout(&layout)?;
    Ok(layout)
}

/// Load every built-in layout into a fresh index.
pub fn builtin_index() -> ferrokey_core::layout::LayoutIndex {
    let mut index = ferrokey_core::layout::LayoutIndex::new("us");
    for id in BUILTIN_IDS {
        match builtin(id) {
            Ok(layout) => index.add(layout),
            Err(e) => log::error!("failed to load builtin layout {id}: {e}"),
        }
    }
    index
}

/// Load a layout from a YAML file on disk (for user-provided layouts).
pub fn load_from_path(path: &std::path::Path) -> Result<Layout, LayoutError> {
    let yaml = std::fs::read_to_string(path).map_err(|e| LayoutError::Parse {
        id: path.display().to_string(),
        msg: e.to_string(),
    })?;
    parse_layout(&yaml)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrokey_core::modifier::ModifierSet;

    #[test]
    fn all_builtins_parse_and_cover_alphabet_and_digits() {
        for id in BUILTIN_IDS {
            let layout = builtin(id).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(layout.id, *id);
            for letter in "abcdefghijklmnopqrstuvwxyz".chars() {
                let found = layout.keys.values().any(|d| {
                    d.primary == KeySymbol::Char(letter)
                        || d.shifted == KeySymbol::Char(letter.to_ascii_uppercase())
                });
                assert!(found, "{id}: letter {letter} not producible");
            }
            for digit in '0'..='9' {
                let found = layout
                    .keys
                    .values()
                    .any(|d| d.primary == KeySymbol::Char(digit));
                assert!(found, "{id}: digit {digit} not producible");
            }
        }
    }

    #[test]
    fn us_shift_symbols() {
        let us = builtin("us").unwrap();
        assert_eq!(
            us.symbol_for(PhysicalKey::D1, ModifierSet::empty()),
            Some(&KeySymbol::Char('1'))
        );
        assert_eq!(
            us.symbol_for(PhysicalKey::D1, ModifierSet::SHIFT),
            Some(&KeySymbol::Char('!'))
        );
        assert_eq!(
            us.symbol_for(PhysicalKey::Q, ModifierSet::SHIFT),
            Some(&KeySymbol::Char('Q'))
        );
        assert_eq!(
            us.symbol_for(PhysicalKey::Space, ModifierSet::empty()),
            Some(&KeySymbol::Char(' '))
        );
    }

    #[test]
    fn de_altgr_and_swapped_rows() {
        let de = builtin("de").unwrap();
        // QWERTZ: the physical Y key shows 'z'.
        assert_eq!(
            de.symbol_for(PhysicalKey::Y, ModifierSet::empty()),
            Some(&KeySymbol::Char('z'))
        );
        assert_eq!(
            de.symbol_for(PhysicalKey::Z, ModifierSet::empty()),
            Some(&KeySymbol::Char('y'))
        );
        // AltGr+E is €.
        assert_eq!(
            de.symbol_for(PhysicalKey::E, ModifierSet::ALTGR),
            Some(&KeySymbol::Char('€'))
        );
        assert_eq!(
            de.symbol_for(PhysicalKey::Q, ModifierSet::ALTGR),
            Some(&KeySymbol::Char('@'))
        );
    }

    #[test]
    fn us_intl_dead_keys() {
        let us_intl = builtin("us-intl").unwrap();
        assert_eq!(
            us_intl.symbol_for(PhysicalKey::Apostrophe, ModifierSet::empty()),
            Some(&KeySymbol::Dead(ferrokey_core::DeadKey::Acute))
        );
        assert_eq!(
            us_intl.symbol_for(PhysicalKey::Grave, ModifierSet::empty()),
            Some(&KeySymbol::Dead(ferrokey_core::DeadKey::Grave))
        );
    }

    #[test]
    fn gb_has_pound_on_shift_3() {
        let gb = builtin("gb").unwrap();
        assert_eq!(
            gb.symbol_for(PhysicalKey::D3, ModifierSet::SHIFT),
            Some(&KeySymbol::Char('£'))
        );
    }

    #[test]
    fn dvorak_places_apostrophe_on_q_row() {
        let dv = builtin("dvorak").unwrap();
        assert_eq!(
            dv.symbol_for(PhysicalKey::Q, ModifierSet::empty()),
            Some(&KeySymbol::Char('\''))
        );
        assert_eq!(
            dv.symbol_for(PhysicalKey::Y, ModifierSet::empty()),
            Some(&KeySymbol::Char('f'))
        );
    }

    #[test]
    fn empty_layout_round_trip() {
        let yaml = "id: test\nname: Test\nkeys: {}\n";
        let layout = parse_layout(yaml).unwrap();
        assert_eq!(layout.id, "test");
        assert!(layout.keys.is_empty());
    }

    /// Deterministic hostile-input fuzz over the layout YAML parser.
    ///
    /// A seeded PRNG (xorshift64*) generates arbitrary byte streams that are
    /// handed to `parse_layout` as lossy UTF-8 (YAML is byte-oriented, so
    /// arbitrary bytes are legal input). The parser — and, for inputs that
    /// parse, the xkb validation gate — must never panic. This runs in
    /// ordinary `cargo test` on stable, so the "hostile layout data cannot
    /// crash the loader" claim is continuously verified even without the
    /// nightly cargo-fuzz harness (`crates/ferrokey-layouts/fuzz`).
    #[test]
    fn hostile_yaml_never_panics_and_stays_bounded() {
        fn next(rng: &mut u64) -> u64 {
            *rng ^= *rng << 13;
            *rng ^= *rng >> 7;
            *rng ^= *rng << 17;
            *rng
        }
        let mut rng: u64 = 0x9E37_79B9_7F4A_7C15;
        // Under Miri the full iteration count is impractically slow; the
        // interpreter catches the same UB classes on a smaller sample.
        let iters: u64 = if cfg!(miri) { 200 } else { 20_000 };
        for _ in 0..iters {
            let len = (next(&mut rng) % 4096) as usize;
            let mut bytes = vec![0u8; len];
            for b in &mut bytes {
                *b = (next(&mut rng) >> 24) as u8;
            }
            // Malformed YAML is *expected*: the parser must return an error,
            // never panic or over-allocate.
            let text = String::from_utf8_lossy(&bytes);
            if let Ok(layout) = parse_layout(&text) {
                let _ = validate_layout(&layout);
            }
        }
    }

    #[test]
    fn shifted_defaults_to_primary() {
        let yaml = r#"
id: mini
name: Mini
keys:
  space:
    primary: " "
"#;
        let layout = parse_layout(yaml).unwrap();
        assert_eq!(
            layout.symbol_for(PhysicalKey::Space, ModifierSet::SHIFT),
            Some(&KeySymbol::Char(' '))
        );
    }
}
