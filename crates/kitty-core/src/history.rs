// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/history.c, Copyright (C) 2016-2024 Kovid Goyal <kovid at kovidgoyal.net>

//! Scrollback history ring buffer.
//!
//! The C implementation stores the ring in fixed 2048-line `mmap` segments and
//! hands out stable pointers into them. Rust does not need stable addresses
//! here, so this port uses one flat owned allocation per cell array
//! (`ynum * xnum` entries) and indexes it with the same wrap arithmetic. This
//! is the only representation difference: `start_of_data`, `count`, reverse
//! line numbering (0 is newest), and the push/overwrite/wrap semantics are kept
//! exactly as in C.
//!
//! Out of scope for this port: pager history (`PagerHistoryBuf`/`pagerhist_*`),
//! rewrap/resize, ANSI serialization and Python bindings. See the `TODO(port)`
//! at the bottom of this file.

use std::cell::RefMut;

use crate::cell::{CpuCell, GpuCell, LineAttrs};
use crate::line::Line;
use crate::linebuf::{IndexType, SharedTextCache};
use crate::text_cache::TextCache;

/// Errors returned by [`HistoryBuf`] construction and line access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryBufError {
    /// `xnum` or `ynum` was zero.
    Empty,
    /// A line was requested from a buffer with `count == 0`.
    BufferIsEmpty,
    /// The requested line number is `>= count`.
    OutOfBounds,
}

impl core::fmt::Display for HistoryBufError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => f.write_str("Cannot create an empty history buffer"),
            Self::BufferIsEmpty => f.write_str("This buffer is empty"),
            Self::OutOfBounds => f.write_str("Out of bounds"),
        }
    }
}

impl std::error::Error for HistoryBufError {}

/// A flat, reverse-indexed scrollback ring.
///
/// Line number 0 is the most recently added line. `start_of_data` is the
/// buffer position of the oldest line and `count` the number of live lines.
#[derive(Debug)]
pub struct HistoryBuf {
    pub xnum: IndexType,
    pub ynum: IndexType,
    pub start_of_data: IndexType,
    pub count: IndexType,
    cpu_cells: Vec<CpuCell>,
    gpu_cells: Vec<GpuCell>,
    line_attrs: Vec<LineAttrs>,
    text_cache: SharedTextCache,
}

impl HistoryBuf {
    /// `create_historybuf()`: `xnum` is the width, `ynum` the ring capacity.
    pub fn new(
        xnum: IndexType,
        ynum: IndexType,
        tc: SharedTextCache,
    ) -> Result<Self, HistoryBufError> {
        if xnum == 0 || ynum == 0 {
            return Err(HistoryBufError::Empty);
        }
        let area = xnum as usize * ynum as usize;
        Ok(Self {
            xnum,
            ynum,
            start_of_data: 0,
            count: 0,
            cpu_cells: vec![CpuCell::new(); area],
            gpu_cells: vec![GpuCell::default(); area],
            line_attrs: vec![LineAttrs::default(); ynum as usize],
            text_cache: tc,
        })
    }

    #[inline]
    pub fn text_cache(&self) -> &SharedTextCache {
        &self.text_cache
    }

