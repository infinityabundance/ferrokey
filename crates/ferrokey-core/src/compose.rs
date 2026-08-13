//! Compose engine: dead keys and multi-key compose sequences.
//!
//! Ferrokey's *primary* path is physical-key semantics: an OSK dead key is a
//! real `KEY_APOSTROPHE`/`KEY_GRAVE` press and the desktop XKB keymap composes
//! it. That works only when the desktop keymap matches the OSK layout.
//!
//! The compose engine is the **text-mode** path. When the OSK layout declares a
//! key as [`KeySymbol::Dead`] or [`KeySymbol::Compose`], the UI feeds the
//! engine; it composes `' + e → é`, `~ + n → ñ`, `compose o c → ©` and emits a
//! single Unicode character that the text backend then types.
//!
//! The engine is pure and deterministic (no clock, no I/O), so the whole
//! behaviour is unit-testable:
//!
//! ```text
//! Dead(Acute) + Char('e')        → Emit('é')
//! Dead(Acute) + Char(' ')        → Emit('\'')            (standalone accent)
//! Dead(Acute) + Char('q')        → Reset, reprocess 'q'  (accent dropped,
//!                                                          like X11)
//! Dead(Acute) + Dead(Acute)      → Emit('\'')            (press twice → literal)
//! Compose + Compose              → Cancelled             (X11 semantics)
//! Compose + Char('o') + Char('c')→ Emit('©')
//! Compose + Char('a') + Char('e')→ Emit('æ')
//! Compose + Char('q') + Char('w')→ Reset: emit 'q', reprocess 'w'
//! ```
//!
//! The table data mirrors the common subset of X11's
//! `en_US.UTF-8/Compose` file. Sequences that are not in the table never
//! invent output: they either cancel or fall through to the caller.

use crate::layout::{DeadKey, KeySymbol};
use std::collections::BTreeMap;
use std::sync::LazyLock;

/// The result of feeding one symbol into the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedOutcome {
    /// The symbol was not compose input; the caller should process it normally
    /// (the engine state is unchanged).
    Pass,
    /// The symbol was consumed by the engine (started or extended a pending
    /// sequence); nothing should be emitted yet.
    Consumed,
    /// The sequence completed; emit these characters.
    Emit(Vec<char>),
    /// The pending sequence could not complete. Emit `standalone` first, then
    /// re-feed `reprocess` (if any) and handle its outcome.
    Reset {
        standalone: Vec<char>,
        reprocess: Option<KeySymbol>,
    },
    /// The pending sequence was cancelled (nothing to emit).
    Cancelled,
}

/// The engine's pending state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    /// A dead accent is waiting for its base character.
    Dead(DeadKey),
    /// The compose key was pressed; the first sequence character is still
    /// expected.
    Compose,
    /// The compose key and the first character arrived; the second character
    /// decides the outcome.
    ComposeChar(char),
}

/// The dead-key / multi-key compose engine.
#[derive(Debug, Clone, Default)]
pub struct ComposeEngine {
    pending: Option<Pending>,
}

impl ComposeEngine {
    pub fn new() -> Self {
        ComposeEngine { pending: None }
    }

