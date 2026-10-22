// SPDX-License-Identifier: GPL-3.0-or-later
//! Environment shared by native kittens and the remote-control client.
//!
//! Kitty exports `KITTY_WINDOW_ID`, `KITTY_LISTEN_ON`, `KITTY_PUBLIC_KEY` and
//! the `KITTY_KITTEN_*` bootstrap variables to child processes. Native kittens
//! (`icat`) and `kitten @` need the same values, so this module reads them
//! once, accepts the `KITTY_KITTEN_*` spelling as an override, and parses the
//! window id in the forms kitty accepts (`17`, `id:17`).
//!
//! The lookup is deliberately injectable: tests never touch the process-wide
//! environment.

use std::path::PathBuf;

/// The environment that a native kitten or the remote client inherits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KittenEnvironment {
    /// `KITTY_WINDOW_ID` (or `KITTY_KITTEN_WINDOW_ID`).
    pub window_id: Option<u64>,
    /// `KITTY_LISTEN_ON` (or `KITTY_KITTEN_LISTEN_ON`).
    pub listen_on: Option<String>,
    /// `KITTY_PUBLIC_KEY` (or `KITTY_KITTEN_PUBLIC_KEY`).
    pub public_key: Option<String>,
    /// Remote-control password from `KITTY_RC_PASSWORD`/`KITTY_PASSWORD`.
    pub password: Option<String>,
    /// `KITTY_KITTEN_NAME`.
    pub kitten_name: Option<String>,
    /// `KITTY_KITTEN_PATH`.
    pub kitten_path: Option<PathBuf>,
    /// `KITTY_KITTEN_DIRECTORY`.
    pub kitten_directory: Option<PathBuf>,
    /// `KITTY_KITTEN_BOOTSTRAPPED`.
    pub bootstrapped: bool,
}

impl KittenEnvironment {
    /// Read the variables from the current process environment.
    pub fn from_process_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Read the variables through a lookup function. This is the testable
    /// entry point; an absent or empty value is treated as unset.
    pub fn from_lookup<F>(mut lookup: F) -> Self
    where
        F: FnMut(&str) -> Option<String>,
    {
        let get = |name: &str, lookup: &mut F| {
            lookup(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let first =
            |names: &[&str], lookup: &mut F| names.iter().find_map(|name| get(name, lookup));

        let window_id = first(&["KITTY_KITTEN_WINDOW_ID", "KITTY_WINDOW_ID"], &mut lookup)
            .and_then(|value| parse_window_id(&value));
        let listen_on = first(&["KITTY_KITTEN_LISTEN_ON", "KITTY_LISTEN_ON"], &mut lookup);
        let public_key = first(
            &["KITTY_KITTEN_PUBLIC_KEY", "KITTY_PUBLIC_KEY"],
            &mut lookup,
        );
        let password = first(
            &[
                "KITTY_KITTEN_PASSWORD",
                "KITTY_RC_PASSWORD",
                "KITTY_PASSWORD",
            ],
            &mut lookup,
        );
        let kitten_name = get("KITTY_KITTEN_NAME", &mut lookup);
        let kitten_path = get("KITTY_KITTEN_PATH", &mut lookup).map(PathBuf::from);
        let kitten_directory = get("KITTY_KITTEN_DIRECTORY", &mut lookup).map(PathBuf::from);
        let bootstrapped =
            get("KITTY_KITTEN_BOOTSTRAPPED", &mut lookup).is_some_and(|value| value != "0");

        Self {
            window_id,
            listen_on,
            public_key,
            password,
            kitten_name,
            kitten_path,
            kitten_directory,
            bootstrapped,
        }
    }

    /// The remote socket, if any.
    pub fn socket(&self) -> Option<&str> {
        self.listen_on.as_deref()
    }

    /// Whether a public key is present for encrypted remote control.
    pub fn has_public_key(&self) -> bool {
        self.public_key.is_some()
    }
}

/// Parse a kitty window id. Accepts decimal ids and the `id:17` spelling used
/// by match expressions. Zero and negative ids are rejected.
pub fn parse_window_id(value: &str) -> Option<u64> {
    let value = value.trim();
    let value = value.strip_prefix("id:").unwrap_or(value);
    if value.is_empty() || value.starts_with('-') || value.starts_with('+') {
        return None;
    }
    let parsed = if let Some(hex) = value.strip_prefix("0x") {
        if hex.is_empty() {
            return None;
        }
        u64::from_str_radix(hex, 16).ok()?
    } else {
        value.parse::<u64>().ok()?
    };
    (parsed != 0).then_some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn env(pairs: &[(&str, &str)]) -> KittenEnvironment {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        KittenEnvironment::from_lookup(|name| map.get(name).cloned())
    }

    #[test]
    fn parses_plain_prefixed_and_hex_window_ids() {
        assert_eq!(parse_window_id("17"), Some(17));
        assert_eq!(parse_window_id(" id:42 "), Some(42));
        assert_eq!(parse_window_id("0x10"), Some(16));
        assert_eq!(parse_window_id("0"), None);
        assert_eq!(parse_window_id("-1"), None);
        assert_eq!(parse_window_id("nope"), None);
    }

    #[test]
    fn kitten_prefixed_variables_override_kitty_variables() {
        let parsed = env(&[
            ("KITTY_WINDOW_ID", "1"),
            ("KITTY_KITTEN_WINDOW_ID", "id:9"),
            ("KITTY_LISTEN_ON", "unix:/a"),
            ("KITTY_KITTEN_LISTEN_ON", "unix:/b"),
            ("KITTY_PUBLIC_KEY", "1:old"),
            ("KITTY_KITTEN_PUBLIC_KEY", "1:new"),
            ("KITTY_RC_PASSWORD", "secret"),
            ("KITTY_KITTEN_NAME", "icat"),
            ("KITTY_KITTEN_BOOTSTRAPPED", "1"),
        ]);
        assert_eq!(parsed.window_id, Some(9));
        assert_eq!(parsed.listen_on.as_deref(), Some("unix:/b"));
        assert_eq!(parsed.public_key.as_deref(), Some("1:new"));
        assert_eq!(parsed.password.as_deref(), Some("secret"));
        assert_eq!(parsed.kitten_name.as_deref(), Some("icat"));
        assert!(parsed.bootstrapped);
    }

    #[test]
    fn empty_values_are_unset_and_invalid_window_ids_are_ignored() {
        let parsed = env(&[
            ("KITTY_WINDOW_ID", "not-a-number"),
            ("KITTY_LISTEN_ON", "   "),
            ("KITTY_KITTEN_BOOTSTRAPPED", "0"),
        ]);
        assert_eq!(parsed.window_id, None);
        assert_eq!(parsed.listen_on, None);
        assert!(!parsed.bootstrapped);
        assert!(!parsed.has_public_key());
    }
}
