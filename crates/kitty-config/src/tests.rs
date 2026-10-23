// SPDX-License-Identifier: GPL-3.0-or-later
//! Tests ported from `kitty_tests/datatypes.py` (shlex/color) and
//! `kitty_tests/options.py`, plus include/override edge cases.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::color::Color;
use crate::config::{BadLine, Config};
use crate::options::{
    clamp_font_size, CursorShape, EasingFunction, FontSpec, JumpType, Modifiers, TabBarEdge,
    TabBarStyle, TabPowerlineStyle, DELETE_ENV_VAR, MAXIMUM_FONT_SIZE, MINIMUM_FONT_SIZE,
};
use crate::shlex::Shlex;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "kitty-config-test-{}-{n}-{tag}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, contents).unwrap();
        path
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn cfg(overrides: &[&str]) -> Config {
    let paths: &[&str] = &[];
    Config::load_lenient(paths, overrides)
}

fn cfg_err(overrides: &[&str]) -> Vec<BadLine> {
    let paths: &[&str] = &[];
    match Config::load(paths, overrides) {
        Ok(_) => panic!("expected configuration errors for {overrides:?}"),
        Err(err) => err.bad_lines,
    }
}

#[test]
fn shlex_reports_kitty_errors() {
    for bad in [
        "abc\\", "\\", "'abc", "'", "\"", "asd\\", "\"a\\\"", "\"a\\",
    ] {
        assert!(Shlex::split(bad, false).is_err(), "failed for {bad:?}");
        assert!(Shlex::split_with_positions(bad, false).is_err());
    }
    assert_eq!(
        Shlex::split("\\", false).unwrap_err().to_string(),
        "Trailing backslash at end of input data"
    );
    assert_eq!(
        Shlex::split("abc\\", false).unwrap_err().to_string(),
        "Trailing backslash at end of input data"
    );
    assert_eq!(
        Shlex::split("'abc", false).unwrap_err().to_string(),
        "Unterminated string at the end of input"
    );
    assert_eq!(
        Shlex::split("\"a\\", false).unwrap_err().to_string(),
        "Trailing backslash at end of input data"
    );
}

#[test]
fn shlex_basic_splits_match_kitty() {
    let cases: &[(&str, &[&str])] = &[
        ("a\"\"", &["a"]),
        ("a\"\"b", &["ab"]),
        ("-1 \"\" 2", &["-1", "", "2"]),
        ("-1 '' 2", &["-1", "", "2"]),
        ("a \"\"", &["a", ""]),
        ("\"\"", &[""]),
        ("\"ab\"", &["ab"]),
        ("x \"ab\"y \\m", &["x", "aby", "m"]),
        ("x'y\"\\z'1", &["xy\"\\z1"]),
        ("\\abc\\ d", &["abc d"]),
        ("", &[""]),
        ("   ", &[""]),
        (" \tabc\n\t\r ", &["abc"]),
        ("$'ab'", &["$ab"]),
        ("😀", &["😀"]),
        ("\"a😀\"", &["a😀"]),
        ("😀 a", &["😀", "a"]),
        (" \t😀a", &["😀a"]),
    ];
    for (input, expected) in cases {
        assert_eq!(
            &Shlex::split(input, false).unwrap(),
            expected,
            "failed for {input:?}"
        );
    }
}

#[test]
fn shlex_positions_match_kitty() {
    let cases: &[(&str, &[(usize, &str)])] = &[
        ("a\"\"", &[(0, "a")]),
        ("a\"\"b", &[(0, "ab")]),
        ("-1 \"\" 2", &[(0, "-1"), (3, ""), (6, "2")]),
        ("-1 '' 2", &[(0, "-1"), (3, ""), (6, "2")]),
        ("a \"\"", &[(0, "a"), (2, "")]),
        ("\"\"", &[(0, "")]),
        ("\"ab\"", &[(0, "ab")]),
        ("x \"ab\"y \\m", &[(0, "x"), (2, "aby"), (8, "m")]),
        ("x'y\"\\z'1", &[(0, "xy\"\\z1")]),
        ("\\abc\\ d", &[(0, "abc d")]),
        ("", &[(0, "")]),
        ("   ", &[(0, "")]),
        (" \tabc\n\t\r ", &[(2, "abc")]),
        ("$'ab'", &[(0, "$ab")]),
        ("😀", &[(0, "😀")]),
        ("\"a😀\"", &[(0, "a😀")]),
        ("😀 a", &[(0, "😀"), (2, "a")]),
        (" \t😀a", &[(2, "😀a")]),
    ];
    for (input, expected) in cases {
        let expected: Vec<(usize, String)> = expected
            .iter()
            .map(|(n, s)| (*n, (*s).to_string()))
            .collect();
        assert_eq!(
            Shlex::split_with_positions(input, false).unwrap(),
            expected,
            "failed for {input:?}"
        );
    }
}

