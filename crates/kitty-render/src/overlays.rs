// SPDX-License-Identifier: GPL-3.0-or-later
//! Selection, hyperlink and tab-bar overlay descriptions.
//!
//! Overlays are pure CPU data. The instance generator turns them into
//! backgrounds, decorations and glyphs so every backend can draw them with the
//! same primitives it already uses for cells.

use kitty_screen::{Selection, SelectionMode, SelectionPoint};

use crate::colors::Rgba8;
use crate::geom::{EdgeWidths, Rect};

/// A selection range in the same range-row coordinate space as
/// [`kitty_screen::Selection`]: non-negative rows are screen rows and negative
/// rows address scrollback (`-1` is the newest history line).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionRange {
    pub start: SelectionPoint,
    pub end: SelectionPoint,
    pub mode: SelectionMode,
    pub rectangle: bool,
}

impl SelectionRange {
    pub fn new(start: SelectionPoint, end: SelectionPoint) -> Self {
        Self {
            start,
            end,
            mode: SelectionMode::Cell,
            rectangle: false,
        }
    }

    pub fn cell(x0: u32, y0: i64, x1: u32, y1: i64) -> Self {
        Self::new(
            SelectionPoint {
                x: x0,
                y: y0,
                in_left_half: true,
            },
            SelectionPoint {
                x: x1,
                y: y1,
                in_left_half: false,
            },
        )
    }

    pub fn block(x0: u32, y0: i64, x1: u32, y1: i64) -> Self {
        Self {
            start: SelectionPoint {
                x: x0,
                y: y0,
                in_left_half: true,
            },
            end: SelectionPoint {
                x: x1,
                y: y1,
                in_left_half: false,
            },
            mode: SelectionMode::Block,
            rectangle: true,
        }
    }

    pub fn from_selection(selection: &Selection) -> Self {
        Self {
            start: selection.start,
            end: selection.end,
            mode: selection.mode,
            rectangle: selection.rectangle || selection.mode == SelectionMode::Block,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end && self.mode != SelectionMode::Block
    }
}

/// One resolved, visible selection cell. `rect` is in device pixels and
/// `width_cells` is greater than one for a selected wide character.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectionCell {
    pub x: u32,
    pub y: u32,
    pub width_cells: u32,
    pub rect: Rect,
}

/// A cell whose hyperlink should be highlighted while hovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HyperlinkHover {
    /// Visual cell coordinates (the scrolled-back viewport), not range rows.
    pub x: u32,
    pub y: u32,
}

/// Colours used by a tab bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabBarColors {
    pub background: Rgba8,
    pub inactive_background: Rgba8,
    pub active_background: Rgba8,
    pub foreground: Rgba8,
    pub active_foreground: Rgba8,
    pub border: Rgba8,
    pub active_border: Rgba8,
}

impl Default for TabBarColors {
    fn default() -> Self {
        Self {
            background: Rgba8::rgb(0x1d, 0x1f, 0x21),
            inactive_background: Rgba8::rgb(0x28, 0x2a, 0x2e),
            active_background: Rgba8::rgb(0x37, 0x3b, 0x41),
            foreground: Rgba8::rgb(0xc5, 0xc8, 0xc6),
            active_foreground: Rgba8::rgb(0xff, 0xff, 0xff),
            border: Rgba8::rgb(0x50, 0x54, 0x58),
            active_border: Rgba8::rgb(0xe0, 0xa0, 0x30),
        }
    }
}

/// Description of the tab bar to draw at the top of the viewport.
///
/// Titles are rendered through the normal glyph atlas path. Tabs that do not
/// fit are clipped (or omitted entirely); scrolling is intentionally not part
/// of this layer. The GUI can derive the fields from `kitty-ui`'s
/// `TabManager`/`Tab` state.
#[derive(Clone, Debug, PartialEq)]
pub struct TabBarSpec {
    pub titles: Vec<String>,
    pub active: usize,
    pub height: f32,
    pub colors: TabBarColors,
    pub border_width: f32,
    pub padding: EdgeWidths,
    pub min_tab_width: f32,
    pub max_tab_width: f32,
    /// Font id stored in the tab-title glyph keys.
    pub font_id: u16,
}

impl Default for TabBarSpec {
    fn default() -> Self {
        Self {
            titles: Vec::new(),
            active: 0,
            height: 28.0,
            colors: TabBarColors::default(),
            border_width: 1.0,
            padding: EdgeWidths::new(8.0, 0.0, 8.0, 0.0),
            min_tab_width: 64.0,
            max_tab_width: 240.0,
            font_id: 0,
        }
    }
}

impl TabBarSpec {
    pub fn new(titles: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            titles: titles.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    pub fn tab_width(&self, viewport_width: f32, count: usize) -> f32 {
        if count == 0 {
            return 0.0;
        }
        let available = viewport_width / count as f32;
        available
            .clamp(self.min_tab_width.max(0.0), self.max_tab_width.max(0.0))
            .max(0.0)
    }
}

/// The visible part of one tab.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabBarTab {
    pub index: usize,
    pub active: bool,
    /// Clipped to the viewport.
    pub rect: Rect,
    /// Unclipped layout rectangle, useful for hit testing.
    pub layout_rect: Rect,
}

/// Resolved tab-bar geometry for a frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabBarFrame {
    pub rect: Rect,
    pub height: f32,
    pub tabs: Vec<TabBarTab>,
    /// Titles after clipping to each tab and the viewport.
    pub visible_titles: Vec<String>,
}

