// SPDX-License-Identifier: GPL-3.0-or-later
// Screen-side graphics sink for the kitty graphics protocol.
// The pixel pipeline lives in the `kitty-graphics` crate; this module stores
// placements and implements the `GraphicsSink` callbacks without any GPU work.
// Copyright (C) 2017-2024 Kovid Goyal <kovid at kovidgoyal.net>

//! Screen integration for the kitty graphics protocol.
//!
//! This is deliberately a *sink*: it stores decoded images and placement
//! metadata keyed by image id and placement id, implements display/delete/query
//! bookkeeping, and exposes placeholder metadata through the line attributes.
//! It does not upload textures or render anything.
//!
//! The VT parser exposes APC (`ESC _ G ... ESC \`) through
//! [`kitty_vt::Handler::apc_dispatch`]. [`crate::ScreenHandler`] forwards those
//! payloads to [`Screen::handle_graphics_escape`]. If you drive a [`Screen`]
//! directly, call [`Screen::handle_graphics_command`] instead.

use std::collections::{HashMap, HashSet};

use kitty_core::cell::IMAGE_PLACEHOLDER_CHAR;
use kitty_graphics::{
    parse_escape, Action, DeleteAction, GraphicsCommand, GraphicsError, GraphicsSink, Image,
    LoadOutcome, Placement, PlacementGraph, PlacementKey, PlacementRecord, SourceRect,
};

use crate::screen::Screen;

/// One materialised virtual placement cell produced by a `U+10EEEE`
/// placeholder.
///
/// The raw diacritic values are kept (zero when absent) so callers can group
/// cells into image-space runs using the same continuation rules as kitty.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MaterializedPlacement {
    pub image_id: u32,
    pub placement_id: u32,
    pub screen_x: u32,
    pub screen_y: u32,
    /// Screen columns covered by this cell, including wide/multicell spans.
    pub screen_columns: u32,
    /// Raw row diacritic (`0` when absent).
    pub image_row: u32,
    /// Raw column diacritic (`0` when absent).
    pub image_column: u32,
    /// Raw high image-id diacritic (`0` when absent).
    pub image_id_high: u32,
}