    #[inline]
    pub fn text_cache_mut(&self) -> RefMut<'_, TextCache> {
        self.text_cache.borrow_mut()
    }

    /// Buffer position of line `lnum` (reverse indexing, 0 is newest).
    ///
    /// Mirrors C's clamping: for a non-empty buffer values larger than
    /// `count - 1` map to the oldest line, and an empty buffer maps to 0.
    #[inline]
    pub fn index_of(&self, lnum: IndexType) -> IndexType {
        if self.count == 0 {
            return 0;
        }
        let idx = self.count - 1 - (self.count - 1).min(lnum);
        ((self.start_of_data as usize + idx as usize) % self.ynum as usize) as IndexType
    }

    #[inline]
    fn row_start(&self, y: IndexType) -> usize {
        y as usize * self.xnum as usize
    }

    #[inline]
    fn init_line_at(&self, num: IndexType, line: &mut Line) {
        let xnum = self.xnum as usize;
        if line.cpu_cells.len() != xnum {
            line.cpu_cells.resize(xnum, CpuCell::new());
        }
        if line.gpu_cells.len() != xnum {
            line.gpu_cells.resize(xnum, GpuCell::default());
        }
        let start = self.row_start(num);
        line.cpu_cells
            .copy_from_slice(&self.cpu_cells[start..start + xnum]);
        line.gpu_cells
            .copy_from_slice(&self.gpu_cells[start..start + xnum]);
        line.attrs = self.line_attrs[num as usize];
    }

    /// `historybuf_init_line()`: copy line `lnum` into `line`.
    #[inline]
    pub fn init_line(&self, lnum: IndexType, line: &mut Line) {
        self.init_line_at(self.index_of(lnum), line);
    }

    /// `historybuf_cpu_cells()`: borrowed CPU cells for line `lnum`.
    #[inline]
    pub fn cpu_cells(&self, lnum: IndexType) -> &[CpuCell] {
        let start = self.row_start(self.index_of(lnum));
        &self.cpu_cells[start..start + self.xnum as usize]
    }

    /// Mutable CPU cells for line `lnum`.
    #[inline]
    pub fn cpu_cells_mut(&mut self, lnum: IndexType) -> &mut [CpuCell] {
        let start = self.row_start(self.index_of(lnum));
        &mut self.cpu_cells[start..start + self.xnum as usize]
    }

    /// Return an owned copy of line `lnum`.
    ///
    /// C returns a borrowed `Line`; ownership avoids exposing raw pointers.
    pub fn line(&self, lnum: IndexType) -> Result<Line, HistoryBufError> {
        if self.count == 0 {
            return Err(HistoryBufError::BufferIsEmpty);
        }
        if lnum >= self.count {
            return Err(HistoryBufError::OutOfBounds);
        }
        let mut line = Line::new(self.xnum as usize);
        self.init_line_at(self.index_of(lnum), &mut line);
        Ok(line)
    }

    /// `hb_line_is_continued()`, operating on a buffer position.
    #[inline]
    fn hb_line_is_continued(&self, num: IndexType) -> bool {
        // The C version also consults pager history here; pager history is out
        // of scope, so the first line in the ring is never continued.
        if num == self.start_of_data {
            return false;
        }
        let prev = ((num as usize + self.ynum as usize - 1) % self.ynum as usize) as IndexType;
        let x = self.xnum as usize - 1;
        self.cpu_cells[self.row_start(prev) + x].next_char_was_wrapped()
    }

    /// `historybuf_is_line_continued()`.
    #[inline]
    pub fn is_line_continued(&self, lnum: IndexType) -> bool {
        self.hb_line_is_continued(self.index_of(lnum))
    }

    /// `history_buf_endswith_wrap()`.
    #[inline]
    pub fn endswith_wrap(&self) -> bool {
        let idx = self.index_of(0);
        let x = self.xnum as usize - 1;
        self.cpu_cells[self.row_start(idx) + x].next_char_was_wrapped()
    }

    /// `historybuf_mark_line_clean()`.
    #[inline]
    pub fn mark_line_clean(&mut self, y: IndexType) {
        let idx = self.index_of(y) as usize;
        self.line_attrs[idx].set_has_dirty_text(false);
    }

    /// `historybuf_mark_line_dirty()`.
    #[inline]
    pub fn mark_line_dirty(&mut self, y: IndexType) {
        let idx = self.index_of(y) as usize;
        self.line_attrs[idx].set_has_dirty_text(true);
    }

    /// `historybuf_set_line_has_image_placeholders()`.
    #[inline]
    pub fn set_line_has_image_placeholders(&mut self, y: IndexType, val: bool) {
        let idx = self.index_of(y) as usize;
        self.line_attrs[idx].set_has_image_placeholders(val);
    }

    /// `historybuf_clear()`.
    ///
    /// C frees and re-creates its zeroed mmap segments. The flat representation
    /// simply zeroes the backing arrays, which is observationally identical.
    pub fn clear(&mut self) {
        self.count = 0;
        self.start_of_data = 0;
        self.cpu_cells.fill(CpuCell::new());
        self.gpu_cells.fill(GpuCell::default());
        self.line_attrs.fill(LineAttrs::default());
    }

    /// `historybuf_push()`: advance the ring and return `(slot, needs_clear)`.
    #[inline]
    pub fn push(&mut self) -> (IndexType, bool) {
        let ynum = self.ynum as usize;
        let idx = ((self.start_of_data as usize + self.count as usize) % ynum) as IndexType;
        if self.count == self.ynum {
            self.start_of_data = ((self.start_of_data as usize + 1) % ynum) as IndexType;
            (idx, true)
        } else {
            self.count += 1;
            (idx, false)
        }
    }

    /// `historybuf_add_line()`: copy `line` into the next history slot.
    pub fn add_line(&mut self, line: &Line) {
        let (idx, _needs_clear) = self.push();
        let start = self.row_start(idx);
        let n = line
            .cpu_cells
            .len()
            .min(line.gpu_cells.len())
            .min(self.xnum as usize);
        self.cpu_cells[start..start + n].copy_from_slice(&line.cpu_cells[..n]);
        self.gpu_cells[start..start + n].copy_from_slice(&line.gpu_cells[..n]);
        self.line_attrs[idx as usize] = line.attrs;
    }

    /// `historybuf_pop_line()`: copy the newest line into `line` and drop it.
    #[inline]
    pub fn pop_line(&mut self, line: &mut Line) -> bool {
        if self.count == 0 {
            return false;
        }
        let idx = ((self.start_of_data as usize + self.count as usize - 1) % self.ynum as usize)
            as IndexType;
        self.init_line_at(idx, line);
        self.count -= 1;
        true
    }

    /// `historybuf_delete_newest_lines()`: drop up to `count` lines, clearing
    /// multicell continuations that would otherwise overlap the kept lines.
    pub fn delete_newest_lines(&mut self, count: IndexType) {
        if count == 0 || self.count == 0 {
            return;
        }
        let ynum = self.ynum as usize;
        let xnum = self.xnum as usize;
        let to_delete = self.count.min(count);
        self.count -= to_delete - 1;
        let idx = ((self.start_of_data as usize + self.count as usize - 1) % ynum) as IndexType;
        self.count -= 1;

        // C walks the cells of the first deleted line, capturing its CPU array
        // before re-pointing its scratch Line inside the loop.
        let base = self.row_start(idx);
        for x in 0..xnum {
            let c = self.cpu_cells[base + x];
            if c.is_multicell() && c.y() != 0 {
                let mut i = 0u32;
                let mut pos = self.count;
                while i < c.y() && pos != 0 {
                    let prev_idx =
                        ((self.start_of_data as usize + pos as usize - 1) % ynum) as IndexType;
                    let m = self.row_start(prev_idx) + x;
                    let cell = &mut self.cpu_cells[m];
                    cell.set_char(0);
                    cell.set_ch_is_idx(false);
                    cell.set_is_multicell(false);
                    self.gpu_cells[m].sprite_idx = 0;
                    i += 1;
                    pos -= 1;
                }
            }
        }
    }

    /// `history_buf_set_last_char_as_continuation()`.
    #[inline]
    pub fn set_last_char_as_continuation(&mut self, y: IndexType, wrapped: bool) {
        if self.count > 0 {
            let pos = self.row_start(self.index_of(y)) + self.xnum as usize - 1;
            self.cpu_cells[pos].set_next_char_was_wrapped(wrapped);
        }
    }

    /// `historybuf_next_dest_line()`.
    ///
    /// C points `dest_line` directly into the ring; the Rust version zeroes the
    /// freshly advanced slot and fills the caller's owned `Line` with that
    /// zeroed content. Returns `dest_y + 1` as in C.
    pub fn next_dest_line(
        &mut self,
        src_line: &Line,
        dest_y: IndexType,
        dest_line: &mut Line,
        continued: bool,
    ) -> IndexType {
        self.set_last_char_as_continuation(0, continued);
        let (idx, _needs_clear) = self.push();
        self.line_attrs[idx as usize] = src_line.attrs;

        let xnum = self.xnum as usize;
        let start = self.row_start(idx);
        self.cpu_cells[start..start + xnum].fill(CpuCell::new());
        self.gpu_cells[start..start + xnum].fill(GpuCell::default());

        if dest_line.cpu_cells.len() != xnum {
            dest_line.cpu_cells.resize(xnum, CpuCell::new());
        }
        if dest_line.gpu_cells.len() != xnum {
            dest_line.gpu_cells.resize(xnum, GpuCell::default());
        }
        dest_line.cpu_cells.fill(CpuCell::new());
        dest_line.gpu_cells.fill(GpuCell::default());
        dest_line.attrs = self.line_attrs[idx as usize];
        dest_y + 1
    }
}

