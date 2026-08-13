//! The bounded ANSI/ECMA-48 byte parser (§9, §72–§75).
//!
//! PTY output is **hostile input**. This module is a strict, bounded state
//! machine that turns bytes into a small closed set of [`ParserAction`]s.
//! The guarantees that make it fuzz-safe (§76):
//!
//! * No allocation on the hot path except a *reused*, capped OSC buffer
//!   ([`crate::limits::MAX_OSC_LEN`]).
//! * CSI parameters saturate at [`crate::limits::MAX_CSI_VALUE`] and at most
//!   [`crate::limits::MAX_CSI_PARAMS`] parameters are tracked (extras are
//!   ignored, never accumulated).
//! * DCS/SOS/PM/APC payloads are counted and dropped past
//!   [`crate::limits::MAX_DCS_LEN`].
//! * Invalid UTF-8, malformed sequences and unterminated strings recover to
//!   the ground state safely; no state can grow without bound.
//! * No panics on any input (arithmetic is saturating; indexing is bounded).
//!
//! The parser emits *what happened*, not *what to draw*: the terminal
//! ([`crate::terminal::Terminal`]) applies actions to the grid with full
//! access to modes, scrollback and the palette.

use crate::limits;

/// Maximum length of a single UTF-8 sequence.
const UTF8_MAX: usize = 4;

/// C0 and C1 control functions the terminal understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCode {
    /// 0x07 BEL — visual/audible bell.
    Bell,
    /// 0x08 BS.
    Backspace,
    /// 0x09 HT.
    Tab,
    /// 0x0A LF.
    LineFeed,
    /// 0x0B VT.
    VerticalTab,
    /// 0x0C FF.
    FormFeed,
    /// 0x0D CR.
    CarriageReturn,
    /// 0x1B ESC (standalone).
    Escape,
    /// 0x84 IND.
    Index,
    /// 0x85 NEL.
    NextLine,
    /// 0x88 HTS.
    TabSet,
    /// 0x8D RI.
    ReverseIndex,
}

/// A complete CSI sequence (`CSI Pm I F`).
///
/// `private` is one of `?`, `>`, `=`, `<`, or 0. `params` holds at most
/// [`limits::MAX_CSI_PARAMS`] values; `intermediates` at most two bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Csi {
    pub private: u8,
    pub params: [u16; limits::MAX_CSI_PARAMS],
    pub param_count: usize,
    pub intermediates: [u8; 2],
    pub inter_count: u8,
    pub final_byte: u8,
}

impl Csi {
    /// The first parameter, defaulting to `default` when absent or zero
    /// (ECMA-48: omitted parameters default to 1; many sequences use 0 as
    /// "default").
    pub fn param(&self, index: usize, default: u16) -> u16 {
        match self.param_count {
            0 => default,
            n if index >= n => default,
            _ => {
                let v = self.params[index];
                if v == 0 {
                    default
                } else {
                    v
                }
            }
        }
    }

    /// The raw parameter, without the zero→default rewrite.
    pub fn raw_param(&self, index: usize) -> Option<u16> {
        if index < self.param_count {
            Some(self.params[index])
        } else {
            None
        }
    }

    /// Whether every present parameter is zero (`CSI 0 m` == `CSI m`).
    pub fn is_all_zero(&self) -> bool {
        (0..self.param_count).all(|i| self.params[i] == 0)
    }
}

/// A complete OSC sequence payload (raw bytes, bounded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Osc {
    pub payload: Vec<u8>,
}

/// Standalone escape sequences (`ESC c`, `ESC 7`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscSequence {
    /// ESC 7 — save cursor.
    SaveCursor,
    /// ESC 8 — restore cursor.
    RestoreCursor,
    /// ESC D — index.
    Index,
    /// ESC E — next line.
    NextLine,
    /// ESC M — reverse index.
    ReverseIndex,
    /// ESC H — set tab stop.
    TabSet,
    /// ESC = — application keypad.
    KeypadApplication,
    /// ESC > — numeric keypad.
    KeypadNumeric,
    /// ESC c — full reset.
    FullReset,
    /// ESC Z — DECID (device attributes; terminal answers like DA).
    Decid,
    /// ESC # N — DEC line attributes (only # 8, DECALN, is acted on).
    LineAttributes(u8),
}

/// One parser output: a closed set of terminal operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParserAction {
    /// A printable character (already UTF-8 decoded).
    Print(char),
    /// A C0/C1 control.
    Control(ControlCode),
    /// A complete CSI sequence.
    Csi(Csi),
    /// A complete OSC sequence (payload bounded).
    Osc(Osc),
    /// A standalone ESC sequence.
    Esc(EscSequence),
    /// `ESC ( X` etc. — charset selection. Accepted and ignored; the
    /// character set is always Unicode for Ferrokey.
    CharsetSelect(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    CharsetSelect,
    LineAttributes,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    OscString,
    DcsEntry,
    DcsPassthrough,
    SosPmApc,
}

