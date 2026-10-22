// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/line.c, kitty/lineops.h and kitty/cursor.c
// Copyright (C) 2016-2024 Kovid Goyal <kovid at kovidgoyal.net>

//! Line-level operations: SGR serialization, cell attributes, and plain/ANSI
//! text extraction.
//!
//! Hyperlinks, images, text-sizing (multicell) escape prefixes and prompt
//! markers are intentionally not serialized; see the crate level docs.

use kitty_core::cell::{SGR_MASK, VS15, VS16};
use kitty_core::{CellAttrs, CpuCell, GpuCell, Line, TextCache};

use crate::cursor::{Cursor, DECORATION_FG_CODE};

/// `color_as_sgr()`: serialize one encoded colour.
fn color_as_sgr(out: &mut String, val: u32, simple_code: u32, aix_code: u32, complex_code: u32) {
    match val & 0xff {
        1 => {
            let idx = val >> 8;
            if idx < 16 && simple_code != 0 {
                let code = if idx < 8 {
                    simple_code + idx
                } else {
                    aix_code + (idx - 8)
                };
                out.push_str(&code.to_string());
                out.push(';');
            } else {
                out.push_str(&format!("{complex_code}:5:{idx};"));
            }
        }
        2 => out.push_str(&format!(
            "{complex_code}:2:{}:{}:{};",
            (val >> 24) & 0xff,
            (val >> 16) & 0xff,
            (val >> 8) & 0xff
        )),
        _ => {
            out.push_str(&(complex_code + 1).to_string());
            out.push(';');
        }
    }
}

fn decoration_as_sgr(decoration: u8) -> &'static str {
    match decoration {
        1 => "4;",
        2 => "4:2;",
        3 => "4:3;",
        4 => "4:4;",
        5 => "4:5;",
        _ => "24;",
    }
}

/// `cell_as_sgr()`: the SGR parameters needed to turn `prev` into `cell`.
///
/// Returns an empty string when no attributes differ. The result does not
/// include the CSI prefix/suffix.
pub fn cell_as_sgr(cell: &GpuCell, prev: &GpuCell) -> String {
    let mut out = String::new();
    let ca = cell.attrs;
    let pa = prev.attrs;
    let intensity_differs = ca.bold() != pa.bold() || ca.dim() != pa.dim();
    if intensity_differs {
        if ca.bold() && ca.dim() {
            if !pa.bold() {
                out.push_str("1;");
            }
            if !pa.dim() {
                out.push_str("2;");
            }
        } else {
            out.push_str("22;");
            if ca.bold() {
                out.push_str("1;");
            }
            if ca.dim() {
                out.push_str("2;");
            }
        }
    }
    if ca.italic() != pa.italic() {
        out.push_str(if ca.italic() { "3;" } else { "23;" });
    }
    if ca.reverse() != pa.reverse() {
        out.push_str(if ca.reverse() { "7;" } else { "27;" });
    }
    if ca.strike() != pa.strike() {
        out.push_str(if ca.strike() { "9;" } else { "29;" });
    }
    if ca.blink() != pa.blink() {
        out.push_str(if ca.blink() { "5;" } else { "25;" });
    }
    if cell.fg != prev.fg {
        color_as_sgr(&mut out, cell.fg, 30, 90, 38);
    }
    if cell.bg != prev.bg {
        color_as_sgr(&mut out, cell.bg, 40, 100, 48);
    }
    if cell.decoration_fg != prev.decoration_fg {
        color_as_sgr(&mut out, cell.decoration_fg, 0, 0, DECORATION_FG_CODE);
    }
    if ca.decoration() != pa.decoration() {
        out.push_str(decoration_as_sgr(ca.decoration() as u8));
    }
    if out.ends_with(';') {
        out.pop();
    }
    out
}

