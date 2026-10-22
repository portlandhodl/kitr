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
//! | [`fast_file_copy`] | `kitty/fast-file-copy.c` | buffered file copy; chunked checksummed stream |
//!
//! # Boundary
//!
//! The socket/peer layer, JSON command dispatch and the concrete `rc/*.py`
//! commands are intentionally **not** part of this crate. [`framing::Framer`]
//! consumes bytes from any transport and returns opaque payloads; callers own
//! the JSON and socket code. Likewise [`protocol::CryptoState`] seals and opens
//! opaque byte payloads; it does not construct or parse the JSON envelope
//! (`version`, `iv`, `tag`, `pubkey`, `encrypted`, `enc_proto`).
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
pub mod crypto;
pub mod error;
pub mod fast_file_copy;
pub mod framing;
pub mod protocol;

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
pub use protocol::{CryptoState, NonceCounter, Role, SealedMessage};
