//! Viewport state: where the user is looking inside the terminal history
//! (§22–§23, §26).
//!
//! The viewport keeps two pieces of state: a `scroll_offset` (how many lines
//! above the live bottom the user is looking) and a `follow_output` flag.
//! While `follow_output` is true new output stays visible; as soon as the
//! user scrolls upward it turns false and the viewport stops following, so
//! typing or output never yanks the user back to the bottom. A dedicated
//! control ("↓ newest") returns to the live edge.

use crate::limits;

/// How far above the newest output the view is positioned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    /// Lines scrolled back from the live bottom (0 = at newest output).
    pub scroll_offset: usize,
    /// Whether the view follows new output.
    pub follow_output: bool,
    /// Maximum reachable offset (the scrollback length at the time of the
    /// last scroll; the terminal clamps dynamically).
    max_offset: usize,
}

impl Default for Viewport {
    fn default() -> Self {
        Viewport {
            scroll_offset: 0,
            follow_output: true,
            max_offset: 0,
        }
    }
}

impl Viewport {
    pub fn at_bottom(&self) -> bool {
        self.scroll_offset == 0
    }

    /// The terminal informs the viewport how far back it *can* look after
    /// history changed. The offset is clamped; if the user was following
    /// output the offset stays 0.
    pub fn update_bounds(&mut self, scrollback_len: usize) {
        self.max_offset = scrollback_len.min(limits::MAX_SCROLLBACK);
        if self.follow_output {
            self.scroll_offset = 0;
        } else {
            self.scroll_offset = self.scroll_offset.min(self.max_offset);
        }
    }

    /// Scroll up `n` lines into history. Disables follow mode.
    pub fn scroll_up(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        self.follow_output = false;
        self.scroll_offset = (self.scroll_offset + n).min(self.max_offset);
    }

    /// Scroll down `n` lines. Reaching the bottom re-enables follow mode.
    pub fn scroll_down(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        if self.scroll_offset == 0 {
            self.follow_output = true;
        }
    }

    /// Jump to the top of history.
    pub fn scroll_to_top(&mut self) {
        self.follow_output = false;
        self.scroll_offset = self.max_offset;
    }

    /// Return to the newest output ("↓ newest" control).
    pub fn return_to_newest(&mut self) {
        self.scroll_offset = 0;
        self.follow_output = true;
    }

    /// The number of history lines currently visible above the live screen
    /// (bounded by the live screen height).
    pub fn visible_history(&self, live_rows: usize) -> usize {
        self.scroll_offset.min(live_rows.saturating_sub(1))
    }

    pub fn max_offset(&self) -> usize {
        self.max_offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_output_by_default() {
        let v = Viewport::default();
        assert!(v.follow_output);
        assert_eq!(v.scroll_offset, 0);
    }

    #[test]
    fn scrolling_up_disables_follow_and_clamps() {
        let mut v = Viewport::default();
        v.update_bounds(50);
        v.scroll_up(10);
        assert!(!v.follow_output);
        assert_eq!(v.scroll_offset, 10);
        v.scroll_up(10_000);
        assert_eq!(v.scroll_offset, 50);
    }

    #[test]
    fn scrolling_down_to_bottom_resumes_follow() {
        let mut v = Viewport::default();
        v.update_bounds(50);
        v.scroll_up(20);
        v.scroll_down(10);
        assert!(!v.follow_output);
        assert_eq!(v.scroll_offset, 10);
        v.scroll_down(10);
        assert!(v.follow_output);
        assert_eq!(v.scroll_offset, 0);
    }

    #[test]
    fn bounds_update_keeps_position_when_history_shrinks() {
        let mut v = Viewport::default();
        v.update_bounds(100);
        v.scroll_up(80);
        // History shrinks (e.g. ED 3).
        v.update_bounds(40);
        assert_eq!(v.scroll_offset, 40);
        // Following stays glued to the bottom.
        let mut v2 = Viewport::default();
        v2.update_bounds(100);
        v2.update_bounds(0);
        assert_eq!(v2.scroll_offset, 0);
        assert!(v2.follow_output);
    }

    #[test]
    fn return_to_newest_resets() {
        let mut v = Viewport::default();
        v.update_bounds(50);
        v.scroll_up(50);
        v.return_to_newest();
        assert!(v.follow_output);
        assert_eq!(v.scroll_offset, 0);
    }
}
