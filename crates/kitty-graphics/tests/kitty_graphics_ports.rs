// SPDX-License-Identifier: GPL-3.0-or-later
//! Integration tests porting parser/load cases from
//! `kitty_tests/graphics.py` and `kitty_tests/parser.py`.
//!
//! These exercise the public API the way kitty's `send_command()` helper does:
//! build `ESC_G <control> ; <base64 payload> ESC \`, parse it, then feed the
//! command to the image assembler.

use kitty_graphics::{
    decode_image, decode_strict, encode_base64, parse_escape, parse_raw, Action, GraphicsCommand,
    GraphicsError, ImageAssembler, ImageLimits, LoadOutcome, Placement, TransmissionType,
};

fn send(control: &str, payload: &[u8]) -> GraphicsCommand {
    let mut raw = control.as_bytes().to_vec();
    if !payload.is_empty() {
        raw.push(b';');
        raw.extend_from_slice(&encode_base64(payload, true));
    }
    parse_raw(&raw).unwrap()
}

fn send_escape(control: &str, payload: &[u8]) -> GraphicsCommand {
    let mut raw = b"\x1b_G".to_vec();
    raw.extend_from_slice(control.as_bytes());
    if !payload.is_empty() {
        raw.push(b';');
        raw.extend_from_slice(&encode_base64(payload, true));
    }
    raw.extend_from_slice(b"\x1b\\");
    parse_escape(&raw).unwrap()
}

const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+P+/HgAFhAJ/wlseKgAAAABJRU5ErkJggg==";

