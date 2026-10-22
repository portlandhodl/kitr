// SPDX-License-Identifier: GPL-3.0-or-later
//! Integration tests against the fonts bundled in `kitty_tests/`.

use std::path::{Path, PathBuf};

use kitty_fonts::{
    ClusterLevel, Error, Face, Feature, FontSpec, FontStack, FontStyle, Fontconfig, GlyphBitmap,
    PixelFormat, RasterCache, ShapeOptions, ShapedRunCache, RASTER_CACHE_MAX_ENTRIES,
};

const FIRA: &str = "FiraCode-Medium.otf";
const IOSEVKA: &str = "iosevka-regular.ttf";
const LIBERATION: &str = "LiberationMono-Regular.ttf";
const CASCADIA: &str = "CascadiaCode-Regular.otf";
const COMFY: &str = "ComfyCode-Regular.ttf";
const TWEMOJI: &str = "twemoji_smiley-cff2_colr_1.otf";

fn bundled(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../kitty_tests")
        .join(name)
}

fn face(name: &str) -> Face {
    let path = bundled(name);
    assert!(path.exists(), "missing test font {}", path.display());
    Face::from_path(&path, 0).unwrap_or_else(|e| panic!("failed to load {}: {e}", path.display()))
}

fn cps(text: &str) -> Vec<u32> {
    text.chars().map(|c| c as u32).collect()
}

fn shape(face: &Face, text: &str) -> Vec<u32> {
    face.shape(&cps(text), &ShapeOptions::default())
        .unwrap()
        .glyphs
        .iter()
        .map(|g| g.glyph_id)
        .collect()
}

#[test]
fn bundled_fonts_load() {
    for name in [FIRA, IOSEVKA, LIBERATION, CASCADIA, COMFY, TWEMOJI] {
        let f = face(name);
        assert!(!f.family_name().is_empty(), "{name} has no family name");
        assert!(f.num_glyphs() > 0, "{name} has no glyphs");
        assert!(f.raw_metrics().units_per_em > 0);
    }
}

#[test]
fn cell_metrics_are_positive_and_kitty_like() {
    let expected = [
        (FIRA, 10, 20),
        (IOSEVKA, 8, 20),
        (LIBERATION, 10, 19),
        (CASCADIA, 9, 19),
        (COMFY, 10, 20),
    ];
    for (name, width, height) in expected {
        let f = face(name);
        let m = f.cell_metrics();
        assert_eq!(
            (m.cell_width, m.cell_height),
            (width, height),
            "{name} {m:?}"
        );
        assert!(m.baseline > 0 && m.baseline < m.cell_height);
        assert!(m.underline_position < m.cell_height);
        assert!(m.underline_thickness >= 1);
        assert!(m.strikethrough_position < m.cell_height);
        assert!(m.strikethrough_thickness >= 1);
    }
}

#[test]
fn raw_face_metrics_are_sane() {
    for name in [FIRA, IOSEVKA, LIBERATION, CASCADIA, COMFY] {
        let f = face(name);
        let m = f.raw_metrics();
        assert!(m.units_per_em >= 1000, "{name} upem {}", m.units_per_em);
        assert!(m.ascender > 0);
        assert!(m.descender < 0);
        assert!(m.height > 0);
        assert!(m.max_advance_width > 0);
    }
}

#[test]
fn cell_metrics_grow_with_size() {
    let path = bundled(FIRA);
    let small = Face::from_path_with_size(&path, 0, 11.0, 96.0, 96.0).unwrap();
    let large = Face::from_path_with_size(&path, 0, 22.0, 96.0, 96.0).unwrap();
    let a = small.cell_metrics();
    let b = large.cell_metrics();
    assert!(b.cell_width > a.cell_width, "{a:?} {b:?}");
    assert!(b.cell_height > a.cell_height, "{a:?} {b:?}");
    assert!(b.baseline > a.baseline);
}

