// SPDX-License-Identifier: GPL-3.0-or-later
//! Binary integer helpers.
//!
//! The fixed-width big/little-endian functions mirror `kitty/binary.h`. The
//! length-prefixed helpers use little-endian lengths and are provided for
//! binary protocols layered on top of remote control. Note that kitty's
//! remote-control wire format itself is JSON inside a DCS frame and does not
//! use a binary length prefix; see [`crate::framing`] for the actual framing.

use crate::error::{Error, Result};

macro_rules! fixed {
    ($put:ident, $get:ident, $ty:ty, $n:expr, $to:ident, $from:ident) => {
        /// Encode a fixed-width integer.
        pub fn $put(out: &mut Vec<u8>, value: $ty) {
            out.extend_from_slice(&value.$to());
        }

        /// Decode a fixed-width integer.
        pub fn $get(src: &[u8]) -> Result<$ty> {
            if src.len() < $n {
                return Err(Error::IncompleteFrame);
            }
            let mut buf = [0u8; $n];
            buf.copy_from_slice(&src[..$n]);
            Ok(<$ty>::$from(buf))
        }
    };
}

fixed!(put_u16_le, get_u16_le, u16, 2, to_le_bytes, from_le_bytes);
fixed!(put_u32_le, get_u32_le, u32, 4, to_le_bytes, from_le_bytes);
fixed!(put_u64_le, get_u64_le, u64, 8, to_le_bytes, from_le_bytes);
fixed!(put_i16_le, get_i16_le, i16, 2, to_le_bytes, from_le_bytes);
fixed!(put_i32_le, get_i32_le, i32, 4, to_le_bytes, from_le_bytes);
fixed!(put_i64_le, get_i64_le, i64, 8, to_le_bytes, from_le_bytes);
fixed!(put_u16_be, get_u16_be, u16, 2, to_be_bytes, from_be_bytes);
fixed!(put_u32_be, get_u32_be, u32, 4, to_be_bytes, from_be_bytes);
fixed!(put_u64_be, get_u64_be, u64, 8, to_be_bytes, from_be_bytes);

/// Append a u32 little-endian length followed by `data`.
pub fn put_length_prefixed_u32(out: &mut Vec<u8>, data: &[u8]) -> Result<()> {
    let len = u32::try_from(data.len()).map_err(|_| Error::MessageTooLarge {
        size: data.len(),
        max: u32::MAX as usize,
    })?;
    put_u32_le(out, len);
    out.extend_from_slice(data);
    Ok(())
}

/// Read a u32 little-endian length-prefixed blob, returning it and the rest of
/// the input.
pub fn get_length_prefixed_u32(src: &[u8]) -> Result<(&[u8], &[u8])> {
    let len = get_u32_le(src)? as usize;
    if src.len() < 4 + len {
        return Err(Error::IncompleteFrame);
    }
    Ok((&src[4..4 + len], &src[4 + len..]))
}

/// Append a u64 little-endian length followed by `data`.
pub fn put_length_prefixed_u64(out: &mut Vec<u8>, data: &[u8]) -> Result<()> {
    let len = u64::try_from(data.len()).map_err(|_| Error::MessageTooLarge {
        size: data.len(),
        max: usize::MAX,
    })?;
    put_u64_le(out, len);
    out.extend_from_slice(data);
    Ok(())
}

/// Read a u64 little-endian length-prefixed blob, returning it and the rest of
/// the input.
pub fn get_length_prefixed_u64(src: &[u8]) -> Result<(&[u8], &[u8])> {
    let len = get_u64_le(src)? as usize;
    if src.len() < 8 + len {
        return Err(Error::IncompleteFrame);
    }
    Ok((&src[8..8 + len], &src[8 + len..]))
}

/// A little-endian binary writer.
#[derive(Debug, Default, Clone)]
pub struct BinaryWriter {
    buf: Vec<u8>,
}

impl BinaryWriter {
    /// Create an empty writer.
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Create a writer with preallocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity),
        }
    }

    /// Access the bytes written so far.
    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    /// Consume the writer and return its bytes.
    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    /// Write a u16 in little-endian order.
    pub fn u16(&mut self, value: u16) -> &mut Self {
        put_u16_le(&mut self.buf, value);
        self
    }

    /// Write a u32 in little-endian order.
    pub fn u32(&mut self, value: u32) -> &mut Self {
        put_u32_le(&mut self.buf, value);
        self
    }

    /// Write a u64 in little-endian order.
    pub fn u64(&mut self, value: u64) -> &mut Self {
        put_u64_le(&mut self.buf, value);
        self
    }

    /// Write raw bytes.
    pub fn bytes(&mut self, value: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(value);
        self
    }

    /// Write a u32 length-prefixed blob.
    pub fn length_prefixed_u32(&mut self, value: &[u8]) -> Result<&mut Self> {
        put_length_prefixed_u32(&mut self.buf, value)?;
        Ok(self)
    }
}

