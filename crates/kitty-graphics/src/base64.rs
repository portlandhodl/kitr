// SPDX-License-Identifier: GPL-3.0-or-later
//! Base64 codec compatible with kitty's bundled libbase64.
//!
//! Ported from `kitty/base64.h`, `kitty/3rdparty/base64/lib/lib.c` and the
//! generic decoder. Kitty has two distinct behaviours which are both exposed
//! here:
//!
//! * [`decode_lenient`] mirrors `base64_decode8()`: the return value of
//!   libbase64 is intentionally ignored, so invalid input is decoded up to the
//!   first invalid character. This is what the graphics command parser uses.
//! * [`decode_strict`] mirrors `base64_decode8_strict()`: characters outside
//!   the RFC-4648 alphabet, whitespace and missing/incorrect padding are
//!   rejected.
//!
//! The streaming decoder mirrors `base64_stream_decode()`, allowing input to
//! be split at any byte boundary (including inside a four character group).
//!
//! Copyright (C) 2023 Kovid Goyal <kovid at kovidgoyal.net>
//! Copyright (C) 2015-2020 Alfred Klomp (libbase64)

const INVALID: u8 = 255;
const PAD: u8 = 254;

const fn build_table() -> [u8; 256] {
    let mut table = [INVALID; 256];
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut i = 0;
    while i < alphabet.len() {
        table[alphabet[i] as usize] = i as u8;
        i += 1;
    }
    table[b'=' as usize] = PAD;
    table
}

static DECODE_TABLE: [u8; 256] = build_table();
const ENCODE_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Errors produced by the strict and streaming decoders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Base64Error {
    /// A byte outside the base64 alphabet (including whitespace) was found.
    InvalidCharacter { offset: usize, byte: u8 },
    /// The input ended without the padding required to complete a group.
    MissingPadding,
    /// Data was supplied after a padded group had completed the stream.
    TrailingData,
    /// The output would exceed the configured limit.
    OutputTooLarge,
}

impl core::fmt::Display for Base64Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidCharacter { offset, byte } => {
                write!(
                    f,
                    "invalid base64 character 0x{byte:02x} at offset {offset}"
                )
            }
            Self::MissingPadding => f.write_str("invalid base64 data: missing padding"),
            Self::TrailingData => f.write_str("invalid base64 data: trailing data after padding"),
            Self::OutputTooLarge => f.write_str("decoded base64 data is too large"),
        }
    }
}

impl std::error::Error for Base64Error {}

/// Size of the buffer required for decoding `src_len` input bytes.
///
/// Mirrors `required_buffer_size_for_base64_decode()` from `base64.h`.
pub const fn required_decode_buffer_size(src_len: usize) -> usize {
    src_len / 4 * 3 + 2
}

/// Size of the buffer required for encoding `src_len` input bytes.
///
/// Mirrors `required_buffer_size_for_base64_encode()` from `base64.h`.
pub const fn required_encode_buffer_size(src_len: usize) -> usize {
    src_len.div_ceil(3) * 4
}

/// Decode base64 data, ignoring errors after the last complete character.
///
/// This is the exact behaviour of kitty's `base64_decode8()`: invalid
/// characters terminate decoding but do not turn into an error. Padding is
/// accepted but not required. Whitespace is **not** ignored (it is invalid,
/// just like in libbase64).
pub fn decode_lenient(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(required_decode_buffer_size(src.len()));
    let mut group = [0u8; 4];
    let mut n = 0usize;
    for &b in src {
        let v = DECODE_TABLE[b as usize];
        if v == PAD {
            match n {
                2 => {
                    out.push((group[0] << 2) | (group[1] >> 4));
                }
                3 => {
                    out.push((group[0] << 2) | (group[1] >> 4));
                    out.push((group[1] << 4) | (group[2] >> 2));
                }
                _ => {}
            }
            return out;
        }
        if v >= 64 {
            // libbase64 has already emitted the bytes for a complete pair
            // (or has no partial byte yet). Flush accordingly.
            if n >= 2 {
                out.push((group[0] << 2) | (group[1] >> 4));
            }
            if n >= 3 {
                out.push((group[1] << 4) | (group[2] >> 2));
            }
            return out;
        }
        group[n] = v;
        n += 1;
        if n == 4 {
            out.push((group[0] << 2) | (group[1] >> 4));
            out.push((group[1] << 4) | (group[2] >> 2));
            out.push((group[2] << 6) | group[3]);
            n = 0;
        }
    }
    match n {
        2 => out.push((group[0] << 2) | (group[1] >> 4)),
        3 => {
            out.push((group[0] << 2) | (group[1] >> 4));
            out.push((group[1] << 4) | (group[2] >> 2));
        }
        _ => {}
    }
    out
}

