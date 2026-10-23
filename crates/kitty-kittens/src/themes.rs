// SPDX-License-Identifier: GPL-3.0-or-later
//! Theme kitten engine, ported from `kittens/themes` and `tools/themes`.
//!
//! Theme files are ordinary kitty `.conf` files. Metadata lives in the leading
//! `## ...` comment block; colors are the normal `key value` settings. The
//! collection can be built from the user theme directory and from an explicit
//! cache directory. ZIP cache archives are not read (no ZIP dependency), and
//! downloading the upstream repository is intentionally left to the caller.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::tui::{self, Key, KeyParser, ListState, Size, Tty};

pub const THEMES_USAGE: &str = "\
Manage kitty color schemes easily.
Usage: kitten themes [theme name to switch to]

Options:
      --cache-age DAYS          Only refresh themes after this many days
      --reload-in none|parent|all
      --dump-theme              Dump the theme's config to STDOUT
      --config-file-name NAME   Config file to edit (default kitty.conf)
  -h, --help";

/// The color settings recognized by kitty. Used for patching the config file.
pub const ALL_COLOR_SETTING_NAMES: &[&str] = &[
    "foreground",
    "background",
    "selection_foreground",
    "selection_background",
    "cursor",
    "cursor_text_color",
    "url_color",
    "active_border_color",
    "inactive_border_color",
    "bell_border_color",
    "active_tab_foreground",
    "active_tab_background",
    "inactive_tab_foreground",
    "inactive_tab_background",
    "tab_bar_background",
    "marked_text_foreground",
    "marked_text_background",
    "mark1_foreground",
    "mark1_background",
    "mark2_foreground",
    "mark2_background",
    "mark3_foreground",
    "mark3_background",
    "visual_bell_color",
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThemeMetadata {
    pub name: String,
    pub filepath: String,
    pub is_dark: bool,
    pub num_settings: usize,
    pub blurb: String,
    pub license: String,
    pub upstream: String,
    pub author: String,
}

#[derive(Clone, Debug, Default)]
pub struct Theme {
    pub metadata: ThemeMetadata,
    pub settings: BTreeMap<String, String>,
    pub code: String,
    pub path: Option<PathBuf>,
    pub user_defined: bool,
}

impl Theme {
    pub fn name(&self) -> &str {
        &self.metadata.name
    }

    pub fn author(&self) -> &str {
        &self.metadata.author
    }

    pub fn blurb(&self) -> &str {
        &self.metadata.blurb
    }

    pub fn is_dark(&self) -> bool {
        self.metadata.is_dark
    }

    pub fn is_user_defined(&self) -> bool {
        self.user_defined
    }

    pub fn save_in_dir(&self, dir: &Path) -> io::Result<PathBuf> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.conf", self.name()));
        fs::write(&path, self.code.as_bytes())?;
        Ok(path)
    }

    pub fn save_in_file(&self, dir: &Path, file_name: &str) -> io::Result<PathBuf> {
        fs::create_dir_all(dir)?;
        let path = dir.join(file_name);
        fs::write(&path, self.code.as_bytes())?;
        Ok(path)
    }

    /// Write `current-theme.conf` and replace include/color settings in the
    /// selected config file inside a `# KITTY_THEME` block.
    pub fn save_in_conf(&self, dir: &Path, config_file_name: &str) -> io::Result<PathBuf> {
        self.save_in_file(dir, "current-theme.conf")?;
        let path = if Path::new(config_file_name).is_absolute() {
            PathBuf::from(config_file_name)
        } else {
            dir.join(config_file_name)
        };
        let existing = fs::read_to_string(&path).unwrap_or_default();
        let mut out = String::new();
        let mut inside = false;
        let block = format!(
            "# KITTY_THEME\n# {}\ninclude current-theme.conf\n",
            self.name()
        );
        for line in existing.lines() {
            if line.trim() == "# KITTY_THEME" {
                inside = true;
                out.push_str(&block);
                continue;
            }
            if inside {
                let key = line.split_whitespace().next().unwrap_or("");
                if line.starts_with('#')
                    || key == "include"
                    || ALL_COLOR_SETTING_NAMES.contains(&key)
                    || key.starts_with("mark")
                    || key.starts_with("active_")
                    || key.starts_with("inactive_")
                    || key.ends_with("_color")
                {
                    continue;
                }
                inside = false;
            }
            out.push_str(line);
            out.push('\n');
        }
        if !out.contains("# KITTY_THEME") {
            out.push_str(&block);
        }
        fs::create_dir_all(dir)?;
        fs::write(&path, out)?;
        Ok(path)
    }

    pub fn as_escape_codes(&self) -> String {
        color_settings_as_escape_codes(&self.settings)
    }
}

