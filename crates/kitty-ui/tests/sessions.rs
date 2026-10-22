// SPDX-License-Identifier: GPL-3.0-or-later
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kitty_ui::*;

fn options() -> SessionOptions {
    SessionOptions {
        enabled_layouts: vec!["tall".into()],
        shell: "/bin/sh".into(),
    }
}

fn parse(raw: &str) -> Session {
    parse_session(raw, &options(), None, "test", "/tmp/test.kitty-session").unwrap()
}

fn parse_err(raw: &str) -> SessionError {
    parse_session(raw, &options(), None, "test", "/tmp/test.kitty-session").unwrap_err()
}

#[test]
fn parse_basic_session() {
    let s = parse(
        "new_tab main\nlayout grid\nenabled_layouts tall,grid\ncd /tmp\nlaunch\nlaunch --title=two\nfocus\n",
    );
    assert_eq!(s.tabs.len(), 1);
    assert_eq!(s.tabs[0].name, "main");
    assert_eq!(s.tabs[0].layout, "grid");
    assert_eq!(s.tabs[0].enabled_layouts, vec!["tall", "grid"]);
    assert_eq!(s.tabs[0].windows.len(), 2);
    assert_eq!(s.tabs[0].windows[1].launch.title.as_deref(), Some("two"));
    assert_eq!(s.active_tab_idx, 0);
    assert_eq!(s.tabs[0].active_window_idx, 1);
    assert_eq!(s.tabs[0].cwd.as_deref(), Some(Path::new("/tmp")));
}

#[test]
fn comments_and_blank_lines_are_ignored() {
    let s = parse("\n# a comment\n   # indented comment\nlaunch\n");
    assert_eq!(s.tabs[0].windows.len(), 1);
}

#[test]
fn quoting_in_launch_commands() {
    let s = parse("launch --title='hello world' sh -c 'echo hi'\n");
    let launch = &s.tabs[0].windows[0].launch;
    assert_eq!(launch.title.as_deref(), Some("hello world"));
    assert_eq!(launch.args, vec!["sh", "-c", "echo hi"]);
}

#[test]
fn env_and_vars_are_parsed() {
    let s = parse("launch --env=FOO=bar --env='SPACED=a b' --var=x=1 sh\n");
    let launch = &s.tabs[0].windows[0].launch;
    assert_eq!(launch.env.get("FOO").map(String::as_str), Some("bar"));
    assert_eq!(launch.env.get("SPACED").map(String::as_str), Some("a b"));
    assert_eq!(launch.vars.get("x").map(String::as_str), Some("1"));
}

#[test]
fn env_expansion_uses_supplied_environment() {
    let mut env = BTreeMap::new();
    env.insert("PROJECT".into(), "/srv/project".into());
    env.insert("NAME".into(), "demo".into());
    let s = parse_session(
        "new_tab $NAME\ncd $PROJECT\nlaunch\n",
        &options(),
        Some(&env),
        "test",
        "/tmp/test.kitty-session",
    )
    .unwrap();
    assert_eq!(s.tabs[0].name, "demo");
    assert_eq!(s.tabs[0].cwd.as_deref(), Some(Path::new("/srv/project")));
}

#[test]
fn unknown_env_vars_are_left_untouched() {
    let mut env = BTreeMap::new();
    env.insert("KNOWN".into(), "yes".into());
    let s = parse_session(
        "new_tab $UNKNOWN\nlaunch\n",
        &options(),
        Some(&env),
        "test",
        "/tmp/test.kitty-session",
    )
    .unwrap();
    assert_eq!(s.tabs[0].name, "$UNKNOWN");
}

#[test]
fn relative_cd_resolves_against_session_directory() {
    let s = parse("cd sub/dir\nlaunch\n");
    assert_eq!(s.tabs[0].cwd.as_deref(), Some(Path::new("/tmp/sub/dir")));
}

