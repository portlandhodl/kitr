// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end checks for the native kitten dispatch.

use std::process::Command;

fn kitten(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kitten"))
        .args(args)
        .output()
        .expect("run kitten")
}

#[test]
fn clipboard_help_and_clear_are_native() {
    let help = kitten(&["clipboard", "--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("OSC 52"));

    let clear = kitten(&["clipboard", "--clear", "--primary"]);
    assert!(
        clear.status.success(),
        "{}",
        String::from_utf8_lossy(&clear.stderr)
    );
    assert_eq!(clear.stdout, b"\x1b]52;p;\x1b\\");

    let set = kitten(&["clipboard", "hello"]);
    assert!(set.status.success());
    assert_eq!(set.stdout, b"\x1b]52;c;aGVsbG8\x1b\\");
}

#[test]
fn notify_only_print_escape_code_is_native() {
    for args in [
        vec!["notify", "--only-print-escape-code", "--identifier=test"],
        vec![
            "notify",
            "--only-print-escape-code",
            "--identifier=test",
            "--wait-till-closed",
            "",
        ],
    ] {
        let output = kitten(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"\x1b]99;i=test:p=close;\x1b\\");
    }
}

#[test]
fn hints_listing_and_selection_are_native() {
    let output = kitten(&[
        "hints",
        "--type=word",
        "--text=alpha beta",
        "--program=@",
        "--select=2",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"clipboard\talpha\n");

    let listing = kitten(&["hints", "--type=url", "--text=see https://example.com"]);
    assert!(listing.status.success());
    let text = String::from_utf8_lossy(&listing.stdout);
    assert!(text.contains("https://example.com"));
}

#[test]
fn unicode_input_emits_a_selected_character() {
    let output = kitten(&["unicode_input", "--codepoint=1f600"]);
    assert!(output.status.success());
    assert_eq!(output.stdout, "😀\n".as_bytes());
}

#[test]
fn help_and_listing_mark_native_kittens() {
    let help = kitten(&["help", "hints"]);
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(text.contains("built-in, native"));
    assert!(text.contains("Usage: kitten hints"));

    let listing = kitten(&["list"]);
    assert!(listing.status.success());
    let text = String::from_utf8_lossy(&listing.stdout);
    for name in ["clipboard", "notify", "hints", "unicode_input", "icat"] {
        assert!(
            text.contains(&format!("{name}\t")),
            "missing {name} in {text}"
        );
    }
    assert!(text.contains("builtin, native"));
}

#[test]
fn newly_native_kittens_have_help() {
    for (name, marker) in [
        ("transfer", "Transfer files"),
        ("ssh", "Connect to a remote host"),
        ("themes", "Manage kitty color schemes"),
        ("show-key", "Show the codes"),
        ("query-terminal", "Queries:"),
        ("resize-window", "Resize the current window"),
        ("broadcast", "Broadcast typed text"),
        ("ask", "Ask the user"),
        ("choose-files", "Choose files"),
        ("choose-fonts", "Choose the fonts"),
        ("command-palette", "Browse and trigger"),
        ("desktop-ui", "Usage: kitten desktop-ui"),
        ("diff", "side-by-side"),
        ("dnd", "drag and drop"),
        ("hyperlinked-grep", "ripgrep"),
        ("pager", "pager"),
        ("panel", "accelerated panel"),
        ("quick-access-terminal", "quick access terminal"),
        ("remote-file", "remote file"),
    ] {
        let output = kitten(&[name, "--help"]);
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.to_lowercase().contains(&marker.to_lowercase()),
            "{name}: missing {marker:?} in {text}"
        );
    }
}

#[test]
fn clipboard_mime_help_is_native() {
    let output = kitten(&["clipboard", "--mime", "--help"]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("MIME"));
}

#[test]
fn native_listing_includes_all_portable_kittens() {
    let output = kitten(&["list"]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    for name in [
        "transfer",
        "ssh",
        "themes",
        "show-key",
        "query-terminal",
        "resize-window",
        "broadcast",
        "ask",
        "choose-files",
        "choose-fonts",
        "command-palette",
        "desktop-ui",
        "diff",
        "dnd",
        "hyperlinked-grep",
        "pager",
        "panel",
        "quick-access-terminal",
        "remote-file",
    ] {
        assert!(
            text.contains(&format!("{name}\t")),
            "missing {name} in {text}"
        );
    }
}
