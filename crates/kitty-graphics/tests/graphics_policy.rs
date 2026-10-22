// SPDX-License-Identifier: GPL-3.0-or-later
//! Policy and correctness tests for temporary files, relative placements,
//! indexed palettes and animation resource accounting.

use std::fs;
use std::path::{Path, PathBuf};

use kitty_graphics::{
    decode_image, decode_image_with_temp_policy, parse_raw, AnimationLimits, AnimationManager,
    GraphicsCommand, Image, ImageFormat, ImageLimits, IndexedPalette, PlacementError,
    PlacementGraph, PlacementKey, PlacementRecord, SourceRect, TempFilePolicy,
};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "kitty-graphics-policy-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_command(path: &Path) -> GraphicsCommand {
    let mut command = parse_raw(b"a=t,f=32,t=t,s=2,v=2,i=1").unwrap();
    command.payload = path.to_string_lossy().as_bytes().to_vec();
    command
}

#[test]
fn temp_file_is_read_and_unlinked() {
    let dir = TempDir::new("ok");
    let path = dir.join("image.bin");
    fs::write(&path, (0..64).collect::<Vec<u8>>()).unwrap();
    let policy = TempFilePolicy::new([dir.0.clone()], 1024);
    let command = temp_command(&path);
    let image =
        decode_image_with_temp_policy(&command, &command.payload, &ImageLimits::default(), &policy)
            .unwrap();
    assert_eq!(image.raw, &(0..16).collect::<Vec<u8>>()[..]);
    assert!(!path.exists());
}

#[test]
fn temp_file_outside_policy_is_rejected_and_preserved() {
    let inside = TempDir::new("inside");
    let outside = TempDir::new("outside");
    let path = outside.join("image.bin");
    fs::write(&path, [1u8; 16]).unwrap();
    let policy = TempFilePolicy::new([inside.0.clone()], 1024);
    let command = temp_command(&path);
    let error =
        decode_image_with_temp_policy(&command, &command.payload, &ImageLimits::default(), &policy)
            .unwrap_err();
    assert_eq!(error.code(), "EBADF");
    assert!(path.exists(), "outside files must never be deleted");
}

#[test]
fn temp_file_traversal_and_relative_paths_are_rejected() {
    let dir = TempDir::new("traversal");
    let policy = TempFilePolicy::new([dir.0.clone()], 1024);
    assert!(policy.validate(Path::new("relative.bin")).is_err());
    let escaped = dir.join("..").join("escaped.bin");
    fs::write(&escaped, [0u8; 16]).unwrap();
    assert!(policy.validate(&escaped).is_err());
    let _ = fs::remove_file(escaped);
}

#[cfg(unix)]
#[test]
fn temp_file_symlinks_are_rejected_and_targets_preserved() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("symlink");
    let target = dir.join("target.bin");
    fs::write(&target, [0u8; 16]).unwrap();
    let link = dir.join("link.bin");
    symlink(&target, &link).unwrap();
    let policy = TempFilePolicy::new([dir.0.clone()], 1024);
    let command = temp_command(&link);
    let error =
        decode_image_with_temp_policy(&command, &command.payload, &ImageLimits::default(), &policy)
            .unwrap_err();
    assert!(error.to_string().contains("symbolic link"));
    assert!(target.exists() && link.exists());

    let real_dir = dir.join("real-dir");
    fs::create_dir(&real_dir).unwrap();
    let nested = real_dir.join("nested.bin");
    fs::write(&nested, [0u8; 16]).unwrap();
    let dir_link = dir.join("dir-link");
    symlink(&real_dir, &dir_link).unwrap();
    let error = policy.validate(&dir_link.join("nested.bin")).unwrap_err();
    assert!(error.to_string().contains("symbolic link"));
    assert!(nested.exists());
}

#[test]
fn indexed_images_expand_through_the_palette() {
    let mut palette = IndexedPalette::default();
    let mut entries = *palette.entries();
    entries[1] = [10, 20, 30, 255];
    entries[2] = [40, 50, 60, 255];
    palette = IndexedPalette::from_rgba(entries);

    let mut opaque = parse_raw(b"a=t,f=8,s=2,v=1,i=1").unwrap();
    opaque.payload = vec![1, 2];
    let image = decode_image(&opaque, &opaque.payload, &ImageLimits::default()).unwrap();
    assert_eq!(image.rgba, None);
    assert_eq!(
        palette.expand_image(&image).unwrap(),
        vec![10, 20, 30, 255, 40, 50, 60, 255]
    );

    let mut alpha = parse_raw(b"a=t,f=16,s=2,v=1,i=1").unwrap();
    alpha.payload = vec![1, 0, 2, 128];
    let image = decode_image(&alpha, &alpha.payload, &ImageLimits::default()).unwrap();
    assert_eq!(
        palette.expand_image(&image).unwrap(),
        vec![10, 20, 30, 0, 40, 50, 60, 128]
    );
}

