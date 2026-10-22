// SPDX-License-Identifier: GPL-3.0-or-later
//! The CPU-side window model ported from ``kitty/window_list.py``.
//!
//! Windows own no child process, screen or GPU resources here. A [`Window`]
//! stores an opaque [`child_id`](Window::child_id) supplied by the caller plus
//! the metadata layouts and sessions need (title, cwd, running state, username
//! variables and the launch specification used when saving a session).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;

pub type WindowId = u64;
pub type GroupId = u64;
pub type TabId = u64;
pub type OsWindowId = u64;

/// Listener invoked when the active window changes.
pub type ActiveWindowCallback = Box<dyn FnMut(Option<WindowId>, Option<WindowId>)>;

/// Maximum number of entries retained in the focus history, as in kitty.
pub const ACTIVE_GROUP_HISTORY_LIMIT: usize = 64;

/// An edge decoration (margin, padding or border).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Edges {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Edges {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn uniform(v: i32) -> Self {
        Self::new(v, v, v, v)
    }

    pub fn get(&self, edge: Edge) -> i32 {
        match edge {
            Edge::Left => self.left,
            Edge::Top => self.top,
            Edge::Right => self.right,
            Edge::Bottom => self.bottom,
        }
    }

    pub fn set(&mut self, edge: Edge, v: i32) {
        match edge {
            Edge::Left => self.left = v,
            Edge::Top => self.top = v,
            Edge::Right => self.right = v,
            Edge::Bottom => self.bottom = v,
        }
    }
}

/// One of the four sides of a rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Edge {
    Left,
    Top,
    Right,
    Bottom,
}

impl Edge {
    pub const ALL: [Edge; 4] = [Edge::Left, Edge::Top, Edge::Right, Edge::Bottom];

    pub fn is_horizontal(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }

    pub fn opposite(self) -> Edge {
        match self {
            Edge::Left => Edge::Right,
            Edge::Top => Edge::Bottom,
            Edge::Right => Edge::Left,
            Edge::Bottom => Edge::Top,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Edge::Left => "left",
            Edge::Top => "top",
            Edge::Right => "right",
            Edge::Bottom => "bottom",
        }
    }

    pub fn from_name(name: &str) -> Option<Edge> {
        match name {
            "left" => Some(Edge::Left),
            "top" => Some(Edge::Top),
            "right" => Some(Edge::Right),
            "bottom" => Some(Edge::Bottom),
            _ => None,
        }
    }
}

/// The content area allocated to a window by a layout, plus the decorations
/// reserved around it. Ported from kitty's ``WindowGeometry``.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowGeometry {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub spaces: Edges,
    pub compensatory: Edges,
}

impl WindowGeometry {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    /// The region including decorations, as ``(left, top, right, bottom)``.
    pub fn outer_rect(&self) -> (i32, i32, i32, i32) {
        (
            self.left - self.spaces.left,
            self.top - self.spaces.top,
            self.right + self.spaces.right,
            self.bottom + self.spaces.bottom,
        )
    }
}

/// A launch specification attached to a window. Sessions use it verbatim and
/// the model stores it so a tab can be serialised again later.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchSpec {
    /// The program and its arguments (empty means the user's shell).
    pub args: Vec<String>,
    /// ``--cwd`` value, if any. Session context fills this in when absent.
    pub cwd: Option<String>,
    /// ``--title`` override.
    pub title: Option<String>,
    pub env: BTreeMap<String, String>,
    pub vars: BTreeMap<String, String>,
    pub hold: bool,
    pub background: bool,
    pub allow_remote_control: bool,
    /// Launch options that are not interpreted by the model, kept so they can
    /// be round-tripped through a session file.
    pub extra_options: Vec<String>,
    /// Command to run inside a shell at startup (restored shell integration).
    pub shell_startup_command: Vec<String>,
}

impl LaunchSpec {
    pub fn with_args(args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }
}

/// A window in the CPU model.
#[derive(Clone, Debug)]
pub struct Window {
    pub id: WindowId,
    /// Stable id recorded in a serialised layout/session.
    pub serialized_id: WindowId,
    pub title: String,
    pub override_title: Option<String>,
    pub cwd: Option<PathBuf>,
    /// Whether a child process is considered running. No ownership of the
    /// actual child is taken; [`child_id`](Self::child_id) is opaque.
    pub running: bool,
    pub child_id: Option<u64>,
    pub needs_attention: bool,
    pub activity_since_focus: bool,
    /// Set by [`WindowList`] when this window has focus.
    pub focused: bool,
    pub is_visible_in_layout: bool,
    pub geometry: WindowGeometry,
    pub margin: Edges,
    pub padding: Edges,
    pub border_width: i32,
    pub launch: LaunchSpec,
    /// User variables (``--var``) exposed to kittens/remote control.
    pub user_vars: BTreeMap<String, String>,
}

