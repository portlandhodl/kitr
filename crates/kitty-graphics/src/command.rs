// SPDX-License-Identifier: GPL-3.0-or-later
//! Parser for kitty graphics protocol commands (`ESC _ G ... ESC \`).
//!
//! Ported from the generated `kitty/parse-graphics-command.h`,
//! `kitty/graphics.h` and `kitty/graphics.c`. The parser deliberately keeps the
//! C state machine's quirks:
//!
//! * an empty control block is allowed (`ESC_G;<base64>ESC\`),
//! * values are unsigned 32 bit except `z`, `H` and `V`, which are signed,
//! * unknown keys/flag values are errors with the same messages as kitty,
//! * exactly one `;` separates the control block from the payload,
//! * the payload is decoded with kitty's lenient base64 decoder.
//!
//! Parsing only validates the control block. Semantic checks (existence of
//! images, dimensions, formats, etc.) belong to the image pipeline or the
//! eventual Screen integration.
//!
//! Copyright (C) 2018-2026 Kovid Goyal <kovid at kovidgoyal.net>

use crate::base64;

/// Kitty's `GRAPHICS_USAGE_HINT_TRANSIENT`.
pub const GRAPHICS_USAGE_HINT_TRANSIENT: u32 = 1;

/// The `a=` action field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    /// `t`: transmit only.
    Transmit,
    /// `T`: transmit and display (the capital-T form from older clients).
    TransmitAndDisplay,
    /// `a`: load an animation frame.
    AnimationFrame,
    /// `c`: compose animation frame data.
    Compose,
    /// `d`: delete images/placements.
    Delete,
    /// `f`: load animation frame data (new spelling).
    FrameData,
    /// `p`: display a previously transmitted image.
    Put,
    /// `q`: query an image transmission.
    Query,
}

impl Action {
    pub fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            b't' => Self::Transmit,
            b'T' => Self::TransmitAndDisplay,
            b'a' => Self::AnimationFrame,
            b'c' => Self::Compose,
            b'd' => Self::Delete,
            b'f' => Self::FrameData,
            b'p' => Self::Put,
            b'q' => Self::Query,
            _ => return None,
        })
    }

    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Transmit => b't',
            Self::TransmitAndDisplay => b'T',
            Self::AnimationFrame => b'a',
            Self::Compose => b'c',
            Self::Delete => b'd',
            Self::FrameData => b'f',
            Self::Put => b'p',
            Self::Query => b'q',
        }
    }
}

/// The `d=` delete action field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum DeleteAction {
    All,
    AllButId,
    AtCursor,
    AtCursorPoint,
    Frames,
    AllFrames,
    ById,
    ByIdNoRefs,
    ByNumber,
    ByNumberNoRefs,
    PlacementId,
    AtPoint,
    AtCell,
    AtPoint3d,
    FramesRange,
    FramesRangeById,
    CellRange,
    CellRangeUpper,
    CellRangeXY,
    CellRangeXYUpper,
    ZIndex,
    ZIndexUpper,
}

impl DeleteAction {
    pub fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            b'a' => Self::All,
            b'A' => Self::AllButId,
            b'c' => Self::AtCursor,
            b'C' => Self::AtCursorPoint,
            b'f' => Self::Frames,
            b'F' => Self::AllFrames,
            b'i' => Self::ById,
            b'I' => Self::ByIdNoRefs,
            b'n' => Self::ByNumber,
            b'N' => Self::ByNumberNoRefs,
            b'p' => Self::PlacementId,
            b'P' => Self::AtPoint,
            b'q' => Self::AtCell,
            b'Q' => Self::AtPoint3d,
            b'r' => Self::FramesRange,
            b'R' => Self::FramesRangeById,
            b'x' => Self::CellRange,
            b'X' => Self::CellRangeUpper,
            b'y' => Self::CellRangeXY,
            b'Y' => Self::CellRangeXYUpper,
            b'z' => Self::ZIndex,
            b'Z' => Self::ZIndexUpper,
            _ => return None,
        })
    }

    pub const fn as_byte(self) -> u8 {
        match self {
            Self::All => b'a',
            Self::AllButId => b'A',
            Self::AtCursor => b'c',
            Self::AtCursorPoint => b'C',
            Self::Frames => b'f',
            Self::AllFrames => b'F',
            Self::ById => b'i',
            Self::ByIdNoRefs => b'I',
            Self::ByNumber => b'n',
            Self::ByNumberNoRefs => b'N',
            Self::PlacementId => b'p',
            Self::AtPoint => b'P',
            Self::AtCell => b'q',
            Self::AtPoint3d => b'Q',
            Self::FramesRange => b'r',
            Self::FramesRangeById => b'R',
            Self::CellRange => b'x',
            Self::CellRangeUpper => b'X',
            Self::CellRangeXY => b'y',
            Self::CellRangeXYUpper => b'Y',
            Self::ZIndex => b'z',
            Self::ZIndexUpper => b'Z',
        }
    }
}

