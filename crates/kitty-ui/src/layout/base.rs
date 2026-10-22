// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared layout machinery ported from ``kitty/layout/base.py``.

use std::collections::BTreeMap;
use std::collections::HashMap;

use crate::model::{Edge, Edges, GroupId, Window, WindowGeometry, WindowId, WindowList};
use crate::value::Value;

/// The kitty layouts, in the order used when expanding ``all``/``*``.
pub const ALL_LAYOUT_NAMES: [&str; 7] = [
    "fat",
    "grid",
    "horizontal",
    "splits",
    "stack",
    "tall",
    "vertical",
];

pub fn is_layout_name(name: &str) -> bool {
    ALL_LAYOUT_NAMES.contains(&name)
}

/// Parse a comma separated ``enabled_layouts`` value. Accepts ``*``/``all`` and
/// ``name:opts`` entries. Preserves order and removes duplicates.
pub fn parse_layout_names(raw: &str) -> Result<Vec<String>, String> {
    let mut ans = Vec::new();
    for part in raw.split(',') {
        let p = part.trim().to_lowercase();
        if p.is_empty() {
            continue;
        }
        if p == "*" || p == "all" {
            for name in ALL_LAYOUT_NAMES {
                if !ans.iter().any(|x: &String| x == name) {
                    ans.push(name.to_string());
                }
            }
            continue;
        }
        let name = p.split(':').next().unwrap_or("");
        if !is_layout_name(name) {
            return Err(format!("The window layout {p} is unknown"));
        }
        if !ans.contains(&p) {
            ans.push(p);
        }
    }
    if ans.is_empty() {
        return Err("No layouts specified".into());
    }
    Ok(ans)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

impl Region {
    pub const fn new(left: i32, top: i32, width: i32, height: i32) -> Self {
        Self {
            left,
            top,
            width,
            height,
        }
    }

    pub fn right(&self) -> i32 {
        self.left + self.width
    }

    pub fn bottom(&self) -> i32 {
        self.top + self.height
    }
}

impl Default for Region {
    fn default() -> Self {
        Self::new(0, 0, 200, 200)
    }
}

/// Per-layout viewport/decoration configuration. kitty keeps this in the
/// global ``lgd`` singleton; passing it explicitly keeps the Rust model pure and
/// deterministic (several tests can run in parallel).
#[derive(Clone, Copy, Debug)]
pub struct LayoutConfig {
    pub central: Region,
    pub cell_width: i32,
    pub cell_height: i32,
    pub draw_minimal_borders: bool,
    /// -1 = left/top, 0 = center, 1 = right/bottom.
    pub alignment_x: i32,
    pub alignment_y: i32,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            central: Region::default(),
            cell_width: 20,
            cell_height: 20,
            draw_minimal_borders: true,
            alignment_x: 0,
            alignment_y: 0,
        }
    }
}

impl LayoutConfig {
    pub fn with_central(mut self, central: Region) -> Self {
        self.central = central;
        self
    }

    pub fn with_cell(mut self, width: i32, height: i32) -> Self {
        self.cell_width = width;
        self.cell_height = height;
        self
    }
}

/// The allocation of one axis to one window, matching kitty's ``LayoutData``.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutData {
    pub content_pos: i32,
    pub cells_per_window: usize,
    pub space_before: i32,
    pub space_after: i32,
    pub content_size: i32,
    pub compensatory_before: i32,
    pub compensatory_after: i32,
}

impl LayoutData {
    pub fn extents(&self) -> (i32, i32) {
        (
            self.content_pos - self.space_before,
            self.content_pos + self.content_size + self.space_after,
        )
    }
}

/// Bias input to [`layout_dimension`]. kitty accepts either a sequence (one
/// fraction per window) or a map from window index to an increment.
#[derive(Clone, Debug, Default)]
pub enum Bias {
    #[default]
    None,
    Seq(Vec<f64>),
    Indexed(BTreeMap<usize, f64>),
}

impl Bias {
    pub fn is_none(&self) -> bool {
        matches!(self, Bias::None)
    }
}

/// Neighbours of a window, as group ids, matching kitty's ``NeighborsMap``.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Neighbors {
    pub left: Vec<GroupId>,
    pub right: Vec<GroupId>,
    pub top: Vec<GroupId>,
    pub bottom: Vec<GroupId>,
}

