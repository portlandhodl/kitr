// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/line.h, kitty/lineops.h and kitty/line.c
// Copyright (C) 2016-2024 Kovid Goyal <kovid at kovidgoyal.net>

//! A single line of cells, plus the small cell helper functions from lineops.h.

use crate::cell::{CellAttrs, CharType, CpuCell, GpuCell, LineAttrs, MAX_NUM_CODEPOINTS_PER_CELL};
use crate::text_cache::TextCache;

pub type IndexType = u32;

/// Maps one attribute name to the corresponding `CellAttrs` setter.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CellAttribute {
    Reverse,
    Strike,
    Dim,
    Mark,
    Bold,
    Italic,
    Decoration,
}

impl CellAttribute {
    /// `set_named_attribute_on_line()` name lookup.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "reverse" => Self::Reverse,
            "strike" => Self::Strike,
            "dim" => Self::Dim,
            "mark" => Self::Mark,
            "bold" => Self::Bold,
            "italic" => Self::Italic,
            "decoration" => Self::Decoration,
            _ => return None,
        })
    }

    fn apply(self, attrs: &mut CellAttrs, val: u16) {
        match self {
            Self::Reverse => attrs.set_reverse(val != 0),
            Self::Strike => attrs.set_strike(val != 0),
            Self::Dim => attrs.set_dim(val != 0),
            Self::Mark => attrs.set_mark(val as u32),
            Self::Bold => attrs.set_bold(val != 0),
            Self::Italic => attrs.set_italic(val != 0),
            Self::Decoration => attrs.set_decoration(val as u32),
        }
    }
}

/// Stack only replacement for `ListOfChars`. The C code caps cell text at
/// `MAX_NUM_CODEPOINTS_PER_CELL`, so we can hold it inline and avoid a heap
/// allocation on every cell read.
#[derive(Clone)]
pub struct ListOfChars {
    chars: [CharType; MAX_NUM_CODEPOINTS_PER_CELL as usize],
    count: usize,
}

impl Default for ListOfChars {
    fn default() -> Self {
        Self {
            chars: [0; MAX_NUM_CODEPOINTS_PER_CELL as usize],
            count: 0,
        }
    }
}

impl ListOfChars {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn as_slice(&self) -> &[CharType] {
        &self.chars[..self.count]
    }

    #[inline]
    pub fn count(&self) -> usize {
        self.count
    }

    #[inline]
    pub fn set(&mut self, chars: &[CharType]) {
        assert!(chars.len() <= MAX_NUM_CODEPOINTS_PER_CELL as usize);
        self.chars[..chars.len()].copy_from_slice(chars);
        self.count = chars.len();
    }
}

/// A heap owned line. The C `Line` can point into a `LineBuf`; the Rust
/// equivalent for borrowed lines is a slice of cells obtained from the buffer.
#[derive(Clone, Debug, Default)]
pub struct Line {
    pub cpu_cells: Vec<CpuCell>,
    pub gpu_cells: Vec<GpuCell>,
    pub attrs: LineAttrs,
}

impl Line {
    pub fn new(xnum: usize) -> Self {
        Self {
            cpu_cells: vec![CpuCell::new(); xnum],
            gpu_cells: vec![GpuCell::default(); xnum],
            attrs: LineAttrs::default(),
        }
    }

    #[inline]
    pub fn xnum(&self) -> usize {
        self.cpu_cells.len()
    }

    /// `copy_line()`.
    pub fn copy_from(&mut self, src: &Line) {
        let n = self.cpu_cells.len().min(src.cpu_cells.len());
        self.cpu_cells[..n].copy_from_slice(&src.cpu_cells[..n]);
        let n = self.gpu_cells.len().min(src.gpu_cells.len());
        self.gpu_cells[..n].copy_from_slice(&src.gpu_cells[..n]);
    }

    /// `clear_chars_in_line()` with the surrounding storage already zeroed.
    pub fn clear_chars(&mut self, ch: CharType) {
        if ch == 0 {
            return;
        }
        let c = {
            let mut c = CpuCell::new();
            c.set_char(ch);
            c
        };
        for (cpu, gpu) in self.cpu_cells.iter_mut().zip(self.gpu_cells.iter_mut()) {
            *cpu = c;
            gpu.attrs = CellAttrs::default();
        }
    }

