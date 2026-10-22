// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU-side tests for relative placements and indexed image uploads.

use kitty_graphics::{parse_raw, IndexedPalette};
use kitty_render::{
    collect_protocol_images_with_palette, BasicColorResolver, CellMetrics, GridGeometry,
    ImageFilter,
};
use kitty_screen::Screen;

fn make_screen() -> Screen {
    Screen::builder(8, 5).scrollback(8).build().unwrap()
}

fn transmit(screen: &mut Screen, control: &str, payload: Vec<u8>) {
    let mut command = parse_raw(control.as_bytes()).unwrap();
    command.payload = payload;
    screen.handle_graphics_command(command).unwrap();
}

fn put(screen: &mut Screen, control: &str) {
    let mut command = parse_raw(control.as_bytes()).unwrap();
    command.payload.clear();
    screen.handle_graphics_command(command).unwrap();
}

fn geometry() -> GridGeometry {
    GridGeometry::new(
        8,
        5,
        CellMetrics {
            cell_width: 10.0,
            cell_height: 10.0,
            ..CellMetrics::default()
        },
    )
}

fn collect(
    screen: &Screen,
    palette: &IndexedPalette,
) -> (
    Vec<kitty_render::ImageInstance>,
    Vec<kitty_render::ImageUpload>,
) {
    let mut instances = Vec::new();
    let mut uploads = Vec::new();
    collect_protocol_images_with_palette(
        screen,
        None,
        palette,
        &geometry(),
        200.0,
        100.0,
        ImageFilter::Nearest,
        &mut instances,
        &mut uploads,
    );
    (instances, uploads)
}

#[test]
fn indexed_image_is_expanded_into_the_upload() {
    let mut screen = make_screen();
    transmit(&mut screen, "a=t,f=8,i=1,s=2,v=2", vec![1, 2, 1, 2]);
    put(&mut screen, "a=p,i=1,p=1,c=2,r=2,C=1");

    let mut resolver = BasicColorResolver::default();
    resolver.set_palette_color(1, kitty_render::Rgba8::rgb(255, 0, 0));
    resolver.set_palette_color(2, kitty_render::Rgba8::rgb(0, 0, 255));
    let palette = kitty_render::indexed_image_palette(&resolver);
    let (instances, uploads) = collect(&screen, &palette);
    assert_eq!(instances.len(), 1);
    assert_eq!(uploads.len(), 1);
    assert_eq!(
        uploads[0].data.as_slice(),
        &[255, 0, 0, 255, 0, 0, 255, 255, 255, 0, 0, 255, 0, 0, 255, 255]
    );
}

#[test]
fn relative_child_uses_parent_anchor_offsets_z_and_source() {
    let mut screen = make_screen();
    transmit(
        &mut screen,
        "a=t,f=32,i=1,s=4,v=4",
        [20u8, 30, 40, 255].repeat(16),
    );
    put(&mut screen, "a=p,i=1,p=1,c=2,r=2,z=-2,x=1,y=0,w=2,h=2,C=1");
    put(&mut screen, "a=p,i=1,p=2,P=1,Q=1,H=1,V=1,c=1,r=1,C=1");

    let palette = IndexedPalette::default();
    let (instances, _) = collect(&screen, &palette);
    let child = instances
        .iter()
        .find(|instance| instance.rect.x == 10.0 && instance.rect.y == 10.0)
        .expect("relative child placement");
    assert_eq!(child.z_index, -2, "zero child z inherits the parent");
    assert!((child.uv.u0 - 0.25).abs() < 1e-6);
    assert!((child.uv.u1 - 0.75).abs() < 1e-6);
    assert!((child.uv.v0 - 0.0).abs() < 1e-6);
    assert!((child.uv.v1 - 0.5).abs() < 1e-6);
}

#[test]
fn relative_cycles_and_missing_parents_are_not_rendered() {
    let mut screen = make_screen();
    transmit(&mut screen, "a=t,f=32,i=1,s=1,v=1", vec![255, 0, 0, 255]);
    transmit(&mut screen, "a=t,f=32,i=2,s=1,v=1", vec![0, 255, 0, 255]);
    put(&mut screen, "a=p,i=1,p=1,P=2,Q=1,C=1");
    put(&mut screen, "a=p,i=2,p=1,P=1,Q=1,C=1");
    let palette = IndexedPalette::default();
    let (instances, uploads) = collect(&screen, &palette);
    assert!(instances.is_empty());
    assert!(uploads.is_empty());

    let mut screen2 = make_screen();
    transmit(&mut screen2, "a=t,f=32,i=1,s=1,v=1", vec![255, 0, 0, 255]);
    put(&mut screen2, "a=p,i=1,p=1,P=99,Q=1,C=1");
    let (instances, _) = collect(&screen2, &palette);
    assert!(instances.is_empty());
}

#[test]
fn relative_anchor_zero_selects_first_parent_placement() {
    let mut screen = make_screen();
    transmit(&mut screen, "a=t,f=32,i=1,s=1,v=1", vec![255, 0, 0, 255]);
    put(&mut screen, "a=p,i=1,p=4,c=1,r=1,C=1");
    screen.cursor_position(3, 2);
    put(&mut screen, "a=p,i=1,p=2,c=1,r=1,C=1");
    transmit(&mut screen, "a=t,f=32,i=2,s=1,v=1", vec![0, 255, 0, 255]);
    put(&mut screen, "a=p,i=2,p=1,P=1,Q=0,H=1,V=1,C=1");
    let palette = IndexedPalette::default();
    let (instances, _) = collect(&screen, &palette);
    let child = instances
        .iter()
        .find(|instance| instance.key == kitty_render::ImageKey::Protocol(2))
        .expect("child of first parent placement");
    assert_eq!((child.rect.x, child.rect.y), (10.0, 10.0));
}
