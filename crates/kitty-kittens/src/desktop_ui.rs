// SPDX-License-Identifier: GPL-3.0-or-later
//! Desktop UI/portal kitten, ported from `kittens/desktop_ui`.
//!
//! This module implements the portable desktop-service state and installation
//! logic: appearance settings, color/number parsing, `portals.conf` patching,
//! the xdg-desktop-portal definition and the D-Bus activation service file.
//! The live D-Bus service and file-chooser portal require a D-Bus connection;
//! [`run_server`] reports that limitation instead of silently doing nothing.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const PORTAL_APPEARANCE_NAMESPACE: &str = "org.freedesktop.appearance";
pub const PORTAL_COLOR_SCHEME_KEY: &str = "color-scheme";
pub const PORTAL_ACCENT_COLOR_KEY: &str = "accent-color";
pub const PORTAL_CONTRAST_KEY: &str = "contrast";
pub const PORTAL_BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.kitty";
pub const SETTINGS_INTERFACE: &str = "org.freedesktop.impl.portal.Settings";
pub const FILE_CHOOSER_INTERFACE: &str = "org.freedesktop.impl.portal.FileChooser";
pub const SETTINGS_CANARY_NAMESPACE: &str = "net.kovidgoyal.kitty";
pub const SETTINGS_CANARY_KEY: &str = "status";

pub const DESKTOP_UI_USAGE: &str = "\
Usage: kitten desktop-ui <command>

Commands:
  run-server                 Start the desktop portal services
  enable-portal              Install the portal definition and activation service
  set-color-scheme VALUE     light, dark, no-preference or toggle
  set-accent-color COLOR
  set-contrast high|normal
  set-setting KEY [VALUE]
  show-settings [--as-json]
  -h, --help";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorScheme {
    #[default]
    NoPreference,
    Dark,
    Light,
}

impl ColorScheme {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "no-preference" => Self::NoPreference,
            "dark" => Self::Dark,
            "light" => Self::Light,
            _ => return None,
        })
    }

    pub fn as_value(self) -> u32 {
        match self {
            Self::NoPreference => 0,
            Self::Dark => 1,
            Self::Light => 2,
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
            Self::NoPreference => Self::NoPreference,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Contrast {
    #[default]
    Normal,
    High,
}

impl Contrast {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "normal" => Self::Normal,
            "high" => Self::High,
            _ => return None,
        })
    }

    pub fn as_value(self) -> u32 {
        match self {
            Self::Normal => 0,
            Self::High => 1,
        }
    }
}

