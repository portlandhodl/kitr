// SPDX-License-Identifier: GPL-3.0-or-later
//! Kitten discovery.
//!
//! The registry scans an ordered list of roots. Earlier roots win, so the
//! default order is:
//!
//! 1. `KITTY_KITTEN_PATH` entries ([`KittenSource::Environment`])
//! 2. the user config directory ([`KittenSource::Config`])
//! 3. system data/lib directories ([`KittenSource::System`])
//! 4. built-in kittens shipped with kitty ([`KittenSource::Builtin`])
//!
//! A root may contain:
//!
//! * `name.py` files (Python kittens);
//! * executable files (`name` or `name.exe`), including static binaries;
//! * directories with `main.py`/`main` (or `name.py`/`name`) inside;
//! * directories or files with a `kitten.json`/`manifest.json` manifest.
//!
//! Canonical names always take precedence over aliases, regardless of root
//! order. Within the canonical namespace, the first root (and then the first
//! sorted filename) wins. A manifest may add aliases, but it cannot steal a
//! canonical name from another kitten. The built-in alias `url_hints -> hints`
//! is always available.
//!
//! Missing roots are not errors. Invalid manifests are reported through
//! [`KittenRegistry::diagnostics`] and skipped by [`KittenRegistry::discover`];
//! [`KittenRegistry::try_discover`] returns the first such error instead.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use kitty_remote::json::Value;

use crate::error::{Error, Result};
use crate::manifest::{KittenKind, KittenManifest, MANIFEST_FILENAMES};
use crate::native::native_kitten;

/// Where a kitten was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KittenSource {
    /// An explicitly registered entry.
    Explicit,
    /// A directory listed in `KITTY_KITTEN_PATH`.
    Environment,
    /// The kitty configuration directory.
    Config,
    /// A system-wide data or library directory.
    System,
    /// A kitten shipped with kitty.
    Builtin,
}

impl KittenSource {
    /// Stable lowercase name used in JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Environment => "environment",
            Self::Config => "config",
            Self::System => "system",
            Self::Builtin => "builtin",
        }
    }
}

impl fmt::Display for KittenSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One search root and its precedence class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchRoot {
    pub path: PathBuf,
    pub source: KittenSource,
}

impl SearchRoot {
    pub fn new(path: impl Into<PathBuf>, source: KittenSource) -> Self {
        Self {
            path: path.into(),
            source,
        }
    }
}

/// Configuration for [`KittenRegistry::discover`].
#[derive(Clone, Debug)]
pub struct DiscoveryConfig {
    /// Roots in priority order (highest first).
    pub roots: Vec<SearchRoot>,
    /// Include the static catalog of built-in kitten names in listing output
    /// even when the built-in implementation is not installed.
    pub include_builtin_catalog: bool,
    /// Register kitty's known aliases (`url_hints -> hints`).
    pub add_known_aliases: bool,
    /// How many directory levels below a root to scan. The default of 2 is
    /// enough for `<root>/<kitten>/main.py`.
    pub max_depth: usize,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

impl DiscoveryConfig {
    /// A completely empty configuration. No filesystem probing is performed.
    pub fn empty() -> Self {
        Self {
            roots: Vec::new(),
            include_builtin_catalog: false,
            add_known_aliases: true,
            max_depth: 2,
        }
    }

    /// Build the default search path from environment variables and the
    /// conventional kitty locations.
    pub fn from_env() -> Self {
        let mut config = Self::empty();
        config.include_builtin_catalog = true;

        if let Some(paths) = std::env::var_os("KITTY_KITTEN_PATH") {
            for path in std::env::split_paths(&paths) {
                config.add_root(path, KittenSource::Environment);
            }
        }

        if let Some(dir) = std::env::var_os("KITTY_CONFIG_DIRECTORY") {
            config.add_root(dir, KittenSource::Config);
        }
        if let Some(home) = home_dir() {
            let config_home = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            config.add_root(config_home.join("kitty"), KittenSource::Config);
            if config_home != home.join(".config") {
                config.add_root(home.join(".config/kitty"), KittenSource::Config);
            }
        }

        if let Some(data_dirs) = std::env::var_os("XDG_DATA_DIRS") {
            for dir in std::env::split_paths(&data_dirs) {
                config.add_root(dir.join("kitty/kittens"), KittenSource::System);
            }
        }
        for dir in [
            "/usr/local/share/kitty/kittens",
            "/usr/share/kitty/kittens",
            "/usr/local/lib/kitty/kittens",
            "/usr/lib/kitty/kittens",
            "/opt/homebrew/share/kitty/kittens",
        ] {
            config.add_root(dir, KittenSource::System);
        }

        if let Some(dir) = std::env::var_os("KITTY_KITTEN_BUILTIN_DIR") {
            config.add_root(dir, KittenSource::Builtin);
        }
        if let Some(dir) = std::env::var_os("KITTY_INSTALLATION_DIR") {
            config.add_root(PathBuf::from(dir).join("kittens"), KittenSource::Builtin);
        }
        config
    }

    /// Append a root. Duplicate paths are ignored, keeping the earlier
    /// (higher-priority) entry.
    pub fn add_root(&mut self, path: impl Into<PathBuf>, source: KittenSource) -> &mut Self {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return self;
        }
        if !self.roots.iter().any(|root| root.path == path) {
            self.roots.push(SearchRoot { path, source });
        }
        self
    }

