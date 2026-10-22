// SPDX-License-Identifier: GPL-3.0-or-later
//! Font family and font specification parsing.
//!
//! This is a port of `FontSpec.from_setting()` from
//! `kitty/fonts/__init__.py` and `kitty/options/utils.py`, including its small
//! `shlex`-style tokenizer. Unlike kitty, family values are also exposed as a
//! comma-separated list through [`FontSpec::families`].

use std::fmt;

use crate::error::{Error, Result};
use crate::shaping::Feature;

/// The four terminal font styles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FontStyle {
    #[default]
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl FontStyle {
    pub fn bold(self) -> bool {
        matches!(self, Self::Bold | Self::BoldItalic)
    }

    pub fn italic(self) -> bool {
        matches!(self, Self::Italic | Self::BoldItalic)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::Bold => "bold",
            Self::Italic => "italic",
            Self::BoldItalic => "bold-italic",
        }
    }
}

/// A parsed `font_family`-style setting.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FontSpec {
    pub family: Option<String>,
    pub style: Option<String>,
    pub postscript_name: Option<String>,
    pub full_name: Option<String>,
    pub system: Option<String>,
    pub axes: Vec<(String, f64)>,
    pub variable_name: Option<String>,
    pub features: Vec<Feature>,
    pub created_from_string: String,
}

impl FontSpec {
    /// Parse kitty's font specification syntax.
    ///
    /// A bare value such as `Fira Code` or `monospace` is a system/family
    /// string. A value containing `key=value` tokens uses the explicit syntax
    /// `family=... style=... postscript_name=... full_name=... variable_name=...
    /// features="..." wght=...`.
    pub fn from_setting(spec: &str) -> Result<Self> {
        if spec == "auto" {
            return Ok(Self {
                system: Some("auto".into()),
                created_from_string: spec.into(),
                ..Self::default()
            });
        }
        let items = split_shell(spec)?;
        if items.first().is_none_or(|item| !item.contains('=')) {
            return Ok(Self {
                system: Some(spec.to_string()),
                created_from_string: spec.into(),
                ..Self::default()
            });
        }

        let mut ans = Self {
            created_from_string: spec.into(),
            ..Self::default()
        };
        for item in items {
            let (key, value) = item
                .split_once('=')
                .ok_or_else(|| Error::InvalidFontSpec(format!("{item} does not contain an =")))?;
            match key {
                "family" => ans.family = Some(value.to_string()),
                "style" => ans.style = Some(value.to_string()),
                "full_name" => ans.full_name = Some(value.to_string()),
                "postscript_name" => ans.postscript_name = Some(value.to_string()),
                "variable_name" => ans.variable_name = Some(value.to_string()),
                "features" => {
                    for feature in value.split_whitespace() {
                        ans.features.push(Feature::parse(feature)?);
                    }
                }
                _ => {
                    let axis = value
                        .parse::<f64>()
                        .map_err(|_| Error::InvalidFontSpec(format!("{value} is not a number")))?;
                    ans.axes.push((key.to_string(), axis));
                }
            }
        }
        Ok(ans)
    }

    /// Comma-separated family list (kitty passes Fontconfig family lists
    /// through unchanged; this exposes the individual names as well).
    pub fn families(&self) -> Vec<&str> {
        let raw = self
            .family
            .as_deref()
            .or(self.system.as_deref())
            .unwrap_or("");
        raw.split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect()
    }

    pub fn primary_family(&self) -> Option<&str> {
        self.families().into_iter().next()
    }

    pub fn is_auto(&self) -> bool {
        self.system.as_deref() == Some("auto")
    }

    pub fn is_system(&self) -> bool {
        self.system.is_some()
    }

    pub fn as_setting(&self) -> String {
        if !self.created_from_string.is_empty() {
            return self.created_from_string.clone();
        }
        if let Some(system) = &self.system {
            return system.clone();
        }
        let mut parts = Vec::new();
        let mut push = |key: &str, value: &str| {
            parts.push(format!("{key}={}", quote(value)));
        };
        if let Some(v) = &self.family {
            push("family", v);
        }
        if let Some(v) = &self.postscript_name {
            push("postscript_name", v);
        }
        if let Some(v) = &self.full_name {
            push("full_name", v);
        }
        if let Some(v) = &self.variable_name {
            push("variable_name", v);
        }
        if let Some(v) = &self.style {
            push("style", v);
        }
        if !self.features.is_empty() {
            let features: Vec<String> = self.features.iter().map(ToString::to_string).collect();
            push("features", &features.join(" "));
        }
        for (key, value) in &self.axes {
            push(key, &format!("{value}"));
        }
        parts.join(" ")
    }
}

impl fmt::Display for FontSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_setting())
    }
}

fn quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-+.,:/".contains(c))
    {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// Tokenize a font setting the way Python's `shlex.split()` does for the
/// subset kitty uses (whitespace, single/double quotes, backslash escapes).
fn split_shell(input: &str) -> Result<Vec<String>> {
    let mut ans = Vec::new();
    let mut current = String::new();
    let mut token_started = false;
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            c if c.is_whitespace() => {
                if token_started {
                    ans.push(std::mem::take(&mut current));
                    token_started = false;
                }
            }
            '\'' => {
                token_started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => {
                            return Err(Error::InvalidFontSpec("unterminated single quote".into()))
                        }
                    }
                }
            }
            '"' => {
                token_started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => current.push(c),
                            Some('\n') => {}
                            Some(c) => {
                                current.push('\\');
                                current.push(c);
                            }
                            None => {
                                return Err(Error::InvalidFontSpec(
                                    "unterminated escape in double quote".into(),
                                ))
                            }
                        },
                        Some(c) => current.push(c),
                        None => {
                            return Err(Error::InvalidFontSpec("unterminated double quote".into()))
                        }
                    }
                }
            }
            '\\' => {
                token_started = true;
                match chars.next() {
                    Some('\n') => {}
                    Some(c) => current.push(c),
                    None => current.push('\\'),
                }
            }
            c => {
                token_started = true;
                current.push(c);
            }
        }
    }
    if token_started {
        ans.push(current);
    }
    Ok(ans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_family() {
        let spec = FontSpec::from_setting("Fira Code").unwrap();
        assert_eq!(spec.system.as_deref(), Some("Fira Code"));
        assert_eq!(spec.primary_family(), Some("Fira Code"));
    }

    #[test]
    fn parses_explicit_spec() {
        let spec = FontSpec::from_setting(
            "family=\"Liberation Mono\" style=Bold features=\"-dlig ss01\" wght=550",
        )
        .unwrap();
        assert_eq!(spec.family.as_deref(), Some("Liberation Mono"));
        assert_eq!(spec.style.as_deref(), Some("Bold"));
        assert_eq!(spec.features.len(), 2);
        assert_eq!(spec.axes, vec![("wght".to_string(), 550.0)]);
    }

    #[test]
    fn parses_family_list() {
        let spec = FontSpec::from_setting("family=\"Fira Code, Iosevka, monospace\"").unwrap();
        assert_eq!(spec.families(), vec!["Fira Code", "Iosevka", "monospace"]);
    }

    #[test]
    fn parses_postscript_name() {
        let spec = FontSpec::from_setting("postscript_name=LiberationMono").unwrap();
        assert_eq!(spec.postscript_name.as_deref(), Some("LiberationMono"));
    }

    #[test]
    fn rejects_unterminated_quote() {
        assert!(FontSpec::from_setting("family=\"oops").is_err());
    }
}