impl Neighbors {
    pub fn get(&self, edge: Edge) -> &[GroupId] {
        match edge {
            Edge::Left => &self.left,
            Edge::Top => &self.top,
            Edge::Right => &self.right,
            Edge::Bottom => &self.bottom,
        }
    }

    pub fn get_mut(&mut self, edge: Edge) -> &mut Vec<GroupId> {
        match edge {
            Edge::Left => &mut self.left,
            Edge::Top => &mut self.top,
            Edge::Right => &mut self.right,
            Edge::Bottom => &mut self.bottom,
        }
    }

    pub fn set(&mut self, edge: Edge, ids: Vec<GroupId>) {
        *self.get_mut(edge) = ids;
    }

    pub fn is_empty(&self) -> bool {
        self.left.is_empty()
            && self.right.is_empty()
            && self.top.is_empty()
            && self.bottom.is_empty()
    }
}

/// Where a newly added window should be placed, mirroring kitty's ``location``
/// strings plus overlay support.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowLocation {
    Default,
    Before,
    After,
    First,
    Last,
    Neighbor,
    VSplit,
    HSplit,
    Split,
}

impl WindowLocation {
    pub fn from_kitty_name(name: &str) -> Option<Self> {
        Some(match name {
            "before" => Self::Before,
            "after" => Self::After,
            "first" => Self::First,
            "last" => Self::Last,
            "neighbor" => Self::Neighbor,
            "vsplit" => Self::VSplit,
            "hsplit" => Self::HSplit,
            "split" => Self::Split,
            "" => Self::Default,
            _ => return None,
        })
    }

    pub fn is_before(self) -> bool {
        matches!(self, Self::Before | Self::First)
    }
}

#[derive(Clone, Debug)]
pub struct LayoutAddOptions {
    pub location: WindowLocation,
    /// Place the new window in the same group as this window (overlay).
    pub overlay_for: Option<WindowId>,
    pub next_to: Option<WindowId>,
    /// Fraction in ``[-100, 100]`` used by splits to size the new pane.
    pub bias: Option<f64>,
}

impl Default for LayoutAddOptions {
    fn default() -> Self {
        Self {
            location: WindowLocation::Default,
            overlay_for: None,
            next_to: None,
            bias: None,
        }
    }
}

pub fn normalize_biases(biases: &[f64]) -> Vec<f64> {
    let s: f64 = biases.iter().sum();
    if (s - 1.0).abs() < f64::EPSILON || s == 0.0 {
        biases.to_vec()
    } else {
        biases.iter().map(|x| x / s).collect()
    }
}

pub fn safe_increment_bias(old: f64, increment: f64) -> f64 {
    (old + increment).clamp(0.1, 0.9)
}

pub fn fractional_resize_steps(remaining: usize, fraction: f64) -> usize {
    if remaining == 0 {
        0
    } else {
        ((remaining as f64 * fraction) - 1e-9).ceil().max(1.0) as usize
    }
}

fn distribute_indexed_bias(base: &[f64], index_bias_map: &BTreeMap<usize, f64>) -> Vec<f64> {
    if index_bias_map.is_empty() {
        return base.to_vec();
    }
    let limit = base.len();
    let mut ans = base.to_vec();
    for (&row, &increment) in index_bias_map {
        if row >= limit || increment == 0.0 {
            continue;
        }
        let other_increment = -increment / (limit.saturating_sub(1)) as f64;
        ans = ans
            .iter()
            .enumerate()
            .map(|(i, b)| {
                safe_increment_bias(*b, if i == row { increment } else { other_increment })
            })
            .collect();
    }
    normalize_biases(&ans)
}

fn convert_bias_map(bias: &BTreeMap<usize, f64>, num_windows: usize, num_cells: usize) -> Vec<f64> {
    let cells_per_window = num_cells / num_windows;
    let mut cell_map = vec![cells_per_window; num_windows];
    if let Some(last) = cell_map.last_mut() {
        *last += num_cells % num_windows;
    }
    let base_bias: Vec<f64> = cell_map
        .iter()
        .map(|x| *x as f64 / num_cells as f64)
        .collect();
    distribute_indexed_bias(&base_bias, bias)
}

