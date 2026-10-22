// SPDX-License-Identifier: GPL-3.0-or-later
//! Configuration loading: line splitting/continuations and include handling
//! ported from `kitty/conf/utils.py`, plus the `--override key=value`
//! normalization from `kitty/cli.py` (Copyright (C) 2018-2025 Kovid Goyal).

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::expand::{expand_user, expand_vars, os_name};
use crate::glob::{fnmatchcase, glob_paths};
use crate::options::Options;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BadLine {
    pub number: usize,
    pub line: String,
    pub error: String,
    pub file: String,
}

impl fmt::Display for BadLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: {}: {}",
            self.file, self.number, self.line, self.error
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    pub bad_lines: Vec<BadLine>,
}

impl ConfigError {
    pub fn bad_lines(&self) -> &[BadLine] {
        &self.bad_lines
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Failed to load kitty configuration:")?;
        for (idx, line) in self.bad_lines.iter().enumerate() {
            if idx > 0 {
                writeln!(f)?;
            }
            write!(f, "{line}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}

#[derive(Clone, Debug)]
pub struct Config {
    pub options: Options,
    /// Paths that existed and were actually parsed, in load order.
    pub found_paths: Vec<PathBuf>,
    /// Every path passed to `load`, including ones that did not exist.
    pub all_config_paths: Vec<PathBuf>,
    /// Normalized overrides that were applied, in order.
    pub overrides: Vec<String>,
    /// Recoverable parse errors. Strict `load` returns an error if non-empty.
    pub bad_lines: Vec<BadLine>,
    /// Non-fatal diagnostics (missing includes, malformed non-key lines).
    pub warnings: Vec<String>,
    /// Lines for supported options, mirroring kitty's effective-config store.
    pub effective_config_lines: Vec<String>,
}

impl Config {
    /// Load configuration and reject any bad line (unknown keys, invalid
    /// values, recursive includes, unsupported geninclude, ...).
    pub fn load<P: AsRef<Path>, S: AsRef<str>>(
        paths: &[P],
        overrides: &[S],
    ) -> Result<Self, ConfigError> {
        let config = Self::load_lenient(paths, overrides);
        if config.bad_lines.is_empty() {
            Ok(config)
        } else {
            Err(ConfigError {
                bad_lines: config.bad_lines,
            })
        }
    }

    /// Load configuration like kitty does: continue after bad lines and expose
    /// them through [`Config::bad_lines`] / [`Config::warnings`].
    pub fn load_lenient<P: AsRef<Path>, S: AsRef<str>>(paths: &[P], overrides: &[S]) -> Self {
        Self::load_with_base(
            paths,
            overrides,
            &std::env::current_dir().unwrap_or_default(),
        )
    }

    /// Variant of [`Config::load_lenient`] with an explicit directory used to
    /// resolve relative includes appearing in overrides.
    pub fn load_with_base<P: AsRef<Path>, S: AsRef<str>>(
        paths: &[P],
        overrides: &[S],
        override_base: &Path,
    ) -> Self {
        let mut parser = Parser::new();
        let all_config_paths: Vec<PathBuf> =
            paths.iter().map(|p| p.as_ref().to_path_buf()).collect();
        let mut found_paths = Vec::new();

        for path in paths {
            let path = path.as_ref();
            if path.as_os_str().is_empty() {
                continue;
            }
            let (lines, label, base) = if path == Path::new("-") {
                let mut text = String::new();
                if std::io::stdin().read_to_string(&mut text).is_err() {
                    continue;
                }
                (
                    universal_lines(&text),
                    "<stdin>".to_string(),
                    std::env::current_dir().unwrap_or_default(),
                )
            } else {
                let bytes = match fs::read(path) {
                    Ok(bytes) => bytes,
                    Err(err)
                        if matches!(
                            err.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                        ) =>
                    {
                        continue;
                    }
                    Err(err) => {
                        parser.warnings.push(format!(
                            "Could not read config file {}: {err}",
                            path.display()
                        ));
                        continue;
                    }
                };
                let text = String::from_utf8_lossy(&bytes);
                let base = path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                (universal_lines(&text), path.display().to_string(), base)
            };
            found_paths.push(path.to_path_buf());
            parser.parse_source(lines, label, &base, false);
        }

        let mut applied_overrides = Vec::new();
        let mut override_lines = Vec::new();
        for raw in overrides {
            let normalized = normalize_override(raw.as_ref());
            applied_overrides.push(normalized.clone());
            override_lines.extend(universal_lines(&normalized));
        }
        if !applied_overrides.is_empty() {
            parser.parse_source(
                override_lines,
                "<override>".to_string(),
                override_base,
                false,
            );
        }

        Self {
            options: parser.options,
            found_paths,
            all_config_paths,
            overrides: applied_overrides,
            bad_lines: parser.bad_lines,
            warnings: parser.warnings,
            effective_config_lines: parser.effective_config_lines,
        }
    }
}

struct Parser {
    options: Options,
    bad_lines: Vec<BadLine>,
    warnings: Vec<String>,
    effective_config_lines: Vec<String>,
    memory: HashSet<PathBuf>,
}

impl Parser {
    fn new() -> Self {
        Self {
            options: Options::default(),
            bad_lines: Vec::new(),
            warnings: Vec::new(),
            effective_config_lines: Vec::new(),
            memory: HashSet::new(),
        }
    }

    fn parse_source(&mut self, lines: Vec<String>, label: String, base: &Path, _is_include: bool) {
        let mut i = 0usize;
        let mut pending: Option<(String, usize)> = None;
        loop {
            let (mut line, line_num) = match pending.take() {
                Some((line, number)) => (line, number),
                None => {
                    if i >= lines.len() {
                        break;
                    }
                    let line = lines[i].trim_start().to_string();
                    i += 1;
                    (line, i)
                }
            };
            let mut next_line: Option<(String, usize)> = None;
            if i < lines.len() {
                let mut nl = lines[i].trim_start().to_string();
                i += 1;
                let mut nl_num = i;
                while let Some(rest) = nl.strip_prefix('\\') {
                    line = format!("{}{}", line.trim_end_matches('\n'), rest);
                    if i >= lines.len() {
                        nl.clear();
                        break;
                    }
                    nl = lines[i].trim_start().to_string();
                    i += 1;
                    nl_num = i;
                }
                if !nl.is_empty() {
                    next_line = Some((nl, nl_num));
                }
            }
            pending = next_line;
            self.parse_line(&line, line_num, &label, base);
        }
    }

    fn parse_line(&mut self, raw_line: &str, line_num: usize, label: &str, base: &Path) {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            return;
        }
        let Some((key, value)) = split_key_value(line) else {
            self.warnings
                .push(format!("Ignoring invalid config line: {line:?}"));
            return;
        };
        if matches!(key, "include" | "globinclude" | "envinclude" | "geninclude") {
            self.handle_include(key, value, raw_line, line_num, label, base);
            return;
        }
        match self.options.apply(key, value) {
            Ok(true) => self.effective_config_lines.push(line.to_string()),
            Ok(false) => self.bad_lines.push(BadLine {
                number: line_num,
                line: raw_line.trim_end().to_string(),
                error: format!("Ignoring unknown config key: {key}"),
                file: label.to_string(),
            }),
            Err(err) => self.bad_lines.push(BadLine {
                number: line_num,
                line: raw_line.trim_end().to_string(),
                error: err,
                file: label.to_string(),
            }),
        }
    }

    fn handle_include(
        &mut self,
        key: &str,
        value: &str,
        raw_line: &str,
        line_num: usize,
        label: &str,
        base: &Path,
    ) {
        let mut env = BTreeMap::new();
        env.insert("KITTY_OS".to_string(), os_name().to_string());
        let expanded = expand_vars(&expand_user(value.trim()), &env, true);
        match key {
            "globinclude" => match glob_paths(base, &expanded) {
                Ok(paths) => {
                    for path in paths {
                        self.include_file(&path, raw_line, line_num, label);
                    }
                }
                Err(err) => self.bad_lines.push(BadLine {
                    number: line_num,
                    line: raw_line.trim_end().to_string(),
                    error: err,
                    file: label.to_string(),
                }),
            },
            "envinclude" => {
                let mut vars: Vec<(String, String)> = std::env::vars().collect();
                vars.sort_by(|a, b| a.0.cmp(&b.0));
                for (name, val) in vars {
                    if fnmatchcase(&name, &expanded) {
                        self.parse_source(
                            universal_lines(&val),
                            format!("<env var: {name}>"),
                            base,
                            true,
                        );
                    }
                }
            }
            "geninclude" => {
                let path = if Path::new(&expanded).is_absolute() {
                    PathBuf::from(&expanded)
                } else {
                    base.join(&expanded)
                };
                self.bad_lines.push(BadLine {
                    number: line_num,
                    line: raw_line.trim_end().to_string(),
                    error: format!(
                        "geninclude is not supported by this Rust port (refusing to execute {}); directive ignored",
                        path.display()
                    ),
                    file: label.to_string(),
                });
            }
            _ => {
                let path = if Path::new(&expanded).is_absolute() {
                    PathBuf::from(&expanded)
                } else {
                    base.join(&expanded)
                };
                self.include_file(&path, raw_line, line_num, label);
            }
        }
    }

    fn include_file(&mut self, path: &Path, raw_line: &str, line_num: usize, label: &str) {
        let path = normalize_path(path);
        if self.memory.contains(&path) {
            self.bad_lines.push(BadLine {
                number: line_num,
                line: raw_line.trim_end().to_string(),
                error: format!(
                    "The file {} has already been included, ignoring",
                    path.display()
                ),
                file: label.to_string(),
            });
            return;
        }
        self.memory.insert(path.clone());
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                self.warnings.push(format!(
                    "Could not find included config file: {}, ignoring",
                    path.display()
                ));
                return;
            }
            Err(_) => {
                self.warnings.push(format!(
                    "Could not read from included config file: {}, ignoring",
                    path.display()
                ));
                return;
            }
        };
        let text = String::from_utf8_lossy(&bytes);
        let base = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        self.parse_source(
            universal_lines(&text),
            path.display().to_string(),
            &base,
            true,
        );
    }
}

/// Port of `kitty.cli.parse_override`.
pub fn normalize_override(input: &str) -> String {
    let input = input.trim_start();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    if i == 0 {
        return input.to_string();
    }
    let key_end = i;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    if bytes.get(i) == Some(&b'=') {
        let mut ans = String::with_capacity(input.len());
        ans.push_str(&input[..key_end]);
        ans.push(' ');
        ans.push_str(&input[i + 1..]);
        ans
    } else {
        input.to_string()
    }
}

pub(crate) fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    if !bytes.first().is_some_and(u8::is_ascii_alphabetic) {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'-')) {
        i += 1;
    }
    let key_end = i;
    if i >= bytes.len() || !bytes[i].is_ascii_whitespace() {
        return None;
    }
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    Some((&line[..key_end], &line[i..]))
}

pub(crate) fn universal_lines(text: &str) -> Vec<String> {
    let mut ans = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                ans.push(std::mem::take(&mut current));
            }
            '\n' => ans.push(std::mem::take(&mut current)),
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        ans.push(current);
    }
    ans
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let absolute = path.is_absolute();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() && !absolute {
                    out.push("..");
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}
