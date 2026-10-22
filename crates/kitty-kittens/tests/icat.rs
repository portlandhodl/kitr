// SPDX-License-Identifier: GPL-3.0-or-later
//! Pixel/byte-level tests for the Rust-native `icat` encoder.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use kitty_graphics::command::{Action, DeleteAction, GraphicsCommand};
use kitty_graphics::png::encode_png_rgba;
use kitty_graphics::{decode_png, parse_escape, ImageAssembler, LoadOutcome};
use kitty_kittens::icat::{
    encode_png, encode_png_frames, parse_icat_args, run_icat, Align, FitMode, IcatError,
    IcatOptions, OutputFormat, Place, Scale, StdinMode, WindowSize,
};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "kitty-kittens-icat-{}-{}-{}",
            std::process::id(),
            label,
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn rgba(width: u32, height: u32) -> Vec<u8> {
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            data.push((x * 40) as u8);
            data.push((y * 60) as u8);
            data.push(((x + y) * 20) as u8);
            data.push(255);
        }
    }
    data
}

fn png(width: u32, height: u32) -> Vec<u8> {
    encode_png_rgba(width, height, &rgba(width, height)).unwrap()
}

fn find(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    haystack[start..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|index| index + start)
}

fn parse_frames(output: &[u8]) -> Vec<GraphicsCommand> {
    let mut frames = Vec::new();
    let mut offset = 0usize;
    while let Some(start) = find(output, b"\x1b_G", offset) {
        let end = find(output, b"\x1b\\", start + 3).expect("graphics frame terminator");
        let frame_end = end + 2;
        frames.push(parse_escape(&output[start..frame_end]).expect("valid graphics frame"));
        offset = frame_end;
    }
    frames
}

fn image_frames(output: &[u8]) -> Vec<GraphicsCommand> {
    parse_frames(output)
        .into_iter()
        .filter(|frame| frame.action != Some(Action::Delete))
        .collect()
}

#[test]
fn one_by_one_png_is_a_single_png_transmission() {
    let data = png(1, 1);
    let options = IcatOptions {
        trailing_newline: false,
        ..IcatOptions::default()
    };
    let frames = image_frames(&encode_png(&data, &options).unwrap());
    assert_eq!(frames.len(), 1);
    let frame = &frames[0];
    assert_eq!(frame.action, Some(Action::TransmitAndDisplay));
    assert_eq!(frame.format, 100);
    assert_eq!((frame.data_width, frame.data_height), (1, 1));
    assert_eq!(frame.payload, data);
}

#[test]
fn two_by_two_png_preserves_pixels_after_parser_roundtrip() {
    let data = png(2, 2);
    let frames = image_frames(&encode_png(&data, &IcatOptions::default()).unwrap());
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].payload, data);
    let decoded = decode_png(&frames[0].payload, 10_000).unwrap();
    assert_eq!((decoded.width, decoded.height), (2, 2));
    assert_eq!(decoded.rgba, rgba(2, 2));
}

#[test]
fn rgb_and_rgba_formats_emit_raw_pixels() {
    let data = png(2, 1);
    let rgb = image_frames(
        &encode_png(
            &data,
            &IcatOptions {
                format: OutputFormat::Rgb,
                ..IcatOptions::default()
            },
        )
        .unwrap(),
    );
    assert_eq!(rgb[0].format, 24);
    assert_eq!(rgb[0].payload.len(), 2 * 3);
    assert_eq!(&rgb[0].payload[..3], &[0, 0, 0]);

    let rgba_frames = image_frames(
        &encode_png(
            &data,
            &IcatOptions {
                format: OutputFormat::Rgba,
                ..IcatOptions::default()
            },
        )
        .unwrap(),
    );
    assert_eq!(rgba_frames[0].format, 32);
    assert_eq!(rgba_frames[0].payload, rgba(2, 1));
}

#[test]
fn chunking_reassembles_to_the_original_payload() {
    let data = png(2, 2);
    let options = IcatOptions {
        chunk_size: 7,
        ..IcatOptions::default()
    };
    let frames = image_frames(&encode_png(&data, &options).unwrap());
    assert!(frames.len() > 1);
    for frame in &frames[..frames.len() - 1] {
        assert_eq!(frame.more, 1);
    }
    assert_eq!(frames.last().unwrap().more, 0);
    let reassembled: Vec<u8> = frames
        .iter()
        .flat_map(|frame| frame.payload.iter().copied())
        .collect();
    assert_eq!(reassembled, data);

    let mut assembler = ImageAssembler::default();
    let mut loaded = None;
    for frame in &frames {
        match assembler.feed(frame).unwrap() {
            LoadOutcome::ChunkAccepted => {}
            LoadOutcome::Loaded(image) => loaded = Some(image),
            LoadOutcome::Deferred(_) => panic!("unexpected deferred frame"),
        }
    }
    let loaded = loaded.expect("chunked image loaded");
    assert_eq!((loaded.image.width, loaded.image.height), (2, 2));
    assert!(loaded.display);
}

