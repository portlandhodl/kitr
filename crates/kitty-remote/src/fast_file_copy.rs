// SPDX-License-Identifier: GPL-3.0-or-later
//! File copying ported from `kitty/fast-file-copy.c`, plus a chunked,
//! checksummed stream format for callers that need to move a file over a
//! message transport.
//!
//! The C file only contains [`copy_between_files`]-style descriptor copying
//! (`pread`/`write` with a 32 KiB buffer, and Linux `copy_file_range` /
//! `sendfile` fast paths). It does **not** define a wire protocol, commands,
//! checksums or chunks. Those pieces are marked as a protocol extension here:
//!
//! * [`copy_between_files`] is the actual port of the C buffered copy.
//! * [`FileCopyChunk`], [`ChunkedReader`] and [`FileCopyReceiver`] define a
//!   small file-agnostic chunk stream with SHA-256 checksums. It is not used by
//!   kitty and is not byte-compatible with `file_transmission.py` (which uses
//!   OSC 5113 key/value commands with base64 data). See the crate report.
//!
//! The fast `copy_file_range`/`sendfile` paths are intentionally not used yet:
//! the portable buffered loop has identical observable behaviour and is easier
//! to test. This is a documented deviation.

use crate::binary::{get_u16_le, get_u32_le, get_u64_le, put_u16_le, put_u32_le, put_u64_le};
use crate::crypto::sha256;
use crate::error::{Error, Result};
use std::io::{self, Read, Seek, SeekFrom, Write};

/// Buffer size used by the C implementation (`32 * 1024`).
pub const COPY_BUFFER_SIZE: usize = 32 * 1024;

/// Copy `len` bytes from `input` starting at absolute offset `in_pos` into
/// `output`, using the same 32 KiB buffering as `copy_between_files()`.
///
/// The C version uses `pread()`, leaving the input file offset untouched. This
/// Rust version uses [`Seek`], which changes the input cursor. Callers that
/// need the old offset should save and restore it. A premature end of input is
/// reported as [`io::ErrorKind::UnexpectedEof`], matching the C `errno = EIO`
/// path.
pub fn copy_between_files<R: Read + Seek, W: Write>(
    input: &mut R,
    output: &mut W,
    in_pos: u64,
    len: u64,
) -> Result<()> {
    copy_between_files_with_buffer(input, output, in_pos, len, COPY_BUFFER_SIZE)
}

/// Like [`copy_between_files`] with a caller-selected buffer size.
pub fn copy_between_files_with_buffer<R: Read + Seek, W: Write>(
    input: &mut R,
    output: &mut W,
    in_pos: u64,
    len: u64,
    buffer_size: usize,
) -> Result<()> {
    if len == 0 {
        return Ok(());
    }
    input.seek(SeekFrom::Start(in_pos))?;
    let buffer_size = buffer_size.max(1);
    let mut buf = vec![0u8; buffer_size];
    let mut remaining = len;
    while remaining > 0 {
        let want = remaining.min(buffer_size as u64) as usize;
        let read = input.read(&mut buf[..want])?;
        if read == 0 {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "input ended before the requested byte count was copied",
            )));
        }
        output.write_all(&buf[..read])?;
        remaining -= read as u64;
    }
    Ok(())
}

/// Magic value at the start of a chunk frame: `KFC1`.
pub const FILE_COPY_MAGIC: [u8; 4] = *b"KFC1";
/// Chunk-frame version.
pub const FILE_COPY_VERSION: u8 = 1;
/// Fixed header size: magic(4) + version(1) + flags(1) + reserved(2) +
/// index(8) + offset(8) + data_len(4) + checksum(32).
pub const FILE_COPY_HEADER_LEN: usize = 60;
/// Default chunk payload size, matching the C copy buffer.
pub const FILE_COPY_CHUNK_SIZE: usize = COPY_BUFFER_SIZE;
/// Largest chunk payload accepted by the decoder.
pub const FILE_COPY_MAX_CHUNK_SIZE: usize = 1024 * 1024;

