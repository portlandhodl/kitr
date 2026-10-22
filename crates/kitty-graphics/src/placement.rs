// SPDX-License-Identifier: GPL-3.0-or-later
//! Relative placement graph (`P`, `Q`, `H`, `V`).
//!
//! A relative placement is anchored to its parent's top-left cell plus the
//! signed cell offsets `H`/`V`. Parent chains are limited to
//! [`PARENT_DEPTH_LIMIT`] entries, may not contain cycles, and cannot make a
//! virtual (Unicode placeholder) placement relative to another placement.
//!
//! The graph in this module is deliberately independent of the terminal
//! `Screen`: callers supply the already stored placement anchors. This makes it
//! usable both by the screen integration and by the renderer, and lets the
//! renderer validate chains defensively even if state was loaded from an older
//! snapshot.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Minimum relative-placement chain length required by the protocol. Kitty
/// rejects chains longer than this, counting the starting placement.
pub const PARENT_DEPTH_LIMIT: usize = 8;

/// Identity of a placement within an image.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct PlacementKey {
    pub image_id: u32,
    pub placement_id: u32,
}

impl PlacementKey {
    pub const fn new(image_id: u32, placement_id: u32) -> Self {
        Self {
            image_id,
            placement_id,
        }
    }

    pub const fn parented(self, parent_id: u32, parent_placement_id: u32) -> bool {
        parent_id != 0 && !(parent_id == self.image_id && parent_placement_id == self.placement_id)
    }
}

/// Source rectangle in image pixels (`x`, `y`, `w`, `h`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SourceRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl SourceRect {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Intersect with an image, matching kitty's clamping in
    /// `handle_put_command`.
    pub fn clamped(self, image_width: u32, image_height: u32) -> Self {
        let width = if self.width == 0 {
            image_width.saturating_sub(self.x)
        } else {
            self.width.min(image_width.saturating_sub(self.x))
        };
        let height = if self.height == 0 {
            image_height.saturating_sub(self.y)
        } else {
            self.height.min(image_height.saturating_sub(self.y))
        };
        Self {
            x: self.x.min(image_width),
            y: self.y.min(image_height),
            width,
            height,
        }
    }
}

/// One placement and its raw parent links.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlacementRecord {
    pub key: PlacementKey,
    pub parent_id: u32,
    pub parent_placement_id: u32,
    /// Screen cell where the placement was created.
    pub anchor_x: u32,
    pub anchor_y: u32,
    /// `H`/`V` offsets in cells.
    pub offset_x: i32,
    pub offset_y: i32,
    pub z_index: i32,
    pub source: SourceRect,
    pub virtual_placement: bool,
    /// Inherit the parent's z-index when this placement's z-index is zero.
    pub inherit_z: bool,
    /// Inherit the parent's source rectangle when the child does not specify
    /// its own `w`/`h`.
    pub inherit_source_rect: bool,
}

impl PlacementRecord {
    pub fn new(key: PlacementKey, anchor_x: u32, anchor_y: u32) -> Self {
        Self {
            key,
            anchor_x,
            anchor_y,
            ..Self::default()
        }
    }

    pub fn with_parent(mut self, parent_id: u32, parent_placement_id: u32) -> Self {
        self.parent_id = parent_id;
        self.parent_placement_id = parent_placement_id;
        self
    }

    pub fn with_offset(mut self, x: i32, y: i32) -> Self {
        self.offset_x = x;
        self.offset_y = y;
        self
    }

    pub fn with_source(mut self, source: SourceRect) -> Self {
        self.source = source;
        self
    }

    pub fn with_z_index(mut self, z_index: i32) -> Self {
        self.z_index = z_index;
        self
    }

    pub fn as_virtual(mut self) -> Self {
        self.virtual_placement = true;
        self
    }

    pub fn inheriting(mut self, z: bool, source: bool) -> Self {
        self.inherit_z = z;
        self.inherit_source_rect = source;
        self
    }

    pub fn has_parent(&self) -> bool {
        self.parent_id != 0
    }
}

/// A resolved relative placement.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResolvedPlacement {
    /// Effective anchor in screen cells (the parent anchor plus `H`/`V`).
    pub anchor_x: i32,
    pub anchor_y: i32,
    pub z_index: i32,
    pub source: SourceRect,
}

/// Relative placement errors, reported with kitty's response codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlacementError {
    /// A parent image or placement does not exist.
    MissingParent(PlacementKey),
    /// A relative placement would form a cycle.
    Cycle(PlacementKey),
    /// The chain exceeds [`PARENT_DEPTH_LIMIT`].
    TooDeep(PlacementKey),
    /// A virtual placement was given a parent, or a placement named itself.
    Invalid(PlacementKey),
}

