// SPDX-License-Identifier: GPL-3.0-or-later
//! Optional real-font integration checks for the HarfBuzz adapter.
//!
//! The Fira Code checks skip when no Fira Code face is installed, or when the
//! installed Nerd Font build has had its contextual ligatures removed. A
//! second check uses any installed serif face with the classic `fi` ligature,
//! which exercises the same shaped-run path without depending on Fira Code.

use std::path::PathBuf;

use kitty_fonts::{Face, ShapeOptions};
use kitty_render::{
    CellMetrics, FontRasterizer, Frame, GlyphAtlas, InstanceGenerator, RenderConfig,
};
use kitty_screen::Screen;

const FIRA_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/dot-fonts/NerdFonts/Fira Code Bold Nerd Font Complete.ttf",
    "/usr/share/fonts/dot-fonts/NerdFonts/Fira Code Regular Nerd Font Complete Mono.ttf",
    "/usr/share/fonts/dot-fonts/NerdFonts/Fira Code SemiBold Nerd Font Complete Mono.ttf",
    "/usr/share/fonts/truetype/firacode/FiraCode-Regular.ttf",
    "/usr/share/fonts/truetype/fira-code/FiraCode-Regular.ttf",
    "/usr/share/fonts/opentype/firacode/FiraCode-Regular.otf",
];

const SERIF_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/dejavu-serif-fonts/DejaVuSerif.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf",
    "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
];

fn locate(candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

fn render(
    text: &[u32],
    rasterizer: &mut FontRasterizer,
    config: RenderConfig,
) -> (Frame, GlyphAtlas) {
    let mut screen = Screen::builder(12, 1).scrollback(4).build().unwrap();
    screen.draw_text(text);
    let mut atlas = GlyphAtlas::default();
    let mut frame = Frame::default();
    let resolver = kitty_render::BasicColorResolver::default();
    InstanceGenerator::new(config).generate(&screen, &mut atlas, rasterizer, &resolver, &mut frame);
    (frame, atlas)
}

#[test]
fn fira_code_ligatures_use_one_sprite_each() {
    let Some(path) = locate(FIRA_CANDIDATES) else {
        eprintln!("skipping: Fira Code is not installed");
        return;
    };
    let face = Face::from_path(&path, 0).expect("load Fira Code");
    let text: Vec<u32> = "-> => !=".chars().map(|c| c as u32).collect();
    let shaped = face
        .shape(&text, &ShapeOptions::default())
        .expect("shape Fira Code text");
    if shaped.glyphs.len() >= text.len() {
        eprintln!("skipping: installed Fira Code build has no contextual ligatures");
        return;
    }
    let metrics = CellMetrics::from(face.cell_metrics());
    assert!(metrics.cell_width >= 1.0 && metrics.cell_height >= 1.0);
    let mut rasterizer = FontRasterizer::new(face);
    let config = RenderConfig {
        metrics,
        font_id: 1,
        ..RenderConfig::default()
    };
    let (frame, _atlas) = render(&text, &mut rasterizer, config);
    assert!(
        frame.overlay_stats.shaped_sprites >= 3,
        "expected three ligature sprites, got {:?}",
        frame.overlay_stats
    );
    assert!(
        frame.glyphs.len() < 8,
        "ligatures should replace per-cell glyphs: {} glyph instances",
        frame.glyphs.len()
    );
    assert!(frame.glyphs.iter().all(|g| g.sprite.is_some()));
}

#[test]
fn real_harfbuzz_ligature_becomes_one_multi_cell_sprite() {
    let Some(path) = locate(SERIF_CANDIDATES) else {
        eprintln!("skipping: no serif font with an fi ligature is installed");
        return;
    };
    let face = Face::from_path(&path, 0).expect("load serif face");
    let text = ['f' as u32, 'i' as u32];
    let shaped = face
        .shape(&text, &ShapeOptions::default())
        .expect("shape serif text");
    if shaped.glyphs.len() != 1 {
        eprintln!("skipping: installed serif face does not ligate fi");
        return;
    }
    let metrics = CellMetrics::from(face.cell_metrics());
    let mut rasterizer = FontRasterizer::new(face);
    let config = RenderConfig {
        metrics,
        font_id: 1,
        ..RenderConfig::default()
    };
    let (frame, _atlas) = render(&text, &mut rasterizer, config);
    assert_eq!(frame.glyphs.len(), 1, "ligature must be drawn once");
    assert_eq!(frame.overlay_stats.shaped_sprites, 1);
    let glyph = &frame.glyphs[0];
    assert_eq!(glyph.rect.width, frame.geometry.cell_width * 2.0);
    assert!(glyph.sprite.is_some());
}

#[test]
fn fira_code_combining_mark_uses_a_shaped_cell_sprite() {
    let Some(path) = locate(FIRA_CANDIDATES) else {
        eprintln!("skipping: Fira Code is not installed");
        return;
    };
    let face = Face::from_path(&path, 0).expect("load Fira Code");
    let metrics = CellMetrics::from(face.cell_metrics());
    let mut rasterizer = FontRasterizer::new(face);
    let config = RenderConfig {
        metrics,
        font_id: 1,
        ..RenderConfig::default()
    };
    let (frame, _atlas) = render(&['e' as u32, 0x301], &mut rasterizer, config);
    assert!(
        frame.overlay_stats.shaped_sprites >= 1,
        "expected a shaped combining cluster, got {:?}",
        frame.overlay_stats
    );
    assert_eq!(frame.glyphs.len(), 1);
}
