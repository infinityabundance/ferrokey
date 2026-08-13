//! The live terminal grid: a bounded rectangle of cells with cursor, scroll
//! region, tab stops and saved-cursor state.
//!
//! The grid is the *live screen* only. Historical output lives in
//! [`crate::scrollback::Scrollback`]; the alternate screen lives in a second
//! `Grid` owned by [`crate::terminal::Terminal`]. This module implements the
//! cursor/edit operations that ANSI control sequences drive, and nothing else
//! — it has no notion of the parser, the PTY or the UI.
//!
//! Memory is bounded by construction: every row is exactly `cols` cells and
//! the row count is fixed by the pane size (§78).

use crate::limits;
use unicode_width::UnicodeWidthChar;

/// A 24-bit RGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b }
    }

    /// Pack to `0xRRGGBB`.
    pub const fn packed(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    pub const fn from_packed(packed: u32) -> Self {
        Color {
            r: (packed >> 16) as u8,
            g: (packed >> 8) as u8,
            b: packed as u8,
        }
    }

    /// Blend `self` (background) toward `fg` by `alpha` in 0..=255.
    pub const fn blend(self, fg: Color, alpha: u8) -> Color {
        let a = alpha as u32;
        let inv = 255 - a;
        Color {
            r: ((self.r as u32 * inv + fg.r as u32 * a) / 255) as u8,
            g: ((self.g as u32 * inv + fg.g as u32 * a) / 255) as u8,
            b: ((self.b as u32 * inv + fg.b as u32 * a) / 255) as u8,
        }
    }
}

/// Per-cell attribute flags (packed into one byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellFlags(u8);

impl CellFlags {
    pub const BOLD: u8 = 1 << 0;
    pub const ITALIC: u8 = 1 << 1;
    pub const UNDERLINE: u8 = 1 << 2;
    pub const STRIKE: u8 = 1 << 3;
    pub const INVERSE: u8 = 1 << 4;
    pub const WIDE: u8 = 1 << 5;
    pub const WIDE_CONT: u8 = 1 << 6;
    pub const BLINK: u8 = 1 << 7;

    pub const fn empty() -> Self {
        CellFlags(0)
    }

    pub const fn contains(self, flag: u8) -> bool {
        self.0 & flag == flag
    }

    pub const fn set(&mut self, flag: u8, on: bool) {
        if on {
            self.0 |= flag;
        } else {
            self.0 &= !flag;
        }
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn from_bits(bits: u8) -> Self {
        CellFlags(bits)
    }
}

/// Maximum number of combining marks retained per cell (bounded memory).
pub const MAX_COMBINING: usize = 2;

/// One terminal cell.
///
/// ~24 bytes per cell; a 120×40 pane is ~115 KiB of live cells, and each
/// scrollback line is `cols × 24` bytes, so the *default* 10_000-line
/// scrollback at 120 columns is ~29 MiB worst case — bounded, documented and
/// configurable (see `limits::MAX_SCROLLBACK`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// `'\0'` marks an empty cell.
    pub ch: char,
    /// Packed `0xRRGGBB` foreground.
    pub fg: u32,
    /// Packed `0xRRGGBB` background.
    pub bg: u32,
    pub flags: CellFlags,
    /// Combining marks (max [`MAX_COMBINING`]; further marks are dropped).
    pub combining: [char; MAX_COMBINING],
    pub combining_len: u8,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: '\0',
            fg: 0,
            bg: 0,
            flags: CellFlags::empty(),
            combining: ['\0'; MAX_COMBINING],
            combining_len: 0,
        }
    }
}

impl Cell {
    pub const fn is_empty(&self) -> bool {
        self.ch == '\0'
    }

    pub const fn is_wide(&self) -> bool {
        self.flags.contains(CellFlags::WIDE)
    }

    pub const fn is_wide_cont(&self) -> bool {
        self.flags.contains(CellFlags::WIDE_CONT)
    }

    /// The visible glyph width of this cell's content.
    pub fn display_width(&self) -> usize {
        if self.is_wide() {
            2
        } else {
            usize::from(self.ch != '\0')
        }
    }
}

/// A cell position: 0-based row and column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub row: u16,
    pub col: u16,
}

impl Pos {
    pub const fn new(row: u16, col: u16) -> Self {
        Pos { row, col }
    }
}

/// A logical line of cells (a grid row or a scrollback entry).
pub type Line = Vec<Cell>;

