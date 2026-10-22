// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/hyperlink.c and kitty/hyperlink.h
// Copyright (C) 2020-2024 Kovid Goyal <kovid at kovidgoyal.net>

//! Hyperlink pool and per-cell hyperlink helpers.
//!
//! Kitty stores a small integer id in every cell and keeps the (id, url) pair
//! in a pool shared by the main and alternate screens. Id 0 means "no link".
//! The key stored in the pool is `id:url`, exactly like the C code, so the
//! id part can be recovered when serializing to OSC 8.

use std::collections::{HashMap, HashSet};

use kitty_core::cell::HyperlinkIdType;

/// Kitty's `MAX_ID_LEN` and `MAX_KEY_LEN` truncation behaviour.
const MAX_ID_LEN: usize = 256;
const MAX_KEY_LEN: usize = 2048;

/// One hyperlink pool entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hyperlink {
    /// The `id=` parameter from OSC 8 (may be empty).
    pub id: String,
    /// The URL part. Never empty for a pooled entry.
    pub url: String,
}

impl Hyperlink {
    /// The `id:url` key used by the C pool.
    pub fn key(&self) -> String {
        format!("{}:{}", self.id, self.url)
    }
}

/// Pool of hyperlinks, mimicking `HyperLinkPool` from `hyperlink.c`.
#[derive(Debug, Default)]
pub struct HyperlinkPool {
    items: Vec<Option<Hyperlink>>,
    map: HashMap<String, HyperlinkIdType>,
    adds_since_last_gc: u32,
}

/// How many new links are inserted before a GC is attempted automatically.
pub const HYPERLINK_GC_THRESHOLD: u32 = 8192;
/// Maximum number of links (`HYPERLINK_MAX_NUMBER - 1` in C).
pub const HYPERLINK_MAX_NUMBER: usize = u16::MAX as usize;

impl HyperlinkPool {
    pub fn new() -> Self {
        Self {
            items: vec![None],
            map: HashMap::new(),
            adds_since_last_gc: 0,
        }
    }

    /// Number of allocated ids including id 0.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.len() <= 1
    }

    /// Look up a link by the id stored in a cell.
    pub fn get(&self, id: HyperlinkIdType) -> Option<&Hyperlink> {
        self.items.get(id as usize).and_then(|x| x.as_ref())
    }

    /// `get_hyperlink_for_id(..., only_url=true)`.
    pub fn url_for_id(&self, id: HyperlinkIdType) -> Option<&str> {
        self.get(id).map(|h| h.url.as_str())
    }

    /// `get_hyperlink_for_id(..., only_url=false)`.
    pub fn key_for_id(&self, id: HyperlinkIdType) -> Option<String> {
        self.get(id).map(Hyperlink::key)
    }

    /// `get_id_for_hyperlink()`: allocate or reuse the id for `(id, url)`.
    pub fn get_id(&mut self, id: Option<&str>, url: Option<&str>) -> HyperlinkIdType {
        let Some(url) = url else {
            return 0;
        };
        if url.is_empty() {
            return 0;
        }
        let id = id.unwrap_or("");
        // C uses snprintf("%.*s:%s", MAX_ID_LEN, id ?: "", url), then caps the
        // final key length at MAX_KEY_LEN - 2.
        let mut key = String::with_capacity(id.len().min(MAX_ID_LEN) + 1 + url.len());
        key.push_str(&id.chars().take(MAX_ID_LEN).collect::<String>());
        key.push(':');
        key.push_str(url);
        while key.len() > MAX_KEY_LEN - 2 {
            key.pop();
        }
        if let Some(&existing) = self.map.get(&key) {
            return existing;
        }
        self.adds_since_last_gc += 1;
        // The caller (Screen) performs GC when this threshold is crossed so it
        // can rewrite cell ids at the same time. Here we only make sure we do
        // not overflow the u16 id space.
        if self.items.len() >= HYPERLINK_MAX_NUMBER {
            return 0;
        }
        let new_id = self.items.len() as HyperlinkIdType;
        let (key_part, url_part) = key
            .split_once(':')
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .unwrap_or_else(|| (String::new(), key.clone()));
        self.items.push(Some(Hyperlink {
            id: key_part,
            url: url_part,
        }));
        self.map.insert(key, new_id);
        new_id
    }

    /// True when the automatic-GC threshold has been reached.
    pub fn needs_gc(&self) -> bool {
        self.adds_since_last_gc > HYPERLINK_GC_THRESHOLD
    }

    /// Discard all links (used by a hard reset).
    pub fn clear(&mut self) {
        self.items.truncate(1);
        self.map.clear();
        self.adds_since_last_gc = 0;
    }

    /// Rebuild the pool keeping only `used` ids. Returns an old→new id map.
    ///
    /// This is the Rust equivalent of `_screen_garbage_collect_hyperlink_pool`.
    /// Id 0 always maps to 0 and the first real id is 1.
    pub fn garbage_collect(&mut self, used: &HashSet<HyperlinkIdType>) -> Vec<HyperlinkIdType> {
        let mut map = vec![0; self.items.len()];
        let mut items = vec![None];
        let mut new_map = HashMap::new();
        let mut old_ids: Vec<_> = used.iter().copied().collect();
        old_ids.sort_unstable();
        for old in old_ids {
            if old == 0 || old as usize >= self.items.len() {
                continue;
            }
            let Some(link) = self.items[old as usize].take() else {
                continue;
            };
            let new_id = items.len() as HyperlinkIdType;
            map[old as usize] = new_id;
            new_map.insert(link.key(), new_id);
            items.push(Some(link));
        }
        self.items = items;
        self.map = new_map;
        self.adds_since_last_gc = 0;
        map
    }

    /// Iterate over `(id, link)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (HyperlinkIdType, &Hyperlink)> {
        self.items
            .iter()
            .enumerate()
            .skip(1)
            .filter_map(|(i, x)| x.as_ref().map(|h| (i as HyperlinkIdType, h)))
    }
}