    /// Builder form of [`Self::add_root`].
    pub fn with_root(mut self, path: impl Into<PathBuf>, source: KittenSource) -> Self {
        self.add_root(path, source);
        self
    }
}

/// A kitten discovered on disk, or registered manually.
#[derive(Clone, Debug, PartialEq)]
pub struct KittenEntry {
    /// Canonical name.
    pub name: String,
    /// Alternate names. This includes manifest aliases, the original `.py`
    /// filename and `-`/`_` spelling variants.
    pub aliases: Vec<String>,
    /// The file that is executed.
    pub entry_path: PathBuf,
    /// The manifest used to build this entry, if any.
    pub manifest_path: Option<PathBuf>,
    /// How the entry point is executed.
    pub kind: KittenKind,
    /// Where the entry came from.
    pub source: KittenSource,
    /// Parsed manifest, when present.
    pub manifest: Option<KittenManifest>,
    /// Whether the kitten should be hidden from listings.
    pub hidden: bool,
    pub description: Option<String>,
    pub help: Option<String>,
    pub version: Option<String>,
}

impl KittenEntry {
    /// Create a manual entry with no manifest.
    pub fn new(
        name: impl Into<String>,
        entry_path: impl Into<PathBuf>,
        kind: KittenKind,
        source: KittenSource,
    ) -> Self {
        Self {
            name: name.into(),
            aliases: Vec::new(),
            entry_path: entry_path.into(),
            manifest_path: None,
            kind,
            source,
            manifest: None,
            hidden: false,
            description: None,
            help: None,
            version: None,
        }
    }

    /// Whether this kitten came from the built-in tree.
    pub fn is_builtin(&self) -> bool {
        self.source == KittenSource::Builtin
    }

    /// Directory containing the entry point.
    pub fn directory(&self) -> &Path {
        self.entry_path.parent().unwrap_or_else(|| Path::new("."))
    }

    /// Arguments fixed by the manifest.
    pub fn fixed_args(&self) -> &[String] {
        self.manifest
            .as_ref()
            .map(|manifest| manifest.args.as_slice())
            .unwrap_or_default()
    }

    fn candidate_aliases(&self) -> Vec<String> {
        let mut ans = Vec::new();
        ans.extend(self.aliases.iter().cloned());
        if let Some(manifest) = &self.manifest {
            ans.extend(manifest.aliases.iter().cloned());
        }
        if self.entry_path.extension().is_some_and(|ext| ext == "py") {
            if let (Some(filename), Some(stem)) =
                (self.entry_path.file_name(), self.entry_path.file_stem())
            {
                if stem.to_string_lossy() == self.name {
                    ans.push(filename.to_string_lossy().into_owned());
                }
            }
        }
        add_spelling_variants(&self.name, &mut ans);
        ans
    }
}

/// A built-in kitten that is known to kitty even if its implementation was not
/// found on this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinKitten {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
}

/// The built-in kitten catalog, mirroring `kittens/` in kitty's source tree.
///
/// The `tui` package and helper modules are intentionally excluded.
pub const BUILTIN_KITTENS: &[BuiltinKitten] = &[
    BuiltinKitten {
        name: "ask",
        aliases: &[],
        description: "Ask the user for input",
    },
    BuiltinKitten {
        name: "broadcast",
        aliases: &[],
        description: "Broadcast keystrokes to multiple windows",
    },
    BuiltinKitten {
        name: "choose-files",
        aliases: &["choose_files"],
        description: "Choose files from the filesystem",
    },
    BuiltinKitten {
        name: "choose-fonts",
        aliases: &["choose_fonts"],
        description: "Choose and configure fonts",
    },
    BuiltinKitten {
        name: "clipboard",
        aliases: &[],
        description: "Manage the clipboard",
    },
    BuiltinKitten {
        name: "command-palette",
        aliases: &["command_palette"],
        description: "Browse and trigger actions",
    },
    BuiltinKitten {
        name: "desktop-ui",
        aliases: &["desktop_ui"],
        description: "Desktop UI integration helpers",
    },
    BuiltinKitten {
        name: "diff",
        aliases: &[],
        description: "View diffs with syntax highlighting",
    },
    BuiltinKitten {
        name: "dnd",
        aliases: &[],
        description: "Drag and drop support",
    },
    BuiltinKitten {
        name: "hints",
        aliases: &["url_hints"],
        description: "Select text and URLs with the keyboard",
    },
    BuiltinKitten {
        name: "hyperlinked-grep",
        aliases: &["hyperlinked_grep"],
        description: "Search with hyperlinked results",
    },
    BuiltinKitten {
        name: "icat",
        aliases: &[],
        description: "Display images in the terminal",
    },
    BuiltinKitten {
        name: "notify",
        aliases: &[],
        description: "Send desktop notifications",
    },
    BuiltinKitten {
        name: "pager",
        aliases: &[],
        description: "A pager with images and syntax highlighting",
    },
    BuiltinKitten {
        name: "panel",
        aliases: &[],
        description: "Run a panel in an OS window",
    },
    BuiltinKitten {
        name: "query-terminal",
        aliases: &["query_terminal"],
        description: "Query terminal capabilities",
    },
    BuiltinKitten {
        name: "quick-access-terminal",
        aliases: &["quick_access_terminal"],
        description: "Run the quick access terminal",
    },
    BuiltinKitten {
        name: "remote-file",
        aliases: &["remote_file"],
        description: "Edit files over SSH",
    },
    BuiltinKitten {
        name: "resize-window",
        aliases: &["resize_window"],
        description: "Resize windows interactively",
    },
    BuiltinKitten {
        name: "show-key",
        aliases: &["show_key"],
        description: "Show the escape codes generated by keys",
    },
    BuiltinKitten {
        name: "ssh",
        aliases: &[],
        description: "SSH with kitty integration",
    },
    BuiltinKitten {
        name: "themes",
        aliases: &[],
        description: "Choose a color theme",
    },
    BuiltinKitten {
        name: "transfer",
        aliases: &[],
        description: "Transfer files",
    },
    BuiltinKitten {
        name: "unicode_input",
        aliases: &[],
        description: "Input arbitrary Unicode characters",
    },
];