#[test]
fn shlex_ansi_c_quoting_matches_kitty() {
    let cases: &[(&str, &str)] = &[
        ("$'ab'", "ab"),
        ("1$'ab'", "1ab"),
        ("\"1$'ab'\"", "1$'ab'"),
        (r"$'a\123b'", "a\u{53}b"),
        (r"$'a\1b'", "a\u{1}b"),
        (r"$'a\12b'", "a\u{a}b"),
        (r"$'a\db'", "adb"),
        (r"$'a\x1bb'", "a\u{1b}b"),
        (r"$'\u123z'", "\u{123}z"),
        (r"$'\U0001F1E8'", "\u{1F1E8}"),
        (r"$'\U1F1E8'", "\u{1F1E8}"),
        (r"$'a\U1F1E8'b", "a\u{1F1E8}b"),
        ("$'a''b'", "ab"),
        ("x$''''''''AAAA", "xAAAA"),
        ("$''''''''AAAA", "AAAA"),
        ("$\"\"'x'", "$x"),
        (r"\$'x'", "$x"),
        (r"$\''x'", "$'x"),
    ];
    for (input, expected) in cases {
        assert_eq!(
            Shlex::split(input, true).unwrap(),
            vec![(*expected).to_string()],
            "failed for {input:?}"
        );
        assert_eq!(Shlex::split_with_positions(input, true).unwrap()[0].0, 0);
    }
}

#[test]
fn shlex_ansi_c_quoting_errors() {
    for bad in ["$'''", "a$'''b"] {
        assert!(Shlex::split(bad, true).is_err(), "failed for {bad:?}");
    }
}

#[test]
fn shlex_unicode_positions_match_kitty() {
    assert_eq!(
        Shlex::split_with_positions("😀 a", false).unwrap(),
        vec![(0, "😀".into()), (2, "a".into())]
    );
    assert_eq!(
        Shlex::split_with_positions(" \t😀a", false).unwrap(),
        vec![(2, "😀a".into())]
    );
}

#[test]
fn shlex_empty_input_and_hash_behavior() {
    assert_eq!(Shlex::split("", false).unwrap(), vec!["".to_string()]);
    assert_eq!(Shlex::split("   ", false).unwrap(), vec!["".to_string()]);
    assert_eq!(
        Shlex::split_with_positions("   ", false).unwrap(),
        vec![(0, "".to_string())]
    );
    assert_eq!(
        Shlex::split("a # comment", false).unwrap(),
        vec!["a", "#", "comment"]
    );
    let mut lexer = Shlex::new("", false);
    assert!(lexer.next_word().is_some());
    assert!(lexer.next_word().is_none());
}

#[test]
fn color_named_colors_and_comments() {
    assert_eq!(Color::parse("rEd"), Some(Color::new(255, 0, 0)));
    assert_eq!(
        Color::parse("aLice blUe # comment"),
        Some(Color::new(240, 248, 255))
    );
    assert_eq!(
        Color::parse("#eee # comment"),
        Some(Color::new(0xee, 0xee, 0xee))
    );
    assert_eq!(Color::parse("red # comment"), Some(Color::new(255, 0, 0)));
}

#[test]
fn color_hex_forms_match_kitty() {
    assert_eq!(Color::parse("#234567"), Some(Color::new(0x23, 0x45, 0x67)));
    assert_eq!(
        Color::parse("#abCabcdef"),
        Some(Color::new(0xab, 0xab, 0xde))
    );
    assert_eq!(Color::parse("#ff0000 # Red"), Some(Color::new(0xff, 0, 0)));
}

