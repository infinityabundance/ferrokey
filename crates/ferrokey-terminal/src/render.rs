//! Pane rendering (§19, §34–§36, §82): monospaced glyph rasterization into a
//! retained RGBA frame with dirty-row redraw.
//!
//! The renderer uses an **embedded** monospace font (DejaVu Sans Mono,
//! Bitstream Vera licence — `assets/fonts/LICENSE-DejaVu.txt`), so rendering
//! is byte-for-byte deterministic on every system, including the minimal VM
//! courts. Glyph bitmaps are cached per `(char, bold)` at the configured
//! size; per-cell work is an alpha blend, and only dirty rows are repainted
//! into the retained frame. The host presents the frame's `Rc<[u8]>` buffer
//! to its UI layer (Slint `Image::from_rgba8`) — all pixels are opaque.
//!
//! Font fallback: embedded DejaVu covers Latin, Cyrillic, Greek, combining
//! marks and common symbols. Glyphs it lacks are drawn as U+FFFD (the
//! replacement character), never silently dropped or mis-shaped.
//!
//! Overlay UI (the "↓ newest" control and the child-exit bar) is drawn here
//! too; the returned [`UiHitRects`] let the pointer bridge hit-test them
//! without involving the UI framework's own input system.

use crate::grid::{Cell, CellFlags, Color};
use crate::modes::CursorShape;
use crate::selection::Selection;
use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use std::collections::HashMap;
use std::rc::Rc;

/// The default terminal palette (a dark, high-contrast scheme).
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub bg: Color,
    pub fg: Color,
    /// ANSI 16: [black, red, green, yellow, blue, magenta, cyan, white,
    /// bright black, bright red, …].
    pub ansi: [Color; 16],
    pub cursor: Color,
    pub selection: Color,
    pub scrollbar: Color,
    pub button_bg: Color,
    pub button_fg: Color,
    pub exited_bg: Color,
}

impl Default for Palette {
    fn default() -> Self {
        Palette {
            bg: Color::new(0x11, 0x14, 0x18),
            fg: Color::new(0xd8, 0xdc, 0xe2),
            ansi: [
                Color::new(0x28, 0x2c, 0x34), // black
                Color::new(0xe0, 0x6c, 0x75), // red
                Color::new(0x98, 0xc3, 0x79), // green
                Color::new(0xe5, 0xc0, 0x7b), // yellow
                Color::new(0x61, 0xaf, 0xef), // blue
                Color::new(0xc6, 0x78, 0xdd), // magenta
                Color::new(0x56, 0xb6, 0xc2), // cyan
                Color::new(0xdc, 0xdf, 0xe4), // white
                Color::new(0x5c, 0x63, 0x70), // bright black
                Color::new(0xe0, 0x7a, 0x82), // bright red
                Color::new(0xa8, 0xd4, 0x8c), // bright green
                Color::new(0xec, 0xd0, 0x8c), // bright yellow
                Color::new(0x7c, 0xc0, 0xf5), // bright blue
                Color::new(0xd2, 0x8f, 0xe3), // bright magenta
                Color::new(0x6e, 0xc9, 0xd0), // bright cyan
                Color::new(0xff, 0xff, 0xff), // bright white
            ],
            cursor: Color::new(0xd8, 0xdc, 0xe2),
            selection: Color::new(0x26, 0x41, 0x5c),
            scrollbar: Color::new(0x3a, 0x40, 0x49),
            button_bg: Color::new(0x26, 0x30, 0x3a),
            button_fg: Color::new(0x8e, 0xcb, 0xff),
            exited_bg: Color::new(0x3a, 0x2a, 0x2a),
        }
    }
}

impl Palette {
    /// Resolve an SGR colour index (0-255) using the standard xterm model:
    /// 0-15 = ANSI, 16-231 = 6×6×6 cube, 232-255 = grayscale ramp.
    pub fn index(&self, idx: u16) -> Color {
        match idx {
            0..=15 => self.ansi[idx as usize],
            16..=231 => {
                let i = idx - 16;
                let r = (i / 36) % 6;
                let g = (i / 6) % 6;
                let b = i % 6;
                let ramp = [0u8, 95, 135, 175, 215, 255];
                Color::new(ramp[r as usize], ramp[g as usize], ramp[b as usize])
            }
            _ => {
                let v = 8 + (idx - 232) * 10;
                Color::new(v as u8, v as u8, v as u8)
            }
        }
    }

