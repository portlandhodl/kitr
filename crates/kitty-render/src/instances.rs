// SPDX-License-Identifier: GPL-3.0-or-later
//! Conversion of visible screen cells into backend-independent instances.
//!
//! The model follows the vertex data consumed by `kitty/shaders/cell.slang`:
//!
//! * one background quad per non-blank or specially coloured cell;
//! * one foreground glyph reference per non-blank cell;
//! * rectangles for underlines, strikethroughs and other decorations;
//! * a cursor instance (block, beam, underline or hollow);
//! * reverse/dim/blink/style flags, leaving the actual blending to the
//!   backend.
//!
//! Wide cells are emitted once, with a rectangle spanning both grid columns.
//! Continuation cells (`CPUCell::x() != 0`) are skipped, as are blank cells
//! with the default background.

use kitty_core::cell::CharType;
use kitty_fonts::FontCellMetrics;
use kitty_screen::{CursorShape, Screen};

use crate::atlas::{AtlasEvent, GlyphAtlas, GlyphKey, GlyphRasterizer, GlyphSlot, GlyphText};
use crate::colors::{ColorResolver, Rgba8};
use crate::damage::Damage;
use crate::decorations::{self, UnderlineStyle};
use crate::geom::{CellMetrics, GridGeometry, Rect};

/// Style flags attached to each instance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstanceFlags {
    pub bold: bool,
    pub italic: bool,
    pub reverse: bool,
    pub dim: bool,
    pub blink: bool,
    pub strikethrough: bool,
    pub underline: UnderlineStyle,
    pub wide: bool,
}

impl InstanceFlags {
    #[inline]
    fn from_cell(attrs: kitty_core::CellAttrs, width_cells: u32) -> Self {
        Self {
            bold: attrs.bold(),
            italic: attrs.italic(),
            reverse: attrs.reverse(),
            dim: attrs.dim(),
            blink: attrs.blink(),
            strikethrough: attrs.strike(),
            underline: UnderlineStyle::from_decoration(attrs.decoration()),
            wide: width_cells > 1,
        }
    }
}

/// Flags packed into [`GlyphKey::flags`].
pub const GLYPH_FLAG_BOLD: u32 = 1 << 0;
pub const GLYPH_FLAG_ITALIC: u32 = 1 << 1;
pub const GLYPH_FLAG_COLOR: u32 = 1 << 2;

fn glyph_flags(attrs: kitty_core::CellAttrs) -> u32 {
    let mut flags = 0;
    if attrs.bold() {
        flags |= GLYPH_FLAG_BOLD;
    }
    if attrs.italic() {
        flags |= GLYPH_FLAG_ITALIC;
    }
    flags
}

/// A filled background cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BackgroundInstance {
    pub rect: Rect,
    pub cell_x: u32,
    pub cell_y: u32,
    pub color: Rgba8,
    pub is_default_bg: bool,
    pub flags: InstanceFlags,
}

/// A foreground glyph and its atlas location.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphInstance {
    pub rect: Rect,
    pub cell_x: u32,
    pub cell_y: u32,
    pub fg: Rgba8,
    pub sprite: Option<GlyphSlot>,
    pub text: GlyphText,
    pub flags: InstanceFlags,
}

/// The kind of a decoration rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecorationKind {
    Underline(UnderlineStyle),
    Strikethrough,
}

/// A decoration rectangle in device pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecorationInstance {
    pub rect: Rect,
    pub cell_x: u32,
    pub cell_y: u32,
    pub kind: DecorationKind,
    pub color: Rgba8,
    pub flags: InstanceFlags,
}

/// Cursor geometry derived from a [`CursorInstance`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CursorGeometry {
    Block(Rect),
    Beam(Rect),
    Underline(Rect),
    Hollow([Rect; 4]),
}

/// A cursor to draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorInstance {
    pub cell_x: u32,
    pub cell_y: u32,
    pub cell_rect: Rect,
    pub shape: CursorShape,
    pub color: Rgba8,
    pub text_color: Rgba8,
    pub beam_thickness: f32,
    pub underline_thickness: f32,
    pub hollow_thickness: f32,
    pub flags: InstanceFlags,
}