#[test]
fn title_command_applies_to_next_window_only() {
    let s = parse("title first\nlaunch\nlaunch\n");
    assert_eq!(s.tabs[0].windows[0].launch.title.as_deref(), Some("first"));
    assert_eq!(s.tabs[0].windows[1].launch.title, None);
}

#[test]
fn launch_title_wins_over_title_command() {
    let s = parse("title from-title\nlaunch --title=explicit\n");
    assert_eq!(
        s.tabs[0].windows[0].launch.title.as_deref(),
        Some("explicit")
    );
}

#[test]
fn os_window_size_and_position_and_state() {
    let s = parse(
        "os_window_size 1180c 760c\nos_window_position -1200x80\nos_window_state maximized\nos_window_class Foo\nos_window_name bar\nlaunch\n",
    );
    let size = s.os_window_size.as_ref().unwrap();
    assert_eq!(size.width.size, 1180);
    assert_eq!(size.width.unit, "cells");
    assert_eq!(size.height.size, 760);
    assert_eq!(s.os_window_position, Some((-1200, 80)));
    assert_eq!(s.os_window_state.as_deref(), Some("maximized"));
    assert_eq!(s.os_window_class.as_deref(), Some("Foo"));
    assert_eq!(s.os_window_name.as_deref(), Some("bar"));
}

#[test]
fn os_window_size_pixels() {
    let s = parse("os_window_size 800 600\nlaunch\n");
    let size = s.os_window_size.unwrap();
    assert_eq!(size.width.unit, "px");
    assert_eq!(size.height.size, 600);
}

#[test]
fn new_os_window_yields_multiple_sessions() {
    let sessions = parse_sessions(
        "launch\nnew_os_window\nlaunch\nnew_os_window\nlaunch\n",
        &options(),
        None,
        "multi",
        "/tmp/multi.kitty-session",
    )
    .unwrap();
    assert_eq!(sessions.len(), 3);
    for s in &sessions {
        assert_eq!(s.tabs.len(), 1);
        assert_eq!(s.num_of_windows_in_definition, 1);
    }
}

#[test]
fn focus_tab_and_focus_os_window() {
    let s = parse("new_tab a\nlaunch\nnew_tab b\nlaunch\nfocus_tab 0\nfocus_os_window\n");
    assert_eq!(s.focus_tab_spec.as_deref(), Some("0"));
    assert!(s.focus_os_window);
    assert_eq!(s.tabs.len(), 2);
}

#[test]
fn empty_tab_is_replaced_by_next_new_tab() {
    // kitty removes a trailing tab with no windows when new_tab is seen.
    let s = parse("new_tab empty\nnew_tab other\nlaunch\n");
    assert_eq!(s.tabs.len(), 1);
    assert_eq!(s.tabs[0].name, "other");
    assert_eq!(s.tabs[0].windows.len(), 1);
}

#[test]
fn enabled_layouts_star_expands_all() {
    let s = parse("enabled_layouts *\nlaunch\n");
    assert_eq!(
        s.tabs[0].enabled_layouts,
        vec![
            "fat",
            "grid",
            "horizontal",
            "splits",
            "stack",
            "tall",
            "vertical"
        ]
    );
}

#[test]
fn unknown_command_is_an_error_with_line() {
    let e = parse_err("# c\nlaunch\nbogus thing\n");
    assert_eq!(e.line, 3);
    assert!(e.message.contains("Unknown command"));
}

#[test]
fn invalid_layout_is_an_error() {
    let e = parse_err("layout bogus\nlaunch\n");
    assert!(e.message.contains("not a valid layout"));
}

#[test]
fn invalid_os_window_position_is_an_error() {
    let e = parse_err("os_window_position 100\nlaunch\n");
    assert!(e.message.contains("Invalid os_window_position"));
}

#[test]
fn malformed_layout_state_is_an_error() {
    let e = parse_err("set_layout_state {not json}\nlaunch\n");
    assert!(e.message.contains("expected object key") || e.message.contains("JSON"));
}