#[test]
fn space_advance_is_positive_and_matches_cell_width() {
    for name in [FIRA, IOSEVKA, LIBERATION, CASCADIA, COMFY] {
        let f = face(name);
        let m = f.cell_metrics();
        let space = f.space_advance_px();
        assert!(space > 0, "{name}");
        assert_eq!(space, m.cell_width, "{name}");
        assert_eq!(f.glyph_id(' ' as u32), f.space_glyph_id());
    }
}

#[test]
fn ascii_coverage_is_complete() {
    for name in [FIRA, IOSEVKA, LIBERATION, CASCADIA, COMFY] {
        let f = face(name);
        for ch in 32u32..127 {
            assert!(f.has_codepoint(ch), "{name} is missing ASCII U+{ch:04X}");
        }
    }
}

#[test]
fn cjk_is_missing_from_text_mono_fonts() {
    for name in [FIRA, IOSEVKA, LIBERATION, CASCADIA, COMFY] {
        let f = face(name);
        assert!(!f.has_codepoint('你' as u32), "{name}");
        assert!(!f.has_codepoint('好' as u32), "{name}");
        assert_eq!(f.glyph_id('你' as u32), 0);
    }
}

#[test]
fn emoji_is_missing_from_text_mono_fonts() {
    for name in [FIRA, IOSEVKA, LIBERATION, CASCADIA, COMFY] {
        let f = face(name);
        assert!(!f.has_codepoint(0x1F601), "{name}");
    }
}

#[test]
fn twemoji_color_face_is_usable() {
    let f = face(TWEMOJI);
    assert!(f.has_color());
    assert!(f.has_codepoint(0x1F601));
    assert!(f.has_codepoint(0x263A));
    let m = f.cell_metrics();
    assert!(m.cell_width > 0 && m.cell_height > 0);
    let run = f.shape(&[0x1F601], &ShapeOptions::default()).unwrap();
    assert_eq!(run.glyphs.len(), 1);
    assert_ne!(run.glyphs[0].glyph_id, 0);
}

#[test]
fn glyph_metrics_for_ascii() {
    let f = face(FIRA);
    let gid = f.glyph_id('A' as u32);
    assert_ne!(gid, 0);
    let m = f.glyph_metrics(gid).unwrap();
    assert_eq!(m.glyph_id, gid);
    assert!(m.advance_x_26_6 > 0);
    assert!(m.advance_x_px() > 0);
    assert!(m.width_26_6 > 0);
    assert!(m.height_26_6 > 0);
    assert_eq!(m.width_px(), (m.width_26_6 / 64) as u32);
}

#[test]
fn missing_codepoint_reports_zero() {
    let f = face(FIRA);
    for cp in [0x10FFFF, 0x10FFFE, 0x0378, 0xE123] {
        assert_eq!(f.glyph_id(cp), 0, "U+{cp:04X}");
        assert!(!f.has_codepoint(cp));
    }
}

#[test]
fn family_and_postscript_names() {
    let cases = [
        (FIRA, "Fira Code", "FiraCode-Medium"),
        (IOSEVKA, "Iosevka", "Iosevka"),
        (LIBERATION, "Liberation Mono", "LiberationMono"),
        (CASCADIA, "Cascadia Code", "CascadiaCode-Regular"),
        (COMFY, "Comfy Code", "Comfy-Code"),
    ];
    for (name, family, ps) in cases {
        let f = face(name);
        assert_eq!(f.family_name(), family);
        assert_eq!(f.postscript_name(), ps);
    }
}

#[test]
fn shape_latin_run() {
    let f = face(FIRA);
    let run = f.shape(&cps("abcd"), &ShapeOptions::default()).unwrap();
    assert_eq!(run.glyphs.len(), 4);
    assert_eq!(run.clusters(), vec![0, 1, 2, 3]);
    assert_eq!(run.num_codepoints, 4);
    assert!(run.glyphs.iter().all(|g| g.x_advance > 0));
    assert_eq!(
        run.total_advance_26_6(),
        4 * f.cell_metrics().cell_width as i32 * 64
    );
}

