// SPDX-License-Identifier: GPL-3.0-or-later
//! Remote-control message framing.
//!
//! Kitty wraps every remote-control command and response in a DCS:
//!
//! ```text
//! ESC P @kitty-cmd <UTF-8 JSON> ESC \
//! ```
//!
//! See `encode_send()` in `kitty/remote_control.py`, `peer_message_received()`
//! in `kitty/boss.py`, and `has_complete_peer_command()` in
//! `kitty/child-monitor.c`. This module deliberately stops at the payload
//! boundary: it does not parse or produce JSON and it does not touch sockets.
//! [`Framer`] is an incremental byte-oriented parser so it can be driven by
//! any transport.
//!
//! Kitty itself has no explicit message-size limit; the C peer buffer grows on
//! demand. [`Framer`] therefore makes the limit configurable and defaults to
//! 16 MiB. Set it to [`usize::MAX`] to disable it.

use crate::error::{Error, Result};

/// DCS introducer plus the kitty remote-control command name.
pub const DCS_PREFIX: &[u8] = b"\x1bP@kitty-cmd";
/// String terminator used by kitty (`ESC \`).
pub const DCS_TERMINATOR: &[u8] = b"\x1b\\";
/// Default maximum payload size accepted by [`Framer`].
pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Wrap a payload in kitty's DCS frame.
pub fn encode_message(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(DCS_PREFIX.len() + payload.len() + DCS_TERMINATOR.len());
    out.extend_from_slice(DCS_PREFIX);
    out.extend_from_slice(payload);
    out.extend_from_slice(DCS_TERMINATOR);
    out
}

/// Alias used by response senders; the response framing is byte-identical.
pub fn encode_response(payload: &[u8]) -> Vec<u8> {
    encode_message(payload)
}

/// Incremental DCS frame parser.
///
/// Feed arbitrary byte slices with [`Framer::push`]. Returned vectors are the
/// payloads between `@kitty-cmd` and `ESC \`. Partial frames are retained until
/// more data arrives. Noise before a frame is discarded.
#[derive(Debug, Clone)]
pub struct Framer {
    buf: Vec<u8>,
    max_message_size: usize,
}

impl Default for Framer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framer {
    /// Create a framer with the default 16 MiB payload limit.
    pub fn new() -> Self {
        Self::with_max_message_size(DEFAULT_MAX_MESSAGE_SIZE)
    }

