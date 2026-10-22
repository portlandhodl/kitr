// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/screen.c, kitty/screen.h, kitty/cursor.c, kitty/line.c,
// kitty/lineops.h, kitty/modes.h and kitty/charsets.c
// Copyright (C) 2016-2024 Kovid Goyal <kovid at kovidgoyal.net>

//! # kitty-screen
//!
//! A 1:1 port of the core terminal screen state from kitty.
//!
//! The crate owns the same data structures as the C code: two [`LineBuf`]s
//! (main and alternate), a [`HistoryBuf`], a shared [`TextCache`], the cursor,
//! scroll margins, tab stops, modes and charset state. Drawing preserves the
//! C quirks that are observable through kitty's tests, including the batched
//! printable-ASCII fast path (driven by
//! [`kitty_simd::printable_ascii_run_length`]), grapheme clustering via
//! `kitty-unicode`, wide-cell continuation cells, insert mode, tabs, and
//! scrolling into scrollback.
//!
//! ## Implemented API
//!
//! * [`Screen`]/[`ScreenBuilder`]/[`ScreenOptions`] construction.
//! * [`Cursor`] and [`SgrAttrs`], including SGR colour encoding.
//! * [`CharSetState`](charset::CharsetState) for G0/G1, VT100 graphics, CP437,
//!   VAX and UK.
//! * [`Modes`], [`modes`] constants and the private-mode shift encoding.
//! * Drawing ([`Screen::draw_text`]), cursor movement, margins, tab stops,
//!   save/restore (DECSC/DECRC), scrolling, alternate screen, erase, insert
//!   and delete line/character operations.
//! * Line text/ANSI extraction ([`lineops::line_as_text`],
//!   [`lineops::line_as_ansi`], [`lineops::cell_as_sgr`]) and SGR application
//!   to GPU cells.
//! * [`ScreenHandler`], a [`kitty_vt::Handler`] implementing the corresponding
//!   ESC/CSI actions. Unsupported actions call `report_error`.
//!
//! ## Explicitly not implemented
//!
//! The following areas have no implementation and never report fake success:
//!
//! * graphics / the kitty graphics protocol, including image placeholders;
//! * hyperlinks and URL detection/serialization;
//! * selections and text-for-selection;
//! * OSC colour/theme mutations (titles and icon names are stored);
//! * resize and rewrap / history reuse;
//! * fonts, cell metrics, ligatures and text-sizing (multicell) commands;
//! * mouse tracking, DnD and pointer shapes;
//! * shell integration / prompt marking semantics (the `LineAttrs` bits exist,
//!   but prompt commands are not dispatched);
//! * scrollbar/pager history and pixel/fractional scrolling;
//! * device status/attribute reports, window manipulation, colour stacks,
//!   key-encoding flag stacks, IME/overlay lines;
//! * DECCARA/DECRARA (the `CSI r` SGR form is reported as unsupported).

pub mod charset;
pub mod cursor;
pub mod handler;
pub mod lineops;
pub mod modes;
pub mod screen;

pub use charset::CharsetState;
pub use cursor::{Cursor, CursorShape, SgrAttrs, DECORATION_FG_CODE};
pub use handler::{parse_into, ScreenHandler};
pub use lineops::{apply_sgr_to_cells, cell_as_sgr, line_as_ansi, line_as_text};
pub use modes::Modes;
pub use screen::{Screen, ScreenBuilder, ScreenError, ScreenOptions, CSI_REP_MAX_REPETITIONS};

// Re-export the core types most users of a screen need.
pub use kitty_core::{CpuCell, GpuCell, HistoryBuf, Line, LineBuf, TextCache};
pub use kitty_vt::{CsiParams, Handler, Parser};
