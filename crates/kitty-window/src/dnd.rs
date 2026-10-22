// SPDX-License-Identifier: GPL-3.0-or-later
//! Platform-neutral drag-and-drop helpers.
//!
//! The X11 and Wayland backends both need the same small pieces of policy:
//! which offered MIME target to prefer, how to turn an RFC 2483 `text/uri-list`
//! payload into local paths, and how an in-progress drag changes state. Keeping
//! those rules here means they can be unit tested without a display server.

use std::fmt;

/// The kind of payload a drop target can consume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropTarget {
    UriList,
    Text,
}

impl DropTarget {
    /// The MIME type to request from the drag source.
    pub fn mime(self) -> &'static str {
        match self {
            Self::UriList => "text/uri-list",
            Self::Text => "text/plain;charset=utf-8",
        }
    }

    pub fn from_mime(name: &str) -> Option<Self> {
        let name = name.trim();
        if name.eq_ignore_ascii_case("text/uri-list") {
            Some(Self::UriList)
        } else if name.eq_ignore_ascii_case("UTF8_STRING")
            || name.eq_ignore_ascii_case("STRING")
            || name.eq_ignore_ascii_case("text/plain")
            || name
                .get(..10)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("text/plain"))
                && name.get(10..).is_some_and(|rest| rest.starts_with(';'))
        {
            Some(Self::Text)
        } else {
            None
        }
    }
}

/// Pick the best payload offered by a drag source.
///
/// File lists win over plain text so that dropping a file onto a terminal
/// inserts the path rather than a detached text representation.
pub fn preferred_drop_target<'a>(mimes: impl IntoIterator<Item = &'a str>) -> Option<DropTarget> {
    let mut text = false;
    for mime in mimes {
        match DropTarget::from_mime(mime) {
            Some(DropTarget::UriList) => return Some(DropTarget::UriList),
            Some(DropTarget::Text) => text = true,
            None => {}
        }
    }
    text.then_some(DropTarget::Text)
}

/// Parse an RFC 2483 `text/uri-list` payload into local file paths.
///
/// Comment lines (`#`), blank lines and non-local URIs are ignored. Percent
/// escapes are decoded as UTF-8, which is how desktops encode spaces and
/// non-ASCII characters in `file://` URIs.
pub fn parse_uri_list(payload: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in payload.split(['\r', '\n']) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(path) = file_uri_to_path(line) {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    paths
}

fn file_uri_to_path(uri: &str) -> Option<String> {
    let rest = uri
        .strip_prefix("file://")
        .or_else(|| uri.strip_prefix("FILE://"))?;
    let path = if rest.starts_with('/') {
        rest
    } else {
        let (authority, _) = rest.split_once('/')?;
        if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
            return None;
        }
        &rest[authority.len()..]
    };
    let decoded = percent_decode(path);
    Some(String::from_utf8_lossy(&decoded).into_owned())
}

fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = hex_value(bytes[index + 1]);
            let low = hex_value(bytes[index + 2]);
            if let (Some(high), Some(low)) = (high, low) {
                out.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    out
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// State of an incoming drag, independent of the display-server protocol.
#[derive(Clone, Debug, Default)]
pub struct DragSession {
    active: bool,
    targets: Vec<String>,
    chosen: Option<String>,
    rejected: bool,
    position: (f64, f64),
    serial: u32,
}

impl DragSession {
    /// Start a session from the MIME types advertised by the source.
    pub fn begin(&mut self, targets: Vec<String>) {
        self.active = true;
        self.rejected = false;
        self.position = (0.0, 0.0);
        self.serial = 0;
        self.targets = targets;
        self.chosen = self.preferred_offered();
    }

    fn preferred_offered(&self) -> Option<String> {
        let preferred = preferred_drop_target(self.targets.iter().map(String::as_str))?;
        self.targets
            .iter()
            .find(|name| DropTarget::from_mime(name) == Some(preferred))
            .cloned()
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn targets(&self) -> &[String] {
        &self.targets
    }

    pub fn chosen_target(&self) -> Option<&str> {
        self.chosen.as_deref()
    }

    pub fn is_accepted(&self) -> bool {
        self.active && !self.rejected && self.chosen.is_some()
    }

    pub fn is_rejected(&self) -> bool {
        self.rejected
    }

    pub fn position(&self) -> (f64, f64) {
        self.position
    }

    pub fn set_position(&mut self, x: f64, y: f64) {
        self.position = (x, y);
    }

    pub fn serial(&self) -> u32 {
        self.serial
    }

    pub fn set_serial(&mut self, serial: u32) {
        self.serial = serial;
    }

    /// Accept the drag, optionally with an explicit MIME type.
    ///
    /// When `target` is `None` the preferred advertised target is used. Returns
    /// the selected target, or `None` if the request cannot be satisfied.
    pub fn accept(&mut self, target: Option<&str>) -> Option<String> {
        if !self.active {
            return None;
        }
        let chosen = match target {
            Some(name) => self
                .targets
                .iter()
                .find(|offered| offered.eq_ignore_ascii_case(name))
                .cloned(),
            None => self.preferred_offered(),
        };
        self.chosen = chosen.clone();
        self.rejected = chosen.is_none();
        chosen
    }

    /// Reject the drag and remember that no target is selected.
    pub fn reject(&mut self) {
        self.rejected = true;
        self.chosen = None;
    }

    /// End the drag. Returns whether a drag was in progress.
    pub fn leave(&mut self) -> bool {
        let was_active = self.active;
        self.active = false;
        self.targets.clear();
        self.chosen = None;
        self.rejected = false;
        self.position = (0.0, 0.0);
        self.serial = 0;
        was_active
    }
}

impl fmt::Display for DropTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.mime())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_list_decodes_paths_and_ignores_comments() {
        let paths = parse_uri_list(
            "# a comment\r\nfile:///tmp/a.txt\r\nfile:///tmp/a.txt\r\nfile:///tmp/with%20space.txt\r\n",
        );
        assert_eq!(
            paths,
            vec!["/tmp/a.txt".to_owned(), "/tmp/with space.txt".to_owned()]
        );
    }

    #[test]
    fn uri_list_decodes_utf8_and_localhost_authority() {
        let paths = parse_uri_list("file://localhost/tmp/caf%C3%A9.txt\nfile://remote/tmp/x\n");
        assert_eq!(paths, vec!["/tmp/café.txt".to_owned()]);
    }

    #[test]
    fn uri_list_accepts_lf_and_cr_separators() {
        let paths = parse_uri_list("file:///one\rfile:///two\n\n");
        assert_eq!(
            paths,
            vec!["/one".to_owned(), "/two".to_owned()],
            "both CR-only and LF separators are valid"
        );
    }

    #[test]
    fn target_preference_prefers_uri_lists_over_text() {
        assert_eq!(
            preferred_drop_target(["text/plain;charset=utf-8", "text/uri-list"]),
            Some(DropTarget::UriList)
        );
        assert_eq!(
            preferred_drop_target(["image/png", "UTF8_STRING"]),
            Some(DropTarget::Text)
        );
        assert_eq!(preferred_drop_target(["image/png"]), None);
        assert_eq!(
            DropTarget::from_mime("text/plain;charset=utf-8"),
            Some(DropTarget::Text)
        );
    }

    #[test]
    fn drag_session_accepts_rejects_and_leaves() {
        let mut drag = DragSession::default();
        assert!(!drag.is_active());
        drag.begin(vec!["text/plain;charset=utf-8".to_owned()]);
        assert!(drag.is_active());
        assert_eq!(drag.chosen_target(), Some("text/plain;charset=utf-8"));
        assert!(drag.is_accepted());

        drag.set_position(12.0, 34.0);
        assert_eq!(drag.position(), (12.0, 34.0));
        drag.reject();
        assert!(drag.is_rejected());
        assert!(!drag.is_accepted());
        assert_eq!(
            drag.accept(None),
            Some("text/plain;charset=utf-8".to_owned())
        );
        assert!(!drag.is_rejected());

        assert_eq!(drag.accept(Some("image/png")), None);
        assert!(drag.is_rejected());
        assert!(drag.leave());
        assert!(!drag.is_active());
        assert!(!drag.leave());
    }

    #[test]
    fn drag_session_keeps_the_actual_offered_text_mime() {
        let mut drag = DragSession::default();
        drag.begin(vec!["UTF8_STRING".to_owned(), "image/png".to_owned()]);
        assert_eq!(drag.chosen_target(), Some("UTF8_STRING"));
        assert_eq!(
            drag.accept(Some("utf8_string")),
            Some("UTF8_STRING".to_owned())
        );
    }

    #[test]
    fn drag_session_ignores_accept_before_enter() {
        let mut drag = DragSession::default();
        assert_eq!(drag.accept(None), None);
        assert!(!drag.is_accepted());
    }
}
