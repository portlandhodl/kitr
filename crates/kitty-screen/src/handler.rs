// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from the dispatch logic in kitty/vt-parser.c and kitty/screen.c
// Copyright (C) 2016-2024 Kovid Goyal <kovid at kovidgoyal.net>

//! Adapter from [`kitty_vt::Handler`] to [`Screen`].
//!
//! Only the CSI/ESC actions implemented by this crate are dispatched. Every
//! other action is recorded through [`Handler::report_error`] instead of
//! pretending to succeed.

use kitty_vt::{CsiParams, Handler, Parser};

use crate::screen::Screen;

/// A parser handler that drives a [`Screen`].
pub struct ScreenHandler {
    pub screen: Screen,
    pub errors: Vec<String>,
}

impl ScreenHandler {
    pub fn new(screen: Screen) -> Self {
        Self {
            screen,
            errors: Vec::new(),
        }
    }

    /// Feed bytes through a parser into the screen.
    pub fn feed(&mut self, parser: &mut Parser, bytes: &[u8]) {
        parser.parse(bytes, self);
    }

    fn positive(&mut self, params: &CsiParams, index: usize, default: i32) -> u32 {
        let value = params.param_or(index, default);
        if value < 0 {
            self.report_error(&format!("CSI code has negative parameter ({value})"));
            1
        } else {
            value as u32
        }
    }

    fn handle_mode(&mut self, params: &CsiParams, set: bool) {
        let private = params.primary() == b'?';
        let shift = if private { 5 } else { 0 };
        for &p in params.raw_params() {
            if p < 0 {
                continue;
            }
            let mode = (p as u32) << shift;
            let ok = if set {
                self.screen.set_mode(mode)
            } else {
                self.screen.reset_mode(mode)
            };
            if !ok {
                self.report_error(&format!(
                    "Unsupported screen mode: {p}{}",
                    if private { " (private)" } else { "" }
                ));
            }
        }
    }

    fn handle_sgr(&mut self, csi: &CsiParams) {
        let mut params = csi.raw_params().to_vec();
        let mut subs = csi.subparam_flags().to_vec();
        if params.is_empty() {
            params.push(0);
            subs.push(false);
        }
        match sgr_chunks(&params, &subs) {
            Ok(chunks) => {
                for (chunk, is_group) in chunks {
                    self.screen.select_graphic_rendition(&chunk, is_group);
                }
            }
            Err(msg) => self.report_error(&msg),
        }
    }
}

fn send_chunk(
    chunks: &mut Vec<(Vec<i32>, bool)>,
    params: &[i32],
    first: &mut usize,
    num: &mut usize,
    state: &mut u8,
) {
    if *num > 0 {
        chunks.push((
            params[*first..*first + *num].to_vec(),
            *state != STATE_NORMAL,
        ));
        *first += *num;
        *num = 0;
        *state = STATE_NORMAL;
    }
}

const STATE_NORMAL: u8 = 0;
const STATE_SUB: u8 = 1;
const STATE_COLOR: u8 = 2;
const STATE_COLOR1: u8 = 3;
const STATE_COLOR3: u8 = 4;