/// A little-endian binary reader.
#[derive(Debug, Clone)]
pub struct BinaryReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BinaryReader<'a> {
    /// Wrap a byte slice.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Number of unread bytes.
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    /// Current read position.
    pub fn position(&self) -> usize {
        self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(Error::IncompleteFrame);
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    /// Read a u16 in little-endian order.
    pub fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        get_u16_le(b)
    }

    /// Read a u32 in little-endian order.
    pub fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        get_u32_le(b)
    }

    /// Read a u64 in little-endian order.
    pub fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        get_u64_le(b)
    }

    /// Read exactly `n` raw bytes.
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.take(n)
    }

    /// Read a u32 length-prefixed blob.
    pub fn length_prefixed_u32(&mut self) -> Result<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn little_endian_roundtrip() {
        let mut out = Vec::new();
        put_u16_le(&mut out, 0x1234);
        put_u32_le(&mut out, 0x89ab_cdef);
        put_u64_le(&mut out, 0x0123_4567_89ab_cdef);
        assert_eq!(
            out,
            [0x34, 0x12, 0xef, 0xcd, 0xab, 0x89, 0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01]
        );
        assert_eq!(get_u16_le(&out).unwrap(), 0x1234);
        assert_eq!(get_u32_le(&out[2..]).unwrap(), 0x89ab_cdef);
        assert_eq!(get_u64_le(&out[6..]).unwrap(), 0x0123_4567_89ab_cdef);
    }

    #[test]
    fn big_endian_roundtrip() {
        let mut out = Vec::new();
        put_u16_be(&mut out, 0x1234);
        put_u32_be(&mut out, 0x89ab_cdef);
        put_u64_be(&mut out, 0x0123_4567_89ab_cdef);
        assert_eq!(
            out,
            [0x12, 0x34, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]
        );
        assert_eq!(get_u16_be(&out).unwrap(), 0x1234);
        assert_eq!(get_u32_be(&out[2..]).unwrap(), 0x89ab_cdef);
        assert_eq!(get_u64_be(&out[6..]).unwrap(), 0x0123_4567_89ab_cdef);
    }

    #[test]
    fn signed_roundtrip() {
        for value in [i16::MIN, -1, 0, 1, i16::MAX] {
            let mut out = Vec::new();
            put_i16_le(&mut out, value);
            assert_eq!(get_i16_le(&out).unwrap(), value);
        }
        for value in [i32::MIN, -123456, 0, 123456, i32::MAX] {
            let mut out = Vec::new();
            put_i32_le(&mut out, value);
            assert_eq!(get_i32_le(&out).unwrap(), value);
        }
        for value in [i64::MIN, -1, 0, 1, i64::MAX] {
            let mut out = Vec::new();
            put_i64_le(&mut out, value);
            assert_eq!(get_i64_le(&out).unwrap(), value);
        }
    }

    #[test]
    fn length_prefixed_roundtrip() {
        let mut out = Vec::new();
        put_length_prefixed_u32(&mut out, b"hello").unwrap();
        put_length_prefixed_u32(&mut out, b"").unwrap();
        let (first, rest) = get_length_prefixed_u32(&out).unwrap();
        assert_eq!(first, b"hello");
        let (second, rest) = get_length_prefixed_u32(rest).unwrap();
        assert!(second.is_empty());
        assert!(rest.is_empty());

        let mut out = Vec::new();
        put_length_prefixed_u64(&mut out, b"world").unwrap();
        let (first, rest) = get_length_prefixed_u64(&out).unwrap();
        assert_eq!(first, b"world");
        assert!(rest.is_empty());
    }

    #[test]
    fn truncated_input_errors() {
        assert!(matches!(get_u16_le(&[1]), Err(Error::IncompleteFrame)));
        assert!(matches!(
            get_u32_le(&[1, 2, 3]),
            Err(Error::IncompleteFrame)
        ));
        assert!(matches!(get_u64_le(&[0; 7]), Err(Error::IncompleteFrame)));
        assert!(matches!(
            get_length_prefixed_u32(&[5, 0, 0, 0, b'a']),
            Err(Error::IncompleteFrame)
        ));
        assert!(matches!(
            get_length_prefixed_u64(&[1, 0, 0, 0, 0, 0, 0]),
            Err(Error::IncompleteFrame)
        ));
    }

    #[test]
    fn writer_and_reader_agree() {
        let mut w = BinaryWriter::new();
        w.u16(7).u32(9).u64(11).length_prefixed_u32(b"abc").unwrap();
        let bytes = w.into_inner();
        let mut r = BinaryReader::new(&bytes);
        assert_eq!(r.u16().unwrap(), 7);
        assert_eq!(r.u32().unwrap(), 9);
        assert_eq!(r.u64().unwrap(), 11);
        assert_eq!(r.length_prefixed_u32().unwrap(), b"abc");
        assert_eq!(r.remaining(), 0);
        assert!(matches!(r.u16(), Err(Error::IncompleteFrame)));
    }
}
