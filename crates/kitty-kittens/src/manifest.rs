// SPDX-License-Identifier: GPL-3.0-or-later
//! Kitten manifest parsing and serialization.
//!
//! A manifest is a small JSON object stored next to a kitten entry point. It
//! is optional: a plain `foo.py` file or an executable named `foo` is a valid
//! kitten on its own. Manifests add metadata, aliases, a custom entry point and
//! launch defaults.
//!
//! Recognized fields (all optional):
//!
//! | field | type | meaning |
//! |---|---|---|
//! | `name` | string | canonical name, defaults to the file/directory name |
//! | `aliases` | array of strings | additional names |
//! | `description` | string | one-line listing text |
//! | `help` | string | long help text |
//! | `version` | string | kitten version |
//! | `kind` | string | `python`, `executable` or `static` |
//! | `entry` | string | entry point relative to the manifest |
//! | `interpreter` | string | Python interpreter for `python` kittens |
//! | `args` | array of strings | arguments inserted before user arguments |
//! | `env` | object of strings | environment defaults |
//! | `cwd` | string | working directory (relative to the manifest) |
//! | `hidden` | boolean | omit from `list` output |
//! | `run_module` | string | exposed as `KITTY_KITTEN_RUN_MODULE` |
//! | `window_type` | string | metadata for overlay creation |
//!
//! Unknown fields are ignored. This mirrors kitty's tolerance for extra
//! metadata in configuration files.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use kitty_remote::json::Value;

use crate::error::{Error, Result};

/// How a kitten entry point is executed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KittenKind {
    /// A Python source file run with an interpreter.
    Python,
    /// A native executable.
    Executable,
    /// A statically linked binary. This is an explicit manifest annotation; it
    /// is launched exactly like [`KittenKind::Executable`].
    StaticBinary,
}

impl KittenKind {
    /// Stable lowercase name used in manifests and `KITTY_KITTEN_KIND`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Executable => "executable",
            Self::StaticBinary => "static",
        }
    }

    /// Whether this kind needs a Python interpreter.
    pub fn is_python(self) -> bool {
        self == Self::Python
    }

    /// Whether this kind is executed directly.
    pub fn is_native(self) -> bool {
        !self.is_python()
    }
}

impl fmt::Display for KittenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for KittenKind {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "python" | "py" | "script" => Ok(Self::Python),
            "executable" | "exe" | "binary" => Ok(Self::Executable),
            "static" | "static-binary" | "native" => Ok(Self::StaticBinary),
            other => Err(format!(
                "unknown kitten kind {other:?}; expected python, executable or static"
            )),
        }
    }
}

/// Parsed contents of a kitten manifest.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KittenManifest {
    pub name: Option<String>,
    pub aliases: Vec<String>,
    pub description: Option<String>,
    pub help: Option<String>,
    pub version: Option<String>,
    pub kind: Option<KittenKind>,
    pub entry: Option<PathBuf>,
    pub interpreter: Option<PathBuf>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub hidden: bool,
    pub run_module: Option<String>,
    pub window_type: Option<String>,
}

