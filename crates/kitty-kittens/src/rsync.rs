// SPDX-License-Identifier: GPL-3.0-or-later
//! The rsync-style block-delta algorithm used by the transfer kitten.
//!
//! This is a Rust port of `tools/rsync/algorithm.go` (the Go implementation
//! shipped with kitty) and `kittens/transfer/algorithm.c`. The wire format is
//! deliberately byte-compatible: a 12-byte little-endian signature header
//! followed by 20-byte `(index, weak, strong)` block hashes, and a delta
//! stream made of block/block-range/data/hash operations.

use crate::xxh3;

/// Block size used when the file size is unknown.
pub const DEFAULT_BLOCK_SIZE: usize = 6 * 1024;
/// Largest block size accepted in a signature header.
pub const MAX_BLOCK_SIZE: usize = 1024 * 1024;
/// Serialized size of the signature header.
pub const SIGNATURE_HEADER_SIZE: usize = 12;
/// Serialized size of one block hash.
pub const BLOCK_HASH_SIZE: usize = 20;

const M: u32 = 1 << 16;
const OP_BLOCK: u8 = 0;
const OP_DATA: u8 = 1;
const OP_HASH: u8 = 2;
const OP_BLOCK_RANGE: u8 = 3;

#[derive(Debug)]
pub enum RsyncError {
    BadSignature(String),
    BadDelta(String),
    ChecksumMismatch { actual: String, expected: String },
    MissingChecksum,
}

impl std::fmt::Display for RsyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadSignature(msg) | Self::BadDelta(msg) => f.write_str(msg),
            Self::ChecksumMismatch { actual, expected } => write!(
                f,
                "Failed to verify overall file checksum actual: {actual} != expected: {expected}, this usually happens because one of the involved files was altered while the operation was in progress."
            ),
            Self::MissingChecksum => {
                f.write_str("The checksum was not received at the end of the delta data")
            }
        }
    }
}

impl std::error::Error for RsyncError {}

/// A single weak/strong hash pair for one source block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockHash {
    pub index: u64,
    pub weak_hash: u32,
    pub strong_hash: u64,
}

/// A parsed signature: the block size and every block hash.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Signature {
    pub block_size: u32,
    pub blocks: Vec<BlockHash>,
}

/// Compute the block size used for a file of `expected_size` bytes.
pub fn block_size_for(expected_size: u64) -> usize {
    if expected_size == 0 {
        DEFAULT_BLOCK_SIZE
    } else {
        let root = (expected_size as f64).sqrt().round() as usize;
        root.max(1)
    }
}

/// Serialize a signature header with the current protocol version.
pub fn signature_header(block_size: u32) -> [u8; SIGNATURE_HEADER_SIZE] {
    let mut out = [0u8; SIGNATURE_HEADER_SIZE];
    out[8..12].copy_from_slice(&block_size.to_le_bytes());
    out
}

/// Parse and validate a signature header, returning the block size.
pub fn parse_signature_header(data: &[u8]) -> Result<u32, RsyncError> {
    if data.len() < SIGNATURE_HEADER_SIZE {
        return Err(RsyncError::BadSignature(
            "signature header is too short".to_owned(),
        ));
    }
    let mut fields = [0u16; 4];
    for (index, field) in fields.iter_mut().enumerate() {
        *field = u16::from_le_bytes([data[index * 2], data[index * 2 + 1]]);
    }
    for (index, field) in fields.iter().enumerate() {
        if *field != 0 {
            let what = match index {
                0 => "version",
                1 => "checksum type",
                2 => "strong hash type",
                _ => "weak hash type",
            };
            return Err(RsyncError::BadSignature(format!(
                "Invalid {what} in signature header: {field}"
            )));
        }
    }
    let block_size = u32::from_le_bytes(data[8..12].try_into().expect("four bytes"));
    if block_size == 0 {
        return Err(RsyncError::BadSignature(
            "Signature header has zero block size".to_owned(),
        ));
    }
    if block_size as usize > MAX_BLOCK_SIZE {
        return Err(RsyncError::BadSignature(format!(
            "Signature header has too large block size {block_size} > {MAX_BLOCK_SIZE}"
        )));
    }
    Ok(block_size)
}

