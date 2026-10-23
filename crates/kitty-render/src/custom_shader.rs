// SPDX-License-Identifier: GPL-3.0-or-later
//! Kitty's animated, multi-group custom-shader post-processing model.
//!
//! A pipeline is an ordered list of groups. Every group runs its shaders on
//! the output of the previous group and either writes back to the backbuffer
//! or into one of the named textures (`a`, `b`, `persist`). Groups can be
//! always active, or started/stopped by animation events. This module contains
//! the CPU-side definition, `.pipeline` parser and animation state machine; the
//! GL backend compiles the group sources and executes the passes.
//!
//! Event, duration and progress semantics follow `update_custom_shader_animations()`
//! and `custom_shader_group_progress()` in `kitty/shaders.c`.

use std::fmt;

/// Bitmask of animation events, using kitty's bit assignments.
pub type ShaderEventMask = u64;

pub const SHADER_ANIM_EVENT_POINTER_LEFT_BUTTON_PRESS: ShaderEventMask = 1 << 0;
pub const SHADER_ANIM_EVENT_OS_WINDOW_FOCUS_IN: ShaderEventMask = 1 << 1;
pub const SHADER_ANIM_EVENT_OS_WINDOW_FOCUS_OUT: ShaderEventMask = 1 << 2;
pub const SHADER_ANIM_EVENT_WINDOW_FOCUS_IN: ShaderEventMask = 1 << 3;
pub const SHADER_ANIM_EVENT_WINDOW_FOCUS_OUT: ShaderEventMask = 1 << 4;
pub const SHADER_ANIM_EVENT_TAB_CHANGE: ShaderEventMask = 1 << 5;
pub const SHADER_ANIM_EVENT_BELL_IN_WINDOW: ShaderEventMask = 1 << 6;
pub const SHADER_ANIM_EVENT_USER_ACTIVITY: ShaderEventMask = 1 << 7;
pub const SHADER_ANIM_EVENT_USER_IDLE: ShaderEventMask = 1 << 8;
pub const SHADER_ANIM_EVENT_CURSOR_TRAIL_MOVE: ShaderEventMask = 1 << 9;
pub const SHADER_ANIM_EVENT_CURSOR_TRAIL_STOP: ShaderEventMask = 1 << 10;
pub const SHADER_ANIM_FIRST_USER_EVENT: u32 = 11;

/// The named output textures understood by the pipeline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NamedTexture {
    #[default]
    Default,
    A,
    B,
    Persist,
}

impl NamedTexture {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "default" | "backbuffer" => Some(Self::Default),
            "a" => Some(Self::A),
            "b" => Some(Self::B),
            "persist" => Some(Self::Persist),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::A => "a",
            Self::B => "b",
            Self::Persist => "persist",
        }
    }
}

/// One group of shaders and its animation configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct CustomShaderGroup {
    pub viewport_pos: (f32, f32),
    pub viewport_size: (f32, f32),
    pub output_texture: NamedTexture,
    /// Shader names (for a parsed `.pipeline`) or sources (for a compiled
    /// pipeline); the backend uses the parallel source list.
    pub shaders: Vec<String>,
    pub start_events: ShaderEventMask,
    pub toggle_events: ShaderEventMask,
    pub end_events: ShaderEventMask,
    /// How long after starting the group stops by itself. `0` means never.
    pub end_duration_ms: i64,
    /// Time for `animation_progress` to reach one. `-1` uses the stop duration.
    pub duration_ms: i64,
    /// Time to reverse to zero after stopping. `0` disables reverse, `-1`
    /// uses `duration_ms`.
    pub reverse_ms: i64,
    pub loops: u32,
    pub step_ms: u64,
    pub attached: bool,
}

impl Default for CustomShaderGroup {
    fn default() -> Self {
        Self {
            viewport_pos: (0.0, 0.0),
            viewport_size: (1.0, 1.0),
            output_texture: NamedTexture::Default,
            shaders: Vec::new(),
            start_events: 0,
            toggle_events: 0,
            end_events: 0,
            end_duration_ms: -1,
            duration_ms: -1,
            reverse_ms: 0,
            loops: 1,
            step_ms: 16,
            attached: false,
        }
    }
}

