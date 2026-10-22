// SPDX-License-Identifier: GPL-3.0-or-later
//! Graphics protocol and image decoding ported from `kitty/graphics.c`,
//! `kitty/png-reader.c`, `kitty/mime-cache.c` and the graphics command
//! parser headers.
//!
//! # API overview
//!
//! * [`command::parse_escape`] / [`command::parse_raw`] parse
//!   `ESC _ G <control>[;<base64 payload>] ESC \` into a
//!   [`GraphicsCommand`](command::GraphicsCommand).
//! * [`base64`] provides both kitty's lenient decoder and a strict RFC-4648
//!   decoder, plus a streaming decoder for chunk-split input.
//! * [`png::decode_png`] decodes any libpng-supported PNG to tightly packed
//!   RGBA8 using libpng's simplified API. [`png::encode_png_rgba`] and friends
//!   are available for generating fixtures/tests.
//! * [`mime`] maps image file extensions and magic bytes to MIME types without
//!   reading XDG MIME caches or using the network.
//! * [`image::ImageAssembler`] accumulates chunked transmissions, decodes
//!   raw RGB/RGBA/indexed data or PNG data (optionally zlib-compressed), loads
//!   regular files, and returns [`Image`](image::Image) values with placement
//!   metadata. [`image::GraphicsSink`] defines the callbacks for the future
//!   Screen integration.
//!
//! # Supported protocol subset
//!
//! Parsing supports every key in kitty's generated parser (`a d t o f m i I p q
//! w h x y v s S O c r X Y z C U P Q N H V`), all actions, delete actions,
//! transmission media and the `o=z` compression flag.
//!
//! The image pipeline implements `a=t`, `a=T`, `a=q`, chunking (`m=`), direct
//! data, regular files (`t=f`), temporary files (`t=t`), formats `f=8/16/24/32`
//! and `f=100` (PNG), and zlib compression.
//!
//! # Unsupported / deferred
//!
//! * Screen/GPU integration, image/placement storage and z-index layer
//!   management are intentionally out of scope. Use [`image::GraphicsSink`].
//! * Animation frames (`a=a`, `a=f`), composition (`a=c`) and the full
//!   placement/parent/unicode-placeholder semantics are parsed but not
//!   executed by the loader; they are reported through
//!   [`GraphicsSink::on_unsupported`](image::GraphicsSink::on_unsupported).
//! * POSIX shared memory transmission (`t=s`) is parsed, but loading it is
//!   deferred to terminal integration (it needs platform shm lifecycle and
//!   kitty's boss/path policy). `t=f`/`t=t` read regular files only.
//! * Temporary file deletion and the boss/path access policy are not applied
//!   by this library; [`image::read_image_file`] only enforces regular-file
//!   and size limits.
//! * ICC/lcms2 colour management is deferred. PNGs are decoded to sRGB-assumed
//!   8-bit RGBA by libpng's simplified API; embedded ICC profiles are ignored.
//!   Gamma handling is left to libpng (the C code applies an explicit gamma
//!   transform when a gAMA chunk is present).
//! * The shared-mime-info binary cache is replaced by an embedded image-only
//!   extension/magic table.
//! * `print-graphics.h` command printing is not ported yet.
//!
//! # Errors
//!
//! Parser diagnostics use kitty's wording. Runtime errors expose a
//! kitty-compatible response code via [`GraphicsError::code`](image::GraphicsError::code)
//! (`EINVAL`, `ENODATA`, `EBADPNG`, `EFBIG`, `EBADF`, `EILSEQ`, ...).

pub mod base64;
pub mod command;
pub mod image;
pub mod mime;
pub mod png;

pub use base64::{
    decode_lenient, decode_strict, encode as encode_base64, Base64Error, StreamingDecoder,
};
pub use command::{
    parse_escape, parse_raw, Action, Compression, DeleteAction, GraphicsCommand, ParseError,
    TransmissionType, GRAPHICS_USAGE_HINT_TRANSIENT,
};
pub use image::{
    decode_image, read_image_file, GraphicsError, GraphicsSink, Image, ImageAssembler, ImageFormat,
    ImageLimits, LoadOutcome, LoadedImage, Placement, MAX_DATA_SIZE, MAX_IMAGE_DIMENSION,
};
pub use mime::{is_image_mime, mime_type_aliases, mime_type_for_data, mime_type_for_filename};
pub use png::{decode_png, DecodedPng, PngError};
