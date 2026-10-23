// SPDX-License-Identifier: GPL-3.0-or-later
//! Choose-fonts kitten, ported from `kittens/choose_fonts`.
//!
//! Implements kitty font-spec parsing/serialization, family grouping from
//! Fontconfig (`fc-list`), face settings and config-file updates, plus a
//! compact family picker. Variable-font axis UI and bitmap previews are not
//! ported.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::tui::{self, Key, KeyParser, ListState, Size, Tty};

pub const CHOOSE_FONTS_USAGE: &str = "\
Usage: kitten choose-fonts [options]

Choose the fonts used in kitty.

Options:
      --reload-in parent|all|none
      --config-file-name NAME
  -h, --help";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListedFont {
    pub family: String,
    pub style: String,
    pub full_name: String,
    pub postscript_name: String,
    pub is_monospace: bool,
    pub is_variable: bool,
}

/// A kitty font setting (`family=... style=... features=...`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontSpec {
    pub family: Option<String>,
    pub style: Option<String>,
    pub postscript_name: Option<String>,
    pub full_name: Option<String>,
    pub variable_name: Option<String>,
    pub system: Option<String>,
    pub axes: BTreeMap<String, String>,
    pub features: Vec<String>,
}

impl FontSpec {
    pub fn from_setting(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(Self::default());
        }
        if !text.contains('=') && !text.contains(' ') {
            return Ok(Self {
                family: Some(text.to_owned()),
                ..Self::default()
            });
        }
        let tokens = split_shell_words(text)?;
        let mut out = Self::default();
        let mut expecting_features = false;
        for token in tokens {
            let Some((key, value)) = token.split_once('=') else {
                if expecting_features {
                    out.features.push(token);
                } else if out.family.is_none() {
                    out.family = Some(token);
                }
                continue;
            };
            expecting_features = key == "features";
            let value = unquote(value);
            match key {
                "family" => out.family = Some(value),
                "style" => out.style = Some(value),
                "postscript_name" => out.postscript_name = Some(value),
                "full_name" => out.full_name = Some(value),
                "variable_name" => out.variable_name = Some(value),
                "features" => out
                    .features
                    .extend(value.split_whitespace().map(str::to_owned)),
                key if key.starts_with("system") => out.system = Some(value),
                key => {
                    out.axes.insert(key.to_owned(), value);
                }
            }
        }
        Ok(out)
    }

    pub fn as_setting(&self) -> String {
        if let Some(system) = &self.system {
            return system.clone();
        }
        let mut parts = Vec::new();
        for (key, value) in [
            ("family", &self.family),
            ("style", &self.style),
            ("postscript_name", &self.postscript_name),
            ("full_name", &self.full_name),
            ("variable_name", &self.variable_name),
        ] {
            if let Some(value) = value {
                parts.push(format!("{key}={}", quote_setting(value)));
            }
        }
        for (key, value) in &self.axes {
            parts.push(format!("{key}={value}"));
        }
        if !self.features.is_empty() {
            parts.push(format!("features={}", self.features.join(" ")));
        }
        parts.join(" ")
    }
}

fn quote_setting(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "_-./:@%+".contains(ch))
    {
        value.to_owned()
    } else {
        format!("\"{}\"", value.replace('"', "\\\""))
    }
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].to_owned()
    } else {
        value.to_owned()
    }
}

fn split_shell_words(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }
    if quote.is_some() {
        return Err("Unterminated quote in font specification".to_owned());
    }
    if !current.is_empty() {
        out.push(current);
    }
    Ok(out)
}

/// Group monospaced fonts by family, retaining all styles.
pub fn group_families(fonts: Vec<ListedFont>) -> BTreeMap<String, Vec<ListedFont>> {
    let mut out: BTreeMap<String, Vec<ListedFont>> = BTreeMap::new();
    for font in fonts {
        if font.family.is_empty() {
            continue;
        }
        out.entry(font.family.clone()).or_default().push(font);
    }
    out
}

