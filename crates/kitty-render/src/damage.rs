// SPDX-License-Identifier: GPL-3.0-or-later
//! Damage tracking for the CPU renderer.
//!
//! `kitty-screen` exposes dirty rows, the scroll position and the cursor but
//! no renderer-specific generation counter. [`DamageTracker`] keeps its own
//! snapshot and generation number, so the screen crate does not need to grow
//! renderer state. The tracker reports:
//!
//! * coalesced, half-open dirty line ranges;
//! * a full-redraw flag for the first frame, resizes, alternate-screen
//!   switches, scrolls and explicit invalidation;
//! * cursor-only movement (including the old and new cursor rows);
//! * a monotonic generation that only advances when something changed.

use kitty_screen::{CursorShape, Screen};

/// A half-open range of screen rows, `start..end`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LineRange {
    pub start: usize,
    pub end: usize,
}

impl LineRange {
    #[inline]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    #[inline]
    pub const fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.start >= self.end
    }

    #[inline]
    pub const fn contains(self, line: usize) -> bool {
        line >= self.start && line < self.end
    }
}

/// The cursor state observed by the tracker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorState {
    pub x: u32,
    pub y: u32,
    pub shape: CursorShape,
    pub visible: bool,
    pub blinking: bool,
}

/// Why a whole frame must be redrawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FullRedrawReason {
    /// The tracker has not seen this screen before.
    FirstFrame,
    /// The number of rows or columns changed.
    Resize,
    /// The main/alternate screen changed.
    AlternateScreen,
    /// The viewport scrolled or a line was added to scrollback.
    Scroll,
    /// A caller explicitly requested a full redraw.
    Explicit,
}

/// The damage for one frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Damage {
    pub generation: u64,
    pub full_redraw: bool,
    pub full_redraw_reason: Option<FullRedrawReason>,
    pub lines: Vec<LineRange>,
    pub cursor_moved: bool,
    pub cursor_only: bool,
    pub old_cursor: Option<CursorState>,
    pub new_cursor: Option<CursorState>,
}

impl Damage {
    /// True when there is nothing to draw.
    #[inline]
    pub fn is_empty(&self) -> bool {
        !self.full_redraw && self.lines.is_empty() && !self.cursor_moved
    }

    /// True when any line in the range is damaged.
    pub fn touches_line(&self, line: usize) -> bool {
        self.full_redraw || self.lines.iter().any(|r| r.contains(line))
    }

    /// Number of damaged rows, ignoring full redraws.
    pub fn damaged_line_count(&self) -> usize {
        self.lines.iter().map(|r| r.len()).sum()
    }
}

/// Tracks differences between successive [`Screen`] observations.
#[derive(Clone, Debug)]
pub struct DamageTracker {
    initialized: bool,
    columns: u32,
    rows: u32,
    alt_screen: bool,
    scrolled_by: u32,
    history_lines: u64,
    cursor: Option<CursorState>,
    generation: u64,
    force_full: bool,
}

impl Default for DamageTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl DamageTracker {
    pub const fn new() -> Self {
        Self {
            initialized: false,
            columns: 0,
            rows: 0,
            alt_screen: false,
            scrolled_by: 0,
            history_lines: 0,
            cursor: None,
            generation: 0,
            force_full: false,
        }
    }

    /// The generation of the most recently produced [`Damage`].
    #[inline]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Request a full redraw on the next update.
    pub fn invalidate(&mut self) {
        self.force_full = true;
    }

    /// Forget the snapshot. The next update is a first frame.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Stop tracking this screen and make the next update a first frame.
    pub fn detach(&mut self) {
        self.initialized = false;
        self.force_full = true;
    }

    /// Alias for [`update`](Self::update).
    pub fn track(&mut self, screen: &Screen) -> Damage {
        self.update(screen)
    }

