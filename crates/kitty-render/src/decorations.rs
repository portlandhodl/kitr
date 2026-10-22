// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU geometry for text decorations and window/tab-bar borders.
//!
//! The underline, strikethrough and cursor masks are direct ports of
//! `kitty/decorations.c`. They are produced as one-byte alpha masks and can
//! either be turned into axis-aligned rectangles (for the instance stream) or
//! handed to a backend for uploading as a texture.
//!
//! Border geometry follows `kitty/borders.py` and `kitty/tab_bar.py`, including
//! the compensatory padding rules and the rounded-corner placeholder rect.
//! Rounded rectangles are also tessellated into triangles for a backend that
//! prefers geometry over the SDF shader.

use kitty_fonts::FontCellMetrics;

use crate::geom::{EdgeWidths, Rect, Region};

// {{{ Underline styles

/// An SGR underline style (`CellAttrs::decoration`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum UnderlineStyle {
    #[default]
    None = 0,
    Straight = 1,
    Double = 2,
    Curly = 3,
    Dotted = 4,
    Dashed = 5,
}

impl UnderlineStyle {
    pub const fn from_decoration(value: u32) -> Self {
        match value & 7 {
            1 => Self::Straight,
            2 => Self::Double,
            3 => Self::Curly,
            4 => Self::Dotted,
            5 => Self::Dashed,
            _ => Self::None,
        }
    }

    #[inline]
    pub const fn as_decoration(self) -> u32 {
        self as u32
    }

    #[inline]
    pub const fn is_none(self) -> bool {
        matches!(self, Self::None)
    }
}

/// The vertical extent of a decoration inside a cell, matching
/// `DecorationGeometry`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecorationGeometry {
    pub top: u32,
    pub height: u32,
}

impl DecorationGeometry {
    #[inline]
    pub const fn bottom(self) -> u32 {
        self.top + self.height
    }
}

/// A one-byte-per-pixel alpha mask for a decoration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DecorationMask {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl DecorationMask {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0; width as usize * height as usize],
        }
    }

    #[inline]
    pub fn pixel(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.data[y as usize * self.width as usize + x as usize]
    }

    #[inline]
    fn add_intensity(&mut self, x: u32, y: i64, value: u8, max_y: u32, position: u32) {
        let y = (y + position as i64).clamp(0, max_y as i64) as u32;
        let idx = self.width as usize * y as usize + x as usize;
        self.data[idx] = self.data[idx].saturating_add(value);
    }

    fn set_row(&mut self, y: u32, x: u32, len: u32, value: u8) {
        if y >= self.height || x >= self.width {
            return;
        }
        let start = y as usize * self.width as usize + x as usize;
        let end = start + len.min(self.width - x) as usize;
        self.data[start..end].fill(value);
    }

    /// Split every row into runs of non-zero pixels and return them as
    /// rectangles in cell-local coordinates.
    pub fn to_rects(&self, geometry: DecorationGeometry) -> Vec<Rect> {
        let mut ans = Vec::new();
        let top = geometry.top.min(self.height);
        let bottom = geometry.bottom().min(self.height);
        for y in top..bottom {
            let row = &self.data
                [y as usize * self.width as usize..(y as usize + 1) * self.width as usize];
            let mut x = 0usize;
            while x < row.len() {
                while x < row.len() && row[x] == 0 {
                    x += 1;
                }
                let start = x;
                while x < row.len() && row[x] != 0 {
                    x += 1;
                }
                if x > start {
                    ans.push(Rect::new(start as f32, y as f32, (x - start) as f32, 1.0));
                }
            }
        }
        ans
    }
}

fn straight_geometry(metrics: FontCellMetrics) -> DecorationGeometry {
    let half = metrics.underline_thickness / 2;
    let top = metrics.underline_position.saturating_sub(half);
    if metrics.underline_thickness == 0 || top >= metrics.cell_height {
        return DecorationGeometry {
            top: top.min(metrics.cell_height),
            height: 0,
        };
    }
    DecorationGeometry {
        top,
        height: metrics.underline_thickness.min(metrics.cell_height - top),
    }
}

/// `add_straight_underline()` geometry.
#[inline]
pub fn straight_underline_geometry(metrics: FontCellMetrics) -> DecorationGeometry {
    straight_geometry(metrics)
}

/// `add_straight_underline()` mask.
pub fn straight_underline_mask(metrics: FontCellMetrics) -> (DecorationMask, DecorationGeometry) {
    let geometry = straight_geometry(metrics);
    let mut mask = DecorationMask::new(metrics.cell_width, metrics.cell_height);
    for y in geometry.top..geometry.bottom() {
        mask.set_row(y, 0, metrics.cell_width, 0xff);
    }
    (mask, geometry)
}

/// `add_strikethrough()` geometry.
pub fn strikethrough_geometry(metrics: FontCellMetrics) -> DecorationGeometry {
    let half = metrics.strikethrough_thickness / 2;
    let top = metrics.strikethrough_position.saturating_sub(half);
    if metrics.strikethrough_thickness == 0 || top >= metrics.cell_height {
        return DecorationGeometry {
            top: top.min(metrics.cell_height),
            height: 0,
        };
    }
    DecorationGeometry {
        top,
        height: metrics
            .strikethrough_thickness
            .min(metrics.cell_height - top),
    }
}

/// `add_strikethrough()` mask.
pub fn strikethrough_mask(metrics: FontCellMetrics) -> (DecorationMask, DecorationGeometry) {
    let geometry = strikethrough_geometry(metrics);
    let mut mask = DecorationMask::new(metrics.cell_width, metrics.cell_height);
    for y in geometry.top..geometry.bottom() {
        mask.set_row(y, 0, metrics.cell_width, 0xff);
    }
    (mask, geometry)
}