#[test]
fn color_rgb_and_rgbi_forms_match_kitty() {
    assert_eq!(
        Color::parse("rgB:23/45/67"),
        Some(Color::new(0x23, 0x45, 0x67))
    );
    assert_eq!(
        Color::parse("rgb:abc/abc/def"),
        Some(Color::new(0xab, 0xab, 0xde))
    );
    assert_eq!(
        Color::parse("rgb:e/e/e # comment"),
        Some(Color::new(0xee, 0xee, 0xee))
    );
    assert_eq!(Color::parse("rgbi:1/0/0"), Some(Color::new(255, 0, 0)));
    assert_eq!(
        Color::parse("rgbI:0.5/0.5/0.5"),
        Some(Color::new(128, 128, 128))
    );
    assert_eq!(Color::parse("rgbi:2/-1/0"), Some(Color::new(255, 0, 0)));
}

#[test]
fn color_invalid_forms() {
    for bad in [
        "xxx",
        "#12",
        "#1234",
        "rgb:a/b",
        "rgb:",
        "rgbi:1/0",
        "rgbi:x/0/0",
        "rgbi:nan/0/0",
        "rgbii:1/0/0",
        "",
        "   ",
        "#",
        "oklch(0.5 0.1 180)",
        "lab(50 0 0)",
    ] {
        assert_eq!(Color::parse(bad), None, "expected invalid color {bad:?}");
    }
}

#[test]
fn color_helper_conversions() {
    let color = Color::new(1, 2, 3);
    assert_eq!(color.as_rgb_int(), 0x010203);
    assert_eq!(color.as_sharp(), "#010203");
    assert_eq!(color.as_sgr(), ":2:1:2:3");
    assert_eq!(Color::rgba(1, 2, 3, 4).as_sharp(), "#04010203");
}

#[test]
fn font_size_and_line_continuations_match_kitty() {
    let lines: &[&str] = &[
        "    font",
        " \t  \t    \\_size",
        "    \\ 12",
        "\\.35",
        "col",
        "\\o",
        "\t \t\\r",
        "\\25",
        " \\ blue",
    ];
    let config = cfg(lines);
    assert!(config.bad_lines.is_empty(), "{:?}", config.bad_lines);
    assert_eq!(config.options.font_size, 12.35);
    assert_eq!(config.options.colors.get(&25), Some(&Color::new(0, 0, 255)));
}

#[test]
fn font_size_clamping_matches_kitty() {
    assert_eq!(clamp_font_size(0.0, 11.0), MINIMUM_FONT_SIZE);
    assert_eq!(clamp_font_size(-100.0, 11.0), MINIMUM_FONT_SIZE);
    assert_eq!(clamp_font_size(11.0, 11.0), 11.0);
    assert_eq!(clamp_font_size(64.0, 11.0), 64.0);
    assert_eq!(clamp_font_size(1e6, 4.0), MAXIMUM_FONT_SIZE);
    assert_eq!(clamp_font_size(1e6, 11.0), MAXIMUM_FONT_SIZE);
    assert_eq!(clamp_font_size(200.0, 4.0), 200.0);
    assert_eq!(clamp_font_size(1e6, 40.0), 400.0);
    assert_eq!(clamp_font_size(1e6, 100.0), 1000.0);
    for configured in [1.0, 4.0, 11.0, 25.6, 40.0, 100.0] {
        assert!(clamp_font_size(1e6, configured) >= MAXIMUM_FONT_SIZE);
    }
    assert_eq!(cfg(&["font_size 11.37"]).options.font_size, 11.37);
    assert_eq!(cfg(&["font_size 0"]).options.font_size, MINIMUM_FONT_SIZE);
}

#[test]
fn typed_colors_and_none_values() {
    let options = cfg(&[
        "foreground red",
        "background #001122",
        "cursor none",
        "cursor_text_color background",
        "selection_foreground #ffffff",
        "selection_background rgb:01/02/03",
    ])
    .options;
    assert_eq!(options.foreground, Color::new(255, 0, 0));
    assert_eq!(options.background, Color::new(0, 0x11, 0x22));
    assert_eq!(options.cursor, None);
    assert_eq!(options.cursor_text_color, None);
    assert_eq!(
        options.selection_foreground,
        Some(Color::new(255, 255, 255))
    );
    assert_eq!(options.selection_background, Some(Color::new(1, 2, 3)));
}