/// `parse_color()` used by [`apply_sgr_to_cells()`].
fn parse_color(params: &[i32], i: &mut usize, result: &mut u32) {
    if *i >= params.len() {
        return;
    }
    let attr = params[*i] as u32;
    *i += 1;
    match attr {
        5 => {
            if *i < params.len() {
                *result = (((params[*i] as u32) & 0xff) << 8) | 1;
                *i += 1;
            }
        }
        2 if *i + 2 < params.len() => {
            if *i + 3 < params.len() {
                *i += 1;
            }
            let r = (params[*i] as u32) & 0xff;
            let g = (params[*i + 1] as u32) & 0xff;
            let b = (params[*i + 2] as u32) & 0xff;
            *i += 3;
            *result = (r << 24) | (g << 16) | (b << 8) | 2;
        }
        _ => {}
    }
}

fn set_color(cells: &mut [GpuCell], which: fn(&mut GpuCell) -> &mut u32, val: u32) {
    for c in cells.iter_mut() {
        *which(c) = val;
    }
}

/// `apply_sgr_to_cells()` from `cursor.c`.
///
/// Applies the parameters to every cell. Colour sequences consume their
/// sub-parameters. When `is_group` is true only the first parameter group is
/// applied.
pub fn apply_sgr_to_cells(cells: &mut [GpuCell], params: &[i32], is_group: bool) {
    let fallback = [0i32];
    let params = if params.is_empty() { &fallback } else { params };
    let mut i = 0usize;
    while i < params.len() {
        let attr = params[i] as u32;
        i += 1;
        match attr {
            0 => {
                for c in cells.iter_mut() {
                    c.attrs = CellAttrs::from_val(c.attrs.val() & !SGR_MASK);
                    c.fg = 0;
                    c.bg = 0;
                    c.decoration_fg = 0;
                }
            }
            1 => {
                for c in cells.iter_mut() {
                    c.attrs.set_bold(true);
                }
            }
            2 => {
                for c in cells.iter_mut() {
                    c.attrs.set_dim(true);
                }
            }
            3 => {
                for c in cells.iter_mut() {
                    c.attrs.set_italic(true);
                }
            }
            4 => {
                let val = if is_group && i < params.len() {
                    let v = params[i].clamp(0, 5) as u32;
                    i += 1;
                    v
                } else {
                    1
                };
                for c in cells.iter_mut() {
                    c.attrs.set_decoration(val);
                }
            }
            5 => {
                for c in cells.iter_mut() {
                    c.attrs.set_blink(true);
                }
            }
            7 => {
                for c in cells.iter_mut() {
                    c.attrs.set_reverse(true);
                }
            }
            9 => {
                for c in cells.iter_mut() {
                    c.attrs.set_strike(true);
                }
            }
            21 => {
                for c in cells.iter_mut() {
                    c.attrs.set_decoration(2);
                }
            }
            221 => {
                for c in cells.iter_mut() {
                    c.attrs.set_bold(false);
                }
            }
            222 => {
                for c in cells.iter_mut() {
                    c.attrs.set_dim(false);
                }
            }
            22 => {
                for c in cells.iter_mut() {
                    c.attrs.set_bold(false);
                    c.attrs.set_dim(false);
                }
            }
            23 => {
                for c in cells.iter_mut() {
                    c.attrs.set_italic(false);
                }
            }
            24 => {
                for c in cells.iter_mut() {
                    c.attrs.set_decoration(0);
                }
            }
            25 => {
                for c in cells.iter_mut() {
                    c.attrs.set_blink(false);
                }
            }
            27 => {
                for c in cells.iter_mut() {
                    c.attrs.set_reverse(false);
                }
            }
            29 => {
                for c in cells.iter_mut() {
                    c.attrs.set_strike(false);
                }
            }
            30..=37 => set_color(cells, |c| &mut c.fg, ((attr - 30) << 8) | 1),
            38 => {
                let mut color = 0;
                parse_color(params, &mut i, &mut color);
                if color != 0 {
                    set_color(cells, |c| &mut c.fg, color);
                }
            }
            39 => set_color(cells, |c| &mut c.fg, 0),
            40..=47 => set_color(cells, |c| &mut c.bg, ((attr - 40) << 8) | 1),
            48 => {
                let mut color = 0;
                parse_color(params, &mut i, &mut color);
                if color != 0 {
                    set_color(cells, |c| &mut c.bg, color);
                }
            }
            49 => set_color(cells, |c| &mut c.bg, 0),
            90..=97 => set_color(cells, |c| &mut c.fg, ((attr - 90 + 8) << 8) | 1),
            100..=107 => set_color(cells, |c| &mut c.bg, ((attr - 100 + 8) << 8) | 1),
            DECORATION_FG_CODE => {
                let mut color = 0;
                parse_color(params, &mut i, &mut color);
                if color != 0 {
                    set_color(cells, |c| &mut c.decoration_fg, color);
                }
            }
            x if x == DECORATION_FG_CODE + 1 => set_color(cells, |c| &mut c.decoration_fg, 0),
            _ => {}
        }
        if is_group {
            break;
        }
    }
}