#[test]
fn place_sets_cells_offsets_and_static_cursor() {
    let data = png(2, 2);
    let options = IcatOptions {
        place: Some(Place {
            width: 40,
            height: 20,
            left: 3,
            top: 4,
        }),
        align: Align::Center,
        window_size: Some(WindowSize {
            cols: 80,
            rows: 24,
            width_px: 800,
            height_px: 480,
        }),
        ..IcatOptions::default()
    };
    let output = encode_png(&data, &options).unwrap();
    assert!(output.starts_with(b"\x1b[5;23H"), "cursor move missing");
    let frames = image_frames(&output);
    let frame = &frames[0];
    assert_eq!(frame.cursor_movement, 1);
    assert_eq!(frame.num_cells, 1);
    assert_eq!(frame.num_lines, 1);
    assert_eq!(frame.cell_x_offset, 4);
}

#[test]
fn exact_and_fit_scaling_change_the_transmitted_png_dimensions() {
    let data = png(20, 10);
    let exact = IcatOptions {
        scale: Some(Scale::Exact {
            width: 7,
            height: 5,
        }),
        ..IcatOptions::default()
    };
    let frames = image_frames(&encode_png(&data, &exact).unwrap());
    assert_eq!((frames[0].data_width, frames[0].data_height), (7, 5));
    assert_eq!(decode_png(&frames[0].payload, 10_000).unwrap().width, 7);

    let fit = IcatOptions {
        scale: Some(Scale::Fit(FitMode::Width)),
        window_size: Some(WindowSize {
            cols: 10,
            rows: 10,
            width_px: 100,
            height_px: 100,
        }),
        ..IcatOptions::default()
    };
    let frames = image_frames(&encode_png(&png(200, 100), &fit).unwrap());
    assert_eq!((frames[0].data_width, frames[0].data_height), (100, 50));
}

#[test]
fn scale_up_only_enlarges_when_requested() {
    let data = png(2, 2);
    let window = WindowSize {
        cols: 10,
        rows: 10,
        width_px: 100,
        height_px: 100,
    };
    let without = IcatOptions {
        scale: Some(Scale::Fit(FitMode::Both)),
        window_size: Some(window),
        ..IcatOptions::default()
    };
    let frames = image_frames(&encode_png(&data, &without).unwrap());
    assert_eq!((frames[0].data_width, frames[0].data_height), (2, 2));

    let with = IcatOptions {
        scale: Some(Scale::Fit(FitMode::Both)),
        scale_up: true,
        window_size: Some(window),
        ..IcatOptions::default()
    };
    let frames = image_frames(&encode_png(&data, &with).unwrap());
    assert_eq!((frames[0].data_width, frames[0].data_height), (100, 100));
}

#[test]
fn alignment_changes_the_horizontal_offset() {
    let data = png(2, 2);
    let base = IcatOptions {
        window_size: Some(WindowSize {
            cols: 10,
            rows: 10,
            width_px: 100,
            height_px: 100,
        }),
        ..IcatOptions::default()
    };
    let left_output = encode_png(
        &data,
        &IcatOptions {
            align: Align::Left,
            ..base.clone()
        },
    )
    .unwrap();
    let center_output = encode_png(
        &data,
        &IcatOptions {
            align: Align::Center,
            ..base.clone()
        },
    )
    .unwrap();
    let right_output = encode_png(
        &data,
        &IcatOptions {
            align: Align::Right,
            ..base
        },
    )
    .unwrap();
    let left = image_frames(&left_output);
    let center = image_frames(&center_output);
    let right = image_frames(&right_output);
    assert_eq!(left[0].cell_x_offset, 0);
    assert_eq!(center[0].cell_x_offset, 4);
    assert_eq!(right[0].cell_x_offset, 8);
    assert!(left_output.starts_with(b"\r"));
    assert!(center_output.starts_with(b"\r\x1b[4C"));
    assert!(right_output.starts_with(b"\r\x1b[9C"));
}

#[test]
fn z_index_and_image_id_are_forwarded() {
    let frames = image_frames(
        &encode_png(
            &png(1, 1),
            &IcatOptions {
                z_index: -5,
                image_id: 42,
                ..IcatOptions::default()
            },
        )
        .unwrap(),
    );
    assert_eq!(frames[0].z_index, -5);
    assert_eq!(frames[0].id, 42);
}

#[test]
fn clear_and_clear_all_emit_delete_commands_first() {
    let data = png(1, 1);
    let plain = image_frames(
        &encode_png(
            &data,
            &IcatOptions {
                clear: true,
                ..IcatOptions::default()
            },
        )
        .unwrap(),
    );
    assert_eq!(plain.len(), 1);

    let frames = parse_frames(
        &encode_png(
            &data,
            &IcatOptions {
                clear: true,
                clear_all: true,
                ..IcatOptions::default()
            },
        )
        .unwrap(),
    );
    assert_eq!(frames[0].action, Some(Action::Delete));
    assert_eq!(frames[0].delete_action, Some(DeleteAction::AllButId));
    assert_eq!(frames[1].action, Some(Action::Delete));
    assert_eq!(frames[1].delete_action, Some(DeleteAction::FramesRangeById));
    assert_eq!(frames[2].action, Some(Action::TransmitAndDisplay));
}

