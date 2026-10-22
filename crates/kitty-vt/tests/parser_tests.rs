// SPDX-License-Identifier: GPL-3.0-or-later

use kitty_vt::{CsiParams, Handler, Parser, MAX_ESCAPE_CODE_LENGTH};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Draw(String),
    Bell,
    Backspace,
    Tab,
    Linefeed,
    CarriageReturn,
    ChangeCharset(u8),
    DesignateCharset(u8, u8),
    Esc {
        intermediates: Vec<u8>,
        action: u8,
    },
    Csi {
        params: Vec<i32>,
        flags: Vec<bool>,
        primary: u8,
        secondary: u8,
        action: u8,
        intermediates: Vec<u8>,
        subgroups: Vec<Vec<i32>>,
    },
    Osc(Vec<u8>),
    OscExtended(Vec<u8>),
    Dcs(Vec<u8>),
    Pm(Vec<u8>),
    Sos(Vec<u8>),
    Apc(Vec<u8>),
    Error(String),
}

#[derive(Default)]
struct RecordingHandler {
    events: Vec<Event>,
}

impl RecordingHandler {
    fn draw_text(&self) -> String {
        let mut ans = String::new();
        for event in &self.events {
            if let Event::Draw(text) = event {
                ans.push_str(text);
            }
        }
        ans
    }

    fn errors(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Error(msg) => Some(msg.as_str()),
                _ => None,
            })
            .collect()
    }

    fn normalized(&self) -> Vec<Event> {
        let mut ans: Vec<Event> = Vec::with_capacity(self.events.len());
        for event in &self.events {
            match event {
                Event::Draw(text) => {
                    if let Some(Event::Draw(prev)) = ans.last_mut() {
                        prev.push_str(text);
                    } else {
                        ans.push(Event::Draw(text.clone()));
                    }
                }
                other => ans.push(other.clone()),
            }
        }
        ans
    }
}

fn u32_to_string(text: &[u32]) -> String {
    text.iter()
        .map(|&ch| char::from_u32(ch).unwrap_or('\u{fffd}'))
        .collect()
}

impl Handler for RecordingHandler {
    fn draw(&mut self, text: &[u32]) {
        self.events.push(Event::Draw(u32_to_string(text)));
    }

    fn bell(&mut self) {
        self.events.push(Event::Bell);
    }

    fn backspace(&mut self) {
        self.events.push(Event::Backspace);
    }

    fn tab(&mut self) {
        self.events.push(Event::Tab);
    }

    fn linefeed(&mut self) {
        self.events.push(Event::Linefeed);
    }

    fn carriage_return(&mut self) {
        self.events.push(Event::CarriageReturn);
    }

    fn change_charset(&mut self, which: u8) {
        self.events.push(Event::ChangeCharset(which));
    }

    fn designate_charset(&mut self, which: u8, ch: u8) {
        self.events.push(Event::DesignateCharset(which, ch));
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], action: u8) {
        self.events.push(Event::Esc {
            intermediates: intermediates.to_vec(),
            action,
        });
    }

    fn csi_dispatch(&mut self, params: &CsiParams, intermediates: &[u8], action: u8) {
        let subgroups = (0..params.len())
            .filter_map(|i| params.subparams(i))
            .map(|group| group.to_vec())
            .collect();
        self.events.push(Event::Csi {
            params: params.raw_params().to_vec(),
            flags: params.subparam_flags().to_vec(),
            primary: params.primary(),
            secondary: params.secondary(),
            action,
            intermediates: intermediates.to_vec(),
            subgroups,
        });
    }

    fn osc_dispatch(&mut self, data: &[u8]) {
        self.events.push(Event::Osc(data.to_vec()));
    }

    fn osc_dispatch_extended(&mut self, data: &[u8]) {
        self.events.push(Event::OscExtended(data.to_vec()));
    }

    fn dcs_dispatch(&mut self, data: &[u8]) {
        self.events.push(Event::Dcs(data.to_vec()));
    }

    fn pm_dispatch(&mut self, data: &[u8]) {
        self.events.push(Event::Pm(data.to_vec()));
    }

    fn sos_dispatch(&mut self, data: &[u8]) {
        self.events.push(Event::Sos(data.to_vec()));
    }

    fn apc_dispatch(&mut self, data: &[u8]) {
        self.events.push(Event::Apc(data.to_vec()));
    }

    fn report_error(&mut self, message: &str) {
        self.events.push(Event::Error(message.to_string()));
    }
}