#[test]
fn cursor_shape_values() {
    assert_eq!(
        cfg(&["cursor_shape beam"]).options.cursor_shape,
        CursorShape::Beam
    );
    assert_eq!(
        cfg(&["cursor_shape UNDERLINE"]).options.cursor_shape,
        CursorShape::Underline
    );
    assert!(cfg(&["cursor_shape hollow"]).bad_lines.len() == 1);
    assert_eq!(
        cfg(&["cursor_shape_unfocused hollow"])
            .options
            .cursor_shape_unfocused,
        CursorShape::Hollow
    );
}

#[test]
fn cursor_blink_interval_simple_values() {
    for (src, interval) in [("3", 3.0), ("-2.3", -2.3)] {
        let blink = cfg(&[&format!("cursor_blink_interval {src}")])
            .options
            .cursor_blink_interval;
        assert_eq!(blink.interval, interval);
        assert_eq!(blink.first, EasingFunction::None);
        assert_eq!(blink.second, EasingFunction::None);
    }
}

#[test]
fn cursor_blink_interval_named_easings() {
    let linear = EasingFunction::CubicBezier {
        cubic_bezier_points: [0.0, 0.0, 1.0, 1.0],
    };
    let blink = cfg(&["cursor_blink_interval linear"])
        .options
        .cursor_blink_interval;
    assert_eq!((blink.interval, blink.first), (-1.0, linear.clone()));
    let blink = cfg(&["cursor_blink_interval linear 19"])
        .options
        .cursor_blink_interval;
    assert_eq!((blink.interval, blink.first), (19.0, linear));
    let blink = cfg(&["cursor_blink_interval ease-in-out cubic-bezier(0.1, 0.2, 0.3, 0.4) 11"])
        .options
        .cursor_blink_interval;
    assert_eq!(blink.interval, 11.0);
    assert_eq!(
        blink.first,
        EasingFunction::CubicBezier {
            cubic_bezier_points: [0.42, 0.0, 0.58, 1.0]
        }
    );
    assert_eq!(
        blink.second,
        EasingFunction::CubicBezier {
            cubic_bezier_points: [0.1, 0.2, 0.3, 0.4]
        }
    );
}

#[test]
fn cursor_blink_interval_steps_and_linear_curves() {
    let steps = |n, jump_type| EasingFunction::Steps {
        num_steps: n,
        jump_type,
    };
    let cases = [
        ("step-start", steps(1, JumpType::Start)),
        ("steps(7, jump-none)", steps(7, JumpType::None)),
        (
            "linear(0, 0.25, 1)",
            EasingFunction::Linear {
                linear_x: vec![0.0, 0.5, 1.0],
                linear_y: vec![0.0, 0.25, 1.0],
            },
        ),
        (
            "linear(0, 0.25 75%, 1)",
            EasingFunction::Linear {
                linear_x: vec![0.0, 0.75, 1.0],
                linear_y: vec![0.0, 0.25, 1.0],
            },
        ),
        (
            "linear(0, 0.25 25% 75%, 1)",
            EasingFunction::Linear {
                linear_x: vec![0.0, 0.25, 0.75, 1.0],
                linear_y: vec![0.0, 0.25, 0.25, 1.0],
            },
        ),
    ];
    for (src, expected) in cases {
        let blink = cfg(&[&format!("cursor_blink_interval {src}")])
            .options
            .cursor_blink_interval;
        assert_eq!(blink.first, expected, "failed for {src}");
    }
}

#[test]
fn env_option_expansion_matches_kitty() {
    let options = cfg(&[
        "env A=1",
        "env B=x$A",
        "env C=",
        "env D",
        "clear_all_shortcuts y",
        "kitten_alias a b --moo",
        "map f1 kitten a arg",
    ])
    .options;
    assert_eq!(options.env.get("A").map(String::as_str), Some("1"));
    assert_eq!(options.env.get("B").map(String::as_str), Some("x1"));
    assert_eq!(options.env.get("C").map(String::as_str), Some(""));
    assert_eq!(
        options.env.get("D").map(String::as_str),
        Some(DELETE_ENV_VAR)
    );
    assert_eq!(options.map, vec!["f1 kitten a arg"]);
    assert_eq!(options.kitten_alias, vec!["a b --moo"]);
}

