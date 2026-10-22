// SPDX-License-Identifier: GPL-3.0-or-later
//! Base64 and base85 codecs compatible with kitty.
//!
//! The base64 behaviour mirrors `kitty/base64.h` / the bundled libbase64:
//!
//! * encoding omits padding unless asked for it (kitty's `base64_encode`
//!   defaults to no padding);
//! * non-strict decoding accepts missing trailing padding and stops at the
//!   first character outside the alphabet (libbase64 reports an error but
//!   `base64_decode8()` ignores it and returns the bytes decoded so far);
//! * strict decoding rejects invalid characters and incomplete groups, exactly
//!   like `base64_decode8_strict()`.
//!
//! Streaming encoders/decoders correspond to `StreamingBase64Encoder` and
//! `StreamingBase64Decoder` in `data-types.c`. Base85 is the alphabet used by
//! Python's `base64.b85encode()`; kitty uses it for the fields of encrypted
//! remote-control messages.

use crate::error::{Error, Result};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const B85_ALPHABET: &[u8; 85] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";

const fn decode_table(alphabet: &[u8; 64]) -> [i8; 256] {
    let mut table = [-1i8; 256];
    let mut i = 0;
    while i < 64 {
        table[alphabet[i] as usize] = i as i8;
        i += 1;
    }
    table
}

const B64_VALUES: [i8; 256] = decode_table(ALPHABET);

fn b85_value(byte: u8) -> Option<u8> {
    B85_ALPHABET
        .iter()
        .position(|&x| x == byte)
        .map(|x| x as u8)
}

/// Required buffer size for encoding, matching
/// `required_buffer_size_for_base64_encode()`.
pub const fn encoded_len(src_len: usize) -> usize {
    src_len.div_ceil(3) * 4
}

/// Required buffer size for decoding, matching
/// `required_buffer_size_for_base64_decode()`.
pub const fn decoded_len(src_len: usize) -> usize {
    src_len / 4 * 3 + 2
}

/// Encode `src` as standard base64. Padding is omitted unless `padding` is
/// true, matching kitty's `base64_encode(data, add_padding)`.
pub fn encode(src: &[u8], padding: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(encoded_len(src.len()));
    let (chunks, rem) = src.as_chunks::<3>();
    for chunk in chunks {
        let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | chunk[2] as u32;
        out.push(ALPHABET[((n >> 18) & 63) as usize]);
        out.push(ALPHABET[((n >> 12) & 63) as usize]);
        out.push(ALPHABET[((n >> 6) & 63) as usize]);
        out.push(ALPHABET[(n & 63) as usize]);
    }
    match rem.len() {
        1 => {
            let n = (rem[0] as u32) << 16;
            out.push(ALPHABET[((n >> 18) & 63) as usize]);
            out.push(ALPHABET[((n >> 12) & 63) as usize]);
            if padding {
                out.extend_from_slice(b"==");
            }
        }
        2 => {
            let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
            out.push(ALPHABET[((n >> 18) & 63) as usize]);
            out.push(ALPHABET[((n >> 12) & 63) as usize]);
            out.push(ALPHABET[((n >> 6) & 63) as usize]);
            if padding {
                out.push(b'=');
            }
        }
        _ => {}
    }
    out
}

/// Decode standard base64 non-strictly, like `base64_decode8()`.
///
/// Missing padding is accepted. Decoding stops at the first `=` or invalid
/// byte and returns the bytes accumulated so far.
pub fn decode(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(decoded_len(src.len()));
    let mut acc: u32 = 0;
    let mut n = 0u8;
    for &byte in src {
        let v = B64_VALUES[byte as usize];
        if v < 0 {
            break;
        }
        let v = v as u32;
        match n {
            0 => {
                acc = v << 2;
                n = 1;
            }
            1 => {
                acc |= v >> 4;
                out.push(acc as u8);
                acc = (v & 0xf) << 4;
                n = 2;
            }
            2 => {
                acc |= v >> 2;
                out.push(acc as u8);
                acc = (v & 0x3) << 6;
                n = 3;
            }
            _ => {
                acc |= v;
                out.push(acc as u8);
                acc = 0;
                n = 0;
            }
        }
    }
    out
}

