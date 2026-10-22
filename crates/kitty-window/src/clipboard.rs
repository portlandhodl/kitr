// SPDX-License-Identifier: GPL-3.0-or-later
//! Clipboard selection state shared by the X11 backend and unit tests.
//!
//! The state machine deliberately knows nothing about Xlib. It tracks the
//! targets we currently offer, outstanding conversions keyed by the X11
//! property atom used for the reply, and per-request results. The X11 layer
//! translates SelectionRequest/SelectionNotify events into these operations.

use std::collections::HashMap;
use std::fmt;

use crate::types::Selection;

/// Opaque handle for an asynchronous clipboard read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClipboardToken(u64);

impl ClipboardToken {
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// Result of a clipboard read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardReply {
    Text(String),
    Targets(Vec<String>),
    Data { target: String, data: Vec<u8> },
}

impl ClipboardReply {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// Clipboard-specific error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardError {
    Timeout,
    NoOwner,
    ConversionFailed,
    UnsupportedTarget(String),
    BadReply(String),
    WindowClosed,
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => f.write_str("timed out waiting for clipboard data"),
            Self::NoOwner => f.write_str("clipboard selection has no owner"),
            Self::ConversionFailed => f.write_str("clipboard conversion failed"),
            Self::UnsupportedTarget(target) => write!(f, "unsupported clipboard target: {target}"),
            Self::BadReply(message) => write!(f, "bad clipboard reply: {message}"),
            Self::WindowClosed => f.write_str("window closed while reading the clipboard"),
        }
    }
}

impl std::error::Error for ClipboardError {}

/// A single offered MIME target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardTarget {
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
struct Offer {
    targets: Vec<ClipboardTarget>,
}

#[derive(Clone, Debug)]
struct Pending {
    selection: Selection,
    target: String,
    property: u64,
    reply: Option<Result<ClipboardReply, ClipboardError>>,
}

/// Async clipboard state machine.
#[derive(Clone, Debug, Default)]
pub struct ClipboardState {
    offers: [Option<Offer>; 2],
    pending: HashMap<u64, Pending>,
    next_token: u64,
}

impl ClipboardState {
    fn index(selection: Selection) -> usize {
        match selection {
            Selection::Clipboard => 0,
            Selection::Primary => 1,
        }
    }

    /// Replace the targets offered for `selection`.
    pub fn set_targets(&mut self, selection: Selection, targets: Vec<ClipboardTarget>) {
        self.offers[Self::index(selection)] = Some(Offer { targets });
    }

    /// Offer UTF-8 text under the standard text targets.
    pub fn set_text(&mut self, selection: Selection, text: &[u8]) {
        let names = [
            "UTF8_STRING",
            "STRING",
            "text/plain",
            "text/plain;charset=utf-8",
        ];
        let targets = names
            .iter()
            .map(|name| ClipboardTarget {
                name: (*name).to_owned(),
                data: text.to_vec(),
            })
            .collect();
        self.set_targets(selection, targets);
    }

    pub fn clear(&mut self, selection: Selection) {
        self.offers[Self::index(selection)] = None;
    }

    pub fn has_offer(&self, selection: Selection) -> bool {
        self.offers[Self::index(selection)].is_some()
    }

    pub fn targets(&self, selection: Selection) -> &[ClipboardTarget] {
        self.offers[Self::index(selection)]
            .as_ref()
            .map(|offer| offer.targets.as_slice())
            .unwrap_or(&[])
    }

    /// Data for a named target, if offered.
    pub fn target_data(&self, selection: Selection, target: &str) -> Option<&[u8]> {
        self.offers[Self::index(selection)]
            .as_ref()?
            .targets
            .iter()
            .find(|entry| entry.name == target)
            .map(|entry| entry.data.as_slice())
    }

    /// Names to advertise for TARGETS (including TARGETS itself).
    pub fn target_names(&self, selection: Selection) -> Vec<String> {
        let mut names = vec!["TARGETS".to_owned()];
        names.extend(
            self.targets(selection)
                .iter()
                .map(|target| target.name.clone()),
        );
        names
    }