#[test]
fn fira_contextual_colon_matches_kitty() {
    let f = face(FIRA);
    let run = f.shape(&cps("9:30"), &ShapeOptions::default()).unwrap();
    assert_eq!(run.glyphs.len(), 4);
    assert_eq!(run.glyphs[1].glyph_id, 1031);
    let colon = f.shape(&cps(":"), &ShapeOptions::default()).unwrap();
    assert_ne!(run.glyphs[1].glyph_id, colon.glyphs[0].glyph_id);
}

#[test]
fn fira_arrow_ligature_uses_contextual_glyphs() {
    let f = face(FIRA);
    let arrow = shape(&f, "->");
    let hyphen = shape(&f, "-");
    let greater = shape(&f, ">");
    assert_eq!(arrow.len(), 2);
    assert_eq!(hyphen.len(), 1);
    assert_eq!(greater.len(), 1);
    assert_ne!(arrow, vec![hyphen[0], greater[0]]);
    let run = f.shape(&cps("a->b"), &ShapeOptions::default()).unwrap();
    assert_eq!(run.glyphs.len(), 4);
    assert_eq!(run.clusters(), vec![0, 1, 2, 3]);
}

#[test]
fn fira_equals_and_dash_sequences_are_contextual() {
    let f = face(FIRA);
    assert_ne!(shape(&f, "=="), vec![shape(&f, "=")[0]; 2]);
    assert_ne!(
        shape(&f, "==>"),
        vec![shape(&f, "=")[0]; 2]
            .into_iter()
            .chain(shape(&f, ">"))
            .collect::<Vec<_>>()
    );
    assert_eq!(shape(&f, "==>").len(), 3);
}

#[test]
fn iosevka_shapes_ascii_and_arrow() {
    let f = face(IOSEVKA);
    assert_eq!(shape(&f, "abcd"), vec![68, 69, 70, 71]);
    let arrow = f.shape(&cps("->"), &ShapeOptions::default()).unwrap();
    assert_eq!(arrow.glyphs.len(), 2);
    assert_eq!(arrow.clusters(), vec![0, 1]);
    assert!(arrow.total_advance_px() > 0);
}

#[test]
fn cascadia_shapes_and_uses_contextual_glyphs() {
    let f = face(CASCADIA);
    let run = f.shape(&cps("==>"), &ShapeOptions::default()).unwrap();
    assert_eq!(run.glyphs.len(), 3);
    assert_ne!(
        run.glyphs[0].glyph_id,
        f.shape(&cps("="), &ShapeOptions::default()).unwrap().glyphs[0].glyph_id
    );
}

#[test]
fn comfy_shapes_and_uses_contextual_glyphs() {
    let f = face(COMFY);
    let arrow = shape(&f, "->");
    let hyphen = shape(&f, "-");
    let greater = shape(&f, ">");
    assert_eq!(arrow.len(), 2);
    assert_ne!(arrow, vec![hyphen[0], greater[0]]);
}

#[test]
fn disable_ligature_changes_contextual_alternates() {
    let f = face(FIRA);
    let normal = shape(&f, "->");
    let disabled = f
        .shape(
            &cps("->"),
            &ShapeOptions {
                disable_ligature: true,
                ..ShapeOptions::default()
            },
        )
        .unwrap()
        .glyphs
        .iter()
        .map(|g| g.glyph_id)
        .collect::<Vec<_>>();
    assert_ne!(normal, disabled);
    assert_eq!(disabled, vec![1059, 1228]);
}

#[test]
fn explicit_liga_feature_is_applied() {
    let f = face(FIRA);
    let normal = shape(&f, "==");
    let no_liga = f
        .shape(
            &cps("=="),
            &ShapeOptions {
                features: vec![
                    Feature::parse("liga=0").unwrap(),
                    Feature::parse("calt=0").unwrap(),
                ],
                ..ShapeOptions::default()
            },
        )
        .unwrap()
        .glyphs
        .iter()
        .map(|g| g.glyph_id)
        .collect::<Vec<_>>();
    assert_ne!(normal, no_liga);
}

