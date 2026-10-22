// SPDX-License-Identifier: GPL-3.0-or-later
//! The VT escape code parser state machine, ported from `kitty/vt-parser.c`.

use kitty_core::CharType;

use crate::charsets::UTF8_DFA;
use crate::params::{CsiParams, MAX_CSI_PARAMS};

/// Size of the C parser's input buffer (`BUF_SZ`).
pub const BUFFER_SIZE: usize = 1024 * 1024;
/// Maximum length of an escape code (`MAX_ESCAPE_CODE_LENGTH`).
pub const MAX_ESCAPE_CODE_LENGTH: usize = BUFFER_SIZE / 4;

const MAX_CSI_DIGITS: u8 = 16;

const BEL: u8 = 0x07;
const BS: u8 = 0x08;
const HT: u8 = 0x09;
const LF: u8 = 0x0a;
const VT: u8 = 0x0b;
const FF: u8 = 0x0c;
const CR: u8 = 0x0d;
const SO: u8 = 0x0e;
const SI: u8 = 0x0f;
const ESC: u8 = 0x1b;

const ESC_DCS: u8 = b'P';
const ESC_OSC: u8 = b']';
const ESC_CSI: u8 = b'[';
const ESC_ST: u8 = b'\\';
const ESC_PM: u8 = b'^';
const ESC_APC: u8 = b'_';
const ESC_SOS: u8 = b'X';

const UTF8_ACCEPT: u32 = 0;
const UTF8_REJECT: u32 = 1;

/// The VTE parser states used by kitty (`VTEState` in the C source).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VteState {
    Normal,
    Esc,
    Csi,
    Osc,
    Dcs,
    Apc,
    Pm,
    Sos,
}

impl VteState {
    /// The name used in the C source and in error messages.
    pub fn name(self) -> &'static str {
        match self {
            VteState::Normal => "VTE_NORMAL",
            VteState::Esc => "VTE_ESC",
            VteState::Csi => "VTE_CSI",
            VteState::Osc => "VTE_OSC",
            VteState::Dcs => "VTE_DCS",
            VteState::Apc => "VTE_APC",
            VteState::Pm => "VTE_PM",
            VteState::Sos => "VTE_SOS",
        }
    }
}

/// Dispatch sink for the parser.
///
/// All methods have empty default bodies. The control hooks
/// ([`bell`](Self::bell), [`tab`](Self::tab), ...) mirror the individual
/// `REPORT_COMMAND` calls emitted by the C loader's `REPORT_DRAW` macro. The
/// `draw` hook receives only raw printable codepoints (`>= ' '`), exactly as
/// `screen_draw_text` receives them before `map_char()` applies a charset.
#[allow(unused_variables)]
pub trait Handler {
    /// Draw raw printable text. Charset translation is the handler's job,
    /// matching `map_char()` in `screen.c`.
    fn draw(&mut self, text: &[CharType]) {}

    /// C0 BEL (0x07).
    fn bell(&mut self) {}
    /// C0 BS (0x08).
    fn backspace(&mut self) {}
    /// C0 HT (0x09).
    fn tab(&mut self) {}
    /// C0 LF, VT or FF.
    fn linefeed(&mut self) {}
    /// C0 CR (0x0d).
    fn carriage_return(&mut self) {}
    /// C0 SI (select G0) or SO (select G1).
    fn change_charset(&mut self, which: u8) {}
    /// ESC `(` (G0) or `)` (G1) charset designation.
    ///
    /// `which` is 0 for G0 and 1 for G1. `ch` is the designation byte
    /// (`A`, `B`, `0`, `U` or `V`). Translating drawn text is the screen's
    /// job, exactly as `screen_designate_charset()` plus `map_char()` do in C.
    fn designate_charset(&mut self, which: u8, ch: u8) {}

    /// An ESC sequence with no DCS/OSC/CSI/APC/PM/SOS introducer.
    ///
    /// `intermediates` contains the intermediates collected so far. The parser
    /// supports the same single intermediate byte as the C code, for example
    /// `(` or `)` for charset designation and `#` for DECALN.
    fn esc_dispatch(&mut self, intermediates: &[u8], action: u8) {}

