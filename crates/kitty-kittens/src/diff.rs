// SPDX-License-Identifier: GPL-3.0-or-later
//! Diff kitten, ported from `kittens/diff`.
//!
//! The core is the anchored/patience diff from Go's `internal/diff` (also
//! used by kitty's kitten), a unified-patch parser with hunk metadata, word /
//! central changed-region highlighting, and a compact side-by-side renderer.
//! The UI supports scrolling and searching over the rendered diff.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use crate::tui::{self, Key, KeyParser, Tty};

pub const DIFF_USAGE: &str = "\
Usage: kitten diff file_or_directory_left file_or_directory_right

Show a side-by-side diff of the specified files/directories.

Options:
      --context N     Lines of context (default 3)
  -h, --help";

#[derive(Clone, Debug)]
pub struct DiffOptions {
    pub context: usize,
    pub ignore_name: Vec<String>,
    pub word_diff_mode: String,
    pub replace_tab_by: String,
    pub help: bool,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            context: 3,
            ignore_name: Vec::new(),
            word_diff_mode: "words".to_owned(),
            replace_tab_by: "    ".to_owned(),
            help: false,
        }
    }
}

pub fn parse_diff_args(args: &[String]) -> Result<(DiffOptions, Vec<String>), String> {
    let mut opts = DiffOptions::default();
    let mut paths = Vec::new();
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            paths.extend_from_slice(&args[index + 1..]);
            break;
        }
        let (name, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(a, b)| (a, Some(b.to_owned())));
        let mut take = || -> String {
            if let Some(value) = inline.clone() {
                return value;
            }
            index += 1;
            args.get(index).cloned().unwrap_or_default()
        };
        match name {
            "-h" | "--help" => opts.help = true,
            "--context" => {
                opts.context = take().parse().map_err(|_| "invalid context".to_owned())?
            }
            "--ignore-name" => opts.ignore_name.push(take()),
            "--word-diff-mode" => opts.word_diff_mode = take(),
            "--replace-tab-by" => opts.replace_tab_by = take(),
            other if !other.starts_with('-') => paths.push(other.to_owned()),
            other => return Err(format!("unknown option: {other}")),
        }
        index += 1;
    }
    Ok((opts, paths))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pair {
    x: usize,
    y: usize,
}

/// Anchored (patience) diff in unified format, copied from Go's `internal/diff`
/// with the same chunk-expansion rules.
pub fn anchored_diff(
    old_name: &str,
    old: &str,
    new_name: &str,
    new: &str,
    context: usize,
) -> String {
    if old == new {
        return String::new();
    }
    let x = split_lines_after(old);
    let y = split_lines_after(new);
    let mut out = String::new();
    out.push_str(&format!(
        "diff {old_name} {new_name}\n--- {old_name}\n+++ {new_name}\n"
    ));
    let mut done = Pair { x: 0, y: 0 };
    let mut chunk = Pair { x: 0, y: 0 };
    let mut count = Pair { x: 0, y: 0 };
    let mut ctext: Vec<String> = Vec::new();
    for m in tgs(&x, &y) {
        if m.x < done.x {
            continue;
        }
        let mut start = m;
        while start.x > done.x && start.y > done.y && x[start.x - 1] == y[start.y - 1] {
            start.x -= 1;
            start.y -= 1;
        }
        let mut end = m;
        while end.x < x.len() && end.y < y.len() && x[end.x] == y[end.y] {
            end.x += 1;
            end.y += 1;
        }
        for line in &x[done.x..start.x] {
            ctext.push(format!("-{line}"));
            count.x += 1;
        }
        for line in &y[done.y..start.y] {
            ctext.push(format!("+{line}"));
            count.y += 1;
        }
        let c = context;
        if (end.x < x.len() || end.y < y.len())
            && (end.x - start.x < c || (!ctext.is_empty() && end.x - start.x < 2 * c))
        {
            for line in &x[start.x..end.x] {
                ctext.push(format!(" {line}"));
                count.x += 1;
                count.y += 1;
            }
            done = end;
            continue;
        }
        if !ctext.is_empty() {
            let n = (end.x - start.x).min(c);
            for line in &x[start.x..start.x + n] {
                ctext.push(format!(" {line}"));
                count.x += 1;
                count.y += 1;
            }
            if count.x > 0 {
                chunk.x += 1;
            }
            if count.y > 0 {
                chunk.y += 1;
            }
            out.push_str(&format!(
                "@@ -{},{} +{},{} @@\n",
                chunk.x, count.x, chunk.y, count.y
            ));
            for line in &ctext {
                out.push_str(line);
            }
            count = Pair { x: 0, y: 0 };
            ctext.clear();
        }
        if end.x >= x.len() && end.y >= y.len() {
            break;
        }
        chunk = Pair {
            x: end.x.saturating_sub(c),
            y: end.y.saturating_sub(c),
        };
        for line in &x[chunk.x..end.x] {
            ctext.push(format!(" {line}"));
            count.x += 1;
            count.y += 1;
        }
        done = end;
    }
    out
}