impl CustomShaderGroup {
    pub fn has_own_animation(&self) -> bool {
        self.start_events != 0 || self.toggle_events != 0
    }

    pub fn is_animated(&self) -> bool {
        self.attached || self.has_own_animation()
    }

    pub fn stop_duration(&self, cursor_stop_ms: i64) -> i64 {
        if self.end_duration_ms < 0 {
            cursor_stop_ms
        } else {
            self.end_duration_ms
        }
    }

    pub fn ramp_duration(&self, cursor_stop_ms: i64) -> i64 {
        if self.duration_ms < 0 {
            self.stop_duration(cursor_stop_ms)
        } else {
            self.duration_ms
        }
    }

    pub fn reverse_duration(&self, cursor_stop_ms: i64) -> i64 {
        if self.reverse_ms < 0 {
            self.ramp_duration(cursor_stop_ms)
        } else {
            self.reverse_ms
        }
    }
}

/// A complete custom-shader pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct CustomShaderPipeline {
    pub name: String,
    pub textures: Vec<NamedTexture>,
    pub groups: Vec<CustomShaderGroup>,
    pub user_events: Vec<String>,
}

impl CustomShaderPipeline {
    pub fn new(name: impl Into<String>, groups: Vec<CustomShaderGroup>) -> Self {
        let mut user_events = Vec::new();
        for group in &groups {
            collect_user_events(group.start_events, &mut user_events);
            collect_user_events(group.toggle_events, &mut user_events);
            collect_user_events(group.end_events, &mut user_events);
        }
        Self {
            name: name.into(),
            textures: Vec::new(),
            groups,
            user_events,
        }
    }

