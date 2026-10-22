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
