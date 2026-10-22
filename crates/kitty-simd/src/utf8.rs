// SPDX-License-Identifier: GPL-3.0-or-later
//! UTF-8 decoding, including the DFA shared by all implementations.
//!
//! The DFA and the incremental/replacement semantics are a direct port of
//! `charsets.c` and `utf8_decode_to_esc_scalar()` from `simd-string.c`.

pub const UTF8_ACCEPT: u32 = 0;
pub const UTF8_REJECT: u32 = 1;

const UTF8_DATA: [u8; 400] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
    0x09, 0x09, 0x09, 0x09, 0x09, 0x09, 0x09, 0x09, 0x09, 0x09, 0x09, 0x09, 0x09, 0x09, 0x09, 0x09,
    0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07,
    0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07,
    0x08, 0x08, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02,
    0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02,
    0x0a, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x04, 0x03, 0x03,
    0x0b, 0x06, 0x06, 0x06, 0x05, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08,
    0x00, 0x01, 0x02, 0x03, 0x05, 0x08, 0x07, 0x01, 0x01, 0x01, 0x04, 0x06, 0x01, 0x01, 0x01, 0x01,
    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
    0x01, 0x00, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
    0x01, 0x02, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x01, 0x02, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
    0x01, 0x02, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x03, 0x01, 0x03, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
    0x01, 0x03, 0x01, 0x01, 0x01, 0x01, 0x01, 0x03, 0x01, 0x03, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
    0x01, 0x03, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
];

/// One step of the Bjoern Hoehrmann UTF-8 DFA.
#[inline]
pub fn decode_utf8(state: &mut u32, codep: &mut u32, byte: u8) -> u32 {
    let ty = UTF8_DATA[byte as usize] as u32;
    *codep = if *state != UTF8_ACCEPT {
        (byte as u32 & 0x3f) | (*codep << 6)
    } else {
        (0xff >> ty) & byte as u32
    };
    *state = UTF8_DATA[256 + (*state as usize) * 16 + ty as usize] as u32;
    *state
}

/// Incremental UTF-8 decoder with the same state layout and semantics as the C
/// `UTF8Decoder`.
///
/// `output` holds the codepoints decoded by the most recent call to
/// [`UTF8Decoder::decode_to_esc`]. The DFA state (`cur`, `prev`, `codep`) is
/// carried across calls, so feeding a byte stream in arbitrarily split pieces
/// produces exactly the same result as feeding it all at once.
#[derive(Clone, Debug, Default)]
pub struct UTF8Decoder {
    /// Decoded codepoints of the current call (also the output buffer storage).
    pub output: Vec<u32>,
    /// Current DFA state.
    pub cur: u32,
    /// DFA state before the most recently processed byte.
    pub prev: u32,
    /// Accumulated codepoint bits.
    pub codep: u32,
    /// Number of input bytes consumed by the current call.
    pub num_consumed: usize,
}

