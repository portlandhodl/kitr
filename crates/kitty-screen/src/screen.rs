// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/screen.c, kitty/chargrid.h and kitty/line.c
// Copyright (C) 2016-2024 Kovid Goyal <kovid at kovidgoyal.net>

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use kitty_core::cell::{CharType, VS15, VS16};
use kitty_core::{
    CpuCell, GpuCell, HistoryBuf, HistoryBufError, Line, LineBuf, LineBufError, TextCache,
};
use kitty_simd::printable_ascii_run_length;
use kitty_unicode::{
    char_props_for, grapheme_segmentation_reset, grapheme_segmentation_step, wcwidth_std,
    CharProps, GraphemeBreakProperty, GraphemeSegmentationResult,
};

use kitty_graphics::{GraphicsError, Image, ImageAssembler};

use crate::charset::CharsetState;
use crate::colors::{ColorOverrides, ColorStack};
use crate::cursor::Cursor;
use crate::graphics::{GraphicsQuery, ImagePlacement, UnsupportedGraphics};
use crate::hyperlink::{HyperlinkAt, HyperlinkPool, HyperlinkSpan};
use crate::lineops::{
    apply_sgr_to_cells, line_apply_cursor, line_as_ansi_with_links, line_as_text,
};
use crate::modes::{self, Modes};
use crate::selection::{range_line, Selections};

/// `CSI_REP_MAX_REPETITIONS`.
pub const CSI_REP_MAX_REPETITIONS: u32 = 65535;

/// Upper bound used by the C code for a single REP operation.
const DEL: u32 = 0x7f;

/// Errors returned while constructing a [`Screen`].
#[derive(Debug)]
pub enum ScreenError {
    LineBuf(LineBufError),
    History(HistoryBufError),
}

impl core::fmt::Display for ScreenError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::LineBuf(e) => write!(f, "line buffer: {e}"),
            Self::History(e) => write!(f, "history buffer: {e}"),
        }
    }
}

impl std::error::Error for ScreenError {}

impl From<LineBufError> for ScreenError {
    fn from(e: LineBufError) -> Self {
        Self::LineBuf(e)
    }
}

impl From<HistoryBufError> for ScreenError {
    fn from(e: HistoryBufError) -> Self {
        Self::History(e)
    }
}

/// Options accepted by [`ScreenBuilder`].
///
/// Only fields required by the implemented subset are present. Font metrics,
/// graphics sizing and shell integration options from the C `Screen`
/// constructor are not needed by this port.
#[derive(Clone, Debug, Default)]
pub struct ScreenOptions {
    /// Scrollback capacity in lines. The effective capacity is
    /// `max(scrollback, lines)`, matching `alloc_historybuf()` in C.
    pub scrollback: u32,
    /// Device pixels per cell. `0` means unknown; graphics placements then
    /// cannot derive their size from pixel dimensions.
    pub cell_width: u32,
    pub cell_height: u32,
}

/// Builder for [`Screen`].
#[derive(Clone, Debug)]
pub struct ScreenBuilder {
    columns: u32,
    lines: u32,
    options: ScreenOptions,
}

impl ScreenBuilder {
    /// Create a builder. The argument order is `(columns, lines)`, matching
    /// the intuitive order rather than the C constructor's `(lines, columns)`.
    pub fn new(columns: u32, lines: u32) -> Self {
        Self {
            columns,
            lines,
            options: ScreenOptions::default(),
        }
    }

    pub fn scrollback(mut self, lines: u32) -> Self {
        self.options.scrollback = lines;
        self
    }

    pub fn options(mut self, options: ScreenOptions) -> Self {
        self.options = options;
        self
    }

    pub fn build(self) -> Result<Screen, ScreenError> {
        Screen::with_options(self.lines, self.columns, self.options)
    }
}

/// A savepoint used by DECSC/DECRC (`Savepoint` in `screen.h`).
#[derive(Clone, Debug, Default)]
pub(crate) struct Savepoint {
    pub(crate) cursor: Cursor,
    modes: Modes,
    charset: CharsetState,
    is_valid: bool,
}

/// The segmentation result corresponding to C's
/// `(GraphemeSegmentationResult){.grapheme_break = GBP_None}`.
#[inline]
fn none_seg() -> GraphemeSegmentationResult {
    GraphemeSegmentationResult::from_bits((GraphemeBreakProperty::GBP_None as u16) << 7)
}

