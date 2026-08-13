//! Bounded scrollback: a ring of historical lines (§17–§18, §78).
//!
//! The scrollback is a **ring buffer** with a hard `max_lines` bound. Lines
//! pushed beyond the bound evict the oldest, so hostile or verbose output can
//! never grow the buffer without limit. Lines are stored by value (each row
//! is `cols` cells, bounded by construction in [`crate::grid::Grid`]).
//!
//! Scrollback is conceptually separate from the live grid and from the
//! alternate screen: output produced while an alternate screen is active
//! never enters the scrollback (§20–§21).

use crate::grid::Line;
use crate::limits;
use std::collections::VecDeque;

/// A bounded ring of terminal history lines.
#[derive(Debug, Clone)]
pub struct Scrollback {
    lines: VecDeque<Line>,
    max_lines: usize,
    /// Cells per line; new lines are padded/truncated to this width.
    cols: usize,
}

/// Errors from scrollback construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScrollbackError {
    #[error("scrollback capacity {requested} out of bounds (min {min}, max {max})")]
    BadCapacity {
        requested: usize,
        min: usize,
        max: usize,
    },
}

impl Scrollback {
    /// Create a scrollback with `max_lines` capacity and `cols` cells per
    /// line. Both are bounds-checked (§25, §78).
    pub fn new(max_lines: usize, cols: u16) -> Result<Self, ScrollbackError> {
        if !(limits::MIN_SCROLLBACK..=limits::MAX_SCROLLBACK).contains(&max_lines) {
            return Err(ScrollbackError::BadCapacity {
                requested: max_lines,
                min: limits::MIN_SCROLLBACK,
                max: limits::MAX_SCROLLBACK,
            });
        }
        if cols == 0 {
            return Err(ScrollbackError::BadCapacity {
                requested: max_lines,
                min: limits::MIN_SCROLLBACK,
                max: limits::MAX_SCROLLBACK,
            });
        }
        Ok(Scrollback {
            lines: VecDeque::with_capacity(max_lines),
            max_lines,
            cols: usize::from(cols),
        })
    }

    pub fn max_lines(&self) -> usize {
        self.max_lines
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.lines.capacity()
    }

    /// Push a line into history. The line is normalised to the current cell
    /// width (padding or truncating — truncated wide glyphs are cleared at
    /// the edge). Returns `true` if the oldest line was evicted.
    pub fn push(&mut self, mut line: Line) -> bool {
        line.truncate(self.cols);
        line.resize(self.cols, crate::grid::Cell::default());
        if let Some(last) = line.last_mut() {
            if last.is_wide() {
                *last = crate::grid::Cell::default();
            }
        }
        let evicted = self.lines.len() == self.max_lines;
        if evicted {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
        evicted
    }

    /// Pop the most recent line (restored by a scroll-down at the top of the
    /// live screen, matching xterm's history restore).
    pub fn pop_newest(&mut self) -> Option<Line> {
        self.lines.pop_back()
    }

    /// Remove the oldest line (used when the live screen is resized taller
    /// and history lines are pulled back).
    pub fn pop_oldest(&mut self) -> Option<Line> {
        self.lines.pop_front()
    }

    /// Line at index `i` from the **bottom** of history (0 = newest).
    pub fn from_bottom(&self, i: usize) -> Option<&Line> {
        if i >= self.lines.len() {
            return None;
        }
        self.lines.get(self.lines.len() - 1 - i)
    }

    /// Line at index `i` from the **top** of history (0 = oldest).
    pub fn from_top(&self, i: usize) -> Option<&Line> {
        self.lines.get(i)
    }

    /// Erase everything (ED 3 clears scrollback).
    pub fn clear(&mut self) {
        self.lines.clear();
    }

    /// Iterate from oldest to newest.
    pub fn iter(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter()
    }

    /// Drop a prefix of the oldest lines (used when the live grid grows and
    /// the window of history that is visible must shift).
    pub fn drain_oldest(&mut self, n: usize) {
        let n = n.min(self.lines.len());
        for _ in 0..n {
            self.lines.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{CellAttrs, Grid};

    fn line_of(text: &str) -> Line {
        let mut g = Grid::new(8, 1).unwrap();
        for ch in text.chars() {
            g.print(ch, CellAttrs::default());
        }
        g.line(0).clone()
    }

    #[test]
    fn capacity_bounds_enforced() {
        assert!(Scrollback::new(0, 80).is_err());
        assert!(Scrollback::new(limits::MIN_SCROLLBACK - 1, 80).is_err());
        assert!(Scrollback::new(limits::MAX_SCROLLBACK + 1, 80).is_err());
        let s = Scrollback::new(1000, 80).unwrap();
        assert_eq!(s.max_lines(), 1000);
    }

    #[test]
    fn ring_evicts_oldest() {
        let mut s = Scrollback::new(100, 8).unwrap();
        for i in 0..105 {
            s.push(line_of(&format!("l{i:03}")));
        }
        assert_eq!(s.len(), 100);
        // The oldest five are gone; the newest 100 remain (l005..l104).
        assert_eq!(s.from_top(0).unwrap()[1].ch, '0');
        assert_eq!(s.from_top(0).unwrap()[3].ch, '5');
        assert_eq!(s.from_top(99).unwrap()[1].ch, '1');
        assert_eq!(s.from_top(99).unwrap()[3].ch, '4');
        assert_eq!(s.from_bottom(0).unwrap()[1].ch, '1');
        assert_eq!(s.from_bottom(0).unwrap()[3].ch, '4');
    }

    #[test]
    fn push_normalises_line_width() {
        let mut s = Scrollback::new(100, 4).unwrap();
        s.push(line_of("abcdefgh"));
        assert_eq!(s.from_bottom(0).unwrap().len(), 4);
        let mut s2 = Scrollback::new(100, 12).unwrap();
        s2.push(line_of("ab"));
        assert_eq!(s2.from_bottom(0).unwrap().len(), 12);
        assert!(s2.from_bottom(0).unwrap()[2].is_empty());
    }

    #[test]
    fn pop_newest_restores_lifo() {
        let mut s = Scrollback::new(100, 8).unwrap();
        s.push(line_of("a"));
        s.push(line_of("b"));
        assert_eq!(s.pop_newest().unwrap()[0].ch, 'b');
        assert_eq!(s.pop_newest().unwrap()[0].ch, 'a');
        assert!(s.pop_newest().is_none());
    }

    #[test]
    fn wide_char_at_eviction_edge_is_cleared() {
        let mut s = Scrollback::new(100, 2).unwrap();
        // '界' needs 2 cells; the line normalises to 2 cells exactly.
        let mut g = Grid::new(4, 1).unwrap();
        g.print('界', CellAttrs::default());
        s.push(g.line(0).clone());
        let line = s.from_bottom(0).unwrap();
        assert!(!line[1].is_wide_cont() || line[1].is_empty());
        assert!(line[1].is_empty(), "continuation cell cleared at width 2");
    }

    #[test]
    fn clear_empties() {
        let mut s = Scrollback::new(100, 8).unwrap();
        s.push(line_of("a"));
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
    }
}