    /// A complete, valid CSI sequence.
    ///
    /// `action` is the final byte. `intermediates` contains the secondary
    /// intermediate byte when present (`params.secondary()` mirrors it). The
    /// start modifier (`?`, `>`, `<`, `=`) is available as
    /// [`CsiParams::primary`].
    fn csi_dispatch(&mut self, params: &CsiParams, intermediates: &[u8], action: u8) {}

    /// A complete OSC string. `data` excludes the OSC introducer and the
    /// terminator (BEL or `ESC \`). It includes the numeric OSC code and its
    /// semicolon, for example `b"2;title"`.
    fn osc_dispatch(&mut self, data: &[u8]) {}

    /// A partial OSC 52 string emitted when the escape code length limit is
    /// hit (`is_extended_osc` in the C code). The default forwards to
    /// [`osc_dispatch`](Self::osc_dispatch).
    fn osc_dispatch_extended(&mut self, data: &[u8]) {
        self.osc_dispatch(data);
    }

    /// A complete DCS string.
    ///
    /// `data` is every byte after the `ESC P` introducer up to, but not
    /// including, the terminator. This mirrors `dispatch_dcs()` in the C
    /// source, which receives the whole accumulated payload at once. The
    /// callback is skipped when fewer than two bytes were accumulated.
    fn dcs_dispatch(&mut self, data: &[u8]) {}

    /// A complete PM string.
    fn pm_dispatch(&mut self, data: &[u8]) {}
    /// A complete SOS string.
    fn sos_dispatch(&mut self, data: &[u8]) {}
    /// A complete APC string.
    fn apc_dispatch(&mut self, data: &[u8]) {}

    /// Report a parser-level error, matching `REPORT_ERROR` in the C source.
    fn report_error(&mut self, message: &str) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CsiState {
    Start,
    Body,
    PostSecondary,
}

struct ParsedCsi {
    params: CsiParams,
    state: CsiState,
    num_digits: u8,
    accumulator: u64,
    mult: i32,
    is_valid: bool,
    length: usize,
    next_is_subparam: bool,
}

impl ParsedCsi {
    fn new() -> Self {
        Self {
            params: CsiParams::new(),
            state: CsiState::Start,
            num_digits: 0,
            accumulator: 0,
            mult: 1,
            is_valid: false,
            length: 0,
            next_is_subparam: false,
        }
    }