/// The live screen.
#[derive(Debug, Clone)]
pub struct Grid {
    rows: Vec<Line>,
    cols: u16,
    row_count: u16,
    cursor: Pos,
    saved_cursor: Pos,
    /// Saved attributes/colour state (DECSC / ESC 7). The facade applies
    /// this when the cell attributes move with the cursor (xterm semantics:
    /// DECSC saves cursor + attributes).
    pub saved_attrs: Option<CellAttrs>,
    /// Scroll region, 0-based inclusive.
    scroll_top: u16,
    scroll_bottom: u16,
    tab_stops: Vec<bool>,
    /// Set after printing in the last column; the next printable wraps.
    wrap_pending: bool,
    /// DECOM origin mode: cursor row is relative to the scroll region.
    origin_mode: bool,
    /// Rows dirtied since the last clear (visible-row dirty tracking).
    dirty: Vec<bool>,
    dirty_all: bool,
    /// Number of cell updates since creation (drives the cursor-blink and
    /// interaction heuristics; cheap and bounded).
    pub revision: u64,
}

/// Colour + attribute state that moves with a saved cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellAttrs {
    pub fg: u32,
    pub bg: u32,
    pub flags: CellFlags,
}

impl Default for CellAttrs {
    fn default() -> Self {
        CellAttrs {
            fg: 0,
            bg: 0,
            flags: CellFlags::empty(),
        }
    }
}

/// Grid editing errors (all impossible under the crate's own bounds checks,
/// kept for defensive completeness).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GridError {
    #[error("grid dimensions out of bounds (cols {cols}, rows {rows})")]
    BadSize { cols: u16, rows: u16 },
    #[error("cursor row {row} outside grid")]
    Row { row: u16 },
    #[error("cursor column {col} outside grid")]
    Col { col: u16 },
}

impl Grid {
    /// Create a grid of `cols × rows` cells.
    pub fn new(cols: u16, rows: u16) -> Result<Self, GridError> {
        if !(limits::MIN_COLS..=limits::MAX_COLS).contains(&cols)
            || !(limits::MIN_ROWS..=limits::MAX_ROWS).contains(&rows)
        {
            return Err(GridError::BadSize { cols, rows });
        }
        let blank = vec![Cell::default(); usize::from(cols)];
        let rows_vec = vec![blank; usize::from(rows)];
        Ok(Grid {
            rows: rows_vec,
            cols,
            row_count: rows,
            cursor: Pos::new(0, 0),
            saved_cursor: Pos::new(0, 0),
            saved_attrs: None,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            tab_stops: (0..cols).map(|c| c % 8 == 0).collect(),
            wrap_pending: false,
            origin_mode: false,
            dirty: vec![true; usize::from(rows)],
            dirty_all: true,
            revision: 0,
        })
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.row_count
    }

    pub fn cursor(&self) -> Pos {
        self.cursor
    }

    pub fn scroll_top(&self) -> u16 {
        self.scroll_top
    }

    pub fn scroll_bottom(&self) -> u16 {
        self.scroll_bottom
    }

    /// The current cell attributes (for DECSC save).
    pub fn cursor_attrs(&self) -> CellAttrs {
        let cell = &self.rows[usize::from(self.cursor.row)][usize::from(self.cursor.col)];
        CellAttrs {
            fg: cell.fg,
            bg: cell.bg,
            flags: cell.flags,
        }
    }

    /// A row as a slice of cells. `row` must be < `rows()`.
    pub fn line(&self, row: u16) -> &Line {
        &self.rows[usize::from(row)]
    }

