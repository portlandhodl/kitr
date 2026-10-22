// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/resize.c and the resize/rewrap paths in kitty/screen.c,
// kitty/history.c and kitty/line-buf.c.
// Copyright (C) 2024 Kovid Goyal <kovid at kovidgoyal.net>

//! Resize and rewrap.
//!
//! Kitty rewraps the scrollback and the main screen to the new width, then
//! truncates/pads the screen to the new height. The alternate screen is *not*
//! rewrapped; it keeps its top-left content. This module does the same using
//! the flat [`HistoryBuf`] and [`LineBuf`] representations from `kitty-core`.
//!
//! A logical line is a sequence of source rows where all but the last carry the
//! `next_char_was_wrapped` flag. Each logical line is re-flowed into the new
//! width; a wide character that does not fit is pushed to the next destination
//! row (leaving blank cells at the end of the previous row), matching kitty's
//! `find_space_in_dest()`. Multi-line (text-sizing) cells are treated
//! conservatively: only their first row is copied and orphan continuation rows
//! are dropped.

use kitty_core::{HistoryBuf, Line, LineBuf, TextCache};
use std::cell::RefCell;
use std::rc::Rc;

use crate::screen::{Screen, ScreenError};

/// Result of [`Screen::resize`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResizeResult {
    pub old_lines: u32,
    pub old_columns: u32,
    pub lines: u32,
    pub columns: u32,
    /// Content rows before/after the resize for the active buffer.
    pub num_content_lines_before: u32,
    pub num_content_lines_after: u32,
    pub main_content_lines_before: u32,
    pub main_content_lines_after: u32,
    pub alternate_content_lines_before: u32,
    pub alternate_content_lines_after: u32,
}

#[derive(Clone, Copy, Debug)]
struct CursorTarget {
    /// Screen row in the source buffer.
    source_y: u32,
    source_x: u32,
    dest: Option<(usize, usize)>,
}

#[derive(Default)]
struct Dest {
    rows: Vec<Line>,
    from_history: Vec<bool>,
    width: usize,
    current: usize,
    x: usize,
}

impl Dest {
    fn new(width: usize) -> Self {
        Self {
            rows: Vec::new(),
            from_history: Vec::new(),
            width,
            current: 0,
            x: 0,
        }
    }

    fn start_row(&mut self, attrs: kitty_core::LineAttrs, wrapped: bool, from_history: bool) {
        if wrapped {
            self.mark_current_wrapped();
        }
        let mut line = Line::new(self.width);
        line.attrs = attrs;
        self.rows.push(line);
        self.from_history.push(from_history);
        self.current = self.rows.len() - 1;
        self.x = 0;
    }

    fn mark_current_wrapped(&mut self) {
        if let Some(row) = self.rows.get_mut(self.current) {
            if self.width > 0 {
                row.cpu_cells[self.width - 1].set_next_char_was_wrapped(true);
            }
        }
    }
}

/// Find the number of non-empty content rows from the bottom (`exclude_empty_
/// lines_at_bottom`).
fn content_rows(lb: &LineBuf) -> u32 {
    let mut last = None;
    for y in 0..lb.ynum as usize {
        if lb.cpu_cells_for_line(y).iter().any(|c| c.has_text()) {
            last = Some(y as u32);
        }
    }
    last.map(|y| y + 1).unwrap_or(0)
}

fn source_row_limit(line: &Line) -> usize {
    let mut limit = line.xnum();
    while limit > 0 && !line.cpu_cells[limit - 1].has_text() {
        limit -= 1;
    }
    limit
}

