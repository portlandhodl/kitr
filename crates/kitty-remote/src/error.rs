// SPDX-License-Identifier: GPL-3.0-or-later
//! Errors shared by the remote-control primitives.
//!
//! Ported from the error paths in `kitty/crypto.c`; framing and file-copy
//! errors are Rust-side additions because the C implementation reports those
//! through `errno`/Python exceptions.

use std::fmt;

/// Result type used throughout this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced by the crypto, encoding, framing and file-copy primitives.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// An OpenSSL operation failed. The string is the drained OpenSSL error queue.
    OpenSsl(String),
    /// AES-256-GCM requires a 32-byte key, exactly like `crypto.c`.
    InvalidKeyLength(usize),
    /// GCM requires an IV of at least 12 bytes.
    InvalidIvLength(usize),
    /// GCM tags shorter than 16 bytes are rejected.
    InvalidTagLength(usize),
    /// Authentication failed while finishing decryption.
    AuthenticationFailed,
    /// A streaming cipher was used in an invalid order.
    InvalidState(&'static str),
    /// The input is not valid base64.
    InvalidBase64,
    /// The input is not valid base85 (Python's `base64.b85*` alphabet).
    InvalidBase85,
    /// A caller-supplied output buffer was too small.
    BufferTooSmall,
    /// A framed message exceeded the configured maximum.
    MessageTooLarge { size: usize, max: usize },
    /// More bytes were expected to complete a frame.
    IncompleteFrame,
    /// A frame had a bad magic/version/reserved field.
    InvalidFrame(&'static str),
    /// The checksum in a file-copy chunk did not match its data.
    BadChecksum,
    /// A file-copy chunk arrived with an unexpected index or offset.
    OutOfOrder {
        expected_index: u64,
        expected_offset: u64,
        got_index: u64,
        got_offset: u64,
    },
    /// The final chunk has not been seen yet.
    MissingLastChunk,
    /// A JSON payload could not be parsed or serialized.
    Json(String),
    /// The remote-control protocol was violated.
    Protocol(String),
    /// The remote-control peer closed the connection.
    ConnectionClosed,
    /// The peer is not permitted to use remote control.
    PermissionDenied(String),
    /// Any underlying I/O error.
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenSsl(msg) => write!(f, "OpenSSL error: {msg}"),
            Self::InvalidKeyLength(n) => write!(f, "the key for AES 256 GCM must be 32 bytes long, not {n}"),
            Self::InvalidIvLength(n) => write!(f, "the IV for AES 256 GCM must be at least 12 bytes long, not {n}"),
            Self::InvalidTagLength(n) => write!(f, "the tag for AES 256 GCM must be at least 16 bytes long, not {n}"),
            Self::AuthenticationFailed => f.write_str("failed to finish decrypt: authentication failed"),
            Self::InvalidState(msg) => write!(f, "invalid cipher state: {msg}"),
            Self::InvalidBase64 => f.write_str("invalid base64 input data"),
            Self::InvalidBase85 => f.write_str("invalid base85 input data"),
            Self::BufferTooSmall => f.write_str("output buffer too small"),
            Self::MessageTooLarge { size, max } => write!(f, "message of {size} bytes exceeds the limit of {max} bytes"),
            Self::IncompleteFrame => f.write_str("incomplete frame"),
            Self::InvalidFrame(msg) => write!(f, "invalid frame: {msg}"),
            Self::BadChecksum => f.write_str("file-copy chunk checksum mismatch"),
            Self::OutOfOrder { expected_index, expected_offset, got_index, got_offset } => write!(
                f,
                "out-of-order file-copy chunk: expected index {expected_index} offset {expected_offset}, got index {got_index} offset {got_offset}"
            ),
            Self::MissingLastChunk => f.write_str("file-copy stream ended without a last chunk"),
            Self::Json(msg) => write!(f, "JSON error: {msg}"),
            Self::Protocol(msg) => write!(f, "remote-control protocol error: {msg}"),
            Self::ConnectionClosed => f.write_str("remote-control connection closed"),
            Self::PermissionDenied(msg) => write!(f, "permission denied: {msg}"),
            Self::Io(err) => write!(f, "I/O error: {err}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<crate::json::JsonError> for Error {
    fn from(value: crate::json::JsonError) -> Self {
        Self::Json(value.to_string())
    }
}

impl From<Error> for std::io::Error {
    fn from(value: Error) -> Self {
        match value {
            Error::Io(err) => err,
            other => std::io::Error::other(other.to_string()),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