#[test]
fn malformed_launch_quoting_is_an_error() {
    let e = parse_err("launch 'unterminated\n");
    assert!(e.message.contains("Unterminated"));
}

#[test]
fn unserialize_launch_data_sets_ids_and_shell_command() {
    let s = parse(
        "launch 'kitty-unserialize-data={\"id\": 7, \"cmd_at_shell_startup\": [\"ls\", \"-l\"]}' sh\n",
    );
    let w = &s.tabs[0].windows[0];
    assert_eq!(w.serialized_id, 7);
    assert_eq!(w.run_command_at_shell_startup, vec!["ls", "-l"]);
    assert_eq!(w.launch.args, vec!["sh"]);
}

#[test]
fn resize_and_focus_matching_are_attached_to_last_window() {
    let s = parse("launch\nresize_window wider 3\nfocus_matching_window state:focused\nlaunch\n");
    assert_eq!(s.tabs[0].windows[0].resize_spec, Some(("wider".into(), 3)));
    assert_eq!(
        s.tabs[0].windows[0].focus_matching_window.as_deref(),
        Some("state:focused")
    );
}

#[test]
fn session_name_and_extension_helpers() {
    assert_eq!(session_arg_to_name("/x/y/foo.kitty-session"), "foo");
    assert_eq!(session_arg_to_name("bar.session"), "bar");
    assert_eq!(session_arg_to_name("-"), "");
    assert!(session::has_session_extension("x.kitty_session"));
}

fn manager_with_window(launch: LaunchSpec) -> TabManager {
    let cfg = LayoutConfig::default()
        .with_central(Region::new(0, 0, 1000, 600))
        .with_cell(10, 20);
    let mut tm = TabManager::new(1);
    tm.add_tab(
        "main",
        Some(PathBuf::from("/tmp/proj")),
        &["tall".into()],
        cfg,
    )
    .unwrap();
    let mut w = Window::new(1, "title").with_cwd("/tmp/proj");
    w.launch = launch;
    w.override_title = Some("override".into());
    tm.add_window(0, w, &LayoutAddOptions::default());
    tm
}

#[test]
fn serialized_session_roundtrips_metadata() {
    let mut launch = LaunchSpec::with_args(["sleep", "10"]);
    launch.env.insert("FOO".into(), "bar baz".into());
    launch.vars.insert("user".into(), "kovid".into());
    launch.hold = true;
    let tm = manager_with_window(launch);
    let lines = serialize_session(&tm, "/tmp/round.kitty-session", false);
    let raw = lines.join("\n");
    assert!(raw.contains("new_tab main"));
    assert!(raw.contains("layout tall"));
    assert!(raw.contains("enabled_layouts tall"));
    assert!(raw.contains("cd /tmp/proj"));
    assert!(raw.contains("--env=FOO=bar baz") || raw.contains("--env='FOO=bar baz'"));
    assert!(raw.contains("--title=override"));
    let parsed = parse_session(
        &raw,
        &options(),
        None,
        "round.kitty-session",
        "/tmp/round.kitty-session",
    )
    .unwrap();
    assert_eq!(parsed.tabs.len(), 1);
    assert_eq!(parsed.tabs[0].windows.len(), 1);
    let w = &parsed.tabs[0].windows[0];
    assert_eq!(w.serialized_id, 1);
    assert_eq!(w.launch.args, vec!["sleep", "10"]);
    assert_eq!(w.launch.env.get("FOO").map(String::as_str), Some("bar baz"));
    assert_eq!(w.launch.vars.get("user").map(String::as_str), Some("kovid"));
    assert!(w.launch.hold);
    assert_eq!(w.launch.title.as_deref(), Some("override"));
    assert_eq!(
        parsed.tabs[0].windows[0].launch.cwd.as_deref(),
        Some("/tmp/proj")
    );
    assert!(parsed.tabs[0].layout_state.is_some());
}