    /// Parse kitty's `.pipeline` syntax.
    pub fn parse(name: &str, text: &str) -> Result<Self, CustomShaderError> {
        let mut groups: Vec<CustomShaderGroup> = Vec::new();
        let mut current: Option<CustomShaderGroup> = None;
        let mut textures = Vec::new();
        let mut user_events: Vec<String> = Vec::new();
        let mut had_animation_stop = false;

        let commit = |groups: &mut Vec<CustomShaderGroup>,
                      current: &mut Option<CustomShaderGroup>,
                      had_animation_stop: bool|
         -> Result<(), CustomShaderError> {
            if let Some(group) = current.take() {
                if group.toggle_events != 0 && group.start_events == 0 && !had_animation_stop {
                    // A toggle-only group stays on until toggled off.
                    let mut group = group;
                    group.end_duration_ms = 0;
                    groups.push(group);
                } else {
                    groups.push(group);
                }
            }
            Ok(())
        };

        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let key = parts.next().unwrap_or("");
            if current.is_none() {
                match key {
                    "slot" => {}
                    "textures" => {
                        textures.clear();
                        for name in parts {
                            textures.push(NamedTexture::from_name(name).ok_or_else(|| {
                                CustomShaderError::Parse(format!("unknown texture {name:?}"))
                            })?);
                        }
                    }
                    "startgroup" => {
                        commit(&mut groups, &mut current, had_animation_stop)?;
                        current = Some(CustomShaderGroup::default());
                        had_animation_stop = false;
                    }
                    "var" => {}
                    _ => {
                        return Err(CustomShaderError::Parse(format!(
                            "unknown top-level key {key:?}"
                        )))
                    }
                }
            } else {
                let group = current.as_mut().expect("group is active");
                match key {
                    "viewport_pos" => {
                        let values: Vec<f32> = parts.filter_map(|v| v.parse().ok()).collect();
                        if let Some(x) = values.first() {
                            group.viewport_pos = (*x, *values.get(1).unwrap_or(x));
                        }
                    }
                    "viewport_size" => {
                        let values: Vec<f32> = parts.filter_map(|v| v.parse().ok()).collect();
                        if let Some(x) = values.first() {
                            group.viewport_size = (*x, *values.get(1).unwrap_or(x));
                        }
                    }
                    "output_texture" => {
                        let name = parts.next().unwrap_or("");
                        group.output_texture = NamedTexture::from_name(name).ok_or_else(|| {
                            CustomShaderError::Parse(format!("unknown output texture {name:?}"))
                        })?;
                    }
                    "shaders" => group.shaders.extend(parts.map(str::to_owned)),
                    "var" => {}
                    "animation_start" => {
                        group.start_events = parse_events(parts, &mut user_events)?;
                    }
                    "animation_toggle" => {
                        group.toggle_events = parse_events(parts, &mut user_events)?;
                    }
                    "animation_stop" => {
                        had_animation_stop = true;
                        let text: String = parts.collect();
                        if text.is_empty() || text == "never" {
                            group.end_events = 0;
                            group.end_duration_ms = 0;
                        } else {
                            let mut events = 0;
                            let mut duration = 0;
                            for token in text.split('|') {
                                if let Ok(value) = token.parse::<i64>() {
                                    duration = value.saturating_mul(1_000_000);
                                } else {
                                    events |=
                                        parse_events(token.split_whitespace(), &mut user_events)?;
                                }
                            }
                            group.end_events = events;
                            group.end_duration_ms = if duration == 0 {
                                0
                            } else {
                                duration / 1_000_000
                            };
                        }
                    }
                    "animation_duration" => {
                        let value: i64 =
                            parts.next().and_then(|v| v.parse().ok()).ok_or_else(|| {
                                CustomShaderError::Parse(
                                    "animation_duration requires milliseconds".into(),
                                )
                            })?;
                        group.duration_ms = value;
                    }
                    "animation_reverse" => {
                        let value = parts.next().unwrap_or("yes");
                        group.reverse_ms = match value {
                            "yes" => -1,
                            "no" => 0,
                            other => other.parse().unwrap_or(-1),
                        };
                    }
                    "animation_loops" => {
                        group.loops = parts
                            .next()
                            .and_then(|v| v.parse().ok())
                            .filter(|v| *v >= 1)
                            .ok_or_else(|| {
                                CustomShaderError::Parse(
                                    "animation_loops requires a positive number".into(),
                                )
                            })?;
                    }
                    "animation_step" => {
                        group.step_ms =
                            parts.next().and_then(|v| v.parse().ok()).ok_or_else(|| {
                                CustomShaderError::Parse(
                                    "animation_step requires milliseconds".into(),
                                )
                            })?;
                    }
                    "animation_curve" => {}
                    "attach" => group.attached = true,
                    "endgroup" => commit(&mut groups, &mut current, had_animation_stop)?,
                    _ => {
                        return Err(CustomShaderError::Parse(format!(
                            "unknown group key {key:?}"
                        )))
                    }
                }
            }
        }
        if current.is_some() {
            return Err(CustomShaderError::Parse("unclosed group".into()));
        }
        if groups.is_empty() {
            groups.push(CustomShaderGroup::default());
        }
        if groups[0].attached {
            return Err(CustomShaderError::Parse(
                "the first group cannot use attach".into(),
            ));
        }
        let last = groups.last().expect("non-empty");
        if last.output_texture != NamedTexture::Default {
            return Err(CustomShaderError::Parse(
                "the final group cannot output to a named texture".into(),
            ));
        }
        if last.viewport_pos != (0.0, 0.0) || last.viewport_size != (1.0, 1.0) {
            return Err(CustomShaderError::Parse(
                "the final group must use the full viewport".into(),
            ));
        }
        Ok(Self {
            name: name.into(),
            textures,
            groups,
            user_events,
        })
    }

    pub fn user_event_bit(&self, name: &str) -> ShaderEventMask {
        self.user_events
            .iter()
            .position(|event| event == name)
            .map(|index| 1u64 << (SHADER_ANIM_FIRST_USER_EVENT + index as u32))
            .unwrap_or(0)
    }
}

fn collect_user_events(mask: ShaderEventMask, events: &mut Vec<String>) {
    let mut bit = SHADER_ANIM_FIRST_USER_EVENT;
    while bit < 64 {
        if mask & (1u64 << bit) != 0 {
            let index = (bit - SHADER_ANIM_FIRST_USER_EVENT) as usize;
            while events.len() <= index {
                events.push(String::new());
            }
        }
        bit += 1;
    }
}