/// `line_clear_text()`: clear character data while preserving formatting.
pub fn line_clear_text(line: &mut Line, at: usize, num: usize, ch: u32) {
    let end = (at + num).min(line.xnum());
    if at >= end {
        return;
    }
    let c = {
        let mut c = CpuCell::new();
        c.set_char(ch);
        c
    };
    line.cpu_cells[at..end].fill(c);
}

/// `line_apply_cursor()`: apply the cursor's attributes to a cell range.
///
/// With `clear_char` the characters are also zeroed, as the C code does for
/// erase and insert operations.
pub fn line_apply_cursor(
    line: &mut Line,
    cursor: &Cursor,
    at: usize,
    num: usize,
    clear_char: bool,
) {
    let end = (at + num).min(line.xnum());
    if at >= end {
        return;
    }
    let template = cursor.sgr.as_gpu_cell();
    if clear_char {
        line.cpu_cells[at..end].fill(CpuCell::new());
        line.gpu_cells[at..end].fill(template);
    } else {
        for i in at..end {
            let mark = line.gpu_cells[i].attrs.mark();
            let sprite = line.gpu_cells[i].sprite_idx;
            let mut gc = template;
            gc.attrs.set_mark(mark);
            gc.sprite_idx = sprite;
            line.gpu_cells[i] = gc;
        }
    }
}

/// `xlimit_for_line()`.
#[inline]
pub fn xlimit_for_line(line: &Line) -> usize {
    line.xlimit()
}

/// The text of one cell, as used by text/ANSI serialization.
fn cell_text<'a>(cell: &CpuCell, tc: &'a TextCache) -> &'a [u32] {
    if cell.ch_is_idx() {
        tc.chars_at(cell.ch())
    } else {
        // Callers handle the inline single-char case; this is only used for
        // indexed cells.
        &[]
    }
}

/// Extract plain text from `start..limit`, following the C `unicode_in_range()`
/// conventions used by `str(line)`.
pub fn line_as_text_range(
    line: &Line,
    tc: &TextCache,
    start: usize,
    limit: usize,
    skip_zero_cells: bool,
    skip_multiline_non_zero_lines: bool,
) -> String {
    let mut out = String::new();
    let limit = limit.min(line.xnum());
    let mut i = start;
    while i < limit {
        let cell = &line.cpu_cells[i];
        if cell.is_multicell()
            && (cell.x() != 0 || (skip_multiline_non_zero_lines && cell.y() != 0))
        {
            i += 1;
            continue;
        }
        let single = [cell.ch()];
        let chars: &[u32] = if cell.ch_is_idx() {
            cell_text(cell, tc)
        } else {
            &single
        };
        let first = chars.first().copied().unwrap_or(0);
        if first == 0 {
            if !skip_zero_cells {
                out.push(' ');
            }
            i += 1;
            continue;
        }
        if first == '\t' as u32 {
            out.push('\t');
            let mut to_skip = chars.get(1).copied().unwrap_or(0) as usize;
            while to_skip > 0 && i + 1 < limit {
                let next = &line.cpu_cells[i + 1];
                if !next.ch_is_idx() && next.ch() == ' ' as u32 {
                    i += 1;
                    to_skip -= 1;
                } else {
                    break;
                }
            }
        } else {
            for &cp in chars {
                if let Some(ch) = char::from_u32(cp) {
                    out.push(ch);
                }
            }
        }
        i += 1;
    }
    out
}