#[test]
fn serialized_session_preserves_layout_state() {
    let tm = manager_with_window(LaunchSpec::with_args(["sh"]));
    let lines = serialize_session(&tm, "/tmp/x.kitty-session", false);
    let raw = lines.join("\n");
    let parsed = parse_session(&raw, &options(), None, "x", "/tmp/x.kitty-session").unwrap();
    let state = parsed.tabs[0].layout_state.as_ref().unwrap();
    assert_eq!(state.get_str("class"), Some("Tall"));
    assert!(state.get("all_windows").is_some());
    assert!(state.get_str("last_used_layout").is_none());
}

#[test]
fn serialize_relocatable_cwds() {
    let cfg = LayoutConfig::default();
    let mut tm = TabManager::new(1);
    tm.add_tab("t", None, &["tall".into()], cfg).unwrap();
    let mut a = Window::new(1, "a").with_cwd("/base/proj/a");
    a.launch = LaunchSpec::with_args(["sh"]);
    let mut b = Window::new(2, "b").with_cwd("/base/proj/b");
    b.launch = LaunchSpec::with_args(["sh"]);
    tm.add_window(0, a, &LayoutAddOptions::default());
    tm.add_window(0, b, &LayoutAddOptions::default());
    let lines = serialize_session(&tm, "/base/session.kitty-session", true);
    let raw = lines.join("\n");
    assert!(raw.contains("cd proj/a") || raw.contains("cd proj/b"));
    assert!(raw.contains("--cwd="));
}

#[test]
fn restore_plan_contains_tabs_and_windows() {
    let s = parse(
        "new_tab work\ncd /tmp/work\nlaunch 'kitty-unserialize-data={\"id\": 9}' --env=A=B --title=hi sh -c 'echo ok'\nfocus\n",
    );
    let plan = s.restore_plan();
    assert_eq!(plan.os_windows.len(), 1);
    let os = &plan.os_windows[0];
    assert_eq!(os.tabs.len(), 1);
    let tab = &os.tabs[0];
    assert_eq!(tab.name, "work");
    assert_eq!(tab.windows.len(), 1);
    let w = &tab.windows[0];
    assert_eq!(w.serialized_id, 9);
    assert_eq!(w.cwd.as_deref(), Some(Path::new("/tmp/work")));
    assert_eq!(w.title.as_deref(), Some("hi"));
    assert_eq!(w.launch.env.get("A").map(String::as_str), Some("B"));
    assert_eq!(w.launch.args, vec!["sh", "-c", "echo ok"]);
    assert_eq!(tab.active_window_idx, 0);
}

#[test]
fn restore_plans_batches_os_windows() {
    let sessions = parse_sessions(
        "launch\nnew_os_window\nlaunch\n",
        &options(),
        None,
        "x",
        "/tmp/x.kitty-session",
    )
    .unwrap();
    let plan = restore_plans(&sessions);
    assert_eq!(plan.os_windows.len(), 2);
}

#[test]
fn defaults_add_shell_when_no_launch_present() {
    let s = parse("new_tab only-title\n");
    assert_eq!(s.num_of_windows_in_definition, 1);
    assert_eq!(s.tabs[0].windows[0].launch.args, vec!["/bin/sh"]);
}

#[test]
fn shell_quoting_handles_single_quotes_and_empty() {
    let args = vec![
        "a b".to_string(),
        "it's".to_string(),
        String::new(),
        "plain".to_string(),
    ];
    let line = kitty_ui::shlex_join(&args);
    assert_eq!(kitty_ui::shlex_split(&line).unwrap(), args);
}