fn parse_events<'a>(
    mut parts: impl Iterator<Item = &'a str>,
    user_events: &mut Vec<String>,
) -> Result<ShaderEventMask, CustomShaderError> {
    let text = parts.by_ref().collect::<Vec<_>>().join(" ");
    let text = text.trim();
    if text.is_empty() || text == "none" {
        return Ok(0);
    }
    let mut mask = 0;
    for name in text.split('|').flat_map(|part| part.split_whitespace()) {
        mask |= event_bit(name, user_events)?;
    }
    Ok(mask)
}

fn event_bit(
    name: &str,
    user_events: &mut Vec<String>,
) -> Result<ShaderEventMask, CustomShaderError> {
    let local = name.strip_prefix("user:").unwrap_or("");
    if !local.is_empty() {
        let index = if let Some(index) = user_events.iter().position(|event| event == local) {
            index
        } else {
            user_events.push(local.to_owned());
            user_events.len() - 1
        };
        if index >= 53 {
            return Err(CustomShaderError::Parse(
                "too many user animation events".into(),
            ));
        }
        return Ok(1u64 << (SHADER_ANIM_FIRST_USER_EVENT + index as u32));
    }
    Ok(match name {
        "pointer-left-button-press" => SHADER_ANIM_EVENT_POINTER_LEFT_BUTTON_PRESS,
        "os-window-focus-in" => SHADER_ANIM_EVENT_OS_WINDOW_FOCUS_IN,
        "os-window-focus-out" => SHADER_ANIM_EVENT_OS_WINDOW_FOCUS_OUT,
        "window-focus-in" => SHADER_ANIM_EVENT_WINDOW_FOCUS_IN,
        "window-focus-out" => SHADER_ANIM_EVENT_WINDOW_FOCUS_OUT,
        "tab-change" => SHADER_ANIM_EVENT_TAB_CHANGE,
        "bell-in-window" => SHADER_ANIM_EVENT_BELL_IN_WINDOW,
        "user-activity" => SHADER_ANIM_EVENT_USER_ACTIVITY,
        "user-idle" => SHADER_ANIM_EVENT_USER_IDLE,
        "cursor-trail-move" => SHADER_ANIM_EVENT_CURSOR_TRAIL_MOVE,
        "cursor-trail-stop" => SHADER_ANIM_EVENT_CURSOR_TRAIL_STOP,
        _ => {
            return Err(CustomShaderError::Parse(format!(
                "unknown animation event {name:?}"
            )))
        }
    })
}

/// Per-group animation state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShaderGroupAnimState {
    pub active: bool,
    pub started_at_ms: u64,
    pub ramp_started_at_ms: u64,
    pub reverse_ends_at_ms: u64,
    pub loops_left: u32,
    pub looping: bool,
}

/// Cached state for one pipeline on one surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomShaderAnimationState {
    pub groups: Vec<ShaderGroupAnimState>,
    pub has_active_shaders: bool,
    pub min_step_ms: u64,
    pub next_end_at_ms: u64,
}

impl CustomShaderAnimationState {
    pub const NEVER: u64 = u64::MAX;

    pub fn new(pipeline: &CustomShaderPipeline) -> Self {
        let mut state = Self {
            groups: vec![ShaderGroupAnimState::default(); pipeline.groups.len()],
            has_active_shaders: false,
            min_step_ms: Self::NEVER,
            next_end_at_ms: Self::NEVER,
        };
        let mut previous = false;
        for (index, group) in pipeline.groups.iter().enumerate() {
            let initially_active = if group.attached {
                previous
            } else {
                !group.has_own_animation()
            };
            if group.attached && initially_active {
                state.groups[index].active = true;
            }
            if initially_active {
                state.has_active_shaders = true;
            }
            previous = initially_active;
        }
        state
    }