pub fn calculate_cells_map(bias: &Bias, num_windows: usize, num_cells: usize) -> Vec<usize> {
    if num_windows == 0 {
        return Vec::new();
    }
    let bias = match bias {
        Bias::Indexed(m) => Bias::Seq(convert_bias_map(m, num_windows, num_cells)),
        other => other.clone(),
    };
    let cells_per_window = num_cells / num_windows;
    let mut cells_map = match &bias {
        Bias::Seq(seq) if num_windows > 1 && seq.len() == num_windows && cells_per_window > 5 => {
            let mut map: Vec<usize> = seq
                .iter()
                .map(|b| (*b * num_cells as f64) as usize)
                .collect();
            while *map.iter().min().unwrap() < 5 {
                let max = *map.iter().max().unwrap();
                let min = *map.iter().min().unwrap();
                if max == min {
                    break;
                }
                let maxi = map.iter().position(|x| *x == max).unwrap();
                let mini = map.iter().position(|x| *x == min).unwrap();
                map[mini] += 1;
                map[maxi] -= 1;
            }
            map
        }
        _ => vec![cells_per_window; num_windows],
    };
    let sum: usize = cells_map.iter().sum();
    if sum < num_cells {
        if let Some(last) = cells_map.last_mut() {
            *last += num_cells - sum;
        }
    }
    cells_map
}

pub fn layout_dimension(
    start_at: i32,
    length: i32,
    cell_length: i32,
    decoration_pairs: &[(i32, i32)],
    alignment: i32,
    bias: &Bias,
) -> Vec<LayoutData> {
    let number_of_windows = decoration_pairs.len();
    if number_of_windows == 0 {
        return Vec::new();
    }
    let cell_length = cell_length.max(1);
    let mut number_of_cells = if length < 0 { 0 } else { length / cell_length } as usize;
    let space_needed_for_decorations: i32 = decoration_pairs.iter().map(|(a, b)| a + b).sum();
    let mut extra = length - number_of_cells as i32 * cell_length;
    while extra < space_needed_for_decorations && number_of_cells > 0 {
        number_of_cells -= 1;
        extra = length - number_of_cells as i32 * cell_length;
    }
    let cells_map = if number_of_cells > 0 {
        calculate_cells_map(bias, number_of_windows, number_of_cells)
    } else {
        vec![0; number_of_windows]
    };
    debug_assert_eq!(cells_map.iter().sum::<usize>(), number_of_cells);

    let extra = length - number_of_cells as i32 * cell_length - space_needed_for_decorations;
    let mut pos = start_at;
    if alignment > 0 {
        pos += extra;
    } else if alignment == 0 {
        pos += extra / 2;
    }
    let last_i = cells_map.len() - 1;
    let mut ans = Vec::with_capacity(number_of_windows);
    for (i, cells_per_window) in cells_map.iter().enumerate() {
        let (before_dec, after_dec) = decoration_pairs[i];
        pos += before_dec;
        let before_space = if i == 0 { pos - start_at } else { before_dec };
        let content_size = *cells_per_window as i32 * cell_length;
        let after_space = if i == last_i {
            (start_at + length) - (pos + content_size)
        } else {
            after_dec
        };
        ans.push(LayoutData {
            content_pos: pos,
            cells_per_window: *cells_per_window,
            space_before: before_space,
            space_after: after_space,
            content_size,
            compensatory_before: before_space - before_dec,
            compensatory_after: after_space - after_dec,
        });
        pos += content_size + after_space;
    }
    ans
}

pub fn window_geometry_from_layouts(
    x: &LayoutData,
    y: &LayoutData,
    cell_width: i32,
    cell_height: i32,
) -> WindowGeometry {
    WindowGeometry {
        left: x.content_pos,
        top: y.content_pos,
        right: x.content_pos + cell_width * x.cells_per_window as i32,
        bottom: y.content_pos + cell_height * y.cells_per_window as i32,
        spaces: Edges::new(x.space_before, y.space_before, x.space_after, y.space_after),
        compensatory: Edges::new(
            x.compensatory_before,
            y.compensatory_before,
            x.compensatory_after,
            y.compensatory_after,
        ),
    }
}

