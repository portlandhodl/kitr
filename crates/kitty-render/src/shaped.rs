// SPDX-License-Identifier: GPL-3.0-or-later
//! Shaped-run requests and sprites.
//!
//! A [`GlyphRasterizer`](crate::atlas::GlyphRasterizer) may additionally shape
//! a run of cells with HarfBuzz. When the run contains a ligature spanning
//! several cells, or a combining cluster with several codepoints in one cell,
//! the shaper returns one cell-aligned bitmap per special cluster. The
//! instance generator packs each bitmap into the normal atlas but keys it by a
//! stable hash of the whole run, so equal runs share one sprite.

use crate::atlas::GlyphBitmap;

/// Maximum number of cells the instance generator submits in one shaped run.
pub const SHAPED_RUN_MAX_CELLS: usize = 16;

/// Input for [`GlyphRasterizer::shape_run`](crate::atlas::GlyphRasterizer::shape_run).
#[derive(Clone, Copy, Debug)]
pub struct ShapedRunRequest<'a> {
    /// Font id also used in per-cell [`GlyphKey`](crate::atlas::GlyphKey)s.
    pub font_id: u16,
    /// Concatenated codepoints of all cells in the run.
    pub text: &'a [u32],
    /// Cell index (relative to the run) for each entry in `text`.
    pub cell_for_codepoint: &'a [u32],
    /// Number of cells in the run.
    pub cell_count: u32,
    /// Bold/italic and other flags from the first cell.
    pub flags: u32,
    pub cell_width: f32,
    pub cell_height: f32,
    pub baseline: f32,
}

/// One shaped cluster that needs its own atlas sprite.
///
/// `bitmap` must be aligned to the top-left of cell `cell_start` and should be
/// no larger than `cell_span * cell_width` by `cell_height`. The renderer draws
/// it into the rectangle covering those cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShapedRunCluster {
    /// Cell offset relative to the start of the run.
    pub cell_start: u32,
    /// Number of cells covered by this cluster.
    pub cell_span: u32,
    pub bitmap: GlyphBitmap,
}

/// Result of shaping one run. Only special clusters need to be returned;
/// ordinary one-glyph-per-cell output can be omitted and will be drawn through
/// the normal per-cell path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShapedRunOutput {
    pub clusters: Vec<ShapedRunCluster>,
}

/// Stable FNV-1a hash of a shaped run and one cluster within it.
///
/// The key includes the font, style flags, full run text and the cluster's
/// starting cell. This mirrors kitty's shaped-run cache keys closely enough
/// for deterministic atlas reuse while remaining independent of HarfBuzz types.
pub fn shaped_run_hash(
    font_id: u16,
    flags: u32,
    text: &[u32],
    cell_start: u32,
    cell_span: u32,
) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mut feed = |value: u64| {
        for byte in value.to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(PRIME);
        }
    };
    feed(font_id as u64);
    feed(flags as u64);
    feed(text.len() as u64);
    for &cp in text {
        feed(cp as u64);
    }
    feed(cell_start as u64);
    feed(cell_span as u64);
    hash
}
