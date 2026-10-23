// SPDX-License-Identifier: GPL-3.0-or-later
//! Choose-files kitten, ported from `kittens/choose_files`.
//!
//! The engine scans a directory, applies file filters and ignore rules, ranks
//! entries with the same subsequence matcher used by the other pickers, and
//! formats selections for insertion at a shell prompt. The UI is a compact
//! ANSI browser with fuzzy filtering, directory navigation and multi-select.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::tui::{self, Key, KeyParser, ListState, Size, Tty};

pub const CHOOSE_FILES_USAGE: &str = "\
Choose files, fast.
Usage: kitten choose-files [options] [directory to start choosing files in]

Options:
      --mode file|files|all|save-file|dir|save-dir|dirs|save-files
      --file-filter TYPE:PATTERN:NAME
      --suggested-save-file-name NAME
      --suggested-save-file-path PATH
      --title TITLE
      --write-output-to PATH
      --output-format text|json|shell|shell-relative
      --write-pid-to PATH
  -h, --help";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChooseMode {
    #[default]
    File,
    Files,
    All,
    SaveFile,
    Dir,
    SaveDir,
    Dirs,
    SaveFiles,
}

impl ChooseMode {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "file" => Self::File,
            "files" => Self::Files,
            "all" => Self::All,
            "save-file" => Self::SaveFile,
            "dir" => Self::Dir,
            "save-dir" => Self::SaveDir,
            "dirs" => Self::Dirs,
            "save-files" => Self::SaveFiles,
            _ => return None,
        })
    }

    pub fn allows_multiple(self) -> bool {
        matches!(self, Self::Files | Self::Dirs | Self::SaveFiles | Self::All)
    }

    pub fn save_mode(self) -> bool {
        matches!(self, Self::SaveFile | Self::SaveDir | Self::SaveFiles)
    }

    pub fn dirs_only(self) -> bool {
        matches!(self, Self::Dir | Self::SaveDir | Self::Dirs)
    }
}

#[derive(Clone, Debug)]
pub struct FileFilter {
    pub name: String,
    pub kind: String,
    pub pattern: String,
}

impl FileFilter {
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut parts = spec.splitn(3, ':');
        let kind = parts.next().unwrap_or("");
        let pattern = parts.next().unwrap_or("");
        let name = parts.next().unwrap_or("");
        if kind.is_empty() || pattern.is_empty() || name.is_empty() {
            return Err(format!(
                "{spec:?} is not a valid filter specifier, must have two colons"
            ));
        }
        if kind != "glob" && kind != "mime" {
            return Err(format!("{kind:?} is not a valid filter type"));
        }
        Ok(Self {
            name: name.to_owned(),
            kind: kind.to_owned(),
            pattern: pattern.to_owned(),
        })
    }

    pub fn matches(&self, path: &Path) -> bool {
        if self.pattern == "*" || self.pattern.is_empty() {
            return true;
        }
        let text = path
            .file_name()
            .map(|x| x.to_string_lossy().into_owned())
            .unwrap_or_default();
        match self.kind.as_str() {
            "glob" => wildcard(&self.pattern, &text),
            "mime" => {
                let mime = crate::clipboard_mime::guess_mime(path);
                wildcard(&self.pattern, &mime)
            }
            _ => false,
        }
    }
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

#[derive(Clone, Debug)]
pub struct ChooseOptions {
    pub mode: ChooseMode,
    pub filters: Vec<FileFilter>,
    pub suggested_save_name: String,
    pub suggested_save_path: String,
    pub title: String,
    pub display_title: bool,
    pub write_output_to: Option<PathBuf>,
    pub output_format: String,
    pub write_pid_to: Option<PathBuf>,
    pub show_hidden: bool,
    pub sort_by_last_modified: bool,
    pub respect_ignores: bool,
    pub ignore: Vec<String>,
    pub dir: PathBuf,
    pub help: bool,
}

impl Default for ChooseOptions {
    fn default() -> Self {
        Self {
            mode: ChooseMode::File,
            filters: Vec::new(),
            suggested_save_name: String::new(),
            suggested_save_path: String::new(),
            title: String::new(),
            display_title: false,
            write_output_to: None,
            output_format: "text".to_owned(),
            write_pid_to: None,
            show_hidden: false,
            sort_by_last_modified: false,
            respect_ignores: true,
            ignore: Vec::new(),
            dir: PathBuf::from("."),
            help: false,
        }
    }
}