fn parse(data: &[u8]) -> RecordingHandler {
    let mut parser = Parser::new();
    let mut handler = RecordingHandler::default();
    parser.parse(data, &mut handler);
    handler
}

fn parse_in_chunks(data: &[u8], split: &[usize]) -> RecordingHandler {
    let mut parser = Parser::new();
    let mut handler = RecordingHandler::default();
    let mut start = 0;
    for &end in split {
        assert!(end >= start && end <= data.len());
        parser.parse(&data[start..end], &mut handler);
        start = end;
    }
    parser.parse(&data[start..], &mut handler);
    handler
}

fn csi_events(handler: &RecordingHandler) -> Vec<&Event> {
    handler
        .events
        .iter()
        .filter(|event| matches!(event, Event::Csi { .. }))
        .collect()
}

// 1
#[test]
fn plain_text_and_control_hooks() {
    let handler = parse(b"ab\x07c\x08d\te\nf\rg");
    assert_eq!(
        handler.normalized(),
        vec![
            Event::Draw("ab".into()),
            Event::Bell,
            Event::Draw("c".into()),
            Event::Backspace,
            Event::Draw("d".into()),
            Event::Tab,
            Event::Draw("e".into()),
            Event::Linefeed,
            Event::Draw("f".into()),
            Event::CarriageReturn,
            Event::Draw("g".into()),
        ]
    );
}

// 2
#[test]
fn incremental_splits_match_single_call() {
    let data =
        b"a\xc3\xa9\x1b[38:2:1:2;3m\x1b]2;ti\x1b\\\x1bP+qtext\x1b\\\x1b(0\x0f_\xf0\x9f\x98\xb8";
    let expected = parse(data).normalized();
    for split in 0..=data.len() {
        assert_eq!(
            parse_in_chunks(data, &[split]).normalized(),
            expected,
            "failed for split at {split}"
        );
    }
    let all: Vec<usize> = (0..=data.len()).collect();
    assert_eq!(parse_in_chunks(data, &all).normalized(), expected);
}

// 3
#[test]
fn utf8_split_inside_sequence() {
    let data = "aé😸b".as_bytes();
    let expected = "aé😸b";
    for split in 0..=data.len() {
        let this = parse_in_chunks(data, &[split]);
        assert_eq!(this.draw_text(), expected, "failed for split at {split}");
    }
    let byte_by_byte: Vec<usize> = (0..=data.len()).collect();
    assert_eq!(parse_in_chunks(data, &byte_by_byte).draw_text(), expected);
}