/// Decode standard base64 strictly, like `base64_decode8_strict()`.
///
/// Rejects any byte outside the RFC 4648 alphabet, missing padding, and data
/// after padding. An empty input decodes to an empty vector.
pub fn decode_strict(src: &[u8]) -> Result<Vec<u8>> {
    if src.is_empty() {
        return Ok(Vec::new());
    }
    if !src.len().is_multiple_of(4) {
        return Err(Error::InvalidBase64);
    }
    let quartets = src.len() / 4;
    let mut out = Vec::with_capacity(decoded_len(src.len()));
    for (index, q) in src.as_chunks::<4>().0.iter().enumerate() {
        let last = index + 1 == quartets;
        let a = B64_VALUES[q[0] as usize];
        let b = B64_VALUES[q[1] as usize];
        if a < 0 || b < 0 {
            return Err(Error::InvalidBase64);
        }
        let a = a as u32;
        let b = b as u32;
        if !last {
            let c = B64_VALUES[q[2] as usize];
            let d = B64_VALUES[q[3] as usize];
            if c < 0 || d < 0 {
                return Err(Error::InvalidBase64);
            }
            let n = (a << 18) | (b << 12) | ((c as u32) << 6) | d as u32;
            out.push((n >> 16) as u8);
            out.push((n >> 8) as u8);
            out.push(n as u8);
            continue;
        }
        if q[2] == b'=' {
            if q[3] != b'=' {
                return Err(Error::InvalidBase64);
            }
            out.push(((a << 2) | (b >> 4)) as u8);
        } else {
            let c = B64_VALUES[q[2] as usize];
            if c < 0 {
                return Err(Error::InvalidBase64);
            }
            let c = c as u32;
            if q[3] == b'=' {
                out.push(((a << 2) | (b >> 4)) as u8);
                out.push((((b & 0xf) << 4) | (c >> 2)) as u8);
            } else {
                let d = B64_VALUES[q[3] as usize];
                if d < 0 {
                    return Err(Error::InvalidBase64);
                }
                let n = (a << 18) | (b << 12) | (c << 6) | d as u32;
                out.push((n >> 16) as u8);
                out.push((n >> 8) as u8);
                out.push(n as u8);
            }
        }
    }
    Ok(out)
}

/// Streaming base64 encoder, matching `StreamingBase64Encoder`.
#[derive(Debug)]
pub struct StreamingBase64Encoder {
    carry: Vec<u8>,
    padding: bool,
}

impl StreamingBase64Encoder {
    /// `padding` corresponds to the constructor's `add_trailing_bytes`
    /// argument in `data-types.c`.
    pub fn new(padding: bool) -> Self {
        Self {
            carry: Vec::with_capacity(3),
            padding,
        }
    }

    /// Encode the next chunk, returning complete base64 groups.
    pub fn update(&mut self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(encoded_len(data.len() + self.carry.len()));
        for &byte in data {
            self.carry.push(byte);
            if self.carry.len() == 3 {
                out.extend_from_slice(&encode(&self.carry, false));
                self.carry.clear();
            }
        }
        out
    }

    /// Flush remaining bytes. This resets the encoder for a new stream, like
    /// `StreamingBase64Encoder.reset()`.
    pub fn finish(&mut self) -> Vec<u8> {
        let out = encode(&self.carry, self.padding);
        self.carry.clear();
        out
    }
}

/// Streaming base64 decoder, matching `StreamingBase64Decoder`.
///
/// Invalid bytes are reported as [`Error::InvalidBase64`] and reset the
/// decoder. Padding ends the current stream; the next `update()` starts a new
/// one, matching the C implementation's automatic reset on EOF.
#[derive(Debug)]
pub struct StreamingBase64Decoder {
    acc: u32,
    n: u8,
    done: bool,
}

impl Default for StreamingBase64Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamingBase64Decoder {
    /// Create an empty decoder.
    pub fn new() -> Self {
        Self {
            acc: 0,
            n: 0,
            done: false,
        }
    }