    /// `xlimit_for_line()`.
    pub fn xlimit(&self) -> usize {
        let mut xlimit = self.cpu_cells.len();
        while xlimit > 0 && !self.cpu_cells[xlimit - 1].has_text() {
            xlimit -= 1;
        }
        xlimit
    }

    /// `line_is_empty()`.
    pub fn is_empty(&self) -> bool {
        self.cpu_cells.iter().all(|c| !c.has_text())
    }

    /// `num_codepoints_in_cell()`.
    pub fn num_codepoints_in_cell(&self, x: usize, tc: &TextCache) -> u32 {
        let c = &self.cpu_cells[x];
        if c.ch_is_idx() {
            let ans = tc.num_codepoints(c.ch());
            if c.is_multicell() {
                ans.saturating_sub(1)
            } else {
                ans
            }
        } else if c.ch() != 0 {
            1
        } else {
            0
        }
    }

    /// `text_in_cell()`.
    pub fn text_in_cell(&self, x: usize, tc: &TextCache, ans: &mut ListOfChars) {
        let c = &self.cpu_cells[x];
        if c.ch_is_idx() {
            ans.set(tc.chars_at(c.ch()));
        } else {
            ans.set(&[c.ch()]);
        }
    }

    /// `cell_first_char()`.
    pub fn cell_first_char(&self, x: usize, tc: &TextCache) -> CharType {
        let c = &self.cpu_cells[x];
        if c.ch_is_idx() {
            if c.is_multicell() && (c.x() != 0 || c.y() != 0) {
                return 0;
            }
            tc.first_char_at(c.ch())
        } else {
            c.ch()
        }
    }
}

/// `cell_set_chars()`: store inline when a cell holds zero or one codepoint,
/// otherwise intern into the text cache.
#[inline]
pub fn cell_set_chars(c: &mut CpuCell, tc: &mut TextCache, chars: &[CharType]) {
    if chars.len() <= 1 {
        c.set_char(chars.first().copied().unwrap_or(0));
        c.set_ch_is_idx(false);
    } else {
        let idx = tc.get_or_insert(chars);
        c.set_char(idx);
        c.set_ch_is_idx(true);
    }
}

/// `set_named_attribute_on_line()` for GPU cells.
pub fn set_named_attribute_on_line(gpu_cells: &mut [GpuCell], attr: CellAttribute, val: u16) {
    for cell in gpu_cells {
        attr.apply(&mut cell.attrs, val);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_and_multi_codepoint_cells() {
        let mut tc = TextCache::new();
        let mut c = CpuCell::new();
        cell_set_chars(&mut c, &mut tc, &['a' as u32]);
        assert_eq!(c.ch(), 'a' as u32);
        assert!(!c.ch_is_idx());

        cell_set_chars(&mut c, &mut tc, &['a' as u32, 0x301]);
        assert!(c.ch_is_idx());
        let mut lc = ListOfChars::new();
        if c.ch_is_idx() {
            lc.set(tc.chars_at(c.ch()));
        }
        assert_eq!(lc.as_slice(), &['a' as u32, 0x301]);
    }

    #[test]
    fn line_helpers() {
        let mut tc = TextCache::new();
        let mut line = Line::new(5);
        assert!(line.is_empty());
        line.cpu_cells[2].set_char('x' as u32);
        assert!(!line.is_empty());
        assert_eq!(line.xlimit(), 3);
        assert_eq!(line.cell_first_char(2, &tc), 'x' as u32);
        assert_eq!(line.num_codepoints_in_cell(2, &tc), 1);
        let mut lc = ListOfChars::new();
        line.text_in_cell(2, &tc, &mut lc);
        assert_eq!(lc.as_slice(), &['x' as u32]);
        let _ = &mut tc;
    }

    #[test]
    fn attribute_lookup() {
        assert_eq!(CellAttribute::from_name("bold"), Some(CellAttribute::Bold));
        assert_eq!(CellAttribute::from_name("nope"), None);
        let mut cells = vec![GpuCell::default(); 3];
        set_named_attribute_on_line(&mut cells, CellAttribute::Bold, 1);
        assert!(cells.iter().all(|c| c.attrs.bold()));
    }
}
