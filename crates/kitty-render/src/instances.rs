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

use std::cell::RefCell;

use kitty_core::cell::IMAGE_PLACEHOLDER_CHAR;
use kitty_core::cell::{CpuCell, GpuCell};
use kitty_fonts::FontCellMetrics;
use kitty_graphics::{AnimationManager, AnimationTick, IndexedPalette};
use kitty_screen::{CursorShape, HyperlinkSpan, Screen};

use crate::atlas::{AtlasEvent, GlyphAtlas, GlyphKey, GlyphRasterizer, GlyphSlot, GlyphText};
use crate::colors::{ColorResolver, Rgba8};
use crate::damage::Damage;
use crate::decorations::{self, UnderlineStyle};
use crate::geom::{CellMetrics, GridGeometry, Rect};
use crate::images::{
    self, BackgroundImageConfig, ImageFilter, ImageInstance, ImageUpload, WindowLogoConfig,
};
use crate::overlays::{
    interval_contains, selection_intervals, HyperlinkHover, OverlayStats, SelectionCell,
    SelectionRange, TabBarFrame, TabBarSpec, TabBarTab,
};
use crate::shaped::{
    shaped_run_hash, ShapedRunCluster, ShapedRunOutput, ShapedRunRequest, SHAPED_RUN_MAX_CELLS,
};
use crate::trail::{CursorTrailConfig, TrailInstance};

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
    /// Dotted underline drawn below an OSC 8 hyperlink.
    Hyperlink,
    /// A tab-bar border segment.
    TabBarBorder,
    /// A split-pane/window border or background gap.
    PaneBorder,
}

/// A solid rectangle supplied by the host for window chrome (pane borders and
/// the background gaps between splits).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SolidRect {
    pub rect: Rect,
    pub color: Rgba8,
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
    /// Drawable size in pixels used for background images and the window logo.
    /// Zero means "use the grid extent plus its origin".
    pub viewport_width: f32,
    pub viewport_height: f32,
    /// Filtering used for graphics-protocol placements.
    pub image_filter: ImageFilter,
    /// Optional background image.
    pub background_image: Option<BackgroundImageConfig>,
    /// Optional window logo.
    pub window_logo: Option<WindowLogoConfig>,
    /// Cursor trail settings.
    pub cursor_trail: CursorTrailConfig,
    /// Selection ranges to tint. Coordinates use range rows, just like
    /// `kitty_screen::Selection`; the renderer adds `Screen::scrolled_by`.
    pub selections: Vec<SelectionRange>,
    /// Override the resolver's selection colours.
    pub selection_bg: Option<Rgba8>,
    pub selection_fg: Option<Rgba8>,
    /// Draw a dotted underline below OSC 8 hyperlinks.
    pub hyperlink_underline: bool,
    /// Override the resolver's URL colour for hyperlink underlines.
    pub hyperlink_color: Option<Rgba8>,
    /// Highlight the link under this visual cell.
    pub hyperlink_hover: Option<HyperlinkHover>,
    /// Override the hover highlight background.
    pub hyperlink_hover_bg: Option<Rgba8>,
    /// Optional tab bar at the top of the viewport.
    pub tab_bar: Option<TabBarSpec>,
    /// Solid rectangles painted below cell backgrounds, used for split-pane
    /// background gaps.
    pub pane_backgrounds: Vec<SolidRect>,
    /// Solid rectangles painted as pane/split borders.
    pub pane_borders: Vec<SolidRect>,
    /// Try to shape runs of adjacent cells into ligature/combining sprites.
    pub shaped_runs: bool,
    /// Root directory for the disk-backed animation frame cache. `None` keeps
    /// frames in memory. The manager owns and removes a private subdirectory.
    pub animation_cache_dir: Option<std::path::PathBuf>,
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
            viewport_width: 0.0,
            viewport_height: 0.0,
            image_filter: ImageFilter::Nearest,
            background_image: None,
            window_logo: None,
            cursor_trail: CursorTrailConfig::default(),
            selections: Vec::new(),
            selection_bg: None,
            selection_fg: None,
            hyperlink_underline: true,
            hyperlink_color: None,
            hyperlink_hover: None,
            hyperlink_hover_bg: None,
            tab_bar: None,
            pane_backgrounds: Vec::new(),
            pane_borders: Vec::new(),
            shaped_runs: true,
            animation_cache_dir: None,
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

    /// Drawable size used for background images and logos.
    #[inline]
    pub fn viewport_for(&self, grid: &GridGeometry) -> (f32, f32) {
        let width = if self.viewport_width > 0.0 {
            self.viewport_width
        } else {
            grid.origin_x + grid.width_px()
        };
        let height = if self.viewport_height > 0.0 {
            self.viewport_height
        } else {
            grid.origin_y + grid.height_px()
        };
        (width.max(0.0), height.max(0.0))
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
    /// Graphics-protocol image placements, sorted by z-index draw order.
    pub images: Vec<ImageInstance>,
    /// Texture data for images referenced by this frame.
    pub image_uploads: Vec<ImageUpload>,
    /// Live palette for indexed protocol images. The GL backend uploads the
    /// palette indices once and looks colours up in this palette in the
    /// fragment shader, so colour changes do not re-upload image textures.
    pub indexed_palette: Option<IndexedPalette>,
    /// Optional background image quad (also present in `image_uploads`).
    pub background_image: Option<ImageInstance>,
    /// Optional window logo quad (also present in `image_uploads`).
    pub logo: Option<ImageInstance>,
    /// Fading cursor-trail quads, oldest first.
    pub trail: Vec<TrailInstance>,
    /// Glyphs that were rasterized and inserted into the atlas this frame.
    /// A backend uses these slots to upload just the newly touched pages.
    pub atlas_updates: Vec<GlyphSlot>,
    /// Page creations/evictions that happened while generating this frame.
    pub atlas_events: Vec<AtlasEvent>,
    /// Selection ranges used for this frame (range-row coordinates).
    pub selections: Vec<SelectionRange>,
    /// Visible selected cells after applying the scrollback view offset.
    pub selection_cells: Vec<SelectionCell>,
    /// Visible contiguous hyperlink runs.
    pub hyperlink_spans: Vec<HyperlinkSpan>,
    /// Resolved tab-bar geometry, when one was requested.
    pub tab_bar: Option<TabBarFrame>,
    /// Counters for overlay and shaped-run work.
    pub overlay_stats: OverlayStats,
    /// When true the backend must not clear the scissor rectangles before
    /// drawing. Used for transparent chrome overlays drawn over pane frames.
    pub skip_clear: bool,
}