#[test]
fn kitty_mod_option() {
    let options = cfg(&["kitty_mod alt"]).options;
    assert_eq!(options.kitty_mod, Modifiers(Modifiers::ALT));
    assert_eq!(options.kitty_mod, Modifiers::parse("alt").unwrap());
    assert_eq!(
        cfg(&["kitty_mod ctrl+shift"]).options.kitty_mod,
        Modifiers(Modifiers::CONTROL | Modifiers::SHIFT)
    );
}

#[test]
fn scrollback_lines_and_padding_width() {
    assert_eq!(
        cfg(&["scrollback_lines -1"]).options.scrollback_lines,
        u32::MAX as u64
    );
    assert_eq!(
        cfg(&["scrollback_lines 12345"]).options.scrollback_lines,
        12345
    );
    let one = cfg(&["window_padding_width 7"])
        .options
        .window_padding_width;
    assert_eq!(
        (one.left, one.top, one.right, one.bottom),
        (7.0, 7.0, 7.0, 7.0)
    );
    let two = cfg(&["window_padding_width 1 2"])
        .options
        .window_padding_width;
    assert_eq!(
        (two.left, two.top, two.right, two.bottom),
        (2.0, 1.0, 2.0, 1.0)
    );
    let three = cfg(&["window_padding_width 1 2 3"])
        .options
        .window_padding_width;
    assert_eq!(
        (three.left, three.top, three.right, three.bottom),
        (2.0, 1.0, 2.0, 3.0)
    );
    let four = cfg(&["window_padding_width 1 2 3 4"])
        .options
        .window_padding_width;
    assert_eq!(
        (four.left, four.top, four.right, four.bottom),
        (4.0, 1.0, 2.0, 3.0)
    );
    let clamped = cfg(&["window_padding_width -5"])
        .options
        .window_padding_width;
    assert_eq!(clamped.top, 0.0);
    assert_eq!(
        cfg(&["background_opacity 2"]).options.background_opacity,
        1.0
    );
    assert_eq!(
        cfg(&["background_opacity -1"]).options.background_opacity,
        0.0
    );
}

#[test]
fn tab_bar_style_choices_and_errors() {
    let choices = [
        ("fade", TabBarStyle::Fade),
        ("hidden", TabBarStyle::Hidden),
        ("powerline", TabBarStyle::Powerline),
        ("separator", TabBarStyle::Separator),
        ("slant", TabBarStyle::Slant),
        ("custom", TabBarStyle::Custom),
    ];
    for (raw, expected) in choices {
        assert_eq!(
            cfg(&[&format!("tab_bar_style {raw}")])
                .options
                .tab_bar_style,
            expected
        );
    }
    let errors = cfg_err(&["tab_bar_style sideways"]);
    assert_eq!(errors.len(), 1);
    assert!(errors[0]
        .error
        .contains("not a valid choice for tab_bar_style"));
}

#[test]
fn tab_bar_visibility_and_geometry_follow_kitty() {
    let mut options = cfg(&[]).options;
    // Default: fade style, minimum of two tabs.
    assert!(options.tab_bar_should_be_visible(2));
    assert!(options.tab_bar_should_be_visible(3));
    assert!(!options.tab_bar_should_be_visible(1));
    assert!(!options.tab_bar_should_be_visible(0));
    assert_eq!(options.tab_bar_height(1, 19.0), 0.0);
    assert_eq!(options.tab_bar_height(2, 19.0), 19.0);

    options = cfg(&["tab_bar_style hidden"]).options;
    assert!(!options.tab_bar_should_be_visible(1));
    assert!(!options.tab_bar_should_be_visible(8));
    assert_eq!(options.tab_bar_height(8, 19.0), 0.0);

    options = cfg(&["tab_bar_min_tabs 1"]).options;
    assert!(options.tab_bar_should_be_visible(1));
    assert_eq!(options.tab_bar_height(1, 15.0), 15.0);

    options = cfg(&["tab_bar_min_tabs 0"]).options;
    assert!(options.tab_bar_should_be_visible(1));
}

