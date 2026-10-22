// SPDX-License-Identifier: GPL-3.0-or-later
//! Tabs and tab-manager state, ported from ``kitty/tabs.py``.
//!
//! A tab owns a [`WindowList`] and a [`LayoutManager`]. No OS window, tab bar
//! rendering or child process is owned here.

use std::collections::VecDeque;
use std::path::PathBuf;

use crate::layout::{LayoutAddOptions, LayoutConfig, LayoutManager, Neighbors};
use crate::model::{
    ActiveWindowCallback, AddWindowOptions, GroupId, OsWindowId, TabId, Window, WindowId,
    WindowList,
};

/// One tab in the model.
pub struct Tab {
    pub id: TabId,
    pub name: String,
    pub cwd: Option<PathBuf>,
    pub created_in_session_name: String,
    windows: WindowList,
    layouts: LayoutManager,
    dirty: bool,
    dirty_callbacks: Vec<Box<dyn FnMut()>>,
}

impl Tab {
    pub fn new(
        id: TabId,
        name: impl Into<String>,
        cwd: Option<PathBuf>,
        enabled_layouts: &[String],
        config: LayoutConfig,
    ) -> Result<Self, String> {
        let layouts = LayoutManager::new(1, id, enabled_layouts, config)?;
        Ok(Self {
            id,
            name: name.into(),
            cwd,
            created_in_session_name: String::new(),
            windows: WindowList::new(),
            layouts,
            dirty: false,
            dirty_callbacks: Vec::new(),
        })
    }

    pub fn windows(&self) -> &WindowList {
        &self.windows
    }

    pub fn windows_mut(&mut self) -> &mut WindowList {
        &mut self.windows
    }

    pub fn layouts(&self) -> &LayoutManager {
        &self.layouts
    }