    /// Feed one key symbol. See [`FeedOutcome`] for the contract.
    pub fn feed(&mut self, symbol: &KeySymbol) -> FeedOutcome {
        match self.pending {
            None => match symbol {
                KeySymbol::Dead(d) => {
                    self.pending = Some(Pending::Dead(*d));
                    FeedOutcome::Consumed
                }
                KeySymbol::Compose => {
                    self.pending = Some(Pending::Compose);
                    FeedOutcome::Consumed
                }
                KeySymbol::Char(_) | KeySymbol::Name(_) | KeySymbol::None => FeedOutcome::Pass,
            },
            Some(Pending::Dead(d)) => self.feed_dead(d, symbol),
            Some(Pending::Compose) => match symbol {
                KeySymbol::Compose => {
                    // Multi_key + Multi_key cancels (X11 semantics).
                    self.pending = None;
                    FeedOutcome::Cancelled
                }
                KeySymbol::Char(c) => {
                    self.pending = Some(Pending::ComposeChar(*c));
                    FeedOutcome::Consumed
                }
                KeySymbol::Dead(d) => {
                    // A dead key as the first compose character behaves like
                    // its standalone accent (the compose table below includes
                    // the `' e → é` family, matching X11's <Multi_key>
                    // entries for <dead_acute> and friends).
                    self.pending = Some(Pending::ComposeChar(standalone_accent(*d)));
                    FeedOutcome::Consumed
                }
                KeySymbol::Name(_) | KeySymbol::None => {
                    // A non-character key cancels the compose sequence.
                    self.pending = None;
                    FeedOutcome::Cancelled
                }
            },
            Some(Pending::ComposeChar(first)) => match symbol {
                KeySymbol::Char(second) => {
                    self.pending = None;
                    match compose_char(first, *second) {
                        Some(composed) => FeedOutcome::Emit(vec![composed]),
                        None => FeedOutcome::Reset {
                            standalone: vec![first],
                            reprocess: Some(KeySymbol::Char(*second)),
                        },
                    }
                }
                KeySymbol::Dead(d) => {
                    // Second key is a dead accent: try the composition
                    // (compose ' e → é), otherwise emit the first char and
                    // restart pending with the dead key.
                    self.pending = None;
                    match compose_char(first, standalone_accent(*d)) {
                        Some(composed) => FeedOutcome::Emit(vec![composed]),
                        None => FeedOutcome::Reset {
                            standalone: vec![first],
                            reprocess: Some(KeySymbol::Dead(*d)),
                        },
                    }
                }
                KeySymbol::Compose | KeySymbol::Name(_) | KeySymbol::None => {
                    // Anything else cancels and drops the first character.
                    self.pending = None;
                    FeedOutcome::Cancelled
                }
            },
        }
    }

    /// Whether a sequence is currently pending (the UI shows a hint).
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// A short human-readable label for the pending state (for the UI), or an
    /// empty string when idle.
    pub fn pending_label(&self) -> String {
        match self.pending {
            None => String::new(),
            Some(Pending::Dead(d)) => format!("◌{} …", d.name()),
            Some(Pending::Compose) => "compose: …".into(),
            Some(Pending::ComposeChar(c)) => format!("compose: {c} …"),
        }
    }

    /// Cancel any pending sequence (release-all, hide, disconnect, …).
    pub fn reset(&mut self) {
        self.pending = None;
    }

    fn feed_dead(&mut self, dead: DeadKey, symbol: &KeySymbol) -> FeedOutcome {
        match symbol {
            KeySymbol::Char(c) => {
                self.pending = None;
                if *c == ' ' {
                    // Dead + space = the standalone accent (X11 behaviour).
                    return FeedOutcome::Emit(vec![standalone_accent(dead)]);
                }
                match compose_dead(dead, *c) {
                    Some(composed) => FeedOutcome::Emit(vec![composed]),
                    None => {
                        // Dead + non-composing char: the accent is dropped and
                        // the base character is delivered (X11 behaviour).
                        FeedOutcome::Reset {
                            standalone: Vec::new(),
                            reprocess: Some(KeySymbol::Char(*c)),
                        }
                    }
                }
            }
            KeySymbol::Dead(d2) => {
                if *d2 == dead {
                    // Pressing the same dead key twice yields the literal
                    // accent (the common "how do I type a quote" escape).
                    self.pending = None;
                    FeedOutcome::Emit(vec![standalone_accent(dead)])
                } else {
                    // A different dead key: emit the first accent literally
                    // and start pending with the second (simplified double-
                    // accent handling; documented divergence from X11).
                    self.pending = Some(Pending::Dead(*d2));
                    FeedOutcome::Reset {
                        standalone: vec![standalone_accent(dead)],
                        reprocess: None,
                    }
                }
            }
            KeySymbol::Compose => {
                self.pending = None;
                FeedOutcome::Reset {
                    standalone: Vec::new(),
                    reprocess: Some(KeySymbol::Compose),
                }
            }
            KeySymbol::Name(_) | KeySymbol::None => {
                self.pending = None;
                FeedOutcome::Cancelled
            }
        }
    }
}

/// The standalone (spacing) form of each dead accent, used for dead+space and
/// the "press twice" escape.
pub const fn standalone_accent(dead: DeadKey) -> char {
    match dead {
        DeadKey::Grave => '`',
        DeadKey::Acute => '\'',
        DeadKey::Circumflex => '^',
        DeadKey::Tilde => '~',
        DeadKey::Diaeresis => '"',
        DeadKey::Cedilla => '\u{00B8}',     // ¸
        DeadKey::Ring => '\u{02DA}',        // ˚
        DeadKey::Caron => '\u{02C7}',       // ˇ
        DeadKey::Breve => '\u{02D8}',       // ˘
        DeadKey::DoubleAcute => '\u{02DD}', // ˝
        DeadKey::Ogonek => '\u{02DB}',      // ˛
        DeadKey::Macron => '\u{00AF}',      // ¯
        DeadKey::Horn => '\u{031B}',        // combining horn
        DeadKey::HookAbove => '\u{0309}',   // combining hook above
        DeadKey::Abovedot => '\u{02D9}',    // ˙
        DeadKey::Belowdot => '\u{0323}',    // combining dot below
        DeadKey::Stroke => '/',
    }
}