    /// Update the state machine with this tick's events.
    pub fn update(
        &mut self,
        pipeline: &CustomShaderPipeline,
        events: ShaderEventMask,
        now_ms: u64,
        cursor_stop_ms: i64,
    ) -> u64 {
        if events == 0 && now_ms < self.next_end_at_ms {
            return self.min_step_ms;
        }
        let mut any_active = false;
        let mut min_step = Self::NEVER;
        let mut next_end = Self::NEVER;
        let mut previous_active = false;
        for (index, group) in pipeline.groups.iter().enumerate() {
            let this_active;
            if !group.is_animated() {
                any_active = true;
                this_active = true;
                min_step = min_step.min(group.step_ms);
            } else {
                let stop_duration = group.stop_duration(cursor_stop_ms);
                let previous_started = index
                    .checked_sub(1)
                    .map(|previous| self.groups[previous].started_at_ms)
                    .unwrap_or(0);
                let state = &mut self.groups[index];
                if group.has_own_animation() {
                    advance_stopped_group(group, state, now_ms, cursor_stop_ms);
                    let was_on = state.active && state.reverse_ends_at_ms == 0 && !state.looping;
                    let focus_out_mask =
                        SHADER_ANIM_EVENT_OS_WINDOW_FOCUS_OUT | SHADER_ANIM_EVENT_WINDOW_FOCUS_OUT;
                    let hard_stop = group.end_events & events & focus_out_mask != 0;
                    let mut turn_on = false;
                    let mut turn_off = false;
                    if was_on {
                        turn_off = (group.end_events != 0 && events & group.end_events != 0)
                            || (stop_duration > 0
                                && now_ms.saturating_sub(state.started_at_ms)
                                    >= stop_duration as u64);
                    }
                    if events & group.start_events != 0 && !hard_stop {
                        turn_on = true;
                    }
                    if events & group.toggle_events != 0 {
                        turn_on = !was_on && !hard_stop;
                        turn_off = was_on;
                    }
                    if turn_on {
                        start_group(group, state, now_ms, cursor_stop_ms);
                    } else if turn_off {
                        stop_group(group, state, now_ms, cursor_stop_ms);
                    }
                }
                if group.attached {
                    if previous_active && !state.active {
                        state.active = true;
                        state.started_at_ms = if previous_started != 0 {
                            previous_started
                        } else {
                            now_ms
                        };
                        state.ramp_started_at_ms = state.started_at_ms;
                        state.reverse_ends_at_ms = 0;
                        state.loops_left = 0;
                        state.looping = false;
                    } else if !previous_active && !group.has_own_animation() {
                        state.active = false;
                    }
                }
                this_active = state.active;
                if this_active {
                    any_active = true;
                    min_step = min_step.min(group.step_ms);
                    if group.has_own_animation() {
                        if state.reverse_ends_at_ms != 0 {
                            next_end = next_end.min(state.reverse_ends_at_ms);
                        } else if state.looping {
                            next_end = next_end.min(
                                state.ramp_started_at_ms
                                    + group.ramp_duration(cursor_stop_ms).max(0) as u64,
                            );
                        } else if stop_duration > 0 {
                            next_end =
                                next_end.min(state.started_at_ms + stop_duration.max(0) as u64);
                        }
                    }
                }
            }
            previous_active = this_active;
        }
        self.has_active_shaders = any_active;
        self.min_step_ms = min_step;
        self.next_end_at_ms = next_end;
        min_step
    }

    /// Current `animation_progress` for every group.
    pub fn progresses(
        &self,
        pipeline: &CustomShaderPipeline,
        now_ms: u64,
        cursor_stop_ms: i64,
    ) -> Vec<f32> {
        let mut ans = vec![0.0; pipeline.groups.len()];
        for (index, group) in pipeline.groups.iter().enumerate() {
            let state = &self.groups[index];
            if !group.is_animated() || !state.active {
                continue;
            }
            if index > 0 && group.attached && !group.has_own_animation() {
                ans[index] = ans[index - 1];
                continue;
            }
            ans[index] = group_linear_progress(group, state, now_ms, cursor_stop_ms) as f32;
        }
        ans
    }

    /// Whether a group should run in this tick.
    pub fn group_active(&self, pipeline: &CustomShaderPipeline, index: usize) -> bool {
        pipeline
            .groups
            .get(index)
            .map(|group| !group.is_animated() || self.groups[index].active)
            .unwrap_or(false)
    }
}