#[test]
fn combining_marks_stay_in_one_cluster() {
    let f = face(LIBERATION);
    let run = f
        .shape(&cps("u\u{0332}\u{0301}"), &ShapeOptions::default())
        .unwrap();
    assert!(run.glyphs.len() >= 2);
    assert!(run.glyphs.iter().all(|g| g.cluster == 0));
    let chars = f
        .shape(
            &cps("u\u{0332}\u{0301}"),
            &ShapeOptions {
                cluster_level: ClusterLevel::MonotoneCharacters,
                ..ShapeOptions::default()
            },
        )
        .unwrap();
    assert!(chars.glyphs.iter().all(|g| g.cluster <= 1));
}

#[test]
fn empty_text_shapes_to_no_glyphs() {
    let f = face(FIRA);
    let run = f.shape(&[], &ShapeOptions::default()).unwrap();
    assert!(run.glyphs.is_empty());
    assert_eq!(run.total_advance_26_6(), 0);
}

#[test]
fn shaped_run_cache_hits_and_reuses_output() {
    let f = face(FIRA);
    let mut cache = ShapedRunCache::new();
    let text = cps("a->b");
    let opts = ShapeOptions::default();
    let first = cache.shape(&f, &text, &opts).unwrap();
    let second = cache.shape(&f, &text, &opts).unwrap();
    assert_eq!(first, second);
    assert_eq!(cache.hits(), 1);
    assert_eq!(cache.misses(), 1);
    assert_eq!(cache.len(), 1);
    assert!(cache.bytes() > 0);

    let arrow_first = cache.shape(&f, &[45, 62], &opts).unwrap();
    let arrow_second = cache.shape(&f, &[45, 62], &opts).unwrap();
    assert_eq!(arrow_first, arrow_second);
    assert_eq!(arrow_first.glyphs.len(), 2);
    assert_eq!(cache.hits(), 2);
    assert_eq!(cache.misses(), 2);

    let other = cache.shape(&f, &cps("==>"), &opts).unwrap();
    assert_eq!(other.glyphs.len(), 3);
    assert_eq!(cache.len(), 3);
}

#[test]
fn shaped_run_cache_does_not_store_overlong_runs() {
    let f = face(FIRA);
    let mut cache = ShapedRunCache::new();
    let text = cps(&"a".repeat(33));
    cache.shape(&f, &text, &ShapeOptions::default()).unwrap();
    assert_eq!(cache.len(), 0);
    assert_eq!(cache.misses(), 1);
}

#[test]
fn fontconfig_matches_liberation_mono() {
    let fc = Fontconfig::new().unwrap();
    let m = fc
        .match_family("Liberation Mono", FontStyle::Regular)
        .unwrap()
        .expect("Liberation Mono should be installed");
    assert!(Path::new(&m.path).exists());
    assert_eq!(m.index, 0);
    assert!(m.hinting);
    let f = m.load(12.0, 96.0, 96.0).unwrap();
    assert!(f.has_codepoint('A' as u32));
    assert!(f.family_name().to_lowercase().contains("liberation"));
}

#[test]
fn fontconfig_style_matching_selects_different_files() {
    let fc = Fontconfig::new().unwrap();
    let regular = fc
        .match_family("Liberation Mono", FontStyle::Regular)
        .unwrap()
        .unwrap();
    let bold = fc
        .match_family("Liberation Mono", FontStyle::Bold)
        .unwrap()
        .unwrap();
    let italic = fc
        .match_family("Liberation Mono", FontStyle::Italic)
        .unwrap()
        .unwrap();
    let bold_italic = fc
        .match_family("Liberation Mono", FontStyle::BoldItalic)
        .unwrap()
        .unwrap();
    assert_ne!(regular.path, bold.path);
    assert_ne!(regular.path, italic.path);
    assert_ne!(bold.path, bold_italic.path);
    assert_ne!(italic.path, bold_italic.path);
    let bold_face = bold.load(12.0, 96.0, 96.0).unwrap();
    let italic_face = italic.load(12.0, 96.0, 96.0).unwrap();
    let bold_italic_face = bold_italic.load(12.0, 96.0, 96.0).unwrap();
    assert!(bold_face.is_bold());
    assert!(italic_face.is_italic());
    assert!(bold_italic_face.is_bold());
    assert!(bold_italic_face.is_italic());
}

