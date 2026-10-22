// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU-only offscreen pixel tests for overlay rendering.
//!
//! A tiny software compositor replays the frame's background, decoration and
//! Gray8 glyph instances into an RGBA buffer. This keeps the overlay tests
//! independent of EGL while still asserting real pixels.

use kitty_render::{
    BasicColorResolver, GlyphAtlas, GlyphBitmap, GlyphKey, InstanceGenerator, RenderConfig, Rgba8,
    SelectionRange, TabBarColors, TabBarSpec,
};
use kitty_screen::Screen;

struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<[u8; 4]>,
}

impl Canvas {
    fn new(width: u32, height: u32, clear: Rgba8) -> Self {
        Self {
            width,
            height,
            pixels: vec![[clear.r, clear.g, clear.b, clear.a]; width as usize * height as usize],
        }
    }

    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        self.pixels[(y * self.width + x) as usize]
    }

    fn fill(&mut self, rect: kitty_render::Rect, color: Rgba8) {
        let x0 = rect.x.floor().max(0.0) as u32;
        let y0 = rect.y.floor().max(0.0) as u32;
        let x1 = rect.right().ceil().clamp(0.0, self.width as f32) as u32;
        let y1 = rect.bottom().ceil().clamp(0.0, self.height as f32) as u32;
        for y in y0..y1 {
            for x in x0..x1 {
                self.pixels[(y * self.width + x) as usize] = [color.r, color.g, color.b, color.a];
            }
        }
    }

    fn blend(&mut self, x: u32, y: u32, color: Rgba8, alpha: u8) {
        let index = (y * self.width + x) as usize;
        let dst = self.pixels[index];
        let a = alpha as u32;
        let inv = 255 - a;
        let mix = |src: u8, dst: u8| ((src as u32 * a + dst as u32 * inv) / 255) as u8;
        self.pixels[index] = [
            mix(color.r, dst[0]),
            mix(color.g, dst[1]),
            mix(color.b, dst[2]),
            255,
        ];
    }

    fn draw_glyph(
        &mut self,
        frame: &kitty_render::Frame,
        atlas: &GlyphAtlas,
        glyph: &kitty_render::GlyphInstance,
    ) {
        let Some(slot) = glyph.sprite else {
            return;
        };
        let Some(page) = atlas.page(slot.page as usize) else {
            return;
        };
        let rect = glyph.rect;
        let x0 = rect.x.floor().max(0.0) as u32;
        let y0 = rect.y.floor().max(0.0) as u32;
        let x1 = rect.right().ceil().clamp(0.0, self.width as f32) as u32;
        let y1 = rect.bottom().ceil().clamp(0.0, self.height as f32) as u32;
        if rect.width <= 0.0 || rect.height <= 0.0 {
            return;
        }
        for y in y0..y1 {
            let sy = slot.y
                + (((y as f32 - rect.y) / rect.height) * slot.height as f32)
                    .floor()
                    .clamp(0.0, (slot.height.saturating_sub(1)) as f32) as u32;
            for x in x0..x1 {
                let sx = slot.x
                    + (((x as f32 - rect.x) / rect.width) * slot.width as f32)
                        .floor()
                        .clamp(0.0, (slot.width.saturating_sub(1)) as f32)
                        as u32;
                let alpha = match slot.format {
                    kitty_render::AtlasFormat::Gray8 => {
                        page.data()[(sy * page.width() + sx) as usize]
                    }
                    kitty_render::AtlasFormat::Rgba8 => {
                        let start = ((sy * page.width() + sx) * 4) as usize;
                        page.data()[start + 3]
                    }
                };
                if alpha != 0 {
                    self.blend(x, y, glyph.fg, alpha);
                }
            }
        }
        let _ = frame;
    }

    fn draw_frame(&mut self, frame: &kitty_render::Frame, atlas: &GlyphAtlas) {
        for background in &frame.backgrounds {
            self.fill(background.rect, background.color);
        }
        for decoration in &frame.decorations {
            self.fill(decoration.rect, decoration.color);
        }
        for glyph in &frame.glyphs {
            self.draw_glyph(frame, atlas, glyph);
        }
    }
}

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

fn opaque_rasterizer(_: &GlyphKey) -> Option<GlyphBitmap> {
    GlyphBitmap::gray(6, 10, vec![255; 60]).ok()
}

fn render(
    screen: &Screen,
    config: RenderConfig,
    resolver: &BasicColorResolver,
) -> (kitty_render::Frame, GlyphAtlas) {
    let mut atlas = GlyphAtlas::default();
    let mut rasterizer = opaque_rasterizer;
    let mut frame = kitty_render::Frame::default();
    InstanceGenerator::new(config).generate(
        screen,
        &mut atlas,
        &mut rasterizer,
        resolver,
        &mut frame,
    );
    (frame, atlas)
}

fn canvas_for(frame: &kitty_render::Frame) -> Canvas {
    let width = frame
        .geometry
        .width_px()
        .max(
            frame
                .tab_bar
                .as_ref()
                .map(|t| t.rect.right())
                .unwrap_or(0.0),
        )
        .ceil()
        .max(1.0) as u32;
    let height = frame
        .geometry
        .height_px()
        .max(
            frame
                .tab_bar
                .as_ref()
                .map(|t| t.rect.bottom())
                .unwrap_or(0.0),
        )
        .ceil()
        .max(1.0) as u32;
    Canvas::new(width, height, frame.default_bg)
}