    pub fn layouts_mut(&mut self) -> &mut LayoutManager {
        &mut self.layouts
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    pub fn add_dirty_callback(&mut self, cb: impl FnMut() + 'static) {
        self.dirty_callbacks.push(Box::new(cb));
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        for cb in &mut self.dirty_callbacks {
            cb();
        }
    }

    /// Add a window using the current layout's placement rules.
    pub fn add_window(&mut self, window: Window, opts: &LayoutAddOptions) -> GroupId {
        let id = window.id;
        self.layouts.add_window(&mut self.windows, window, opts);
        self.mark_dirty();
        self.windows
            .group_id_for_window(id)
            .or_else(|| self.windows.active_group().map(|g| g.id))
            .unwrap_or(0)
    }

    /// Add a window directly to a group/position without consulting the layout
    /// (used by session restore before layouts are applied).
    pub fn add_window_raw(&mut self, window: Window, opts: AddWindowOptions) -> GroupId {
        let gid = self.windows.add_window(window, opts);
        self.mark_dirty();
        gid
    }

    /// Close a window and update the layout tree.
    pub fn close_window(&mut self, window_id: WindowId) -> bool {
        let ans = self.layouts.remove_window(&mut self.windows, window_id);
        if ans {
            self.mark_dirty();
        }
        ans
    }

    /// Remove a window from this tab and return it, keeping it alive so it can
    /// be attached to another tab.
    pub fn detach_window(&mut self, window_id: WindowId) -> Option<Window> {
        let gid = self.windows.group_id_for_window(window_id)?;
        let window = self.windows.take_window(window_id)?;
        if self.windows.group(gid).is_none() {
            self.layouts
                .current_mut()
                .remove_windows(&mut self.windows, &[gid]);
        }
        self.layouts.apply(&mut self.windows);
        self.mark_dirty();
        Some(window)
    }

    pub fn set_active_window(&mut self, window_id: WindowId) -> bool {
        let ans = self.windows.set_active_window_group_for(window_id);
        if ans {
            self.windows.move_window_to_top_of_group(window_id);
            self.mark_dirty();
        }
        ans
    }

    pub fn next_window(&mut self, delta: i64) {
        self.layouts.current().next_window(&mut self.windows, delta);
        self.mark_dirty();
    }

    pub fn nth_window(&mut self, num: i64) -> Option<WindowId> {
        self.layouts.current().nth_window(&self.windows, num)
    }

    pub fn activate_nth_window(&mut self, num: usize) {
        self.layouts
            .current()
            .activate_nth_window(&mut self.windows, num);
        self.mark_dirty();
    }

    pub fn neighbors(&self) -> Neighbors {
        self.layouts.neighbors(&self.windows)
    }

    pub fn neighbors_for_window(&self, window_id: WindowId) -> Neighbors {
        self.layouts.neighbors_for_window(window_id, &self.windows)
    }

    pub fn neighboring_window(&mut self, edge: crate::model::Edge) -> Option<WindowId> {
        let neighbors = self.neighbors();
        let gids = neighbors.get(edge);
        if gids.is_empty() {
            return None;
        }
        // Prefer the most recently focused candidate, as kitty does.
        let history = self.windows.active_window_history();
        let mut chosen = gids[0];
        for wid in history {
            if let Some(gid) = self.windows.group_id_for_window(wid) {
                if gids.contains(&gid) {
                    chosen = gid;
                    break;
                }
            }
        }
        self.windows.set_active_group(chosen);
        self.mark_dirty();
        self.windows.active_window_id()
    }

    pub fn resize_window(
        &mut self,
        window_id: WindowId,
        increment: f64,
        is_horizontal: bool,
    ) -> bool {
        let ans =
            self.layouts
                .resize_window(&mut self.windows, window_id, increment, is_horizontal);
        if ans {
            self.layouts.apply(&mut self.windows);
            self.mark_dirty();
        }
        ans
    }

    pub fn resize_active_window(&mut self, increment: f64, is_horizontal: bool) -> bool {
        match self.windows.active_window_id() {
            Some(id) => self.resize_window(id, increment, is_horizontal),
            None => false,
        }
    }

    pub fn move_window(&mut self, delta: i64) -> bool {
        let ans = self.layouts.move_window(&mut self.windows, delta);
        if ans {
            self.mark_dirty();
        }
        ans
    }

    pub fn move_window_to_group(&mut self, group: GroupId) -> bool {
        let ans = self.layouts.move_window_to_group(&mut self.windows, group);
        if ans {
            self.mark_dirty();
        }
        ans
    }

    /// Cycle to the next/previous enabled layout.
    pub fn next_layout(&mut self, delta: i64) -> String {
        let name = self.layouts.next_layout(delta).to_string();
        self.layouts.apply(&mut self.windows);
        self.mark_dirty();
        name
    }

    pub fn set_layout(&mut self, name: &str) -> Result<(), String> {
        self.layouts.set_layout(name)?;
        self.layouts.apply(&mut self.windows);
        self.mark_dirty();
        Ok(())
    }

    pub fn set_enabled_layouts(&mut self, names: &[String]) -> Result<(), String> {
        self.layouts.set_enabled_layouts(names)?;
        self.layouts.apply(&mut self.windows);
        self.mark_dirty();
        Ok(())
    }

    pub fn layout_action(&mut self, action: &str, args: &[String]) -> Option<bool> {
        let ans = self.layouts.layout_action(action, args, &mut self.windows);
        if ans == Some(true) {
            self.mark_dirty();
        }
        ans
    }

    pub fn apply_layout(&mut self) {
        self.layouts.apply(&mut self.windows);
    }

    /// Restore serialized layout state (including other layouts) for this tab.
    ///
    /// This convenience borrows both the layout manager and window list
    /// internally, which callers cannot do through the individual `_mut`
    /// accessors.
    pub fn unserialize_layout_state_from_session(&mut self, state: &crate::value::Value) -> bool {
        self.layouts
            .unserialize_layout_state_from_session(state, &mut self.windows)
    }

    pub fn active_window(&self) -> Option<&Window> {
        self.windows.active_window()
    }

    pub fn active_window_id(&self) -> Option<WindowId> {
        self.windows.active_window_id()
    }

    pub fn effective_title(&self) -> String {
        if !self.name.is_empty() {
            self.name.clone()
        } else {
            self.windows
                .active_window()
                .map(|w| w.effective_title().to_string())
                .unwrap_or_default()
        }
    }

    pub fn set_title(&mut self, title: impl Into<String>) {
        self.name = title.into();
        self.mark_dirty();
    }
}

/// Metadata about the OS window that owns a [`TabManager`]. The GUI layer is
/// responsible for applying it; the model only stores and serialises it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OsWindowMetadata {
    pub class: Option<String>,
    pub name: Option<String>,
    pub title: Option<String>,
    pub state: Option<String>,
    pub size: Option<OsWindowSize>,
    pub position: Option<(i32, i32)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OsWindowSize {
    pub width: u32,
    pub width_unit: String,
    pub height: u32,
    pub height_unit: String,
}

/// All tabs in one OS window plus tab focus history.
pub struct TabManager {
    pub os_window_id: OsWindowId,
    pub os_window: OsWindowMetadata,
    tabs: Vec<Tab>,
    active_tab_idx: usize,
    history: VecDeque<TabId>,
    next_tab_id: TabId,
    dirty_callbacks: Vec<Box<dyn FnMut()>>,
    active_callbacks: Vec<ActiveWindowCallback>,
    dirty: bool,
}

impl TabManager {
    pub fn new(os_window_id: OsWindowId) -> Self {
        Self {
            os_window_id,
            os_window: OsWindowMetadata::default(),
            tabs: Vec::new(),
            active_tab_idx: 0,
            history: VecDeque::new(),
            next_tab_id: 1,
            dirty_callbacks: Vec::new(),
            active_callbacks: Vec::new(),
            dirty: false,
        }
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn tabs_mut(&mut self) -> &mut [Tab] {
        &mut self.tabs
    }

    pub fn tab(&self, idx: usize) -> Option<&Tab> {
        self.tabs.get(idx)
    }

    pub fn tab_mut(&mut self, idx: usize) -> Option<&mut Tab> {
        self.tabs.get_mut(idx)
    }

    pub fn tab_for_id(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn active_tab_idx(&self) -> usize {
        self.active_tab_idx
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active_tab_idx)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active_tab_idx)
    }

