//! Terminal emulation modes — the state the key encoder and renderer query.
//!
//! Modes are set by escape sequences (DECSET/DECRST, SM/RM) emitted by the
//! child application. The key encoder must respect them: application cursor
//! keys, application keypad and bracketed paste change the byte stream Ferrokey
//! produces; cursor visibility and shape affect rendering; synchronized-output
//! mode (DECSET 2026) lets a TUI batch its redraws, which the renderer honours
//! to avoid tearing and needless work (§10, §20, §82 of the addendum).

/// The DECSCUSR cursor shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    /// Blinking block (DECSCUSR 1).
    BlinkingBlock,
    SteadyBlock,
    /// Blinking underline (DECSCUSR 3).
    BlinkingUnderline,
    SteadyUnderline,
    /// Blinking bar (DECSCUSR 5).
    BlinkingBar,
    SteadyBar,
}

impl CursorShape {
    pub const fn blinks(self) -> bool {
        matches!(
            self,
            CursorShape::BlinkingBlock | CursorShape::BlinkingUnderline | CursorShape::BlinkingBar
        )
    }
}

/// Mouse tracking mode (DECSET 1000/1002/1003/1006). Ferrokey does not emit
/// mouse events in the first milestone; the mode is tracked so the terminal's
/// touch interaction policy can consult it before deciding whether a drag is a
/// scroll or application input (§24).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseMode {
    #[default]
    None,
    X10,
    Normal,
    Highlight,
    Button,
    Any,
}

/// All emulation modes that matter to the encoder, renderer and interaction
/// policy.
///
/// Each bool mirrors one DEC/ANSI mode bit; a bitmask or enums would obscure
/// the one-to-one mode mapping the parser and encoder need.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalModes {
    /// DECCKM — application cursor keys (ESC O A vs ESC [ A).
    pub application_cursor_keys: bool,
    /// DECKPAM — application keypad (ESC O p vs plain digits).
    pub application_keypad: bool,
    /// DECSET 2004 — bracketed paste.
    pub bracketed_paste: bool,
    /// IRM — insert mode.
    pub insert_mode: bool,
    /// DECOM — origin mode (cursor relative to scroll region).
    pub origin_mode: bool,
    /// DECAWM — auto-wrap (default on).
    pub auto_wrap: bool,
    /// DECTCEM — cursor visible.
    pub cursor_visible: bool,
    /// Cursor blink on/off (DECSET 12).
    pub cursor_blink: bool,
    /// DECSCUSR cursor shape.
    pub cursor_shape: CursorShape,
    /// DECSCNM — reverse video (screen colours swap).
    pub reverse_video: bool,
    /// Mouse tracking.
    pub mouse_mode: MouseMode,
    /// DECSET 1004 — focus reporting. Not emitted by Ferrokey (no synthetic
    /// focus), but tracked so the policy can be explicit.
    pub focus_reporting: bool,
    /// DECSET 2026 — synchronized output. While set, the renderer defers
    /// pane updates until the reset arrives.
    pub synchronized_output: bool,
    /// DECSET 8 — modifyOtherKeys (1 = app sends modified keys; 2 = also
    /// unmodified). Tracked; the encoder currently emits the classic
    /// sequences (which remain correct in both modes).
    pub modify_other_keys: u8,
    /// Whether the child is currently in the alternate screen (set by the
    /// terminal, not by an escape).
    pub alt_screen: bool,
}

impl Default for TerminalModes {
    fn default() -> Self {
        TerminalModes {
            application_cursor_keys: false,
            application_keypad: false,
            bracketed_paste: false,
            insert_mode: false,
            origin_mode: false,
            auto_wrap: true,
            cursor_visible: true,
            cursor_blink: true,
            cursor_shape: CursorShape::Block,
            reverse_video: false,
            mouse_mode: MouseMode::None,
            focus_reporting: false,
            synchronized_output: false,
            modify_other_keys: 0,
            alt_screen: false,
        }
    }
}

