// SPDX-License-Identifier: GPL-3.0-or-later
//! Animation frames, composition and terminal-driven timing (`a=f`, `a=a`,
//! `a=c`).
//!
//! Frame data uses a delta chain: the root and edited frames are keyframes,
//! while a newly transmitted frame stores only its patch and a reference to the
//! frame it was based on (`c=`). Reconstructing a frame walks that chain, so a
//! long animation does not keep one full RGBA canvas per frame. The timing model
//! follows `kitty/graphics.c`: a frame's gap is the delay before advancing to
//! the next frame, zero-gap frames are skipped, loading mode waits at the end,
//! and loop count zero means infinite.
//!
//! Images and frames are subject to per-image and global byte quotas. New data
//! is admitted after evicting the oldest non-current frames (promoting their
//! dependents first); if no eviction can make enough room the command fails
//! with `ENOSPC`, matching kitty's cache-full response.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::command::{Action, DeleteAction, GraphicsCommand};
use crate::image::{decode_image, GraphicsError, Image, ImageFormat, ImageLimits};

/// Default inter-frame gap used when a new frame omits `z=`.
pub const DEFAULT_FRAME_GAP_MS: u32 = 40;
/// Kitty's default global graphics storage limit (320 MiB).
pub const DEFAULT_STORAGE_LIMIT: usize = 320 * 1024 * 1024;
/// Default per-image storage limit (128 MiB).
pub const DEFAULT_IMAGE_STORAGE_LIMIT: usize = 128 * 1024 * 1024;
/// Maximum delta-reference chain walked while reconstructing a frame.
pub const MAX_FRAME_CHAIN: usize = 32;

/// Animation run state (`s=`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AnimationState {
    #[default]
    Stopped,
    Loading,
    Running,
}

impl AnimationState {
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::Stopped),
            2 => Some(Self::Loading),
            3 => Some(Self::Running),
            _ => None,
        }
    }
}

/// Pixel data that is either resident or spilled to the optional disk cache.
#[derive(Clone, Debug, Eq, PartialEq)]
enum FrameBlob {
    Memory(Arc<Vec<u8>>),
    Disk { path: PathBuf, len: usize },
}

impl FrameBlob {
    fn len(&self) -> usize {
        match self {
            Self::Memory(data) => data.len(),
            Self::Disk { len, .. } => *len,
        }
    }

    fn to_arc(&self) -> Option<Arc<Vec<u8>>> {
        match self {
            Self::Memory(data) => Some(data.clone()),
            Self::Disk { path, .. } => std::fs::read(path).ok().map(Arc::new),
        }
    }
}

/// Storage for one animation frame.
#[derive(Clone, Debug, Eq, PartialEq)]
enum FrameStorage {
    /// A full RGBA8 canvas.
    Key(FrameBlob),
    /// A patch composed over `base_frame_id` (zero means a transparent or
    /// background-filled canvas).
    Delta {
        base_frame_id: u32,
        patch: DeltaPatch,
        bgcolor: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeltaPatch {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    rgba: FrameBlob,
    blend: bool,
}

/// One animation frame.
///
/// Frame pixel data is not stored inline: use [`AnimatedImage::frame_data`] or
/// [`AnimatedImage::current_data`] to reconstruct the coalesced RGBA canvas.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnimationFrame {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub gap_ms: u32,
    pub transient: bool,
    storage: FrameStorage,
}

impl AnimationFrame {
    /// Bytes actually stored for this frame. Delta frames only count their
    /// patch, which is the point of the chain representation.
    pub fn stored_bytes(&self) -> usize {
        match &self.storage {
            FrameStorage::Key(data) => data.len(),
            FrameStorage::Delta { patch, .. } => patch.rgba.len() + 32,
        }
    }

    pub fn is_keyframe(&self) -> bool {
        matches!(self.storage, FrameStorage::Key(_))
    }

    /// For a delta frame, the id of the frame it is based on.
    pub fn base_frame_id(&self) -> Option<u32> {
        match self.storage {
            FrameStorage::Delta { base_frame_id, .. } if base_frame_id != 0 => Some(base_frame_id),
            _ => None,
        }
    }
}

/// Resource limits for animation storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnimationLimits {
    /// Maximum bytes used by one image and all of its frames.
    pub max_image_bytes: usize,
    /// Maximum bytes used by all images tracked by a manager.
    pub max_total_bytes: usize,
    /// Maximum delta-chain length before a frame is promoted to a keyframe.
    pub max_frame_chain: usize,
}

impl Default for AnimationLimits {
    fn default() -> Self {
        Self {
            max_image_bytes: DEFAULT_IMAGE_STORAGE_LIMIT,
            max_total_bytes: DEFAULT_STORAGE_LIMIT,
            max_frame_chain: MAX_FRAME_CHAIN,
        }
    }
}

impl AnimationLimits {
    pub const fn new(max_image_bytes: usize, max_total_bytes: usize) -> Self {
        Self {
            max_image_bytes,
            max_total_bytes,
            max_frame_chain: MAX_FRAME_CHAIN,
        }
    }
}

/// An image with a root frame plus optional animation frames.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnimatedImage {
    pub id: u32,
    pub number: u32,
    pub width: u32,
    pub height: u32,
    pub frames: Vec<AnimationFrame>,
    pub current_frame_index: usize,
    pub animation_state: AnimationState,
    pub max_loops: u32,
    pub current_loop: u32,
    pub current_frame_shown_at_ms: u64,
    pub animation_duration_ms: u64,
    pub is_drawn: bool,
    next_frame_id: u32,
    last_access: u64,
}

impl AnimatedImage {
    pub fn current_frame(&self) -> &AnimationFrame {
        &self.frames[self.current_frame_index]
    }

    /// The current frame, reconstructed to a full RGBA8 canvas.
    pub fn current_rgba(&self) -> Vec<u8> {
        self.reconstruct_index(self.current_frame_index, &mut Vec::new(), 0)
            .map(|data| data.as_ref().clone())
            .unwrap_or_default()
    }

    /// Shared handle to the current frame's reconstructed RGBA pixels.
    pub fn current_data(&self) -> Arc<Vec<u8>> {
        self.reconstruct_index(self.current_frame_index, &mut Vec::new(), 0)
            .unwrap_or_else(|| Arc::new(Vec::new()))
    }

    /// Frame by 1-based protocol number (`1` is the root frame).
    pub fn frame(&self, number: u32) -> Option<&AnimationFrame> {
        if number == 0 {
            return None;
        }
        self.frames.get(number as usize - 1)
    }

    /// Reconstructed RGBA data for a 1-based frame number.
    pub fn frame_data(&self, number: u32) -> Option<Arc<Vec<u8>>> {
        let index = number.checked_sub(1)? as usize;
        if index >= self.frames.len() {
            return None;
        }
        self.reconstruct_index(index, &mut Vec::new(), 0)
    }

    /// Total bytes actually stored by all frames (deltas count only patches).
    pub fn stored_bytes(&self) -> usize {
        self.frames.iter().map(AnimationFrame::stored_bytes).sum()
    }

    /// Full canvas size in bytes.
    pub fn canvas_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    fn frame_index_for_id(&self, id: u32) -> Option<usize> {
        self.frames.iter().position(|frame| frame.id == id)
    }