pub fn parse_choose_files_args(args: &[String]) -> Result<ChooseOptions, String> {
    let mut opts = ChooseOptions::default();
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            if let Some(dir) = args.get(index + 1) {
                opts.dir = PathBuf::from(dir);
            }
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
            "--mode" => {
                let value = take();
                opts.mode =
                    ChooseMode::parse(&value).ok_or_else(|| format!("invalid mode: {value}"))?
            }
            "--file-filter" => opts.filters.push(FileFilter::parse(&take())?),
            "--suggested-save-file-name" => opts.suggested_save_name = take(),
            "--suggested-save-file-path" => opts.suggested_save_path = take(),
            "--title" => opts.title = take(),
            "--display-title" => opts.display_title = true,
            "--write-output-to" => opts.write_output_to = Some(PathBuf::from(take())),
            "--output-format" => opts.output_format = take(),
            "--write-pid-to" => opts.write_pid_to = Some(PathBuf::from(take())),
            "--clear-cache" => {}
            "--config" | "--override" | "-o" | "-c" => {
                let _ = take();
            }
            other if !other.starts_with('-') => opts.dir = PathBuf::from(other),
            other => return Err(format!("unknown option: {other}")),
        }
        index += 1;
    }
    Ok(opts)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub modified: Option<SystemTime>,
}

impl Entry {
    pub fn display(&self) -> String {
        if self.is_dir {
            format!("{}/", self.name)
        } else {
            self.name.clone()
        }
    }
}

fn load_ignore_patterns(dir: &Path) -> Vec<String> {
    let mut patterns = Vec::new();
    for name in [".gitignore", ".ignore"] {
        if let Ok(text) = fs::read_to_string(dir.join(name)) {
            for line in text.lines() {
                let line = line.trim();
                if !line.is_empty() && !line.starts_with('#') {
                    patterns.push(line.trim_start_matches('/').to_owned());
                }
            }
        }
    }
    patterns
}

/// List visible entries in a directory, applying hidden/ignore rules and the
/// active file filter (directories are always shown).
pub fn scan_directory(dir: &Path, opts: &ChooseOptions) -> io::Result<Vec<Entry>> {
    let ignore = if opts.respect_ignores {
        let mut patterns = load_ignore_patterns(dir);
        patterns.extend(opts.ignore.iter().cloned());
        patterns
    } else {
        opts.ignore.clone()
    };
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !opts.show_hidden && name.starts_with('.') {
            continue;
        }
        if ignore
            .iter()
            .any(|pattern| wildcard(pattern, &name) || wildcard(pattern, &format!("{name}/")))
        {
            continue;
        }
        let path = entry.path();
        let metadata = entry.metadata()?;
        let is_dir = metadata.is_dir();
        if !is_dir && !opts.filters.is_empty() {
            let allowed = opts.filters.iter().any(|filter| filter.matches(&path));
            if !allowed {
                continue;
            }
        }
        entries.push(Entry {
            name,
            path,
            is_dir,
            modified: metadata.modified().ok(),
        });
    }
    if opts.sort_by_last_modified {
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.modified));
    } else {
        entries.sort_by_key(|entry| entry.name.to_ascii_lowercase());
    }
    Ok(entries)
}

pub fn filter_entries(entries: &[Entry], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..entries.len()).collect();
    }
    let mut scored: Vec<(i64, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            tui::fuzzy_score(query, &entry.display()).map(|score| (score, index))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, index)| index).collect()
}

fn relative_path(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn shlex_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "_-./:@%+".contains(ch))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Format selected paths for insertion at a shell prompt or a plain list.
pub fn format_selection_for_paste(
    paths: &[PathBuf],
    cwd: Option<&Path>,
    at_prompt: bool,
) -> String {
    let separator = if at_prompt { " " } else { "\n" };
    paths
        .iter()
        .map(|path| {
            let mut text = match cwd {
                Some(base) => relative_path(path, base),
                None => path.to_string_lossy().into_owned(),
            };
            if at_prompt {
                text = shlex_quote(&text);
            }
            text
        })
        .collect::<Vec<_>>()
        .join(separator)
}

