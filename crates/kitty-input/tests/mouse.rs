// SPDX-License-Identifier: GPL-3.0-or-later
//! Ported expectations from `kitty_tests/keys.py::test_encode_mouse_event`,
//! `kitty_tests/mouse.py` and the encoding/mode logic in `kitty/mouse.c`.

use kitty_input::key_constants::*;
use kitty_input::*;

const L: u32 = 1;
const M: u32 = 2;
const R: u32 = 3;

fn encode(
    x: u32,
    y: u32,
    protocol: MouseTrackingProtocol,
    button: u32,
    action: MouseAction,
    mods: Modifiers,
) -> String {
    let pos = MousePosition::new(x - 1, y - 1);
    String::from_utf8(encode_mouse_event(&pos, protocol, button, action, mods).unwrap())
        .expect("ASCII mouse encoding")
}

fn sgr() -> MouseTrackingProtocol {
    MouseTrackingProtocol::Sgr
}

#[test]
fn sgr_buttons_and_actions() {
    assert_eq!(
        encode(1, 1, sgr(), L, MouseAction::Press, Modifiers::NONE),
        "<0;1;1M"
    );
    assert_eq!(
        encode(1, 1, sgr(), L, MouseAction::Release, Modifiers::NONE),
        "<0;1;1m"
    );
    assert_eq!(
        encode(1, 1, sgr(), 0, MouseAction::Move, Modifiers::NONE),
        "<35;1;1M"
    );
    assert_eq!(
        encode(1, 1, sgr(), L, MouseAction::Drag, Modifiers::NONE),
        "<32;1;1M"
    );
    assert_eq!(
        encode(1, 1, sgr(), R, MouseAction::Press, Modifiers::NONE),
        "<2;1;1M"
    );
    assert_eq!(
        encode(1, 1, sgr(), R, MouseAction::Release, Modifiers::NONE),
        "<2;1;1m"
    );
    assert_eq!(
        encode(1, 1, sgr(), R, MouseAction::Drag, Modifiers::NONE),
        "<34;1;1M"
    );
    assert_eq!(
        encode(1, 1, sgr(), M, MouseAction::Press, Modifiers::NONE),
        "<1;1;1M"
    );
    assert_eq!(
        encode(1, 1, sgr(), M, MouseAction::Release, Modifiers::NONE),
        "<1;1;1m"
    );
    assert_eq!(
        encode(1, 1, sgr(), M, MouseAction::Drag, Modifiers::NONE),
        "<33;1;1M"
    );
}

#[test]
fn sgr_large_coordinates_and_modifiers() {
    assert_eq!(
        encode(1234, 5678, sgr(), L, MouseAction::Press, Modifiers::NONE),
        "<0;1234;5678M"
    );
    assert_eq!(
        encode(1, 1, sgr(), L, MouseAction::Press, Modifiers::SHIFT),
        "<4;1;1M"
    );
    assert_eq!(
        encode(1, 1, sgr(), L, MouseAction::Press, Modifiers::ALT),
        "<8;1;1M"
    );
    assert_eq!(
        encode(1, 1, sgr(), L, MouseAction::Press, Modifiers::CTRL),
        "<16;1;1M"
    );
    assert_eq!(
        encode(
            3,
            4,
            sgr(),
            L,
            MouseAction::Press,
            Modifiers::SHIFT | Modifiers::ALT | Modifiers::CTRL
        ),
        "<28;3;4M"
    );
}

#[test]
fn normal_protocol_buttons() {
    let p = MouseTrackingProtocol::Normal;
    assert_eq!(
        encode(1, 1, p, L, MouseAction::Press, Modifiers::NONE),
        "M !!"
    );
    assert_eq!(
        encode(1, 1, p, L, MouseAction::Release, Modifiers::NONE),
        "M#!!"
    );
    assert_eq!(
        encode(1, 1, p, L, MouseAction::Drag, Modifiers::NONE),
        "M@!!"
    );
    assert_eq!(
        encode(1, 1, p, 0, MouseAction::Move, Modifiers::NONE),
        "MC!!"
    );
    assert_eq!(
        encode(1, 1, p, R, MouseAction::Press, Modifiers::NONE),
        "M\"!!"
    );
    assert_eq!(
        encode(1, 1, p, M, MouseAction::Press, Modifiers::NONE),
        "M!!!"
    );
    assert_eq!(
        encode(2, 3, p, L, MouseAction::Press, Modifiers::CTRL),
        "M0\"#"
    );
}