impl Frame {
    /// Clear all instance vectors while retaining their allocations.
    pub fn clear_instances(&mut self) {
        self.backgrounds.clear();
        self.glyphs.clear();
        self.decorations.clear();
        self.cursors.clear();
        self.images.clear();
        self.image_uploads.clear();
        self.indexed_palette = None;
        self.background_image = None;
        self.logo = None;
        self.trail.clear();
        self.atlas_updates.clear();
        self.atlas_events.clear();
        self.selections.clear();
        self.selection_cells.clear();
        self.hyperlink_spans.clear();
        self.tab_bar = None;
        self.overlay_stats = OverlayStats::default();
        self.skip_clear = false;
    }

    /// Total number of instances in the frame.
    pub fn instance_count(&self) -> usize {
        self.backgrounds.len()
            + self.glyphs.len()
            + self.decorations.len()
            + self.cursors.len()
            + self.images.len()
            + usize::from(self.background_image.is_some())
            + usize::from(self.logo.is_some())
            + self.trail.len()
    }
}

/// Internal shaped cluster ready to be emitted by the cell loop.
struct ShapePlan {
    start: u32,
    span: u32,
    sprite: GlyphSlot,
    text: GlyphText,
}

/// Paint decision for one cell, shared by the normal and shaped paths.
struct CellPaint {
    flags: InstanceFlags,
    fg: Rgba8,
}

fn cell_text<'a>(
    tc: &'a kitty_core::TextCache,
    cpu: CpuCell,
    single: &'a mut [u32; 1],
) -> &'a [u32] {
    if cpu.ch_is_idx() {
        tc.chars_at(cpu.ch())
    } else if cpu.ch() != 0 {
        single[0] = cpu.ch();
        &single[..1]
    } else {
        &[]
    }
}

fn is_blank_text(text: &[u32]) -> bool {
    text.is_empty() || text.iter().all(|&c| c == 0 || c == b' ' as u32)
}

fn intern_glyph(
    frame: &mut Frame,
    atlas: &mut GlyphAtlas,
    rasterizer: &mut dyn GlyphRasterizer,
    key: GlyphKey,
) -> Option<GlyphSlot> {
    match atlas.lookup(&key).copied() {
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
    }
}

fn clip_rect(rect: Rect, bounds: Rect) -> Option<Rect> {
    let left = rect.x.max(bounds.x);
    let top = rect.y.max(bounds.y);
    let right = rect.right().min(bounds.right());
    let bottom = rect.bottom().min(bounds.bottom());
    (right > left && bottom > top).then(|| Rect::from_ltrb(left, top, right, bottom))
}

#[allow(clippy::too_many_arguments)]
fn emit_cell(
    config: &RenderConfig,
    resolver: &dyn ColorResolver,
    frame: &mut Frame,
    x: u32,
    y: u32,
    width_cells: u32,
    cpu: CpuCell,
    gpu: GpuCell,
    blank: bool,
    selected: bool,
    hovered: bool,
    cursor_here: bool,
    cursor_shape: CursorShape,
    metrics: FontCellMetrics,
    selection_bg: Rgba8,
    selection_fg: Rgba8,
    hover_bg: Rgba8,
    link_color: Rgba8,
) -> CellPaint {
    let attrs = gpu.attrs;
    let flags = InstanceFlags::from_cell(attrs, width_cells);

    let mut fg = resolver.resolve(gpu.fg, frame.default_fg);
    let mut bg = resolver.resolve(gpu.bg, frame.default_bg);
    if attrs.reverse() {
        core::mem::swap(&mut fg, &mut bg);
    }
    if selected {
        bg = selection_bg;
        fg = selection_fg;
    } else if hovered {
        bg = hover_bg;
    }

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
    let emit_background = config.emit_default_backgrounds
        || !blank
        || bg != frame.default_bg
        || cursor_here
        || attrs.reverse()
        || selected
        || hovered;
    if emit_background {
        frame.backgrounds.push(BackgroundInstance {
            rect: cell_rect,
            cell_x: x,
            cell_y: y,
            color: bg,
            is_default_bg: bg == frame.default_bg && !cursor_here && !selected && !hovered,
            flags,
        });
    }

    if selected {
        frame.selection_cells.push(SelectionCell {
            x,
            y,
            width_cells,
            rect: cell_rect,
        });
        frame.overlay_stats.selection_cells += 1;
    }

    let link_id = cpu.hyperlink_id();
    if link_id != 0 {
        frame.overlay_stats.hyperlink_cells += 1;
    }
    if hovered {
        frame.overlay_stats.hover_cells += 1;
    }
    if config.hyperlink_underline && link_id != 0 {
        let cell_scale = width_cells as f32;
        for rect in
            decorations::underline_rects(UnderlineStyle::Dotted, metrics, config.undercurl_style)
        {
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
                kind: DecorationKind::Hyperlink,
                color: link_color,
                flags,
            });
            frame.overlay_stats.hyperlink_decorations += 1;
        }
    }

    let cell_scale = width_cells as f32;
    if !flags.underline.is_none() {
        for rect in decorations::underline_rects(flags.underline, metrics, config.undercurl_style) {
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
            beam_thickness: config.cursor_beam_thickness,
            underline_thickness: config.cursor_underline_thickness,
            hollow_thickness: config.cursor_hollow_thickness,
            flags,
        });
    }

    CellPaint { flags, fg }
}