    /// The colour a cell's packed `0xRRGGBB` fg value means (0 = default fg).
    pub fn fg_of(&self, packed: u32) -> Color {
        if packed == 0 {
            self.fg
        } else {
            Color::from_packed(packed)
        }
    }

    pub fn bg_of(&self, packed: u32) -> Color {
        if packed == 0 {
            self.bg
        } else {
            Color::from_packed(packed)
        }
    }

    /// Resolve a cell foreground with the SGR encoding documented in
    /// [`crate::terminal`]: 0 = default, 1..=16 = ANSI 0-15 (1-based), 0x200|n
    /// = 256-colour index, otherwise truecolour 0xRRGGBB.
    pub fn resolve_fg(&self, packed: u32, bold: bool) -> Color {
        match packed {
            0 => self.fg,
            1..=16 => {
                let idx = (packed - 1) as usize;
                if bold && idx < 8 {
                    self.ansi[idx + 8]
                } else {
                    self.ansi[idx]
                }
            }
            0x200..=0x2FF => self.index((packed & 0xFF) as u16),
            other => Color::from_packed(other),
        }
    }

    /// Resolve a cell background (see [`Palette::resolve_fg`]).
    pub fn resolve_bg(&self, packed: u32) -> Color {
        match packed {
            0 => self.bg,
            1..=16 => self.ansi[(packed - 1) as usize],
            0x200..=0x2FF => self.index((packed & 0xFF) as u16),
            other => Color::from_packed(other),
        }
    }
}

/// The computed cell size for a font size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellMetrics {
    pub cell_w: u32,
    pub cell_h: u32,
}

/// Renderer configuration (bounds-checked at construction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RendererConfig {
    /// Target font size in physical px; the cell size is derived from it.
    pub font_size_px: u32,
}

impl Default for RendererConfig {
    fn default() -> Self {
        RendererConfig { font_size_px: 16 }
    }
}

/// One cached glyph bitmap (alpha coverage + placement).
#[derive(Debug, Clone)]
struct CachedGlyph {
    w: u32,
    h: u32,
    x_off: i32,
    y_off: i32,
    /// Alpha coverage 0-255, row-major, `w × h`.
    bitmap: Vec<u8>,
}

/// The retained frame handed to the host.
#[derive(Debug, Clone)]
pub struct RenderedFrame {
    pub width: u32,
    pub height: u32,
    /// RGBA, fully opaque, `width × height × 4` bytes.
    pub buffer: Rc<[u8]>,
    /// Overlay controls the pointer bridge can hit-test.
    pub ui: UiHitRects,
    /// Visible pane dimensions in cells.
    pub visible_rows: u32,
    pub visible_cols: u32,
}

/// A rectangular hit region in physical px.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiButton {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl UiButton {
    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Overlay controls for the current frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UiHitRects {
    /// "↓ newest" pill (shown when scrolled into history).
    pub newest: Option<UiButton>,
    /// "[Restart]" control in the child-exit bar.
    pub restart: Option<UiButton>,
    /// "copy" pill (shown while a selection is active, §28).
    pub copy: Option<UiButton>,
    /// "paste" pill (§29–§30).
    pub paste: Option<UiButton>,
}

/// What to draw: the visible rows plus state that affects painting.
pub struct PaneView<'a> {
    /// Visible lines, top to bottom (scrollback lines first, then live rows).
    pub lines: Vec<&'a [Cell]>,
    /// Document row of `lines[0]` (oldest visible line).
    pub first_document_row: i64,
    /// The live-screen cursor as `(visible_row_index, col, visible, shape,
    /// blink_phase_on)`.
    pub cursor: Option<(usize, usize, bool, CursorShape, bool)>,
    /// Active selection in document space.
    pub selection: Option<&'a Selection>,
    /// Global reverse video (DECSCNM).
    pub reverse_video: bool,
    /// Scrollbar state: `Some((scroll_offset, total_lines))`.
    pub scrollbar: Option<(usize, usize)>,
    /// Child-exit banner: `Some((summary_text, show_restart))`.
    pub exited: Option<(String, bool)>,
}