/// The SGR grouping state machine from `_parse_sgr()` in `vt-parser.c`.
fn sgr_chunks(params: &[i32], subs: &[bool]) -> Result<Vec<(Vec<i32>, bool)>, String> {
    let mut chunks = Vec::new();
    let mut first = 0usize;
    let mut num = 0usize;
    let mut state = STATE_NORMAL;
    let mut pos = 0usize;
    while pos < params.len() {
        match state {
            STATE_NORMAL => {
                if subs[pos] {
                    if num == 0 || pos == 0 {
                        return Err(
                            "SGR escape code has an unexpected sub-parameter ignoring the full code"
                                .to_string(),
                        );
                    }
                    num -= 1;
                    send_chunk(&mut chunks, params, &mut first, &mut num, &mut state);
                    state = STATE_SUB;
                    first = pos - 1;
                    num = 1;
                }
                match params[pos] {
                    38 | 48 | 58 => {
                        send_chunk(&mut chunks, params, &mut first, &mut num, &mut state);
                        state = STATE_COLOR;
                        first = pos;
                        num = 1;
                    }
                    _ => num += 1,
                }
            }
            STATE_SUB => {
                if subs[pos] {
                    num += 1;
                } else {
                    send_chunk(&mut chunks, params, &mut first, &mut num, &mut state);
                    // Re-process this parameter in the normal state.
                    pos = pos.wrapping_sub(1);
                }
            }
            STATE_COLOR => match params[pos] {
                2 => {
                    state = if subs[pos] { STATE_SUB } else { STATE_COLOR3 };
                    num += 1;
                }
                5 => {
                    state = if subs[pos] { STATE_SUB } else { STATE_COLOR1 };
                    num += 1;
                }
                other => {
                    return Err(format!(
                        "SGR escape code has unknown color type: {other} ignoring the full code"
                    ));
                }
            },
            STATE_COLOR1 => {
                num += 1;
                send_chunk(&mut chunks, params, &mut first, &mut num, &mut state);
            }
            STATE_COLOR3 => {
                num += 1;
                if num >= 5 {
                    send_chunk(&mut chunks, params, &mut first, &mut num, &mut state);
                }
            }
            _ => unreachable!(),
        }
        pos += 1;
    }
    send_chunk(&mut chunks, params, &mut first, &mut num, &mut state);
    Ok(chunks)
}

impl Handler for ScreenHandler {
    fn draw(&mut self, text: &[u32]) {
        self.screen.draw_text(text);
    }

    fn bell(&mut self) {
        self.screen.bell_count += 1;
    }

    fn backspace(&mut self) {
        self.screen.backspace();
    }

    fn tab(&mut self) {
        self.screen.tab();
    }

    fn linefeed(&mut self) {
        self.screen.linefeed();
    }

    fn carriage_return(&mut self) {
        self.screen.carriage_return();
    }

    fn change_charset(&mut self, which: u8) {
        self.screen.charset.select(which);
    }

