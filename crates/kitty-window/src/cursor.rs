// SPDX-License-Identifier: GPL-3.0-or-later
//! Cursor theme discovery and hot-reload bookkeeping.
//!
//! Both Linux backends need the same answers: which theme is active, at what
//! pixel size, and whether the on-disk theme changed since the last frame. The
//! actual loading lives in the backends (`Xcursor` on X11 and
//! `libwayland-cursor` on Wayland) so this module stays platform-neutral and
//! unit testable.

use std::env;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

/// Snapshot of the cursor theme configuration and on-disk state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CursorTheme {
    pub name: String,
    pub size: u32,
    pub scale_bits: u64,
    pub mtime: u64,
}

impl CursorTheme {
    pub fn scale(&self) -> f64 {
        f64::from_bits(self.scale_bits)
    }

    pub fn signature(&self) -> (String, u32, u64, u64) {
        (self.name.clone(), self.size, self.scale_bits, self.mtime)
    }
}

/// A cursor theme change observed by the backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorThemeEvent {
    pub previous: CursorTheme,
    pub current: CursorTheme,
}

/// Environment variable consulted for the theme name.
pub const THEME_ENV: &str = "XCURSOR_THEME";
/// Environment variable consulted for the nominal cursor size.
pub const SIZE_ENV: &str = "XCURSOR_SIZE";
/// Search path for themes, colon separated.
pub const PATH_ENV: &str = "XCURSOR_PATH";

/// Read the effective theme name from the environment or a caller override.
pub fn theme_name(override_name: Option<&str>) -> String {
    override_name
        .map(str::to_owned)
        .or_else(|| env::var(THEME_ENV).ok().filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "default".to_owned())
}

/// Read the effective nominal cursor size in pixels.
pub fn theme_size(override_size: Option<u32>) -> u32 {
    override_size
        .or_else(|| {
            env::var(SIZE_ENV)
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|value| *value > 0)
        })
        .unwrap_or(24)
}

/// Compute the scaled pixel size for a per-window cursor scale.
pub fn scaled_size(size: u32, scale: f64) -> u32 {
    ((size.max(1) as f64) * scale.clamp(0.5, 8.0))
        .round()
        .max(1.0) as u32
}

/// Directories that may contain cursor themes, in lookup order.
pub fn theme_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(path) = env::var(PATH_ENV) {
        for entry in path.split(':').filter(|entry| !entry.is_empty()) {
            roots.push(expand_home(entry));
        }
    }
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join(".icons"));
        roots.push(home.join(".local/share/icons"));
    }
    roots.push(PathBuf::from("/usr/share/icons"));
    roots.push(PathBuf::from("/usr/share/pixmaps"));
    roots
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// Newest modification time for a theme directory and one cursor file.
///
/// A theme is hot-reloaded when this value changes. Looking at both the
/// directory and the current cursor file catches package upgrades that replace
/// files as well as themes that add or remove cursor names.
pub fn theme_mtime(theme: &str, cursor_name: &str) -> u64 {
    let mut newest = 0;
    for root in theme_roots() {
        let directory = root.join(theme);
        newest = newest.max(mtime(&directory));
        let cursors = directory.join("cursors");
        newest = newest.max(mtime(&cursors));
        if !cursor_name.is_empty() {
            newest = newest.max(mtime(&cursors.join(cursor_name)));
        }
    }
    newest
}

fn mtime(path: &std::path::Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos() as u64)
}

/// Build a snapshot for the current environment and override values.
pub fn snapshot(
    name_override: Option<&str>,
    size_override: Option<u32>,
    scale: f64,
    cursor_name: &str,
) -> CursorTheme {
    let name = theme_name(name_override);
    let size = theme_size(size_override);
    CursorTheme {
        mtime: theme_mtime(&name, cursor_name),
        name,
        size,
        scale_bits: scale.clamp(0.5, 8.0).to_bits(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_sizes_are_clamped_and_rounded() {
        assert_eq!(scaled_size(24, 1.0), 24);
        assert_eq!(scaled_size(24, 1.5), 36);
        assert_eq!(scaled_size(24, 2.0), 48);
        assert_eq!(scaled_size(1, 0.01), 1);
        assert_eq!(scaled_size(24, 100.0), 192);
    }

    #[test]
    fn snapshot_tracks_scale_and_theme_name() {
        let theme = snapshot(Some("Adwaita"), Some(32), 1.5, "left_ptr");
        assert_eq!(theme.name, "Adwaita");
        assert_eq!(theme.size, 32);
        assert_eq!(theme.scale(), 1.5);
        assert_eq!(theme.signature().0, "Adwaita");
    }
}