    pub fn active_window(&self) -> Option<&Window> {
        self.active_tab().and_then(Tab::active_window)
    }

    pub fn active_window_id(&self) -> Option<WindowId> {
        self.active_tab().and_then(Tab::active_window_id)
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = false;
        for t in &mut self.tabs {
            t.clear_dirty();
        }
    }

    pub fn add_dirty_callback(&mut self, cb: impl FnMut() + 'static) {
        self.dirty_callbacks.push(Box::new(cb));
    }

    pub fn add_active_callback(
        &mut self,
        cb: impl FnMut(Option<WindowId>, Option<WindowId>) + 'static,
    ) {
        self.active_callbacks.push(Box::new(cb));
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
        for cb in &mut self.dirty_callbacks {
            cb();
        }
    }

    fn notify_active(&mut self, old: Option<WindowId>, new: Option<WindowId>) {
        if old == new {
            return;
        }
        for cb in &mut self.active_callbacks {
            cb(old, new);
        }
    }

    /// Append a new tab and focus it.
    pub fn add_tab(
        &mut self,
        name: impl Into<String>,
        cwd: Option<PathBuf>,
        enabled_layouts: &[String],
        config: LayoutConfig,
    ) -> Result<TabId, String> {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let tab = Tab::new(id, name, cwd, enabled_layouts, config)?;
        let old = self.active_window_id();
        self.tabs.push(tab);
        let idx = self.tabs.len() - 1;
        self.active_tab_idx = idx;
        if self.tabs.len() > 1 {
            let prev_id = self.tabs[idx - 1].id;
            self.history.push_back(prev_id);
        }
        let new = self.active_window_id();
        self.notify_active(old, new);
        self.mark_dirty();
        Ok(id)
    }

    pub fn set_active_tab_idx(&mut self, idx: usize) -> bool {
        if idx >= self.tabs.len() || idx == self.active_tab_idx {
            return false;
        }
        let old = self.active_window_id();
        let old_tab_id = self.tabs[self.active_tab_idx].id;
        self.history.retain(|x| *x != old_tab_id);
        self.history.push_back(old_tab_id);
        while self.history.len() > 64 {
            self.history.pop_front();
        }
        self.active_tab_idx = idx;
        let new = self.active_window_id();
        self.notify_active(old, new);
        self.mark_dirty();
        true
    }

    pub fn set_active_tab(&mut self, id: TabId) -> bool {
        match self.tabs.iter().position(|t| t.id == id) {
            Some(idx) => self.set_active_tab_idx(idx),
            None => false,
        }
    }

    pub fn next_tab(&mut self, delta: i64) -> bool {
        if self.tabs.len() < 2 {
            return false;
        }
        let n = self.tabs.len() as i64;
        let mut d = delta % n;
        if d < 0 {
            d += n;
        }
        let next = ((self.active_tab_idx as i64 + d) % n) as usize;
        self.set_active_tab_idx(next)
    }

    pub fn previous_tab(&mut self) -> bool {
        self.next_tab(-1)
    }