impl CursorInstance {
    /// Expand the cursor into rectangles.
    pub fn geometry(&self) -> CursorGeometry {
        let r = self.cell_rect;
        match self.shape {
            CursorShape::Block | CursorShape::NoCursor => CursorGeometry::Block(r),
            CursorShape::Beam => {
                let w = self.beam_thickness.clamp(1.0, r.width.max(1.0));
                CursorGeometry::Beam(Rect::new(r.x, r.y, w, r.height))
            }
            CursorShape::Underline => {
                let h = self.underline_thickness.clamp(1.0, r.height.max(1.0));
                CursorGeometry::Underline(Rect::new(r.x, r.bottom() - h, r.width, h))
            }
            CursorShape::Hollow => {
                let t = self
                    .hollow_thickness
                    .clamp(1.0, r.width.min(r.height).max(1.0));
                CursorGeometry::Hollow([
                    Rect::new(r.x, r.y, t, r.height),
                    Rect::new(r.right() - t, r.y, t, r.height),
                    Rect::new(r.x, r.y, r.width, t),
                    Rect::new(r.x, r.bottom() - t, r.width, t),
                ])
            }
        }
    }
}

/// Per-frame configuration for instance generation.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderConfig {
    pub metrics: CellMetrics,
    pub origin_x: f32,
    pub origin_y: f32,
    /// Font id stored in glyph keys.
    pub font_id: u16,
    pub cursor_beam_thickness: f32,
    pub cursor_underline_thickness: f32,
    pub cursor_hollow_thickness: f32,
    /// Bit 0: tighter undercurl, bit 1: thicker undercurl.
    pub undercurl_style: u8,
    /// Emit a background quad even for blank cells with the default
    /// background. Normally only the clear colour handles those.
    pub emit_default_backgrounds: bool,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            metrics: CellMetrics::default(),
            origin_x: 0.0,
            origin_y: 0.0,
            font_id: 0,
            cursor_beam_thickness: 2.0,
            cursor_underline_thickness: 2.0,
            cursor_hollow_thickness: 1.0,
            undercurl_style: 0,
            emit_default_backgrounds: false,
        }
    }
}

impl RenderConfig {
    #[inline]
    pub fn font_metrics(&self) -> FontCellMetrics {
        self.metrics.into()
    }

    #[inline]
    pub fn grid_for(&self, screen: &Screen) -> GridGeometry {
        GridGeometry::new(screen.columns, screen.lines, self.metrics)
            .with_origin(self.origin_x, self.origin_y)
    }
}

/// All instances for one frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub damage: Damage,
    pub geometry: GridGeometry,
    pub default_fg: Rgba8,
    pub default_bg: Rgba8,
    pub backgrounds: Vec<BackgroundInstance>,
    pub glyphs: Vec<GlyphInstance>,
    pub decorations: Vec<DecorationInstance>,
    pub cursors: Vec<CursorInstance>,
    /// Glyphs that were rasterized and inserted into the atlas this frame.
    /// A backend uses these slots to upload just the newly touched pages.
    pub atlas_updates: Vec<GlyphSlot>,
    /// Page creations/evictions that happened while generating this frame.
    pub atlas_events: Vec<AtlasEvent>,
}

impl Frame {
    /// Clear all instance vectors while retaining their allocations.
    pub fn clear_instances(&mut self) {
        self.backgrounds.clear();
        self.glyphs.clear();
        self.decorations.clear();
        self.cursors.clear();
        self.atlas_updates.clear();
        self.atlas_events.clear();
    }

    /// Total number of instances in the frame.
    pub fn instance_count(&self) -> usize {
        self.backgrounds.len() + self.glyphs.len() + self.decorations.len() + self.cursors.len()
    }
}

/// Builds a [`Frame`] from a screen.
#[derive(Clone, Debug, Default)]
pub struct InstanceGenerator {
    pub config: RenderConfig,
}

impl InstanceGenerator {
    pub fn new(config: RenderConfig) -> Self {
        Self { config }
    }