fn rolling_full(data: &[u8]) -> (u32, Rolling) {
    let l = data.len() as u32;
    let mut alpha = 0u32;
    let mut beta = 0u32;
    for (i, byte) in data.iter().enumerate() {
        alpha = alpha.wrapping_add(u32::from(*byte));
        beta = beta.wrapping_add((l - i as u32).wrapping_mul(u32::from(*byte)));
    }
    let first = data.first().copied().unwrap_or(0);
    let alpha = alpha % M;
    let beta = beta % M;
    (
        alpha.wrapping_add(M.wrapping_mul(beta)),
        Rolling {
            alpha,
            beta,
            l,
            first_byte_of_previous_window: u32::from(first),
        },
    )
}

#[derive(Clone, Copy, Debug, Default)]
struct Rolling {
    alpha: u32,
    beta: u32,
    l: u32,
    first_byte_of_previous_window: u32,
}

impl Rolling {
    fn add_one_byte(&mut self, first_byte: u8, last_byte: u8) -> u32 {
        self.alpha = (self
            .alpha
            .wrapping_sub(self.first_byte_of_previous_window)
            .wrapping_add(u32::from(last_byte)))
            % M;
        self.beta = (self
            .beta
            .wrapping_sub(self.l.wrapping_mul(self.first_byte_of_previous_window))
            .wrapping_add(self.alpha))
            % M;
        self.first_byte_of_previous_window = u32::from(first_byte);
        self.alpha.wrapping_add(M.wrapping_mul(self.beta))
    }
}

/// Create the serialized signature of `data`.
pub fn create_signature(data: &[u8], block_size: u32) -> Result<Vec<u8>, RsyncError> {
    if block_size == 0 {
        return Err(RsyncError::BadSignature(
            "signature block size must not be zero".to_owned(),
        ));
    }
    let block_size_usize = block_size as usize;
    let count = data.len().div_ceil(block_size_usize);
    let mut out = Vec::with_capacity(SIGNATURE_HEADER_SIZE + count * BLOCK_HASH_SIZE);
    out.extend_from_slice(&signature_header(block_size));
    for (index, block) in data.chunks(block_size_usize).enumerate() {
        let (weak, _) = rolling_full(block);
        let strong = xxh3::hash64(block);
        out.extend_from_slice(&(index as u64).to_le_bytes());
        out.extend_from_slice(&weak.to_le_bytes());
        out.extend_from_slice(&strong.to_le_bytes());
    }
    Ok(out)
}

/// Parse a full serialized signature.
pub fn parse_signature(data: &[u8]) -> Result<Signature, RsyncError> {
    let block_size = parse_signature_header(data)?;
    let mut blocks = Vec::new();
    let mut offset = SIGNATURE_HEADER_SIZE;
    while offset < data.len() {
        if data.len() - offset < BLOCK_HASH_SIZE {
            return Err(RsyncError::BadSignature(format!(
                "{} bytes of unused signature data",
                data.len() - offset
            )));
        }
        let index = u64::from_le_bytes(data[offset..offset + 8].try_into().expect("8 bytes"));
        let weak = u32::from_le_bytes(data[offset + 8..offset + 12].try_into().expect("4 bytes"));
        let strong =
            u64::from_le_bytes(data[offset + 12..offset + 20].try_into().expect("8 bytes"));
        blocks.push(BlockHash {
            index,
            weak_hash: weak,
            strong_hash: strong,
        });
        offset += BLOCK_HASH_SIZE;
    }
    Ok(Signature { block_size, blocks })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Block(u64),
    Range(u64, u64),
}