pub fn group_decoration(
    windows: &WindowList,
    group_id: GroupId,
    edge: Edge,
    border_mult: i32,
) -> i32 {
    let Some(g) = windows.group(group_id) else {
        return 0;
    };
    let Some(wid) = g.windows.first() else {
        return 0;
    };
    let Some(w) = windows.window(*wid) else {
        return 0;
    };
    w.effective_margin(edge) + w.effective_border() * border_mult + w.effective_padding(edge)
}

pub fn decoration_pairs(
    windows: &WindowList,
    groups: &[GroupId],
    edge_before: Edge,
    edge_after: Edge,
    border_mult: i32,
) -> Vec<(i32, i32)> {
    groups
        .iter()
        .map(|g| {
            (
                group_decoration(windows, *g, edge_before, border_mult),
                group_decoration(windows, *g, edge_after, border_mult),
            )
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub fn xlayout(
    config: &LayoutConfig,
    windows: &WindowList,
    groups: &[GroupId],
    bias: &Bias,
    start: Option<i32>,
    size: Option<i32>,
    offset: usize,
    border_mult: i32,
) -> Vec<LayoutData> {
    let groups: Vec<GroupId> = groups.iter().skip(offset).copied().collect();
    let pairs = decoration_pairs(windows, &groups, Edge::Left, Edge::Right, border_mult);
    layout_dimension(
        start.unwrap_or(config.central.left),
        size.unwrap_or(config.central.width),
        config.cell_width,
        &pairs,
        config.alignment_x,
        bias,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn ylayout(
    config: &LayoutConfig,
    windows: &WindowList,
    groups: &[GroupId],
    bias: &Bias,
    start: Option<i32>,
    size: Option<i32>,
    offset: usize,
    border_mult: i32,
) -> Vec<LayoutData> {
    let groups: Vec<GroupId> = groups.iter().skip(offset).copied().collect();
    let pairs = decoration_pairs(windows, &groups, Edge::Top, Edge::Bottom, border_mult);
    layout_dimension(
        start.unwrap_or(config.central.top),
        size.unwrap_or(config.central.height),
        config.cell_height,
        &pairs,
        config.alignment_y,
        bias,
    )
}

pub fn set_group_geometry(windows: &mut WindowList, group_id: GroupId, geom: WindowGeometry) {
    if let Some(g) = windows.group_mut(group_id) {
        g.allocated_geometry = Some(geom);
        let wids = g.windows.clone();
        for wid in wids {
            if let Some(w) = windows.window_mut(wid) {
                w.set_geometry(geom);
            }
        }
    }
}

pub fn layout_single_window_group(
    config: &LayoutConfig,
    windows: &mut WindowList,
    group_id: GroupId,
    must_draw_borders: bool,
) {
    let bw = i32::from(must_draw_borders);
    let xpairs = vec![(
        group_decoration(windows, group_id, Edge::Left, bw),
        group_decoration(windows, group_id, Edge::Right, bw),
    )];
    let ypairs = vec![(
        group_decoration(windows, group_id, Edge::Top, bw),
        group_decoration(windows, group_id, Edge::Bottom, bw),
    )];
    let x = layout_dimension(
        config.central.left,
        config.central.width,
        config.cell_width,
        &xpairs,
        config.alignment_x,
        &Bias::None,
    );
    let y = layout_dimension(
        config.central.top,
        config.central.height,
        config.cell_height,
        &ypairs,
        config.alignment_y,
        &Bias::None,
    );
    if let (Some(x), Some(y)) = (x.first(), y.first()) {
        let geom = window_geometry_from_layouts(x, y, config.cell_width, config.cell_height);
        set_group_geometry(windows, group_id, geom);
    }
}

fn group_ids(windows: &WindowList) -> Vec<GroupId> {
    windows.groups().iter().map(|g| g.id).collect()
}

/// The trait implemented by all layouts. Default methods mirror
/// ``kitty/layout/base.py``; layouts override only what they need.
pub trait LayoutImpl {
    fn name(&self) -> &'static str;
    fn config(&self) -> &LayoutConfig;
    fn config_mut(&mut self) -> &mut LayoutConfig;

    fn only_active_window_visible(&self) -> bool {
        false
    }

    fn do_layout(&mut self, windows: &mut WindowList);

    fn neighbors_for_window(&self, _window: WindowId, _windows: &WindowList) -> Neighbors {
        Neighbors::default()
    }

    fn apply(&mut self, windows: &mut WindowList) {
        self.update_visibility(windows);
        if !windows.groups().is_empty() {
            self.do_layout(windows);
        }
    }

    fn update_visibility(&self, windows: &mut WindowList) {
        let active = windows.active_main_group().map(|g| g.id);
        let only_active = self.only_active_window_visible();
        let groups: Vec<(GroupId, Vec<WindowId>)> = windows
            .groups()
            .iter()
            .map(|g| (g.id, g.windows.clone()))
            .collect();
        for (gid, wids) in groups {
            let visible = !only_active || Some(gid) == active;
            for wid in wids {
                if let Some(w) = windows.window_mut(wid) {
                    w.set_visible_in_layout(visible);
                }
            }
        }
    }

    fn nth_window(&self, windows: &WindowList, num: i64) -> Option<WindowId> {
        windows.active_window_in_nth_group(num, true).map(|w| w.id)
    }

    fn activate_nth_window(&self, windows: &mut WindowList, num: usize) {
        if num < windows.num_groups() {
            windows.set_active_group_idx(num, true);
        }
    }

    fn next_window(&self, windows: &mut WindowList, delta: i64) {
        windows.activate_next_window_group(delta);
    }

    /// Resize a window by a raw bias increment. Most layouts use group index
    /// internally, so convert the window id first.
    fn modify_size_of_window(
        &mut self,
        windows: &mut WindowList,
        window_id: WindowId,
        increment: f64,
        is_horizontal: bool,
    ) -> bool {
        let Some(idx) = windows.group_idx_for_window(window_id) else {
            return false;
        };
        if increment == 0.0 {
            return false;
        }
        self.apply_bias(idx, increment, windows, is_horizontal)
    }

    fn apply_bias(
        &mut self,
        _idx: usize,
        _increment: f64,
        _windows: &mut WindowList,
        _is_horizontal: bool,
    ) -> bool {
        false
    }

    fn remove_all_biases(&mut self) -> bool {
        false
    }

    fn bias_slot(
        &mut self,
        _windows: &mut WindowList,
        _idx: usize,
        _fractional_bias: f64,
        _cell_increment_bias_h: f64,
        _cell_increment_bias_v: f64,
    ) -> bool {
        false
    }

    fn move_window(&mut self, windows: &mut WindowList, delta: i64) -> bool {
        if windows.num_groups() < 2 || delta == 0 {
            return false;
        }
        windows.move_window_group(Some(delta), None)
    }

    fn move_window_to_group(&mut self, windows: &mut WindowList, group: GroupId) -> bool {
        windows.move_window_group(None, Some(group))
    }

    fn insert_window_next_to(
        &mut self,
        windows: &mut WindowList,
        window: WindowId,
        next_to: WindowId,
        _horizontal: bool,
        after: bool,
    ) {
        let Some(src_gid) = windows.group_id_for_window(window) else {
            return;
        };
        let Some(dest_gid) = windows.group_id_for_window(next_to) else {
            return;
        };
        if src_gid == dest_gid {
            return;
        }
        windows.set_active_group(src_gid);
        windows.insert_window_group_next_to(dest_gid, after);
    }

    fn add_window(
        &mut self,
        windows: &mut WindowList,
        window: Window,
        opts: &LayoutAddOptions,
    ) -> Option<WindowId> {
        if let Some(overlay_for) = opts.overlay_for {
            if let Some(underlay) = windows.window(overlay_for).cloned() {
                let mut window = window;
                window.margin = underlay.margin;
                window.padding = underlay.padding;
                windows.add_window(
                    window,
                    crate::model::AddWindowOptions {
                        group_of: Some(overlay_for),
                        make_active: true,
                        ..Default::default()
                    },
                );
                return Some(underlay.id);
            }
        }
        self.add_non_overlay_window(windows, window, opts);
        None
    }

    fn add_non_overlay_window(
        &mut self,
        windows: &mut WindowList,
        window: Window,
        opts: &LayoutAddOptions,
    ) {
        let mut before = opts.location.is_before();
        let mut next_to = opts.next_to.or_else(|| windows.active_window_id());
        match opts.location {
            WindowLocation::First => {
                before = true;
                next_to = None;
            }
            WindowLocation::Last => next_to = None,
            _ => {}
        }
        let id = window.id;
        windows.add_window(
            window,
            crate::model::AddWindowOptions {
                next_to,
                before,
                make_active: true,
                ..Default::default()
            },
        );
        if let Some(bias) = opts.bias {
            if let Some(idx) = windows.group_idx_for_window(id) {
                let fractional = bias.abs().clamp(10.0, 90.0) / 100.0;
                let nh = self.config().central.width as f64 / self.config().cell_width as f64;
                let nv = self.config().central.height as f64 / self.config().cell_height as f64;
                let f = bias.clamp(-90.0, 90.0) / 100.0;
                let h = (self.config().cell_width + 1) as f64 / self.config().central.width as f64;
                let v =
                    (self.config().cell_height + 1) as f64 / self.config().central.height as f64;
                self.bias_slot(windows, idx, fractional, h * nh * f, v * nv * f);
            }
        }
    }

    fn remove_windows(&mut self, _windows: &mut WindowList, _group_ids: &[GroupId]) {}

    fn on_window_removed(&mut self, _windows: &mut WindowList, _removed: &[GroupId]) -> bool {
        false
    }

    fn layout_action(
        &mut self,
        _action_name: &str,
        _args: &[String],
        _windows: &mut WindowList,
    ) -> Option<bool> {
        None
    }

    fn layout_state(&self) -> Value {
        Value::Map(Value::map())
    }

    /// Access to the binary split tree, used by tests and by callers that need
    /// to inspect a splits layout. Other layouts return ``None``.
    fn splits_root(&self) -> Option<&crate::layout::Pair> {
        None
    }

    fn splits_root_mut(&mut self) -> Option<&mut crate::layout::Pair> {
        None
    }

    fn set_layout_state(
        &mut self,
        _layout_state: &Value,
        _group_id_map: &HashMap<GroupId, GroupId>,
    ) -> bool {
        true
    }

    fn layout_opts_serialized(&self) -> BTreeMap<String, Value> {
        BTreeMap::new()
    }

    fn class_name(&self) -> &'static str {
        match self.name() {
            "stack" => "Stack",
            "tall" => "Tall",
            "fat" => "Fat",
            "vertical" => "Vertical",
            "horizontal" => "Horizontal",
            "grid" => "Grid",
            "splits" => "Splits",
            other => other,
        }
    }

    fn serialize(&self, windows: &WindowList) -> Value {
        let mut map = self.layout_state().as_map().cloned().unwrap_or_default();
        map.insert("opts".into(), Value::Map(self.layout_opts_serialized()));
        map.insert("class".into(), Value::Str(self.class_name().into()));
        map.insert("all_windows".into(), windows.serialize_layout_state());
        Value::Map(map)
    }

    fn unserialize(
        &mut self,
        state: &Value,
        windows: &mut WindowList,
        apply_to_window_list: bool,
    ) -> bool {
        if state.get_str("class") != Some(self.class_name()) {
            return false;
        }
        let Some(all_windows) = state.get("all_windows") else {
            return false;
        };
        let window_map = windows.window_id_map_for_layout();
        let Some(group_map) =
            windows.unserialize_layout_state(all_windows, &window_map, apply_to_window_list)
        else {
            return false;
        };
        self.set_layout_state(state, &group_map)
    }

    fn bias_increment_for_cell(&self, is_horizontal: bool) -> f64 {
        if is_horizontal {
            (self.config().cell_width + 1) as f64 / self.config().central.width.max(1) as f64
        } else {
            (self.config().cell_height + 1) as f64 / self.config().central.height.max(1) as f64
        }
    }

    /// Resize by a number of cells, returning the number of cells applied.
    fn drag_resize_window(
        &mut self,
        windows: &mut WindowList,
        window_id: WindowId,
        increment: f64,
        is_horizontal: bool,
    ) -> f64 {
        let percent = self.bias_increment_for_cell(is_horizontal) * increment;
        if self.modify_size_of_window(windows, window_id, percent, is_horizontal) {
            increment
        } else {
            0.0
        }
    }
}

pub fn all_groups(windows: &WindowList) -> Vec<GroupId> {
    group_ids(windows)
}
