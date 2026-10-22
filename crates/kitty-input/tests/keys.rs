// SPDX-License-Identifier: GPL-3.0-or-later
//! Ported expectations from `kitty_tests/keys.py`.

use kitty_input::key_constants::*;
use kitty_input::*;

fn s(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).expect("key encoding is UTF-8")
}

fn enc_full(
    key: u32,
    shifted_key: u32,
    alternate_key: u32,
    mods: Modifiers,
    action: KeyAction,
    flags: KittyKeyboardFlags,
    text: Option<&str>,
) -> String {
    let mut ev = KeyEvent {
        key,
        shifted_key,
        alternate_key,
        modifiers: mods,
        action,
        ..KeyEvent::default()
    };
    ev.text = text.map(str::to_string);
    s(encode_key_for_tty(&ev, false, flags))
}

fn enc(key: u32) -> String {
    enc_full(
        key,
        0,
        0,
        Modifiers::NONE,
        KeyAction::Press,
        KittyKeyboardFlags::NONE,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn csi(
    mods: u32,
    num: u32,
    action: u8,
    shifted_key: u32,
    alternate_key: u32,
    text: Option<&str>,
    trailer: char,
) -> String {
    let mut ans = String::from("\x1b[");
    if num != 1 || mods != 0 || shifted_key != 0 || alternate_key != 0 || text.is_some() {
        ans.push_str(&num.to_string());
    }
    if shifted_key != 0 || alternate_key != 0 {
        ans.push(':');
        if shifted_key != 0 {
            ans.push_str(&shifted_key.to_string());
        }
        if alternate_key != 0 {
            ans.push(':');
            ans.push_str(&alternate_key.to_string());
        }
    }
    if mods != 0 || action > 1 || text.is_some() {
        let m = mods & 0xff;
        if action > 1 || m != 0 {
            ans.push_str(&format!(";{}", m + 1));
            if action > 1 {
                ans.push_str(&format!(":{action}"));
            }
        } else if text.is_some() {
            ans.push(';');
        }
    }
    if let Some(text) = text {
        ans.push(';');
        ans.push_str(
            &text
                .chars()
                .map(|c| (c as u32).to_string())
                .collect::<Vec<_>>()
                .join(":"),
        );
    }
    ans.push(trailer);
    ans
}

fn simple_csi(mods: u32, num: u32, trailer: char) -> String {
    csi(mods, num, 1, 0, 0, None, trailer)
}

#[allow(clippy::too_many_arguments)]
fn mods_test(
    key: u32,
    plain: Option<&str>,
    shift: Option<&str>,
    ctrl: Option<&str>,
    _alt: Option<&str>,
    calt: Option<&str>,
    cshift: Option<&str>,
    ashift: Option<&str>,
    csi_num: u32,
    trailer: char,
) {
    let num = if csi_num == 0 { key } else { csi_num };
    let c = |mods: u32| simple_csi(mods, num, trailer);
    let e = |mods: Modifiers, action: KeyAction, text: Option<&str>| {
        enc_full(key, 0, 0, mods, action, KittyKeyboardFlags::NONE, text)
    };
    assert_eq!(
        e(Modifiers::NONE, KeyAction::Press, None),
        plain.map(str::to_string).unwrap_or_else(|| c(0))
    );
    assert_eq!(
        e(Modifiers::SHIFT, KeyAction::Press, None),
        shift
            .map(str::to_string)
            .unwrap_or_else(|| c(GLFW_MOD_SHIFT))
    );
    assert_eq!(
        e(Modifiers::CTRL, KeyAction::Press, None),
        ctrl.map(str::to_string)
            .unwrap_or_else(|| c(GLFW_MOD_CONTROL))
    );
    assert_eq!(
        e(Modifiers::ALT | Modifiers::CTRL, KeyAction::Press, None),
        calt.map(str::to_string)
            .unwrap_or_else(|| c(GLFW_MOD_ALT | GLFW_MOD_CONTROL))
    );
    assert_eq!(
        e(Modifiers::SHIFT | Modifiers::CTRL, KeyAction::Press, None),
        cshift
            .map(str::to_string)
            .unwrap_or_else(|| c(GLFW_MOD_CONTROL | GLFW_MOD_SHIFT))
    );
    assert_eq!(
        e(Modifiers::SHIFT | Modifiers::ALT, KeyAction::Press, None),
        ashift
            .map(str::to_string)
            .unwrap_or_else(|| c(GLFW_MOD_ALT | GLFW_MOD_SHIFT))
    );
}

fn mkp(name: &str, plain: Option<&str>, csi_num: u32, trailer: char) {
    let key = key_from_name(name).unwrap();
    mods_test(
        key, plain, None, None, None, None, None, None, csi_num, trailer,
    );
    let kp = key_from_name(&format!("KP_{name}")).unwrap();
    mods_test(
        kp, plain, None, None, None, None, None, None, csi_num, trailer,
    );
}

#[test]
fn legacy_functional_keys_with_modifiers() {
    mods_test(
        GLFW_FKEY_ENTER,
        Some("\r"),
        Some("\r"),
        Some("\r"),
        Some("\x1b\r"),
        Some("\x1b\r"),
        Some("\r"),
        Some("\x1b\r"),
        0,
        'u',
    );
    mods_test(
        GLFW_FKEY_KP_ENTER,
        Some("\r"),
        Some("\r"),
        Some("\r"),
        Some("\x1b\r"),
        Some("\x1b\r"),
        Some("\r"),
        Some("\x1b\r"),
        0,
        'u',
    );
    mods_test(
        GLFW_FKEY_ESCAPE,
        Some("\x1b"),
        Some("\x1b"),
        Some("\x1b"),
        Some("\x1b\x1b"),
        Some("\x1b\x1b"),
        Some("\x1b"),
        Some("\x1b\x1b"),
        0,
        'u',
    );
    mods_test(
        GLFW_FKEY_BACKSPACE,
        Some("\x7f"),
        Some("\x7f"),
        Some("\x08"),
        Some("\x1b\x7f"),
        Some("\x1b\x08"),
        Some("\x08"),
        Some("\x1b\x7f"),
        0,
        'u',
    );
    mods_test(
        GLFW_FKEY_TAB,
        Some("\t"),
        Some("\x1b[Z"),
        Some("\t"),
        Some("\x1b\t"),
        Some("\x1b\t"),
        Some("\x1b[Z"),
        Some("\x1b\x1b[Z"),
        0,
        'u',
    );
    mkp("INSERT", None, 2, '~');
    mkp("DELETE", None, 3, '~');
    mkp("PAGE_UP", None, 5, '~');
    mkp("PAGE_DOWN", None, 6, '~');
    mkp("HOME", None, 1, 'H');
    mkp("END", None, 1, 'F');
    mods_test(
        GLFW_FKEY_F1,
        Some("\x1bOP"),
        None,
        None,
        None,
        None,
        None,
        None,
        1,
        'P',
    );
    mods_test(
        GLFW_FKEY_F2,
        Some("\x1bOQ"),
        None,
        None,
        None,
        None,
        None,
        None,
        1,
        'Q',
    );
    mods_test(
        GLFW_FKEY_F3,
        Some("\x1bOR"),
        None,
        None,
        None,
        None,
        None,
        None,
        13,
        '~',
    );
    mods_test(
        GLFW_FKEY_F4,
        Some("\x1bOS"),
        None,
        None,
        None,
        None,
        None,
        None,
        1,
        'S',
    );
    mods_test(
        GLFW_FKEY_F5,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        15,
        '~',
    );
    mods_test(
        GLFW_FKEY_F6,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        17,
        '~',
    );
    mods_test(
        GLFW_FKEY_F7,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        18,
        '~',
    );
    mods_test(
        GLFW_FKEY_F8,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        19,
        '~',
    );
    mods_test(
        GLFW_FKEY_F9,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        20,
        '~',
    );
    mods_test(
        GLFW_FKEY_F10,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        21,
        '~',
    );
    mods_test(
        GLFW_FKEY_F11,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        23,
        '~',
    );
    mods_test(
        GLFW_FKEY_F12,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        24,
        '~',
    );
    mkp("UP", None, 1, 'A');
    mkp("DOWN", None, 1, 'B');
    mkp("RIGHT", None, 1, 'C');
    mkp("LEFT", None, 1, 'D');
}

#[test]
fn legacy_printable_ascii() {
    let shift_map: Vec<(char, char)> =
        "`~ 1! 2@ 3# 4$ 5% 6^ 7& 8* 9( 0) -_ =+ [{ ]} \\| ;: '\" ,< .> /?"
            .split_whitespace()
            .map(|pair| {
                let mut chars = pair.chars();
                (chars.next().unwrap(), chars.next().unwrap())
            })
            .chain(('a'..='z').map(|c| (c, c.to_ascii_uppercase())))
            .collect();
    let ctrl_map = |key: char| -> char {
        match key {
            ' ' | '@' | '2' => '\0',
            'a'..='z' => char::from_u32(key as u32 - 'a' as u32 + 1).unwrap(),
            '/' | '7' | '_' => '\x1f',
            '0' => '0',
            '1' => '1',
            '3' | '[' => '\x1b',
            '4' | '\\' => '\x1c',
            '5' | ']' => '\x1d',
            '6' | '^' | '~' => '\x1e',
            '8' | '?' => '\x7f',
            '9' => '9',
            _ => key,
        }
    };
    for (key, shifted) in shift_map {
        let base = key as u32;
        let sh = shifted as u32;
        let e = |mods: Modifiers| {
            enc_full(
                base,
                sh,
                0,
                mods,
                KeyAction::Press,
                KittyKeyboardFlags::NONE,
                None,
            )
        };
        assert_eq!(e(Modifiers::NONE), key.to_string());
        assert_eq!(e(Modifiers::SHIFT), shifted.to_string());
        assert_eq!(e(Modifiers::ALT), format!("\x1b{key}"));
        assert_eq!(e(Modifiers::CTRL), ctrl_map(key).to_string());
        assert_eq!(
            e(Modifiers::SHIFT | Modifiers::ALT),
            format!("\x1b{shifted}")
        );
        assert_eq!(
            e(Modifiers::CTRL | Modifiers::ALT),
            format!("\x1b{}", ctrl_map(key))
        );
    }
}

#[test]
fn legacy_ascii_edge_cases() {
    assert_eq!(
        enc_full(
            b':' as u32,
            b'/' as u32,
            0,
            Modifiers::SHIFT | Modifiers::ALT,
            KeyAction::Press,
            KittyKeyboardFlags::NONE,
            None
        ),
        "\x1b/"
    );
    for key in "~!@#$%^&*()_+{}|:\"<>?".chars() {
        assert_eq!(
            enc_full(
                key as u32,
                0,
                0,
                Modifiers::ALT,
                KeyAction::Press,
                KittyKeyboardFlags::NONE,
                None
            ),
            format!("\x1b{key}")
        );
    }
    let space = b' ' as u32;
    let e = |mods: Modifiers| {
        enc_full(
            space,
            0,
            0,
            mods,
            KeyAction::Press,
            KittyKeyboardFlags::NONE,
            None,
        )
    };
    assert_eq!(e(Modifiers::NONE), " ");
    assert_eq!(e(Modifiers::CTRL), "\0");
    assert_eq!(
        e(Modifiers::CTRL | Modifiers::NUM_LOCK | Modifiers::CAPS_LOCK),
        "\0"
    );
    assert_eq!(e(Modifiers::ALT), "\x1b ");
    assert_eq!(e(Modifiers::SHIFT), " ");
    assert_eq!(e(Modifiers::CTRL | Modifiers::ALT), "\x1b\0");
    assert_eq!(e(Modifiers::CTRL | Modifiers::SHIFT), "\0");
    assert_eq!(e(Modifiers::ALT | Modifiers::SHIFT), "\x1b ");
    assert_eq!(
        enc_full(
            b'i' as u32,
            0,
            0,
            Modifiers::CTRL | Modifiers::SHIFT,
            KeyAction::Press,
            KittyKeyboardFlags::NONE,
            None
        ),
        csi(
            GLFW_MOD_CONTROL | GLFW_MOD_SHIFT,
            b'i' as u32,
            1,
            0,
            0,
            None,
            'u'
        )
    );
    assert_eq!(enc(GLFW_FKEY_LEFT_SHIFT), "");
    assert_eq!(enc(GLFW_FKEY_CAPS_LOCK), "");
}

#[test]
fn release_repeat_and_text_fallback() {
    let base = b'a' as u32;
    assert_eq!(enc(base), "a");
    assert_eq!(
        enc_full(
            base,
            0,
            0,
            Modifiers::NONE,
            KeyAction::Press,
            KittyKeyboardFlags::NONE,
            Some("a")
        ),
        "a"
    );
    assert_eq!(
        enc_full(
            base,
            0,
            0,
            Modifiers::NONE,
            KeyAction::Repeat,
            KittyKeyboardFlags::NONE,
            None
        ),
        "a"
    );
    assert_eq!(
        enc_full(
            base,
            0,
            0,
            Modifiers::NONE,
            KeyAction::Release,
            KittyKeyboardFlags::NONE,
            None
        ),
        ""
    );
}

#[test]
fn disambiguate_escape_codes() {
    let d = KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES;
    let e = |key: u32, mods: Modifiers| enc_full(key, 0, 0, mods, KeyAction::Press, d, None);
    assert_eq!(e(b'a' as u32, Modifiers::NONE), "a");
    assert_eq!(
        e(GLFW_FKEY_ESCAPE, Modifiers::NONE),
        csi(0, 27, 1, 0, 0, None, 'u')
    );
    assert_eq!(e(GLFW_FKEY_ENTER, Modifiers::NONE), "\r");
    assert_eq!(
        e(GLFW_FKEY_ENTER, Modifiers::SHIFT),
        csi(GLFW_MOD_SHIFT, 13, 1, 0, 0, None, 'u')
    );
    assert_eq!(e(GLFW_FKEY_TAB, Modifiers::NONE), "\t");
    assert_eq!(e(GLFW_FKEY_BACKSPACE, Modifiers::NONE), "\x7f");
    assert_eq!(
        e(GLFW_FKEY_TAB, Modifiers::SHIFT),
        csi(GLFW_MOD_SHIFT, 9, 1, 0, 0, None, 'u')
    );
    for mods in [
        Modifiers::CTRL,
        Modifiers::ALT,
        Modifiers::CTRL | Modifiers::SHIFT,
        Modifiers::ALT | Modifiers::SHIFT,
    ] {
        assert_eq!(
            e(b'a' as u32, mods),
            csi(mods.bits(), b'a' as u32, 1, 0, 0, None, 'u')
        );
    }
    assert_eq!(
        e(b' ' as u32, Modifiers::CTRL),
        csi(GLFW_MOD_CONTROL, b' ' as u32, 1, 0, 0, None, 'u')
    );
    for key in [GLFW_FKEY_KP_PAGE_UP, GLFW_FKEY_KP_0] {
        assert_eq!(e(key, Modifiers::NONE), csi(0, key, 1, 0, 0, None, 'u'));
        assert_eq!(
            e(key, Modifiers::CTRL),
            csi(GLFW_MOD_CONTROL, key, 1, 0, 0, None, 'u')
        );
    }
    assert_eq!(e(GLFW_FKEY_UP, Modifiers::NONE), "\x1b[A");
    assert_eq!(
        e(GLFW_FKEY_UP, Modifiers::CTRL),
        csi(GLFW_MOD_CONTROL, 1, 1, 0, 0, None, 'A')
    );
}

#[test]
fn report_event_types() {
    let t = KittyKeyboardFlags::REPORT_EVENT_TYPES;
    let e =
        |key: u32, mods: Modifiers, action: KeyAction| enc_full(key, 0, 0, mods, action, t, None);
    assert_eq!(e(b'a' as u32, Modifiers::NONE, KeyAction::Press), "a");
    assert_eq!(
        e(b'a' as u32, Modifiers::NONE, KeyAction::Repeat),
        csi(0, b'a' as u32, 2, 0, 0, None, 'u')
    );
    assert_eq!(
        e(b'a' as u32, Modifiers::NONE, KeyAction::Release),
        csi(0, b'a' as u32, 3, 0, 0, None, 'u')
    );
    assert_eq!(
        e(b'a' as u32, Modifiers::SHIFT, KeyAction::Release),
        csi(GLFW_MOD_SHIFT, b'a' as u32, 3, 0, 0, None, 'u')
    );
    let t = KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES | KittyKeyboardFlags::REPORT_EVENT_TYPES;
    assert_eq!(
        enc_full(
            GLFW_FKEY_BACKSPACE,
            0,
            0,
            Modifiers::NONE,
            KeyAction::Press,
            t,
            None
        ),
        "\x7f"
    );
    assert_eq!(
        enc_full(
            GLFW_FKEY_BACKSPACE,
            0,
            0,
            Modifiers::NONE,
            KeyAction::Release,
            t,
            None
        ),
        ""
    );
    let locks = Modifiers::NUM_LOCK | Modifiers::CAPS_LOCK;
    assert_eq!(
        enc_full(GLFW_FKEY_ENTER, 0, 0, locks, KeyAction::Press, t, None),
        "\r"
    );
    assert_eq!(
        enc_full(GLFW_FKEY_ENTER, 0, 0, locks, KeyAction::Release, t, None),
        ""
    );
}

#[test]
fn report_alternate_keys() {
    let a = KittyKeyboardFlags::REPORT_ALTERNATE_KEYS;
    let e = |key: u32, shifted: u32, alternate: u32, mods: Modifiers| {
        enc_full(key, shifted, alternate, mods, KeyAction::Press, a, None)
    };
    assert_eq!(e(b'a' as u32, 0, 0, Modifiers::NONE), "a");
    assert_eq!(e(b'a' as u32, b'A' as u32, 0, Modifiers::NONE), "a");
    assert_eq!(
        e(b'a' as u32, b'A' as u32, 0, Modifiers::SHIFT),
        csi(GLFW_MOD_SHIFT, b'a' as u32, 1, b'A' as u32, 0, None, 'u')
    );
    assert_eq!(
        e(b'a' as u32, 0, b'A' as u32, Modifiers::NONE),
        csi(0, b'a' as u32, 1, 0, b'A' as u32, None, 'u')
    );
    assert_eq!(
        e(b'a' as u32, b'A' as u32, b'b' as u32, Modifiers::SHIFT),
        csi(
            GLFW_MOD_SHIFT,
            b'a' as u32,
            1,
            b'A' as u32,
            b'b' as u32,
            None,
            'u'
        )
    );
}

#[test]
fn report_all_keys_as_escape_codes() {
    let k = KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
    let e =
        |key: u32, mods: Modifiers, action: KeyAction| enc_full(key, 0, 0, mods, action, k, None);
    assert_eq!(
        e(b'a' as u32, Modifiers::NONE, KeyAction::Press),
        csi(0, b'a' as u32, 1, 0, 0, None, 'u')
    );
    assert_eq!(
        e(b'a' as u32, Modifiers::NONE, KeyAction::Repeat),
        csi(0, b'a' as u32, 1, 0, 0, None, 'u')
    );
    assert_eq!(
        e(b'a' as u32, Modifiers::CTRL, KeyAction::Press),
        csi(GLFW_MOD_CONTROL, b'a' as u32, 1, 0, 0, None, 'u')
    );
    assert_eq!(e(GLFW_FKEY_UP, Modifiers::NONE, KeyAction::Press), "\x1b[A");
    assert_eq!(
        e(GLFW_FKEY_LEFT_SHIFT, Modifiers::NONE, KeyAction::Press),
        csi(0, GLFW_FKEY_LEFT_SHIFT, 1, 0, 0, None, 'u')
    );
    assert_eq!(
        e(GLFW_FKEY_ENTER, Modifiers::NONE, KeyAction::Press),
        "\x1b[13u"
    );
    assert_eq!(
        e(GLFW_FKEY_ENTER, Modifiers::CTRL, KeyAction::Press),
        "\x1b[13;5u"
    );
    assert_eq!(
        e(GLFW_FKEY_TAB, Modifiers::NONE, KeyAction::Press),
        "\x1b[9u"
    );
    assert_eq!(
        e(GLFW_FKEY_BACKSPACE, Modifiers::NONE, KeyAction::Press),
        "\x1b[127u"
    );
}

#[test]
fn report_associated_text() {
    let flags = KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
        | KittyKeyboardFlags::REPORT_ASSOCIATED_TEXT;
    let e = |mods: Modifiers, text: &str| {
        enc_full(b'a' as u32, 0, 0, mods, KeyAction::Press, flags, Some(text))
    };
    assert_eq!(
        e(Modifiers::NONE, "a"),
        csi(0, b'a' as u32, 1, 0, 0, Some("a"), 'u')
    );
    assert_eq!(
        e(Modifiers::SHIFT, "A"),
        csi(GLFW_MOD_SHIFT, b'a' as u32, 1, 0, 0, Some("A"), 'u')
    );
    assert_eq!(
        e(Modifiers::SHIFT, "AB"),
        csi(GLFW_MOD_SHIFT, b'a' as u32, 1, 0, 0, Some("AB"), 'u')
    );
}

#[test]
fn all_kitty_flag_combinations() {
    for bits in 0u32..32 {
        let flags = KittyKeyboardFlags::from_bits_truncate(bits);
        let mods = Modifiers::SHIFT | Modifiers::CTRL;
        let got = enc_full(
            b'a' as u32,
            b'A' as u32,
            b'b' as u32,
            mods,
            KeyAction::Press,
            flags,
            Some("A"),
        );
        let alt = bits & 4 != 0;
        let text = bits & 16 != 0;
        let report_all = bits & 8 != 0;
        let expected = if !report_all {
            "A".to_string()
        } else {
            let key_field = if alt { "97:65:98" } else { "97" };
            let mut expected = format!("\x1b[{key_field};6");
            if text {
                expected.push_str(";65");
            }
            expected.push('u');
            expected
        };
        assert_eq!(got, expected, "flags={bits:#07b}");
    }
}

#[test]
fn application_cursor_keys() {
    let ctx = ScreenInputContext::new();
    let mut dispatcher = KeyDispatcher::new(|_: &KeyEvent| false);
    let up = KeyEvent::press(GLFW_FKEY_UP);
    assert_eq!(
        dispatcher.dispatch(&up, &ctx),
        KeyOutput::Bytes(b"\x1b[A".to_vec())
    );
    let mut ctx2 = ScreenInputContext::new();
    ctx2.modes.decckm = true;
    assert_eq!(
        dispatcher.dispatch(&up, &ctx2),
        KeyOutput::Bytes(b"\x1bOA".to_vec())
    );
    for (key, seq) in [
        (GLFW_FKEY_DOWN, "\x1bOB"),
        (GLFW_FKEY_RIGHT, "\x1bOC"),
        (GLFW_FKEY_LEFT, "\x1bOD"),
        (GLFW_FKEY_KP_BEGIN, "\x1bOE"),
        (GLFW_FKEY_END, "\x1bOF"),
        (GLFW_FKEY_HOME, "\x1bOH"),
    ] {
        assert_eq!(
            dispatcher.dispatch(&KeyEvent::press(key), &ctx2),
            KeyOutput::Bytes(seq.as_bytes().to_vec())
        );
    }
    let shifted = KeyEvent::press(GLFW_FKEY_UP).with_modifiers(Modifiers::SHIFT);
    assert_eq!(
        dispatcher.dispatch(&shifted, &ctx2),
        KeyOutput::Bytes(b"\x1b[1;2A".to_vec())
    );
    ctx2.key_encoding_stack.set(1, 1);
    assert_eq!(
        dispatcher.dispatch(&up, &ctx2),
        KeyOutput::Bytes(b"\x1b[A".to_vec())
    );
}

#[test]
fn decarm_and_shortcut_release() {
    let mut ctx = ScreenInputContext::new();
    ctx.modes.decarm = false;
    let mut seen = Vec::new();
    let mut dispatcher = KeyDispatcher::new(|ev: &KeyEvent| {
        seen.push(ev.key);
        ev.key == b'a' as u32 && ev.modifiers.ctrl()
    });
    let repeat = KeyEvent {
        key: b'a' as u32,
        modifiers: Modifiers::CTRL,
        action: KeyAction::Repeat,
        ..KeyEvent::default()
    };
    assert_eq!(dispatcher.dispatch(&repeat, &ctx), KeyOutput::None);
    ctx.modes.decarm = true;
    let press = KeyEvent {
        action: KeyAction::Press,
        ..repeat.clone()
    };
    assert_eq!(dispatcher.dispatch(&press, &ctx), KeyOutput::Shortcut);
    let release = KeyEvent {
        action: KeyAction::Release,
        ..repeat
    };
    assert_eq!(dispatcher.dispatch(&release, &ctx), KeyOutput::Shortcut);
    assert_eq!(seen, vec![b'a' as u32]);
}

#[test]
fn application_keypad_and_fake_scroll() {
    let mut ctx = ScreenInputContext::new();
    ctx.modes.deckpam = true;
    let mut dispatcher = KeyDispatcher::new(|_: &KeyEvent| false);
    assert_eq!(
        dispatcher.dispatch(&KeyEvent::press(GLFW_FKEY_KP_0), &ctx),
        KeyOutput::Bytes(b"\x1bOp".to_vec())
    );
    assert_eq!(
        dispatcher.dispatch(&KeyEvent::press(GLFW_FKEY_KP_ENTER), &ctx),
        KeyOutput::Bytes(b"\x1bOM".to_vec())
    );
    let scroll = dispatcher.fake_scroll(2, true, &ctx);
    assert_eq!(scroll, vec![b"\x1b[A".to_vec(), b"\x1b[A".to_vec()]);
    ctx.modes.decckm = false;
    assert_eq!(
        dispatcher.fake_scroll(1, false, &ctx),
        vec![b"\x1b[B".to_vec()]
    );
}

#[test]
fn key_encoding_flag_stack() {
    let mut ctx = ScreenInputContext::new();
    assert!(ctx.key_encoding_flags().is_empty());
    ctx.key_encoding_stack.push(3);
    assert_eq!(ctx.key_encoding_flags().bits(), 3);
    ctx.key_encoding_stack.set(4, 2);
    assert_eq!(ctx.key_encoding_flags().bits(), 7);
    ctx.key_encoding_stack.push(8);
    assert_eq!(ctx.key_encoding_flags().bits(), 8);
    ctx.key_encoding_stack.pop(1);
    assert_eq!(ctx.key_encoding_flags().bits(), 7);
    ctx.key_encoding_stack.pop(1);
    assert_eq!(ctx.key_encoding_flags().bits(), 0);
}

fn event_for(spec: &str, kitty_mod: Modifiers) -> KeyEvent {
    let shortcut = parse_shortcut(spec).unwrap();
    KeyEvent {
        key: shortcut.key,
        modifiers: shortcut.mods.resolve_kitty_mod(kitty_mod),
        action: KeyAction::Press,
        ..KeyEvent::default()
    }
}

#[test]
fn keymap_single_and_multi_key() {
    let kitty_mod = Modifiers::CTRL | Modifiers::SHIFT;
    let mut map = KeyMap::new(kitty_mod);
    map.add_line("ctrl+a new_window_with_cwd").unwrap();
    assert_eq!(
        map.dispatch(&event_for("ctrl+a", kitty_mod)),
        MappingResult::Action("new_window_with_cwd".into())
    );

    let mut map = KeyMap::new(kitty_mod);
    map.add_line("ctrl+f>2 set_font_size 20").unwrap();
    assert_eq!(
        map.dispatch(&event_for("ctrl+f", kitty_mod)),
        MappingResult::Consumed
    );
    assert_eq!(
        map.dispatch(&event_for("2", kitty_mod)),
        MappingResult::Action("set_font_size 20".into())
    );

    let mut map = KeyMap::new(kitty_mod);
    map.add_line("ctrl+f>2 set_font_size 20").unwrap();
    assert_eq!(
        map.dispatch(&event_for("ctrl+f", kitty_mod)),
        MappingResult::Consumed
    );
    match map.dispatch(&event_for("1", kitty_mod)) {
        MappingResult::NotConsumed(flush) => assert_eq!(flush.len(), 1),
        other => panic!("unexpected {other:?}"),
    }

    let mut map = KeyMap::new(kitty_mod);
    map.add_line("kitty_mod+p>f").unwrap();
    assert_eq!(
        map.dispatch(&event_for("ctrl+shift+p", kitty_mod)),
        MappingResult::Consumed
    );
    match map.dispatch(&event_for("f", kitty_mod)) {
        MappingResult::NotConsumed(flush) => assert_eq!(flush.len(), 1),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn keymap_override_and_unmap() {
    let kitty_mod = Modifiers::CTRL | Modifiers::SHIFT;
    let mut map = KeyMap::new(kitty_mod);
    map.add_line("kitty_mod+enter").unwrap();
    assert!(matches!(
        map.dispatch(&event_for("ctrl+shift+enter", kitty_mod)),
        MappingResult::NotConsumed(_)
    ));
    map.add_line("kitty_mod+p new_window").unwrap();
    assert_eq!(
        map.dispatch(&event_for("ctrl+shift+p", kitty_mod)),
        MappingResult::Action("new_window".into())
    );
    map.add_line("kitty_mod+s>p new_window").unwrap();
    assert_eq!(
        map.dispatch(&event_for("ctrl+shift+s", kitty_mod)),
        MappingResult::Consumed
    );
    assert_eq!(
        map.dispatch(&event_for("p", kitty_mod)),
        MappingResult::Action("new_window".into())
    );
    map.add_line("alt+p>1 multi1").unwrap();
    map.add_line("alt+p single1").unwrap();
    map.add_line("alt+p>2 multi2").unwrap();
    assert_eq!(
        map.dispatch(&event_for("alt+p", kitty_mod)),
        MappingResult::Consumed
    );
    assert_eq!(
        map.dispatch(&event_for("2", kitty_mod)),
        MappingResult::Action("multi2".into())
    );
}

#[test]
fn keymap_long_sequence_not_premature() {
    let kitty_mod = Modifiers::CTRL | Modifiers::SHIFT;
    let mut map = KeyMap::new(kitty_mod);
    map.add_line("alt+1>2>3 new_window").unwrap();
    assert_eq!(
        map.dispatch(&event_for("alt+1", kitty_mod)),
        MappingResult::Consumed
    );
    assert_eq!(
        map.dispatch(&event_for("2", kitty_mod)),
        MappingResult::Consumed
    );
    assert_eq!(
        map.dispatch(&event_for("3", kitty_mod)),
        MappingResult::Action("new_window".into())
    );
}