/// The `t=` transmission medium field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransmissionType {
    /// `d`: data is in the command payload.
    Direct,
    /// `f`: payload is the name of a regular file.
    File,
    /// `s`: payload is a POSIX shared memory object name.
    SharedMemory,
    /// `t`: payload is the name of a temporary file.
    TempFile,
}

impl TransmissionType {
    pub fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            b'd' => Self::Direct,
            b'f' => Self::File,
            b's' => Self::SharedMemory,
            b't' => Self::TempFile,
            _ => return None,
        })
    }

    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Direct => b'd',
            Self::File => b'f',
            Self::SharedMemory => b's',
            Self::TempFile => b't',
        }
    }
}

/// The `o=` compression field. Kitty currently only defines zlib.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Compression {
    Zlib,
}

impl Compression {
    pub fn from_byte(b: u8) -> Option<Self> {
        (b == b'z').then_some(Self::Zlib)
    }

    pub const fn as_byte(self) -> u8 {
        b'z'
    }
}

/// A fully parsed graphics command.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GraphicsCommand {
    pub action: Option<Action>,
    pub delete_action: Option<DeleteAction>,
    pub transmission_type: Option<TransmissionType>,
    pub compressed: Option<Compression>,

    pub format: u32,
    pub more: u32,
    pub id: u32,
    pub image_number: u32,
    pub placement_id: u32,
    pub quiet: u32,

    pub width: u32,
    pub height: u32,
    pub x_offset: u32,
    pub y_offset: u32,
    pub data_width: u32,
    pub data_height: u32,
    pub data_sz: u32,
    pub data_offset: u32,
    pub num_cells: u32,
    pub num_lines: u32,
    pub cell_x_offset: u32,
    pub cell_y_offset: u32,
    pub cursor_movement: u32,

    pub unicode_placement: bool,
    pub parent_id: u32,
    pub parent_placement_id: u32,
    pub usage_hints: u32,

    pub z_index: i32,
    pub offset_from_parent_x: i32,
    pub offset_from_parent_y: i32,

    /// Decoded base64 payload (or the raw filename for file transmission).
    pub payload: Vec<u8>,
}

impl GraphicsCommand {
    /// True when this command asks for more chunks to follow.
    pub fn has_more_chunks(&self) -> bool {
        self.more != 0
    }

    /// Transient images are not written to the disk cache.
    pub fn is_transient(&self) -> bool {
        self.usage_hints & GRAPHICS_USAGE_HINT_TRANSIENT != 0
    }

    /// Frame number for `a=f`/`a=a`/`a=c` (`r=`).
    pub const fn frame_number(&self) -> u32 {
        self.num_lines
    }

    /// Other frame number (`c=`), used as the frame source for `a=f` and
    /// `a=c` and as the selected frame for `a=a`.
    pub const fn other_frame_number(&self) -> u32 {
        self.num_cells
    }

    /// Frame gap in milliseconds (`z=`), signed.
    pub const fn gap(&self) -> i32 {
        self.z_index
    }

    /// Background colour for a frame canvas (`Y=`), packed RGBA.
    pub const fn bgcolor(&self) -> u32 {
        self.cell_y_offset
    }

    /// Frame blend mode (`X=`): `1` replaces, anything else alpha blends.
    pub const fn blend_mode(&self) -> u32 {
        self.cell_x_offset
    }