// 4
#[test]
fn invalid_utf8_replacement() {
    let cases: &[(&[u8], &str)] = &[
        (b"abcd\xf51234", "abcd\u{fffd}1234"),
        (b"abcd\xff1234", "abcd\u{fffd}1234"),
        (b"\"\xbf\"", "\"\u{fffd}\""),
        (b"\"\x80\"", "\"\u{fffd}\""),
        (b"\"\x80\xbf\"", "\"\u{fffd}\u{fffd}\""),
        (b"\"\x80\xbf\x80\"", "\"\u{fffd}\u{fffd}\u{fffd}\""),
        (b"\"\xc0 \"", "\"\u{fffd} \""),
        (b"\"\xfe\"", "\"\u{fffd}\""),
        (b"\"\xff\"", "\"\u{fffd}\""),
        (b"\"\xff\xfe\"", "\"\u{fffd}\u{fffd}\""),
        (
            b"\"\xfe\xfe\xff\xff\"",
            "\"\u{fffd}\u{fffd}\u{fffd}\u{fffd}\"",
        ),
        (b"\"\xef\xbf\"", "\"\u{fffd}\""),
        (b"\"\xe0\xa0\"", "\"\u{fffd}\""),
        (b"\"\xf0\x9f\x98\"", "\"\u{fffd}\""),
        (b"\"\xef\x93\x94\x95\"", "\"\u{f4d4}\u{fffd}\""),
        (b"\"\xe1\x28\xa1\"", "\"\u{fffd}(\u{fffd}\""),
        (b"\"\xc0\x80\"", "\"\u{fffd}\u{fffd}\""),
        (b"\"\xe0\x80\x80\"", "\"\u{fffd}\u{fffd}\u{fffd}\""),
        (
            b"\"\xf0\x80\x80\x80\"",
            "\"\u{fffd}\u{fffd}\u{fffd}\u{fffd}\"",
        ),
        (b"\"\xed\xa0\x80\"", "\"\u{fffd}\u{fffd}\u{fffd}\""),
        (b"\"\xed\xb0\x80\"", "\"\u{fffd}\u{fffd}\u{fffd}\""),
        (
            b"\"\xff\x80\x80\x80\"",
            "\"\u{fffd}\u{fffd}\u{fffd}\u{fffd}\"",
        ),
        (b"\"\xc2\x80\"", "\"\u{80}\""),
        (b"\"\xdf\xbf\"", "\"\u{7ff}\""),
        (b"\"\xe0\xa0\x80\"", "\"\u{800}\""),
        (b"\"\xf0\x90\x80\x80\"", "\"\u{10000}\""),
        (b"\"\xf4\x8f\xbf\xbf\"", "\"\u{10ffff}\""),
        (
            b"\"\xf5\x80\x80\x80\"",
            "\"\u{fffd}\u{fffd}\u{fffd}\u{fffd}\"",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(parse(input).draw_text(), *expected, "failed for {input:?}");
    }
}

// 5
#[test]
fn esc_single_codes_and_unknown() {
    let handler = parse(b"\x1bDa\x1bcb\x1bxa");
    assert_eq!(
        handler.normalized(),
        vec![
            Event::Esc {
                intermediates: vec![],
                action: b'D'
            },
            Event::Draw("a".into()),
            Event::Esc {
                intermediates: vec![],
                action: b'c'
            },
            Event::Draw("b".into()),
            Event::Error("Unknown char after ESC: 0x78".into()),
            Event::Draw("a".into()),
        ]
    );

    let handler = parse(b"\x1b\x1b%a");
    assert_eq!(
        handler.normalized(),
        vec![
            Event::Error("Unknown char after ESC: 0x1b".into()),
            Event::Draw("%a".into()),
        ]
    );
}

// 6
#[test]
fn esc_charset_designation_and_shift() {
    let handler = parse(b"\x1b)0\x0e/_");
    assert_eq!(
        handler.normalized(),
        vec![
            Event::DesignateCharset(1, b'0'),
            Event::ChangeCharset(1),
            Event::Draw("/_".into()),
        ]
    );

    let handler = parse(b"\x1b(0/_");
    assert_eq!(
        handler.normalized(),
        vec![Event::DesignateCharset(0, b'0'), Event::Draw("/_".into())]
    );

    let handler = parse(b"\x1b(A#");
    assert_eq!(
        handler.normalized(),
        vec![Event::DesignateCharset(0, b'A'), Event::Draw("#".into())]
    );

    let handler = parse(b"\x1b(Va");
    assert_eq!(
        handler.normalized(),
        vec![Event::DesignateCharset(0, b'V'), Event::Draw("a".into())]
    );

    let handler = parse(b"\x1b(U\x0e\x0f_");
    assert_eq!(
        handler.normalized(),
        vec![
            Event::DesignateCharset(0, b'U'),
            Event::ChangeCharset(1),
            Event::ChangeCharset(0),
            Event::Draw("_".into()),
        ]
    );
}

#[derive(Default)]
struct CharsetApplyingHandler {
    g0: Option<&'static [u32; 256]>,
    g1: Option<&'static [u32; 256]>,
    active: usize,
    text: String,
}

impl Handler for CharsetApplyingHandler {
    fn designate_charset(&mut self, which: u8, ch: u8) {
        let table = kitty_vt::translation_table(ch);
        if which == 0 {
            self.g0 = table;
        } else {
            self.g1 = table;
        }
    }

    fn change_charset(&mut self, which: u8) {
        self.active = usize::from(which);
    }

    fn draw(&mut self, text: &[u32]) {
        let table = if self.active == 0 { self.g0 } else { self.g1 };
        for &ch in text {
            let mapped = match table {
                Some(table) if ch < 256 => table[ch as usize],
                _ => ch,
            };
            self.text.push(char::from_u32(mapped).unwrap_or('\u{fffd}'));
        }
    }
}

fn parse_with_charsets(data: &[u8]) -> String {
    let mut parser = Parser::new();
    let mut handler = CharsetApplyingHandler::default();
    parser.parse(data, &mut handler);
    handler.text
}

// 7
#[test]
fn charset_translation_is_applied_by_the_handler() {
    assert_eq!(parse_with_charsets(b"\x1b)0\x0e/_"), "/\u{a0}");
    assert_eq!(parse_with_charsets(b"\x1b(0/_"), "/\u{a0}");
    assert_eq!(parse_with_charsets(b"\x1b(A#"), "\u{a3}");
    assert_eq!(parse_with_charsets(b"\x1b(Va"), "\u{441}");
    assert_eq!(parse_with_charsets(b"\x1b(U\x0e\x0f_"), "_");
}

// 7
#[test]
fn esc_percent_hash_and_charset_errors() {
    let cases: &[(&[u8], &str)] = &[
        (b"\x1b%@", "Ignoring attempt to switch to non-utf8 encoding"),
        (
            b"\x1b%G",
            "Ignoring attempt to switch to utf8 encoding as we are always utf-8",
        ),
        (b"\x1b%a", "Unhandled Esc % code: 0x61"),
        (b"\x1b#x", "Unhandled Esc # code: 0x78"),
        (
            b"\x1b F",
            "Ignoring attempt to turn on/off C1 controls as we only support C0 controls",
        ),
        (
            b"\x1b G",
            "Ignoring attempt to turn on/off C1 controls as we only support C0 controls",
        ),
        (b"\x1b Q", "Unhandled ESC SP escape code: 0x51"),
        (
            b"\x1b.\x1b",
            "Unhandled charset related escape code: 0x2e 0x1b",
        ),
        (b"\x1b(X", "Unknown charset: 0x58"),
    ];
    for (input, expected) in cases {
        let handler = parse(input);
        assert_eq!(handler.errors(), vec![*expected], "input {input:?}");
    }

    let handler = parse(b"\x1b#8");
    assert_eq!(
        handler.events,
        vec![Event::Esc {
            intermediates: vec![b'#'],
            action: b'8'
        }]
    );
}

// 8
type CsiCase = (&'static [u8], Vec<i32>, u8, u8, u8);

#[test]
fn csi_basic_params_and_defaults() {
    let cases: &[CsiCase] = &[
        (b"\x1b[H", vec![], 0, 0, b'H'),
        (b"\x1b[4;0H", vec![4, 0], 0, 0, b'H'),
        (b"\x1b[3;2H", vec![3, 2], 0, 0, b'H'),
        (b"\x1b[3;2;H", vec![3, 2], 0, 0, b'H'),
        (
            b"\x1b[00000000003;0000000000000002H",
            vec![3, 2],
            0,
            0,
            b'H',
        ),
        (b"\x1b[0001234567890H", vec![1234567890], 0, 0, b'H'),
        (b"\x1b[J", vec![], 0, 0, b'J'),
        (b"\x1b[?J", vec![], b'?', 0, b'J'),
        (b"\x1b[?2J", vec![2], b'?', 0, b'J'),
        (b"\x1b[;2m", vec![0, 2], 0, 0, b'm'),
        (b"\x1b[1;;2m", vec![1, 0, 2], 0, 0, b'm'),
    ];
    for (input, params, primary, secondary, action) in cases {
        let handler = parse(input);
        let events = csi_events(&handler);
        assert_eq!(events.len(), 1, "input {input:?}");
        match events[0] {
            Event::Csi {
                params: p,
                primary: pr,
                secondary: sec,
                action: a,
                ..
            } => {
                assert_eq!(p, params, "input {input:?}");
                assert_eq!(*pr, *primary, "input {input:?}");
                assert_eq!(*sec, *secondary, "input {input:?}");
                assert_eq!(*a, *action, "input {input:?}");
            }
            _ => unreachable!(),
        }
    }
}

// 9
#[test]
fn csi_private_modes_and_secondary_intermediates() {
    let handler = parse(b"\x1b[?1000;1004h");
    assert_eq!(
        handler.events,
        vec![Event::Csi {
            params: vec![1000, 1004],
            flags: vec![false, false],
            primary: b'?',
            secondary: 0,
            action: b'h',
            intermediates: vec![],
            subgroups: vec![vec![1000], vec![1004]],
        }]
    );

    let handler = parse(b"\x1b[1 q");
    assert_eq!(
        handler.events,
        vec![Event::Csi {
            params: vec![1],
            flags: vec![false],
            primary: 0,
            secondary: b' ',
            action: b'q',
            intermediates: vec![b' '],
            subgroups: vec![vec![1]],
        }]
    );

    let handler = parse(b"\x1b[?1$p");
    assert_eq!(
        handler.events,
        vec![Event::Csi {
            params: vec![1],
            flags: vec![false],
            primary: b'?',
            secondary: b'$',
            action: b'p',
            intermediates: vec![b'$'],
            subgroups: vec![vec![1]],
        }]
    );
}

// 10
#[test]
fn csi_subparams_and_accessors() {
    let handler = parse(b"\x1b[38:2:1:2:3;48:5:9;58;5;7m");
    let events = csi_events(&handler);
    assert_eq!(events.len(), 1);
    match events[0] {
        Event::Csi {
            params,
            flags,
            subgroups,
            ..
        } => {
            assert_eq!(params, &vec![38, 2, 1, 2, 3, 48, 5, 9, 58, 5, 7]);
            assert_eq!(
                flags,
                &vec![false, true, true, true, true, false, true, true, false, false, false]
            );
            assert_eq!(
                subgroups,
                &vec![
                    vec![38, 2, 1, 2, 3],
                    vec![48, 5, 9],
                    vec![58],
                    vec![5],
                    vec![7]
                ]
            );
        }
        _ => unreachable!(),
    }

    let handler = parse(b"\x1b[4:5m");
    match &handler.events[0] {
        Event::Csi {
            params,
            flags,
            subgroups,
            ..
        } => {
            assert_eq!(params, &vec![4, 5]);
            assert_eq!(flags, &vec![false, true]);
            assert_eq!(subgroups, &vec![vec![4, 5]]);
        }
        _ => unreachable!(),
    }

    let handler = parse(b"\x1b[?:m\x1b[31m");
    let events = csi_events(&handler);
    assert_eq!(events.len(), 2);
    match events[1] {
        Event::Csi { params, flags, .. } => {
            assert_eq!(params, &vec![31]);
            assert_eq!(flags, &vec![false]);
        }
        _ => unreachable!(),
    }
}

// 11
#[test]
fn csi_negative_numbers_and_invalid_chars() {
    let handler = parse(b"\x1b[-0001234567890@");
    match &handler.events[0] {
        Event::Csi { params, action, .. } => {
            assert_eq!(params, &vec![-1234567890]);
            assert_eq!(*action, b'@');
        }
        _ => unreachable!(),
    }

    let handler = parse(b"\x1b[-@");
    match &handler.events[0] {
        Event::Csi {
            params,
            secondary,
            action,
            ..
        } => {
            assert!(params.is_empty());
            assert_eq!(*secondary, b'-');
            assert_eq!(*action, b'@');
        }
        _ => unreachable!(),
    }

    let handler = parse(b"\x1b[2-3@y");
    assert_eq!(
        handler.normalized(),
        vec![
            Event::Error("Invalid character in CSI: 3 (0x33), ignoring the sequence".into()),
            Event::Draw("@y".into()),
        ]
    );
}

// 12
#[test]
fn csi_max_params_behaviour() {
    let mut input = Vec::new();
    input.extend_from_slice(b"\x1b[");
    for _ in 0..255 {
        input.extend_from_slice(b"1;");
    }
    input.extend_from_slice(b"1:2m\x1b[31m");
    let handler = parse(&input);
    assert_eq!(
        handler.errors(),
        vec!["CSI escape code has too many parameters, ignoring it"]
    );
    let events = csi_events(&handler);
    assert_eq!(events.len(), 1);
    match events[0] {
        Event::Csi { params, .. } => assert_eq!(params, &vec![31]),
        _ => unreachable!(),
    }

    let mut input = Vec::new();
    input.extend_from_slice(b"\x1b[");
    for _ in 0..256 {
        input.extend_from_slice(b"1;");
    }
    input.push(b'm');
    let handler = parse(&input);
    assert!(handler.errors().is_empty());
    let events = csi_events(&handler);
    assert_eq!(events.len(), 1);
    match events[0] {
        Event::Csi { params, .. } => {
            assert_eq!(params.len(), 256);
            assert!(params.iter().all(|&p| p == 1));
        }
        _ => unreachable!(),
    }
}

// 13
#[test]
fn csi_too_long_reports_and_recovers() {
    let mut input = Vec::with_capacity(MAX_ESCAPE_CODE_LENGTH + 4);
    input.extend_from_slice(b"\x1b[");
    input.resize(input.len() + MAX_ESCAPE_CODE_LENGTH + 1, b'1');
    let mut parser = Parser::new();
    let mut handler = RecordingHandler::default();
    parser.parse(&input, &mut handler);
    assert_eq!(
        handler.errors(),
        vec!["CSI escape too long ignoring and truncating"]
    );
    assert_eq!(parser.state(), kitty_vt::VteState::Normal);
    parser.parse(b"m", &mut handler);
    assert_eq!(handler.draw_text(), "m");
}

// 14
#[test]
fn osc_titles_bel_st_and_split_across_calls() {
    let mut parser = Parser::new();
    let mut handler = RecordingHandler::default();
    parser.parse(b"a\x1b]2;some title", &mut handler);
    assert_eq!(handler.normalized(), vec![Event::Draw("a".into())]);
    parser.parse(b" full\x1b\\", &mut handler);
    assert_eq!(
        handler.normalized(),
        vec![
            Event::Draw("a".into()),
            Event::Osc(b"2;some title full".to_vec())
        ]
    );

    let handler = parse(b"\x1b]2;x\\ryz\x07bcde");
    assert_eq!(
        handler.normalized(),
        vec![Event::Osc(b"2;x\\ryz".to_vec()), Event::Draw("bcde".into())]
    );

    let handler = parse(b"1\x1b]ab\x072");
    assert_eq!(
        handler.normalized(),
        vec![
            Event::Draw("1".into()),
            Event::Osc(b"ab".to_vec()),
            Event::Draw("2".into())
        ]
    );

    let handler = parse(b"\x1b]2;a\x1bb\x1b\\");
    assert_eq!(handler.events, vec![Event::Osc(b"2;a\x1bb".to_vec())]);

    let handler = parse(b"\x1b]2;a\x1b\x1b\\");
    assert_eq!(handler.events, vec![Event::Osc(b"2;a\x1b".to_vec())]);

    let handler = parse(b"\x1b]\x07");
    assert_eq!(handler.events, vec![Event::Osc(vec![])]);
}

// 15
#[test]
fn osc_split_at_every_byte() {
    let data = b"\x1b]2;some title\x1b\\after";
    let expected = parse(data).normalized();
    for split in 0..=data.len() {
        assert_eq!(
            parse_in_chunks(data, &[split]).normalized(),
            expected,
            "failed for split at {split}"
        );
    }
}

// 16
#[test]
fn osc_52_length_limit_chunks() {
    let mut data = Vec::new();
    data.extend_from_slice(b"\x1b]52;p;");
    data.extend(std::iter::repeat_n(b'A', MAX_ESCAPE_CODE_LENGTH));
    let mut parser = Parser::new();
    let mut handler = RecordingHandler::default();
    parser.parse(&data, &mut handler);
    assert_eq!(handler.events.len(), 1);
    match &handler.events[0] {
        Event::OscExtended(payload) => {
            assert_eq!(payload.len(), MAX_ESCAPE_CODE_LENGTH + 4);
            assert!(payload.starts_with(b"52;p;"));
            assert!(payload.iter().all(|&b| b == b'A'
                || b == b'5'
                || b == b'2'
                || b == b';'
                || b == b'p'));
        }
        other => panic!("unexpected event: {other:?}"),
    }
    parser.parse(b"XYZ\x07", &mut handler);
    assert_eq!(handler.events.len(), 2);
    assert_eq!(handler.events[1], Event::Osc(b"52;;AXYZ".to_vec()));
}

// 17
#[test]
fn apc_pm_sos_dispatch_and_splits() {
    let handler = parse(b"a\x1b_+\\+\x1b\\bcde");
    assert_eq!(
        handler.normalized(),
        vec![
            Event::Draw("a".into()),
            Event::Apc(b"+\\+".to_vec()),
            Event::Draw("bcde".into())
        ]
    );

    let handler = parse(b"a\x1b^+\\+\x1b\\bcde");
    assert_eq!(
        handler.normalized(),
        vec![
            Event::Draw("a".into()),
            Event::Pm(b"+\\+".to_vec()),
            Event::Draw("bcde".into())
        ]
    );

    let handler = parse(b"a\x1bX+\\+\x1b\\bcde");
    assert_eq!(
        handler.normalized(),
        vec![
            Event::Draw("a".into()),
            Event::Sos(b"+\\+".to_vec()),
            Event::Draw("bcde".into())
        ]
    );

    let handler = parse(b"\x1b_ab\x07c");
    assert_eq!(
        handler.normalized(),
        vec![Event::Apc(b"ab".to_vec()), Event::Draw("c".into())]
    );

    for short in [
        &b"\x1b_\x1b\\"[..],
        &b"\x1b^\x1b\\"[..],
        &b"\x1bX\x1b\\"[..],
        &b"\x1b_a\x1b\\"[..],
        &b"\x1b^a\x1b\\"[..],
        &b"\x1bXa\x1b\\"[..],
    ] {
        let handler = parse(short);
        assert!(handler.events.is_empty(), "unexpected events for {short:?}");
    }

    let handler = parse(b"\x1b]\x07");
    assert_eq!(handler.events, vec![Event::Osc(vec![])]);

    let data = b"\x1b_hello world\x1b\\x";
    let expected = parse(data).normalized();
    for split in 0..=data.len() {
        assert_eq!(parse_in_chunks(data, &[split]).normalized(), expected);
    }
}

// 18
#[test]
fn dcs_dispatch_receives_opaque_payload() {
    let handler = parse(b"a\x1bP+qkind\x1b\\bcde");
    assert_eq!(
        handler.normalized(),
        vec![
            Event::Draw("a".into()),
            Event::Dcs(b"+qkind".to_vec()),
            Event::Draw("bcde".into()),
        ]
    );

    let handler = parse(b"\x1bP@kitty-cmd{abc\x1b\\");
    assert_eq!(handler.events, vec![Event::Dcs(b"@kitty-cmd{abc".to_vec())]);

    let handler = parse(b"\x1bP=1s\x1b\\");
    assert_eq!(handler.events, vec![Event::Dcs(b"=1s".to_vec())]);

    let handler = parse(b"\x1bP$q q\x1b\\");
    assert_eq!(handler.events, vec![Event::Dcs(b"$q q".to_vec())]);

    let handler = parse(b"\x1bP\x1b\\");
    assert!(handler.events.is_empty());

    let data = b"\x1bP=1sdata\x1b\\after";
    let expected = parse(data).normalized();
    for split in 0..=data.len() {
        assert_eq!(parse_in_chunks(data, &[split]).normalized(), expected);
    }
}

// 19
#[test]
fn dcs_payload_is_not_header_parsed() {
    let handler = parse(b"\x1bP1;2:3;4srest\x1b\\");
    assert_eq!(handler.events, vec![Event::Dcs(b"1;2:3;4srest".to_vec())]);
}

// 20
#[test]
fn c0_and_c1_handling() {
    assert_eq!(parse(b"\x84\x85").draw_text(), "\u{fffd}\u{fffd}");
    assert_eq!(parse("\u{84}\u{85}".as_bytes()).draw_text(), "\u{84}\u{85}");
    assert_eq!(parse(b"a\x00\x18\x1ab").draw_text(), "ab");

    let handler = parse(b"\x1b\x18a");
    assert_eq!(
        handler.normalized(),
        vec![
            Event::Error("Unknown char after ESC: 0x18".into()),
            Event::Draw("a".into())
        ]
    );

    let handler = parse(b"\x1b_ab\x18\x1a\x1b\\");
    assert_eq!(handler.events, vec![Event::Apc(b"ab\x18\x1a".to_vec())]);
}

// 21
#[test]
fn incomplete_utf8_then_escape_is_replaced() {
    let mut parser = Parser::new();
    let mut handler = RecordingHandler::default();
    parser.parse(b"\xf0\x9f\x98", &mut handler);
    assert!(handler.events.is_empty());
    parser.parse(b"\x1b\x1b%a", &mut handler);
    assert_eq!(
        handler.normalized(),
        vec![
            Event::Draw("\u{fffd}".into()),
            Event::Error("Unknown char after ESC: 0x1b".into()),
            Event::Draw("%a".into()),
        ]
    );
}

// 22
#[test]
fn reset_discards_partial_state() {
    let mut parser = Parser::new();
    let mut handler = RecordingHandler::default();
    parser.parse(b"\x1b[12", &mut handler);
    assert_eq!(parser.state(), kitty_vt::VteState::Csi);
    parser.reset();
    assert_eq!(parser.state(), kitty_vt::VteState::Normal);
    parser.parse(b"ok", &mut handler);
    assert_eq!(handler.draw_text(), "ok");
    assert!(csi_events(&handler).is_empty());
}

// 23
#[test]
fn parser_can_be_used_as_default() {
    let mut parser = Parser::default();
    let mut handler = RecordingHandler::default();
    parser.parse(b"x", &mut handler);
    assert_eq!(handler.draw_text(), "x");
}

// 24
#[test]
fn csi_params_accessors() {
    struct AccessorCheck;
    impl Handler for AccessorCheck {
        fn csi_dispatch(&mut self, params: &CsiParams, intermediates: &[u8], action: u8) {
            assert_eq!(action, b'm');
            assert!(intermediates.is_empty());
            assert_eq!(params.len(), 5);
            assert!(!params.is_empty());
            assert_eq!(params.get(0), Some(38));
            assert_eq!(params.get(5), None);
            assert_eq!(params.param_or(0, 7), 38);
            assert_eq!(params.param_or(5, 7), 7);
            assert_eq!(params.max_params_constraint(5), Ok(()));
            assert_eq!(params.max_params_constraint(4), Err(5));
            assert_eq!(params.iter_flat().collect::<Vec<_>>(), vec![38, 2, 1, 2, 3]);
            assert_eq!(params.subparams(0), Some(&[38, 2, 1, 2, 3][..]));
            assert_eq!(params.subparams(1), None);
            assert!(params.is_sub_param(1));
            assert!(!params.is_sub_param(0));
        }
    }
    let mut parser = Parser::new();
    parser.parse(b"\x1b[38:2:1:2:3m", &mut AccessorCheck);
}

// 25
#[test]
fn invalid_utf8_split_at_every_byte() {
    let cases: &[&[u8]] = &[
        b"\"\xe1\x28\xa1\"",
        b"\"\xef\x93\x94\x95\"",
        b"\"\x80\xbf\x80\"",
        b"\"\xf0\x80\x80\x80\"",
        b"\"\xff\xfe\"",
        b"a\xf5xyz\xc0\x80b",
        b"\xed\xa0\x80\xed\xb0\x80",
    ];
    for data in cases {
        let expected = parse(data).normalized();
        for split in 0..=data.len() {
            assert_eq!(
                parse_in_chunks(data, &[split]).normalized(),
                expected,
                "failed for {data:?} split at {split}"
            );
        }
    }
}