/// Map a combining diacritic to kitty's image row/column number.
pub fn diacritic_to_num(code: u32) -> u32 {
    match code {
        0x305..=0x306 => code - 0x305 + 1,
        0x30d..=0x30f => code - 0x30d + 2,
        0x310..=0x311 => code - 0x310 + 4,
        0x312..=0x313 => code - 0x312 + 5,
        0x33d..=0x340 => code - 0x33d + 6,
        0x346..=0x347 => code - 0x346 + 9,
        0x34a..=0x34d => code - 0x34a + 10,
        0x350..=0x353 => code - 0x350 + 13,
        0x357..=0x358 => code - 0x357 + 16,
        0x35b..=0x35c => code - 0x35b + 17,
        0x363..=0x370 => code - 0x363 + 18,
        0x483..=0x488 => code - 0x483 + 31,
        0x592..=0x596 => code - 0x592 + 36,
        0x597..=0x59a => code - 0x597 + 40,
        0x59c..=0x5a2 => code - 0x59c + 43,
        0x5a8..=0x5aa => code - 0x5a8 + 49,
        0x5ab..=0x5ad => code - 0x5ab + 51,
        0x5af..=0x5b0 => code - 0x5af + 53,
        0x5c4..=0x5c5 => code - 0x5c4 + 54,
        0x610..=0x618 => code - 0x610 + 55,
        0x657..=0x65c => code - 0x657 + 63,
        0x65d..=0x65f => code - 0x65d + 68,
        0x6d6..=0x6dd => code - 0x6d6 + 70,
        0x6df..=0x6e3 => code - 0x6df + 77,
        0x6e4..=0x6e5 => code - 0x6e4 + 81,
        0x6e7..=0x6e9 => code - 0x6e7 + 82,
        0x6eb..=0x6ed => code - 0x6eb + 84,
        0x730..=0x731 => code - 0x730 + 86,
        0x732..=0x734 => code - 0x732 + 87,
        0x735..=0x737 => code - 0x735 + 89,
        0x73a..=0x73b => code - 0x73a + 91,
        0x73d..=0x73e => code - 0x73d + 92,
        0x73f..=0x742 => code - 0x73f + 93,
        0x743..=0x744 => code - 0x743 + 96,
        0x745..=0x746 => code - 0x745 + 97,
        0x747..=0x748 => code - 0x747 + 98,
        0x749..=0x74b => code - 0x749 + 99,
        0x7eb..=0x7f2 => code - 0x7eb + 101,
        0x7f3..=0x7f4 => code - 0x7f3 + 108,
        0x816..=0x81a => code - 0x816 + 109,
        0x81b..=0x824 => code - 0x81b + 113,
        0x825..=0x828 => code - 0x825 + 122,
        0x829..=0x82e => code - 0x829 + 125,
        0x951..=0x952 => code - 0x951 + 130,
        0x953..=0x955 => code - 0x953 + 131,
        0xf82..=0xf84 => code - 0xf82 + 133,
        0xf86..=0xf88 => code - 0xf86 + 135,
        0x135d..=0x1360 => code - 0x135d + 137,
        0x17dd..=0x17de => code - 0x17dd + 140,
        0x193a..=0x193b => code - 0x193a + 141,
        0x1a17..=0x1a18 => code - 0x1a17 + 142,
        0x1a75..=0x1a7d => code - 0x1a75 + 143,
        0x1b6b..=0x1b6c => code - 0x1b6b + 151,
        0x1b6d..=0x1b74 => code - 0x1b6d + 152,
        0x1cd0..=0x1cd3 => code - 0x1cd0 + 159,
        0x1cda..=0x1cdc => code - 0x1cda + 162,
        0x1ce0..=0x1ce1 => code - 0x1ce0 + 164,
        0x1dc0..=0x1dc2 => code - 0x1dc0 + 165,
        0x1dc3..=0x1dca => code - 0x1dc3 + 167,
        0x1dcb..=0x1dcd => code - 0x1dcb + 174,
        0x1dd1..=0x1de7 => code - 0x1dd1 + 176,
        0x1dfe..=0x1dff => code - 0x1dfe + 198,
        0x20d0..=0x20d2 => code - 0x20d0 + 199,
        0x20d4..=0x20d8 => code - 0x20d4 + 201,
        0x20db..=0x20dd => code - 0x20db + 205,
        0x20e1..=0x20e2 => code - 0x20e1 + 207,
        0x20e7..=0x20e8 => code - 0x20e7 + 208,
        0x20e9..=0x20ea => code - 0x20e9 + 209,
        0x20f0..=0x20f1 => code - 0x20f0 + 210,
        0x2cef..=0x2cf2 => code - 0x2cef + 211,
        0x2de0..=0x2e00 => code - 0x2de0 + 214,
        0xa66f..=0xa670 => code - 0xa66f + 246,
        0xa67c..=0xa67e => code - 0xa67c + 247,
        0xa6f0..=0xa6f2 => code - 0xa6f0 + 249,
        0xa8e0..=0xa8f2 => code - 0xa8e0 + 251,
        0xaab0..=0xaab1 => code - 0xaab0 + 269,
        0xaab2..=0xaab4 => code - 0xaab2 + 270,
        0xaab7..=0xaab9 => code - 0xaab7 + 272,
        0xaabe..=0xaac0 => code - 0xaabe + 274,
        0xaac1..=0xaac2 => code - 0xaac1 + 276,
        0xfe20..=0xfe27 => code - 0xfe20 + 277,
        0x10a0f..=0x10a10 => code - 0x10a0f + 284,
        0x10a38..=0x10a39 => code - 0x10a38 + 285,
        0x1d185..=0x1d18a => code - 0x1d185 + 286,
        0x1d1aa..=0x1d1ae => code - 0x1d1aa + 291,
        0x1d242..=0x1d245 => code - 0x1d242 + 295,
        _ => 0,
    }
}

/// Extract the low 24 image-id bits from a packed cell colour.
#[inline]
pub fn color_to_image_id(color: u32) -> u32 {
    (color >> 8) & 0x00ff_ffff
}

/// A stored placement. The image itself lives in [`Screen::images`].
#[derive(Clone, Debug, PartialEq)]
pub struct ImagePlacement {
    pub image_id: u32,
    pub placement_id: u32,
    /// Unique identity for anonymous (`p=0`) placements. The protocol allows
    /// several placements of one image without a placement id; this stays zero
    /// for named placements and is non-zero for each anonymous one.
    pub graph_id: u32,
    pub image_number: u32,
    pub placement: Placement,
    /// Screen cell where the placement was created (used by `d=c`/`d=C`).
    pub anchor_x: u32,
    pub anchor_y: u32,
}

impl ImagePlacement {
    /// Key used by [`Screen::placement_graph`] and cascade deletion.
    pub const fn graph_key(&self) -> PlacementKey {
        PlacementKey::new(self.image_id, self.placement_id).with_instance(self.graph_id)
    }
}

/// Query bookkeeping for `a=q`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphicsQuery {
    pub image_id: u32,
    pub image_number: u32,
    pub found: bool,
}

/// Unsupported graphics commands seen by the screen, in arrival order.
#[derive(Clone, Debug, PartialEq)]
pub struct UnsupportedGraphics {
    pub action: Option<Action>,
    pub delete_action: Option<DeleteAction>,
    pub image_id: u32,
}