#[test]
fn tab_bar_defaults_match_kitty() {
    let options = cfg(&[]).options;
    assert_eq!(options.tab_bar_style, TabBarStyle::Fade);
    assert_eq!(options.tab_bar_edge, TabBarEdge::Bottom);
    assert_eq!(options.tab_bar_min_tabs, 2);
    assert_eq!(options.tab_separator, " ┇");
    assert_eq!(options.tab_powerline_style, TabPowerlineStyle::Angled);
    assert_eq!(options.tab_fade, vec![0.25, 0.5, 0.75, 1.0]);
    assert_eq!(options.active_tab_foreground, Color::new(0, 0, 0));
    assert_eq!(options.active_tab_background, Color::new(0xee, 0xee, 0xee));
    assert_eq!(
        options.inactive_tab_foreground,
        Color::new(0x44, 0x44, 0x44)
    );
    assert_eq!(
        options.inactive_tab_background,
        Color::new(0x99, 0x99, 0x99)
    );
    assert!(options.active_tab_font_style.bold && options.active_tab_font_style.italic);
    assert!(!options.inactive_tab_font_style.bold && !options.inactive_tab_font_style.italic);
    assert_eq!(options.tab_bar_background, None);

    let options = cfg(&[
        "tab_bar_style powerline",
        "tab_powerline_style round",
        "tab_separator \" | \"",
        "active_tab_background #112233",
        "tab_bar_min_tabs 3",
    ])
    .options;
    assert_eq!(options.tab_powerline_style, TabPowerlineStyle::Round);
    assert_eq!(options.tab_separator, " | ");
    assert_eq!(options.active_tab_background, Color::new(0x11, 0x22, 0x33));
    assert_eq!(options.tab_bar_min_tabs, 3);
}

#[test]
fn tab_bar_edge_choices_and_errors() {
    let choices = [
        ("top", TabBarEdge::Top),
        ("bottom", TabBarEdge::Bottom),
        ("left", TabBarEdge::Left),
        ("right", TabBarEdge::Right),
    ];
    for (raw, expected) in choices {
        assert_eq!(
            cfg(&[&format!("tab_bar_edge {raw}")]).options.tab_bar_edge,
            expected
        );
    }
    let errors = cfg_err(&["tab_bar_edge middle"]);
    assert_eq!(errors.len(), 1);
    assert!(errors[0]
        .error
        .contains("not a valid choice for tab_bar_edge"));
}

#[test]
fn text_composition_strategy_matches_kitty() {
    let options = cfg(&[]).options;
    assert_eq!(options.text_composition_strategy, "platform");
    assert_eq!(options.text_gamma_adjustment, 1.0);
    assert_eq!(options.text_contrast, 0.0);
    assert!(!options.text_old_gamma);

    let options = cfg(&["text_composition_strategy legacy"]).options;
    assert!(options.text_old_gamma);

    let options = cfg(&["text_composition_strategy 1.3 30"]).options;
    assert_eq!(options.text_gamma_adjustment, 1.3);
    assert_eq!(options.text_contrast, 30.0);

    let options = cfg(&["text_composition_strategy 0"]).options;
    assert_eq!(options.text_gamma_adjustment, 0.01);

    let options = cfg(&["text_composition_strategy 1.3 150"]).options;
    assert_eq!(options.text_contrast, 100.0);

    assert_eq!(cfg_err(&["text_composition_strategy a"]).len(), 1);
    assert_eq!(cfg_err(&["text_composition_strategy 1 2 3"]).len(), 1);
}

#[test]
fn window_border_defaults_and_parsing() {
    let options = cfg(&[]).options;
    assert_eq!(options.window_border_width, 0.5);
    assert_eq!(options.window_border_radius, 0.0);
    assert!(options.draw_minimal_borders);
    assert!(!options.draw_window_borders_for_single_window);
    assert_eq!(options.active_border_color, Some(Color::new(0, 255, 0)));
    assert_eq!(options.inactive_border_color, Color::new(204, 204, 204));
    assert_eq!(options.bell_border_color, Color::new(255, 90, 0));
    assert_eq!(options.window_margin_width.left, 0.0);

    let options = cfg(&[
        "window_border_width 2px",
        "window_border_radius 4pt",
        "draw_minimal_borders no",
        "draw_window_borders_for_single_window yes",
        "active_border_color none",
        "window_margin_width 1 2 3 4",
    ])
    .options;
    assert_eq!(options.window_border_width, 2.0);
    assert_eq!(options.window_border_radius, 4.0);
    assert!(!options.draw_minimal_borders);
    assert!(options.draw_window_borders_for_single_window);
    assert_eq!(options.active_border_color, None);
    assert_eq!(options.window_margin_width.left, 4.0);
    assert_eq!(options.window_margin_width.top, 1.0);
    assert_eq!(options.window_margin_width.right, 2.0);
    assert_eq!(options.window_margin_width.bottom, 3.0);
}