/// Parse a color name or `#RGB`/`#RRGGBB`, returning normalized RGBA floats.
pub fn parse_color(spec: &str) -> Option<[f64; 4]> {
    let spec = spec.trim();
    let (r, g, b) = if let Some(hex) = spec.strip_prefix('#') {
        match hex.len() {
            3 => (
                u8::from_str_radix(&hex[0..1], 16).ok()? * 17,
                u8::from_str_radix(&hex[1..2], 16).ok()? * 17,
                u8::from_str_radix(&hex[2..3], 16).ok()? * 17,
            ),
            6 => (
                u8::from_str_radix(&hex[0..2], 16).ok()?,
                u8::from_str_radix(&hex[2..4], 16).ok()?,
                u8::from_str_radix(&hex[4..6], 16).ok()?,
            ),
            _ => return None,
        }
    } else {
        match spec.to_ascii_lowercase().as_str() {
            "black" => (0, 0, 0),
            "white" => (255, 255, 255),
            "red" => (255, 0, 0),
            "green" => (0, 128, 0),
            "blue" => (0, 0, 255),
            "cyan" => (0, 255, 255),
            "magenta" => (255, 0, 255),
            "yellow" => (255, 255, 0),
            "gray" | "grey" => (128, 128, 128),
            _ => return None,
        }
    };
    Some([r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0, 1.0])
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DesktopSettings {
    /// namespace -> key -> textual value
    pub values: BTreeMap<String, BTreeMap<String, String>>,
}

impl DesktopSettings {
    pub fn new(color_scheme: ColorScheme, accent: &str, contrast: Contrast) -> Self {
        let mut settings = Self::default();
        settings.values.insert(
            SETTINGS_CANARY_NAMESPACE.to_owned(),
            BTreeMap::from([(SETTINGS_CANARY_KEY.to_owned(), "running".to_owned())]),
        );
        let appearance = settings
            .values
            .entry(PORTAL_APPEARANCE_NAMESPACE.to_owned())
            .or_default();
        appearance.insert(
            PORTAL_COLOR_SCHEME_KEY.to_owned(),
            color_scheme.as_value().to_string(),
        );
        if let Some(color) = parse_color(accent) {
            appearance.insert(
                PORTAL_ACCENT_COLOR_KEY.to_owned(),
                format!("{:.6},{:.6},{:.6}", color[0], color[1], color[2]),
            );
        }
        appearance.insert(
            PORTAL_CONTRAST_KEY.to_owned(),
            contrast.as_value().to_string(),
        );
        settings
    }

    pub fn read(&self, namespace: &str, key: &str) -> Option<&str> {
        self.values
            .get(namespace)
            .and_then(|map| map.get(key))
            .map(String::as_str)
    }

    pub fn change(&mut self, namespace: &str, key: &str, value: Option<&str>) {
        let map = self.values.entry(namespace.to_owned()).or_default();
        match value {
            Some(value) if !value.is_empty() => {
                map.insert(key.to_owned(), value.to_owned());
            }
            _ => {
                map.remove(key);
            }
        }
    }

    pub fn color_scheme(&self) -> ColorScheme {
        match self.read(PORTAL_APPEARANCE_NAMESPACE, PORTAL_COLOR_SCHEME_KEY) {
            Some("1") => ColorScheme::Dark,
            Some("2") => ColorScheme::Light,
            _ => ColorScheme::NoPreference,
        }
    }

    pub fn set_color_scheme(&mut self, which: &str) -> Result<(), String> {
        let new = match which {
            "toggle" => self.color_scheme().toggle(),
            value => ColorScheme::parse(value)
                .ok_or_else(|| format!("{value} is not a valid value of the color-scheme"))?,
        };
        self.change(
            PORTAL_APPEARANCE_NAMESPACE,
            PORTAL_COLOR_SCHEME_KEY,
            Some(&new.as_value().to_string()),
        );
        Ok(())
    }

    pub fn as_json(&self) -> String {
        let mut namespaces = Vec::new();
        for (namespace, keys) in &self.values {
            let mut entries = Vec::new();
            for (key, value) in keys {
                entries.push(format!(
                    "\"{}\":\"{}\"",
                    escape_json(key),
                    escape_json(value)
                ));
            }
            namespaces.push(format!(
                "\"{}\":{{{}}}",
                escape_json(namespace),
                entries.join(",")
            ));
        }
        format!("{{{}}}\n", namespaces.join(","))
    }
}

fn escape_json(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Patch `portals.conf` so the kitty settings and file chooser portals are
/// preferred, replacing existing assignments for those interfaces.
pub fn patch_portals_conf(text: &str) -> String {
    let interfaces = [SETTINGS_INTERFACE, FILE_CHOOSER_INTERFACE];
    let mut lines: Vec<String> = Vec::new();
    let mut in_preferred = false;
    let mut patched = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_preferred = trimmed == "[preferred]";
            lines.push(line.to_owned());
            if in_preferred {
                for interface in interfaces {
                    lines.push(format!("{interface}=kitty;*"));
                }
                patched = true;
            }
        } else if in_preferred {
            if interfaces
                .iter()
                .any(|interface| trimmed.starts_with(interface))
            {
                continue;
            }
            lines.push(line.to_owned());
        } else {
            lines.push(line.to_owned());
        }
    }
    if !patched {
        lines.push("[preferred]".to_owned());
        for interface in interfaces {
            lines.push(format!("{interface}=kitty;*"));
        }
    }
    lines.join("\n")
}

pub fn portal_definition() -> String {
    format!(
        "[portal]\nDBusName={PORTAL_BUS_NAME}\nInterfaces={SETTINGS_INTERFACE};{FILE_CHOOSER_INTERFACE};\n"
    )
}

pub fn dbus_service_definition(kitten_exe: &Path) -> String {
    format!(
        "[D-BUS Service]\nName={PORTAL_BUS_NAME}\nExec={} desktop-ui run-server\n",
        kitten_exe.display()
    )
}