impl PlacementError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::MissingParent(_) => "ENOENT",
            Self::Cycle(_) => "ECYCLE",
            Self::TooDeep(_) => "ETOODEEP",
            Self::Invalid(_) => "EINVAL",
        }
    }

    pub const fn key(self) -> PlacementKey {
        match self {
            Self::MissingParent(key)
            | Self::Cycle(key)
            | Self::TooDeep(key)
            | Self::Invalid(key) => key,
        }
    }
}

impl core::fmt::Display for PlacementError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingParent(key) => write!(
                f,
                "One of the ancestors of this ref with image id: {} and ref id: {} not found",
                key.image_id, key.placement_id
            ),
            Self::Cycle(_) => f.write_str("This parent reference creates a cycle"),
            Self::TooDeep(_) => f.write_str("Too many levels of parent references"),
            Self::Invalid(_) => f.write_str("Invalid relative placement reference"),
        }
    }
}

impl std::error::Error for PlacementError {}

/// A collection of placements with parent/child resolution.
#[derive(Clone, Debug, Default)]
pub struct PlacementGraph {
    records: BTreeMap<PlacementKey, PlacementRecord>,
    insertion_order: BTreeMap<PlacementKey, u64>,
    next_order: u64,
}

impl PlacementGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_records(records: impl IntoIterator<Item = PlacementRecord>) -> Self {
        let mut graph = Self::new();
        for record in records {
            let _ = graph.insert(record);
        }
        graph
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn get(&self, key: PlacementKey) -> Option<&PlacementRecord> {
        self.records.get(&key)
    }

    pub fn records(&self) -> impl Iterator<Item = &PlacementRecord> {
        self.records.values()
    }

    /// Insert or replace a placement, returning the previous record.
    ///
    /// Virtual placements cannot have a parent, matching kitty's `EINVAL`
    /// response to such a put command.
    pub fn insert(
        &mut self,
        record: PlacementRecord,
    ) -> Result<Option<PlacementRecord>, PlacementError> {
        if record.has_parent()
            && (record.virtual_placement
                || (record.parent_id == record.key.image_id
                    && record.parent_placement_id == record.key.placement_id))
        {
            return Err(PlacementError::Invalid(record.key));
        }
        if !self.records.contains_key(&record.key) {
            self.next_order = self.next_order.wrapping_add(1);
            self.insertion_order.insert(record.key, self.next_order);
        }
        Ok(self.records.insert(record.key, record))
    }

    /// Override the anchor of a virtual placement. Renderers use this to set
    /// the minimum position of all placeholder cells referring to the
    /// placement.
    pub fn set_anchor(&mut self, key: PlacementKey, x: u32, y: u32) {
        if let Some(record) = self.records.get_mut(&key) {
            record.anchor_x = x;
            record.anchor_y = y;
        }
    }

    fn first_placement(&self, image_id: u32) -> Option<PlacementKey> {
        self.records
            .keys()
            .filter(|key| key.image_id == image_id)
            .min_by_key(|key| self.insertion_order.get(key).copied().unwrap_or(u64::MAX))
            .copied()
    }

    fn parent_key(&self, record: &PlacementRecord) -> Result<Option<PlacementKey>, PlacementError> {
        if record.parent_id == 0 {
            return Ok(None);
        }
        if record.parent_id == record.key.image_id
            && record.parent_placement_id == record.key.placement_id
        {
            return Err(PlacementError::Invalid(record.key));
        }
        let key = if record.parent_placement_id != 0 {
            PlacementKey::new(record.parent_id, record.parent_placement_id)
        } else {
            self.first_placement(record.parent_id)
                .ok_or(PlacementError::MissingParent(PlacementKey::new(
                    record.parent_id,
                    record.parent_placement_id,
                )))?
        };
        if !self.records.contains_key(&key) {
            return Err(PlacementError::MissingParent(key));
        }
        Ok(Some(key))
    }

    /// Validate the full ancestor chain of `key`.
    pub fn validate(&self, key: PlacementKey) -> Result<(), PlacementError> {
        let record = self
            .records
            .get(&key)
            .ok_or(PlacementError::MissingParent(key))?;
        let mut seen = BTreeSet::new();
        seen.insert(key);
        let mut current = record;
        let mut depth = 0usize;
        while let Some(parent) = self.parent_key(current)? {
            if !seen.insert(parent) {
                return Err(PlacementError::Cycle(parent));
            }
            depth += 1;
            if depth >= PARENT_DEPTH_LIMIT {
                return Err(PlacementError::TooDeep(key));
            }
            current = self.records.get(&parent).expect("parent existence checked");
        }
        Ok(())
    }