/// Counters for overlay and shaped-run work done while building a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OverlayStats {
    pub selection_ranges: usize,
    pub selection_cells: usize,
    pub hyperlink_cells: usize,
    pub hyperlink_decorations: usize,
    pub hover_cells: usize,
    pub tab_tabs: usize,
    pub tab_glyphs: usize,
    pub shaped_run_attempts: usize,
    pub shaped_run_successes: usize,
    pub shaped_sprites: usize,
    pub shaped_cache_hits: usize,
    pub shaped_cache_misses: usize,
    pub shaped_fallbacks: usize,
}

// {{{ Selection interval computation

#[inline]
fn boundary_less(a: &SelectionPoint, b: &SelectionPoint) -> bool {
    if a.y != b.y {
        return a.y < b.y;
    }
    if a.x != b.x {
        return a.x < b.x;
    }
    a.in_left_half && !b.in_left_half
}

#[inline]
fn effective_start(p: &SelectionPoint) -> u32 {
    p.x + u32::from(!p.in_left_half)
}

#[inline]
fn effective_end(p: &SelectionPoint) -> u32 {
    p.x + u32::from(!p.in_left_half)
}

fn rectangle_x(sel: &SelectionRange, columns: u32) -> (u32, u32) {
    let (lo, hi) = if sel.start.x <= sel.end.x {
        (effective_start(&sel.start), effective_end(&sel.end))
    } else {
        (effective_start(&sel.end), effective_end(&sel.start))
    };
    let (lo, hi) = if sel.start.x == sel.end.x {
        (sel.start.x, sel.start.x + 1)
    } else {
        (lo.min(hi), hi.max(lo))
    };
    (lo.min(columns), hi.min(columns))
}

fn selection_intervals_for_range(
    sel: &SelectionRange,
    row: i64,
    columns: u32,
) -> Option<(u32, u32)> {
    let top = sel.start.y.min(sel.end.y);
    let bottom = sel.start.y.max(sel.end.y);
    if row < top || row > bottom {
        return None;
    }
    if sel.mode == SelectionMode::Line {
        return (columns > 0).then_some((0, columns));
    }
    if sel.rectangle {
        let (lo, hi) = rectangle_x(sel, columns);
        return (lo < hi).then_some((lo, hi));
    }
    let (first, last) = if boundary_less(&sel.start, &sel.end) {
        (sel.start, sel.end)
    } else {
        (sel.end, sel.start)
    };
    let range = if first.y == last.y {
        let lo = effective_start(&first);
        let hi = effective_end(&last);
        (lo, hi)
    } else if row == first.y {
        (effective_start(&first), columns)
    } else if row == last.y {
        (0, effective_end(&last))
    } else {
        (0, columns)
    };
    let lo = range.0.min(columns);
    let hi = range.1.min(columns);
    (lo < hi).then_some((lo, hi))
}

/// Sorted, merged `[start, end)` x intervals selected on range row `row`.
pub fn selection_intervals(
    selections: &[SelectionRange],
    row: i64,
    columns: u32,
) -> Vec<(u32, u32)> {
    let mut intervals = Vec::new();
    for sel in selections {
        if let Some((lo, hi)) = selection_intervals_for_range(sel, row, columns) {
            intervals.push((lo, hi));
        }
    }
    intervals.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(intervals.len());
    for (lo, hi) in intervals {
        match merged.last_mut() {
            Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
            _ => merged.push((lo, hi)),
        }
    }
    merged
}

/// Whether `x` is inside one of the sorted intervals.
#[inline]
pub fn interval_contains(intervals: &[(u32, u32)], x: u32) -> bool {
    intervals.iter().any(|&(lo, hi)| x >= lo && x < hi)
}

// }}}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_selection_intervals_are_exclusive_at_the_end() {
        let sel = SelectionRange::cell(1, 0, 3, 0);
        assert_eq!(selection_intervals(&[sel], 0, 10), vec![(1, 4)]);
        assert_eq!(selection_intervals(&[], 0, 10), Vec::<(u32, u32)>::new());
    }

    #[test]
    fn multi_row_and_block_intervals() {
        let sel = SelectionRange::cell(2, 0, 3, 2);
        assert_eq!(
            selection_intervals(std::slice::from_ref(&sel), 0, 10),
            vec![(2, 10)]
        );
        assert_eq!(
            selection_intervals(std::slice::from_ref(&sel), 1, 10),
            vec![(0, 10)]
        );
        assert_eq!(
            selection_intervals(std::slice::from_ref(&sel), 2, 10),
            vec![(0, 4)]
        );

        let block = SelectionRange::block(1, 0, 3, 1);
        assert_eq!(
            selection_intervals(std::slice::from_ref(&block), 0, 10),
            vec![(1, 4)]
        );
        assert_eq!(
            selection_intervals(std::slice::from_ref(&block), 1, 10),
            vec![(1, 4)]
        );
    }

    #[test]
    fn intervals_merge_and_clip() {
        let a = SelectionRange::cell(0, 0, 4, 0);
        let b = SelectionRange::cell(3, 0, 20, 0);
        assert_eq!(selection_intervals(&[a, b], 0, 8), vec![(0, 8)]);
        assert!(interval_contains(&[(0, 8)], 7));
        assert!(!interval_contains(&[(0, 8)], 8));
    }
}
