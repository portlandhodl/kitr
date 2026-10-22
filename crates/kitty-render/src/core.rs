// SPDX-License-Identifier: GPL-3.0-or-later
//! The CPU-side renderer core.
//!
//! [`RendererCore`] owns the glyph atlas and damage tracker and turns a
//! [`Screen`] into a [`Frame`] of backend-independent instances. Nothing here
//! touches OpenGL, EGL or any platform API; a later backend only needs to
//! upload atlas pages and draw the instance lists.

use std::time::Instant;

use kitty_screen::{CursorShape, Screen};

use crate::atlas::{AtlasConfig, GlyphAtlas, GlyphRasterizer};
use crate::colors::ColorResolver;
use crate::damage::{DamageTracker, FullRedrawReason};
use crate::instances::{Frame, InstanceGenerator, RenderConfig};
use crate::trail::CursorTrail;

/// Owns all persistent CPU renderer state.
#[derive(Clone, Debug)]
pub struct RendererCore {
    pub atlas: GlyphAtlas,
    pub damage: DamageTracker,
    pub generator: InstanceGenerator,
    pub trail: CursorTrail,
    started: Instant,
    spare: Option<Frame>,
}

impl Default for RendererCore {
    fn default() -> Self {
        Self::new(RenderConfig::default(), AtlasConfig::default())
    }
}

impl RendererCore {
    pub fn new(config: RenderConfig, atlas_config: AtlasConfig) -> Self {
        Self {
            atlas: GlyphAtlas::new(atlas_config),
            damage: DamageTracker::new(),
            generator: InstanceGenerator::new(config),
            trail: CursorTrail::new(),
            started: Instant::now(),
            spare: None,
        }
    }

    /// Build a frame for `screen`.
    ///
    /// The returned allocation is reused if it was previously handed back via
    /// [`recycle_frame`](Self::recycle_frame). For a caller that keeps its own
    /// frame, [`prepare_frame_into`](Self::prepare_frame_into) avoids the move
    /// entirely.
    pub fn prepare_frame(
        &mut self,
        screen: &Screen,
        rasterizer: &mut dyn GlyphRasterizer,
        resolver: &dyn ColorResolver,
    ) -> Frame {
        let mut frame = self.spare.take().unwrap_or_default();
        self.prepare_frame_into(screen, rasterizer, resolver, &mut frame);
        frame
    }

    /// Build a frame with an explicit monotonic time in seconds.
    ///
    /// Use this for deterministic cursor-trail animation and tests. The
    /// regular [`prepare_frame`](Self::prepare_frame) uses seconds since the
    /// core was created.
    pub fn prepare_frame_at(
        &mut self,
        screen: &Screen,
        rasterizer: &mut dyn GlyphRasterizer,
        resolver: &dyn ColorResolver,
        now: f32,
    ) -> Frame {
        let mut frame = self.spare.take().unwrap_or_default();
        self.prepare_frame_into_at(screen, rasterizer, resolver, &mut frame, now);
        frame
    }

    /// Build a frame into an existing allocation, retaining all vector
    /// capacities.
    pub fn prepare_frame_into(
        &mut self,
        screen: &Screen,
        rasterizer: &mut dyn GlyphRasterizer,
        resolver: &dyn ColorResolver,
        frame: &mut Frame,
    ) {
        let now = self.started.elapsed().as_secs_f32();
        self.prepare_frame_into_at(screen, rasterizer, resolver, frame, now);
    }

    /// [`prepare_frame_into`](Self::prepare_frame_into) with an explicit time.
    pub fn prepare_frame_into_at(
        &mut self,
        screen: &Screen,
        rasterizer: &mut dyn GlyphRasterizer,
        resolver: &dyn ColorResolver,
        frame: &mut Frame,
        now: f32,
    ) {
        let damage = self.damage.update(screen);
        self.generator
            .generate(screen, &mut self.atlas, rasterizer, resolver, frame);
        frame.damage = damage;

        let config = &self.generator.config.cursor_trail;
        let color = config.color.unwrap_or_else(|| resolver.cursor_bg());
        let visible = screen.is_cursor_visible() && screen.cursor.shape != CursorShape::NoCursor;
        let (cursor_rect, cursor_cell) = if visible {
            let x = screen.cursor.x.min(screen.columns.saturating_sub(1));
            let y = screen.cursor.y.min(screen.lines.saturating_sub(1));
            (Some(frame.geometry.cell_rect(x, y)), Some((x, y)))
        } else {
            (None, None)
        };
        self.trail.update(
            cursor_rect,
            cursor_cell,
            now,
            config,
            color,
            &mut frame.trail,
        );

        // Image and trail state is not part of kitty-screen's dirty-line
        // tracking. Replaying those layers only inside damaged row scissor
        // rectangles would leave stale pixels, so force a full redraw while
        // they are present.
        let media = !frame.images.is_empty()
            || frame.background_image.is_some()
            || frame.logo.is_some()
            || !frame.trail.is_empty();
        if media && !frame.damage.full_redraw {
            frame.damage.full_redraw = true;
            frame.damage.full_redraw_reason = Some(FullRedrawReason::Explicit);
        }
    }

    /// Reset the screen's dirty-line state after the frame has been consumed.
    ///
    /// `prepare_frame` takes `&Screen` and never mutates it, so the owner of
    /// the screen must call this (or [`Screen::reset_dirty`]) once the frame is
    /// drawn. Otherwise the same dirty rows are reported again.
    pub fn finish_frame(&self, screen: &mut Screen) {
        screen.reset_dirty();
    }