impl UTF8Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Zero the DFA state, exactly like `utf8_decoder_reset()`.
    pub fn reset(&mut self) {
        self.cur = 0;
        self.prev = 0;
        self.codep = 0;
    }

    /// Codepoints decoded by the most recent call.
    pub fn decoded(&self) -> &[u32] {
        &self.output
    }

    /// Decode `src` until an ESC (0x1b) sentinel is found or the input is
    /// exhausted. Returns `true` when the sentinel was found. The sentinel
    /// itself is consumed but not added to the output.
    ///
    /// This is the same entry point as [`crate::utf8_decode_to_esc`] and uses
    /// runtime SIMD dispatch. Like the C vector decoder, a call stops after the
    /// chunk containing invalid UTF-8 (the chunk itself is decoded through the
    /// scalar fallback); `num_consumed` reports the progress and the caller
    /// must call again with `&src[num_consumed..]` until the input is
    /// exhausted or the sentinel is found. This is exactly the loop used by
    /// the C test harness in `kitty_tests/parser.py`.
    pub fn decode_to_esc(&mut self, src: &[u8]) -> bool {
        crate::utf8_decode_to_esc(self, src)
    }

    #[inline]
    pub(crate) fn begin_call(&mut self) {
        self.output.clear();
        self.num_consumed = 0;
    }

    /// Ensure room for `additional` output codepoints plus the overwrite slack
    /// the SIMD writers rely on (the C code reserves 64 extra bytes).
    #[inline]
    pub(crate) fn ensure_capacity(&mut self, additional: usize) {
        self.output.reserve(additional + 64);
    }

    #[inline]
    pub(crate) fn push_codepoint(&mut self, ch: u32) {
        self.output.push(ch);
    }

    /// Raw output pointer for the SIMD writers. The caller must have reserved
    /// enough capacity and must publish the written length with
    /// [`Self::set_output_len`].
    #[inline]
    pub(crate) fn output_ptr(&mut self) -> *mut u32 {
        self.output.as_mut_ptr()
    }

    #[inline]
    pub(crate) fn output_len(&self) -> usize {
        self.output.len()
    }

    /// Publish `len` initialized codepoints in the output buffer.
    #[inline]
    pub(crate) fn set_output_len(&mut self, len: usize) {
        assert!(len <= self.output.capacity());
        // SAFETY: the caller has written `len` initialized u32 values into the
        // spare capacity obtained from output_ptr().
        unsafe { self.output.set_len(len) };
    }
}

/// Scalar decode helper used by the SIMD implementations for their slow path.
/// Returns the number of bytes consumed.
pub fn decode_all_scalar(d: &mut UTF8Decoder, src: &[u8]) -> usize {
    let mut pos = 0;
    while pos < src.len() {
        let ch = src[pos];
        pos += 1;
        match decode_utf8(&mut d.cur, &mut d.codep, ch) {
            UTF8_ACCEPT => d.push_codepoint(d.codep),
            UTF8_REJECT => {
                let prev_was_accept = d.prev == UTF8_ACCEPT;
                d.reset();
                d.push_codepoint(0xfffd);
                if !prev_was_accept {
                    pos -= 1;
                    continue; // so that prev is correct
                }
            }
            _ => {}
        }
        d.prev = d.cur;
    }
    pos
}

/// Scalar decode helper used by the SIMD implementations for finishing a
/// partial sequence at the start of an input. Returns the number of bytes
/// consumed.
pub fn decode_to_accept_scalar(d: &mut UTF8Decoder, src: &[u8]) -> usize {
    let mut pos = 0;
    while pos < src.len() && d.cur != UTF8_ACCEPT {
        let ch = src[pos];
        pos += 1;
        match decode_utf8(&mut d.cur, &mut d.codep, ch) {
            UTF8_ACCEPT => d.push_codepoint(d.codep),
            UTF8_REJECT => {
                let prev_was_accept = d.prev == UTF8_ACCEPT;
                d.reset();
                d.push_codepoint(0xfffd);
                if !prev_was_accept {
                    pos -= 1;
                    continue;
                }
            }
            _ => {}
        }
        d.prev = d.cur;
    }
    pos
}

/// Scalar reference `utf8_decode_to_esc`, matching `utf8_decode_to_esc_scalar`
/// in `simd-string.c`.
pub fn decode_to_esc_scalar(d: &mut UTF8Decoder, src: &[u8]) -> bool {
    d.begin_call();
    d.ensure_capacity(src.len());
    while d.num_consumed < src.len() {
        let ch = src[d.num_consumed];
        d.num_consumed += 1;
        if ch == 0x1b {
            if d.cur != UTF8_ACCEPT {
                d.push_codepoint(0xfffd);
            }
            d.reset();
            return true;
        } else {
            match decode_utf8(&mut d.cur, &mut d.codep, ch) {
                UTF8_ACCEPT => d.push_codepoint(d.codep),
                UTF8_REJECT => {
                    let prev_was_accept = d.prev == UTF8_ACCEPT;
                    d.reset();
                    d.push_codepoint(0xfffd);
                    if !prev_was_accept {
                        d.num_consumed -= 1;
                        continue; // so that prev is correct
                    }
                }
                _ => {}
            }
        }
        d.prev = d.cur;
    }
    false
}
