// SPDX-License-Identifier: GPL-3.0-or-later
//! Layouts and the per-tab layout manager.
//!
//! This is a CPU-only port of ``kitty/layout/*.py``: layouts allocate cell
//! geometries on [`WindowList`] groups and implement focus/neighbour/resize
//! operations. No rendering or border painting is done here.

pub mod base;
pub mod grid;
pub mod splits;
pub mod stack;
pub mod tall;
pub mod vertical;

use std::collections::BTreeMap;

use crate::model::{GroupId, Window, WindowId, WindowList};
use crate::value::Value;

pub use base::{
    all_groups, is_layout_name, layout_dimension, parse_layout_names, Bias, LayoutAddOptions,
    LayoutConfig, LayoutData, LayoutImpl, Neighbors, Region, WindowLocation, ALL_LAYOUT_NAMES,
};
pub use grid::{calc_grid_size, Grid};
pub use splits::{Pair, SplitChild, Splits, SplitsLayoutOpts};
pub use stack::Stack;
pub use tall::{Tall, TallLayoutOpts};
pub use vertical::Vertical;

fn split_name_opts(name: &str) -> (&str, &str) {
    name.split_once(':').unwrap_or((name, ""))
}

/// Create a layout by kitty name, optionally with ``name:opts``.
pub fn create_layout(name: &str, config: LayoutConfig) -> Result<Box<dyn LayoutImpl>, String> {
    let (base, opts) = split_name_opts(name);
    Ok(match base {
        "stack" => Box::new(Stack::new(config)),
        "tall" => Box::new(Tall::new(true, TallLayoutOpts::parse(opts), config)),
        "fat" => Box::new(Tall::new(false, TallLayoutOpts::parse(opts), config)),
        "vertical" => Box::new(Vertical::new(false, config)),
        "horizontal" => Box::new(Vertical::new(true, config)),
        "grid" => Box::new(Grid::new(config)),
        "splits" => Box::new(Splits::new(SplitsLayoutOpts::parse(opts), config)),
        other => return Err(format!("The window layout {other} is unknown")),
    })
}

/// Manages the enabled layouts of one tab, preserving the state of layouts
/// while they are not current, and cycling through them in enabled order.
pub struct LayoutManager {
    pub os_window_id: u64,
    pub tab_id: u64,
    enabled_layouts: Vec<String>,
    current_name: String,
    last_used: Option<String>,
    layouts: BTreeMap<String, Box<dyn LayoutImpl>>,
    config: LayoutConfig,
}

impl LayoutManager {
    pub fn new(
        os_window_id: u64,
        tab_id: u64,
        enabled_layouts: &[String],
        config: LayoutConfig,
    ) -> Result<Self, String> {
        let mut enabled: Vec<String> = enabled_layouts.to_vec();
        if enabled.is_empty() {
            enabled.push("tall".into());
        }
        // Validate names before creating anything.
        for name in &enabled {
            let (base, _) = split_name_opts(name);
            if !is_layout_name(base) {
                return Err(format!("The window layout {name} is unknown"));
            }
        }
        let mut layouts = BTreeMap::new();
        for name in &enabled {
            layouts.insert(name.clone(), create_layout(name, config)?);
        }
        let current_name = enabled[0].clone();
        Ok(Self {
            os_window_id,
            tab_id,
            enabled_layouts: enabled,
            current_name,
            last_used: None,
            layouts,
            config,
        })
    }