    /// Return a frame to the internal pool so its allocations are reused by
    /// the next [`prepare_frame`](Self::prepare_frame).
    pub fn recycle_frame(&mut self, mut frame: Frame) {
        frame.clear_instances();
        frame.damage = Default::default();
        self.spare = Some(frame);
    }

    /// Discard the atlas contents. The next frame is still limited by the
    /// damage tracker; call [`DamageTracker::invalidate`] as well when the
    /// backend has lost the atlas textures.
    pub fn clear_atlas(&mut self) {
        self.atlas.clear();
    }

    /// Force the next frame to be a full redraw.
    pub fn invalidate(&mut self) {
        self.damage.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::GlyphBitmap;
    use crate::colors::{BasicColorResolver, Rgba8};
    use crate::damage::FullRedrawReason;
    use kitty_screen::Screen;

    fn resolve() -> BasicColorResolver {
        BasicColorResolver::new(Rgba8::rgb(250, 250, 250), Rgba8::rgb(10, 10, 10))
    }

    fn rasterize(key: &crate::atlas::GlyphKey) -> Option<GlyphBitmap> {
        let _ = key;
        GlyphBitmap::gray(8, 12, vec![0xff; 96]).ok()
    }

    #[test]
    fn first_frame_is_full_and_instances_are_complete() {
        let mut s = Screen::builder(20, 5).scrollback(16).build().unwrap();
        let chars: Vec<u32> = "hello".chars().map(|c| c as u32).collect();
        s.draw_text(&chars);

        let mut core = RendererCore::default();
        let frame = core.prepare_frame(&s, &mut rasterize, &resolve());
        assert!(frame.damage.full_redraw);
        assert_eq!(
            frame.damage.full_redraw_reason,
            Some(FullRedrawReason::FirstFrame)
        );
        assert_eq!(frame.glyphs.len(), 5);
        assert_eq!(frame.backgrounds.len(), 5);
        assert_eq!(core.atlas.len(), 4); // "hello" has four distinct glyph keys
        assert_eq!(frame.atlas_updates.len(), 4);
    }

    #[test]
    fn frame_recycling_reuses_capacities() {
        let mut s = Screen::builder(20, 5).scrollback(16).build().unwrap();
        let chars: Vec<u32> = "hello".chars().map(|c| c as u32).collect();
        s.draw_text(&chars);
        let mut core = RendererCore::default();
        let r = resolve();
        let frame = core.prepare_frame(&s, &mut rasterize, &r);
        let capacity = frame.glyphs.capacity();
        core.recycle_frame(frame);

        s.reset_dirty();
        // Drawing again produces no change in this screen because the cursor
        // did not move; ensure the recycled allocation survives.
        let frame = core.prepare_frame(&s, &mut rasterize, &r);
        assert_eq!(frame.glyphs.capacity(), capacity);
        assert!(frame.damage.is_empty());
    }

    #[test]
    fn prepare_frame_into_keeps_allocations() {
        let mut s = Screen::builder(20, 5).scrollback(16).build().unwrap();
        let chars: Vec<u32> = "hi".chars().map(|c| c as u32).collect();
        s.draw_text(&chars);
        let mut core = RendererCore::default();
        let r = resolve();
        let mut frame = Frame::default();
        core.prepare_frame_into(&s, &mut rasterize, &r, &mut frame);
        let cap = frame.backgrounds.capacity();
        s.reset_dirty();
        s.cursor_position(2, 1);
        core.prepare_frame_into(&s, &mut rasterize, &r, &mut frame);
        assert_eq!(frame.backgrounds.capacity(), cap);
        assert!(frame.damage.cursor_moved);
    }

    #[test]
    fn atlas_page_events_are_forwarded() {
        let mut s = Screen::builder(20, 5).scrollback(16).build().unwrap();
        let chars: Vec<u32> = "abc".chars().map(|c| c as u32).collect();
        s.draw_text(&chars);
        let cfg = RenderConfig::default();
        let atlas_cfg = AtlasConfig {
            page_width: 16,
            page_height: 16,
            padding: 1,
            max_gray_pages: 4,
            max_rgba_pages: 4,
            allow_eviction: false,
        };
        let mut core = RendererCore::new(cfg, atlas_cfg);
        let frame = core.prepare_frame(&s, &mut rasterize, &resolve());
        assert!(!frame.atlas_events.is_empty());
    }
    #[test]
    fn cursor_trail_is_generated_and_fades() {
        use crate::trail::CursorTrailConfig;

        let mut s = Screen::builder(10, 4).scrollback(8).build().unwrap();
        s.cursor.shape = CursorShape::Block;
        let config = RenderConfig {
            cursor_trail: CursorTrailConfig::enabled(4, 1.0),
            ..RenderConfig::default()
        };
        let mut core = RendererCore::new(config, AtlasConfig::default());
        let r = resolve();

        let frame = core.prepare_frame_at(&s, &mut rasterize, &r, 0.0);
        assert!(frame.trail.is_empty());
        assert!(frame.damage.full_redraw);

        s.cursor_position(3, 1);
        let frame = core.prepare_frame_at(&s, &mut rasterize, &r, 0.1);
        assert_eq!(frame.trail.len(), 1);
        assert!((frame.trail[0].opacity - 1.0).abs() < 1e-6);
        assert!(frame.damage.full_redraw, "trail forces a full redraw");

        let frame = core.prepare_frame_at(&s, &mut rasterize, &r, 0.6);
        assert_eq!(frame.trail.len(), 1);
        assert!((frame.trail[0].opacity - 0.5).abs() < 1e-6);

        let frame = core.prepare_frame_at(&s, &mut rasterize, &r, 1.2);
        assert!(frame.trail.is_empty());
    }
}