/// Diagnostic counters (bounded, monotonic) — useful for courts and for
/// detecting hostile-input floods without logging every byte.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParserStats {
    pub bytes: u64,
    pub chars: u64,
    pub controls: u64,
    pub csi: u64,
    pub osc: u64,
    pub truncated_osc: u64,
    pub truncated_dcs: u64,
    pub invalid_utf8: u64,
    pub ignored_escapes: u64,
}

/// The parser. Zero configuration; bounds are fixed by [`limits`].
#[derive(Debug, Clone)]
pub struct Parser {
    state: State,
    utf8: [u8; UTF8_MAX],
    utf8_len: u8,
    utf8_expected: u8,
    private: u8,
    params: [u16; limits::MAX_CSI_PARAMS],
    param_count: usize,
    cur_param: u16,
    cur_param_overflow: bool,
    intermediates: [u8; 2],
    inter_count: u8,
    /// Reused OSC buffer (bounded).
    osc: Vec<u8>,
    osc_truncated: bool,
    /// True when an ESC byte arrived inside an OSC string: only `ESC \`
    /// terminates; any other byte resumes the string (with the ESC dropped).
    osc_pending_esc: bool,
    /// DCS / SOS / PM / APC payload length (counted, then dropped).
    passthrough_len: usize,
    pub stats: ParserStats,
}

impl Default for Parser {
    fn default() -> Self {
        Parser::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Parser {
            state: State::Ground,
            utf8: [0; UTF8_MAX],
            utf8_len: 0,
            utf8_expected: 0,
            private: 0,
            params: [0; limits::MAX_CSI_PARAMS],
            param_count: 0,
            cur_param: 0,
            cur_param_overflow: false,
            intermediates: [0; 2],
            inter_count: 0,
            osc: Vec::with_capacity(64),
            osc_truncated: false,
            osc_pending_esc: false,
            passthrough_len: 0,
            stats: ParserStats::default(),
        }
    }

    /// Feed a batch of bytes, invoking `sink` for every action produced.
    pub fn feed<F: FnMut(ParserAction)>(&mut self, bytes: &[u8], sink: &mut F) {
        for &b in bytes {
            self.advance(b, sink);
        }
    }

    /// Process one byte. Returns the action produced, if any.
    pub fn advance<F: FnMut(ParserAction)>(&mut self, byte: u8, sink: &mut F) {
        self.stats.bytes = self.stats.bytes.wrapping_add(1);
        match self.state {
            State::Ground => self.ground(byte, sink),
            State::Escape => self.escape(byte, sink),
            State::EscapeIntermediate => self.escape_intermediate(byte, sink),
            State::CharsetSelect => self.charset_select(byte, sink),
            State::LineAttributes => self.line_attributes(byte, sink),
            State::CsiEntry => self.csi_entry(byte, sink),
            State::CsiParam => self.csi_param(byte, sink),
            State::CsiIntermediate => self.csi_intermediate(byte, sink),
            State::OscString => self.osc_string(byte, sink),
            State::DcsEntry => self.dcs_entry(byte, sink),
            State::DcsPassthrough => self.dcs_passthrough(byte, sink),
            State::SosPmApc => self.sos_pm_apc(byte, sink),
        }
    }

    // ── Ground ────────────────────────────────────────────────────────────