#[allow(clippy::too_many_arguments)]
fn emit_tab_bar(
    spec: &TabBarSpec,
    config: &RenderConfig,
    frame: &mut Frame,
    atlas: &mut GlyphAtlas,
    rasterizer: &mut dyn GlyphRasterizer,
    viewport_width: f32,
) {
    if spec.height <= 0.0 || viewport_width <= 0.0 {
        return;
    }
    let metrics = config.font_metrics();
    let cell_width = metrics.cell_width.max(1) as f32;
    let cell_height = metrics.cell_height as f32;
    let height = spec.height.max(cell_height).max(1.0);
    let bar = Rect::new(0.0, 0.0, viewport_width, height);

    frame.backgrounds.push(BackgroundInstance {
        rect: bar,
        cell_x: 0,
        cell_y: 0,
        color: spec.colors.background,
        is_default_bg: false,
        flags: InstanceFlags::default(),
    });

    let count = spec.titles.len();
    let width = spec.tab_width(viewport_width, count);
    let mut tabs = Vec::new();
    let mut visible_titles = Vec::new();
    for (index, title) in spec.titles.iter().enumerate() {
        let layout_rect = Rect::new(index as f32 * width, 0.0, width, height);
        if layout_rect.x >= viewport_width || width <= 0.0 {
            break;
        }
        let Some(rect) = clip_rect(layout_rect, bar) else {
            continue;
        };
        let active = index == spec.active;
        let background = if active {
            spec.colors.active_background
        } else {
            spec.colors.inactive_background
        };
        frame.backgrounds.push(BackgroundInstance {
            rect,
            cell_x: 0,
            cell_y: 0,
            color: background,
            is_default_bg: false,
            flags: InstanceFlags::default(),
        });

        let border_color = if active {
            spec.colors.active_border
        } else {
            spec.colors.border
        };
        let geometry =
            decorations::WindowGeometry::new(rect.x, rect.y, rect.right(), rect.bottom());
        let mut border_rects = Vec::new();
        decorations::add_borders(
            &mut border_rects,
            decorations::BorderColor::Active,
            &geometry,
            crate::geom::EdgeWidths::default(),
            spec.border_width.max(0.0),
            0,
            index as i64,
        );
        for border in border_rects {
            if let Some(rect) = clip_rect(border.rect(), bar) {
                frame.decorations.push(DecorationInstance {
                    rect,
                    cell_x: 0,
                    cell_y: 0,
                    kind: DecorationKind::TabBarBorder,
                    color: border_color,
                    flags: InstanceFlags::default(),
                });
            }
        }

        let fg = if active {
            spec.colors.active_foreground
        } else {
            spec.colors.foreground
        };
        let key_flags = if active { GLYPH_FLAG_BOLD } else { 0 };
        let flags = InstanceFlags {
            bold: active,
            ..InstanceFlags::default()
        };
        let chars: Vec<u32> = title.chars().map(|c| c as u32).collect();
        let text_width = chars.len() as f32 * cell_width;
        let start_x = rect.x + ((rect.width - text_width) / 2.0).max(0.0);
        let start_y = rect.y + ((rect.height - cell_height) / 2.0).max(0.0);
        let mut visible = String::new();
        for (offset, ch) in chars.iter().enumerate() {
            let glyph_rect = Rect::new(
                start_x + offset as f32 * cell_width,
                start_y,
                cell_width,
                cell_height,
            );
            if glyph_rect.x < rect.x || glyph_rect.right() > rect.right() {
                break;
            }
            if *ch == 0 || *ch == b' ' as u32 {
                visible.push(' ');
                continue;
            }
            let key = GlyphKey::from_codepoints(spec.font_id, &[*ch], key_flags);
            let sprite = intern_glyph(frame, atlas, rasterizer, key);
            frame.glyphs.push(GlyphInstance {
                rect: glyph_rect,
                cell_x: (glyph_rect.x / cell_width).max(0.0) as u32,
                cell_y: 0,
                fg,
                sprite,
                text: key.text,
                flags,
            });
            frame.overlay_stats.tab_glyphs += 1;
            if let Some(c) = char::from_u32(*ch) {
                visible.push(c);
            }
        }
        visible_titles.push(visible);
        tabs.push(TabBarTab {
            index,
            active,
            rect,
            layout_rect,
        });
    }
    frame.overlay_stats.tab_tabs = tabs.len();
    frame.tab_bar = Some(TabBarFrame {
        rect: bar,
        height,
        tabs,
        visible_titles,
    });
}

/// Builds a [`Frame`] from a screen.
#[derive(Clone, Debug, Default)]
pub struct InstanceGenerator {
    pub config: RenderConfig,
    /// Animation state used for timed frame swaps and placeholder images.
    pub animations: RefCell<AnimationManager>,
}

impl InstanceGenerator {
    pub fn new(config: RenderConfig) -> Self {
        let animations = match &config.animation_cache_dir {
            Some(dir) => AnimationManager::new_with_disk_cache(dir)
                .unwrap_or_else(|_| AnimationManager::new()),
            None => AnimationManager::new(),
        };
        Self {
            config,
            animations: RefCell::new(animations),
        }
    }

    /// Replace the animation manager.
    pub fn with_animations(mut self, animations: AnimationManager) -> Self {
        self.animations = RefCell::new(animations);
        self
    }

    /// Add screen images as animation roots when they are not tracked yet.
    pub fn ensure_animation_roots(&self, screen: &Screen) {
        let mut animations = self.animations.borrow_mut();
        for image in screen.images().values() {
            animations.ensure_root(image);
        }
    }

    /// Advance running animations and report which images need a redraw.
    pub fn tick_animations(&self, now_ms: u64) -> AnimationTick {
        self.animations.borrow_mut().tick(now_ms)
    }