/// Look up `(dead, base)` in the dead-key composition table.
fn compose_dead(dead: DeadKey, base: char) -> Option<char> {
    DEAD_TABLE.get(&(dead, base)).copied()
}

/// Look up `(first, second)` in the multi-key compose table.
fn compose_char(first: char, second: char) -> Option<char> {
    COMPOSE_TABLE.get(&(first, second)).copied()
}

type DeadPair = (DeadKey, char);
type CharPair = (char, char);

/// Dead-key composition table: `(dead accent, base character) → composed char`.
///
/// Covers the classic Latin repertoire for all seventeen accents Ferrokey
/// knows, plus the `dead + space → standalone accent` entries so the engine
/// can special-case nothing.
fn dead_table() -> BTreeMap<DeadPair, char> {
    let mut t = BTreeMap::new();
    let mut add = |dead: DeadKey, base: &str, out: &str| {
        for (b, o) in base.chars().zip(out.chars()) {
            t.insert((dead, b), o);
        }
    };
    // The compact notation below is (base string, composed string) — both
    // must have identical character counts.

    add(DeadKey::Grave, "aAeEiIoOuUnNwWyY", "àÀèÈìÌòÒùÙǹǸẁẀỳỲ");
    add(
        DeadKey::Acute,
        "aAeEiIoOuUyYcClLnNrRsSzZgGkKmMpP",
        "áÁéÉíÍóÓúÚýÝćĆĺĹńŃŕŔśŚźŹǵǴḱḰḿḾṕṔ",
    );
    add(DeadKey::Circumflex, "aAeEiIoOuU", "âÂêÊîÎôÔûÛ");
    add(DeadKey::Tilde, "aAnNoO", "ãÃñÑõÕ");
    add(DeadKey::Diaeresis, "aAeEiIoOuUyY", "äÄëËïÏöÖüÜÿŸ");
    add(DeadKey::Cedilla, "cCsStT", "çÇşŞţŢ");
    add(DeadKey::Ring, "aAuU", "åÅůŮ");
    add(
        DeadKey::Caron,
        "cCdDeElLnNrRsStTzZgGoO",
        "čČďĎěĚľĽňŇřŘšŠťŤžŽǧǦǒǑ",
    );
    add(DeadKey::Breve, "aAeEgGuU", "ăĂĕĔğĞŭŬ");
    add(DeadKey::DoubleAcute, "oOuU", "őŐűŰ");
    add(DeadKey::Ogonek, "aAeEiIoOuU", "ąĄęĘįĮǫǪųŲ");
    add(DeadKey::Macron, "aAeEiIoOuUyY", "āĀēĒīĪōŌūŪȳȲ");
    add(DeadKey::Horn, "oOuU", "ơƠưƯ");
    add(DeadKey::HookAbove, "aAeEiIoOuUyY", "ảẢẻẺỉỈỏỎủỦỷỶ");
    add(DeadKey::Abovedot, "aAcCeEgGoOsSzZyY", "ȧȦċĊėĖġĠȯȮṡṠżŻẏẎ");
    add(DeadKey::Belowdot, "aAeEiIoOuUyY", "ạẠẹẸịỊọỌụỤỵỴ");
    add(DeadKey::Stroke, "dDhHlLoOtT", "đĐħĦłŁøØŧŦ");

    // dead + space → standalone accent (X11 emits the accent alone).
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
        t.insert((dead, ' '), standalone_accent(dead));
    }
    t
}

