// SPDX-License-Identifier: GPL-3.0-or-later
//! Small CPU-side geometry primitives shared by the renderer.
//!
//! All coordinates are in device pixels with the origin at the top-left of the
//! window. The OpenGL backend is responsible for flipping these into its own
//! coordinate system.

use kitty_fonts::FontCellMetrics;

/// A left/top/width/height rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    #[inline]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Build a rectangle from its edges.
    #[inline]
    pub const fn from_ltrb(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self::new(left, top, right - left, bottom - top)
    }

    #[inline]
    pub const fn right(self) -> f32 {
        self.x + self.width
    }

    #[inline]
    pub const fn bottom(self) -> f32 {
        self.y + self.height
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    /// A rectangle translated by `(dx, dy)`.
    #[inline]
    pub const fn translated(self, dx: f32, dy: f32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.width, self.height)
    }

    /// Whether the two rectangles overlap. Touching edges do not count.
    #[inline]
    pub fn intersects(self, other: Self) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// A rectangle described by its four edges.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Region {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Region {
    #[inline]
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
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

impl From<Rect> for Region {
    fn from(r: Rect) -> Self {
        Self::new(r.x, r.y, r.right(), r.bottom())
    }
}

/// Per-edge widths used for padding and margins.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EdgeWidths {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl EdgeWidths {
    #[inline]
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    #[inline]
    pub const fn uniform(value: f32) -> Self {
        Self::new(value, value, value, value)
    }

    #[inline]
    pub const fn is_zero(self) -> bool {
        self.left == 0.0 && self.top == 0.0 && self.right == 0.0 && self.bottom == 0.0
    }
}

/// Cell metrics in pixels.
///
/// This is the floating point form of [`FontCellMetrics`], which is what the
/// renderer needs for fractional layout. Conversion preserves the integer
/// pixel values computed by `kitty-fonts`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellMetrics {
    pub cell_width: f32,
    pub cell_height: f32,
    pub baseline: f32,
    pub underline_position: f32,
    pub underline_thickness: f32,
    pub strikethrough_position: f32,
    pub strikethrough_thickness: f32,
}

impl Default for CellMetrics {
    fn default() -> Self {
        Self {
            cell_width: 10.0,
            cell_height: 20.0,
            baseline: 15.0,
            underline_position: 18.0,
            underline_thickness: 1.0,
            strikethrough_position: 10.0,
            strikethrough_thickness: 1.0,
        }
    }
}

impl From<FontCellMetrics> for CellMetrics {
    fn from(m: FontCellMetrics) -> Self {
        Self {
            cell_width: m.cell_width as f32,
            cell_height: m.cell_height as f32,
            baseline: m.baseline as f32,
            underline_position: m.underline_position as f32,
            underline_thickness: m.underline_thickness as f32,
            strikethrough_position: m.strikethrough_position as f32,
            strikethrough_thickness: m.strikethrough_thickness as f32,
        }
    }
}

impl From<CellMetrics> for FontCellMetrics {
    fn from(m: CellMetrics) -> Self {
        Self {
            cell_width: m.cell_width.max(0.0).round() as u32,
            cell_height: m.cell_height.max(0.0).round() as u32,
            baseline: m.baseline.max(0.0).round() as u32,
            underline_position: m.underline_position.max(0.0).round() as u32,
            underline_thickness: m.underline_thickness.max(0.0).round() as u32,
            strikethrough_position: m.strikethrough_position.max(0.0).round() as u32,
            strikethrough_thickness: m.strikethrough_thickness.max(0.0).round() as u32,
        }
    }
}

/// The cell grid used to place render instances.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GridGeometry {
    pub columns: u32,
    pub rows: u32,
    pub cell_width: f32,
    pub cell_height: f32,
    pub origin_x: f32,
    pub origin_y: f32,
}

impl GridGeometry {
    #[inline]
    pub const fn new(columns: u32, rows: u32, metrics: CellMetrics) -> Self {
        Self {
            columns,
            rows,
            cell_width: metrics.cell_width,
            cell_height: metrics.cell_height,
            origin_x: 0.0,
            origin_y: 0.0,
        }
    }

    #[inline]
    pub const fn with_origin(mut self, x: f32, y: f32) -> Self {
        self.origin_x = x;
        self.origin_y = y;
        self
    }

    #[inline]
    pub const fn cell_rect(self, x: u32, y: u32) -> Rect {
        Rect::new(
            self.origin_x + x as f32 * self.cell_width,
            self.origin_y + y as f32 * self.cell_height,
            self.cell_width,
            self.cell_height,
        )
    }

    #[inline]
    pub const fn cell_rect_with_width(self, x: u32, y: u32, width_cells: u32) -> Rect {
        Rect::new(
            self.origin_x + x as f32 * self.cell_width,
            self.origin_y + y as f32 * self.cell_height,
            self.cell_width * width_cells as f32,
            self.cell_height,
        )
    }

    #[inline]
    pub const fn width_px(self) -> f32 {
        self.cell_width * self.columns as f32
    }

    #[inline]
    pub const fn height_px(self) -> f32 {
        self.cell_height * self.rows as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_basics() {
        let r = Rect::from_ltrb(1.0, 2.0, 11.0, 22.0);
        assert_eq!(r, Rect::new(1.0, 2.0, 10.0, 20.0));
        assert_eq!(r.right(), 11.0);
        assert_eq!(r.bottom(), 22.0);
        assert!(r.intersects(Rect::new(10.0, 21.0, 2.0, 2.0)));
        assert!(!r.intersects(Rect::new(11.0, 2.0, 2.0, 2.0)));
    }

    #[test]
    fn cell_rects() {
        let g = GridGeometry::new(80, 24, CellMetrics::default()).with_origin(4.0, 6.0);
        assert_eq!(g.cell_rect(0, 0), Rect::new(4.0, 6.0, 10.0, 20.0));
        assert_eq!(g.cell_rect(2, 3), Rect::new(24.0, 66.0, 10.0, 20.0));
        assert_eq!(
            g.cell_rect_with_width(1, 1, 2),
            Rect::new(14.0, 26.0, 20.0, 20.0)
        );
    }

    #[test]
    fn font_metrics_roundtrip() {
        let m = FontCellMetrics {
            cell_width: 10,
            cell_height: 20,
            baseline: 15,
            underline_position: 18,
            underline_thickness: 2,
            strikethrough_position: 9,
            strikethrough_thickness: 1,
        };
        let c = CellMetrics::from(m);
        assert_eq!(FontCellMetrics::from(c), m);
    }
}