const FLAG_LAST: u8 = 1;

/// One chunk of a file-copy stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileCopyChunk {
    /// Zero-based chunk index.
    pub index: u64,
    /// Absolute offset in the file.
    pub offset: u64,
    /// Whether this is the final chunk.
    pub last: bool,
    /// SHA-256 of `data`.
    pub checksum: [u8; 32],
    /// Payload bytes.
    pub data: Vec<u8>,
}

impl FileCopyChunk {
    /// Build a chunk and compute its checksum.
    pub fn new(index: u64, offset: u64, data: Vec<u8>, last: bool) -> Self {
        let checksum = sha256(&data);
        Self {
            index,
            offset,
            last,
            checksum,
            data,
        }
    }

    /// Encode this chunk into its wire representation.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(FILE_COPY_HEADER_LEN + self.data.len());
        out.extend_from_slice(&FILE_COPY_MAGIC);
        out.push(FILE_COPY_VERSION);
        out.push(if self.last { FLAG_LAST } else { 0 });
        put_u16_le(&mut out, 0);
        put_u64_le(&mut out, self.index);
        put_u64_le(&mut out, self.offset);
        put_u32_le(&mut out, self.data.len() as u32);
        out.extend_from_slice(&self.checksum);
        out.extend_from_slice(&self.data);
        out
    }

    /// Decode and validate a complete chunk frame.
    pub fn decode(frame: &[u8]) -> Result<Self> {
        if frame.len() < FILE_COPY_HEADER_LEN {
            return Err(Error::IncompleteFrame);
        }
        if frame[..4] != FILE_COPY_MAGIC {
            return Err(Error::InvalidFrame("bad magic"));
        }
        if frame[4] != FILE_COPY_VERSION {
            return Err(Error::InvalidFrame("unsupported version"));
        }
        let flags = frame[5];
        if flags & !FLAG_LAST != 0 {
            return Err(Error::InvalidFrame("unknown flags"));
        }
        if get_u16_le(&frame[6..8])? != 0 {
            return Err(Error::InvalidFrame("non-zero reserved field"));
        }
        let index = get_u64_le(&frame[8..16])?;
        let offset = get_u64_le(&frame[16..24])?;
        let data_len = get_u32_le(&frame[24..28])? as usize;
        if data_len > FILE_COPY_MAX_CHUNK_SIZE {
            return Err(Error::InvalidFrame("chunk data too large"));
        }
        let total = FILE_COPY_HEADER_LEN + data_len;
        if frame.len() < total {
            return Err(Error::IncompleteFrame);
        }
        if frame.len() != total {
            return Err(Error::InvalidFrame("trailing bytes after chunk"));
        }
        let mut checksum = [0u8; 32];
        checksum.copy_from_slice(&frame[28..60]);
        let data = frame[FILE_COPY_HEADER_LEN..].to_vec();
        if sha256(&data) != checksum {
            return Err(Error::BadChecksum);
        }
        Ok(Self {
            index,
            offset,
            last: flags & FLAG_LAST != 0,
            checksum,
            data,
        })
    }
}

/// Incremental decoder for chunk frames. Handles arbitrary splits.
#[derive(Debug, Clone)]
pub struct ChunkDecoder {
    buf: Vec<u8>,
    max_chunk_size: usize,
}

impl Default for ChunkDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ChunkDecoder {
    /// Create a decoder with the default maximum chunk size.
    pub fn new() -> Self {
        Self::with_max_chunk_size(FILE_COPY_MAX_CHUNK_SIZE)
    }

