// SPDX-License-Identifier: GPL-3.0-or-later
//! Simple cursor-trail geometry.
//!
//! Kitty's full cursor trail animates four corners with distance-weighted
//! exponential decay and supports custom shaders (`kitty/shaders/trail.slang`,
//! `kitty/cursor_trail.c`). This port keeps the useful part for a terminal
//! compositor: remember recent cursor positions and fade them out over a
//! configurable time. It is intentionally not a line-for-line port; see the
//! crate-level documentation for the differences.
//!
//! The trail is entirely CPU-side. [`CursorTrail::update`] produces
//! [`TrailInstance`]s that the `gl` backend draws as translucent solid quads.

use std::collections::VecDeque;

use crate::colors::Rgba8;
use crate::geom::Rect;

/// Cursor-trail configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorTrailConfig {
    /// Enable or disable the trail entirely.
    pub enabled: bool,
    /// Maximum number of remembered cursor positions.
    pub length: usize,
    /// Time in seconds for a remembered position to fade completely.
    pub fade_seconds: f32,
    /// Trail colour. `None` uses the resolver's cursor background colour.
    pub color: Option<Rgba8>,
}

impl Default for CursorTrailConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            length: 8,
            fade_seconds: 0.25,
            color: None,
        }
    }
}

impl CursorTrailConfig {
    pub fn enabled(length: usize, fade_seconds: f32) -> Self {
        Self {
            enabled: true,
            length: length.max(1),
            fade_seconds: fade_seconds.max(0.001),
            color: None,
        }
    }

    pub fn with_color(mut self, color: Rgba8) -> Self {
        self.color = Some(color);
        self
    }
}

/// One fading quad emitted by the trail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrailInstance {
    pub rect: Rect,
    pub color: Rgba8,
    pub opacity: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TrailPoint {
    rect: Rect,
    at: f32,
}

/// Remembered cursor positions.
#[derive(Clone, Debug, Default)]
pub struct CursorTrail {
    points: VecDeque<TrailPoint>,
    last_cell: Option<(u32, u32)>,
    last_rect: Option<Rect>,
}

impl CursorTrail {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget all remembered positions.
    pub fn clear(&mut self) {
        self.points.clear();
        self.last_cell = None;
        self.last_rect = None;
    }

    /// Number of remembered positions (including ones that have not started
    /// fading yet).
    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Update the trail for a frame and append visible quads to `out`.
    ///
    /// `cursor_rect` is `None` when the cursor is hidden. Existing points keep
    /// fading even while the cursor is hidden. When the cursor moves, the
    /// previous cell rectangle is remembered; it then fades over
    /// [`CursorTrailConfig::fade_seconds`]. This is a deliberately simple
    /// model of kitty's corner animation.
    pub fn update(
        &mut self,
        cursor_rect: Option<Rect>,
        cursor_cell: Option<(u32, u32)>,
        now: f32,
        config: &CursorTrailConfig,
        color: Rgba8,
        out: &mut Vec<TrailInstance>,
    ) {
        if !config.enabled || config.length == 0 || config.fade_seconds <= 0.0 {
            self.clear();
            return;
        }

        match (cursor_cell, cursor_rect) {
            (Some(cell), Some(rect)) => {
                if let Some(last) = self.last_cell {
                    if last != cell {
                        if let Some(previous) = self.last_rect {
                            self.points.push_front(TrailPoint {
                                rect: previous,
                                at: now,
                            });
                        }
                    }
                }
                self.last_cell = Some(cell);
                self.last_rect = Some(rect);
            }
            _ => {
                self.last_cell = None;
                self.last_rect = None;
            }
        }

        while self.points.len() > config.length {
            self.points.pop_back();
        }
        while self
            .points
            .back()
            .is_some_and(|point| now - point.at >= config.fade_seconds)
        {
            self.points.pop_back();
        }

        // Oldest first so later quads draw over earlier ones.
        for point in self.points.iter().rev() {
            let age = (now - point.at).max(0.0);
            let opacity = 1.0 - age / config.fade_seconds;
            if opacity <= 0.0 {
                continue;
            }
            out.push(TrailInstance {
                rect: point.rect,
                color,
                opacity: opacity.clamp(0.0, 1.0),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32) -> Rect {
        Rect::new(x, y, 10.0, 20.0)
    }

    #[test]
    fn trail_records_moves_and_fades_out() {
        let config = CursorTrailConfig::enabled(4, 1.0);
        let mut trail = CursorTrail::new();
        let mut out = Vec::new();

        trail.update(
            Some(rect(0.0, 0.0)),
            Some((0, 0)),
            0.0,
            &config,
            Rgba8::WHITE,
            &mut out,
        );
        assert!(out.is_empty(), "the first position only seeds the trail");

        trail.update(
            Some(rect(10.0, 0.0)),
            Some((1, 0)),
            0.1,
            &config,
            Rgba8::WHITE,
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rect, rect(0.0, 0.0));
        // The remembered position starts fading when the move is observed.
        assert!((out[0].opacity - 1.0).abs() < 1e-6);

        out.clear();
        trail.update(
            Some(rect(10.0, 0.0)),
            Some((1, 0)),
            0.6,
            &config,
            Rgba8::WHITE,
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert!((out[0].opacity - 0.5).abs() < 1e-6);

        out.clear();
        trail.update(
            Some(rect(10.0, 0.0)),
            Some((1, 0)),
            1.2,
            &config,
            Rgba8::WHITE,
            &mut out,
        );
        assert!(out.is_empty());
        assert!(trail.is_empty());
    }

    #[test]
    fn length_limit_drops_old_positions() {
        let config = CursorTrailConfig::enabled(2, 10.0);
        let mut trail = CursorTrail::new();
        let mut out = Vec::new();
        for step in 0..5 {
            trail.update(
                Some(rect(step as f32 * 10.0, 0.0)),
                Some((step, 0)),
                step as f32,
                &config,
                Rgba8::WHITE,
                &mut out,
            );
            out.clear();
        }
        assert_eq!(trail.len(), 2);
    }

    #[test]
    fn disabled_config_clears_trail() {
        let config = CursorTrailConfig::default();
        let mut trail = CursorTrail::new();
        let mut out = Vec::new();
        trail.update(
            Some(rect(0.0, 0.0)),
            Some((0, 0)),
            0.0,
            &CursorTrailConfig::enabled(4, 1.0),
            Rgba8::WHITE,
            &mut out,
        );
        trail.update(
            Some(rect(10.0, 0.0)),
            Some((1, 0)),
            0.1,
            &config,
            Rgba8::WHITE,
            &mut out,
        );
        assert!(out.is_empty());
        assert!(trail.is_empty());
    }
}