/// `add_double_underline()` geometry.
pub fn double_underline_geometry(metrics: FontCellMetrics) -> DecorationGeometry {
    let height = metrics.cell_height;
    if height == 0 {
        return DecorationGeometry::default();
    }
    let a = metrics
        .underline_position
        .saturating_sub(metrics.underline_thickness)
        .min(height - 1);
    let b = metrics.underline_position.min(height - 1);
    let mut top = a.min(b);
    let mut bottom = a.max(b);
    let deficit = 2i32 - (bottom as i32 - top as i32);
    if deficit > 0 {
        let deficit = deficit as u32;
        if bottom + deficit < height {
            bottom += deficit;
        } else if bottom < height - 1 {
            bottom += 1;
            if deficit > 1 {
                top = top.saturating_sub(deficit - 1);
            }
        } else {
            top = top.saturating_sub(deficit);
        }
    }
    top = top.min(height - 1);
    bottom = bottom.min(height - 1);
    DecorationGeometry {
        top,
        height: bottom + 1 - top,
    }
}

/// `add_double_underline()` mask.
pub fn double_underline_mask(metrics: FontCellMetrics) -> (DecorationMask, DecorationGeometry) {
    let geometry = double_underline_geometry(metrics);
    let mut mask = DecorationMask::new(metrics.cell_width, metrics.cell_height);
    if geometry.height > 0 {
        mask.set_row(geometry.top, 0, metrics.cell_width, 0xff);
        mask.set_row(geometry.bottom() - 1, 0, metrics.cell_width, 0xff);
    }
    (mask, geometry)
}

fn distribute_dots(
    available_space: u32,
    num_of_dots: u32,
    gaps: &mut [u32],
    summed: &mut [u32],
) -> u32 {
    let dot_size = (available_space / (2 * num_of_dots)).max(1);
    let extra_used = 2 * num_of_dots * dot_size;
    let mut extra = available_space.saturating_sub(extra_used);
    for g in gaps.iter_mut() {
        *g = dot_size;
    }
    let mut idx = 0usize;
    while extra > 0 {
        gaps[idx] += 1;
        idx = (idx + 1) % num_of_dots as usize;
        extra -= 1;
    }
    gaps[0] /= 2;
    for i in 0..num_of_dots as usize {
        summed[i] = gaps[..=i].iter().sum();
    }
    dot_size
}

/// `add_dotted_underline()` mask.
pub fn dotted_underline_mask(metrics: FontCellMetrics) -> (DecorationMask, DecorationGeometry) {
    let geometry = straight_geometry(metrics);
    let mut mask = DecorationMask::new(metrics.cell_width, metrics.cell_height);
    let n = (metrics.cell_width / (2 * metrics.underline_thickness.max(1))).max(1);
    let mut gaps = vec![0u32; n as usize];
    let mut summed = vec![0u32; n as usize];
    let size = distribute_dots(metrics.cell_width, n, &mut gaps, &mut summed);
    for y in geometry.top..geometry.bottom() {
        for j in 0..n {
            let start = j * size + gaps[j as usize];
            mask.set_row(y, start, size, 0xff);
        }
    }
    (mask, geometry)
}

/// `add_dashed_underline()` mask.
pub fn dashed_underline_mask(metrics: FontCellMetrics) -> (DecorationMask, DecorationGeometry) {
    let geometry = straight_geometry(metrics);
    let mut mask = DecorationMask::new(metrics.cell_width, metrics.cell_height);
    let quarter = metrics.cell_width / 4;
    let dash_width = metrics.cell_width.saturating_sub(3 * quarter);
    for y in geometry.top..geometry.bottom() {
        mask.set_row(y, 0, dash_width, 0xff);
        mask.set_row(y, 3 * quarter, dash_width, 0xff);
    }
    (mask, geometry)
}

/// `add_curl_underline()` mask.
///
/// `undercurl_style` uses the same bit meanings as kitty's option: bit 0 makes
/// the wave tighter (four half-periods instead of two) and bit 1 makes the
/// stroke thicker.
pub fn curly_underline_mask(
    metrics: FontCellMetrics,
    undercurl_style: u8,
) -> (DecorationMask, DecorationGeometry) {
    let width = metrics.cell_width;
    let height = metrics.cell_height;
    let mut mask = DecorationMask::new(width, height);
    if width == 0 || height == 0 {
        return (mask, DecorationGeometry::default());
    }
    let max_x = width - 1;
    let max_y = height - 1;
    let pi = core::f64::consts::PI;
    let xfactor = (if undercurl_style & 1 != 0 { 4.0 } else { 2.0 }) * pi / (max_x as f64).max(1.0);

    let quotient = metrics.underline_thickness / 2;
    let remainder = metrics.underline_thickness % 2;
    let position = metrics
        .underline_position
        .min(height.saturating_sub(quotient + remainder));
    let mut thickness = metrics
        .underline_thickness
        .max(1)
        .min(height.saturating_sub(position + 1));
    let max_height = height.saturating_sub(position.saturating_sub(thickness / 2));
    let half_height = (max_height / 4).max(1);
    if undercurl_style & 2 != 0 {
        thickness = half_height.max(thickness);
    } else {
        thickness = thickness.max(1) - if thickness < 3 { 1 } else { 2 };
    }

    let mut position = position + half_height * 2;
    if position + half_height > max_y {
        position = max_y.saturating_sub(half_height);
    }

    let mut miny = height;
    let mut maxy = 0u32;
    for x in 0..width {
        let y = half_height as f64 * (x as f64 * xfactor).cos();
        let y1 = (y - thickness as f64).floor() as i64;
        let y2 = y.ceil() as i64;
        let intensity = (255.0 * (y - y.floor()).abs()) as u32;
        let i1 = (255 - intensity) as u8;
        let i2 = intensity as u8;
        if i1 != 0 {
            mask.add_intensity(x, y1, i1, max_y, position);
            let yc = (y1 + position as i64).clamp(0, max_y as i64) as u32;
            miny = miny.min(yc);
            maxy = maxy.max(yc);
        }
        if i2 != 0 {
            mask.add_intensity(x, y2, i2, max_y, position);
            let yc = (y2 + position as i64).clamp(0, max_y as i64) as u32;
            miny = miny.min(yc);
            maxy = maxy.max(yc);
        }
        for t in 1..=thickness {
            mask.add_intensity(x, y1 + t as i64, 255, max_y, position);
        }
    }
    if miny > maxy {
        (mask, DecorationGeometry::default())
    } else {
        (
            mask,
            DecorationGeometry {
                top: miny,
                height: maxy - miny + 1,
            },
        )
    }
}