#[test]
fn selection_background_and_foreground_pixels() {
    let mut s = screen(6, 1);
    draw(&mut s, "a cde");
    let mut resolver = BasicColorResolver::new(Rgba8::rgb(1, 2, 3), Rgba8::rgb(4, 5, 6));
    resolver.selection_bg = Rgba8::rgb(38, 79, 120);
    resolver.selection_fg = Rgba8::rgb(250, 200, 10);
    let config = RenderConfig {
        selections: vec![SelectionRange::cell(1, 0, 3, 0)],
        ..RenderConfig::default()
    };
    let (frame, atlas) = render(&s, config, &resolver);
    let mut canvas = canvas_for(&frame);
    canvas.draw_frame(&frame, &atlas);

    // Cell 0 is unselected text; cell 1 is a selected space; cell 2 is
    // selected text, so its glyph uses the selection foreground.
    assert_eq!(canvas.pixel(5, 10), [1, 2, 3, 255]);
    assert_eq!(canvas.pixel(15, 10), [38, 79, 120, 255]);
    assert_eq!(canvas.pixel(25, 10), [250, 200, 10, 255]);
}

#[test]
fn block_selection_uses_scrolled_back_view_offset_pixels() {
    let mut s = screen(5, 2);
    draw(&mut s, "a   d");
    s.cursor_position(2, 1);
    draw(&mut s, "e   h");
    s.scrolled_by = 1;
    let mut resolver = BasicColorResolver::new(Rgba8::rgb(1, 2, 3), Rgba8::rgb(4, 5, 6));
    resolver.selection_bg = Rgba8::rgb(38, 79, 120);
    let config = RenderConfig {
        // Range -1 is the newest history row, which is visual row 0 when
        // scrolled back by one line.
        selections: vec![SelectionRange::block(1, -1, 2, 0)],
        ..RenderConfig::default()
    };
    let (frame, atlas) = render(&s, config, &resolver);
    let mut canvas = canvas_for(&frame);
    canvas.draw_frame(&frame, &atlas);
    assert_eq!(canvas.pixel(5, 5), [1, 2, 3, 255]);
    assert_eq!(canvas.pixel(35, 5), [4, 5, 6, 255]);
    assert_eq!(canvas.pixel(15, 5), [38, 79, 120, 255]);
    assert_eq!(canvas.pixel(25, 5), [38, 79, 120, 255]);
    assert_eq!(canvas.pixel(15, 25), [38, 79, 120, 255]);
    assert_eq!(canvas.pixel(25, 25), [38, 79, 120, 255]);
}

fn tab_bar_colors() -> TabBarColors {
    TabBarColors {
        background: Rgba8::rgb(1, 1, 1),
        inactive_background: Rgba8::rgb(20, 30, 40),
        active_background: Rgba8::rgb(10, 200, 30),
        foreground: Rgba8::WHITE,
        active_foreground: Rgba8::WHITE,
        border: Rgba8::rgb(60, 60, 60),
        active_border: Rgba8::rgb(220, 160, 20),
    }
}

fn tab_spec(titles: Vec<&str>, active: usize) -> TabBarSpec {
    TabBarSpec {
        titles: titles.into_iter().map(str::to_string).collect(),
        active,
        height: 30.0,
        colors: tab_bar_colors(),
        min_tab_width: 40.0,
        max_tab_width: 40.0,
        ..TabBarSpec::default()
    }
}

#[test]
fn tab_bar_active_inactive_and_overflow_pixels() {
    let mut s = screen(10, 1);
    draw(&mut s, "x");
    let config = RenderConfig {
        viewport_width: 100.0,
        viewport_height: 60.0,
        tab_bar: Some(tab_spec(vec!["one", "two"], 1)),
        ..RenderConfig::default()
    };
    let resolver = BasicColorResolver::default();
    let (frame, atlas) = render(&s, config.clone(), &resolver);
    let mut canvas = canvas_for(&frame);
    canvas.draw_frame(&frame, &atlas);
    assert_eq!(canvas.pixel(30, 2), [20, 30, 40, 255], "inactive tab");
    assert_eq!(canvas.pixel(45, 2), [10, 200, 30, 255], "active tab");

    let mut overflow = tab_spec(
        vec![
            "one", "two", "three", "four", "five", "six", "seven", "eight",
        ],
        0,
    );
    overflow.max_tab_width = 40.0;
    let config = RenderConfig {
        viewport_width: 100.0,
        viewport_height: 60.0,
        tab_bar: Some(overflow),
        ..RenderConfig::default()
    };
    let (frame, atlas) = render(&s, config, &resolver);
    let mut canvas = Canvas::new(130, 60, frame.default_bg);
    canvas.draw_frame(&frame, &atlas);
    let bar = frame.tab_bar.as_ref().unwrap();
    assert_eq!(bar.tabs.len(), 3, "only tabs intersecting the viewport");
    assert!(bar.tabs.iter().all(|t| t.rect.right() <= 100.0));
    assert_eq!(canvas.pixel(95, 2), [20, 30, 40, 255], "clipped tab");
    assert_eq!(canvas.pixel(115, 2), [17, 17, 17, 255], "outside viewport");
}

#[test]
fn glyph_atlas_path_is_used_for_tab_titles() {
    let mut s = screen(10, 1);
    draw(&mut s, "x");
    let config = RenderConfig {
        viewport_width: 100.0,
        viewport_height: 60.0,
        tab_bar: Some(tab_spec(vec!["A"], 0)),
        ..RenderConfig::default()
    };
    let resolver = BasicColorResolver::default();
    let (frame, atlas) = render(&s, config, &resolver);
    assert!(frame
        .glyphs
        .iter()
        .any(|g| g.cell_y == 0 && g.rect.y >= 5.0 && g.sprite.is_some()));
    assert!(!atlas.is_empty());
}