impl Screen {
    /// Feed one parsed graphics command. This is the parser hook needed when
    /// you do not use [`crate::ScreenHandler`].
    pub fn handle_graphics_command(
        &mut self,
        command: GraphicsCommand,
    ) -> Result<LoadOutcome, GraphicsError> {
        let outcome = match self.image_assembler.feed(&command) {
            Ok(o) => o,
            Err(e) => {
                self.last_graphics_error = Some(e.clone());
                return Err(e);
            }
        };
        match &outcome {
            LoadOutcome::Loaded(loaded) => {
                if loaded.query {
                    self.on_query(&command, Some(&loaded.image))?;
                } else {
                    self.on_image(&command, &loaded.image)?;
                    if loaded.display {
                        self.on_display(&command, &loaded.image.placement)?;
                    }
                }
            }
            LoadOutcome::Deferred(_) => match command.action {
                Some(Action::Delete) => self.on_delete(&command)?,
                Some(Action::Put) => {
                    let placement = Placement::from_command(&command);
                    self.on_display(&command, &placement)?;
                }
                _ => self.on_unsupported(&command)?,
            },
            LoadOutcome::ChunkAccepted => {}
        }
        Ok(outcome)
    }

    /// Parse and feed one complete `ESC _ G ... ESC \` sequence.
    pub fn handle_graphics_escape(&mut self, input: &[u8]) -> Result<LoadOutcome, GraphicsError> {
        let command = parse_escape(input).map_err(|e| {
            let e = GraphicsError::Parse(e);
            self.last_graphics_error = Some(e.clone());
            e
        })?;
        self.handle_graphics_command(command)
    }

    /// Stored image by client image id.
    pub fn image(&self, id: u32) -> Option<&Image> {
        self.images.get(&id)
    }

    /// All stored images, keyed by client image id.
    pub fn images(&self) -> &HashMap<u32, Image> {
        &self.images
    }

    /// All placements, in insertion order.
    pub fn placements(&self) -> &[ImagePlacement] {
        &self.placements
    }

    /// Placement by `(image id, placement id)`.
    pub fn placement(&self, image_id: u32, placement_id: u32) -> Option<&ImagePlacement> {
        self.placements
            .iter()
            .find(|p| p.image_id == image_id && p.placement_id == placement_id)
    }

    /// Remove all graphics state.
    pub fn clear_images(&mut self) {
        self.images.clear();
        self.placements.clear();
        self.image_assembler.clear_pending();
        self.graphics_queries.clear();
        self.graphics_unsupported.clear();
    }

    /// True if the screen row has image placeholders.
    pub fn line_has_image_placeholders(&self, y: u32) -> bool {
        if y >= self.lines {
            return false;
        }
        self.linebuf()
            .line_attrs(y as usize)
            .has_image_placeholders()
    }

    /// History row placeholder metadata (`0` is the newest history line).
    pub fn history_line_has_image_placeholders(&self, lnum: u32) -> bool {
        if lnum >= self.historybuf().count {
            return false;
        }
        self.historybuf()
            .line(lnum)
            .map(|l| l.attrs.has_image_placeholders())
            .unwrap_or(false)
    }

    /// Materialise every `U+10EEEE` placeholder cell currently on screen.
    ///
    /// Virtual placements are stored once, but the protocol resolves them per
    /// placeholder cell. This exposes those cells (including their raw
    /// diacritics) so the renderer can group them into image-space runs and so
    /// the placement graph can anchor a virtual placement at the top-left
    /// placeholder cell.
    pub fn materialize_virtual_placements(&self) -> Vec<MaterializedPlacement> {
        let mut cells = Vec::new();
        let tc = self.text_cache().borrow();
        for y in 0..self.lines {
            for x in 0..self.columns {
                let cpu = self.cpu_cell(x, y);
                let mut single = [0u32; 1];
                let text: &[u32] = if cpu.ch_is_idx() {
                    tc.chars_at(cpu.ch())
                } else if cpu.ch() != 0 {
                    single[0] = cpu.ch();
                    &single[..1]
                } else {
                    &[]
                };
                if text.first().copied() != Some(IMAGE_PLACEHOLDER_CHAR) {
                    continue;
                }
                let gpu = self.gpu_cell(x, y);
                let lower = color_to_image_id(gpu.fg);
                let placement_id = color_to_image_id(gpu.decoration_fg);
                let image_row = text
                    .get(1)
                    .copied()
                    .filter(|code| *code != 0)
                    .map_or(0, diacritic_to_num);
                let image_column = text
                    .get(2)
                    .copied()
                    .filter(|code| *code != 0)
                    .map_or(0, diacritic_to_num);
                let image_id_high = text
                    .get(3)
                    .copied()
                    .filter(|code| *code != 0)
                    .map_or(0, diacritic_to_num);
                let screen_columns = if cpu.is_multicell() && cpu.width() > 0 {
                    cpu.mcd_x_limit().max(1).min(self.columns - x)
                } else {
                    1
                };
                cells.push(MaterializedPlacement {
                    image_id: lower | (image_id_high.saturating_sub(1) << 24),
                    placement_id,
                    screen_x: x,
                    screen_y: y,
                    screen_columns,
                    image_row,
                    image_column,
                    image_id_high,
                });
            }
        }
        cells
    }

