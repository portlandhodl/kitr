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

/// Payload offered by a local drag source.
///
/// A source can carry plain text, a list of local files, or both. File lists
/// are encoded as an RFC 2483 `text/uri-list`; text is offered under the usual
/// UTF-8 MIME types so that both native applications and terminals accept it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DragSourceData {
    pub text: Option<String>,
    pub paths: Vec<String>,
}

impl DragSourceData {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            paths: Vec::new(),
        }
    }

    pub fn from_paths(paths: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            text: None,
            paths: paths.into_iter().map(Into::into).collect(),
        }
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    pub fn with_paths(mut self, paths: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.paths = paths.into_iter().map(Into::into).collect();
        self
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_none() && self.paths.is_empty()
    }

    /// MIME types offered by this source, in preference order.
    pub fn targets(&self) -> Vec<String> {
        let mut targets = Vec::new();
        if !self.paths.is_empty() {
            targets.push("text/uri-list".to_owned());
        }
        if self.text.is_some() {
            for mime in [
                "text/plain;charset=utf-8",
                "text/plain",
                "UTF8_STRING",
                "STRING",
            ] {
                targets.push(mime.to_owned());
            }
        }
        targets
    }

    /// Alias for [`DragSourceData::targets`].
    pub fn mimes(&self) -> Vec<String> {
        self.targets()
    }

    /// Data for a named target, if this source offers it.
    pub fn target_data(&self, target: &str) -> Option<Vec<u8>> {
        if target.eq_ignore_ascii_case("TARGETS") {
            let mut payload = String::from("TARGETS\n");
            payload.push_str(&self.targets().join("\n"));
            if !self.targets().is_empty() {
                payload.push('\n');
            }
            return Some(payload.into_bytes());
        }
        if target.eq_ignore_ascii_case("text/uri-list") {
            if self.paths.is_empty() {
                return None;
            }
            return Some(build_uri_list(&self.paths).into_bytes());
        }
        if target.eq_ignore_ascii_case("UTF8_STRING")
            || target.eq_ignore_ascii_case("STRING")
            || target.eq_ignore_ascii_case("text/plain")
            || target
                .get(..10)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("text/plain"))
                && target.get(10..).is_some_and(|rest| rest.starts_with(';'))
        {
            return self.text.as_ref().map(|text| text.as_bytes().to_vec());
        }
        None
    }
}

impl From<&str> for DragSourceData {
    fn from(text: &str) -> Self {
        Self::from_text(text)
    }
}

impl From<String> for DragSourceData {
    fn from(text: String) -> Self {
        Self::from_text(text)
    }
}

impl From<&String> for DragSourceData {
    fn from(text: &String) -> Self {
        Self::from_text(text.clone())
    }
}

impl From<Vec<String>> for DragSourceData {
    fn from(paths: Vec<String>) -> Self {
        Self::from_paths(paths)
    }
}

impl From<&[String]> for DragSourceData {
    fn from(paths: &[String]) -> Self {
        Self::from_paths(paths.iter().cloned())
    }
}

impl From<&Vec<String>> for DragSourceData {
    fn from(paths: &Vec<String>) -> Self {
        Self::from_paths(paths.iter().cloned())
    }
}

impl From<&DragSourceData> for DragSourceData {
    fn from(data: &DragSourceData) -> Self {
        data.clone()
    }
}

/// Encode local paths as an RFC 2483 `text/uri-list` payload.
pub fn build_uri_list(paths: &[String]) -> String {
    let mut payload = String::new();
    for path in paths {
        payload.push_str(&path_to_uri(path));
        payload.push_str("\r\n");
    }
    payload
}

fn path_to_uri(path: &str) -> String {
    let mut uri = String::from("file://");
    for byte in path.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(*byte as char)
            }
            other => {
                use std::fmt::Write as _;
                let _ = write!(uri, "%{other:02X}");
            }
        }
    }
    uri
}

/// State of a local drag source, independent of the display-server protocol.
///
/// The state machine is intentionally small: a source is begun with a set of
/// targets and a serial, optionally gains an accepted target, and then either
/// finishes (dropped) or is cancelled. Both the Wayland `wl_data_source` and
/// the X11 XDND source paths drive the same state.
#[derive(Clone, Debug, Default)]
pub struct DragSourceState {
    active: bool,
    targets: Vec<String>,
    chosen: Option<String>,
    accepted: bool,
    dropped: bool,
    finished: bool,
    cancelled: bool,
    serial: u32,
}

