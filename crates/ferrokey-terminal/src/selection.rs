//! Text selection in terminal history (§27): drag, word and line selection.
//!
//! Selection positions live in **document space**: row 0 is the oldest
//! scrollback line, and live-screen rows follow. This keeps a selection
//! anchored to its content even as the viewport scrolls.
//!
//! Selection is purely geometric here; extracting the text ([`crate::terminal`]
//! does that) must never log the content (§28, §79).

/// A position in document space (row 0 = oldest scrollback line).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellPos {
    pub row: i64,
    pub col: i64,
}

impl CellPos {
    pub const fn new(row: i64, col: i64) -> Self {
        CellPos { row, col }
    }
}

/// How the selection was initiated (affects expansion).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionMode {
    #[default]
    Character,
    Word,
    Line,
}

/// An active selection between `anchor` and `end` (inclusive of both).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: CellPos,
    pub end: CellPos,
    pub mode: SelectionMode,
}

impl Selection {
    pub fn new(anchor: CellPos, end: CellPos, mode: SelectionMode) -> Self {
        Selection { anchor, end, mode }
    }

    /// The normalized start of the selection (min corner).
    pub fn start(&self) -> CellPos {
        if self.anchor <= self.end {
            self.anchor
        } else {
            self.end
        }
    }

    /// The normalized end of the selection (max corner).
    pub fn end(&self) -> CellPos {
        if self.anchor <= self.end {
            self.end
        } else {
            self.anchor
        }
    }

    /// Whether a document-space position is inside the selection.
    pub fn contains(&self, pos: CellPos) -> bool {
        let (s, e) = (self.start(), self.end());
        pos.row >= s.row && pos.row <= e.row && pos.col >= s.col && pos.col <= e.col
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.end
    }

    /// The number of lines spanned (0 = single line).
    pub fn span_lines(&self) -> i64 {
        self.end().row - self.start().row
    }
}

/// Whether `ch` is a word character for double-tap word selection.
pub fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '@' | '/' | '~' | ':')
}

/// Expand a character selection to the whole word containing `pos`.
///
/// `line_at` returns the characters of the line at document row `row`.
/// Returns `None` when the line cannot be read (out of range).
pub fn expand_word<F>(pos: CellPos, line_at: &F) -> Option<Selection>
where
    F: Fn(i64) -> Option<Vec<char>>,
{
    let line = line_at(pos.row)?;
    if line.is_empty() {
        return None;
    }
    let col = pos.col.clamp(0, line.len() as i64 - 1);
    let mut start = col;
    let mut end = col;
    if !is_word_char(line[col as usize]) {
        return None;
    }
    while start > 0 && is_word_char(line[(start - 1) as usize]) {
        start -= 1;
    }
    while (end as usize + 1) < line.len() && is_word_char(line[(end + 1) as usize]) {
        end += 1;
    }
    Some(Selection::new(
        CellPos::new(pos.row, start),
        CellPos::new(pos.row, end),
        SelectionMode::Word,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_orders_corners() {
        let s = Selection::new(
            CellPos::new(5, 10),
            CellPos::new(2, 3),
            SelectionMode::Character,
        );
        assert_eq!(s.start(), CellPos::new(2, 3));
        assert_eq!(s.end(), CellPos::new(5, 10));
    }

    #[test]
    fn contains_checks_rectangle() {
        let s = Selection::new(
            CellPos::new(2, 3),
            CellPos::new(5, 10),
            SelectionMode::Character,
        );
        assert!(s.contains(CellPos::new(3, 5)));
        assert!(s.contains(CellPos::new(5, 10)));
        assert!(s.contains(CellPos::new(2, 3)));
        assert!(!s.contains(CellPos::new(6, 5)));
        assert!(!s.contains(CellPos::new(3, 11)));
    }

    #[test]
    fn word_expansion() {
        let line = |_row: i64| Some("hello world, foo_bar-1".chars().collect::<Vec<_>>());
        let sel = expand_word(CellPos::new(0, 6), &line).unwrap();
        assert_eq!(sel.start(), CellPos::new(0, 6));
        assert_eq!(sel.end(), CellPos::new(0, 10));
        // foo_bar-1 spans indices 13..=21 (the '-' and digits are word
        // characters).
        let sel = expand_word(CellPos::new(0, 17), &line).unwrap();
        assert_eq!(sel.start(), CellPos::new(0, 13));
        assert_eq!(sel.end(), CellPos::new(0, 21));
        // A space selects nothing.
        assert!(expand_word(CellPos::new(0, 5), &line).is_none());
    }
}