    /// Build a validated relative-placement graph for the current screen.
    ///
    /// Virtual placements are anchored at the minimum placeholder cell that
    /// refers to them, matching `resolve_cell_ref()` in `kitty/graphics.c`.
    pub fn placement_graph(&self) -> PlacementGraph {
        let cells = self.materialize_virtual_placements();
        let mut graph = PlacementGraph::new();
        for placement in &self.placements {
            let p = &placement.placement;
            let mut record = PlacementRecord::new(
                placement.graph_key(),
                placement.anchor_x,
                placement.anchor_y,
            )
            .with_parent(p.parent_id, p.parent_placement_id)
            .with_offset(p.offset_from_parent_x, p.offset_from_parent_y)
            .with_source(SourceRect::new(p.x_offset, p.y_offset, p.width, p.height))
            .with_z_index(p.z_index)
            .inheriting(p.parent_id != 0 && p.z_index == 0, p.parent_id != 0);
            if p.unicode_placement {
                record = record.as_virtual();
            }
            let key = record.key;
            if graph.insert(record).is_err() {
                continue;
            }
            if p.unicode_placement {
                let anchor = cells
                    .iter()
                    .filter(|cell| {
                        cell.image_id == key.image_id
                            && (key.placement_id == 0 || cell.placement_id == key.placement_id)
                    })
                    .map(|cell| (cell.screen_x, cell.screen_y))
                    .min();
                if let Some((x, y)) = anchor {
                    graph.set_anchor(key, x, y);
                }
            }
        }
        graph
    }

    fn remove_keys_with_descendants(
        &mut self,
        graph: &mut PlacementGraph,
        keys: impl IntoIterator<Item = PlacementKey>,
    ) {
        let mut removed = HashSet::new();
        for key in keys {
            for key in graph.remove_cascade(key) {
                removed.insert(key);
            }
        }
        if removed.is_empty() {
            return;
        }
        self.placements
            .retain(|p| !removed.contains(&p.graph_key()));
    }
}

impl Screen {
    /// Mirror kitty's `update_dest_rect()`: resolve `c=`/`r=` from the source
    /// dimensions and the cell pixel size when either is omitted.
    fn placement_cell_span(&self, image_id: u32, placement: &Placement) -> (u32, u32) {
        let (cell_width, cell_height) = (self.cell_width.max(1), self.cell_height.max(1));
        let image = self.images.get(&image_id);
        let mut src_width = if placement.width != 0 {
            placement.width
        } else {
            image.map(|img| img.width).unwrap_or(1)
        };
        let mut src_height = if placement.height != 0 {
            placement.height
        } else {
            image.map(|img| img.height).unwrap_or(1)
        };
        if let Some(image) = image {
            src_width = src_width.min(
                (image.width as f32 - placement.x_offset as f32)
                    .max(0.0)
                    .max(0.0) as u32,
            );
            src_height = src_height.min(
                (image.height as f32 - placement.y_offset as f32)
                    .max(0.0)
                    .max(0.0) as u32,
            );
        }
        let cell_x_offset = placement.cell_x_offset.min(cell_width - 1);
        let cell_y_offset = placement.cell_y_offset.min(cell_height - 1);
        let auto_cols = placement.num_cols == 0;
        let auto_rows = placement.num_rows == 0;
        let mut cols = placement.num_cols;
        let mut rows = placement.num_rows;
        if auto_cols {
            if auto_rows {
                let t = src_width.saturating_add(cell_x_offset);
                cols = t / cell_width;
                if t > cols * cell_width {
                    cols += 1;
                }
            } else {
                let height_px = cell_height * rows + cell_y_offset;
                let width_px = height_px as f64 * src_width as f64 / src_height.max(1) as f64;
                cols = (width_px / cell_width as f64).ceil() as u32;
            }
        }
        if auto_rows {
            if auto_cols {
                let t = src_height.saturating_add(cell_y_offset);
                rows = t / cell_height;
                if t > rows * cell_height {
                    rows += 1;
                }
            } else {
                let width_px = cell_width * cols + cell_x_offset;
                let height_px = width_px as f64 * src_height as f64 / src_width.max(1) as f64;
                rows = (height_px / cell_height as f64).ceil() as u32;
            }
        }
        (cols.max(1), rows.max(1))
    }
}

impl GraphicsSink for Screen {
    fn on_image(&mut self, command: &GraphicsCommand, image: &Image) -> Result<(), GraphicsError> {
        let id = if image.id != 0 {
            image.id
        } else if command.id != 0 {
            command.id
        } else {
            image.number
        };
        if id != 0 {
            let mut stored = image.clone();
            stored.id = id;
            self.images.insert(id, stored);
        }
        Ok(())
    }