    /// Create a decoder with an explicit maximum payload size.
    pub fn with_max_chunk_size(max_chunk_size: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_chunk_size,
        }
    }

    /// Number of buffered bytes belonging to a partial frame.
    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }

    /// Feed bytes and return all complete chunks.
    pub fn push(&mut self, data: &[u8]) -> Result<Vec<FileCopyChunk>> {
        self.buf.extend_from_slice(data);
        let mut chunks = Vec::new();
        loop {
            if self.buf.len() < FILE_COPY_HEADER_LEN {
                break;
            }
            if self.buf[..4] != FILE_COPY_MAGIC {
                self.buf.clear();
                return Err(Error::InvalidFrame("bad magic"));
            }
            let data_len = get_u32_le(&self.buf[24..28])? as usize;
            if data_len > self.max_chunk_size {
                self.buf.clear();
                return Err(Error::InvalidFrame("chunk data too large"));
            }
            let total = FILE_COPY_HEADER_LEN + data_len;
            if self.buf.len() < total {
                break;
            }
            let frame: Vec<u8> = self.buf.drain(..total).collect();
            chunks.push(FileCopyChunk::decode(&frame)?);
        }
        Ok(chunks)
    }

    /// Assert that the stream ended on a frame boundary.
    pub fn finish(self) -> Result<()> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(Error::IncompleteFrame)
        }
    }
}

/// Reads a [`Read`] source and produces [`FileCopyChunk`] values.
#[derive(Debug)]
pub struct ChunkedReader<R> {
    inner: R,
    chunk_size: usize,
    index: u64,
    offset: u64,
    eof: bool,
}

impl<R: Read> ChunkedReader<R> {
    /// Create a reader with the default 32 KiB chunk size.
    pub fn new(inner: R) -> Self {
        Self::with_chunk_size(inner, FILE_COPY_CHUNK_SIZE)
    }

    /// Create a reader with an explicit chunk size.
    pub fn with_chunk_size(inner: R, chunk_size: usize) -> Self {
        Self {
            inner,
            chunk_size: chunk_size.max(1),
            index: 0,
            offset: 0,
            eof: false,
        }
    }

    /// Read the next chunk. Returns `None` after the last chunk has been
    /// emitted.
    pub fn next_chunk(&mut self) -> Result<Option<FileCopyChunk>> {
        if self.eof {
            return Ok(None);
        }
        let mut buf = vec![0u8; self.chunk_size];
        let mut filled = 0usize;
        while filled < self.chunk_size {
            let read = self.inner.read(&mut buf[filled..])?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        if filled == 0 {
            self.eof = true;
            // Empty file, or a full previous chunk followed by EOF.
            return Ok(Some(FileCopyChunk::new(
                self.index,
                self.offset,
                Vec::new(),
                true,
            )));
        }
        let last = filled < self.chunk_size;
        if last {
            self.eof = true;
        }
        buf.truncate(filled);
        let chunk = FileCopyChunk::new(self.index, self.offset, buf, last);
        self.index += 1;
        self.offset += filled as u64;
        Ok(Some(chunk))
    }
}

/// Writes and validates a stream of [`FileCopyChunk`] values.
#[derive(Debug)]
pub struct FileCopyReceiver<W> {
    output: W,
    expected_index: u64,
    expected_offset: u64,
    last_seen: bool,
}

impl<W: Write> FileCopyReceiver<W> {
    /// Create a receiver writing to `output`.
    pub fn new(output: W) -> Self {
        Self {
            output,
            expected_index: 0,
            expected_offset: 0,
            last_seen: false,
        }
    }

    /// Validate and write one chunk.
    pub fn accept(&mut self, chunk: &FileCopyChunk) -> Result<()> {
        if chunk.index != self.expected_index || chunk.offset != self.expected_offset {
            return Err(Error::OutOfOrder {
                expected_index: self.expected_index,
                expected_offset: self.expected_offset,
                got_index: chunk.index,
                got_offset: chunk.offset,
            });
        }
        if sha256(&chunk.data) != chunk.checksum {
            return Err(Error::BadChecksum);
        }
        self.output.write_all(&chunk.data)?;
        self.expected_index += 1;
        self.expected_offset = self.expected_offset.saturating_add(chunk.data.len() as u64);
        if chunk.last {
            self.last_seen = true;
        }
        Ok(())
    }