/// Width of a single code point with a freshly reset `wcswidth` state.
///
/// This is exactly what `wcswidth_std(&[ch])` computes for one character, but
/// reuses the `CharProps` the caller already looked up.
#[inline]
fn single_char_width(cp: CharProps) -> i32 {
    match wcwidth_std(cp) {
        -1 | 0 => 0,
        2 => 2,
        _ => 1,
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct PrevCell {
    x: u32,
    y: u32,
    valid: bool,
}

#[derive(Clone, Copy, Debug)]
struct TextLoopState {
    prev: PrevCell,
    seg: GraphemeSegmentationResult,
    image_placeholder_marked: bool,
}

impl Default for TextLoopState {
    fn default() -> Self {
        Self {
            prev: PrevCell::default(),
            seg: GraphemeSegmentationResult::new(),
            image_placeholder_marked: false,
        }
    }
}

/// The terminal screen.
///
/// Owns two line buffers (main and alternate), the scrollback history buffer,
/// the cursor, margins, tab stops, modes and charset state. All buffers share
/// one text cache, exactly like the C implementation.
pub struct Screen {
    pub columns: u32,
    pub lines: u32,
    pub margin_top: u32,
    pub margin_bottom: u32,
    pub cursor: Cursor,
    pub modes: Modes,
    pub saved_modes: Modes,
    pub charset: CharsetState,
    pub is_dirty: bool,
    pub scroll_changed: bool,
    pub scrolled_by: u32,
    pub history_line_added_count: u64,
    pub title: Option<String>,
    pub icon_name: Option<String>,
    pub bell_count: u64,
    pub last_graphic_char: CharType,
    pub active_hyperlink_id: u16,

    pub(crate) text_cache: Rc<RefCell<TextCache>>,
    pub(crate) main_linebuf: LineBuf,
    pub(crate) alt_linebuf: LineBuf,
    pub(crate) on_alt: bool,
    pub(crate) historybuf: HistoryBuf,
    pub(crate) main_tabstops: Vec<bool>,
    pub(crate) alt_tabstops: Vec<bool>,
    pub(crate) main_savepoint: Savepoint,
    pub(crate) alt_savepoint: Savepoint,
    pub(crate) history_scratch: Line,

    pub(crate) hyperlinks: HyperlinkPool,
    pub(crate) selections: Selections,
    pub(crate) images: HashMap<u32, Image>,
    pub(crate) placements: Vec<ImagePlacement>,
    pub(crate) image_assembler: ImageAssembler,
    pub(crate) graphics_queries: Vec<GraphicsQuery>,
    pub(crate) graphics_unsupported: Vec<UnsupportedGraphics>,
    pub last_graphics_error: Option<GraphicsError>,
    /// Device pixels per cell, mirrored from the window's font metrics.
    pub cell_width: u32,
    pub cell_height: u32,
    pub(crate) colors: ColorOverrides,
    pub(crate) color_stack: ColorStack,
    pub(crate) output: Vec<u8>,
}

impl Screen {
    /// Construct a screen with the C constructor's `(lines, columns)` order and
    /// the given scrollback size.
    pub fn new(lines: u32, columns: u32, scrollback: u32) -> Result<Self, ScreenError> {
        Self::with_options(
            lines,
            columns,
            ScreenOptions {
                scrollback,
                cell_width: 0,
                cell_height: 0,
            },
        )
    }

    /// Construct a screen from [`ScreenOptions`].
    pub fn with_options(
        lines: u32,
        columns: u32,
        options: ScreenOptions,
    ) -> Result<Self, ScreenError> {
        let tc: Rc<RefCell<TextCache>> = Rc::new(RefCell::new(TextCache::new()));
        let main_linebuf = LineBuf::new(lines, columns, tc.clone())?;
        let alt_linebuf = LineBuf::new(lines, columns, tc.clone())?;
        let historybuf = HistoryBuf::new(columns, options.scrollback.max(lines), tc.clone())?;
        let tabstops = default_tabstops(columns);
        let mut screen = Self {
            columns,
            lines,
            margin_top: 0,
            margin_bottom: lines - 1,
            cursor: Cursor::new(),
            modes: default_modes(),
            saved_modes: default_modes(),
            charset: CharsetState::default(),
            is_dirty: true,
            scroll_changed: false,
            scrolled_by: 0,
            history_line_added_count: 0,
            title: None,
            icon_name: None,
            bell_count: 0,
            last_graphic_char: 0,
            active_hyperlink_id: 0,
            text_cache: tc,
            main_linebuf,
            alt_linebuf,
            on_alt: false,
            historybuf,
            main_tabstops: tabstops.clone(),
            alt_tabstops: tabstops,
            main_savepoint: Savepoint::default(),
            alt_savepoint: Savepoint::default(),
            history_scratch: Line::new(columns as usize),
            hyperlinks: HyperlinkPool::new(),
            selections: Selections::default(),
            images: HashMap::new(),
            placements: Vec::new(),
            image_assembler: ImageAssembler::default(),
            graphics_queries: Vec::new(),
            graphics_unsupported: Vec::new(),
            last_graphics_error: None,
            cell_width: options.cell_width,
            cell_height: options.cell_height,
            colors: ColorOverrides::new(),
            color_stack: ColorStack::default(),
            output: Vec::new(),
        };
        screen.cursor.x = 0;
        screen.cursor.y = 0;
        Ok(screen)
    }

    /// Update the device pixel size of one cell. The renderer uses this for
    /// graphics placements whose `c=`/`r=` values are omitted.
    pub fn set_cell_size(&mut self, width: u32, height: u32) {
        self.cell_width = width;
        self.cell_height = height;
    }

    /// Device pixel size of one cell, if known.
    pub fn cell_size(&self) -> (u32, u32) {
        (self.cell_width, self.cell_height)
    }

    /// Create a builder for a screen.
    pub fn builder(columns: u32, lines: u32) -> ScreenBuilder {
        ScreenBuilder::new(columns, lines)
    }

    #[inline]
    pub fn text_cache(&self) -> &Rc<RefCell<TextCache>> {
        &self.text_cache
    }

    #[inline]
    fn lb(&self) -> &LineBuf {
        if self.on_alt {
            &self.alt_linebuf
        } else {
            &self.main_linebuf
        }
    }

    #[inline]
    pub(crate) fn lb_mut(&mut self) -> &mut LineBuf {
        if self.on_alt {
            &mut self.alt_linebuf
        } else {
            &mut self.main_linebuf
        }
    }

    #[inline]
    fn tabstops(&self) -> &[bool] {
        if self.on_alt {
            &self.alt_tabstops
        } else {
            &self.main_tabstops
        }
    }

    #[inline]
    fn tabstops_mut(&mut self) -> &mut [bool] {
        if self.on_alt {
            &mut self.alt_tabstops
        } else {
            &mut self.main_tabstops
        }
    }

    #[inline]
    pub fn historybuf(&self) -> &HistoryBuf {
        &self.historybuf
    }

    #[inline]
    pub fn is_main_linebuf(&self) -> bool {
        !self.on_alt
    }

    #[inline]
    pub fn is_using_alternate_linebuf(&self) -> bool {
        self.on_alt
    }

    #[inline]
    pub fn linebuf(&self) -> &LineBuf {
        self.lb()
    }

    #[inline]
    pub(crate) fn linebuf_mut(&mut self) -> &mut LineBuf {
        self.lb_mut()
    }

    #[inline]
    pub fn hyperlinks(&self) -> &HyperlinkPool {
        &self.hyperlinks
    }

    /// Dirty screen rows, in row order.
    pub fn dirty_lines(&self) -> Vec<usize> {
        self.lb().dirty_lines()
    }

    /// Mark every live screen row clean.
    pub fn reset_dirty(&mut self) {
        for y in 0..self.lines as usize {
            self.lb_mut().mark_line_clean(y);
        }
        self.is_dirty = false;
    }

    /// Get a copy of a screen row as a [`Line`].
    pub fn line(&self, y: u32) -> Option<Line> {
        self.lb().create_line_copy(y as usize).ok()
    }

    /// `str(line)` for a screen row.
    pub fn line_text(&self, y: u32) -> String {
        match self.line(y) {
            Some(line) => {
                let tc = self.text_cache.borrow();
                line_as_text(&line, &tc)
            }
            None => String::new(),
        }
    }

    /// `line.as_ansi()` for a screen row.
    pub fn line_ansi(&self, y: u32) -> String {
        match self.line(y) {
            Some(line) => {
                let tc = self.text_cache.borrow();
                line_as_ansi_with_links(
                    &line,
                    &tc,
                    0,
                    self.columns as usize,
                    Some(&self.hyperlinks),
                )
            }
            None => String::new(),
        }
    }

    /// Get a copy of a scrollback line (0 is the newest history line).
    pub fn history_line(&self, lnum: u32) -> Option<Line> {
        self.historybuf.line(lnum).ok()
    }

    /// Plain text of a scrollback line.
    pub fn history_line_text(&self, lnum: u32) -> String {
        match self.history_line(lnum) {
            Some(line) => {
                let tc = self.text_cache.borrow();
                line_as_text(&line, &tc)
            }
            None => String::new(),
        }
    }

    /// The CPU cell at `(x, y)`.
    #[inline]
    pub fn cpu_cell(&self, x: u32, y: u32) -> CpuCell {
        *self.lb().cpu_cell_at(x as usize, y as usize)
    }

    /// The GPU cell at `(x, y)`.
    #[inline]
    pub fn gpu_cell(&self, x: u32, y: u32) -> GpuCell {
        *self.lb().gpu_cell_at(x as usize, y as usize)
    }

    /// True if the row continues onto the next row.
    pub fn line_is_continued(&self, y: u32) -> bool {
        self.lb().is_continued(y as usize)
    }

    #[inline]
    fn set_cpu_cell(&mut self, x: u32, y: u32, cell: CpuCell) {
        self.lb_mut().cpu_cells_for_line_mut(y as usize)[x as usize] = cell;
    }

    #[inline]
    fn set_gpu_cell(&mut self, x: u32, y: u32, cell: GpuCell) {
        self.lb_mut().gpu_cells_for_line_mut(y as usize)[x as usize] = cell;
    }

    // {{{ Cursor and margins

    #[inline]
    fn cursor_within_margins(&self) -> bool {
        self.margin_top <= self.cursor.y && self.cursor.y <= self.margin_bottom
    }

    /// `screen_ensure_bounds()`.
    pub fn ensure_bounds(&mut self, force_use_margins: bool, in_margins: bool) {
        let (top, bottom) =
            if in_margins && (force_use_margins || self.modes.contains(Modes::DECOM)) {
                (self.margin_top, self.margin_bottom)
            } else {
                (0, self.lines - 1)
            };
        self.cursor.x = self.cursor.x.min(self.columns - 1);
        self.cursor.y = self.cursor.y.max(top).min(bottom);
    }

    /// `screen_cursor_position()`; arguments are 1-based.
    pub fn cursor_position(&mut self, line: u32, column: u32) {
        let in_margins = self.cursor_within_margins();
        let mut line = line.max(1) - 1;
        let column = column.max(1) - 1;
        if self.modes.contains(Modes::DECOM) {
            line += self.margin_top;
            line = line.max(self.margin_top).min(self.margin_bottom);
        }
        self.cursor.x = column;
        self.cursor.y = line;
        self.ensure_bounds(false, in_margins);
    }

    /// `screen_cursor_to_line()` (VPA).
    pub fn cursor_to_line(&mut self, line: u32) {
        self.cursor_position(line, self.cursor.x + 1);
    }

    /// `screen_cursor_to_column()` (CHA/HPA), 1-based.
    pub fn cursor_to_column(&mut self, column: u32) {
        let x = column.max(1) - 1;
        if x != self.cursor.x {
            self.cursor.x = x;
            let in_margins = self.cursor_within_margins();
            self.ensure_bounds(false, in_margins);
        }
    }

    /// `screen_cursor_move()`. A negative `move_direction` moves left/up and
    /// may wrap to the previous line only when `allow_move_to_previous_line`.
    pub fn cursor_move(
        &mut self,
        count: u32,
        move_direction: i32,
        allow_move_to_previous_line: bool,
    ) {
        let mut count = count.max(1);
        let in_margins = self.cursor_within_margins();
        if move_direction > 0 {
            self.cursor.x += count;
            self.ensure_bounds(false, in_margins);
        } else {
            let top = if in_margins && self.modes.contains(Modes::DECOM) {
                self.margin_top
            } else {
                0
            };
            while count > 0 {
                if count <= self.cursor.x {
                    self.cursor.x -= count;
                    count = 0;
                } else if self.cursor.x > 0 {
                    count -= self.cursor.x;
                    self.cursor.x = 0;
                } else if self.cursor.y == top || !allow_move_to_previous_line {
                    count = 0;
                } else {
                    count -= 1;
                    self.cursor.y -= 1;
                    self.cursor.x = self.columns - 1;
                }
            }
        }
    }

    /// `screen_cursor_forward()`.
    pub fn cursor_forward(&mut self, count: u32) {
        self.cursor_move(count, 1, false);
    }

    /// `screen_cursor_up()`.
    pub fn cursor_up_move(&mut self, count: u32, do_carriage_return: bool, move_direction: i32) {
        let in_margins = self.cursor_within_margins();
        let count = count.max(1);
        if move_direction < 0 && count > self.cursor.y {
            self.cursor.y = 0;
        } else {
            self.cursor.y =
                (self.cursor.y as i64 + move_direction as i64 * count as i64).max(0) as u32;
        }
        if do_carriage_return {
            self.cursor.x = 0;
        }
        self.ensure_bounds(true, in_margins);
    }

    /// `screen_cursor_up()`.
    pub fn cursor_up(&mut self, count: u32) {
        self.cursor_up_move(count, false, -1);
    }

    /// `screen_cursor_up1()`.
    pub fn cursor_up1(&mut self, count: u32) {
        self.cursor_up_move(count, true, -1);
    }

    /// `screen_cursor_down()`.
    pub fn cursor_down(&mut self, count: u32) {
        self.cursor_up_move(count, false, 1);
    }

    /// `screen_cursor_down1()`.
    pub fn cursor_down1(&mut self, count: u32) {
        self.cursor_up_move(count, true, 1);
    }

    /// `screen_backspace()`.
    pub fn backspace(&mut self) {
        self.cursor_move(1, -1, true);
    }

    /// `screen_carriage_return()`.
    pub fn carriage_return(&mut self) {
        self.cursor.x = 0;
    }

    /// `screen_set_margins()` (DECSTBM); arguments are 1-based, 0 means default.
    pub fn set_margins(&mut self, top: u32, bottom: u32) {
        let top = if top == 0 { 1 } else { top }.min(self.lines);
        let bottom = if bottom == 0 { self.lines } else { bottom }.min(self.lines);
        let top = top - 1;
        let bottom = bottom - 1;
        if bottom > top {
            self.margin_top = top;
            self.margin_bottom = bottom;
            self.cursor_position(1, 1);
        }
    }

    // }}}

    // {{{ Scrolling and line feed

    #[inline]
    fn continue_to_next_line(&mut self) {
        let y = self.cursor.y;
        self.lb_mut()
            .set_last_char_as_continuation(y as usize, true);
        self.cursor.x = 0;
        self.linefeed();
    }

    fn index_up(&mut self, add_to_history: bool) {
        let top = self.margin_top as usize;
        let bottom = self.margin_bottom as usize;
        self.lb_mut().index(top, bottom);
        if add_to_history {
            // Reuse one owned line instead of allocating (and zeroing) a new
            // one for every scroll. `LineBuf::create_line_copy` did both.
            let mut line = core::mem::take(&mut self.history_scratch);
            {
                let lb = self.lb();
                let xnum = lb.xnum as usize;
                if line.cpu_cells.len() != xnum {
                    line.cpu_cells.resize(xnum, CpuCell::new());
                }
                if line.gpu_cells.len() != xnum {
                    line.gpu_cells.resize(xnum, GpuCell::default());
                }
                line.cpu_cells
                    .copy_from_slice(lb.cpu_cells_for_line(bottom));
                line.gpu_cells
                    .copy_from_slice(lb.gpu_cells_for_line(bottom));
                line.attrs = lb.line_attrs(bottom);
            }
            self.historybuf.add_line(&line);
            self.history_scratch = line;
            self.history_line_added_count += 1;
        }
        self.lb_mut().clear_line(bottom, true);
        self.index_selections(true);
        self.is_dirty = true;
    }

    fn index_down(&mut self) {
        let top = self.margin_top as usize;
        let bottom = self.margin_bottom as usize;
        self.lb_mut().reverse_index(top, bottom);
        self.lb_mut().clear_line(top, true);
        self.index_selections(false);
        self.is_dirty = true;
    }

    /// `screen_index()`.
    pub fn index(&mut self) {
        let bottom = self.margin_bottom;
        if self.cursor.y == bottom {
            let add_to_history = !self.on_alt && self.margin_top == 0;
            self.index_up(add_to_history);
        } else {
            self.cursor_down(1);
        }
    }

    /// `screen_reverse_index()`.
    pub fn reverse_index(&mut self) {
        let top = self.margin_top;
        if self.cursor.y == top {
            self.index_down();
        } else {
            self.cursor_up_move(1, false, -1);
        }
    }

    /// `screen_scroll()`.
    pub fn scroll(&mut self, count: u32) {
        let add_to_history = !self.on_alt && self.margin_top == 0;
        let count = count.min(self.lines);
        for _ in 0..count {
            self.index_up(add_to_history);
        }
    }

    /// `screen_reverse_scroll()`.
    pub fn reverse_scroll(&mut self, count: u32) {
        let count = count.min(self.lines);
        for _ in 0..count {
            self.index_down();
        }
    }

    /// `screen_reverse_scroll_and_fill_from_scrollback()`.
    pub fn reverse_scroll_and_fill_from_scrollback(&mut self, count: u32) {
        let fill = !self.on_alt;
        let limit = if fill {
            self.lines.max(self.historybuf.count)
        } else {
            self.lines
        };
        let count = count.min(limit);
        for _ in 0..count {
            let mut popped = Line::new(self.columns as usize);
            let copied = fill && self.historybuf.pop_line(&mut popped);
            self.index_down();
            if copied {
                self.main_linebuf.copy_line_to(&popped, 0);
            }
        }
    }

    /// `screen_linefeed()`.
    pub fn linefeed(&mut self) {
        let in_margins = self.cursor_within_margins();
        self.index();
        if self.modes.contains(Modes::LNM) {
            self.carriage_return();
        }
        self.ensure_bounds(false, in_margins);
    }

    // }}}

    // {{{ Tabs

    fn reset_tab_stops_for(&mut self, alt: bool) {
        let stops = default_tabstops(self.columns);
        if alt {
            self.alt_tabstops = stops;
        } else {
            self.main_tabstops = stops;
        }
    }

    /// `screen_tab()`: move to the next tab stop (no character is drawn).
    pub fn tab(&mut self) {
        let mut found = 0u32;
        for i in self.cursor.x + 1..self.columns {
            if self.tabstops()[i as usize] {
                found = i;
                break;
            }
        }
        if found == 0 {
            found = self.columns - 1;
        }
        if found != self.cursor.x {
            if self.cursor.x < self.columns {
                let x = self.cursor.x;
                let diff = found - x;
                let mut ok = true;
                for i in 0..diff {
                    let c = self.cpu_cell(x + i, self.cursor.y);
                    if c.has_text() && !c.is_char(' ' as u32) {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    for i in 0..diff {
                        let mut c = self.cpu_cell(x + i, self.cursor.y);
                        c.set_char(' ' as u32);
                        c.set_ch_is_idx(false);
                        self.set_cpu_cell(x + i, self.cursor.y, c);
                    }
                    let cache_chars = ['\t' as u32, diff - 1];
                    let idx = self.text_cache.borrow_mut().get_or_insert(&cache_chars);
                    let mut c = self.cpu_cell(x, self.cursor.y);
                    c.set_char(idx);
                    c.set_ch_is_idx(true);
                    self.set_cpu_cell(x, self.cursor.y, c);
                }
            }
            self.cursor.x = found;
        }
    }

    /// `screen_backtab()`.
    pub fn backtab(&mut self, count: u32) {
        let mut count = count.max(1);
        while count > 0 && self.cursor.x > 0 {
            count -= 1;
            let mut found = 0;
            for i in (0..self.cursor.x).rev() {
                if self.tabstops()[i as usize] {
                    self.cursor.x = i;
                    found = 1;
                    break;
                }
            }
            if found == 0 {
                self.cursor.x = 0;
            }
        }
    }

    /// `screen_set_tab_stop()` (HTS).
    pub fn set_tab_stop(&mut self) {
        let x = self.cursor.x;
        if x < self.columns {
            self.tabstops_mut()[x as usize] = true;
        }
    }

    /// `screen_clear_tab_stop()` (TBC).
    pub fn clear_tab_stop(&mut self, how: u32) -> bool {
        match how {
            0 => {
                let x = self.cursor.x;
                if x < self.columns {
                    self.tabstops_mut()[x as usize] = false;
                }
                true
            }
            2 => true,
            3 => {
                for t in self.tabstops_mut() {
                    *t = false;
                }
                true
            }
            _ => false,
        }
    }

    /// `screen_reset_tab_stops()` (DECST8C).
    pub fn reset_tab_stops(&mut self) {
        let alt = self.on_alt;
        self.reset_tab_stops_for(alt);
    }

    // }}}

    // {{{ Save/restore and modes

    fn savepoint(&self) -> &Savepoint {
        if self.on_alt {
            &self.alt_savepoint
        } else {
            &self.main_savepoint
        }
    }

    fn savepoint_mut(&mut self) -> &mut Savepoint {
        if self.on_alt {
            &mut self.alt_savepoint
        } else {
            &mut self.main_savepoint
        }
    }

    /// `screen_save_cursor()` (DECSC).
    pub fn save_cursor(&mut self) {
        let cursor = self.cursor;
        let modes = self.modes;
        let charset = self.charset;
        let sp = self.savepoint_mut();
        sp.cursor = cursor;
        sp.modes = modes;
        sp.charset = charset;
        sp.is_valid = true;
    }

    /// `screen_restore_cursor()` (DECRC).
    pub fn restore_cursor(&mut self) {
        if !self.savepoint().is_valid {
            self.cursor_position(1, 1);
            self.modes.remove(Modes::DECOM);
            self.modes.remove(Modes::DECSCNM);
            self.charset.reset();
            return;
        }
        let sp = self.savepoint().clone();
        self.modes
            .set(Modes::DECOM, sp.modes.contains(Modes::DECOM));
        self.modes
            .set(Modes::DECAWM, sp.modes.contains(Modes::DECAWM));
        self.modes
            .set(Modes::DECSCNM, sp.modes.contains(Modes::DECSCNM));
        self.cursor = sp.cursor;
        self.charset = sp.charset;
        self.ensure_bounds(false, false);
    }

    /// `screen_save_modes()` (kitty XTSAVE extension).
    pub fn save_modes(&mut self) {
        self.saved_modes = self.modes;
    }

    /// `screen_restore_modes()` (kitty XTRESTORE extension).
    pub fn restore_modes(&mut self) {
        self.modes = self.saved_modes;
    }

    /// `screen_set_mode()`. Returns `false` when the mode is not implemented.
    pub fn set_mode(&mut self, mode: u32) -> bool {
        self.apply_mode(mode, true)
    }

    /// `screen_reset_mode()`. Returns `false` when the mode is not implemented.
    pub fn reset_mode(&mut self, mode: u32) -> bool {
        self.apply_mode(mode, false)
    }

    fn apply_mode(&mut self, mode: u32, val: bool) -> bool {
        match mode {
            modes::LNM => self.modes.set(Modes::LNM, val),
            modes::IRM => self.modes.set(Modes::IRM, val),
            modes::DECARM => self.modes.set(Modes::DECARM, val),
            modes::DECCKM => self.modes.set(Modes::DECCKM, val),
            modes::DECTCEM => self.modes.set(Modes::DECTCEM, val),
            modes::BRACKETED_PASTE => self.modes.set(Modes::BRACKETED_PASTE, val),
            modes::FOCUS_TRACKING => self.modes.set(Modes::FOCUS_TRACKING, val),
            modes::DECSCNM => {
                if self.modes.contains(Modes::DECSCNM) != val {
                    self.modes.set(Modes::DECSCNM, val);
                    self.is_dirty = true;
                }
            }
            modes::DECOM => {
                self.modes.set(Modes::DECOM, val);
                self.cursor_position(1, 1);
            }
            modes::DECAWM => self.modes.set(Modes::DECAWM, val),
            modes::DECCOLM => {
                self.modes.set(Modes::DECCOLM, val);
                if val {
                    self.erase_in_display(2, false);
                    self.cursor_position(1, 1);
                }
            }
            modes::CONTROL_CURSOR_BLINK => self.cursor.non_blinking = !val,
            modes::SAVE_CURSOR => self.save_cursor(),
            modes::TOGGLE_ALT_SCREEN_1 | modes::TOGGLE_ALT_SCREEN_2 | modes::ALTERNATE_SCREEN => {
                let save_cursor = mode == modes::ALTERNATE_SCREEN;
                let clear_alt = mode == modes::ALTERNATE_SCREEN;
                if val != self.on_alt {
                    self.toggle_screen_buffer(save_cursor, clear_alt);
                }
            }
            // The C screen explicitly ignores these.
            modes::DECSCLM | modes::DECNRCM => {}
            // Mouse, paste-event, visibility and pending-update modes are not
            // implemented by this port; report them rather than pretending.
            _ => return false,
        }
        true
    }

    /// `screen_toggle_screen_buffer()`.
    pub fn toggle_screen_buffer(&mut self, save_cursor: bool, clear_alt_screen: bool) {
        let to_alt = !self.on_alt;
        self.active_hyperlink_id = 0;
        self.scrolled_by = 0;
        if to_alt {
            if clear_alt_screen {
                self.alt_linebuf.clear(0);
            }
            if save_cursor {
                self.save_cursor();
            }
            self.on_alt = true;
            self.cursor_position(1, 1);
            self.cursor.reset();
        } else {
            self.on_alt = false;
            if save_cursor {
                self.restore_cursor();
            }
        }
        self.is_dirty = true;
    }

    /// `screen_set_cursor()` (DECSCUSR).
    pub fn set_cursor_shape(&mut self, mode: u32, secondary: u8) {
        if secondary != b' ' {
            return;
        }
        let mut shape = crate::cursor::CursorShape::NoCursor;
        let mut blink = true;
        if mode > 0 {
            blink = !mode.is_multiple_of(2);
            shape = if mode < 3 {
                crate::cursor::CursorShape::Block
            } else if mode < 5 {
                crate::cursor::CursorShape::Underline
            } else if mode < 7 {
                crate::cursor::CursorShape::Beam
            } else {
                crate::cursor::CursorShape::NoCursor
            };
        }
        self.cursor.shape = shape;
        self.cursor.non_blinking = !blink;
    }

    /// `screen_is_cursor_visible()`.
    pub fn is_cursor_visible(&self) -> bool {
        self.modes.contains(Modes::DECTCEM)
    }

    // }}}

    // {{{ Reset

    fn do_reset(&mut self, hard: bool) {
        if hard {
            self.on_alt = false;
        }
        self.main_savepoint.is_valid = false;
        self.alt_savepoint.is_valid = false;
        self.last_graphic_char = 0;
        if hard {
            self.main_linebuf.clear(0);
            self.alt_linebuf.clear(0);
            self.historybuf.clear();
            self.history_line_added_count = 0;
        }
        self.selections.clear();
        self.colors = ColorOverrides::new();
        self.color_stack.clear();
        if hard {
            self.hyperlinks.clear();
            self.clear_images();
        }
        self.modes = default_modes();
        self.saved_modes = default_modes();
        self.charset.reset();
        self.margin_top = 0;
        self.margin_bottom = self.lines - 1;
        self.main_tabstops = default_tabstops(self.columns);
        self.alt_tabstops = default_tabstops(self.columns);
        self.cursor.reset();
        self.cursor_position(1, 1);
        self.is_dirty = true;
    }

    /// `screen_reset()` (RIS).
    pub fn reset(&mut self) {
        self.do_reset(true);
    }

    /// `screen_soft_reset()` (DECSTR). Screen contents and cursor position are
    /// preserved.
    pub fn soft_reset(&mut self) {
        let (x, y) = (self.cursor.x, self.cursor.y);
        self.do_reset(false);
        self.cursor.x = x;
        self.cursor.y = y;
    }

    // }}}

    // {{{ Drawing

    fn map_char(&self, ch: u32) -> u32 {
        self.charset.map_char(ch)
    }

    fn init_prev_cell(&mut self, st: &mut TextLoopState) {
        st.prev = PrevCell::default();
        if self.cursor.x > 0 {
            st.prev.x = self.cursor.x - 1;
            st.prev.y = self.cursor.y;
            st.prev.valid = true;
        } else if self.cursor.y > 0 {
            st.prev.x = self.columns - 1;
            st.prev.y = self.cursor.y - 1;
            let c = self.cpu_cell(st.prev.x, st.prev.y);
            st.prev.valid = c.next_char_was_wrapped();
        }
    }

    fn init_segmentation_state(&mut self, st: &mut TextLoopState) {
        self.init_prev_cell(st);
        grapheme_segmentation_reset(&mut st.seg);
        if !st.prev.valid {
            return;
        }
        let cell = self.cpu_cell(st.prev.x, st.prev.y);
        if !cell.ch_is_idx() {
            let ch = cell.ch();
            if (b' ' as u32..DEL).contains(&ch) {
                st.seg = none_seg();
            } else if ch == 0 {
                st.seg = grapheme_segmentation_step(st.seg, char_props_for(0));
            } else {
                st.seg = grapheme_segmentation_step(st.seg, char_props_for(ch));
            }
        } else {
            let chars: Vec<u32> = self.text_cache.borrow().chars_at(cell.ch()).to_vec();
            for ch in chars {
                st.seg = grapheme_segmentation_step(st.seg, char_props_for(ch));
            }
        }
    }

    fn init_text_loop_line(&mut self, st: &mut TextLoopState) {
        let y = self.cursor.y;
        self.lb_mut().mark_line_dirty(y as usize);
        st.image_placeholder_marked = false;
        self.init_segmentation_state(st);
    }

    /// `nuke_multicell_char_at()` for single-line multicell cells.
    fn nuke_multicell_char_at(&mut self, x: u32, y: u32, replace_with_spaces: bool) {
        let mut start = x;
        while start > 0 {
            let c = self.cpu_cell(start, y);
            if c.is_multicell() && c.x() != 0 {
                start -= 1;
            } else {
                break;
            }
        }
        let base = self.cpu_cell(start, y);
        let width = if base.is_multicell() {
            base.mcd_x_limit()
        } else {
            1
        };
        let limit = self.columns.min(start + width);
        let ch = if replace_with_spaces { b' ' as u32 } else { 0 };
        for cx in start..limit {
            let mut c = self.cpu_cell(cx, y);
            c.set_char(ch);
            c.set_ch_is_idx(false);
            c.set_is_multicell(false);
            c.set_width(0);
            c.set_x(0);
            c.set_y(0);
            self.set_cpu_cell(cx, y, c);
            let mut g = self.gpu_cell(cx, y);
            g.sprite_idx = 0;
            self.set_gpu_cell(cx, y, g);
        }
        self.lb_mut().mark_line_dirty(y as usize);
        self.is_dirty = true;
    }

    fn add_combining_char(&mut self, ch: CharType, x: u32, y: u32) -> bool {
        let cell = self.cpu_cell(x, y);
        if !cell.has_text() || (cell.is_multicell() && cell.y() != 0) {
            return false;
        }
        let mut chars: Vec<u32> = if cell.ch_is_idx() {
            self.text_cache.borrow().chars_at(cell.ch()).to_vec()
        } else {
            vec![cell.ch()]
        };
        if chars.len() >= kitty_core::cell::MAX_NUM_CODEPOINTS_PER_CELL as usize {
            return false;
        }
        chars.push(ch);
        let idx = self.text_cache.borrow_mut().get_or_insert(&chars);
        let mut new_cell = cell;
        new_cell.set_char(idx);
        new_cell.set_ch_is_idx(true);
        self.set_cpu_cell(x, y, new_cell);
        if cell.is_multicell() {
            let start = x.saturating_sub(cell.x());
            let width = cell.mcd_x_limit();
            let limit = self.columns.min(start + width);
            for cx in start..limit {
                let mut c = self.cpu_cell(cx, y);
                c.set_char(idx);
                c.set_ch_is_idx(true);
                self.set_cpu_cell(cx, y, c);
            }
        }
        true
    }

    fn widen_cell_to_two(&mut self, x: u32, y: u32) {
        if x + 1 >= self.columns {
            return;
        }
        let second = self.cpu_cell(x + 1, y);
        if second.is_multicell() {
            self.nuke_multicell_char_at(x + 1, y, false);
        }
        let mut first = self.cpu_cell(x, y);
        first.set_is_multicell(true);
        first.set_width(2);
        first.set_scale_field(1);
        first.set_natural_width(true);
        first.set_x(0);
        first.set_y(0);
        let mut continuation = first;
        continuation.set_x(1);
        self.set_cpu_cell(x, y, first);
        self.set_cpu_cell(x + 1, y, continuation);
        let g = self.gpu_cell(x, y);
        self.set_gpu_cell(x + 1, y, g);
        self.cursor.x += 1;
        self.is_dirty = true;
    }

    fn halve_multicell_width(&mut self, x: u32, y: u32) -> bool {
        let cell = self.cpu_cell(x, y);
        if !cell.is_multicell() {
            return false;
        }
        let new_width = cell.width() / 2;
        let start = x.saturating_sub(cell.x());
        let limit = self.columns.min(start + cell.mcd_x_limit());
        let half = self.columns.min(start + cell.mcd_x_limit() / 2);
        for cx in start..half {
            let mut c = self.cpu_cell(cx, y);
            c.set_width(new_width);
            self.set_cpu_cell(cx, y, c);
        }
        for cx in half..limit {
            self.set_cpu_cell(cx, y, CpuCell::new());
            let mut g = self.gpu_cell(cx, y);
            g.sprite_idx = 0;
            self.set_gpu_cell(cx, y, g);
        }
        self.cursor.x = self
            .cursor
            .x
            .saturating_sub(cell.scale() * cell.width() / 2);
        self.lb_mut().mark_line_dirty(y as usize);
        self.is_dirty = true;
        true
    }

    fn draw_combining_char(&mut self, ch: CharType, st: &mut TextLoopState) {
        let mut xpos = st.prev.x;
        while xpos > 0 {
            let c = self.cpu_cell(xpos, st.prev.y);
            if c.is_multicell() && c.x() != 0 {
                xpos -= 1;
            } else {
                break;
            }
        }
        if !self.add_combining_char(ch, xpos, st.prev.y) {
            return;
        }
        let chars: Vec<u32> = {
            let cell = self.cpu_cell(xpos, st.prev.y);
            if cell.ch_is_idx() {
                self.text_cache.borrow().chars_at(cell.ch()).to_vec()
            } else {
                vec![cell.ch()]
            }
        };
        if chars.len() < 2 {
            return;
        }
        let base_pos = chars.len() - 2;
        let cell = self.cpu_cell(xpos, st.prev.y);
        if ch == VS16 {
            if chars[base_pos + 1] == VS16
                && !cell.is_multicell()
                && char_props_for(chars[base_pos]).is_emoji_presentation_base()
            {
                self.widen_cell_to_two(xpos, st.prev.y);
            }
        } else if ch == VS15 {
            if chars[base_pos + 1] == VS15
                && cell.is_multicell()
                && cell.natural_width()
                && cell.width() == 2
                && char_props_for(chars[base_pos]).is_emoji_presentation_base()
            {
                self.halve_multicell_width(xpos, st.prev.y);
                self.init_segmentation_state(st);
            }
        } else if st.seg.grapheme_break() == GraphemeBreakProperty::GBP_SpacingMark
            && !cell.is_multicell()
            && single_char_width(char_props_for(ch)) > 0
        {
            self.widen_cell_to_two(xpos, st.prev.y);
        }
    }

    fn draw_control_char(&mut self, ch: u32, st: &mut TextLoopState) {
        match ch {
            0x07 => self.bell_count += 1,
            0x08 => {
                let before = self.cursor.y;
                self.backspace();
                if before == self.cursor.y {
                    self.init_segmentation_state(st);
                } else {
                    self.init_text_loop_line(st);
                }
            }
            0x09 => {
                if self.cursor.x >= self.columns {
                    if self.modes.contains(Modes::DECAWM) {
                        self.continue_to_next_line();
                        self.init_text_loop_line(st);
                    } else if self.columns > 0 {
                        self.cursor.x = self.columns - 1;
                        let c = self.cpu_cell(self.cursor.x, self.cursor.y);
                        if c.is_multicell() {
                            if c.y() != 0 {
                                // Multiline cells are outside this port.
                            } else {
                                self.nuke_multicell_char_at(self.cursor.x, self.cursor.y, true);
                            }
                        }
                        self.tab();
                    }
                } else {
                    self.tab();
                }
                self.init_segmentation_state(st);
            }
            0x0e => self.charset.select(1),
            0x0f => self.charset.select(0),
            0x0a..=0x0c => {
                self.linefeed();
                self.init_text_loop_line(st);
            }
            0x0d => {
                self.carriage_return();
                self.init_segmentation_state(st);
            }
            _ => {}
        }
    }

    /// Insert `num` cells at `at`, shifting the rest right (`insert_characters()`
    /// in `screen.c`). The caller is responsible for clearing the inserted
    /// cells when required.
    fn insert_cells_internal(&mut self, at: u32, num: u32, y: u32, replace_with_spaces: bool) {
        let n = num.min(self.columns - at);
        if n == 0 {
            return;
        }
        // Split/remove multicell chars at the left boundary.
        let left = self.cpu_cell(at, y);
        if left.is_multicell() && left.x() != 0 {
            self.nuke_multicell_char_at(at, y, replace_with_spaces);
        }
        let columns = self.columns as usize;
        let at_usize = at as usize;
        let n_usize = n as usize;
        {
            let lb = self.lb_mut();
            let cp = lb.cpu_cells_for_line_mut(y as usize);
            for i in (at_usize + n_usize..columns).rev() {
                cp[i] = cp[i - n_usize];
            }
            let gp = lb.gpu_cells_for_line_mut(y as usize);
            for i in (at_usize + n_usize..columns).rev() {
                gp[i] = gp[i - n_usize];
            }
        }
        // Incomplete wide chars copied across the new right boundary are
        // removed.
        let limit = self.columns.min(at + n);
        let mut x = at;
        while x < limit {
            let c = self.cpu_cell(x, y);
            if c.is_multicell() {
                let cell_limit = x + c.width() - c.x();
                if c.x() != 0 || cell_limit > limit {
                    for cx in x..self.columns.min(cell_limit) {
                        let mut cc = self.cpu_cell(cx, y);
                        cc.set_char(if replace_with_spaces { b' ' as u32 } else { 0 });
                        cc.set_is_multicell(false);
                        self.set_cpu_cell(cx, y, cc);
                    }
                }
                x = cell_limit.max(x + 1);
            } else {
                x += 1;
            }
        }
    }

    /// Remove `num` cells at `at`, shifting the rest left (`remove_characters()`
    /// in `screen.c`).
    fn remove_cells_internal(&mut self, at: u32, num: u32, y: u32, replace_with_spaces: bool) {
        let n = num.min(self.columns - at);
        if n == 0 {
            return;
        }
        let left = self.cpu_cell(at, y);
        if left.is_multicell() && left.x() != 0 {
            self.nuke_multicell_char_at(at, y, replace_with_spaces);
        }
        let columns = self.columns as usize;
        let at_usize = at as usize;
        let n_usize = n as usize;
        {
            let lb = self.lb_mut();
            let cp = lb.cpu_cells_for_line_mut(y as usize);
            for i in at_usize..columns - n_usize {
                cp[i] = cp[i + n_usize];
            }
            let gp = lb.gpu_cells_for_line_mut(y as usize);
            for i in at_usize..columns - n_usize {
                gp[i] = gp[i + n_usize];
            }
        }
        let mut x = at;
        while x < self.columns {
            let c = self.cpu_cell(x, y);
            if c.is_multicell() {
                let cell_limit = x + c.width() - c.x();
                if c.x() != 0 || cell_limit > self.columns - n {
                    for cx in x..self.columns.min(cell_limit) {
                        let mut cc = self.cpu_cell(cx, y);
                        cc.set_char(if replace_with_spaces { b' ' as u32 } else { 0 });
                        cc.set_is_multicell(false);
                        self.set_cpu_cell(cx, y, cc);
                    }
                }
                x = cell_limit.max(x + 1);
            } else {
                x += 1;
            }
        }
    }

    /// Draw raw codepoints. Charset translation is applied here, matching
    /// `screen_draw_text()`.
    pub fn draw_text(&mut self, chars: &[CharType]) {
        self.is_dirty = true;
        if !chars.is_empty() {
            self.selections.clear();
        }
        let gpu_template = self.cursor.sgr.as_gpu_cell();
        let mut cpu_template = CpuCell::new();
        cpu_template.set_hyperlink_id(self.active_hyperlink_id);
        let mut st = TextLoopState::default();
        self.init_text_loop_line(&mut st);

        let mut i = 0usize;
        while i < chars.len() {
            let ch = self.map_char(chars[i]);
            let char_width: u32 = if ch < DEL
                && st.seg.grapheme_break() as u8 <= GraphemeBreakProperty::GBP_None as u8
            {
                if ch < b' ' as u32 {
                    self.draw_control_char(ch, &mut st);
                    i += 1;
                    continue;
                }
                if self.charset.current().is_none()
                    && !self.modes.contains(Modes::IRM)
                    && self.cursor.x < self.columns
                {
                    let x0 = self.cursor.x;
                    let y0 = self.cursor.y;
                    let limit = (chars.len() - i).min((self.columns - x0) as usize);
                    let mut n = printable_ascii_run_length(&chars[i..i + limit]);
                    let start = x0 as usize;
                    if n > 0 {
                        // Fuse the multicell check with the cell writes: stop at
                        // the first multicell cell, which needs the scalar path.
                        let mut written = 0usize;
                        let cp = self.lb_mut().cpu_cells_for_line_mut(y0 as usize);
                        for (dst, &raw) in cp[start..start + n].iter_mut().zip(&chars[i..i + n]) {
                            if dst.is_multicell() {
                                break;
                            }
                            let mut c = cpu_template;
                            c.set_char(raw);
                            *dst = c;
                            written += 1;
                        }
                        n = written;
                    }
                    if n > 0 {
                        {
                            let gp = self.lb_mut().gpu_cells_for_line_mut(y0 as usize);
                            gp[start..start + n].fill(gpu_template);
                        }
                        self.last_graphic_char = chars[i + n - 1];
                        let x = x0 + n as u32;
                        self.cursor.x = x;
                        st.prev.x = x - 1;
                        st.prev.y = y0;
                        st.prev.valid = true;
                        st.seg = none_seg();
                        i += n;
                        continue;
                    }
                }
                st.seg = none_seg();
                1
            } else {
                let cp: CharProps = char_props_for(ch);
                if cp.is_invalid() {
                    if ch < b' ' as u32 {
                        self.draw_control_char(ch, &mut st);
                    }
                    i += 1;
                    continue;
                }
                st.seg = grapheme_segmentation_step(st.seg, cp);
                if st.seg.add_to_current_cell() && st.prev.valid {
                    self.draw_combining_char(ch, &mut st);
                    i += 1;
                    continue;
                }
                let mut w = single_char_width(cp);
                if w < 1 {
                    if w == 0 {
                        if st.prev.valid {
                            self.draw_combining_char(ch, &mut st);
                        }
                        i += 1;
                        continue;
                    }
                    w = 1;
                }
                w as u32
            };

            // Remove/relocate a multicell cell under the cursor. A width-2
            // multicell starting here will be fully overwritten by the wide
            // character below, so clearing it first is redundant.
            let mut skip_wide_pair_clear = false;
            if self.cursor.x < self.columns {
                let c = self.cpu_cell(self.cursor.x, self.cursor.y);
                if c.is_multicell() {
                    skip_wide_pair_clear = char_width == 2 && c.x() == 0 && c.mcd_x_limit() == 2;
                    if !skip_wide_pair_clear {
                        self.nuke_multicell_char_at(self.cursor.x, self.cursor.y, c.x() != 0);
                    }
                }
            }

            self.last_graphic_char = ch;
            if self.columns < self.cursor.x + char_width {
                if char_width > self.columns {
                    i += 1;
                    continue;
                }
                if self.modes.contains(Modes::DECAWM) {
                    self.continue_to_next_line();
                    self.init_text_loop_line(&mut st);
                } else {
                    self.cursor.x = self.columns - char_width;
                }
                if !skip_wide_pair_clear {
                    let c = self.cpu_cell(self.cursor.x, self.cursor.y);
                    if c.is_multicell() {
                        self.nuke_multicell_char_at(self.cursor.x, self.cursor.y, c.x() > 0);
                    }
                }
            }

            if self.modes.contains(Modes::IRM) {
                self.insert_cells_internal(self.cursor.x, char_width, self.cursor.y, true);
            }

            let x = self.cursor.x;
            let y = self.cursor.y;
            if char_width == 2 {
                if x + 1 < self.columns && !skip_wide_pair_clear {
                    let second = self.cpu_cell(x + 1, y);
                    if second.is_multicell() {
                        self.nuke_multicell_char_at(x + 1, y, true);
                    }
                }
                self.write_wide_char(ch, x, y, cpu_template, gpu_template);
                st.prev.x = x;
                st.prev.y = y;
                st.prev.valid = true;
                self.cursor.x += 2;
            } else {
                self.write_narrow_char(ch, x, y, cpu_template, gpu_template);
                st.prev.x = x;
                st.prev.y = y;
                st.prev.valid = true;
                self.cursor.x += 1;
            }
            i += 1;
        }
    }

    fn write_narrow_char(
        &mut self,
        ch: CharType,
        x: u32,
        y: u32,
        cpu_template: CpuCell,
        gpu_template: GpuCell,
    ) {
        let mut c = cpu_template;
        c.set_char(ch);
        c.set_ch_is_idx(false);
        c.set_is_multicell(false);
        c.set_width(0);
        c.set_x(0);
        c.set_y(0);
        self.set_cpu_cell(x, y, c);
        self.set_gpu_cell(x, y, gpu_template);
    }

    fn write_wide_char(
        &mut self,
        ch: CharType,
        x: u32,
        y: u32,
        cpu_template: CpuCell,
        gpu_template: GpuCell,
    ) {
        let mut c = cpu_template;
        c.set_char(ch);
        c.set_ch_is_idx(false);
        c.set_is_multicell(true);
        c.set_width(2);
        c.set_scale_field(1);
        c.set_natural_width(true);
        c.set_x(0);
        c.set_y(0);
        let mut second = c;
        second.set_x(1);
        let start = x as usize;
        {
            let cpu = self.lb_mut().cpu_cells_for_line_mut(y as usize);
            cpu[start] = c;
            if start + 1 < cpu.len() {
                cpu[start + 1] = second;
            }
        }
        {
            let gpu = self.lb_mut().gpu_cells_for_line_mut(y as usize);
            gpu[start] = gpu_template;
            if start + 1 < gpu.len() {
                gpu[start + 1] = gpu_template;
            }
        }
    }

    /// `screen_align()` (DECALN): fill the screen with `E`, preserving
    /// existing cell attributes as the C code does.
    pub fn align(&mut self) {
        self.cursor_position(1, 1);
        self.margin_top = 0;
        self.margin_bottom = self.lines - 1;
        let mut c = CpuCell::new();
        c.set_char('E' as u32);
        for y in 0..self.lines {
            self.lb_mut().cpu_cells_for_line_mut(y as usize).fill(c);
            self.lb_mut().mark_line_dirty(y as usize);
        }
        self.is_dirty = true;
    }

    // }}}

    // {{{ SGR

    /// `select_graphic_rendition()`: update the cursor's display attributes.
    pub fn select_graphic_rendition(&mut self, params: &[i32], is_group: bool) {
        self.cursor.from_sgr(params, is_group);
    }

    /// Apply SGR parameters to a range of GPU cells.
    pub fn apply_sgr_to_cells(&self, cells: &mut [GpuCell], params: &[i32], is_group: bool) {
        apply_sgr_to_cells(cells, params, is_group);
    }

    // }}}

    // {{{ Editing

    /// `screen_erase_in_line()`. `how` is 0 (to end), 1 (to start) or 2 (all).
    pub fn erase_in_line(&mut self, how: u32, private: bool) -> bool {
        let (s, n) = match how {
            0 => (self.cursor.x, self.columns - self.cursor.x),
            1 => (0, self.cursor.x + 1),
            2 => (0, self.columns),
            _ => return false,
        };
        if n == 0 {
            return true;
        }
        let y = self.cursor.y;
        // Remove wide chars intersecting the erased range.
        let mut x = s;
        while x < s + n {
            let c = self.cpu_cell(x, y);
            if c.is_multicell() {
                self.nuke_multicell_char_at(x, y, false);
            }
            x += 1;
        }
        let start = s as usize;
        let end = (s + n) as usize;
        self.lb_mut().cpu_cells_for_line_mut(y as usize)[start..end].fill(CpuCell::new());
        if !private {
            let template = self.cursor.sgr.as_gpu_cell();
            self.lb_mut().gpu_cells_for_line_mut(y as usize)[start..end].fill(template);
        }
        self.lb_mut().mark_line_dirty(y as usize);
        self.is_dirty = true;
        true
    }

    fn clear_line_private(&mut self, y: u32) {
        let y = y as usize;
        self.lb_mut().cpu_cells_for_line_mut(y).fill(CpuCell::new());
        self.lb_mut().set_last_char_as_continuation(y, false);
        self.lb_mut().clear_attrs_and_dirty(y);
    }

    fn clear_line_with_cursor(&mut self, y: u32) {
        let y = y as usize;
        let template = self.cursor.sgr.as_gpu_cell();
        self.lb_mut().cpu_cells_for_line_mut(y).fill(CpuCell::new());
        self.lb_mut().gpu_cells_for_line_mut(y).fill(template);
        self.lb_mut().clear_attrs_and_dirty(y);
    }

    /// `screen_erase_in_display()`.
    ///
    /// `how` is 0 (to end), 1 (to start), 2 (all) or 3 (all plus scrollback).
    /// Returns `false` for unsupported values such as `22`.
    pub fn erase_in_display(&mut self, how: u32, private: bool) -> bool {
        let (a, b, clear_scrollback) = match how {
            0 => (self.cursor.y + 1, self.lines, false),
            1 => (0, self.cursor.y, false),
            2 => (0, self.lines, false),
            3 => (0, self.lines, true),
            _ => return false,
        };
        if b > a {
            for y in a..b {
                if private {
                    self.clear_line_private(y);
                } else {
                    self.clear_line_with_cursor(y);
                }
            }
            self.is_dirty = true;
        }
        if how < 2 {
            self.erase_in_line(how, private);
            if how == 1 {
                let y = self.cursor.y as usize;
                self.lb_mut().clear_attrs_and_dirty(y);
            }
        }
        if clear_scrollback && !self.on_alt {
            self.historybuf.clear();
            self.scrolled_by = 0;
            self.history_line_added_count = 0;
        }
        true
    }

    /// `screen_insert_lines()` (IL).
    pub fn insert_lines(&mut self, count: u32) {
        let top = self.margin_top;
        let bottom = self.margin_bottom;
        let count = count.max(1);
        if top <= self.cursor.y && self.cursor.y <= bottom {
            // Remove split multicell chars at the insertion edge.
            let mut x = 0;
            while x < self.columns {
                let c = self.cpu_cell(x, self.cursor.y);
                if c.is_multicell() && c.y() != 0 {
                    self.nuke_multicell_char_at(x, self.cursor.y, false);
                }
                x += 1;
            }
            let y = self.cursor.y as usize;
            self.lb_mut().insert_lines(count, y, bottom as usize);
            self.is_dirty = true;
            self.carriage_return();
        }
    }

    /// `screen_delete_lines()` (DL).
    pub fn delete_lines(&mut self, count: u32) {
        let top = self.margin_top;
        let bottom = self.margin_bottom;
        let count = count.max(1);
        if top <= self.cursor.y && self.cursor.y <= bottom {
            let start = self.cursor.y;
            let mut x = 0;
            while x < self.columns {
                if self.cpu_cell(x, start).is_multicell() {
                    self.nuke_multicell_char_at(x, start, false);
                }
                x += 1;
            }
            let y = (start + count).min(bottom);
            let mut x = 0;
            while x < self.columns {
                if self.cpu_cell(x, y).is_multicell() {
                    self.nuke_multicell_char_at(x, y, false);
                }
                x += 1;
            }
            self.lb_mut()
                .delete_lines(count, start as usize, bottom as usize);
            self.is_dirty = true;
            self.carriage_return();
        }
    }

    /// `screen_insert_characters()` (ICH).
    pub fn insert_characters(&mut self, count: u32) {
        let count = count.max(1);
        if self.cursor.y >= self.lines {
            return;
        }
        let x = self.cursor.x;
        let num = count.min(self.columns - x);
        self.insert_cells_internal(x, num, self.cursor.y, false);
        let y = self.cursor.y as usize;
        if let Some(mut line) = self.line(self.cursor.y) {
            line_apply_cursor(&mut line, &self.cursor, x as usize, num as usize, true);
            self.lb_mut().copy_line_to(&line, y);
        }
        self.lb_mut().mark_line_dirty(y);
        self.is_dirty = true;
    }

    /// `screen_repeat_character()` (REP).
    pub fn repeat_character(&mut self, count: u32) {
        if self.last_graphic_char == 0 {
            return;
        }
        let count = count.clamp(1, CSI_REP_MAX_REPETITIONS);
        let buf = [self.last_graphic_char; 64];
        let mut done = 0;
        while done < count {
            let n = (count - done).min(64) as usize;
            self.draw_text(&buf[..n]);
            done += n as u32;
        }
    }

    /// `screen_delete_characters()` (DCH).
    pub fn delete_characters(&mut self, count: u32) {
        let count = count.max(1);
        if self.cursor.y >= self.lines {
            return;
        }
        let x = self.cursor.x;
        let y = self.cursor.y;
        let num = count.min(self.columns - x);
        self.remove_cells_internal(x, num, y, false);
        if let Some(mut line) = self.line(y) {
            line_apply_cursor(
                &mut line,
                &self.cursor,
                (self.columns - num) as usize,
                num as usize,
                true,
            );
            self.lb_mut().copy_line_to(&line, y as usize);
        }
        self.lb_mut().mark_line_dirty(y as usize);
        self.is_dirty = true;
    }

    /// `screen_erase_characters()` (ECH).
    pub fn erase_characters(&mut self, count: u32) {
        let count = count.max(1);
        let x = self.cursor.x;
        let num = count.min(self.columns - x);
        let y = self.cursor.y;
        let mut cx = x;
        while cx < x + num {
            if self.cpu_cell(cx, y).is_multicell() {
                self.nuke_multicell_char_at(cx, y, false);
            }
            cx += 1;
        }
        if let Some(mut line) = self.line(y) {
            line_apply_cursor(&mut line, &self.cursor, x as usize, num as usize, true);
            self.lb_mut().copy_line_to(&line, y as usize);
        }
        self.lb_mut().mark_line_dirty(y as usize);
        self.is_dirty = true;
    }

    // }}}

    // {{{ Hyperlinks

    /// `set_active_hyperlink()`: OSC 8 start/end.
    pub fn set_active_hyperlink(&mut self, id: Option<&str>, url: Option<&str>) {
        self.active_hyperlink_id = if url.map(|u| !u.is_empty()).unwrap_or(false) {
            self.hyperlinks.get_id(id, url)
        } else {
            0
        };
    }

    /// The hyperlink id stored in the cell at visual coordinates.
    pub fn hyperlink_id_at(&self, x: u32, y: u32) -> u16 {
        if x >= self.columns || y >= self.lines {
            return 0;
        }
        let row = y as i64 - self.scrolled_by as i64;
        range_line(self, row)
            .map(|line| {
                line.cpu_cells
                    .get(x as usize)
                    .map(|c| c.hyperlink_id())
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    }

    fn spans_for_row(&self, row: i64, wanted: Option<u16>) -> Vec<HyperlinkSpan> {
        let Some(line) = range_line(self, row) else {
            return Vec::new();
        };
        let mut ans = Vec::new();
        let mut start: Option<(u16, u32)> = None;
        for x in 0..line.xnum() as u32 {
            let id = line.cpu_cells[x as usize].hyperlink_id();
            let matches = id != 0 && wanted.map(|w| w == id).unwrap_or(true);
            match (start, matches) {
                (Some((_sid, _sx)), true) if _sid == id => {}
                (Some((_sid, sx)), _) => {
                    ans.push(HyperlinkSpan {
                        y: row,
                        x_start: sx,
                        x_end: x,
                    });
                    start = if matches { Some((id, x)) } else { None };
                }
                (None, true) => start = Some((id, x)),
                (None, false) => {}
            }
        }
        if let Some((_, sx)) = start {
            ans.push(HyperlinkSpan {
                y: row,
                x_start: sx,
                x_end: line.xnum() as u32,
            });
        }
        ans
    }

    /// Contiguous hyperlink runs on one visual screen row.
    pub fn url_spans(&self, y: u32) -> Vec<HyperlinkSpan> {
        if y >= self.lines {
            return Vec::new();
        }
        let row = y as i64 - self.scrolled_by as i64;
        self.spans_for_row(row, None)
    }

    /// `hyperlink_at()`: the link under a cell, including links that span
    /// nearby soft-wrapped rows (kitty searches five rows in each direction).
    pub fn hyperlink_at(&self, x: u32, y: u32) -> Option<HyperlinkAt> {
        if x >= self.columns || y >= self.lines {
            return None;
        }
        let row = y as i64 - self.scrolled_by as i64;
        let id = self.hyperlink_id_at(x, y);
        if id == 0 {
            return None;
        }
        let mut spans = Vec::new();
        for r in
            (row - 5).max(-(self.historybuf.count as i64))..=(row + 5).min(self.lines as i64 - 1)
        {
            spans.extend(self.spans_for_row(r, Some(id)));
        }
        let url = self
            .hyperlinks
            .url_for_id(id)
            .unwrap_or_default()
            .to_string();
        Some(HyperlinkAt { id, url, spans })
    }

    /// `hyperlinks_as_set()`: `(key, id)` pairs still in the pool.
    pub fn hyperlinks_as_set(&self) -> HashSet<(String, u16)> {
        self.hyperlinks
            .iter()
            .map(|(id, link)| (link.key(), id))
            .collect()
    }

    /// Drop links that are no longer referenced by any cell.
    pub fn garbage_collect_hyperlink_pool(&mut self) {
        let mut used: HashSet<u16> = HashSet::new();
        for lnum in 0..self.historybuf.count {
            for c in self.historybuf.cpu_cells(lnum) {
                let id = c.hyperlink_id();
                if id != 0 {
                    used.insert(id);
                }
            }
        }
        for lb in [&self.main_linebuf, &self.alt_linebuf] {
            for y in 0..lb.ynum as usize {
                for c in lb.cpu_cells_for_line(y) {
                    let id = c.hyperlink_id();
                    if id != 0 {
                        used.insert(id);
                    }
                }
            }
        }
        let map = self.hyperlinks.garbage_collect(&used);
        let remap = |id: u16| -> u16 { map.get(id as usize).copied().unwrap_or(0) };
        for lnum in 0..self.historybuf.count {
            for c in self.historybuf.cpu_cells_mut(lnum) {
                let id = c.hyperlink_id();
                if id != 0 {
                    c.set_hyperlink_id(remap(id));
                }
            }
        }
        for lb in [&mut self.main_linebuf, &mut self.alt_linebuf] {
            for y in 0..lb.ynum as usize {
                for c in lb.cpu_cells_for_line_mut(y) {
                    let id = c.hyperlink_id();
                    if id != 0 {
                        c.set_hyperlink_id(remap(id));
                    }
                }
            }
        }
        self.active_hyperlink_id = remap(self.active_hyperlink_id);
    }

    // }}}

    // {{{ DSR helpers used by the handler

    /// The cursor position as 1-based `(row, column)`.
    pub fn cursor_position_1_based(&self) -> (u32, u32) {
        (self.cursor.y + 1, self.cursor.x + 1)
    }

    /// The current margins as 1-based `(top, bottom)`.
    pub fn margins_1_based(&self) -> (u32, u32) {
        (self.margin_top + 1, self.margin_bottom + 1)
    }

    // }}}
}

pub(crate) fn default_tabstops(count: u32) -> Vec<bool> {
    (0..count).map(|i| i % 8 == 0).collect()
}

/// `empty_modes` from `screen.c`: auto-wrap, cursor visible and auto-repeat.
fn default_modes() -> Modes {
    let mut m = Modes::default();
    m.insert(Modes::DECAWM);
    m.insert(Modes::DECTCEM);
    m.insert(Modes::DECARM);
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(columns: u32, lines: u32) -> Screen {
        Screen::builder(columns, lines)
            .scrollback(8)
            .build()
            .unwrap()
    }

    fn draw(s: &mut Screen, text: &str) {
        let chars: Vec<u32> = text.chars().map(|c| c as u32).collect();
        s.draw_text(&chars);
    }

    #[test]
    fn draw_wrap_and_continue_flags() {
        let mut s = screen(5, 5);
        draw(&mut s, &"a".repeat(5));
        assert_eq!(s.line_text(0), "aaaaa");
        draw(&mut s, &"b".repeat(7));
        assert!(s.line_is_continued(1));
        assert!(s.line_is_continued(2));
        assert_eq!(s.line_text(1), "bbbbb");
        assert_eq!(s.line_text(2), "bb");
        assert_eq!((s.cursor.x, s.cursor.y), (2, 2));
    }

    #[test]
    fn draw_without_wrap_overwrites_last_column() {
        let mut s = screen(5, 5);
        s.reset_mode(modes::DECAWM);
        draw(&mut s, "0123456789");
        assert_eq!(s.line_text(0), "01239");
        assert_eq!((s.cursor.x, s.cursor.y), (5, 0));
        draw(&mut s, "ab");
        assert_eq!(s.line_text(0), "0123b");
    }

    #[test]
    fn insert_mode_draws_by_inserting() {
        let mut s = screen(5, 5);
        draw(&mut s, "12345");
        s.cursor_position(1, 1);
        s.set_mode(modes::IRM);
        draw(&mut s, "abc");
        assert_eq!(s.line_text(0), "abc12");
        assert_eq!((s.cursor.x, s.cursor.y), (3, 0));
    }

    #[test]
    fn wide_and_combining_chars() {
        let mut s = screen(5, 5);
        draw(&mut s, "ココx");
        assert_eq!(s.line_text(0), "ココx");
        assert_eq!(s.cpu_cell(0, 0).width(), 2);
        assert_eq!(s.cpu_cell(1, 0).x(), 1);
        draw(&mut s, "ニチハ");
        assert_eq!(s.line_text(1), "ニチ");
        assert_eq!(s.line_text(2), "ハ");
        draw(&mut s, "Ƶ\u{327}\u{308}");
        assert_eq!(s.line_text(2), "ハƵ\u{327}\u{308}");
        assert_eq!(s.cursor.x, 3);
    }

    #[test]
    fn wide_overwrite_clears_trailer() {
        let mut s = screen(5, 5);
        draw(&mut s, "ニチ");
        s.cursor_position(1, 1);
        draw(&mut s, "ab");
        assert_eq!(s.line_text(0), "abチ");
        let mut s = screen(5, 5);
        draw(&mut s, "ニチ");
        s.cursor_position(1, 2);
        draw(&mut s, "a");
        assert_eq!(s.line_text(0), " aチ");
    }

    #[test]
    fn cursor_on_wide_trailer() {
        let mut s = screen(5, 5);
        draw(&mut s, "😸");
        assert_eq!(s.cursor.x, 2);
        s.cursor.x = 1;
        draw(&mut s, "a");
        assert_eq!(s.line_text(0), " a");
        assert_eq!(s.cursor.x, 2);
    }

    #[test]
    fn variation_selectors_resize_cells() {
        let mut s = screen(8, 2);
        draw(&mut s, "\u{25b6}");
        assert_eq!(s.cursor.x, 1);
        draw(&mut s, "\u{fe0f}");
        assert_eq!(s.cursor.x, 2);
        assert_eq!(s.cpu_cell(0, 0).width(), 2);
        // A later VS15 does not narrow a cell that was widened by VS16,
        // matching the C variation-selector tests.
        draw(&mut s, "\u{fe0e}");
        assert_eq!(s.cursor.x, 2);
        // A naturally wide emoji is narrowed by VS15.
        let mut s = screen(8, 2);
        draw(&mut s, "\u{1f610}");
        assert_eq!(s.cursor.x, 2);
        draw(&mut s, "\u{fe0e}");
        assert_eq!(s.cursor.x, 1);
        assert_eq!(s.cpu_cell(0, 0).width(), 1);
    }

    #[test]
    fn spacing_mark_widens_narrow_cell() {
        let mut s = screen(5, 5);
        draw(&mut s, "จำ");
        assert_eq!(s.cursor.x, 2);
        assert_eq!(s.cpu_cell(0, 0).width(), 2);
        assert_eq!(s.cpu_cell(1, 0).x(), 1);
    }

    #[test]
    fn tabs_and_backtabs() {
        let mut s = screen(32, 2);
        draw(&mut s, "a\tb");
        assert_eq!(s.cursor.x, 9);
        assert_eq!(s.line_ansi(0), "a\tb");
        assert_eq!(s.line_text(0), "a\tb");
        s.cursor_position(1, 1);
        s.tab();
        assert_eq!(s.cursor.x, 8);
        s.backtab(1);
        assert_eq!(s.cursor.x, 0);
    }

    #[test]
    fn tab_stop_manipulation() {
        let mut s = screen(20, 2);
        s.clear_tab_stop(3);
        s.reset_tab_stops();
        s.cursor_position(1, 1);
        s.tab();
        assert_eq!(s.cursor.x, 8);
        s.tab();
        assert_eq!(s.cursor.x, 16);
        s.clear_tab_stop(3);
        s.cursor_position(1, 1);
        s.tab();
        assert_eq!(s.cursor.x, 19);
    }

    #[test]
    fn erase_in_line_modes() {
        let mut s = screen(5, 5);
        draw(&mut s, "abcde");
        s.cursor.x = 1;
        s.cursor.sgr.bold = true;
        s.erase_in_line(0, false);
        assert_eq!(s.line_text(0), "a");
        assert!(!s.cpu_cell(1, 0).has_text());
        s.reset();
        draw(&mut s, "abcde");
        s.cursor.x = 1;
        s.erase_in_line(1, false);
        assert_eq!(s.line_text(0), "  cde");
        s.reset();
        draw(&mut s, "abcde");
        s.cursor.x = 1;
        s.erase_in_line(2, true);
        assert_eq!(s.line_text(0), "");
    }

    #[test]
    fn erase_in_display_modes() {
        let mut s = screen(5, 5);
        draw(&mut s, &"12345".repeat(5));
        s.cursor.x = 2;
        s.cursor.y = 1;
        s.erase_in_display(0, false);
        assert_eq!(s.line_text(1), "12");
        assert_eq!(s.line_text(2), "");
        s.reset();
        draw(&mut s, &"12345".repeat(5));
        s.cursor.x = 2;
        s.cursor.y = 1;
        s.erase_in_display(1, false);
        assert_eq!(s.line_text(0), "");
        assert_eq!(s.line_text(1), "   45");
        s.reset();
        draw(&mut s, &"12345".repeat(5));
        s.erase_in_display(2, false);
        assert_eq!(s.line_text(0), "");
        assert!(!s.cpu_cell(1, 0).has_text());
    }

    #[test]
    fn insert_and_delete_characters() {
        let mut s = screen(5, 5);
        draw(&mut s, "abcde");
        s.cursor.x = 1;
        s.insert_characters(2);
        assert_eq!(s.line_text(0), "a  bc");
        s.reset();
        draw(&mut s, "abcde");
        s.cursor.x = 1;
        s.delete_characters(2);
        assert_eq!(s.line_text(0), "ade");
        s.reset();
        draw(&mut s, "abcde");
        s.cursor.x = 1;
        s.erase_characters(2);
        assert_eq!(s.line_text(0), "a  de");
    }

    #[test]
    fn insert_and_delete_lines() {
        let mut s = screen(5, 5);
        for i in 0..5 {
            draw(&mut s, &i.to_string());
            if i + 1 < 5 {
                s.linefeed();
                s.carriage_return();
            }
        }
        s.cursor.y = 1;
        s.cursor.x = 0;
        s.insert_lines(1);
        assert_eq!(s.line_text(1), "");
        s.delete_lines(1);
        assert_eq!(s.line_text(1), "1");
    }

    #[test]
    fn margins_scroll_to_history() {
        let mut s = Screen::builder(80, 6).scrollback(4).build().unwrap();
        s.set_margins(0, 5);
        for i in 0..8 {
            draw(&mut s, &i.to_string());
            s.linefeed();
            s.carriage_return();
        }
        let rows: Vec<String> = (0..6).map(|y| s.line_text(y)).collect();
        assert_eq!(rows, vec!["4", "5", "6", "7", "", ""]);
        let hist: Vec<String> = (0..s.historybuf().count)
            .map(|i| s.history_line_text(i))
            .collect();
        assert_eq!(hist, vec!["3", "2", "1", "0"]);
    }

    #[test]
    fn top_margin_does_not_add_history() {
        let mut s = Screen::builder(80, 6).scrollback(4).build().unwrap();
        s.set_margins(2, 6);
        for i in 0..8 {
            draw(&mut s, &i.to_string());
            s.linefeed();
            s.carriage_return();
        }
        assert_eq!(s.historybuf().count, 0);
        let rows: Vec<String> = (0..6).map(|y| s.line_text(y)).collect();
        assert_eq!(rows, vec!["0", "4", "5", "6", "7", ""]);
    }

    #[test]
    fn save_restore_cursor_and_charset() {
        let mut s = screen(10, 5);
        s.cursor_position(3, 4);
        s.cursor.sgr.bold = true;
        s.charset.designate(0, b'0');
        s.save_cursor();
        s.cursor_position(1, 1);
        s.cursor.sgr.bold = false;
        s.charset.reset();
        s.restore_cursor();
        assert_eq!((s.cursor.y, s.cursor.x), (2, 3));
        assert!(s.cursor.sgr.bold);
        assert_eq!(s.charset.map_char('q' as u32), 0x2500);
    }

    #[test]
    fn alternate_screen_round_trip() {
        let mut s = screen(5, 5);
        draw(&mut s, "main");
        assert!(s.set_mode(modes::ALTERNATE_SCREEN));
        assert!(s.is_using_alternate_linebuf());
        assert_eq!(s.line_text(0), "");
        draw(&mut s, "alt");
        assert!(s.reset_mode(modes::ALTERNATE_SCREEN));
        assert!(s.is_main_linebuf());
        assert_eq!(s.line_text(0), "main");
    }

    #[test]
    fn alt_screen_47_does_not_clear() {
        let mut s = screen(5, 5);
        s.set_mode(modes::TOGGLE_ALT_SCREEN_2);
        draw(&mut s, "x");
        s.reset_mode(modes::TOGGLE_ALT_SCREEN_2);
        s.set_mode(modes::TOGGLE_ALT_SCREEN_2);
        assert_eq!(s.line_text(0), "x");
    }

    #[test]
    fn cursor_visibility_is_global() {
        let mut s = screen(5, 5);
        s.set_mode(modes::ALTERNATE_SCREEN);
        s.reset_mode(modes::DECTCEM);
        s.reset_mode(modes::ALTERNATE_SCREEN);
        assert!(!s.is_cursor_visible());
    }

    #[test]
    fn repeat_character() {
        let mut s = screen(8, 5);
        draw(&mut s, "a");
        s.repeat_character(4);
        assert_eq!(s.line_text(0), "aaaaa");
        s.reset();
        draw(&mut s, " ");
        s.repeat_character(3);
        assert_eq!(s.line_text(0), "    ");
    }

    #[test]
    fn huge_scroll_count_does_not_loop() {
        let mut s = screen(5, 5);
        for i in 0..5 {
            draw(&mut s, &i.to_string());
            if i + 1 < 5 {
                s.carriage_return();
                s.linefeed();
            }
        }
        s.scroll(u32::MAX);
        for y in 0..5 {
            assert_eq!(s.line_text(y).trim(), "");
        }
    }

    #[test]
    fn soft_reset_preserves_content_and_cursor() {
        let mut s = screen(5, 5);
        draw(&mut s, "hello");
        s.cursor_position(3, 4);
        s.set_mode(modes::IRM);
        s.cursor.sgr.bold = true;
        s.soft_reset();
        assert_eq!(s.line_text(0), "hello");
        assert_eq!((s.cursor.y, s.cursor.x), (2, 3));
        assert!(!s.cursor.sgr.bold);
        assert!(!s.modes.contains(Modes::IRM));
    }

    #[test]
    fn soft_reset_stays_on_alternate_screen() {
        let mut s = screen(5, 5);
        s.set_mode(modes::ALTERNATE_SCREEN);
        assert!(s.is_using_alternate_linebuf());
        s.soft_reset();
        assert!(s.is_using_alternate_linebuf());
        s.reset();
        assert!(s.is_main_linebuf());
    }

    #[test]
    fn ansi_serialization_matches_c() {
        let mut s = screen(5, 5);
        s.select_graphic_rendition(&[0, 1, 5, 37, 42], false);
        draw(&mut s, "a");
        assert_eq!(s.line_ansi(0), "\x1b[22;1;5;37;42ma");
        s.cursor_position(2, 1);
        s.select_graphic_rendition(&[0, 35], false);
        draw(&mut s, "b");
        assert_eq!(s.line_text(0), "a");
        assert_eq!(s.cpu_cell(0, 1).ch(), 'b' as u32);
        s.cursor_position(2, 2);
        s.select_graphic_rendition(&[38, 2, 99, 1, 2, 3], false);
        draw(&mut s, "c");
        let cell = s.gpu_cell(1, 1);
        assert_eq!(cell.fg, (1 << 24) | (2 << 16) | (3 << 8) | 2);
    }

    #[test]
    fn charset_graphics_drawing() {
        let mut s = screen(5, 5);
        s.charset.designate(0, b'0');
        draw(&mut s, "qqq");
        assert_eq!(s.line_text(0), "───");
        s.charset.designate(0, b'B');
        draw(&mut s, "q");
        assert_eq!(s.line_text(0), "───q");
    }

    #[test]
    fn rep_caps_at_max_and_works_after_space() {
        let mut s = screen(8, 5);
        draw(&mut s, "x");
        s.repeat_character(7);
        assert_eq!(s.line_text(0), "xxxxxxxx");
        s.cursor_position(2, 1);
        draw(&mut s, "yz");
        s.repeat_character(1);
        assert_eq!(s.line_text(0), "xxxxxxxx");
        assert_eq!(s.line_text(1), "yzz");
    }

    #[test]
    fn erase_display_to_start_keeps_attrs() {
        let mut s = screen(5, 5);
        draw(&mut s, &"12345".repeat(5));
        s.cursor.x = 2;
        s.cursor.y = 1;
        s.cursor.sgr.bold = true;
        s.erase_in_display(1, false);
        assert_eq!(s.line_text(1), "   45");
        assert!(s.gpu_cell(0, 0).attrs.bold());
    }

    #[test]
    fn history_reverse_scroll_fill() {
        let mut s = Screen::builder(5, 3).scrollback(8).build().unwrap();
        for i in 0..6 {
            draw(&mut s, &i.to_string());
            s.carriage_return();
            s.linefeed();
        }
        let before = s.historybuf().count;
        assert!(before > 0);
        s.reverse_scroll_and_fill_from_scrollback(1);
        assert_eq!(s.historybuf().count, before - 1);
    }

    #[test]
    fn cursor_movement_matches_c() {
        let mut s = screen(5, 5);
        draw(&mut s, &"12345".repeat(5));
        s.cursor_up(2);
        assert_eq!((s.cursor.x, s.cursor.y), (4, 2));
        s.cursor_up1(1);
        assert_eq!((s.cursor.x, s.cursor.y), (0, 1));
        s.cursor_forward(3);
        assert_eq!((s.cursor.x, s.cursor.y), (3, 1));
        s.cursor_move(1, -1, false);
        assert_eq!((s.cursor.x, s.cursor.y), (2, 1));
        s.cursor_down(1);
        assert_eq!((s.cursor.x, s.cursor.y), (2, 2));
        s.cursor_down1(5);
        assert_eq!((s.cursor.x, s.cursor.y), (0, 4));
    }

    #[test]
    fn backspace_wide_character() {
        let mut s = screen(5, 5);
        draw(&mut s, "⛅");
        assert_eq!(s.cursor.x, 2);
        s.backspace();
        draw(&mut s, " ");
        s.backspace();
        assert_eq!(s.cursor.x, 1);
    }

    #[test]
    fn batched_ascii_crosses_wraps() {
        let mut s = screen(5, 5);
        draw(&mut s, "abcdefghijkl");
        assert_eq!(s.line_text(0), "abcde");
        assert_eq!(s.line_text(1), "fghij");
        assert_eq!(s.line_text(2), "kl");
        assert!(s.line_is_continued(1));
        assert!(s.line_is_continued(2));
        assert_eq!((s.cursor.x, s.cursor.y), (2, 2));
    }

    #[test]
    fn top_and_bottom_margin_scroll_region() {
        let mut s = Screen::builder(80, 6).scrollback(4).build().unwrap();
        s.set_margins(2, 5);
        for i in 0..8 {
            draw(&mut s, &i.to_string());
            s.linefeed();
            s.carriage_return();
        }
        let rows: Vec<String> = (0..6).map(|y| s.line_text(y)).collect();
        assert_eq!(rows, vec!["0", "5", "6", "7", "", ""]);
        assert_eq!(s.historybuf().count, 0);
    }

    #[test]
    fn reverse_index_scrolls_region() {
        let mut s = screen(5, 5);
        draw(&mut s, &"12345".repeat(5));
        s.cursor_up(5);
        s.reverse_index();
        assert_eq!(s.line_text(0), "");
        for y in 1..5 {
            assert_eq!(s.line_text(y), "12345");
        }
    }

    #[test]
    fn index_moves_or_scrolls() {
        let mut s = screen(5, 5);
        draw(&mut s, &"12345".repeat(5));
        s.index();
        assert_eq!(s.line_text(4), "");
        for y in 0..4 {
            assert_eq!(s.line_text(y), "12345");
        }
    }

    #[test]
    fn huge_repetition_count_is_capped() {
        let mut s = screen(8, 5);
        draw(&mut s, "x");
        s.repeat_character(u32::MAX);
        // 65535 repetitions wrap many times; the point is that it terminates.
        assert!(s.is_dirty);
    }

    #[test]
    fn unsupported_mode_is_rejected() {
        let mut s = screen(5, 5);
        assert!(!s.set_mode(modes::MOUSE_BUTTON_TRACKING));
        assert!(!s.set_mode(modes::PENDING_UPDATE));
        assert!(s.set_mode(modes::BRACKETED_PASTE));
    }

    #[test]
    fn alignment_display_preserves_attrs() {
        let mut s = screen(3, 2);
        s.set_margins(1, 2);
        s.cursor_position(2, 2);
        s.align();
        assert_eq!(s.line_text(0), "EEE");
        assert_eq!(s.line_text(1), "EEE");
        assert_eq!(s.margins_1_based(), (1, 2));
        assert_eq!((s.cursor.x, s.cursor.y), (0, 0));
    }

    #[test]
    fn builder_matches_constructor() {
        let s = Screen::builder(10, 4).scrollback(2).build().unwrap();
        assert_eq!(s.columns, 10);
        assert_eq!(s.lines, 4);
        assert_eq!(s.historybuf().ynum, 4);
        let s2 = Screen::new(4, 10, 2).unwrap();
        assert_eq!(s2.columns, 10);
        assert_eq!(s2.lines, 4);
    }
}
