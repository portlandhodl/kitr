// SPDX-License-Identifier: GPL-3.0-or-later
//! Desktop UI/portal kitten, ported from `kittens/desktop_ui`.
//!
//! This module implements the portable desktop-service state and installation
//! logic: appearance settings, color/number parsing, `portals.conf` patching,
//! the xdg-desktop-portal definition and the D-Bus activation service file.
//! [`serve_portal`] exports the Settings and FileChooser interfaces on the
//! session bus and serves until shutdown; [`run_server`] serves forever.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use zbus::zvariant::{OwnedValue, Value};

pub const PORTAL_APPEARANCE_NAMESPACE: &str = "org.freedesktop.appearance";
pub const PORTAL_COLOR_SCHEME_KEY: &str = "color-scheme";
pub const PORTAL_ACCENT_COLOR_KEY: &str = "accent-color";
pub const PORTAL_CONTRAST_KEY: &str = "contrast";
pub const PORTAL_BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.kitty";
pub const SETTINGS_INTERFACE: &str = "org.freedesktop.impl.portal.Settings";
pub const FILE_CHOOSER_INTERFACE: &str = "org.freedesktop.impl.portal.FileChooser";
pub const SETTINGS_CANARY_NAMESPACE: &str = "net.kovidgoyal.kitty";
pub const SETTINGS_CANARY_KEY: &str = "status";

pub const DESKTOP_OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";
pub const KITTY_OBJECT_PATH: &str = "/net/kovidgoyal/kitty/portal";
pub const CHANGE_SETTINGS_INTERFACE: &str = "net.kovidgoyal.kitty.settings";

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

// The live portal service ///////////////////////////////////////////////////

/// One filter group as carried by the FileChooser portal (`(sa(us))`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileFilter {
    pub name: String,
    /// `(kind, pattern)` pairs. `kind` is 0 for glob and 1 for MIME.
    pub rules: Vec<(u32, String)>,
}

impl FileFilter {
    pub fn from_dbus(value: &OwnedValue) -> Option<Self> {
        let (name, rules) = <(String, Vec<(u32, String)>)>::try_from(value.clone()).ok()?;
        Some(Self { name, rules })
    }

    pub fn to_dbus(&self) -> OwnedValue {
        OwnedValue::try_from(Value::new((self.name.clone(), self.rules.clone())))
            .expect("file filters are representable")
    }
}

/// A file chooser request sent by xdg-desktop-portal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileChooserRequest {
    pub handle: String,
    pub app_id: String,
    pub parent_window: String,
    pub title: String,
    pub mode: String,
    pub directory: bool,
    pub multiple: bool,
    pub current_folder: String,
    pub current_name: String,
    pub current_file: String,
    pub filters: Vec<(String, String, String)>,
    /// Filter groups in portal order, with `current_filter` moved to the front.
    pub filter_groups: Vec<FileFilter>,
    /// The filter requested by the caller via `current_filter`.
    pub current_filter: Option<FileFilter>,
}

/// The result returned by a [`ChooserLauncher`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileChooserResult {
    pub paths: Vec<String>,
    pub interrupted: bool,
    pub current_filter: Option<String>,
}

/// A host-supplied file chooser implementation.
///
/// The default implementation runs the native `choose-files` kitten. Tests and
/// embedding applications can provide a different implementation without a
/// display server.
pub trait ChooserLauncher: Send + Sync {
    fn choose(&self, request: &FileChooserRequest) -> Result<FileChooserResult, String>;

    /// Run the chooser while allowing a portal `Close` request to cancel it.
    /// Implementations that cannot cancel can use the default.
    fn choose_cancelable(
        &self,
        request: &FileChooserRequest,
        _cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<FileChooserResult, String> {
        self.choose(request)
    }
}

#[derive(Clone, Debug)]
struct ProcessChooserLauncher {
    kitten_exe: PathBuf,
}

impl ChooserLauncher for ProcessChooserLauncher {
    fn choose(&self, request: &FileChooserRequest) -> Result<FileChooserResult, String> {
        self.choose_cancelable(request, &std::sync::atomic::AtomicBool::new(false))
    }