/// A non-fatal problem encountered during discovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscoveryDiagnostic {
    /// A configured root does not exist or is not a directory.
    MissingRoot(PathBuf),
    /// A root could not be read.
    UnreadableRoot { path: PathBuf, message: String },
    /// A manifest was invalid and the corresponding entry was skipped.
    InvalidManifest { path: PathBuf, message: String },
    /// An individual entry was skipped.
    SkippedEntry { path: PathBuf, message: String },
}

impl fmt::Display for DiscoveryDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRoot(path) => write!(f, "ignoring missing kitten root {}", path.display()),
            Self::UnreadableRoot { path, message } => {
                write!(f, "cannot read kitten root {}: {message}", path.display())
            }
            Self::InvalidManifest { path, message } => {
                write!(f, "ignoring invalid manifest {}: {message}", path.display())
            }
            Self::SkippedEntry { path, message } => {
                write!(f, "ignoring kitten entry {}: {message}", path.display())
            }
        }
    }
}

/// The result of discovery.
#[derive(Clone, Debug)]
pub struct KittenRegistry {
    entries: Vec<KittenEntry>,
    canonical: BTreeMap<String, usize>,
    aliases: BTreeMap<String, usize>,
    diagnostics: Vec<DiscoveryDiagnostic>,
    searched: Vec<PathBuf>,
    catalog: Vec<BuiltinKitten>,
}