/// Errors from the renderer.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("embedded font failed to load: {0}")]
    Font(String),
    #[error("invalid renderer configuration: {0}")]
    Config(String),
    #[error("pane dimensions out of bounds")]
    BadSize,
}

/// The pane renderer.
pub struct PaneRenderer {
    font: FontArc,
    bold_font: FontArc,
    palette: Palette,
    font_size: f32,
    cell_w: u32,
    cell_h: u32,
    /// Baseline offset in px from the top of a cell.
    baseline: f32,
    frame: RenderedFrame,
    ui: UiHitRects,
    glyphs: HashMap<(char, bool), CachedGlyph>,
    /// Maximum glyph cache entries before eviction (bounded memory §78).
    glyph_cache_limit: usize,
}

const FONT_REGULAR: &[u8] = include_bytes!("../assets/fonts/DejaVuSansMono.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../assets/fonts/DejaVuSansMono-Bold.ttf");

impl PaneRenderer {
    pub fn new(config: RendererConfig, palette: Palette) -> Result<Self, RenderError> {
        use crate::limits;
        if !(limits::MIN_CELL_PX..=limits::MAX_CELL_PX).contains(&config.font_size_px) {
            return Err(RenderError::Config(format!(
                "font size {} out of bounds",
                config.font_size_px
            )));
        }
        let font = FontArc::try_from_vec(FONT_REGULAR.to_vec())
            .map_err(|e| RenderError::Font(e.to_string()))?;
        let bold_font = FontArc::try_from_vec(FONT_BOLD.to_vec())
            .map_err(|e| RenderError::Font(e.to_string()))?;
        let font_size = config.font_size_px as f32;
        let (cell_h, baseline) = line_metrics(&font, font_size);
        let cell_w = advance_px(&font, font_size).max(1);
        Ok(PaneRenderer {
            font,
            bold_font,
            palette,
            font_size,
            cell_w,
            cell_h,
            baseline,
            frame: RenderedFrame {
                width: 0,
                height: 0,
                buffer: Rc::from(vec![0u8; 0]),
                ui: UiHitRects::default(),
                visible_rows: 0,
                visible_cols: 0,
            },
            ui: UiHitRects::default(),
            glyphs: HashMap::new(),
            glyph_cache_limit: 8192,
        })
    }

    /// The cell size in physical px for the configured font size.
    pub fn cell_metrics(&self) -> CellMetrics {
        CellMetrics {
            cell_w: self.cell_w,
            cell_h: self.cell_h,
        }
    }

