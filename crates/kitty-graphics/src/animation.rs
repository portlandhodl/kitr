// SPDX-License-Identifier: GPL-3.0-or-later
//! Animation frames, composition and terminal-driven timing (`a=f`, `a=a`,
//! `a=c`).
//!
//! Frames are stored fully coalesced as RGBA8, which keeps rendering simple and
//! makes composition independent of transmission order. The timing model
//! follows `kitty/graphics.c`: a frame's gap is the delay before advancing to
//! the next frame, zero-gap frames are skipped, loading mode waits at the end,
//! and loop count zero means infinite.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::command::{Action, DeleteAction, GraphicsCommand};
use crate::image::{decode_image, GraphicsError, Image, ImageFormat, ImageLimits};

/// Default inter-frame gap used when a new frame omits `z=`.
pub const DEFAULT_FRAME_GAP_MS: u32 = 40;

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

/// One fully composited animation frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnimationFrame {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub gap_ms: u32,
    pub transient: bool,
    pub rgba: Arc<Vec<u8>>,
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
}

impl AnimatedImage {
    pub fn current_frame(&self) -> &AnimationFrame {
        &self.frames[self.current_frame_index]
    }

    pub fn current_rgba(&self) -> &[u8] {
        &self.current_frame().rgba
    }

    /// Shared handle to the current frame's RGBA pixels.
    pub fn current_data(&self) -> Arc<Vec<u8>> {
        self.current_frame().rgba.clone()
    }

    /// Frame by 1-based protocol number (`1` is the root frame).
    pub fn frame(&self, number: u32) -> Option<&AnimationFrame> {
        if number == 0 {
            return None;
        }
        self.frames.get(number as usize - 1)
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
#[derive(Clone, Debug, Default)]
pub struct AnimationManager {
    images: BTreeMap<u32, AnimatedImage>,
    pending: Option<PendingFrame>,
}

impl AnimationManager {
    pub fn new() -> Self {
        Self::default()
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

    pub fn clear(&mut self) {
        self.images.clear();
        self.pending = None;
    }

    pub fn has_pending_frame(&self) -> bool {
        self.pending.is_some()
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
                    rgba: Arc::new(rgba),
                }],
                current_frame_index: 0,
                animation_state: AnimationState::Stopped,
                max_loops: 0,
                current_loop: 0,
                current_frame_shown_at_ms: 0,
                animation_duration_ms: 0,
                is_drawn: true,
                next_frame_id: 2,
            },
        );
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
        self.images.remove(&id).is_some()
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
        let image = self.images.get_mut(&image_id).expect("resolved image");
        if image.animation_state == AnimationState::Loading {
            image.is_drawn = true;
        }
        if decoded.width > image.width || decoded.height > image.height {
            return Err(animation_error(
                "EINVAL",
                "Frame is larger than the animation image",
            ));
        }
        let frame_number = {
            let requested = command.frame_number();
            let last = image.frames.len() as u32;
            if requested == 0 || requested > last + 1 {
                last + 1
            } else {
                requested
            }
        };
        let is_new = frame_number == image.frames.len() as u32 + 1;
        let gap_ms = match command.gap() {
            gap if gap > 0 => gap as u32,
            gap if gap < 0 => 0,
            _ => DEFAULT_FRAME_GAP_MS,
        };
        let blend = command.blend_mode() != 1 && !decoded.is_opaque;
        let x = command.x_offset;
        let y = command.y_offset;

        let redraw;
        if is_new {
            let mut canvas = if command.other_frame_number() != 0 {
                let other = image
                    .frame(command.other_frame_number())
                    .ok_or_else(|| animation_error("EINVAL", "No source frame found"))?;
                other.rgba.as_ref().clone()
            } else if command.bgcolor() != 0 {
                fill_bgcolor(image.width, image.height, command.bgcolor())
            } else {
                vec![0u8; image.width as usize * image.height as usize * 4]
            };
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
            let id = image.next_frame_id;
            image.next_frame_id = image.next_frame_id.wrapping_add(1);
            let transient = command.is_transient();
            image.frames.push(AnimationFrame {
                id,
                width: image.width,
                height: image.height,
                gap_ms,
                transient,
                rgba: Arc::new(canvas),
            });
            image.animation_duration_ms = image
                .animation_duration_ms
                .saturating_add(u64::from(gap_ms));
            redraw = image.animation_state == AnimationState::Loading;
            if redraw {
                image.current_frame_shown_at_ms = now_ms;
                image.is_drawn = true;
            }
        } else {
            let gap = if command.gap() != 0 {
                Some(gap_ms)
            } else {
                None
            };
            let is_current = image.current_frame_index == frame_number as usize - 1;
            let transient = command.is_transient();
            let image_width = image.width;
            let image_height = image.height;
            let frame = image
                .frame_mut(frame_number)
                .expect("frame number validated");
            let mut canvas = frame.rgba.as_ref().clone();
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
            frame.rgba = Arc::new(canvas);
            frame.transient |= transient;
            if let Some(gap) = gap {
                let old_gap = frame.gap_ms;
                frame.gap_ms = gap;
                image.animation_duration_ms = image
                    .animation_duration_ms
                    .saturating_sub(u64::from(old_gap))
                    .saturating_add(u64::from(gap));
            }
            redraw = is_current;
        }
        if redraw {
            image.current_frame_shown_at_ms = now_ms;
        }
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
        let image = self.images.get_mut(&image_id).expect("resolved image");
        let source_number = command.frame_number();
        let destination_number = command.other_frame_number();
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
        let source = image.frames[source_index].rgba.as_ref().clone();
        let mut destination = image.frames[destination_index].rgba.as_ref().clone();
        compose_rect(
            &mut destination,
            image.width,
            image.height,
            &source,
            image.width,
            image.height,
            width,
            height,
            source_x,
            source_y,
            dest_x,
            dest_y,
            command.compose_mode() == 1,
        );
        let transient = image.frames[source_index].transient;
        let frame = &mut image.frames[destination_index];
        frame.rgba = Arc::new(destination);
        frame.transient |= transient;
        let redraw = destination_index == image.current_frame_index;
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
        let frame = &manager.image(1).unwrap().frames[1];
        assert_eq!(&frame.rgba[0..4], &[255, 0, 0, 255]);
        let index = 3 * 4;
        assert_eq!(&frame.rgba[index..index + 4], &[9, 8, 7, 255]);
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
        let frame = &manager.image(1).unwrap().frames[1];
        assert_eq!(&frame.rgba[0..4], &[2, 2, 2, 255]);
        assert_eq!(&frame.rgba[4..8], &[7, 7, 7, 255]);
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
        let rgba = manager.image(1).unwrap().frames[1].rgba.clone();
        assert_eq!(&rgba[4..8], &[0, 0, 0, 128]);

        let mut blend = parse_raw(b"a=c,i=1,r=1,c=2,w=1,h=1,x=0,y=0,X=0,Y=0").unwrap();
        blend.payload.clear();
        manager.feed(&blend, &ImageLimits::default(), 0).unwrap();
        let rgba = manager.image(1).unwrap().frames[1].rgba.clone();
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
        let frame = &manager.image(1).unwrap().frames[1];
        assert_eq!(frame.rgba.as_slice(), &[1, 2, 3, 255, 4, 255, 5, 255]);
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
            manager.image(1).unwrap().frames[1].rgba.as_slice(),
            &[2, 0, 0, 255]
        );
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
