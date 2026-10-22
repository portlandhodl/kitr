// SPDX-License-Identifier: GPL-3.0-or-later
//! Deterministic filesystem globbing for `globinclude`, plus the
//! `fnmatch.fnmatchcase` subset used by `envinclude`. No external crates.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub fn glob_paths(base: &Path, pattern: &str) -> Result<Vec<PathBuf>, String> {
    let mut ans = Vec::new();
    let (mut root, parts) = if pattern.starts_with('/') {
        (PathBuf::from("/"), split_components(pattern))
    } else {
        (base.to_path_buf(), split_components(pattern))
    };
    if parts.is_empty() {
        if root.exists() {
            ans.push(root);
        }
        return Ok(ans);
    }
    // A literal prefix is cheaper to walk and preserves the normal path shape.
    let mut idx = 0;
    while idx < parts.len() && !has_wildcard(&parts[idx]) {
        root.push(&parts[idx]);
        idx += 1;
    }
    if idx == parts.len() {
        if root.exists() {
            ans.push(root);
        }
    } else {
        walk(&root, &parts[idx..], &mut ans);
    }
    ans.sort();
    ans.dedup();
    Ok(ans)
}

fn split_components(pattern: &str) -> Vec<String> {
    pattern
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .map(str::to_string)
        .collect()
}

fn has_wildcard(part: &str) -> bool {
    part.contains(['*', '?', '['])
}

fn walk(dir: &Path, parts: &[String], out: &mut Vec<PathBuf>) {
    if parts.is_empty() {
        if dir.exists() {
            out.push(dir.to_path_buf());
        }
        return;
    }
    let part = &parts[0];
    let rest = &parts[1..];
    if part == "**" {
        walk(dir, rest, out);
        let mut entries = match read_dir_sorted(dir) {
            Some(entries) => entries,
            None => return,
        };
        entries.retain(|path| !is_symlink(path) && path.is_dir());
        for entry in entries {
            walk(&entry, parts, out);
        }
        return;
    }
    if !has_wildcard(part) {
        let next = dir.join(part);
        if !rest.is_empty() {
            if next.is_dir() {
                walk(&next, rest, out);
            }
        } else if next.exists() {
            out.push(next);
        }
        return;
    }
    if let Some(entries) = read_dir_sorted(dir) {
        for entry in entries {
            let Some(name) = entry.file_name().and_then(|x| x.to_str()) else {
                continue;
            };
            if fnmatchcase(name, part) {
                walk(&entry, rest, out);
            }
        }
    }
}

fn read_dir_sorted(dir: &Path) -> Option<Vec<PathBuf>> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|x| x.path()))
        .collect();
    entries.sort();
    Some(entries)
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
}

/// Python's `fnmatch.fnmatchcase` for the common `*`, `?`, `[seq]` and
/// `[!seq]` forms.
pub fn fnmatchcase(name: &str, pattern: &str) -> bool {
    let name: Vec<char> = name.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    let mut memo = HashSet::new();
    match_here(&name, 0, &pattern, 0, &mut memo)
}

fn match_here(
    name: &[char],
    ni: usize,
    pattern: &[char],
    pi: usize,
    memo: &mut HashSet<(usize, usize)>,
) -> bool {
    if !memo.insert((ni, pi)) {
        return false;
    }
    if pi == pattern.len() {
        return ni == name.len();
    }
    match pattern[pi] {
        '*' => {
            let mut next = ni;
            loop {
                if match_here(name, next, pattern, pi + 1, memo) {
                    return true;
                }
                if next == name.len() {
                    return false;
                }
                next += 1;
            }
        }
        '?' => ni < name.len() && match_here(name, ni + 1, pattern, pi + 1, memo),
        '[' => {
            if ni >= name.len() {
                return false;
            }
            if let Some((matches, next_pi)) = match_class(name[ni], pattern, pi) {
                matches && match_here(name, ni + 1, pattern, next_pi, memo)
            } else {
                // Malformed class: Python treats '[' literally.
                name[ni] == '[' && match_here(name, ni + 1, pattern, pi + 1, memo)
            }
        }
        ch => ni < name.len() && name[ni] == ch && match_here(name, ni + 1, pattern, pi + 1, memo),
    }
}

fn match_class(ch: char, pattern: &[char], start: usize) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negated = matches!(pattern.get(i), Some('!'));
    if negated {
        i += 1;
    }
    let class_start = i;
    let mut matched = false;
    while i < pattern.len() {
        if pattern[i] == ']' && i > class_start {
            return Some((matched != negated, i + 1));
        }
        let low = pattern[i];
        if i + 2 < pattern.len() && pattern[i + 1] == '-' && pattern[i + 2] != ']' {
            let high = pattern[i + 2];
            if low <= ch && ch <= high {
                matched = true;
            }
            i += 3;
        } else {
            if low == ch {
                matched = true;
            }
            i += 1;
        }
    }
    None
}
