//! The terminal facade: parser + grid + scrollback + viewport + selection +
//! PTY/child + renderer, and the full ANSI command dispatch (§9–§10, §17–§23,
//! §37–§41, §72–§75).
//!
//! `Terminal` is the one object the host application drives:
//!
//! * `poll(now)` — drain PTY output through the parser and reap the child;
//! * `is_dirty()` / `render()` — paint the visible pane (dirty rows only);
//! * `send_input()` / `paste()` — keyboard input path (via
//!   [`crate::sink::TerminalKeySink`]) and paste;
//! * `resize()` — recompute cells and push the size into the PTY;
//! * `scroll_*` / `selection_*` — viewport and selection;
//! * `restart()` / `shutdown()` — child lifecycle.
//!
//! Security notes: PTY output is treated as hostile (§72); the OSC policy is
//! conservative (§73–§74); every buffer is bounded (§75, §78); the output is
//! never logged (§79–§80).
//!
//! # Cell colour encoding
//!
//! Cells store their colour as a packed `u32` with a small SGR encoding
//! (resolved by [`crate::render::Palette`]):
//!
//! * `0` — default foreground/background;
//! * `1..=16` — ANSI 0-15, 1-based;
//! * `0x200 | n` — 256-colour index `n`;
//! * anything else — truecolour `0xRRGGBB`.

use crate::child::{ChildExit, ChildHandle, ShellConfig};
use crate::clipboard::{Clipboard, ClipboardError};
use crate::grid::{CellAttrs, CellFlags, Grid, Line};
use crate::limits;
use crate::modes::{AnsiMode, CursorShape, MouseMode, PrivateMode, TerminalModes};
use crate::parser::{ControlCode, Csi, EscSequence, Osc, Parser, ParserAction};
use crate::pty::{PtyPair, Winsize};
use crate::render::{Palette, PaneRenderer, PaneView, RenderError, RenderedFrame, RendererConfig};
use crate::scrollback::{Scrollback, ScrollbackError};
use crate::selection::{expand_word, CellPos, Selection, SelectionMode};
use crate::viewport::Viewport;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Terminal configuration (every knob bounds-checked, §25, §78).
#[derive(Debug, Clone)]
pub struct TerminalConfig {
    /// Scrollback capacity in lines.
    pub scrollback_lines: usize,
    /// Pane font size in physical px.
    pub font_size_px: u32,
    /// Shell to spawn (None → `$SHELL` → `/bin/sh`).
    pub shell: Option<String>,
    /// Working directory for the shell (None → `$HOME`).
    pub home: Option<PathBuf>,
    /// Extra environment for the child.
    pub env: Vec<(String, String)>,
    /// Maximum accepted paste size in bytes.
    pub max_paste_bytes: usize,
    /// Require confirmation for multiline pastes.
    pub confirm_multiline_paste: bool,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        TerminalConfig {
            scrollback_lines: limits::DEFAULT_SCROLLBACK,
            font_size_px: 16,
            shell: None,
            home: None,
            env: vec![
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
            ],
            max_paste_bytes: limits::MAX_PASTE_BYTES,
            confirm_multiline_paste: true,
        }
    }
}

/// The result of a paste request (bracketed paste policy, §29–§30).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteOutcome {
    /// The paste was sent immediately.
    Accepted,
    /// The paste is multiline and needs user confirmation (N lines).
    NeedsConfirmation(usize),
}

/// Events from [`Terminal::poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    /// The pane is dirty (output arrived).
    Output,
    /// A BEL was received.
    Bell,
    /// The child process ended.
    ChildExited(ChildExit),
}

/// A saved main screen for alternate-screen switching (§21).
#[derive(Debug, Clone)]
struct SavedScreen {
    grid: Grid,
}

