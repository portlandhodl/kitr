// SPDX-License-Identifier: GPL-3.0-or-later
//! Small ports of `kitty.utils.expandvars` and the tilde part of
//! `os.path.expanduser`, used by include paths and the `env` option.

use std::collections::BTreeMap;

/// Expand `$VAR`, `${VAR}` and `$$`, preferring `env` over the process
/// environment. Unknown variables are left untouched, exactly like kitty.
pub fn expand_vars(input: &str, env: &BTreeMap<String, String>, fallback_to_os: bool) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch != '$' {
            out.push(ch);
            i += 1;
            continue;
        }
        if chars.get(i + 1) == Some(&'$') {
            out.push('$');
            i += 2;
            continue;
        }
        if chars.get(i + 1) == Some(&'{') {
            if let Some(end) = chars[i + 2..].iter().position(|&c| c == '}') {
                let end = i + 2 + end;
                if end > i + 2 {
                    let key: String = chars[i + 2..end].iter().collect();
                    push_value_or_original(&mut out, &key, env, fallback_to_os, &chars[i..=end]);
                    i = end + 1;
                    continue;
                }
            }
            out.push('$');
            i += 1;
            continue;
        }
        let mut end = i + 1;
        while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
        if end > i + 1 {
            let key: String = chars[i + 1..end].iter().collect();
            push_value_or_original(&mut out, &key, env, fallback_to_os, &chars[i..end]);
            i = end;
        } else {
            out.push('$');
            i += 1;
        }
    }
    out
}

fn push_value_or_original(
    out: &mut String,
    key: &str,
    env: &BTreeMap<String, String>,
    fallback_to_os: bool,
    original: &[char],
) {
    if let Some(value) = env.get(key) {
        out.push_str(value);
    } else if fallback_to_os {
        match std::env::var(key) {
            Ok(value) => out.push_str(&value),
            Err(_) => out.extend(original.iter()),
        }
    } else {
        out.extend(original.iter());
    }
}

/// Expand a leading `~` or `~/` using `$HOME`. Unlike Python's
/// `os.path.expanduser`, `~other-user` is left untouched (documented
/// deviation; this port does not read the password database).
pub fn expand_user(path: &str) -> String {
    if path == "~" {
        return std::env::var("HOME").unwrap_or_else(|_| path.to_string());
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    path.to_string()
}

pub fn os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(any(
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )) {
        "bsd"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}