/// Multi-key compose table: `(first char, second char) → composed char`.
///
/// The well-known `en_US.UTF-8/Compose` subset: symbols, fractions, quotes,
/// currency, and the accent-prefix family (`' e → é`) that makes the compose
/// key work like a dead key.
fn compose_table() -> BTreeMap<CharPair, char> {
    let mut t = BTreeMap::new();
    let mut add = |first: char, second: char, out: char| {
        t.insert((first, second), out);
    };

    // Symbols.
    add('o', 'c', '©');
    add('c', 'o', '©');
    add('r', 'o', '®');
    add('t', 'm', '™');
    add('T', 'M', '™');
    add('c', '|', '¢');
    add('L', '-', '£');
    add('l', '-', '£');
    add('y', '=', '¥');
    add('Y', '=', '¥');
    add('e', '=', '€');
    add('E', '=', '€');
    add('s', 'o', '§');
    add('S', 'o', '§');
    add('p', '!', '¶');
    add('P', '!', '¶');
    add('m', 'u', 'µ');
    add('M', 'u', 'µ');
    add('N', 'o', '№');
    add('d', 'g', '°');
    add('o', 'x', '¤');
    add('-', '-', '–'); // en dash
    add('?', '?', '¿');
    add('!', '!', '¡');
    add('<', '<', '«');
    add('>', '>', '»');
    add('/', '/', '\\');
    add('-', ':', '÷');
    add('.', '^', '·');
    add('o', 'o', '°');
    add('t', 'h', 'þ');
    add('T', 'H', 'Þ');
    add('d', 'h', 'ð');
    add('D', 'H', 'Ð');
    add('/', 'o', 'ø');
    add('/', 'O', 'Ø');
    add('a', 'e', 'æ');
    add('A', 'E', 'Æ');
    add('o', 'e', 'œ');
    add('O', 'E', 'Œ');
    add('o', 'a', 'å');
    add('O', 'A', 'Å');
    add('s', 's', 'ß');

    // Fractions.
    add('1', '2', '½');
    add('1', '4', '¼');
    add('3', '4', '¾');
    add('1', '3', '⅓');
    add('2', '3', '⅔');
    add('1', '8', '⅛');
    add('3', '8', '⅜');
    add('5', '8', '⅝');
    add('7', '8', '⅞');

    // Superscripts.
    add('^', '1', '¹');
    add('^', '2', '²');
    add('^', '3', '³');
    add('s', '1', '¹');
    add('s', '2', '²');
    add('s', '3', '³');

    // Accent-prefix family (compose works like a dead key too).
    add('\'', 'a', 'á');
    add('\'', 'e', 'é');
    add('\'', 'i', 'í');
    add('\'', 'o', 'ó');
    add('\'', 'u', 'ú');
    add('\'', 'y', 'ý');
    add('\'', 'c', 'ć');
    add('\'', 'n', 'ń');
    add('\'', 's', 'ś');
    add('\'', 'z', 'ź');
    add('\'', 'A', 'Á');
    add('\'', 'E', 'É');
    add('\'', 'I', 'Í');
    add('\'', 'O', 'Ó');
    add('\'', 'U', 'Ú');
    add('\'', 'Y', 'Ý');
    add('\'', 'C', 'Ć');
    add('\'', 'N', 'Ń');
    add('\'', 'S', 'Ś');
    add('\'', 'Z', 'Ź');
    add('`', 'a', 'à');
    add('`', 'e', 'è');
    add('`', 'i', 'ì');
    add('`', 'o', 'ò');
    add('`', 'u', 'ù');
    add('`', 'A', 'À');
    add('`', 'E', 'È');
    add('`', 'I', 'Ì');
    add('`', 'O', 'Ò');
    add('`', 'U', 'Ù');
    add('^', 'a', 'â');
    add('^', 'e', 'ê');
    add('^', 'i', 'î');
    add('^', 'o', 'ô');
    add('^', 'u', 'û');
    add('^', 'A', 'Â');
    add('^', 'E', 'Ê');
    add('^', 'I', 'Î');
    add('^', 'O', 'Ô');
    add('^', 'U', 'Û');
    add('~', 'a', 'ã');
    add('~', 'n', 'ñ');
    add('~', 'o', 'õ');
    add('~', 'A', 'Ã');
    add('~', 'N', 'Ñ');
    add('~', 'O', 'Õ');
    add('"', 'a', 'ä');
    add('"', 'e', 'ë');
    add('"', 'i', 'ï');
    add('"', 'o', 'ö');
    add('"', 'u', 'ü');
    add('"', 'y', 'ÿ');
    add('"', 'A', 'Ä');
    add('"', 'E', 'Ë');
    add('"', 'I', 'Ï');
    add('"', 'O', 'Ö');
    add('"', 'U', 'Ü');
    add('"', 'Y', 'Ÿ');
    add(',', 'c', 'ç');
    add(',', 'C', 'Ç');

    t
}