    fn designate_charset(&mut self, which: u8, ch: u8) {
        self.screen.charset.designate(which, ch);
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], action: u8) {
        if intermediates == b"#" && action == b'8' {
            self.screen.align();
            return;
        }
        match action {
            b'c' => self.screen.reset(),
            b'D' => self.screen.index(),
            b'E' => {
                self.screen.carriage_return();
                self.screen.linefeed();
            }
            b'H' => self.screen.set_tab_stop(),
            b'M' => self.screen.reverse_index(),
            b'7' => self.screen.save_cursor(),
            b'8' => self.screen.restore_cursor(),
            b'>' | b'=' => {}
            other => self.report_error(&format!("Unknown char after ESC: 0x{other:x}")),
        }
    }

    fn csi_dispatch(&mut self, params: &CsiParams, intermediates: &[u8], action: u8) {
        let secondary = intermediates.first().copied().unwrap_or(0);
        match action {
            b'@' => {
                let count = self.positive(params, 0, 1);
                self.screen.insert_characters(count);
            }
            b'b' => {
                let count = self.positive(params, 0, 1);
                self.screen.repeat_character(count);
            }
            b'A' => {
                if secondary == b' ' {
                    self.report_error("Shift right escape code not implemented");
                    return;
                }
                let count = self.positive(params, 0, 1);
                self.screen.cursor_up(count);
            }
            b'B' | b'e' => {
                let count = self.positive(params, 0, 1);
                self.screen.cursor_down(count);
            }
            b'C' | b'a' => {
                let count = self.positive(params, 0, 1);
                self.screen.cursor_forward(count);
            }
            b'D' => {
                let count = self.positive(params, 0, 1);
                self.screen.cursor_move(count, -1, false);
            }
            b'E' => {
                let count = self.positive(params, 0, 1);
                self.screen.cursor_down1(count);
            }
            b'F' => {
                let count = self.positive(params, 0, 1);
                self.screen.cursor_up1(count);
            }
            b'G' | b'`' => {
                let column = self.positive(params, 0, 1);
                self.screen.cursor_to_column(column);
            }
            b'd' => {
                let line = self.positive(params, 0, 1);
                self.screen.cursor_to_line(line);
            }
            b'Z' => {
                let count = self.positive(params, 0, 1);
                self.screen.backtab(count);
            }
            b'I' => {
                let count = self.positive(params, 0, 1).max(1);
                for _ in 0..count {
                    let before = self.screen.cursor.x;
                    self.screen.tab();
                    if self.screen.cursor.x == before {
                        break;
                    }
                }
            }
            b'H' | b'f' => {
                let line = self.positive(params, 0, 1);
                let column = self.positive(params, 1, 1);
                self.screen.cursor_position(line, column);
            }
            b'J' => {
                let how = self.positive(params, 0, 0);
                let private = params.primary() == b'?';
                if !self.screen.erase_in_display(how, private) {
                    self.report_error(&format!("Unsupported erase in display mode: {how}"));
                }
            }
            b'K' => {
                let how = self.positive(params, 0, 0);
                let private = params.primary() == b'?';
                if !self.screen.erase_in_line(how, private) {
                    self.report_error(&format!("Unsupported erase in line mode: {how}"));
                }
            }
            b'L' => {
                let count = self.positive(params, 0, 1);
                self.screen.insert_lines(count);
            }
            b'M' => {
                let count = self.positive(params, 0, 1);
                self.screen.delete_lines(count);
            }
            b'P' => {
                if secondary == b'#' && params.primary() == 0 {
                    self.report_error("Color stack is not implemented");
                    return;
                }
                let count = self.positive(params, 0, 1);
                self.screen.delete_characters(count);
            }
            b'X' => {
                let count = self.positive(params, 0, 1);
                self.screen.erase_characters(count);
            }
            b'S' => {
                if secondary == b' ' {
                    self.report_error("Select presentation directions escape code not implemented");
                    return;
                }
                let count = self.positive(params, 0, 1);
                self.screen.scroll(count);
            }
            b'T' => {
                let count = self.positive(params, 0, 1);
                if secondary == b'+' {
                    self.screen.reverse_scroll_and_fill_from_scrollback(count);
                } else if params.primary() == 0 {
                    self.screen.reverse_scroll(count);
                } else {
                    self.report_error("Unsupported SD start modifier");
                }
            }
            b'g' => {
                let how = self.positive(params, 0, 0);
                if !self.screen.clear_tab_stop(how) {
                    self.report_error(&format!("Unsupported clear tab stop mode: {how}"));
                }
            }
            b'h' => self.handle_mode(params, true),
            b'l' => self.handle_mode(params, false),
            b'm' => self.handle_sgr(params),
            b's' => {
                if params.primary() == 0 && secondary == 0 && params.is_empty() {
                    self.screen.save_cursor();
                } else if params.primary() == b'?' && secondary == 0 {
                    if params.is_empty() {
                        self.screen.save_modes();
                    } else {
                        self.handle_mode(params, true);
                    }
                } else {
                    self.report_error("Unknown CSI s sequence with modifiers");
                }
            }
            b'u' => {
                if params.primary() == 0 && secondary == 0 && params.is_empty() {
                    self.screen.restore_cursor();
                } else {
                    self.report_error("Unsupported CSI u sequence");
                }
            }
            b'r' => {
                if params.primary() == 0 && secondary == 0 {
                    let top = self.positive(params, 0, 0);
                    let bottom = self.positive(params, 1, 0);
                    self.screen.set_margins(top, bottom);
                } else if params.primary() == b'?' && secondary == 0 {
                    if params.is_empty() {
                        self.screen.restore_modes();
                    } else {
                        self.handle_mode(params, false);
                    }
                } else {
                    self.report_error("Unsupported CSI r sequence with modifiers");
                }
            }
            b'W' => {
                if params.primary() == b'?'
                    && secondary == 0
                    && params.len() == 1
                    && params.get(0) == Some(5)
                {
                    self.screen.reset_tab_stops();
                } else {
                    self.report_error("Unknown CSI W sequence");
                }
            }
            b'q' => {
                if secondary == b' ' && params.primary() == 0 {
                    let mode = self.positive(params, 0, 1);
                    self.screen.set_cursor_shape(mode, secondary);
                } else {
                    self.report_error("Unsupported CSI q sequence");
                }
            }
            b'c' | b'n' | b't' => {
                self.report_error("Device reports and window manipulation are not implemented");
            }
            other => self.report_error(&format!("Unknown CSI code: '{}'", char::from(other))),
        }
    }

    fn osc_dispatch(&mut self, data: &[u8]) {
        let Some(sep) = data.iter().position(|&b| b == b';') else {
            self.report_error("Unsupported OSC sequence without a separator");
            return;
        };
        let code = &data[..sep];
        let payload = String::from_utf8_lossy(&data[sep + 1..]).into_owned();
        match code {
            b"0" => {
                self.screen.title = Some(payload.clone());
                self.screen.icon_name = Some(payload);
            }
            b"1" => self.screen.icon_name = Some(payload),
            b"2" => self.screen.title = Some(payload),
            _ => self.report_error(&format!(
                "Unsupported OSC code: {}",
                String::from_utf8_lossy(code)
            )),
        }
    }

    fn osc_dispatch_extended(&mut self, data: &[u8]) {
        self.osc_dispatch(data);
    }

    fn dcs_dispatch(&mut self, _data: &[u8]) {
        self.report_error("DCS sequences are not implemented");
    }

    fn pm_dispatch(&mut self, _data: &[u8]) {
        self.report_error("PM sequences are not implemented");
    }

    fn sos_dispatch(&mut self, _data: &[u8]) {
        self.report_error("SOS sequences are not implemented");
    }

    fn apc_dispatch(&mut self, _data: &[u8]) {
        self.report_error("APC sequences are not implemented");
    }

    fn report_error(&mut self, message: &str) {
        self.errors.push(message.to_string());
    }
}