    /// Observe a screen and compute the damage since the previous call.
    ///
    /// The caller normally calls [`Screen::reset_dirty`] after drawing a
    /// frame; this method only reads the screen and never mutates it.
    pub fn update(&mut self, screen: &Screen) -> Damage {
        let cursor = CursorState {
            x: screen.cursor.x,
            y: screen.cursor.y,
            shape: screen.cursor.shape,
            visible: screen.is_cursor_visible(),
            blinking: screen.cursor.blinking(),
        };

        let mut reason = None;
        if !self.initialized {
            reason = Some(FullRedrawReason::FirstFrame);
        } else if self.columns != screen.columns || self.rows != screen.lines {
            reason = Some(FullRedrawReason::Resize);
        } else if self.alt_screen != screen.is_using_alternate_linebuf() {
            reason = Some(FullRedrawReason::AlternateScreen);
        } else if screen.scroll_changed
            || self.scrolled_by != screen.scrolled_by
            || self.history_lines != screen.history_line_added_count
        {
            reason = Some(FullRedrawReason::Scroll);
        }
        if reason.is_none() && self.force_full {
            reason = Some(FullRedrawReason::Explicit);
        }

        let cursor_moved = self.cursor != Some(cursor) && self.initialized;
        let old_cursor = self.cursor;

        let dirty = if reason.is_some() {
            Vec::new()
        } else {
            screen.dirty_lines()
        };
        let mut lines = if reason.is_some() {
            Vec::new()
        } else {
            coalesce_lines(&dirty)
        };

        if !self.initialized {
            self.generation = 1;
        } else if reason.is_some() || !lines.is_empty() || cursor_moved {
            self.generation = self.generation.wrapping_add(1);
        }

        if reason.is_none() && cursor_moved {
            let mut cursor_lines = Vec::with_capacity(2);
            if let Some(old) = old_cursor {
                cursor_lines.push(old.y as usize);
            }
            cursor_lines.push(cursor.y as usize);
            cursor_lines.sort_unstable();
            cursor_lines.dedup();
            for line in cursor_lines {
                insert_line(&mut lines, line);
            }
        }

        let cursor_only = reason.is_none() && cursor_moved && dirty.is_empty();

        self.initialized = true;
        self.columns = screen.columns;
        self.rows = screen.lines;
        self.alt_screen = screen.is_using_alternate_linebuf();
        self.scrolled_by = screen.scrolled_by;
        self.history_lines = screen.history_line_added_count;
        self.cursor = Some(cursor);
        self.force_full = false;

        Damage {
            generation: self.generation,
            full_redraw: reason.is_some(),
            full_redraw_reason: reason,
            lines,
            cursor_moved,
            cursor_only,
            old_cursor,
            new_cursor: Some(cursor),
        }
    }
}

fn insert_line(ranges: &mut Vec<LineRange>, line: usize) {
    // Ranges are kept sorted and non-adjacent. Cursor rows are only ever
    // one or two lines, so a linear insert is fine.
    if let Some(pos) = ranges.iter().position(|r| line < r.start) {
        if pos > 0 && ranges[pos - 1].end == line {
            ranges[pos - 1].end = line + 1;
            if ranges[pos - 1].end == ranges[pos].start {
                ranges[pos - 1].end = ranges[pos].end;
                ranges.remove(pos);
            }
        } else if ranges[pos].start == line + 1 {
            ranges[pos].start = line;
        } else {
            ranges.insert(pos, LineRange::new(line, line + 1));
        }
    } else if let Some(last) = ranges.last_mut() {
        if line <= last.end {
            last.end = last.end.max(line + 1);
        } else {
            ranges.push(LineRange::new(line, line + 1));
        }
    } else {
        ranges.push(LineRange::new(line, line + 1));
    }
}

/// Coalesce a sorted list of dirty rows into half-open ranges.
pub fn coalesce_lines(lines: &[usize]) -> Vec<LineRange> {
    let mut ans: Vec<LineRange> = Vec::new();
    for &line in lines {
        if let Some(last) = ans.last_mut() {
            if line <= last.end {
                last.end = last.end.max(line + 1);
                continue;
            }
        }
        ans.push(LineRange::new(line, line + 1));
    }
    ans
}

#[cfg(test)]
mod tests {
    use super::*;
    use kitty_screen::Screen;

    fn screen(columns: u32, lines: u32) -> Screen {
        Screen::builder(columns, lines)
            .scrollback(16)
            .build()
            .unwrap()
    }