    /// Resize the pane (physical px). Allocates a fresh opaque frame.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        use crate::limits;
        if width < limits::MIN_PANE_PX || height < limits::MIN_PANE_PX {
            return Err(RenderError::BadSize);
        }
        if width == self.frame.width && height == self.frame.height {
            return Ok(());
        }
        let len = (width as usize) * (height as usize) * 4;
        let mut buffer = vec![0u8; len];
        let (bg_r, bg_g, bg_b) = (self.palette.bg.r, self.palette.bg.g, self.palette.bg.b);
        for px in buffer.chunks_exact_mut(4) {
            px[0] = bg_r;
            px[1] = bg_g;
            px[2] = bg_b;
            px[3] = 255;
        }
        self.frame = RenderedFrame {
            width,
            height,
            buffer: Rc::from(buffer),
            ui: UiHitRects::default(),
            visible_rows: 0,
            visible_cols: 0,
        };
        self.ui = UiHitRects::default();
        Ok(())
    }

    /// Render (or re-render dirty rows of) the pane. `dirty` is a set of
    /// visible-row indices, or `None` for all rows. Returns the frame.
    pub fn render(&mut self, view: &PaneView<'_>, dirty: Option<&[u16]>) -> &RenderedFrame {
        if self.frame.width == 0 || self.frame.height == 0 {
            return &self.frame;
        }
        let visible_rows = self.frame.height / self.cell_h;
        let visible_cols = self.frame.width / self.cell_w;
        if visible_rows == 0 || visible_cols == 0 {
            return &self.frame;
        }

        let repaint: Vec<u16> = match dirty {
            None => (0..visible_rows as u16).collect(),
            Some(rows) => rows
                .iter()
                .copied()
                .filter(|r| u32::from(*r) < visible_rows)
                .collect(),
        };

        for row in repaint {
            let doc_row = view.first_document_row + i64::from(row);
            let line = view.lines.get(row as usize).copied();
            self.paint_row(row, doc_row, line, view);
        }

        // Overlay UI (also paints the exit bar into the frame).
        self.ui = self.compute_ui(view);
        self.frame.ui = self.ui;
        self.frame.visible_rows = visible_rows;
        self.frame.visible_cols = visible_cols;
        &self.frame
    }

    /// Full repaint of everything (used on resize/viewport changes).
    pub fn repaint_all(&mut self, view: &PaneView<'_>) -> &RenderedFrame {
        self.render(view, None)
    }

    /// The overlay hit regions of the most recent frame (for the pointer
    /// bridge; cheap, no repaint).
    pub fn frame_ui(&self) -> UiHitRects {
        self.frame.ui
    }

    /// Paint one visible row at document row `doc_row`.
    fn paint_row(
        &mut self,
        visible_row: u16,
        doc_row: i64,
        line: Option<&[Cell]>,
        view: &PaneView,
    ) {
        let line = match line {
            Some(l) => l,
            None => return,
        };
        let selection = view.selection.copied();
        let cursor = view.cursor;

        for (col, cell) in line.iter().enumerate() {
            if col as u32 >= self.frame.width / self.cell_w {
                break;
            }
            if cell.is_wide_cont() {
                continue;
            }
            let selected = selection
                .is_some_and(|s| s.contains(crate::selection::CellPos::new(doc_row, col as i64)));
            let is_cursor =
                cursor.is_some_and(|(r, c, _, _, _)| r == visible_row as usize && c == col);
            self.paint_cell(visible_row, col as u16, cell, selected, is_cursor, view);
        }
    }

    fn paint_cell(
        &mut self,
        visible_row: u16,
        col: u16,
        cell: &Cell,
        selected: bool,
        is_cursor: bool,
        view: &PaneView,
    ) {
        let x0 = u32::from(col) * self.cell_w;
        let y0 = u32::from(visible_row) * self.cell_h;
        let (cw, ch) = (self.cell_w, self.cell_h);

        let mut fg = self
            .palette
            .resolve_fg(cell.fg, cell.flags.contains(CellFlags::BOLD));
        let mut bg = self.palette.resolve_bg(cell.bg);

        // DECSCNM global reverse video.
        if view.reverse_video {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.flags.contains(CellFlags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if selected {
            bg = bg.blend(self.palette.selection, 110);
        }

        // Cursor: block = reverse video on the cell; underline/bar drawn as
        // overlays afterwards.
        let (cursor_visible, cursor_shape, blink_on) = match view.cursor {
            Some((r, c, visible, shape, blink))
                if r == visible_row as usize && c == col as usize =>
            {
                (visible, shape, blink)
            }
            _ => (false, CursorShape::Block, true),
        };
        if is_cursor && cursor_visible {
            match cursor_shape {
                CursorShape::Block | CursorShape::BlinkingBlock | CursorShape::SteadyBlock
                    if (matches!(cursor_shape, CursorShape::Block) || blink_on) =>
                {
                    std::mem::swap(&mut fg, &mut bg);
                }
                _ => {}
            }
        }

        self.fill_rect(x0, y0, cw, ch, bg);

        if is_cursor && cursor_visible {
            let draw = match cursor_shape {
                CursorShape::BlinkingUnderline
                | CursorShape::BlinkingBar
                | CursorShape::BlinkingBlock => blink_on,
                _ => true,
            };
            if draw {
                match cursor_shape {
                    CursorShape::SteadyUnderline | CursorShape::BlinkingUnderline => {
                        self.fill_rect(x0, y0 + ch - 2, cw, 2, self.palette.cursor);
                    }
                    CursorShape::SteadyBar | CursorShape::BlinkingBar => {
                        self.fill_rect(x0, y0, 2, ch, self.palette.cursor);
                    }
                    _ => {}
                }
            }
        }

        if cell.ch != '\0' && cell.ch != ' ' {
            self.blit_char(
                x0,
                y0,
                cell.ch,
                fg,
                cell.flags.contains(CellFlags::BOLD),
                cell.flags.contains(CellFlags::ITALIC),
            );
        }
        for i in 0..usize::from(cell.combining_len) {
            self.blit_char(
                x0,
                y0,
                cell.combining[i],
                fg,
                cell.flags.contains(CellFlags::BOLD),
                false,
            );
        }

        if cell.flags.contains(CellFlags::UNDERLINE) && !(is_cursor && cursor_visible) {
            self.fill_rect(x0, y0 + ch - 2, cw, 1, fg);
        }
        if cell.flags.contains(CellFlags::STRIKE) {
            self.fill_rect(x0, y0 + ch / 2, cw, 1, fg);
        }
    }

    /// Fill a rectangle with a colour (clipped to the frame).
    #[allow(clippy::many_single_char_names)]
    fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: Color) {
        let (fw, fh) = (self.frame.width, self.frame.height);
        let x1 = x.saturating_add(w).min(fw);
        let y1 = y.saturating_add(h).min(fh);
        let (r, g, b) = (color.r, color.g, color.b);
        let Some(buffer) = Rc::get_mut(&mut self.frame.buffer) else {
            return;
        };
        for yy in y..y1 {
            let row = yy as usize * fw as usize;
            for xx in x..x1 {
                let i = (row + xx as usize) * 4;
                buffer[i] = r;
                buffer[i + 1] = g;
                buffer[i + 2] = b;
                buffer[i + 3] = 255;
            }
        }
    }

    /// Blend a glyph bitmap into the frame at the cell origin (with optional
    /// horizontal shear for italics).
    #[allow(clippy::many_single_char_names)]
    fn blit_char(&mut self, x0: u32, y0: u32, ch: char, color: Color, bold: bool, italic: bool) {
        let glyph = self.glyph(ch, bold);
        if glyph.w == 0 {
            return;
        }
        let (fw, fh) = (self.frame.width, self.frame.height);
        let Some(buffer) = Rc::get_mut(&mut self.frame.buffer) else {
            return;
        };
        let (r, g, b) = (color.r, color.g, color.b);
        // Glyph baseline: y0 + baseline; the bitmap's top is baseline +
        // bounds.min.y (negative). Centered horizontally in the cell.
        let cx = x0 as i32 + self.cell_w as i32 / 2 - glyph.w as i32 / 2 + glyph.x_off;
        let cy = y0 as i32 + self.baseline as i32 + glyph.y_off;
        let skew = if italic { 0.22 } else { 0.0 };
        for gy in 0..glyph.h {
            let row_shift = if skew > 0.0 {
                let center = glyph.h as f32 / 2.0;
                ((gy as f32 - center) * skew).round() as i32
            } else {
                0
            };
            for gx in 0..glyph.w {
                let alpha = glyph.bitmap[(gy * glyph.w + gx) as usize];
                if alpha == 0 {
                    continue;
                }
                let px = cx + gx as i32 + row_shift;
                let py = cy + gy as i32;
                if px < 0
                    || py < 0
                    || px >= i32::try_from(fw).unwrap_or(i32::MAX)
                    || py >= i32::try_from(fh).unwrap_or(i32::MAX)
                {
                    continue;
                }
                let i = ((py as usize) * fw as usize + px as usize) * 4;
                let a = u32::from(alpha);
                let inv = 255 - a;
                buffer[i] = ((u32::from(buffer[i]) * inv + u32::from(r) * a) / 255) as u8;
                buffer[i + 1] = ((u32::from(buffer[i + 1]) * inv + u32::from(g) * a) / 255) as u8;
                buffer[i + 2] = ((u32::from(buffer[i + 2]) * inv + u32::from(b) * a) / 255) as u8;
            }
        }
    }

    /// Get (or rasterize) the glyph for a char.
    fn glyph(&mut self, ch: char, bold: bool) -> CachedGlyph {
        if let Some(g) = self.glyphs.get(&(ch, bold)) {
            return g.clone();
        }
        let font = if bold { &self.bold_font } else { &self.font };
        let scale = PxScale::from(self.font_size);
        let fallback = |f: &FontArc| f.outline_glyph(f.glyph_id('\u{FFFD}').with_scale(scale));
        let outlined = font
            .outline_glyph(font.glyph_id(ch).with_scale(scale))
            .or_else(|| fallback(font));
        let Some(outlined) = outlined else {
            let cached = CachedGlyph {
                w: 0,
                h: 0,
                x_off: 0,
                y_off: 0,
                bitmap: Vec::new(),
            };
            self.cache_glyph((ch, bold), cached.clone());
            return cached;
        };
        let bounds = outlined.px_bounds();
        let w = bounds.width().ceil().max(0.0) as u32;
        let h = bounds.height().ceil().max(0.0) as u32;
        let mut bitmap = vec![0u8; (w * h) as usize];
        // ab_glyph ≥ 0.2.2x reports draw coordinates already relative to
        // `px_bounds.min`, so no offset subtraction is applied here.
        outlined.draw(|x, y, coverage| {
            if x < w && y < h {
                let i = (y * w + x) as usize;
                bitmap[i] = (coverage * 255.0) as u8;
            }
        });
        let cached = CachedGlyph {
            w,
            h,
            x_off: bounds.min.x as i32,
            y_off: bounds.min.y as i32,
            bitmap,
        };
        self.cache_glyph((ch, bold), cached.clone());
        cached
    }

    fn cache_glyph(&mut self, key: (char, bool), glyph: CachedGlyph) {
        if self.glyphs.len() >= self.glyph_cache_limit {
            self.glyphs.clear();
        }
        self.glyphs.insert(key, glyph);
    }

    /// Compute overlay hit regions and paint the overlays.
    fn compute_ui(&mut self, view: &PaneView) -> UiHitRects {
        let mut ui = UiHitRects::default();
        // "↓ newest" pill when scrolled into history.
        if let Some((offset, _)) = view.scrollbar {
            if offset > 0 {
                let w = 96u32.min(self.frame.width.saturating_sub(16));
                let h = 26u32;
                let x = self.frame.width.saturating_sub(w + 8);
                let y = self.frame.height.saturating_sub(h + 8);
                self.draw_pill(
                    "↓ newest",
                    self.palette.button_fg,
                    self.palette.button_bg,
                    x,
                    y,
                    w,
                    h,
                );
                ui.newest = Some(UiButton { x, y, w, h });
            }
        }
        // Copy + paste pills at the bottom left (§27–§30). Copy is only
        // offered while a selection exists.
        let mut bx = 8u32;
        if view.selection.is_some() {
            let w = 64u32;
            let h = 26u32;
            let y = self.frame.height.saturating_sub(h + 8);
            self.draw_pill(
                "copy",
                self.palette.button_fg,
                self.palette.button_bg,
                bx,
                y,
                w,
                h,
            );
            ui.copy = Some(UiButton { x: bx, y, w, h });
            bx += w + 6;
        }
        {
            let w = 64u32;
            let h = 26u32;
            let y = self.frame.height.saturating_sub(h + 8);
            self.draw_pill(
                "paste",
                self.palette.button_fg,
                self.palette.button_bg,
                bx,
                y,
                w,
                h,
            );
            ui.paste = Some(UiButton { x: bx, y, w, h });
        }
        // Child-exit bar with [Restart].
        if let Some((text, show_restart)) = &view.exited {
            let bar_h = 28u32;
            let x = 8u32;
            let w = self.frame.width.saturating_sub(16);
            self.draw_pill(
                &format!(" {text}"),
                self.palette.fg,
                self.palette.exited_bg,
                x,
                8,
                w,
                bar_h,
            );
            if *show_restart {
                let rw = 96u32.min(w / 2);
                let rx = x + w - rw;
                self.draw_pill(
                    " [Restart]",
                    self.palette.button_fg,
                    self.palette.button_bg,
                    rx,
                    8,
                    rw,
                    bar_h,
                );
                ui.restart = Some(UiButton {
                    x: rx,
                    y: 8,
                    w: rw,
                    h: bar_h,
                });
            }
        }
        ui
    }

    /// Draw a labelled pill: background + centered text.
    #[allow(clippy::too_many_arguments)]
    fn draw_pill(&mut self, text: &str, fg: Color, bg: Color, x: u32, y: u32, w: u32, h: u32) {
        self.fill_rect(x, y, w, h, bg);
        let chars: Vec<char> = text.chars().collect();
        let mut text_w: u32 = 0;
        for ch in &chars {
            text_w = text_w.saturating_add(self.glyph(*ch, false).w.max(2));
        }
        if text_w >= w {
            return;
        }
        let x0 = x + (w - text_w) / 2;
        let mut cx = x0;
        for ch in chars {
            self.blit_char(cx, y, ch, fg, false, false);
            cx = cx.saturating_add(self.glyph(ch, false).w.max(2));
        }
    }
}