#[test]
fn fontconfig_matches_postscript_name() {
    let fc = Fontconfig::new().unwrap();
    let m = fc
        .match_postscript_name("LiberationMono")
        .unwrap()
        .expect("LiberationMono PostScript name should resolve");
    assert!(m.path.to_lowercase().contains("liberationmono-regular"));
    assert_eq!(m.postscript_name.as_deref(), Some("LiberationMono"));
}

#[test]
fn fontconfig_default_monospace_is_usable() {
    let fc = Fontconfig::new().unwrap();
    let m = fc
        .default_monospace(FontStyle::Regular)
        .unwrap()
        .expect("a monospace font should be installed");
    let f = m.load(12.0, 96.0, 96.0).unwrap();
    let metrics = f.cell_metrics();
    assert!(metrics.cell_width > 0 && metrics.cell_height > 0);
    assert!(f.has_codepoint('x' as u32));
}

#[test]
fn font_stack_uses_explicit_fallback_faces() {
    let mut stack = FontStack::new(vec![face(FIRA), face(TWEMOJI)]).without_fontconfig();
    assert_eq!(stack.face_index_for_char('a' as u32).unwrap(), Some(0));
    assert_eq!(stack.face_index_for_char(0x1F601).unwrap(), Some(1));
    assert_eq!(stack.face_index_for_char('你' as u32).unwrap(), None);
    let shaped = stack
        .shape(&cps("a\u{1F601}b"), &ShapeOptions::default())
        .unwrap();
    assert!(shaped.is_complete());
    assert_eq!(shaped.runs.len(), 3);
    assert_eq!(shaped.num_glyphs(), 3);
    assert_eq!(shaped.runs[1].face_index, 1);
}

#[test]
fn font_stack_reports_missing_codepoints() {
    let mut stack = FontStack::new(vec![face(FIRA)]).without_fontconfig();
    let shaped = stack.shape(&cps("a你b"), &ShapeOptions::default()).unwrap();
    assert!(!shaped.is_complete());
    assert_eq!(shaped.missing, vec![(1, '你' as u32)]);
    assert_eq!(shaped.runs.len(), 2);
    assert_eq!(shaped.runs[0].start, 0);
    assert_eq!(shaped.runs[1].start, 2);
}

#[test]
fn font_stack_lazily_loads_fontconfig_fallback() {
    let mut stack =
        FontStack::from_family("Liberation Mono", FontStyle::Regular, 12.0, 96.0).unwrap();
    assert_eq!(stack.faces().len(), 1);
    match stack.face_index_for_char('你' as u32).unwrap() {
        Some(idx) => {
            assert!(idx > 0);
            assert!(stack.faces()[idx].has_codepoint('你' as u32));
        }
        None => {
            // Some minimal systems have no CJK font; the stack must simply
            // report the glyph as missing in that case.
            assert!(stack.faces().len() <= 2);
        }
    }
}

#[test]
fn font_stack_shape_cached_matches_uncached() {
    let mut stack = FontStack::new(vec![face(FIRA), face(TWEMOJI)]).without_fontconfig();
    let opts = ShapeOptions::default();
    let text = cps("a\u{1F601}->");
    let plain = stack.shape(&text, &opts).unwrap();
    let mut cache = ShapedRunCache::new();
    let cached = stack.shape_cached(&mut cache, &text, &opts).unwrap();
    assert_eq!(plain, cached);
    let again = stack.shape_cached(&mut cache, &text, &opts).unwrap();
    assert_eq!(cached, again);
    assert!(cache.hits() >= 1);
}