impl Window {
    pub fn new(id: WindowId, title: impl Into<String>) -> Self {
        Self {
            id,
            serialized_id: id,
            title: title.into(),
            override_title: None,
            cwd: None,
            running: true,
            child_id: None,
            needs_attention: false,
            activity_since_focus: false,
            focused: false,
            is_visible_in_layout: true,
            geometry: WindowGeometry::default(),
            margin: Edges::default(),
            padding: Edges::default(),
            border_width: 0,
            launch: LaunchSpec::default(),
            user_vars: BTreeMap::new(),
        }
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_launch(mut self, launch: LaunchSpec) -> Self {
        self.launch = launch;
        self
    }

    pub fn effective_title(&self) -> &str {
        self.override_title.as_deref().unwrap_or(&self.title)
    }

    pub fn effective_border(&self) -> i32 {
        self.border_width
    }

    pub fn effective_padding(&self, edge: Edge) -> i32 {
        self.padding.get(edge)
    }

    pub fn effective_margin(&self, edge: Edge) -> i32 {
        self.margin.get(edge)
    }

    pub fn set_geometry(&mut self, geometry: WindowGeometry) {
        self.geometry = geometry;
    }

    pub fn focus_changed(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn set_visible_in_layout(&mut self, visible: bool) {
        self.is_visible_in_layout = visible;
    }
}

/// A group of windows occupying one pane in a layout. Overlay windows share a
/// group; the last window in the group is the active one.
#[derive(Clone, Debug)]
pub struct WindowGroup {
    pub id: GroupId,
    pub windows: Vec<WindowId>,
    /// The geometry allocated to the group (including space for docks).
    pub allocated_geometry: Option<WindowGeometry>,
}

impl WindowGroup {
    pub fn new(id: GroupId) -> Self {
        Self {
            id,
            windows: Vec::new(),
            allocated_geometry: None,
        }
    }

    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    pub fn contains(&self, id: WindowId) -> bool {
        self.windows.contains(&id)
    }

    pub fn active_window_id(&self) -> Option<WindowId> {
        self.windows.last().copied()
    }

    pub fn main_window_id(&self) -> Option<WindowId> {
        self.windows.first().copied()
    }

    fn add_window(&mut self, id: WindowId, head: bool) {
        if head {
            self.windows.insert(0, id);
        } else {
            self.windows.push(id);
        }
    }

    fn remove_window(&mut self, id: WindowId) {
        self.windows.retain(|w| *w != id);
    }

    fn move_to_top(&mut self, id: WindowId) -> bool {
        let Some(idx) = self.windows.iter().position(|w| *w == id) else {
            return false;
        };
        if idx == self.windows.len() - 1 {
            return false;
        }
        let w = self.windows.remove(idx);
        self.windows.push(w);
        true
    }
}

/// Options for [`WindowList::add_window`], mirroring the arguments of kitty's
/// ``WindowList.add_window``.
#[derive(Clone, Debug, Default)]
pub struct AddWindowOptions {
    pub group_of: Option<WindowId>,
    pub next_to: Option<WindowId>,
    pub before: bool,
    pub make_active: bool,
    pub head_of_group: bool,
}

impl AddWindowOptions {
    pub fn active() -> Self {
        Self {
            make_active: true,
            ..Self::default()
        }
    }
}

/// The list of windows and window groups belonging to a tab.
pub struct WindowList {
    windows: BTreeMap<WindowId, Window>,
    all_windows: Vec<WindowId>,
    groups: Vec<WindowGroup>,
    active_group_idx: i32,
    active_group_history: VecDeque<GroupId>,
    next_group_id: GroupId,
    pub force_show_title_bars: bool,
    dirty_callbacks: Vec<Box<dyn FnMut()>>,
    active_callbacks: Vec<ActiveWindowCallback>,
}

impl Default for WindowList {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowList {
    pub fn new() -> Self {
        Self {
            windows: BTreeMap::new(),
            all_windows: Vec::new(),
            groups: Vec::new(),
            active_group_idx: -1,
            active_group_history: VecDeque::new(),
            next_group_id: 1,
            force_show_title_bars: false,
            dirty_callbacks: Vec::new(),
            active_callbacks: Vec::new(),
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

    pub fn mark_dirty(&mut self) {
        for cb in &mut self.dirty_callbacks {
            cb();
        }
    }

    pub fn len(&self) -> usize {
        self.all_windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.all_windows.is_empty()
    }

    pub fn all_window_ids(&self) -> &[WindowId] {
        &self.all_windows
    }

    pub fn window(&self, id: WindowId) -> Option<&Window> {
        self.windows.get(&id)
    }

    pub fn window_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.get_mut(&id)
    }

    pub fn windows(&self) -> &BTreeMap<WindowId, Window> {
        &self.windows
    }

    pub fn windows_mut(&mut self) -> &mut BTreeMap<WindowId, Window> {
        &mut self.windows
    }

    pub fn groups(&self) -> &[WindowGroup] {
        &self.groups
    }

    pub fn groups_mut(&mut self) -> &mut Vec<WindowGroup> {
        &mut self.groups
    }

    pub fn group(&self, id: GroupId) -> Option<&WindowGroup> {
        self.groups.iter().find(|g| g.id == id)
    }

    pub fn group_mut(&mut self, id: GroupId) -> Option<&mut WindowGroup> {
        self.groups.iter_mut().find(|g| g.id == id)
    }

    pub fn group_for_id(&self, id: GroupId) -> Option<&WindowGroup> {
        self.group(id)
    }

    pub fn group_for_window(&self, id: WindowId) -> Option<&WindowGroup> {
        self.groups.iter().find(|g| g.contains(id))
    }

    pub fn group_idx_for_window(&self, id: WindowId) -> Option<usize> {
        self.groups.iter().position(|g| g.contains(id))
    }

    pub fn group_id_for_window(&self, id: WindowId) -> Option<GroupId> {
        self.group_for_window(id).map(|g| g.id)
    }

    pub fn num_groups(&self) -> usize {
        self.groups.len()
    }

    pub fn active_group_idx(&self) -> i32 {
        self.active_group_idx
    }

    pub fn active_group_index(&self) -> Option<usize> {
        if self.active_group_idx < 0 {
            None
        } else {
            Some(self.active_group_idx as usize)
        }
    }

    pub fn active_group(&self) -> Option<&WindowGroup> {
        self.active_group_index().and_then(|i| self.groups.get(i))
    }

    pub fn active_group_mut(&mut self) -> Option<&mut WindowGroup> {
        let i = self.active_group_index()?;
        self.groups.get_mut(i)
    }

    pub fn active_main_group(&self) -> Option<&WindowGroup> {
        self.active_group()
    }

    pub fn active_window_id(&self) -> Option<WindowId> {
        self.active_group().and_then(|g| g.active_window_id())
    }

    pub fn active_window(&self) -> Option<&Window> {
        self.active_window_id().and_then(|id| self.window(id))
    }

    pub fn active_main_window_id(&self) -> Option<WindowId> {
        self.active_main_group().and_then(|g| g.active_window_id())
    }

    pub fn active_main_window(&self) -> Option<&Window> {
        self.active_main_window_id().and_then(|id| self.window(id))
    }

    pub fn active_group_history(&self) -> &VecDeque<GroupId> {
        &self.active_group_history
    }

    /// Window ids in most-recently-active order (deduplicated), as exposed by
    /// kitty's ``active_window_history``.
    pub fn active_window_history(&self) -> Vec<WindowId> {
        let mut ans = Vec::new();
        for gid in self.active_group_history.iter().rev() {
            if let Some(g) = self.group(*gid) {
                if let Some(w) = g.active_window_id() {
                    if !ans.contains(&w) {
                        ans.push(w);
                    }
                }
            }
        }
        ans
    }

    fn push_history(&mut self, gid: GroupId) {
        self.active_group_history.retain(|x| *x != gid);
        self.active_group_history.push_back(gid);
        while self.active_group_history.len() > ACTIVE_GROUP_HISTORY_LIMIT {
            self.active_group_history.pop_front();
        }
    }

    fn notify_active_change(&mut self, old: Option<WindowId>, new: Option<WindowId>) {
        if old == new {
            return;
        }
        if let Some(id) = old {
            if let Some(w) = self.windows.get_mut(&id) {
                w.focus_changed(false);
            }
        }
        if let Some(id) = new {
            if let Some(w) = self.windows.get_mut(&id) {
                w.focus_changed(true);
            }
        }
        for cb in &mut self.active_callbacks {
            cb(old, new);
        }
    }

    pub fn set_active_group_idx(&mut self, i: usize, notify: bool) -> bool {
        if i >= self.groups.len() {
            return false;
        }
        if i as i32 == self.active_group_idx {
            return false;
        }
        let old = self.active_window_id();
        if let Some(g) = self.active_group_index().and_then(|q| self.groups.get(q)) {
            let gid = g.id;
            self.push_history(gid);
        }
        self.active_group_idx = i as i32;
        let new = self.active_window_id();
        if notify {
            self.notify_active_change(old, new);
        } else if old != new {
            if let Some(id) = old {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.focus_changed(false);
                }
            }
            if let Some(id) = new {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.focus_changed(true);
                }
            }
        }
        old != new
    }

    pub fn set_active_group(&mut self, gid: GroupId) -> bool {
        let Some(i) = self.groups.iter().position(|g| g.id == gid) else {
            return false;
        };
        self.set_active_group_idx(i, true)
    }

    pub fn set_active_window_group_for(&mut self, window_id: WindowId) -> bool {
        let Some(gid) = self.group_id_for_window(window_id) else {
            return false;
        };
        self.set_active_group(gid)
    }

    pub fn move_window_to_top_of_group(&mut self, window_id: WindowId) -> bool {
        let Some(idx) = self.group_idx_for_window(window_id) else {
            return false;
        };
        let old = self.active_window_id();
        if !self.groups[idx].move_to_top(window_id) {
            return false;
        }
        let new = self.active_window_id();
        if old != new {
            self.notify_active_change(old, new);
        }
        old != new
    }

    /// Add a window. By default a new group is created (at the position
    /// requested by ``next_to``/``before``) and it becomes active.
    pub fn add_window(&mut self, window: Window, opts: AddWindowOptions) -> GroupId {
        debug_assert!(!self.windows.contains_key(&window.id));
        let wid = window.id;
        self.windows.insert(wid, window);
        self.all_windows.push(wid);

        let make_active = opts.make_active;
        let target_idx = if let Some(group_of) = opts.group_of {
            self.group_idx_for_window(group_of)
        } else if let Some(next_to) = opts.next_to {
            if let Some(pos) = self.group_idx_for_window(next_to) {
                let gid = self.next_group_id;
                self.next_group_id += 1;
                let insert_at = pos + usize::from(!opts.before);
                self.groups.insert(insert_at, WindowGroup::new(gid));
                Some(insert_at)
            } else {
                None
            }
        } else {
            None
        };

        let target_idx = match target_idx {
            Some(i) => i,
            None => {
                let gid = self.next_group_id;
                self.next_group_id += 1;
                let group = WindowGroup::new(gid);
                if opts.before {
                    self.groups.insert(0, group);
                    0
                } else {
                    self.groups.push(group);
                    self.groups.len() - 1
                }
            }
        };

        let old = self.active_window_id();
        self.groups[target_idx].add_window(wid, opts.head_of_group);
        if make_active {
            let _ = self.set_active_group_idx(target_idx, false);
        }
        let new = self.active_window_id();
        self.notify_active_change(old, new);
        self.groups[target_idx].id
    }

    /// Remove a window, updating focus/history exactly like kitty's
    /// ``WindowList.remove_window``. Returns the removed window.
    pub fn remove_window(&mut self, id: WindowId) -> Option<Window> {
        let old = self.active_window_id();
        let window = self.windows.remove(&id)?;
        self.all_windows.retain(|w| *w != id);
        let Some(idx) = self.groups.iter().position(|g| g.contains(id)) else {
            return Some(window);
        };
        self.groups[idx].remove_window(id);
        if self.groups[idx].is_empty() {
            self.remove_main_group(idx);
        }
        let new = self.active_window_id();
        self.notify_active_change(old, new);
        Some(window)
    }

    /// Remove a window without notifying active listeners, returning it. Used
    /// when moving a window between tabs.
    pub fn take_window(&mut self, id: WindowId) -> Option<Window> {
        let window = self.windows.remove(&id)?;
        self.all_windows.retain(|w| *w != id);
        if let Some(idx) = self.groups.iter().position(|g| g.contains(id)) {
            self.groups[idx].remove_window(id);
            if self.groups[idx].is_empty() {
                self.remove_main_group(idx);
            }
        }
        Some(window)
    }

    fn remove_main_group(&mut self, idx: usize) {
        let group = self.groups.remove(idx);
        if !self.groups.is_empty() {
            let active = self.active_group_idx as usize;
            if active == idx {
                self.make_previous_group_active(1, false);
            } else if active > idx {
                self.active_group_idx -= 1;
            }
        } else {
            self.active_group_idx = -1;
        }
        // Its windows are gone with the group; the caller owns removal of the
        // actual Window objects.
        let _ = group;
    }

    /// Focus the previously active group. ``which`` is 1 for the most recent.
    pub fn make_previous_group_active(&mut self, which: usize, notify: bool) {
        let which = which.max(1);
        let gid_map: HashMap<GroupId, usize> = self
            .groups
            .iter()
            .enumerate()
            .map(|(i, g)| (g.id, i))
            .collect();
        let mut remaining = which;
        let n = self.active_group_history.len();
        for i in 0..n {
            let gid = self.active_group_history[n - i - 1];
            if let Some(idx) = gid_map.get(&gid) {
                remaining -= 1;
                if remaining < 1 {
                    self.set_active_group_idx(*idx, notify);
                    return;
                }
            }
        }
        if !self.groups.is_empty() {
            self.set_active_group_idx(self.groups.len() - 1, notify);
        }
    }

    pub fn activate_next_window_group(&mut self, delta: i64) {
        let n = self.groups.len();
        if n == 0 {
            return;
        }
        let cur = self.active_group_idx.max(0) as i64;
        let mut d = delta % n as i64;
        if d < 0 {
            d += n as i64;
        }
        let next = ((cur + d) % n as i64) as usize;
        self.set_active_group_idx(next, true);
    }

    /// Swap the active group with another one, or move it to a group id.
    pub fn move_window_group(&mut self, by: Option<i64>, to_group: Option<GroupId>) -> bool {
        if self.active_group_idx < 0 || self.groups.is_empty() {
            return false;
        }
        let cur = self.active_group_idx as usize;
        let mut target = None;
        if let Some(by) = by {
            let n = self.groups.len() as i64;
            let mut d = by % n;
            if d < 0 {
                d += n;
            }
            target = Some(((cur as i64 + d) % n) as usize);
        }
        if let Some(gid) = to_group {
            if let Some(i) = self.groups.iter().position(|g| g.id == gid) {
                target = Some(i);
            }
        }
        let Some(target) = target else {
            return false;
        };
        if target == cur {
            return false;
        }
        self.groups.swap(cur, target);
        self.set_active_group_idx(target, true);
        true
    }

    /// Positional insert of the active group next to ``target`` without
    /// changing the order of the other groups.
    pub fn insert_window_group_next_to(&mut self, target_group_id: GroupId, after: bool) -> bool {
        if self.active_group_idx < 0 || self.groups.is_empty() {
            return false;
        }
        let src = self.active_group_idx as usize;
        let Some(mut target) = self.groups.iter().position(|g| g.id == target_group_id) else {
            return false;
        };
        if src == target {
            return false;
        }
        let group = self.groups.remove(src);
        if src < target {
            target -= 1;
        }
        let pos = target + usize::from(after);
        self.groups.insert(pos, group);
        self.set_active_group_idx(pos, true);
        true
    }

    pub fn active_window_in_nth_group(&self, n: i64, clamp: bool) -> Option<&Window> {
        let mut n = n;
        if clamp {
            n = n.max(0).min(self.groups.len().saturating_sub(1) as i64);
        }
        if n < 0 || n as usize >= self.groups.len() {
            return None;
        }
        self.groups[n as usize]
            .active_window_id()
            .and_then(|id| self.window(id))
    }

    pub fn has_more_than_one_visible_group(&self) -> bool {
        self.groups
            .iter()
            .filter(|g| {
                g.windows
                    .last()
                    .and_then(|id| self.window(*id))
                    .map(|w| w.is_visible_in_layout)
                    .unwrap_or(false)
            })
            .count()
            > 1
    }

    /// Layout-state serialisation, matching
    /// ``WindowList.serialize_layout_state``.
    pub fn serialize_layout_state(&self) -> crate::value::Value {
        use crate::value::Value;
        let mut map = Value::map();
        map.insert(
            "active_group_idx".into(),
            Value::Int(self.active_group_idx as i64),
        );
        map.insert(
            "active_group_history".into(),
            Value::List(
                self.active_group_history
                    .iter()
                    .map(|g| Value::Int(*g as i64))
                    .collect(),
            ),
        );
        map.insert(
            "window_groups".into(),
            Value::List(
                self.groups
                    .iter()
                    .map(|g| {
                        let mut m = Value::map();
                        m.insert("id".into(), Value::Int(g.id as i64));
                        m.insert(
                            "window_ids".into(),
                            Value::List(g.windows.iter().map(|w| Value::Int(*w as i64)).collect()),
                        );
                        Value::Map(m)
                    })
                    .collect(),
            ),
        );
        Value::Map(map)
    }

    /// Compute (and optionally apply) the restoration of a serialised layout
    /// state. Returns a map of serialised group id to current group id.
    pub fn unserialize_layout_state(
        &mut self,
        state: &crate::value::Value,
        window_id_map: &HashMap<WindowId, WindowId>,
        apply: bool,
    ) -> Option<HashMap<GroupId, GroupId>> {
        use crate::value::Value;
        let groups_state = state.get("window_groups")?.as_list()?;
        let current_window_ids: std::collections::HashSet<WindowId> = self
            .groups
            .iter()
            .flat_map(|g| g.windows.iter().copied())
            .collect();
        let mapped_values: std::collections::HashSet<WindowId> =
            window_id_map.values().copied().collect();
        if !current_window_ids.is_subset(&mapped_values) {
            return None;
        }

        // Keep the current group order, matching kitty's insertion-ordered
        // gmap: when several groups are valid matches the first one wins.
        let mut present: Vec<(GroupId, std::collections::HashSet<WindowId>)> = self
            .groups
            .iter()
            .map(|g| (g.id, g.windows.iter().copied().collect()))
            .collect();

        let mut ans = HashMap::new();
        for wg in groups_state {
            let old_gid = wg.get_i64("id")? as GroupId;
            let wids = wg.get("window_ids")?.as_list()?;
            let mut mapped_wids = std::collections::HashSet::new();
            for wid in wids {
                let old_wid = wid.as_i64()? as WindowId;
                if let Some(new_wid) = window_id_map.get(&old_wid) {
                    mapped_wids.insert(*new_wid);
                }
            }
            let found = present
                .iter()
                .position(|(_gid, wids)| wids.is_subset(&mapped_wids));
            if let Some(pos) = found {
                let (gid, _) = present.remove(pos);
                ans.insert(old_gid, gid);
            }
        }
        if ans.len() != self.groups.len() {
            return None;
        }
        if !apply {
            return Some(ans);
        }

        let mut new_groups = Vec::new();
        for wg in groups_state {
            let old_gid = wg.get_i64("id")? as GroupId;
            let Some(new_gid) = ans.get(&old_gid).copied() else {
                continue;
            };
            let Some(mut group) = self.groups.iter().find(|g| g.id == new_gid).cloned() else {
                continue;
            };
            let wids = wg.get("window_ids")?.as_list()?;
            let mut new_window_ids = Vec::new();
            for wid in wids {
                let old_wid = wid.as_i64()? as WindowId;
                if let Some(new_wid) = window_id_map.get(&old_wid) {
                    new_window_ids.push(*new_wid);
                }
            }
            let order: HashMap<WindowId, usize> = new_window_ids
                .iter()
                .enumerate()
                .map(|(i, w)| (*w, i))
                .collect();
            group
                .windows
                .sort_by_key(|w| order.get(w).copied().unwrap_or(usize::MAX));
            new_groups.push(group);
        }
        let active_group_id = self.active_group().map(|g| g.id);
        self.groups = new_groups;
        if let Some(agid) = active_group_id {
            if let Some(i) = self.groups.iter().position(|g| g.id == agid) {
                self.active_group_idx = i as i32;
            } else {
                self.active_group_idx = -1;
            }
        } else {
            self.active_group_idx = -1;
        }
        let mut history = VecDeque::new();
        if let Some(items) = state.get("active_group_history").and_then(Value::as_list) {
            for item in items {
                let old = item.as_i64()? as GroupId;
                // kitty maps these through the window map; accept either a
                // group-map hit or a window-map hit for compatibility.
                if let Some(new) = ans.get(&old) {
                    history.push_back(*new);
                } else if let Some(new) = window_id_map.get(&(old as WindowId)) {
                    if let Some(gid) = self.groups.iter().find(|g| g.contains(*new)).map(|g| g.id) {
                        history.push_back(gid);
                    }
                }
            }
        }
        self.active_group_history = history;
        Some(ans)
    }

    /// Build the serialised-id to current-id map used by layout restoration.
    pub fn window_id_map_for_layout(&self) -> HashMap<WindowId, WindowId> {
        self.windows
            .values()
            .filter(|w| w.serialized_id != 0)
            .map(|w| (w.serialized_id, w.id))
            .collect()
    }
}