    pub fn config(&self) -> &LayoutConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: LayoutConfig) {
        self.config = config;
        for layout in self.layouts.values_mut() {
            *layout.config_mut() = config;
        }
    }

    pub fn set_viewport(&mut self, central: Region, cell_width: i32, cell_height: i32) {
        let mut config = self.config;
        config.central = central;
        config.cell_width = cell_width;
        config.cell_height = cell_height;
        self.set_config(config);
    }

    pub fn enabled_layouts(&self) -> &[String] {
        &self.enabled_layouts
    }

    pub fn current_name(&self) -> &str {
        &self.current_name
    }

    pub fn current(&self) -> &dyn LayoutImpl {
        self.layouts
            .get(&self.current_name)
            .expect("current layout must exist")
            .as_ref()
    }

    pub fn current_mut(&mut self) -> &mut dyn LayoutImpl {
        self.layouts
            .get_mut(&self.current_name)
            .expect("current layout must exist")
            .as_mut()
    }

    pub fn layout(&self, name: &str) -> Option<&dyn LayoutImpl> {
        self.layouts.get(name).map(|x| x.as_ref())
    }

    pub fn layout_mut(&mut self, name: &str) -> Option<&mut (dyn LayoutImpl + '_)> {
        match self.layouts.get_mut(name) {
            Some(x) => Some(x.as_mut()),
            None => None,
        }
    }

    fn ensure_layout(&mut self, name: &str) -> Result<(), String> {
        if !self.layouts.contains_key(name) {
            let layout = create_layout(name, self.config)?;
            self.layouts.insert(name.to_string(), layout);
        }
        Ok(())
    }

    /// Set the current layout. The layout is created on demand even if it is not
    /// in ``enabled_layouts`` (kitty allows ``layout`` to set the current one).
    pub fn set_layout(&mut self, name: &str) -> Result<(), String> {
        self.ensure_layout(name)?;
        if self.current_name != name {
            self.last_used = Some(self.current_name.clone());
            self.current_name = name.to_string();
        }
        Ok(())
    }

    /// Switch to the next enabled layout, wrapping around.
    pub fn next_layout(&mut self, delta: i64) -> &str {
        if self.enabled_layouts.is_empty() {
            return &self.current_name;
        }
        let n = self.enabled_layouts.len() as i64;
        let cur = self
            .enabled_layouts
            .iter()
            .position(|x| *x == self.current_name)
            .unwrap_or(0) as i64;
        let mut d = delta % n;
        if d < 0 {
            d += n;
        }
        let next = ((cur + d) % n) as usize;
        let name = self.enabled_layouts[next].clone();
        if name != self.current_name {
            self.last_used = Some(self.current_name.clone());
            self.current_name = name;
        }
        &self.current_name
    }

    pub fn set_enabled_layouts(&mut self, names: &[String]) -> Result<(), String> {
        let mut enabled: Vec<String> = names.to_vec();
        if enabled.is_empty() {
            enabled.push("tall".into());
        }
        for name in &enabled {
            let (base, _) = split_name_opts(name);
            if !is_layout_name(base) {
                return Err(format!("The window layout {name} is unknown"));
            }
            self.ensure_layout(name)?;
        }
        self.enabled_layouts = enabled;
        if !self.enabled_layouts.contains(&self.current_name) {
            let first = self.enabled_layouts[0].clone();
            self.last_used = Some(self.current_name.clone());
            self.current_name = first;
        }
        Ok(())
    }

    pub fn last_used_layout(&self) -> Option<&str> {
        self.last_used.as_deref()
    }

    /// Apply the current layout to the window list.
    pub fn apply(&mut self, windows: &mut WindowList) {
        self.current_mut().apply(windows);
    }

    /// Add a window using the semantics of the current layout, then re-layout.
    pub fn add_window(
        &mut self,
        windows: &mut WindowList,
        window: Window,
        opts: &LayoutAddOptions,
    ) -> Option<WindowId> {
        let ans = self.current_mut().add_window(windows, window, opts);
        self.apply(windows);
        ans
    }

    pub fn remove_window(&mut self, windows: &mut WindowList, window_id: WindowId) -> bool {
        let gid = windows.group_id_for_window(window_id);
        let Some(gid) = gid else {
            return false;
        };
        let removed = windows.remove_window(window_id).is_some();
        if removed {
            // An overlay window can be closed while its group survives; only
            // prune the layout tree when the whole group disappeared.
            let group_gone = windows.group(gid).is_none();
            if group_gone {
                self.current_mut().remove_windows(windows, &[gid]);
            }
            if self.current_mut().on_window_removed(windows, &[gid]) {
                // equalize_on_close needs a second pass
                self.apply(windows);
            }
            self.apply(windows);
        }
        removed
    }

    pub fn resize_window(
        &mut self,
        windows: &mut WindowList,
        window_id: WindowId,
        increment: f64,
        is_horizontal: bool,
    ) -> bool {
        let ans =
            self.current_mut()
                .modify_size_of_window(windows, window_id, increment, is_horizontal);
        if ans {
            self.apply(windows);
        }
        ans
    }

    pub fn neighbors(&self, windows: &WindowList) -> Neighbors {
        match windows.active_window_id() {
            Some(id) => self.current().neighbors_for_window(id, windows),
            None => Neighbors::default(),
        }
    }

    pub fn neighbors_for_window(&self, window_id: WindowId, windows: &WindowList) -> Neighbors {
        self.current().neighbors_for_window(window_id, windows)
    }

    pub fn move_window(&mut self, windows: &mut WindowList, delta: i64) -> bool {
        let ans = self.current_mut().move_window(windows, delta);
        if ans {
            self.apply(windows);
        }
        ans
    }

    pub fn move_window_to_group(&mut self, windows: &mut WindowList, group: GroupId) -> bool {
        let ans = self.current_mut().move_window_to_group(windows, group);
        if ans {
            self.apply(windows);
        }
        ans
    }

    pub fn layout_action(
        &mut self,
        action: &str,
        args: &[String],
        windows: &mut WindowList,
    ) -> Option<bool> {
        let ans = self.current_mut().layout_action(action, args, windows);
        if ans == Some(true) {
            self.apply(windows);
        }
        ans
    }

    pub fn remove_all_biases(&mut self) -> bool {
        self.current_mut().remove_all_biases()
    }

    /// Serialise the current and all previously used layout states, matching
    /// ``Tab.serialize_layout_state_for_session``.
    pub fn serialize_layout_state_for_session(&self, windows: &WindowList) -> Value {
        let mut ans = self
            .current()
            .serialize(windows)
            .as_map()
            .cloned()
            .unwrap_or_default();
        let mut others = BTreeMap::new();
        for (name, layout) in &self.layouts {
            if *name != self.current_name {
                others.insert(name.clone(), layout.serialize(windows));
            }
        }
        if !others.is_empty() {
            ans.insert("other_layouts".into(), Value::Map(others));
        }
        if let Some(last) = &self.last_used {
            ans.insert("last_used_layout".into(), Value::Str(last.clone()));
        }
        Value::Map(ans)
    }

    /// Restore the current layout plus any other layouts stored in the session
    /// state. Only the current layout reorders the window list.
    pub fn unserialize_layout_state_from_session(
        &mut self,
        state: &Value,
        windows: &mut WindowList,
    ) -> bool {
        let mut ok = self.current_mut().unserialize(state, windows, true);
        if let Some(others) = state.get("other_layouts").and_then(Value::as_map) {
            for (name, layout_state) in others {
                if *name == self.current_name {
                    continue;
                }
                if !is_layout_name(split_name_opts(name).0) {
                    continue;
                }
                if self.ensure_layout(name).is_err() {
                    continue;
                }
                if let Some(layout) = self.layouts.get_mut(name) {
                    // apply_to_window_list = false, so the window order set by
                    // the current layout is preserved.
                    layout.unserialize(layout_state, windows, false);
                }
            }
        }
        if let Some(last) = state.get_str("last_used_layout") {
            if last != self.current_name && self.enabled_layouts.iter().any(|x| x == last) {
                self.last_used = Some(last.to_string());
            }
        }
        ok = ok && self.layouts.contains_key(&self.current_name);
        ok
    }
}