/// Re-flow source rows into destination rows of `width` columns.
fn rewrap_rows(
    source: &[(i64, Line)],
    width: u32,
    mut targets: Vec<CursorTarget>,
) -> (Vec<Line>, Vec<bool>, Vec<CursorTarget>) {
    let width = width.max(1) as usize;
    let mut dest = Dest::new(width);
    let mut i = 0usize;
    while i < source.len() {
        let attrs = source[i].1.attrs;
        let in_history = source[i].0 < 0;
        dest.start_row(attrs, false, in_history);
        loop {
            let (row_abs, line) = &source[i];
            let source_width = line.xnum().max(1);
            let continues = line.cpu_cells[source_width - 1].next_char_was_wrapped();
            let limit = source_row_limit(line);
            // Fallback mapping for targets on this source row.
            for t in targets.iter_mut() {
                if t.source_y as i64 == *row_abs && t.dest.is_none() {
                    t.dest = Some((dest.current, dest.x));
                }
            }
            let mut ix = 0usize;
            while ix < limit {
                let cell = line.cpu_cells[ix];
                let mut w = 1usize;
                if cell.is_multicell() {
                    if cell.y() != 0 {
                        // Multi-line text-sizing cells are outside this port.
                        ix += 1;
                        continue;
                    }
                    if cell.x() != 0 {
                        // Orphan continuation cell (its base was dropped).
                        ix += 1;
                        continue;
                    }
                    w = cell.mcd_x_limit().max(1) as usize;
                    if w > width {
                        ix += w;
                        continue;
                    }
                    if ix + w > line.xnum() {
                        w = line.xnum() - ix;
                    }
                }
                if dest.x >= width || dest.x + w > width {
                    dest.start_row(attrs, true, *row_abs < 0);
                }
                for k in 0..w {
                    let src_x = ix + k;
                    if src_x >= line.xnum() || dest.x + k >= width {
                        break;
                    }
                    let d = &mut dest.rows[dest.current];
                    d.cpu_cells[dest.x + k] = line.cpu_cells[src_x];
                    d.gpu_cells[dest.x + k] = line.gpu_cells[src_x];
                    for t in targets.iter_mut() {
                        if t.source_y as i64 == *row_abs && t.source_x as usize == src_x {
                            t.dest = Some((dest.current, dest.x + k));
                        }
                    }
                }
                ix += w;
                dest.x += w;
            }
            // Cursor at or beyond the end of the source row.
            for t in targets.iter_mut() {
                if t.source_y as i64 == *row_abs && t.source_x as usize >= limit {
                    t.dest = Some((dest.current, dest.x.min(width)));
                }
            }
            i += 1;
            if !continues {
                break;
            }
            if i >= source.len() {
                break;
            }
            // Continue the same logical line. If the destination row is exactly
            // full the next cell will start a wrapped row; if the next source
            // row is empty, keep the current row as-is.
            if dest.x >= width {
                let next_attrs = source[i].1.attrs;
                dest.start_row(next_attrs, true, source[i].0 < 0);
            }
        }
    }
    (dest.rows, dest.from_history, targets)
}

fn resize_alt_without_rewrap(
    old: &LineBuf,
    lines: u32,
    columns: u32,
    tc: Rc<RefCell<TextCache>>,
) -> Result<LineBuf, ScreenError> {
    let mut ans = LineBuf::new(lines, columns, tc)?;
    let copy_lines = old.ynum.min(lines);
    for y in 0..copy_lines as usize {
        let mut line = old.create_line_copy(y)?;
        line.cpu_cells
            .resize(columns as usize, kitty_core::CpuCell::new());
        line.gpu_cells
            .resize(columns as usize, kitty_core::GpuCell::default());
        if old.xnum > columns {
            // Drop a wide character split by the right edge, like kitty's
            // resize_screen_buffer_without_rewrap().
            let n = columns as usize;
            if n > 0 {
                let last = line.cpu_cells[n - 1];
                if last.is_multicell() && last.x() + 1 < last.mcd_x_limit() {
                    let mut base = n - 1;
                    while base > 0 && line.cpu_cells[base].x() != 0 {
                        base -= 1;
                    }
                    for c in line.cpu_cells[base..].iter_mut() {
                        c.set_char(0);
                        c.set_is_multicell(false);
                    }
                }
            }
        }
        ans.copy_line_to(&line, y);
        ans.mark_line_dirty(y);
    }
    Ok(ans)
}

fn mapped_cursor(
    target: Option<CursorTarget>,
    lines: u32,
    columns: u32,
    screen_start_flat: usize,
) -> (u32, u32) {
    let (x, y) = match target.and_then(|t| t.dest) {
        Some((row, x)) => (x as u32, row.saturating_sub(screen_start_flat) as u32),
        None => (0, 0),
    };
    (
        x.min(columns.saturating_sub(1)),
        y.min(lines.saturating_sub(1)),
    )
}