static DEAD_TABLE: LazyLock<BTreeMap<DeadPair, char>> = LazyLock::new(dead_table);
static COMPOSE_TABLE: LazyLock<BTreeMap<CharPair, char>> = LazyLock::new(compose_table);

#[cfg(test)]
mod tests {
    use super::*;

    fn dead(d: DeadKey) -> KeySymbol {
        KeySymbol::Dead(d)
    }
    fn ch(c: char) -> KeySymbol {
        KeySymbol::Char(c)
    }

    #[test]
    fn acute_e_composes() {
        let mut e = ComposeEngine::new();
        assert_eq!(e.feed(&dead(DeadKey::Acute)), FeedOutcome::Consumed);
        assert!(e.is_pending());
        assert_eq!(e.feed(&ch('e')), FeedOutcome::Emit(vec!['é']));
        assert!(!e.is_pending());
    }

    #[test]
    fn tilde_n_composes() {
        let mut e = ComposeEngine::new();
        e.feed(&dead(DeadKey::Tilde));
        assert_eq!(e.feed(&ch('n')), FeedOutcome::Emit(vec!['ñ']));
    }

    #[test]
    fn dead_space_is_standalone_accent() {
        let mut e = ComposeEngine::new();
        e.feed(&dead(DeadKey::Grave));
        assert_eq!(e.feed(&ch(' ')), FeedOutcome::Emit(vec!['`']));
        let mut e = ComposeEngine::new();
        e.feed(&dead(DeadKey::Acute));
        assert_eq!(e.feed(&ch(' ')), FeedOutcome::Emit(vec!['\'']));
    }

    #[test]
    fn dead_non_composing_drops_accent() {
        let mut e = ComposeEngine::new();
        e.feed(&dead(DeadKey::Acute));
        assert_eq!(
            e.feed(&ch('q')),
            FeedOutcome::Reset {
                standalone: vec![],
                reprocess: Some(ch('q')),
            }
        );
        assert!(!e.is_pending());
    }

    #[test]
    fn dead_twice_is_literal_accent() {
        let mut e = ComposeEngine::new();
        e.feed(&dead(DeadKey::Acute));
        assert_eq!(e.feed(&dead(DeadKey::Acute)), FeedOutcome::Emit(vec!['\'']));
        assert!(!e.is_pending());
    }

    #[test]
    fn dead_then_other_dead_emits_first_and_pends_second() {
        let mut e = ComposeEngine::new();
        e.feed(&dead(DeadKey::Grave));
        assert_eq!(
            e.feed(&dead(DeadKey::Acute)),
            FeedOutcome::Reset {
                standalone: vec!['`'],
                reprocess: None,
            }
        );
        assert!(e.is_pending());
        // The second dead key is now pending.
        assert_eq!(e.feed(&ch('e')), FeedOutcome::Emit(vec!['é']));
    }

    #[test]
    fn compose_oc_is_copyright() {
        let mut e = ComposeEngine::new();
        assert_eq!(e.feed(&KeySymbol::Compose), FeedOutcome::Consumed);
        assert_eq!(e.feed(&ch('o')), FeedOutcome::Consumed);
        assert_eq!(e.feed(&ch('c')), FeedOutcome::Emit(vec!['©']));
        assert!(!e.is_pending());
    }

    #[test]
    fn compose_ae_is_ae_ligature() {
        let mut e = ComposeEngine::new();
        e.feed(&KeySymbol::Compose);
        e.feed(&ch('a'));
        assert_eq!(e.feed(&ch('e')), FeedOutcome::Emit(vec!['æ']));
    }

    #[test]
    fn compose_quote_e_is_acute_e() {
        let mut e = ComposeEngine::new();
        e.feed(&KeySymbol::Compose);
        e.feed(&ch('\''));
        assert_eq!(e.feed(&ch('e')), FeedOutcome::Emit(vec!['é']));
    }

    #[test]
    fn compose_compose_cancels() {
        let mut e = ComposeEngine::new();
        e.feed(&KeySymbol::Compose);
        assert_eq!(e.feed(&KeySymbol::Compose), FeedOutcome::Cancelled);
        assert!(!e.is_pending());
    }

    #[test]
    fn compose_no_match_emits_first_and_reprocesses_second() {
        let mut e = ComposeEngine::new();
        e.feed(&KeySymbol::Compose);
        e.feed(&ch('q'));
        assert_eq!(
            e.feed(&ch('w')),
            FeedOutcome::Reset {
                standalone: vec!['q'],
                reprocess: Some(ch('w')),
            }
        );
        assert!(!e.is_pending());
    }