#[test]
fn tabs_are_preserved_in_serialization() {
    let cfg = LayoutConfig::default();
    let mut tm = TabManager::new(1);
    tm.add_tab("one", None, &["tall".into()], cfg).unwrap();
    tm.add_window(0, Window::new(1, "a"), &LayoutAddOptions::default());
    tm.add_tab("two", None, &["tall".into()], cfg).unwrap();
    tm.add_window(1, Window::new(2, "b"), &LayoutAddOptions::default());
    tm.set_active_tab_idx(0);
    let raw = serialize_session(&tm, "/tmp/t.kitty-session", false).join("\n");
    let s = parse_session(&raw, &options(), None, "t", "/tmp/t.kitty-session").unwrap();
    assert_eq!(s.tabs.len(), 2);
    assert_eq!(s.tabs[0].name, "one");
    assert_eq!(s.tabs[1].name, "two");
    assert!(raw.contains("focus_tab 0"));
}

#[test]
fn os_window_metadata_roundtrips() {
    let tm = manager_with_window(LaunchSpec::with_args(["sh"]));
    let mut tm = tm;
    tm.os_window.class = Some("MyClass".into());
    tm.os_window.name = Some("MyName".into());
    tm.os_window.title = Some("My Title".into());
    tm.os_window.state = Some("maximized".into());
    tm.os_window.size = Some(kitty_ui::OsWindowSize {
        width: 1200,
        width_unit: "px".into(),
        height: 760,
        height_unit: "px".into(),
    });
    tm.os_window.position = Some((-1200, 80));
    let raw = serialize_session(&tm, "/tmp/meta.kitty-session", false).join("\n");
    let s = parse_session(&raw, &options(), None, "meta", "/tmp/meta.kitty-session").unwrap();
    assert_eq!(s.os_window_class.as_deref(), Some("MyClass"));
    assert_eq!(s.os_window_name.as_deref(), Some("MyName"));
    assert_eq!(s.os_window_title.as_deref(), Some("My Title"));
    assert_eq!(s.os_window_state.as_deref(), Some("maximized"));
    assert_eq!(s.os_window_position, Some((-1200, 80)));
    let size = s.os_window_size.unwrap();
    assert_eq!((size.width.size, size.width.unit.as_str()), (1200, "px"));
    assert_eq!((size.height.size, size.height.unit.as_str()), (760, "px"));
}

#[test]
fn launch_option_values_are_env_expanded() {
    let mut env = BTreeMap::new();
    env.insert("DIR".into(), "/srv/app".into());
    env.insert("VAL".into(), "hello".into());
    env.insert("TITLE".into(), "expanded title".into());
    let s = parse_session(
        "launch --cwd=$DIR --env=A=$VAL --title='$TITLE' sh -c 'echo $VAL'\n",
        &options(),
        Some(&env),
        "x",
        "/tmp/x.kitty-session",
    )
    .unwrap();
    let w = &s.tabs[0].windows[0];
    assert_eq!(w.launch.cwd.as_deref(), Some("/srv/app"));
    assert_eq!(w.launch.env.get("A").map(String::as_str), Some("hello"));
    assert_eq!(w.launch.title.as_deref(), Some("expanded title"));
    // Program arguments are not expanded, matching kitty.
    assert_eq!(w.launch.args, vec!["sh", "-c", "echo $VAL"]);
}

#[test]
fn window_user_vars_are_serialized() {
    let cfg = LayoutConfig::default();
    let mut tm = TabManager::new(1);
    tm.add_tab("t", None, &["tall".into()], cfg).unwrap();
    let mut w = Window::new(1, "a");
    w.launch = LaunchSpec::with_args(["sh"]);
    w.user_vars.insert("kitten".into(), "value".into());
    tm.add_window(0, w, &LayoutAddOptions::default());
    let raw = serialize_session(&tm, "/tmp/v.kitty-session", false).join("\n");
    assert!(raw.contains("--var=kitten=value"));
    let s = parse_session(&raw, &options(), None, "v", "/tmp/v.kitty-session").unwrap();
    assert_eq!(
        s.tabs[0].windows[0]
            .launch
            .vars
            .get("kitten")
            .map(String::as_str),
        Some("value")
    );
}