/// Build the decoration mask for an underline style.
pub fn underline_mask(
    style: UnderlineStyle,
    metrics: FontCellMetrics,
    undercurl_style: u8,
) -> (DecorationMask, DecorationGeometry) {
    match style {
        UnderlineStyle::None => (
            DecorationMask::new(metrics.cell_width, metrics.cell_height),
            DecorationGeometry::default(),
        ),
        UnderlineStyle::Straight => straight_underline_mask(metrics),
        UnderlineStyle::Double => double_underline_mask(metrics),
        UnderlineStyle::Curly => curly_underline_mask(metrics, undercurl_style),
        UnderlineStyle::Dotted => dotted_underline_mask(metrics),
        UnderlineStyle::Dashed => dashed_underline_mask(metrics),
    }
}

/// Geometry as axis-aligned rectangles in cell-local coordinates.
pub fn underline_rects(
    style: UnderlineStyle,
    metrics: FontCellMetrics,
    undercurl_style: u8,
) -> Vec<Rect> {
    let (mask, geometry) = underline_mask(style, metrics, undercurl_style);
    mask.to_rects(geometry)
}

/// Strikethrough geometry as rectangles in cell-local coordinates.
pub fn strikethrough_rects(metrics: FontCellMetrics) -> Vec<Rect> {
    let (mask, geometry) = strikethrough_mask(metrics);
    mask.to_rects(geometry)
}

/// `add_missing_glyph()` mask.
pub fn missing_glyph_mask(metrics: FontCellMetrics) -> (DecorationMask, DecorationGeometry) {
    let mut mask = DecorationMask::new(metrics.cell_width, metrics.cell_height);
    let thickness = metrics
        .underline_thickness
        .min(metrics.strikethrough_thickness)
        .min(metrics.cell_width);
    let height = metrics.cell_height;
    for y in 0..height {
        if y < thickness || y >= height.saturating_sub(thickness) {
            mask.set_row(y, 0, metrics.cell_width, 0xff);
        } else {
            mask.set_row(y, 0, thickness, 0xff);
            if metrics.cell_width >= thickness {
                mask.set_row(y, metrics.cell_width - thickness, thickness, 0xff);
            }
        }
    }
    (mask, DecorationGeometry { top: 0, height })
}

// }}}

// {{{ Cursor geometry

#[inline]
fn vert_width(width_pt: f64, dpi_x: f64, cell_width: u32) -> u32 {
    ((width_pt * dpi_x / 72.0).round() as i64).clamp(1, cell_width.max(1) as i64) as u32
}

#[inline]
fn horz_height(height_pt: f64, dpi_y: f64, cell_height: u32) -> u32 {
    ((height_pt * dpi_y / 72.0).round() as i64).clamp(1, cell_height.max(1) as i64) as u32
}

/// `add_beam_cursor()` geometry and rectangle.
pub fn beam_cursor_rect(
    metrics: FontCellMetrics,
    thickness_pt: f64,
    dpi_x: f64,
) -> (Rect, DecorationGeometry) {
    let width = vert_width(thickness_pt, dpi_x, metrics.cell_width);
    (
        Rect::new(0.0, 0.0, width as f32, metrics.cell_height as f32),
        DecorationGeometry {
            top: 0,
            height: metrics.cell_height,
        },
    )
}

/// `add_underline_cursor()` geometry and rectangle.
pub fn underline_cursor_rect(
    metrics: FontCellMetrics,
    thickness_pt: f64,
    dpi_y: f64,
) -> (Rect, DecorationGeometry) {
    let height = horz_height(thickness_pt, dpi_y, metrics.cell_height);
    let top = metrics.cell_height.saturating_sub(height);
    (
        Rect::new(0.0, top as f32, metrics.cell_width as f32, height as f32),
        DecorationGeometry {
            top,
            height: metrics.cell_height - top,
        },
    )
}

/// `add_hollow_cursor()` rectangles (left, right, top, bottom).
pub fn hollow_cursor_rects(
    metrics: FontCellMetrics,
    dpi_x: f64,
    dpi_y: f64,
) -> ([Rect; 4], DecorationGeometry) {
    let w = metrics.cell_width as f32;
    let h = metrics.cell_height as f32;
    let vw = vert_width(1.0, dpi_x, metrics.cell_width) as f32;
    let hh = horz_height(1.0, dpi_y, metrics.cell_height) as f32;
    let rects = [
        Rect::new(0.0, 0.0, vw, h),
        Rect::new((w - vw).max(0.0), 0.0, vw, h),
        Rect::new(0.0, 0.0, w, hh),
        Rect::new(0.0, (h - hh).max(0.0), w, hh),
    ];
    (
        rects,
        DecorationGeometry {
            top: 0,
            height: metrics.cell_height,
        },
    )
}

// }}}

// {{{ Borders and tab bar

/// Indices into the border shader's colour table
/// (`BorderColor` in `kitty/borders.py`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BorderColor {
    DefaultBackground = 0,
    Active = 1,
    Inactive = 2,
    WindowBackground = 3,
    Bell = 4,
    TabBarBackground = 5,
    TabBarMargin = 6,
    TabBarLeftEdge = 7,
    TabBarRightEdge = 8,
}

impl BorderColor {
    #[inline]
    pub const fn index(self) -> u8 {
        self as u8
    }
}

/// A rectangle for the border shader. Mirrors `BorderRect` from `state.h`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub color: BorderColor,
    pub border_type: i64,
    pub horizontal: bool,
    pub render: bool,
    pub radius: u32,
    pub thickness: u32,
}

