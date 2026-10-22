// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from the selection model in kitty/screen.c and kitty/screen.h
// Copyright (C) 2016-2024 Kovid Goyal <kovid at kovidgoyal.net>

//! Selection model used by mouse and keyboard selection.
//!
//! This module deliberately contains no mouse bindings or UI. It provides the
//! data structures and text extraction operations: cell/word/line/block
//! selection, start/update/clear and plain/ANSI serialization.
//!
//! Selection rows are stored as *range rows*: non-negative values are screen
//! rows, negative values address scrollback (`-1` is the newest history line).
//! This is the same coordinate space as `range_line_()` in kitty, and makes a
//! selection stay anchored to its content while the screen scrolls.

use kitty_core::{CpuCell, Line, TextCache};

use crate::lineops::{line_as_ansi_with_links, line_as_text_range};
use crate::screen::Screen;

/// The kind of selection initiated by the user.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectionMode {
    /// Select exactly the cells between the two points.
    #[default]
    Cell,
    /// Expand both endpoints to word boundaries.
    Word,
    /// Select whole logical lines (including soft-wrapped continuations).
    Line,
    /// Rectangular block selection.
    Block,
}

impl SelectionMode {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "cell" | "cells" => Self::Cell,
            "word" | "words" => Self::Word,
            "line" | "lines" => Self::Line,
            "block" | "rectangle" | "rect" => Self::Block,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cell => "cell",
            Self::Word => "word",
            Self::Line => "line",
            Self::Block => "block",
        }
    }
}

/// One endpoint of a selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectionPoint {
    pub x: u32,
    /// Range row (negative values are scrollback).
    pub y: i64,
    pub in_left_half: bool,
}

/// A single selection range.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub start: SelectionPoint,
    pub end: SelectionPoint,
    pub mode: SelectionMode,
    pub rectangle: bool,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.start.x == self.end.x
            && self.start.y == self.end.y
            && self.start.in_left_half == self.end.in_left_half
    }
}

/// Container for the selection(s) on a screen. Kitty currently has at most one
/// primary selection; the vector keeps the door open for URL ranges later.
#[derive(Clone, Debug, Default)]
pub struct Selections {
    items: Vec<Selection>,
    pub in_progress: bool,
}

impl Selections {
    pub fn primary(&self) -> Option<&Selection> {
        self.items.first()
    }

    pub fn primary_mut(&mut self) -> Option<&mut Selection> {
        self.items.first_mut()
    }

    pub fn items(&self) -> &[Selection] {
        &self.items
    }

    pub fn count(&self) -> usize {
        self.items.len()
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.in_progress = false;
    }

    pub fn set_primary(&mut self, selection: Selection) {
        self.items.clear();
        self.items.push(selection);
    }
}

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

/// Get a screen/history line by range row.
pub(crate) fn range_line(screen: &Screen, y: i64) -> Option<Line> {
    if y < 0 {
        let lnum = (-y - 1) as u32;
        screen.historybuf().line(lnum).ok()
    } else if y < screen.lines as i64 {
        screen.linebuf().create_line_copy(y as usize).ok()
    } else {
        None
    }
}

/// Is the line at range row `y` continued onto the next row?
pub(crate) fn range_line_is_continued(screen: &Screen, y: i64) -> bool {
    if y < 0 {
        let lnum = (-y - 1) as u32;
        screen.historybuf().is_line_continued(lnum)
    } else if y == 0 {
        if screen.is_main_linebuf() {
            screen.historybuf().endswith_wrap()
        } else {
            false
        }
    } else if y < screen.lines as i64 {
        screen.linebuf().is_continued(y as usize)
    } else {
        false
    }
}

/// C's `xlimit_for_line()`: the number of cells up to the last non-zero one.
fn xlimit_for_line(line: &Line) -> u32 {
    let mut limit = line.xnum() as u32;
    while limit > 0 && !line.cpu_cells[(limit - 1) as usize].has_text() {
        limit -= 1;
    }
    limit
}

fn cell_is_blank(c: &CpuCell) -> bool {
    !c.has_text() || c.is_char(' ' as u32)
}

