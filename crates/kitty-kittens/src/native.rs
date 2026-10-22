// SPDX-License-Identifier: GPL-3.0-or-later
//! Catalog of kittens that have a Rust-native implementation in this crate.

/// A native kitten entry used by help/listing output and the binary dispatcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeKitten {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub usage: &'static str,
}

/// Native kittens, in dispatch order.
pub const NATIVE_KITTENS: &[NativeKitten] = &[
    NativeKitten {
        name: "icat",
        aliases: &[],
        description: "Display images in the terminal",
        usage: crate::icat::ICAT_USAGE,
    },
    NativeKitten {
        name: "clipboard",
        aliases: &[],
        description: "Copy/paste with the system clipboard, even over SSH",
        usage: crate::clipboard::CLIPBOARD_USAGE,
    },
    NativeKitten {
        name: "notify",
        aliases: &[],
        description: "Send notifications to the user",
        usage: crate::notify::NOTIFY_USAGE,
    },
    NativeKitten {
        name: "hints",
        aliases: &["url_hints"],
        description: "Select text from the screen using the keyboard",
        usage: crate::hints::HINTS_USAGE,
    },
    NativeKitten {
        name: "unicode_input",
        aliases: &[],
        description: "Browse and select Unicode characters by name",
        usage: crate::unicode_input::UNICODE_INPUT_USAGE,
    },
];

/// Look up a native kitten by canonical name or alias.
pub fn native_kitten(name: &str) -> Option<&'static NativeKitten> {
    NATIVE_KITTENS
        .iter()
        .find(|kitten| kitten.name == name || kitten.aliases.contains(&name))
}

/// Whether `name` has a native implementation.
pub fn is_native_kitten(name: &str) -> bool {
    native_kitten(name).is_some()
}