    #[test]
    fn plain_char_passes_through() {
        let mut e = ComposeEngine::new();
        assert_eq!(e.feed(&ch('a')), FeedOutcome::Pass);
        assert_eq!(e.feed(&KeySymbol::Name("enter".into())), FeedOutcome::Pass);
        assert_eq!(e.feed(&KeySymbol::None), FeedOutcome::Pass);
    }

    #[test]
    fn reset_clears_pending() {
        let mut e = ComposeEngine::new();
        e.feed(&dead(DeadKey::Acute));
        assert!(e.is_pending());
        e.reset();
        assert!(!e.is_pending());
        assert_eq!(e.feed(&ch('e')), FeedOutcome::Pass);
    }

    #[test]
    fn pending_labels() {
        let mut e = ComposeEngine::new();
        assert_eq!(e.pending_label(), "");
        e.feed(&dead(DeadKey::Acute));
        assert_eq!(e.pending_label(), "◌acute …");
        e.reset();
        e.feed(&KeySymbol::Compose);
        assert_eq!(e.pending_label(), "compose: …");
        e.feed(&ch('o'));
        assert_eq!(e.pending_label(), "compose: o …");
    }

    #[test]
    fn representative_compositions_cover_common_accents() {
        // ' + e → é, ~ + n → ñ, ^ + o → ô, " + u → ü, ` + a → à, , + c → ç.
        let cases = [
            (DeadKey::Acute, 'e', 'é'),
            (DeadKey::Tilde, 'n', 'ñ'),
            (DeadKey::Circumflex, 'o', 'ô'),
            (DeadKey::Diaeresis, 'u', 'ü'),
            (DeadKey::Grave, 'a', 'à'),
            (DeadKey::Cedilla, 'c', 'ç'),
            (DeadKey::Ring, 'a', 'å'),
            (DeadKey::Caron, 'c', 'č'),
            (DeadKey::Macron, 'o', 'ō'),
            (DeadKey::Ogonek, 'e', 'ę'),
            (DeadKey::Belowdot, 'a', 'ạ'),
            (DeadKey::Stroke, 'o', 'ø'),
        ];
        for (d, base, composed) in cases {
            let mut e = ComposeEngine::new();
            e.feed(&dead(d));
            assert_eq!(e.feed(&ch(base)), FeedOutcome::Emit(vec![composed]));
        }
    }

    #[test]
    fn every_dead_key_has_a_standalone_form() {
        for d in [
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
            assert_ne!(standalone_accent(d), '\0', "{d:?} has no standalone form");
            let mut e = ComposeEngine::new();
            e.feed(&dead(d));
            let out = e.feed(&ch(' '));
            assert_eq!(out, FeedOutcome::Emit(vec![standalone_accent(d)]));
        }
    }

    #[test]
    fn dead_followed_by_shift_like_name_cancels() {
        // A non-character key cancels the pending dead accent (documented
        // simplification: the user changed their mind).
        let mut e = ComposeEngine::new();
        e.feed(&dead(DeadKey::Acute));
        assert_eq!(
            e.feed(&KeySymbol::Name("shift".into())),
            FeedOutcome::Cancelled
        );
        assert!(!e.is_pending());
        // The next character is a plain char again.
        assert_eq!(e.feed(&ch('e')), FeedOutcome::Pass);
    }

    #[test]
    fn tables_are_internally_consistent() {
        // Every dead+space entry must agree with standalone_accent.
        for (d, base) in DEAD_TABLE.keys() {
            if *base == ' ' {
                assert_eq!(DEAD_TABLE[&(*d, ' ')], standalone_accent(*d));
            }
        }
        // The accent-prefix compose family must agree with the dead table.
        for ((first, second), out) in COMPOSE_TABLE.iter() {
            if let Some(dead) = accent_for(*first) {
                if let Some(composed) = compose_dead(dead, *second) {
                    assert_eq!(*out, composed, "compose {first} {second} != dead {dead:?}");
                }
            }
        }
    }

    fn accent_for(c: char) -> Option<DeadKey> {
        match c {
            '\'' => Some(DeadKey::Acute),
            '`' => Some(DeadKey::Grave),
            '^' => Some(DeadKey::Circumflex),
            '~' => Some(DeadKey::Tilde),
            '"' => Some(DeadKey::Diaeresis),
            ',' => Some(DeadKey::Cedilla),
            _ => None,
        }
    }
}
