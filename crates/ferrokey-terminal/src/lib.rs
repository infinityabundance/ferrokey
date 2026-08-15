//! # ferrokey-terminal
//!
//! The embedded PTY terminal engine behind Ferrokey's **terminal workspace
//! mode**: a real, self-contained terminal emulator that lives inside the
//! Ferrokey surface, above which the on-screen keyboard is always available.
//!
//! The mode is architecturally independent from the system-wide uinput path:
//!
//! ```text
//! OSK ──► ferrokey-core (same state machine as system mode)
//!              │
//!              ▼
//!       TerminalKeySink (this crate)
//!              │
//!              ▼
//!        key_encoder (PhysicalKey + modifiers + modes → bytes)
//!              │
//!              ▼
//!              PTY master ──► shell / TUI
//! ```
//!
//! There is **no** `/dev/uinput` involvement, no `ferrokeyd`, no compositor
//! focus dependency. The terminal stack is fully unprivileged and runs as the
//! desktop user.
//!
//! # Security posture
//!
//! * The PTY byte stream is treated as **hostile input**: the parser
//!   ([`parser`]) is a bounded state machine with strict buffer limits,
//!   saturating arithmetic and fail-safe recovery. Escape sequences can only
//!   affect the terminal model — never execute code, load files or touch the
//!   clipboard (§72–§75 of the terminal addendum).
//! * OSC sequences follow a conservative policy ([`terminal::Terminal`]):
//!   titles and sanitised hyperlinks are allowed; clipboard read/write and
//!   arbitrary actions are denied by default (§73–§74).
//! * Scrollback, parser buffers, selection state and paste input are all
//!   bounded (§78); output is processed in bounded chunks with coalesced
//!   redraws (§80–§81).
//! * The terminal child is spawned unprivileged as the desktop user; the
//!   child talks to Ferrokey only through PTY bytes (§106–§107).
//!
//! # Rendering
//!
//! [`render`] rasterises the visible pane with an embedded monospaced font
//! (DejaVu Sans Mono, Bitstream Vera licence — see
//! `assets/fonts/LICENSE-DejaVu.txt`) so rendering is deterministic on every
//! system, including the minimal VM courts. The pane is produced as an RGBA
//! buffer that the host application presents through its UI framework.

#![deny(unsafe_code)]

pub mod child;
pub mod clipboard;
pub mod grid;
pub mod key_encoder;
pub mod modes;
pub mod parser;
pub mod pty;
pub mod render;
pub mod scrollback;
pub mod selection;
pub mod shell;
pub mod sink;
pub mod terminal;
pub mod viewport;

mod syscall;

pub use child::{ChildExit, ChildHandle, ShellConfig};
pub use clipboard::{Clipboard, ClipboardError, ExternalClipboard, NoClipboard};
pub use grid::{Cell, CellFlags, Color, Grid, Pos};
pub use key_encoder::{EncodedInput, TerminalKeyEncoder};
pub use modes::{CursorShape, MouseMode, TerminalModes};
pub use parser::{ControlCode, Csi, EscSequence, Osc, Parser, ParserAction};
pub use pty::{PtyPair, Winsize};
pub use render::{
    CellMetrics, Palette, PaneRenderer, PaneView, RenderError, RenderedFrame, RendererConfig,
    UiButton, UiHitRects,
};
pub use scrollback::{Scrollback, ScrollbackError};
pub use selection::{expand_word, CellPos, Selection, SelectionMode};
pub use shell::{
    encode_sequence, shell_row, ProcReader, ProcTreeReader, ShellContext, ShellIdentitySource,
    ShellKind, ShellRowKey, BASH_ROW, FISH_ROW, GENERIC_ROW, NUSHELL_ROW, TMUX_ROW, ZSH_ROW,
};
pub use sink::{PtySink, TerminalInputSink, TerminalKeySink, TerminalSinkError};
pub use terminal::{PasteOutcome, Terminal, TerminalConfig, TerminalError, TerminalEvent};
pub use viewport::Viewport;

/// Bounds for every tunable in the terminal engine.
///
/// Configuration is input too (§25): every knob has a lower and upper limit,
/// enforced at construction. `18446744073709551615` scrollback lines are as
/// unacceptable here as they were in the broker.
pub mod limits {
    /// Minimum pane width, in cells.
    pub const MIN_COLS: u16 = 2;
    /// Maximum pane width, in cells.
    pub const MAX_COLS: u16 = 4096;
    /// Minimum pane height, in cells.
    pub const MIN_ROWS: u16 = 1;
    /// Maximum pane height, in cells.
    pub const MAX_ROWS: u16 = 4096;

    /// Minimum scrollback capacity (lines).
    pub const MIN_SCROLLBACK: usize = 100;
    /// Maximum scrollback capacity (lines).
    pub const MAX_SCROLLBACK: usize = 100_000;
    /// Default scrollback capacity (lines).
    pub const DEFAULT_SCROLLBACK: usize = 10_000;

    /// Minimum cell height in physical px.
    pub const MIN_CELL_PX: u32 = 8;
    /// Maximum cell height in physical px.
    pub const MAX_CELL_PX: u32 = 64;

    /// Minimum pane width in physical px.
    pub const MIN_PANE_PX: u32 = 64;
    /// Maximum paste size accepted in one call (bytes).
    pub const MAX_PASTE_BYTES: usize = 1 << 20; // 1 MiB

    /// Maximum OSC payload buffered before it is truncated (bytes).
    pub const MAX_OSC_LEN: usize = 4096;
    /// Maximum DCS/SOS/PM/APC payload counted before it is dropped (bytes).
    pub const MAX_DCS_LEN: usize = 4096;
    /// Maximum number of CSI parameters tracked (extras are ignored).
    pub const MAX_CSI_PARAMS: usize = 16;
    /// Maximum value a CSI parameter can reach before saturating.
    pub const MAX_CSI_VALUE: u16 = u16::MAX;
}