#[test]
fn normal_protocol_coordinate_clamping() {
    let p = MouseTrackingProtocol::Normal;
    let bytes = encode_mouse_event(
        &MousePosition::new(222, 222),
        p,
        L,
        MouseAction::Press,
        Modifiers::NONE,
    )
    .unwrap();
    assert_eq!(bytes, vec![b'M', 32, 255, 255]);
    assert!(encode_mouse_event(
        &MousePosition::new(223, 0),
        p,
        L,
        MouseAction::Press,
        Modifiers::NONE
    )
    .is_none());
    assert!(encode_mouse_event(
        &MousePosition::new(0, 223),
        p,
        L,
        MouseAction::Press,
        Modifiers::NONE
    )
    .is_none());
}

#[test]
fn utf8_protocol() {
    let p = MouseTrackingProtocol::Utf8;
    assert_eq!(
        encode(1, 1, p, L, MouseAction::Press, Modifiers::NONE),
        "M !!"
    );
    assert_eq!(
        encode(1, 1, p, L, MouseAction::Release, Modifiers::NONE),
        "M#!!"
    );
    assert_eq!(
        encode(1, 1, p, L, MouseAction::Drag, Modifiers::NONE),
        "M@!!"
    );
    let bytes = encode_mouse_event(
        &MousePosition::new(199, 299),
        p,
        L,
        MouseAction::Press,
        Modifiers::NONE,
    )
    .unwrap();
    assert_eq!(&bytes[..2], b"M ");
    assert_eq!(&bytes[2..], "\u{e8}\u{14c}".as_bytes());
}

#[test]
fn urxvt_protocol() {
    let p = MouseTrackingProtocol::Urxvt;
    assert_eq!(
        encode(1, 1, p, L, MouseAction::Press, Modifiers::NONE),
        "32;1;1M"
    );
    assert_eq!(
        encode(1, 1, p, L, MouseAction::Release, Modifiers::NONE),
        "32;1;1M"
    );
    assert_eq!(
        encode(1, 1, p, L, MouseAction::Drag, Modifiers::NONE),
        "64;1;1M"
    );
    assert_eq!(
        encode(5, 6, p, R, MouseAction::Press, Modifiers::NONE),
        "34;5;6M"
    );
}

#[test]
fn sgr_pixel_protocol() {
    let p = MouseTrackingProtocol::SgrPixel;
    let pos = MousePosition::new(0, 0).with_pixels(12.4, 34.6);
    assert_eq!(
        String::from_utf8(
            encode_mouse_event(&pos, p, L, MouseAction::Press, Modifiers::NONE).unwrap()
        )
        .unwrap(),
        "<0;12;35M"
    );
    assert_eq!(
        String::from_utf8(
            encode_mouse_event(&pos, p, L, MouseAction::Release, Modifiers::NONE).unwrap()
        )
        .unwrap(),
        "<0;12;35m"
    );
    assert_eq!(
        String::from_utf8(
            encode_mouse_event(&pos, p, 0, MouseAction::Leave, Modifiers::NONE).unwrap()
        )
        .unwrap(),
        "<288;12;35M"
    );
    assert!(encode_mouse_event(
        &pos,
        MouseTrackingProtocol::Sgr,
        0,
        MouseAction::Leave,
        Modifiers::NONE
    )
    .is_none());
    assert!(encode_mouse_event(
        &pos,
        MouseTrackingProtocol::Normal,
        0,
        MouseAction::Leave,
        Modifiers::NONE
    )
    .is_none());
}