/// Parse a theme file: leading `## key: value` metadata, then settings.
pub fn parse_theme_text(text: &str) -> (ThemeMetadata, BTreeMap<String, String>) {
    let mut metadata = ThemeMetadata {
        is_dark: true,
        ..ThemeMetadata::default()
    };
    let mut settings = BTreeMap::new();
    let mut in_metadata = false;
    let mut in_blurb = false;
    let mut finished = false;
    for raw in text.lines() {
        let line = raw.trim_end();
        let is_block = line.starts_with("## ");
        if in_metadata && !is_block {
            finished = true;
        }
        if !finished {
            if !in_metadata && is_block {
                in_metadata = true;
            }
            if in_metadata {
                let body = line[3..].trim();
                if in_blurb {
                    metadata.blurb.push(' ');
                    metadata.blurb.push_str(body);
                } else if let Some((key, value)) = body.split_once(':') {
                    let key = key.trim().to_ascii_lowercase();
                    let value = value.trim().to_owned();
                    match key.as_str() {
                        "name" => {
                            if value
                                != "The name of the theme (if not present, derived from filename)"
                            {
                                metadata.name = value;
                            }
                        }
                        "author" => metadata.author = value,
                        "upstream" => metadata.upstream = value,
                        "license" => metadata.license = value,
                        "blurb" => {
                            metadata.blurb = value;
                            in_blurb = true;
                        }
                        _ => {}
                    }
                }
            }
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or("");
        let value = parts.next().unwrap_or("").trim();
        if key.is_empty() {
            continue;
        }
        settings.insert(key.to_owned(), value.to_owned());
        if key == "background" {
            metadata.is_dark = parse_color(value)
                .map(|(r, g, b)| r.max(g).max(b) < 115)
                .unwrap_or(true);
        }
    }
    metadata.num_settings = settings.len();
    (metadata, settings)
}

pub fn theme_name_from_file_name(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_owned());
    let spaced = stem.replace('_', " ");
    let mut out = String::new();
    let chars: Vec<char> = spaced.chars().collect();
    for (i, ch) in chars.iter().enumerate() {
        if i > 0 && ch.is_uppercase() && chars[i - 1].is_lowercase() {
            out.push(' ');
        }
        out.push(*ch);
    }
    out.split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn theme_from_file(path: &Path) -> io::Result<Theme> {
    let code = fs::read_to_string(path)?;
    let (mut metadata, settings) = parse_theme_text(&code);
    if metadata.name.is_empty() {
        metadata.name = theme_name_from_file_name(&path.to_string_lossy());
    }
    metadata.filepath = path.to_string_lossy().into_owned();
    Ok(Theme {
        metadata,
        settings,
        code,
        path: Some(path.to_path_buf()),
        user_defined: true,
    })
}

#[derive(Clone, Debug, Default)]
pub struct Themes {
    themes: Vec<Theme>,
    names: Vec<String>,
}

impl Themes {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, theme: Theme) {
        if let Some(existing) = self
            .themes
            .iter_mut()
            .find(|item| item.name() == theme.name())
        {
            *existing = theme;
        } else {
            self.themes.push(theme);
        }
        self.reindex();
    }

    pub fn add_from_file(&mut self, path: &Path) -> io::Result<&Theme> {
        let theme = theme_from_file(path)?;
        self.add(theme);
        Ok(self
            .themes
            .iter()
            .find(|item| item.path.as_deref() == Some(path))
            .expect("just added"))
    }

    pub fn add_from_dir(&mut self, dir: &Path) -> io::Result<()> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "conf") {
                let _ = self.add_from_file(&path);
            }
        }
        Ok(())
    }

    fn reindex(&mut self) {
        self.names = self
            .themes
            .iter()
            .map(|theme| theme.name().to_owned())
            .collect();
        self.names.sort_by_key(|name| name.to_ascii_lowercase());
    }

    pub fn len(&self) -> usize {
        self.themes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.themes.is_empty()
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn at(&self, index: usize) -> Option<&Theme> {
        self.themes.get(index)
    }

    pub fn theme_by_name(&self, name: &str) -> Option<&Theme> {
        self.themes
            .iter()
            .find(|theme| theme.name() == name)
            .or_else(|| {
                let lower = name.to_ascii_lowercase();
                self.themes
                    .iter()
                    .find(|theme| theme.name().to_ascii_lowercase() == lower)
            })
    }

    pub fn filtered<F: Fn(&Theme) -> bool>(&self, predicate: F) -> Themes {
        let mut out = Themes::new();
        for theme in &self.themes {
            if predicate(theme) {
                out.add(theme.clone());
            }
        }
        out
    }

    /// Subsequence search over names, returned in score order.
    pub fn search(&self, query: &str) -> Vec<&Theme> {
        let mut scored: Vec<(i64, &Theme)> = self
            .themes
            .iter()
            .filter_map(|theme| tui::fuzzy_score(query, theme.name()).map(|s| (s, theme)))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name().cmp(b.1.name())));
        scored.into_iter().map(|(_, theme)| theme).collect()
    }
}