    /// Flush and check that the last chunk was received.
    pub fn finish(&mut self) -> Result<()> {
        if !self.last_seen {
            return Err(Error::MissingLastChunk);
        }
        self.output.flush()?;
        Ok(())
    }

    /// Consume the receiver, returning the writer after a successful finish.
    pub fn into_inner(mut self) -> Result<W> {
        self.finish()?;
        Ok(self.output)
    }
}

/// Copy all of `input` to `output` using the chunked protocol.
pub fn copy_chunked<R: Read, W: Write>(input: &mut R, output: &mut W) -> Result<()> {
    let mut reader = ChunkedReader::new(input);
    let mut receiver = FileCopyReceiver::new(output);
    while let Some(chunk) = reader.next_chunk()? {
        receiver.accept(&chunk)?;
    }
    receiver.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File, OpenOptions};
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempPath(PathBuf);
    impl TempPath {
        fn new(tag: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut path = std::env::temp_dir();
            path.push(format!("kitty-remote-{tag}-{}-{n}.tmp", std::process::id()));
            Self(path)
        }
        fn path(&self) -> &PathBuf {
            &self.0
        }
    }
    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn write_temp(tag: &str, data: &[u8]) -> (TempPath, File) {
        let temp = TempPath::new(tag);
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(temp.path())
            .unwrap();
        file.set_len(0).unwrap();
        let mut file = file;
        file.write_all(data).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        (temp, file)
    }

    fn read_temp(path: &PathBuf) -> Vec<u8> {
        fs::read(path).unwrap()
    }

    #[test]
    fn copy_between_files_full_and_range() {
        let data: Vec<u8> = (0..10_000u32).map(|x| (x % 251) as u8).collect();
        let (_in_path, mut input) = write_temp("copy-in", &data);
        let (out_path, mut output) = write_temp("copy-out", b"");
        copy_between_files(&mut input, &mut output, 0, data.len() as u64).unwrap();
        output.flush().unwrap();
        assert_eq!(read_temp(out_path.path()), data);

        let (out_path2, mut output2) = write_temp("copy-out2", b"");
        copy_between_files(&mut input, &mut output2, 1000, 2500).unwrap();
        output2.flush().unwrap();
        assert_eq!(read_temp(out_path2.path()), &data[1000..3500]);
    }

    #[test]
    fn copy_between_files_buffer_boundaries() {
        let data: Vec<u8> = (0..1000u32).map(|x| (x * 7 % 256) as u8).collect();
        let (_in_path, mut input) = write_temp("boundary-in", &data);
        let (out_path, mut output) = write_temp("boundary-out", b"");
        copy_between_files_with_buffer(&mut input, &mut output, 0, data.len() as u64, 3).unwrap();
        output.flush().unwrap();
        assert_eq!(read_temp(out_path.path()), data);
    }