    /// Move the active tab by ``delta`` positions, preserving which tab is
    /// active. Mirrors ``TabManager.move_tab``.
    pub fn move_tab(&mut self, delta: i64) -> bool {
        if self.tabs.len() < 2 {
            return false;
        }
        let n = self.tabs.len() as i64;
        let mut d = delta % n;
        if d < 0 {
            d += n;
        }
        if d == 0 {
            return false;
        }
        let idx = self.active_tab_idx;
        let new_idx = ((idx as i64 + d) % n) as usize;
        let step: i64 = if idx < new_idx { 1 } else { -1 };
        let mut i = idx as i64;
        while i != new_idx as i64 {
            let j = i + step;
            self.tabs.swap(i as usize, j as usize);
            i = j;
        }
        self.active_tab_idx = new_idx;
        self.mark_dirty();
        true
    }

    /// Close a tab. Returns the removed tab, or ``None`` if ``idx`` is invalid.
    /// The last remaining tab is not removed (there would be nowhere to put its
    /// windows in the pure model).
    pub fn close_tab(&mut self, idx: usize) -> Option<Tab> {
        if idx >= self.tabs.len() {
            return None;
        }
        if self.tabs.len() == 1 {
            return None;
        }
        let old = self.active_window_id();
        let was_active = idx == self.active_tab_idx;
        let removed = self.tabs.remove(idx);
        self.history.retain(|x| *x != removed.id);
        if was_active {
            // Prefer the most recently used tab, like kitty's default
            // "previous" tab_switch_strategy.
            let mut found = None;
            while let Some(tid) = self.history.pop_back() {
                if tid == removed.id {
                    continue;
                }
                if let Some(i) = self.tabs.iter().position(|t| t.id == tid) {
                    found = Some(i);
                    break;
                }
            }
            self.active_tab_idx = found.unwrap_or_else(|| idx.min(self.tabs.len() - 1));
        } else if idx < self.active_tab_idx {
            self.active_tab_idx -= 1;
        }
        let new = self.active_window_id();
        self.notify_active(old, new);
        self.mark_dirty();
        Some(removed)
    }

    pub fn add_window(
        &mut self,
        tab_idx: usize,
        window: Window,
        opts: &LayoutAddOptions,
    ) -> Option<GroupId> {
        let tab = self.tabs.get_mut(tab_idx)?;
        let gid = tab.add_window(window, opts);
        self.mark_dirty();
        Some(gid)
    }

    pub fn close_window(&mut self, tab_idx: usize, window_id: WindowId) -> bool {
        let old = self.active_window_id();
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        let ans = tab.close_window(window_id);
        if !ans {
            return false;
        }
        if tab.windows().is_empty() && self.tabs.len() > 1 {
            self.close_tab(tab_idx);
        }
        let new = self.active_window_id();
        self.notify_active(old, new);
        self.mark_dirty();
        true
    }

    /// Close the active window, then its tab if it became empty.
    pub fn close_active_window(&mut self) -> bool {
        match self.active_window_id() {
            Some(id) => self.close_window(self.active_tab_idx, id),
            None => false,
        }
    }

    /// Move a window from one tab to another, preserving its id and metadata.
    pub fn move_window_to_tab(
        &mut self,
        from_tab_idx: usize,
        window_id: WindowId,
        to_tab_id: TabId,
    ) -> bool {
        let Some(to_idx) = self.tabs.iter().position(|t| t.id == to_tab_id) else {
            return false;
        };
        if from_tab_idx == to_idx {
            return false;
        }
        let Some(window) = self
            .tabs
            .get_mut(from_tab_idx)
            .and_then(|t| t.detach_window(window_id))
        else {
            return false;
        };
        let tab = &mut self.tabs[to_idx];
        tab.add_window(
            window,
            &LayoutAddOptions {
                next_to: tab.active_window_id(),
                ..Default::default()
            },
        );
        // If the source tab is now empty and it is not the only tab, remove it.
        if self.tabs[from_tab_idx].windows().is_empty() && self.tabs.len() > 1 {
            self.close_tab(from_tab_idx);
        }
        self.mark_dirty();
        true
    }

    pub fn move_active_window_to_tab(&mut self, target_tab_id: TabId) -> bool {
        let Some(wid) = self.active_window_id() else {
            return false;
        };
        let idx = self.active_tab_idx;
        self.move_window_to_tab(idx, wid, target_tab_id)
    }

    pub fn next_layout(&mut self, delta: i64) -> Option<String> {
        let tab = self.active_tab_mut()?;
        let name = tab.next_layout(delta);
        Some(name)
    }
}