    /// Decode the next chunk.
    pub fn update(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        if self.done {
            self.reset();
        }
        let mut out = Vec::with_capacity(decoded_len(data.len()));
        for &byte in data {
            if byte == b'=' {
                if self.n == 1 {
                    self.reset();
                    return Err(Error::InvalidBase64);
                }
                self.done = true;
                continue;
            }
            let v = B64_VALUES[byte as usize];
            if v < 0 {
                self.reset();
                return Err(Error::InvalidBase64);
            }
            let v = v as u32;
            match self.n {
                0 => {
                    self.acc = v << 2;
                    self.n = 1;
                }
                1 => {
                    self.acc |= v >> 4;
                    out.push(self.acc as u8);
                    self.acc = (v & 0xf) << 4;
                    self.n = 2;
                }
                2 => {
                    self.acc |= v >> 2;
                    out.push(self.acc as u8);
                    self.acc = (v & 0x3) << 6;
                    self.n = 3;
                }
                _ => {
                    self.acc |= v;
                    out.push(self.acc as u8);
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }
        Ok(out)
    }

    /// Whether the decoder is waiting for more input in the middle of a group.
    pub fn needs_more_data(&self) -> bool {
        self.n != 0 && !self.done
    }

    /// Reset the decoder for a new stream.
    pub fn reset(&mut self) {
        self.acc = 0;
        self.n = 0;
        self.done = false;
    }
}

/// Encode using Python's `base64.b85encode()` alphabet and framing.
pub fn encode_b85(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len().div_ceil(4) * 5);
    let (chunks, rem) = src.as_chunks::<4>();
    for chunk in chunks {
        let mut n = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as u64;
        let mut group = [0u8; 5];
        for slot in group.iter_mut().rev() {
            *slot = B85_ALPHABET[(n % 85) as usize];
            n /= 85;
        }
        out.extend_from_slice(&group);
    }
    if !rem.is_empty() {
        let mut padded = [0u8; 4];
        padded[..rem.len()].copy_from_slice(rem);
        let mut n = u32::from_be_bytes(padded) as u64;
        let mut group = [0u8; 5];
        for slot in group.iter_mut().rev() {
            *slot = B85_ALPHABET[(n % 85) as usize];
            n /= 85;
        }
        out.extend_from_slice(&group[..rem.len() + 1]);
    }
    out
}

/// Decode using Python's `base64.b85decode()` rules.
pub fn decode_b85(src: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(src.len() / 5 * 4 + 3);
    for group in src.chunks(5) {
        let rem = group.len();
        if rem == 1 {
            return Err(Error::InvalidBase85);
        }
        let mut padded = [B85_ALPHABET[84]; 5];
        padded[..rem].copy_from_slice(group);
        let mut n: u64 = 0;
        for &byte in &padded {
            let v = b85_value(byte).ok_or(Error::InvalidBase85)?;
            n = n
                .checked_mul(85)
                .and_then(|x| x.checked_add(v as u64))
                .ok_or(Error::InvalidBase85)?;
        }
        if n > u32::MAX as u64 {
            return Err(Error::InvalidBase85);
        }
        let decoded = (n as u32).to_be_bytes();
        if rem == 5 {
            out.extend_from_slice(&decoded);
        } else {
            out.extend_from_slice(&decoded[..rem - 1]);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_vectors() {
        for (raw, encoded) in [
            (&b""[..], &b""[..]),
            (b"f", b"Zg"),
            (b"fo", b"Zm8"),
            (b"foo", b"Zm9v"),
            (b"foob", b"Zm9vYg"),
            (b"fooba", b"Zm9vYmE"),
            (b"foobar", b"Zm9vYmFy"),
            (b"light w", b"bGlnaHQgdw"),
            (b"light wo", b"bGlnaHQgd28"),
            (b"light wor", b"bGlnaHQgd29y"),
        ] {
            assert_eq!(encode(raw, false), encoded);
            assert_eq!(decode(encoded), raw);
        }
        assert_eq!(encode(b"f", true), b"Zg==");
        assert_eq!(encode(b"fo", true), b"Zm8=");
        assert_eq!(encode(b"foo", true), b"Zm9v");
    }

    #[test]
    fn base64_strict_rejects_bad_input() {
        assert_eq!(decode_strict(b"bGlnaHQgdw==").unwrap(), b"light w");
        assert_eq!(decode_strict(b"bGlnaHQgd28=").unwrap(), b"light wo");
        assert_eq!(decode_strict(b"bGlnaHQgd29y").unwrap(), b"light wor");
        assert_eq!(decode_strict(b"").unwrap(), b"");
        for invalid in [
            &b"bGlnaHQgdw="[..],
            b"bGlnaHQgdw",
            b"bGln!!Qgdw==",
            b"bGlnaHQgdw==\n",
            b"!!!!",
            b"bGl",
        ] {
            assert!(decode_strict(invalid).is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn base64_non_strict_truncates_at_invalid() {
        assert_eq!(decode(b"bGlnaHQgdw"), b"light w");
        assert_eq!(decode(b"bGlnaHQgdw=="), b"light w");
        assert_eq!(decode(b"Zm9v!!Zm9v"), b"foo");
        assert_eq!(decode(b"Zg"), b"f");
        assert_eq!(decode(b"Z"), b"");
    }

    #[test]
    fn base64_streaming_split_at_every_byte() {
        let raw = b"the quick brown fox jumps over the lazy dog, 0123456789";
        let encoded = encode(raw, true);
        let mut decoder = StreamingBase64Decoder::new();
        let mut out = Vec::new();
        for byte in &encoded {
            out.extend_from_slice(&decoder.update(std::slice::from_ref(byte)).unwrap());
        }
        assert_eq!(out, raw);
        assert!(!decoder.needs_more_data());

        let mut encoder = StreamingBase64Encoder::new(true);
        let mut encoded_out = Vec::new();
        for byte in raw {
            encoded_out.extend_from_slice(&encoder.update(std::slice::from_ref(byte)));
        }
        encoded_out.extend_from_slice(&encoder.finish());
        assert_eq!(encoded_out, encoded);
    }

    #[test]
    fn base64_streaming_chunk_sizes() {
        let raw: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        for chunk_size in [1usize, 2, 3, 4, 5, 7, 16, 63, 1000] {
            let mut encoder = StreamingBase64Encoder::new(false);
            let mut encoded = Vec::new();
            for chunk in raw.chunks(chunk_size) {
                encoded.extend_from_slice(&encoder.update(chunk));
            }
            encoded.extend_from_slice(&encoder.finish());
            assert_eq!(encoded, encode(&raw, false));

            let mut decoder = StreamingBase64Decoder::new();
            let mut decoded = Vec::new();
            for chunk in encoded.chunks(chunk_size) {
                decoded.extend_from_slice(&decoder.update(chunk).unwrap());
            }
            assert_eq!(decoded, raw, "chunk size {chunk_size}");
        }
    }

    #[test]
    fn base64_streaming_rejects_invalid() {
        let mut decoder = StreamingBase64Decoder::new();
        assert!(decoder.update(b"Zm9v").is_ok());
        assert!(decoder.update(b"!!!!").is_err());
        assert!(decoder.update(b"Z").is_ok());
        assert!(decoder.update(b"=").is_err());
    }

    #[test]
    fn base85_python_vectors() {
        for (raw, encoded) in [
            (&b""[..], &b""[..]),
            (b"f", b"W&"),
            (b"fo", b"W^V"),
            (b"foo", b"W^Zo"),
            (b"foob", b"W^Zp|"),
            (b"fooba", b"W^Zp|VE"),
            (b"foobar", b"W^Zp|VR8"),
            (b"hello world", b"Xk~0{Zy<MXa%^M"),
            (
                &(0u8..32).collect::<Vec<_>>()[..],
                b"009C61O)~M2nh-c3=Iws5D^j+6crX17#SKH9337X",
            ),
        ] {
            assert_eq!(encode_b85(raw), encoded);
            assert_eq!(decode_b85(encoded).unwrap(), raw);
        }
        assert!(decode_b85(b"A").is_err());
        assert!(decode_b85(b"~~~~~").is_err());
        assert!(decode_b85(b"W&\"").is_err());
    }

    #[test]
    fn base85_roundtrip_all_lengths() {
        for len in 0..70 {
            let raw: Vec<u8> = (0..len).map(|x| (x * 37 + 11) as u8).collect();
            let encoded = encode_b85(&raw);
            assert_eq!(decode_b85(&encoded).unwrap(), raw, "length {len}");
        }
    }
}