#[test]
fn stdin_and_file_inputs_run_through_the_same_encoder() {
    let data = png(2, 2);
    let args = parse_icat_args(["-"]).unwrap();
    let mut input = Cursor::new(data.clone());
    let mut output = Vec::new();
    run_icat(&args, &mut input, &mut output).unwrap();
    let frames = image_frames(&output);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].payload, data);

    let dir = TempDir::new("file-input");
    let path = dir.path.join("image.png");
    std::fs::write(&path, &data).unwrap();
    let args = parse_icat_args([path.to_str().unwrap()]).unwrap();
    let mut output = Vec::new();
    run_icat(&args, &mut Cursor::new(Vec::new()), &mut output).unwrap();
    assert_eq!(image_frames(&output)[0].payload, data);
}

#[test]
fn stdin_can_be_disabled_and_writes_a_trailing_newline() {
    let args = parse_icat_args(["--stdin=no"]).unwrap();
    let error = run_icat(&args, &mut Cursor::new(png(1, 1)), &mut Vec::new()).unwrap_err();
    assert!(matches!(error, IcatError::NoInput));

    let args = parse_icat_args(["-"]).unwrap();
    let mut output = Vec::new();
    run_icat(&args, &mut Cursor::new(png(1, 1)), &mut output).unwrap();
    assert!(output.ends_with(b"\n"));

    let args = parse_icat_args(["--no-trailing-newline", "-"]).unwrap();
    let mut output = Vec::new();
    run_icat(&args, &mut Cursor::new(png(1, 1)), &mut output).unwrap();
    assert!(!output.ends_with(b"\n"));
}

#[test]
fn place_rejects_multiple_inputs() {
    let dir = TempDir::new("place-multiple");
    let first = dir.path.join("a.png");
    let second = dir.path.join("b.png");
    std::fs::write(&first, png(1, 1)).unwrap();
    std::fs::write(&second, png(1, 1)).unwrap();
    let args = parse_icat_args([
        "--place",
        "10x10@0x0",
        first.to_str().unwrap(),
        second.to_str().unwrap(),
    ])
    .unwrap();
    let error = run_icat(&args, &mut Cursor::new(Vec::new()), &mut Vec::new()).unwrap_err();
    assert!(matches!(error, IcatError::PlaceRequiresSingleImage(2)));
}

#[test]
fn invalid_png_and_missing_files_are_errors() {
    let error = encode_png(b"not a png", &IcatOptions::default()).unwrap_err();
    assert!(matches!(error, IcatError::Png(_)));

    let args = parse_icat_args(["--stdin=no", "/definitely/missing/image.png"]).unwrap();
    let error = run_icat(&args, &mut Cursor::new(Vec::new()), &mut Vec::new()).unwrap_err();
    assert!(matches!(error, IcatError::Message(_)));
}

#[test]
fn parsed_cli_options_drive_the_encoder() {
    let args = parse_icat_args([
        "--place=10x5@1x2",
        "--scale=up",
        "--align=left",
        "--z-index",
        "-3",
        "--format=rgba",
        "--image-id=9",
        "--use-window-size=20,10,200,100",
        "--clear",
        "image.png",
    ])
    .unwrap();
    assert_eq!(args.options.place.unwrap().width, 10);
    assert_eq!(args.options.align, Align::Left);
    assert_eq!(args.options.z_index, -3);
    assert_eq!(args.options.format, OutputFormat::Rgba);
    assert_eq!(args.options.image_id, 9);
    assert!(args.options.scale_up);
    assert_eq!(args.options.window_size.unwrap().width_px, 200);
    assert!(args.options.clear);
    assert_eq!(args.options.stdin, StdinMode::Auto);
    assert_eq!(args.files, vec!["image.png".to_owned()]);
}

#[test]
fn encode_png_frames_does_not_include_clear_or_newlines() {
    let frames = encode_png_frames(
        &png(1, 1),
        &IcatOptions {
            clear: true,
            trailing_newline: true,
            ..IcatOptions::default()
        },
    )
    .unwrap();
    assert_eq!(frames.len(), 2);
    assert!(frames[0].starts_with(b"\r"));
    assert!(frames[1].starts_with(b"\x1b_G"));
    assert!(!frames[1].ends_with(b"\n"));
}

#[test]
fn text_before_and_after_frames_is_not_confused_with_payload() {
    let output = encode_png(&png(1, 1), &IcatOptions::default()).unwrap();
    let mut noisy = b"prefix\r\n".to_vec();
    noisy.extend_from_slice(&output);
    noisy.extend_from_slice(b"suffix");
    let frames = image_frames(&noisy);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].format, 100);
}

#[test]
fn path_helper_keeps_file_names_intact() {
    assert_eq!(Path::new("/tmp/a.png").file_name().unwrap(), "a.png");
}