/// `screen_selection_range_for_line_()`: content start and exclusive limit.
pub(crate) fn content_range(line: &Line) -> (u32, u32) {
    let xnum = line.xnum() as u32;
    let mut xlimit = xnum;
    while xlimit > 0 && cell_is_blank(&line.cpu_cells[(xlimit - 1) as usize]) {
        xlimit -= 1;
    }
    let mut xstart = 0;
    while xstart < xlimit && cell_is_blank(&line.cpu_cells[xstart as usize]) {
        xstart += 1;
    }
    (xstart, xlimit)
}

fn is_word_char(line: &Line, x: u32, tc: &TextCache) -> bool {
    let ch = line.cell_first_char(x as usize, tc);
    if ch == ':' as u32 {
        // Allow "://" so URLs select as one word.
        if x as usize + 2 < line.xnum()
            && line.cpu_cells[x as usize + 1].is_char('/' as u32)
            && line.cpu_cells[x as usize + 2].is_char('/' as u32)
        {
            return true;
        }
    }
    kitty_unicode::char_props_for(ch).is_word_char()
}

/// Find the word containing `(row, x)` across soft-wrapped lines.
fn word_range_at(
    screen: &Screen,
    row: i64,
    x: u32,
    tc: &TextCache,
) -> Option<(SelectionPoint, SelectionPoint)> {
    let line = range_line(screen, row)?;
    let mut top = row;
    let mut bottom = row;
    let cols = screen.columns;
    if x >= cols {
        return None;
    }
    if !is_word_char(&line, x, tc) {
        return None;
    }
    let mut start_x = x;
    let mut end_x = x;
    let mut top_line = line.clone();
    loop {
        while start_x > 0 && is_word_char(&top_line, start_x - 1, tc) {
            start_x -= 1;
        }
        if start_x > 0 || !range_line_is_continued(screen, top) || top == i64::MIN {
            break;
        }
        let Some(above) = range_line(screen, top - 1) else {
            break;
        };
        if !is_word_char(&above, cols - 1, tc) {
            break;
        }
        top -= 1;
        start_x = cols - 1;
        top_line = above;
    }
    let mut bottom_line = line;
    loop {
        while end_x + 1 < cols && is_word_char(&bottom_line, end_x + 1, tc) {
            end_x += 1;
        }
        if end_x + 1 < cols {
            break;
        }
        let Some(below) = range_line(screen, bottom + 1) else {
            break;
        };
        if !range_line_is_continued(screen, bottom + 1) || !is_word_char(&below, 0, tc) {
            break;
        }
        bottom += 1;
        end_x = 0;
        bottom_line = below;
    }
    Some((
        SelectionPoint {
            x: start_x,
            y: top,
            in_left_half: true,
        },
        SelectionPoint {
            x: end_x,
            y: bottom,
            in_left_half: false,
        },
    ))
}

/// Logical line bounds for line selection, including wrapped continuations.
fn line_range_at(screen: &Screen, row: i64) -> Option<(SelectionPoint, SelectionPoint)> {
    let mut top = row;
    while top > -(screen.historybuf().count as i64) && range_line_is_continued(screen, top) {
        top -= 1;
    }
    let mut bottom = row;
    while bottom + 1 < screen.lines as i64 && range_line_is_continued(screen, bottom + 1) {
        bottom += 1;
    }
    let top_line = range_line(screen, top)?;
    let bottom_line = range_line(screen, bottom)?;
    let (start_x, _) = content_range(&top_line);
    let (_, end_x) = content_range(&bottom_line);
    Some((
        SelectionPoint {
            x: start_x,
            y: top,
            in_left_half: true,
        },
        SelectionPoint {
            x: end_x.saturating_sub(1),
            y: bottom,
            in_left_half: false,
        },
    ))
}

fn point_from_visual(screen: &Screen, x: u32, y: u32, in_left_half: bool) -> SelectionPoint {
    SelectionPoint {
        x: x.min(screen.columns.saturating_sub(1)),
        y: y as i64 - screen.scrolled_by as i64,
        in_left_half,
    }
}