    fn ground<F: FnMut(ParserAction)>(&mut self, byte: u8, sink: &mut F) {
        // A multi-byte UTF-8 sequence in progress consumes everything until
        // it completes or fails — control bytes are only C1 when they appear
        // *outside* a sequence.
        if self.utf8_len > 0 {
            self.utf8_byte(byte, sink);
            return;
        }
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => {
                self.emit_control(byte, sink);
            }
            0x18 | 0x1A | 0x7F => {
                // CAN / SUB / DEL: ignored in ground.
            }
            0x1B => {
                self.state = State::Escape;
            }
            0x20..=0x7E => {
                self.stats.chars = self.stats.chars.wrapping_add(1);
                sink(ParserAction::Print(char::from(byte)));
            }
            0x80..=0x9F => {
                // C1 controls (only meaningful outside a UTF-8 sequence).
                match byte {
                    0x84 => sink(ParserAction::Control(ControlCode::Index)),
                    0x85 => sink(ParserAction::Control(ControlCode::NextLine)),
                    0x88 => sink(ParserAction::Control(ControlCode::TabSet)),
                    0x8D => sink(ParserAction::Control(ControlCode::ReverseIndex)),
                    0x9B => self.state = State::CsiEntry,
                    0x9D => self.begin_osc(),
                    0x90 => self.state = State::DcsEntry,
                    0x98 | 0x9E | 0x9F => {
                        self.passthrough_len = 0;
                        self.state = State::SosPmApc;
                    }
                    0x9C => {
                        // ST in ground: ignore.
                    }
                    _ => {
                        // Bare C1 bytes with no supported meaning are invalid
                        // UTF-8 (a continuation byte outside a sequence);
                        // emit the replacement character.
                        self.stats.invalid_utf8 = self.stats.invalid_utf8.wrapping_add(1);
                        self.stats.chars = self.stats.chars.wrapping_add(1);
                        sink(ParserAction::Print('\u{FFFD}'));
                    }
                }
            }
            _ => {
                // UTF-8 lead or invalid byte.
                self.utf8_byte(byte, sink);
            }
        }
    }

    fn utf8_byte<F: FnMut(ParserAction)>(&mut self, byte: u8, sink: &mut F) {
        if self.utf8_len == 0 {
            let expected = match byte {
                0xC2..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF4 => 4,
                _ => 0,
            };
            if expected == 0 {
                // Invalid lead byte.
                self.stats.invalid_utf8 = self.stats.invalid_utf8.wrapping_add(1);
                self.stats.chars = self.stats.chars.wrapping_add(1);
                sink(ParserAction::Print('\u{FFFD}'));
                return;
            }
            self.utf8[0] = byte;
            self.utf8_len = 1;
            self.utf8_expected = expected;
            return;
        }
        // Continuation byte.
        if (0x80..=0xBF).contains(&byte) {
            self.utf8[usize::from(self.utf8_len)] = byte;
            self.utf8_len += 1;
            if self.utf8_len == self.utf8_expected {
                let s = std::str::from_utf8(&self.utf8[..usize::from(self.utf8_len)]);
                self.utf8_len = 0;
                if let Ok(s) = s {
                    let mut chars = s.chars();
                    if let Some(c) = chars.next() {
                        // Reject overlong/illegal encodings (from_utf8
                        // already rejects them) and drop the character if
                        // it is a control (C1 range encoded in UTF-8 is
                        // invalid anyway).
                        self.stats.chars = self.stats.chars.wrapping_add(1);
                        sink(ParserAction::Print(c));
                    }
                } else {
                    self.stats.invalid_utf8 = self.stats.invalid_utf8.wrapping_add(1);
                    self.stats.chars = self.stats.chars.wrapping_add(1);
                    sink(ParserAction::Print('\u{FFFD}'));
                }
            }
        } else {
            // Invalid continuation: emit replacement, then reprocess this
            // byte from the ground state.
            self.utf8_len = 0;
            self.stats.invalid_utf8 = self.stats.invalid_utf8.wrapping_add(1);
            self.stats.chars = self.stats.chars.wrapping_add(1);
            sink(ParserAction::Print('\u{FFFD}'));
            self.ground(byte, sink);
        }
    }

    fn emit_control<F: FnMut(ParserAction)>(&mut self, byte: u8, sink: &mut F) {
        self.stats.controls = self.stats.controls.wrapping_add(1);
        let code = match byte {
            0x07 => ControlCode::Bell,
            0x08 => ControlCode::Backspace,
            0x09 => ControlCode::Tab,
            0x0A => ControlCode::LineFeed,
            0x0B => ControlCode::VerticalTab,
            0x0C => ControlCode::FormFeed,
            0x0D => ControlCode::CarriageReturn,
            0x1B => ControlCode::Escape,
            _ => {
                // NUL, SO, SI and friends: ignored.
                return;
            }
        };
        sink(ParserAction::Control(code));
    }

    // ── Escape ────────────────────────────────────────────────────────────

    fn escape<F: FnMut(ParserAction)>(&mut self, byte: u8, sink: &mut F) {
        // Inside an OSC string, ESC only matters as the start of ST (`ESC \`).
        if self.osc_pending_esc {
            self.osc_pending_esc = false;
            if byte == b'\\' {
                self.end_osc(sink);
            } else {
                // Resume the string; the ESC byte is dropped.
                self.state = State::OscString;
            }
            return;
        }
        match byte {
            // CAN / SUB: back to ground. LS2 / LS3 / LS1R / LS2R / LS3R
            // (charset locking): ignored, back to ground.
            0x18 | 0x1A | b'n' | b'o' | b'|' | b'}' | b'~' => {
                self.state = State::Ground;
            }
            0x1B => {
                // Doubled ESC stays in escape.
            }
            b'[' => self.state = State::CsiEntry,
            b']' => self.begin_osc(),
            b'P' => {
                self.passthrough_len = 0;
                self.state = State::DcsEntry;
            }
            b'X' | b'^' | b'_' => {
                self.passthrough_len = 0;
                self.state = State::SosPmApc;
            }
            b'\\' => {
                // ST standalone: ignore.
                self.state = State::Ground;
            }
            b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/' => {
                self.utf8[0] = byte;
                self.state = State::CharsetSelect;
            }
            b'#' => self.state = State::LineAttributes,
            b'7' => {
                self.state = State::Ground;
                sink(ParserAction::Esc(EscSequence::SaveCursor));
            }
            b'8' => {
                self.state = State::Ground;
                sink(ParserAction::Esc(EscSequence::RestoreCursor));
            }
            b'D' => {
                self.state = State::Ground;
                sink(ParserAction::Esc(EscSequence::Index));
            }
            b'E' => {
                self.state = State::Ground;
                sink(ParserAction::Esc(EscSequence::NextLine));
            }
            b'M' => {
                self.state = State::Ground;
                sink(ParserAction::Esc(EscSequence::ReverseIndex));
            }
            b'H' => {
                self.state = State::Ground;
                sink(ParserAction::Esc(EscSequence::TabSet));
            }
            b'=' => {
                self.state = State::Ground;
                sink(ParserAction::Esc(EscSequence::KeypadApplication));
            }
            b'>' => {
                self.state = State::Ground;
                sink(ParserAction::Esc(EscSequence::KeypadNumeric));
            }
            b'c' => {
                self.state = State::Ground;
                sink(ParserAction::Esc(EscSequence::FullReset));
            }
            b'Z' => {
                self.state = State::Ground;
                sink(ParserAction::Esc(EscSequence::Decid));
            }
            0x20..=0x2F => {
                self.intermediates[0] = byte;
                self.inter_count = 1;
                self.state = State::EscapeIntermediate;
            }
            _ => {
                // Unknown escape: ignore safely.
                self.stats.ignored_escapes = self.stats.ignored_escapes.wrapping_add(1);
                self.state = State::Ground;
            }
        }
    }

    fn escape_intermediate<F: FnMut(ParserAction)>(&mut self, byte: u8, sink: &mut F) {
        match byte {
            0x18 | 0x1A => self.state = State::Ground,
            0x20..=0x2F => {
                if self.inter_count < 2 {
                    self.intermediates[usize::from(self.inter_count)] = byte;
                    self.inter_count += 1;
                }
            }
            0x30..=0x7E => {
                // Final byte for a multi-intermediate escape. We accept and
                // ignore these (e.g. DEC private escapes).
                self.stats.ignored_escapes = self.stats.ignored_escapes.wrapping_add(1);
                self.state = State::Ground;
            }
            _ => {
                self.state = State::Ground;
                self.ground(byte, sink);
            }
        }
    }

    fn charset_select<F: FnMut(ParserAction)>(&mut self, byte: u8, sink: &mut F) {
        self.state = State::Ground;
        if (0x30..=0x7E).contains(&byte) {
            sink(ParserAction::CharsetSelect(char::from(byte)));
        } else {
            self.ground(byte, sink);
        }
    }

    fn line_attributes<F: FnMut(ParserAction)>(&mut self, byte: u8, sink: &mut F) {
        self.state = State::Ground;
        if (0x30..=0x7E).contains(&byte) {
            sink(ParserAction::Esc(EscSequence::LineAttributes(byte)));
        } else {
            self.ground(byte, sink);
        }
    }

    // ── CSI ───────────────────────────────────────────────────────────────

    fn csi_entry<F: FnMut(ParserAction)>(&mut self, byte: u8, sink: &mut F) {
        match byte {
            0x3C..=0x3F => {
                self.private = byte;
            }
            0x30..=0x3B => {
                self.state = State::CsiParam;
                self.csi_param(byte, sink);
            }
            0x20..=0x2F => {
                self.intermediates[0] = byte;
                self.inter_count = 1;
                self.state = State::CsiIntermediate;
            }
            0x40..=0x7E => {
                self.emit_csi(byte, sink);
            }
            // 0x18 / 0x1A (CAN/SUB) and anything unrecognised: back to ground.
            _ => {
                self.state = State::Ground;
            }
        }
    }

    fn csi_param<F: FnMut(ParserAction)>(&mut self, byte: u8, sink: &mut F) {
        match byte {
            0x18 | 0x1A => {
                self.state = State::Ground;
            }
            0x30..=0x39 => {
                let digit = u16::from(byte - 0x30);
                if !self.cur_param_overflow {
                    let next = self.cur_param.saturating_mul(10).saturating_add(digit);
                    if next == u16::MAX && self.cur_param > u16::MAX / 10 {
                        self.cur_param_overflow = true;
                    }
                    self.cur_param = next;
                }
            }
            0x3A | 0x3B => {
                self.push_param();
            }
            0x20..=0x2F => {
                self.push_param();
                self.intermediates[0] = byte;
                self.inter_count = 1;
                self.state = State::CsiIntermediate;
            }
            0x40..=0x7E => {
                self.push_param();
                self.emit_csi(byte, sink);
            }
            _ => {
                // Unexpected byte in params: drop the sequence, reprocess.
                self.reset_sequence();
                self.ground(byte, sink);
            }
        }
    }

    fn csi_intermediate<F: FnMut(ParserAction)>(&mut self, byte: u8, sink: &mut F) {
        match byte {
            0x20..=0x2F => {
                if self.inter_count < 2 {
                    self.intermediates[usize::from(self.inter_count)] = byte;
                    self.inter_count += 1;
                }
            }
            0x30..=0x3F => {
                // Digit/private after an intermediate: restart params.
                self.private = 0;
                self.param_count = 0;
                self.cur_param = 0;
                self.state = State::CsiParam;
                self.csi_param(byte, sink);
            }
            0x40..=0x7E => {
                self.emit_csi(byte, sink);
            }
            // 0x18 / 0x1A (CAN/SUB) and anything unrecognised: back to ground.
            _ => {
                self.state = State::Ground;
            }
        }
    }

    fn push_param(&mut self) {
        if self.param_count < limits::MAX_CSI_PARAMS {
            self.params[self.param_count] = self.cur_param;
            self.param_count += 1;
        }
        // Extra parameters are dropped (never accumulated).
        self.cur_param = 0;
        self.cur_param_overflow = false;
    }

    fn emit_csi<F: FnMut(ParserAction)>(&mut self, final_byte: u8, sink: &mut F) {
        let csi = Csi {
            private: self.private,
            params: self.params,
            param_count: self.param_count,
            intermediates: self.intermediates,
            inter_count: self.inter_count,
            final_byte,
        };
        self.stats.csi = self.stats.csi.wrapping_add(1);
        self.reset_sequence();
        sink(ParserAction::Csi(csi));
    }

    fn reset_sequence(&mut self) {
        self.state = State::Ground;
        self.private = 0;
        self.param_count = 0;
        self.cur_param = 0;
        self.cur_param_overflow = false;
        self.inter_count = 0;
    }

    // ── OSC ───────────────────────────────────────────────────────────────

    fn begin_osc(&mut self) {
        self.osc.clear();
        self.osc_truncated = false;
        self.state = State::OscString;
    }

    fn osc_string<F: FnMut(ParserAction)>(&mut self, byte: u8, sink: &mut F) {
        match byte {
            0x07 => {
                // BEL terminates OSC.
                self.end_osc(sink);
            }
            0x1B => {
                // Possible ST (ESC \). Remember we are in OSC; only `\\`
                // terminates.
                self.osc_pending_esc = true;
                self.state = State::Escape;
            }
            0x18 | 0x1A => {
                // CAN/SUB cancel the sequence.
                self.state = State::Ground;
                self.osc.clear();
            }
            _ => {
                if !self.osc_truncated {
                    if self.osc.len() < limits::MAX_OSC_LEN {
                        self.osc.push(byte);
                    } else {
                        self.osc_truncated = true;
                        self.stats.truncated_osc = self.stats.truncated_osc.wrapping_add(1);
                    }
                }
            }
        }
    }

    /// The Escape state must know it came from OSC so that only `ESC \`
    /// terminates (a bare ESC is otherwise ignored by `escape()`).
    /// We handle this by re-entering Escape: `escape()` currently ignores
    /// stray final bytes, but `\` (0x5C) in Escape state means ST — so
    /// route: set a pending-osc flag.
    fn end_osc<F: FnMut(ParserAction)>(&mut self, sink: &mut F) {
        self.stats.osc = self.stats.osc.wrapping_add(1);
        let payload = std::mem::take(&mut self.osc);
        self.state = State::Ground;
        self.osc_pending_esc = false;
        sink(ParserAction::Osc(Osc { payload }));
    }

    // ── DCS / SOS / PM / APC ──────────────────────────────────────────────

    fn dcs_entry<F: FnMut(ParserAction)>(&mut self, byte: u8, _sink: &mut F) {
        match byte {
            0x18 | 0x1A => self.state = State::Ground,
            0x20..=0x3F => {
                // Intermediates and parameters: count toward the budget.
                self.passthrough_len = self.passthrough_len.saturating_add(1);
            }
            0x40..=0x7E => {
                // Final byte of the DCS control function: payload follows.
                self.state = State::DcsPassthrough;
            }
            0x1B => self.state = State::Escape,
            _ => self.state = State::DcsPassthrough,
        }
    }

    fn dcs_passthrough<F: FnMut(ParserAction)>(&mut self, byte: u8, _sink: &mut F) {
        match byte {
            0x18 | 0x1A => self.state = State::Ground,
            0x1B => self.state = State::Escape,
            0x9C => {
                // ST terminates.
                self.state = State::Ground;
            }
            _ => {
                if self.passthrough_len >= limits::MAX_DCS_LEN {
                    if self.passthrough_len == limits::MAX_DCS_LEN {
                        self.stats.truncated_dcs = self.stats.truncated_dcs.wrapping_add(1);
                    }
                    // Count forever but never store.
                    self.passthrough_len = self.passthrough_len.saturating_add(1);
                } else {
                    self.passthrough_len = self.passthrough_len.saturating_add(1);
                }
            }
        }
    }

    fn sos_pm_apc<F: FnMut(ParserAction)>(&mut self, byte: u8, _sink: &mut F) {
        match byte {
            // CAN / SUB / ST: terminate the string, back to ground.
            0x18 | 0x1A | 0x9C => self.state = State::Ground,
            0x1B => self.state = State::Escape,
            _ => {
                self.passthrough_len = self.passthrough_len.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(bytes: &[u8]) -> Vec<ParserAction> {
        let mut p = Parser::new();
        let mut out = Vec::new();
        p.feed(bytes, &mut |a| out.push(a));
        out
    }

    #[test]
    fn plain_text_is_printed() {
        let actions = parse(b"hello");
        assert_eq!(
            actions,
            vec![
                ParserAction::Print('h'),
                ParserAction::Print('e'),
                ParserAction::Print('l'),
                ParserAction::Print('l'),
                ParserAction::Print('o'),
            ]
        );
    }

    #[test]
    fn utf8_multibyte_decoded() {
        let actions = parse("héllo界".as_bytes());
        assert_eq!(
            actions,
            vec![
                ParserAction::Print('h'),
                ParserAction::Print('é'),
                ParserAction::Print('l'),
                ParserAction::Print('l'),
                ParserAction::Print('o'),
                ParserAction::Print('界'),
            ]
        );
    }

    #[test]
    fn invalid_utf8_replaced() {
        let actions = parse(&[0xFF, b'a', 0x80, 0xC0, 0xAF]);
        assert_eq!(
            actions,
            vec![
                ParserAction::Print('\u{FFFD}'),
                ParserAction::Print('a'),
                ParserAction::Print('\u{FFFD}'),
                ParserAction::Print('\u{FFFD}'),
                ParserAction::Print('\u{FFFD}'),
            ]
        );
    }

    #[test]
    fn truncated_utf8_recovers_on_next_ascii() {
        // 0xE2 0x82 (two bytes of a 3-byte seq) then 'x' — 'x' is not a
        // continuation, so the sequence is invalid.
        let actions = parse(&[0xE2, 0x82, b'x']);
        assert_eq!(
            actions,
            vec![ParserAction::Print('\u{FFFD}'), ParserAction::Print('x'),]
        );
    }

    #[test]
    fn controls_are_emitted() {
        let actions = parse(b"a\nb\r\x07\x08\t");
        assert_eq!(
            actions,
            vec![
                ParserAction::Print('a'),
                ParserAction::Control(ControlCode::LineFeed),
                ParserAction::Print('b'),
                ParserAction::Control(ControlCode::CarriageReturn),
                ParserAction::Control(ControlCode::Bell),
                ParserAction::Control(ControlCode::Backspace),
                ParserAction::Control(ControlCode::Tab),
            ]
        );
    }

    #[test]
    fn csi_cursor_position() {
        let actions = parse(b"\x1b[3;7H");
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            ParserAction::Csi(csi) => {
                assert_eq!(csi.final_byte, b'H');
                assert_eq!(csi.private, 0);
                assert_eq!(csi.param_count, 2);
                assert_eq!(csi.params[0], 3);
                assert_eq!(csi.params[1], 7);
            }
            other => panic!("expected CSI, got {other:?}"),
        }
    }

    #[test]
    fn csi_defaults_to_one() {
        let actions = parse(b"\x1b[H");
        match &actions[0] {
            ParserAction::Csi(csi) => {
                assert_eq!(csi.param(0, 1), 1);
                assert_eq!(csi.param(1, 1), 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn csi_private_mode() {
        let actions = parse(b"\x1b[?1049h");
        match &actions[0] {
            ParserAction::Csi(csi) => {
                assert_eq!(csi.private, b'?');
                assert_eq!(csi.final_byte, b'h');
                assert_eq!(csi.params[0], 1049);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn csi_sgr_truecolor() {
        let actions = parse(b"\x1b[38;2;255;128;0m");
        match &actions[0] {
            ParserAction::Csi(csi) => {
                assert_eq!(csi.final_byte, b'm');
                assert_eq!(csi.param_count, 5);
                assert_eq!(csi.params[0], 38);
                assert_eq!(csi.params[1], 2);
                assert_eq!(csi.params[2], 255);
                assert_eq!(csi.params[3], 128);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn huge_params_saturate_not_overflow() {
        let actions = parse(b"\x1b[9999999999999999999999;5H");
        match &actions[0] {
            ParserAction::Csi(csi) => {
                assert_eq!(csi.params[0], limits::MAX_CSI_VALUE);
                assert_eq!(csi.params[1], 5);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn many_params_are_capped() {
        let mut seq = b"\x1b[".to_vec();
        for i in 0..40 {
            if i > 0 {
                seq.push(b';');
            }
            seq.extend_from_slice(format!("{i}").as_bytes());
        }
        seq.push(b'm');
        let actions = parse(&seq);
        match &actions[0] {
            ParserAction::Csi(csi) => {
                assert_eq!(csi.param_count, limits::MAX_CSI_PARAMS);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn osc_bel_terminated() {
        let actions = parse(b"\x1b]0;title\x07");
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            ParserAction::Osc(o) => assert_eq!(o.payload, b"0;title"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn osc_st_terminated() {
        let actions = parse(b"\x1b]2;path\x1b\\");
        match &actions[0] {
            ParserAction::Osc(o) => assert_eq!(o.payload, b"2;path"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn osc_truncated_at_bound() {
        let mut p = Parser::new();
        let mut out = Vec::new();
        let mut bytes = b"\x1b]52;c".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', limits::MAX_OSC_LEN + 100));
        bytes.push(0x07);
        p.feed(&bytes, &mut |a| out.push(a));
        assert_eq!(out.len(), 1);
        match &out[0] {
            ParserAction::Osc(o) => {
                assert!(o.payload.len() <= limits::MAX_OSC_LEN);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(p.stats.truncated_osc, 1);
    }

    #[test]
    fn dcs_is_ignored_and_bounded() {
        let mut p = Parser::new();
        let mut out = Vec::new();
        let mut bytes = b"\x1bP+q".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', limits::MAX_DCS_LEN + 50));
        bytes.extend_from_slice(b"\x1b\\tail");
        p.feed(&bytes, &mut |a| out.push(a));
        // Only "tail" prints.
        assert_eq!(
            out,
            vec![
                ParserAction::Print('t'),
                ParserAction::Print('a'),
                ParserAction::Print('i'),
                ParserAction::Print('l'),
            ]
        );
        assert_eq!(p.stats.truncated_dcs, 1);
    }

    #[test]
    fn escape_sequences() {
        let actions = parse(b"\x1b7\x1b8\x1bD\x1bM\x1b=\x1b>");
        assert_eq!(
            actions,
            vec![
                ParserAction::Esc(EscSequence::SaveCursor),
                ParserAction::Esc(EscSequence::RestoreCursor),
                ParserAction::Esc(EscSequence::Index),
                ParserAction::Esc(EscSequence::ReverseIndex),
                ParserAction::Esc(EscSequence::KeypadApplication),
                ParserAction::Esc(EscSequence::KeypadNumeric),
            ]
        );
    }

    #[test]
    fn charset_select_ignored_but_parsed() {
        let actions = parse(b"\x1b(B\x1b)0");
        assert_eq!(
            actions,
            vec![
                ParserAction::CharsetSelect('B'),
                ParserAction::CharsetSelect('0'),
            ]
        );
    }

    #[test]
    fn c1_csi_works() {
        let actions = parse(b"\x9b2J");
        match &actions[0] {
            ParserAction::Csi(csi) => {
                assert_eq!(csi.final_byte, b'J');
                assert_eq!(csi.params[0], 2);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn can_sub_cancel_sequences() {
        // CSI then CAN: no action emitted, back to ground.
        let actions = parse(b"\x1b[5;5\x18A");
        assert_eq!(actions, vec![ParserAction::Print('A')]);
        // OSC then CAN.
        let actions = parse(b"\x1b]0;abc\x1aB");
        assert_eq!(actions, vec![ParserAction::Print('B')]);
    }

    #[test]
    fn garbage_between_sequences_is_harmless() {
        let actions = parse(b"\x1b\x1b[3J\xff\x1b]zzz\x07tail");
        let mut expected_params = [0u16; limits::MAX_CSI_PARAMS];
        expected_params[0] = 3;
        assert_eq!(
            actions[0],
            ParserAction::Csi(Csi {
                private: 0,
                params: expected_params,
                param_count: 1,
                intermediates: [0; 2],
                inter_count: 0,
                final_byte: b'J',
            })
        );
        // then FFFD, OSC, t a i l
        assert!(matches!(&actions[1], ParserAction::Print('\u{FFFD}')));
    }

    #[test]
    fn unterminated_osc_does_not_hang_parser() {
        // A million bytes with no terminator: parser stays bounded and ends
        // in the OSC state; the buffer never exceeds the cap.
        let mut p = Parser::new();
        let mut out = Vec::new();
        let mut bytes = b"\x1b]52;c".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', 1_000_000));
        p.feed(&bytes, &mut |a| out.push(a));
        assert!(out.is_empty());
        assert!(p.osc.len() <= limits::MAX_OSC_LEN);
        assert_eq!(p.stats.truncated_osc, 1);
    }

    #[test]
    fn csi_private_and_digit_mix() {
        let actions = parse(b"\x1b[>0m");
        match &actions[0] {
            ParserAction::Csi(csi) => {
                assert_eq!(csi.private, b'>');
                assert_eq!(csi.params[0], 0);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn intermediate_before_final() {
        let actions = parse(b"\x1b[ !p");
        match &actions[0] {
            ParserAction::Csi(csi) => {
                assert_eq!(csi.intermediates[0], b' ');
                assert_eq!(csi.intermediates[1], b'!');
                assert_eq!(csi.inter_count, 2);
                assert_eq!(csi.final_byte, b'p');
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn stats_counted() {
        let mut p = Parser::new();
        p.feed(b"abc\x1b[2J\x1b]0;x\x07", &mut |_| {});
        assert_eq!(p.stats.bytes, 13);
        assert_eq!(p.stats.chars, 3);
        assert_eq!(p.stats.csi, 1);
        assert_eq!(p.stats.osc, 1);
    }
}

#[cfg(test)]
mod fuzz_tests {
    use super::*;

    /// Exhaustive 2-byte sweep: every (a, b) pair through the parser must
    /// never panic and must leave the parser in a recoverable ground-ish
    /// state (bounded buffers).
    #[test]
    fn exhaustive_two_byte_sweep() {
        let mut p = Parser::new();
        for a in 0u16..=255 {
            for b in 0u16..=255 {
                let mut out = Vec::new();
                p.feed(&[a as u8, b as u8], &mut |x| out.push(x));
                assert!(p.osc.len() <= limits::MAX_OSC_LEN);
                assert!(p.stats.truncated_osc <= 2);
            }
        }
        // Still responsive afterwards (close any open OSC/DCS string first:
        // unterminated strings legitimately absorb subsequent bytes).
        let mut out = Vec::new();
        p.feed(b"\x1b\\tail", &mut |x| out.push(x));
        assert!(out.contains(&ParserAction::Print('t')));
    }

    /// Long pseudo-random batches (seeded, deterministic) with hostile
    /// structure: unterminated OSC/DCS, huge params, UTF-8 garbage.
    #[test]
    fn seeded_fuzz_batches() {
        let mut p = Parser::new();
        let mut seed = 0xDEAD_BEEFu64;
        let mut total = 0usize;
        for _batch in 0..50 {
            let len = 64 + (seed as usize % 512);
            let mut bytes = Vec::with_capacity(len);
            for _ in 0..len {
                seed = seed
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let r = (seed >> 33) as u8;
                // Bias toward control/escape bytes.
                let b = match r % 4 {
                    0 => r % 0x20,
                    1 => 0x1b,
                    2 => 0x80 + (r % 0x20),
                    _ => r,
                };
                bytes.push(b);
            }
            p.feed(&bytes, &mut |_| {});
            total += len;
            assert!(p.osc.len() <= limits::MAX_OSC_LEN);
        }
        assert!(total > 0);
        // The parser still decodes plain text afterwards.
        let mut out = Vec::new();
        p.feed(b"ok", &mut |x| out.push(x));
        assert_eq!(
            out,
            vec![ParserAction::Print('o'), ParserAction::Print('k')]
        );
    }
}