pub fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(home));
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share"));
    }
    let value =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
    for dir in value.split(':') {
        if !dir.is_empty() {
            dirs.push(PathBuf::from(dir));
        }
    }
    let mut unique = Vec::new();
    for dir in dirs {
        if !unique.contains(&dir) {
            unique.push(dir);
        }
    }
    unique
}

pub fn writable_data_dirs() -> Vec<PathBuf> {
    data_dirs()
        .into_iter()
        .filter(|dir| {
            fs::create_dir_all(dir).is_ok()
                && fs::metadata(dir)
                    .map(|meta| !meta.permissions().readonly())
                    .unwrap_or(false)
        })
        .collect()
}

/// Install the portal definition and activation service. Returns the paths
/// written. `portals.conf` patching is performed when the file exists.
pub fn enable_portal(kitten_exe: &Path) -> Result<Vec<PathBuf>, String> {
    let writable = writable_data_dirs();
    if writable.is_empty() {
        return Err("Could not find any writable data directories. Make sure XDG_DATA_DIRS is set and contains at least one writable directory".to_owned());
    }
    let mut written = Vec::new();
    let portal_dir = writable[0].join("xdg-desktop-portal/portals");
    fs::create_dir_all(&portal_dir).map_err(|err| err.to_string())?;
    let portal_path = portal_dir.join("kitty.portal");
    fs::write(&portal_path, portal_definition()).map_err(|err| err.to_string())?;
    written.push(portal_path);

    let service_dir = writable[0].join("dbus-1/services");
    fs::create_dir_all(&service_dir).map_err(|err| err.to_string())?;
    let service_path = service_dir.join(format!("{PORTAL_BUS_NAME}.service"));
    fs::write(&service_path, dbus_service_definition(kitten_exe)).map_err(|err| err.to_string())?;
    written.push(service_path);

    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    let portal_config_dir = config_home.join("xdg-desktop-portal");
    fs::create_dir_all(&portal_config_dir).map_err(|err| err.to_string())?;
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let candidates: Vec<PathBuf> = desktop
        .split(':')
        .filter(|item| !item.is_empty())
        .map(|item| portal_config_dir.join(format!("{}-portals.conf", item.to_lowercase())))
        .chain(std::iter::once(portal_config_dir.join("portals.conf")))
        .collect();
    let mut patched = false;
    for candidate in candidates {
        if let Ok(text) = fs::read_to_string(&candidate) {
            fs::write(&candidate, patch_portals_conf(&text)).map_err(|err| err.to_string())?;
            written.push(candidate);
            patched = true;
            break;
        }
    }
    if !patched {
        let path = portal_config_dir.join("portals.conf");
        fs::write(&path, patch_portals_conf("")).map_err(|err| err.to_string())?;
        written.push(path);
    }
    Ok(written)
}

/// The live service requires a D-Bus connection, which this dependency-free
/// crate does not implement. The settings/installation engine remains usable.
pub fn run_server() -> Result<(), String> {
    Err("The desktop-ui D-Bus service is not available in this build; use `enable-portal` and the settings commands, which are implemented".to_owned())
}

