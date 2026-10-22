// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/line-buf.c, Copyright (C) 2016-2024 Kovid Goyal <kovid at kovidgoyal.net>

//! The screen line buffer. Rows are stable slots of cells; `line_map` is an
//! indirection that allows scrolling by rotating indices instead of moving
//! memory, exactly like the C implementation.

use std::cell::RefCell;
use std::rc::Rc;

use crate::cell::{CellAttrs, CharType, CpuCell, GpuCell, LineAttrs, BLANK_CHAR};
use crate::line::{set_named_attribute_on_line, CellAttribute, Line, ListOfChars};
use crate::text_cache::TextCache;

pub type IndexType = u32;

pub const MAX_LINES: u32 = 50_000;
pub const MAX_COLUMNS: u32 = 5_000;

pub type SharedTextCache = Rc<RefCell<TextCache>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineBufError {
    TooLarge,
    Empty,
    OutOfBounds,
    TextCacheMismatch,
}

impl core::fmt::Display for LineBufError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooLarge => f.write_str("Number of rows or columns is too large."),
            Self::Empty => f.write_str("Cannot create an empty LineBuf"),
            Self::OutOfBounds => f.write_str("Out of bounds"),
            Self::TextCacheMismatch => f.write_str("Text caches do not match"),
        }
    }
}

impl std::error::Error for LineBufError {}

#[derive(Debug)]
pub struct LineBuf {
    pub xnum: IndexType,
    pub ynum: IndexType,
    cpu_cell_buf: Vec<CpuCell>,
    gpu_cell_buf: Vec<GpuCell>,
    line_map: Vec<IndexType>,
    scratch: Vec<IndexType>,
    line_attrs: Vec<LineAttrs>,
    text_cache: SharedTextCache,
}

impl LineBuf {
    /// `alloc_linebuf()`: validated construction. `lines` is the number of
    /// rows, `columns` the width, matching the C argument order.
    pub fn new(lines: u32, columns: u32, tc: SharedTextCache) -> Result<Self, LineBufError> {
        if columns > MAX_COLUMNS || lines > MAX_LINES {
            return Err(LineBufError::TooLarge);
        }
        let area = columns as usize * lines as usize;
        if area == 0 {
            return Err(LineBufError::Empty);
        }
        let mut ans = Self {
            xnum: columns,
            ynum: lines,
            cpu_cell_buf: vec![CpuCell::new(); area],
            gpu_cell_buf: vec![GpuCell::default(); area],
            line_map: (0..lines).collect(),
            scratch: vec![0; lines as usize],
            line_attrs: vec![LineAttrs::default(); lines as usize],
            text_cache: tc,
        };
        if BLANK_CHAR != 0 {
            for y in 0..lines as usize {
                ans.clear_chars_to(y, BLANK_CHAR);
            }
        }
        Ok(ans)
    }

    #[inline]
    pub fn text_cache(&self) -> &SharedTextCache {
        &self.text_cache
    }