impl Default for KittenRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl KittenRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            canonical: BTreeMap::new(),
            aliases: BTreeMap::new(),
            diagnostics: Vec::new(),
            searched: Vec::new(),
            catalog: Vec::new(),
        }
    }

    /// Discover kittens, skipping invalid manifests and recording diagnostics.
    pub fn discover(config: &DiscoveryConfig) -> Self {
        match Self::discover_impl(config, false) {
            Ok(registry) => registry,
            Err(_) => unreachable!("lenient discovery returns Ok"),
        }
    }

    /// Discover kittens, returning an error for the first invalid manifest.
    pub fn try_discover(config: &DiscoveryConfig) -> Result<Self> {
        Self::discover_impl(config, true)
    }

    /// Discover using [`DiscoveryConfig::from_env`].
    pub fn discover_default() -> Self {
        Self::discover(&DiscoveryConfig::from_env())
    }

    fn discover_impl(config: &DiscoveryConfig, strict: bool) -> Result<Self> {
        let mut registry = Self::new();
        registry.searched = config.roots.iter().map(|root| root.path.clone()).collect();
        if config.include_builtin_catalog {
            registry.catalog = BUILTIN_KITTENS.to_vec();
        }
        let mut visited: BTreeSet<PathBuf> = BTreeSet::new();
        for root in &config.roots {
            if !root.path.is_dir() {
                registry
                    .diagnostics
                    .push(DiscoveryDiagnostic::MissingRoot(root.path.clone()));
                continue;
            }
            registry.scan_directory(
                &root.path,
                root.source,
                1,
                config.max_depth,
                &mut visited,
                strict,
            )?;
        }
        registry.rebuild_aliases(config.add_known_aliases);
        Ok(registry)
    }

    fn scan_directory(
        &mut self,
        directory: &Path,
        source: KittenSource,
        depth: usize,
        max_depth: usize,
        visited: &mut BTreeSet<PathBuf>,
        strict: bool,
    ) -> Result<()> {
        let canonical = fs::canonicalize(directory).unwrap_or_else(|_| directory.to_path_buf());
        if !visited.insert(canonical) {
            return Ok(());
        }
        let read = match fs::read_dir(directory) {
            Ok(read) => read,
            Err(err) => {
                self.diagnostics.push(DiscoveryDiagnostic::UnreadableRoot {
                    path: directory.to_path_buf(),
                    message: err.to_string(),
                });
                return Ok(());
            }
        };
        let mut paths: Vec<PathBuf> = read
            .filter_map(|item| item.ok().map(|e| e.path()))
            .collect();
        paths.sort();
        for path in paths {
            let Some(filename) = path.file_name().map(|x| x.to_string_lossy().into_owned()) else {
                continue;
            };
            if filename.starts_with('.') || filename == "__pycache__" {
                continue;
            }
            if path.is_dir() {
                match self.entry_from_directory(&path, source) {
                    Ok(Some(entry)) => self.push_entry(entry),
                    Ok(None) => {
                        if depth < max_depth {
                            self.scan_directory(
                                &path,
                                source,
                                depth + 1,
                                max_depth,
                                visited,
                                strict,
                            )?;
                        }
                    }
                    Err(err) if strict => return Err(err),
                    Err(err) => self.diagnostics.push(DiscoveryDiagnostic::InvalidManifest {
                        path: path.clone(),
                        message: err.to_string(),
                    }),
                }
            } else if path.is_file() {
                match self.entry_from_file(&path, source) {
                    Ok(Some(entry)) => self.push_entry(entry),
                    Ok(None) => {}
                    Err(err) if strict => return Err(err),
                    Err(err) => self.diagnostics.push(DiscoveryDiagnostic::InvalidManifest {
                        path: path.clone(),
                        message: err.to_string(),
                    }),
                }
            }
        }
        Ok(())
    }

    fn entry_from_directory(
        &self,
        directory: &Path,
        source: KittenSource,
    ) -> Result<Option<KittenEntry>> {
        let Some(dirname) = directory
            .file_name()
            .map(|x| x.to_string_lossy().into_owned())
        else {
            return Ok(None);
        };
        let mut manifest_candidates: Vec<PathBuf> = MANIFEST_FILENAMES
            .iter()
            .map(|name| directory.join(name))
            .collect();
        manifest_candidates.push(directory.join(format!("{dirname}.json")));
        manifest_candidates.push(directory.join(format!("{dirname}.kitten.json")));
        let mut first_manifest_error: Option<Error> = None;
        for candidate in manifest_candidates {
            if !candidate.is_file() {
                continue;
            }
            match KittenManifest::load(&candidate) {
                Ok(manifest) => {
                    return Ok(Some(self.entry_from_manifest(
                        &candidate,
                        Some(directory.to_path_buf()),
                        &dirname,
                        manifest,
                        source,
                    )));
                }
                Err(err) => {
                    if first_manifest_error.is_none() {
                        first_manifest_error = Some(err);
                    }
                }
            }
        }
        if let Some(err) = first_manifest_error {
            return Err(err);
        }

        if let Some(entry_path) = default_entry_in(directory, &dirname) {
            let kind = infer_kind(&entry_path);
            return Ok(Some(KittenEntry::new(dirname, entry_path, kind, source)));
        }
        Ok(None)
    }

    fn entry_from_file(&self, path: &Path, source: KittenSource) -> Result<Option<KittenEntry>> {
        let Some(filename) = path.file_name().map(|x| x.to_string_lossy().into_owned()) else {
            return Ok(None);
        };
        if filename.ends_with(".json") {
            let manifest = KittenManifest::load(path)?;
            if manifest.entry.is_none() {
                return Err(Error::InvalidManifest {
                    path: path.to_path_buf(),
                    message: "a standalone manifest must specify an \"entry\"".to_owned(),
                });
            }
            let stem = path
                .file_stem()
                .map(|x| x.to_string_lossy().into_owned())
                .unwrap_or_else(|| filename.clone());
            return Ok(Some(
                self.entry_from_manifest(path, None, &stem, manifest, source),
            ));
        }
        if filename.ends_with(".py") {
            let name = filename.trim_end_matches(".py").to_owned();
            return Ok(Some(KittenEntry::new(
                name,
                path.to_path_buf(),
                KittenKind::Python,
                source,
            )));
        }
        if is_executable(path) {
            let name = filename
                .strip_suffix(".exe")
                .unwrap_or(&filename)
                .to_owned();
            return Ok(Some(KittenEntry::new(
                name,
                path.to_path_buf(),
                infer_kind(path),
                source,
            )));
        }
        Ok(None)
    }

    fn entry_from_manifest(
        &self,
        manifest_path: &Path,
        directory: Option<PathBuf>,
        fallback_name: &str,
        manifest: KittenManifest,
        source: KittenSource,
    ) -> KittenEntry {
        let base = directory
            .or_else(|| manifest_path.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        let entry_path = match &manifest.entry {
            Some(entry) if entry.is_absolute() => entry.clone(),
            Some(entry) => base.join(entry),
            None => default_entry_in(&base, fallback_name).unwrap_or_else(|| base.join("main.py")),
        };
        let kind = manifest.kind.unwrap_or_else(|| infer_kind(&entry_path));
        let name = manifest.name.clone().unwrap_or_else(|| {
            fallback_name
                .strip_suffix(".json")
                .unwrap_or(fallback_name)
                .to_owned()
        });
        let aliases = manifest.aliases.clone();
        KittenEntry {
            name,
            aliases,
            entry_path,
            manifest_path: Some(manifest_path.to_path_buf()),
            kind,
            source,
            hidden: manifest.hidden,
            description: manifest.description.clone(),
            help: manifest.help.clone(),
            version: manifest.version.clone(),
            manifest: Some(manifest),
        }
    }

    fn push_entry(&mut self, mut entry: KittenEntry) {
        if validate_name(&entry.name).is_err() {
            self.diagnostics.push(DiscoveryDiagnostic::SkippedEntry {
                path: entry.entry_path.clone(),
                message: format!("invalid kitten name {:?}", entry.name),
            });
            return;
        }
        if self.canonical.contains_key(&entry.name) {
            return;
        }
        let mut aliases = entry.candidate_aliases();
        dedup_strings(&mut aliases);
        aliases.retain(|alias| alias != &entry.name);
        entry.aliases = aliases;
        let index = self.entries.len();
        self.canonical.insert(entry.name.clone(), index);
        self.entries.push(entry);
    }

    /// Register an entry manually. Later registrations with an existing
    /// canonical name are ignored.
    pub fn register(&mut self, entry: KittenEntry) {
        self.push_entry(entry);
        self.rebuild_aliases(true);
    }

    fn rebuild_aliases(&mut self, add_known_aliases: bool) {
        self.aliases.clear();
        let canonical: BTreeSet<String> = self.canonical.keys().cloned().collect();
        for (index, entry) in self.entries.iter().enumerate() {
            for alias in &entry.aliases {
                if validate_name(alias).is_ok()
                    && !canonical.contains(alias)
                    && !self.aliases.contains_key(alias)
                {
                    self.aliases.insert(alias.clone(), index);
                }
            }
        }
        if add_known_aliases && !canonical.contains("url_hints") {
            if let Some(&index) = self.canonical.get("hints") {
                self.aliases.entry("url_hints".to_owned()).or_insert(index);
            }
        }
    }

    /// All discovered entries in priority order.
    pub fn entries(&self) -> &[KittenEntry] {
        &self.entries
    }

    /// Non-fatal discovery diagnostics.
    pub fn diagnostics(&self) -> &[DiscoveryDiagnostic] {
        &self.diagnostics
    }

    /// The built-in catalog, if enabled.
    pub fn catalog(&self) -> &[BuiltinKitten] {
        &self.catalog
    }

    /// Roots that were searched.
    pub fn searched_paths(&self) -> &[PathBuf] {
        &self.searched
    }

    /// Number of discovered entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no entries were discovered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Look up a canonical name only.
    pub fn get(&self, name: &str) -> Option<&KittenEntry> {
        self.canonical
            .get(name)
            .and_then(|index| self.entries.get(*index))
    }

    /// Look up a canonical name or alias, including `-`/`_` spelling
    /// variants.
    pub fn lookup(&self, name: &str) -> Option<&KittenEntry> {
        if let Some(entry) = self.get(name) {
            return Some(entry);
        }
        if let Some(entry) = self.aliases.get(name) {
            return self.entries.get(*entry);
        }
        if name.contains('-') || name.contains('_') {
            let alternate = if name.contains('-') {
                name.replace('-', "_")
            } else {
                name.replace('_', "-")
            };
            if let Some(entry) = self.get(&alternate) {
                return Some(entry);
            }
            if let Some(entry) = self.aliases.get(&alternate) {
                return self.entries.get(*entry);
            }
        }
        None
    }

    /// Resolve a name or alias, returning a structured error if not found.
    pub fn resolve(&self, name: &str) -> Result<&KittenEntry> {
        self.lookup(name).ok_or_else(|| Error::NotFound {
            name: name.to_owned(),
            searched: self.searched.clone(),
        })
    }

    /// Canonical names, sorted.
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.canonical.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// All aliases mapped to their canonical names, sorted by alias.
    pub fn alias_map(&self) -> BTreeMap<&str, &str> {
        self.aliases
            .iter()
            .filter_map(|(alias, index)| {
                self.entries
                    .get(*index)
                    .map(|entry| (alias.as_str(), entry.name.as_str()))
            })
            .collect()
    }

    /// Human-readable listing used by `kitten list`.
    pub fn list_text(&self) -> String {
        let mut lines = Vec::new();
        let mut visible: Vec<&KittenEntry> =
            self.entries.iter().filter(|entry| !entry.hidden).collect();
        visible.sort_by(|a, b| a.name.cmp(&b.name));
        for entry in &visible {
            let description = entry
                .description
                .as_deref()
                .or_else(|| catalog_description(&entry.name))
                .unwrap_or("(no description)");
            lines.push(format!(
                "{}\t{}\t[{}]",
                entry.name, description, entry.source
            ));
        }
        let discovered: BTreeSet<&str> = self.canonical.keys().map(String::as_str).collect();
        for builtin in &self.catalog {
            if !discovered.contains(builtin.name) {
                let state = if native_kitten(builtin.name).is_some() {
                    "builtin, native"
                } else {
                    "builtin, not installed"
                };
                lines.push(format!(
                    "{}\t{}\t[{state}]",
                    builtin.name, builtin.description
                ));
            }
        }
        if lines.is_empty() {
            "No kittens found.".to_owned()
        } else {
            lines.join("\n")
        }
    }

    /// Human-readable help for `kitten help NAME`.
    pub fn help_text(&self, name: &str) -> Result<String> {
        if let Some(entry) = self.lookup(name) {
            let mut lines = vec![format!("{} ({})", entry.name, entry.kind)];
            if let Some(description) = &entry.description {
                lines.push(String::new());
                lines.push(description.clone());
            }
            if let Some(help) = &entry.help {
                lines.push(String::new());
                lines.push(help.clone());
            }
            lines.push(String::new());
            lines.push(format!("Usage: kitten run {} [args...]", entry.name));
            lines.push(format!("Executable: {}", entry.entry_path.display()));
            if let Some(manifest_path) = &entry.manifest_path {
                lines.push(format!("Manifest: {}", manifest_path.display()));
            }
            if !entry.aliases.is_empty() {
                lines.push(format!("Aliases: {}", entry.aliases.join(", ")));
            }
            if let Some(version) = &entry.version {
                lines.push(format!("Version: {version}"));
            }
            lines.push(format!("Source: {}", entry.source));
            return Ok(lines.join("\n"));
        }
        if let Some(builtin) = self.catalog.iter().find(|item| item.name == name) {
            if let Some(native) = native_kitten(builtin.name) {
                let mut lines = vec![
                    format!("{} (built-in, native)", native.name),
                    String::new(),
                    native.description.to_owned(),
                    String::new(),
                    native.usage.to_owned(),
                ];
                if !native.aliases.is_empty() {
                    lines.push(String::new());
                    lines.push(format!("Aliases: {}", native.aliases.join(", ")));
                }
                return Ok(lines.join("\n"));
            }
            let mut lines = vec![
                format!("{} (built-in)", builtin.name),
                String::new(),
                builtin.description.to_owned(),
                String::new(),
                format!("Usage: kitten run {} [args...]", builtin.name),
                "This built-in kitten implementation is not installed.".to_owned(),
            ];
            if !builtin.aliases.is_empty() {
                lines.push(format!("Aliases: {}", builtin.aliases.join(", ")));
            }
            return Ok(lines.join("\n"));
        }
        Err(Error::NotFound {
            name: name.to_owned(),
            searched: self.searched.clone(),
        })
    }

    /// Render manifest JSON for one kitten, or for all kittens when `name` is
    /// `None`.
    ///
    /// A single kitten is rendered as an object; the full listing is an array.
    pub fn manifest_json(&self, name: Option<&str>) -> Result<String> {
        let value = match name {
            Some(name) => self.entry_value(name)?,
            None => {
                let mut values = Vec::new();
                let mut names = self.names();
                names.sort_unstable();
                for name in names {
                    if let Some(entry) = self.get(name) {
                        values.push(entry.to_value());
                    }
                }
                let discovered: BTreeSet<&str> =
                    self.canonical.keys().map(String::as_str).collect();
                for builtin in &self.catalog {
                    if !discovered.contains(builtin.name) {
                        values.push(catalog_value(builtin));
                    }
                }
                Value::Array(values)
            }
        };
        Ok(value.to_json()?)
    }

    fn entry_value(&self, name: &str) -> Result<Value> {
        if let Some(entry) = self.lookup(name) {
            return Ok(entry.to_value());
        }
        if let Some(builtin) = self.catalog.iter().find(|item| item.name == name) {
            return Ok(catalog_value(builtin));
        }
        Err(Error::NotFound {
            name: name.to_owned(),
            searched: self.searched.clone(),
        })
    }
}