fn group_linear_progress(
    group: &CustomShaderGroup,
    state: &ShaderGroupAnimState,
    now_ms: u64,
    cursor_stop_ms: i64,
) -> f64 {
    let mut ans = 0.0;
    if state.reverse_ends_at_ms != 0 {
        let duration = group.reverse_duration(cursor_stop_ms);
        if duration > 0 {
            ans = (state.reverse_ends_at_ms.saturating_sub(now_ms)) as f64 / duration as f64;
        }
    } else {
        let duration = group.ramp_duration(cursor_stop_ms);
        if duration > 0 {
            ans = now_ms.saturating_sub(state.ramp_started_at_ms) as f64 / duration as f64;
        }
    }
    ans.clamp(0.0, 1.0)
}

fn start_group(
    group: &CustomShaderGroup,
    state: &mut ShaderGroupAnimState,
    now_ms: u64,
    cursor_stop_ms: i64,
) {
    if state.active && (state.reverse_ends_at_ms != 0 || state.looping) {
        state.ramp_started_at_ms = now_ms.saturating_sub(
            (group_linear_progress(group, state, now_ms, cursor_stop_ms)
                * group.ramp_duration(cursor_stop_ms).max(0) as f64) as u64,
        );
    } else {
        state.ramp_started_at_ms = now_ms;
    }
    state.active = true;
    state.started_at_ms = now_ms;
    state.reverse_ends_at_ms = 0;
    state.loops_left = 0;
    state.looping = false;
}

fn stop_group(
    group: &CustomShaderGroup,
    state: &mut ShaderGroupAnimState,
    now_ms: u64,
    cursor_stop_ms: i64,
) {
    let reverse_for = (group_linear_progress(group, state, now_ms, cursor_stop_ms)
        * group.reverse_duration(cursor_stop_ms).max(0) as f64) as u64;
    if reverse_for > 0 {
        state.reverse_ends_at_ms = now_ms + reverse_for;
        state.loops_left = group.loops.saturating_sub(1);
    } else {
        state.active = false;
    }
}

fn advance_stopped_group(
    group: &CustomShaderGroup,
    state: &mut ShaderGroupAnimState,
    now_ms: u64,
    cursor_stop_ms: i64,
) {
    loop {
        if state.reverse_ends_at_ms != 0 && now_ms >= state.reverse_ends_at_ms {
            if state.loops_left > 0 {
                state.loops_left -= 1;
                state.ramp_started_at_ms = state.reverse_ends_at_ms;
                state.reverse_ends_at_ms = 0;
                state.looping = true;
            } else {
                state.active = false;
                state.reverse_ends_at_ms = 0;
                break;
            }
        } else if state.looping {
            let ramp = group.ramp_duration(cursor_stop_ms).max(0) as u64;
            if now_ms >= state.ramp_started_at_ms.saturating_add(ramp) {
                state.looping = false;
                state.reverse_ends_at_ms = state
                    .ramp_started_at_ms
                    .saturating_add(ramp)
                    .saturating_add(group.reverse_duration(cursor_stop_ms).max(0) as u64);
            } else {
                break;
            }
        } else {
            break;
        }
    }
}

/// Errors from parsing or compiling a custom shader pipeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CustomShaderError {
    Parse(String),
    Compile(String),
}

impl fmt::Display for CustomShaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "invalid custom shader pipeline: {error}"),
            Self::Compile(error) => write!(f, "custom shader compilation failed: {error}"),
        }
    }
}