/// Execute one desktop-ui subcommand. Returns text to print.
pub fn run_desktop_ui_command(args: &[String], kitten_exe: &Path) -> Result<String, String> {
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help") {
        return Ok(DESKTOP_UI_USAGE.to_owned());
    }
    match args[0].as_str() {
        "run-server" => run_server().map(|_| String::new()),
        "enable-portal" => {
            let paths = enable_portal(kitten_exe)?;
            Ok(paths
                .iter()
                .map(|path| format!("Wrote {}", path.display()))
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "set-color-scheme" => {
            let value = args
                .get(1)
                .ok_or_else(|| "must specify the new color scheme value".to_owned())?;
            let mut settings =
                DesktopSettings::new(ColorScheme::NoPreference, "cyan", Contrast::Normal);
            settings.set_color_scheme(value)?;
            Ok(settings
                .read(PORTAL_APPEARANCE_NAMESPACE, PORTAL_COLOR_SCHEME_KEY)
                .unwrap_or("0")
                .to_owned())
        }
        "set-accent-color" => {
            let value = args
                .get(1)
                .ok_or_else(|| "must specify the new accent color value".to_owned())?;
            parse_color(value)
                .map(|color| format!("{:.6},{:.6},{:.6}", color[0], color[1], color[2]))
                .ok_or_else(|| format!("{value} is not a valid color"))
        }
        "set-contrast" => {
            let value = args
                .get(1)
                .ok_or_else(|| "must specify the new contrast value".to_owned())?;
            Contrast::parse(value)
                .map(|contrast| contrast.as_value().to_string())
                .ok_or_else(|| format!("{value} is not a valid contrast value"))
        }
        "set-setting" => {
            let actual_key = args
                .get(1)
                .ok_or_else(|| "must specify the key".to_owned())?;
            let value = args.get(2).map(String::as_str);
            let mut settings = DesktopSettings::default();
            settings.change(PORTAL_APPEARANCE_NAMESPACE, actual_key, value);
            Ok(settings.as_json())
        }
        "show-settings" => {
            let as_json = args.iter().any(|arg| arg == "--as-json");
            let settings =
                DesktopSettings::new(ColorScheme::NoPreference, "cyan", Contrast::Normal);
            if as_json {
                Ok(settings.as_json())
            } else {
                let mut out = String::new();
                for (namespace, keys) in &settings.values {
                    for (key, value) in keys {
                        out.push_str(&format!("{namespace}:{key}={value}\n"));
                    }
                }
                Ok(out)
            }
        }
        other => Err(format!("Unknown desktop-ui command: {other}")),
    }
}

pub fn write_result(out: &mut dyn Write, text: &str) -> std::io::Result<()> {
    write!(out, "{text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_and_settings() {
        assert_eq!(parse_color("#fff"), Some([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(parse_color("black"), Some([0.0, 0.0, 0.0, 1.0]));
        assert!(parse_color("not-a-color").is_none());
        let mut settings = DesktopSettings::new(ColorScheme::Dark, "#ff0000", Contrast::High);
        assert_eq!(settings.color_scheme(), ColorScheme::Dark);
        assert_eq!(
            settings.read(PORTAL_APPEARANCE_NAMESPACE, PORTAL_CONTRAST_KEY),
            Some("1")
        );
        settings.set_color_scheme("toggle").unwrap();
        assert_eq!(settings.color_scheme(), ColorScheme::Light);
        assert!(settings.set_color_scheme("bogus").is_err());
    }

    #[test]
    fn patches_portals_conf() {
        let text =
            "[preferred]\norg.freedesktop.impl.portal.Settings=gtk;\nother=value\n[other]\nx=y\n";
        let patched = patch_portals_conf(text);
        assert!(patched.contains("org.freedesktop.impl.portal.Settings=kitty;*"));
        assert!(patched.contains("org.freedesktop.impl.portal.FileChooser=kitty;*"));
        assert!(!patched.contains("Settings=gtk"));
        assert!(patched.contains("other=value"));
        let empty = patch_portals_conf("");
        assert!(empty.starts_with("[preferred]"));
    }

    #[test]
    fn definitions() {
        let definition = portal_definition();
        assert!(definition.contains(PORTAL_BUS_NAME));
        assert!(definition.contains(FILE_CHOOSER_INTERFACE));
        let service = dbus_service_definition(Path::new("/usr/bin/kitten"));
        assert!(service.contains("Exec=/usr/bin/kitten desktop-ui run-server"));
    }

    #[test]
    fn commands() {
        let help = run_desktop_ui_command(&[], Path::new("/bin/kitten")).unwrap();
        assert!(help.contains("enable-portal"));
        assert_eq!(
            run_desktop_ui_command(
                &["set-color-scheme".to_owned(), "dark".to_owned()],
                Path::new("/bin/kitten")
            )
            .unwrap(),
            "1"
        );
        assert_eq!(
            run_desktop_ui_command(
                &["set-accent-color".to_owned(), "#00ff00".to_owned()],
                Path::new("/bin/kitten")
            )
            .unwrap(),
            "0.000000,1.000000,0.000000"
        );
        assert!(
            run_desktop_ui_command(&["run-server".to_owned()], Path::new("/bin/kitten")).is_err()
        );
    }

    #[test]
    fn json_output() {
        let mut settings = DesktopSettings::new(ColorScheme::Light, "cyan", Contrast::Normal);
        settings.change("ns", "key", Some("value"));
        let json = settings.as_json();
        assert!(json.contains("\"key\":\"value\""));
        assert!(json.contains(PORTAL_APPEARANCE_NAMESPACE));
    }
}