/// Default user theme directory (`$KITTY_CONFIG_DIRECTORY/themes` or
/// `$XDG_CONFIG_HOME/kitty/themes`).
pub fn default_theme_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("KITTY_CONFIG_DIRECTORY") {
        return PathBuf::from(dir).join("themes");
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join("kitty/themes");
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".config/kitty/themes")
}

pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("KITTY_CONFIG_DIRECTORY") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join("kitty");
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".config/kitty")
}

/// Load themes from the user theme directory plus any explicitly supplied
/// directories (in order, later directories override earlier names).
pub fn load_themes(dirs: &[PathBuf]) -> Themes {
    let mut themes = Themes::new();
    for dir in dirs {
        let _ = themes.add_from_dir(dir);
    }
    themes
}

fn parse_color(value: &str) -> Option<(u8, u8, u8)> {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix('#') {
        if hex.len() == 6 {
            return Some((
                u8::from_str_radix(&hex[0..2], 16).ok()?,
                u8::from_str_radix(&hex[2..4], 16).ok()?,
                u8::from_str_radix(&hex[4..6], 16).ok()?,
            ));
        }
        if hex.len() == 3 {
            let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
            return Some((r, g, b));
        }
    }
    if let Some(rest) = value.strip_prefix("rgb:") {
        let parts: Vec<_> = rest.split('/').collect();
        if parts.len() == 3 {
            let scale = |part: &str| -> Option<u8> {
                let n = u16::from_str_radix(part, 16).ok()?;
                let max = (16u32.pow(part.len() as u32) - 1).max(1);
                Some(((n as u32 * 255) / max) as u8)
            };
            return Some((scale(parts[0])?, scale(parts[1])?, scale(parts[2])?));
        }
    }
    let named = match value.to_ascii_lowercase().as_str() {
        "black" => (0, 0, 0),
        "white" => (255, 255, 255),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "blue" => (0, 0, 255),
        "yellow" => (255, 255, 0),
        "cyan" => (0, 255, 255),
        "magenta" => (255, 0, 255),
        "gray" | "grey" => (128, 128, 128),
        _ => return None,
    };
    Some(named)
}

/// Encode theme colors as OSC 4/10/11/12/17/19 sequences. Indexed colors use
/// the settings when present, otherwise the standard xterm table.
pub fn color_settings_as_escape_codes(settings: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let default_color = |name: &str, fallback: &str| -> Option<String> {
        settings
            .get(name)
            .map(String::as_str)
            .or(Some(fallback))
            .and_then(parse_color)
            .map(|(r, g, b)| format!("#{r:02x}{g:02x}{b:02x}"))
    };
    for (code, name, fallback) in [
        ("10", "foreground", "#ffffff"),
        ("11", "background", "#000000"),
        ("12", "cursor", "#ffffff"),
        ("17", "selection_background", "#444444"),
        ("19", "selection_foreground", "#ffffff"),
    ] {
        if let Some(color) = default_color(name, fallback) {
            out.push_str(&format!("\x1b]{code};{color}\x1b\\"));
        }
    }
    out.push_str("\x1b]4");
    for index in 0..256u16 {
        if let Some(value) = settings.get(&format!("color{index}")) {
            if let Some((r, g, b)) = parse_color(value) {
                out.push_str(&format!(";{index};#{r:02x}{g:02x}{b:02x}"));
            }
        }
    }
    out.push_str("\x1b\\");
    out
}