#[test]
fn indexed_special_colours_follow_the_protocol() {
    let palette = IndexedPalette::new(
        [[0; 4]; 256],
        [1, 2, 3, 9],
        [4, 5, 6, 9],
        [7, 8, 9, 9],
        [10, 11, 12, 9],
        [13, 14, 15, 9],
        [16, 17, 18, 9],
        [19, 20, 21, 9],
    );
    assert_eq!(palette.color(248), [1, 2, 3, 255]);
    assert_eq!(palette.color(249), [4, 5, 6, 255]);
    assert_eq!(palette.color(254), [19, 20, 21, 255]);
    assert_eq!(palette.color(255), [4, 5, 6, 255]);
    let mut image = parse_raw(b"a=t,f=8,s=1,v=1,i=1").unwrap();
    image.payload = vec![250];
    let decoded = decode_image(&image, &image.payload, &ImageLimits::default()).unwrap();
    assert_eq!(palette.expand_image(&decoded).unwrap(), vec![7, 8, 9, 255]);
}

fn placement(image: u32, id: u32, x: u32, y: u32) -> PlacementRecord {
    PlacementRecord::new(PlacementKey::new(image, id), x, y)
}

#[test]
fn relative_placements_resolve_offsets_z_and_source() {
    let parent = placement(1, 1, 3, 4)
        .with_source(SourceRect::new(1, 2, 3, 4))
        .with_z_index(-3);
    let child = placement(2, 1, 0, 0)
        .with_parent(1, 1)
        .with_offset(2, -1)
        .inheriting(true, true);
    let graph = PlacementGraph::from_records([parent, child]);
    let resolved = graph.resolve(PlacementKey::new(2, 1)).unwrap();
    assert_eq!((resolved.anchor_x, resolved.anchor_y), (5, 3));
    assert_eq!(resolved.z_index, -3);
    assert_eq!(resolved.source, SourceRect::new(1, 2, 3, 4));
}

#[test]
fn relative_placement_cycles_depth_and_cascades_are_enforced() {
    let a = placement(1, 1, 0, 0).with_parent(2, 1);
    let b = placement(2, 1, 0, 0).with_parent(1, 1);
    let graph = PlacementGraph::from_records([a, b]);
    assert_eq!(
        graph.validate(PlacementKey::new(1, 1)).unwrap_err().code(),
        "ECYCLE"
    );

    let mut chain = vec![placement(1, 1, 0, 0)];
    for depth in 2..=12 {
        chain.push(placement(1, depth, 0, 0).with_parent(1, depth - 1));
    }
    let graph = PlacementGraph::from_records(chain);
    assert_eq!(
        graph.validate(PlacementKey::new(1, 10)).unwrap_err().code(),
        "ETOODEEP"
    );

    let mut cascade = PlacementGraph::from_records([
        placement(1, 1, 0, 0),
        placement(2, 1, 0, 0).with_parent(1, 1),
        placement(3, 1, 0, 0).with_parent(2, 1),
        placement(4, 1, 0, 0),
    ]);
    assert_eq!(cascade.remove_cascade(PlacementKey::new(1, 1)).len(), 3);
    assert!(cascade.get(PlacementKey::new(4, 1)).is_some());
    assert_eq!(
        graph.validate(PlacementKey::new(99, 1)),
        Err(PlacementError::MissingParent(PlacementKey::new(99, 1)))
    );
}

fn root_image(id: u32, width: u32, height: u32, color: [u8; 4]) -> Image {
    let mut command =
        parse_raw(format!("a=t,f=32,s={width},v={height},i={id}").as_bytes()).unwrap();
    command.payload = color.repeat(width as usize * height as usize);
    decode_image(&command, &command.payload, &ImageLimits::default()).unwrap()
}

fn frame_command(
    id: u32,
    number: u32,
    source: u32,
    x: u32,
    y: u32,
    color: [u8; 4],
) -> GraphicsCommand {
    let mut command =
        parse_raw(format!("a=f,i={id},r={number},c={source},x={x},y={y},s=1,v=1").as_bytes())
            .unwrap();
    command.payload = color.to_vec();
    command
}

