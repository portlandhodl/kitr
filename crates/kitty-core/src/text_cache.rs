// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/text-cache.c, Copyright (C) 2024 Kovid Goyal <kovid at kovidgoyal.net>

//! Interning cache for multi-codepoint cell text.
//!
//! Cells store either a single `char_type` directly or an index into this
//! cache. Indices are never invalidated except by an explicit GC pass, which
//! mirrors `tc_gc_*` in the C implementation.

use std::collections::HashMap;

use crate::cell::{CharType, MAX_CHAR_TYPE_VALUE};

const INITIAL_CAPACITY: usize = 256;
const ADDS_BETWEEN_GCS: u32 = 8192;

/// Data captured by [`TextCache::gc_begin`] and consumed by
/// [`TextCache::gc_end`]. While a GC is in progress the owner must remap every
/// live cell index with [`TextCache::gc_map_index`].
pub struct TextCacheGcData {
    old_items: Vec<Box<[CharType]>>,
    map: Vec<u32>,
    old_count: u32,
}

#[derive(Default, Debug)]
pub struct TextCache {
    items: Vec<Box<[CharType]>>,
    map: HashMap<Box<[CharType]>, CharType>,
    adds_since_last_gc: u32,
    adds_between_gcs: u32,
}

impl TextCache {
    pub fn new() -> Self {
        Self {
            items: Vec::with_capacity(INITIAL_CAPACITY),
            map: HashMap::with_capacity(INITIAL_CAPACITY),
            adds_since_last_gc: 0,
            adds_between_gcs: 0,
        }
    }

    #[inline]
    pub fn num_entries(&self) -> CharType {
        self.items.len() as CharType
    }

    /// `tc_get_or_insert_chars()`.
    #[inline]
    pub fn get_or_insert(&mut self, chars: &[CharType]) -> CharType {
        if let Some(&idx) = self.map.get(chars) {
            return idx;
        }
        self.adds_since_last_gc += 1;
        let idx = self.items.len() as CharType;
        assert!(idx != MAX_CHAR_TYPE_VALUE, "Too many items in TextCache");
        let key: Box<[CharType]> = chars.into();
        self.items.push(key.clone());
        self.map.insert(key, idx);
        idx
    }

    /// `tc_chars_at_index()`. Returns an empty slice for out of range indices,
    /// matching the C behavior.
    #[inline]
    pub fn chars_at(&self, idx: CharType) -> &[CharType] {
        self.items.get(idx as usize).map_or(&[], |x| x)
    }

    #[inline]
    pub fn first_char_at(&self, idx: CharType) -> CharType {
        self.chars_at(idx).first().copied().unwrap_or(0)
    }

    #[inline]
    pub fn last_char_at(&self, idx: CharType) -> CharType {
        self.chars_at(idx).last().copied().unwrap_or(0)
    }

    /// `tc_num_codepoints()`.
    #[inline]
    pub fn num_codepoints(&self, idx: CharType) -> u32 {
        self.chars_at(idx).len() as u32
    }

    /// `tc_should_gc()`.
    #[inline]
    pub fn should_gc(&self) -> bool {
        self.adds_since_last_gc > ADDS_BETWEEN_GCS.max(self.adds_between_gcs)
    }

    /// `tc_gc_begin()`. Steals all entries; the caller must remap every live
    /// index before calling [`Self::gc_end`].
    pub fn gc_begin(&mut self) -> TextCacheGcData {
        let old_items = std::mem::take(&mut self.items);
        let old_count = old_items.len() as u32;
        self.items = Vec::with_capacity(INITIAL_CAPACITY);
        self.map.clear();
        self.map.reserve(INITIAL_CAPACITY);
        TextCacheGcData {
            old_items,
            map: vec![0; old_count as usize],
            old_count,
        }
    }

    /// `tc_gc_map_index()`. Returns `None` if `old_idx` was never valid.
    pub fn gc_map_index(
        &mut self,
        gc: &mut TextCacheGcData,
        old_idx: CharType,
    ) -> Option<CharType> {
        if old_idx >= gc.old_count {
            return None;
        }
        let slot = gc.map.get(old_idx as usize).copied()?;
        let new_idx = if slot == 0 {
            let key = &gc.old_items[old_idx as usize];
            let nidx = self.get_or_insert(key);
            gc.map[old_idx as usize] = nidx + 1;
            nidx
        } else {
            slot - 1
        };
        Some(new_idx)
    }

    /// `tc_gc_end()`.
    pub fn gc_end(&mut self, gc: TextCacheGcData) {
        self.adds_since_last_gc = 0;
        self.adds_between_gcs = self.items.len() as u32;
        drop(gc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_and_lookup() {
        let mut tc = TextCache::new();
        let a = tc.get_or_insert(&[b'h' as u32, b'i' as u32]);
        let b = tc.get_or_insert(&[b'h' as u32, b'i' as u32]);
        let c = tc.get_or_insert(&[b'y' as u32]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(tc.num_entries(), 2);
        assert_eq!(tc.chars_at(a), &[b'h' as u32, b'i' as u32]);
        assert_eq!(tc.num_codepoints(a), 2);
        assert_eq!(tc.first_char_at(a), b'h' as u32);
        assert_eq!(tc.last_char_at(a), b'i' as u32);
        assert_eq!(tc.chars_at(99), &[] as &[CharType]);
        assert_eq!(tc.num_codepoints(99), 0);
    }

    #[test]
    fn gc_remaps_live_entries() {
        let mut tc = TextCache::new();
        let keep = tc.get_or_insert(&[1, 2, 3]);
        let _drop = tc.get_or_insert(&[4, 5, 6]);
        let mut gc = tc.gc_begin();
        assert_eq!(tc.num_entries(), 0);
        let new_keep = tc.gc_map_index(&mut gc, keep).unwrap();
        assert_eq!(tc.chars_at(new_keep), &[1, 2, 3]);
        assert_eq!(tc.gc_map_index(&mut gc, 77), None);
        tc.gc_end(gc);
        assert_eq!(tc.num_entries(), 1);
    }
}