    fn reconstruct_index(
        &self,
        index: usize,
        stack: &mut Vec<u32>,
        depth: usize,
    ) -> Option<Arc<Vec<u8>>> {
        if depth > MAX_FRAME_CHAIN {
            return None;
        }
        let frame = self.frames.get(index)?;
        match &frame.storage {
            FrameStorage::Key(data) => data.to_arc(),
            FrameStorage::Delta {
                base_frame_id,
                patch,
                bgcolor,
            } => {
                let mut canvas = if *base_frame_id != 0 {
                    if stack.contains(base_frame_id) {
                        return None;
                    }
                    let base_index = self.frame_index_for_id(*base_frame_id)?;
                    stack.push(*base_frame_id);
                    let base = self.reconstruct_index(base_index, stack, depth + 1)?;
                    stack.pop();
                    base.as_ref().clone()
                } else if *bgcolor != 0 {
                    fill_bgcolor(self.width, self.height, *bgcolor)
                } else {
                    vec![0u8; self.canvas_bytes()]
                };
                let patch_data = patch.rgba.to_arc()?;
                compose_patch(
                    &mut canvas,
                    self.width,
                    self.height,
                    &patch_data,
                    patch.width,
                    patch.height,
                    patch.x,
                    patch.y,
                    patch.blend,
                );
                Some(Arc::new(canvas))
            }
        }
    }

    /// Convert a frame into a keyframe, preserving its visible pixels.
    fn promote_to_keyframe(&mut self, index: usize) -> bool {
        let Some(data) = self.reconstruct_index(index, &mut Vec::new(), 0) else {
            return false;
        };
        self.frames[index].storage = FrameStorage::Key(FrameBlob::Memory(data));
        true
    }

    /// Number of delta links needed to reconstruct a frame by id.
    fn chain_length_from_id(&self, id: u32) -> usize {
        let mut length = 1usize;
        let mut frame = self.frames.iter().find(|frame| frame.id == id);
        while let Some(current) = frame {
            match current.base_frame_id() {
                Some(base) if length <= MAX_FRAME_CHAIN => {
                    length += 1;
                    frame = self.frames.iter().find(|frame| frame.id == base);
                }
                _ => break,
            }
        }
        length
    }

    fn frame_mut(&mut self, number: u32) -> Option<&mut AnimationFrame> {
        if number == 0 {
            return None;
        }
        self.frames.get_mut(number as usize - 1)
    }

    pub fn is_animatable(&self) -> bool {
        self.animation_state != AnimationState::Stopped
            && self.frames.len() > 1
            && self.is_drawn
            && self.animation_duration_ms > 0
            && (self.max_loops == 0 || self.current_loop < self.max_loops)
    }
}

/// Result of [`AnimationManager::tick`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AnimationTick {
    /// Image ids whose current frame changed and that need a redraw.
    pub redraw: Vec<u32>,
    /// Earliest absolute monotonic deadline in milliseconds, if one exists.
    pub next_deadline_ms: Option<u64>,
}

/// Result of feeding an animation command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnimationOutcome {
    ChunkAccepted,
    Deferred,
    FrameAdded {
        image_id: u32,
        frame_number: u32,
        is_new: bool,
        redraw: bool,
    },
    Composed {
        image_id: u32,
        source_frame: u32,
        destination_frame: u32,
        redraw: bool,
    },
    Control {
        image_id: u32,
        redraw: bool,
    },
}

#[derive(Clone, Debug)]
struct PendingFrame {
    start: GraphicsCommand,
    payload: Vec<u8>,
}

/// Stores images and their animation frames.
#[derive(Clone, Debug)]
pub struct AnimationManager {
    images: BTreeMap<u32, AnimatedImage>,
    pending: Option<PendingFrame>,
    limits: AnimationLimits,
    total_bytes: usize,
    access_clock: u64,
    evicted_frames: u64,
    evicted_images: u64,
    disk: Option<Arc<Mutex<AnimationDiskCache>>>,
}

#[derive(Clone, Debug)]
struct AnimationDiskCache {
    dir: PathBuf,
    next: u64,
}

impl AnimationDiskCache {
    fn new(root: &Path) -> io::Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = root.join(format!(
            "kitr-animation-cache-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir, next: 0 })
    }

    fn write(&mut self, data: &[u8]) -> io::Result<FrameBlob> {
        self.next = self.next.wrapping_add(1);
        let path = self.dir.join(format!("frame-{:016x}.bin", self.next));
        std::fs::write(&path, data)?;
        Ok(FrameBlob::Disk {
            path,
            len: data.len(),
        })
    }

    fn clear(&mut self) {
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        self.next = 0;
    }
}

impl Drop for AnimationDiskCache {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Default for AnimationManager {
    fn default() -> Self {
        Self::new_with_limits(AnimationLimits::default())
    }
}

impl AnimationManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a manager with explicit storage limits.
    pub fn new_with_limits(limits: AnimationLimits) -> Self {
        Self {
            images: BTreeMap::new(),
            pending: None,
            limits,
            total_bytes: 0,
            access_clock: 0,
            evicted_frames: 0,
            evicted_images: 0,
            disk: None,
        }
    }

    /// Create a manager that spills frame pixels into a private directory
    /// under `root`. The directory is removed when the manager is dropped.
    pub fn new_with_disk_cache(root: impl AsRef<Path>) -> io::Result<Self> {
        let mut manager = Self::new_with_limits(AnimationLimits::default());
        manager.disk = Some(Arc::new(Mutex::new(AnimationDiskCache::new(
            root.as_ref(),
        )?)));
        Ok(manager)
    }

    /// Enable disk spilling on an existing manager.
    pub fn enable_disk_cache(&mut self, root: impl AsRef<Path>) -> io::Result<()> {
        self.disk = Some(Arc::new(Mutex::new(AnimationDiskCache::new(
            root.as_ref(),
        )?)));
        Ok(())
    }

    pub fn disk_cache_dir(&self) -> Option<PathBuf> {
        self.disk
            .as_ref()
            .and_then(|disk| disk.lock().ok())
            .map(|disk| disk.dir.clone())
    }

    fn store_blob(&mut self, data: Vec<u8>) -> FrameBlob {
        if let Some(disk) = &self.disk {
            if let Ok(mut disk) = disk.lock() {
                if let Ok(blob) = disk.write(&data) {
                    return blob;
                }
            }
        }
        FrameBlob::Memory(Arc::new(data))
    }

    pub fn limits(&self) -> AnimationLimits {
        self.limits
    }

    pub fn set_limits(&mut self, limits: AnimationLimits) {
        self.limits = limits;
    }

    /// Bytes currently stored by all images (deltas count only their patches).
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Bytes currently stored by one image.
    pub fn image_bytes(&self, id: u32) -> usize {
        self.images.get(&id).map_or(0, AnimatedImage::stored_bytes)
    }

    /// Number of frames evicted to satisfy a quota.
    pub fn evicted_frames(&self) -> u64 {
        self.evicted_frames
    }