    /// Animation run state (`s=`) for `a=a`: `1` stopped, `2` loading,
    /// `3` running.
    pub const fn animation_state(&self) -> u32 {
        self.data_width
    }

    /// Animation loop count (`v=`) for `a=a`.
    pub const fn loop_count(&self) -> u32 {
        self.data_height
    }

    /// Composition operation (`C=`) for `a=c`: `1` replaces, `0` alpha blends.
    pub const fn compose_mode(&self) -> u32 {
        self.cursor_movement
    }
}

/// Parser error. The `Display` strings match kitty's parser where practical.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    MissingEscapePrefix,
    MissingEscapeTerminator,
    InvalidKey(u8),
    NoEqualsFound(u8),
    MissingEquals,
    UnknownFlag { field: &'static str, value: u8 },
    MissingInteger(Option<u8>),
    NumberTooLarge,
    UnexpectedAfterValue(u8),
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingEscapePrefix => f.write_str("graphics command does not start with ESC_G"),
            Self::MissingEscapeTerminator => {
                f.write_str("graphics command does not end with ESC \\")
            }
            Self::InvalidKey(key) => write!(
                f,
                "Malformed GraphicsCommand control block, invalid key character: 0x{key:x}"
            ),
            Self::NoEqualsFound(found) => write!(
                f,
                "Malformed GraphicsCommand control block, no = after key, found: 0x{found:x} instead"
            ),
            Self::MissingEquals => {
                f.write_str("Malformed GraphicsCommand control block, no = after key")
            }
            Self::UnknownFlag { field, value } => write!(
                f,
                "Malformed GraphicsCommand control block, unknown flag value for {field}: 0x{value:x}"
            ),
            Self::MissingInteger(Some(key)) => write!(
                f,
                "Malformed GraphicsCommand control block, expecting an integer value for key: {}",
                *key as char
            ),
            Self::MissingInteger(None) => {
                f.write_str("Malformed GraphicsCommand control block, expecting an integer value")
            }
            Self::NumberTooLarge => {
                f.write_str("Malformed GraphicsCommand control block, number is too large")
            }
            Self::UnexpectedAfterValue(found) => write!(
                f,
                "Malformed GraphicsCommand control block, expecting a , or semi-colon after a value, found: 0x{found:x}"
            ),
        }
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Key,
    Equal,
    Uint,
    Int,
    Flag,
    AfterValue,
    Payload,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ValueState {
    Uint,
    Int,
    Flag,
}

fn value_state_for_key(key: u8) -> Option<ValueState> {
    Some(match key {
        b'a' | b'd' | b't' | b'o' => ValueState::Flag,
        b'f' | b'm' | b'i' | b'I' | b'p' | b'q' | b'w' | b'h' | b'x' | b'y' | b'v' | b's'
        | b'S' | b'O' | b'c' | b'r' | b'X' | b'Y' | b'C' | b'U' | b'P' | b'Q' | b'N' => {
            ValueState::Uint
        }
        b'z' | b'H' | b'V' => ValueState::Int,
        _ => return None,
    })
}

/// Parse an escape sequence beginning with `ESC _ G` and ending with `ESC \`.
pub fn parse_escape(input: &[u8]) -> Result<GraphicsCommand, ParseError> {
    const PREFIX: &[u8] = b"\x1b_G";
    const SUFFIX: &[u8] = b"\x1b\\";
    if !input.starts_with(PREFIX) {
        return Err(ParseError::MissingEscapePrefix);
    }
    if !input.ends_with(SUFFIX) || input.len() < PREFIX.len() + SUFFIX.len() {
        return Err(ParseError::MissingEscapeTerminator);
    }
    parse_raw(&input[PREFIX.len()..input.len() - SUFFIX.len()])
}