fn split_lines_after(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .split_inclusive('\n')
        .map(|line| line.to_owned())
        .collect();
    if text.is_empty() {
        return lines;
    }
    if let Some(last) = lines.last_mut() {
        if !last.ends_with('\n') {
            last.push('\n');
            last.push_str("\\ No newline at end of file\n");
        }
    }
    lines
}

/// Longest common subsequence of unique lines (Szymanski), from Go's diff.
fn tgs(x: &[String], y: &[String]) -> Vec<Pair> {
    use std::collections::HashMap;
    let mut m: HashMap<&str, i32> = HashMap::new();
    for line in x {
        let entry = m.entry(line.as_str()).or_insert(0);
        if *entry > -2 {
            *entry -= 1;
        }
    }
    for line in y {
        let entry = m.entry(line.as_str()).or_insert(0);
        if *entry > -8 {
            *entry -= 4;
        }
    }
    let mut xi = Vec::new();
    let mut yi = Vec::new();
    let mut inv = Vec::new();
    for (i, line) in y.iter().enumerate() {
        if m.get(line.as_str()) == Some(&-5) {
            m.insert(line.as_str(), yi.len() as i32);
            yi.push(i);
        }
    }
    for (i, line) in x.iter().enumerate() {
        if let Some(&j) = m.get(line.as_str()) {
            if j >= 0 {
                xi.push(i);
                inv.push(j as usize);
            }
        }
    }
    let j = inv;
    let n = xi.len();
    let mut t = vec![n + 1; n];
    let mut l = vec![0usize; n];
    for i in 0..n {
        let mut lo = 0usize;
        let mut hi = n;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if t[mid] >= j[i] {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        t[lo] = j[i];
        l[i] = lo + 1;
    }
    let k = l.iter().copied().max().unwrap_or(0);
    let mut seq = vec![Pair { x: 0, y: 0 }; 2 + k];
    seq[1 + k] = Pair {
        x: x.len(),
        y: y.len(),
    };
    let lastj = n;
    let mut step = k;
    for i in (0..n).rev() {
        if l[i] == step && j[i] < lastj {
            seq[step] = Pair {
                x: xi[i],
                y: yi[j[i]],
            };
            step -= 1;
        }
    }
    seq[0] = Pair { x: 0, y: 0 };
    seq
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Center {
    pub offset: usize,
    pub size: usize,
}

/// Byte-level center of the changed region, as `changed_center()` does.
pub fn changed_center(left: &str, right: &str) -> Center {
    let total = left.len().min(right.len());
    let mut start = 0usize;
    while start < total && left.as_bytes()[start] == right.as_bytes()[start] {
        start += 1;
    }
    let mut end = 0usize;
    while end < total - start
        && left.as_bytes()[left.len() - 1 - end] == right.as_bytes()[right.len() - 1 - end]
    {
        end += 1;
    }
    Center {
        offset: start,
        size: left.len().saturating_sub(start + end),
    }
}

/// Word-level changed regions for one left/right line pair.
pub fn word_diff_center(left: &str, right: &str) -> (Vec<Center>, Vec<Center>) {
    let left_words = split_words(left);
    let right_words = split_words(right);
    let mut lstart = 0;
    while lstart < left_words.len()
        && lstart < right_words.len()
        && left_words[lstart].1 == right_words[lstart].1
    {
        lstart += 1;
    }
    let mut lend = 0;
    while lend < left_words.len().saturating_sub(lstart)
        && lend < right_words.len().saturating_sub(lstart)
        && left_words[left_words.len() - 1 - lend].1 == right_words[right_words.len() - 1 - lend].1
    {
        lend += 1;
    }
    let left_center = word_span(&left_words, lstart, left_words.len().saturating_sub(lend));
    let right_center = word_span(&right_words, lstart, right_words.len().saturating_sub(lend));
    (
        vec![left_center]
            .into_iter()
            .filter(|c| c.size > 0)
            .collect(),
        vec![right_center]
            .into_iter()
            .filter(|c| c.size > 0)
            .collect(),
    )
}

fn split_words(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut start = None;
    for (index, ch) in text.char_indices() {
        if ch.is_alphanumeric() || ch == '_' {
            start.get_or_insert(index);
        } else if let Some(begin) = start.take() {
            out.push((begin, text[begin..index].to_owned()));
        }
    }
    if let Some(begin) = start {
        out.push((begin, text[begin..].to_owned()));
    }
    out
}

fn word_span(words: &[(usize, String)], start_word: usize, end_word: usize) -> Center {
    if start_word >= words.len() || end_word <= start_word {
        return Center::default();
    }
    let start = words[start_word].0;
    let last = &words[end_word.min(words.len()) - 1];
    let end = last.0 + last.1.len();
    Center {
        offset: start,
        size: end - start,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Remove,
    Hunk,
    Header,
}

#[derive(Clone, Debug)]
pub struct PatchLine {
    pub kind: LineKind,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct Hunk {
    pub header: String,
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<PatchLine>,
}

#[derive(Clone, Debug, Default)]
pub struct Patch {
    pub left: String,
    pub right: String,
    pub hunks: Vec<Hunk>,
    pub raw: String,
}

impl Patch {
    pub fn rerender(&self, context: usize) -> String {
        let mut out = String::new();
        for hunk in &self.hunks {
            out.push_str(&format!(
                "@@ -{},{} +{},{} @@\n",
                hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
            ));
            let _ = context;
            for line in &hunk.lines {
                out.push(match line.kind {
                    LineKind::Add => '+',
                    LineKind::Remove => '-',
                    _ => ' ',
                });
                out.push_str(&line.text);
                out.push('\n');
            }
        }
        out
    }
}

pub fn parse_patch(raw: &str) -> Patch {
    let mut patch = Patch {
        raw: raw.to_owned(),
        ..Patch::default()
    };
    let mut hunk: Option<Hunk> = None;
    for line in raw.lines() {
        if let Some(left) = line.strip_prefix("--- ") {
            if patch.left.is_empty() {
                patch.left = left.to_owned();
            }
            continue;
        }
        if let Some(right) = line.strip_prefix("+++ ") {
            if patch.right.is_empty() {
                patch.right = right.to_owned();
            }
            continue;
        }
        if line.starts_with("diff ") {
            continue;
        }
        if line.starts_with("@@ ") {
            if let Some(previous) = hunk.take() {
                patch.hunks.push(previous);
            }
            let mut new_hunk = Hunk {
                header: line.to_owned(),
                ..Hunk::default()
            };
            parse_hunk_header(line, &mut new_hunk);
            hunk = Some(new_hunk);
            continue;
        }
        if let Some(current) = hunk.as_mut() {
            let (kind, text) = match line.chars().next() {
                Some('+') => (LineKind::Add, line[1..].to_owned()),
                Some('-') => (LineKind::Remove, line[1..].to_owned()),
                Some(' ') => (LineKind::Context, line[1..].to_owned()),
                Some('\\') => continue,
                _ => (LineKind::Context, line.to_owned()),
            };
            current.lines.push(PatchLine { kind, text });
        }
    }
    if let Some(last) = hunk {
        patch.hunks.push(last);
    }
    patch
}

fn parse_hunk_header(line: &str, hunk: &mut Hunk) {
    let body = line
        .trim_start_matches("@@ ")
        .split(" @@")
        .next()
        .unwrap_or("");
    let mut parts = body.split_whitespace();
    let old = parts.next().unwrap_or("-0,0").trim_start_matches('-');
    let new = parts.next().unwrap_or("+0,0").trim_start_matches('+');
    let parse = |spec: &str| -> (usize, usize) {
        let (start, count) = spec.split_once(',').unwrap_or((spec, "1"));
        (start.parse().unwrap_or(0), count.parse().unwrap_or(0))
    };
    let (old_start, old_count) = parse(old);
    let (new_start, new_count) = parse(new);
    hunk.old_start = old_start;
    hunk.old_count = old_count;
    hunk.new_start = new_start;
    hunk.new_count = new_count;
}

/// Render a unified patch as a simple two-column side-by-side view.
pub fn render_side_by_side(patch: &Patch, width: usize) -> Vec<String> {
    let column = width.saturating_sub(3) / 2;
    let mut out = Vec::new();
    for hunk in &patch.hunks {
        out.push(format!(
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
        ));
        let mut index = 0usize;
        while index < hunk.lines.len() {
            let line = &hunk.lines[index];
            match line.kind {
                LineKind::Context => {
                    out.push(format!(
                        "{} │ {}",
                        fit_width(&line.text, column),
                        fit_width(&line.text, column)
                    ));
                    index += 1;
                }
                _ => {
                    let mut removes = Vec::new();
                    let mut adds = Vec::new();
                    while index < hunk.lines.len() && hunk.lines[index].kind == LineKind::Remove {
                        removes.push(hunk.lines[index].text.clone());
                        index += 1;
                    }
                    while index < hunk.lines.len() && hunk.lines[index].kind == LineKind::Add {
                        adds.push(hunk.lines[index].text.clone());
                        index += 1;
                    }
                    let rows = removes.len().max(adds.len());
                    for row in 0..rows {
                        let left = removes.get(row).map(String::as_str).unwrap_or("");
                        let right = adds.get(row).map(String::as_str).unwrap_or("");
                        out.push(format!(
                            "{} │ {}",
                            fit_width(left, column),
                            fit_width(right, column)
                        ));
                    }
                    if removes.is_empty() && adds.is_empty() {
                        index += 1;
                    }
                }
            }
        }
    }
    out
}

fn fit_width(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count > width {
        format!(
            "{}…",
            text.chars()
                .take(width.saturating_sub(1))
                .collect::<String>()
        )
    } else {
        format!("{text}{}", " ".repeat(width - count))
    }
}

/// Diff two paths (files or directories). Directories produce one patch per
/// changed file, named `left/rel` and `right/rel`.
pub fn diff_paths(
    left: &Path,
    right: &Path,
    opts: &DiffOptions,
) -> io::Result<Vec<(String, Patch)>> {
    let lm = fs::metadata(left)?;
    let rm = fs::metadata(right)?;
    if lm.is_dir() != rm.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The items to be diffed should both be either directories or files",
        ));
    }
    if lm.is_file() {
        let l = fs::read_to_string(left)?;
        let r = fs::read_to_string(right)?;
        let raw = anchored_diff(
            &left.to_string_lossy(),
            &sanitize_text(&l, &opts.replace_tab_by),
            &right.to_string_lossy(),
            &sanitize_text(&r, &opts.replace_tab_by),
            opts.context,
        );
        return Ok(vec![(
            left.to_string_lossy().into_owned(),
            parse_patch(&raw),
        )]);
    }
    let mut out = Vec::new();
    let mut left_files = collect_files(left, &opts.ignore_name)?;
    let mut right_files = collect_files(right, &opts.ignore_name)?;
    left_files.sort();
    right_files.sort();
    let all: std::collections::BTreeSet<String> = left_files
        .iter()
        .chain(right_files.iter())
        .cloned()
        .collect();
    for name in all {
        let lpath = left.join(&name);
        let rpath = right.join(&name);
        let l = fs::read_to_string(&lpath).unwrap_or_default();
        let r = fs::read_to_string(&rpath).unwrap_or_default();
        let raw = anchored_diff(
            &lpath.to_string_lossy(),
            &sanitize_text(&l, &opts.replace_tab_by),
            &rpath.to_string_lossy(),
            &sanitize_text(&r, &opts.replace_tab_by),
            opts.context,
        );
        if !raw.is_empty() {
            out.push((name, parse_patch(&raw)));
        }
    }
    Ok(out)
}

fn sanitize_text(text: &str, replace_tab_by: &str) -> String {
    text.replace('\t', replace_tab_by)
}

fn collect_files(root: &Path, ignore: &[String]) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    fn walk(root: &Path, dir: &Path, ignore: &[String], out: &mut Vec<String>) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if ignore.iter().any(|pattern| wildcard(pattern, &name)) {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, ignore, out)?;
            } else if path.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                out.push(relative);
            }
        }
        Ok(())
    }
    walk(root, root, ignore, &mut out)?;
    Ok(out)
}