    /// Resolve a placement's anchor, inherited z-index and source rectangle.
    pub fn resolve(&self, key: PlacementKey) -> Result<ResolvedPlacement, PlacementError> {
        self.validate(key)?;
        let record = self.records.get(&key).expect("validated placement");
        let mut anchor_x = 0i64;
        let mut anchor_y = 0i64;
        let mut current = record;
        let z_index = if record.inherit_z {
            let mut z = record.z_index;
            let mut cursor = record;
            while let Some(parent) = self.parent_key(cursor)? {
                let parent_record = self.records.get(&parent).expect("parent existence checked");
                z = parent_record.z_index;
                if !parent_record.inherit_z {
                    break;
                }
                cursor = parent_record;
            }
            z
        } else {
            record.z_index
        };
        let source = if record.inherit_source_rect {
            let mut cursor = record;
            let mut source = record.source;
            let mut inherited = false;
            while let Some(parent) = self.parent_key(cursor)? {
                let parent_record = self.records.get(&parent).expect("parent existence checked");
                if !parent_record.source_is_unset() {
                    source = parent_record.source;
                    inherited = true;
                    break;
                }
                cursor = parent_record;
            }
            if !inherited {
                record.source
            } else {
                source
            }
        } else {
            record.source
        };
        while let Some(parent) = self.parent_key(current)? {
            anchor_x += i64::from(current.offset_x);
            anchor_y += i64::from(current.offset_y);
            current = self.records.get(&parent).expect("parent existence checked");
        }
        anchor_x += i64::from(current.anchor_x);
        anchor_y += i64::from(current.anchor_y);
        Ok(ResolvedPlacement {
            anchor_x: clamp_i32(anchor_x),
            anchor_y: clamp_i32(anchor_y),
            z_index,
            source,
        })
    }

    /// Resolve every valid placement, skipping records whose chain is broken,
    /// cyclic or too deep.
    pub fn resolve_all(&self) -> Vec<(PlacementKey, ResolvedPlacement)> {
        self.records
            .keys()
            .filter_map(|key| self.resolve(*key).ok().map(|resolved| (*key, resolved)))
            .collect()
    }

    /// Remove `key` and all of its descendants, returning the removed keys.
    ///
    /// This mirrors the protocol rule that the lifetime of a relative placement
    /// is tied to its parent.
    pub fn remove_cascade(&mut self, key: PlacementKey) -> Vec<PlacementKey> {
        let mut removed = Vec::new();
        let mut queue = VecDeque::from([key]);
        let mut seen = BTreeSet::new();
        seen.insert(key);
        while let Some(parent) = queue.pop_front() {
            // `Q=0` means "the first placement of the parent image". Resolve
            // that before removing the placement so removing a non-first
            // placement does not take unrelated children with it.
            let first = self.first_placement(parent.image_id);
            if self.records.remove(&parent).is_none() {
                continue;
            }
            self.insertion_order.remove(&parent);
            removed.push(parent);
            let children: Vec<PlacementKey> = self
                .records
                .values()
                .filter(|record| {
                    record.parent_id == parent.image_id
                        && (record.parent_placement_id == parent.placement_id
                            || (record.parent_placement_id == 0 && first == Some(parent)))
                })
                .map(|record| record.key)
                .collect();
            for child in children {
                if seen.insert(child) {
                    queue.push_back(child);
                }
            }
        }
        removed
    }

    /// Keys whose ancestor is missing or invalid.
    pub fn invalid_keys(&self) -> Vec<PlacementKey> {
        self.records
            .keys()
            .copied()
            .filter(|key| self.validate(*key).is_err())
            .collect()
    }
}

impl PlacementRecord {
    fn source_is_unset(&self) -> bool {
        self.source.width == 0 && self.source.height == 0
    }
}