    fn on_display(
        &mut self,
        command: &GraphicsCommand,
        placement: &Placement,
    ) -> Result<(), GraphicsError> {
        let image_id = if placement.image_id != 0 {
            placement.image_id
        } else {
            command.id
        };
        if image_id == 0 {
            return Ok(());
        }
        let (effective_cols, effective_rows) = self.placement_cell_span(image_id, placement);
        let placement_anchor_x = self.cursor.x.min(self.columns.saturating_sub(1));
        let placement_anchor_y = self.cursor.y.min(self.lines.saturating_sub(1));
        // `p=0` means "anonymous": every put creates a new placement. Named
        // placements replace an existing placement with the same id.
        let graph_id = if placement.placement_id == 0 {
            self.placements
                .iter()
                .filter(|p| p.image_id == image_id && p.placement_id == 0)
                .map(|p| p.graph_id)
                .max()
                .unwrap_or(0)
                .wrapping_add(1)
                .max(1)
        } else {
            0
        };
        let entry = ImagePlacement {
            image_id,
            placement_id: placement.placement_id,
            graph_id,
            image_number: placement.image_number,
            placement: placement.clone(),
            anchor_x: placement_anchor_x,
            anchor_y: placement_anchor_y,
        };

        // Validate the full parent chain before mutating screen state. The
        // graph is rebuilt from the stored placements so it also sees anchors
        // that were materialised from Unicode placeholder cells.
        let mut graph = self.placement_graph();
        let record = PlacementRecord::new(entry.graph_key(), entry.anchor_x, entry.anchor_y)
            .with_parent(
                entry.placement.parent_id,
                entry.placement.parent_placement_id,
            )
            .with_offset(
                entry.placement.offset_from_parent_x,
                entry.placement.offset_from_parent_y,
            )
            .with_source(SourceRect::new(
                entry.placement.x_offset,
                entry.placement.y_offset,
                entry.placement.width,
                entry.placement.height,
            ))
            .with_z_index(entry.placement.z_index)
            .inheriting(
                entry.placement.parent_id != 0 && entry.placement.z_index == 0,
                entry.placement.parent_id != 0,
            );
        let record = if entry.placement.unicode_placement {
            record.as_virtual()
        } else {
            record
        };
        let key = record.key;
        graph.insert(record)?;
        graph.validate(key)?;

        if entry.placement_id != 0 {
            if let Some(existing) = self
                .placements
                .iter_mut()
                .find(|p| p.image_id == entry.image_id && p.placement_id == entry.placement_id)
            {
                *existing = entry;
            } else {
                self.placements.push(entry);
            }
        } else {
            self.placements.push(entry);
        }
        if placement.unicode_placement {
            let rows = if placement.num_rows == 0 {
                1
            } else {
                placement.num_rows
            };
            let start = placement_anchor_y as usize;
            for y in start..(start + rows as usize).min(self.lines as usize) {
                self.linebuf_mut().set_line_has_image_placeholders(y, true);
            }
        } else if placement.cursor_movement != 1 {
            // kitty advances the cursor by the image's effective cell span
            // unless `C=1` asks it not to. The span uses `c=`/`r=` when given,
            // otherwise it is derived from the source pixels and the cell
            // pixel metrics.
            self.cursor.x = self.cursor.x.saturating_add(effective_cols);
            if effective_rows > 0 {
                self.cursor.y = self.cursor.y.saturating_add(effective_rows - 1);
            }
            self.ensure_bounds(false, false);
        }
        Ok(())
    }