#[test]
fn animation_frames_are_stored_as_deltas_and_reconstruct() {
    let mut manager = AnimationManager::new();
    manager
        .insert_root(&root_image(1, 8, 8, [10, 0, 0, 255]))
        .unwrap();
    manager
        .feed(
            &frame_command(1, 2, 1, 1, 1, [0, 20, 0, 255]),
            &ImageLimits::default(),
            0,
        )
        .unwrap();
    manager
        .feed(
            &frame_command(1, 3, 2, 2, 2, [0, 0, 30, 255]),
            &ImageLimits::default(),
            0,
        )
        .unwrap();
    let image = manager.image(1).unwrap();
    assert!(image.frames[0].is_keyframe());
    assert!(!image.frames[1].is_keyframe());
    assert_eq!(image.frames[1].base_frame_id(), Some(1));
    assert_eq!(image.frames[2].base_frame_id(), Some(2));
    assert!(image.stored_bytes() < image.canvas_bytes() * image.frames.len());
    let frame = image.frame_data(3).unwrap();
    let pixel = |x: usize, y: usize| {
        let offset = (y * 8 + x) * 4;
        [
            frame[offset],
            frame[offset + 1],
            frame[offset + 2],
            frame[offset + 3],
        ]
    };
    assert_eq!(pixel(1, 1), [0, 20, 0, 255]);
    assert_eq!(pixel(2, 2), [0, 0, 30, 255]);
    assert_eq!(pixel(0, 0), [10, 0, 0, 255]);
}

#[test]
fn per_image_quota_evicts_old_frames() {
    let limits = AnimationLimits {
        max_image_bytes: 160,
        max_total_bytes: 4096,
        max_frame_chain: 32,
    };
    let mut manager = AnimationManager::new_with_limits(limits);
    manager
        .insert_root(&root_image(1, 4, 4, [1, 1, 1, 255]))
        .unwrap();
    for number in 2..=6 {
        manager
            .feed(
                &frame_command(1, number, 0, 0, 0, [number as u8, 0, 0, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
    }
    assert!(manager.image_bytes(1) <= limits.max_image_bytes);
    assert!(manager.evicted_frames() > 0);
    assert!(manager.image(1).unwrap().frames.len() < 6);
}

#[test]
fn image_quota_reports_enospc_and_global_eviction_counts() {
    let limits = AnimationLimits::new(8, 8);
    let mut manager = AnimationManager::new_with_limits(limits);
    let error = manager
        .insert_root(&root_image(1, 4, 4, [0, 0, 0, 255]))
        .unwrap_err();
    assert_eq!(error.code(), "ENOSPC");
    assert!(manager.is_empty());

    let limits = AnimationLimits::new(80, 100);
    let mut manager = AnimationManager::new_with_limits(limits);
    manager
        .insert_root(&root_image(1, 4, 4, [0, 0, 0, 255]))
        .unwrap();
    manager
        .insert_root(&root_image(2, 4, 4, [0, 0, 0, 255]))
        .unwrap();
    assert!(manager.total_bytes() <= limits.max_total_bytes);
    assert!(manager.evicted_images() > 0 || manager.evicted_frames() > 0);
}

#[test]
fn delete_and_clear_update_storage_accounting() {
    let mut manager = AnimationManager::new();
    manager
        .insert_root(&root_image(1, 4, 4, [0, 0, 0, 255]))
        .unwrap();
    manager
        .feed(
            &frame_command(1, 2, 1, 0, 0, [9, 9, 9, 255]),
            &ImageLimits::default(),
            0,
        )
        .unwrap();
    let total = manager.total_bytes();
    assert!(total > 0);
    assert!(manager.delete_frame(1, 2));
    assert!(manager.total_bytes() < total);
    manager.clear();
    assert_eq!(manager.total_bytes(), 0);
    assert!(manager.is_empty());
}

#[test]
fn png_indexed_data_still_expands_to_rgba() {
    let png =
        kitty_graphics::png::encode_png_rgba(2, 1, &[10, 20, 30, 255, 40, 50, 60, 128]).unwrap();
    let mut command = parse_raw(b"a=t,f=100,i=1").unwrap();
    command.payload = png;
    let image = decode_image(&command, &command.payload, &ImageLimits::default()).unwrap();
    assert_eq!(image.format, ImageFormat::Png);
    assert_eq!(
        image.rgba.as_deref(),
        Some(&[10, 20, 30, 255, 40, 50, 60, 128][..])
    );
}