impl Screen {
    /// Start a cell selection (`start_selection` in kitty).
    pub fn start_selection(&mut self, x: u32, y: u32) {
        self.start_selection_mode(x, y, SelectionMode::Cell, false, true);
    }

    /// Start a selection with an explicit mode and rectangle flag.
    pub fn start_selection_mode(
        &mut self,
        x: u32,
        y: u32,
        mode: SelectionMode,
        rectangle: bool,
        in_left_half: bool,
    ) {
        let mut start = point_from_visual(self, x, y, in_left_half);
        let mut end = start;
        match mode {
            SelectionMode::Word => {
                let tc = self.text_cache().borrow();
                if let Some((a, b)) = word_range_at(self, start.y, start.x, &tc) {
                    start = a;
                    end = b;
                }
            }
            SelectionMode::Line => {
                if let Some((a, b)) = line_range_at(self, start.y) {
                    start = a;
                    end = b;
                }
            }
            SelectionMode::Block => {
                start.in_left_half = in_left_half;
                end = start;
            }
            SelectionMode::Cell => {}
        }
        self.selections.set_primary(Selection {
            start,
            end,
            mode,
            rectangle: rectangle || mode == SelectionMode::Block,
        });
        self.selections.in_progress = true;
    }

    /// Update the in-progress selection (`update_selection` in kitty).
    pub fn update_selection(&mut self, x: u32, y: u32) {
        self.update_selection_ext(x, y, false, true);
    }

    /// Update the selection. `ended` finalizes it; `set_as_nearest_extend`
    /// keeps the nearer endpoint anchored while extending.
    pub fn update_selection_ext(&mut self, x: u32, y: u32, ended: bool, _set_as_nearest: bool) {
        let Some(sel) = self.selections.primary().cloned() else {
            return;
        };
        let cur = point_from_visual(self, x, y, false);
        let mut new_sel = sel.clone();
        match sel.mode {
            SelectionMode::Cell => {
                new_sel.end = cur;
            }
            SelectionMode::Block => {
                new_sel.end = cur;
                new_sel.rectangle = true;
            }
            SelectionMode::Word => {
                let tc = self.text_cache().borrow();
                let anchor = word_range_at(self, sel.start.y, sel.start.x, &tc);
                let current = word_range_at(self, cur.y, cur.x, &tc);
                match (anchor, current) {
                    (Some((a_start, a_end)), Some((c_start, c_end))) => {
                        if boundary_less(&c_start, &a_start) {
                            new_sel.start = c_start;
                            new_sel.end = a_end;
                        } else {
                            new_sel.start = a_start;
                            new_sel.end = c_end;
                        }
                    }
                    (Some((a_start, a_end)), None) => {
                        new_sel.start = a_start;
                        new_sel.end = cur;
                        let _ = a_end;
                    }
                    (None, Some((c_start, c_end))) => {
                        new_sel.start = c_start;
                        new_sel.end = c_end;
                    }
                    (None, None) => {
                        new_sel.start = sel.start;
                        new_sel.end = cur;
                    }
                }
            }
            SelectionMode::Line => {
                let anchor = line_range_at(self, sel.start.y);
                let current = line_range_at(self, cur.y);
                match (anchor, current) {
                    (Some((a_start, a_end)), Some((c_start, c_end))) => {
                        if boundary_less(&c_start, &a_start) {
                            new_sel.start = c_start;
                            new_sel.end = a_end;
                        } else {
                            new_sel.start = a_start;
                            new_sel.end = c_end;
                        }
                    }
                    (Some((a_start, a_end)), None) => {
                        new_sel.start = a_start;
                        new_sel.end = a_end;
                    }
                    (None, Some((c_start, c_end))) => {
                        new_sel.start = c_start;
                        new_sel.end = c_end;
                    }
                    (None, None) => {
                        new_sel.start = sel.start;
                        new_sel.end = cur;
                    }
                }
            }
        }
        self.selections.set_primary(new_sel);
        self.selections.in_progress = !ended;
    }

    /// Adjust selections when the screen scrolls up/down, so they stay
    /// anchored to their content (kitty's `index_selection`).
    pub(crate) fn index_selections(&mut self, up: bool) {
        if self.selections.count() == 0 {
            return;
        }
        for sel in self.selections.items.iter_mut() {
            if up {
                sel.start.y -= 1;
                sel.end.y -= 1;
            } else {
                sel.start.y += 1;
                sel.end.y += 1;
            }
        }
    }

