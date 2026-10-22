// SPDX-License-Identifier: GPL-3.0-or-later
//! Core data structures for the Rust port of kitty.
//!
//! This crate currently mirrors `kitty/line.h`, `kitty/text-cache.c` and
//! `kitty/line-buf.c`. The on-disk/in-memory layout of the cell types matches
//! the C implementation exactly (verified by compile time assertions in
//! [`cell`]).

pub mod cell;
pub mod history;
pub mod line;
pub mod linebuf;
pub mod text_cache;

pub use cell::{CellAttrs, CharType, CpuCell, GpuCell, LineAttrs};
pub use history::{HistoryBuf, HistoryBufError};
pub use line::{CellAttribute, Line, ListOfChars};
pub use linebuf::{LineBuf, LineBufError, SharedTextCache};
pub use text_cache::TextCache;