    /// Begin an async conversion. `property` is the X11 property atom the
    /// reply will be written to; it also identifies the pending request.
    pub fn begin(
        &mut self,
        selection: Selection,
        target: impl Into<String>,
        property: u64,
    ) -> ClipboardToken {
        self.next_token = self.next_token.wrapping_add(1).max(1);
        let token = ClipboardToken(self.next_token);
        self.pending.insert(
            token.0,
            Pending {
                selection,
                target: target.into(),
                property,
                reply: None,
            },
        );
        token
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Complete the request matching an X11 property atom.
    pub fn complete(
        &mut self,
        property: u64,
        reply: Result<ClipboardReply, ClipboardError>,
    ) -> Option<ClipboardToken> {
        let token = self
            .pending
            .iter()
            .find_map(|(token, pending)| (pending.property == property).then_some(*token))?;
        if let Some(pending) = self.pending.get_mut(&token) {
            pending.reply = Some(reply);
        }
        Some(ClipboardToken(token))
    }

    /// Complete the oldest pending request matching a selection and target.
    /// This is used when X11 refuses a conversion by returning `property=None`
    /// and therefore gives us no property atom to correlate with.
    pub fn complete_matching(
        &mut self,
        selection: Selection,
        target: &str,
        reply: Result<ClipboardReply, ClipboardError>,
    ) -> Option<ClipboardToken> {
        let token = self.pending.iter().find_map(|(token, pending)| {
            (pending.reply.is_none() && pending.selection == selection && pending.target == target)
                .then_some(*token)
        })?;
        if let Some(pending) = self.pending.get_mut(&token) {
            pending.reply = Some(reply);
        }
        Some(ClipboardToken(token))
    }

    /// Return a completed result and remove the pending request. Incomplete
    /// requests stay in the map so their result can be collected later.
    pub fn take_result(
        &mut self,
        token: ClipboardToken,
    ) -> Option<Result<ClipboardReply, ClipboardError>> {
        let ready = self
            .pending
            .get(&token.0)
            .map(|pending| pending.reply.is_some())
            .unwrap_or(false);
        if !ready {
            return None;
        }
        self.pending
            .remove(&token.0)
            .and_then(|pending| pending.reply)
    }

    /// Drop a pending request without waiting for a reply.
    pub fn forget(&mut self, token: ClipboardToken) {
        self.pending.remove(&token.0);
    }

    /// Metadata about an outstanding request (selection and target).
    pub fn pending_info(&self, token: ClipboardToken) -> Option<(Selection, &str)> {
        self.pending
            .get(&token.0)
            .map(|pending| (pending.selection, pending.target.as_str()))
    }

    /// Mark every pending request as failed and return their tokens.
    pub fn fail_all(&mut self, error: ClipboardError) -> Vec<ClipboardToken> {
        let tokens: Vec<u64> = self.pending.keys().copied().collect();
        for pending in self.pending.values_mut() {
            pending.reply = Some(Err(error.clone()));
        }
        tokens.into_iter().map(ClipboardToken).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_offer_advertises_standard_targets() {
        let mut state = ClipboardState::default();
        state.set_text(Selection::Clipboard, b"hello");
        assert!(state.has_offer(Selection::Clipboard));
        assert_eq!(
            state.target_data(Selection::Clipboard, "UTF8_STRING"),
            Some(&b"hello"[..])
        );
        assert_eq!(
            state.target_data(Selection::Clipboard, "text/plain"),
            Some(&b"hello"[..])
        );
        let targets = state.target_names(Selection::Clipboard);
        assert_eq!(targets[0], "TARGETS");
        assert!(targets.contains(&"UTF8_STRING".to_owned()));
        assert!(!state.has_offer(Selection::Primary));
    }

    #[test]
    fn async_request_lifecycle() {
        let mut state = ClipboardState::default();
        let token = state.begin(Selection::Primary, "UTF8_STRING", 42);
        assert_eq!(state.pending_count(), 1);
        assert_eq!(state.take_result(token), None);
        let found = state.complete(42, Ok(ClipboardReply::Text("data".into())));
        assert_eq!(found, Some(token));
        assert_eq!(
            state.take_result(token),
            Some(Ok(ClipboardReply::Text("data".into())))
        );
        assert_eq!(state.take_result(token), None);
        assert_eq!(state.pending_count(), 0);
    }

    #[test]
    fn refusal_without_a_property_completes_by_selection_and_target() {
        let mut state = ClipboardState::default();
        let token = state.begin(Selection::Clipboard, "UTF8_STRING", 3);
        let found = state.complete_matching(
            Selection::Clipboard,
            "UTF8_STRING",
            Err(ClipboardError::ConversionFailed),
        );
        assert_eq!(found, Some(token));
        assert_eq!(
            state.take_result(token),
            Some(Err(ClipboardError::ConversionFailed))
        );
    }

    #[test]
    fn unmatched_completion_is_ignored() {
        let mut state = ClipboardState::default();
        let token = state.begin(Selection::Clipboard, "TARGETS", 7);
        assert_eq!(state.complete(8, Ok(ClipboardReply::Targets(vec![]))), None);
        assert_eq!(state.take_result(token), None);
        assert_eq!(state.pending_count(), 1);
    }

    #[test]
    fn failure_propagates_to_all_pending() {
        let mut state = ClipboardState::default();
        let first = state.begin(Selection::Clipboard, "UTF8_STRING", 1);
        let second = state.begin(Selection::Primary, "TARGETS", 2);
        let tokens = state.fail_all(ClipboardError::WindowClosed);
        assert_eq!(tokens.len(), 2);
        assert_eq!(
            state.take_result(first),
            Some(Err(ClipboardError::WindowClosed))
        );
        assert_eq!(
            state.take_result(second),
            Some(Err(ClipboardError::WindowClosed))
        );
    }
}