/// `line_as_unicode()` with the defaults used by `str(line)`.
pub fn line_as_text(line: &Line, tc: &TextCache) -> String {
    line_as_text_range(line, tc, 0, xlimit_for_line(line), false, true)
}

/// `line_as_ansi()`: serialize a line with SGR escape codes.
///
/// This mirrors `line_as_ansi()` for attributes and colours. Hyperlinks,
/// images, prompt markers and text-sizing prefixes are omitted because those
/// features are outside this port's scope.
pub fn line_as_ansi(line: &Line, tc: &TextCache, start_at: usize, stop_before: usize) -> String {
    let limit = stop_before.min(xlimit_for_line(line));
    if limit <= start_at {
        return String::new();
    }
    let mut out = String::new();
    let blank = GpuCell::default();
    let mut prev = blank;
    let mut pos = start_at;
    while pos < limit {
        let cell = &line.gpu_cells[pos];
        let cpu = &line.cpu_cells[pos];
        if (cell.attrs.val() & SGR_MASK) != (prev.attrs.val() & SGR_MASK)
            || cell.fg != prev.fg
            || cell.bg != prev.bg
            || cell.decoration_fg != prev.decoration_fg
        {
            let sgr = cell_as_sgr(cell, &prev);
            if !sgr.is_empty() {
                out.push_str("\x1b[");
                out.push_str(&sgr);
                out.push('m');
            }
        }
        prev = *cell;

        if cpu.is_multicell() && (cpu.x() != 0 || cpu.y() != 0) {
            pos += 1;
            continue;
        }
        let single = [cpu.ch()];
        let chars: &[u32] = if cpu.ch_is_idx() {
            cell_text(cpu, tc)
        } else {
            &single
        };
        let first = chars.first().copied().unwrap_or(0);
        if first == 0 {
            out.push(' ');
        } else if first == '\t' as u32 {
            out.push('\t');
            let mut to_skip = chars.get(1).copied().unwrap_or(0);
            while to_skip > 0 && pos + 1 < limit {
                let next = &line.cpu_cells[pos + 1];
                if !next.ch_is_idx() && next.ch() == ' ' as u32 {
                    pos += 1;
                    to_skip -= 1;
                } else {
                    break;
                }
            }
        } else {
            for &cp in chars {
                if let Some(ch) = char::from_u32(cp) {
                    out.push(ch);
                }
            }
        }
        pos += 1;
    }
    out
}

/// True if the cell text is a variation selector that should be hidden in the
/// fallback UTF-8 rendering.
#[inline]
pub fn is_variation_selector(ch: u32) -> bool {
    ch == VS15 || ch == VS16
}

#[cfg(test)]
mod tests {
    use super::*;
    use kitty_core::Line;

    #[test]
    fn sgr_serialization_round_trip() {
        let mut line = Line::new(2);
        let mut c = GpuCell::default();
        c.attrs.set_bold(true);
        c.attrs.set_blink(true);
        c.fg = (7 << 8) | 1;
        c.bg = (2 << 8) | 1;
        line.gpu_cells[0] = c;
        line.cpu_cells[0].set_char('a' as u32);
        let tc = TextCache::new();
        assert_eq!(line_as_ansi(&line, &tc, 0, 2), "\x1b[22;1;5;37;42ma");
    }

    #[test]
    fn apply_to_cells_simple() {
        let mut cells = vec![GpuCell::default(); 3];
        apply_sgr_to_cells(&mut cells, &[1, 31, 44], false);
        assert!(cells[0].attrs.bold());
        assert_eq!(cells[0].fg, (1 << 8) | 1);
        assert_eq!(cells[0].bg, (4 << 8) | 1);
        apply_sgr_to_cells(&mut cells, &[0], false);
        assert_eq!(cells[0].attrs.val() & SGR_MASK, 0);
        assert_eq!(cells[0].fg, 0);
    }
}