/// Rewrap a destination row list into a line buffer + history buffer.
fn install_rewrapped(
    screen: &Screen,
    rows: Vec<Line>,
    from_history: Vec<bool>,
    extra_scroll: usize,
    history_capacity: u32,
    lines: u32,
    columns: u32,
) -> Result<(LineBuf, HistoryBuf), ScreenError> {
    let tc = screen.text_cache.clone();
    // Rows produced while the source was still in scrollback stay in the
    // destination scrollback. The remaining rows form the destination line
    // buffer; any overflow is pushed into history as well.
    let mut prefix = 0usize;
    while prefix < rows.len() && from_history.get(prefix).copied().unwrap_or(false) {
        prefix += 1;
    }
    let tail = &rows[prefix..];
    let mut tail_split = tail.len().saturating_sub(lines as usize);
    tail_split = (tail_split + extra_scroll).min(tail.len());
    let hist_end = prefix + tail_split;
    let mut hist_start = 0usize;
    if hist_end > history_capacity as usize {
        hist_start = hist_end - history_capacity as usize;
    }
    let mut hb = HistoryBuf::new(columns, history_capacity.max(1), tc.clone())?;
    for line in &rows[hist_start..hist_end] {
        hb.add_line(line);
    }
    for lnum in 0..hb.count {
        hb.mark_line_dirty(lnum);
    }
    let mut lb = LineBuf::new(lines, columns, tc)?;
    let screen_rows = &rows[hist_end..];
    for (y, line) in screen_rows.iter().take(lines as usize).enumerate() {
        lb.copy_line_to(line, y);
    }
    for y in 0..lines as usize {
        lb.mark_line_dirty(y);
    }
    Ok((lb, hb))
}