/// Create a delta that transforms `signature` (the target) into `source`.
pub fn create_delta(source: &[u8], signature: &Signature) -> Result<Vec<u8>, RsyncError> {
    let block_size = signature.block_size as usize;
    if block_size == 0 {
        return Err(RsyncError::BadSignature(
            "signature block size must not be zero".to_owned(),
        ));
    }
    if block_size > MAX_BLOCK_SIZE {
        return Err(RsyncError::BadSignature(format!(
            "Signature header has too large block size {block_size} > {MAX_BLOCK_SIZE}"
        )));
    }
    let mut by_weak: std::collections::HashMap<u32, Vec<BlockHash>> =
        std::collections::HashMap::with_capacity(signature.blocks.len());
    for block in &signature.blocks {
        by_weak.entry(block.weak_hash).or_default().push(*block);
    }
    let mut out = Vec::new();
    let mut pending: Option<Pending> = None;
    let mut data_start = 0usize;
    let mut pos = 0usize;

    macro_rules! flush_pending {
        () => {
            if let Some(op) = pending.take() {
                emit_pending(&mut out, op);
            }
        };
    }
    macro_rules! flush_data {
        ($end:expr) => {
            if $end > data_start {
                flush_pending!();
                emit_data(&mut out, &source[data_start..$end]);
            }
        };
    }
    macro_rules! enqueue_block {
        ($index:expr) => {
            match pending {
                Some(Pending::Block(prev)) if prev + 1 == $index => {
                    pending = Some(Pending::Range(prev, $index));
                }
                Some(Pending::Range(start, end)) if end + 1 == $index => {
                    pending = Some(Pending::Range(start, $index));
                }
                Some(op) => {
                    emit_pending(&mut out, op);
                    pending = Some(Pending::Block($index));
                }
                None => pending = Some(Pending::Block($index)),
            }
        };
    }

    while !source.is_empty() && pos < source.len() {
        let remaining = source.len() - pos;
        if remaining < block_size {
            flush_data!(source.len());
            break;
        }
        let window = &source[pos..pos + block_size];
        let (weak, mut rolling) = rolling_full(window);
        let mut matched = by_weak.get(&weak).and_then(|candidates| {
            let strong = xxh3::hash64(window);
            candidates
                .iter()
                .find(|block| block.strong_hash == strong)
                .map(|block| block.index)
        });
        if let Some(index) = matched {
            flush_data!(pos);
            enqueue_block!(index);
            pos += block_size;
            data_start = pos;
        } else {
            // Slide one byte at a time until the window matches or less than a
            // block remains.
            let mut next = pos + 1;
            loop {
                if source.len() - next < block_size {
                    break;
                }
                let weak = rolling.add_one_byte(source[next], source[next + block_size - 1]);
                if let Some(candidates) = by_weak.get(&weak) {
                    let window = &source[next..next + block_size];
                    let strong = xxh3::hash64(window);
                    if let Some(index) = candidates
                        .iter()
                        .find(|block| block.strong_hash == strong)
                        .map(|block| block.index)
                    {
                        matched = Some(index);
                        break;
                    }
                }
                next += 1;
            }
            if let Some(index) = matched {
                flush_data!(next);
                enqueue_block!(index);
                pos = next + block_size;
                data_start = pos;
            } else {
                // No match anywhere in the remaining data: emit everything.
                flush_data!(source.len());
                break;
            }
        }
    }
    flush_pending!();
    let checksum = xxh3::hash128(source).to_be_bytes();
    out.push(OP_HASH);
    out.extend_from_slice(&(checksum.len() as u16).to_le_bytes());
    out.extend_from_slice(&checksum);
    Ok(out)
}

fn emit_data(out: &mut Vec<u8>, data: &[u8]) {
    const MAX_CHUNK: usize = u32::MAX as usize;
    for chunk in data.chunks(MAX_CHUNK) {
        out.push(OP_DATA);
        out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(chunk);
    }
}