#[test]
fn load_png_simple_from_graphics_py() {
    // test_load_png_simple: 1x1 transparent PNG decoded by kitty as
    // b'\x00\xff\xff\x7f'.
    let png = decode_strict(PNG_1X1.as_bytes()).unwrap();
    let cmd = send_escape("a=t,f=100,i=1", &png);
    let mut assembler = ImageAssembler::default();
    match assembler.feed(&cmd).unwrap() {
        LoadOutcome::Loaded(loaded) => {
            assert_eq!((loaded.image.width, loaded.image.height), (1, 1));
            assert_eq!(loaded.image.rgba.as_deref(), Some(&[0, 255, 255, 127][..]));
            assert!(!loaded.image.is_opaque);
            assert!(loaded.image.is_4byte_aligned);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn load_png_malformed_is_ebadpng() {
    // test_load_png/png_simple: malformed bytes and short buffers must fail
    // with EBADPNG, never crash or panic.
    for data in [
        b"dsfsdfsfsfd".as_slice(),
        b"a".repeat(20).as_slice(),
        b"x".repeat(25).as_slice(),
    ] {
        let cmd = send("a=t,f=100,i=1", data);
        let err = decode_image(&cmd, &cmd.payload, &ImageLimits::default()).unwrap_err();
        assert_eq!(err.code(), "EBADPNG");
        assert!(err.to_string().starts_with("[EBADPNG]"));
    }
}

#[test]
fn load_images_raw_24_and_32() {
    // test_load_images: f=32 data is 4-byte aligned, f=24 is opaque RGB.
    let mut assembler = ImageAssembler::default();
    for (format, payload) in [(32u32, &b"abcd"[..]), (24u32, &b"abc"[..])] {
        let cmd = send(&format!("a=t,f={format},s=1,v=1,i=1"), payload);
        match assembler.feed(&cmd).unwrap() {
            LoadOutcome::Loaded(loaded) => {
                assert_eq!(loaded.image.raw, payload);
                assert_eq!(loaded.image.is_opaque, format == 24);
                assert_eq!(loaded.image.is_4byte_aligned, format == 32);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn chunked_png_transmission() {
    // test_load_images: chunked load with m=1/m=0. Each chunk is independently
    // base64 encoded, exactly as kitty's send_command() does.
    let png = decode_strict(PNG_1X1.as_bytes()).unwrap();
    let mid = png.len() / 2;
    let mut assembler = ImageAssembler::default();
    let first = send("a=t,f=100,i=1,m=1", &png[..mid]);
    assert_eq!(assembler.feed(&first).unwrap(), LoadOutcome::ChunkAccepted);
    let second = send("m=1", &png[mid..mid + 1]);
    assert_eq!(assembler.feed(&second).unwrap(), LoadOutcome::ChunkAccepted);
    let last = send("m=0", &png[mid + 1..]);
    match assembler.feed(&last).unwrap() {
        LoadOutcome::Loaded(loaded) => {
            assert_eq!(loaded.image.rgba.as_deref(), Some(&[0, 255, 255, 127][..]));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(!assembler.has_pending_transfer());
}

#[test]
fn delete_command_clears_interrupted_chunked_load() {
    // test_load_images: send_command(s, 'a=d') should clear partial transfer.
    let mut assembler = ImageAssembler::default();
    let first = send("a=t,f=32,i=1,s=1,v=2,m=1", b"abcd");
    assembler.feed(&first).unwrap();
    assert!(assembler.has_pending_transfer());
    let del = send("a=d", b"");
    assert_eq!(
        assembler.feed(&del).unwrap(),
        LoadOutcome::Deferred(del.clone())
    );
    assert!(!assembler.has_pending_transfer());
    let mut assembler = ImageAssembler::default();
    let first = send("a=t,f=32,i=1,s=1,v=2,m=1", b"abcd");
    assembler.feed(&first).unwrap();
    let last = send("m=0", b"efgh");
    match assembler.feed(&last).unwrap() {
        LoadOutcome::Loaded(loaded) => assert_eq!(loaded.image.raw, b"abcdefgh"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn operations_with_numbers_and_quiet_parse() {
    // test_gr_operations_with_numbers / test_suppressing_gr_command_responses.
    let cmd = send("a=t,f=24,i=3,I=1,s=1,v=1,q=2", b"abc");
    assert_eq!(cmd.id, 3);
    assert_eq!(cmd.image_number, 1);
    assert_eq!(cmd.quiet, 2);

    let cmd = send("a=q,i=1,q=1,s=1,v=1", b"abcd");
    assert_eq!(cmd.action, Some(Action::Query));
    assert_eq!(cmd.quiet, 1);
}

#[test]
fn image_number_only_and_continuation() {
    // test_gr_operations_with_numbers: chunked load started with I= and no i=.
    let mut assembler = ImageAssembler::default();
    let first = send("a=t,f=32,I=93,s=1,v=2,m=1", b"abcd");
    assert_eq!(first.image_number, 93);
    assert_eq!(first.id, 0);
    assembler.feed(&first).unwrap();
    let last = send("m=0", b"efgh");
    match assembler.feed(&last).unwrap() {
        LoadOutcome::Loaded(loaded) => {
            assert_eq!(loaded.image.number, 93);
            assert_eq!(loaded.image.raw, b"abcdefgh");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn transmit_and_display_placement() {
    // test_image_put: a=T carries the placement fields and requests display.
    let cmd = send(
        "a=T,f=32,i=2,s=2,v=2,x=3,y=4,w=5,h=6,c=7,r=8,X=9,Y=10,p=11,z=-1",
        &[0u8; 16],
    );
    let mut assembler = ImageAssembler::default();
    match assembler.feed(&cmd).unwrap() {
        LoadOutcome::Loaded(loaded) => {
            assert!(loaded.display);
            assert!(!loaded.query);
            let p = loaded.image.placement;
            assert_eq!((p.x_offset, p.y_offset), (3, 4));
            assert_eq!((p.width, p.height), (5, 6));
            assert_eq!((p.num_cols, p.num_rows), (7, 8));
            assert_eq!((p.cell_x_offset, p.cell_y_offset), (9, 10));
            assert_eq!(p.placement_id, 11);
            assert_eq!(p.z_index, -1);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn query_and_put_are_routed_correctly() {
    // test_load_images: a=q loads but does not display; a=p is not a load.
    let query = send("a=q,i=1,f=24,s=1,v=1", b"abc");
    let mut assembler = ImageAssembler::default();
    match assembler.feed(&query).unwrap() {
        LoadOutcome::Loaded(loaded) => assert!(loaded.query),
        other => panic!("unexpected {other:?}"),
    }
    let put = send("a=p,i=1,c=2,r=2", b"");
    assert_eq!(put.action, Some(Action::Put));
    assert!(matches!(
        assembler.feed(&put).unwrap(),
        LoadOutcome::Deferred(_)
    ));
}

#[test]
fn indexed_color_images_parse_and_load() {
    // test_indexed_color_images.
    for (format, width, height, payload, px_sz) in [
        (8u32, 4u32, 2u32, (0u8..8).collect::<Vec<_>>(), 1u8),
        (16, 2, 2, (0u8..8).collect::<Vec<_>>(), 2),
    ] {
        let cmd = send(
            &format!("a=t,f={format},s={width},v={height},i=1"),
            &payload,
        );
        let img = decode_image(&cmd, &cmd.payload, &ImageLimits::default()).unwrap();
        assert_eq!(img.raw, payload);
        assert_eq!(img.format.as_u32(), format);
        assert_eq!(img.format.bytes_per_pixel(), Some(px_sz as usize));
    }
    let cmd = send("a=t,f=8,s=3,v=1,i=1", &[0, 1, 2]);
    // f=8, 3 bytes: 3 < 3? no; data is exactly 3, size check passes.
    let img = decode_image(&cmd, &cmd.payload, &ImageLimits::default()).unwrap();
    assert_eq!(img.raw, [0, 1, 2]);
}

#[test]
fn insufficient_and_mismatched_data() {
    // test_indexed_color_images: f=16, s=3, v=1 with 5 bytes is insufficient.
    let cmd = send("a=t,f=16,s=3,v=1,i=1", &[0u8; 5]);
    let err = decode_image(&cmd, &cmd.payload, &ImageLimits::default()).unwrap_err();
    assert_eq!(err.code(), "ENODATA");
    assert!(matches!(err, GraphicsError::InsufficientData { .. }));

    // Declared S= does not match the dimensions for raw data.
    let cmd = send("a=t,f=32,s=1,v=1,S=7,i=1", &[0u8; 7]);
    let err = decode_image(&cmd, &cmd.payload, &ImageLimits::default()).unwrap_err();
    assert!(matches!(err, GraphicsError::DimensionMismatch { .. }));
    assert_eq!(err.code(), "EINVAL");

    // Zero dimensions are invalid for raw formats.
    let cmd = send("a=t,f=8,s=0,v=1,i=1", b"");
    assert_eq!(
        decode_image(&cmd, &cmd.payload, &ImageLimits::default()).unwrap_err(),
        GraphicsError::ZeroDimensions
    );
}

#[test]
fn transient_usage_hint_is_exposed() {
    // test_transient_graphics_image: N=1 marks the image transient.
    let cmd = send("a=t,f=24,s=1,v=1,i=1,N=1", b"abc");
    assert_eq!(cmd.usage_hints, 1);
    assert!(cmd.is_transient());
    let img = decode_image(&cmd, &cmd.payload, &ImageLimits::default()).unwrap();
    assert!(img.transient);
    assert!(img.placement.is_transient());
}

#[test]
fn file_transmission_fields_and_loading() {
    // test_load_images: t=f with S=/O= reads a file window.
    let path = std::env::temp_dir().join(format!(
        "kitty-graphics-ports-{}-{}",
        std::process::id(),
        line!()
    ));
    let data: Vec<u8> = (0..32).collect();
    std::fs::write(&path, &data).unwrap();
    let cmd = send_escape(
        "a=t,f=32,t=f,s=2,v=2,S=16,O=4,i=1",
        path.to_string_lossy().as_bytes(),
    );
    assert_eq!(cmd.transmission_type, Some(TransmissionType::File));
    let img = decode_image(&cmd, &cmd.payload, &ImageLimits::default()).unwrap();
    assert_eq!(img.raw, &data[4..20]);
    let _ = std::fs::remove_file(path);
}

#[test]
fn frame_and_compose_commands_are_deferred() {
    // Animation frame loading and compose are parsed but out of the image
    // loader's scope.
    let mut assembler = ImageAssembler::default();
    for (control, action) in [
        ("a=f,i=1,r=2", Action::FrameData),
        ("a=a,i=1", Action::AnimationFrame),
        ("a=c,i=1,r=1,c=2", Action::Compose),
    ] {
        let cmd = send(control, b"");
        assert_eq!(cmd.action, Some(action));
        assert!(matches!(
            assembler.feed(&cmd).unwrap(),
            LoadOutcome::Deferred(_)
        ));
    }
}

#[test]
fn malformed_control_block_messages_match_kitty() {
    // test_graphics_command in kitty_tests/parser.py.
    let cases: &[(&[u8], &str)] = &[
        (
            b"i=4294967296",
            "Malformed GraphicsCommand control block, number is too large",
        ),
        (
            b",s=1",
            "Malformed GraphicsCommand control block, invalid key character: 0x2c",
        ),
        (
            b"W=1",
            "Malformed GraphicsCommand control block, invalid key character: 0x57",
        ),
        (
            b"1=1",
            "Malformed GraphicsCommand control block, invalid key character: 0x31",
        ),
        (
            b"a=t,,w=2",
            "Malformed GraphicsCommand control block, invalid key character: 0x2c",
        ),
        (
            b"s",
            "Malformed GraphicsCommand control block, no = after key",
        ),
        (
            b"s=",
            "Malformed GraphicsCommand control block, expecting an integer value",
        ),
        (
            b"s==",
            "Malformed GraphicsCommand control block, expecting an integer value for key: s",
        ),
        (
            b"s=1=",
            "Malformed GraphicsCommand control block, expecting a , or semi-colon after a value, found: 0x3d",
        ),
    ];
    for (raw, expected) in cases {
        assert_eq!(parse_raw(raw).unwrap_err().to_string(), *expected);
    }
}

#[test]
fn payload_whitespace_and_invalid_chars_follow_libbase64() {
    // Kitty's graphics parser uses the lenient decoder: an invalid character
    // ends decoding but does not fail the control block. Whitespace is invalid.
    let cmd = parse_raw(b"a=t,f=24,s=1,v=1;YWJj\n").unwrap();
    assert_eq!(cmd.payload, b"abc");
    let cmd = parse_raw(b"a=t,f=24,s=1,v=1;YWJj!!!").unwrap();
    assert_eq!(cmd.payload, b"abc");
    // A chunk that ends in the middle of a base64 group is accepted leniently.
    let cmd = parse_raw(b"a=t,f=24,s=1,v=1;YWJjZ").unwrap();
    assert_eq!(cmd.payload, b"abc");
}

#[test]
fn parser_rejects_unknown_actions_and_media() {
    assert!(parse_raw(b"a=x").is_err());
    assert!(parse_raw(b"t=x").is_err());
    assert!(parse_raw(b"o=x").is_err());
    assert!(parse_raw(b"d=D").is_err());
    // 'C' is a valid delete action even though D/E are not.
    assert_eq!(
        parse_raw(b"d=C").unwrap().delete_action.unwrap().as_byte(),
        b'C'
    );
}

#[test]
fn limits_and_placement_defaults() {
    let defaults = ImageLimits::default();
    assert_eq!(defaults.max_dimension, 10_000);
    assert_eq!(defaults.max_data_size, 400_000_000);
    assert_eq!(Placement::default(), Placement::default());
    let cmd = send("a=t,f=24,s=1,v=1", b"abc");
    let img = decode_image(&cmd, &cmd.payload, &defaults).unwrap();
    assert_eq!(img.id, 0);
    assert_eq!(img.number, 0);
    assert_eq!(img.placement, Placement::default());
}