impl KittenManifest {
    /// Read and parse a manifest file.
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).map_err(|source| Error::IoPath {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(path, &bytes)
    }

    /// Parse manifest bytes. `path` is only used for diagnostics.
    pub fn parse(path: &Path, bytes: &[u8]) -> Result<Self> {
        let value = kitty_remote::json::parse(bytes).map_err(|err| Error::InvalidManifest {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
        Self::from_value(path, &value)
    }

    /// Build a manifest from an already parsed JSON value.
    pub fn from_value(path: &Path, value: &Value) -> Result<Self> {
        if !value.is_object() {
            return Err(Error::InvalidManifest {
                path: path.to_path_buf(),
                message: "the top level value must be a JSON object".to_owned(),
            });
        }
        let mut manifest = Self::default();
        if let Some(v) = value.get("name") {
            manifest.name = Some(string_field(path, "name", v)?);
        }
        if let Some(v) = value.get("aliases") {
            manifest.aliases = string_array(path, "aliases", v)?;
        }
        if let Some(v) = value.get("description") {
            manifest.description = Some(string_field(path, "description", v)?);
        }
        if let Some(v) = value.get("help") {
            manifest.help = Some(string_field(path, "help", v)?);
        }
        if let Some(v) = value.get("version") {
            manifest.version = Some(string_field(path, "version", v)?);
        }
        if let Some(v) = value.get("kind") {
            let raw = string_field(path, "kind", v)?;
            let kind =
                KittenKind::from_str(&raw).map_err(|message| Error::InvalidManifestField {
                    path: path.to_path_buf(),
                    field: "kind",
                    message,
                })?;
            manifest.kind = Some(kind);
        }
        if let Some(v) = value.get("entry") {
            manifest.entry = Some(PathBuf::from(string_field(path, "entry", v)?));
        }
        if let Some(v) = value.get("interpreter") {
            manifest.interpreter = Some(PathBuf::from(string_field(path, "interpreter", v)?));
        }
        if let Some(v) = value.get("args") {
            manifest.args = string_array(path, "args", v)?;
        }
        if let Some(v) = value.get("env") {
            let Some(items) = v.as_object() else {
                return Err(field_error(path, "env", "expected an object of strings"));
            };
            for (key, value) in items {
                let Some(value) = value.as_str() else {
                    return Err(field_error(
                        path,
                        "env",
                        format!("value for {key:?} must be a string"),
                    ));
                };
                manifest.env.insert(key.clone(), value.to_owned());
            }
        }
        if let Some(v) = value.get("cwd").or_else(|| value.get("working_directory")) {
            manifest.cwd = Some(PathBuf::from(string_field(path, "cwd", v)?));
        }
        if let Some(v) = value.get("hidden") {
            let Some(hidden) = v.as_bool() else {
                return Err(field_error(path, "hidden", "expected a boolean"));
            };
            manifest.hidden = hidden;
        }
        if let Some(v) = value.get("run_module") {
            manifest.run_module = Some(string_field(path, "run_module", v)?);
        }
        if let Some(v) = value.get("window_type") {
            manifest.window_type = Some(string_field(path, "window_type", v)?);
        }
        Ok(manifest)
    }

    /// Serialize the manifest to a JSON value.
    pub fn to_value(&self) -> Value {
        let mut items: Vec<(String, Value)> = Vec::new();
        if let Some(name) = &self.name {
            items.push(("name".to_owned(), Value::from(name.as_str())));
        }
        if !self.aliases.is_empty() {
            items.push((
                "aliases".to_owned(),
                Value::Array(
                    self.aliases
                        .iter()
                        .map(|x| Value::from(x.as_str()))
                        .collect(),
                ),
            ));
        }
        if let Some(description) = &self.description {
            items.push(("description".to_owned(), Value::from(description.as_str())));
        }
        if let Some(help) = &self.help {
            items.push(("help".to_owned(), Value::from(help.as_str())));
        }
        if let Some(version) = &self.version {
            items.push(("version".to_owned(), Value::from(version.as_str())));
        }
        if let Some(kind) = self.kind {
            items.push(("kind".to_owned(), Value::from(kind.as_str())));
        }
        if let Some(entry) = &self.entry {
            items.push((
                "entry".to_owned(),
                Value::from(entry.to_string_lossy().into_owned()),
            ));
        }
        if let Some(interpreter) = &self.interpreter {
            items.push((
                "interpreter".to_owned(),
                Value::from(interpreter.to_string_lossy().into_owned()),
            ));
        }
        if !self.args.is_empty() {
            items.push((
                "args".to_owned(),
                Value::Array(self.args.iter().map(|x| Value::from(x.as_str())).collect()),
            ));
        }
        if !self.env.is_empty() {
            items.push((
                "env".to_owned(),
                Value::Object(
                    self.env
                        .iter()
                        .map(|(key, value)| (key.clone(), Value::from(value.as_str())))
                        .collect(),
                ),
            ));
        }
        if let Some(cwd) = &self.cwd {
            items.push((
                "cwd".to_owned(),
                Value::from(cwd.to_string_lossy().into_owned()),
            ));
        }
        if self.hidden {
            items.push(("hidden".to_owned(), Value::Bool(true)));
        }
        if let Some(run_module) = &self.run_module {
            items.push(("run_module".to_owned(), Value::from(run_module.as_str())));
        }
        if let Some(window_type) = &self.window_type {
            items.push(("window_type".to_owned(), Value::from(window_type.as_str())));
        }
        Value::Object(items)
    }

    /// Serialize to a compact JSON string.
    pub fn to_json(&self) -> Result<String> {
        Ok(self.to_value().to_json()?)
    }
}

fn string_field(path: &Path, field: &'static str, value: &Value) -> Result<String> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| field_error(path, field, "expected a string"))
}

fn string_array(path: &Path, field: &'static str, value: &Value) -> Result<Vec<String>> {
    let Some(items) = value.as_array() else {
        return Err(field_error(path, field, "expected an array of strings"));
    };
    let mut ans = Vec::with_capacity(items.len());
    for item in items {
        let Some(text) = item.as_str() else {
            return Err(field_error(path, field, "expected an array of strings"));
        };
        ans.push(text.to_owned());
    }
    Ok(ans)
}

fn field_error(path: &Path, field: &'static str, message: impl Into<String>) -> Error {
    Error::InvalidManifestField {
        path: path.to_path_buf(),
        field,
        message: message.into(),
    }
}

/// The conventional manifest filenames checked inside a kitten directory.
pub const MANIFEST_FILENAMES: &[&str] = &["kitten.json", "manifest.json"];

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> PathBuf {
        PathBuf::from("/tmp/example/kitten.json")
    }

    #[test]
    fn parses_all_supported_fields() {
        let json = br#"{
            "name": "demo",
            "aliases": ["d", "de"],
            "description": "A demo",
            "help": "Long help",
            "version": "1.2.3",
            "kind": "python",
            "entry": "run.py",
            "interpreter": "/usr/bin/python3",
            "args": ["--fixed"],
            "env": {"DEMO": "1"},
            "cwd": "subdir",
            "hidden": true,
            "run_module": "demo_askpass",
            "window_type": "overlay"
        }"#;
        let manifest = KittenManifest::parse(&path(), json).unwrap();
        assert_eq!(manifest.name.as_deref(), Some("demo"));
        assert_eq!(manifest.aliases, ["d", "de"]);
        assert_eq!(manifest.kind, Some(KittenKind::Python));
        assert_eq!(manifest.entry.as_deref(), Some(Path::new("run.py")));
        assert_eq!(
            manifest.interpreter.as_deref(),
            Some(Path::new("/usr/bin/python3"))
        );
        assert_eq!(manifest.args, ["--fixed"]);
        assert_eq!(manifest.env.get("DEMO").map(String::as_str), Some("1"));
        assert_eq!(manifest.cwd.as_deref(), Some(Path::new("subdir")));
        assert!(manifest.hidden);
        assert_eq!(manifest.run_module.as_deref(), Some("demo_askpass"));
    }

    #[test]
    fn empty_object_is_a_valid_manifest() {
        let manifest = KittenManifest::parse(&path(), b"{}").unwrap();
        assert_eq!(manifest, KittenManifest::default());
    }

    #[test]
    fn rejects_wrong_field_types() {
        let err = KittenManifest::parse(&path(), br#"{"aliases": "x"}"#).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidManifestField {
                field: "aliases",
                ..
            }
        ));
        let err = KittenManifest::parse(&path(), br#"{"env": {"A": 1}}"#).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidManifestField { field: "env", .. }
        ));
        let err = KittenManifest::parse(&path(), br#"{"hidden": "yes"}"#).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidManifestField {
                field: "hidden",
                ..
            }
        ));
    }

    #[test]
    fn rejects_bad_json_and_unknown_kind() {
        assert!(matches!(
            KittenManifest::parse(&path(), b"{not json"),
            Err(Error::InvalidManifest { .. })
        ));
        let err = KittenManifest::parse(&path(), br#"{"kind": "ruby"}"#).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidManifestField { field: "kind", .. }
        ));
    }

    #[test]
    fn round_trips_through_json() {
        let manifest = KittenManifest {
            name: Some("x".to_owned()),
            aliases: vec!["y".to_owned()],
            kind: Some(KittenKind::StaticBinary),
            args: vec!["a".to_owned()],
            env: BTreeMap::from([("K".to_owned(), "V".to_owned())]),
            hidden: true,
            ..KittenManifest::default()
        };
        let text = manifest.to_json().unwrap();
        let parsed = KittenManifest::parse(&path(), text.as_bytes()).unwrap();
        assert_eq!(parsed, manifest);
    }

    #[test]
    fn kind_parsing_is_forgiving() {
        assert_eq!("script".parse::<KittenKind>(), Ok(KittenKind::Python));
        assert_eq!("binary".parse::<KittenKind>(), Ok(KittenKind::Executable));
        assert_eq!("native".parse::<KittenKind>(), Ok(KittenKind::StaticBinary));
        assert!("ruby".parse::<KittenKind>().is_err());
    }
}