#[test]
fn font_spec_drives_font_stack() {
    let spec = FontSpec::from_setting("family=\"Liberation Mono\" features=\"-dlig\"").unwrap();
    assert_eq!(spec.primary_family(), Some("Liberation Mono"));
    assert_eq!(spec.features.len(), 1);
    let mut stack = FontStack::from_spec(&spec, FontStyle::Regular, 12.0, 96.0).unwrap();
    assert_eq!(stack.primary_face().family_name(), "Liberation Mono");
    assert!(stack.face_index_for_char('A' as u32).unwrap().is_some());
}

#[test]
fn font_stack_family_list_falls_through_to_an_installed_family() {
    let spec = FontSpec::from_setting("family=\"Definitely Not A Font, Liberation Mono\"").unwrap();
    assert_eq!(spec.families().len(), 2);
    let stack = FontStack::from_spec(&spec, FontStyle::Regular, 12.0, 96.0).unwrap();
    assert!(stack
        .primary_face()
        .family_name()
        .to_lowercase()
        .contains("liberation"));
}

#[test]
fn font_stack_unknown_family_uses_monospace_default() {
    let spec = FontSpec::from_setting("Definitely Not A Font").unwrap();
    let stack = FontStack::from_spec(&spec, FontStyle::Regular, 12.0, 96.0).unwrap();
    let m = stack.primary_face().cell_metrics();
    assert!(m.cell_width > 0 && m.cell_height > 0);
    assert!(stack.primary_face().has_codepoint('A' as u32));
}

#[test]
fn face_descriptor_round_trip() {
    let f = face(FIRA);
    let d = f.descriptor().clone();
    assert_eq!(d.path, bundled(FIRA));
    assert_eq!(d.index, 0);
    let f2 = d.load(12.0, 96.0, 96.0).unwrap();
    assert_eq!(f2.cell_metrics(), f.cell_metrics());
    assert_eq!(f2.postscript_name(), f.postscript_name());
}

fn assert_bitmap_integrity(bitmap: &GlyphBitmap) {
    assert_eq!(
        bitmap.data.len(),
        bitmap.pitch * bitmap.height as usize,
        "data length must be pitch * height"
    );
    assert!(
        bitmap.pitch >= bitmap.width as usize * bitmap.format.bytes_per_pixel(),
        "stride {} is smaller than {} pixels of {}",
        bitmap.pitch,
        bitmap.width,
        bitmap.format
    );
    if !bitmap.is_empty() {
        assert!(bitmap.pitch > 0);
        for y in 0..bitmap.height {
            assert_eq!(bitmap.row(y).unwrap().len(), bitmap.pitch);
        }
    }
}

#[test]
fn rasterize_ascii_glyph() {
    let f = face(FIRA);
    let bitmap = f.rasterize_codepoint('A' as u32).unwrap();
    assert!(!bitmap.is_empty());
    assert!(bitmap.width >= 5 && bitmap.height >= 5, "{bitmap:?}");
    assert_eq!(bitmap.format, PixelFormat::Gray8);
    assert!(bitmap.advance_x > 0);
    assert!(bitmap.advance_x_26_6 > 0);
    assert!(bitmap.top > 0);
    assert!(
        bitmap.data.iter().any(|&p| p != 0),
        "glyph is all transparent"
    );
    assert_bitmap_integrity(&bitmap);
}

#[test]
fn rasterize_space_is_a_valid_empty_bitmap() {
    let f = face(FIRA);
    let bitmap = f.rasterize_codepoint(' ' as u32).unwrap();
    assert!(bitmap.is_empty());
    assert_eq!((bitmap.width, bitmap.height, bitmap.pitch), (0, 0, 0));
    assert!(bitmap.data.is_empty());
    assert!(bitmap.advance_x > 0);
    assert_bitmap_integrity(&bitmap);
}

#[test]
fn rasterize_codepoint_matches_glyph_id() {
    let f = face(FIRA);
    let gid = f.glyph_id('g' as u32);
    let by_id = f.rasterize_glyph(gid).unwrap();
    let by_cp = f.rasterize_codepoint('g' as u32).unwrap();
    assert_eq!(by_id, by_cp);
    assert!(!by_id.is_empty());
}