    /// Create a framer with an explicit payload limit.
    pub fn with_max_message_size(max_message_size: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_message_size,
        }
    }

    /// Number of bytes currently buffered (incomplete frame or noise).
    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }

    /// The configured maximum payload size.
    pub fn max_message_size(&self) -> usize {
        self.max_message_size
    }

    /// Drop all buffered bytes.
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// Feed bytes and return every complete payload now available.
    pub fn push(&mut self, data: &[u8]) -> Result<Vec<Vec<u8>>> {
        self.buf.extend_from_slice(data);
        let mut messages = Vec::new();
        loop {
            if !self.buf.starts_with(DCS_PREFIX) {
                match find_subslice(&self.buf, DCS_PREFIX) {
                    Some(pos) => {
                        self.buf.drain(..pos);
                    }
                    None => {
                        // Keep a possible partial prefix, discard anything that
                        // cannot be the start of one.
                        let keep = DCS_PREFIX.len().saturating_sub(1);
                        if self.buf.len() > keep {
                            let drain = self.buf.len() - keep;
                            self.buf.drain(..drain);
                        }
                        break;
                    }
                }
            }
            let search_from = DCS_PREFIX.len();
            match find_subslice(&self.buf[search_from..], DCS_TERMINATOR) {
                Some(rel) => {
                    let end = search_from + rel;
                    let payload_len = end - search_from;
                    if payload_len > self.max_message_size {
                        self.buf.clear();
                        return Err(Error::MessageTooLarge {
                            size: payload_len,
                            max: self.max_message_size,
                        });
                    }
                    let payload = self.buf[search_from..end].to_vec();
                    self.buf.drain(..end + DCS_TERMINATOR.len());
                    messages.push(payload);
                }
                None => {
                    let pending = self.buf.len() - search_from;
                    if pending > self.max_message_size {
                        self.buf.clear();
                        return Err(Error::MessageTooLarge {
                            size: pending,
                            max: self.max_message_size,
                        });
                    }
                    break;
                }
            }
        }
        Ok(messages)
    }

    /// Assert that no partial frame remains. Consumes the framer.
    pub fn finish(self) -> Result<()> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(Error::IncompleteFrame)
        }
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_and_parse_one_message() {
        let payload = br#"{"cmd":"ls","version":"0.1"}"#;
        let framed = encode_message(payload);
        assert!(framed.starts_with(b"\x1bP@kitty-cmd"));
        assert!(framed.ends_with(b"\x1b\\"));
        let mut framer = Framer::new();
        let messages = framer.push(&framed).unwrap();
        assert_eq!(messages, vec![payload.to_vec()]);
        assert_eq!(framer.buffered_len(), 0);
        framer.finish().unwrap();
    }

    #[test]
    fn parse_split_at_every_byte() {
        let payload = br#"{"cmd":"get_text","payload":{"extent":"all"}}"#;
        let framed = encode_message(payload);
        for split in 0..=framed.len() {
            let mut framer = Framer::new();
            let mut messages = framer.push(&framed[..split]).unwrap();
            messages.extend(framer.push(&framed[split..]).unwrap());
            assert_eq!(messages, vec![payload.to_vec()], "split at {split}");
            framer.finish().unwrap();
        }
    }

    #[test]
    fn parse_when_fed_one_byte_at_a_time() {
        let framed = encode_message(b"hello");
        let mut framer = Framer::new();
        let mut messages = Vec::new();
        for byte in framed {
            messages.extend(framer.push(&[byte]).unwrap());
        }
        assert_eq!(messages, vec![b"hello".to_vec()]);
        framer.finish().unwrap();
    }

    #[test]
    fn parse_multiple_and_coalesced_messages() {
        let mut wire = encode_message(b"one");
        wire.extend_from_slice(&encode_message(b"two"));
        wire.extend_from_slice(&encode_message(b""));
        wire.extend_from_slice(&encode_message(b"three"));
        let mut framer = Framer::new();
        let messages = framer.push(&wire).unwrap();
        assert_eq!(
            messages,
            vec![
                b"one".to_vec(),
                b"two".to_vec(),
                Vec::new(),
                b"three".to_vec()
            ]
        );
        assert_eq!(framer.buffered_len(), 0);
    }

    #[test]
    fn discards_noise_before_prefix() {
        let mut wire = b"garbage\x1bnot-a-frame".to_vec();
        wire.extend_from_slice(&encode_message(b"payload"));
        let mut framer = Framer::new();
        let messages = framer.push(&wire).unwrap();
        assert_eq!(messages, vec![b"payload".to_vec()]);
        assert_eq!(framer.buffered_len(), 0);
    }

    #[test]
    fn incomplete_frame_is_retained() {
        let framed = encode_message(b"partial payload");
        let mut framer = Framer::new();
        assert!(framer.push(&framed[..10]).unwrap().is_empty());
        assert!(framer.buffered_len() > 0);
        assert!(matches!(
            framer.clone().finish(),
            Err(Error::IncompleteFrame)
        ));
        let messages = framer.push(&framed[10..]).unwrap();
        assert_eq!(messages, vec![b"partial payload".to_vec()]);
    }

    #[test]
    fn message_size_limit_is_enforced() {
        let mut framer = Framer::with_max_message_size(4);
        let framed = encode_message(b"12345");
        assert!(matches!(
            framer.push(&framed),
            Err(Error::MessageTooLarge { size: 5, max: 4 })
        ));
        assert_eq!(framer.buffered_len(), 0);

        let mut framer = Framer::with_max_message_size(5);
        assert_eq!(framer.push(&framed).unwrap(), vec![b"12345".to_vec()]);
    }

    #[test]
    fn empty_input_is_a_noop() {
        let mut framer = Framer::new();
        assert!(framer.push(b"").unwrap().is_empty());
        assert_eq!(framer.buffered_len(), 0);
        framer.finish().unwrap();
    }

    #[test]
    fn response_framing_matches_command_framing() {
        assert_eq!(encode_response(b"ok"), encode_message(b"ok"));
        assert_eq!(encode_response(b"ok"), b"\x1bP@kitty-cmdok\x1b\\");
    }
}