    /// Generate instances into `frame`, clearing its instance vectors first.
    pub fn generate(
        &self,
        screen: &Screen,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
        resolver: &dyn ColorResolver,
        frame: &mut Frame,
    ) {
        frame.clear_instances();
        frame.geometry = self.config.grid_for(screen);
        frame.default_fg = resolver.default_fg();
        frame.default_bg = resolver.default_bg();

        let default_fg = frame.default_fg;
        let default_bg = frame.default_bg;
        let cursor_visible =
            screen.is_cursor_visible() && screen.cursor.shape != CursorShape::NoCursor;
        let cursor_x = screen.cursor.x;
        let cursor_y = screen.cursor.y;
        let cursor_shape = screen.cursor.shape;
        let metrics = self.config.font_metrics();

        let tc = screen.text_cache().borrow();
        for y in 0..screen.lines {
            for x in 0..screen.columns {
                let cpu = screen.cpu_cell(x, y);
                // Continuation cells of a wide/multicell character are covered
                // by the instance for the first cell.
                if cpu.is_multicell() && (cpu.x() != 0 || cpu.y() != 0) {
                    continue;
                }
                let single;
                let text: &[CharType] = if cpu.ch_is_idx() {
                    tc.chars_at(cpu.ch())
                } else if cpu.ch() != 0 {
                    single = [cpu.ch()];
                    &single
                } else {
                    &[]
                };
                let blank = text.is_empty() || text.iter().all(|&c| c == 0 || c == b' ' as u32);

                let width_cells = if cpu.is_multicell() && cpu.width() > 0 {
                    cpu.mcd_x_limit().max(1).min(screen.columns - x)
                } else {
                    1
                };
                let gpu = screen.gpu_cell(x, y);
                let attrs = gpu.attrs;
                let flags = InstanceFlags::from_cell(attrs, width_cells);

                let mut fg = resolver.resolve(gpu.fg, default_fg);
                let mut bg = resolver.resolve(gpu.bg, default_bg);
                if attrs.reverse() {
                    core::mem::swap(&mut fg, &mut bg);
                }

                let cursor_here = cursor_visible && cursor_x == x && cursor_y == y;
                let block_cursor = cursor_here && cursor_shape == CursorShape::Block;
                if block_cursor {
                    bg = resolver.cursor_bg();
                    fg = resolver.cursor_fg();
                }

                let decoration_color = if block_cursor || gpu.decoration_fg == 0 {
                    fg
                } else {
                    resolver.resolve(gpu.decoration_fg, fg)
                };

                let cell_rect = frame.geometry.cell_rect_with_width(x, y, width_cells);
                let emit_background = self.config.emit_default_backgrounds
                    || !blank
                    || bg != default_bg
                    || cursor_here
                    || attrs.reverse();
                if emit_background {
                    frame.backgrounds.push(BackgroundInstance {
                        rect: cell_rect,
                        cell_x: x,
                        cell_y: y,
                        color: bg,
                        is_default_bg: bg == default_bg && !cursor_here,
                        flags,
                    });
                }

                if !blank {
                    let key =
                        GlyphKey::from_codepoints(self.config.font_id, text, glyph_flags(attrs));
                    let sprite = match atlas.lookup(&key).copied() {
                        Some(slot) => Some(slot),
                        None => match rasterizer.rasterize(&key) {
                            Some(bitmap) => match atlas.insert(key, bitmap) {
                                Ok(inserted) => {
                                    if let Some(event) = inserted.event {
                                        frame.atlas_events.push(event);
                                    }
                                    if !inserted.already_present {
                                        frame.atlas_updates.push(inserted.slot);
                                    }
                                    Some(inserted.slot)
                                }
                                Err(_) => None,
                            },
                            None => None,
                        },
                    };
                    frame.glyphs.push(GlyphInstance {
                        rect: cell_rect,
                        cell_x: x,
                        cell_y: y,
                        fg,
                        sprite,
                        text: key.text,
                        flags,
                    });
                }

                let cell_scale = width_cells as f32;
                if !flags.underline.is_none() {
                    for rect in decorations::underline_rects(
                        flags.underline,
                        metrics,
                        self.config.undercurl_style,
                    ) {
                        let rect = Rect::new(
                            rect.x * cell_scale,
                            rect.y,
                            rect.width * cell_scale,
                            rect.height,
                        )
                        .translated(cell_rect.x, cell_rect.y);
                        frame.decorations.push(DecorationInstance {
                            rect,
                            cell_x: x,
                            cell_y: y,
                            kind: DecorationKind::Underline(flags.underline),
                            color: decoration_color,
                            flags,
                        });
                    }
                }
                if flags.strikethrough {
                    for rect in decorations::strikethrough_rects(metrics) {
                        let rect = Rect::new(
                            rect.x * cell_scale,
                            rect.y,
                            rect.width * cell_scale,
                            rect.height,
                        )
                        .translated(cell_rect.x, cell_rect.y);
                        frame.decorations.push(DecorationInstance {
                            rect,
                            cell_x: x,
                            cell_y: y,
                            kind: DecorationKind::Strikethrough,
                            color: decoration_color,
                            flags,
                        });
                    }
                }

                if cursor_here {
                    frame.cursors.push(CursorInstance {
                        cell_x: x,
                        cell_y: y,
                        cell_rect,
                        shape: cursor_shape,
                        color: resolver.cursor_bg(),
                        text_color: fg,
                        beam_thickness: self.config.cursor_beam_thickness,
                        underline_thickness: self.config.cursor_underline_thickness,
                        hollow_thickness: self.config.cursor_hollow_thickness,
                        flags,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::{AtlasConfig, GlyphBitmap};
    use crate::colors::BasicColorResolver;

    fn screen(columns: u32, lines: u32) -> Screen {
        Screen::builder(columns, lines)
            .scrollback(8)
            .build()
            .unwrap()
    }

    fn draw(screen: &mut Screen, text: &str) {
        let chars: Vec<u32> = text.chars().map(|c| c as u32).collect();
        screen.draw_text(&chars);
    }

    fn rasterizer() -> impl FnMut(&GlyphKey) -> Option<GlyphBitmap> {
        |key: &GlyphKey| {
            let w = if key.text.as_slice().first().copied().unwrap_or(0) > 0x2e80 {
                8
            } else {
                6
            };
            GlyphBitmap::gray(w, 10, vec![0x80; (w * 10) as usize]).ok()
        }
    }

    fn generator() -> InstanceGenerator {
        InstanceGenerator::new(RenderConfig::default())
    }

    fn resolver() -> BasicColorResolver {
        BasicColorResolver::new(Rgba8::rgb(1, 2, 3), Rgba8::rgb(4, 5, 6))
    }

    #[test]
    fn text_cells_make_background_and_glyph() {
        let mut s = screen(10, 3);
        draw(&mut s, "hi");
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut r = rasterizer();
        let res = resolver();
        let mut frame = Frame::default();
        generator().generate(&s, &mut atlas, &mut r, &res, &mut frame);
        assert_eq!(frame.glyphs.len(), 2);
        assert_eq!(frame.backgrounds.len(), 2);
        assert_eq!(frame.glyphs[0].text.as_slice(), &[b'h' as u32]);
        assert_eq!(frame.glyphs[1].text.as_slice(), &[b'i' as u32]);
        assert_eq!(frame.glyphs[0].fg, Rgba8::rgb(1, 2, 3));
        assert_eq!(frame.backgrounds[0].color, Rgba8::rgb(4, 5, 6));
        assert!(frame.glyphs.iter().all(|g| g.sprite.is_some()));
        assert!(frame.cursors.is_empty());
        assert_eq!(atlas.len(), 2);
    }

    #[test]
    fn blank_cells_are_skipped_but_default_bg_can_be_forced() {
        let mut s = screen(4, 1);
        draw(&mut s, "a b");
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut r = rasterizer();
        let res = resolver();
        let mut frame = Frame::default();
        generator().generate(&s, &mut atlas, &mut r, &res, &mut frame);
        assert_eq!(frame.glyphs.len(), 2, "space is blank");
        assert_eq!(frame.backgrounds.len(), 2);

        let cfg = RenderConfig {
            emit_default_backgrounds: true,
            ..RenderConfig::default()
        };
        let gen = InstanceGenerator::new(cfg);
        let mut frame = Frame::default();
        gen.generate(&s, &mut atlas, &mut r, &res, &mut frame);
        assert_eq!(frame.backgrounds.len(), 4);
        assert_eq!(frame.glyphs.len(), 2);
    }

    #[test]
    fn sgr_colors_and_attributes() {
        let mut s = screen(10, 2);
        s.select_graphic_rendition(&[31, 44, 1, 4, 9], false);
        draw(&mut s, "x");
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut r = rasterizer();
        let res = resolver();
        let mut frame = Frame::default();
        generator().generate(&s, &mut atlas, &mut r, &res, &mut frame);
        let g = frame.glyphs[0];
        assert_eq!(g.fg, res.palette()[1]);
        assert_eq!(frame.backgrounds[0].color, res.palette()[4]);
        assert!(g.flags.bold);
        assert_eq!(g.flags.underline, UnderlineStyle::Straight);
        assert!(g.flags.strikethrough);
        assert!(!g.flags.reverse);
        assert_eq!(frame.decorations.len(), 2);
        assert!(frame
            .decorations
            .iter()
            .any(|d| d.kind == DecorationKind::Strikethrough));
    }

    #[test]
    fn reverse_video_swaps_colors() {
        let mut s = screen(10, 2);
        s.select_graphic_rendition(&[7], false);
        draw(&mut s, "x");
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut r = rasterizer();
        let res = resolver();
        let mut frame = Frame::default();
        generator().generate(&s, &mut atlas, &mut r, &res, &mut frame);
        assert!(frame.glyphs[0].flags.reverse);
        assert_eq!(frame.glyphs[0].fg, res.default_bg());
        assert_eq!(frame.backgrounds[0].color, res.default_fg());
    }

    #[test]
    fn dim_and_blink_flags() {
        let mut s = screen(10, 2);
        s.select_graphic_rendition(&[2, 5], false);
        draw(&mut s, "x");
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut r = rasterizer();
        let res = resolver();
        let mut frame = Frame::default();
        generator().generate(&s, &mut atlas, &mut r, &res, &mut frame);
        assert!(frame.glyphs[0].flags.dim);
        assert!(frame.glyphs[0].flags.blink);
    }

    #[test]
    fn wide_char_is_one_instance_with_two_columns() {
        let mut s = screen(10, 2);
        draw(&mut s, "你");
        assert_eq!(s.cpu_cell(0, 0).width(), 2);
        assert_eq!(s.cpu_cell(1, 0).x(), 1);
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut r = rasterizer();
        let res = resolver();
        let mut frame = Frame::default();
        generator().generate(&s, &mut atlas, &mut r, &res, &mut frame);
        assert_eq!(frame.glyphs.len(), 1);
        assert_eq!(frame.backgrounds.len(), 1);
        assert_eq!(frame.glyphs[0].rect.width, 20.0);
        assert!(frame.glyphs[0].flags.wide);
    }

    #[test]
    fn cursors_shapes_and_colors() {
        for shape in [
            CursorShape::Block,
            CursorShape::Beam,
            CursorShape::Underline,
            CursorShape::Hollow,
        ] {
            let mut s = screen(10, 2);
            s.cursor.shape = shape;
            draw(&mut s, "x");
            s.cursor_position(1, 1);
            let mut atlas = GlyphAtlas::new(AtlasConfig::default());
            let mut r = rasterizer();
            let res = resolver();
            let mut frame = Frame::default();
            generator().generate(&s, &mut atlas, &mut r, &res, &mut frame);
            assert_eq!(frame.cursors.len(), 1);
            assert_eq!(frame.cursors[0].shape, shape);
            match shape {
                CursorShape::Block => {
                    assert_eq!(frame.cursors[0].color, res.cursor_bg());
                    assert_eq!(frame.backgrounds[0].color, res.cursor_bg());
                    assert_eq!(frame.glyphs[0].fg, res.cursor_fg());
                    assert!(matches!(
                        frame.cursors[0].geometry(),
                        CursorGeometry::Block(_)
                    ));
                }
                CursorShape::Beam => {
                    assert!(matches!(
                        frame.cursors[0].geometry(),
                        CursorGeometry::Beam(r) if r.width == 2.0
                    ));
                }
                CursorShape::Underline => {
                    assert!(matches!(
                        frame.cursors[0].geometry(),
                        CursorGeometry::Underline(r) if r.height == 2.0
                    ));
                }
                CursorShape::Hollow => {
                    assert!(matches!(
                        frame.cursors[0].geometry(),
                        CursorGeometry::Hollow(_)
                    ));
                }
                CursorShape::NoCursor => {}
            }
        }
    }

    #[test]
    fn atlas_is_reused_across_frames() {
        let mut s = screen(10, 2);
        draw(&mut s, "aa");
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut r = rasterizer();
        let res = resolver();
        let gen = generator();
        let mut frame = Frame::default();
        gen.generate(&s, &mut atlas, &mut r, &res, &mut frame);
        assert_eq!(atlas.len(), 1);
        let cap = frame.glyphs.capacity();
        gen.generate(&s, &mut atlas, &mut r, &res, &mut frame);
        assert_eq!(atlas.len(), 1);
        assert_eq!(frame.glyphs.capacity(), cap);
    }
}