/// Parse a complete chunk into a screen (convenience for tests).
pub fn parse_into(screen: &mut Screen, bytes: &[u8]) -> Vec<String> {
    let mut parser = Parser::new();
    let mut handler = ScreenHandler {
        screen: std::mem::replace(
            screen,
            Screen::builder(1, 1).build().expect("one cell fallback"),
        ),
        errors: Vec::new(),
    };
    parser.parse(bytes, &mut handler);
    *screen = handler.screen;
    handler.errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler(cols: u32, lines: u32) -> ScreenHandler {
        ScreenHandler::new(Screen::builder(cols, lines).scrollback(8).build().unwrap())
    }

    fn feed(h: &mut ScreenHandler, bytes: &[u8]) {
        h.feed(&mut Parser::new(), bytes);
    }

    #[test]
    fn csi_cursor_and_edit_dispatch() {
        let mut h = handler(5, 5);
        feed(&mut h, b"abcde");
        feed(&mut h, b"\x1b[1;2H");
        feed(&mut h, b"\x1b[2@");
        assert_eq!(h.screen.line_text(0), "a  bc");
        feed(&mut h, b"\x1b[2;7@");
        assert!(!h.errors.iter().any(|e| e.contains("2 > 1")));
        feed(&mut h, b"\x1b[H");
        assert_eq!((h.screen.cursor.x, h.screen.cursor.y), (0, 0));
        feed(&mut h, b"\x1b[3;2H");
        assert_eq!((h.screen.cursor.x, h.screen.cursor.y), (1, 2));
        feed(&mut h, b"\x1b[J");
        assert_eq!(h.screen.line_text(2), "");
    }

    #[test]
    fn modes_and_alt_screen() {
        let mut h = handler(5, 5);
        feed(&mut h, b"abc");
        feed(&mut h, b"\x1b[?1049h");
        assert!(h.screen.is_using_alternate_linebuf());
        feed(&mut h, b"x");
        feed(&mut h, b"\x1b[?1049l");
        assert!(h.screen.is_main_linebuf());
        assert_eq!(h.screen.line_text(0), "abc");
        feed(&mut h, b"\x1b[?25l");
        assert!(!h.screen.is_cursor_visible());
    }

    #[test]
    fn sgr_dispatch() {
        let mut h = handler(5, 5);
        feed(&mut h, b"\x1b[1;31;44m");
        assert!(h.screen.cursor.sgr.bold);
        assert_eq!(h.screen.cursor.sgr.fg, (1 << 8) | 1);
        assert_eq!(h.screen.cursor.sgr.bg, (4 << 8) | 1);
        feed(&mut h, b"\x1b[38;5;200m");
        assert_eq!(h.screen.cursor.sgr.fg, (200 << 8) | 1);
        feed(&mut h, b"\x1b[38;2;1;2;3m");
        assert_eq!(h.screen.cursor.sgr.fg, (1 << 24) | (2 << 16) | (3 << 8) | 2);
    }

    #[test]
    fn sgr_subparameters_and_groups() {
        let mut h = handler(5, 5);
        feed(&mut h, b"\x1b[38:2:1:2:3;48:5:9;4:5m");
        assert_eq!(h.screen.cursor.sgr.fg, (1 << 24) | (2 << 16) | (3 << 8) | 2);
        assert_eq!(h.screen.cursor.sgr.bg, (9 << 8) | 1);
        assert_eq!(h.screen.cursor.sgr.decoration, 5);
        feed(&mut h, b"\x1b[1;2m");
        assert!(h.screen.cursor.sgr.bold);
        assert!(h.screen.cursor.sgr.dim);
    }

    #[test]
    fn esc_dispatch_and_charset() {
        let mut h = handler(10, 3);
        feed(&mut h, b"12\x1bDa");
        assert_eq!(h.screen.line_text(0), "12");
        assert_eq!(h.screen.line_text(1), "  a");
        feed(&mut h, b"\x1b(0qqq\x1b(B");
        assert!(h.screen.line_text(1).contains("───"));
    }

    #[test]
    fn unsupported_actions_report_errors() {
        let mut h = handler(5, 5);
        feed(&mut h, b"\x1bPqq\x1b\\");
        assert!(h.errors.iter().any(|e| e.contains("DCS")));
        feed(&mut h, b"\x1b]8;;http://x\x1b\\");
        assert!(h.errors.iter().any(|e| e.contains("OSC")));
        feed(&mut h, b"\x1b[14t");
        assert!(h.errors.iter().any(|e| e.contains("window")));
    }

    #[test]
    fn osc_title_and_icon() {
        let mut h = handler(5, 5);
        feed(&mut h, b"\x1b]2;hello\x1b\\");
        assert_eq!(h.screen.title.as_deref(), Some("hello"));
        feed(&mut h, b"\x1b]1;icon\x1b\\");
        assert_eq!(h.screen.icon_name.as_deref(), Some("icon"));
    }

    #[test]
    fn rep_and_margins() {
        let mut h = handler(8, 6);
        feed(&mut h, b"ab\x1b[3b");
        assert_eq!(h.screen.line_text(0), "abbbb");
        feed(&mut h, b"\x1b[2;4r");
        assert_eq!(h.screen.margins_1_based(), (2, 4));
        feed(&mut h, b"\x1b[r");
        assert_eq!(h.screen.margins_1_based(), (1, 6));
    }

    #[test]
    fn parse_into_helper() {
        let mut s = Screen::builder(5, 5).build().unwrap();
        let errors = parse_into(&mut s, b"hi\x1b[2Dx");
        assert!(errors.is_empty());
        assert_eq!(s.line_text(0), "xi");
    }
}