impl std::error::Error for CustomShaderError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipeline(text: &str) -> CustomShaderPipeline {
        CustomShaderPipeline::parse("test", text).expect("parse")
    }

    #[test]
    fn parses_groups_textures_and_animation() {
        let p = pipeline(
            "textures a b\n\
             startgroup\n\
             shaders negative\n\
             endgroup\n\
             startgroup\n\
             shaders bloom\n\
             output_texture a\n\
             animation_start user:sparkle bell-in-window\n\
             animation_stop 250\n\
             animation_duration 100\n\
             animation_reverse yes\n\
             animation_loops 2\n\
             animation_step 20\n\
             attach\n\
             endgroup\n\
             startgroup\n\
             shaders crt\n\
             endgroup\n",
        );
        assert_eq!(p.textures, vec![NamedTexture::A, NamedTexture::B]);
        assert_eq!(p.groups.len(), 3);
        assert_eq!(p.groups[0].output_texture, NamedTexture::Default);
        assert_eq!(p.groups[1].output_texture, NamedTexture::A);
        assert_eq!(p.groups[1].end_duration_ms, 250);
        assert_eq!(p.groups[1].duration_ms, 100);
        assert_eq!(p.groups[1].reverse_ms, -1);
        assert_eq!(p.groups[1].loops, 2);
        assert_eq!(p.groups[1].step_ms, 20);
        assert!(p.groups[1].attached);
        assert_eq!(p.groups[1].start_events, (1u64 << 11) | (1 << 6));
        assert_eq!(p.user_events, vec!["sparkle"]);
    }

    #[test]
    fn rejects_a_named_output_in_the_final_group() {
        assert!(CustomShaderPipeline::parse(
            "test",
            "startgroup\nshaders x\noutput_texture a\nendgroup"
        )
        .is_err());
    }

    #[test]
    fn static_groups_run_immediately() {
        let p = pipeline("startgroup\nshaders x\nendgroup");
        let state = CustomShaderAnimationState::new(&p);
        assert!(state.has_active_shaders);
        assert!(state.group_active(&p, 0));
        assert_eq!(state.progresses(&p, 0, 500), vec![0.0]);
    }

    #[test]
    fn events_start_stop_and_reverse_a_group() {
        let p = pipeline(
            "startgroup\nshaders x\nanimation_start bell-in-window\nanimation_stop 100\n\
             animation_duration 100\nanimation_reverse yes\nanimation_step 10\nendgroup",
        );
        let mut state = CustomShaderAnimationState::new(&p);
        assert!(!state.has_active_shaders);
        state.update(&p, SHADER_ANIM_EVENT_BELL_IN_WINDOW, 0, 500);
        assert!(state.has_active_shaders);
        assert_eq!(state.progresses(&p, 50, 500), vec![0.5]);
        assert_eq!(state.progresses(&p, 100, 500), vec![1.0]);
        // Duration ends: reverse starts, so the group is still active.
        state.update(&p, 0, 100, 500);
        assert!(state.has_active_shaders);
        assert_eq!(state.progresses(&p, 150, 500), vec![0.5]);
        state.update(&p, 0, 200, 500);
        assert!(!state.has_active_shaders);
        assert_eq!(state.next_end_at_ms, CustomShaderAnimationState::NEVER);
    }

    #[test]
    fn toggle_events_flip_the_state_and_focus_out_is_a_hard_stop() {
        let p = pipeline(
            "startgroup\nshaders x\nanimation_toggle user:flash\n             animation_stop os-window-focus-out\nanimation_step 5\nendgroup",
        );
        let mut state = CustomShaderAnimationState::new(&p);
        let flash = p.user_event_bit("flash");
        state.update(&p, flash, 0, 500);
        assert!(state.has_active_shaders);
        state.update(&p, flash, 10, 500);
        assert!(!state.has_active_shaders);
        state.update(&p, flash, 20, 500);
        assert!(state.has_active_shaders);
        state.update(&p, SHADER_ANIM_EVENT_OS_WINDOW_FOCUS_OUT, 30, 500);
        assert!(!state.has_active_shaders);
    }

    #[test]
    fn attached_groups_follow_their_predecessor_and_share_progress() {
        let p = pipeline(
            "startgroup\nshaders a\nanimation_start bell-in-window\nanimation_duration 100\nendgroup\n\
             startgroup\nshaders b\nattach\nendgroup",
        );
        let mut state = CustomShaderAnimationState::new(&p);
        state.update(&p, SHADER_ANIM_EVENT_BELL_IN_WINDOW, 0, 500);
        assert!(state.group_active(&p, 0));
        assert!(state.group_active(&p, 1));
        assert_eq!(state.progresses(&p, 25, 500), vec![0.25, 0.25]);
    }
}