/// Strict RFC-4648 decoding, matching `base64_decode8_strict()`.
pub fn decode_strict(src: &[u8]) -> Result<Vec<u8>, Base64Error> {
    let mut out = Vec::with_capacity(required_decode_buffer_size(src.len()));
    let mut group = [0u8; 4];
    let mut n = 0usize;
    let mut offset = 0usize;
    while offset < src.len() {
        let b = src[offset];
        let v = DECODE_TABLE[b as usize];
        if v == PAD {
            match n {
                2 => {
                    // Requires exactly one more '=' immediately after this one.
                    if src.get(offset + 1) != Some(&b'=') {
                        return Err(Base64Error::InvalidCharacter { offset, byte: b });
                    }
                    if offset + 2 != src.len() {
                        return Err(Base64Error::TrailingData);
                    }
                    out.push((group[0] << 2) | (group[1] >> 4));
                    return Ok(out);
                }
                3 => {
                    if offset + 1 != src.len() {
                        return Err(Base64Error::TrailingData);
                    }
                    out.push((group[0] << 2) | (group[1] >> 4));
                    out.push((group[1] << 4) | (group[2] >> 2));
                    return Ok(out);
                }
                _ => return Err(Base64Error::InvalidCharacter { offset, byte: b }),
            }
        }
        if v >= 64 {
            return Err(Base64Error::InvalidCharacter { offset, byte: b });
        }
        group[n] = v;
        n += 1;
        if n == 4 {
            out.push((group[0] << 2) | (group[1] >> 4));
            out.push((group[1] << 4) | (group[2] >> 2));
            out.push((group[2] << 6) | group[3]);
            n = 0;
        }
        offset += 1;
    }
    match n {
        0 => Ok(out),
        // libbase64's base64_decode() reports incomplete groups as failure.
        _ => Err(Base64Error::MissingPadding),
    }
}

/// Standard base64 encoding.
///
/// If `add_padding` is false, trailing `=` characters are removed, matching
/// `base64_encode8()` used by the graphics printer.
pub fn encode(src: &[u8], add_padding: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(required_encode_buffer_size(src.len()));
    for chunk in src.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied();
        let b2 = chunk.get(2).copied();
        out.push(ENCODE_TABLE[(b0 >> 2) as usize]);
        out.push(ENCODE_TABLE[(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4)) as usize]);
        if let Some(b1) = b1 {
            out.push(ENCODE_TABLE[(((b1 & 0x0f) << 2) | (b2.unwrap_or(0) >> 6)) as usize]);
        } else if add_padding {
            out.push(b'=');
        }
        if let Some(b2) = b2 {
            out.push(ENCODE_TABLE[(b2 & 0x3f) as usize]);
        } else if add_padding {
            out.push(b'=');
        }
    }
    out
}

/// Stateful base64 decoder equivalent to libbase64's `base64_state`.
#[derive(Clone, Debug, Default)]
pub struct StreamingDecoder {
    group: [u8; 4],
    n: usize,
    finished: bool,
    failed: bool,
    awaiting_second_pad: bool,
}

impl StreamingDecoder {
    pub const fn new() -> Self {
        Self {
            group: [0; 4],
            n: 0,
            finished: false,
            failed: false,
            awaiting_second_pad: false,
        }
    }

    /// True while a four character group has not been completed yet.
    pub const fn needs_more_data(&self) -> bool {
        !self.finished && !self.failed && (self.n != 0 || self.awaiting_second_pad)
    }

    /// Decode the next chunk. Input may be split at any byte boundary.
    pub fn decode(&mut self, src: &[u8]) -> Result<Vec<u8>, Base64Error> {
        if self.failed {
            return Err(Base64Error::TrailingData);
        }
        let mut out = Vec::with_capacity(required_decode_buffer_size(src.len()));
        for (offset, &b) in src.iter().enumerate() {
            if self.finished {
                self.failed = true;
                return Err(Base64Error::TrailingData);
            }
            if self.awaiting_second_pad {
                if b == b'=' {
                    self.awaiting_second_pad = false;
                    self.finished = true;
                    continue;
                }
                self.failed = true;
                return Err(Base64Error::InvalidCharacter { offset, byte: b });
            }
            let v = DECODE_TABLE[b as usize];
            if v == PAD {
                match self.n {
                    2 => {
                        out.push((self.group[0] << 2) | (self.group[1] >> 4));
                        self.awaiting_second_pad = true;
                        self.n = 0;
                    }
                    3 => {
                        out.push((self.group[0] << 2) | (self.group[1] >> 4));
                        out.push((self.group[1] << 4) | (self.group[2] >> 2));
                        self.finished = true;
                        self.n = 0;
                    }
                    _ => {
                        self.failed = true;
                        return Err(Base64Error::InvalidCharacter { offset, byte: b });
                    }
                }
                continue;
            }
            if v >= 64 {
                self.failed = true;
                return Err(Base64Error::InvalidCharacter { offset, byte: b });
            }
            self.group[self.n] = v;
            self.n += 1;
            if self.n == 4 {
                out.push((self.group[0] << 2) | (self.group[1] >> 4));
                out.push((self.group[1] << 4) | (self.group[2] >> 2));
                out.push((self.group[2] << 6) | self.group[3]);
                self.n = 0;
            }
        }
        Ok(out)
    }