    fn choose_cancelable(
        &self,
        request: &FileChooserRequest,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<FileChooserResult, String> {
        let temp = std::env::temp_dir().join(format!(
            "kitty-portal-{}-{}",
            std::process::id(),
            new_request_suffix()
        ));
        fs::create_dir_all(&temp).map_err(|err| err.to_string())?;
        let output = temp.join("output.json");
        let mut command = std::process::Command::new(&self.kitten_exe);
        command
            .arg("choose-files")
            .arg("--mode")
            .arg(&request.mode)
            .arg("--write-output-to")
            .arg(&output)
            .arg("--output-format=json");
        if !request.title.is_empty() {
            command.arg("--title").arg(&request.title);
        }
        if !request.current_name.is_empty() {
            command
                .arg("--suggested-save-file-name")
                .arg(&request.current_name);
        }
        if !request.current_file.is_empty() {
            command
                .arg("--suggested-save-file-path")
                .arg(&request.current_file);
        }
        for filter in &request.filter_groups {
            let kind = if filter.rules.iter().all(|(kind, _)| *kind == 0) {
                "glob"
            } else if filter.rules.iter().all(|(kind, _)| *kind == 1) {
                "mime"
            } else {
                "glob"
            };
            let value = filter
                .rules
                .iter()
                .map(|(_, pattern)| pattern.as_str())
                .collect::<Vec<_>>()
                .join(",");
            command
                .arg("--file-filter")
                .arg(format!("{kind}:{value}:{}", filter.name));
        }
        if request.filter_groups.is_empty() {
            for (kind, value, name) in &request.filters {
                command
                    .arg("--file-filter")
                    .arg(format!("{kind}:{value}:{name}"));
            }
        }
        let target = if request.current_folder.is_empty() {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("~"))
        } else {
            PathBuf::from(&request.current_folder)
        };
        command.arg(target);
        let mut child = command.spawn().map_err(|err| err.to_string())?;
        let status = loop {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_dir_all(&temp);
                return Ok(FileChooserResult {
                    interrupted: true,
                    current_filter: request.current_filter.as_ref().map(|f| f.name.clone()),
                    ..FileChooserResult::default()
                });
            }
            match child.try_wait().map_err(|err| err.to_string())? {
                Some(status) => break status,
                None => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        };
        let result = if status.success() {
            let text = fs::read_to_string(&output).map_err(|err| err.to_string())?;
            parse_chooser_output(&text)?
        } else {
            FileChooserResult {
                interrupted: true,
                ..FileChooserResult::default()
            }
        };
        let _ = fs::remove_dir_all(temp);
        Ok(result)
    }
}

fn parse_chooser_output(text: &str) -> Result<FileChooserResult, String> {
    let value = kitty_remote::json::parse(text.as_bytes()).map_err(|err| err.to_string())?;
    let paths = value
        .get("paths")
        .and_then(kitty_remote::json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(kitty_remote::json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if let Some(error) = value
        .get("error")
        .and_then(kitty_remote::json::Value::as_str)
        .filter(|error| !error.is_empty())
    {
        return Err(error.to_owned());
    }
    let interrupted = value
        .get("interrupted")
        .and_then(kitty_remote::json::Value::as_bool)
        .unwrap_or(false);
    let current_filter = value
        .get("current_filter")
        .and_then(kitty_remote::json::Value::as_str)
        .map(str::to_owned);
    Ok(FileChooserResult {
        paths,
        interrupted,
        current_filter,
    })
}

/// Configuration for the live portal service.
#[derive(Clone)]
pub struct PortalConfig {
    pub bus_name: String,
    pub object_path: String,
    pub settings: DesktopSettings,
    pub accent_color: String,
    pub chooser: std::sync::Arc<dyn ChooserLauncher>,
}

impl std::fmt::Debug for PortalConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortalConfig")
            .field("bus_name", &self.bus_name)
            .field("object_path", &self.object_path)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl PortalConfig {
    pub fn new(color_scheme: ColorScheme, accent: &str, contrast: Contrast) -> Self {
        let settings = DesktopSettings::new(color_scheme, accent, contrast);
        let kitten_exe = std::env::var_os("KITTEN_EXE")
            .map(PathBuf::from)
            .or_else(|| std::env::current_exe().ok())
            .unwrap_or_else(|| PathBuf::from("kitten"));
        Self {
            bus_name: PORTAL_BUS_NAME.to_owned(),
            object_path: DESKTOP_OBJECT_PATH.to_owned(),
            settings,
            accent_color: accent.to_owned(),
            chooser: std::sync::Arc::new(ProcessChooserLauncher { kitten_exe }),
        }
    }

    pub fn with_chooser(mut self, chooser: std::sync::Arc<dyn ChooserLauncher>) -> Self {
        self.chooser = chooser;
        self
    }
}

impl Default for PortalConfig {
    fn default() -> Self {
        Self::new(ColorScheme::NoPreference, "cyan", Contrast::Normal)
    }
}

fn value_to_owned_u32(value: u32) -> OwnedValue {
    OwnedValue::try_from(Value::from(value)).expect("u32 is representable")
}

fn text_to_owned(value: &str) -> OwnedValue {
    OwnedValue::try_from(Value::from(value)).expect("strings are representable")
}

/// Convert the portable string map used by [`DesktopSettings`] to D-Bus
/// variants. Appearance values that are numeric are typed as `u32`; everything
/// else is a string.
fn settings_to_variants(
    settings: &DesktopSettings,
) -> BTreeMap<String, BTreeMap<String, OwnedValue>> {
    let mut out = BTreeMap::new();
    for (namespace, keys) in &settings.values {
        let mut converted = BTreeMap::new();
        for (key, value) in keys {
            let owned = if namespace == PORTAL_APPEARANCE_NAMESPACE
                && matches!(key.as_str(), PORTAL_COLOR_SCHEME_KEY | PORTAL_CONTRAST_KEY)
            {
                value
                    .parse::<u32>()
                    .map(value_to_owned_u32)
                    .unwrap_or_else(|_| text_to_owned(value))
            } else {
                text_to_owned(value)
            };
            converted.insert(key.clone(), owned);
        }
        out.insert(namespace.clone(), converted);
    }
    out
}

#[derive(Default)]
struct PortalState {
    values: BTreeMap<String, BTreeMap<String, OwnedValue>>,
}

impl PortalState {
    fn new(settings: &DesktopSettings) -> Self {
        Self {
            values: settings_to_variants(settings),
        }
    }

    fn read(&self, namespace: &str, key: &str) -> Option<OwnedValue> {
        self.values
            .get(namespace)
            .and_then(|keys| keys.get(key))
            .cloned()
    }

    fn read_all(&self, namespaces: &[String]) -> BTreeMap<String, BTreeMap<String, OwnedValue>> {
        if namespaces.is_empty() || namespaces.iter().any(String::is_empty) {
            return self.values.clone();
        }
        let mut out = BTreeMap::new();
        for requested in namespaces {
            if let Some(prefix) = requested.strip_suffix(".*") {
                let prefix = format!("{prefix}.");
                for (namespace, keys) in &self.values {
                    if namespace == prefix.trim_end_matches('.') || namespace.starts_with(&prefix) {
                        out.insert(namespace.clone(), keys.clone());
                    }
                }
            } else if let Some(keys) = self.values.get(requested) {
                out.insert(requested.clone(), keys.clone());
            }
        }
        out
    }

    fn change(&mut self, namespace: &str, key: &str, value: Option<OwnedValue>) {
        let keys = self.values.entry(namespace.to_owned()).or_default();
        match value {
            Some(value) => {
                keys.insert(key.to_owned(), value);
            }
            None => {
                keys.remove(key);
            }
        }
    }
}

type SharedState = std::sync::Arc<std::sync::Mutex<PortalState>>;
type SharedConnection = std::sync::Arc<std::sync::Mutex<Option<zbus::blocking::Connection>>>;

fn emit_setting_changed(
    connection: &SharedConnection,
    namespace: &str,
    key: &str,
    value: OwnedValue,
) {
    if let Ok(guard) = connection.lock() {
        if let Some(connection) = guard.as_ref() {
            let _ = connection.emit_signal(
                None::<()>,
                DESKTOP_OBJECT_PATH,
                SETTINGS_INTERFACE,
                "SettingChanged",
                &(namespace, key, value),
            );
        }
    }
}

struct SettingsInterface {
    state: SharedState,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Settings")]
impl SettingsInterface {
    #[zbus(property)]
    fn version(&self) -> u32 {
        1
    }

    fn read(&self, namespace: &str, key: &str) -> zbus::fdo::Result<OwnedValue> {
        let state = self.state.lock().map_err(|_| {
            zbus::fdo::Error::Failed("portal settings state is poisoned".to_owned())
        })?;
        state.read(namespace, key).ok_or_else(|| {
            zbus::fdo::Error::Failed(format!(
                "the setting {key} in the namespace {namespace} is not supported"
            ))
        })
    }

    fn read_all(
        &self,
        namespaces: Vec<String>,
    ) -> zbus::fdo::Result<BTreeMap<String, BTreeMap<String, OwnedValue>>> {
        let state = self.state.lock().map_err(|_| {
            zbus::fdo::Error::Failed("portal settings state is poisoned".to_owned())
        })?;
        Ok(state.read_all(&namespaces))
    }

    #[zbus(signal)]
    async fn setting_changed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        namespace: &str,
        key: &str,
        value: OwnedValue,
    ) -> zbus::Result<()>;
}

struct ChangeSettingsInterface {
    state: SharedState,
    connection: SharedConnection,
}

#[zbus::interface(name = "net.kovidgoyal.kitty.settings")]
impl ChangeSettingsInterface {
    #[zbus(property)]
    fn version(&self) -> u32 {
        1
    }

    fn change_setting(
        &self,
        namespace: &str,
        key: &str,
        value: OwnedValue,
    ) -> zbus::fdo::Result<()> {
        {
            let mut state = self.state.lock().map_err(|_| {
                zbus::fdo::Error::Failed("portal settings state is poisoned".to_owned())
            })?;
            state.change(namespace, key, Some(value.clone()));
        }
        emit_setting_changed(&self.connection, namespace, key, value);
        Ok(())
    }

    fn remove_setting(&self, namespace: &str, key: &str) -> zbus::fdo::Result<()> {
        {
            let mut state = self.state.lock().map_err(|_| {
                zbus::fdo::Error::Failed("portal settings state is poisoned".to_owned())
            })?;
            state.change(namespace, key, None);
        }
        Ok(())
    }
}

struct FileChooserInterface {
    chooser: std::sync::Arc<dyn ChooserLauncher>,
    connection: SharedConnection,
}

/// The per-request `org.freedesktop.impl.portal.Request` object. Exporting it
/// at `handle` lets the portal frontend cancel a running chooser by calling
/// `Close`.
struct RequestInterface {
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Request")]
impl RequestInterface {
    fn close(&self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

async fn run_chooser_request(
    chooser: std::sync::Arc<dyn ChooserLauncher>,
    connection: SharedConnection,
    request: FileChooserRequest,
) -> Result<FileChooserResult, String> {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let connection = connection
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().cloned());
    let registered = if let Some(connection) = &connection {
        connection
            .inner()
            .object_server()
            .at(
                request.handle.as_str(),
                RequestInterface {
                    cancel: cancel.clone(),
                },
            )
            .await
            .unwrap_or(false)
    } else {
        false
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    let thread_cancel = cancel.clone();
    let thread_request = request.clone();
    std::thread::spawn(move || {
        let result = chooser.choose_cancelable(&thread_request, &thread_cancel);
        let _ = sender.send(result);
    });
    let result = loop {
        match receiver.try_recv() {
            Ok(result) => break result,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                break Err("file chooser thread exited without a result".to_owned())
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                async_io::Timer::after(std::time::Duration::from_millis(20)).await;
            }
        }
    };
    if registered {
        if let Some(connection) = &connection {
            let _ = connection
                .inner()
                .object_server()
                .remove::<RequestInterface, _>(request.handle.as_str())
                .await;
        }
    }
    result
}

fn option_bool(options: &std::collections::HashMap<String, OwnedValue>, key: &str) -> bool {
    options
        .get(key)
        .and_then(|value| value.downcast_ref::<bool>().ok())
        .unwrap_or(false)
}

fn option_bytes(options: &std::collections::HashMap<String, OwnedValue>, key: &str) -> String {
    options
        .get(key)
        .and_then(|value| Vec::<u8>::try_from(value.clone()).ok())
        .map(|bytes| {
            let end = bytes
                .iter()
                .rposition(|byte| *byte != 0)
                .map(|index| index + 1)
                .unwrap_or(0);
            String::from_utf8_lossy(&bytes[..end]).into_owned()
        })
        .unwrap_or_default()
}

fn option_filter_groups(
    options: &std::collections::HashMap<String, OwnedValue>,
) -> Vec<FileFilter> {
    let Some(value) = options.get("filters") else {
        return Vec::new();
    };
    let Ok(groups) = Vec::<(String, Vec<(u32, String)>)>::try_from(value.clone()) else {
        return Vec::new();
    };
    groups
        .into_iter()
        .map(|(name, rules)| FileFilter { name, rules })
        .collect()
}

fn option_filters(groups: &[FileFilter]) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for group in groups {
        for (kind, pattern) in &group.rules {
            out.push((
                if *kind == 0 { "glob" } else { "mime" }.to_owned(),
                pattern.clone(),
                group.name.clone(),
            ));
        }
    }
    out
}

fn option_current_filter(
    options: &std::collections::HashMap<String, OwnedValue>,
    groups: &[FileFilter],
) -> Option<FileFilter> {
    let value = options.get("current_filter")?;
    let filter = FileFilter::from_dbus(value)?;
    groups
        .iter()
        .find(|group| **group == filter)
        .cloned()
        .or(Some(filter))
}

fn file_chooser_request(
    handle: &str,
    app_id: &str,
    parent_window: &str,
    title: &str,
    options: &std::collections::HashMap<String, OwnedValue>,
    save: bool,
) -> FileChooserRequest {
    let directory = option_bool(options, "directory");
    let multiple = option_bool(options, "multiple");
    let mode = if save {
        if multiple {
            "save-files"
        } else {
            "save-file"
        }
    } else if directory {
        if multiple {
            "dirs"
        } else {
            "dir"
        }
    } else if multiple {
        "files"
    } else {
        "file"
    };
    let mut filter_groups = option_filter_groups(options);
    let current_filter = option_current_filter(options, &filter_groups);
    if let Some(current) = &current_filter {
        filter_groups.retain(|group| group != current);
        filter_groups.insert(0, current.clone());
    }
    FileChooserRequest {
        handle: handle.to_owned(),
        app_id: app_id.to_owned(),
        parent_window: parent_window.to_owned(),
        title: title.to_owned(),
        mode: mode.to_owned(),
        directory,
        multiple,
        current_folder: option_bytes(options, "current_folder"),
        current_name: option_bytes(options, "current_name"),
        current_file: option_bytes(options, "current_file"),
        filters: option_filters(&filter_groups),
        filter_groups,
        current_filter,
    }
}

fn uri_for_path(path: &str) -> String {
    let mut out = String::from("file://");
    for byte in path.as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(byte) {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn chooser_response(
    result: &FileChooserResult,
    request: &FileChooserRequest,
) -> (u32, std::collections::HashMap<String, OwnedValue>) {
    let mut results = std::collections::HashMap::new();
    if result.interrupted {
        return (1, results);
    }
    let uris: Vec<String> = result.paths.iter().map(|path| uri_for_path(path)).collect();
    let uris = OwnedValue::try_from(Value::from(uris)).expect("string arrays are representable");
    results.insert("uris".to_owned(), uris);
    if let Some(name) = &result.current_filter {
        if let Some(filter) = request
            .filter_groups
            .iter()
            .find(|filter| &filter.name == name)
            .or_else(|| {
                request
                    .current_filter
                    .as_ref()
                    .filter(|filter| &filter.name == name)
            })
        {
            results.insert("current_filter".to_owned(), filter.to_dbus());
        }
    }
    (0, results)
}

#[zbus::interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooserInterface {
    #[zbus(out_args("response", "results"))]
    async fn open_file(
        &self,
        handle: &str,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: std::collections::HashMap<String, OwnedValue>,
    ) -> (u32, std::collections::HashMap<String, OwnedValue>) {
        let request = file_chooser_request(handle, app_id, parent_window, title, &options, false);
        match run_chooser_request(
            self.chooser.clone(),
            self.connection.clone(),
            request.clone(),
        )
        .await
        {
            Ok(result) => chooser_response(&result, &request),
            Err(_) => (2, std::collections::HashMap::new()),
        }
    }

    #[zbus(out_args("response", "results"))]
    async fn save_file(
        &self,
        handle: &str,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: std::collections::HashMap<String, OwnedValue>,
    ) -> (u32, std::collections::HashMap<String, OwnedValue>) {
        let request = file_chooser_request(handle, app_id, parent_window, title, &options, true);
        match run_chooser_request(
            self.chooser.clone(),
            self.connection.clone(),
            request.clone(),
        )
        .await
        {
            Ok(result) => chooser_response(&result, &request),
            Err(_) => (2, std::collections::HashMap::new()),
        }
    }
}

fn new_request_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos as u64)
}

fn session_connection() -> Result<zbus::blocking::Connection, String> {
    zbus::blocking::Connection::session()
        .map_err(|err| format!("could not connect to session D-Bus: {err}"))
}

fn call_change_setting(namespace: &str, key: &str, value: OwnedValue) -> Result<(), String> {
    let connection = session_connection()?;
    connection
        .call_method(
            Some(PORTAL_BUS_NAME),
            KITTY_OBJECT_PATH,
            Some(CHANGE_SETTINGS_INTERFACE),
            "ChangeSetting",
            &(namespace, key, value),
        )
        .map_err(|err| format!("failed to call ChangeSetting: {err}"))?;
    Ok(())
}

fn call_remove_setting(namespace: &str, key: &str) -> Result<(), String> {
    let connection = session_connection()?;
    connection
        .call_method(
            Some(PORTAL_BUS_NAME),
            KITTY_OBJECT_PATH,
            Some(CHANGE_SETTINGS_INTERFACE),
            "RemoveSetting",
            &(namespace, key),
        )
        .map_err(|err| format!("failed to call RemoveSetting: {err}"))?;
    Ok(())
}

fn call_read_all(
    namespaces: &[String],
) -> Result<BTreeMap<String, BTreeMap<String, OwnedValue>>, String> {
    let connection = session_connection()?;
    let reply = connection
        .call_method(
            Some(PORTAL_BUS_NAME),
            DESKTOP_OBJECT_PATH,
            Some(SETTINGS_INTERFACE),
            "ReadAll",
            &(namespaces,),
        )
        .map_err(|err| format!("failed to call ReadAll: {err}"))?;
    reply
        .body()
        .deserialize()
        .map_err(|err| format!("failed to decode ReadAll response: {err}"))
}

/// Register the portal interfaces on the session bus and serve forever (or
/// until `shutdown` becomes true). Returns once the bus name is owned and all
/// objects are exported.
pub fn serve_portal(
    config: PortalConfig,
    shutdown: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<(), String> {
    let state = std::sync::Arc::new(std::sync::Mutex::new(PortalState::new(&config.settings)));
    let connection_slot: SharedConnection = std::sync::Arc::new(std::sync::Mutex::new(None));
    let builder = zbus::blocking::connection::Builder::session()
        .map_err(|err| format!("could not connect to session D-Bus: {err}"))?
        .name(config.bus_name.clone())
        .map_err(|err| format!("invalid D-Bus name {}: {err}", config.bus_name))?
        .serve_at(
            config.object_path.clone(),
            SettingsInterface {
                state: state.clone(),
            },
        )
        .map_err(|err| format!("failed to export Settings interface: {err}"))?
        .serve_at(
            config.object_path.clone(),
            FileChooserInterface {
                chooser: config.chooser.clone(),
                connection: connection_slot.clone(),
            },
        )
        .map_err(|err| format!("failed to export FileChooser interface: {err}"))?
        .serve_at(
            KITTY_OBJECT_PATH,
            ChangeSettingsInterface {
                state: state.clone(),
                connection: connection_slot.clone(),
            },
        )
        .map_err(|err| format!("failed to export settings control interface: {err}"))?;
    let connection = builder
        .build()
        .map_err(|err| format!("failed to register portal D-Bus name: {err}"))?;
    if let Ok(mut slot) = connection_slot.lock() {
        *slot = Some(connection.clone());
    }
    match shutdown {
        Some(flag) => {
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
        None => loop {
            std::thread::park();
        },
    }
    Ok(())
}

/// Run the live desktop portal service on the session bus.
pub fn run_server() -> Result<(), String> {
    serve_portal(PortalConfig::default(), None)
}

fn owned_to_text(value: &OwnedValue) -> String {
    if let Ok(text) = value.downcast_ref::<String>() {
        return text;
    }
    if let Ok(value) = value.downcast_ref::<u32>() {
        return value.to_string();
    }
    if let Ok(value) = value.downcast_ref::<i32>() {
        return value.to_string();
    }
    if let Ok(value) = value.downcast_ref::<bool>() {
        return value.to_string();
    }
    if let Ok(value) = value.downcast_ref::<f64>() {
        return value.to_string();
    }
    if let Ok(array) = Vec::<f64>::try_from(value.clone()) {
        return array
            .iter()
            .map(|value| format!("{value:.6}"))
            .collect::<Vec<_>>()
            .join(",");
    }
    format!("{}", **value)
}

fn live_read(namespace: &str, key: &str) -> Option<OwnedValue> {
    let values = call_read_all(&[namespace.to_owned()]).ok()?;
    values.get(namespace)?.get(key).cloned()
}

fn try_live_set_color_scheme(which: &str) -> Result<String, String> {
    let current = live_read(PORTAL_APPEARANCE_NAMESPACE, PORTAL_COLOR_SCHEME_KEY)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0);
    let current = match current {
        1 => ColorScheme::Dark,
        2 => ColorScheme::Light,
        _ => ColorScheme::NoPreference,
    };
    let new = match which {
        "toggle" => current.toggle(),
        value => ColorScheme::parse(value)
            .ok_or_else(|| format!("{value} is not a valid value of the color-scheme"))?,
    };
    call_change_setting(
        PORTAL_APPEARANCE_NAMESPACE,
        PORTAL_COLOR_SCHEME_KEY,
        value_to_owned_u32(new.as_value()),
    )?;
    Ok(new.as_value().to_string())
}

fn try_live_set_accent(value: &str) -> Result<String, String> {
    let color = parse_color(value).ok_or_else(|| format!("{value} is not a valid color"))?;
    let array = Value::Array(
        vec![
            Value::from(color[0]),
            Value::from(color[1]),
            Value::from(color[2]),
        ]
        .into(),
    );
    let owned = OwnedValue::try_from(array).map_err(|err| err.to_string())?;
    call_change_setting(PORTAL_APPEARANCE_NAMESPACE, PORTAL_ACCENT_COLOR_KEY, owned)?;
    Ok(format!("{:.6},{:.6},{:.6}", color[0], color[1], color[2]))
}

fn try_live_set_contrast(value: &str) -> Result<String, String> {
    let contrast =
        Contrast::parse(value).ok_or_else(|| format!("{value} is not a valid contrast value"))?;
    call_change_setting(
        PORTAL_APPEARANCE_NAMESPACE,
        PORTAL_CONTRAST_KEY,
        value_to_owned_u32(contrast.as_value()),
    )?;
    Ok(contrast.as_value().to_string())
}

fn try_live_set_setting(namespace: &str, key: &str, value: Option<&str>) -> Result<String, String> {
    match value {
        Some(value) if !value.is_empty() => {
            let owned = if namespace == PORTAL_APPEARANCE_NAMESPACE
                && matches!(key, PORTAL_COLOR_SCHEME_KEY | PORTAL_CONTRAST_KEY)
            {
                value
                    .parse::<u32>()
                    .map(value_to_owned_u32)
                    .unwrap_or_else(|_| text_to_owned(value))
            } else if namespace == PORTAL_APPEARANCE_NAMESPACE && key == PORTAL_ACCENT_COLOR_KEY {
                let color =
                    parse_color(value).ok_or_else(|| format!("{value} is not a valid color"))?;
                let array = Value::Array(
                    vec![
                        Value::from(color[0]),
                        Value::from(color[1]),
                        Value::from(color[2]),
                    ]
                    .into(),
                );
                OwnedValue::try_from(array).map_err(|err| err.to_string())?
            } else {
                text_to_owned(value)
            };
            call_change_setting(namespace, key, owned)?;
        }
        _ => call_remove_setting(namespace, key)?,
    }
    Ok(String::new())
}

fn live_show_settings(namespaces: &[String], as_json: bool) -> Result<String, String> {
    let values = call_read_all(namespaces)?;
    if as_json {
        let mut out = String::from(
            "{
",
        );
        let mut first_namespace = true;
        for (namespace, keys) in &values {
            if !first_namespace {
                out.push_str(
                    ",
",
                );
            }
            first_namespace = false;
            out.push_str(&format!(
                "  \"{}\": {{
",
                escape_json(namespace)
            ));
            let mut first_key = true;
            for (key, value) in keys {
                if !first_key {
                    out.push_str(
                        ",
",
                    );
                }
                first_key = false;
                out.push_str(&format!(
                    "    \"{}\": \"{}\"",
                    escape_json(key),
                    escape_json(&owned_to_text(value))
                ));
            }
            out.push_str("\n  }");
        }
        out.push_str("\n}\n");
        return Ok(out);
    }
    let mut out = String::new();
    for (namespace, keys) in &values {
        out.push_str(namespace);
        out.push_str(":\n");
        for (key, value) in keys {
            out.push_str(&format!("\t{key}: {}\n", owned_to_text(value)));
        }
    }
    Ok(out)
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
            if let Ok(value) = try_live_set_color_scheme(value) {
                return Ok(value);
            }
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
            if let Ok(value) = try_live_set_accent(value) {
                return Ok(value);
            }
            parse_color(value)
                .map(|color| format!("{:.6},{:.6},{:.6}", color[0], color[1], color[2]))
                .ok_or_else(|| format!("{value} is not a valid color"))
        }
        "set-contrast" => {
            let value = args
                .get(1)
                .ok_or_else(|| "must specify the new contrast value".to_owned())?;
            if let Ok(value) = try_live_set_contrast(value) {
                return Ok(value);
            }
            Contrast::parse(value)
                .map(|contrast| contrast.as_value().to_string())
                .ok_or_else(|| format!("{value} is not a valid contrast value"))
        }
        "set-setting" => {
            let actual_key = args
                .get(1)
                .ok_or_else(|| "must specify the key".to_owned())?;
            let value = args.get(2).map(String::as_str);
            let namespace = args
                .iter()
                .position(|arg| arg == "--namespace")
                .and_then(|index| args.get(index + 1))
                .map(String::as_str)
                .unwrap_or(PORTAL_APPEARANCE_NAMESPACE);
            if let Ok(value) = try_live_set_setting(namespace, actual_key, value) {
                return Ok(value);
            }
            let mut settings = DesktopSettings::default();
            settings.change(namespace, actual_key, value);
            Ok(settings.as_json())
        }
        "show-settings" => {
            let as_json = args.iter().any(|arg| arg == "--as-json");
            let namespaces: Vec<String> = args
                .iter()
                .position(|arg| arg == "--namespace")
                .and_then(|index| args.get(index + 1))
                .map(|value| vec![value.clone()])
                .unwrap_or_default();
            if let Ok(text) = live_show_settings(&namespaces, as_json) {
                return Ok(text);
            }
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
            !run_desktop_ui_command(&["show-settings".to_owned()], Path::new("/bin/kitten"))
                .unwrap()
                .is_empty()
        );
    }

    struct FakeChooser;

    impl ChooserLauncher for FakeChooser {
        fn choose(&self, request: &FileChooserRequest) -> Result<FileChooserResult, String> {
            if request.mode == "file" && request.title == "Pick one" {
                Ok(FileChooserResult {
                    paths: vec!["/tmp/a b.txt".to_owned()],
                    interrupted: false,
                    current_filter: None,
                })
            } else {
                Ok(FileChooserResult {
                    interrupted: true,
                    ..FileChooserResult::default()
                })
            }
        }
    }

    struct LivePortal {
        bus_name: String,
        shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for LivePortal {
        fn drop(&mut self) {
            self.shutdown
                .store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn start_live_portal() -> LivePortal {
        let bus_name = format!(
            "org.freedesktop.impl.portal.desktop.kitty.test{}",
            std::process::id()
        );
        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut config = PortalConfig::new(ColorScheme::Dark, "#112233", Contrast::High);
        config.bus_name = bus_name.clone();
        config = config.with_chooser(std::sync::Arc::new(FakeChooser));
        let flag = shutdown.clone();
        let thread = std::thread::spawn(move || {
            let _ = serve_portal(config, Some(flag));
        });
        // The connection builder owns the name before returning from
        // serve_portal, so retry briefly until the client can resolve it.
        for _ in 0..100 {
            if zbus::blocking::Connection::session()
                .and_then(|connection| {
                    connection.call_method(
                        Some(bus_name.as_str()),
                        "/org/freedesktop/portal/desktop",
                        Some("org.freedesktop.impl.portal.Settings"),
                        "Read",
                        &(PORTAL_APPEARANCE_NAMESPACE, PORTAL_COLOR_SCHEME_KEY),
                    )
                })
                .is_ok()
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        LivePortal {
            bus_name,
            shutdown,
            thread: Some(thread),
        }
    }

    #[test]
    fn live_dbus_portal_settings_and_file_chooser() {
        let portal = start_live_portal();
        let connection = zbus::blocking::Connection::session().unwrap();
        let read = |namespace: &str, key: &str| -> OwnedValue {
            connection
                .call_method(
                    Some(portal.bus_name.as_str()),
                    "/org/freedesktop/portal/desktop",
                    Some(SETTINGS_INTERFACE),
                    "Read",
                    &(namespace, key),
                )
                .unwrap()
                .body()
                .deserialize()
                .unwrap()
        };
        assert_eq!(
            u32::try_from(read(PORTAL_APPEARANCE_NAMESPACE, PORTAL_COLOR_SCHEME_KEY)).unwrap(),
            ColorScheme::Dark.as_value()
        );
        assert_eq!(
            u32::try_from(read(PORTAL_APPEARANCE_NAMESPACE, PORTAL_CONTRAST_KEY)).unwrap(),
            Contrast::High.as_value()
        );

        let all: BTreeMap<String, BTreeMap<String, OwnedValue>> = connection
            .call_method(
                Some(portal.bus_name.as_str()),
                "/org/freedesktop/portal/desktop",
                Some(SETTINGS_INTERFACE),
                "ReadAll",
                &(vec![PORTAL_APPEARANCE_NAMESPACE],),
            )
            .unwrap()
            .body()
            .deserialize()
            .unwrap();
        assert!(all.contains_key(PORTAL_APPEARANCE_NAMESPACE));

        connection
            .call_method(
                Some(portal.bus_name.as_str()),
                KITTY_OBJECT_PATH,
                Some(CHANGE_SETTINGS_INTERFACE),
                "ChangeSetting",
                &(
                    PORTAL_APPEARANCE_NAMESPACE,
                    PORTAL_COLOR_SCHEME_KEY,
                    value_to_owned_u32(ColorScheme::Light.as_value()),
                ),
            )
            .unwrap();
        assert_eq!(
            u32::try_from(read(PORTAL_APPEARANCE_NAMESPACE, PORTAL_COLOR_SCHEME_KEY)).unwrap(),
            ColorScheme::Light.as_value()
        );

        let empty = std::collections::HashMap::<String, OwnedValue>::new();
        let response: (u32, std::collections::HashMap<String, OwnedValue>) = connection
            .call_method(
                Some(portal.bus_name.as_str()),
                "/org/freedesktop/portal/desktop",
                Some(FILE_CHOOSER_INTERFACE),
                "OpenFile",
                &(
                    "/org/freedesktop/portal/desktop/request/1",
                    "app",
                    "",
                    "Pick one",
                    empty,
                ),
            )
            .unwrap()
            .body()
            .deserialize()
            .unwrap();
        assert_eq!(response.0, 0);
        let uris = Vec::<String>::try_from(response.1.get("uris").unwrap().clone()).unwrap();
        assert_eq!(uris, vec!["file:///tmp/a%20b.txt"]);
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

#[cfg(test)]
mod portal_request_tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn options_with_filters() -> HashMap<String, OwnedValue> {
        let filters = vec![
            ("Images".to_owned(), vec![(0u32, "*.png".to_owned())]),
            ("Text".to_owned(), vec![(0u32, "*.txt".to_owned())]),
        ];
        let current = ("Text".to_owned(), vec![(0u32, "*.txt".to_owned())]);
        HashMap::from([
            (
                "filters".to_owned(),
                OwnedValue::try_from(Value::new(filters)).unwrap(),
            ),
            (
                "current_filter".to_owned(),
                OwnedValue::try_from(Value::new(current)).unwrap(),
            ),
            (
                "current_folder".to_owned(),
                OwnedValue::try_from(Value::from(b"/tmp\0".to_vec())).unwrap(),
            ),
        ])
    }

    #[test]
    fn current_filter_is_first_group_and_round_trips() {
        let options = options_with_filters();
        let request = file_chooser_request(
            "/org/freedesktop/portal/desktop/request/1",
            "app",
            "",
            "Pick",
            &options,
            false,
        );
        assert_eq!(request.current_filter.as_ref().unwrap().name, "Text");
        assert_eq!(request.filter_groups[0].name, "Text");
        assert_eq!(request.filter_groups[1].name, "Images");
        assert_eq!(request.filters.len(), 2);

        let result = FileChooserResult {
            paths: vec!["/tmp/a.txt".to_owned()],
            interrupted: false,
            current_filter: Some("Text".to_owned()),
        };
        let (status, response) = chooser_response(&result, &request);
        assert_eq!(status, 0);
        let value = response.get("current_filter").unwrap();
        let filter = FileFilter::from_dbus(value).unwrap();
        assert_eq!(filter.name, "Text");
        assert_eq!(filter.rules, vec![(0u32, "*.txt".to_owned())]);
    }

    #[test]
    fn request_close_sets_the_cancel_flag() {
        let cancel = Arc::new(AtomicBool::new(false));
        let interface = RequestInterface {
            cancel: cancel.clone(),
        };
        interface.close();
        assert!(cancel.load(Ordering::Relaxed));
    }

    struct BlockingChooser;
    impl ChooserLauncher for BlockingChooser {
        fn choose(&self, _: &FileChooserRequest) -> Result<FileChooserResult, String> {
            Ok(FileChooserResult::default())
        }

        fn choose_cancelable(
            &self,
            request: &FileChooserRequest,
            cancel: &AtomicBool,
        ) -> Result<FileChooserResult, String> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !cancel.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(FileChooserResult {
                paths: Vec::new(),
                interrupted: cancel.load(Ordering::Relaxed),
                current_filter: request.current_filter.as_ref().map(|f| f.name.clone()),
            })
        }
    }

    #[test]
    fn live_portal_close_cancels_the_chooser() {
        let bus_name = format!(
            "org.freedesktop.impl.portal.desktop.kitty.cancel{}",
            std::process::id()
        );
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut config = PortalConfig::new(ColorScheme::Dark, "cyan", Contrast::Normal);
        config.bus_name = bus_name.clone();
        config = config.with_chooser(Arc::new(BlockingChooser));
        let flag = shutdown.clone();
        let thread = std::thread::spawn(move || {
            let _ = serve_portal(config, Some(flag));
        });
        let connection = loop {
            if let Ok(connection) = zbus::blocking::Connection::session() {
                break connection;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        // Wait for the name to be owned.
        for _ in 0..200 {
            if connection
                .call_method(
                    Some(bus_name.as_str()),
                    DESKTOP_OBJECT_PATH,
                    Some(SETTINGS_INTERFACE),
                    "Read",
                    &(PORTAL_APPEARANCE_NAMESPACE, PORTAL_COLOR_SCHEME_KEY),
                )
                .is_ok()
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let handle = "/org/freedesktop/portal/desktop/request/cancel/1";
        let caller = connection.clone();
        let chooser_bus_name = bus_name.clone();
        let chooser_thread = std::thread::spawn(move || {
            let empty = HashMap::<String, OwnedValue>::new();
            caller
                .call_method(
                    Some(chooser_bus_name.as_str()),
                    DESKTOP_OBJECT_PATH,
                    Some(FILE_CHOOSER_INTERFACE),
                    "OpenFile",
                    &(handle, "app", "", "Cancel me", empty),
                )
                .map(|reply| {
                    reply
                        .body()
                        .deserialize::<(u32, HashMap<String, OwnedValue>)>()
                        .unwrap()
                })
        });
        let mut closed = false;
        for _ in 0..100 {
            match connection.call_method(
                Some(bus_name.as_str()),
                handle,
                Some("org.freedesktop.impl.portal.Request"),
                "Close",
                &(),
            ) {
                Ok(_) => {
                    closed = true;
                    break;
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
        assert!(closed, "request Close object was never exported");
        let (status, _) = chooser_thread.join().unwrap().unwrap();
        assert_eq!(status, 1);
        shutdown.store(true, Ordering::Relaxed);
        let _ = thread.join();
    }
}