/// List monospaced families using Fontconfig's `fc-list`.
pub fn list_system_fonts() -> Vec<ListedFont> {
    let output = Command::new("fc-list")
        .args([
            ":spacing=mono",
            "family",
            "style",
            "fullname",
            "postscriptname",
            "file",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = line.split(':').map(str::trim);
        let family = parts.next().unwrap_or("").to_owned();
        if family.is_empty() {
            continue;
        }
        let mut styles: Vec<&str> = parts.collect();
        let file = styles.pop().unwrap_or("");
        let postscript = styles.pop().unwrap_or("").to_owned();
        let full_name = styles.pop().unwrap_or("").to_owned();
        let style = styles.join(":");
        out.push(ListedFont {
            family,
            style,
            full_name,
            postscript_name: postscript,
            is_monospace: true,
            is_variable: file.contains("[") || file.contains("variable"),
        });
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FacesSettings {
    pub font_family: String,
    pub bold_font: String,
    pub italic_font: String,
    pub bold_italic_font: String,
}

impl FacesSettings {
    pub fn serialized(&self) -> String {
        [
            ("font_family", &self.font_family),
            ("bold_font", &self.bold_font),
            ("italic_font", &self.italic_font),
            ("bold_italic_font", &self.bold_italic_font),
        ]
        .iter()
        .map(|(key, value)| format!("{key}      {value}"))
        .collect::<Vec<_>>()
        .join("\n")
    }

    /// Write/replace the font settings in a config file under a `# KITTY_FONTS`
    /// marker.
    pub fn save_in_conf(&self, dir: &Path, file_name: &str) -> io::Result<PathBuf> {
        let path = if Path::new(file_name).is_absolute() {
            PathBuf::from(file_name)
        } else {
            dir.join(file_name)
        };
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let keys = [
            "font_family",
            "bold_font",
            "italic_font",
            "bold_italic_font",
        ];
        let mut out = String::new();
        let mut inserted = false;
        for line in existing.lines() {
            if line.trim() == "# KITTY_FONTS" {
                if !inserted {
                    out.push_str(&format!("# KITTY_FONTS\n{}\n", self.serialized()));
                    inserted = true;
                }
                continue;
            }
            let key = line.split_whitespace().next().unwrap_or("");
            if keys.contains(&key) {
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        if !inserted {
            out.push_str(&format!("# KITTY_FONTS\n{}\n", self.serialized()));
        }
        std::fs::create_dir_all(dir)?;
        std::fs::write(&path, out)?;
        Ok(path)
    }

    /// Build a face setting from the family plus a style name.
    pub fn for_family(family: &str, style: &str) -> Self {
        let spec = if style.is_empty() {
            format!("family=\"{family}\"")
        } else {
            format!("family=\"{family}\" style=\"{style}\"")
        };
        Self {
            font_family: spec.clone(),
            bold_font: spec.clone(),
            italic_font: spec.clone(),
            bold_italic_font: spec,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ChooseFontsOptions {
    pub reload_in: String,
    pub config_file_name: String,
    pub help: bool,
}

impl Default for ChooseFontsOptions {
    fn default() -> Self {
        Self {
            reload_in: "parent".to_owned(),
            config_file_name: "kitty.conf".to_owned(),
            help: false,
        }
    }
}

pub fn parse_choose_fonts_args(args: &[String]) -> Result<ChooseFontsOptions, String> {
    let mut opts = ChooseFontsOptions::default();
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
            "--reload-in" => {
                let value = take();
                if !matches!(value.as_str(), "parent" | "all" | "none") {
                    return Err(format!("invalid reload-in: {value}"));
                }
                opts.reload_in = value;
            }
            "--config-file-name" => opts.config_file_name = take(),
            other => return Err(format!("unknown option: {other}")),
        }
        index += 1;
    }
    Ok(opts)
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

/// Family picker. Returns the selected family and its chosen style (empty for
/// the family's default face).
pub fn run_choose_fonts_ui(
    tty: &mut dyn Tty,
    families: &BTreeMap<String, Vec<ListedFont>>,
) -> io::Result<Option<(String, String)>> {
    let mut list = ListState::new(families.keys().cloned().collect());
    let mut parser = KeyParser::new();
    tui::enter_screen(tty)?;
    loop {
        let size = tty.size().unwrap_or(Size::new(80, 24));
        tui::clear_screen(tty)?;
        let mut text = format!("\x1b[1mChoose a font for kitty\x1b[0m  /{}\r\n", list.query);
        let rows = size.rows.saturating_sub(2) as usize;
        for (row, index) in list
            .filtered
            .iter()
            .skip(list.offset)
            .take(rows)
            .enumerate()
        {
            let family = &list.items[*index];
            let marker = if list.offset + row == list.selected {
                ">"
            } else {
                " "
            };
            let styles = families
                .get(family)
                .map(|fonts| {
                    fonts
                        .iter()
                        .map(|font| font.style.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            text.push_str(&format!(
                "{} {:<30} {}\r\n",
                marker,
                tui::fit(family, 30),
                tui::fit(&styles, 40)
            ));
        }
        text.push_str("\r\nEnter: choose  Q: quit");
        tty.write_all(text.as_bytes())?;
        tty.flush()?;
        let Some(key) = tui::read_key(tty, &mut parser, Duration::from_millis(500))? else {
            continue;
        };
        if list.handle_edit_key(&key) {
            continue;
        }
        match key {
            Key::Enter => {
                if let Some(family) = list.current() {
                    let family = family.to_owned();
                    tui::leave_screen(tty)?;
                    return Ok(Some((family, String::new())));
                }
            }
            Key::Esc | Key::Ctrl('c') | Key::Char('q') => break,
            _ => {}
        }
    }
    tui::leave_screen(tty)?;
    Ok(None)
}

pub fn run_choose_fonts_cli(
    args: &[String],
    tty: Option<&mut dyn Tty>,
    out: &mut dyn Write,
) -> Result<(), String> {
    let opts = parse_choose_fonts_args(args)?;
    if opts.help {
        writeln!(out, "{CHOOSE_FONTS_USAGE}").map_err(|err| err.to_string())?;
        return Ok(());
    }
    let fonts = list_system_fonts();
    let families = group_families(fonts);
    if families.is_empty() {
        return Err("No monospaced fonts found (fc-list returned nothing)".to_owned());
    }
    let Some(tty) = tty else {
        return Err("The font chooser requires a terminal".to_owned());
    };
    let Some((family, style)) =
        run_choose_fonts_ui(tty, &families).map_err(|err| err.to_string())?
    else {
        return Ok(());
    };
    let settings = FacesSettings::for_family(&family, &style);
    let dir = config_dir();
    settings
        .save_in_conf(&dir, &opts.config_file_name)
        .map_err(|err| err.to_string())?;
    writeln!(out, "{}", settings.serialized()).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::FakeTty;

    #[test]
    fn font_spec_roundtrip() {
        let spec = FontSpec::from_setting("family=\"Fira Code\" style=Medium features=+liga -calt")
            .unwrap();
        assert_eq!(spec.family.as_deref(), Some("Fira Code"));
        assert_eq!(spec.style.as_deref(), Some("Medium"));
        assert_eq!(spec.features, vec!["+liga", "-calt"]);
        let setting = spec.as_setting();
        assert!(setting.contains("family=\"Fira Code\""));
        assert!(setting.contains("features=+liga -calt"));
        let axes = FontSpec::from_setting("family=Recursive wght=700 CASL=0.5").unwrap();
        assert_eq!(axes.axes["wght"], "700");
        assert_eq!(axes.axes["CASL"], "0.5");
        assert_eq!(
            FontSpec::from_setting("Courier").unwrap().family.as_deref(),
            Some("Courier")
        );
    }

    #[test]
    fn grouping() {
        let fonts = vec![
            ListedFont {
                family: "Fira Code".to_owned(),
                style: "Medium".to_owned(),
                ..ListedFont::default()
            },
            ListedFont {
                family: "Fira Code".to_owned(),
                style: "Bold".to_owned(),
                ..ListedFont::default()
            },
            ListedFont {
                family: "Iosevka".to_owned(),
                style: "Regular".to_owned(),
                ..ListedFont::default()
            },
        ];
        let groups = group_families(fonts);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups["Fira Code"].len(), 2);
    }

    #[test]
    fn faces_settings_and_conf() {
        let settings = FacesSettings::for_family("Fira Code", "");
        assert!(settings.font_family.contains("Fira Code"));
        assert_eq!(settings.bold_font, settings.font_family);
        let dir = std::env::temp_dir().join(format!("kitty-fonts-{}-1", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("kitty.conf"), "font_size 12\nfont_family Old\n").unwrap();
        let path = settings.save_in_conf(&dir, "kitty.conf").unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("# KITTY_FONTS"));
        assert!(text.contains("font_family"));
        assert!(text.contains("font_size 12"));
        assert!(!text.contains("font_family Old"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ui_selects_family() {
        let mut families = BTreeMap::new();
        families.insert(
            "Fira Code".to_owned(),
            vec![ListedFont {
                family: "Fira Code".to_owned(),
                style: "Medium".to_owned(),
                ..ListedFont::default()
            }],
        );
        families.insert(
            "Iosevka".to_owned(),
            vec![ListedFont {
                family: "Iosevka".to_owned(),
                style: "Regular".to_owned(),
                ..ListedFont::default()
            }],
        );
        let mut tty = FakeTty::new(80, 24, b"\r");
        let selected = run_choose_fonts_ui(&mut tty, &families).unwrap();
        assert_eq!(selected, Some(("Fira Code".to_owned(), String::new())));
        assert!(tty.output_text().contains("Choose a font"));
    }

    #[test]
    fn options() {
        let args = ["--reload-in=all", "--config-file-name=fonts.conf"]
            .iter()
            .map(|x| (*x).to_owned())
            .collect::<Vec<_>>();
        let opts = parse_choose_fonts_args(&args).unwrap();
        assert_eq!(opts.reload_in, "all");
        assert_eq!(opts.config_file_name, "fonts.conf");
        assert!(parse_choose_fonts_args(&["--reload-in=bad".to_owned()]).is_err());
    }
}