impl BorderRect {
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32, color: BorderColor) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
            color,
            border_type: 0,
            horizontal: false,
            render: true,
            radius: 0,
            thickness: 0,
        }
    }

    #[inline]
    pub const fn width(self) -> f32 {
        self.right - self.left
    }

    #[inline]
    pub const fn height(self) -> f32 {
        self.bottom - self.top
    }

    #[inline]
    pub const fn rect(self) -> Rect {
        Rect::from_ltrb(self.left, self.top, self.right, self.bottom)
    }
}

/// Window geometry as used by `add_borders()`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowGeometry {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    /// Pixels left over after fitting whole cells. The border includes these.
    pub compensatory: EdgeWidths,
}

impl WindowGeometry {
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
            compensatory: EdgeWidths {
                left: 0.0,
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
            },
        }
    }
}

/// `horizontal_edge()` from `borders.py`.
#[allow(clippy::too_many_arguments)]
pub fn horizontal_edge(
    rects: &mut Vec<BorderRect>,
    color: BorderColor,
    height: f32,
    left: f32,
    right: f32,
    top: f32,
    border_type: i64,
    render: bool,
) {
    if height > 0.0 {
        let mut r = BorderRect::new(left, top, right, top + height, color);
        r.border_type = border_type;
        r.horizontal = true;
        r.render = render;
        rects.push(r);
    }
}

/// `vertical_edge()` from `borders.py`.
#[allow(clippy::too_many_arguments)]
pub fn vertical_edge(
    rects: &mut Vec<BorderRect>,
    color: BorderColor,
    width: f32,
    top: f32,
    bottom: f32,
    left: f32,
    border_type: i64,
    render: bool,
) {
    if width > 0.0 {
        let mut r = BorderRect::new(left, top, left + width, bottom, color);
        r.border_type = border_type;
        r.horizontal = false;
        r.render = render;
        rects.push(r);
    }
}

/// `add_borders()` from `borders.py`.
///
/// The four straight edges are emitted first, then, when `radius > 0`, the
/// full rounded rectangle placeholder with `render = false`. A backend draws
/// that placeholder with the rounded-rect shader instead of the edges.
pub fn add_borders(
    rects: &mut Vec<BorderRect>,
    color: BorderColor,
    geometry: &WindowGeometry,
    padding: EdgeWidths,
    border_width: f32,
    radius: u32,
    window_id: i64,
) {
    let c = geometry.compensatory;
    let pl = padding.left + c.left;
    let pt = padding.top + c.top;
    let pr = padding.right + c.right;
    let pb = padding.bottom + c.bottom;

    let mut left = geometry.left - pl;
    let mut top = geometry.top - pt;
    let lr = geometry.right;
    let mut right = lr + pr;
    let bt = geometry.bottom;
    let mut bottom = bt + pb;

    let width = border_width;
    left -= width;
    top -= width;
    right += width;
    bottom += width;

    let render_edges = radius < 1;
    horizontal_edge(
        rects,
        color,
        width,
        left,
        right,
        top,
        -window_id,
        render_edges,
    );
    horizontal_edge(
        rects,
        color,
        width,
        left,
        right,
        bt,
        window_id,
        render_edges,
    );
    vertical_edge(
        rects,
        color,
        width,
        top,
        bottom,
        left,
        -window_id,
        render_edges,
    );
    vertical_edge(
        rects,
        color,
        width,
        top,
        bottom,
        lr,
        window_id,
        render_edges,
    );

    if radius > 0 {
        let mut r = BorderRect::new(left, top, right, bottom, color);
        r.render = false;
        r.radius = radius;
        r.thickness = width.max(0.0).round() as u32;
        rects.push(r);
    }
}

/// A visible pane rectangle used to derive split borders.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneBorderPane {
    pub rect: Rect,
    pub active: bool,
    pub window_id: i64,
}

/// One split-border strip, coloured according to the pane on whose side it
/// lies. Neighbouring panes each contribute their own half of the 2x border
/// gap, matching `Pair.layout_pair()` in `kitty/layout/splits.py`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneBorderLine {
    pub rect: Rect,
    pub active: bool,
    pub window_id: i64,
}

/// Compute minimal split borders for visible pane rectangles.
///
/// This mirrors kitty's `draw_minimal_borders`: a pane only gets a border on
/// an edge that touches another pane. The strip is placed just outside the
/// pane, so two adjacent panes produce two contiguous thickness-wide strips.
/// When `draw_all` is set (kitty's non-minimal mode), all four edges of every
/// pane are returned instead.
pub fn pane_border_rects(
    panes: &[PaneBorderPane],
    thickness: f32,
    draw_all: bool,
) -> Vec<PaneBorderLine> {
    let mut ans = Vec::new();
    if thickness <= 0.0 || panes.is_empty() {
        return ans;
    }
    let t = thickness;
    for (i, pane) in panes.iter().enumerate() {
        let r = pane.rect;
        if draw_all {
            ans.push(PaneBorderLine {
                rect: Rect::new(r.x, r.y - t, r.width, t),
                active: pane.active,
                window_id: pane.window_id,
            });
            ans.push(PaneBorderLine {
                rect: Rect::new(r.x, r.bottom(), r.width, t),
                active: pane.active,
                window_id: pane.window_id,
            });
            ans.push(PaneBorderLine {
                rect: Rect::new(r.x - t, r.y, t, r.height),
                active: pane.active,
                window_id: pane.window_id,
            });
            ans.push(PaneBorderLine {
                rect: Rect::new(r.right(), r.y, t, r.height),
                active: pane.active,
                window_id: pane.window_id,
            });
            continue;
        }
        for (j, other) in panes.iter().enumerate() {
            if i == j {
                continue;
            }
            let o = other.rect;
            let overlap_y = r.y.max(o.y) < r.bottom().min(o.bottom());
            let overlap_x = r.x.max(o.x) < r.right().min(o.right());
            // Horizontal neighbour: one pane's right edge meets the other's
            // left edge (allow a small tolerance for rounded pixel geometry).
            let horizontal = overlap_y && (r.right() - o.x).abs() <= t;
            let horizontal_rev = overlap_y && (o.right() - r.x).abs() <= t;
            if horizontal {
                let top = r.y.max(o.y);
                let bottom = r.bottom().min(o.bottom());
                ans.push(PaneBorderLine {
                    rect: Rect::new(r.right(), top, t, bottom - top),
                    active: pane.active,
                    window_id: pane.window_id,
                });
            }
            if horizontal_rev {
                let top = r.y.max(o.y);
                let bottom = r.bottom().min(o.bottom());
                ans.push(PaneBorderLine {
                    rect: Rect::new(r.x - t, top, t, bottom - top),
                    active: pane.active,
                    window_id: pane.window_id,
                });
            }
            let vertical = overlap_x && (r.bottom() - o.y).abs() <= t;
            let vertical_rev = overlap_x && (o.bottom() - r.y).abs() <= t;
            if vertical {
                let left = r.x.max(o.x);
                let right = r.right().min(o.right());
                ans.push(PaneBorderLine {
                    rect: Rect::new(left, r.bottom(), right - left, t),
                    active: pane.active,
                    window_id: pane.window_id,
                });
            }
            if vertical_rev {
                let left = r.x.max(o.x);
                let right = r.right().min(o.right());
                ans.push(PaneBorderLine {
                    rect: Rect::new(left, r.y - t, right - left, t),
                    active: pane.active,
                    window_id: pane.window_id,
                });
            }
        }
    }
    ans
}