    #[inline]
    pub fn text_cache_mut(&self) -> std::cell::RefMut<'_, TextCache> {
        self.text_cache.borrow_mut()
    }

    #[inline]
    fn row_start(&self, y: usize) -> usize {
        y * self.xnum as usize
    }

    #[inline]
    fn mapped_row(&self, y: usize) -> usize {
        self.line_map[y] as usize
    }

    fn clear_chars_to(&mut self, y: usize, ch: CharType) {
        if ch == 0 {
            return;
        }
        let start = self.row_start(y);
        let end = start + self.xnum as usize;
        let c = {
            let mut c = CpuCell::new();
            c.set_char(ch);
            c
        };
        for i in start..end {
            self.cpu_cell_buf[i] = c;
            self.gpu_cell_buf[i].attrs = CellAttrs::default();
        }
    }

    /// `linebuf_clear()`.
    pub fn clear(&mut self, ch: CharType) {
        self.cpu_cell_buf.fill(CpuCell::new());
        self.gpu_cell_buf.fill(GpuCell::default());
        self.line_attrs.fill(LineAttrs::default());
        for (i, v) in self.line_map.iter_mut().enumerate() {
            *v = i as u32;
        }
        if ch != 0 {
            for y in 0..self.ynum as usize {
                self.clear_chars_to(y, ch);
                self.line_attrs[y] = LineAttrs::default();
                self.line_attrs[y].set_has_dirty_text(true);
            }
        }
    }

    #[inline]
    pub fn mark_line_dirty(&mut self, y: usize) {
        self.line_attrs[y].set_has_dirty_text(true);
    }

    #[inline]
    pub fn mark_line_clean(&mut self, y: usize) {
        self.line_attrs[y].set_has_dirty_text(false);
    }

    #[inline]
    pub fn set_line_has_image_placeholders(&mut self, y: usize, val: bool) {
        self.line_attrs[y].set_has_image_placeholders(val);
    }

    #[inline]
    pub fn clear_attrs_and_dirty(&mut self, y: usize) {
        self.line_attrs[y] = LineAttrs::default();
        self.line_attrs[y].set_has_dirty_text(true);
    }

    /// CPU cells of the physical row backing screen row `y`.
    #[inline]
    pub fn cpu_cells_for_line(&self, y: usize) -> &[CpuCell] {
        let start = self.row_start(self.mapped_row(y));
        &self.cpu_cell_buf[start..start + self.xnum as usize]
    }

    #[inline]
    pub fn gpu_cells_for_line(&self, y: usize) -> &[GpuCell] {
        let start = self.row_start(self.mapped_row(y));
        &self.gpu_cell_buf[start..start + self.xnum as usize]
    }

    #[inline]
    pub fn cpu_cells_for_line_mut(&mut self, y: usize) -> &mut [CpuCell] {
        let start = self.row_start(self.mapped_row(y));
        &mut self.cpu_cell_buf[start..start + self.xnum as usize]
    }

    #[inline]
    pub fn gpu_cells_for_line_mut(&mut self, y: usize) -> &mut [GpuCell] {
        let start = self.row_start(self.mapped_row(y));
        &mut self.gpu_cell_buf[start..start + self.xnum as usize]
    }

    #[inline]
    pub fn cpu_cell_at(&self, x: usize, y: usize) -> &CpuCell {
        &self.cpu_cells_for_line(y)[x]
    }

    #[inline]
    pub fn gpu_cell_at(&self, x: usize, y: usize) -> &GpuCell {
        &self.gpu_cells_for_line(y)[x]
    }

    #[inline]
    pub fn line_attrs(&self, y: usize) -> LineAttrs {
        self.line_attrs[y]
    }

    #[inline]
    pub fn line_ends_with_continuation(&self, y: usize) -> bool {
        if y < self.ynum as usize {
            self.cpu_cells_for_line(y)[self.xnum as usize - 1].next_char_was_wrapped()
        } else {
            false
        }
    }

    #[inline]
    pub fn set_last_char_as_continuation(&mut self, y: usize, continued: bool) {
        if y < self.ynum as usize {
            let x = self.xnum as usize - 1;
            self.cpu_cells_for_line_mut(y)[x].set_next_char_was_wrapped(continued);
        }
    }

    #[inline]
    pub fn is_continued(&self, y: usize) -> bool {
        y > 0 && self.line_ends_with_continuation(y - 1)
    }

    /// `set_attribute()`: applies to every row and marks it dirty.
    pub fn set_attribute(&mut self, attr: CellAttribute, val: u16) -> bool {
        let xnum = self.xnum as usize;
        for y in 0..self.ynum as usize {
            let start = self.row_start(y);
            set_named_attribute_on_line(&mut self.gpu_cell_buf[start..start + xnum], attr, val);
            self.line_attrs[y].set_has_dirty_text(true);
        }
        true
    }

    /// `linebuf_clear_line()`.
    pub fn clear_line(&mut self, y: usize, clear_attrs: bool) {
        let start = self.row_start(self.mapped_row(y));
        let end = start + self.xnum as usize;
        self.cpu_cell_buf[start..end].fill(CpuCell::new());
        self.gpu_cell_buf[start..end].fill(GpuCell::default());
        if clear_attrs {
            self.line_attrs[y] = LineAttrs::default();
        }
    }

    /// `linebuf_index()`.
    pub fn index(&mut self, top: usize, bottom: usize) {
        if top + 1 >= self.ynum as usize || bottom >= self.ynum as usize || bottom <= top {
            return;
        }
        let old_top = self.line_map[top];
        let old_attrs = self.line_attrs[top];
        let num = bottom - top;
        self.line_map.copy_within(top + 1..top + 1 + num, top);
        self.line_attrs.copy_within(top + 1..top + 1 + num, top);
        self.line_map[bottom] = old_top;
        self.line_attrs[bottom] = old_attrs;
    }

    /// `linebuf_reverse_index()`.
    pub fn reverse_index(&mut self, top: usize, bottom: usize) {
        if top + 1 >= self.ynum as usize || bottom >= self.ynum as usize || bottom <= top {
            return;
        }
        let old_bottom = self.line_map[bottom];
        let old_attrs = self.line_attrs[bottom];
        for i in (top + 1..=bottom).rev() {
            self.line_map[i] = self.line_map[i - 1];
            self.line_attrs[i] = self.line_attrs[i - 1];
        }
        self.line_map[top] = old_bottom;
        self.line_attrs[top] = old_attrs;
    }

    /// `linebuf_insert_lines()`.
    pub fn insert_lines(&mut self, num: u32, y: usize, bottom: usize) {
        if y >= self.ynum as usize || y > bottom || bottom >= self.ynum as usize {
            return;
        }
        let ylimit = bottom + 1;
        let num = num.min((ylimit - y) as u32) as usize;
        if num < 1 {
            return;
        }
        let scratch_start = ylimit - num;
        self.scratch[..num].copy_from_slice(&self.line_map[scratch_start..ylimit]);
        for i in (y + num..ylimit).rev() {
            self.line_map[i] = self.line_map[i - num];
            self.line_attrs[i] = self.line_attrs[i - num];
        }
        self.line_map[y..y + num].copy_from_slice(&self.scratch[..num]);
        for i in y..y + num {
            self.clear_line(i, true);
        }
    }

    /// `linebuf_delete_lines()`.
    pub fn delete_lines(&mut self, num: u32, y: usize, bottom: usize) {
        let ylimit = bottom + 1;
        let num = (num.min((ylimit - y) as u32)) as usize;
        if y >= self.ynum as usize || y > bottom || bottom >= self.ynum as usize || num < 1 {
            return;
        }
        self.scratch[..num].copy_from_slice(&self.line_map[y..y + num]);
        let mut i = y;
        while i < ylimit && i + num < self.ynum as usize {
            self.line_map[i] = self.line_map[i + num];
            self.line_attrs[i] = self.line_attrs[i + num];
            i += 1;
        }
        let dest = ylimit - num;
        self.line_map[dest..dest + num].copy_from_slice(&self.scratch[..num]);
        for i in dest..ylimit {
            self.clear_line(i, true);
        }
    }

    /// `dirty_lines()`.
    pub fn dirty_lines(&self) -> Vec<usize> {
        self.line_attrs
            .iter()
            .enumerate()
            .filter(|(_, a)| a.has_dirty_text())
            .map(|(i, _)| i)
            .collect()
    }

    /// `create_line_copy()`.
    pub fn create_line_copy(&self, y: usize) -> Result<Line, LineBufError> {
        if y >= self.ynum as usize {
            return Err(LineBufError::OutOfBounds);
        }
        let mut line = Line::new(self.xnum as usize);
        line.cpu_cells.copy_from_slice(self.cpu_cells_for_line(y));
        line.gpu_cells.copy_from_slice(self.gpu_cells_for_line(y));
        line.attrs = self.line_attrs[y];
        Ok(line)
    }

    /// `linebuf_copy_line_to()`.
    pub fn copy_line_to(&mut self, line: &Line, where_: usize) {
        let start = self.row_start(self.mapped_row(where_));
        let n = self.xnum as usize;
        let xn = line.xnum().min(n);
        self.cpu_cell_buf[start..start + xn].copy_from_slice(&line.cpu_cells[..xn]);
        self.gpu_cell_buf[start..start + xn].copy_from_slice(&line.gpu_cells[..xn]);
        self.line_attrs[where_] = line.attrs;
        self.line_attrs[where_].set_has_dirty_text(true);
    }

    /// `copy_old()`: copies bottom aligned content from `other`. Both buffers
    /// must have the same width and share a text cache.
    pub fn copy_old(&mut self, other: &LineBuf) -> Result<(), LineBufError> {
        if other.xnum != self.xnum {
            return Err(LineBufError::TextCacheMismatch);
        }
        if !Rc::ptr_eq(&self.text_cache, &other.text_cache) {
            return Err(LineBufError::TextCacheMismatch);
        }
        let count = (self.ynum as usize).min(other.ynum as usize);
        for i in 0..count {
            let s = self.ynum as usize - 1 - i;
            let o = other.ynum as usize - 1 - i;
            self.line_attrs[s] = other.line_attrs[o];
            let srow = self.row_start(self.mapped_row(s));
            let orow = other.row_start(other.mapped_row(o));
            let n = self.xnum as usize;
            self.cpu_cell_buf[srow..srow + n].copy_from_slice(&other.cpu_cell_buf[orow..orow + n]);
            self.gpu_cell_buf[srow..srow + n].copy_from_slice(&other.gpu_cell_buf[orow..orow + n]);
        }
        Ok(())
    }

    /// Plain text rendering. The full ANSI/SGR renderer lives in `lineops` and
    /// will be ported separately; this is used by the core tests and mirrors
    /// `as_text` for the simple case.
    pub fn as_text(&self) -> String {
        let tc = self.text_cache.borrow();
        let mut ans = String::new();
        let mut lc = ListOfChars::new();
        let mut last_non_blank = 0usize;
        for y in 0..self.ynum as usize {
            let (cpu, gpu) = (self.cpu_cells_for_line(y), self.gpu_cells_for_line(y));
            let mut xlimit = cpu.len();
            while xlimit > 0 && !cpu[xlimit - 1].has_text() {
                xlimit -= 1;
            }
            last_non_blank = xlimit;
            for x in 0..xlimit {
                let c = &cpu[x];
                if c.ch_is_idx() {
                    lc.set(tc.chars_at(c.ch()));
                } else {
                    lc.set(&[c.ch()]);
                }
                for &ch in lc.as_slice() {
                    if let Some(ch) = char::from_u32(ch) {
                        ans.push(ch);
                    }
                }
                let _ = &gpu[x];
            }
            if y + 1 < self.ynum as usize && !self.line_ends_with_continuation(y) {
                ans.push('\n');
            }
        }
        let _ = last_non_blank;
        ans
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::CpuCell;

    fn buf(lines: u32, columns: u32) -> LineBuf {
        LineBuf::new(lines, columns, Rc::new(RefCell::new(TextCache::new()))).unwrap()
    }

    #[test]
    fn validation() {
        let tc = Rc::new(RefCell::new(TextCache::new()));
        assert_eq!(
            LineBuf::new(0, 10, tc.clone()).unwrap_err(),
            LineBufError::Empty
        );
        assert_eq!(
            LineBuf::new(10, 0, tc.clone()).unwrap_err(),
            LineBufError::Empty
        );
        assert_eq!(
            LineBuf::new(10, 5001, tc.clone()).unwrap_err(),
            LineBufError::TooLarge
        );
        assert_eq!(
            LineBuf::new(50_001, 10, tc).unwrap_err(),
            LineBufError::TooLarge
        );
    }

    #[test]
    fn index_rotates_line_map() {
        let mut b = buf(4, 2);
        b.line_map[0] = 0;
        b.line_map[1] = 1;
        b.line_map[2] = 2;
        b.line_map[3] = 3;
        b.line_attrs[0].set_prompt_kind(1);
        b.index(0, 3);
        assert_eq!(b.line_map, vec![1, 2, 3, 0]);
        assert_eq!(b.line_attrs[3].prompt_kind(), 1);
        b.reverse_index(0, 3);
        assert_eq!(b.line_map, vec![0, 1, 2, 3]);
        assert_eq!(b.line_attrs[0].prompt_kind(), 1);
    }

    #[test]
    fn insert_and_delete_lines_keep_indices_valid() {
        let mut b = buf(5, 3);
        b.insert_lines(2, 1, 4);
        assert_eq!(b.line_map, vec![0, 3, 4, 1, 2]);
        b.delete_lines(1, 1, 4);
        assert_eq!(b.line_map, vec![0, 4, 1, 2, 3]);
        assert_eq!(b.line_map.len(), 5);
    }

    #[test]
    fn continuation_flags() {
        let mut b = buf(2, 3);
        assert!(!b.line_ends_with_continuation(0));
        b.set_last_char_as_continuation(0, true);
        assert!(b.line_ends_with_continuation(0));
        assert!(b.is_continued(1));
        assert!(!b.is_continued(0));
    }

    #[test]
    fn clear_and_dirty() {
        let mut b = buf(2, 3);
        b.clear(0);
        assert_eq!(b.dirty_lines(), Vec::<usize>::new());
        b.clear(b' ' as u32);
        assert_eq!(b.dirty_lines(), vec![0, 1]);
        assert!(b.cpu_cell_at(0, 0).is_char(b' ' as u32));
        b.mark_line_clean(0);
        assert_eq!(b.dirty_lines(), vec![1]);
    }

    #[test]
    fn text_uses_line_map_and_continuations() {
        let mut b = buf(3, 4);
        let mut c = CpuCell::new();
        c.set_char('a' as u32);
        b.cpu_cells_for_line_mut(0)[0] = c;
        b.set_last_char_as_continuation(0, true);
        let mut c = CpuCell::new();
        c.set_char('b' as u32);
        b.cpu_cells_for_line_mut(1)[0] = c;
        assert_eq!(b.as_text(), "ab\n");
        b.index(0, 1);
        assert_eq!(b.as_text(), "b\na");
    }
}