// TODO(port): pager history (PagerHistoryBuf, pagerhist_*), rewrap/resize
// (historybuf_alloc_for_rewrap, historybuf_finish_rewrap,
// historybuf_fast_rewrap, historybuf_next_dest_line's ANSI-buffer side),
// ANSI serialization (as_ansi, as_text, __str__/dirty_lines) and the Python
// type bindings are intentionally not implemented here.

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn buf(xnum: u32, ynum: u32) -> HistoryBuf {
        HistoryBuf::new(xnum, ynum, Rc::new(RefCell::new(TextCache::new()))).unwrap()
    }

    fn line_with(xnum: u32, ch: char) -> Line {
        let mut line = Line::new(xnum as usize);
        line.cpu_cells[0].set_char(ch as u32);
        line
    }

    #[test]
    fn validates_dimensions() {
        let tc = || Rc::new(RefCell::new(TextCache::new()));
        assert_eq!(
            HistoryBuf::new(0, 5, tc()).unwrap_err(),
            HistoryBufError::Empty
        );
        assert_eq!(
            HistoryBuf::new(5, 0, tc()).unwrap_err(),
            HistoryBufError::Empty
        );
        assert!(HistoryBuf::new(1, 1, tc()).is_ok());
    }

    #[test]
    fn empty_buffer_access() {
        let mut b = buf(3, 4);
        assert_eq!(b.count, 0);
        assert_eq!(b.start_of_data, 0);
        assert_eq!(b.index_of(0), 0);
        assert_eq!(b.index_of(99), 0);
        assert_eq!(b.cpu_cells(0).len(), 3);
        assert!(b.cpu_cells(0).iter().all(|c| !c.has_text()));
        assert!(!b.endswith_wrap());
        assert!(!b.is_line_continued(0));
        assert_eq!(b.line(0).unwrap_err(), HistoryBufError::BufferIsEmpty);
        assert_eq!(b.line(1).unwrap_err(), HistoryBufError::BufferIsEmpty);

        let mut line = Line::new(3);
        assert!(!b.pop_line(&mut line));
        b.delete_newest_lines(5);
        assert_eq!(b.count, 0);
        assert_eq!(b.start_of_data, 0);
    }

    #[test]
    fn push_wrap_semantics() {
        let mut b = buf(2, 4);

        // count < ynum: index advances, count grows, start stays put.
        for expected in 0..4u32 {
            let (idx, needs_clear) = b.push();
            assert_eq!(idx, expected);
            assert!(!needs_clear);
            assert_eq!(b.count, expected + 1);
            assert_eq!(b.start_of_data, 0);
        }

        // count == ynum: start advances and the overwritten slot is returned.
        for (idx, start) in [(0u32, 1u32), (1, 2), (2, 3), (3, 0)] {
            let (got_idx, needs_clear) = b.push();
            assert_eq!((got_idx, needs_clear), (idx, true));
            assert_eq!(b.count, 4);
            assert_eq!(b.start_of_data, start);
        }
    }

    #[test]
    fn add_line_order_and_pop() {
        let mut b = buf(4, 3);
        let mut a = line_with(4, 'a');
        a.attrs.set_has_dirty_text(true);
        b.add_line(&a);
        b.add_line(&line_with(4, 'b'));
        b.add_line(&line_with(4, 'c'));

        assert_eq!(b.count, 3);
        assert_eq!(b.start_of_data, 0);
        assert_eq!(b.line(0).unwrap().cpu_cells[0].ch(), 'c' as u32);
        assert_eq!(b.line(1).unwrap().cpu_cells[0].ch(), 'b' as u32);
        let oldest = b.line(2).unwrap();
        assert_eq!(oldest.cpu_cells[0].ch(), 'a' as u32);
        assert!(oldest.attrs.has_dirty_text());
        assert_eq!(b.line(3).unwrap_err(), HistoryBufError::OutOfBounds);

        // A fourth line overwrites the oldest slot.
        b.add_line(&line_with(4, 'd'));
        assert_eq!(b.count, 3);
        assert_eq!(b.start_of_data, 1);
        assert_eq!(b.line(0).unwrap().cpu_cells[0].ch(), 'd' as u32);
        assert_eq!(b.line(1).unwrap().cpu_cells[0].ch(), 'c' as u32);
        assert_eq!(b.line(2).unwrap().cpu_cells[0].ch(), 'b' as u32);

        let mut line = Line::new(4);
        assert!(b.pop_line(&mut line));
        assert_eq!(line.cpu_cells[0].ch(), 'd' as u32);
        assert_eq!(b.count, 2);
        assert!(b.pop_line(&mut line));
        assert_eq!(line.cpu_cells[0].ch(), 'c' as u32);
        assert!(b.pop_line(&mut line));
        assert_eq!(line.cpu_cells[0].ch(), 'b' as u32);
        assert!(!b.pop_line(&mut line));
        assert_eq!(b.count, 0);
    }

    #[test]
    fn delete_newest_lines_simple() {
        let mut b = buf(2, 5);
        for ch in ['a', 'b', 'c', 'd', 'e'] {
            b.add_line(&line_with(2, ch));
        }
        assert_eq!(b.count, 5);

        b.delete_newest_lines(2);
        assert_eq!(b.count, 3);
        assert_eq!(b.line(0).unwrap().cpu_cells[0].ch(), 'c' as u32);
        assert_eq!(b.line(1).unwrap().cpu_cells[0].ch(), 'b' as u32);
        assert_eq!(b.line(2).unwrap().cpu_cells[0].ch(), 'a' as u32);

        b.delete_newest_lines(99);
        assert_eq!(b.count, 0);
        assert_eq!(b.line(0).unwrap_err(), HistoryBufError::BufferIsEmpty);
    }

    #[test]
    fn delete_newest_lines_clears_multicell_overlap() {
        let mut b = buf(4, 5);
        let a = line_with(4, 'a');
        let bb = line_with(4, 'b');
        let mut c = line_with(4, 'c');
        let mut d = line_with(4, 'd');
        let mut e = line_with(4, 'e');

        // The two kept lines that the deleted multicell cell overlaps.
        c.cpu_cells[0].set_ch_is_idx(true);
        c.gpu_cells[0].sprite_idx = 7;
        d.cpu_cells[0].set_ch_is_idx(true);
        d.gpu_cells[0].sprite_idx = 8;

        // First deleted line: a multicell continuation two rows down.
        e.cpu_cells[0].set_is_multicell(true);
        e.cpu_cells[0].set_y(2);
        e.gpu_cells[0].sprite_idx = 9;

        for line in [&a, &bb, &c, &d, &e] {
            b.add_line(line);
        }
        // Force the ring to wrap so start_of_data is non-zero.
        b.add_line(&line_with(4, 'f'));
        assert_eq!(b.start_of_data, 1);

        b.delete_newest_lines(2);
        assert_eq!(b.count, 3);
        assert_eq!(b.start_of_data, 1);

        // Remaining lines are d (newest), c, b; d and c had their multicell
        // continuations cleared, b is untouched.
        for lnum in [0u32, 1] {
            let line = b.line(lnum).unwrap();
            assert_eq!(line.cpu_cells[0].ch(), 0, "line {lnum} char");
            assert!(!line.cpu_cells[0].ch_is_idx(), "line {lnum} index flag");
            assert!(!line.cpu_cells[0].is_multicell(), "line {lnum} multicell");
            assert_eq!(line.gpu_cells[0].sprite_idx, 0, "line {lnum} sprite");
        }

        let oldest = b.line(2).unwrap();
        assert_eq!(oldest.cpu_cells[0].ch(), 'b' as u32);
    }

    #[test]
    fn attrs_follow_reverse_index() {
        let mut b = buf(2, 3);
        for ch in ['a', 'b', 'c'] {
            b.add_line(&line_with(2, ch));
        }

        b.mark_line_dirty(1);
        assert!(b.line_attrs[b.index_of(1) as usize].has_dirty_text());
        b.mark_line_clean(1);
        assert!(!b.line_attrs[b.index_of(1) as usize].has_dirty_text());

        b.set_line_has_image_placeholders(1, true);
        assert!(b.line_attrs[b.index_of(1) as usize].has_image_placeholders());
        b.set_line_has_image_placeholders(1, false);
        assert!(!b.line_attrs[b.index_of(1) as usize].has_image_placeholders());
    }

    #[test]
    fn continuation_flags() {
        let mut b = buf(4, 4);
        b.set_last_char_as_continuation(0, true);
        assert!(!b.endswith_wrap());

        for ch in ['a', 'b', 'c'] {
            b.add_line(&line_with(4, ch));
        }
        assert!(!b.endswith_wrap());

        b.set_last_char_as_continuation(0, true);
        assert!(b.endswith_wrap());
        assert!(!b.is_line_continued(0));
        assert!(!b.is_line_continued(2));

        // The oldest line in the ring can never be continued.
        b.set_last_char_as_continuation(2, true);
        assert!(b.is_line_continued(1));
        assert!(!b.is_line_continued(2));

        b.set_last_char_as_continuation(2, false);
        assert!(!b.is_line_continued(1));
    }

    #[test]
    fn next_dest_line_zeroes_slot_and_marks_continuation() {
        let mut b = buf(3, 4);
        b.add_line(&line_with(3, 'x'));
        b.add_line(&line_with(3, 'y'));

        let mut src = Line::new(3);
        src.attrs.set_prompt_kind(2);

        let mut dest = Line::new(3);
        dest.cpu_cells[0].set_char('z' as u32);
        dest.gpu_cells[0].sprite_idx = 5;

        assert_eq!(b.next_dest_line(&src, 7, &mut dest, true), 8);
        assert_eq!(b.count, 3);

        // The previous newest line was marked as continued.
        assert!(b.is_line_continued(0));

        // The destination slot is zeroed and carries the source attrs.
        assert_eq!(dest.cpu_cells[0].ch(), 0);
        assert_eq!(dest.gpu_cells[0].sprite_idx, 0);
        assert_eq!(dest.attrs.prompt_kind(), 2);
        let newest = b.line(0).unwrap();
        assert!(newest.cpu_cells.iter().all(|c| !c.has_text()));
        assert_eq!(newest.attrs.prompt_kind(), 2);
    }

    #[test]
    fn clear_resets_everything() {
        let mut b = buf(3, 2);
        let mut a = line_with(3, 'a');
        a.attrs.set_has_dirty_text(true);
        a.attrs.set_has_image_placeholders(true);
        b.add_line(&a);
        b.add_line(&line_with(3, 'b'));
        b.add_line(&line_with(3, 'c'));

        assert_eq!(b.start_of_data, 1);
        b.set_last_char_as_continuation(0, true);

        b.clear();
        assert_eq!(b.count, 0);
        assert_eq!(b.start_of_data, 0);
        assert!(!b.endswith_wrap());
        assert!(b.cpu_cells(0).iter().all(|c| !c.has_text()));
        for attrs in &b.line_attrs {
            assert_eq!(*attrs, LineAttrs::default());
        }

        // The ring starts from slot zero again after clear.
        assert_eq!(b.push(), (0, false));
        assert_eq!(b.count, 1);
    }
}