/// The edge a tab bar is attached to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// Inputs for `TabBar.update_blank_rects()`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabBarBlankRectConfig {
    pub central: Region,
    pub tab_bar: Region,
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub window_geometry: Region,
    pub is_vertical: bool,
    pub edge: Edge,
    pub margin_outer: bool,
    pub margin_inner: bool,
    /// True when `tab_bar_margin_color` is unset.
    pub margin_color_is_none: bool,
    pub left_edge_is_default: bool,
    pub right_edge_is_default: bool,
}

/// `TabBar.update_blank_rects()` from `tab_bar.py`.
pub fn tab_bar_blank_rects(cfg: &TabBarBlankRectConfig) -> Vec<BorderRect> {
    let mut rects = Vec::new();
    let bg = if cfg.margin_color_is_none {
        BorderColor::DefaultBackground
    } else {
        BorderColor::TabBarMargin
    };
    let tb = cfg.tab_bar;
    let central = cfg.central;
    let vw = cfg.viewport_width;
    let vh = cfg.viewport_height;

    if cfg.is_vertical {
        match cfg.edge {
            Edge::Left => {
                if cfg.margin_outer {
                    rects.push(BorderRect::new(0.0, 0.0, tb.left, vh, bg));
                }
                if cfg.margin_inner {
                    rects.push(BorderRect::new(tb.right, 0.0, central.left, vh, bg));
                }
            }
            _ => {
                if cfg.margin_outer {
                    rects.push(BorderRect::new(tb.right, 0.0, vw, vh, bg));
                }
                if cfg.margin_inner {
                    rects.push(BorderRect::new(central.right, 0.0, tb.left, vh, bg));
                }
            }
        }
    } else if cfg.edge == Edge::Bottom {
        if cfg.margin_outer {
            rects.push(BorderRect::new(0.0, tb.bottom, vw, vh, bg));
        }
        if cfg.margin_inner {
            rects.push(BorderRect::new(0.0, central.bottom, vw, tb.top, bg));
        }
    } else {
        if cfg.margin_outer {
            rects.push(BorderRect::new(0.0, 0.0, vw, tb.top, bg));
        }
        if cfg.margin_inner {
            rects.push(BorderRect::new(0.0, tb.bottom, vw, central.top, bg));
        }
    }

    let g = cfg.window_geometry;
    if cfg.is_vertical {
        let (top_bg, bottom_bg) = if cfg.margin_color_is_none {
            (
                if cfg.left_edge_is_default {
                    bg
                } else {
                    BorderColor::TabBarLeftEdge
                },
                if cfg.right_edge_is_default {
                    bg
                } else {
                    BorderColor::TabBarRightEdge
                },
            )
        } else {
            (bg, bg)
        };
        if g.left > tb.left {
            rects.push(BorderRect::new(tb.left, g.top, g.left, g.bottom, bg));
        }
        if g.right < tb.right {
            rects.push(BorderRect::new(g.right, g.top, tb.right, g.bottom, bg));
        }
        if g.top > tb.top {
            rects.push(BorderRect::new(g.left, tb.top, g.right, g.top, top_bg));
        }
        if g.bottom < tb.bottom {
            rects.push(BorderRect::new(
                g.left, g.bottom, g.right, tb.bottom, bottom_bg,
            ));
        }
    } else {
        let (left_bg, right_bg) = if cfg.margin_color_is_none {
            (
                if cfg.left_edge_is_default {
                    bg
                } else {
                    BorderColor::TabBarLeftEdge
                },
                if cfg.right_edge_is_default {
                    bg
                } else {
                    BorderColor::TabBarRightEdge
                },
            )
        } else {
            (bg, bg)
        };
        if g.left > tb.left {
            rects.push(BorderRect::new(tb.left, g.top, g.left, g.bottom, left_bg));
        }
        if g.right < tb.right {
            rects.push(BorderRect::new(
                g.right, g.top, tb.right, g.bottom, right_bg,
            ));
        }
        if g.top > tb.top {
            rects.push(BorderRect::new(g.left, tb.top, g.right, g.top, bg));
        }
        if g.bottom < tb.bottom {
            rects.push(BorderRect::new(g.left, g.bottom, g.right, tb.bottom, bg));
        }
    }
    rects
}

// }}}

// {{{ Padding and rounded rectangles