    fn reset(&mut self) {
        self.params.reset();
        self.state = CsiState::Start;
        self.num_digits = 0;
        self.accumulator = 0;
        self.mult = 1;
        self.is_valid = false;
        self.length = 0;
        self.next_is_subparam = false;
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Utf8State {
    cur: u32,
    prev: u32,
    codep: u32,
}

impl Utf8State {
    #[inline]
    fn decode(&mut self, byte: u8) -> u32 {
        let typ = UTF8_DFA[byte as usize] as u32;
        self.codep = if self.cur != UTF8_ACCEPT {
            (byte as u32 & 0x3f) | (self.codep << 6)
        } else {
            (0xffu32 >> typ) & byte as u32
        };
        self.cur = UTF8_DFA[256 + self.cur as usize * 16 + typ as usize] as u32;
        self.cur
    }
}

/// The incremental VT escape code parser.
pub struct Parser {
    state: VteState,
    utf8: Utf8State,
    csi: ParsedCsi,
    text: Vec<CharType>,
    string: Vec<u8>,
    string_pending_esc: bool,
    esc_intermediate: Option<u8>,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    /// Create a parser in the normal state.
    pub fn new() -> Self {
        Self {
            state: VteState::Normal,
            utf8: Utf8State::default(),
            csi: ParsedCsi::new(),
            text: Vec::new(),
            string: Vec::new(),
            string_pending_esc: false,
            esc_intermediate: None,
        }
    }

    /// The current parser state.
    pub fn state(&self) -> VteState {
        self.state
    }

    /// Reset the state machine and incremental UTF-8 decoder.
    ///
    /// This mirrors `reset_vt_parser()`. Charset state lives in the handler,
    /// not the parser.
    pub fn reset(&mut self) {
        self.state = VteState::Normal;
        self.utf8 = Utf8State::default();
        self.csi.reset();
        self.text.clear();
        self.string.clear();
        self.string_pending_esc = false;
        self.esc_intermediate = None;
    }

    /// Feed bytes to the parser.
    ///
    /// The input may be split at any byte boundary; parser state (including
    /// partial UTF-8, escape and string sequences) is carried across calls.
    pub fn parse<H: Handler + ?Sized>(&mut self, input: &[u8], handler: &mut H) {
        let mut pos = 0;
        while pos < input.len() {
            match self.state {
                VteState::Normal => self.consume_normal(input, &mut pos, handler),
                VteState::Esc => self.consume_esc(input, &mut pos, handler),
                VteState::Csi => {
                    if self.consume_csi(input, &mut pos, handler) {
                        self.state = VteState::Normal;
                        if self.csi.is_valid {
                            self.flush_text(handler);
                            let secondary = self.csi.params.secondary();
                            let intermediates: &[u8] = if secondary != 0 {
                                std::slice::from_ref(&secondary)
                            } else {
                                &[]
                            };
                            let action = self.csi.params.trailer();
                            handler.csi_dispatch(&self.csi.params, intermediates, action);
                        }
                    }
                }
                VteState::Osc | VteState::Dcs | VteState::Apc | VteState::Pm | VteState::Sos => {
                    self.consume_string(input, &mut pos, handler)
                }
            }
        }
        self.flush_text(handler);
    }

    fn consume_normal<H: Handler + ?Sized>(
        &mut self,
        input: &[u8],
        pos: &mut usize,
        handler: &mut H,
    ) {
        while *pos < input.len() {
            if self.utf8.cur == UTF8_ACCEPT {
                let start = *pos;
                let mut end = start;
                while end < input.len() && (b' '..=b'~').contains(&input[end]) {
                    end += 1;
                }
                if end > start {
                    self.text
                        .extend(input[start..end].iter().map(|&byte| u32::from(byte)));
                    *pos = end;
                    continue;
                }
            }
            let ch = input[*pos];
            *pos += 1;
            if ch == ESC {
                if self.utf8.cur != UTF8_ACCEPT {
                    self.emit_char(0xfffd, handler);
                }
                self.utf8 = Utf8State::default();
                self.flush_text(handler);
                self.state = VteState::Esc;
                return;
            }
            self.decode_byte(ch, handler);
        }
    }

    fn decode_byte<H: Handler + ?Sized>(&mut self, ch: u8, handler: &mut H) {
        loop {
            match self.utf8.decode(ch) {
                UTF8_ACCEPT => {
                    let codep = self.utf8.codep;
                    self.emit_char(codep, handler);
                }
                UTF8_REJECT => {
                    let prev_was_accept = self.utf8.prev == UTF8_ACCEPT;
                    self.utf8 = Utf8State::default();
                    self.emit_char(0xfffd, handler);
                    if !prev_was_accept {
                        continue;
                    }
                }
                _ => {}
            }
            self.utf8.prev = self.utf8.cur;
            break;
        }
    }

    fn emit_char<H: Handler + ?Sized>(&mut self, ch: u32, handler: &mut H) {
        match ch {
            x if x == BEL as u32 => {
                self.flush_text(handler);
                handler.bell();
            }
            x if x == BS as u32 => {
                self.flush_text(handler);
                handler.backspace();
            }
            x if x == HT as u32 => {
                self.flush_text(handler);
                handler.tab();
            }
            x if x == LF as u32 || x == VT as u32 || x == FF as u32 => {
                self.flush_text(handler);
                handler.linefeed();
            }
            x if x == CR as u32 => {
                self.flush_text(handler);
                handler.carriage_return();
            }
            x if x == SI as u32 => {
                self.flush_text(handler);
                handler.change_charset(0);
            }
            x if x == SO as u32 => {
                self.flush_text(handler);
                handler.change_charset(1);
            }
            x if x >= ' ' as u32 => self.text.push(x),
            _ => {}
        }
    }

    fn flush_text<H: Handler + ?Sized>(&mut self, handler: &mut H) {
        if !self.text.is_empty() {
            handler.draw(&self.text);
            self.text.clear();
        }
    }

    fn emit_error<H: Handler + ?Sized>(&mut self, handler: &mut H, message: &str) {
        self.flush_text(handler);
        handler.report_error(message);
    }

    fn consume_esc<H: Handler + ?Sized>(&mut self, input: &[u8], pos: &mut usize, handler: &mut H) {
        let ch = input[*pos];
        *pos += 1;
        if let Some(prev) = self.esc_intermediate.take() {
            self.handle_esc_pair(prev, ch, handler);
            self.state = VteState::Normal;
            return;
        }

        match ch {
            ESC_DCS => {
                self.state = VteState::Dcs;
                self.begin_string();
            }
            ESC_OSC => {
                self.state = VteState::Osc;
                self.begin_string();
            }
            ESC_CSI => {
                self.state = VteState::Csi;
                self.csi.reset();
            }
            ESC_APC => {
                self.state = VteState::Apc;
                self.begin_string();
            }
            ESC_PM => {
                self.state = VteState::Pm;
                self.begin_string();
            }
            ESC_SOS => {
                self.state = VteState::Sos;
                self.begin_string();
            }
            b'%' | b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/' | b' ' | b'#' => {
                self.esc_intermediate = Some(ch);
            }
            b'c' | b'D' | b'E' | b'H' | b'M' | b'7' | b'8' | b'>' | b'=' => {
                self.flush_text(handler);
                handler.esc_dispatch(&[], ch);
                self.state = VteState::Normal;
            }
            _ => {
                let msg = format!("Unknown char after ESC: 0x{ch:x}");
                self.emit_error(handler, &msg);
                self.state = VteState::Normal;
            }
        }
    }

    fn handle_esc_pair<H: Handler + ?Sized>(&mut self, prev: u8, ch: u8, handler: &mut H) {
        match prev {
            b'%' => {
                let msg = match ch {
                    b'@' => "Ignoring attempt to switch to non-utf8 encoding".to_string(),
                    b'G' => "Ignoring attempt to switch to utf8 encoding as we are always utf-8"
                        .to_string(),
                    _ => format!("Unhandled Esc % code: 0x{ch:x}"),
                };
                self.emit_error(handler, &msg);
            }
            b'#' => {
                if ch == b'8' {
                    self.flush_text(handler);
                    handler.esc_dispatch(b"#", ch);
                } else {
                    let msg = format!("Unhandled Esc # code: 0x{ch:x}");
                    self.emit_error(handler, &msg);
                }
            }
            b'(' | b')' => {
                if matches!(ch, b'A' | b'B' | b'0' | b'U' | b'V') {
                    self.flush_text(handler);
                    handler.designate_charset(prev - b'(', ch);
                } else {
                    let msg = format!("Unknown charset: 0x{ch:x}");
                    self.emit_error(handler, &msg);
                }
            }
            b' ' => {
                let msg = match ch {
                    b'F' | b'G' => {
                        "Ignoring attempt to turn on/off C1 controls as we only support C0 controls"
                            .to_string()
                    }
                    _ => format!("Unhandled ESC SP escape code: 0x{ch:x}"),
                };
                self.emit_error(handler, &msg);
            }
            _ => {
                let msg = format!("Unhandled charset related escape code: 0x{prev:x} 0x{ch:x}");
                self.emit_error(handler, &msg);
            }
        }
    }

    fn begin_string(&mut self) {
        self.string.clear();
        self.string_pending_esc = false;
    }

    fn consume_csi<H: Handler + ?Sized>(
        &mut self,
        input: &[u8],
        pos: &mut usize,
        handler: &mut H,
    ) -> bool {
        while *pos < input.len() {
            let ch = input[*pos];
            *pos += 1;
            self.csi.length += 1;
            match self.csi.state {
                CsiState::Start => match ch {
                    BEL | BS | HT | LF | VT | FF | CR | SO | SI => {
                        self.emit_char(ch as u32, handler)
                    }
                    b';' => {
                        if self.csi.params.len() < MAX_CSI_PARAMS {
                            self.csi.params.push_param(0, false);
                        }
                        self.csi.next_is_subparam = false;
                        self.csi.state = CsiState::Body;
                    }
                    b'0'..=b'9' => {
                        self.csi_add_digit(ch);
                        self.consume_digit_run(input, pos);
                        self.csi.state = CsiState::Body;
                    }
                    b'?' | b'>' | b'<' | b'=' => {
                        self.csi.params.set_primary(ch);
                        self.csi.state = CsiState::Body;
                    }
                    ch if is_csi_secondary(ch) => {
                        if ch == b'-' {
                            self.csi.mult = -1;
                            self.csi.num_digits = 1;
                            self.csi.state = CsiState::Body;
                        } else {
                            self.csi.params.set_secondary(ch);
                            self.csi.state = CsiState::PostSecondary;
                        }
                    }
                    ch if is_csi_trailer(ch) => {
                        self.csi.params.set_trailer(ch);
                        self.csi.is_valid = true;
                        return true;
                    }
                    _ => {
                        self.report_invalid_csi(ch, handler);
                        return true;
                    }
                },
                CsiState::PostSecondary => {
                    match ch {
                        BEL | BS | HT | LF | VT | FF | CR | SO | SI => {
                            self.emit_char(ch as u32, handler)
                        }
                        ch if is_csi_trailer(ch) => {
                            self.csi.params.set_trailer(ch);
                            self.csi.is_valid = true;
                        }
                        _ => self.report_invalid_csi(ch, handler),
                    }
                    return true;
                }
                CsiState::Body => match ch {
                    BEL | BS | HT | LF | VT | FF | CR | SO | SI => {
                        self.emit_char(ch as u32, handler)
                    }
                    ch if is_csi_secondary(ch) => {
                        if ch == b'-' && self.csi.num_digits == 0 {
                            self.csi.mult = -1;
                            self.csi.num_digits = 1;
                        } else {
                            if !self.csi_commit(handler) {
                                return true;
                            }
                            self.csi.params.set_secondary(ch);
                            self.csi.state = CsiState::PostSecondary;
                        }
                    }
                    ch if is_csi_trailer(ch) => {
                        if self.csi.num_digits == 1
                            && self.csi.params.secondary() == 0
                            && self.csi.mult == -1
                        {
                            self.csi.num_digits = 0;
                            self.csi.params.set_secondary(b'-');
                        }
                        if !self.csi_commit(handler) {
                            return true;
                        }
                        self.csi.params.set_trailer(ch);
                        self.csi.is_valid = true;
                        return true;
                    }
                    b':' => {
                        if !self.csi_commit(handler) {
                            return true;
                        }
                        self.csi.next_is_subparam = true;
                    }
                    b';' => {
                        if self.csi.num_digits == 0 {
                            self.csi.num_digits = 1;
                        }
                        if !self.csi_commit(handler) {
                            return true;
                        }
                        self.csi.next_is_subparam = false;
                    }
                    b'0'..=b'9' => {
                        self.csi_add_digit(ch);
                        self.consume_digit_run(input, pos);
                    }
                    _ => {
                        self.report_invalid_csi(ch, handler);
                        return true;
                    }
                },
            }
        }

        if self.csi.length > MAX_ESCAPE_CODE_LENGTH {
            self.emit_error(handler, "CSI escape too long ignoring and truncating");
            return true;
        }
        false
    }

    fn csi_add_digit(&mut self, ch: u8) {
        if self.csi.num_digits >= MAX_CSI_DIGITS {
            return;
        }
        self.csi.num_digits += 1;
        self.csi.accumulator = self.csi.accumulator * 10 + (ch - b'0') as u64;
    }

    fn consume_digit_run(&mut self, input: &[u8], pos: &mut usize) {
        while *pos < input.len() && input[*pos].is_ascii_digit() {
            let ch = input[*pos];
            *pos += 1;
            self.csi.length += 1;
            self.csi_add_digit(ch);
        }
    }

    fn csi_commit<H: Handler + ?Sized>(&mut self, handler: &mut H) -> bool {
        if self.csi.num_digits == 0 {
            return true;
        }
        if self.csi.params.len() >= MAX_CSI_PARAMS {
            self.emit_error(
                handler,
                "CSI escape code has too many parameters, ignoring it",
            );
            return false;
        }
        let value = (self.csi.mult as i64).wrapping_mul(self.csi.accumulator as i64) as i32;
        let is_sub = self.csi.next_is_subparam;
        self.csi.params.push_param(value, is_sub);
        self.csi.num_digits = 0;
        self.csi.mult = 1;
        self.csi.accumulator = 0;
        true
    }

    fn report_invalid_csi<H: Handler + ?Sized>(&mut self, ch: u8, handler: &mut H) {
        let letter = csi_letter(ch);
        let msg = format!("Invalid character in CSI: {letter} (0x{ch:x}), ignoring the sequence");
        self.emit_error(handler, &msg);
    }

    fn consume_string<H: Handler + ?Sized>(
        &mut self,
        input: &[u8],
        pos: &mut usize,
        handler: &mut H,
    ) {
        while *pos < input.len() {
            let ch = input[*pos];
            if self.string_pending_esc {
                self.string_pending_esc = false;
                if ch == ESC_ST {
                    *pos += 1;
                    self.string.pop();
                    self.finish_string(handler);
                    return;
                }
            }
            if ch == ESC {
                self.string.push(ESC);
                self.string_pending_esc = true;
                *pos += 1;
            } else if ch == BEL {
                *pos += 1;
                self.finish_string(handler);
                return;
            } else {
                self.string.push(ch);
                *pos += 1;
            }
        }
        self.check_string_overflow(handler);
    }

    fn check_string_overflow<H: Handler + ?Sized>(&mut self, handler: &mut H) {
        if self.string.len() <= MAX_ESCAPE_CODE_LENGTH {
            return;
        }
        if self.state == VteState::Osc && self.string.starts_with(b"52;") {
            let len = self.string.len();
            let last = self.string[len - 1];
            let partial = self.string[..len - 1].to_vec();
            handler.osc_dispatch_extended(&partial);
            let mut next = Vec::with_capacity(5);
            next.extend_from_slice(b"52;;");
            next.push(last);
            self.string = next;
            self.string_pending_esc = last == ESC;
            return;
        }
        let len = self.string.len();
        let msg = format!(
            "{} escape code too long ({} bytes), ignoring it",
            self.state.name(),
            len
        );
        self.emit_error(handler, &msg);
        self.string.clear();
        self.string_pending_esc = false;
        self.state = VteState::Normal;
    }

    fn finish_string<H: Handler + ?Sized>(&mut self, handler: &mut H) {
        self.flush_text(handler);
        match self.state {
            VteState::Osc => handler.osc_dispatch(&self.string),
            VteState::Dcs if self.string.len() >= 2 => handler.dcs_dispatch(&self.string),
            VteState::Apc if self.string.len() >= 2 => handler.apc_dispatch(&self.string),
            VteState::Pm if self.string.len() >= 2 => handler.pm_dispatch(&self.string),
            VteState::Sos if self.string.len() >= 2 => handler.sos_dispatch(&self.string),
            _ => {}
        }
        self.string.clear();
        self.string_pending_esc = false;
        self.state = VteState::Normal;
    }
}

fn csi_letter(code: u8) -> String {
    if (33..=126).contains(&code) {
        (code as char).to_string()
    } else {
        format!("0x{code:x}")
    }
}

fn is_csi_secondary(ch: u8) -> bool {
    matches!(
        ch,
        b' ' | b'!'
            | b'"'
            | b'#'
            | b'$'
            | b'%'
            | b'&'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b'+'
            | b','
            | b'-'
            | b'.'
            | b'/'
    )
}

fn is_csi_trailer(ch: u8) -> bool {
    ch.is_ascii_alphabetic() || matches!(ch, b'@' | b'`' | b'{' | b'|' | b'}' | b'~')
}