/// Standard ECMA-48/DEC private modes Ferrokey understands (DECSET/DECRST
/// `CSI ? Pm h/l`). Unknown private modes are ignored safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateMode {
    /// 1 — application cursor keys.
    CursorKeys,
    /// 3 — 132-column mode. Accepted and ignored (window size is owned by
    /// the host; reflowing to 132 columns would fight the layout).
    Columns132,
    /// 5 — reverse video.
    ReverseVideo,
    /// 6 — origin mode.
    Origin,
    /// 7 — auto-wrap.
    AutoWrap,
    /// 8 — auto-repeat. Accepted; autorepeat is owned by ferrokey-core.
    AutoRepeat,
    /// 12 — cursor blink.
    CursorBlink,
    /// 25 — cursor visible.
    CursorVisible,
    /// 47 — alternate screen.
    AlternateScreen,
    /// 1000 — normal mouse tracking.
    MouseNormal,
    /// 1002 — button-event tracking.
    MouseButton,
    /// 1003 — any-event tracking.
    MouseAny,
    /// 1004 — focus reporting.
    FocusReporting,
    /// 1006 — SGR mouse. Accepted; Ferrokey sends no mouse events yet.
    SgrMouse,
    /// 1047 — alternate screen (with saved cursor).
    AlternateScreenSave,
    /// 1048 — save/restore cursor.
    SaveCursor,
    /// 1049 — alt screen + save/restore cursor (the vim/tmux idiom).
    AlternateScreenSaveCursor,
    /// 2004 — bracketed paste.
    BracketedPaste,
    /// 2026 — synchronized output.
    SynchronizedOutput,
}

impl PrivateMode {
    /// Resolve a DECSET/DECRST private mode number. Returns `None` for
    /// unknown-but-safe modes (the terminal ignores them, as xterm does).
    pub const fn from_code(code: u16) -> Option<Self> {
        Some(match code {
            1 => PrivateMode::CursorKeys,
            3 => PrivateMode::Columns132,
            5 => PrivateMode::ReverseVideo,
            6 => PrivateMode::Origin,
            7 => PrivateMode::AutoWrap,
            8 => PrivateMode::AutoRepeat,
            12 => PrivateMode::CursorBlink,
            25 => PrivateMode::CursorVisible,
            47 => PrivateMode::AlternateScreen,
            1000 => PrivateMode::MouseNormal,
            1002 => PrivateMode::MouseButton,
            1003 => PrivateMode::MouseAny,
            1004 => PrivateMode::FocusReporting,
            1006 => PrivateMode::SgrMouse,
            1047 => PrivateMode::AlternateScreenSave,
            1048 => PrivateMode::SaveCursor,
            1049 => PrivateMode::AlternateScreenSaveCursor,
            2004 => PrivateMode::BracketedPaste,
            2026 => PrivateMode::SynchronizedOutput,
            _ => return None,
        })
    }
}

/// Standard non-private modes (SM/RM `CSI Pm h/l`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnsiMode {
    /// 4 — insert mode.
    Insert,
    /// 20 — automatic newline (LNM). Accepted and ignored: Ferrokey always
    /// uses the standard LF semantics.
    Newline,
}

impl AnsiMode {
    pub const fn from_code(code: u16) -> Option<Self> {
        Some(match code {
            4 => AnsiMode::Insert,
            20 => AnsiMode::Newline,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_mode_lookup_round_trip() {
        for code in [
            1, 3, 5, 6, 7, 8, 12, 25, 47, 1000, 1002, 1003, 1004, 1006, 1047, 1048, 1049, 2004,
            2026,
        ] {
            let mode = PrivateMode::from_code(code).expect("known mode");
            assert_eq!(
                PrivateMode::from_code(code),
                Some(mode),
                "mode {code} round-trips"
            );
        }
        assert!(PrivateMode::from_code(9999).is_none());
        assert!(PrivateMode::from_code(0).is_none());
    }

    #[test]
    fn defaults_are_xterm_like() {
        let m = TerminalModes::default();
        assert!(m.auto_wrap);
        assert!(m.cursor_visible);
        assert!(!m.application_cursor_keys);
        assert!(!m.bracketed_paste);
        assert_eq!(m.mouse_mode, MouseMode::None);
    }
}