    /// Number of whole images evicted to satisfy a quota.
    pub fn evicted_images(&self) -> u64 {
        self.evicted_images
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    pub fn image(&self, id: u32) -> Option<&AnimatedImage> {
        self.images.get(&id)
    }

    pub fn image_mut(&mut self, id: u32) -> Option<&mut AnimatedImage> {
        self.images.get_mut(&id)
    }

    pub fn images(&self) -> impl Iterator<Item = &AnimatedImage> {
        self.images.values()
    }

    /// Drop all images and frames. No resources are kept on disk, so this is
    /// also the exit/delete cleanup path.
    pub fn clear(&mut self) {
        self.images.clear();
        self.pending = None;
        self.total_bytes = 0;
        if let Some(disk) = &self.disk {
            if let Ok(mut disk) = disk.lock() {
                disk.clear();
            }
        }
    }

    pub fn has_pending_frame(&self) -> bool {
        self.pending.is_some()
    }

    fn next_access(&mut self) -> u64 {
        self.access_clock = self.access_clock.wrapping_add(1);
        self.access_clock
    }

    fn recompute_total(&mut self) {
        self.total_bytes = self.images.values().map(AnimatedImage::stored_bytes).sum();
    }

    fn make_room(&mut self, required: usize, owner: u32, protected_frame_id: Option<u32>) -> bool {
        if required > self.limits.max_image_bytes || required > self.limits.max_total_bytes {
            return false;
        }
        while self.image_bytes(owner).saturating_add(required) > self.limits.max_image_bytes {
            if !self.evict_one(Some(owner), protected_frame_id) {
                return false;
            }
        }
        while self.total_bytes.saturating_add(required) > self.limits.max_total_bytes {
            if !self.evict_one(None, protected_frame_id) {
                return false;
            }
        }
        true
    }

    /// Evict one frame, preferring `preferred_image`, transient frames and then
    /// the least recently used image. Returns false when nothing can be
    /// evicted without touching the protected frame.
    fn evict_one(&mut self, preferred_image: Option<u32>, protected_frame_id: Option<u32>) -> bool {
        let mut candidates: Vec<(u32, usize, bool, u64)> = Vec::new();
        for (id, image) in &self.images {
            for (index, frame) in image.frames.iter().enumerate() {
                if Some(frame.id) == protected_frame_id {
                    continue;
                }
                candidates.push((*id, index, frame.transient, image.last_access));
            }
        }
        candidates.sort_by_key(|(id, index, transient, access)| {
            (Some(*id) != preferred_image, !*transient, *access, *index)
        });
        for (id, index, _, _) in candidates {
            let remove_image = {
                let image = self.images.get(&id).expect("candidate image");
                image.frames.len() == 1
            };
            if remove_image && Some(id) == preferred_image {
                continue;
            }
            if remove_image {
                self.images.remove(&id);
                self.evicted_images += 1;
                self.recompute_total();
                return true;
            }
            let Some(image) = self.images.get_mut(&id) else {
                continue;
            };
            let victim_id = image.frames[index].id;
            let mut promoted = Vec::new();
            let mut ok = true;
            for (frame_index, frame) in image.frames.iter().enumerate() {
                if frame.base_frame_id() == Some(victim_id) {
                    promoted.push(frame_index);
                }
            }
            for frame_index in promoted {
                if !image.promote_to_keyframe(frame_index) {
                    ok = false;
                    break;
                }
            }
            if !ok {
                continue;
            }
            let removed_gap = image.frames[index].gap_ms;
            image.frames.remove(index);
            image.animation_duration_ms = image
                .animation_duration_ms
                .saturating_sub(u64::from(removed_gap));
            if image.current_frame_index > index {
                image.current_frame_index -= 1;
            } else if image.current_frame_index == index {
                image.current_frame_index = image.current_frame_index.min(image.frames.len() - 1);
            }
            self.evicted_frames += 1;
            self.recompute_total();
            return true;
        }
        false
    }

    fn key_for(&self, id: u32, number: u32) -> Option<u32> {
        if id != 0 && self.images.contains_key(&id) {
            return Some(id);
        }
        if number != 0 {
            return self
                .images
                .iter()
                .find(|(_, image)| image.number == number)
                .map(|(id, _)| *id);
        }
        None
    }

    /// Store `image` as the root frame, replacing any existing animation.
    pub fn insert_root(&mut self, image: &Image) -> Result<u32, GraphicsError> {
        let id = if image.id != 0 {
            image.id
        } else {
            image.number
        };
        if id == 0 {
            return Err(animation_error("EINVAL", "Animation image has no id"));
        }
        if matches!(
            image.format,
            ImageFormat::Indexed | ImageFormat::IndexedAlpha
        ) {
            return Err(animation_error(
                "EINVAL",
                "Animation is not supported for indexed color images",
            ));
        }
        let rgba = image
            .rgba
            .clone()
            .ok_or_else(|| animation_error("EINVAL", "Animation image has no RGBA data"))?;
        if rgba.len() != image.width as usize * image.height as usize * 4 {
            return Err(animation_error(
                "EINVAL",
                "Animation image has inconsistent RGBA data",
            ));
        }
        let required = rgba.len();
        self.images.remove(&id);
        self.recompute_total();
        if !self.make_room(required, id, None) {
            return Err(animation_error(
                "ENOSPC",
                "Failed to store image data in cache",
            ));
        }
        let root_storage = self.store_blob(rgba);
        self.images.insert(
            id,
            AnimatedImage {
                id,
                number: image.number,
                width: image.width,
                height: image.height,
                frames: vec![AnimationFrame {
                    id: 1,
                    width: image.width,
                    height: image.height,
                    gap_ms: 0,
                    transient: image.transient,
                    storage: FrameStorage::Key(root_storage),
                }],
                current_frame_index: 0,
                animation_state: AnimationState::Stopped,
                max_loops: 0,
                current_loop: 0,
                current_frame_shown_at_ms: 0,
                animation_duration_ms: 0,
                is_drawn: true,
                next_frame_id: 2,
                last_access: 0,
            },
        );
        let access = self.next_access();
        if let Some(image) = self.images.get_mut(&id) {
            image.last_access = access;
        }
        self.recompute_total();
        self.pending = None;
        Ok(id)
    }

    /// Add a root only when the image is not already tracked.
    pub fn ensure_root(&mut self, image: &Image) {
        let id = if image.id != 0 {
            image.id
        } else {
            image.number
        };
        if id != 0 && !self.images.contains_key(&id) {
            let _ = self.insert_root(image);
        }
    }

    pub fn remove_image(&mut self, id: u32) -> bool {
        let removed = self.images.remove(&id).is_some();
        if removed {
            self.recompute_total();
        }
        removed
    }

    /// Resolve an image by client id or image number.
    pub fn resolve_id(&self, id: u32, number: u32) -> Option<u32> {
        self.key_for(id, number)
    }

    /// Delete one frame. Frame numbers are 1-based; deleting the root promotes
    /// the next frame. Returns whether a frame was removed.
    pub fn delete_frame(&mut self, id: u32, frame_number: u32) -> bool {
        let Some(image) = self.images.get_mut(&id) else {
            return false;
        };
        if image.frames.len() <= 1 {
            return false;
        }
        let index = if frame_number <= 1 {
            0
        } else {
            frame_number as usize - 1
        };
        if index >= image.frames.len() {
            return false;
        }
        let victim_id = image.frames[index].id;
        let dependents: Vec<usize> = image
            .frames
            .iter()
            .enumerate()
            .filter(|(_, frame)| frame.base_frame_id() == Some(victim_id))
            .map(|(index, _)| index)
            .collect();
        for dependent in dependents {
            if !image.promote_to_keyframe(dependent) {
                return false;
            }
        }
        let removed_gap = image.frames[index].gap_ms;
        image.frames.remove(index);
        image.animation_duration_ms = image
            .animation_duration_ms
            .saturating_sub(u64::from(removed_gap));
        if image.current_frame_index > index {
            image.current_frame_index -= 1;
        } else if image.current_frame_index == index {
            image.current_frame_index = image.current_frame_index.min(image.frames.len() - 1);
        }
        self.recompute_total();
        true
    }

    pub fn set_state(&mut self, id: u32, state: AnimationState, now_ms: u64) -> bool {
        let Some(image) = self.images.get_mut(&id) else {
            return false;
        };
        let was_stopped = image.animation_state == AnimationState::Stopped;
        image.animation_state = state;
        if state == AnimationState::Stopped {
            image.current_loop = 0;
        } else {
            if was_stopped {
                image.current_frame_shown_at_ms = now_ms;
                image.is_drawn = true;
            }
            image.current_loop = 0;
        }
        true
    }

    pub fn select_frame(&mut self, id: u32, frame_number: u32, now_ms: u64) -> bool {
        let Some(image) = self.images.get_mut(&id) else {
            return false;
        };
        if frame_number == 0 || frame_number as usize > image.frames.len() {
            return false;
        }
        let index = frame_number as usize - 1;
        if index == image.current_frame_index {
            return false;
        }
        image.current_frame_index = index;
        image.current_frame_shown_at_ms = now_ms;
        true
    }

    pub fn set_gap(&mut self, id: u32, frame_number: u32, gap_ms: i32) -> bool {
        let Some(image) = self.images.get_mut(&id) else {
            return false;
        };
        let Some(frame) = image.frame_mut(frame_number) else {
            return false;
        };
        let new_gap = gap_ms.max(0) as u32;
        let old_gap = frame.gap_ms;
        frame.gap_ms = new_gap;
        image.animation_duration_ms = image
            .animation_duration_ms
            .saturating_sub(u64::from(old_gap))
            .saturating_add(u64::from(new_gap));
        true
    }

    /// Feed one parsed animation command at monotonic time `now_ms`.
    pub fn feed(
        &mut self,
        command: &GraphicsCommand,
        limits: &ImageLimits,
        now_ms: u64,
    ) -> Result<AnimationOutcome, GraphicsError> {
        if let Some(mut pending) = self.pending.take() {
            if command.action == Some(Action::Delete) {
                return Ok(AnimationOutcome::Deferred);
            }
            pending.payload.extend_from_slice(&command.payload);
            if pending.payload.len() > limits.max_data_size {
                return Err(GraphicsError::DataTooLarge {
                    limit: limits.max_data_size,
                });
            }
            if command.more != 0 {
                self.pending = Some(pending);
                return Ok(AnimationOutcome::ChunkAccepted);
            }
            let mut start = pending.start;
            start.payload = pending.payload;
            return self.apply_frame(&start, limits, now_ms);
        }

        match command.action {
            Some(Action::FrameData) => {
                if command.more != 0 {
                    if command.payload.len() > limits.max_data_size {
                        return Err(GraphicsError::DataTooLarge {
                            limit: limits.max_data_size,
                        });
                    }
                    self.pending = Some(PendingFrame {
                        start: command.clone(),
                        payload: command.payload.clone(),
                    });
                    Ok(AnimationOutcome::ChunkAccepted)
                } else {
                    self.apply_frame(command, limits, now_ms)
                }
            }
            Some(Action::AnimationFrame) => self.apply_control(command, now_ms),
            Some(Action::Compose) => self.apply_compose(command),
            Some(Action::Delete) => self.apply_delete(command),
            _ => Ok(AnimationOutcome::Deferred),
        }
    }

    fn apply_delete(
        &mut self,
        command: &GraphicsCommand,
    ) -> Result<AnimationOutcome, GraphicsError> {
        let action = command.delete_action.unwrap_or(DeleteAction::All);
        if !matches!(action, DeleteAction::Frames | DeleteAction::AllFrames) {
            return Ok(AnimationOutcome::Deferred);
        }
        let Some(image_id) = self.key_for(command.id, command.image_number) else {
            return Ok(AnimationOutcome::Deferred);
        };
        let frame_count = self
            .images
            .get(&image_id)
            .map_or(0, |image| image.frames.len());
        let has_extra_frames = frame_count > 1;
        let frame_number = if command.frame_number() == 0 {
            1
        } else {
            command.frame_number().min(frame_count as u32)
        };
        if action == DeleteAction::AllFrames && !has_extra_frames {
            self.remove_image(image_id);
            return Ok(AnimationOutcome::Control {
                image_id,
                redraw: true,
            });
        }
        if has_extra_frames && self.delete_frame(image_id, frame_number) {
            Ok(AnimationOutcome::Control {
                image_id,
                redraw: true,
            })
        } else {
            Ok(AnimationOutcome::Deferred)
        }
    }

    fn apply_frame(
        &mut self,
        command: &GraphicsCommand,
        limits: &ImageLimits,
        now_ms: u64,
    ) -> Result<AnimationOutcome, GraphicsError> {
        let image_id = self
            .key_for(command.id, command.image_number)
            .ok_or_else(|| {
                animation_error("ENOENT", "Animation command refers to missing image")
            })?;
        let decoded = decode_image(command, &command.payload, limits)?;
        if matches!(
            decoded.format,
            ImageFormat::Indexed | ImageFormat::IndexedAlpha
        ) {
            return Err(animation_error(
                "EINVAL",
                "Animation is not supported for indexed color images",
            ));
        }
        let patch = decoded
            .rgba
            .ok_or_else(|| animation_error("EINVAL", "Frame has no RGBA data"))?;
        let gap_ms = match command.gap() {
            gap if gap > 0 => gap as u32,
            gap if gap < 0 => 0,
            _ => DEFAULT_FRAME_GAP_MS,
        };
        let blend = command.blend_mode() != 1 && !decoded.is_opaque;
        let x = command.x_offset;
        let y = command.y_offset;
        let requested = command.frame_number();
        let source_number = command.other_frame_number();

        let (initial_len, source_id, edit_id) = {
            let image = self.images.get(&image_id).expect("resolved image");
            if decoded.width > image.width || decoded.height > image.height {
                return Err(animation_error(
                    "EINVAL",
                    "Frame is larger than the animation image",
                ));
            }
            let last = image.frames.len() as u32;
            let is_new = requested == 0 || requested > last;
            let source_id = if source_number != 0 {
                Some(
                    image
                        .frame(source_number)
                        .ok_or_else(|| animation_error("EINVAL", "No source frame found"))?
                        .id,
                )
            } else {
                None
            };
            let edit_id = if is_new {
                None
            } else {
                Some(image.frames[requested as usize - 1].id)
            };
            (last, source_id, edit_id)
        };

        let access = self.next_access();
        let (frame_number, is_new, redraw, transient) = if let Some(edit_id) = edit_id {
            // Editing an existing frame promotes it to a keyframe, exactly like
            // kitty coalesces an edited frame.
            let mut canvas = self
                .images
                .get(&image_id)
                .and_then(|image| image.frame_data(edit_id))
                .ok_or_else(|| animation_error("EINVAL", "No data associated with frame"))?
                .as_ref()
                .clone();
            let (image_width, image_height) = {
                let image = self.images.get(&image_id).expect("resolved image");
                (image.width, image.height)
            };
            compose_patch(
                &mut canvas,
                image_width,
                image_height,
                &patch,
                decoded.width,
                decoded.height,
                x,
                y,
                blend,
            );
            let old_bytes = self
                .images
                .get(&image_id)
                .and_then(|image| image.frames.iter().find(|frame| frame.id == edit_id))
                .map_or(0, AnimationFrame::stored_bytes);
            let required = canvas.len().saturating_sub(old_bytes);
            if !self.make_room(required, image_id, Some(edit_id)) {
                return Err(animation_error(
                    "ENOSPC",
                    "Failed to cache data for image frame",
                ));
            }
            let transient = command.is_transient();
            let edited_storage = FrameStorage::Key(self.store_blob(canvas));
            let image = self.images.get_mut(&image_id).expect("resolved image");
            let index = image
                .frame_index_for_id(edit_id)
                .ok_or_else(|| animation_error("ENOENT", "Frame was evicted"))?;
            let is_current = image.current_frame_index == index;
            let gap = if command.gap() != 0 {
                Some(gap_ms)
            } else {
                None
            };
            let frame = &mut image.frames[index];
            frame.storage = edited_storage;
            frame.transient |= transient;
            if let Some(gap) = gap {
                let old_gap = frame.gap_ms;
                frame.gap_ms = gap;
                image.animation_duration_ms = image
                    .animation_duration_ms
                    .saturating_sub(u64::from(old_gap))
                    .saturating_add(u64::from(gap));
            }
            image.last_access = access;
            (index as u32 + 1, false, is_current, transient)
        } else {
            let (storage, coalesced) = {
                let image = self.images.get(&image_id).expect("resolved image");
                let coalesce = source_id.is_some_and(|source| {
                    image.chain_length_from_id(source) >= self.limits.max_frame_chain
                });
                if coalesce {
                    let _source = source_id.expect("coalescing requires a source");
                    let mut canvas = if command.bgcolor() != 0 {
                        fill_bgcolor(image.width, image.height, command.bgcolor())
                    } else {
                        vec![0u8; image.canvas_bytes()]
                    };
                    if let Some(data) = image.frame_data(source_number) {
                        canvas = data.as_ref().clone();
                    }
                    compose_patch(
                        &mut canvas,
                        image.width,
                        image.height,
                        &patch,
                        decoded.width,
                        decoded.height,
                        x,
                        y,
                        blend,
                    );
                    let blob = self.store_blob(canvas);
                    (FrameStorage::Key(blob), true)
                } else {
                    (
                        FrameStorage::Delta {
                            base_frame_id: source_id.unwrap_or(0),
                            patch: DeltaPatch {
                                x,
                                y,
                                width: decoded.width,
                                height: decoded.height,
                                rgba: self.store_blob(patch),
                                blend,
                            },
                            bgcolor: if source_id.is_none() {
                                command.bgcolor()
                            } else {
                                0
                            },
                        },
                        false,
                    )
                }
            };
            let mut frame = AnimationFrame {
                id: 0,
                width: 0,
                height: 0,
                gap_ms,
                transient: command.is_transient(),
                storage,
            };
            let required = frame.stored_bytes();
            if !self.make_room(required, image_id, source_id) {
                return Err(animation_error(
                    "ENOSPC",
                    "Cache size exceeded cannot add new frames",
                ));
            }
            let image = self.images.get_mut(&image_id).expect("resolved image");
            let id = image.next_frame_id;
            image.next_frame_id = image.next_frame_id.wrapping_add(1);
            frame.id = id;
            frame.width = image.width;
            frame.height = image.height;
            let frame_number = image.frames.len() as u32 + 1;
            image.frames.push(frame);
            image.animation_duration_ms = image
                .animation_duration_ms
                .saturating_add(u64::from(gap_ms));
            image.last_access = access;
            let redraw = image.animation_state == AnimationState::Loading;
            if redraw {
                image.current_frame_shown_at_ms = now_ms;
                image.is_drawn = true;
            }
            let transient = command.is_transient();
            let _ = coalesced;
            (frame_number, true, redraw, transient)
        };
        self.recompute_total();
        if let Some(image) = self.images.get_mut(&image_id) {
            if redraw {
                image.current_frame_shown_at_ms = now_ms;
            }
            image.last_access = access;
        }
        let _ = initial_len;
        let _ = transient;
        Ok(AnimationOutcome::FrameAdded {
            image_id,
            frame_number,
            is_new,
            redraw,
        })
    }

    fn apply_control(
        &mut self,
        command: &GraphicsCommand,
        now_ms: u64,
    ) -> Result<AnimationOutcome, GraphicsError> {
        let image_id = self
            .key_for(command.id, command.image_number)
            .ok_or_else(|| {
                animation_error("ENOENT", "Animation command refers to missing image")
            })?;
        let image = self.images.get_mut(&image_id).expect("resolved image");
        let mut redraw = false;
        if command.gap() != 0 && command.frame_number() != 0 {
            let gap = command.gap().max(0) as u32;
            if let Some(frame) = image.frame_mut(command.frame_number()) {
                let old_gap = frame.gap_ms;
                frame.gap_ms = gap;
                image.animation_duration_ms = image
                    .animation_duration_ms
                    .saturating_sub(u64::from(old_gap))
                    .saturating_add(u64::from(gap));
            }
        }
        let selected = command.other_frame_number();
        if selected != 0 && selected as usize <= image.frames.len() {
            let index = selected as usize - 1;
            if index != image.current_frame_index {
                image.current_frame_index = index;
                image.current_frame_shown_at_ms = now_ms;
                redraw = true;
            }
        }
        let state = command.animation_state();
        if state != 0 {
            let was_stopped = image.animation_state == AnimationState::Stopped;
            image.animation_state = AnimationState::from_u32(state)
                .ok_or_else(|| animation_error("EINVAL", "Unknown animation state"))?;
            if image.animation_state == AnimationState::Stopped {
                image.current_loop = 0;
            } else {
                if was_stopped {
                    image.current_frame_shown_at_ms = now_ms;
                    image.is_drawn = true;
                }
                image.current_loop = 0;
            }
        }
        if command.loop_count() != 0 {
            image.max_loops = command.loop_count() - 1;
        }
        Ok(AnimationOutcome::Control { image_id, redraw })
    }

    fn apply_compose(
        &mut self,
        command: &GraphicsCommand,
    ) -> Result<AnimationOutcome, GraphicsError> {
        let image_id = self
            .key_for(command.id, command.image_number)
            .ok_or_else(|| animation_error("ENOENT", "Compose command refers to missing image"))?;
        let source_number = command.frame_number();
        let destination_number = command.other_frame_number();
        let (
            source,
            mut destination,
            destination_id,
            old_bytes,
            transient,
            image_width,
            image_height,
        ) = {
            let image = self.images.get(&image_id).expect("resolved image");
            let source_index = source_number
                .checked_sub(1)
                .map(|index| index as usize)
                .filter(|index| *index < image.frames.len())
                .ok_or_else(|| animation_error("ENOENT", "No source frame found"))?;
            let destination_index = destination_number
                .checked_sub(1)
                .map(|index| index as usize)
                .filter(|index| *index < image.frames.len())
                .ok_or_else(|| animation_error("ENOENT", "No destination frame found"))?;
            let width = if command.width != 0 {
                command.width
            } else {
                image.width
            };
            let height = if command.height != 0 {
                command.height
            } else {
                image.height
            };
            let source_x = command.cell_x_offset;
            let source_y = command.cell_y_offset;
            let dest_x = command.x_offset;
            let dest_y = command.y_offset;
            let fits = |value: u32, offset: u32, limit: u32| {
                u64::from(offset) + u64::from(value) <= u64::from(limit)
            };
            if !fits(width, dest_x, image.width) || !fits(height, dest_y, image.height) {
                return Err(animation_error(
                    "EINVAL",
                    "The destination rectangle is out of bounds",
                ));
            }
            if !fits(width, source_x, image.width) || !fits(height, source_y, image.height) {
                return Err(animation_error(
                    "EINVAL",
                    "The source rectangle is out of bounds",
                ));
            }
            if source_index == destination_index {
                let x_overlaps = source_x.max(dest_x) < source_x.min(dest_x) + width;
                let y_overlaps = source_y.max(dest_y) < source_y.min(dest_y) + height;
                if x_overlaps && y_overlaps {
                    return Err(animation_error(
                        "EINVAL",
                        "The source and destination rectangles overlap in the same frame",
                    ));
                }
            }
            let source = image
                .frame_data(source_number)
                .ok_or_else(|| animation_error("ENOENT", "No source frame data found"))?
                .as_ref()
                .clone();
            let destination = image
                .frame_data(destination_number)
                .ok_or_else(|| animation_error("ENOENT", "No destination frame data found"))?
                .as_ref()
                .clone();
            let destination_frame = &image.frames[destination_index];
            (
                source,
                destination,
                destination_frame.id,
                destination_frame.stored_bytes(),
                image.frames[source_index].transient,
                image.width,
                image.height,
            )
        };
        let (width, height, source_x, source_y, dest_x, dest_y) = (
            if command.width != 0 {
                command.width
            } else {
                image_width
            },
            if command.height != 0 {
                command.height
            } else {
                image_height
            },
            command.cell_x_offset,
            command.cell_y_offset,
            command.x_offset,
            command.y_offset,
        );
        compose_rect(
            &mut destination,
            image_width,
            image_height,
            &source,
            image_width,
            image_height,
            width,
            height,
            source_x,
            source_y,
            dest_x,
            dest_y,
            command.compose_mode() == 1,
        );
        let required = destination.len().saturating_sub(old_bytes);
        if !self.make_room(required, image_id, Some(destination_id)) {
            return Err(animation_error(
                "ENOSPC",
                "Failed to store image data in cache",
            ));
        }
        let composed_storage = FrameStorage::Key(self.store_blob(destination));
        let image = self.images.get_mut(&image_id).expect("resolved image");
        let index = image
            .frame_index_for_id(destination_id)
            .ok_or_else(|| animation_error("ENOENT", "Destination frame was evicted"))?;
        let frame = &mut image.frames[index];
        frame.storage = composed_storage;
        frame.transient |= transient;
        let redraw = index == image.current_frame_index;
        self.recompute_total();
        Ok(AnimationOutcome::Composed {
            image_id,
            source_frame: source_number,
            destination_frame: destination_number,
            redraw,
        })
    }

    /// Advance every running animation to `now_ms`.
    pub fn tick(&mut self, now_ms: u64) -> AnimationTick {
        let mut redraw = Vec::new();
        let mut next_deadline_ms: Option<u64> = None;
        for (id, image) in &mut self.images {
            if !image.is_animatable() {
                continue;
            }
            let gap = image.current_frame().gap_ms;
            let mut next_at = image
                .current_frame_shown_at_ms
                .saturating_add(u64::from(gap));
            if now_ms >= next_at {
                let frame_count = image.frames.len();
                loop {
                    let mut next = (image.current_frame_index + 1) % frame_count;
                    if next == 0 {
                        if image.animation_state == AnimationState::Loading {
                            next_at = u64::MAX;
                            break;
                        }
                        image.current_loop = image.current_loop.saturating_add(1);
                        if image.max_loops != 0 && image.current_loop >= image.max_loops {
                            next_at = u64::MAX;
                            break;
                        }
                    }
                    image.current_frame_index = next;
                    if image.current_frame().gap_ms != 0 {
                        break;
                    }
                    next = (image.current_frame_index + 1) % frame_count;
                    if next == image.current_frame_index {
                        break;
                    }
                }
                if next_at != u64::MAX {
                    image.current_frame_shown_at_ms = now_ms;
                    redraw.push(*id);
                    next_at = now_ms.saturating_add(u64::from(image.current_frame().gap_ms));
                }
            }
            if next_at != u64::MAX {
                next_deadline_ms = Some(match next_deadline_ms {
                    Some(current) => current.min(next_at),
                    None => next_at,
                });
            }
        }
        redraw.sort_unstable();
        AnimationTick {
            redraw,
            next_deadline_ms,
        }
    }
}

fn animation_error(code: &'static str, message: &str) -> GraphicsError {
    GraphicsError::Animation {
        code,
        message: message.to_string(),
    }
}

fn fill_bgcolor(width: u32, height: u32, color: u32) -> Vec<u8> {
    let px = [
        (color >> 24) as u8,
        (color >> 16) as u8,
        (color >> 8) as u8,
        color as u8,
    ];
    let mut out = vec![0u8; width as usize * height as usize * 4];
    for chunk in out.chunks_mut(4) {
        chunk.copy_from_slice(&px);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn compose_rect(
    under: &mut [u8],
    under_width: u32,
    under_height: u32,
    over: &[u8],
    over_width: u32,
    over_height: u32,
    width: u32,
    height: u32,
    over_offset_x: u32,
    over_offset_y: u32,
    under_offset_x: u32,
    under_offset_y: u32,
    replace: bool,
) {
    let copy_width = width
        .min(under_width.saturating_sub(under_offset_x))
        .min(over_width.saturating_sub(over_offset_x));
    let copy_height = height
        .min(under_height.saturating_sub(under_offset_y))
        .min(over_height.saturating_sub(over_offset_y));
    for row in 0..copy_height {
        for col in 0..copy_width {
            let src = ((over_offset_y + row) as usize * over_width as usize
                + (over_offset_x + col) as usize)
                * 4;
            let dst = ((under_offset_y + row) as usize * under_width as usize
                + (under_offset_x + col) as usize)
                * 4;
            if src + 4 > over.len() || dst + 4 > under.len() {
                return;
            }
            let src_px = [over[src], over[src + 1], over[src + 2], over[src + 3]];
            if replace {
                under[dst..dst + 4].copy_from_slice(&src_px);
            } else {
                blend_pixel(&mut under[dst..dst + 4], src_px);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compose_patch(
    under: &mut [u8],
    under_width: u32,
    under_height: u32,
    over: &[u8],
    over_width: u32,
    over_height: u32,
    offset_x: u32,
    offset_y: u32,
    blend: bool,
) {
    compose_rect(
        under,
        under_width,
        under_height,
        over,
        over_width,
        over_height,
        over_width,
        over_height,
        0,
        0,
        offset_x,
        offset_y,
        !blend,
    );
}

fn blend_pixel(dst: &mut [u8], src: [u8; 4]) {
    let alpha = u32::from(src[3]);
    let dst_alpha = u32::from(dst[3]);
    let inv_alpha = 255 - alpha;
    let denom = alpha * 255 + dst_alpha * inv_alpha;
    if denom == 0 {
        return;
    }
    for channel in 0..3 {
        let num =
            u32::from(src[channel]) * alpha * 255 + u32::from(dst[channel]) * dst_alpha * inv_alpha;
        dst[channel] = ((num as f32 / denom as f32).round() as u32).min(255) as u8;
    }
    let y = denom + 128;
    dst[3] = ((y + (y >> 8)) >> 8).min(255) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::parse_raw;

    fn root(id: u32, width: u32, height: u32, color: [u8; 4]) -> Image {
        let mut command =
            parse_raw(format!("a=t,f=32,s={width},v={height},i={id}").as_bytes()).unwrap();
        let mut rgba = Vec::new();
        for _ in 0..width * height {
            rgba.extend_from_slice(&color);
        }
        command.payload = rgba.clone();
        decode_image(&command, &command.payload, &ImageLimits::default()).unwrap()
    }

    fn simple_frame(image_id: u32, number: u32, color: [u8; 4]) -> GraphicsCommand {
        let mut command =
            parse_raw(format!("a=f,i={image_id},r={number},s=1,v=1").as_bytes()).unwrap();
        command.payload = color.to_vec();
        command
    }

    #[test]
    fn frame_gap_and_tick_advance() {
        let mut manager = AnimationManager::new();
        manager
            .insert_root(&root(1, 2, 2, [10, 0, 0, 255]))
            .unwrap();
        manager
            .feed(
                &simple_frame(1, 2, [20, 0, 0, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
        manager.set_gap(1, 1, 10);
        manager.set_gap(1, 2, 20);
        manager.set_state(1, AnimationState::Running, 1000);
        let tick = manager.tick(1009);
        assert!(tick.redraw.is_empty());
        let tick = manager.tick(1010);
        assert_eq!(tick.redraw, vec![1]);
        assert_eq!(manager.image(1).unwrap().current_frame_index, 1);
        assert_eq!(tick.next_deadline_ms, Some(1030));
    }

    #[test]
    fn looping_wraps_and_finite_loops_stop() {
        let mut manager = AnimationManager::new();
        manager.insert_root(&root(1, 1, 1, [0, 0, 0, 255])).unwrap();
        manager
            .feed(
                &simple_frame(1, 2, [1, 0, 0, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
        manager.set_gap(1, 1, 5);
        manager.set_gap(1, 2, 5);
        manager.set_state(1, AnimationState::Running, 0);
        assert_eq!(manager.tick(5).redraw, vec![1]);
        assert_eq!(manager.image(1).unwrap().current_frame_index, 1);
        assert_eq!(manager.tick(10).redraw, vec![1]);
        assert_eq!(manager.image(1).unwrap().current_frame_index, 0);

        manager.set_state(1, AnimationState::Stopped, 0);
        manager.set_state(1, AnimationState::Running, 0);
        let mut control = parse_raw(b"a=a,i=1,v=2").unwrap();
        control.payload.clear();
        manager.feed(&control, &ImageLimits::default(), 0).unwrap();
        assert_eq!(manager.image(1).unwrap().max_loops, 1);
        assert_eq!(manager.tick(5).redraw, vec![1]);
        assert_eq!(manager.image(1).unwrap().current_frame_index, 1);
        assert!(manager.tick(10).redraw.is_empty());
        assert_eq!(manager.image(1).unwrap().current_frame_index, 1);
    }

    #[test]
    fn loading_mode_waits_at_last_frame() {
        let mut manager = AnimationManager::new();
        manager.insert_root(&root(1, 1, 1, [0, 0, 0, 255])).unwrap();
        manager
            .feed(
                &simple_frame(1, 2, [1, 0, 0, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
        manager.set_gap(1, 1, 5);
        manager.set_gap(1, 2, 5);
        let mut control = parse_raw(b"a=a,i=1,s=2").unwrap();
        control.payload.clear();
        manager.feed(&control, &ImageLimits::default(), 0).unwrap();
        assert_eq!(manager.tick(5).redraw, vec![1]);
        assert_eq!(manager.image(1).unwrap().current_frame_index, 1);
        assert!(manager.tick(100).redraw.is_empty());
        assert_eq!(manager.image(1).unwrap().current_frame_index, 1);
    }

    #[test]
    fn stopping_resets_and_stops_redraws() {
        let mut manager = AnimationManager::new();
        manager.insert_root(&root(1, 1, 1, [0, 0, 0, 255])).unwrap();
        manager
            .feed(
                &simple_frame(1, 2, [1, 0, 0, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
        manager.set_gap(1, 1, 5);
        manager.set_state(1, AnimationState::Running, 0);
        let mut control = parse_raw(b"a=a,i=1,s=1").unwrap();
        control.payload.clear();
        manager.feed(&control, &ImageLimits::default(), 0).unwrap();
        assert_eq!(
            manager.image(1).unwrap().animation_state,
            AnimationState::Stopped
        );
        assert!(manager.tick(1000).redraw.is_empty());
    }

    #[test]
    fn frame_selection_and_gap_control() {
        let mut manager = AnimationManager::new();
        manager.insert_root(&root(1, 1, 1, [0, 0, 0, 255])).unwrap();
        manager
            .feed(
                &simple_frame(1, 2, [1, 0, 0, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
        let mut control = parse_raw(b"a=a,i=1,c=2,r=1,z=12").unwrap();
        control.payload.clear();
        let outcome = manager.feed(&control, &ImageLimits::default(), 50).unwrap();
        assert!(matches!(
            outcome,
            AnimationOutcome::Control { redraw: true, .. }
        ));
        assert_eq!(manager.image(1).unwrap().current_frame_index, 1);
        assert_eq!(manager.image(1).unwrap().frames[0].gap_ms, 12);
        assert_eq!(manager.image(1).unwrap().current_frame_shown_at_ms, 50);
    }

    #[test]
    fn gapless_frames_are_skipped() {
        let mut manager = AnimationManager::new();
        manager.insert_root(&root(1, 1, 1, [0, 0, 0, 255])).unwrap();
        manager
            .feed(
                &simple_frame(1, 2, [1, 0, 0, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
        manager
            .feed(
                &simple_frame(1, 3, [2, 0, 0, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
        manager.set_gap(1, 1, 5);
        manager.set_gap(1, 2, 0);
        manager.set_gap(1, 3, 5);
        manager.set_state(1, AnimationState::Running, 0);
        assert_eq!(manager.tick(5).redraw, vec![1]);
        assert_eq!(manager.image(1).unwrap().current_frame_index, 2);
    }

    #[test]
    fn partial_frame_and_background_color_composition() {
        let mut manager = AnimationManager::new();
        manager.insert_root(&root(1, 2, 2, [0, 0, 0, 0])).unwrap();
        let mut command = parse_raw(b"a=f,i=1,r=2,x=1,y=1,s=1,v=1,Y=4278190335").unwrap();
        command.payload = vec![9, 8, 7, 255];
        manager.feed(&command, &ImageLimits::default(), 0).unwrap();
        let frame = manager.image(1).unwrap().frame_data(2).unwrap();
        assert_eq!(&frame[0..4], &[255, 0, 0, 255]);
        let index = 3 * 4;
        assert_eq!(&frame[index..index + 4], &[9, 8, 7, 255]);
    }

    #[test]
    fn frame_edit_existing_frame() {
        let mut manager = AnimationManager::new();
        manager.insert_root(&root(1, 2, 1, [1, 1, 1, 255])).unwrap();
        manager
            .feed(
                &simple_frame(1, 2, [2, 2, 2, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
        let mut command = parse_raw(b"a=f,i=1,r=2,x=1,y=0,s=1,v=1").unwrap();
        command.payload = vec![7, 7, 7, 255];
        manager.feed(&command, &ImageLimits::default(), 0).unwrap();
        let frame = manager.image(1).unwrap().frame_data(2).unwrap();
        assert_eq!(&frame[0..4], &[2, 2, 2, 255]);
        assert_eq!(&frame[4..8], &[7, 7, 7, 255]);
    }

    #[test]
    fn compose_blends_and_replaces() {
        let mut manager = AnimationManager::new();
        manager.insert_root(&root(1, 2, 1, [0, 0, 0, 128])).unwrap();
        let mut frame2 = simple_frame(1, 2, [10, 20, 30, 255]);
        frame2.data_width = 2;
        frame2.payload = vec![10, 20, 30, 255, 40, 50, 60, 255];
        manager.feed(&frame2, &ImageLimits::default(), 0).unwrap();
        let mut compose = parse_raw(b"a=c,i=1,r=1,c=2,w=1,h=1,x=1,y=0,X=0,Y=0,C=1").unwrap();
        compose.payload.clear();
        manager.feed(&compose, &ImageLimits::default(), 0).unwrap();
        let rgba = manager.image(1).unwrap().frame_data(2).unwrap();
        assert_eq!(&rgba[4..8], &[0, 0, 0, 128]);

        let mut blend = parse_raw(b"a=c,i=1,r=1,c=2,w=1,h=1,x=0,y=0,X=0,Y=0").unwrap();
        blend.payload.clear();
        manager.feed(&blend, &ImageLimits::default(), 0).unwrap();
        let rgba = manager.image(1).unwrap().frame_data(2).unwrap();
        assert_eq!(&rgba[0..4], &[5, 10, 15, 255]);
    }

    #[test]
    fn compose_rejects_bad_rectangles() {
        let mut manager = AnimationManager::new();
        manager.insert_root(&root(1, 2, 2, [0, 0, 0, 255])).unwrap();
        let mut command = parse_raw(b"a=c,i=1,r=1,c=1,w=2,h=2,x=1,y=1").unwrap();
        command.payload.clear();
        let error = manager
            .feed(&command, &ImageLimits::default(), 0)
            .unwrap_err();
        assert_eq!(error.code(), "EINVAL");

        let mut command = parse_raw(b"a=c,i=1,r=1,c=1,w=3,h=1").unwrap();
        command.payload.clear();
        let error = manager
            .feed(&command, &ImageLimits::default(), 0)
            .unwrap_err();
        assert_eq!(error.code(), "EINVAL");

        let mut command = parse_raw(b"a=c,i=1,r=2,c=1").unwrap();
        command.payload.clear();
        let error = manager
            .feed(&command, &ImageLimits::default(), 0)
            .unwrap_err();
        assert_eq!(error.code(), "ENOENT");
    }

    #[test]
    fn delete_frame_adjusts_current_and_duration() {
        let mut manager = AnimationManager::new();
        manager.insert_root(&root(1, 1, 1, [0, 0, 0, 255])).unwrap();
        manager
            .feed(
                &simple_frame(1, 2, [1, 0, 0, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
        manager
            .feed(
                &simple_frame(1, 3, [2, 0, 0, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
        manager.set_gap(1, 2, 10);
        manager.set_gap(1, 3, 20);
        manager.select_frame(1, 3, 0);
        assert!(manager.delete_frame(1, 2));
        let image = manager.image(1).unwrap();
        assert_eq!(image.frames.len(), 2);
        assert_eq!(image.current_frame_index, 1);
        assert_eq!(image.animation_duration_ms, 20);

        assert!(manager.delete_frame(1, 1));
        let image = manager.image(1).unwrap();
        assert_eq!(image.frames.len(), 1);
        assert_eq!(image.current_frame_index, 0);
    }

    #[test]
    fn indexed_images_cannot_animate() {
        let mut command = parse_raw(b"a=t,f=8,s=1,v=1,i=1").unwrap();
        command.payload = vec![0];
        let image = decode_image(&command, &command.payload, &ImageLimits::default()).unwrap();
        let mut manager = AnimationManager::new();
        assert_eq!(manager.insert_root(&image).unwrap_err().code(), "EINVAL");
    }

    #[test]
    fn chunked_frame_transmission() {
        let mut manager = AnimationManager::new();
        manager.insert_root(&root(1, 1, 2, [0, 0, 0, 255])).unwrap();
        let mut first = parse_raw(b"a=f,i=1,r=2,s=1,v=2,m=1").unwrap();
        first.payload = vec![1, 2];
        assert_eq!(
            manager.feed(&first, &ImageLimits::default(), 0).unwrap(),
            AnimationOutcome::ChunkAccepted
        );
        let mut last = parse_raw(b"a=f,m=0").unwrap();
        last.payload = vec![3, 255, 4, 255, 5, 255];
        manager.feed(&last, &ImageLimits::default(), 0).unwrap();
        let frame = manager.image(1).unwrap().frame_data(2).unwrap();
        assert_eq!(frame.as_slice(), &[1, 2, 3, 255, 4, 255, 5, 255]);
    }

    #[test]
    fn delete_frame_command_removes_one_frame() {
        let mut manager = AnimationManager::new();
        manager.insert_root(&root(1, 1, 1, [0, 0, 0, 255])).unwrap();
        manager
            .feed(
                &simple_frame(1, 2, [1, 0, 0, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
        manager
            .feed(
                &simple_frame(1, 3, [2, 0, 0, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
        let mut delete = parse_raw(b"a=d,d=f,i=1,r=2").unwrap();
        delete.payload.clear();
        let outcome = manager.feed(&delete, &ImageLimits::default(), 0).unwrap();
        assert!(matches!(
            outcome,
            AnimationOutcome::Control { redraw: true, .. }
        ));
        assert_eq!(manager.image(1).unwrap().frames.len(), 2);
        assert_eq!(
            manager.image(1).unwrap().frame_data(2).unwrap().as_slice(),
            &[2, 0, 0, 255]
        );
    }

    #[test]
    fn disk_cache_spills_frames_and_cleans_up_on_drop() {
        let root_dir =
            std::env::temp_dir().join(format!("kitr-animation-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root_dir);
        let mut manager = AnimationManager::new_with_disk_cache(&root_dir).unwrap();
        let cache_dir = manager.disk_cache_dir().unwrap().to_path_buf();
        manager.insert_root(&root(1, 1, 1, [0, 0, 0, 255])).unwrap();
        manager
            .feed(
                &simple_frame(1, 2, [1, 2, 3, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
        assert!(cache_dir.is_dir());
        assert!(std::fs::read_dir(&cache_dir).unwrap().next().is_some());
        assert_eq!(
            manager.image(1).unwrap().frame_data(2).unwrap().as_slice(),
            &[1, 2, 3, 255]
        );
        drop(manager);
        assert!(
            !cache_dir.exists(),
            "cache directory must be removed on exit"
        );
        let _ = std::fs::remove_dir_all(&root_dir);
    }

    #[test]
    fn transient_frame_marks_image_frame() {
        let mut manager = AnimationManager::new();
        manager.insert_root(&root(1, 1, 1, [0, 0, 0, 255])).unwrap();
        let mut command = parse_raw(b"a=f,i=1,r=2,s=1,v=1,N=1").unwrap();
        command.payload = vec![1, 2, 3, 255];
        manager.feed(&command, &ImageLimits::default(), 0).unwrap();
        assert!(manager.image(1).unwrap().frames[1].transient);
    }
}