#[test]
fn enabled_layouts_aliases_and_unknown() {
    assert_eq!(
        cfg(&["enabled_layouts tall,grid:foo,*"])
            .options
            .enabled_layouts,
        vec![
            "tall",
            "grid:foo",
            "fat",
            "grid",
            "horizontal",
            "splits",
            "stack",
            "vertical"
        ]
    );
    let errors = cfg_err(&["enabled_layouts nosuch"]);
    assert_eq!(errors.len(), 1);
    assert!(errors[0]
        .error
        .contains("The window layout nosuch is unknown"));
}

#[test]
fn unknown_key_is_an_error() {
    let errors = cfg_err(&["not_a_kitty_option yes"]);
    assert_eq!(errors.len(), 1);
    assert_eq!(
        errors[0].error,
        "Ignoring unknown config key: not_a_kitty_option"
    );
    let lenient = cfg(&["not_a_kitty_option yes"]);
    assert_eq!(lenient.bad_lines.len(), 1);
}

#[test]
fn invalid_value_is_an_error() {
    let errors = cfg_err(&["font_size nope"]);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].error.contains("Invalid number"));
    let errors = cfg_err(&["cursor_shape triangle"]);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].error.contains("Invalid cursor shape"));
}

#[test]
fn font_spec_parsing_matches_kitty() {
    let auto = FontSpec::from_setting("auto").unwrap();
    assert_eq!(auto.system.as_deref(), Some("auto"));
    assert!(auto.family.is_none());
    let mono = FontSpec::from_setting("monospace").unwrap();
    assert_eq!(mono.system.as_deref(), Some("monospace"));
    let spec = FontSpec::from_setting(
        "family='Fira Code' style=Bold postscript_name=FiraCode-Bold wght=500 features='liga off'",
    )
    .unwrap();
    assert_eq!(spec.family.as_deref(), Some("Fira Code"));
    assert_eq!(spec.style.as_deref(), Some("Bold"));
    assert_eq!(spec.axes, vec![("wght".to_string(), 500.0)]);
    assert_eq!(spec.features, vec!["liga", "off"]);
    assert!(FontSpec::from_setting("family=x bad").is_err());
}

#[test]
fn clear_all_shortcuts_and_mouse_actions_clear_raw_entries() {
    let options = cfg(&[
        "map f1 next_window",
        "map f2 new_window",
        "clear_all_shortcuts yes",
        "map f3 next_tab",
        "mouse_map left click ungrabbed mouse_click_url",
        "clear_all_mouse_actions yes",
        "mouse_map right click ungrabbed close_window",
    ])
    .options;
    assert_eq!(options.map, vec!["f3 next_tab"]);
    assert_eq!(
        options.mouse_map,
        vec!["right click ungrabbed close_window"]
    );
}

#[test]
fn include_basic_and_ordering() {
    let dir = TempDir::new("include-order");
    dir.write("extra.conf", "foreground blue\nbackground white\n");
    let main = dir.write(
        "kitty.conf",
        "foreground red\nfont_size 9\ninclude extra.conf\n",
    );
    let config = Config::load(&[main.as_path()], &[] as &[&str]).unwrap();
    assert_eq!(config.options.foreground, Color::new(0, 0, 255));
    assert_eq!(config.options.background, Color::new(255, 255, 255));
    assert_eq!(config.options.font_size, 9.0);
    assert_eq!(config.found_paths, vec![main]);
    assert!(config.warnings.is_empty());
    assert!(config.bad_lines.is_empty());
}

#[test]
fn globinclude_is_sorted_and_deterministic() {
    let dir = TempDir::new("globinclude");
    dir.write("glob/02.conf", "background white\n");
    dir.write("glob/01.conf", "foreground red\n");
    let main = dir.write("kitty.conf", "globinclude glob/*\n");
    let config = Config::load(&[main.as_path()], &[] as &[&str]).unwrap();
    assert_eq!(config.options.foreground, Color::new(255, 0, 0));
    assert_eq!(config.options.background, Color::new(255, 255, 255));
    assert!(config.bad_lines.is_empty());
}