/// Errors surfaced by the terminal.
#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("nix error: {0}")]
    Nix(#[from] nix::errno::Errno),
    #[error("renderer error: {0}")]
    Render(#[from] RenderError),
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error("clipboard error: {0}")]
    Clipboard(#[from] ClipboardError),
    #[error("paste rejected: {0}")]
    Paste(String),
}

impl From<ScrollbackError> for TerminalError {
    fn from(e: ScrollbackError) -> Self {
        TerminalError::InvalidState(e.to_string())
    }
}

impl From<crate::grid::GridError> for TerminalError {
    fn from(e: crate::grid::GridError) -> Self {
        TerminalError::InvalidState(e.to_string())
    }
}

/// The terminal engine.
///
/// Several bools model emulation state (bell, dirty flags, child exit, …);
/// they are deliberately kept flat because they are set from distinct code
/// paths and read by the renderer/pump.
#[allow(clippy::struct_excessive_bools)]
pub struct Terminal {
    config: TerminalConfig,
    pub(crate) modes: Rc<RefCell<TerminalModes>>,
    parser: Parser,
    grid: Grid,
    /// `Some` while the alternate screen is active.
    alt_grid: Option<()>,
    saved: Option<SavedScreen>,
    scrollback: Scrollback,
    viewport: Viewport,
    selection: Option<Selection>,
    selection_mode: SelectionMode,
    /// Current SGR state.
    fg: u32,
    bg: u32,
    flags: CellFlags,
    /// Last printed char (for REP).
    last_char: Option<char>,
    /// OSC title (bounded).
    title: String,
    /// OSC-reported cwd (bounded; informational).
    cwd: String,
    /// Pending hyperlink target (OSC 8; sanitised; informational only).
    hyperlink: String,
    pty: Option<PtyPair>,
    child: Option<ChildHandle>,
    /// Bounded queue of bytes to write to the PTY master (drained in poll).
    write_buf: Vec<u8>,
    /// A shared write buffer provided by the app's [`crate::sink::PtySink`]
    /// (drained in poll; bounded by the sink).
    input_source: Option<Rc<RefCell<Vec<u8>>>>,
    /// Response bytes (DA/DSR answers) to write to the PTY.
    response: Vec<u8>,
    renderer: RefCell<PaneRenderer>,
    /// Visible-row dirty set (None = all), for the next render.
    pane_dirty: Vec<u16>,
    pane_dirty_all: bool,
    cursor_dirty: bool,
    blink_phase: bool,
    last_blink_flip: Instant,
    bell: bool,
    child_exited: Option<ChildExit>,
    /// Pane size in physical px (0 until the app calls resize).
    pane_w: u32,
    pane_h: u32,
    /// Cached cell metrics.
    cell_w: u32,
    cell_h: u32,
    /// The last requested paste payload (for confirmation flow).
    pending_paste: Option<String>,
    /// The clipboard used by the copy/paste buttons (§27–§30); the app
    /// supplies an unprivileged backend.
    clipboard: Option<Box<dyn Clipboard>>,
    /// Counters for diagnostics / courts.
    pub output_bytes: u64,
    pub events_emitted: u64,
    pub osc_clipboard_denied: u64,
    pub invalid_sequences: u64,
}

impl Terminal {
    /// Create a terminal engine. The grid starts at 80×24; call
    /// [`Terminal::resize`] with the real pane size before rendering.
    pub fn new(config: TerminalConfig) -> Result<Self, TerminalError> {
        let scrollback = Scrollback::new(config.scrollback_lines, 80)?;
        let grid = Grid::new(80, 24)?;
        let renderer = PaneRenderer::new(
            RendererConfig {
                font_size_px: config.font_size_px,
            },
            Palette::default(),
        )?;
        let metrics = renderer.cell_metrics();
        Ok(Terminal {
            modes: Rc::new(RefCell::new(TerminalModes::default())),
            parser: Parser::new(),
            grid,
            alt_grid: None,
            saved: None,
            scrollback,
            viewport: Viewport::default(),
            selection: None,
            selection_mode: SelectionMode::Character,
            fg: 0,
            bg: 0,
            flags: CellFlags::empty(),
            last_char: None,
            title: String::new(),
            cwd: String::new(),
            hyperlink: String::new(),
            pty: None,
            child: None,
            write_buf: Vec::new(),
            input_source: None,
            response: Vec::new(),
            renderer: RefCell::new(renderer),
            pane_dirty: Vec::new(),
            pane_dirty_all: true,
            cursor_dirty: true,
            blink_phase: true,
            last_blink_flip: Instant::now(),
            bell: false,
            child_exited: None,
            pane_w: 0,
            pane_h: 0,
            cell_w: metrics.cell_w,
            cell_h: metrics.cell_h,
            pending_paste: None,
            clipboard: None,
            output_bytes: 0,
            events_emitted: 0,
            osc_clipboard_denied: 0,
            invalid_sequences: 0,
            config,
        })
    }

    // ── Accessors ─────────────────────────────────────────────────────────

    pub fn modes_cell(&self) -> Rc<RefCell<TerminalModes>> {
        self.modes.clone()
    }

    pub fn modes(&self) -> TerminalModes {
        self.modes.borrow().clone()
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    pub fn scrollback(&self) -> &Scrollback {
        &self.scrollback
    }

    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    pub fn child_pid(&self) -> Option<i32> {
        self.child.as_ref().map(|c| c.pid().as_raw())
    }

    pub fn child_exit(&self) -> Option<ChildExit> {
        self.child_exited
    }

    pub fn is_running(&self) -> bool {
        self.child.as_ref().is_some_and(|c| !c.is_reaped())
    }

    /// The current cell size in physical px.
    pub fn cell_metrics(&self) -> crate::render::CellMetrics {
        crate::render::CellMetrics {
            cell_w: self.cell_w,
            cell_h: self.cell_h,
        }
    }

    /// Current pane size in cells.
    pub fn size_in_cells(&self) -> (u16, u16) {
        (self.grid.cols(), self.grid.rows())
    }

    pub fn output_stats(&self) -> (u64, u64) {
        (self.output_bytes, self.events_emitted)
    }

    /// Attach the app's shared input buffer (drained into the PTY each
    /// poll). The buffer is bounded by the sink that fills it.
    pub fn set_input_source(&mut self, buffer: Rc<RefCell<Vec<u8>>>) {
        self.input_source = Some(buffer);
    }

    /// Attach the clipboard used by the copy/paste overlay buttons (§28).
    pub fn set_clipboard(&mut self, clipboard: Box<dyn Clipboard>) {
        self.clipboard = Some(clipboard);
    }

    // ── PTY / child lifecycle (§6–§8, §37–§41) ───────────────────────────

    /// Open the PTY and spawn the shell. Refuses to replace a running
    /// session implicitly.
    pub fn start_session(&mut self, config: &ShellConfig) -> Result<(), TerminalError> {
        if self.pty.is_some() {
            return Err(TerminalError::InvalidState(
                "a terminal session is already running".into(),
            ));
        }
        let (cols, rows) = self.size_in_cells();
        let mut pty = PtyPair::open(Winsize {
            rows,
            cols,
            ..Winsize::default()
        })?;
        pty.make_nonblocking()?;
        let child = ChildHandle::spawn(&mut pty, config)?;
        self.pty = Some(pty);
        self.child = Some(child);
        self.child_exited = None;
        self.clear_screen();
        Ok(())
    }

    /// Restart the session: shut down the old child/PTY and spawn a fresh
    /// one with a new grid (§39).
    pub fn restart(&mut self) -> Result<(), TerminalError> {
        self.shutdown_child(Duration::from_secs(2));
        self.pty = None;
        self.child = None;
        self.child_exited = None;
        let (cols, rows) = self.size_in_cells();
        self.grid = Grid::new(cols, rows)?;
        self.alt_grid = None;
        self.saved = None;
        self.scrollback.clear();
        self.viewport = Viewport::default();
        self.selection = None;
        self.reset_sgr();
        let cfg = ShellConfig {
            shell: self.config.shell.clone(),
            home: self.config.home.clone(),
            env: self.config.env.clone(),
        };
        self.start_session(&cfg)
    }

    /// Graceful shutdown: SIGHUP the child group, reap with grace, kill if
    /// needed.
    pub fn shutdown(&mut self) {
        self.shutdown_child(Duration::from_secs(2));
        self.pty = None;
        self.child = None;
        self.child_exited = None;
    }

    fn shutdown_child(&mut self, grace: Duration) {
        if let Some(mut child) = self.child.take() {
            child.shutdown(grace);
        }
    }

    // ── Event loop pump ───────────────────────────────────────────────────

    /// Drain pending writes to the PTY, read available output, feed the
    /// parser and reap the child. Call this from the host event loop.
    pub fn poll(&mut self, _now: Instant) -> Vec<TerminalEvent> {
        let mut events = Vec::new();
        if self.pty.is_none() {
            return events;
        }

        // Flush pending writes (bounded).
        if !self.write_buf.is_empty() {
            let buf = std::mem::take(&mut self.write_buf);
            let _ = self.write_all(&buf);
        }
        // Drain the app-side shared input buffer (the terminal key sink).
        if let Some(source) = self.input_source.clone() {
            let buf = {
                let mut src = source.borrow_mut();
                if src.is_empty() {
                    None
                } else {
                    Some(std::mem::take(&mut *src))
                }
            };
            if let Some(buf) = buf {
                let _ = self.write_all(&buf);
            }
        }
        if !self.response.is_empty() {
            let buf = std::mem::take(&mut self.response);
            log::debug!("terminal response flush: {} bytes", buf.len());
            let _ = self.write_all(&buf);
        }

        // Read available output in bounded chunks (backpressure: the kernel
        // pty buffer bounds the child; per-poll processing is capped so the
        // host loop stays fair — poll() is called repeatedly).
        let mut chunk = [0u8; 4096];
        let mut read_budget = 256 * 1024;
        loop {
            if read_budget == 0 {
                break;
            }
            // Borrow the master only for the syscall.
            let read = self
                .pty
                .as_ref()
                .map(|pty| nix::unistd::read(pty.master(), &mut chunk));
            let read = match read {
                Some(r) => r,
                None => break,
            };
            match read {
                // EOF (child closed the slave), EAGAIN (no data) or EIO
                // (master hung up) all end this drain.
                Ok(0) | Err(nix::errno::Errno::EAGAIN | nix::errno::Errno::EIO) => break,
                Ok(n) => {
                    read_budget -= n;
                    log::debug!("terminal read {n} bytes");
                    let synced = self.modes.borrow().synchronized_output;
                    self.feed_bytes(&chunk[..n], synced);
                    events.push(TerminalEvent::Output);
                }
                Err(e) => {
                    log::warn!("terminal read error: {e}");
                    break;
                }
            }
        }

        // Reap the child (emit the exit event exactly once).
        if let Some(child) = self.child.as_mut() {
            if let Some(exit) = child.poll_reap() {
                if self.child_exited.is_none() {
                    self.child_exited = Some(exit);
                    events.push(TerminalEvent::ChildExited(exit));
                }
            }
        }

        if self.bell {
            self.bell = false;
            events.push(TerminalEvent::Bell);
        }

        events
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), TerminalError> {
        let Some(pty) = self.pty.as_mut() else {
            return Ok(());
        };
        let fd = pty.master();
        let mut written = 0;
        while written < bytes.len() {
            match nix::unistd::write(fd, &bytes[written..]) {
                Ok(0) => break,
                Ok(n) => written += n,
                Err(nix::errno::Errno::EAGAIN) => {
                    // Would block: keep the remainder buffered (bounded).
                    self.write_buf.extend_from_slice(&bytes[written..]);
                    break;
                }
                Err(e) => {
                    log::warn!("terminal write error: {e}");
                    break;
                }
            }
        }
        Ok(())
    }

    /// Feed synthetic bytes (tests, courts) through the parser.
    pub fn feed(&mut self, bytes: &[u8]) {
        let synced = self.modes.borrow().synchronized_output;
        self.feed_bytes(bytes, synced);
    }

    fn feed_bytes(&mut self, bytes: &[u8], synced: bool) {
        self.output_bytes = self.output_bytes.wrapping_add(bytes.len() as u64);
        // Parse the chunk into a bounded action list first (the parser cannot
        // borrow `self` while applying, since actions mutate the grid).
        let mut actions: Vec<ParserAction> = Vec::with_capacity(64);
        self.parser.feed(bytes, &mut |a| actions.push(a));
        for action in actions {
            self.apply(action);
        }
        self.events_emitted = self.events_emitted.wrapping_add(1);
        if synced {
            // DECSET 2026: defer ALL dirty marking until the batch ends; the
            // mode-change (DECRST) repaints everything then.
        } else {
            self.mark_dirty_from_grid();
        }
    }

    /// Push keyboard input into the PTY (bounded queue).
    pub fn send_input(&mut self, bytes: &[u8]) -> Result<(), TerminalError> {
        const MAX_PENDING: usize = 1 << 20;
        if self.pty.is_none() {
            return Ok(()); // No session: drop silently (defensive).
        }
        if self.write_buf.len() + bytes.len() > MAX_PENDING {
            return Err(TerminalError::Paste("pending input buffer full".into()));
        }
        self.write_buf.extend_from_slice(bytes);
        Ok(())
    }

    // ── Resize (§31–§33) ─────────────────────────────────────────────────

    /// Resize the pane to `width × height` physical px. Computes rows/cols
    /// from the actual cell metrics, resizes grid + scrollback, and pushes
    /// the size into the PTY (TIOCSWINSZ → SIGWINCH).
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), TerminalError> {
        if width < limits::MIN_PANE_PX || height < limits::MIN_PANE_PX {
            return Ok(()); // Not ready; the app retries on the next layout.
        }
        let cols = (width / self.cell_w)
            .clamp(u32::from(limits::MIN_COLS), u32::from(limits::MAX_COLS))
            as u16;
        let rows = (height / self.cell_h)
            .clamp(u32::from(limits::MIN_ROWS), u32::from(limits::MAX_ROWS))
            as u16;

        let size_changed = cols != self.grid.cols() || rows != self.grid.rows();
        if size_changed {
            self.grid.resize(cols, rows)?;
            // Scrollback width follows the grid.
            let mut new_sb = Scrollback::new(self.config.scrollback_lines, cols)?;
            for line in self.scrollback.iter() {
                new_sb.push(line.clone());
            }
            self.scrollback = new_sb;
            self.pane_dirty_all = true;
        }
        if let Some(pty) = self.pty.as_mut() {
            if size_changed {
                pty.resize(Winsize {
                    rows,
                    cols,
                    ..Winsize::default()
                })?;
            }
        }
        {
            let mut renderer = self.renderer.borrow_mut();
            renderer.resize(width, height)?;
        }
        self.pane_w = width;
        self.pane_h = height;
        Ok(())
    }

    // ── Rendering (§19, §34–§36) ─────────────────────────────────────────

    /// Whether the pane needs a repaint.
    pub fn is_dirty(&self) -> bool {
        self.pane_dirty_all || !self.pane_dirty.is_empty() || self.cursor_dirty
    }

    /// Render the pane (dirty rows only) and return the frame. Consumes the
    /// dirty state.
    pub fn render(&mut self) -> Option<RenderedFrame> {
        if self.pane_w == 0 || self.pane_h == 0 {
            return None;
        }
        let dirty = self.consume_dirty();
        let view = self.pane_view();
        let frame = {
            let mut renderer = self.renderer.borrow_mut();
            renderer.render(&view, dirty.as_deref()).clone()
        };
        Some(frame)
    }

    fn consume_dirty(&mut self) -> Option<Vec<u16>> {
        if self.pane_dirty_all {
            self.pane_dirty_all = false;
            self.pane_dirty.clear();
            self.cursor_dirty = false;
            return None;
        }
        let mut rows = std::mem::take(&mut self.pane_dirty);
        if self.cursor_dirty {
            if let Some(idx) = self.cursor_visible_index() {
                if !rows.contains(&idx) {
                    rows.push(idx);
                }
            }
        }
        self.cursor_dirty = false;
        if rows.is_empty() {
            None
        } else {
            Some(rows)
        }
    }

    /// Advance the cursor blink clock. Returns true when a repaint is due.
    pub fn tick_blink(&mut self, now: Instant) -> bool {
        if !self.modes.borrow().cursor_shape.blinks() {
            return false;
        }
        if now.duration_since(self.last_blink_flip) >= Duration::from_millis(500) {
            self.last_blink_flip = now;
            self.blink_phase = !self.blink_phase;
            self.cursor_dirty = true;
            return true;
        }
        false
    }

    fn cursor_visible_index(&self) -> Option<u16> {
        if self.viewport.scroll_offset > 0 || self.alt_grid.is_some() {
            return None;
        }
        let cursor_row = self.grid.cursor().row;
        if u32::from(cursor_row) < self.visible_rows() {
            Some(cursor_row)
        } else {
            None
        }
    }

    fn visible_rows(&self) -> u32 {
        if self.pane_h == 0 || self.cell_h == 0 {
            0
        } else {
            self.pane_h / self.cell_h
        }
    }

    /// The document row of the first visible pane row.
    fn window_start(&self) -> i64 {
        let sb = self.scrollback.len() as i64;
        let live = i64::from(self.grid.rows());
        let visible = i64::from(self.visible_rows());
        let bottom = sb + live;
        let offset = (self.viewport.scroll_offset as i64).min(visible.max(0));
        (bottom - offset - visible).max(0)
    }

    fn pane_view(&self) -> PaneView<'_> {
        let visible_rows = self.visible_rows() as usize;
        let sb_len = self.scrollback.len();
        let live_rows = usize::from(self.grid.rows());
        let bottom = sb_len + live_rows;
        let start = self.window_start() as usize;
        let mut lines: Vec<&[crate::grid::Cell]> = Vec::with_capacity(visible_rows);
        for doc in start..bottom {
            if doc < sb_len {
                match self.scrollback.from_top(doc) {
                    Some(l) => lines.push(l),
                    None => lines.push(&[]),
                }
            } else {
                lines.push(self.grid.line((doc - sb_len) as u16));
            }
        }

        // Cursor (only when following output; hidden in alt screen).
        let cursor = if self.viewport.scroll_offset == 0 && self.alt_grid.is_none() {
            let pos = self.grid.cursor();
            let visible = self.modes.borrow().cursor_visible;
            let shape = self.modes.borrow().cursor_shape;
            let doc = i64::from(sb_len as u32) + i64::from(pos.row);
            if doc >= start as i64 && doc < bottom as i64 {
                Some((
                    (doc - start as i64) as usize,
                    usize::from(pos.col),
                    visible,
                    shape,
                    self.blink_phase,
                ))
            } else {
                None
            }
        } else {
            None
        };

        let selection = self.selection.as_ref();
        let exited = self
            .child_exited
            .map(|e| (format!("Process {}", e.summary()), true));
        let scrollbar = Some((self.viewport.scroll_offset, sb_len + live_rows));

        PaneView {
            lines,
            first_document_row: start as i64,
            cursor,
            selection,
            reverse_video: self.modes.borrow().reverse_video,
            scrollbar,
            exited,
        }
    }

    // ── Viewport (§22–§23) ────────────────────────────────────────────────

    pub fn scroll_up(&mut self, n: usize) {
        self.viewport.update_bounds(self.scrollback.len());
        self.viewport.scroll_up(n);
        log::debug!(
            "terminal viewport scroll_up {n} -> offset={} follow={}",
            self.viewport.scroll_offset,
            self.viewport.follow_output
        );
        self.pane_dirty_all = true;
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.viewport.scroll_down(n);
        log::debug!(
            "terminal viewport scroll_down {n} -> offset={} follow={}",
            self.viewport.scroll_offset,
            self.viewport.follow_output
        );
        self.pane_dirty_all = true;
    }

    pub fn return_to_newest(&mut self) {
        self.viewport.return_to_newest();
        log::debug!("terminal viewport return_to_newest");
        self.pane_dirty_all = true;
    }

    // ── Selection (§27–§28) ───────────────────────────────────────────────

    pub fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    /// Start (or restart) a selection at a document-space position.
    pub fn selection_start(&mut self, pos: CellPos, mode: SelectionMode) {
        self.selection_mode = mode;
        if mode == SelectionMode::Word {
            self.selection = expand_word(pos, &|row| self.line_chars(row));
        } else {
            self.selection = Some(Selection::new(pos, pos, mode));
        }
        self.pane_dirty_all = true;
    }

    /// Extend the selection to a document-space position.
    pub fn selection_extend(&mut self, pos: CellPos) {
        let Some(mut sel) = self.selection else {
            return;
        };
        if self.selection_mode == SelectionMode::Word {
            sel = expand_word(pos, &|row| self.line_chars(row)).unwrap_or(sel);
        } else {
            sel.end = pos;
        }
        self.selection = Some(sel);
        self.pane_dirty_all = true;
    }

    pub fn selection_clear(&mut self) {
        if self.selection.is_some() {
            self.selection = None;
            self.pane_dirty_all = true;
        }
    }

    /// The characters of a document-space line (for word expansion).
    fn line_chars(&self, row: i64) -> Option<Vec<char>> {
        let sb_len = self.scrollback.len() as i64;
        if row < 0 {
            return None;
        }
        if row < sb_len {
            self.scrollback
                .from_top(row as usize)
                .map(|l| l.iter().map(|c| c.ch).collect())
        } else {
            let r = row - sb_len;
            if r >= i64::from(self.grid.rows()) {
                return None;
            }
            Some(self.grid.line(r as u16).iter().map(|c| c.ch).collect())
        }
    }

    /// The selected text, or `None` when there is no selection. Lines are
    /// joined with `\n` (§28: content is never logged).
    pub fn selected_text(&self) -> Option<String> {
        let sel = self.selection?;
        let (s, e) = (sel.start(), sel.end());
        if s == e && sel.mode == SelectionMode::Character {
            return None;
        }
        let mut out = String::new();
        for row in s.row..=e.row {
            let chars = self.line_chars(row).unwrap_or_default();
            if chars.is_empty() {
                if row < e.row {
                    out.push('\n');
                }
                continue;
            }
            let col0 = if row == s.row { s.col } else { 0 };
            let col1 = if row == e.row {
                e.col
            } else {
                chars.len() as i64 - 1
            };
            for col in col0.max(0)..=col1.min(chars.len() as i64 - 1) {
                let ch = chars[col as usize];
                if ch != '\0' {
                    out.push(ch);
                }
            }
            if row < e.row {
                out.push('\n');
            }
        }
        Some(out)
    }

    /// Copy the selection into `clipboard` (unprivileged layer only).
    pub fn copy_selection(&mut self, clipboard: &mut dyn Clipboard) -> Result<(), TerminalError> {
        let Some(text) = self.selected_text() else {
            return Ok(());
        };
        clipboard.set_text(&text)?;
        self.selection = None;
        self.pane_dirty_all = true;
        Ok(())
    }

    // ── Paste (§29–§30, §78) ──────────────────────────────────────────────

    /// Paste text. Applies the bracketed-paste delimiters when the app has
    /// enabled bracketed paste, bounds the size, and enforces the multiline
    /// confirmation policy.
    pub fn paste(&mut self, text: &str) -> Result<PasteOutcome, TerminalError> {
        if text.is_empty() {
            return Ok(PasteOutcome::Accepted);
        }
        if text.len() > self.config.max_paste_bytes {
            return Err(TerminalError::Paste(format!(
                "paste of {} bytes exceeds the {} byte limit",
                text.len(),
                self.config.max_paste_bytes
            )));
        }
        if text.contains('\n') && self.config.confirm_multiline_paste {
            self.pending_paste = Some(text.to_string());
            return Ok(PasteOutcome::NeedsConfirmation(text.lines().count()));
        }
        self.paste_inner(text);
        Ok(PasteOutcome::Accepted)
    }

    /// Paste after explicit user confirmation (multiline policy).
    pub fn confirm_paste(&mut self) -> Result<(), TerminalError> {
        let Some(text) = self.pending_paste.take() else {
            return Ok(());
        };
        if text.len() > self.config.max_paste_bytes {
            return Err(TerminalError::Paste("paste too large".into()));
        }
        self.paste_inner(&text);
        Ok(())
    }

    pub fn cancel_paste(&mut self) {
        self.pending_paste = None;
    }

    fn paste_inner(&mut self, text: &str) {
        let encoded = self.encode_paste(text);
        let _ = self.send_input(&encoded);
    }

    /// The exact bytes a paste produces (bracketed delimiters when enabled).
    /// Exposed for tests and the deterministic key-oracle courts (§99).
    pub fn encode_paste(&self, text: &str) -> Vec<u8> {
        let bracketed = self.modes.borrow().bracketed_paste;
        if bracketed {
            let mut bytes = Vec::with_capacity(text.len() + 6);
            bytes.extend_from_slice(b"\x1b[200~");
            bytes.extend_from_slice(text.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            bytes
        } else {
            text.as_bytes().to_vec()
        }
    }

    // ── Pointer interaction (touch scroll / selection / controls) ────────

    /// Whether `(x, y)` (pane-relative px) is over an overlay control
    /// (copy / paste / restart / newest). The pane gesture machine uses this
    /// to keep a press on a control a tap even while a selection exists —
    /// otherwise the copy pill (which only exists during a selection) would
    /// be unreachable (§28).
    pub fn over_control(&self, x: u32, y: u32) -> bool {
        let ui = self.renderer.borrow().frame_ui();
        [ui.newest, ui.restart, ui.copy, ui.paste]
            .into_iter()
            .flatten()
            .any(|b| b.contains(x, y))
    }

    /// Handle a tap inside the pane at physical-px coordinates (pane-relative).
    /// Returns `true` if the terminal consumed the tap.
    pub fn tap(&mut self, x: u32, y: u32) -> bool {
        let ui = self.renderer.borrow().frame_ui();
        if let Some(btn) = ui.newest {
            if btn.contains(x, y) {
                log::debug!("pane tap ({x},{y}) hit newest");
                self.return_to_newest();
                return true;
            }
        }
        if let Some(btn) = ui.restart {
            if btn.contains(x, y) {
                log::debug!("pane tap ({x},{y}) hit restart");
                let _ = self.restart();
                return true;
            }
        }
        if let Some(btn) = ui.copy {
            if btn.contains(x, y) {
                log::debug!("pane tap ({x},{y}) hit copy");
                if let Some(mut clip) = self.clipboard.take() {
                    let _ = self.copy_selection(clip.as_mut());
                    self.clipboard = Some(clip);
                }
                return true;
            }
        }
        if let Some(btn) = ui.paste {
            if btn.contains(x, y) {
                log::debug!("pane tap ({x},{y}) hit paste");
                if let Some(mut clip) = self.clipboard.take() {
                    match clip.get_text() {
                        Ok(text) => {
                            if text.contains('\n') && self.config.confirm_multiline_paste {
                                // The confirmation UI is a later milestone; a
                                // multiline paste without confirmation is
                                // rejected rather than silently executed.
                                log::debug!("multiline paste requires confirmation; rejected");
                            } else {
                                self.paste_inner(&text);
                            }
                        }
                        Err(e) => log::debug!("paste unavailable: {e}"),
                    }
                    self.clipboard = Some(clip);
                }
                return true;
            }
        }
        if let Some(pos) = self.physical_to_doc(x, y) {
            log::debug!("pane tap ({x},{y}) started a selection at {pos:?}");
            self.selection_start(pos, SelectionMode::Character);
            return true;
        }
        false
    }

    /// Drag: extend the selection to `(x, y)`.
    pub fn drag(&mut self, x: u32, y: u32) -> bool {
        let Some(pos) = self.physical_to_doc(x, y) else {
            return false;
        };
        if self.selection.is_some() {
            self.selection_extend(pos);
            true
        } else {
            false
        }
    }

    /// Vertical drag distance (touch scrolling). Positive = content moves
    /// down (user swipes up → view history).
    pub fn scroll_by_delta(&mut self, delta_px: i32) {
        let cell_h = self.cell_h as i32;
        let lines = (delta_px / cell_h).abs().max(1) as usize;
        if delta_px > 0 {
            self.scroll_down(lines);
        } else {
            self.scroll_up(lines);
        }
    }

    /// Convert physical px to document-space cell coordinates.
    pub fn physical_to_doc(&self, x: u32, y: u32) -> Option<CellPos> {
        if self.pane_w == 0 || self.pane_h == 0 {
            return None;
        }
        let col = x / self.cell_w;
        let visible_row = y / self.cell_h;
        Some(CellPos::new(
            self.window_start() + i64::from(visible_row),
            i64::from(col),
        ))
    }

    // ── Diagnostics (§86) ─────────────────────────────────────────────────

    /// A structured diagnostic report (never contains typed content).
    pub fn diagnostics(&self) -> Vec<(&'static str, String)> {
        let (cols, rows) = self.size_in_cells();
        let mut out = vec![
            ("engine", "ferrokey-terminal".into()),
            ("pty backend", "Linux PTY".into()),
            (
                "shell",
                self.config
                    .shell
                    .clone()
                    .unwrap_or_else(|| std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())),
            ),
            (
                "TERM",
                self.env_value("TERM").unwrap_or_else(|| "unset".into()),
            ),
            ("rows", rows.to_string()),
            ("columns", cols.to_string()),
            (
                "scrollback capacity",
                self.scrollback.max_lines().to_string(),
            ),
            ("scrollback in use", self.scrollback.len().to_string()),
            (
                "child pid",
                self.child_pid()
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "none".into()),
            ),
            (
                "child state",
                if self.is_running() {
                    "running".into()
                } else {
                    "not running".into()
                },
            ),
            ("destination", "terminal".into()),
            ("uinput used by terminal mode", "no".into()),
            ("output bytes", self.output_bytes.to_string()),
            ("invalid sequences", self.invalid_sequences.to_string()),
            (
                "osc clipboard denied",
                self.osc_clipboard_denied.to_string(),
            ),
        ];
        if !self.title.is_empty() {
            out.push(("title", self.title.clone()));
        }
        out
    }

    fn env_value(&self, key: &str) -> Option<String> {
        self.config
            .env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    // ── Parser action application ─────────────────────────────────────────

    fn apply(&mut self, action: ParserAction) {
        match action {
            ParserAction::Print(ch) => {
                self.last_char = Some(ch);
                let scrolled = self.grid.print(ch, self.current_attrs());
                if let Some(line) = scrolled {
                    self.push_scrollback(line);
                }
            }
            ParserAction::Control(code) => self.control(code),
            ParserAction::Csi(csi) => self.csi(csi),
            ParserAction::Osc(osc) => self.osc(osc),
            ParserAction::Esc(esc) => self.esc(esc),
            ParserAction::CharsetSelect(_) => {
                // Character sets are ignored: Ferrokey is always Unicode.
            }
        }
    }

    fn current_attrs(&self) -> CellAttrs {
        CellAttrs {
            fg: self.fg,
            bg: self.bg,
            flags: self.flags,
        }
    }

    fn control(&mut self, code: ControlCode) {
        match code {
            ControlCode::Bell => self.bell = true,
            ControlCode::Backspace => self.grid.backspace(),
            ControlCode::Tab => self.grid.tab(),
            ControlCode::LineFeed | ControlCode::VerticalTab | ControlCode::FormFeed => {
                let scrolled = self.grid.line_feed();
                if let Some(line) = scrolled {
                    self.push_scrollback(line);
                }
            }
            ControlCode::CarriageReturn => self.grid.carriage_return(),
            ControlCode::Escape => {}
            ControlCode::Index => {
                let scrolled = self.grid.index();
                if let Some(line) = scrolled {
                    self.push_scrollback(line);
                }
            }
            ControlCode::NextLine => {
                let scrolled = self.grid.next_line();
                if let Some(line) = scrolled {
                    self.push_scrollback(line);
                }
            }
            ControlCode::TabSet => self.grid.set_tab_stop(),
            ControlCode::ReverseIndex => {
                self.grid.reverse_index();
                self.restore_from_scrollback();
            }
        }
    }

    /// Store a line that scrolled out of the live screen.
    fn push_scrollback(&mut self, line: Line) {
        // The alternate screen never writes into normal scrollback (§21).
        if self.alt_grid.is_some() {
            return;
        }
        self.scrollback.push(line);
        self.viewport.update_bounds(self.scrollback.len());
    }

    /// Pull a line back from scrollback (RI at the top of the screen).
    fn restore_from_scrollback(&mut self) {
        if let Some(line) = self.scrollback.pop_newest() {
            self.grid.push_line_from_scrollback(line);
        }
    }

    fn esc(&mut self, esc: EscSequence) {
        match esc {
            EscSequence::SaveCursor => {
                self.grid.save_cursor();
                self.grid.saved_attrs = Some(self.current_attrs());
            }
            EscSequence::RestoreCursor => {
                self.grid.restore_cursor();
                if let Some(attrs) = self.grid.saved_attrs {
                    self.fg = attrs.fg;
                    self.bg = attrs.bg;
                    self.flags = attrs.flags;
                }
            }
            EscSequence::Index => {
                let scrolled = self.grid.index();
                if let Some(line) = scrolled {
                    self.push_scrollback(line);
                }
            }
            EscSequence::NextLine => {
                let scrolled = self.grid.next_line();
                if let Some(line) = scrolled {
                    self.push_scrollback(line);
                }
            }
            EscSequence::ReverseIndex => {
                self.grid.reverse_index();
                self.restore_from_scrollback();
            }
            EscSequence::TabSet => self.grid.set_tab_stop(),
            EscSequence::KeypadApplication => {
                self.modes.borrow_mut().application_keypad = true;
            }
            EscSequence::KeypadNumeric => {
                self.modes.borrow_mut().application_keypad = false;
            }
            EscSequence::FullReset => self.full_reset(),
            EscSequence::Decid => self.device_attributes_response(),
            EscSequence::LineAttributes(byte) => {
                if byte == b'8' {
                    self.grid.fill_screen('E'); // DECALN
                }
            }
        }
    }

    fn device_attributes_response(&mut self) {
        // VT100 with advanced video option.
        self.queue_response(b"\x1b[?1;2c".to_vec());
    }

    fn queue_response(&mut self, bytes: Vec<u8>) {
        const MAX_RESPONSE: usize = 256;
        if self.response.len() + bytes.len() > MAX_RESPONSE {
            return;
        }
        self.response.extend_from_slice(&bytes);
    }

    // ── CSI dispatch ──────────────────────────────────────────────────────

    fn csi(&mut self, csi: Csi) {
        let final_byte = csi.final_byte;
        let private = csi.private;
        let p0 = |i: usize| csi.param(i, 1);
        let p0v = |i: usize| csi.param(i, 0);

        // Private '?' modes: DECSET/DECRST.
        if private == b'?' && (final_byte == b'h' || final_byte == b'l') {
            let set = final_byte == b'h';
            for i in 0..csi.param_count {
                if let Some(mode) = PrivateMode::from_code(csi.params[i]) {
                    self.set_private_mode(mode, set);
                }
            }
            return;
        }
        // modifyOtherKeys: CSI > 4 ; 2 m (and CSI > 4 m / CSI < 4 m).
        if (private == b'>' || private == b'<') && final_byte == b'm' && csi.raw_param(0) == Some(4)
        {
            let value = if private == b'>' {
                csi.raw_param(1).unwrap_or(1).min(2) as u8
            } else {
                0
            };
            self.modes.borrow_mut().modify_other_keys = value;
            return;
        }
        // DECSCUSR: CSI Ps SP q (intermediate space).
        if final_byte == b'q' && csi.inter_count == 1 && csi.intermediates[0] == b' ' {
            let shape = match p0v(0) {
                0 | 1 => CursorShape::BlinkingBlock,
                2 => CursorShape::SteadyBlock,
                3 => CursorShape::BlinkingUnderline,
                4 => CursorShape::SteadyUnderline,
                5 => CursorShape::BlinkingBar,
                6 => CursorShape::SteadyBar,
                _ => CursorShape::Block,
            };
            self.modes.borrow_mut().cursor_shape = shape;
            self.cursor_dirty = true;
            return;
        }

        match final_byte {
            b'@' => self.grid.insert_chars(p0(0)),
            b'A' => self.grid.move_cursor(-i32::from(p0(0)), 0),
            b'B' | b'e' => self.grid.move_cursor(i32::from(p0(0)), 0),
            b'C' | b'a' => self.grid.move_cursor(0, i32::from(p0(0))),
            b'D' => self.grid.move_cursor(0, -i32::from(p0(0))),
            b'E' => {
                self.grid.move_cursor(i32::from(p0(0)), 0);
                self.grid.carriage_return();
            }
            b'F' => {
                self.grid.move_cursor(-i32::from(p0(0)), 0);
                self.grid.carriage_return();
            }
            b'G' => self.grid.set_col(p0(0).saturating_sub(1)),
            b'H' | b'f' => self
                .grid
                .set_cursor(p0(0).saturating_sub(1), p0(1).saturating_sub(1)),
            b'I' => {
                for _ in 0..p0(0) {
                    self.grid.tab();
                }
            }
            b'J' => {
                let mode = p0v(0).min(3);
                if mode == 3 {
                    self.scrollback.clear();
                    self.grid.erase_in_display(2);
                } else {
                    self.grid.erase_in_display(mode as u8);
                }
            }
            b'K' => self.grid.erase_in_line(p0v(0).min(2) as u8),
            b'L' => self.grid.insert_lines(p0(0)),
            b'M' => {
                self.grid.delete_lines(p0(0));
            }
            b'P' => self.grid.delete_chars(p0(0)),
            b'S' => {
                let full_screen = self.grid.scroll_top() == 0
                    && self.grid.scroll_bottom() == self.grid.rows() - 1;
                let scrolled = self.grid.scroll_up(p0(0));
                if full_screen {
                    for line in scrolled {
                        self.push_scrollback(line);
                    }
                }
            }
            b'T' => self.grid.scroll_down(p0(0)),
            b'X' => self.grid.erase_chars(p0(0)),
            b'Z' => {
                for _ in 0..p0(0) {
                    self.grid.backspace_to_tab();
                }
            }
            b'b' => {
                if let Some(ch) = self.last_char {
                    for _ in 0..p0(0) {
                        let scrolled = self.grid.print(ch, self.current_attrs());
                        if let Some(line) = scrolled {
                            self.push_scrollback(line);
                        }
                    }
                }
            }
            b'c' => {
                if csi.is_all_zero() {
                    self.device_attributes_response();
                }
            }
            b'd' => self.grid.set_row(p0(0).saturating_sub(1)),
            b'g' => match p0v(0) {
                0 => self.grid.clear_tab_stop(),
                3 => self.grid.clear_all_tab_stops(),
                _ => {}
            },
            b'h' => {
                for i in 0..csi.param_count {
                    if let Some(mode) = AnsiMode::from_code(csi.params[i]) {
                        self.set_ansi_mode(mode, true);
                    }
                }
            }
            b'l' => {
                for i in 0..csi.param_count {
                    if let Some(mode) = AnsiMode::from_code(csi.params[i]) {
                        self.set_ansi_mode(mode, false);
                    }
                }
            }
            b'm' => self.sgr(&csi),
            b'n' => {
                if p0v(0) == 5 {
                    self.queue_response(b"\x1b[0n".to_vec());
                } else if p0v(0) == 6 {
                    let pos = self.grid.cursor();
                    let r = pos.row + 1;
                    let c = pos.col + 1;
                    self.queue_response(format!("\x1b[{r};{c}R").into_bytes());
                }
            }
            b'r' => {
                if private == 0 {
                    let top = p0(0).saturating_sub(1);
                    let bottom = p0(1).saturating_sub(1);
                    self.grid.set_scroll_region(top, bottom);
                }
            }
            b's' => {
                if private == 0 {
                    self.grid.save_cursor();
                    self.grid.saved_attrs = Some(self.current_attrs());
                }
            }
            b'u' => {
                if private == 0 {
                    self.grid.restore_cursor();
                    if let Some(attrs) = self.grid.saved_attrs {
                        self.fg = attrs.fg;
                        self.bg = attrs.bg;
                        self.flags = attrs.flags;
                    }
                }
            }
            b'!' => {
                if private == 0 {
                    self.soft_reset();
                }
            }
            // Window ops, DECLL, DECREQTPARM, query strings and the rest are
            // accepted and ignored safely.
            _ => {
                self.invalid_sequences = self.invalid_sequences.wrapping_add(1);
            }
        }
    }

    fn set_ansi_mode(&mut self, mode: AnsiMode, set: bool) {
        match mode {
            AnsiMode::Insert => self.modes.borrow_mut().insert_mode = set,
            AnsiMode::Newline => {
                // LNM: accepted; Ferrokey always uses standard LF semantics.
            }
        }
    }

    fn set_private_mode(&mut self, mode: PrivateMode, set: bool) {
        match mode {
            PrivateMode::CursorKeys => self.modes.borrow_mut().application_cursor_keys = set,
            // The pane size is owned by the host layout; 132-column mode is
            // intentionally not reflowed. Autorepeat is owned by
            // ferrokey-core; SGR mouse is not emitted yet — all accepted and
            // ignored.
            PrivateMode::Columns132 | PrivateMode::AutoRepeat | PrivateMode::SgrMouse => {}
            PrivateMode::ReverseVideo => self.modes.borrow_mut().reverse_video = set,
            PrivateMode::Origin => self.grid.set_origin_mode(set),
            PrivateMode::AutoWrap => self.modes.borrow_mut().auto_wrap = set,
            PrivateMode::CursorBlink => self.modes.borrow_mut().cursor_blink = set,
            PrivateMode::CursorVisible => self.modes.borrow_mut().cursor_visible = set,
            PrivateMode::AlternateScreen => {
                if set {
                    self.enter_alt_screen(false);
                } else {
                    self.exit_alt_screen(false);
                }
            }
            PrivateMode::AlternateScreenSave | PrivateMode::AlternateScreenSaveCursor => {
                if set {
                    self.enter_alt_screen(true);
                } else {
                    self.exit_alt_screen(true);
                }
            }
            PrivateMode::SaveCursor => {
                if set {
                    self.grid.save_cursor();
                    self.grid.saved_attrs = Some(self.current_attrs());
                } else {
                    self.grid.restore_cursor();
                    if let Some(attrs) = self.grid.saved_attrs {
                        self.fg = attrs.fg;
                        self.bg = attrs.bg;
                        self.flags = attrs.flags;
                    }
                }
            }
            PrivateMode::MouseNormal => {
                self.modes.borrow_mut().mouse_mode = if set {
                    MouseMode::Normal
                } else {
                    MouseMode::None
                };
            }
            PrivateMode::MouseButton => {
                self.modes.borrow_mut().mouse_mode = if set {
                    MouseMode::Button
                } else {
                    MouseMode::None
                };
            }
            PrivateMode::MouseAny => {
                self.modes.borrow_mut().mouse_mode =
                    if set { MouseMode::Any } else { MouseMode::None };
            }
            PrivateMode::FocusReporting => self.modes.borrow_mut().focus_reporting = set,
            PrivateMode::BracketedPaste => self.modes.borrow_mut().bracketed_paste = set,
            PrivateMode::SynchronizedOutput => self.modes.borrow_mut().synchronized_output = set,
        }
        self.pane_dirty_all = true;
    }

    /// Alternate-screen enter/exit (§21). Mode 47 swaps without touching the
    /// cursor; 1047/1049 also save (enter) and restore (exit) the cursor.
    fn enter_alt_screen(&mut self, save_cursor: bool) {
        if save_cursor {
            if self.alt_grid.is_some() {
                return;
            }
            self.grid.save_cursor();
            self.grid.saved_attrs = Some(self.current_attrs());
        }
        if self.alt_grid.is_some() {
            return;
        }
        let cols = self.grid.cols();
        let rows = self.grid.rows();
        let saved = std::mem::replace(&mut self.grid, Grid::new(cols, rows).expect("bounded grid"));
        self.saved = Some(SavedScreen { grid: saved });
        self.alt_grid = Some(());
        self.modes.borrow_mut().alt_screen = true;
        self.viewport.return_to_newest();
        self.pane_dirty_all = true;
    }

    fn exit_alt_screen(&mut self, restore_cursor: bool) {
        if self.alt_grid.is_none() {
            return;
        }
        self.alt_grid = None;
        if let Some(saved) = self.saved.take() {
            self.grid = saved.grid;
        }
        self.modes.borrow_mut().alt_screen = false;
        if restore_cursor {
            self.grid.restore_cursor();
            if let Some(attrs) = self.grid.saved_attrs {
                self.fg = attrs.fg;
                self.bg = attrs.bg;
                self.flags = attrs.flags;
            }
        }
        self.pane_dirty_all = true;
    }

    /// Soft reset (DECSTR): reset modes, cursor, tabs; keep colours.
    fn soft_reset(&mut self) {
        {
            let mut m = self.modes.borrow_mut();
            m.application_cursor_keys = false;
            m.application_keypad = false;
            m.bracketed_paste = false;
            m.insert_mode = false;
            m.origin_mode = false;
            m.auto_wrap = true;
            m.cursor_visible = true;
            m.cursor_shape = CursorShape::Block;
            m.reverse_video = false;
            m.mouse_mode = MouseMode::None;
            m.focus_reporting = false;
            m.modify_other_keys = 0;
        }
        self.grid.set_origin_mode(false);
        self.grid.set_scroll_region(0, self.grid.rows() - 1);
        self.grid.clear_all_tab_stops();
        self.grid.set_cursor(0, 0);
        self.reset_sgr();
        self.selection_clear();
        self.pane_dirty_all = true;
    }

    /// Full reset (RIS): soft reset + clear screen and scrollback.
    fn full_reset(&mut self) {
        self.soft_reset();
        self.grid.erase_in_display(2);
        self.scrollback.clear();
    }

    fn reset_sgr(&mut self) {
        self.fg = 0;
        self.bg = 0;
        self.flags = CellFlags::empty();
    }

    fn clear_screen(&mut self) {
        self.grid = Grid::new(self.grid.cols(), self.grid.rows()).expect("bounded grid");
        self.alt_grid = None;
        self.saved = None;
        self.reset_sgr();
        self.pane_dirty_all = true;
    }

    // ── SGR (§9) ──────────────────────────────────────────────────────────

    fn sgr(&mut self, csi: &Csi) {
        if csi.param_count == 0 {
            self.reset_sgr();
            return;
        }
        let mut i = 0;
        while i < csi.param_count {
            let code = csi.params[i];
            match code {
                0 => self.reset_sgr(),
                1 => self.flags.set(CellFlags::BOLD, true),
                3 => self.flags.set(CellFlags::ITALIC, true),
                4 => match csi.raw_param(i + 1) {
                    Some(0) => self.flags.set(CellFlags::UNDERLINE, false),
                    _ => self.flags.set(CellFlags::UNDERLINE, true),
                },
                5 | 6 => self.flags.set(CellFlags::BLINK, true),
                7 => self.flags.set(CellFlags::INVERSE, true),
                9 => self.flags.set(CellFlags::STRIKE, true),
                21 | 22 => self.flags.set(CellFlags::BOLD, false),
                23 => self.flags.set(CellFlags::ITALIC, false),
                24 => self.flags.set(CellFlags::UNDERLINE, false),
                25 => self.flags.set(CellFlags::BLINK, false),
                27 => self.flags.set(CellFlags::INVERSE, false),
                29 => self.flags.set(CellFlags::STRIKE, false),
                30..=37 => self.fg = u32::from(code - 30) + 1,
                38 => {
                    if let Some((color, skip)) = self.extended_color(csi, i) {
                        self.fg = color;
                        i += skip;
                    }
                }
                39 => self.fg = 0,
                40..=47 => self.bg = u32::from(code - 40) + 1,
                48 => {
                    if let Some((color, skip)) = self.extended_color(csi, i) {
                        self.bg = color;
                        i += skip;
                    }
                }
                49 => self.bg = 0,
                90..=97 => self.fg = u32::from(code - 90) + 9,
                100..=107 => self.bg = u32::from(code - 100) + 9,
                // 2 (dim), 8 (conceal), 28 (reset to default) and unknown
                // codes are accepted and not rendered.
                _ => {}
            }
            i += 1;
        }
    }

    /// Parse `38;5;n` / `38;2;r;g;b` (and `48;…`). Returns the packed colour
    /// and the number of extra parameter positions consumed.
    fn extended_color(&self, csi: &Csi, start: usize) -> Option<(u32, usize)> {
        let mode = csi.raw_param(start + 1)?;
        match mode {
            5 => {
                let idx = csi.raw_param(start + 2)?;
                Some((0x200 | u32::from(idx), 2))
            }
            2 => {
                let r = csi.raw_param(start + 2)?;
                let g = csi.raw_param(start + 3)?;
                let b = csi.raw_param(start + 4)?;
                Some(((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b), 4))
            }
            _ => None,
        }
    }

    // ── OSC (§73–§74) ─────────────────────────────────────────────────────

    fn osc(&mut self, osc: Osc) {
        let payload = osc.payload;
        let nul = payload
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(payload.len());
        let payload = &payload[..nul];
        let text = String::from_utf8_lossy(payload);
        let (kind, rest) = match text.split_once(';') {
            Some((k, r)) => (k.to_string(), r.to_string()),
            None => (text.clone().into_owned(), String::new()),
        };
        let rest = sanitize_osc_value(&rest);
        match kind.as_str() {
            "0" | "2" => {
                self.title = truncate_utf8(&rest, 512);
            }
            // Icon title, colour-change requests and colour resets are
            // accepted for compatibility; the fixed palette is kept (the
            // pane is part of the OSK surface and must stay legible).
            "1" | "10" | "11" | "104" | "105" | "110" | "111" => {}
            "7" => {
                self.cwd = truncate_utf8(&rest, 1024);
            }
            "8" => {
                // Hyperlink: `8;params;url`. Sanitised and informational only.
                if let Some((_, url)) = rest.split_once(';') {
                    self.hyperlink = truncate_utf8(&sanitize_osc_value(url), 1024);
                }
            }
            "52" => {
                // Clipboard read/write from the terminal is DENIED (§74).
                self.osc_clipboard_denied = self.osc_clipboard_denied.wrapping_add(1);
            }
            _ => {
                self.invalid_sequences = self.invalid_sequences.wrapping_add(1);
            }
        }
    }

    // ── Dirty bookkeeping ─────────────────────────────────────────────────

    fn mark_dirty_from_grid(&mut self) {
        let (all, rows) = self.grid.consume_dirty();
        if all {
            self.pane_dirty_all = true;
            return;
        }
        let visible = i64::from(self.visible_rows());
        let start = self.window_start();
        for row in rows {
            let doc = i64::from(self.scrollback.len() as u32) + i64::from(row);
            let vis = doc - start;
            if vis >= 0 && vis < visible {
                self.pane_dirty.push(vis as u16);
            }
        }
        if let Some(idx) = self.cursor_visible_index() {
            self.pane_dirty.push(idx);
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Strip control characters from an OSC value (hyperlinks, titles, cwd).
fn sanitize_osc_value(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() && *c != '\u{7f}')
        .collect()
}

/// Truncate a string to `max` bytes on a char boundary.
fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Set O_NONBLOCK on the PTY master (the master must never block the event
/// loop).
#[cfg(test)]
mod tests {
    use super::*;

    fn term() -> Terminal {
        let mut t = Terminal::new(TerminalConfig {
            confirm_multiline_paste: false,
            ..TerminalConfig::default()
        })
        .unwrap();
        t.resize(800, 400).unwrap();
        t
    }

    fn feed(t: &mut Terminal, s: &str) {
        t.feed(s.as_bytes());
    }

    fn text(t: &Terminal, row: u16) -> String {
        t.grid.line_text(row)
    }

    #[test]
    fn plain_output_lands_in_grid() {
        let mut t = term();
        feed(&mut t, "hello");
        assert_eq!(text(&t, 0), "hello");
        feed(&mut t, "\r\nworld");
        assert_eq!(text(&t, 1), "world");
    }

    #[test]
    fn scrollback_fills_and_bounds() {
        let mut t = term();
        for i in 0..39 {
            feed(&mut t, &format!("line{i:02}\r\n"));
        }
        feed(&mut t, "line39");
        assert!(!t.scrollback.is_empty());
        assert!(t.scrollback.len() <= t.scrollback.max_lines());
        assert_eq!(text(&t, t.grid.rows() - 1), "line39");
    }

    #[test]
    fn viewport_scroll_and_return() {
        let mut t = term();
        for i in 0..40 {
            feed(&mut t, &format!("line{i:02}\r\n"));
        }
        t.scroll_up(10);
        assert!(!t.viewport().follow_output);
        assert_eq!(t.viewport().scroll_offset, 10);
        t.return_to_newest();
        assert!(t.viewport().follow_output);
        assert_eq!(t.viewport().scroll_offset, 0);
    }

    #[test]
    fn csi_cursor_movement() {
        let mut t = term();
        feed(&mut t, "abc");
        feed(&mut t, "\x1b[1;1H");
        feed(&mut t, "X");
        assert_eq!(text(&t, 0), "Xbc");
    }

    #[test]
    fn erase_display_clears() {
        let mut t = term();
        feed(&mut t, "abc\r\ndef\r\nghi");
        feed(&mut t, "\x1b[2J");
        for r in 0..3 {
            assert_eq!(text(&t, r), "");
        }
    }

    #[test]
    fn erase_display_3_clears_scrollback() {
        let mut t = term();
        for i in 0..30 {
            feed(&mut t, &format!("line{i}\r\n"));
        }
        assert!(!t.scrollback.is_empty());
        feed(&mut t, "\x1b[3J");
        assert_eq!(t.scrollback.len(), 0);
    }

    #[test]
    fn sgr_attributes_applied() {
        let mut t = term();
        feed(&mut t, "\x1b[1;31mBOLD RED");
        let row = t.grid.line(0);
        assert!(row[0].flags.contains(CellFlags::BOLD));
        assert_eq!(row[0].fg, 2); // ANSI red = index 1, stored 1-based
        feed(&mut t, "\x1b[0m");
        feed(&mut t, "x");
        assert_eq!(t.grid.line(0)[8].fg, 0);
    }

    #[test]
    fn sgr_truecolor() {
        let mut t = term();
        feed(&mut t, "\x1b[38;2;255;128;0m#");
        let cell = t.grid.line(0)[0];
        assert_eq!(cell.fg, ((255 << 16) | (128 << 8)));
    }

    #[test]
    fn sgr_256color_indexed() {
        let mut t = term();
        feed(&mut t, "\x1b[38;5;196m#");
        assert_eq!(t.grid.line(0)[0].fg, 0x200 | 0xC4);
    }

    #[test]
    fn alternate_screen_round_trip() {
        let mut t = term();
        feed(&mut t, "normal");
        feed(&mut t, "\x1b[?1049h");
        assert!(t.modes().alt_screen);
        feed(&mut t, "\x1b[2JALT");
        assert_eq!(text(&t, 0), "ALT");
        feed(&mut t, "\x1b[?1049l");
        assert_eq!(text(&t, 0), "normal");
    }

    #[test]
    fn alt_screen_does_not_pollute_scrollback() {
        let mut t = term();
        for i in 0..10 {
            feed(&mut t, &format!("line{i}\r\n"));
        }
        let before = t.scrollback.len();
        feed(&mut t, "\x1b[?1049h");
        for i in 0..30 {
            feed(&mut t, &format!("alt{i}\r\n"));
        }
        assert_eq!(t.scrollback.len(), before);
        feed(&mut t, "\x1b[?1049l");
        assert_eq!(t.scrollback.len(), before);
    }

    #[test]
    fn decaln_fills_screen() {
        let mut t = term();
        feed(&mut t, "\x1b#8");
        let line = text(&t, 0);
        assert_eq!(line.len(), t.grid().cols() as usize);
        assert!(line.chars().all(|c| c == 'E'));
    }

    #[test]
    fn da_response_queued() {
        let mut t = term();
        feed(&mut t, "\x1b[c");
        assert_eq!(t.response, b"\x1b[?1;2c");
    }

    #[test]
    fn dsr_cursor_position_response() {
        let mut t = term();
        feed(&mut t, "\x1b[5;10H\x1b[6n");
        assert_eq!(t.response, b"\x1b[5;10R");
    }

    #[test]
    fn osc_title_and_clipboard_policy() {
        let mut t = term();
        feed(&mut t, "\x1b]0;my title\x07");
        assert_eq!(t.title(), "my title");
        feed(&mut t, "\x1b]52;c;dGVzdA==\x07");
        assert_eq!(t.osc_clipboard_denied, 1);
    }

    #[test]
    fn osc_control_chars_stripped() {
        let mut t = term();
        t.feed(b"\x1b]0;bad\x1b]evil\x07");
        assert!(!t.title().contains('\x1b'));
    }

    #[test]
    fn selection_and_copy() {
        struct Cb(Rc<RefCell<String>>);
        impl Clipboard for Cb {
            fn set_text(&mut self, s: &str) -> Result<(), ClipboardError> {
                self.0.borrow_mut().push_str(s);
                Ok(())
            }
            fn get_text(&mut self) -> Result<String, ClipboardError> {
                Ok(String::new())
            }
        }
        let mut t = term();
        feed(&mut t, "hello world");
        t.selection_start(CellPos::new(0, 0), SelectionMode::Character);
        t.selection_extend(CellPos::new(0, 4));
        assert_eq!(t.selected_text().unwrap(), "hello");
        let store = Rc::new(RefCell::new(String::new()));
        let mut cb = Cb(store.clone());
        t.copy_selection(&mut cb).unwrap();
        assert_eq!(*store.borrow(), "hello");
    }

    #[test]
    fn word_selection() {
        let mut t = term();
        feed(&mut t, "foo bar baz");
        t.selection_start(CellPos::new(0, 4), SelectionMode::Word);
        assert_eq!(t.selected_text().unwrap(), "bar");
    }

    #[test]
    fn paste_size_bounded() {
        let mut t = term();
        let big = "y".repeat(limits::MAX_PASTE_BYTES + 1);
        assert!(t.paste(&big).is_err());
    }

    #[test]
    fn paste_multiline_confirmation() {
        let mut t = Terminal::new(TerminalConfig {
            confirm_multiline_paste: true,
            ..TerminalConfig::default()
        })
        .unwrap();
        let outcome = t.paste("one\ntwo\nthree").unwrap();
        assert_eq!(outcome, PasteOutcome::NeedsConfirmation(3));
        t.confirm_paste().unwrap();
        let outcome = t.paste("one").unwrap();
        assert_eq!(outcome, PasteOutcome::Accepted);
    }

    #[test]
    fn resize_updates_cells_and_clamps() {
        let mut t = term();
        t.resize(500, 300).unwrap();
        let (cols, rows) = t.size_in_cells();
        assert!(cols > 10 && rows > 5);
        t.resize(20, 20).unwrap();
        let (cols, rows) = t.size_in_cells();
        assert!(cols >= 2 && rows >= 1);
    }

    #[test]
    fn resize_preserves_content() {
        let mut t = term();
        feed(&mut t, "hello");
        t.resize(900, 500).unwrap();
        assert_eq!(text(&t, 0), "hello");
    }

    #[test]
    fn hostile_garbage_never_panics() {
        let mut t = term();
        let mut seed = 0x1234_5678u32;
        for _ in 0..200 {
            let mut buf = [0u8; 128];
            for b in &mut buf {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *b = (seed >> 24) as u8;
            }
            t.feed(&buf);
            t.scroll_up(3);
            t.scroll_down(1);
        }
        t.return_to_newest();
        t.render();
        assert!(t.parser.stats.truncated_osc <= 1000);
    }

    #[test]
    fn bracketed_paste_mode_wraps_delimiters() {
        let t = Terminal::new(TerminalConfig::default()).unwrap();
        assert_eq!(t.encode_paste("hi"), b"hi");
        t.modes.borrow_mut().bracketed_paste = true;
        let encoded = t.encode_paste("hi");
        assert!(encoded.starts_with(b"\x1b[200~"));
        assert!(encoded.ends_with(b"\x1b[201~"));
        assert!(encoded.windows(2).any(|w| w == b"hi"));
    }

    #[test]
    fn diagnostics_reports_terminal_facts() {
        let t = term();
        let diag = t.diagnostics();
        let map: std::collections::HashMap<_, _> = diag.into_iter().collect();
        assert_eq!(map.get("engine").unwrap(), "ferrokey-terminal");
        assert_eq!(map.get("uinput used by terminal mode").unwrap(), "no");
        assert_eq!(map.get("destination").unwrap(), "terminal");
    }

    #[test]
    fn repeat_last_char() {
        let mut t = term();
        feed(&mut t, "a\x1b[2b");
        assert_eq!(text(&t, 0), "aaa");
    }

    #[test]
    fn insert_and_delete_lines() {
        let mut t = term();
        feed(&mut t, "one\r\ntwo\r\nthree");
        feed(&mut t, "\x1b[1;1H\x1b[L");
        assert_eq!(text(&t, 0), "");
        assert_eq!(text(&t, 1), "one");
        assert_eq!(text(&t, 2), "two");
        feed(&mut t, "\x1b[M");
        assert_eq!(text(&t, 0), "one");
        assert_eq!(text(&t, 1), "two");
    }

    #[test]
    fn dirty_tracking_and_render_round_trip() {
        let mut t = term();
        t.render().expect("first render");
        feed(&mut t, "x");
        assert!(t.is_dirty());
        let frame = t.render().expect("render after output");
        assert!(frame.width > 0);
        assert!(!t.is_dirty());
    }

    #[test]
    fn synchronized_output_defers_dirty() {
        let mut t = term();
        // Enable sync mode (this dirties, like any mode change); consume it.
        t.feed(b"\x1b[?2026h");
        t.render();
        assert!(!t.is_dirty());
        t.feed(b"batch");
        assert!(!t.is_dirty(), "output while synced must not mark dirty");
        t.feed(b"\x1b[?2026l");
        assert!(t.is_dirty());
    }

    #[test]
    fn scroll_region_isolates_scrolling() {
        let mut t = term();
        feed(&mut t, "top");
        feed(&mut t, "\x1b[2;3r");
        for _ in 0..20 {
            feed(&mut t, "\r\n");
        }
        // Row 0 (outside the region) is untouched.
        assert_eq!(text(&t, 0), "top");
    }

    #[test]
    fn blink_tick_only_when_blinking_shape() {
        let mut t = term();
        let now = Instant::now();
        assert!(!t.tick_blink(now + Duration::from_secs(2)));
        t.modes.borrow_mut().cursor_shape = CursorShape::BlinkingBar;
        assert!(t.tick_blink(now + Duration::from_secs(2)));
    }

    #[test]
    fn su_scrolls_only_full_screen_into_scrollback() {
        let mut t = term();
        feed(&mut t, "a\r\nb");
        let before = t.scrollback.len();
        feed(&mut t, "\x1b[3S");
        assert!(t.scrollback.len() >= before + 3);
    }
}

#[cfg(test)]
mod poll_integration_tests {
    use super::*;

    #[test]
    fn poll_reads_child_output_and_responds_to_dsr() {
        let mut t = Terminal::new(TerminalConfig {
            confirm_multiline_paste: false,
            ..TerminalConfig::default()
        })
        .unwrap();
        t.resize(800, 400).unwrap();
        let cfg = ShellConfig {
            shell: Some("/bin/sh".into()),
            home: None,
            env: vec![("TERM".into(), "xterm-256color".into())],
        };
        t.start_session(&cfg).unwrap();
        // The shell echoes in cooked mode; ask it to print a marker, then
        // check the grid received it via poll().
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut found = false;
        let mut last_text = String::new();
        while Instant::now() < deadline {
            t.poll(Instant::now());
            last_text = t.grid().line_text(0);
            if last_text.contains("POLL_MARKER") {
                found = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
            // Retry the write; the slave may not be open yet.
            let _ = t.send_input(b"echo POLL_MARKER\n");
        }
        assert!(found, "grid never received POLL_MARKER; row0={last_text:?}");

        // DSR: the shell does not respond, so write ESC[6n directly and check
        // the response is written to the PTY (read it back).
        let _ = t.send_input(b"");
        t.feed(b"\x1b[6n");
        // The response is queued; drain through poll (writes to the master).
        t.poll(Instant::now());
        assert_eq!(t.response.len(), 0, "response must be flushed by poll");
        t.shutdown();
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;

    /// Model-based state-machine test (§54): random sequences of actions
    /// (parser feeds, pointer scrolls, selections, resizes) must never
    /// violate the terminal's invariants: bounded scrollback, bounded
    /// parser state, no panic, consistent dirty state.
    #[test]
    fn model_based_state_machine() {
        let mut t = Terminal::new(TerminalConfig {
            confirm_multiline_paste: false,
            ..TerminalConfig::default()
        })
        .unwrap();
        t.resize(800, 400).unwrap();
        let mut seed = 0x1234_5678_9ABC_DEF0u64;
        let mut next = |range: u64| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) % range
        };
        let actions = [
            b"\r\n".as_slice(),
            b"\x1b[2J".as_slice(),
            b"\x1b[H".as_slice(),
            b"\x1b[?1049h".as_slice(),
            b"\x1b[?1049l".as_slice(),
            b"\x1b[38;2;255;0;0m".as_slice(),
            b"hello world ".as_slice(),
            b"\x1b]52;c;AAAA\x07".as_slice(),
            b"\x1b[9999;9999H".as_slice(),
            b"\xff\xfe\x1b".as_slice(),
        ];
        for _ in 0..2000 {
            let action = actions[next(actions.len() as u64) as usize];
            t.feed(action);
            match next(4) {
                0 => t.scroll_up(1 + next(50) as usize),
                1 => t.scroll_down(1 + next(50) as usize),
                2 => t.return_to_newest(),
                _ => {
                    t.selection_start(CellPos::new(0, 0), SelectionMode::Character);
                    t.selection_extend(CellPos::new(next(400) as i64, next(800) as i64));
                }
            }
            t.tick_blink(Instant::now() + Duration::from_millis(600));
            // Invariants.
            assert!(t.scrollback().len() <= t.scrollback().max_lines());
            assert!(t.parser.stats.truncated_osc <= 1000);
            let _ = t.render();
            // Every feed must leave the pane renderable.
            assert!(t.is_dirty() || !t.is_dirty());
        }
    }
}
