// SPDX-License-Identifier: GPL-3.0-or-later
use kitty_fonts::*;
use std::path::Path;
fn p(n: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../kitty_tests")
        .join(n)
}
fn show(label: &str, f: &Face, cp: u32) {
    match f.rasterize_codepoint(cp){Ok(b)=>println!("{label} U+{cp:X}: {}x{} left={} top={} adv={} ({}/{}) fmt={} pitch={} data={} empty={}",b.width,b.height,b.left,b.top,b.advance_x,b.advance_x_26_6,b.advance_y_26_6,b.format,b.pitch,b.data.len(),b.is_empty()),Err(e)=>println!("{label} U+{cp:X}: ERR {e}")}
}
fn main() {
    let fira = Face::from_path(p("FiraCode-Medium.otf"), 0).unwrap();
    for cp in ['A' as u32, ' ' as u32, 'i' as u32, '_' as u32] {
        show("fira", &fira, cp);
    }
    for cp in [0x4F60, 0x1F600, 0x10FFFF] {
        show("fira", &fira, cp);
    }
    match fira.rasterize_glyph(u32::MAX) {
        Ok(b) => println!("bogus gid -> {:?}", b),
        Err(e) => println!("bogus gid err: {e}"),
    }
    let tw = Face::from_path(p("twemoji_smiley-cff2_colr_1.otf"), 0).unwrap();
    for cp in [0x1F601, 0x263A] {
        show("twemoji", &tw, cp);
    }
    let fc = Fontconfig::new().unwrap();
    if let Some(m) = fc
        .match_for_codepoint(0x1F600, FontStyle::Regular, true)
        .unwrap()
    {
        println!("color fallback: {}", m.path);
        match m.load(12.0, 96.0, 96.0) {
            Ok(f) => {
                println!(
                    "  color={} upem={} num={}",
                    f.has_color(),
                    f.raw_metrics().units_per_em,
                    f.num_glyphs()
                );
                show("  color", &f, 0x1F600);
                show("  color", &f, 'A' as u32);
            }
            Err(e) => println!("  load err {e}"),
        }
    }
    if let Some(m) = fc
        .match_for_codepoint('你' as u32, FontStyle::Regular, false)
        .unwrap()
    {
        println!("cjk fallback: {}", m.path);
        match m.load(12.0, 96.0, 96.0) {
            Ok(f) => show("  cjk", &f, '你' as u32),
            Err(e) => println!("  load err {e}"),
        }
    }
    let mut stack = FontStack::new(vec![
        fira,
        Face::from_path(p("twemoji_smiley-cff2_colr_1.otf"), 0).unwrap(),
    ])
    .without_fontconfig();
    for cp in ['A' as u32, 0x1F601, '你' as u32] {
        match stack.rasterize_codepoint(cp) {
            Ok(Some((i, b))) => println!(
                "stack U+{cp:X}: face {i} {}x{} fmt {} data {}",
                b.width,
                b.height,
                b.format,
                b.data.len()
            ),
            Ok(None) => println!("stack U+{cp:X}: none"),
            Err(e) => println!("stack U+{cp:X}: err {e}"),
        }
    }
}