/// The strips between an outer rectangle and an inner rectangle. This is used
/// for window/tab-bar padding and background gaps.
pub fn padding_rects_between(outer: Rect, inner: Rect) -> Vec<Rect> {
    let mut ans = Vec::new();
    let left = inner.x.max(outer.x);
    let right = inner.right().min(outer.right());
    let top = inner.y.max(outer.y);
    let bottom = inner.bottom().min(outer.bottom());
    let top_h = top - outer.y;
    let bottom_h = outer.bottom() - bottom;
    if top_h > 0.0 {
        ans.push(Rect::new(outer.x, outer.y, outer.width, top_h));
    }
    if bottom_h > 0.0 {
        ans.push(Rect::new(outer.x, bottom, outer.width, bottom_h));
    }
    if right > left {
        if left > outer.x {
            ans.push(Rect::new(
                outer.x,
                top,
                left - outer.x,
                (bottom - top).max(0.0),
            ));
        }
        if outer.right() > right {
            ans.push(Rect::new(
                right,
                top,
                outer.right() - right,
                (bottom - top).max(0.0),
            ));
        }
    }
    ans
}

/// Padding strips for an outer rectangle, using `padding` as the inset.
pub fn padding_rects(outer: Rect, padding: EdgeWidths) -> Vec<Rect> {
    let inner = Rect::new(
        outer.x + padding.left,
        outer.y + padding.top,
        (outer.width - padding.left - padding.right).max(0.0),
        (outer.height - padding.top - padding.bottom).max(0.0),
    );
    padding_rects_between(outer, inner)
}

/// A full-window background rectangle with a border colour index.
pub fn background_rect(rect: Rect, color: BorderColor) -> BorderRect {
    BorderRect::new(rect.x, rect.y, rect.right(), rect.bottom(), color)
}

/// A vertex for rounded-rectangle tessellation.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vertex {
    pub x: f32,
    pub y: f32,
}

impl Vertex {
    #[inline]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// A triangle for rounded-rectangle tessellation.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Triangle {
    pub a: Vertex,
    pub b: Vertex,
    pub c: Vertex,
}

fn push_quad(triangles: &mut Vec<Triangle>, a: Vertex, b: Vertex, c: Vertex, d: Vertex) {
    triangles.push(Triangle { a, b, c });
    triangles.push(Triangle { a, b: c, c: d });
}

fn corner_points(
    out: &mut Vec<Vertex>,
    cx: f32,
    cy: f32,
    r: f32,
    start: f32,
    end: f32,
    segments: u32,
) {
    for i in 0..=segments {
        let t = start + (end - start) * i as f32 / segments as f32;
        out.push(Vertex::new(cx + r * t.cos(), cy + r * t.sin()));
    }
}

/// Boundary points of a rounded rectangle in clockwise order.
pub fn rounded_rect_points(rect: Rect, radius: f32, segments_per_corner: u32) -> Vec<Vertex> {
    if rect.is_empty() {
        return Vec::new();
    }
    let radius = radius.max(0.0).min(rect.width.min(rect.height) / 2.0);
    let segments = segments_per_corner.max(1);
    if radius <= f32::EPSILON {
        return vec![
            Vertex::new(rect.x, rect.y),
            Vertex::new(rect.right(), rect.y),
            Vertex::new(rect.right(), rect.bottom()),
            Vertex::new(rect.x, rect.bottom()),
        ];
    }
    let pi = core::f32::consts::PI;
    let mut points = Vec::with_capacity(4 * (segments as usize + 1));
    corner_points(
        &mut points,
        rect.x + radius,
        rect.y + radius,
        radius,
        pi,
        1.5 * pi,
        segments,
    );
    corner_points(
        &mut points,
        rect.right() - radius,
        rect.y + radius,
        radius,
        1.5 * pi,
        2.0 * pi,
        segments,
    );
    corner_points(
        &mut points,
        rect.right() - radius,
        rect.bottom() - radius,
        radius,
        0.0,
        0.5 * pi,
        segments,
    );
    corner_points(
        &mut points,
        rect.x + radius,
        rect.bottom() - radius,
        radius,
        0.5 * pi,
        pi,
        segments,
    );
    points
}

/// Tessellate a filled rounded rectangle as a triangle fan.
pub fn tessellate_rounded_rect_fill(
    rect: Rect,
    radius: f32,
    segments_per_corner: u32,
) -> Vec<Triangle> {
    let points = rounded_rect_points(rect, radius, segments_per_corner);
    if points.len() < 3 {
        return Vec::new();
    }
    let center = Vertex::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    let mut triangles = Vec::with_capacity(points.len());
    for i in 0..points.len() {
        triangles.push(Triangle {
            a: center,
            b: points[i],
            c: points[(i + 1) % points.len()],
        });
    }
    triangles
}

/// Tessellate a rounded-rectangle border as a ring of quads (two triangles
/// each) between the outer and inner outlines.
pub fn tessellate_rounded_rect_border(
    rect: Rect,
    radius: f32,
    thickness: f32,
    segments_per_corner: u32,
) -> Vec<Triangle> {
    if rect.is_empty() || thickness <= 0.0 {
        return Vec::new();
    }
    let max_thickness = rect.width.min(rect.height) / 2.0;
    let thickness = thickness.min(max_thickness);
    let radius = radius.max(0.0).min(max_thickness);
    let inner_rect = Rect::new(
        rect.x + thickness,
        rect.y + thickness,
        rect.width - 2.0 * thickness,
        rect.height - 2.0 * thickness,
    );
    if inner_rect.is_empty() {
        return Vec::new();
    }
    let inner_radius = (radius - thickness).max(0.0);
    let outer = rounded_rect_points(rect, radius, segments_per_corner);
    let inner = rounded_rect_points(inner_rect, inner_radius, segments_per_corner);
    if outer.len() != inner.len() || outer.len() < 2 {
        return Vec::new();
    }
    let mut triangles = Vec::with_capacity(outer.len() * 2);
    for i in 0..outer.len() {
        let j = (i + 1) % outer.len();
        push_quad(&mut triangles, outer[i], outer[j], inner[j], inner[i]);
    }
    triangles
}