    /// Generate instances into `frame`, clearing its instance vectors first.
    #[allow(clippy::too_many_lines)]
    pub fn generate(
        &self,
        screen: &Screen,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
        resolver: &dyn ColorResolver,
        frame: &mut Frame,
    ) {
        frame.clear_instances();
        self.ensure_animation_roots(screen);
        frame.geometry = self.config.grid_for(screen);
        frame.default_fg = resolver.default_fg();
        frame.default_bg = resolver.default_bg();
        frame.selections = self.config.selections.clone();
        frame.overlay_stats.selection_ranges = self.config.selections.len();

        let default_bg = frame.default_bg;
        let cursor_visible =
            screen.is_cursor_visible() && screen.cursor.shape != CursorShape::NoCursor;
        let cursor_x = screen.cursor.x;
        let cursor_y = screen.cursor.y;
        let cursor_shape = screen.cursor.shape;
        let metrics = self.config.font_metrics();
        let selection_bg = self
            .config
            .selection_bg
            .unwrap_or_else(|| resolver.selection_bg());
        let selection_fg = self
            .config
            .selection_fg
            .unwrap_or_else(|| resolver.selection_fg());
        let hover_bg = self
            .config
            .hyperlink_hover_bg
            .unwrap_or_else(|| resolver.selection_bg());
        let link_color = self
            .config
            .hyperlink_color
            .unwrap_or_else(|| resolver.url_color());
        let hover_id = self
            .config
            .hyperlink_hover
            .map(|hover| screen.hyperlink_id_at(hover.x, hover.y))
            .unwrap_or(0);
        let collect_links =
            self.config.hyperlink_underline || self.config.hyperlink_hover.is_some();
        let scrolled_by = screen.scrolled_by;

        let tc = screen.text_cache().borrow();
        for y in 0..screen.lines {
            let range_row = y as i64 - scrolled_by as i64;
            let intervals = if self.config.selections.is_empty() {
                Vec::new()
            } else {
                selection_intervals(&self.config.selections, range_row, screen.columns)
            };
            if collect_links {
                frame.hyperlink_spans.extend(screen.url_spans(y));
            }

            let mut plans: Vec<ShapePlan> = Vec::new();
            if self.config.shaped_runs {
                let mut px = 0u32;
                while px < screen.columns {
                    let cpu = screen.cpu_cell(px, y);
                    if cpu.is_multicell() && (cpu.x() != 0 || cpu.y() != 0) {
                        px += 1;
                        continue;
                    }
                    let width_cells = if cpu.is_multicell() && cpu.width() > 0 {
                        cpu.mcd_x_limit().max(1).min(screen.columns - px)
                    } else {
                        1
                    };
                    let mut single = [0u32; 1];
                    let text = cell_text(&tc, cpu, &mut single);
                    let placeholder = text.first().copied() == Some(IMAGE_PLACEHOLDER_CHAR);
                    let blank = is_blank_text(text);
                    let selected = interval_contains(&intervals, px);
                    let hovered = hover_id != 0 && cpu.hyperlink_id() == hover_id;
                    let cursor_here = cursor_visible && cursor_x == px && cursor_y == y;
                    if blank
                        || placeholder
                        || width_cells != 1
                        || cursor_here
                        || selected
                        || hovered
                    {
                        px += width_cells.max(1);
                        continue;
                    }

                    let gpu0 = screen.gpu_cell(px, y);
                    let attrs0 = gpu0.attrs;
                    let fg0 = gpu0.fg;
                    let bg0 = gpu0.bg;
                    let link0 = cpu.hyperlink_id();
                    let mut run_text = Vec::new();
                    let mut cell_for_codepoint = Vec::new();
                    let mut run_cells = Vec::new();
                    let mut xx = px;
                    while xx < screen.columns && run_cells.len() < SHAPED_RUN_MAX_CELLS {
                        let c2 = screen.cpu_cell(xx, y);
                        if c2.is_multicell() && (c2.x() != 0 || c2.y() != 0) {
                            break;
                        }
                        let w2 = if c2.is_multicell() && c2.width() > 0 {
                            c2.mcd_x_limit().max(1).min(screen.columns - xx)
                        } else {
                            1
                        };
                        if w2 != 1 {
                            break;
                        }
                        let mut single2 = [0u32; 1];
                        let t2 = cell_text(&tc, c2, &mut single2);
                        if is_blank_text(t2)
                            || t2.first().copied() == Some(IMAGE_PLACEHOLDER_CHAR)
                            || interval_contains(&intervals, xx)
                            || (hover_id != 0 && c2.hyperlink_id() == hover_id)
                            || (cursor_visible && cursor_x == xx && cursor_y == y)
                        {
                            break;
                        }
                        let g2 = screen.gpu_cell(xx, y);
                        if g2.attrs != attrs0
                            || g2.fg != fg0
                            || g2.bg != bg0
                            || c2.hyperlink_id() != link0
                        {
                            break;
                        }
                        if run_text.len() + t2.len() > crate::atlas::MAX_GLYPH_CODEPOINTS {
                            break;
                        }
                        for cp in t2 {
                            run_text.push(*cp);
                            cell_for_codepoint.push(run_cells.len() as u32);
                        }
                        run_cells.push(xx);
                        xx += 1;
                    }

                    if !run_cells.is_empty() && (run_cells.len() > 1 || run_text.len() > 1) {
                        frame.overlay_stats.shaped_run_attempts += 1;
                        let request = ShapedRunRequest {
                            font_id: self.config.font_id,
                            text: &run_text,
                            cell_for_codepoint: &cell_for_codepoint,
                            cell_count: run_cells.len() as u32,
                            flags: glyph_flags(attrs0),
                            cell_width: metrics.cell_width as f32,
                            cell_height: metrics.cell_height as f32,
                            baseline: metrics.baseline as f32,
                        };
                        let output = rasterizer.shape_run(&request);
                        match output {
                            Some(output) => {
                                let accepted = Self::accept_shaped_clusters(
                                    output,
                                    &run_text,
                                    glyph_flags(attrs0),
                                    &run_cells,
                                    &mut plans,
                                    frame,
                                    atlas,
                                    self.config.font_id,
                                );
                                if accepted {
                                    frame.overlay_stats.shaped_run_successes += 1;
                                } else {
                                    frame.overlay_stats.shaped_fallbacks += 1;
                                }
                            }
                            None => frame.overlay_stats.shaped_fallbacks += 1,
                        }
                    }
                    px = xx.max(px + 1);
                }
            }

            let mut x = 0u32;
            let mut plan_index = 0usize;
            while x < screen.columns {
                if plan_index < plans.len() && plans[plan_index].start == x {
                    let span = plans[plan_index].span.max(1).min(screen.columns - x);
                    let mut first: Option<CellPaint> = None;
                    for cx in x..x + span {
                        let cpu = screen.cpu_cell(cx, y);
                        let mut single = [0u32; 1];
                        let text = cell_text(&tc, cpu, &mut single);
                        let blank = is_blank_text(text);
                        let width_cells = 1;
                        let gpu = screen.gpu_cell(cx, y);
                        let selected = interval_contains(&intervals, cx);
                        let hovered = hover_id != 0 && cpu.hyperlink_id() == hover_id;
                        let cursor_here = cursor_visible && cursor_x == cx && cursor_y == y;
                        let paint = emit_cell(
                            &self.config,
                            resolver,
                            frame,
                            cx,
                            y,
                            width_cells,
                            cpu,
                            gpu,
                            blank,
                            selected,
                            hovered,
                            cursor_here,
                            cursor_shape,
                            metrics,
                            selection_bg,
                            selection_fg,
                            hover_bg,
                            link_color,
                        );
                        if first.is_none() {
                            first = Some(paint);
                        }
                    }
                    let plan = &plans[plan_index];
                    let paint = first.expect("shaped run has at least one cell");
                    let rect = frame.geometry.cell_rect_with_width(x, y, span);
                    frame.glyphs.push(GlyphInstance {
                        rect,
                        cell_x: x,
                        cell_y: y,
                        fg: paint.fg,
                        sprite: Some(plan.sprite),
                        text: plan.text,
                        flags: paint.flags,
                    });
                    x += span;
                    plan_index += 1;
                    continue;
                }

                let cpu = screen.cpu_cell(x, y);
                if cpu.is_multicell() && (cpu.x() != 0 || cpu.y() != 0) {
                    x += 1;
                    continue;
                }
                let mut single = [0u32; 1];
                let text = cell_text(&tc, cpu, &mut single);
                let placeholder = text.first().copied() == Some(IMAGE_PLACEHOLDER_CHAR);
                let blank = is_blank_text(text) || placeholder;
                let width_cells = if cpu.is_multicell() && cpu.width() > 0 {
                    cpu.mcd_x_limit().max(1).min(screen.columns - x)
                } else {
                    1
                };
                let gpu = screen.gpu_cell(x, y);
                let selected = interval_contains(&intervals, x);
                let hovered = hover_id != 0 && cpu.hyperlink_id() == hover_id;
                let cursor_here = cursor_visible && cursor_x == x && cursor_y == y;
                let paint = emit_cell(
                    &self.config,
                    resolver,
                    frame,
                    x,
                    y,
                    width_cells,
                    cpu,
                    gpu,
                    blank,
                    selected,
                    hovered,
                    cursor_here,
                    cursor_shape,
                    metrics,
                    selection_bg,
                    selection_fg,
                    hover_bg,
                    link_color,
                );

                if !blank {
                    let key = GlyphKey::from_codepoints(
                        self.config.font_id,
                        text,
                        glyph_flags(gpu.attrs),
                    );
                    let sprite = intern_glyph(frame, atlas, rasterizer, key);
                    frame.glyphs.push(GlyphInstance {
                        rect: frame.geometry.cell_rect_with_width(x, y, width_cells),
                        cell_x: x,
                        cell_y: y,
                        fg: paint.fg,
                        sprite,
                        text: key.text,
                        flags: paint.flags,
                    });
                }

                x += width_cells.max(1);
            }
        }
        drop(tc);

        if !self.config.pane_backgrounds.is_empty() {
            let gaps: Vec<BackgroundInstance> = self
                .config
                .pane_backgrounds
                .iter()
                .map(|solid| BackgroundInstance {
                    rect: solid.rect,
                    cell_x: 0,
                    cell_y: 0,
                    color: solid.color,
                    is_default_bg: false,
                    flags: InstanceFlags::default(),
                })
                .collect();
            frame.backgrounds.splice(0..0, gaps);
        }
        for border in &self.config.pane_borders {
            frame.decorations.push(DecorationInstance {
                rect: border.rect,
                cell_x: 0,
                cell_y: 0,
                kind: DecorationKind::PaneBorder,
                color: border.color,
                flags: InstanceFlags::default(),
            });
        }

        let (viewport_width, viewport_height) = self.config.viewport_for(&frame.geometry);
        if let Some(spec) = &self.config.tab_bar {
            emit_tab_bar(spec, &self.config, frame, atlas, rasterizer, viewport_width);
        }

        let animations = self.animations.borrow();
        let palette = crate::colors::indexed_image_palette(resolver);
        frame.indexed_palette = Some(palette.clone());
        images::collect_protocol_images_with_palette(
            screen,
            Some(&animations),
            &palette,
            &frame.geometry,
            viewport_width,
            viewport_height,
            self.config.image_filter,
            &mut frame.images,
            &mut frame.image_uploads,
        );
        if let Some(config) = &self.config.background_image {
            if let Some((instance, upload)) =
                images::background_image(config, viewport_width, viewport_height, default_bg)
            {
                frame.background_image = Some(instance);
                frame.image_uploads.push(upload);
            }
        }
        if let Some(config) = &self.config.window_logo {
            if let Some((instance, upload)) =
                images::window_logo(config, viewport_width, viewport_height)
            {
                frame.logo = Some(instance);
                frame.image_uploads.push(upload);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn accept_shaped_clusters(
        output: ShapedRunOutput,
        run_text: &[u32],
        flags: u32,
        run_cells: &[u32],
        plans: &mut Vec<ShapePlan>,
        frame: &mut Frame,
        atlas: &mut GlyphAtlas,
        font_id: u16,
    ) -> bool {
        let mut accepted = false;
        for cluster in output.clusters {
            let ShapedRunCluster {
                cell_start,
                cell_span,
                bitmap,
            } = cluster;
            if cell_span == 0
                || cell_start as usize >= run_cells.len()
                || cell_start.saturating_add(cell_span) as usize > run_cells.len()
                || bitmap.width == 0
                || bitmap.height == 0
            {
                frame.overlay_stats.shaped_fallbacks += 1;
                continue;
            }
            let start_cell = run_cells[cell_start as usize];
            let end_cell = run_cells[(cell_start + cell_span - 1) as usize] + 1;
            if end_cell != start_cell + cell_span {
                frame.overlay_stats.shaped_fallbacks += 1;
                continue;
            }
            if plans
                .iter()
                .any(|plan| start_cell < plan.start + plan.span && plan.start < end_cell)
            {
                frame.overlay_stats.shaped_fallbacks += 1;
                continue;
            }
            let hash = shaped_run_hash(font_id, flags, run_text, cell_start, cell_span);
            let key = GlyphKey::from_shaped_run(font_id, run_text, flags, hash, cell_start);
            match atlas.insert(key, bitmap) {
                Ok(inserted) => {
                    if let Some(event) = inserted.event {
                        frame.atlas_events.push(event);
                    }
                    if inserted.already_present {
                        frame.overlay_stats.shaped_cache_hits += 1;
                    } else {
                        frame.overlay_stats.shaped_cache_misses += 1;
                        frame.atlas_updates.push(inserted.slot);
                    }
                    plans.push(ShapePlan {
                        start: start_cell,
                        span: cell_span,
                        sprite: inserted.slot,
                        text: key.text,
                    });
                    frame.overlay_stats.shaped_sprites += 1;
                    accepted = true;
                }
                Err(_) => frame.overlay_stats.shaped_fallbacks += 1,
            }
        }
        plans.sort_unstable_by_key(|plan| plan.start);
        accepted
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
    #[test]
    fn protocol_placements_and_media_layers() {
        use kitty_graphics::parse_raw;

        let mut s = screen(10, 5);
        let mut image = parse_raw(b"a=t,f=32,s=2,v=2,i=7").unwrap();
        image.payload = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        s.handle_graphics_command(image).unwrap();
        let mut put = parse_raw(b"a=p,i=7,p=3,c=2,r=1,X=1,Y=2,z=-1").unwrap();
        put.payload.clear();
        s.handle_graphics_command(put).unwrap();

        let bg = crate::images::BackgroundImageConfig {
            asset: crate::images::ImageAsset::from_rgba(1, 1, vec![9, 8, 7, 255]).unwrap(),
            layout: crate::images::BackgroundLayout::Scaled,
            opacity: 0.75,
            tint: 0.25,
            linear: false,
        };
        let logo = crate::images::WindowLogoConfig {
            asset: crate::images::ImageAsset::from_rgba(1, 1, vec![1, 2, 3, 255]).unwrap(),
            position: crate::images::LogoPosition::TopLeft,
            alpha: 0.5,
            scale_x: 0.0,
            scale_y: 0.0,
            linear: true,
        };
        let config = RenderConfig {
            viewport_width: 100.0,
            viewport_height: 60.0,
            background_image: Some(bg),
            window_logo: Some(logo),
            ..RenderConfig::default()
        };
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut r = rasterizer();
        let res = resolver();
        let mut frame = Frame::default();
        InstanceGenerator::new(config).generate(&s, &mut atlas, &mut r, &res, &mut frame);

        assert_eq!(frame.images.len(), 1);
        let image = frame.images[0];
        assert_eq!(image.key, crate::images::ImageKey::Protocol(7));
        assert_eq!(image.rect, Rect::new(1.0, 2.0, 20.0, 20.0));
        assert_eq!(image.layer, crate::images::ImageLayer::BelowText);
        assert_eq!(image.z_index, -1);
        assert_eq!(image.uv.u1, 1.0);

        let background = frame.background_image.expect("background image");
        assert_eq!(background.rect, Rect::new(0.0, 0.0, 100.0, 60.0));
        assert_eq!(background.key, crate::images::ImageKey::Background(0));
        assert_eq!(background.tint, 0.25);
        assert_eq!(background.opacity, 0.75);
        assert!(frame.logo.is_some());
        assert_eq!(
            frame
                .image_uploads
                .iter()
                .map(|u| u.key)
                .collect::<Vec<_>>()
                .len(),
            3
        );
    }

    #[test]
    fn protocol_placement_preserves_aspect_ratio() {
        use kitty_graphics::parse_raw;

        let mut s = screen(10, 5);
        let mut image = parse_raw(b"a=t,f=32,s=4,v=2,i=4").unwrap();
        image.payload = vec![0; 4 * 2 * 4];
        s.handle_graphics_command(image).unwrap();
        let mut put = parse_raw(b"a=p,i=4,p=1,c=2").unwrap();
        put.payload.clear();
        s.handle_graphics_command(put).unwrap();

        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut r = rasterizer();
        let res = resolver();
        let mut frame = Frame::default();
        generator().generate(&s, &mut atlas, &mut r, &res, &mut frame);
        // 2 cells wide = 20px; the 2:1 image gives 10px height.
        assert_eq!(frame.images[0].rect.width, 20.0);
        assert_eq!(frame.images[0].rect.height, 10.0);
    }
    fn bg_at(frame: &Frame, x: u32, y: u32) -> Option<Rgba8> {
        frame
            .backgrounds
            .iter()
            .find(|b| b.cell_x == x && b.cell_y == y)
            .map(|b| b.color)
    }

    fn glyph_at(frame: &Frame, x: u32, y: u32) -> Option<&GlyphInstance> {
        frame.glyphs.iter().find(|g| g.cell_x == x && g.cell_y == y)
    }

    #[test]
    fn selection_ranges_tint_multiple_cells_and_rows() {
        let mut s = screen(6, 2);
        draw(&mut s, "a bcde");
        s.cursor_position(1, 2);
        draw(&mut s, "fghij");
        let sel_a = SelectionRange::cell(1, 0, 3, 0);
        let sel_b = SelectionRange::block(0, 1, 1, 1);
        let cfg = RenderConfig {
            selections: vec![sel_a, sel_b],
            ..RenderConfig::default()
        };
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut r = rasterizer();
        let res = resolver();
        let mut frame = Frame::default();
        InstanceGenerator::new(cfg).generate(&s, &mut atlas, &mut r, &res, &mut frame);

        let selected: Vec<(u32, u32)> = frame.selection_cells.iter().map(|c| (c.x, c.y)).collect();
        assert!(selected.contains(&(1, 0)));
        assert!(selected.contains(&(2, 0)));
        assert!(selected.contains(&(3, 0)));
        assert!(!selected.contains(&(0, 0)));
        assert!(selected.contains(&(0, 1)));
        assert!(selected.contains(&(1, 1)));
        assert!(!selected.contains(&(2, 1)));
        assert_eq!(frame.selections.len(), 2);
        assert_eq!(frame.overlay_stats.selection_ranges, 2);
        assert_eq!(bg_at(&frame, 1, 0), Some(res.selection_bg()));
        assert_eq!(bg_at(&frame, 3, 0), Some(res.selection_bg()));
        assert_eq!(bg_at(&frame, 0, 1), Some(res.selection_bg()));
        assert_eq!(glyph_at(&frame, 1, 0).unwrap().fg, res.selection_fg());
        assert_eq!(glyph_at(&frame, 0, 0).unwrap().fg, res.default_fg());
    }

    #[test]
    fn selection_uses_scrolled_back_view_offset() {
        let mut s = screen(6, 2);
        draw(&mut s, "abc");
        s.scrolled_by = 1;
        let sel = SelectionRange::cell(1, -1, 2, -1);
        let cfg = RenderConfig {
            selections: vec![sel],
            ..RenderConfig::default()
        };
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut r = rasterizer();
        let res = resolver();
        let mut frame = Frame::default();
        InstanceGenerator::new(cfg).generate(&s, &mut atlas, &mut r, &res, &mut frame);
        let selected: Vec<(u32, u32)> = frame.selection_cells.iter().map(|c| (c.x, c.y)).collect();
        assert_eq!(selected, vec![(1, 0), (2, 0)]);
    }

    #[test]
    fn hyperlinks_add_dotted_underlines_and_hover_highlight() {
        let mut s = screen(8, 2);
        s.set_active_hyperlink(Some("id"), Some("https://example.com"));
        draw(&mut s, "abc");
        s.set_active_hyperlink(None, None);
        let hover = Rgba8::rgb(200, 20, 30);
        let cfg = RenderConfig {
            hyperlink_hover: Some(HyperlinkHover { x: 1, y: 0 }),
            hyperlink_hover_bg: Some(hover),
            ..RenderConfig::default()
        };
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut r = rasterizer();
        let res = resolver();
        let mut frame = Frame::default();
        InstanceGenerator::new(cfg).generate(&s, &mut atlas, &mut r, &res, &mut frame);

        let link_cells: std::collections::BTreeSet<u32> = frame
            .decorations
            .iter()
            .filter(|d| d.kind == DecorationKind::Hyperlink)
            .map(|d| d.cell_x)
            .collect();
        assert_eq!(link_cells, [0, 1, 2].into_iter().collect());
        assert!(frame.overlay_stats.hyperlink_decorations >= 3);
        assert!(frame.overlay_stats.hyperlink_cells >= 3);
        assert_eq!(bg_at(&frame, 1, 0), Some(hover));
        assert_eq!(
            frame.overlay_stats.hover_cells, 3,
            "the whole link is hovered"
        );
        assert!(!frame.hyperlink_spans.is_empty());
    }

    fn tab_config(viewport_width: f32) -> RenderConfig {
        let spec = TabBarSpec {
            titles: vec!["one".into(), "two".into()],
            active: 0,
            height: 30.0,
            min_tab_width: 40.0,
            max_tab_width: 80.0,
            ..TabBarSpec::default()
        };
        RenderConfig {
            viewport_width,
            viewport_height: 80.0,
            tab_bar: Some(spec),
            ..RenderConfig::default()
        }
    }

    #[test]
    fn pane_borders_and_background_gaps_are_emitted() {
        let mut s = Screen::builder(2, 1).build().unwrap();
        s.draw_text(&[b'a' as u32]);
        let config = RenderConfig {
            metrics: CellMetrics {
                cell_width: 10.0,
                cell_height: 10.0,
                baseline: 8.0,
                underline_position: 2.0,
                underline_thickness: 1.0,
                strikethrough_position: 5.0,
                strikethrough_thickness: 1.0,
            },
            pane_backgrounds: vec![SolidRect {
                rect: Rect::new(0.0, 0.0, 20.0, 10.0),
                color: Rgba8::rgb(1, 2, 3),
            }],
            pane_borders: vec![SolidRect {
                rect: Rect::new(10.0, 0.0, 1.0, 10.0),
                color: Rgba8::rgb(4, 5, 6),
            }],
            ..RenderConfig::default()
        };
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut r = rasterizer();
        let res = resolver();
        let mut frame = Frame::default();
        InstanceGenerator::new(config).generate(&s, &mut atlas, &mut r, &res, &mut frame);
        assert!(frame
            .backgrounds
            .iter()
            .any(|b| b.color == Rgba8::rgb(1, 2, 3)));
        assert!(frame.decorations.iter().any(|d| {
            d.kind == DecorationKind::PaneBorder
                && d.color == Rgba8::rgb(4, 5, 6)
                && d.rect == Rect::new(10.0, 0.0, 1.0, 10.0)
        }));
    }

    #[test]
    fn tab_bar_emits_backgrounds_borders_and_titles() {
        let mut s = screen(12, 2);
        draw(&mut s, "x");
        let cfg = tab_config(120.0);
        let spec = cfg.tab_bar.clone().unwrap();
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut r = rasterizer();
        let res = resolver();
        let mut frame = Frame::default();
        InstanceGenerator::new(cfg).generate(&s, &mut atlas, &mut r, &res, &mut frame);
        let bar = frame.tab_bar.as_ref().expect("tab bar");
        assert_eq!(bar.rect, Rect::new(0.0, 0.0, 120.0, 30.0));
        assert_eq!(bar.tabs.len(), 2);
        assert_eq!(bar.tabs[0].rect.width, 60.0);
        assert!(bar.tabs[0].active);
        assert!(!bar.tabs[1].active);
        assert_eq!(bar.visible_titles, vec!["one", "two"]);
        assert_eq!(frame.overlay_stats.tab_tabs, 2);
        assert_eq!(frame.overlay_stats.tab_glyphs, 6);
        assert!(frame
            .decorations
            .iter()
            .any(|d| d.kind == DecorationKind::TabBarBorder));
        assert!(frame
            .backgrounds
            .iter()
            .any(|b| b.color == spec.colors.active_background));
        assert!(frame
            .backgrounds
            .iter()
            .any(|b| b.color == spec.colors.inactive_background));
    }

    #[test]
    fn tab_bar_overflow_is_clipped_to_viewport() {
        let spec = TabBarSpec {
            titles: (0..8).map(|i| format!("tab{i}")).collect(),
            active: 0,
            height: 24.0,
            min_tab_width: 40.0,
            max_tab_width: 40.0,
            ..TabBarSpec::default()
        };
        let cfg = RenderConfig {
            viewport_width: 100.0,
            viewport_height: 60.0,
            tab_bar: Some(spec),
            ..RenderConfig::default()
        };
        let mut s = screen(10, 2);
        draw(&mut s, "x");
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut r = rasterizer();
        let res = resolver();
        let mut frame = Frame::default();
        InstanceGenerator::new(cfg).generate(&s, &mut atlas, &mut r, &res, &mut frame);
        let bar = frame.tab_bar.as_ref().unwrap();
        assert_eq!(bar.tabs.len(), 3);
        assert!(bar.tabs.iter().all(|t| t.rect.right() <= 100.0));
        assert!(frame
            .backgrounds
            .iter()
            .all(|b| b.rect.right() <= 100.0 + 1e-4));
        assert!(frame.glyphs.iter().all(|g| g.rect.right() <= 100.0 + 1e-4));
    }

    struct FakeShaper {
        fail: bool,
        calls: usize,
    }

    impl GlyphRasterizer for FakeShaper {
        fn rasterize(&mut self, _glyph: &GlyphKey) -> Option<GlyphBitmap> {
            GlyphBitmap::gray(6, 10, vec![0x80; 60]).ok()
        }

        fn shape_run(&mut self, run: &ShapedRunRequest<'_>) -> Option<ShapedRunOutput> {
            self.calls += 1;
            if self.fail {
                return None;
            }
            let cw = run.cell_width.round() as u32;
            let ch = run.cell_height.round() as u32;
            let (start, span) = if run.text.len() >= 2
                && run.text[0] == b'-' as u32
                && run.text[1] == b'>' as u32
            {
                (0, 2)
            } else if run.text.len() > 1 && run.text[0] == b'e' as u32 && run.text[1] == 0x301 {
                (0, 1)
            } else {
                return None;
            };
            let bitmap =
                GlyphBitmap::gray(cw * span, ch, vec![0xff; (cw * span * ch) as usize]).ok()?;
            Some(ShapedRunOutput {
                clusters: vec![ShapedRunCluster {
                    cell_start: start,
                    cell_span: span,
                    bitmap,
                }],
            })
        }
    }

    fn shaped_screen(text: &[u32]) -> Screen {
        let mut s = screen(8, 1);
        s.draw_text(text);
        s
    }

    #[test]
    fn shaped_ligature_is_one_sprite_without_double_draw() {
        let s = shaped_screen(&['-' as u32, '>' as u32, 'x' as u32]);
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut shaper = FakeShaper {
            fail: false,
            calls: 0,
        };
        let res = resolver();
        let gen = generator();
        let mut frame = Frame::default();
        gen.generate(&s, &mut atlas, &mut shaper, &res, &mut frame);
        assert_eq!(frame.glyphs.len(), 2, "ligature + x");
        assert_eq!(frame.overlay_stats.shaped_sprites, 1);
        assert_eq!(frame.overlay_stats.shaped_run_attempts, 1);
        assert_eq!(frame.overlay_stats.shaped_run_successes, 1);
        let ligature = glyph_at(&frame, 0, 0).unwrap();
        assert_eq!(ligature.rect.width, 20.0);
        let slot = ligature.sprite.unwrap();
        assert!(slot.uv.u0 >= 0.0 && slot.uv.v0 >= 0.0);
        assert!(slot.uv.u1 <= 1.0 && slot.uv.v1 <= 1.0);
        assert!(slot.width == 20 && slot.height == 20);
        assert!(frame.glyphs.iter().all(|g| g.cell_x != 1));

        let hits_before = frame.overlay_stats.shaped_cache_hits;
        gen.generate(&s, &mut atlas, &mut shaper, &res, &mut frame);
        assert_eq!(frame.glyphs.len(), 2);
        assert!(frame.overlay_stats.shaped_cache_hits > hits_before);
        assert_eq!(shaper.calls, 2);
    }

    #[test]
    fn shaped_combining_cluster_uses_one_cell_sprite() {
        let s = shaped_screen(&['e' as u32, 0x301, 'x' as u32]);
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut shaper = FakeShaper {
            fail: false,
            calls: 0,
        };
        let res = resolver();
        let mut frame = Frame::default();
        generator().generate(&s, &mut atlas, &mut shaper, &res, &mut frame);
        assert_eq!(frame.glyphs.len(), 2, "combining cluster + x");
        assert_eq!(frame.overlay_stats.shaped_sprites, 1);
        let cluster = glyph_at(&frame, 0, 0).unwrap();
        assert_eq!(cluster.rect.width, 10.0);
        assert!(cluster.sprite.is_some());
    }

    #[test]
    fn shaped_failure_falls_back_to_per_cell_glyphs() {
        let s = shaped_screen(&['-' as u32, '>' as u32, 'x' as u32]);
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut shaper = FakeShaper {
            fail: true,
            calls: 0,
        };
        let res = resolver();
        let mut frame = Frame::default();
        generator().generate(&s, &mut atlas, &mut shaper, &res, &mut frame);
        assert_eq!(frame.glyphs.len(), 3);
        assert_eq!(frame.overlay_stats.shaped_sprites, 0);
        assert!(frame.overlay_stats.shaped_fallbacks >= 1);
        assert!(frame.glyphs.iter().all(|g| g.sprite.is_some()));
    }
}