    fn on_delete(&mut self, command: &GraphicsCommand) -> Result<(), GraphicsError> {
        let action = command.delete_action.unwrap_or(DeleteAction::All);
        let mut graph = self.placement_graph();
        let keys = ImagePlacement::graph_key;
        let point = (
            i64::from(command.x_offset.saturating_sub(1)),
            i64::from(command.y_offset.saturating_sub(1)),
        );
        let contains = |placement: &ImagePlacement, x: i64, y: i64| {
            let px = i64::from(placement.anchor_x);
            let py = i64::from(placement.anchor_y);
            let cols = i64::from(placement.placement.num_cols.max(1));
            let rows = i64::from(placement.placement.num_rows.max(1));
            px <= x && x < px + cols && py <= y && y < py + rows
        };
        match action {
            DeleteAction::All => {
                self.images.clear();
                self.placements.clear();
                self.image_assembler.clear_pending();
            }
            DeleteAction::AllButId => {
                if command.id != 0 {
                    let victims: Vec<PlacementKey> = self
                        .placements
                        .iter()
                        .filter(|p| p.image_id != command.id)
                        .map(keys)
                        .collect();
                    self.remove_keys_with_descendants(&mut graph, victims);
                    self.images.retain(|id, _| *id == command.id);
                }
            }
            DeleteAction::ById | DeleteAction::ByIdNoRefs => {
                let victims: Vec<PlacementKey> = self
                    .placements
                    .iter()
                    .filter(|p| p.image_id == command.id)
                    .map(keys)
                    .collect();
                self.remove_keys_with_descendants(&mut graph, victims);
                self.images.remove(&command.id);
            }
            DeleteAction::ByNumber | DeleteAction::ByNumberNoRefs => {
                let image_ids: Vec<u32> = self
                    .images
                    .iter()
                    .filter(|(_, img)| img.number == command.image_number)
                    .map(|(id, _)| *id)
                    .collect();
                let victims: Vec<PlacementKey> = self
                    .placements
                    .iter()
                    .filter(|p| image_ids.contains(&p.image_id))
                    .map(keys)
                    .collect();
                self.remove_keys_with_descendants(&mut graph, victims);
                for id in image_ids {
                    self.images.remove(&id);
                }
            }
            DeleteAction::PlacementId => {
                let pid = command.placement_id;
                let iid = command.id;
                let victims: Vec<PlacementKey> = self
                    .placements
                    .iter()
                    .filter(|p| p.placement_id == pid && (iid == 0 || p.image_id == iid))
                    .map(keys)
                    .collect();
                self.remove_keys_with_descendants(&mut graph, victims);
            }
            DeleteAction::AtCursor => {
                let (x, y) = (self.cursor.x, self.cursor.y);
                let victims: Vec<PlacementKey> = self
                    .placements
                    .iter()
                    .filter(|p| p.anchor_x == x && p.anchor_y == y)
                    .map(keys)
                    .collect();
                self.remove_keys_with_descendants(&mut graph, victims);
            }
            DeleteAction::AtCursorPoint => {
                let (x, y) = (i64::from(self.cursor.x), i64::from(self.cursor.y));
                let victims: Vec<PlacementKey> = self
                    .placements
                    .iter()
                    .filter(|p| contains(p, x, y))
                    .map(keys)
                    .collect();
                self.remove_keys_with_descendants(&mut graph, victims);
            }
            DeleteAction::AtPoint => {
                let victims: Vec<PlacementKey> = self
                    .placements
                    .iter()
                    .filter(|p| contains(p, point.0, point.1))
                    .map(keys)
                    .collect();
                self.remove_keys_with_descendants(&mut graph, victims);
            }
            DeleteAction::AtPoint3d => {
                let z = command.z_index;
                let victims: Vec<PlacementKey> = self
                    .placements
                    .iter()
                    .filter(|p| p.placement.z_index == z && contains(p, point.0, point.1))
                    .map(keys)
                    .collect();
                self.remove_keys_with_descendants(&mut graph, victims);
            }
            DeleteAction::AtCell => {
                let (x, y) = (self.cursor.x, self.cursor.y);
                let victims: Vec<PlacementKey> = self
                    .placements
                    .iter()
                    .filter(|p| p.anchor_x == x && p.anchor_y == y)
                    .map(keys)
                    .collect();
                self.remove_keys_with_descendants(&mut graph, victims);
            }
            DeleteAction::ZIndex | DeleteAction::ZIndexUpper => {
                let z = command.z_index;
                let victims: Vec<PlacementKey> = self
                    .placements
                    .iter()
                    .filter(|p| p.placement.z_index == z)
                    .map(keys)
                    .collect();
                self.remove_keys_with_descendants(&mut graph, victims);
            }
            _ => {
                // Animation/cell-range variants are not executed by the loader.
                let victims: Vec<PlacementKey> = self.placements.iter().map(keys).collect();
                self.remove_keys_with_descendants(&mut graph, victims);
            }
        }
        Ok(())
    }

    fn on_query(
        &mut self,
        command: &GraphicsCommand,
        image: Option<&Image>,
    ) -> Result<(), GraphicsError> {
        let found = image.is_some() || self.images.contains_key(&command.id);
        self.graphics_queries.push(GraphicsQuery {
            image_id: command.id,
            image_number: command.image_number,
            found,
        });
        Ok(())
    }