// }}}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> FontCellMetrics {
        FontCellMetrics {
            cell_width: 10,
            cell_height: 20,
            baseline: 15,
            underline_position: 18,
            underline_thickness: 2,
            strikethrough_position: 10,
            strikethrough_thickness: 2,
        }
    }

    #[test]
    fn straight_and_strike_offsets() {
        let m = metrics();
        let g = straight_underline_geometry(m);
        assert_eq!(g, DecorationGeometry { top: 17, height: 2 });
        let rects = underline_rects(UnderlineStyle::Straight, m, 0);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0], Rect::new(0.0, 17.0, 10.0, 1.0));
        assert_eq!(rects[1], Rect::new(0.0, 18.0, 10.0, 1.0));
        let sg = strikethrough_geometry(m);
        assert_eq!(sg, DecorationGeometry { top: 9, height: 2 });
        assert_eq!(strikethrough_rects(m).len(), 2);
    }

    #[test]
    fn zero_thickness_decorations_are_empty() {
        let mut m = metrics();
        m.underline_thickness = 0;
        m.strikethrough_thickness = 0;
        assert!(underline_rects(UnderlineStyle::Straight, m, 0).is_empty());
        assert!(strikethrough_rects(m).is_empty());
        m.underline_position = 100;
        m.underline_thickness = 2;
        assert!(underline_rects(UnderlineStyle::Straight, m, 0).is_empty());
    }

    #[test]
    fn double_underline_has_two_rows_and_gap() {
        let m = metrics();
        let g = double_underline_geometry(m);
        assert_eq!(g, DecorationGeometry { top: 16, height: 3 });
        let rects = underline_rects(UnderlineStyle::Double, m, 0);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].y, 16.0);
        assert_eq!(rects[1].y, 18.0);
    }

    #[test]
    fn dotted_and_dashed_generate_multiple_runs() {
        let m = metrics();
        let dotted = underline_rects(UnderlineStyle::Dotted, m, 0);
        assert!(dotted.len() >= 4, "{dotted:?}");
        assert!(dotted.iter().all(|r| r.right() <= 10.0 && r.y >= 17.0));
        let dashed = underline_rects(UnderlineStyle::Dashed, m, 0);
        assert_eq!(dashed.len(), 4);
        assert_eq!(dashed[0], Rect::new(0.0, 17.0, 4.0, 1.0));
        assert_eq!(dashed[1], Rect::new(6.0, 17.0, 4.0, 1.0));
    }

    #[test]
    fn curly_underline_stays_in_cell() {
        let m = metrics();
        let (mask, geometry) = curly_underline_mask(m, 0);
        assert!(!mask.data.is_empty());
        assert!(geometry.top + geometry.height <= m.cell_height);
        let rects = underline_rects(UnderlineStyle::Curly, m, 0);
        assert!(!rects.is_empty());
        for r in rects {
            assert!(r.x >= 0.0 && r.right() <= 10.0);
            assert!(r.y >= 0.0 && r.bottom() <= 20.0);
        }
        let (_, tighter) = curly_underline_mask(m, 3);
        assert!(tighter.height > 0);
    }

    #[test]
    fn cursor_rects() {
        let m = metrics();
        let (beam, _) = beam_cursor_rect(m, 2.0, 72.0);
        assert_eq!(beam.width, 2.0);
        assert_eq!(beam.height, 20.0);
        let (under, g) = underline_cursor_rect(m, 2.0, 72.0);
        assert_eq!(under.y, 18.0);
        assert_eq!(g.top, 18);
        let (hollow, _) = hollow_cursor_rects(m, 72.0, 72.0);
        assert_eq!(hollow[0].width, 1.0);
        assert_eq!(hollow[2].height, 1.0);
        assert_eq!(hollow[3].y, 19.0);
    }

    #[test]
    fn window_border_geometry_matches_python() {
        let geometry = WindowGeometry::new(10.0, 20.0, 110.0, 120.0);
        let padding = EdgeWidths::uniform(1.0);
        let mut rects = Vec::new();
        add_borders(
            &mut rects,
            BorderColor::Active,
            &geometry,
            padding,
            1.0,
            0,
            7,
        );
        let expected = [
            BorderRect {
                left: 8.0,
                top: 18.0,
                right: 112.0,
                bottom: 19.0,
                color: BorderColor::Active,
                border_type: -7,
                horizontal: true,
                render: true,
                radius: 0,
                thickness: 0,
            },
            BorderRect {
                left: 8.0,
                top: 120.0,
                right: 112.0,
                bottom: 121.0,
                color: BorderColor::Active,
                border_type: 7,
                horizontal: true,
                render: true,
                radius: 0,
                thickness: 0,
            },
            BorderRect {
                left: 8.0,
                top: 18.0,
                right: 9.0,
                bottom: 122.0,
                color: BorderColor::Active,
                border_type: -7,
                horizontal: false,
                render: true,
                radius: 0,
                thickness: 0,
            },
            BorderRect {
                left: 110.0,
                top: 18.0,
                right: 111.0,
                bottom: 122.0,
                color: BorderColor::Active,
                border_type: 7,
                horizontal: false,
                render: true,
                radius: 0,
                thickness: 0,
            },
        ];
        assert_eq!(rects, expected);
    }

    #[test]
    fn rounded_border_uses_placeholder() {
        let geometry = WindowGeometry::new(10.0, 20.0, 110.0, 120.0);
        let mut rects = Vec::new();
        add_borders(
            &mut rects,
            BorderColor::Active,
            &geometry,
            EdgeWidths::uniform(1.0),
            1.0,
            12,
            7,
        );
        assert_eq!(rects.len(), 5);
        assert!(rects[..4].iter().all(|r| !r.render));
        let rounded = rects[4];
        assert!(!rounded.render);
        assert_eq!(rounded.radius, 12);
        assert_eq!(rounded.thickness, 1);
        assert_eq!(
            (rounded.left, rounded.top, rounded.right, rounded.bottom),
            (8.0, 18.0, 112.0, 122.0)
        );
    }

    #[test]
    fn zero_margins_and_zero_border() {
        let geometry = WindowGeometry::new(0.0, 0.0, 100.0, 100.0);
        let mut rects = Vec::new();
        add_borders(
            &mut rects,
            BorderColor::Inactive,
            &geometry,
            EdgeWidths::default(),
            0.0,
            0,
            1,
        );
        assert!(rects.is_empty());

        add_borders(
            &mut rects,
            BorderColor::Inactive,
            &geometry,
            EdgeWidths::default(),
            0.0,
            4,
            1,
        );
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].thickness, 0);
    }

    #[test]
    fn tab_bar_blank_rects_top_and_vertical() {
        let cfg = TabBarBlankRectConfig {
            central: Region::new(0.0, 40.0, 800.0, 600.0),
            tab_bar: Region::new(0.0, 5.0, 800.0, 35.0),
            viewport_width: 800.0,
            viewport_height: 600.0,
            window_geometry: Region::new(0.0, 5.0, 800.0, 35.0),
            is_vertical: false,
            edge: Edge::Top,
            margin_outer: true,
            margin_inner: true,
            margin_color_is_none: true,
            left_edge_is_default: true,
            right_edge_is_default: true,
        };
        let rects = tab_bar_blank_rects(&cfg);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].rect(), Rect::new(0.0, 0.0, 800.0, 5.0));
        assert_eq!(rects[1].rect(), Rect::new(0.0, 35.0, 800.0, 5.0));

        let cfg = TabBarBlankRectConfig {
            is_vertical: true,
            edge: Edge::Left,
            central: Region::new(40.0, 0.0, 800.0, 600.0),
            tab_bar: Region::new(5.0, 0.0, 35.0, 600.0),
            window_geometry: Region::new(5.0, 0.0, 35.0, 600.0),
            ..cfg
        };
        let rects = tab_bar_blank_rects(&cfg);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].rect(), Rect::new(0.0, 0.0, 5.0, 600.0));
        assert_eq!(rects[1].rect(), Rect::new(35.0, 0.0, 5.0, 600.0));
    }

    #[test]
    fn minimal_pane_borders_only_touch_neighbours() {
        let panes = [
            PaneBorderPane {
                rect: Rect::new(0.0, 0.0, 400.0, 300.0),
                active: true,
                window_id: 1,
            },
            PaneBorderPane {
                rect: Rect::new(400.0, 0.0, 400.0, 300.0),
                active: false,
                window_id: 2,
            },
        ];
        let lines = pane_border_rects(&panes, 2.0, false);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].rect, Rect::new(400.0, 0.0, 2.0, 300.0));
        assert!(lines[0].active);
        assert_eq!(lines[1].rect, Rect::new(398.0, 0.0, 2.0, 300.0));
        assert!(!lines[1].active);

        let single = [panes[0]];
        assert!(pane_border_rects(&single, 2.0, false).is_empty());
        let full = pane_border_rects(&single, 2.0, true);
        assert_eq!(full.len(), 4);
        assert_eq!(full[0].rect, Rect::new(0.0, -2.0, 400.0, 2.0));
        assert_eq!(full[1].rect, Rect::new(0.0, 300.0, 400.0, 2.0));
        assert_eq!(full[2].rect, Rect::new(-2.0, 0.0, 2.0, 300.0));
        assert_eq!(full[3].rect, Rect::new(400.0, 0.0, 2.0, 300.0));
        assert!(pane_border_rects(&panes, 0.0, true).is_empty());
    }

    #[test]
    fn pane_borders_support_vertical_splits() {
        let panes = [
            PaneBorderPane {
                rect: Rect::new(0.0, 0.0, 300.0, 200.0),
                active: false,
                window_id: 1,
            },
            PaneBorderPane {
                rect: Rect::new(0.0, 200.0, 300.0, 100.0),
                active: true,
                window_id: 2,
            },
        ];
        let lines = pane_border_rects(&panes, 1.0, false);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].rect, Rect::new(0.0, 200.0, 300.0, 1.0));
        assert_eq!(lines[1].rect, Rect::new(0.0, 199.0, 300.0, 1.0));
    }

    #[test]
    fn padding_strips() {
        let rects = padding_rects(Rect::new(0.0, 0.0, 100.0, 50.0), EdgeWidths::uniform(5.0));
        assert_eq!(rects.len(), 4);
        assert_eq!(rects[0], Rect::new(0.0, 0.0, 100.0, 5.0));
        assert_eq!(rects[1], Rect::new(0.0, 45.0, 100.0, 5.0));
        assert_eq!(rects[2], Rect::new(0.0, 5.0, 5.0, 40.0));
        assert_eq!(rects[3], Rect::new(95.0, 5.0, 5.0, 40.0));
        assert!(padding_rects(Rect::new(0.0, 0.0, 100.0, 50.0), EdgeWidths::default()).is_empty());
    }

    #[test]
    fn rounded_rect_tessellation() {
        let rect = Rect::new(0.0, 0.0, 100.0, 50.0);
        let border = tessellate_rounded_rect_border(rect, 10.0, 2.0, 4);
        assert_eq!(border.len(), 4 * 5 * 2);
        for t in &border {
            for v in [t.a, t.b, t.c] {
                assert!(v.x >= -1e-4 && v.x <= 100.0 + 1e-4);
                assert!(v.y >= -1e-4 && v.y <= 50.0 + 1e-4);
            }
        }
        let fill = tessellate_rounded_rect_fill(rect, 10.0, 4);
        assert_eq!(fill.len(), 20);
        let square_border = tessellate_rounded_rect_border(rect, 0.0, 2.0, 4);
        assert_eq!(square_border.len(), 8);
        assert!(tessellate_rounded_rect_border(rect, 10.0, 0.0, 4).is_empty());
        assert!(tessellate_rounded_rect_border(Rect::default(), 10.0, 2.0, 4).is_empty());
        let points = rounded_rect_points(rect, 10.0, 2);
        assert_eq!(points.len(), 12);
        assert_eq!(rounded_rect_points(rect, 0.0, 2).len(), 4);
    }
}
