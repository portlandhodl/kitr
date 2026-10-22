// SPDX-License-Identifier: GPL-3.0-or-later
//! Remote-control crypto, framing and file copy ported from kitty.
//!
//! This crate ports the reusable primitives behind kitty's remote control:
//!
//! | module | C/Python source | contents |
//! |---|---|---|
//! | [`crypto`] | `kitty/crypto.c` | X25519 ECDH, SHA hashes, AES-256-GCM, `Secret` |
//! | [`base64`] | `kitty/base64.h`, `data-types.c` | base64 (strict/streaming) and base85 |
//! | [`binary`] | `kitty/binary.h` | fixed-width LE/BE integers, length prefixes |
//! | [`framing`] | `remote_control.py`, `boss.py`, `child-monitor.c` | DCS `@kitty-cmd` message framing |
//! | [`protocol`] | `remote_control.py`, `boss.py` | `CryptoState` handshake and sealed messages |
//! | [`json`] | Python `json` via `remote_control.py` | dependency-free JSON value/parser/serializer |
//! | [`commands`] | `kitty/rc/*.py`, `remote_control.py` | payload types, `RemoteHandler` trait, dispatch |
//! | [`server`] | `boss.py`, `child-monitor.c`, socket half | Unix listener, peer creds, encrypted envelopes |
//! | [`client`] | `kitty/client.py`, `remote_control.py` | test/embedding client with split-read support |
//! | [`fast_file_copy`] | `kitty/fast-file-copy.c` | buffered file copy; chunked checksummed stream |
//!
//! # Boundary
//!
//! Terminal and window work is still delegated through [`commands::RemoteHandler`].
//! This crate owns the Unix socket, handshake envelope, JSON payloads, command
//! dispatch, response correlation and notifications, but not tabs, layouts or
//! kitten integration. Those live above this layer.
//!
//! # Compatibility notes
//!
//! * `crypto.c` implements **X25519**, not P-256. The port matches the C source.
//! * Kitty uses AES-GCM with a fresh random 12-byte IV for every message, not a
//!   nonce counter. [`protocol::NonceCounter`] is an opt-in utility and is not
//!   used by [`protocol::CryptoState`].
//! * Kitty uses Python's `base64.b85*` alphabet for encrypted envelope fields
//!   and standard base64 for file-transmission payloads. Both are provided.
//! * The chunked file-copy format in [`fast_file_copy`] is an extension: the C
//!   `fast-file-copy.c` has no wire protocol, commands or checksums.

pub mod base64;
pub mod binary;
pub mod client;
pub mod commands;
pub mod crypto;
pub mod error;
pub mod fast_file_copy;
pub mod framing;
pub mod json;
pub mod protocol;
pub mod server;

pub use client::{
    parse_public_key_env, seal_request, seal_request_with_timestamp, Client, ClientConfig,
    Incoming, Notification,
};
pub use commands::{
    capabilities_value, color_as_u32, error_response, execute, success_response, BracketedPaste,
    CloseTabRequest, CloseWindowRequest, CommandError, CommandResult, DispatchOutcome,
    FocusWindowRequest, GetColorsRequest, GetTextRequest, PeerInfo, RemoteHandler, RequestContext,
    ResizeAxis, ResizeWindowRequest, ScreenshotCallback, ScreenshotFormat, ScreenshotImage,
    ScreenshotRequest, SendKeyRequest, SendTextData, SendTextRequest, SetColorsRequest,
    SetWindowTitleRequest, TextExtent, IMPLEMENTED_COMMANDS, KITTY_COMPAT_VERSION,
    RC_ENCRYPTION_PROTOCOL_VERSION,
};
pub use crypto::{
    digest, secure_random_bytes, sha256, Aes256GcmDecrypt, Aes256GcmEncrypt, EllipticCurveKey,
    HashAlgorithm, Secret, AES_KEY_LEN, DEFAULT_TIMESTAMP_SKEW_NS, GCM_IV_LEN, GCM_TAG_LEN,
    X25519_KEY_LEN,
};
pub use error::{Error, Result};
pub use fast_file_copy::{
    copy_between_files, copy_between_files_with_buffer, copy_chunked, ChunkDecoder, ChunkedReader,
    FileCopyChunk, FileCopyReceiver, COPY_BUFFER_SIZE, FILE_COPY_CHUNK_SIZE, FILE_COPY_HEADER_LEN,
    FILE_COPY_MAGIC, FILE_COPY_MAX_CHUNK_SIZE, FILE_COPY_VERSION,
};
pub use framing::{
    encode_message, encode_response, Framer, DCS_PREFIX, DCS_TERMINATOR, DEFAULT_MAX_MESSAGE_SIZE,
};
pub use json::{JsonError, Number, Value};
pub use protocol::{CryptoState, NonceCounter, Role, SealedMessage};
pub use server::{
    notification_value, EncryptionMode, PeerCredentialPolicy, RemoteServer, ServerConfig,
    ServerHandle, ServerKey, MAX_TIMESTAMP_SKEW_NS,
};