/// A contiguous run of one hyperlink on one visual/range line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HyperlinkSpan {
    /// Range row: negative values are scrollback (`-1` is the newest history
    /// line), non-negative values are screen rows.
    pub y: i64,
    pub x_start: u32,
    /// Exclusive.
    pub x_end: u32,
}

/// Result of [`crate::Screen::hyperlink_at`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HyperlinkAt {
    pub id: HyperlinkIdType,
    pub url: String,
    pub spans: Vec<HyperlinkSpan>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_start_at_one_and_are_reused() {
        let mut p = HyperlinkPool::new();
        assert_eq!(p.get_id(Some("a"), Some("url")), 1);
        assert_eq!(p.get_id(Some("a"), Some("url")), 1);
        assert_eq!(p.get_id(None, Some("url")), 2);
        assert_eq!(p.get_id(Some("a"), Some("")), 0);
        assert_eq!(p.get_id(Some("a"), None), 0);
        assert_eq!(p.url_for_id(1), Some("url"));
        assert_eq!(p.key_for_id(1).as_deref(), Some("a:url"));
    }

    #[test]
    fn gc_remaps_used_ids() {
        let mut p = HyperlinkPool::new();
        let a = p.get_id(Some("a"), Some("a-url"));
        let b = p.get_id(Some("b"), Some("b-url"));
        let used: HashSet<_> = [a, b].into_iter().collect();
        // Keep only b by pretending a is no longer referenced.
        let used_b: HashSet<_> = [b].into_iter().collect();
        let map = p.garbage_collect(&used_b);
        assert_eq!(map[a as usize], 0);
        assert_eq!(map[b as usize], 1);
        assert_eq!(p.len(), 2);
        assert_eq!(p.url_for_id(1), Some("b-url"));
        let _ = used;
    }
}

#[cfg(test)]
mod screen_tests {
    use crate::handler::ScreenHandler;
    use crate::screen::Screen;
    use kitty_vt::Parser;

    fn handler(cols: u32, lines: u32) -> ScreenHandler {
        ScreenHandler::new(Screen::builder(cols, lines).scrollback(8).build().unwrap())
    }

    fn feed(h: &mut ScreenHandler, bytes: &[u8]) {
        h.feed(&mut Parser::new(), bytes);
    }

    #[test]
    fn osc8_parser_assigns_ids_to_wide_cells() {
        let mut h = handler(6, 2);
        feed(
            &mut h,
            "\x1b]8;id=XX;http://example.com\x1b\\状\x1b]8;;\x1b\\".as_bytes(),
        );
        let ids: Vec<u16> = (0..6)
            .map(|x| h.screen.cpu_cell(x, 0).hyperlink_id())
            .collect();
        assert_eq!(ids, vec![1, 1, 0, 0, 0, 0]);
        let at = h.screen.hyperlink_at(0, 0).unwrap();
        assert_eq!(at.url, "http://example.com");
        assert_eq!(at.id, 1);
        assert!(at.spans.iter().any(|s| s.y == 0 && s.x_end == 2));
    }

    #[test]
    fn ansi_output_contains_osc8_start_and_end() {
        let mut h = handler(6, 1);
        feed(&mut h, b"\x1b]8;id=a;http://x\x1b\\ab\x1b]8;;\x1b\\c");
        let ansi = h.screen.line_ansi(0);
        assert!(ansi.starts_with("\x1b]8;id=a;http://x\x1b\\ab\x1b]8;;\x1b\\c"));
    }

    #[test]
    fn spans_follow_wraps_and_gc_drops_erased_links() {
        let mut h = handler(4, 2);
        feed(&mut h, b"\x1b]8;;http://wrap\x1b\\abcdef\x1b]8;;\x1b\\");
        assert_eq!(h.screen.hyperlink_id_at(0, 0), 1);
        assert_eq!(h.screen.hyperlink_id_at(1, 1), 1);
        let at = h.screen.hyperlink_at(0, 0).unwrap();
        assert!(at.spans.iter().any(|s| s.y == 0));
        assert!(at.spans.iter().any(|s| s.y == 1));
        let spans = h.screen.url_spans(0);
        assert_eq!(spans.len(), 1);
        h.screen.garbage_collect_hyperlink_pool();
        assert_eq!(h.screen.hyperlinks_as_set().len(), 1);
        h.screen.erase_in_display(2, false);
        h.screen.garbage_collect_hyperlink_pool();
        assert!(h.screen.hyperlinks_as_set().is_empty());
        assert_eq!(h.screen.hyperlink_id_at(0, 0), 0);
    }
}