fn emit_pending(out: &mut Vec<u8>, op: Pending) {
    match op {
        Pending::Block(index) => {
            out.push(OP_BLOCK);
            out.extend_from_slice(&index.to_le_bytes());
        }
        Pending::Range(start, end) => {
            out.push(OP_BLOCK_RANGE);
            out.extend_from_slice(&start.to_le_bytes());
            out.extend_from_slice(&((end - start) as u32).to_le_bytes());
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation<'a> {
    Block(u64),
    Range(u64, u64),
    Data(&'a [u8]),
    Hash(&'a [u8]),
}

fn parse_operation(data: &[u8]) -> Result<(Operation<'_>, usize), RsyncError> {
    let Some(kind) = data.first().copied() else {
        return Err(RsyncError::BadDelta("empty operation".to_owned()));
    };
    match kind {
        OP_BLOCK => {
            if data.len() < 9 {
                return Err(RsyncError::BadDelta("truncated block operation".to_owned()));
            }
            Ok((
                Operation::Block(u64::from_le_bytes(data[1..9].try_into().expect("8 bytes"))),
                9,
            ))
        }
        OP_BLOCK_RANGE => {
            if data.len() < 13 {
                return Err(RsyncError::BadDelta(
                    "truncated block range operation".to_owned(),
                ));
            }
            let start = u64::from_le_bytes(data[1..9].try_into().expect("8 bytes"));
            let span = u32::from_le_bytes(data[9..13].try_into().expect("4 bytes")) as u64;
            Ok((Operation::Range(start, start + span), 13))
        }
        OP_HASH => {
            if data.len() < 3 {
                return Err(RsyncError::BadDelta("truncated hash operation".to_owned()));
            }
            let len = u16::from_le_bytes(data[1..3].try_into().expect("2 bytes")) as usize;
            if data.len() < 3 + len {
                return Err(RsyncError::BadDelta("truncated hash data".to_owned()));
            }
            Ok((Operation::Hash(&data[3..3 + len]), 3 + len))
        }
        OP_DATA => {
            if data.len() < 5 {
                return Err(RsyncError::BadDelta("truncated data operation".to_owned()));
            }
            let len = u32::from_le_bytes(data[1..5].try_into().expect("4 bytes")) as usize;
            if data.len() < 5 + len {
                return Err(RsyncError::BadDelta("truncated data payload".to_owned()));
            }
            Ok((Operation::Data(&data[5..5 + len]), 5 + len))
        }
        other => Err(RsyncError::BadDelta(format!(
            "record has unknown operation type: {other}"
        ))),
    }
}

/// Apply a delta to `target`, returning the final contents.
#[allow(dead_code)]
pub fn apply_delta(target: &[u8], delta: &[u8]) -> Result<Vec<u8>, RsyncError> {
    let block_size = DEFAULT_BLOCK_SIZE;
    apply_delta_with_block_size(target, delta, block_size)
}

/// Apply a delta when the block size is known from the signature header.
pub fn apply_delta_with_block_size(
    target: &[u8],
    delta: &[u8],
    block_size: usize,
) -> Result<Vec<u8>, RsyncError> {
    apply_delta_impl(target, delta, block_size, true)
}

fn apply_delta_impl(
    target: &[u8],
    delta: &[u8],
    block_size: usize,
    verify: bool,
) -> Result<Vec<u8>, RsyncError> {
    if block_size == 0 {
        return Err(RsyncError::BadSignature(
            "signature block size must not be zero".to_owned(),
        ));
    }
    let mut out = Vec::new();
    let mut offset = 0usize;
    let mut checksum: Option<Vec<u8>> = None;
    let copy_block = |out: &mut Vec<u8>, index: u64| -> Result<(), RsyncError> {
        let start = index
            .checked_mul(block_size as u64)
            .ok_or_else(|| RsyncError::BadDelta("block index overflow".to_owned()))?
            as usize;
        if start >= target.len() {
            return Err(RsyncError::BadDelta(format!(
                "block index {index} is outside the target file"
            )));
        }
        let end = (start + block_size).min(target.len());
        out.extend_from_slice(&target[start..end]);
        Ok(())
    };
    while offset < delta.len() {
        let (operation, consumed) = parse_operation(&delta[offset..])?;
        offset += consumed;
        match operation {
            Operation::Block(index) => copy_block(&mut out, index)?,
            Operation::Range(start, end) => {
                for index in start..=end {
                    copy_block(&mut out, index)?;
                }
            }
            Operation::Data(data) => out.extend_from_slice(data),
            Operation::Hash(expected) => {
                if checksum.is_some() {
                    return Err(RsyncError::BadDelta(
                        "multiple checksum operations in delta".to_owned(),
                    ));
                }
                checksum = Some(expected.to_vec());
            }
        }
    }
    let Some(expected) = checksum else {
        return Err(RsyncError::MissingChecksum);
    };
    let actual = xxh3::hash128(&out).to_be_bytes();
    if verify && actual.as_slice() != expected.as_slice() {
        let hex = |bytes: &[u8]| {
            let mut text = String::new();
            for byte in bytes {
                text.push_str(&format!("{byte:02x}"));
            }
            text
        };
        return Err(RsyncError::ChecksumMismatch {
            actual: hex(&actual),
            expected: hex(&expected),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xxh3_vectors_match_the_reference_implementation() {
        assert_eq!(xxh3::hash64(b""), 3244421341483603138);
        assert_eq!(xxh3::hash64(b"a"), 0xE6C632B61E964E1F);
        assert_eq!(xxh3::hash64(b"hello"), 0x9555_E855_5C62_DCFD);
        assert_eq!(xxh3::hash64(&b"abc".repeat(1000)), 0x1FA2_F942_314C_D628);
        assert_eq!(
            xxh3::hash128(&b"abc".repeat(1000)).to_be_bytes(),
            [
                0x14, 0xB4, 0x7F, 0xE9, 0x92, 0x59, 0x60, 0x70, 0x1F, 0xA2, 0xF9, 0x42, 0x31, 0x4C,
                0xD6, 0x28
            ]
        );
        assert_eq!(
            xxh3::hash128(b"hello").to_be_bytes(),
            [
                0xB5, 0xE9, 0xC1, 0xAD, 0x07, 0x1B, 0x3E, 0x7F, 0xC7, 0x79, 0xCF, 0xAA, 0x5E, 0x52,
                0x38, 0x18
            ]
        );
    }

    #[test]
    fn signature_roundtrip_and_header_validation() {
        let data = b"the quick brown fox jumps over the lazy dog".repeat(100);
        let signature = create_signature(&data, 64).unwrap();
        let parsed = parse_signature(&signature).unwrap();
        assert_eq!(parsed.block_size, 64);
        assert_eq!(parsed.blocks.len(), data.len().div_ceil(64));
        assert_eq!(parsed.blocks[0].index, 0);
        let mut bad = signature.clone();
        bad[0] = 1;
        assert!(parse_signature(&bad).is_err());
        let mut zero = signature_header(0);
        zero[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert!(parse_signature_header(&zero).is_err());
    }

    #[test]
    fn identical_file_produces_only_references() {
        let data = b"0123456789abcdef".repeat(2000);
        let signature = create_signature(&data, 512).unwrap();
        let parsed = parse_signature(&signature).unwrap();
        let delta = create_delta(&data, &parsed).unwrap();
        assert!(delta.len() < data.len() / 4, "delta={}", delta.len());
        assert_eq!(
            apply_delta_with_block_size(&data, &delta, 512).unwrap(),
            data
        );
    }

    #[test]
    fn changed_file_roundtrips_and_keeps_unchanged_blocks() {
        let original = b"the quick brown fox jumps over the lazy dog\n".repeat(500);
        let mut changed = original.clone();
        changed.extend_from_slice(b"one new line at the end\n");
        changed[100..110].copy_from_slice(b"0123456789");
        let signature = parse_signature(&create_signature(&original, 128).unwrap()).unwrap();
        let delta = create_delta(&changed, &signature).unwrap();
        assert!(delta.len() < changed.len());
        assert_eq!(
            apply_delta_with_block_size(&original, &delta, 128).unwrap(),
            changed
        );
    }

    #[test]
    fn corrupt_delta_is_rejected() {
        let original = b"abc".repeat(1000);
        let signature = parse_signature(&create_signature(&original, 128).unwrap()).unwrap();
        let mut delta = create_delta(&original, &signature).unwrap();
        let last = delta.len() - 1;
        delta[last] ^= 0xff;
        assert!(matches!(
            apply_delta_with_block_size(&original, &delta, 128),
            Err(RsyncError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn block_size_for_uses_square_root() {
        assert_eq!(block_size_for(0), DEFAULT_BLOCK_SIZE);
        assert_eq!(block_size_for(1_000_000), 1000);
        assert_eq!(block_size_for(1), 1);
    }
}