#[test]
fn rasterize_missing_glyph_returns_an_error() {
    let f = face(FIRA);
    assert!(matches!(
        f.rasterize_codepoint('你' as u32),
        Err(Error::MissingGlyph(cp)) if cp == '你' as u32
    ));
    assert!(matches!(
        f.rasterize_codepoint(0x1F600),
        Err(Error::MissingGlyph(0x1F600))
    ));
    assert!(matches!(
        f.rasterize_glyph(u32::MAX),
        Err(Error::MissingGlyph(_))
    ));
}

#[test]
fn rasterize_at_varying_sizes() {
    let path = bundled(FIRA);
    let small = Face::from_path_with_size(&path, 0, 11.0, 96.0, 96.0).unwrap();
    let large = Face::from_path_with_size(&path, 0, 22.0, 96.0, 96.0).unwrap();
    let a = small.rasterize_codepoint('A' as u32).unwrap();
    let b = large.rasterize_codepoint('A' as u32).unwrap();
    assert_bitmap_integrity(&a);
    assert_bitmap_integrity(&b);
    assert!(b.width > a.width, "{a:?} {b:?}");
    assert!(b.height > a.height, "{a:?} {b:?}");
    assert!(b.advance_x > a.advance_x);
}

#[test]
fn rasterize_bitmap_integrity_across_glyphs() {
    let f = face(FIRA);
    for ch in ['W', 'i', '_', '|', '8', 'g'] {
        let bitmap = f.rasterize_codepoint(ch as u32).unwrap();
        assert!(!bitmap.is_empty(), "{ch:?}");
        assert_bitmap_integrity(&bitmap);
    }
}

#[test]
fn rasterize_with_custom_load_flags() {
    use kitty_fonts::ffi::freetype::{FT_LOAD_DEFAULT, FT_LOAD_NO_HINTING, FT_LOAD_RENDER};
    let f = face(FIRA);
    let normal = f.rasterize_glyph(f.glyph_id('A' as u32)).unwrap();
    let unhinted = f
        .rasterize_glyph_with_flags(
            f.glyph_id('A' as u32),
            FT_LOAD_DEFAULT | FT_LOAD_RENDER | FT_LOAD_NO_HINTING,
        )
        .unwrap();
    assert!(!unhinted.is_empty());
    assert!(unhinted.advance_x > 0);
    assert_bitmap_integrity(&unhinted);
    assert!(normal.width > 0 && normal.height > 0);
}

#[test]
fn rasterize_cjk_through_font_stack() {
    let mut stack =
        FontStack::from_family("Liberation Mono", FontStyle::Regular, 12.0, 96.0).unwrap();
    match stack.rasterize_codepoint('你' as u32).unwrap() {
        Some((idx, bitmap)) => {
            assert!(idx > 0);
            assert!(!bitmap.is_empty());
            assert_bitmap_integrity(&bitmap);
        }
        None => {
            // No CJK font is installed; the stack must report that cleanly.
            assert_eq!(stack.faces().len(), 1);
        }
    }
}

#[test]
fn rasterize_emoji_from_bundled_color_face() {
    let f = face(TWEMOJI);
    assert!(f.has_color());
    assert!(f.has_codepoint(0x1F601));
    let bitmap = f.rasterize_codepoint(0x1F601).unwrap();
    assert_bitmap_integrity(&bitmap);
    assert!(bitmap.advance_x > 0);
    // The bundled face is a COLRv1 font. Stock FreeType builds without a
    // COLRv1 painter report a bitmap format but zero pixels; builds with one
    // emit BGRA. Both outcomes are valid at this layer.
    if !bitmap.is_empty() {
        assert!(matches!(
            bitmap.format,
            PixelFormat::Bgra8 | PixelFormat::Rgba8 | PixelFormat::Gray8
        ));
    }
}