    fn on_unsupported(&mut self, command: &GraphicsCommand) -> Result<(), GraphicsError> {
        self.graphics_unsupported.push(UnsupportedGraphics {
            action: command.action,
            delete_action: command.delete_action,
            image_id: command.id,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kitty_graphics::parse_raw;

    fn screen() -> Screen {
        Screen::builder(10, 5).scrollback(8).build().unwrap()
    }

    fn transmit(id: u32) -> GraphicsCommand {
        let mut c = parse_raw(format!("a=t,f=32,s=1,v=1,i={id}").as_bytes()).unwrap();
        c.payload = vec![1, 2, 3, 255];
        c
    }

    #[test]
    fn image_and_placement_lifecycle() {
        let mut s = screen();
        s.handle_graphics_command(transmit(7)).unwrap();
        assert!(s.image(7).is_some());
        let mut put = parse_raw(b"a=p,i=7,p=3,c=2,r=1,z=5").unwrap();
        put.payload.clear();
        s.handle_graphics_command(put).unwrap();
        assert!(s.placement(7, 3).is_some());
        assert_eq!(s.placements()[0].placement.num_cols, 2);
        assert_eq!(s.placements()[0].placement.z_index, 5);
        let del = parse_raw(b"a=d,d=p,i=7,p=3").unwrap();
        s.handle_graphics_command(del).unwrap();
        assert!(s.placement(7, 3).is_none());
        assert!(s.image(7).is_some());
        let del = parse_raw(b"a=d,d=i,i=7").unwrap();
        s.handle_graphics_command(del).unwrap();
        assert!(s.image(7).is_none());
    }

    #[test]
    fn delete_all_and_query() {
        let mut s = screen();
        s.handle_graphics_command(transmit(1)).unwrap();
        s.handle_graphics_command(transmit(2)).unwrap();
        let mut put = parse_raw(b"a=p,i=1,p=1").unwrap();
        put.payload.clear();
        s.handle_graphics_command(put).unwrap();
        assert_eq!(s.placements().len(), 1);
        let mut q = parse_raw(b"a=q,f=32,s=1,v=1,i=1").unwrap();
        q.payload = vec![1, 2, 3, 255];
        s.handle_graphics_command(q).unwrap();
        assert_eq!(s.graphics_queries.len(), 1);
        assert!(s.graphics_queries[0].found);
        let del = parse_raw(b"a=d,d=a").unwrap();
        s.handle_graphics_command(del).unwrap();
        assert!(s.images().is_empty());
        assert!(s.placements().is_empty());
    }

    #[test]
    fn put_advances_cursor_by_cell_metrics() {
        use crate::ScreenOptions;
        let mut s = Screen::builder(10, 5)
            .options(ScreenOptions {
                scrollback: 8,
                cell_width: 10,
                cell_height: 19,
            })
            .build()
            .unwrap();
        let mut t = parse_raw(b"a=t,f=32,s=25,v=40,i=1").unwrap();
        t.payload = vec![0u8; 25 * 40 * 4];
        s.handle_graphics_command(t).unwrap();
        let mut put = parse_raw(b"a=p,i=1").unwrap();
        put.payload.clear();
        s.handle_graphics_command(put).unwrap();
        assert_eq!((s.cursor.x, s.cursor.y), (3, 2));

        s.cursor_position(1, 1);
        let mut no_move = parse_raw(b"a=p,i=1,C=1").unwrap();
        no_move.payload.clear();
        s.handle_graphics_command(no_move).unwrap();
        assert_eq!((s.cursor.x, s.cursor.y), (0, 0));
    }

    #[test]
    fn placeholder_metadata() {
        let mut s = screen();
        s.linebuf_mut().set_line_has_image_placeholders(1, true);
        assert!(s.line_has_image_placeholders(1));
        assert!(!s.line_has_image_placeholders(0));
        assert!(!s.history_line_has_image_placeholders(0));
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;
    use kitty_core::cell::IMAGE_PLACEHOLDER_CHAR;
    use kitty_graphics::parse_raw;

    fn put(screen: &mut Screen, command: &'static [u8]) -> Result<(), GraphicsError> {
        let mut command = parse_raw(command).unwrap();
        command.payload.clear();
        screen.handle_graphics_command(command).map(|_| ())
    }

    fn transmit(screen: &mut Screen, id: u32) {
        let mut command = parse_raw(format!("a=t,f=32,s=1,v=1,i={id}").as_bytes()).unwrap();
        command.payload = vec![1, 2, 3, 255];
        screen.handle_graphics_command(command).unwrap();
    }

    #[test]
    fn relative_parent_chains_are_validated_and_deleted_as_a_cascade() {
        let mut screen = Screen::builder(8, 4).scrollback(4).build().unwrap();
        transmit(&mut screen, 1);
        transmit(&mut screen, 2);
        put(&mut screen, b"a=p,i=1,p=1,c=1,r=1,C=1").unwrap();
        put(&mut screen, b"a=p,i=2,p=1,P=1,Q=1,c=1,r=1,C=1").unwrap();
        assert_eq!(screen.placements().len(), 2);
        assert!(screen
            .placement_graph()
            .validate(PlacementKey::new(2, 1))
            .is_ok());

        assert!(put(&mut screen, b"a=p,i=2,p=1,P=2,Q=1,c=1,r=1,C=1").is_err());
        assert!(put(&mut screen, b"a=p,i=1,p=1,P=1,Q=1,c=1,r=1,C=1").is_err());
        assert!(put(&mut screen, b"a=p,i=2,p=2,P=99,Q=1,c=1,r=1,C=1").is_err());

        put(&mut screen, b"a=d,d=p,i=1,p=1").unwrap();
        assert!(screen.placement(1, 1).is_none());
        assert!(screen.placement(2, 1).is_none(), "child must be cascaded");
    }

    #[test]
    fn virtual_placements_materialise_one_entry_per_placeholder_cell() {
        let mut screen = Screen::builder(8, 4).scrollback(4).build().unwrap();
        transmit(&mut screen, 7);
        put(&mut screen, b"a=p,i=7,p=3,U=1,c=2,r=1,C=1").unwrap();
        screen.cursor.sgr.fg = (7 << 8) | 2;
        screen.draw_text(&[IMAGE_PLACEHOLDER_CHAR]);
        screen.draw_text(&[IMAGE_PLACEHOLDER_CHAR]);
        let cells = screen.materialize_virtual_placements();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].screen_x, 0);
        assert_eq!(cells[1].screen_x, 1);
        assert_eq!(cells[0].screen_y, 0);
        assert!(cells.iter().all(|cell| cell.image_id == 7));
    }

    #[test]
    fn anonymous_placements_are_distinct_and_cascade_by_instance() {
        let mut screen = Screen::builder(8, 4).scrollback(4).build().unwrap();
        transmit(&mut screen, 1);
        transmit(&mut screen, 2);
        screen.cursor_position(1, 1);
        put(&mut screen, b"a=p,i=1,c=1,r=1,C=1").unwrap();
        screen.cursor_position(3, 4);
        put(&mut screen, b"a=p,i=1,c=1,r=1,C=1").unwrap();
        assert_eq!(screen.placements().len(), 2, "p=0 must not replace");

        let first = screen.placements()[0].clone();
        let second = screen.placements()[1].clone();
        assert_eq!((first.graph_id, second.graph_id), (1, 2));
        assert_ne!(first.graph_key(), second.graph_key());
        let graph = screen.placement_graph();
        assert_eq!(
            (
                graph.resolve(first.graph_key()).unwrap().anchor_x,
                graph.resolve(first.graph_key()).unwrap().anchor_y
            ),
            (0, 0)
        );
        assert_eq!(
            (
                graph.resolve(second.graph_key()).unwrap().anchor_x,
                graph.resolve(second.graph_key()).unwrap().anchor_y
            ),
            (3, 2)
        );

        // Q=0 names the first anonymous placement only.
        screen.cursor_position(6, 4);
        put(&mut screen, b"a=p,i=2,p=1,P=1,Q=0,c=1,r=1,C=1").unwrap();
        let child = screen.placement(2, 1).unwrap().clone();
        let graph = screen.placement_graph();
        assert_eq!(
            (
                graph.resolve(child.graph_key()).unwrap().anchor_x,
                graph.resolve(child.graph_key()).unwrap().anchor_y
            ),
            (0, 0)
        );

        screen.cursor_position(3, 4);
        put(&mut screen, b"a=d,d=c").unwrap();
        assert!(screen.placement(2, 1).is_some(), "child belongs to first");
        assert_eq!(screen.placements().len(), 2);

        screen.cursor_position(1, 1);
        put(&mut screen, b"a=d,d=c").unwrap();
        assert!(screen.placement(2, 1).is_none(), "child must cascade");
        assert!(screen.placements().is_empty());
    }

    #[test]
    fn unsupported_actions_are_recorded() {
        let mut s = Screen::builder(4, 2).scrollback(4).build().unwrap();
        let cmd = parse_raw(b"a=f,i=9").unwrap();
        s.handle_graphics_command(cmd).unwrap();
        assert_eq!(s.graphics_unsupported.len(), 1);
        assert_eq!(s.graphics_unsupported[0].image_id, 9);
    }

    #[test]
    fn delete_by_number_and_placement_id() {
        let mut s = Screen::builder(4, 2).scrollback(4).build().unwrap();
        let mut a = parse_raw(b"a=t,f=32,s=1,v=1,i=5,I=42").unwrap();
        a.payload = vec![1, 2, 3, 255];
        s.handle_graphics_command(a).unwrap();
        let mut p = parse_raw(b"a=p,i=5,p=9").unwrap();
        p.payload.clear();
        s.handle_graphics_command(p).unwrap();
        let mut p2 = parse_raw(b"a=p,i=5,p=10").unwrap();
        p2.payload.clear();
        s.handle_graphics_command(p2).unwrap();
        let del = parse_raw(b"a=d,d=p,i=5,p=9").unwrap();
        s.handle_graphics_command(del).unwrap();
        assert!(s.placement(5, 9).is_none());
        assert!(s.placement(5, 10).is_some());
        let del = parse_raw(b"a=d,d=n,I=42").unwrap();
        s.handle_graphics_command(del).unwrap();
        assert!(s.image(5).is_none());
    }
}