/// Parse theme-kitten options and a positional theme name.
#[derive(Clone, Debug)]
pub struct ThemesOptions {
    pub cache_age_days: f64,
    pub reload_in: String,
    pub dump_theme: bool,
    pub config_file_name: String,
    pub name: Option<String>,
    pub help: bool,
}

impl Default for ThemesOptions {
    fn default() -> Self {
        Self {
            cache_age_days: 1.0,
            reload_in: "parent".to_owned(),
            dump_theme: false,
            config_file_name: "kitty.conf".to_owned(),
            name: None,
            help: false,
        }
    }
}

pub fn parse_themes_args(args: &[String]) -> ThemesOptions {
    let mut opts = ThemesOptions::default();
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
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
            "--cache-age" => opts.cache_age_days = take().parse().unwrap_or(1.0),
            "--reload-in" => opts.reload_in = take(),
            "--dump-theme" => opts.dump_theme = true,
            "--config-file-name" => opts.config_file_name = take(),
            other if !other.starts_with('-') => opts.name = Some(other.to_owned()),
            _ => {}
        }
        index += 1;
    }
    opts
}

/// Run the non-interactive theme selection. Returns the text to print.
pub fn run_theme_selection(
    opts: &ThemesOptions,
    config_dir: &Path,
    extra_dirs: &[PathBuf],
    themes: &Themes,
) -> Result<String, ThemesError> {
    let name = opts
        .name
        .as_ref()
        .ok_or_else(|| ThemesError::Usage("No theme specified".to_owned()))?;
    let theme = themes
        .theme_by_name(name)
        .or_else(|| themes.theme_by_name(&name.replace('\\', "")))
        .ok_or_else(|| ThemesError::NotFound(name.clone()))?;
    if opts.dump_theme {
        return Ok(theme.code.clone());
    }
    let _ = theme.save_in_conf(config_dir, &opts.config_file_name)?;
    let _ = extra_dirs;
    Ok(String::new())
}

/// Minimal full-screen theme picker. Keys: up/down, printable search, enter
/// accept, `p` save theme file, `q`/Esc quit.
pub fn run_theme_ui(
    tty: &mut dyn Tty,
    themes: &Themes,
    config_dir: &Path,
    opts: &ThemesOptions,
) -> io::Result<Option<String>> {
    let mut list = ListState::new(themes.names().to_vec());
    let mut parser = KeyParser::new();
    tui::enter_screen(tty)?;
    loop {
        let size = tty.size().unwrap_or(Size::new(80, 24));
        tui::clear_screen(tty)?;
        let mut output = String::new();
        output.push_str("\x1b[1mChoose a theme\x1b[0m  /");
        output.push_str(&list.query);
        output.push_str("\r\n");
        let rows = size.rows.saturating_sub(2) as usize;
        for (row, index) in list
            .filtered
            .iter()
            .skip(list.offset)
            .take(rows)
            .enumerate()
        {
            let theme = themes.at(*index).expect("index from list");
            let selected = list.offset + row == list.selected;
            let marker = if selected { ">" } else { " " };
            output.push_str(&format!(
                "{} {} [{}]\r\n",
                marker,
                theme.name(),
                if theme.is_dark() { "dark" } else { "light" }
            ));
        }
        output.push_str(&format!(
            "\r\n{} themes  Enter: use  P: place file  Q: quit",
            themes.len()
        ));
        tty.write_all(output.as_bytes())?;
        tty.flush()?;
        let Some(key) = tui::read_key(tty, &mut parser, Duration::from_secs(1))? else {
            continue;
        };
        if list.handle_edit_key(&key) {
            continue;
        }
        match key {
            Key::Enter => {
                if let Some(name) = list.current() {
                    let name = name.to_owned();
                    tui::leave_screen(tty)?;
                    return Ok(Some(name));
                }
            }
            Key::Char('p') => {
                if let Some(name) = list.current() {
                    if let Some(theme) = themes.theme_by_name(name) {
                        let _ = theme.save_in_dir(&config_dir.join("themes"));
                    }
                }
            }
            Key::Esc | Key::Ctrl('c') | Key::Char('q') => {
                tui::leave_screen(tty)?;
                return Ok(None);
            }
            Key::Char('j') | Key::Down => list.move_by(1),
            Key::Char('k') | Key::Up => list.move_by(-1),
            _ => {}
        }
        let _ = opts;
    }
}