impl KittenEntry {
    /// Serialize this entry as manifest/list JSON.
    pub fn to_value(&self) -> Value {
        let mut items: Vec<(String, Value)> = vec![
            ("name".to_owned(), Value::from(self.name.as_str())),
            (
                "aliases".to_owned(),
                Value::Array(
                    self.aliases
                        .iter()
                        .map(|x| Value::from(x.as_str()))
                        .collect(),
                ),
            ),
            ("kind".to_owned(), Value::from(self.kind.as_str())),
            ("source".to_owned(), Value::from(self.source.as_str())),
            (
                "path".to_owned(),
                Value::from(self.entry_path.to_string_lossy().into_owned()),
            ),
            ("available".to_owned(), Value::Bool(true)),
            ("hidden".to_owned(), Value::Bool(self.hidden)),
        ];
        if let Some(path) = &self.manifest_path {
            items.push((
                "manifest_path".to_owned(),
                Value::from(path.to_string_lossy().into_owned()),
            ));
        }
        if let Some(manifest) = &self.manifest {
            items.push(("manifest".to_owned(), manifest.to_value()));
        }
        if let Some(value) = &self.description {
            items.push(("description".to_owned(), Value::from(value.as_str())));
        }
        if let Some(value) = &self.help {
            items.push(("help".to_owned(), Value::from(value.as_str())));
        }
        if let Some(value) = &self.version {
            items.push(("version".to_owned(), Value::from(value.as_str())));
        }
        Value::Object(items)
    }
}

