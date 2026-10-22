// SPDX-License-Identifier: GPL-3.0-or-later
//! Errors shared by kitten discovery, launching, the protocol host and shell
//! integration.
//!
//! The error type is deliberately small and dependency-free. Every error that
//! refers to a file carries the path so a caller can render a useful message
//! without having to reconstruct it.

use std::fmt;
use std::path::PathBuf;

/// Result type used throughout this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced by this crate.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// An underlying filesystem or process operation failed.
    Io(std::io::Error),
    /// A filesystem operation failed for a specific path.
    IoPath {
        path: PathBuf,
        source: std::io::Error,
    },
    /// No kitten with the requested name was found.
    NotFound {
        name: String,
        searched: Vec<PathBuf>,
    },
    /// A kitten name was empty or contained path separators.
    InvalidName(String),
    /// A command was missing a required argument.
    MissingArgument(&'static str),
    /// A command-line command was not recognized.
    UnknownCommand(String),
    /// A manifest could not be parsed.
    InvalidManifest { path: PathBuf, message: String },
    /// A manifest field had the wrong type or value.
    InvalidManifestField {
        path: PathBuf,
        field: &'static str,
        message: String,
    },
    /// A base85 payload was malformed.
    InvalidBase85(String),
    /// A JSON payload was malformed or could not be serialized.
    InvalidJson(String),
    /// A DCS frame was malformed or incomplete.
    InvalidDcs(String),
    /// The shell is not bash, zsh or fish.
    UnsupportedShell(String),
    /// An environment variable was malformed.
    InvalidEnvironment(String),
    /// A kitten selected for execution is not executable.
    NotExecutable(PathBuf),
    /// The entry point of a kitten does not exist.
    MissingEntry(PathBuf),
    /// An internal invariant was violated.
    InvalidState(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::IoPath { path, source } => {
                write!(f, "I/O error for {}: {source}", path.display())
            }
            Self::NotFound { name, searched } => {
                write!(f, "no kitten named {name:?}")?;
                if !searched.is_empty() {
                    write!(f, "; searched:")?;
                    for path in searched {
                        write!(f, " {}", path.display())?;
                    }
                }
                Ok(())
            }
            Self::InvalidName(name) => write!(f, "invalid kitten name: {name:?}"),
            Self::MissingArgument(name) => write!(f, "missing required argument: {name}"),
            Self::UnknownCommand(name) => write!(f, "unknown kitten command: {name}"),
            Self::InvalidManifest { path, message } => {
                write!(f, "invalid kitten manifest {}: {message}", path.display())
            }
            Self::InvalidManifestField {
                path,
                field,
                message,
            } => write!(
                f,
                "invalid kitten manifest {}: field {field:?}: {message}",
                path.display()
            ),
            Self::InvalidBase85(message) => write!(f, "invalid base85 payload: {message}"),
            Self::InvalidJson(message) => write!(f, "invalid JSON payload: {message}"),
            Self::InvalidDcs(message) => write!(f, "invalid DCS frame: {message}"),
            Self::UnsupportedShell(shell) => write!(f, "unsupported shell: {shell}"),
            Self::InvalidEnvironment(message) => {
                write!(f, "invalid environment variable: {message}")
            }
            Self::NotExecutable(path) => write!(f, "kitten is not executable: {}", path.display()),
            Self::MissingEntry(path) => {
                write!(f, "kitten entry point does not exist: {}", path.display())
            }
            Self::InvalidState(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::IoPath { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<kitty_remote::json::JsonError> for Error {
    fn from(value: kitty_remote::json::JsonError) -> Self {
        Self::InvalidJson(value.to_string())
    }
}

impl From<kitty_remote::Error> for Error {
    fn from(value: kitty_remote::Error) -> Self {
        match value {
            kitty_remote::Error::Io(err) => Self::Io(err),
            kitty_remote::Error::InvalidBase85 => {
                Self::InvalidBase85("invalid base85 input".to_owned())
            }
            other => Self::InvalidDcs(other.to_string()),
        }
    }
}