/// Line height in px and baseline offset from the cell top.
fn line_metrics(font: &FontArc, size_px: f32) -> (u32, f32) {
    let scaled = font.as_scaled(size_px);
    let asc = scaled.ascent();
    let desc = -scaled.descent();
    let line = asc + desc;
    let cell_h = line.ceil() as u32 + 1;
    // Baseline sits so that (ascent + descent) fits, centered vertically.
    let baseline = (cell_h as f32 - line) / 2.0 + asc;
    (cell_h, baseline)
}

/// The advance width in px (monospace: any glyph works; use 'M').
fn advance_px(font: &FontArc, size_px: f32) -> u32 {
    let scaled = font.as_scaled(size_px);
    scaled.h_advance(font.glyph_id('M')).round().max(1.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renderer() -> PaneRenderer {
        PaneRenderer::new(RendererConfig::default(), Palette::default()).unwrap()
    }

    #[test]
    fn fonts_load_and_metrics_are_sane() {
        let r = renderer();
        let m = r.cell_metrics();
        assert!(m.cell_w >= 4 && m.cell_w <= 24);
        assert!(m.cell_h >= 8 && m.cell_h <= 32);
        assert!(m.cell_h >= m.cell_w, "cells are at least as tall as wide");
    }

    #[test]
    fn out_of_bounds_font_size_rejected() {
        assert!(
            PaneRenderer::new(RendererConfig { font_size_px: 1000 }, Palette::default()).is_err()
        );
    }

    #[test]
    fn resize_allocates_opaque_frame() {
        let mut r = renderer();
        r.resize(320, 200).unwrap();
        let frame = r.frame.clone();
        assert_eq!(frame.width, 320);
        assert_eq!(frame.height, 200);
        assert_eq!(frame.buffer.len(), 320 * 200 * 4);
        for px in frame.buffer.chunks_exact(4) {
            assert_eq!(px[3], 255);
        }
    }

    #[test]
    fn render_produces_frame_with_newest_button() {
        let mut r = renderer();
        r.resize(400, 300).unwrap();
        let cells = [Cell::default(); 40];
        let view = PaneView {
            lines: vec![&cells[..]],
            first_document_row: 0,
            cursor: None,
            selection: None,
            reverse_video: false,
            scrollbar: Some((5, 100)),
            exited: None,
        };
        let frame = r.render(&view, None);
        assert_eq!(frame.width, 400);
        assert_eq!(frame.height, 300);
        assert!(frame.ui.newest.is_some());
        // No restart control when the child is alive.
        assert!(frame.ui.restart.is_none());
    }

    #[test]
    fn draw_text_row_changes_pixels() {
        let mut r = renderer();
        r.resize(200, 100).unwrap();
        let cell = Cell {
            ch: 'x',
            fg: 0,
            bg: 0,
            flags: CellFlags::empty(),
            combining: ['\0'; 2],
            combining_len: 0,
        };
        let cells = [cell];
        let view = PaneView {
            lines: vec![&cells[..]],
            first_document_row: 0,
            cursor: None,
            selection: None,
            reverse_video: false,
            scrollbar: None,
            exited: None,
        };
        let _ = r.render(&view, None);
        let bg = Palette::default().bg;
        let painted = r
            .frame
            .buffer
            .chunks_exact(4)
            .any(|px| px[0] != bg.r || px[1] != bg.g || px[2] != bg.b);
        assert!(painted, "glyph pixels must differ from the background");
    }

    #[test]
    fn palette_index_matches_xterm() {
        let p = Palette::default();
        assert_eq!(p.index(0), p.ansi[0]);
        assert_eq!(p.index(9), p.ansi[9]);
        assert_eq!(p.index(16), Color::new(0, 0, 0));
        assert_eq!(p.index(196), Color::new(255, 0, 0));
        assert_eq!(p.index(231), Color::new(255, 255, 255));
        assert_eq!(p.index(232), Color::new(8, 8, 8));
        assert_eq!(p.index(255), Color::new(238, 238, 238));
    }

    #[test]
    fn exited_bar_has_restart_rect() {
        let mut r = renderer();
        r.resize(400, 300).unwrap();
        let cells = [Cell::default(); 40];
        let view = PaneView {
            lines: vec![&cells[..]],
            first_document_row: 0,
            cursor: None,
            selection: None,
            reverse_video: false,
            scrollbar: None,
            exited: Some(("exited with status 0".into(), true)),
        };
        let frame = r.render(&view, None);
        assert!(frame.ui.restart.is_some());
    }

    #[test]
    fn dirty_rows_only_repaint_those_rows() {
        let mut r = renderer();
        r.resize(100, 100).unwrap();
        let cell = Cell {
            ch: 'a',
            fg: 0,
            bg: 0,
            flags: CellFlags::empty(),
            combining: ['\0'; 2],
            combining_len: 0,
        };
        let cells = [cell];
        let view = PaneView {
            lines: vec![&cells[..]],
            first_document_row: 0,
            cursor: None,
            selection: None,
            reverse_video: false,
            scrollbar: None,
            exited: None,
        };
        let _ = r.render(&view, None);
        // Repaint only row 0; the frame must still contain the glyph.
        let _ = r.render(&view, Some(&[0]));
        let bg = Palette::default().bg;
        let painted = r
            .frame
            .buffer
            .chunks_exact(4)
            .any(|px| px[0] != bg.r || px[1] != bg.g || px[2] != bg.b);
        assert!(painted);
    }

    #[test]
    fn wide_and_combining_cells_render() {
        let mut r = renderer();
        r.resize(200, 100).unwrap();
        let mut wide = Cell {
            ch: '界',
            ..Cell::default()
        };
        wide.flags.set(CellFlags::WIDE, true);
        let mut cont = Cell::default();
        cont.flags.set(CellFlags::WIDE_CONT, true);
        let wide_cells = [wide, cont];
        let view = PaneView {
            lines: vec![&wide_cells[..]],
            first_document_row: 0,
            cursor: None,
            selection: None,
            reverse_video: false,
            scrollbar: None,
            exited: None,
        };
        let _ = r.render(&view, None);
        // Must not panic; the frame has non-background pixels.
        let bg = Palette::default().bg;
        let painted = r
            .frame
            .buffer
            .chunks_exact(4)
            .any(|px| px[0] != bg.r || px[1] != bg.g || px[2] != bg.b);
        assert!(painted);
    }
}