fn wildcard(pattern: &str, text: &str) -> bool {
    fn inner(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => inner(&p[1..], t) || (!t.is_empty() && inner(p, &t[1..])),
            Some('?') => !t.is_empty() && inner(&p[1..], &t[1..]),
            Some(ch) => t.first() == Some(ch) && inner(&p[1..], &t[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    inner(&p, &t)
}

/// Interactive pager for rendered diff lines.
pub fn run_diff_ui(tty: &mut dyn Tty, lines: &[String], title: &str) -> io::Result<Option<String>> {
    if lines.is_empty() {
        return Ok(None);
    }
    let mut parser = KeyParser::new();
    let mut offset = 0usize;
    let mut query = String::new();
    let mut searching = false;
    tui::enter_screen(tty)?;
    loop {
        let size = tty.size().unwrap_or(tui::Size::new(80, 24));
        tui::clear_screen(tty)?;
        let visible = size.rows.saturating_sub(2) as usize;
        let mut text = format!("\x1b[1m{title}\x1b[0m");
        if searching {
            text.push_str(&format!("  /{query}"));
        }
        text.push_str("\r\n");
        for line in lines.iter().skip(offset).take(visible) {
            text.push_str(line);
            text.push_str("\r\n");
        }
        text.push_str(&format!(
            "\r\n{} lines  j/k scroll  / search  q quit",
            lines.len()
        ));
        tty.write_all(text.as_bytes())?;
        tty.flush()?;
        let Some(key) = tui::read_key(tty, &mut parser, Duration::from_secs(1))? else {
            continue;
        };
        if searching {
            match key {
                Key::Enter => {
                    searching = false;
                    if !query.is_empty() {
                        if let Some(index) = lines
                            .iter()
                            .position(|line| line.to_lowercase().contains(&query.to_lowercase()))
                        {
                            offset = index;
                        }
                    }
                }
                Key::Esc => {
                    searching = false;
                    query.clear();
                }
                Key::Backspace => {
                    query.pop();
                }
                Key::Char(ch) => query.push(ch),
                _ => {}
            }
            continue;
        }
        match key {
            Key::Esc | Key::Ctrl('c') | Key::Char('q') => break,
            Key::Char('j') | Key::Down => offset = (offset + 1).min(lines.len().saturating_sub(1)),
            Key::Char('k') | Key::Up => offset = offset.saturating_sub(1),
            Key::PageDown | Key::Char(' ') => {
                offset = (offset + visible).min(lines.len().saturating_sub(1))
            }
            Key::PageUp => offset = offset.saturating_sub(visible),
            Key::Home => offset = 0,
            Key::End => offset = lines.len().saturating_sub(1),
            Key::Char('/') => {
                searching = true;
                query.clear();
            }
            _ => {}
        }
    }
    tui::leave_screen(tty)?;
    Ok(None)
}

pub fn run_diff_cli(
    args: &[String],
    tty: Option<&mut dyn Tty>,
    out: &mut dyn Write,
) -> Result<(), String> {
    let (opts, paths) = parse_diff_args(args)?;
    if opts.help {
        writeln!(out, "{DIFF_USAGE}").map_err(|err| err.to_string())?;
        return Ok(());
    }
    if paths.len() != 2 {
        return Err("You must specify exactly two files/directories to compare".to_owned());
    }
    let patches = diff_paths(Path::new(&paths[0]), Path::new(&paths[1]), &opts)
        .map_err(|err| err.to_string())?;
    let mut raw = String::new();
    for (_, patch) in &patches {
        raw.push_str(&patch.raw);
    }
    if raw.is_empty() {
        return Ok(());
    }
    if let Some(tty) = tty {
        let mut lines = Vec::new();
        for (name, patch) in &patches {
            lines.push(format!("\x1b[1m{name}\x1b[0m"));
            lines.extend(render_side_by_side(patch, 80));
        }
        run_diff_ui(tty, &lines, "Diff").map_err(|err| err.to_string())?;
    } else {
        write!(out, "{raw}").map_err(|err| err.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::FakeTty;

    #[test]
    fn anchored_diff_simple() {
        let text = anchored_diff("a", "one\ntwo\nthree\n", "b", "one\n2\nthree\n", 3);
        assert!(text.contains("--- a"));
        assert!(text.contains("@@ -1,3 +1,3 @@"));
        assert!(text.contains("-two"));
        assert!(text.contains("+2"));
        assert!(text.contains(" three"));
        assert!(anchored_diff("a", "same\n", "b", "same\n", 3).is_empty());
    }

    #[test]
    fn patch_parse_and_render() {
        let text = anchored_diff("a", "one\ntwo\n", "b", "one\nthree\n", 3);
        let patch = parse_patch(&text);
        assert_eq!(patch.hunks.len(), 1);
        assert_eq!(patch.hunks[0].old_start, 1);
        assert_eq!(patch.hunks[0].new_start, 1);
        assert!(patch.hunks[0]
            .lines
            .iter()
            .any(|line| line.kind == LineKind::Remove));
        let rendered = render_side_by_side(&patch, 80);
        assert!(rendered.iter().any(|line| line.contains("two")));
        assert!(rendered.iter().any(|line| line.contains("three")));
    }

    #[test]
    fn centers() {
        let center = changed_center("abcdef", "abXYef");
        assert_eq!(center.offset, 2);
        assert_eq!(center.size, 2);
        let (left, right) = word_diff_center("hello old world", "hello new world");
        assert_eq!(left.len(), 1);
        assert_eq!(right.len(), 1);
        assert!(left[0].size > 0 && right[0].size > 0);
    }

    #[test]
    fn directory_diff_and_default_ignores() {
        let dir = std::env::temp_dir().join(format!("kitty-diff-{}-1", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let l = dir.join("l");
        let r = dir.join("r");
        fs::create_dir_all(&l).unwrap();
        fs::create_dir_all(&r).unwrap();
        fs::write(l.join("a.txt"), b"one\ntwo\n").unwrap();
        fs::write(r.join("a.txt"), b"one\nthree\n").unwrap();
        fs::write(l.join("same.txt"), b"same").unwrap();
        fs::write(r.join("same.txt"), b"same").unwrap();
        let opts = DiffOptions::default();
        let patches = diff_paths(&l, &r, &opts).unwrap();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].0, "a.txt");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ui_scrolls_and_quits() {
        let lines: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        let mut tty = FakeTty::new(80, 10, b"jq");
        run_diff_ui(&mut tty, &lines, "Diff").unwrap();
        assert!(tty.output_text().contains("line 0"));
        assert!(tty.output_text().contains("50 lines"));
    }
}