/// Parse the contents between `ESC _ G` and `ESC \`.
///
/// The input is `control[;payload]`, where `control` is a comma-separated list
/// of `key=value` pairs and `payload` is base64 data.
pub fn parse_raw(input: &[u8]) -> Result<GraphicsCommand, ParseError> {
    let mut g = GraphicsCommand::default();
    let mut pos = 0usize;
    let mut state = State::Key;
    let mut value_state = ValueState::Flag;
    let mut key = b'a';

    if input.first() == Some(&b';') {
        state = State::AfterValue;
    }

    while pos < input.len() {
        match state {
            State::Key => {
                key = input[pos];
                pos += 1;
                value_state = value_state_for_key(key).ok_or(ParseError::InvalidKey(key))?;
                state = State::Equal;
            }
            State::Equal => {
                if pos >= input.len() {
                    // The loop will end and the final state check reports this.
                    break;
                }
                let found = input[pos];
                pos += 1;
                if found != b'=' {
                    return Err(ParseError::NoEqualsFound(found));
                }
                state = match value_state {
                    ValueState::Uint => State::Uint,
                    ValueState::Int => State::Int,
                    ValueState::Flag => State::Flag,
                };
            }
            State::Flag => {
                // The C parser relies on the NUL terminator here. Treat a
                // missing value as NUL so the diagnostic is the same.
                let value = input.get(pos).copied().unwrap_or(0);
                if pos < input.len() {
                    pos += 1;
                }
                match key {
                    b'a' => {
                        g.action =
                            Some(Action::from_byte(value).ok_or(ParseError::UnknownFlag {
                                field: "action",
                                value,
                            })?)
                    }
                    b'd' => {
                        g.delete_action = Some(DeleteAction::from_byte(value).ok_or(
                            ParseError::UnknownFlag {
                                field: "delete_action",
                                value,
                            },
                        )?)
                    }
                    b't' => {
                        g.transmission_type = Some(TransmissionType::from_byte(value).ok_or(
                            ParseError::UnknownFlag {
                                field: "transmission_type",
                                value,
                            },
                        )?)
                    }
                    b'o' => {
                        g.compressed = Some(Compression::from_byte(value).ok_or(
                            ParseError::UnknownFlag {
                                field: "compressed",
                                value,
                            },
                        )?)
                    }
                    _ => unreachable!("validated key has flag value state"),
                }
                state = State::AfterValue;
            }
            State::Uint | State::Int => {
                let is_int = state == State::Int;
                let negative = if is_int && input.get(pos) == Some(&b'-') {
                    pos += 1;
                    true
                } else {
                    false
                };
                let start = pos;
                let mut code: u64 = 0;
                let end = input.len().min(pos.saturating_add(10));
                while pos < end {
                    let digit = input[pos].wrapping_sub(b'0');
                    if digit > 9 {
                        break;
                    }
                    code = code * 10 + u64::from(digit);
                    pos += 1;
                }
                if pos == start {
                    return Err(ParseError::MissingInteger(Some(key)));
                }
                if code > u64::from(u32::MAX) {
                    return Err(ParseError::NumberTooLarge);
                }
                let code = code as u32;
                if is_int {
                    let value = if negative {
                        (code as i32).wrapping_neg()
                    } else {
                        code as i32
                    };
                    match key {
                        b'z' => g.z_index = value,
                        b'H' => g.offset_from_parent_x = value,
                        b'V' => g.offset_from_parent_y = value,
                        _ => unreachable!("validated key has int value state"),
                    }
                } else {
                    match key {
                        b'f' => g.format = code,
                        b'm' => g.more = code,
                        b'i' => g.id = code,
                        b'I' => g.image_number = code,
                        b'p' => g.placement_id = code,
                        b'q' => g.quiet = code,
                        b'w' => g.width = code,
                        b'h' => g.height = code,
                        b'x' => g.x_offset = code,
                        b'y' => g.y_offset = code,
                        b'v' => g.data_height = code,
                        b's' => g.data_width = code,
                        b'S' => g.data_sz = code,
                        b'O' => g.data_offset = code,
                        b'c' => g.num_cells = code,
                        b'r' => g.num_lines = code,
                        b'X' => g.cell_x_offset = code,
                        b'Y' => g.cell_y_offset = code,
                        b'C' => g.cursor_movement = code,
                        // In C this field is a bool, so any non-zero value is
                        // true. Preserve that observable behaviour.
                        b'U' => g.unicode_placement = code != 0,
                        b'P' => g.parent_id = code,
                        b'Q' => g.parent_placement_id = code,
                        b'N' => g.usage_hints = code,
                        _ => unreachable!("validated key has uint value state"),
                    }
                }
                state = State::AfterValue;
            }
            State::AfterValue => {
                let found = input.get(pos).copied().unwrap_or(0);
                if pos < input.len() {
                    pos += 1;
                }
                match found {
                    b',' => state = State::Key,
                    b';' => state = State::Payload,
                    _ => return Err(ParseError::UnexpectedAfterValue(found)),
                }
            }
            State::Payload => {
                g.payload = base64::decode_lenient(&input[pos..]);
                pos = input.len();
            }
        }
    }

    match state {
        State::Equal => Err(ParseError::MissingEquals),
        State::Uint | State::Int => Err(ParseError::MissingInteger(None)),
        _ => Ok(g),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_u32_limit_and_signed_values() {
        let g = parse_escape(b"\x1b_Gi=4294967295\x1b\\").unwrap();
        assert_eq!(g.id, u32::MAX);

        let g = parse_escape(b"\x1b_Gi=3,p=4\x1b\\").unwrap();
        assert_eq!(g.id, 3);
        assert_eq!(g.placement_id, 4);

        let g = parse_escape(b"\x1b_Ga=t,t=d,s=100,z=-9\x1b\\").unwrap();
        assert_eq!(g.action, Some(Action::Transmit));
        assert_eq!(g.transmission_type, Some(TransmissionType::Direct));
        assert_eq!(g.data_width, 100);
        assert_eq!(g.z_index, -9);

        let g = parse_escape(b"\x1b_Gz=2147483648\x1b\\").unwrap();
        assert_eq!(g.z_index, i32::MIN);
    }

    #[test]
    fn parses_every_documented_key() {
        let raw = b"a=T,d=a,t=f,o=z,f=100,m=1,i=1,I=2,p=3,q=4,w=5,h=6,x=7,y=8,v=9,s=10,S=11,O=12,c=13,r=14,X=15,Y=16,z=-17,C=18,U=1,P=19,Q=20,N=1,H=-21,V=-22;YWJj";
        let g = parse_raw(raw).unwrap();
        assert_eq!(g.action, Some(Action::TransmitAndDisplay));
        assert_eq!(g.delete_action, Some(DeleteAction::All));
        assert_eq!(g.transmission_type, Some(TransmissionType::File));
        assert_eq!(g.compressed, Some(Compression::Zlib));
        assert_eq!(
            (
                g.format,
                g.more,
                g.id,
                g.image_number,
                g.placement_id,
                g.quiet
            ),
            (100, 1, 1, 2, 3, 4)
        );
        assert_eq!((g.width, g.height, g.x_offset, g.y_offset), (5, 6, 7, 8));
        assert_eq!(
            (g.data_height, g.data_width, g.data_sz, g.data_offset),
            (9, 10, 11, 12)
        );
        assert_eq!(
            (g.num_cells, g.num_lines, g.cell_x_offset, g.cell_y_offset),
            (13, 14, 15, 16)
        );
        assert_eq!(g.z_index, -17);
        assert_eq!(g.cursor_movement, 18);
        assert!(g.unicode_placement);
        assert_eq!((g.parent_id, g.parent_placement_id), (19, 20));
        assert_eq!(g.usage_hints, 1);
        assert_eq!((g.offset_from_parent_x, g.offset_from_parent_y), (-21, -22));
        assert_eq!(g.payload, b"abc");
    }

    #[test]
    fn parses_empty_control_and_empty_payload() {
        let g = parse_escape(b"\x1b_G\x1b\\").unwrap();
        assert_eq!(g, GraphicsCommand::default());
        let g = parse_escape(b"\x1b_G;\x1b\\").unwrap();
        assert_eq!(g.action, None);
        assert!(g.payload.is_empty());
        let g = parse_escape(b"\x1b_Ga=q,i=1;YWJj\x1b\\").unwrap();
        assert_eq!(g.action, Some(Action::Query));
        assert_eq!(g.payload, b"abc");
    }

    #[test]
    fn rejects_oversized_integers() {
        assert_eq!(
            parse_escape(b"\x1b_Gi=4294967296\x1b\\"),
            Err(ParseError::NumberTooLarge)
        );
        assert_eq!(parse_raw(b"i=99999999999"), Err(ParseError::NumberTooLarge));
    }

    #[test]
    fn rejects_invalid_keys_and_missing_equals() {
        assert_eq!(
            parse_escape(b"\x1b_G,s=1\x1b\\"),
            Err(ParseError::InvalidKey(b','))
        );
        assert_eq!(
            parse_escape(b"\x1b_GW=1\x1b\\"),
            Err(ParseError::InvalidKey(b'W'))
        );
        assert_eq!(
            parse_escape(b"\x1b_G1=1\x1b\\"),
            Err(ParseError::InvalidKey(b'1'))
        );
        assert_eq!(
            parse_escape(b"\x1b_Ga=t,,w=2\x1b\\"),
            Err(ParseError::InvalidKey(b','))
        );
        assert_eq!(
            parse_escape(b"\x1b_Gs\x1b\\"),
            Err(ParseError::MissingEquals)
        );
        assert_eq!(parse_raw(b"sx"), Err(ParseError::NoEqualsFound(b'x')));
    }

    #[test]
    fn rejects_missing_integer_values() {
        assert_eq!(
            parse_escape(b"\x1b_Gs=\x1b\\"),
            Err(ParseError::MissingInteger(None))
        );
        assert_eq!(
            parse_escape(b"\x1b_Gs==\x1b\\"),
            Err(ParseError::MissingInteger(Some(b's')))
        );
        assert_eq!(
            parse_escape(b"\x1b_Gz=-\x1b\\"),
            Err(ParseError::MissingInteger(Some(b'z')))
        );
    }

    #[test]
    fn rejects_bad_value_separators() {
        assert_eq!(
            parse_escape(b"\x1b_Gs=1=\x1b\\"),
            Err(ParseError::UnexpectedAfterValue(b'='))
        );
        // A value at the end of the control block is accepted, as in kitty.
        let g = parse_escape(b"\x1b_Gs=1\x1b\\").unwrap();
        assert_eq!(g.data_width, 1);
    }

    #[test]
    fn rejects_unknown_flags() {
        assert_eq!(
            parse_escape(b"\x1b_Ga=x\x1b\\"),
            Err(ParseError::UnknownFlag {
                field: "action",
                value: b'x'
            })
        );
        assert_eq!(
            parse_escape(b"\x1b_Gd=W\x1b\\"),
            Err(ParseError::UnknownFlag {
                field: "delete_action",
                value: b'W'
            })
        );
        assert_eq!(
            parse_escape(b"\x1b_Gt=x\x1b\\"),
            Err(ParseError::UnknownFlag {
                field: "transmission_type",
                value: b'x'
            })
        );
        assert_eq!(
            parse_escape(b"\x1b_Go=x\x1b\\"),
            Err(ParseError::UnknownFlag {
                field: "compressed",
                value: b'x'
            })
        );
    }

    #[test]
    fn parses_ascii_payload_leniently() {
        let g = parse_escape(b"\x1b_Ga=t,f=24,s=1,v=1;eA\x1b\\").unwrap();
        assert_eq!(g.payload, b"x");
        let g = parse_escape(b"\x1b_Ga=t,f=24;eA!!\x1b\\").unwrap();
        assert_eq!(g.payload, b"x");
    }

    #[test]
    fn requires_apc_framing() {
        assert_eq!(parse_escape(b"a=t"), Err(ParseError::MissingEscapePrefix));
        assert_eq!(
            parse_escape(b"\x1b_Ga=t"),
            Err(ParseError::MissingEscapeTerminator)
        );
    }

    #[test]
    fn all_delete_actions_parse() {
        for b in b"ACFINPQRXYZacfinpqrxyz" {
            let raw = [b"d=".as_slice(), &[*b]].concat();
            let g = parse_raw(&raw).unwrap();
            assert_eq!(g.delete_action.unwrap().as_byte(), *b);
        }
    }

    #[test]
    fn all_actions_parse() {
        for b in b"Tacdfpqt" {
            let raw = [b"a=".as_slice(), &[*b]].concat();
            let g = parse_raw(&raw).unwrap();
            assert_eq!(g.action.unwrap().as_byte(), *b);
        }
    }
}