    #[test]
    fn copy_between_files_empty_and_past_eof() {
        let (_in_path, mut input) = write_temp("eof-in", b"abc");
        let (out_path, mut output) = write_temp("eof-out", b"");
        copy_between_files(&mut input, &mut output, 0, 0).unwrap();
        output.flush().unwrap();
        assert!(read_temp(out_path.path()).is_empty());

        let mut output = Vec::new();
        let err = copy_between_files(&mut input, &mut output, 0, 4).unwrap_err();
        match err {
            Error::Io(e) => assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn chunk_frame_roundtrip() {
        let chunk = FileCopyChunk::new(3, 96, b"some payload".to_vec(), true);
        let frame = chunk.encode();
        assert_eq!(frame.len(), FILE_COPY_HEADER_LEN + chunk.data.len());
        assert_eq!(FileCopyChunk::decode(&frame).unwrap(), chunk);
    }

    #[test]
    fn chunk_decoder_split_at_every_byte() {
        let chunk = FileCopyChunk::new(0, 0, b"split me across buffers".to_vec(), false);
        let frame = chunk.encode();
        for split in 0..=frame.len() {
            let mut decoder = ChunkDecoder::new();
            let mut chunks = decoder.push(&frame[..split]).unwrap();
            chunks.extend(decoder.push(&frame[split..]).unwrap());
            assert_eq!(chunks, vec![chunk.clone()], "split at {split}");
            decoder.finish().unwrap();
        }
    }

    #[test]
    fn chunk_decoder_one_byte_at_a_time_and_coalesced() {
        let a = FileCopyChunk::new(0, 0, b"first".to_vec(), false);
        let b = FileCopyChunk::new(1, 5, b"second".to_vec(), true);
        let mut wire = a.encode();
        wire.extend_from_slice(&b.encode());
        let mut decoder = ChunkDecoder::new();
        let mut chunks = Vec::new();
        for byte in wire {
            chunks.extend(decoder.push(&[byte]).unwrap());
        }
        assert_eq!(chunks, vec![a, b]);
        decoder.finish().unwrap();
    }

    #[test]
    fn bad_checksum_is_rejected() {
        let chunk = FileCopyChunk::new(0, 0, b"payload".to_vec(), true);
        let mut frame = chunk.encode();
        let last = frame.len() - 1;
        frame[last] ^= 1;
        assert!(matches!(
            FileCopyChunk::decode(&frame),
            Err(Error::BadChecksum)
        ));

        let mut decoder = ChunkDecoder::new();
        assert!(matches!(decoder.push(&frame), Err(Error::BadChecksum)));
    }

    #[test]
    fn bad_header_is_rejected() {
        let chunk = FileCopyChunk::new(0, 0, b"payload".to_vec(), true);
        let mut frame = chunk.encode();
        frame[0] = b'X';
        assert!(matches!(
            FileCopyChunk::decode(&frame),
            Err(Error::InvalidFrame("bad magic"))
        ));

        let mut frame = chunk.encode();
        frame[4] = 99;
        assert!(matches!(
            FileCopyChunk::decode(&frame),
            Err(Error::InvalidFrame("unsupported version"))
        ));

        let mut frame = chunk.encode();
        frame[5] = 0x80;
        assert!(matches!(
            FileCopyChunk::decode(&frame),
            Err(Error::InvalidFrame("unknown flags"))
        ));

        let mut frame = chunk.encode();
        frame[6] = 1;
        assert!(matches!(
            FileCopyChunk::decode(&frame),
            Err(Error::InvalidFrame("non-zero reserved field"))
        ));

        let mut frame = chunk.encode();
        frame[24] = 255;
        frame[25] = 255;
        frame[26] = 255;
        frame[27] = 255;
        assert!(matches!(
            FileCopyChunk::decode(&frame),
            Err(Error::InvalidFrame("chunk data too large"))
        ));
    }

    #[test]
    fn truncated_and_trailing_frames_are_rejected() {
        let chunk = FileCopyChunk::new(0, 0, b"payload".to_vec(), true);
        let frame = chunk.encode();
        assert!(matches!(
            FileCopyChunk::decode(&frame[..FILE_COPY_HEADER_LEN - 1]),
            Err(Error::IncompleteFrame)
        ));
        assert!(matches!(
            FileCopyChunk::decode(&frame[..frame.len() - 1]),
            Err(Error::IncompleteFrame)
        ));
        let mut trailing = frame.clone();
        trailing.push(0);
        assert!(matches!(
            FileCopyChunk::decode(&trailing),
            Err(Error::InvalidFrame("trailing bytes after chunk"))
        ));

        let mut decoder = ChunkDecoder::new();
        decoder.push(&frame[..10]).unwrap();
        assert!(matches!(decoder.finish(), Err(Error::IncompleteFrame)));
    }

    #[test]
    fn chunked_file_roundtrip_partial_chunks() {
        let data: Vec<u8> = (0..1234u32).map(|x| (x % 257) as u8).collect();
        let mut reader = ChunkedReader::with_chunk_size(Cursor::new(data.clone()), 7);
        let mut receiver = FileCopyReceiver::new(Vec::new());
        while let Some(chunk) = reader.next_chunk().unwrap() {
            assert!(chunk.data.len() <= 7);
            receiver.accept(&chunk).unwrap();
        }
        receiver.finish().unwrap();
        assert_eq!(receiver.into_inner().unwrap(), data);
    }

    #[test]
    fn chunked_empty_file_roundtrip() {
        let mut reader = ChunkedReader::new(Cursor::new(Vec::<u8>::new()));
        let first = reader.next_chunk().unwrap().unwrap();
        assert!(first.last);
        assert!(first.data.is_empty());
        assert_eq!(first.index, 0);
        assert!(reader.next_chunk().unwrap().is_none());

        let mut output = Vec::new();
        copy_chunked(&mut Cursor::new(Vec::<u8>::new()), &mut output).unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn chunked_exact_multiple_gets_empty_last_chunk() {
        let data = vec![9u8; 14];
        let mut reader = ChunkedReader::with_chunk_size(Cursor::new(data.clone()), 7);
        let a = reader.next_chunk().unwrap().unwrap();
        let b = reader.next_chunk().unwrap().unwrap();
        let c = reader.next_chunk().unwrap().unwrap();
        assert_eq!(a.data.len(), 7);
        assert!(!a.last);
        assert_eq!(b.data.len(), 7);
        assert!(!b.last);
        assert!(c.last);
        assert!(c.data.is_empty());
        assert!(reader.next_chunk().unwrap().is_none());

        let mut output = Vec::new();
        copy_chunked(&mut Cursor::new(data.clone()), &mut output).unwrap();
        assert_eq!(output, data);
    }

    #[test]
    fn receiver_rejects_out_of_order_and_missing_last() {
        let a = FileCopyChunk::new(0, 0, b"abc".to_vec(), false);
        let b = FileCopyChunk::new(1, 3, b"def".to_vec(), true);
        let mut receiver = FileCopyReceiver::new(Vec::new());
        assert!(matches!(
            receiver.accept(&b),
            Err(Error::OutOfOrder {
                expected_index: 0,
                expected_offset: 0,
                got_index: 1,
                got_offset: 3
            })
        ));
        receiver.accept(&a).unwrap();
        assert!(matches!(receiver.accept(&a), Err(Error::OutOfOrder { .. })));
        receiver.accept(&b).unwrap();
        receiver.finish().unwrap();

        let mut receiver = FileCopyReceiver::new(Vec::new());
        receiver.accept(&a).unwrap();
        assert!(matches!(receiver.finish(), Err(Error::MissingLastChunk)));
    }

    #[test]
    fn receiver_rejects_bad_checksum_in_struct() {
        let mut chunk = FileCopyChunk::new(0, 0, b"abc".to_vec(), true);
        chunk.data[0] ^= 1;
        let mut receiver = FileCopyReceiver::new(Vec::new());
        assert!(matches!(receiver.accept(&chunk), Err(Error::BadChecksum)));
    }

    #[test]
    fn chunk_size_limit_is_enforced() {
        let chunk = FileCopyChunk::new(0, 0, vec![0u8; 16], true);
        let frame = chunk.encode();
        let mut decoder = ChunkDecoder::with_max_chunk_size(8);
        assert!(matches!(
            decoder.push(&frame),
            Err(Error::InvalidFrame("chunk data too large"))
        ));
    }

    #[test]
    fn copy_chunked_temp_files_roundtrip() {
        let data: Vec<u8> = (0..70_000u32).map(|x| (x * 13 % 256) as u8).collect();
        let (_in_path, input) = write_temp("chunk-in", &data);
        let (out_path, output) = write_temp("chunk-out", b"");
        let mut input = input;
        let mut output = output;
        copy_chunked(&mut input, &mut output).unwrap();
        output.flush().unwrap();
        assert_eq!(read_temp(out_path.path()), data);
    }
}
