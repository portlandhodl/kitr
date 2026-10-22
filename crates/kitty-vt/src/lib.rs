// SPDX-License-Identifier: GPL-3.0-or-later
//! VT escape code parser, ported from `kitty/vt-parser.c`.
//!
//! The parser turns arbitrary byte chunks into [`Handler`] callbacks. It is
//! fully incremental: input can be split anywhere, including inside a UTF-8
//! sequence, an escape sequence, CSI parameters or an OSC/DCS string.
//!
//! ```
//! use kitty_vt::{Handler, Parser};
//!
//! #[derive(Default)]
//! struct Title(Option<String>);
//!
//! impl Handler for Title {
//!     fn osc_dispatch(&mut self, data: &[u8]) {
//!         if let Some(rest) = data.strip_prefix(b"2;") {
//!             self.0 = Some(String::from_utf8_lossy(rest).into_owned());
//!         }
//!     }
//! }
//!
//! let mut parser = Parser::new();
//! let mut title = Title::default();
//! parser.parse(b"\x1b]2;hello", &mut title);
//! parser.parse(b"\x1b\\", &mut title);
//! assert_eq!(title.0.as_deref(), Some("hello"));
//! ```

pub use kitty_core as core;
pub use kitty_core::CharType;

pub mod charsets;
pub mod params;
pub mod parser;

pub use charsets::translation_table;
pub use params::{CsiParams, MAX_CSI_PARAMS};
pub use parser::{Handler, Parser, VteState, BUFFER_SIZE, MAX_ESCAPE_CODE_LENGTH};