fn catalog_value(builtin: &BuiltinKitten) -> Value {
    Value::object_from([
        ("name", Value::from(builtin.name)),
        (
            "aliases",
            Value::Array(builtin.aliases.iter().map(|x| Value::from(*x)).collect()),
        ),
        ("kind", Value::Null),
        ("source", Value::from("builtin")),
        ("description", Value::from(builtin.description)),
        ("available", Value::Bool(false)),
        ("hidden", Value::Bool(false)),
    ])
}

fn catalog_description(name: &str) -> Option<&'static str> {
    BUILTIN_KITTENS
        .iter()
        .find(|entry| entry.name == name)
        .map(|entry| entry.description)
}

fn default_entry_in(directory: &Path, name: &str) -> Option<PathBuf> {
    [
        directory.join("main.py"),
        directory.join("main"),
        directory.join(format!("{name}.py")),
        directory.join(name),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}

fn infer_kind(path: &Path) -> KittenKind {
    if path.extension().is_some_and(|ext| ext == "py") {
        KittenKind::Python
    } else {
        KittenKind::Executable
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

/// Validate a canonical name or alias.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name == "."
        || name == ".."
    {
        return Err(Error::InvalidName(name.to_owned()));
    }
    Ok(())
}

fn add_spelling_variants(name: &str, out: &mut Vec<String>) {
    if name.contains('-') {
        out.push(name.replace('-', "_"));
    } else if name.contains('_') {
        out.push(name.replace('_', "-"));
    }
}

fn dedup_strings(values: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "kitty-kittens-discovery-{}-{}-{}",
                std::process::id(),
                label,
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    fn discovers_python_files_and_executables() {
        let dir = TempDir::new("basic");
        write(&dir.path.join("alpha.py"), "print('a')");
        write(&dir.path.join("beta.py"), "print('b')");
        let exe = dir.path.join("gamma");
        write(&exe, "#!/bin/sh\n");
        #[cfg(unix)]
        make_executable(&exe);
        let config = DiscoveryConfig::empty().with_root(&dir.path, KittenSource::Config);
        let registry = KittenRegistry::discover(&config);
        assert_eq!(registry.len(), 3);
        assert_eq!(registry.get("alpha").unwrap().kind, KittenKind::Python);
        assert_eq!(
            registry.get("beta").unwrap().entry_path,
            dir.path.join("beta.py")
        );
        if cfg!(unix) {
            assert_eq!(registry.get("gamma").unwrap().kind, KittenKind::Executable);
        }
    }

    #[test]
    fn discovers_directory_with_main_py() {
        let dir = TempDir::new("dir-main");
        write(&dir.path.join("pretty/main.py"), "print('x')");
        let config = DiscoveryConfig::empty().with_root(&dir.path, KittenSource::Builtin);
        let registry = KittenRegistry::discover(&config);
        let entry = registry.get("pretty").unwrap();
        assert_eq!(entry.kind, KittenKind::Python);
        assert!(entry.is_builtin());
        assert_eq!(entry.entry_path, dir.path.join("pretty/main.py"));
    }

    #[test]
    fn discovers_manifest_directory_and_aliases() {
        let dir = TempDir::new("manifest-dir");
        write(
            &dir.path.join("demo/kitten.json"),
            r#"{
            "name": "demo-kitten",
            "aliases": ["dk", "demo_alias"],
            "description": "Demo",
            "kind": "python",
            "entry": "run.py",
            "args": ["--fixed"],
            "env": {"DEMO": "yes"}
        }"#,
        );
        write(&dir.path.join("demo/run.py"), "print('x')");
        let config = DiscoveryConfig::empty().with_root(&dir.path, KittenSource::Environment);
        let registry = KittenRegistry::discover(&config);
        let entry = registry.get("demo-kitten").unwrap();
        assert_eq!(entry.aliases, ["dk", "demo_alias", "demo_kitten"]);
        assert_eq!(registry.lookup("dk").unwrap().name, "demo-kitten");
        assert_eq!(registry.lookup("demo_kitten").unwrap().name, "demo-kitten");
        assert_eq!(entry.fixed_args(), ["--fixed"]);
        assert_eq!(
            entry
                .manifest
                .as_ref()
                .unwrap()
                .env
                .get("DEMO")
                .map(String::as_str),
            Some("yes")
        );
    }

    #[test]
    fn canonical_names_beat_aliases_from_higher_priority_roots() {
        let root = TempDir::new("canonical-beats-alias");
        let high = root.path.join("high");
        let low = root.path.join("low");
        write(
            &high.join("first/kitten.json"),
            r#"{"name": "first", "aliases": ["second"], "entry": "main.py"}"#,
        );
        write(&high.join("first/main.py"), "");
        write(&low.join("second.py"), "");
        let config = DiscoveryConfig::empty()
            .with_root(&high, KittenSource::Config)
            .with_root(&low, KittenSource::System);
        let registry = KittenRegistry::discover(&config);
        assert_eq!(registry.resolve("second").unwrap().name, "second");
        assert_eq!(
            registry.resolve("second").unwrap().source,
            KittenSource::System
        );
    }

    #[test]
    fn earlier_roots_win_for_canonical_names() {
        let root = TempDir::new("priority");
        let high = root.path.join("high");
        let low = root.path.join("low");
        write(&high.join("same.py"), "high");
        write(&low.join("same.py"), "low");
        let config = DiscoveryConfig::empty()
            .with_root(&high, KittenSource::Config)
            .with_root(&low, KittenSource::System);
        let registry = KittenRegistry::discover(&config);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.get("same").unwrap().source, KittenSource::Config);
        assert_eq!(
            registry.get("same").unwrap().entry_path,
            high.join("same.py")
        );
    }

    #[test]
    fn environment_root_outranks_everything_else() {
        let root = TempDir::new("env-priority");
        let env = root.path.join("env");
        let builtin = root.path.join("builtin");
        write(&env.join("x.py"), "");
        write(&builtin.join("x.py"), "");
        let config = DiscoveryConfig::empty()
            .with_root(&env, KittenSource::Environment)
            .with_root(&builtin, KittenSource::Builtin);
        let registry = KittenRegistry::discover(&config);
        assert_eq!(registry.get("x").unwrap().source, KittenSource::Environment);
    }

    #[test]
    fn missing_roots_are_ignored_and_diagnosed() {
        let dir = TempDir::new("missing");
        let missing = dir.path.join("does-not-exist");
        let config = DiscoveryConfig::empty().with_root(&missing, KittenSource::Config);
        let registry = KittenRegistry::discover(&config);
        assert!(registry.is_empty());
        assert_eq!(
            registry.diagnostics(),
            &[DiscoveryDiagnostic::MissingRoot(missing)]
        );
    }

    #[test]
    fn invalid_manifest_is_lenient_or_strict() {
        let dir = TempDir::new("invalid-manifest");
        write(&dir.path.join("broken/kitten.json"), "{");
        let config = DiscoveryConfig::empty().with_root(&dir.path, KittenSource::Config);
        let registry = KittenRegistry::discover(&config);
        assert!(registry.is_empty());
        assert!(matches!(
            registry.diagnostics().first(),
            Some(DiscoveryDiagnostic::InvalidManifest { .. })
        ));
        assert!(KittenRegistry::try_discover(&config).is_err());
    }

    #[test]
    fn builtin_alias_url_hints_resolves_to_hints() {
        let dir = TempDir::new("builtin-alias");
        write(&dir.path.join("hints.py"), "");
        let config = DiscoveryConfig::empty().with_root(&dir.path, KittenSource::Builtin);
        let registry = KittenRegistry::discover(&config);
        assert_eq!(registry.resolve("url_hints").unwrap().name, "hints");
        assert_eq!(
            registry.alias_map().get("url_hints").copied(),
            Some("hints")
        );
    }

    #[test]
    fn standalone_manifest_requires_entry() {
        let dir = TempDir::new("standalone-manifest");
        write(&dir.path.join("thing.json"), r#"{"name": "thing"}"#);
        let config = DiscoveryConfig::empty().with_root(&dir.path, KittenSource::Config);
        let registry = KittenRegistry::discover(&config);
        assert!(registry.is_empty());
        assert!(matches!(
            registry.diagnostics().first(),
            Some(DiscoveryDiagnostic::InvalidManifest { .. })
        ));
    }

    #[test]
    fn hidden_kittens_are_not_listed_but_resolve() {
        let dir = TempDir::new("hidden");
        write(
            &dir.path.join("secret/kitten.json"),
            r#"{"name": "secret", "hidden": true, "entry": "main.py"}"#,
        );
        write(&dir.path.join("secret/main.py"), "");
        let config = DiscoveryConfig::empty().with_root(&dir.path, KittenSource::Config);
        let registry = KittenRegistry::discover(&config);
        assert!(registry.resolve("secret").is_ok());
        assert!(!registry.list_text().contains("secret"));
    }

    #[test]
    fn list_help_and_manifest_json() {
        let dir = TempDir::new("listing");
        write(
            &dir.path.join("demo/kitten.json"),
            r#"{"name": "demo", "aliases": ["d"], "description": "Demo kitten", "help": "Long help", "version": "1.0", "entry": "main.py"}"#,
        );
        write(&dir.path.join("demo/main.py"), "");
        let config = DiscoveryConfig::empty().with_root(&dir.path, KittenSource::Config);
        let mut registry = KittenRegistry::discover(&config);
        registry.catalog = BUILTIN_KITTENS.to_vec();
        let listing = registry.list_text();
        assert!(listing.contains("demo\tDemo kitten\t[config]"));
        assert!(listing.contains("hints\t"));
        let help = registry.help_text("d").unwrap();
        assert!(help.contains("Demo kitten"));
        assert!(help.contains("Long help"));
        assert!(help.contains("Version: 1.0"));
        assert!(help.contains("Aliases: d"));
        let one = registry.manifest_json(Some("d")).unwrap();
        assert!(one.starts_with('{'));
        assert!(one.contains("\"available\":true"));
        let all = registry.manifest_json(None).unwrap();
        assert!(all.starts_with('['));
        assert!(all.contains("\"available\":false"));
        assert!(registry.manifest_json(Some("nope")).is_err());
    }

    #[test]
    fn hyphen_underscore_names_are_interchangeable() {
        let dir = TempDir::new("spelling");
        write(&dir.path.join("foo-bar.py"), "");
        let config = DiscoveryConfig::empty().with_root(&dir.path, KittenSource::Config);
        let registry = KittenRegistry::discover(&config);
        assert_eq!(registry.resolve("foo_bar").unwrap().name, "foo-bar");
        assert_eq!(registry.resolve("foo-bar").unwrap().name, "foo-bar");
        assert_eq!(registry.resolve("foo-bar.py").unwrap().name, "foo-bar");
    }

    #[test]
    fn duplicate_roots_are_ignored() {
        let mut config = DiscoveryConfig::empty();
        config.add_root("/tmp/a", KittenSource::Config);
        config.add_root("/tmp/a", KittenSource::Builtin);
        assert_eq!(config.roots.len(), 1);
        assert_eq!(config.roots[0].source, KittenSource::Config);
    }
}