impl Screen {
    /// Resize the screen to `new_lines` rows and `new_columns` columns.
    ///
    /// The main screen and scrollback are rewrapped; the alternate screen is
    /// truncated/padded without reflow, exactly like `screen_resize()` in kitty.
    pub fn resize(&mut self, new_lines: u32, new_columns: u32) -> ResizeResult {
        let new_lines = new_lines.max(1);
        let new_columns = new_columns.max(1);
        let old_lines = self.lines;
        let old_columns = self.columns;
        if new_lines == old_lines && new_columns == old_columns {
            return ResizeResult {
                old_lines,
                old_columns,
                lines: old_lines,
                columns: old_columns,
                ..Default::default()
            };
        }
        self.selections.clear();
        let main_active = !self.on_alt;
        let cursor_y = self.cursor.y;
        let cursor_x = self.cursor.x;

        // Gather source rows: oldest history line first, then the non-empty
        // portion of the main screen.
        let hist_count = self.historybuf.count;
        let content_before = content_rows(&self.main_linebuf);
        let mut source: Vec<(i64, Line)> =
            Vec::with_capacity(hist_count as usize + content_before as usize);
        for lnum in (0..hist_count).rev() {
            if let Ok(line) = self.historybuf.line(lnum) {
                source.push((-(lnum as i64) - 1, line));
            }
        }
        for y in 0..content_before {
            if let Ok(line) = self.main_linebuf.create_line_copy(y as usize) {
                source.push((y as i64, line));
            }
        }
        let current_target = CursorTarget {
            source_y: cursor_y,
            source_x: cursor_x,
            dest: None,
        };
        let saved_target = CursorTarget {
            source_y: self.main_savepoint.cursor.y,
            source_x: self.main_savepoint.cursor.x,
            dest: None,
        };
        let targets = if main_active {
            vec![current_target, saved_target]
        } else {
            vec![saved_target]
        };
        let (main_rows, main_from_history, main_targets) =
            rewrap_rows(&source, new_columns, targets);
        let main_cursor_beyond = main_active && content_before > 0 && cursor_y >= content_before;
        let mut history_prefix = 0usize;
        while history_prefix < main_rows.len()
            && main_from_history
                .get(history_prefix)
                .copied()
                .unwrap_or(false)
        {
            history_prefix += 1;
        }
        let linebuf_rows = main_rows.len() - history_prefix;
        let main_num_after = linebuf_rows.min(new_lines as usize) as u32;
        let extra_scroll = if main_cursor_beyond && main_num_after >= new_lines {
            // screen_index(): the cursor is one line below the last content
            // line, so scroll once and leave a blank bottom row.
            1
        } else {
            0
        };
        let tail_split =
            (linebuf_rows.saturating_sub(new_lines as usize) + extra_scroll).min(linebuf_rows);
        let screen_start_flat = history_prefix + tail_split;
        let history_capacity = self.historybuf.ynum;
        let (new_main, new_hist) = match install_rewrapped(
            self,
            main_rows,
            main_from_history,
            extra_scroll,
            history_capacity,
            new_lines,
            new_columns,
        ) {
            Ok(x) => x,
            Err(_) => {
                // Allocation failure cannot happen with the bounded sizes
                // used here; keep the old buffers rather than panic.
                return ResizeResult {
                    old_lines,
                    old_columns,
                    lines: old_lines,
                    columns: old_columns,
                    ..Default::default()
                };
            }
        };

        // Alternate screen: no rewrap.
        let alt_tc = self.text_cache.clone();
        let new_alt =
            match resize_alt_without_rewrap(&self.alt_linebuf, new_lines, new_columns, alt_tc) {
                Ok(x) => x,
                Err(_) => {
                    return ResizeResult {
                        old_lines,
                        old_columns,
                        lines: old_lines,
                        columns: old_columns,
                        ..Default::default()
                    }
                }
            };

        // Cursor mapping.
        let (current_mapped, main_sp_mapped) = if main_active {
            let mut it = main_targets.into_iter();
            (it.next(), it.next())
        } else {
            (None, main_targets.into_iter().next())
        };
        let (mut new_cursor_x, mut new_cursor_y) =
            mapped_cursor(current_mapped, new_lines, new_columns, screen_start_flat);
        let (main_sp_x, main_sp_y) =
            mapped_cursor(main_sp_mapped, new_lines, new_columns, screen_start_flat);
        if !main_active {
            new_cursor_x = cursor_x.min(new_columns.saturating_sub(1));
            new_cursor_y = cursor_y.min(new_lines.saturating_sub(1));
        }
        if main_cursor_beyond {
            new_cursor_x = cursor_x.min(new_columns.saturating_sub(1));
            new_cursor_y = main_num_after.min(new_lines).saturating_sub(1);
            if main_num_after >= new_lines {
                new_cursor_x = new_cursor_x.min(new_columns.saturating_sub(1));
                new_cursor_y = new_lines - 1;
            }
        }
        if main_active {
            self.cursor.x = new_cursor_x;
            self.cursor.y = new_cursor_y;
        }
        self.main_savepoint.cursor.x = main_sp_x;
        self.main_savepoint.cursor.y = main_sp_y;

        // Alternate savepoint/cursor are only clamped, never rewrapped.
        let alt_sp_old = (self.alt_savepoint.cursor.x, self.alt_savepoint.cursor.y);
        self.alt_savepoint.cursor.x = alt_sp_old.0.min(new_columns - 1);
        self.alt_savepoint.cursor.y = alt_sp_old.1.min(new_lines - 1);
        if !main_active {
            self.cursor.x = cursor_x.min(new_columns - 1);
            self.cursor.y = cursor_y.min(new_lines - 1);
        }

        // Install buffers and dimensions.
        self.main_linebuf = new_main;
        self.historybuf = new_hist;
        self.alt_linebuf = new_alt;
        self.history_scratch = Line::new(new_columns as usize);
        self.lines = new_lines;
        self.columns = new_columns;
        self.margin_top = 0;
        self.margin_bottom = new_lines - 1;

        // Tab stops: initialise defaults, then copy the old prefix.
        let mut new_main_tabs = crate::screen::default_tabstops(new_columns);
        let mut new_alt_tabs = new_main_tabs.clone();
        let copy = old_columns.min(new_columns) as usize;
        new_main_tabs[..copy].copy_from_slice(&self.main_tabstops[..copy]);
        new_alt_tabs[..copy].copy_from_slice(&self.alt_tabstops[..copy]);
        self.main_tabstops = new_main_tabs;
        self.alt_tabstops = new_alt_tabs;

        self.cursor.x = self.cursor.x.min(new_columns - 1);
        self.cursor.y = self.cursor.y.min(new_lines - 1);
        self.scrolled_by = self.scrolled_by.min(self.historybuf.count);
        self.is_dirty = true;
        self.scroll_changed = true;

        let alt_before = content_rows(&self.alt_linebuf);
        ResizeResult {
            old_lines,
            old_columns,
            lines: new_lines,
            columns: new_columns,
            num_content_lines_before: if main_active {
                content_before
            } else {
                alt_before
            },
            num_content_lines_after: if main_active {
                main_num_after
            } else {
                alt_before.min(new_lines)
            },
            main_content_lines_before: content_before,
            main_content_lines_after: main_num_after,
            alternate_content_lines_before: alt_before,
            alternate_content_lines_after: alt_before.min(new_lines),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(cols: u32, lines: u32, scrollback: u32) -> Screen {
        Screen::builder(cols, lines)
            .scrollback(scrollback)
            .build()
            .unwrap()
    }

    fn draw(s: &mut Screen, text: &str) {
        let chars: Vec<u32> = text.chars().map(|c| c as u32).collect();
        s.draw_text(&chars);
    }

    #[test]
    fn narrower_rewraps_and_preserves_content() {
        let mut s = screen(5, 5, 20);
        draw(&mut s, "abcdefghij");
        let r = s.resize(5, 3);
        assert_eq!((r.lines, r.columns), (5, 3));
        assert_eq!(s.line_text(0), "abc");
        assert_eq!(s.line_text(1), "def");
        assert_eq!(s.line_text(2), "ghi");
        assert_eq!(s.line_text(3), "j");
        assert!(s.line_is_continued(1));
        assert!(s.line_is_continued(2));
        assert!(s.line_is_continued(3));
    }

    #[test]
    fn wider_restitches_wrapped_lines() {
        let mut s = screen(3, 4, 20);
        draw(&mut s, "abcdefgh");
        s.resize(4, 8);
        assert_eq!(s.line_text(0), "abcdefgh");
        assert_eq!(s.line_text(1), "");
    }

    #[test]
    fn history_is_rewrapped_and_overflow_dropped() {
        let mut s = screen(5, 3, 3);
        draw(&mut s, "00000");
        s.linefeed();
        draw(&mut s, "11111");
        s.linefeed();
        draw(&mut s, "22222");
        s.linefeed();
        draw(&mut s, "33333");
        assert!(s.historybuf().count > 0);
        let before = s.selected_text();
        let _ = before;
        s.resize(3, 1);
        assert!(s.historybuf().count <= 3);
        assert_eq!(s.line_text(2), "3");
    }

    #[test]
    fn cursor_tracks_rewrapped_content() {
        let mut s = screen(5, 5, 20);
        draw(&mut s, "00000");
        s.cursor.x = 1;
        s.cursor.y = 0;
        s.resize(5, 10);
        assert_eq!((s.cursor.x, s.cursor.y), (1, 0));
        s.resize(5, 2);
        assert_eq!(s.line_text(0), "00");
    }

    #[test]
    fn alternate_screen_is_not_rewrapped() {
        let mut s = screen(5, 3, 20);
        draw(&mut s, "main");
        s.toggle_screen_buffer(true, true);
        draw(&mut s, "alt");
        s.resize(4, 4);
        assert!(s.is_using_alternate_linebuf());
        assert_eq!(s.line_text(0), "alt");
        s.toggle_screen_buffer(true, false);
        assert_eq!(s.line_text(0), "main");
    }

    #[test]
    fn trailing_blank_line_and_cursor_below_content() {
        let mut s = screen(5, 5, 15);
        for i in 0..3 {
            draw(&mut s, &format!("oo{i}"));
            s.index();
            s.carriage_return();
        }
        draw(&mut s, "$ pp");
        s.index();
        s.carriage_return();
        s.resize(5, 2);
        assert_eq!(s.cursor.x, 0);
        assert_eq!(s.line_text(s.cursor.y), "");
    }

    #[test]
    fn tab_stops_survive_resize() {
        let mut s = screen(20, 3, 10);
        s.cursor.x = 5;
        s.set_tab_stop();
        s.resize(3, 10);
        assert_eq!(s.line_text(0), "");
        s.cursor.x = 0;
        s.tab();
        assert_eq!(s.cursor.x, 5);
    }
}

#[cfg(test)]
mod ported_tests {
    use super::*;

    fn screen(cols: u32, lines: u32, scrollback: u32) -> Screen {
        Screen::builder(cols, lines)
            .scrollback(scrollback)
            .build()
            .unwrap()
    }

    fn draw(s: &mut Screen, text: &str) {
        let chars: Vec<u32> = text.chars().map(|c| c as u32).collect();
        s.draw_text(&chars);
    }

    fn history_string(s: &Screen) -> String {
        let mut out = String::new();
        for lnum in 0..s.historybuf().count {
            if lnum != 0 {
                out.push('\n');
            }
            out.push_str(&s.history_line_text(lnum));
        }
        out
    }

    fn linebuf_string(s: &Screen) -> String {
        (0..s.lines)
            .map(|y| s.line_text(y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn all_text(s: &Screen) -> String {
        let mut out = String::new();
        let mut first = true;
        let mut prev_wrapped = false;
        for lnum in (0..s.historybuf().count).rev() {
            if !first && !prev_wrapped {
                out.push('\n');
            }
            first = false;
            out.push_str(&s.history_line_text(lnum));
            prev_wrapped = s
                .history_line(lnum)
                .map(|l| {
                    l.cpu_cells
                        .last()
                        .map(|c| c.next_char_was_wrapped())
                        .unwrap_or(false)
                })
                .unwrap_or(false);
        }
        for y in 0..s.lines {
            if !first && !prev_wrapped {
                out.push('\n');
            }
            first = false;
            out.push_str(&s.line_text(y));
            prev_wrapped = s
                .line(y)
                .map(|l| {
                    l.cpu_cells
                        .last()
                        .map(|c| c.next_char_was_wrapped())
                        .unwrap_or(false)
                })
                .unwrap_or(false);
        }
        out
    }

    #[test]
    fn kitty_rewrap_split_history() {
        let mut s = screen(4, 4, 4);
        let mut text = String::new();
        for i in 0..s.lines + 1 {
            if i == 2 {
                text.push_str("abcd");
            } else {
                text.push_str(&(i + 1).to_string().repeat(s.columns as usize));
            }
        }
        draw(&mut s, &text);
        assert!(s.historybuf().endswith_wrap());
        s.cursor.x = 1;
        s.cursor.y = 1;
        s.resize(4, 6);
        assert!(s.historybuf().endswith_wrap());
        assert_eq!(history_string(&s), "111122");
        assert_eq!(all_text(&s), text + "\n");
        assert_eq!((s.cursor.x, s.cursor.y), (3, 0));
    }
    #[test]
    fn kitty_rewrap_two_history_lines_and_screen() {
        let mut s = screen(4, 4, 4);
        draw(&mut s, "1111222");
        s.linefeed();
        s.carriage_return();
        draw(&mut s, "333344445555");
        s.resize(4, 6);
        assert_eq!(history_string(&s), "111122");
        assert_eq!(s.line_text(0), "2");
        assert_eq!(all_text(&s), "1111222\n333344445555\n");
    }

    #[test]
    fn kitty_rewrap_wide_emoji_and_empty_lines() {
        let mut s = screen(4, 4, 4);
        draw(&mut s, "1111\u{1f638}2");
        s.linefeed();
        s.carriage_return();
        s.index();
        s.index();
        s.resize(4, 5);
        assert_eq!(history_string(&s), "1111");
        assert!(s.historybuf().endswith_wrap());
        assert_eq!(all_text(&s), "1111\u{1f638}2\n\n\n");
    }

    #[test]
    fn kitty_rewrap_double_width_preserves_text_and_cursor() {
        let mut s = screen(4, 4, 4);
        let mut text = String::new();
        for i in 0..s.lines + 1 {
            if i == 2 {
                text.push_str("abcd");
            } else {
                text.push_str(&(i + 1).to_string().repeat(s.columns as usize));
            }
        }
        draw(&mut s, &text);
        s.cursor.x = 1;
        s.cursor.y = 1;
        s.resize(4, 8);
        assert_eq!(history_string(&s), "11112222");
        assert_eq!(all_text(&s), text + "\n\n");
        assert_eq!((s.cursor.x, s.cursor.y), (1, 0));
    }

    #[test]
    fn kitty_narrow_shrinks_a_wrapped_line() {
        let mut s = screen(5, 5, 5);
        draw(&mut s, "12345");
        s.carriage_return();
        s.index();
        s.resize(5, 4);
        assert_eq!(
            (s.line_text(0), s.line_text(1), s.line_text(2)),
            ("1234".to_string(), "5".to_string(), String::new())
        );
    }

    #[test]
    fn kitty_resize_height_and_width_history_cases() {
        let mut s = screen(5, 5, 6);
        let rows: String = (0..s.lines)
            .map(|i| i.to_string().repeat(s.columns as usize))
            .collect();
        draw(&mut s, &rows);
        s.resize(3, 10);
        assert_eq!(s.line_text(0), "0000011111");
        assert_eq!(s.line_text(1), "2222233333");
        assert_eq!(s.line_text(2), "44444");
        s.resize(5, 1);
        assert_eq!(s.line_text(0), "4");
        assert_eq!(history_string(&s), "3\n3\n3\n3\n3\n2");
    }

    #[test]
    fn kitty_rewrap_scrollback_capacity_and_text_preserved() {
        let mut s = screen(5, 5, 20);
        let rows: String = (0..s.lines * 2)
            .map(|i| i.to_string().repeat(s.columns as usize))
            .collect();
        draw(&mut s, &rows);
        assert_eq!(linebuf_string(&s), "55555\n66666\n77777\n88888\n99999");
        let before = all_text(&s);
        s.resize(5, 2);
        assert_eq!(before, all_text(&s));
        assert_eq!(linebuf_string(&s), "88\n88\n89\n99\n99");
    }
}