    /// Clear the current selection.
    pub fn clear_selection(&mut self) {
        self.selections.clear();
    }

    /// True when a non-empty selection exists.
    pub fn has_selection(&self) -> bool {
        self.selections
            .primary()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    /// Access the current selection.
    pub fn selection(&self) -> Option<&Selection> {
        self.selections.primary()
    }

    /// Access the selection container.
    pub fn selections(&self) -> &Selections {
        &self.selections
    }

    /// The x range (inclusive start, exclusive limit) selected on `row`.
    fn selection_x_range(&self, sel: &Selection, row: i64) -> (u32, u32) {
        let columns = self.columns;
        if sel.mode == SelectionMode::Line {
            return match range_line(self, row) {
                Some(line) => content_range(&line),
                None => (0, 0),
            };
        }
        if sel.rectangle {
            let (lo, hi) = if sel.start.x <= sel.end.x {
                (
                    effective_start(&SelectionPoint {
                        x: sel.start.x,
                        y: sel.start.y,
                        in_left_half: sel.start.in_left_half,
                    }),
                    effective_end(&SelectionPoint {
                        x: sel.end.x,
                        y: sel.end.y,
                        in_left_half: sel.end.in_left_half,
                    }),
                )
            } else {
                (
                    effective_start(&SelectionPoint {
                        x: sel.end.x,
                        y: sel.end.y,
                        in_left_half: sel.end.in_left_half,
                    }),
                    effective_end(&SelectionPoint {
                        x: sel.start.x,
                        y: sel.start.y,
                        in_left_half: sel.start.in_left_half,
                    }),
                )
            };
            let (lo, hi) = if sel.start.x == sel.end.x {
                (sel.start.x, sel.start.x + 1)
            } else {
                (lo.min(hi), hi.max(lo))
            };
            return (lo.min(columns), hi.min(columns));
        }
        let (first, last) = if boundary_less(&sel.start, &sel.end) {
            (sel.start, sel.end)
        } else {
            (sel.end, sel.start)
        };
        if first.y == last.y {
            let lo = effective_start(&first);
            let hi = effective_end(&last);
            if lo >= hi {
                return (0, 0);
            }
            return (lo.min(columns), hi.min(columns));
        }
        if row == first.y {
            (effective_start(&first).min(columns), columns)
        } else if row == last.y {
            (0, effective_end(&last).min(columns))
        } else {
            (0, columns)
        }
    }

    /// Expand a range so a wide character is either fully included or fully
    /// excluded, and skip its continuation cells.
    fn adjust_wide_range(line: &Line, mut start: u32, mut limit: u32) -> (u32, u32) {
        let xnum = line.xnum() as u32;
        if start >= xnum {
            return (0, 0);
        }
        let c = line.cpu_cells[start as usize];
        if c.is_multicell() && c.x() != 0 {
            start = start.saturating_sub(c.x());
        }
        if limit <= start {
            return (0, 0);
        }
        let last = &line.cpu_cells[(limit - 1) as usize];
        if last.is_multicell() && last.x() + 1 < last.mcd_x_limit() {
            let base = (limit - 1).saturating_sub(last.x());
            limit = (base + last.mcd_x_limit()).min(xnum);
        }
        (start, limit)
    }

    fn text_for_selection_impl(&self, ansi: bool, strip_trailing_whitespace: bool) -> Vec<String> {
        let Some(sel) = self.selections.primary() else {
            return Vec::new();
        };
        if sel.is_empty() {
            return Vec::new();
        }
        let top = sel.start.y.min(sel.end.y);
        let bottom = sel.start.y.max(sel.end.y);
        let used_hyperlink_pool = &self.hyperlinks;
        let tc = self.text_cache.borrow();
        let mut rows: Vec<(i64, String)> = Vec::new();
        for row in top..=bottom {
            let Some(line) = range_line(self, row) else {
                rows.push((row, String::new()));
                continue;
            };
            let (s, l) = self.selection_x_range(sel, row);
            let content_limit = xlimit_for_line(&line);
            let l = l.min(content_limit);
            let (s, l) = Self::adjust_wide_range(&line, s, l);
            let line_is_empty = line.is_empty();
            if s >= l || line_is_empty {
                rows.push((row, String::new()));
                continue;
            }
            let mut limit = l;
            let continued_to_end = limit >= self.columns
                && line.cpu_cells[(self.columns - 1) as usize].next_char_was_wrapped();
            if strip_trailing_whitespace && !continued_to_end {
                while limit > s {
                    let c = &line.cpu_cells[(limit - 1) as usize];
                    if c.is_multicell() && (c.x() != 0 || c.y() != 0) {
                        limit -= 1;
                        continue;
                    }
                    let ch = line.cell_first_char((limit - 1) as usize, &tc);
                    if ch == ' ' as u32 || ch == '\t' as u32 || ch == 0 {
                        limit -= 1;
                    } else {
                        break;
                    }
                }
                if limit <= s {
                    rows.push((row, String::new()));
                    continue;
                }
            }
            let text = if ansi {
                line_as_ansi_with_links(
                    &line,
                    &tc,
                    s as usize,
                    limit as usize,
                    Some(used_hyperlink_pool),
                )
            } else {
                line_as_text_range(&line, &tc, s as usize, limit as usize, false, true)
            };
            rows.push((row, text));
        }
        while rows.last().map(|(_, t)| t.is_empty()).unwrap_or(false) {
            rows.pop();
        }
        if ansi {
            let has_escape_codes = rows.iter().any(|(_, t)| t.contains('\u{1b}'));
            rows.push((
                i64::MAX,
                if has_escape_codes {
                    "\u{1b}[m".to_string()
                } else {
                    String::new()
                },
            ));
        }
        rows.into_iter().map(|(_, t)| t).collect()
    }

    /// Text for the current selection, split into one string per logical line.
    ///
    /// Join with `\n` for the usual primary-selection text.
    pub fn text_for_selection(&self, ansi: bool, strip_trailing_whitespace: bool) -> Vec<String> {
        self.text_for_selection_impl(ansi, strip_trailing_whitespace)
    }

    /// Convenience: the selected text joined with newlines.
    pub fn selected_text(&self) -> String {
        self.text_for_selection(false, false).join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::Screen;

    fn screen(cols: u32, lines: u32) -> Screen {
        Screen::builder(cols, lines).scrollback(8).build().unwrap()
    }

    #[test]
    fn selection_modes_parse() {
        assert_eq!(SelectionMode::from_name("word"), Some(SelectionMode::Word));
        assert_eq!(SelectionMode::from_name("rect"), Some(SelectionMode::Block));
        assert_eq!(SelectionMode::from_name("nope"), None);
        assert_eq!(SelectionMode::Line.as_str(), "line");
    }

    #[test]
    fn cell_selection_across_wrapped_lines() {
        let mut s = screen(5, 4);
        s.draw_text(&"abcdefghij".chars().map(|c| c as u32).collect::<Vec<_>>());
        s.start_selection(0, 0);
        s.update_selection(1, 1);
        assert_eq!(s.selected_text(), "abcde\nfg");
    }

    #[test]
    fn word_selection_includes_whole_word() {
        let mut s = screen(20, 2);
        s.draw_text(&"hello world".chars().map(|c| c as u32).collect::<Vec<_>>());
        s.start_selection_mode(7, 0, SelectionMode::Word, false, true);
        assert_eq!(s.selected_text(), "world");
    }

    #[test]
    fn line_selection_includes_wrapped_continuations() {
        let mut s = screen(5, 4);
        s.draw_text(&"abcdefgh".chars().map(|c| c as u32).collect::<Vec<_>>());
        s.start_selection_mode(0, 0, SelectionMode::Line, false, true);
        assert_eq!(s.selected_text(), "abcde\nfgh");
    }

    #[test]
    fn block_selection_is_rectangular() {
        let mut s = screen(5, 3);
        for (i, y) in [0u32, 1, 2].iter().enumerate() {
            s.cursor.y = *y;
            s.cursor.x = 0;
            let text = format!("row{i}");
            s.draw_text(&text.chars().map(|c| c as u32).collect::<Vec<_>>());
        }
        s.start_selection_mode(1, 0, SelectionMode::Block, true, true);
        s.update_selection(2, 2);
        let text = s.text_for_selection(false, false);
        assert_eq!(text, vec!["ow", "ow", "ow"]);
    }

    #[test]
    fn wide_cells_and_combining_are_not_split() {
        let mut s = screen(6, 2);
        s.draw_text(&[0x4e16, 0x754c]);
        s.start_selection(1, 0);
        s.update_selection(1, 0);
        assert_eq!(s.selected_text(), "世");
        let mut s = screen(6, 2);
        s.draw_text(&['a' as u32, 0x0301, 'b' as u32]);
        s.start_selection(0, 0);
        s.update_selection(1, 0);
        assert_eq!(s.selected_text(), "a\u{301}b");
    }

    #[test]
    fn trailing_whitespace_strip_and_soft_wrap_preservation() {
        let mut s = screen(5, 2);
        s.draw_text(&"ab   cd".chars().map(|c| c as u32).collect::<Vec<_>>());
        s.start_selection(0, 0);
        s.update_selection(1, 1);
        assert_eq!(s.text_for_selection(false, true), vec!["ab   ", "cd"]);
        let mut s = screen(5, 2);
        s.draw_text(&"1234 5".chars().map(|c| c as u32).collect::<Vec<_>>());
        s.start_selection(0, 0);
        s.update_selection(4, 1);
        assert_eq!(s.text_for_selection(false, true), vec!["1234 ", "5"]);
    }

    #[test]
    fn ansi_selection_reports_attributes() {
        let mut s = screen(5, 2);
        s.draw_text(&['a' as u32]);
        s.apply_sgr_to_cells(&mut [], &[], false);
        s.select_graphic_rendition(&[32], false);
        s.draw_text(&['b' as u32]);
        s.select_graphic_rendition(&[39], false);
        s.draw_text(&"c  xy".chars().map(|c| c as u32).collect::<Vec<_>>());
        s.start_selection(0, 0);
        s.update_selection(1, 1);
        assert_eq!(
            s.text_for_selection(true, false),
            vec!["a\u{1b}[32mb\u{1b}[39mc  ", "xy", "\u{1b}[m"]
        );
    }
}

#[cfg(test)]
mod scored_tests {
    use crate::screen::Screen;

    fn screen(cols: u32, lines: u32, scrollback: u32) -> Screen {
        Screen::builder(cols, lines)
            .scrollback(scrollback)
            .build()
            .unwrap()
    }

    #[test]
    fn selection_stays_anchored_when_scrolling_into_history() {
        let mut s = screen(5, 3, 20);
        for i in 0..4 {
            if i != 0 {
                s.linefeed();
                s.carriage_return();
            }
            let text: Vec<u32> = std::iter::repeat_n('0' as u32 + i, 5).collect();
            s.draw_text(&text);
        }
        s.start_selection(0, 0);
        s.update_selection(4, 0);
        let before = s.selected_text();
        s.linefeed();
        assert_eq!(s.selected_text(), before);
    }

    #[test]
    fn middle_empty_lines_are_kept_but_trailing_empty_lines_trimmed() {
        let mut s = screen(5, 5, 5);
        s.draw_text(&['a' as u32]);
        s.carriage_return();
        s.linefeed();
        s.linefeed();
        s.draw_text(&['b' as u32]);
        s.start_selection(0, 0);
        s.update_selection(4, 4);
        assert_eq!(s.selected_text(), "a\n\nb");
    }

    #[test]
    fn ansi_and_plain_selection_agree_on_widths() {
        let mut s = screen(6, 2, 5);
        s.draw_text(&['a' as u32, 0x4e16, 0x754c, 'b' as u32]);
        s.start_selection(0, 0);
        s.update_selection(3, 0);
        assert_eq!(s.text_for_selection(false, false), vec!["a世界"]);
        let ansi = s.text_for_selection(true, false);
        assert_eq!(ansi[0], "a世界");
    }
}