#[derive(Debug)]
pub enum ThemesError {
    Io(io::Error),
    Usage(String),
    NotFound(String),
}

impl std::fmt::Display for ThemesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Usage(msg) => f.write_str(msg),
            Self::NotFound(name) => write!(f, "No theme named: {name}"),
        }
    }
}

impl std::error::Error for ThemesError {}

impl From<io::Error> for ThemesError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Run the CLI. `interactive` is used when no theme name was supplied.
pub fn run_themes_cli(
    args: &[String],
    tty: Option<&mut dyn Tty>,
    out: &mut dyn Write,
) -> Result<(), ThemesError> {
    let opts = parse_themes_args(args);
    if opts.help {
        writeln!(out, "{THEMES_USAGE}")?;
        return Ok(());
    }
    let mut dirs = vec![default_theme_dir()];
    if let Some(extra) = std::env::var_os("KITTY_THEMES_DIR") {
        dirs.insert(0, PathBuf::from(extra));
    }
    let themes = load_themes(&dirs);
    let dir = config_dir();
    if opts.name.is_some() {
        let text = run_theme_selection(&opts, &dir, &dirs, &themes)?;
        if !text.is_empty() {
            write!(out, "{text}")?;
            if !text.ends_with('\n') {
                writeln!(out)?;
            }
        }
        return Ok(());
    }
    let Some(tty) = tty else {
        return Err(ThemesError::Usage(
            "No theme name specified and no terminal available".to_owned(),
        ));
    };
    if let Some(name) = run_theme_ui(tty, &themes, &dir, &opts)? {
        let selected = ThemesOptions {
            name: Some(name),
            ..opts
        };
        run_theme_selection(&selected, &dir, &dirs, &themes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::FakeTty;

    const SAMPLE: &str = "## Name: Test Theme\n## Author: A. Person\n## Blurb: A nice theme\n## License: MIT\n\nbackground #101010\nforeground #f0f0f0\ncolor1 #ff0000\ncursor #ffffff\n";

    #[test]
    fn parses_metadata_and_settings() {
        let (metadata, settings) = parse_theme_text(SAMPLE);
        assert_eq!(metadata.name, "Test Theme");
        assert_eq!(metadata.author, "A. Person");
        assert_eq!(metadata.blurb, "A nice theme License: MIT");
        assert!(metadata.is_dark);
        assert_eq!(settings["color1"], "#ff0000");
        assert_eq!(metadata.num_settings, 4);
    }

    #[test]
    fn collection_search_and_save() {
        let dir = std::env::temp_dir().join(format!("kitty-themes-{}-1", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("Test_Theme.conf"), SAMPLE).unwrap();
        let mut themes = Themes::new();
        themes.add_from_dir(&dir).unwrap();
        assert_eq!(themes.len(), 1);
        let theme = themes.theme_by_name("test theme").unwrap();
        assert_eq!(theme.name(), "Test Theme");
        assert_eq!(themes.search("tt").len(), 1);
        let out = dir.join("out");
        let path = theme.save_in_conf(&out, "kitty.conf").unwrap();
        let text = fs::read_to_string(path).unwrap();
        assert!(text.contains("include current-theme.conf"));
        assert!(fs::read_to_string(out.join("current-theme.conf"))
            .unwrap()
            .contains("background #101010"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn escape_codes_include_defaults_and_indexed_colors() {
        let (_, settings) = parse_theme_text(SAMPLE);
        let codes = color_settings_as_escape_codes(&settings);
        assert!(codes.contains("]10;"));
        assert!(codes.contains("]11;"));
        assert!(codes.contains(";1;#ff0000"));
    }

    #[test]
    fn ui_selects_and_saves() {
        let dir = std::env::temp_dir().join(format!("kitty-themes-{}-2", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("Test.conf"), SAMPLE).unwrap();
        let themes = load_themes(std::slice::from_ref(&dir));
        let mut tty = FakeTty::new(80, 24, b"\r");
        let opts = ThemesOptions::default();
        let selected = run_theme_ui(&mut tty, &themes, &dir, &opts).unwrap();
        assert_eq!(selected.as_deref(), Some("Test Theme"));
        assert!(tty.output_text().contains("Choose a theme"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_name_titles() {
        assert_eq!(
            theme_name_from_file_name("My_CoolTheme.conf"),
            "My Cool Theme"
        );
    }
}
