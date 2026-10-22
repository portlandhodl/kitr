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

use std::collections::HashMap;

use kitty_graphics::{
    parse_escape, Action, DeleteAction, GraphicsCommand, GraphicsError, GraphicsSink, Image,
    LoadOutcome, Placement,
};

use crate::screen::Screen;

/// A stored placement. The image itself lives in [`Screen::images`].
#[derive(Clone, Debug, PartialEq)]
pub struct ImagePlacement {
    pub image_id: u32,
    pub placement_id: u32,
    pub image_number: u32,
    pub placement: Placement,
    /// Screen cell where the placement was created (used by `d=c`/`d=C`).
    pub anchor_x: u32,
    pub anchor_y: u32,
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

    fn remove_placements(&mut self, predicate: impl Fn(&ImagePlacement) -> bool) {
        self.placements.retain(|p| !predicate(p));
    }

    fn remove_images(&mut self, predicate: impl Fn(&Image) -> bool) {
        let removed: Vec<u32> = self
            .images
            .iter()
            .filter(|(_, img)| predicate(img))
            .map(|(id, _)| *id)
            .collect();
        for id in removed {
            self.images.remove(&id);
            self.placements.retain(|p| p.image_id != id);
        }
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
        let placement_anchor_x = self.cursor.x.min(self.columns.saturating_sub(1));
        let placement_anchor_y = self.cursor.y.min(self.lines.saturating_sub(1));
        let entry = ImagePlacement {
            image_id,
            placement_id: placement.placement_id,
            image_number: placement.image_number,
            placement: placement.clone(),
            anchor_x: placement_anchor_x,
            anchor_y: placement_anchor_y,
        };
        if let Some(existing) = self
            .placements
            .iter_mut()
            .find(|p| p.image_id == entry.image_id && p.placement_id == entry.placement_id)
        {
            *existing = entry;
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
        }
        Ok(())
    }

    fn on_delete(&mut self, command: &GraphicsCommand) -> Result<(), GraphicsError> {
        let action = command.delete_action.unwrap_or(DeleteAction::All);
        match action {
            DeleteAction::All => {
                self.images.clear();
                self.placements.clear();
                self.image_assembler.clear_pending();
            }
            DeleteAction::AllButId => {
                if command.id != 0 {
                    self.placements.retain(|p| p.image_id == command.id);
                    self.images.retain(|id, _| *id == command.id);
                }
            }
            DeleteAction::ById | DeleteAction::ByIdNoRefs => {
                self.images.remove(&command.id);
                self.placements.retain(|p| p.image_id != command.id);
            }
            DeleteAction::ByNumber | DeleteAction::ByNumberNoRefs => {
                self.remove_images(|img| img.number == command.image_number);
            }
            DeleteAction::PlacementId => {
                let pid = command.placement_id;
                let iid = command.id;
                self.remove_placements(|p| {
                    p.placement_id == pid && (iid == 0 || p.image_id == iid)
                });
            }
            DeleteAction::AtCursor | DeleteAction::AtCursorPoint => {
                let (x, y) = (self.cursor.x, self.cursor.y);
                self.remove_placements(|p| p.anchor_x == x && p.anchor_y == y);
            }
            DeleteAction::AtCell | DeleteAction::AtPoint | DeleteAction::AtPoint3d => {
                self.remove_placements(|_| true);
            }
            DeleteAction::ZIndex | DeleteAction::ZIndexUpper => {
                let z = command.z_index;
                self.remove_placements(|p| p.placement.z_index == z);
            }
            _ => {
                // Animation/cell-range variants are not executed by the loader.
                self.remove_placements(|_| true);
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
    use kitty_graphics::parse_raw;

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