#[test]
fn wheel_directions_in_every_protocol() {
    let expected = [("\x1b", 64, 65, 66, 67)];
    let _ = expected;
    for (protocol, press) in [
        (MouseTrackingProtocol::Sgr, "<64;1;1M"),
        (MouseTrackingProtocol::Sgr, "<65;1;1M"),
        (MouseTrackingProtocol::Sgr, "<66;1;1M"),
        (MouseTrackingProtocol::Sgr, "<67;1;1M"),
    ] {
        let _ = (protocol, press);
    }
    for (button, code) in [
        (MouseButton::WheelUp, 64),
        (MouseButton::WheelDown, 65),
        (MouseButton::WheelLeft, 66),
        (MouseButton::WheelRight, 67),
    ] {
        let pos = MousePosition::new(0, 0);
        let sgr =
            String::from_utf8(encode_mouse_scroll(&pos, sgr(), button, Modifiers::NONE).unwrap())
                .unwrap();
        assert_eq!(sgr, format!("<{code};1;1M"));
        let normal = String::from_utf8(
            encode_mouse_scroll(&pos, MouseTrackingProtocol::Normal, button, Modifiers::NONE)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(normal.as_bytes(), &[b'M', (code + 32) as u8, 33, 33]);
        let urxvt = String::from_utf8(
            encode_mouse_scroll(&pos, MouseTrackingProtocol::Urxvt, button, Modifiers::NONE)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(urxvt, format!("{};1;1M", code + 32));
        let utf8 = encode_mouse_scroll(&pos, MouseTrackingProtocol::Utf8, button, Modifiers::NONE)
            .unwrap();
        assert_eq!(utf8, vec![b'M', (code + 32) as u8, 33, 33]);
    }
}

#[test]
fn extra_buttons() {
    assert_eq!(button_map(GLFW_MOUSE_BUTTON_LEFT), 1);
    assert_eq!(button_map(GLFW_MOUSE_BUTTON_MIDDLE), 2);
    assert_eq!(button_map(GLFW_MOUSE_BUTTON_RIGHT), 3);
    for (i, expected) in [(3, 8), (4, 9), (5, 10), (6, 11), (7, 12)] {
        assert_eq!(button_map(i), expected);
    }
    assert_eq!(
        String::from_utf8(
            encode_mouse_button(
                &MousePosition::new(0, 0),
                sgr(),
                Some(MouseButton::Button4),
                MouseAction::Press,
                Modifiers::NONE
            )
            .unwrap()
        )
        .unwrap(),
        "<128;1;1M"
    );
    assert!(encode_mouse_button(
        &MousePosition::new(0, 0),
        sgr(),
        Some(MouseButton::Button8),
        MouseAction::Press,
        Modifiers::NONE
    )
    .is_none());
}

#[test]
fn tracking_modes_from_decset() {
    let mut modes = InputModes::default();
    assert_eq!(modes.mouse_tracking_mode, MouseTrackingMode::Off);
    modes.set_private_mode(1000, true);
    assert_eq!(modes.mouse_tracking_mode, MouseTrackingMode::Click);
    modes.set_private_mode(1000, false);
    assert_eq!(modes.mouse_tracking_mode, MouseTrackingMode::Off);
    modes.set_private_mode(1002, true);
    assert_eq!(modes.mouse_tracking_mode, MouseTrackingMode::Motion);
    modes.set_private_mode(1003, true);
    assert_eq!(modes.mouse_tracking_mode, MouseTrackingMode::Any);
    modes.set_private_mode(1003, false);
    assert_eq!(modes.mouse_tracking_mode, MouseTrackingMode::Off);
}

#[test]
fn protocols_from_decset() {
    let mut modes = InputModes::default();
    assert_eq!(modes.mouse_tracking_protocol, MouseTrackingProtocol::Normal);
    for (mode, protocol) in [
        (1005, MouseTrackingProtocol::Utf8),
        (1006, MouseTrackingProtocol::Sgr),
        (1015, MouseTrackingProtocol::Urxvt),
        (1016, MouseTrackingProtocol::SgrPixel),
    ] {
        modes.set_private_mode(mode, true);
        assert_eq!(modes.mouse_tracking_protocol, protocol);
        modes.set_private_mode(mode, false);
        assert_eq!(modes.mouse_tracking_protocol, MouseTrackingProtocol::Normal);
    }
}

#[test]
fn dispatch_respects_tracking_mode() {
    let pos = MousePosition::new(0, 0);
    let mut ctx = ScreenInputContext::new();
    assert!(dispatch_mouse_event(
        &ctx,
        &pos,
        Some(MouseButton::Left),
        MouseAction::Press,
        Modifiers::NONE
    )
    .is_none());
    ctx.modes.set_private_mode(1000, true);
    assert!(dispatch_mouse_event(
        &ctx,
        &pos,
        Some(MouseButton::Left),
        MouseAction::Press,
        Modifiers::NONE
    )
    .is_some());
    assert!(dispatch_mouse_event(&ctx, &pos, None, MouseAction::Move, Modifiers::NONE).is_none());
    ctx.modes.set_private_mode(1000, false);
    ctx.modes.set_private_mode(1002, true);
    assert!(dispatch_mouse_event(
        &ctx,
        &pos,
        Some(MouseButton::Left),
        MouseAction::Drag,
        Modifiers::NONE
    )
    .is_some());
    assert!(dispatch_mouse_event(&ctx, &pos, None, MouseAction::Move, Modifiers::NONE).is_none());
    ctx.modes.set_private_mode(1003, true);
    assert_eq!(
        String::from_utf8(
            dispatch_mouse_event(&ctx, &pos, None, MouseAction::Move, Modifiers::NONE).unwrap()
        )
        .unwrap(),
        "MC!!"
    );
}

struct ScaledCtx {
    inner: ScreenInputContext,
    scale: f64,
}

impl InputContext for ScaledCtx {
    fn modes(&self) -> &InputModes {
        self.inner.modes()
    }
    fn key_encoding_flags(&self) -> KittyKeyboardFlags {
        self.inner.key_encoding_flags()
    }
    fn pixel_scale(&self) -> f64 {
        self.scale
    }
}

#[test]
fn pixel_scaling_callback() {
    let mut inner = ScreenInputContext::new();
    inner.modes.set_private_mode(1003, true);
    inner.modes.set_private_mode(1016, true);
    let ctx = ScaledCtx { inner, scale: 2.0 };
    let pos = MousePosition::new(0, 0).with_pixels(10.0, 20.0);
    assert_eq!(
        String::from_utf8(
            dispatch_mouse_event(
                &ctx,
                &pos,
                Some(MouseButton::Left),
                MouseAction::Press,
                Modifiers::NONE
            )
            .unwrap()
        )
        .unwrap(),
        "<0;20;40M"
    );
}

#[test]
fn cell_for_pos_mapping_and_clamping() {
    let geom = MouseGeometry {
        left: 10.0,
        top: 20.0,
        right: 110.0,
        bottom: 100.0,
        cell_width: 10.0,
        cell_height: 10.0,
        columns: 10,
        lines: 8,
        pixel_scroll_offset_y: 0.0,
    };
    let p = cell_for_pos(15.0, 25.0, &geom, false, 1.0).unwrap();
    assert_eq!((p.cell_x, p.cell_y, p.in_left_half_of_cell), (0, 0, true));
    assert_eq!((p.global_x, p.global_y), (5.0, 5.0));
    let p = cell_for_pos(18.0, 25.0, &geom, false, 1.0).unwrap();
    assert_eq!((p.cell_x, p.cell_y, p.in_left_half_of_cell), (0, 0, false));
    let p = cell_for_pos(25.0, 25.0, &geom, false, 1.0).unwrap();
    assert_eq!((p.cell_x, p.cell_y, p.in_left_half_of_cell), (1, 0, true));
    let p = cell_for_pos(110.0, 100.0, &geom, false, 1.0).unwrap();
    assert_eq!((p.cell_x, p.cell_y, p.in_left_half_of_cell), (9, 7, false));
    assert!(cell_for_pos(5.0, 5.0, &geom, false, 1.0).is_none());
    let p = cell_for_pos(5.0, 5.0, &geom, true, 1.0).unwrap();
    assert_eq!((p.cell_x, p.cell_y), (0, 0));
    assert_eq!((p.global_x, p.global_y), (-5.0, -15.0));
    let p = cell_for_pos(15.0, 25.0, &geom, false, 2.0).unwrap();
    assert_eq!((p.global_x, p.global_y), (10.0, 10.0));
    let mut geom2 = geom;
    geom2.pixel_scroll_offset_y = 5.0;
    assert_eq!(
        cell_for_pos(15.0, 25.0, &geom2, false, 1.0).unwrap().cell_y,
        0
    );
}

#[test]
fn mouse_shapes_are_present() {
    assert_eq!(MouseShape::default(), MouseShape::Invalid);
    assert_eq!(MouseShape::Text as u8, 2);
    assert_eq!(MouseShape::Grabbing as u8, 30);
}