/// Format a result according to `--output-format`.
pub fn format_output(paths: &[PathBuf], format: &str, start_dir: &Path) -> String {
    match format {
        "json" => {
            let items = paths
                .iter()
                .map(|path| format!("\"{}\"", escape_json(&path.to_string_lossy())))
                .collect::<Vec<_>>()
                .join(",");
            format!("[{items}]\n")
        }
        "shell" => format!(
            "{}\n",
            paths
                .iter()
                .map(|path| shlex_quote(&path.to_string_lossy()))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        "shell-relative" => format!(
            "{}\n",
            paths
                .iter()
                .map(|path| shlex_quote(&relative_path(path, start_dir)))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        _ => {
            if paths.is_empty() {
                String::new()
            } else {
                format!(
                    "{}\n",
                    paths
                        .iter()
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            }
        }
    }
}

fn escape_json(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Resolve a save-file name/path. Returns `None` when the user cancelled.
pub fn save_path(
    dir: &Path,
    typed_name: &str,
    suggested_path: &str,
    suggested_name: &str,
) -> Option<PathBuf> {
    if !suggested_path.is_empty() {
        return Some(PathBuf::from(suggested_path));
    }
    let name = if typed_name.is_empty() {
        suggested_name
    } else {
        typed_name
    };
    if name.is_empty() {
        return None;
    }
    let path = PathBuf::from(name);
    Some(if path.is_absolute() {
        path
    } else {
        dir.join(path)
    })
}

/// Interactive chooser. Returns paths selected by the user.
pub fn run_choose_files_ui(tty: &mut dyn Tty, opts: &ChooseOptions) -> io::Result<Vec<PathBuf>> {
    let mut current = if opts.dir.is_absolute() {
        opts.dir.clone()
    } else {
        std::env::current_dir()?.join(&opts.dir)
    };
    let mut list = ListState::default();
    let mut entries: Vec<Entry> = Vec::new();
    let mut selected: Vec<PathBuf> = Vec::new();
    let mut query = String::new();
    let mut typed_name = opts.suggested_save_name.clone();
    let mut parser = KeyParser::new();
    let mut reload = true;
    tui::enter_screen(tty)?;
    loop {
        if reload {
            entries = scan_directory(&current, opts)?;
            list = ListState {
                items: entries.iter().map(Entry::display).collect(),
                ..ListState::default()
            };
            list.query = query.clone();
            list.refilter();
            reload = false;
        }
        let size = tty.size().unwrap_or(Size::new(80, 24));
        tui::clear_screen(tty)?;
        let title = if opts.title.is_empty() {
            current.to_string_lossy().into_owned()
        } else {
            opts.title.clone()
        };
        let mut text = format!("\x1b[1m{title}\x1b[0m  /{query}\r\n");
        let rows = size.rows.saturating_sub(3) as usize;
        for (row, index) in list
            .filtered
            .iter()
            .skip(list.offset)
            .take(rows)
            .enumerate()
        {
            let entry = &entries[*index];
            let marker = if list.offset + row == list.selected {
                ">"
            } else if selected.contains(&entry.path) {
                "*"
            } else {
                " "
            };
            text.push_str(&format!("{marker} {}\r\n", entry.display()));
        }
        if opts.mode.save_mode() && !typed_name.is_empty() {
            text.push_str(&format!("Name: {typed_name}\r\n"));
        }
        text.push_str(&format!(
            "\r\n{} matches  Enter: accept  Tab: enter dir  S-Tab: parent  Space: select  Q: quit",
            list.filtered.len()
        ));
        tty.write_all(text.as_bytes())?;
        tty.flush()?;
        let Some(key) = tui::read_key(tty, &mut parser, Duration::from_millis(500))? else {
            continue;
        };
        match key {
            Key::Esc | Key::Ctrl('c') | Key::Char('q') => break,
            Key::Char(ch @ ('j' | 'k')) => {
                list.move_by(if ch == 'j' { 1 } else { -1 });
            }
            Key::Down => list.move_by(1),
            Key::Up => list.move_by(-1),
            Key::Char(' ') => {
                if let Some(index) = list.filtered.get(list.selected) {
                    let path = entries[*index].path.clone();
                    if let Some(position) = selected.iter().position(|item| item == &path) {
                        selected.remove(position);
                    } else if opts.mode.allows_multiple() {
                        selected.push(path);
                    } else {
                        selected = vec![path];
                    }
                }
            }
            Key::Tab => {
                if let Some(index) = list.filtered.get(list.selected) {
                    let entry = &entries[*index];
                    if entry.is_dir {
                        current = entry.path.clone();
                        query.clear();
                        reload = true;
                    }
                }
            }
            Key::BackTab => {
                if let Some(parent) = current.parent() {
                    current = parent.to_path_buf();
                    query.clear();
                    reload = true;
                }
            }
            Key::Enter => {
                if opts.mode.save_mode() {
                    if let Some(path) = save_path(
                        &current,
                        &typed_name,
                        &opts.suggested_save_path,
                        &opts.suggested_save_name,
                    ) {
                        selected.push(path);
                        break;
                    }
                } else if let Some(index) = list.filtered.get(list.selected) {
                    let entry = &entries[*index];
                    if entry.is_dir && !opts.mode.dirs_only() {
                        current = entry.path.clone();
                        query.clear();
                        reload = true;
                    } else if opts.mode.dirs_only() && !entry.is_dir {
                        continue;
                    } else {
                        selected.push(entry.path.clone());
                        if !opts.mode.allows_multiple() {
                            break;
                        }
                    }
                }
            }
            Key::Backspace => {
                if !query.is_empty() {
                    query.pop();
                } else if opts.mode.save_mode() {
                    typed_name.pop();
                }
                list.query = query.clone();
                list.refilter();
            }
            Key::Char(ch) => {
                if opts.mode.save_mode() && (query.is_empty() || typed_name.is_empty()) {
                    typed_name.push(ch);
                }
                query.push(ch);
                list.query = query.clone();
                list.refilter();
            }
            _ => {}
        }
    }
    tui::leave_screen(tty)?;
    Ok(selected)
}

pub fn run_choose_files_cli(
    args: &[String],
    tty: Option<&mut dyn Tty>,
    out: &mut dyn Write,
) -> Result<(), String> {
    let opts = parse_choose_files_args(args)?;
    if opts.help {
        writeln!(out, "{CHOOSE_FILES_USAGE}").map_err(|err| err.to_string())?;
        return Ok(());
    }
    if let Some(pid_path) = &opts.write_pid_to {
        let _ = fs::write(pid_path, std::process::id().to_string());
    }
    let Some(tty) = tty else {
        return Err("No terminal available for the file chooser".to_owned());
    };
    let start = if opts.dir.is_absolute() {
        opts.dir.clone()
    } else {
        std::env::current_dir()
            .map_err(|err| err.to_string())?
            .join(&opts.dir)
    };
    let paths = run_choose_files_ui(tty, &opts).map_err(|err| err.to_string())?;
    let text = format_output(&paths, &opts.output_format, &start);
    if let Some(path) = &opts.write_output_to {
        fs::write(path, &text).map_err(|err| err.to_string())?;
    }
    write!(out, "{text}").map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::FakeTty;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kitty-choose-{}-{}-{}",
            std::process::id(),
            label,
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn filters_parse_and_match() {
        let filter = FileFilter::parse("glob:*.txt:Text").unwrap();
        assert!(filter.matches(Path::new("a.txt")));
        assert!(!filter.matches(Path::new("a.png")));
        assert!(FileFilter::parse("mime:image/*:Images")
            .unwrap()
            .matches(Path::new("a.png")));
        assert!(FileFilter::parse("bad").is_err());
    }

    #[test]
    fn scans_sorts_and_filters_hidden() {
        let dir = temp_dir("scan");
        fs::write(dir.join("b.txt"), b"b").unwrap();
        fs::write(dir.join("a.txt"), b"a").unwrap();
        fs::write(dir.join(".hidden"), b"h").unwrap();
        fs::create_dir_all(dir.join("sub")).unwrap();
        let opts = ChooseOptions {
            dir: dir.clone(),
            filters: vec![FileFilter::parse("glob:*.txt:Text").unwrap()],
            ..ChooseOptions::default()
        };
        let entries = scan_directory(&dir, &opts).unwrap();
        let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "sub"]);
        let filtered = filter_entries(&entries, "at");
        assert_eq!(filtered.len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn formats_selection_and_output() {
        let cwd = Path::new("/tmp");
        let paths = vec![PathBuf::from("/tmp/a b"), PathBuf::from("/tmp/c")];
        assert_eq!(
            format_selection_for_paste(&paths, Some(cwd), false),
            "a b\nc"
        );
        assert_eq!(
            format_selection_for_paste(&paths, Some(cwd), true),
            "'a b' c"
        );
        assert_eq!(
            format_output(&paths, "json", cwd),
            "[\"/tmp/a b\",\"/tmp/c\"]\n"
        );
        assert_eq!(format_output(&paths, "shell-relative", cwd), "'a b' c\n");
    }

    #[test]
    fn save_paths() {
        let dir = Path::new("/tmp");
        assert_eq!(
            save_path(dir, "new.txt", "", ""),
            Some(PathBuf::from("/tmp/new.txt"))
        );
        assert_eq!(save_path(dir, "", "/x/y", ""), Some(PathBuf::from("/x/y")));
        assert_eq!(
            save_path(dir, "", "", "suggested"),
            Some(PathBuf::from("/tmp/suggested"))
        );
    }

    #[test]
    fn ui_selects_files_and_quits() {
        let dir = temp_dir("ui");
        fs::write(dir.join("a.txt"), b"a").unwrap();
        fs::write(dir.join("b.txt"), b"b").unwrap();
        let opts = ChooseOptions {
            dir: dir.clone(),
            mode: ChooseMode::Files,
            ..ChooseOptions::default()
        };
        let mut tty = FakeTty::new(80, 24, b" q");
        let paths = run_choose_files_ui(&mut tty, &opts).unwrap();
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("a.txt"));
        assert!(tty.output_text().contains("a.txt"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn cli_args() {
        let args = ["--mode=files", "--file-filter=glob:*.rs:Rust", "/tmp"]
            .iter()
            .map(|x| (*x).to_owned())
            .collect::<Vec<_>>();
        let opts = parse_choose_files_args(&args).unwrap();
        assert_eq!(opts.mode, ChooseMode::Files);
        assert_eq!(opts.filters.len(), 1);
        assert_eq!(opts.dir, PathBuf::from("/tmp"));
    }
}