impl DragSourceState {
    /// Begin a source. Returns `false` if one was already active.
    pub fn begin<I, S>(&mut self, targets: I, serial: u32) -> bool
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        if self.active {
            return false;
        }
        self.active = true;
        self.targets = targets.into_iter().map(Into::into).collect();
        self.chosen = None;
        self.accepted = false;
        self.dropped = false;
        self.finished = false;
        self.cancelled = false;
        self.serial = serial;
        true
    }

    pub fn is_active(&self) -> bool {
        self.active && !self.finished && !self.cancelled
    }

    pub fn targets(&self) -> &[String] {
        &self.targets
    }

    pub fn chosen_target(&self) -> Option<&str> {
        self.chosen.as_deref()
    }

    pub fn set_chosen_target(&mut self, target: Option<&str>) -> Option<String> {
        self.chosen = target.and_then(|name| {
            self.targets
                .iter()
                .find(|offered| offered.eq_ignore_ascii_case(name))
                .cloned()
        });
        self.chosen.clone()
    }

    pub fn serial(&self) -> u32 {
        self.serial
    }

    pub fn set_serial(&mut self, serial: u32) {
        self.serial = serial;
    }

    pub fn is_accepted(&self) -> bool {
        self.accepted
    }

    pub fn set_accepted(&mut self, accepted: bool) {
        self.accepted = accepted;
    }

    pub fn has_dropped(&self) -> bool {
        self.dropped
    }

    pub fn mark_dropped(&mut self) -> bool {
        if !self.is_active() || self.dropped {
            return false;
        }
        self.dropped = true;
        self.accepted = true;
        true
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Finish the drag. Returns `true` when a source was active.
    pub fn finish(&mut self, accepted: bool) -> bool {
        if !self.is_active() {
            return false;
        }
        self.accepted = accepted;
        self.finished = true;
        self.active = false;
        true
    }

    /// Cancel the drag. Returns `true` when a source was active.
    pub fn cancel(&mut self) -> bool {
        if !self.is_active() {
            return false;
        }
        self.cancelled = true;
        self.accepted = false;
        self.active = false;
        true
    }

    pub fn reset(&mut self) {
        *self = Self::default();
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
    fn drag_source_data_encodes_uri_lists_and_text() {
        let data = DragSourceData::from_paths(["/tmp/a b.txt", "/tmp/café.txt"]).with_text("hello");
        let targets = data.targets();
        assert!(targets.contains(&"text/uri-list".to_owned()));
        assert!(targets.contains(&"UTF8_STRING".to_owned()));
        let uri = data.target_data("text/uri-list").expect("uri list");
        let uri = String::from_utf8(uri).expect("utf8");
        assert!(uri.contains("file:///tmp/a%20b.txt"));
        assert!(uri.contains("file:///tmp/caf%C3%A9.txt"));
        assert_eq!(
            data.target_data("UTF8_STRING").as_deref(),
            Some(b"hello".as_slice())
        );
        assert!(DragSourceData::from_text("x")
            .target_data("text/uri-list")
            .is_none());
    }

    #[test]
    fn drag_source_state_machine_finishes_and_cancels() {
        let mut source = DragSourceState::default();
        assert!(source.begin(vec!["text/uri-list".to_owned()], 7));
        assert!(source.is_active());
        assert_eq!(source.serial(), 7);
        assert!(
            !source.begin(Vec::<String>::new(), 8),
            "a second begin is refused"
        );
        assert_eq!(
            source.set_chosen_target(Some("TEXT/URI-LIST")),
            Some("text/uri-list".to_owned())
        );
        assert!(source.mark_dropped());
        assert!(source.has_dropped());
        assert!(source.is_accepted());
        assert!(source.finish(true));
        assert!(source.is_finished());
        assert!(!source.is_active());
        assert!(!source.finish(true));

        let mut source = DragSourceState::default();
        assert!(source.begin(vec!["UTF8_STRING".to_owned()], 1));
        assert!(source.cancel());
        assert!(source.is_cancelled());
        assert!(!source.is_active());
        assert!(!source.cancel());
    }

    #[test]
    fn drag_source_ignores_unoffered_targets() {
        let mut source = DragSourceState::default();
        source.begin(vec!["UTF8_STRING".to_owned()], 1);
        assert_eq!(source.set_chosen_target(Some("image/png")), None);
        assert_eq!(source.chosen_target(), None);
    }

    #[test]
    fn drag_session_ignores_accept_before_enter() {
        let mut drag = DragSession::default();
        assert_eq!(drag.accept(None), None);
        assert!(!drag.is_accepted());
    }
}