    /// A single cell, `None` for continuation halves of wide glyphs.
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        if row >= self.row_count || col >= self.cols {
            return None;
        }
        Some(&self.rows[usize::from(row)][usize::from(col)])
    }

    pub fn cell_mut(&mut self, row: u16, col: u16) -> Option<&mut Cell> {
        if row >= self.row_count || col >= self.cols {
            return None;
        }
        self.mark_dirty_row(row);
        Some(&mut self.rows[usize::from(row)][usize::from(col)])
    }

    // ── Dirty tracking ─────────────────────────────────────────────────────

    pub fn mark_all_dirty(&mut self) {
        self.dirty_all = true;
    }

    pub fn consume_dirty(&mut self) -> (bool, Vec<u16>) {
        let all = self.dirty_all;
        let rows: Vec<u16> = if all {
            (0..self.row_count).collect()
        } else {
            self.dirty
                .iter()
                .enumerate()
                .filter(|(_, d)| **d)
                .map(|(i, _)| i as u16)
                .collect()
        };
        self.dirty_all = false;
        for d in &mut self.dirty {
            *d = false;
        }
        (all, rows)
    }

    fn mark_dirty_row(&mut self, row: u16) {
        self.revision = self.revision.wrapping_add(1);
        if row < self.row_count {
            self.dirty[usize::from(row)] = true;
        }
    }

    // ── Cursor ────────────────────────────────────────────────────────────

    /// Clamp `pos` into the current *active* region: origin mode restricts the
    /// cursor to the scroll region; otherwise the whole screen.
    pub fn clamp_cursor(&self, row: u16, col: u16) -> Pos {
        let max_row = self.row_count - 1;
        let max_col = self.cols - 1;
        Pos {
            row: row.min(max_row),
            col: col.min(max_col),
        }
    }

    /// Position the cursor at an absolute position (CUP/HVP).
    /// When origin mode is set, row 0 is `scroll_top` (DECOM semantics);
    /// otherwise the cursor may reach any row of the screen.
    pub fn set_cursor(&mut self, row: u16, col: u16) {
        let row = if self.origin_mode_active() {
            let (min_row, max_row) = self.active_row_range();
            min_row.saturating_add(row).min(max_row)
        } else {
            row.min(self.row_count - 1)
        };
        self.cursor = self.clamp_cursor(row, col);
        self.wrap_pending = false;
    }

    /// Absolute row without origin offset (used by scroll-region commands).
    fn active_row_range(&self) -> (u16, u16) {
        (self.scroll_top, self.scroll_bottom)
    }

    fn origin_mode_active(&self) -> bool {
        self.origin_mode
    }

    pub fn set_origin_mode(&mut self, on: bool) {
        self.origin_mode = on;
        if on {
            let (min, _) = self.active_row_range();
            self.cursor = Pos::new(min, 0);
        } else {
            self.cursor = Pos::new(0, 0);
        }
        self.wrap_pending = false;
    }

    pub fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let top = top.min(self.row_count - 1);
        let bottom = bottom.min(self.row_count - 1);
        if bottom >= top {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
        }
        // DECSTBM homes the cursor (to the region top under origin mode).
        self.set_cursor(0, 0);
    }

    /// Relative cursor movement (CUU/CUD/CUF/CUB). Negative clamps to 0.
    pub fn move_cursor(&mut self, drow: i32, dcol: i32) {
        let row = i32::from(self.cursor.row) + drow;
        let col = i32::from(self.cursor.col) + dcol;
        let row = row.max(0).min(i32::from(self.row_count - 1)) as u16;
        let col = col.max(0).min(i32::from(self.cols - 1)) as u16;
        self.cursor = Pos::new(row, col);
        self.wrap_pending = false;
    }

    /// CHA / HPA: absolute column.
    pub fn set_col(&mut self, col: u16) {
        self.cursor.col = col.min(self.cols - 1);
        self.wrap_pending = false;
    }

    /// VPA: absolute row (no origin offset — ANSI semantics).
    pub fn set_row(&mut self, row: u16) {
        self.cursor.row = row.min(self.row_count - 1);
        self.wrap_pending = false;
    }

    pub fn carriage_return(&mut self) {
        self.cursor.col = 0;
        self.wrap_pending = false;
    }

    pub fn backspace(&mut self) {
        if self.cursor.col > 0 {
            self.cursor.col -= 1;
        }
        self.wrap_pending = false;
    }

    /// HT — advance to the next tab stop.
    pub fn tab(&mut self) {
        let col = self.cursor.col;
        let next = (usize::from(col) + 1..usize::from(self.cols))
            .find(|&c| self.tab_stops[c])
            .unwrap_or(usize::from(self.cols) - 1);
        self.cursor.col = next as u16;
        self.wrap_pending = false;
    }

    /// CBT — move to the previous tab stop (or column 0).
    pub fn backspace_to_tab(&mut self) {
        let col = usize::from(self.cursor.col);
        if col > 0 {
            let prev = (0..col).rev().find(|&c| self.tab_stops[c]);
            self.cursor.col = prev.unwrap_or(0) as u16;
        }
        self.wrap_pending = false;
    }

    /// The column of the next tab stop after `col` (for encoder/renderer).
    pub fn next_tab_stop(&self, col: u16) -> Option<u16> {
        (usize::from(col) + 1..usize::from(self.cols))
            .find(|&c| self.tab_stops[c])
            .map(|c| c as u16)
    }

    pub fn set_tab_stop(&mut self) {
        let c = usize::from(self.cursor.col);
        if c < self.tab_stops.len() {
            self.tab_stops[c] = true;
        }
    }

    pub fn clear_tab_stop(&mut self) {
        let c = usize::from(self.cursor.col);
        if c < self.tab_stops.len() {
            self.tab_stops[c] = false;
        }
    }

    pub fn clear_all_tab_stops(&mut self) {
        for t in &mut self.tab_stops {
            *t = false;
        }
    }

    pub fn save_cursor(&mut self) {
        self.saved_cursor = self.cursor;
    }

    pub fn restore_cursor(&mut self) {
        self.cursor = self.clamp_cursor(self.saved_cursor.row, self.saved_cursor.col);
        self.wrap_pending = false;
    }

    // ── Content ───────────────────────────────────────────────────────────

    /// Print one character at the cursor (handles wrap, wide glyphs and
    /// combining marks). `attrs` carries the current SGR state.
    ///
    /// Returns the line scrolled into history when the wrap forced a scroll
    /// (the facade stores it in scrollback); `None` otherwise.
    pub fn print(&mut self, ch: char, attrs: CellAttrs) -> Option<Line> {
        let width = ch.width().unwrap_or(0);
        if width == 0 {
            // Zero-width characters (combining marks, ZWJ, variation
            // selectors) attach to the previous cell; at the start of a
            // line they are dropped.
            if self.cursor.col > 0 {
                let row = usize::from(self.cursor.row);
                let col = usize::from(self.cursor.col) - 1;
                let cell = &mut self.rows[row][col];
                if cell.combining_len < MAX_COMBINING as u8 {
                    cell.combining[usize::from(cell.combining_len)] = ch;
                    cell.combining_len += 1;
                    self.mark_dirty_row(self.cursor.row);
                }
            }
            return None;
        }

        // Wrap if pending or if the glyph needs more room than remains.
        let mut scrolled = None;
        let needs_wrap = self.wrap_pending
            || (width > 1 && usize::from(self.cursor.col) + width > usize::from(self.cols));
        if needs_wrap {
            scrolled = self.index();
            self.cursor.col = 0;
        }

        let row = usize::from(self.cursor.row);
        let col = usize::from(self.cursor.col);
        let cols = usize::from(self.cols);
        let new_cell = self.make_cell(ch, attrs);
        let cell = &mut self.rows[row][col];
        *cell = new_cell;
        if width == 2 {
            cell.flags.set(CellFlags::WIDE, true);
            if col + 1 < cols {
                let mut cont = Cell::default();
                cont.flags.set(CellFlags::WIDE_CONT, true);
                self.rows[row][col + 1] = cont;
            }
        }
        self.mark_dirty_row(self.cursor.row);

        let new_col = usize::from(self.cursor.col) + width;
        if new_col >= cols {
            self.cursor.col = (cols - 1) as u16;
            self.wrap_pending = true;
        } else {
            self.cursor.col = new_col as u16;
            self.wrap_pending = false;
        }
        scrolled
    }

    fn make_cell(&self, ch: char, attrs: CellAttrs) -> Cell {
        Cell {
            ch,
            fg: attrs.fg,
            bg: attrs.bg,
            flags: attrs.flags,
            ..Cell::default()
        }
    }

    /// Apply SGR attributes to the current cursor position (the default is to
    /// apply to future output, but xterm applies SGR 25/27 etc. to the
    /// current cell in some modes — Ferrokey applies attributes to *future*
    /// cells, which is the modern convention).
    pub fn set_attrs(&mut self, _attrs: CellAttrs) {
        // Attributes are carried by the facade's SGR state; nothing to store
        // on the grid itself.
    }

    /// IND: move down one row, scrolling the region if at the bottom.
    /// Returns the line scrolled out of the top of the region (for
    /// scrollback), or `None`.
    pub fn index(&mut self) -> Option<Line> {
        self.wrap_pending = false;
        if self.cursor.row < self.scroll_bottom {
            self.cursor.row += 1;
            None
        } else {
            // Scroll the region up by one.
            let scrolled = self.scroll_up_region(1);
            scrolled.into_iter().next()
        }
    }

    /// RI: reverse index — move up, scrolling the region down if at the top.
    /// Returns the line restored from scrollback (the facade pops it), or
    /// `None`.
    pub fn reverse_index(&mut self) -> Option<()> {
        self.wrap_pending = false;
        if self.cursor.row > self.scroll_top {
            self.cursor.row -= 1;
            None
        } else {
            self.scroll_down_region(1);
            Some(())
        }
    }

    /// NEL: CR + LF.
    pub fn next_line(&mut self) -> Option<Line> {
        self.carriage_return();
        self.line_feed()
    }

    /// LF / VT / FF: move down (with region scroll). Returns the line pushed
    /// into history, if any.
    pub fn line_feed(&mut self) -> Option<Line> {
        self.wrap_pending = false;
        if self.cursor.row < self.scroll_bottom {
            self.cursor.row += 1;
            None
        } else {
            let mut out = self.scroll_up_region(1);
            out.pop()
        }
    }

    /// Scroll the region up by `n` lines. Returns the lines that left the
    /// top of the region (the facade stores them in scrollback).
    pub fn scroll_up_region(&mut self, n: u16) -> Vec<Line> {
        let n = n.min(self.scroll_bottom - self.scroll_top + 1);
        let mut out = Vec::with_capacity(usize::from(n));
        for _ in 0..n {
            let removed = self.rows.remove(usize::from(self.scroll_top));
            out.push(removed);
            let blank = vec![Cell::default(); usize::from(self.cols)];
            self.rows.insert(usize::from(self.scroll_bottom), blank);
        }
        self.mark_region_dirty();
        out
    }

    /// Scroll the region down by `n` lines. `incoming` (from scrollback) is
    /// inserted at the top; extra blank lines pad.
    pub fn scroll_down_region(&mut self, n: u16) {
        let n = n.min(self.scroll_bottom - self.scroll_top + 1);
        for _ in 0..n {
            let blank = vec![Cell::default(); usize::from(self.cols)];
            self.rows.insert(usize::from(self.scroll_top), blank);
            self.rows.remove(usize::from(self.scroll_bottom) + 1);
        }
        self.mark_region_dirty();
    }

    /// Restore a line from scrollback at the top of the region (RI at the
    /// top of the screen scrolls history back in, xterm behaviour).
    pub fn push_line_from_scrollback(&mut self, mut line: Line) {
        self.scroll_down_region(1);
        let top = usize::from(self.scroll_top);
        line.truncate(usize::from(self.cols));
        line.resize(usize::from(self.cols), Cell::default());
        if let Some(last) = line.last_mut() {
            if last.is_wide() {
                *last = Cell::default();
            }
        }
        self.rows[top] = line;
        self.mark_region_dirty();
    }

    /// SU: scroll up (whole screen or region) by `n`.
    pub fn scroll_up(&mut self, n: u16) -> Vec<Line> {
        self.scroll_up_region(n)
    }

    /// SD: scroll down (whole screen or region) by `n`.
    pub fn scroll_down(&mut self, n: u16) {
        self.scroll_down_region(n);
    }

    fn mark_region_dirty(&mut self) {
        self.dirty_all = true;
    }

    /// Insert `n` blank cells at the cursor (ICH).
    pub fn insert_chars(&mut self, n: u16) {
        let row = usize::from(self.cursor.row);
        let col = usize::from(self.cursor.col);
        let cols = usize::from(self.cols);
        let n = usize::from(n).min(cols - col);
        let line = &mut self.rows[row];
        line.splice(col..col, vec![Cell::default(); n]);
        line.truncate(cols);
        self.mark_dirty_row(self.cursor.row);
    }

    /// Delete `n` cells at the cursor (DCH).
    pub fn delete_chars(&mut self, n: u16) {
        let row = usize::from(self.cursor.row);
        let col = usize::from(self.cursor.col);
        let cols = usize::from(self.cols);
        let n = usize::from(n).min(cols - col);
        let line = &mut self.rows[row];
        line.drain(col..col + n);
        line.extend(vec![Cell::default(); n]);
        self.mark_dirty_row(self.cursor.row);
    }

    /// Erase `n` cells from the cursor right (ECH).
    pub fn erase_chars(&mut self, n: u16) {
        let row = usize::from(self.cursor.row);
        let col = usize::from(self.cursor.col);
        let cols = usize::from(self.cols);
        let n = usize::from(n).min(cols - col);
        for c in col..col + n {
            self.rows[row][c] = Cell::default();
        }
        self.mark_dirty_row(self.cursor.row);
    }

    /// Erase in line: 0 = right of cursor, 1 = left of cursor, 2 = all.
    pub fn erase_in_line(&mut self, mode: u8) {
        let row = usize::from(self.cursor.row);
        let col = usize::from(self.cursor.col);
        let cols = usize::from(self.cols);
        let range = match mode {
            0 => col..cols,
            1 => 0..col + 1,
            _ => 0..cols,
        };
        for c in range {
            self.rows[row][c] = Cell::default();
        }
        self.mark_dirty_row(self.cursor.row);
    }

    /// Erase in display: 0 = below cursor, 1 = above cursor, 2 = all,
    /// 3 = all + scrollback (the facade clears scrollback for mode 3).
    pub fn erase_in_display(&mut self, mode: u8) {
        match mode {
            0 => {
                let row = usize::from(self.cursor.row);
                let col = usize::from(self.cursor.col);
                let cols = usize::from(self.cols);
                for c in col..cols {
                    self.rows[row][c] = Cell::default();
                }
                for r in row + 1..usize::from(self.row_count) {
                    for c in 0..cols {
                        self.rows[r][c] = Cell::default();
                    }
                }
            }
            1 => {
                let row = usize::from(self.cursor.row);
                let col = usize::from(self.cursor.col);
                let cols = usize::from(self.cols);
                for r in 0..row {
                    for c in 0..cols {
                        self.rows[r][c] = Cell::default();
                    }
                }
                for c in 0..=col {
                    self.rows[row][c] = Cell::default();
                }
            }
            _ => {
                for r in 0..usize::from(self.row_count) {
                    for c in 0..usize::from(self.cols) {
                        self.rows[r][c] = Cell::default();
                    }
                }
            }
        }
        self.mark_all_dirty();
    }

    /// IL: insert `n` blank lines at the cursor row (region semantics).
    pub fn insert_lines(&mut self, n: u16) {
        let n = n.min(self.scroll_bottom - self.scroll_top + 1);
        let from = self.cursor.row;
        for _ in 0..n {
            let blank = vec![Cell::default(); usize::from(self.cols)];
            self.rows.insert(usize::from(from), blank);
            self.rows.remove(usize::from(self.scroll_bottom) + 1);
        }
        self.mark_all_dirty();
    }

    /// DL: delete `n` lines at the cursor row (region semantics).
    pub fn delete_lines(&mut self, n: u16) -> Vec<Line> {
        let n = n.min(self.scroll_bottom - self.scroll_top + 1);
        let mut removed = Vec::with_capacity(usize::from(n));
        for _ in 0..n {
            removed.push(self.rows.remove(usize::from(self.cursor.row)));
            let blank = vec![Cell::default(); usize::from(self.cols)];
            self.rows.insert(usize::from(self.scroll_bottom), blank);
        }
        self.mark_all_dirty();
        removed
    }

    /// DECALN — fill the screen with `ch` (the "E" test pattern).
    pub fn fill_screen(&mut self, ch: char) {
        for row in &mut self.rows {
            for cell in row.iter_mut() {
                *cell = Cell {
                    ch,
                    fg: 0,
                    bg: 0,
                    flags: CellFlags::empty(),
                    ..Cell::default()
                };
            }
        }
        self.set_cursor(0, 0);
        self.mark_all_dirty();
    }

    /// Resize the grid, preserving content. Lines scrolled out at the top are
    /// returned for scrollback storage. Cursor and scroll region are clamped.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<Vec<Line>, GridError> {
        if !(limits::MIN_COLS..=limits::MAX_COLS).contains(&cols)
            || !(limits::MIN_ROWS..=limits::MAX_ROWS).contains(&rows)
        {
            return Err(GridError::BadSize { cols, rows });
        }
        let old_rows = self.row_count;

        if rows < old_rows {
            // Bottom rows are dropped; keep the top of the screen.
            self.rows.truncate(usize::from(rows));
            // Dropped lines are *not* added to scrollback: history only grows
            // through normal scrolling, which keeps semantics predictable.
        } else if rows > old_rows {
            let blank = vec![Cell::default(); usize::from(cols)];
            for _ in 0..(rows - old_rows) {
                self.rows.push(blank.clone());
            }
        }

        // Normalise every line to the new width.
        for line in &mut self.rows {
            if line.len() > usize::from(cols) {
                line.truncate(usize::from(cols));
            } else {
                line.resize(usize::from(cols), Cell::default());
            }
            // The last cell of a truncated wide glyph must be cleared.
            if cols > 0 {
                let last = &mut line[usize::from(cols) - 1];
                if last.is_wide() {
                    *last = Cell::default();
                }
            }
        }

        self.cols = cols;
        self.row_count = rows;
        self.cursor = self.clamp_cursor(self.cursor.row, self.cursor.col);
        self.saved_cursor = self.clamp_cursor(self.saved_cursor.row, self.saved_cursor.col);
        if self.scroll_bottom >= rows {
            self.scroll_bottom = rows - 1;
        }
        if self.scroll_top > self.scroll_bottom {
            self.scroll_top = 0;
        }
        self.tab_stops = (0..cols).map(|c| c % 8 == 0).collect();
        self.dirty = vec![true; usize::from(rows)];
        self.dirty_all = true;
        self.wrap_pending = false;
        Ok(Vec::new())
    }

    /// Remove the alternate screen's scrollback-relevant state (used on alt
    /// screen exit to clear the alt grid).
    pub fn clear(&mut self) {
        for row in &mut self.rows {
            row.fill(Cell::default());
        }
        self.cursor = Pos::new(0, 0);
        self.wrap_pending = false;
        self.mark_all_dirty();
    }

    /// The text of a row (used by selection/copy). Wide glyphs collapse to
    /// one character; trailing empty cells are trimmed.
    pub fn line_text(&self, row: u16) -> String {
        let mut s = String::new();
        let mut prev_wide = false;
        for cell in self.line(row) {
            if cell.is_wide_cont() || (prev_wide && cell.is_empty()) {
                prev_wide = false;
                continue;
            }
            prev_wide = cell.is_wide();
            if cell.ch != '\0' {
                s.push(cell.ch);
                for i in 0..usize::from(cell.combining_len) {
                    s.push(cell.combining[i]);
                }
            }
        }
        s.trim_end().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs() -> CellAttrs {
        CellAttrs::default()
    }

    #[test]
    fn new_grid_is_bounded_and_blank() {
        let g = Grid::new(80, 24).unwrap();
        assert_eq!(g.cols(), 80);
        assert_eq!(g.rows(), 24);
        assert!(g.line(0).iter().all(super::Cell::is_empty));
        assert_eq!(g.cursor(), Pos::new(0, 0));
    }

    #[test]
    fn out_of_bounds_dimensions_rejected() {
        assert!(Grid::new(0, 24).is_err());
        assert!(Grid::new(80, 0).is_err());
        assert!(Grid::new(5000, 24).is_err());
        assert!(Grid::new(80, 5000).is_err());
    }

    #[test]
    fn print_advances_cursor_and_wraps() {
        let mut g = Grid::new(10, 3).unwrap();
        for _ in 0..10 {
            g.print('a', attrs());
        }
        // Printed 10 chars in 10 cols: cursor on last col, wrap pending.
        assert_eq!(g.cursor(), Pos::new(0, 9));
        assert!(g.wrap_pending);
        g.print('b', attrs());
        // Wrapped to row 1.
        assert_eq!(g.cursor(), Pos::new(1, 1));
        assert_eq!(g.line(0)[0].ch, 'a');
        assert_eq!(g.line(0)[9].ch, 'a');
        assert_eq!(g.line(1)[0].ch, 'b');
    }

    #[test]
    fn index_scrolls_region_and_returns_line() {
        let mut g = Grid::new(4, 3).unwrap();
        g.set_cursor(0, 0);
        for c in 0..4 {
            g.print(char::from(b'a' + c), attrs());
        }
        g.carriage_return();
        g.line_feed();
        g.line_feed();
        // Cursor at bottom row (2). The next index() scrolls the region.
        assert_eq!(g.cursor().row, 2);
        let scrolled = g.index();
        assert!(scrolled.is_some());
        let line = scrolled.unwrap();
        assert_eq!(line[0].ch, 'a');
        assert_eq!(line[3].ch, 'd');
        // All rows are blank after the scroll (the only content was row 0).
        for r in 0..3 {
            assert!(g.line(r).iter().all(super::Cell::is_empty));
        }
    }

    #[test]
    fn wide_chars_occupy_two_cells() {
        let mut g = Grid::new(6, 2).unwrap();
        g.print('界', attrs());
        assert_eq!(g.cursor().col, 2);
        assert!(g.line(0)[0].is_wide());
        assert!(g.line(0)[1].is_wide_cont());
    }

    #[test]
    fn combining_marks_attach_to_previous_cell() {
        let mut g = Grid::new(6, 2).unwrap();
        g.print('e', attrs());
        g.print('\u{0301}', attrs()); // combining acute
        let cell = g.line(0)[0];
        assert_eq!(cell.combining_len, 1);
        assert_eq!(cell.combining[0], '\u{0301}');
    }

    #[test]
    fn erase_in_line_modes() {
        let mut g = Grid::new(5, 2).unwrap();
        for _c in 0..5 {
            g.print('x', attrs());
        }
        g.carriage_return();
        g.erase_in_line(2);
        assert!(g.line(0).iter().all(super::Cell::is_empty));
    }

    #[test]
    fn scroll_region_restricts_scrolling() {
        let mut g = Grid::new(4, 4).unwrap();
        // Region rows 1..=2.
        g.set_scroll_region(1, 2);
        // Fill row 1.
        g.set_cursor(1, 0);
        for _ in 0..4 {
            g.print('r', attrs());
        }
        g.carriage_return();
        // Scroll within the region: line at row 1 leaves the region top.
        let scrolled = g.scroll_up_region(1);
        assert_eq!(scrolled.len(), 1);
        assert_eq!(scrolled[0][0].ch, 'r');
        // Row 0 and row 3 untouched.
        assert!(g.line(0).iter().all(super::Cell::is_empty));
        assert!(g.line(3).iter().all(super::Cell::is_empty));
    }

    #[test]
    fn resize_grows_and_shrinks_safely() {
        let mut g = Grid::new(4, 2).unwrap();
        for _ in 0..4 {
            g.print('a', attrs());
        }
        g.resize(8, 4).unwrap();
        assert_eq!(g.cols(), 8);
        assert_eq!(g.rows(), 4);
        assert_eq!(g.line(0)[3].ch, 'a');
        assert!(g.line(0)[4].is_empty());
        g.resize(2, 2).unwrap();
        assert_eq!(g.cols(), 2);
        assert_eq!(g.line(0)[1].ch, 'a');
        // A wide glyph occupying the full width is preserved; rows are
        // normalised to the new width without orphans.
        let mut g2 = Grid::new(8, 2).unwrap();
        g2.print('界', attrs());
        g2.resize(2, 2).unwrap();
        assert!(g2.line(0)[0].is_wide());
        assert!(g2.line(0)[1].is_wide_cont());
    }

    #[test]
    fn line_text_renders_plain_and_wide() {
        let mut g = Grid::new(10, 2).unwrap();
        g.print('h', attrs());
        g.print('i', attrs());
        assert_eq!(g.line_text(0), "hi");
        // Place the wide glyph after the letters so it does not overwrite
        // them.
        g.set_col(2);
        g.print('界', attrs());
        assert_eq!(g.line_text(0), "hi界");
    }

    #[test]
    fn insert_delete_chars_bound_at_edge() {
        let mut g = Grid::new(4, 1).unwrap();
        for _c in 0..4 {
            g.print('x', attrs());
        }
        g.carriage_return();
        g.delete_chars(2);
        assert_eq!(g.line(0)[0].ch, 'x');
        assert_eq!(g.line(0)[1].ch, 'x');
        assert!(g.line(0)[2].is_empty());
        assert!(g.line(0)[3].is_empty());
        g.set_col(1);
        g.insert_chars(2);
        assert!(g.line(0)[1].is_empty());
        assert!(g.line(0)[2].is_empty());
        assert_eq!(g.line(0)[3].ch, 'x');
    }

    #[test]
    fn tab_stops_default_every_eight() {
        let mut g = Grid::new(24, 2).unwrap();
        g.tab();
        assert_eq!(g.cursor().col, 8);
        g.tab();
        assert_eq!(g.cursor().col, 16);
        g.clear_all_tab_stops();
        g.carriage_return();
        g.tab();
        assert_eq!(g.cursor().col, 23);
    }

    #[test]
    fn fill_screen_decaln() {
        let mut g = Grid::new(4, 2).unwrap();
        g.fill_screen('E');
        for r in 0..2 {
            assert!(g.line(r).iter().all(|c| c.ch == 'E'));
        }
        assert_eq!(g.cursor(), Pos::new(0, 0));
    }

    #[test]
    fn dirty_rows_tracked() {
        let mut g = Grid::new(4, 3).unwrap();
        let (all, _) = g.consume_dirty();
        assert!(all);
        g.set_cursor(1, 0);
        g.print('z', attrs());
        let (all, rows) = g.consume_dirty();
        assert!(!all);
        assert!(rows.contains(&1));
    }

    #[test]
    fn origin_mode_clamps_cursor_to_region() {
        let mut g = Grid::new(4, 4).unwrap();
        g.set_scroll_region(1, 2);
        g.set_origin_mode(true);
        g.set_cursor(5, 0);
        assert!(g.cursor().row <= 2);
        g.set_origin_mode(false);
        g.set_cursor(5, 0);
        assert_eq!(g.cursor().row, 3);
    }
}