#[test]
fn color_and_bitmap_only_faces_never_panic() {
    let fc = Fontconfig::new().unwrap();
    let Some(m) = fc
        .match_for_codepoint(0x1F600, FontStyle::Regular, true)
        .unwrap()
    else {
        return;
    };
    let Ok(f) = m.load(12.0, 96.0, 96.0) else {
        return;
    };
    assert!(f.has_color());
    match f.rasterize_codepoint(0x1F600) {
        Ok(bitmap) => {
            assert_bitmap_integrity(&bitmap);
            if !bitmap.is_empty() {
                assert!(matches!(
                    bitmap.format,
                    PixelFormat::Bgra8 | PixelFormat::Rgba8 | PixelFormat::Gray8
                ));
            }
        }
        Err(
            Error::FreeType { .. }
            | Error::UnsupportedPixelMode(_)
            | Error::UnsupportedGlyphFormat(_)
            | Error::Raster(_),
        ) => {
            // Example: a CBDT/PNG color font when FreeType was built without
            // PNG support. The failure is reported as an error, not a panic.
        }
        Err(other) => panic!("unexpected rasterization error: {other}"),
    }
}

#[test]
fn rasterize_font_stack_returns_the_fallback_face_index() {
    let mut stack = FontStack::new(vec![face(FIRA), face(TWEMOJI)]).without_fontconfig();
    let (idx, bitmap) = stack.rasterize_codepoint(0x1F601).unwrap().unwrap();
    assert_eq!(idx, 1);
    assert_bitmap_integrity(&bitmap);
    assert!(bitmap.advance_x > 0);

    let (idx, bitmap) = stack.rasterize_codepoint('A' as u32).unwrap().unwrap();
    assert_eq!(idx, 0);
    assert!(!bitmap.is_empty());

    assert!(stack.rasterize_codepoint('你' as u32).unwrap().is_none());
}

#[test]
fn raster_cache_hits_misses_and_stats() {
    let f = face(FIRA);
    let mut cache = RasterCache::new();
    let a = f.glyph_id('A' as u32);
    let g = f.glyph_id('g' as u32);

    let first = cache.rasterize(&f, a).unwrap();
    let second = cache.rasterize(&f, a).unwrap();
    assert_eq!(first, second);
    assert_eq!((cache.hits(), cache.misses()), (1, 1));
    assert_eq!(cache.len(), 1);

    let _ = cache.rasterize(&f, g).unwrap();
    assert_eq!((cache.hits(), cache.misses()), (1, 2));
    assert_eq!(cache.len(), 2);
    assert!(cache.bytes() > 0);

    cache.clear();
    assert!(cache.is_empty());
    assert_eq!(cache.bytes(), 0);
}

#[test]
fn raster_cache_handles_zero_size_glyphs() {
    let f = face(FIRA);
    let mut cache = RasterCache::new();
    let space = f.glyph_id(' ' as u32);
    let first = cache.rasterize(&f, space).unwrap();
    let second = cache.rasterize(&f, space).unwrap();
    assert!(first.is_empty());
    assert_eq!(first, second);
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.hits(), 1);
    assert_eq!(cache.misses(), 1);
}

#[test]
fn raster_cache_is_bounded() {
    let f = face(FIRA);
    let mut cache = RasterCache::new();
    for gid in 1..=RASTER_CACHE_MAX_ENTRIES as u32 + 8 {
        if f.rasterize_glyph(gid).is_ok() {
            let _ = cache.rasterize(&f, gid);
        }
    }
    assert!(cache.len() <= RASTER_CACHE_MAX_ENTRIES);
    assert!(cache.bytes() <= kitty_fonts::RASTER_CACHE_MAX_BYTES);
}

#[test]
fn raster_cache_codepoint_api() {
    let f = face(FIRA);
    let mut cache = RasterCache::new();
    let bitmap = cache.rasterize_codepoint(&f, 'A' as u32).unwrap();
    assert!(!bitmap.is_empty());
    assert!(matches!(
        cache.rasterize_codepoint(&f, '你' as u32),
        Err(Error::MissingGlyph(_))
    ));
}