    /// Finish the stream, requiring it to end on a group boundary.
    pub fn finish(&mut self) -> Result<Vec<u8>, Base64Error> {
        if self.failed {
            return Err(Base64Error::TrailingData);
        }
        if self.n != 0 || self.awaiting_second_pad {
            self.failed = true;
            return Err(Base64Error::MissingPadding);
        }
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_and_decode_roundtrip() {
        for data in [
            b"".as_slice(),
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"fooba",
            b"foobar",
            b"any carnal pleasure.",
        ] {
            let encoded = encode(data, true);
            assert_eq!(decode_strict(&encoded).unwrap(), data);
            assert_eq!(decode_lenient(&encoded), data);
        }
    }

    #[test]
    fn padded_examples() {
        assert_eq!(decode_strict(b"Zg==").unwrap(), b"f");
        assert_eq!(decode_strict(b"Zm8=").unwrap(), b"fo");
        assert_eq!(decode_strict(b"Zm9v").unwrap(), b"foo");
        assert_eq!(decode_strict(b"").unwrap(), b"");
    }

    #[test]
    fn lenient_accepts_missing_padding_but_strict_does_not() {
        assert_eq!(decode_lenient(b"Zg"), b"f");
        assert_eq!(decode_strict(b"Zg"), Err(Base64Error::MissingPadding));
        assert_eq!(decode_lenient(b"Zm8"), b"fo");
        assert_eq!(decode_strict(b"Zm8"), Err(Base64Error::MissingPadding));
    }

    #[test]
    fn invalid_characters_stop_lenient_decoding() {
        assert_eq!(decode_lenient(b"Zm9v!!!"), b"foo");
        assert_eq!(decode_lenient(b"Zm9v\n"), b"foo");
        assert_eq!(
            decode_strict(b"Zm9v!!!"),
            Err(Base64Error::InvalidCharacter {
                offset: 4,
                byte: b'!'
            })
        );
    }

    #[test]
    fn whitespace_is_not_ignored() {
        assert!(decode_lenient(b" Zm9v").is_empty());
        assert!(matches!(
            decode_strict(b"Zm 9v"),
            Err(Base64Error::InvalidCharacter { byte: b' ', .. })
        ));
    }

    #[test]
    fn padding_rules_match_libbase64() {
        assert_eq!(decode_lenient(b"Zg==Zg=="), b"f");
        assert_eq!(decode_strict(b"Zg==Zg=="), Err(Base64Error::TrailingData));
        assert_eq!(
            decode_strict(b"Zg="),
            Err(Base64Error::InvalidCharacter {
                offset: 2,
                byte: b'='
            })
        );
        assert_eq!(
            decode_strict(b"Z==="),
            Err(Base64Error::InvalidCharacter {
                offset: 1,
                byte: b'='
            })
        );
        assert_eq!(
            decode_strict(b"=Zg="),
            Err(Base64Error::InvalidCharacter {
                offset: 0,
                byte: b'='
            })
        );
    }

    #[test]
    fn streaming_decoder_handles_arbitrary_split_points() {
        let encoded = encode(b"abcdef", true);
        let mut d = StreamingDecoder::new();
        assert_eq!(d.decode(&encoded[..1]).unwrap(), b"");
        assert!(d.needs_more_data());
        let mut out = d.decode(&encoded[1..4]).unwrap();
        out.extend(d.decode(&encoded[4..]).unwrap());
        assert_eq!(out, b"abcdef");
        assert!(!d.needs_more_data());
    }

    #[test]
    fn streaming_decoder_handles_one_byte_chunks() {
        let encoded = encode(b"some data", true);
        let mut d = StreamingDecoder::new();
        let mut out = Vec::new();
        for b in encoded {
            out.extend(d.decode(&[b]).unwrap());
        }
        assert_eq!(out, b"some data");
        assert!(!d.needs_more_data());
    }

    #[test]
    fn streaming_decoder_handles_padding_split_across_chunks() {
        let mut d = StreamingDecoder::new();
        assert_eq!(d.decode(b"Zg=").unwrap(), b"f");
        assert!(d.needs_more_data());
        assert_eq!(d.decode(b"=").unwrap(), b"");
        assert!(!d.needs_more_data());
        d.finish().unwrap();

        let mut d = StreamingDecoder::new();
        let mut out = Vec::new();
        for b in encode(b"foobar", true) {
            out.extend(d.decode(&[b]).unwrap());
        }
        assert_eq!(out, b"foobar");
        assert!(!d.needs_more_data());
        d.finish().unwrap();
    }

    #[test]
    fn streaming_decoder_rejects_invalid_and_unpadded_end() {
        let mut d = StreamingDecoder::new();
        assert!(matches!(
            d.decode(b"SGVs!!!"),
            Err(Base64Error::InvalidCharacter { .. })
        ));
        let mut d = StreamingDecoder::new();
        d.decode(b"SGVsbG8").unwrap();
        assert!(d.needs_more_data());
        assert_eq!(d.finish(), Err(Base64Error::MissingPadding));
    }
}