fn clamp_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(image: u32, placement: u32, x: u32, y: u32) -> PlacementRecord {
        PlacementRecord::new(PlacementKey::new(image, placement), x, y)
    }

    #[test]
    fn resolves_nested_offsets_and_inheritance() {
        let parent = record(1, 1, 2, 3)
            .with_source(SourceRect::new(4, 5, 6, 7))
            .with_z_index(-2);
        let child = record(2, 1, 0, 0)
            .with_parent(1, 1)
            .with_offset(1, -1)
            .inheriting(true, true);
        let grandchild = record(3, 1, 0, 0)
            .with_parent(2, 1)
            .with_offset(-1, 2)
            .inheriting(true, true);
        let graph = PlacementGraph::from_records([parent, child, grandchild]);
        let resolved = graph.resolve(PlacementKey::new(3, 1)).unwrap();
        assert_eq!((resolved.anchor_x, resolved.anchor_y), (2, 4));
        assert_eq!(resolved.z_index, -2);
        assert_eq!(resolved.source, SourceRect::new(4, 5, 6, 7));
    }

    #[test]
    fn parent_placement_zero_selects_the_first_inserted_placement() {
        let graph = PlacementGraph::from_records([
            record(1, 4, 7, 8),
            record(1, 2, 1, 2),
            record(2, 0, 0, 0).with_parent(1, 0),
        ]);
        let resolved = graph.resolve(PlacementKey::new(2, 0)).unwrap();
        assert_eq!((resolved.anchor_x, resolved.anchor_y), (7, 8));
    }

    #[test]
    fn rejects_cycles_and_self_parents() {
        let mut graph = PlacementGraph::from_records([
            record(1, 1, 0, 0).with_parent(2, 1),
            record(2, 1, 0, 0).with_parent(1, 1),
        ]);
        assert_eq!(
            graph.validate(PlacementKey::new(1, 1)),
            Err(PlacementError::Cycle(PlacementKey::new(1, 1)))
        );
        let self_parent = record(3, 1, 0, 0).with_parent(3, 1);
        assert_eq!(
            graph.insert(self_parent),
            Err(PlacementError::Invalid(PlacementKey::new(3, 1)))
        );

        let mut direct = PlacementGraph::new();
        direct.insert(record(1, 1, 0, 0)).unwrap();
        let mut record = direct.get(PlacementKey::new(1, 1)).unwrap().clone();
        record.parent_id = 1;
        record.parent_placement_id = 1;
        assert_eq!(
            direct.insert(record),
            Err(PlacementError::Invalid(PlacementKey::new(1, 1)))
        );
    }

    #[test]
    fn enforces_the_depth_limit() {
        let mut records = vec![record(1, 1, 0, 0)];
        for depth in 2..=PARENT_DEPTH_LIMIT + 2 {
            records.push(record(1, depth as u32, 0, 0).with_parent(1, depth as u32 - 1));
        }
        let graph = PlacementGraph::from_records(records);
        assert!(graph
            .validate(PlacementKey::new(1, PARENT_DEPTH_LIMIT as u32))
            .is_ok());
        assert_eq!(
            graph.validate(PlacementKey::new(1, PARENT_DEPTH_LIMIT as u32 + 1)),
            Err(PlacementError::TooDeep(PlacementKey::new(
                1,
                PARENT_DEPTH_LIMIT as u32 + 1
            )))
        );
    }

    #[test]
    fn missing_parents_and_virtual_parents_are_errors() {
        let graph = PlacementGraph::from_records([record(1, 1, 0, 0)]);
        assert_eq!(
            graph.resolve(PlacementKey::new(2, 1)),
            Err(PlacementError::MissingParent(PlacementKey::new(2, 1)))
        );
        let mut virtual_parent = record(2, 1, 0, 0).as_virtual();
        virtual_parent.parent_id = 1;
        virtual_parent.parent_placement_id = 1;
        let mut graph = PlacementGraph::from_records([record(1, 1, 0, 0)]);
        assert_eq!(
            graph.insert(virtual_parent),
            Err(PlacementError::Invalid(PlacementKey::new(2, 1)))
        );
    }

    #[test]
    fn deleting_a_non_first_parent_keeps_q_zero_children() {
        let mut graph = PlacementGraph::from_records([
            record(1, 1, 0, 0),
            record(1, 2, 2, 0),
            record(2, 1, 0, 0).with_parent(1, 0),
        ]);
        // Q=0 resolved to the first placement (1, 1), so deleting (1, 2)
        // must not remove the child.
        let removed = graph.remove_cascade(PlacementKey::new(1, 2));
        assert_eq!(removed, vec![PlacementKey::new(1, 2)]);
        assert!(graph.get(PlacementKey::new(2, 1)).is_some());
        let removed = graph.remove_cascade(PlacementKey::new(1, 1));
        assert_eq!(
            removed,
            vec![PlacementKey::new(1, 1), PlacementKey::new(2, 1)]
        );
    }

    #[test]
    fn deletion_removes_the_whole_descendant_group() {
        let mut graph = PlacementGraph::from_records([
            record(1, 1, 0, 0),
            record(2, 1, 0, 0).with_parent(1, 1),
            record(3, 1, 0, 0).with_parent(2, 1),
            record(4, 1, 0, 0),
        ]);
        let removed = graph.remove_cascade(PlacementKey::new(1, 1));
        assert_eq!(
            removed,
            vec![
                PlacementKey::new(1, 1),
                PlacementKey::new(2, 1),
                PlacementKey::new(3, 1)
            ]
        );
        assert!(graph.get(PlacementKey::new(4, 1)).is_some());
        assert!(graph.get(PlacementKey::new(2, 1)).is_none());
    }
}