    fn draw(screen: &mut Screen, text: &str) {
        let chars: Vec<u32> = text.chars().map(|c| c as u32).collect();
        screen.draw_text(&chars);
    }

    #[test]
    fn coalesces_adjacent_lines() {
        assert_eq!(coalesce_lines(&[]), Vec::<LineRange>::new());
        assert_eq!(
            coalesce_lines(&[0, 1, 2, 4, 5, 9]),
            vec![
                LineRange::new(0, 3),
                LineRange::new(4, 6),
                LineRange::new(9, 10)
            ]
        );
        let mut ranges = vec![LineRange::new(2, 3), LineRange::new(5, 6)];
        insert_line(&mut ranges, 4);
        assert_eq!(ranges, vec![LineRange::new(2, 3), LineRange::new(4, 6)]);
        insert_line(&mut ranges, 3);
        assert_eq!(ranges, vec![LineRange::new(2, 6)]);
    }

    #[test]
    fn first_frame_is_full_and_then_no_change_is_empty() {
        let mut s = screen(10, 4);
        let mut tracker = DamageTracker::new();
        let d = tracker.update(&s);
        assert!(d.full_redraw);
        assert_eq!(d.full_redraw_reason, Some(FullRedrawReason::FirstFrame));
        assert_eq!(d.generation, 1);

        s.reset_dirty();
        let d = tracker.update(&s);
        assert!(!d.full_redraw);
        assert!(d.is_empty());
        assert_eq!(d.generation, 1);
        assert_eq!(d.lines, Vec::<LineRange>::new());
    }

    #[test]
    fn wrap_coalesces_adjacent_rows() {
        let mut s = screen(5, 4);
        let mut tracker = DamageTracker::new();
        tracker.update(&s);
        s.reset_dirty();
        draw(&mut s, "abcdef");
        let d = tracker.update(&s);
        assert!(!d.full_redraw);
        assert!(!d.cursor_only);
        assert!(d.lines.iter().any(|r| r.start == 0 && r.end >= 2));
        assert_eq!(d.damaged_line_count(), 2);
    }

    #[test]
    fn scroll_requests_full_redraw() {
        let mut s = screen(10, 4);
        let mut tracker = DamageTracker::new();
        tracker.update(&s);
        s.reset_dirty();
        s.scroll(1);
        let d = tracker.update(&s);
        assert!(d.full_redraw);
        assert_eq!(d.full_redraw_reason, Some(FullRedrawReason::Scroll));
        assert!(d.lines.is_empty());
        assert_eq!(d.generation, 2);
    }

    #[test]
    fn cursor_only_movement() {
        let mut s = screen(10, 4);
        let mut tracker = DamageTracker::new();
        tracker.update(&s);
        s.reset_dirty();
        s.cursor_position(3, 4);
        let d = tracker.update(&s);
        assert!(!d.full_redraw);
        assert!(d.cursor_moved);
        assert!(d.cursor_only);
        assert_eq!(d.lines, vec![LineRange::new(0, 1), LineRange::new(2, 3)]);
        assert_eq!(d.old_cursor.unwrap().y, 0);
        assert_eq!(d.new_cursor.unwrap().y, 2);
    }

    #[test]
    fn explicit_full_redraw() {
        let mut s = screen(10, 4);
        let mut tracker = DamageTracker::new();
        tracker.update(&s);
        s.reset_dirty();
        tracker.invalidate();
        let d = tracker.update(&s);
        assert!(d.full_redraw);
        assert_eq!(d.full_redraw_reason, Some(FullRedrawReason::Explicit));

        s.reset_dirty();
        let d = tracker.update(&s);
        assert!(!d.full_redraw);
        assert!(d.is_empty());
    }

    #[test]
    fn changes_to_cursor_shape_count_as_cursor_damage() {
        let mut s = screen(10, 4);
        let mut tracker = DamageTracker::new();
        tracker.update(&s);
        s.reset_dirty();
        s.cursor.shape = CursorShape::Beam;
        let d = tracker.update(&s);
        assert!(d.cursor_moved);
        assert!(d.cursor_only);
    }
}