#[test]
fn envinclude_pattern() {
    let name = format!(
        "KITTY_RUST_ENVINCLUDE_{}_{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    std::env::set_var(&name, "cursor yellow\nbackground_opacity 0.5\n");
    let result = cfg(&[&format!("envinclude {name}")]);
    std::env::remove_var(&name);
    assert!(result.bad_lines.is_empty(), "{:?}", result.bad_lines);
    assert_eq!(result.options.cursor, Some(Color::new(255, 255, 0)));
    assert_eq!(result.options.background_opacity, 0.5);
}

#[test]
fn include_cycle_detection() {
    let dir = TempDir::new("include-cycle");
    dir.write("a", "background red\ninclude b\n");
    dir.write("b", "foreground red\ninclude a\n");
    let a = dir.path().join("a");
    let config = Config::load_lenient(&[a.as_path()], &[] as &[&str]);
    assert_eq!(config.bad_lines.len(), 1);
    assert!(config.bad_lines[0]
        .error
        .contains("has already been included"));
    assert_eq!(config.options.foreground, Color::new(255, 0, 0));
    assert_eq!(config.options.background, Color::new(255, 0, 0));
}

#[test]
fn missing_include_is_a_warning() {
    let dir = TempDir::new("missing-include");
    let main = dir.write(
        "kitty.conf",
        &format!("include missing-{}.conf\n", std::process::id()),
    );
    let config = Config::load_lenient(&[main.as_path()], &[] as &[&str]);
    assert!(config.bad_lines.is_empty());
    assert_eq!(config.warnings.len(), 1);
    assert!(config.warnings[0].contains("Could not find included config file"));
}

#[test]
fn geninclude_is_rejected_without_execution() {
    let dir = TempDir::new("geninclude");
    let script = dir.write("g.py", "print('foreground red')\n");
    let main = dir.write("kitty.conf", "geninclude g.py\n");
    let errors = match Config::load(&[main.as_path()], &[] as &[&str]) {
        Ok(_) => panic!("geninclude must not silently succeed"),
        Err(err) => err.bad_lines,
    };
    assert_eq!(errors.len(), 1);
    assert!(errors[0].error.contains("geninclude is not supported"));
    assert_eq!(script.file_name().unwrap(), "g.py");
}

#[test]
fn overrides_normalize_equals_and_apply_in_order() {
    let config = cfg(&[
        "font_size=12",
        "foreground=#ff0000",
        "foreground=blue",
        "kitty_mod = alt",
    ]);
    assert!(config.bad_lines.is_empty(), "{:?}", config.bad_lines);
    assert_eq!(config.options.font_size, 12.0);
    assert_eq!(config.options.foreground, Color::new(0, 0, 255));
    assert_eq!(config.options.kitty_mod, Modifiers(Modifiers::ALT));
    assert_eq!(
        config.overrides,
        vec![
            "font_size 12",
            "foreground #ff0000",
            "foreground blue",
            "kitty_mod  alt"
        ]
    );
}

#[test]
fn multiple_paths_then_overrides_order() {
    let dir = TempDir::new("multi-path");
    let first = dir.write("first.conf", "foreground red\nfont_size 10\n");
    let second = dir.write("second.conf", "foreground blue\n");
    let config = Config::load(&[first.as_path(), second.as_path()], &["font_size=12"]).unwrap();
    assert_eq!(config.options.foreground, Color::new(0, 0, 255));
    assert_eq!(config.options.font_size, 12.0);
    assert_eq!(config.found_paths, vec![first.clone(), second.clone()]);
    assert_eq!(config.all_config_paths, vec![first, second]);
}

#[test]
fn include_paths_are_relative_to_the_including_file() {
    let dir = TempDir::new("relative-include");
    dir.write("sub/two.conf", "background white\n");
    dir.write("sub/one.conf", "include two.conf\nforeground red\n");
    let main = dir.write("kitty.conf", "include sub/one.conf\n");
    let config = Config::load(&[main.as_path()], &[] as &[&str]).unwrap();
    assert_eq!(config.options.background, Color::new(255, 255, 255));
    assert_eq!(config.options.foreground, Color::new(255, 0, 0));
    assert!(config.bad_lines.is_empty());
}
