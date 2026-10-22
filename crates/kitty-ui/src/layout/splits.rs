// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of ``kitty/layout/splits.py``: a binary tree of panes.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::base::{
    all_groups, xlayout, ylayout, Bias, LayoutAddOptions, LayoutConfig, LayoutImpl, Neighbors,
    WindowLocation,
};
use crate::model::{Edge, GroupId, Window, WindowGeometry, WindowId, WindowList};
use crate::value::Value;

#[derive(Clone, Debug)]
pub enum SplitChild {
    Leaf(GroupId),
    Pair(Box<Pair>),
}

impl SplitChild {
    fn leaf_id(&self) -> Option<GroupId> {
        match self {
            SplitChild::Leaf(id) => Some(*id),
            SplitChild::Pair(_) => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Pair {
    pub horizontal: bool,
    pub one: Option<SplitChild>,
    pub two: Option<SplitChild>,
    pub bias: f64,
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
    pub first_extent: (i32, i32, i32, i32),
    pub second_extent: (i32, i32, i32, i32),
    pub border_width: i32,
}

impl Default for Pair {
    fn default() -> Self {
        Self::new(true)
    }
}

impl Pair {
    pub fn new(horizontal: bool) -> Self {
        Self {
            horizontal,
            one: None,
            two: None,
            bias: 0.5,
            left: 0,
            top: 0,
            width: 0,
            height: 0,
            first_extent: (0, 0, 0, 0),
            second_extent: (0, 0, 0, 0),
            border_width: 0,
        }
    }

    pub fn is_redundant(&self) -> bool {
        self.one.is_none() || self.two.is_none()
    }

    pub fn all_window_ids(&self) -> Vec<GroupId> {
        let mut ans = Vec::new();
        self.collect_ids(&mut ans);
        ans
    }

    fn collect_ids(&self, out: &mut Vec<GroupId>) {
        if let Some(one) = &self.one {
            match one {
                SplitChild::Leaf(id) => out.push(*id),
                SplitChild::Pair(p) => p.collect_ids(out),
            }
        }
        if let Some(two) = &self.two {
            match two {
                SplitChild::Leaf(id) => out.push(*id),
                SplitChild::Pair(p) => p.collect_ids(out),
            }
        }
    }

    pub fn contains(&self, id: GroupId) -> bool {
        match (&self.one, &self.two) {
            (Some(SplitChild::Leaf(a)), _) if *a == id => true,
            (_, Some(SplitChild::Leaf(b))) if *b == id => true,
            (Some(SplitChild::Pair(p)), _) if p.contains(id) => true,
            (_, Some(SplitChild::Pair(p))) if p.contains(id) => true,
            _ => false,
        }
    }

    fn for_each_pair<'a>(&'a self, out: &mut Vec<&'a Pair>) {
        out.push(self);
        if let Some(SplitChild::Pair(p)) = &self.one {
            p.for_each_pair(out);
        }
        if let Some(SplitChild::Pair(p)) = &self.two {
            p.for_each_pair(out);
        }
    }

    fn for_each_pair_mut(&mut self, f: &mut impl FnMut(&mut Pair)) {
        f(self);
        if let Some(SplitChild::Pair(p)) = &mut self.one {
            p.for_each_pair_mut(f);
        }
        if let Some(SplitChild::Pair(p)) = &mut self.two {
            p.for_each_pair_mut(f);
        }
    }

    pub fn pairs(&self) -> Vec<&Pair> {
        let mut ans = Vec::new();
        self.for_each_pair(&mut ans);
        ans
    }

    pub fn pair_for_window(&self, id: GroupId) -> Option<&Pair> {
        if self.one.as_ref().and_then(SplitChild::leaf_id) == Some(id)
            || self.two.as_ref().and_then(SplitChild::leaf_id) == Some(id)
        {
            return Some(self);
        }
        if let Some(SplitChild::Pair(p)) = &self.one {
            if let Some(found) = p.pair_for_window(id) {
                return Some(found);
            }
        }
        if let Some(SplitChild::Pair(p)) = &self.two {
            if let Some(found) = p.pair_for_window(id) {
                return Some(found);
            }
        }
        None
    }

    pub fn pair_for_window_mut(&mut self, id: GroupId) -> Option<&mut Pair> {
        let here = self.one.as_ref().and_then(SplitChild::leaf_id) == Some(id)
            || self.two.as_ref().and_then(SplitChild::leaf_id) == Some(id);
        if here {
            return Some(self);
        }
        if let Some(SplitChild::Pair(p)) = &mut self.one {
            if p.contains(id) {
                return p.pair_for_window_mut(id);
            }
        }
        if let Some(SplitChild::Pair(p)) = &mut self.two {
            if p.contains(id) {
                return p.pair_for_window_mut(id);
            }
        }
        None
    }

    pub fn count_axis_units(&self, horizontal: bool) -> usize {
        if self.horizontal != horizontal {
            return 1;
        }
        let one = match &self.one {
            Some(SplitChild::Pair(p)) => p.count_axis_units(horizontal),
            Some(SplitChild::Leaf(_)) => 1,
            None => 0,
        };
        let two = match &self.two {
            Some(SplitChild::Pair(p)) => p.count_axis_units(horizontal),
            Some(SplitChild::Leaf(_)) => 1,
            None => 0,
        };
        one + two
    }

    pub fn serialize(&self) -> Value {
        let mut m = Value::map();
        if !self.horizontal {
            m.insert("horizontal".into(), Value::Bool(false));
        }
        if self.bias != 0.5 {
            m.insert("bias".into(), Value::Float(self.bias));
        }
        if let Some(one) = &self.one {
            m.insert(
                "one".into(),
                match one {
                    SplitChild::Leaf(id) => Value::Int(*id as i64),
                    SplitChild::Pair(p) => p.serialize(),
                },
            );
        }
        if let Some(two) = &self.two {
            m.insert(
                "two".into(),
                match two {
                    SplitChild::Leaf(id) => Value::Int(*id as i64),
                    SplitChild::Pair(p) => p.serialize(),
                },
            );
        }
        Value::Map(m)
    }

    pub fn from_value(value: &Value, map: &HashMap<GroupId, GroupId>) -> Option<Pair> {
        let mut pair = Pair::new(
            value
                .get("horizontal")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        );
        pair.bias = value.get_f64("bias").unwrap_or(0.5);
        fn child(value: &Value, map: &HashMap<GroupId, GroupId>) -> Option<SplitChild> {
            match value {
                Value::Int(id) => map.get(&(*id as GroupId)).copied().map(SplitChild::Leaf),
                Value::Map(_) => {
                    let p = Pair::from_value(value, map)?;
                    if p.one.is_some() || p.two.is_some() {
                        Some(SplitChild::Pair(Box::new(p)))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        pair.one = value.get("one").and_then(|v| child(v, map));
        pair.two = value.get("two").and_then(|v| child(v, map));
        Some(pair)
    }

    pub fn unserialize(&mut self, value: &Value, map: &HashMap<GroupId, GroupId>) -> bool {
        if let Some(p) = Pair::from_value(value, map) {
            *self = p;
            true
        } else {
            false
        }
    }

    pub fn balanced_add(&mut self, window_id: GroupId) {
        if self.one.is_none() || self.two.is_none() {
            if self.one.is_none() {
                if self.two.is_none() {
                    self.one = Some(SplitChild::Leaf(window_id));
                    return;
                }
                std::mem::swap(&mut self.one, &mut self.two);
            }
            self.two = Some(SplitChild::Leaf(window_id));
            return;
        }
        if matches!(self.one, Some(SplitChild::Pair(_)))
            && matches!(self.two, Some(SplitChild::Pair(_)))
        {
            let one_count = self
                .one
                .as_ref()
                .map(|c| match c {
                    SplitChild::Pair(p) => p.all_window_ids().len(),
                    _ => 0,
                })
                .unwrap_or(0);
            let two_count = self
                .two
                .as_ref()
                .map(|c| match c {
                    SplitChild::Pair(p) => p.all_window_ids().len(),
                    _ => 0,
                })
                .unwrap_or(0);
            if one_count < two_count {
                if let Some(SplitChild::Pair(p)) = &mut self.one {
                    p.balanced_add(window_id);
                }
            } else if let Some(SplitChild::Pair(p)) = &mut self.two {
                p.balanced_add(window_id);
            }
            return;
        }
        if !matches!(self.one, Some(SplitChild::Pair(_)))
            && !matches!(self.two, Some(SplitChild::Pair(_)))
        {
            let mut pair = Pair::new(self.horizontal);
            if let Some(SplitChild::Leaf(a)) = self.one.take() {
                pair.balanced_add(a);
            }
            if let Some(SplitChild::Leaf(b)) = self.two.take() {
                pair.balanced_add(b);
            }
            self.one = Some(SplitChild::Pair(Box::new(pair)));
            self.two = Some(SplitChild::Leaf(window_id));
            return;
        }
        let (window_to_be_split, target) = if matches!(self.one, Some(SplitChild::Pair(_))) {
            let old = self.two.take();
            self.two = Some(SplitChild::Pair(Box::new(Pair::new(self.horizontal))));
            (old, self.two.as_mut())
        } else {
            let old = self.one.take();
            self.one = Some(SplitChild::Pair(Box::new(Pair::new(self.horizontal))));
            (old, self.one.as_mut())
        };
        if let Some(SplitChild::Pair(pair)) = target {
            if let Some(SplitChild::Leaf(w)) = window_to_be_split {
                pair.balanced_add(w);
            }
            pair.balanced_add(window_id);
        }
    }

    pub fn split_and_add(
        &mut self,
        existing_window_id: GroupId,
        new_window_id: GroupId,
        horizontal: bool,
        after: bool,
    ) {
        let q = if after {
            (existing_window_id, new_window_id)
        } else {
            (new_window_id, existing_window_id)
        };
        if self.is_redundant() {
            self.horizontal = horizontal;
            self.one = Some(SplitChild::Leaf(q.0));
            self.two = Some(SplitChild::Leaf(q.1));
            return;
        }
        let mut pair = Pair::new(horizontal);
        pair.balanced_add(q.0);
        pair.balanced_add(q.1);
        let is_one = self.one.as_ref().and_then(SplitChild::leaf_id) == Some(existing_window_id);
        if is_one {
            self.one = Some(SplitChild::Pair(Box::new(pair)));
        } else {
            self.two = Some(SplitChild::Pair(Box::new(pair)));
        }
    }

    pub fn remove_windows(&mut self, ids: &HashSet<GroupId>) {
        if let Some(SplitChild::Leaf(id)) = self.one {
            if ids.contains(&id) {
                self.one = None;
            }
        }
        if let Some(SplitChild::Leaf(id)) = self.two {
            if ids.contains(&id) {
                self.two = None;
            }
        }
        if self.one.is_none() && self.two.is_some() {
            std::mem::swap(&mut self.one, &mut self.two);
        }
    }

    pub fn remove_windows_deep(&mut self, ids: &HashSet<GroupId>) {
        if let Some(SplitChild::Pair(p)) = &mut self.one {
            p.remove_windows_deep(ids);
        }
        if let Some(SplitChild::Pair(p)) = &mut self.two {
            p.remove_windows_deep(ids);
        }
        self.remove_windows(ids);
    }

    pub fn collapse_redundant_pairs(&mut self) {
        if let Some(SplitChild::Pair(p)) = &mut self.one {
            p.collapse_redundant_pairs();
        }
        if let Some(SplitChild::Pair(p)) = &mut self.two {
            p.collapse_redundant_pairs();
        }
        loop {
            let replace = match &self.one {
                Some(SplitChild::Pair(p)) if p.is_redundant() => {
                    Some(p.one.clone().or_else(|| p.two.clone()))
                }
                _ => None,
            };
            match replace {
                Some(Some(child)) => self.one = Some(child),
                Some(None) => self.one = None,
                None => break,
            }
        }
        loop {
            let replace = match &self.two {
                Some(SplitChild::Pair(p)) if p.is_redundant() => {
                    Some(p.one.clone().or_else(|| p.two.clone()))
                }
                _ => None,
            };
            match replace {
                Some(Some(child)) => self.two = Some(child),
                Some(None) => self.two = None,
                None => break,
            }
        }
        if self.one.is_none() && self.two.is_some() {
            std::mem::swap(&mut self.one, &mut self.two);
        }
    }

    pub fn window_weights(&self) -> BTreeMap<GroupId, f64> {
        let mut ans = BTreeMap::new();
        fn walk(child: &SplitChild, weight: f64, out: &mut BTreeMap<GroupId, f64>) {
            match child {
                SplitChild::Leaf(id) => {
                    out.insert(*id, weight);
                }
                SplitChild::Pair(p) => {
                    if p.is_redundant() {
                        if let Some(c) = p.one.as_ref().or(p.two.as_ref()) {
                            walk(c, weight, out);
                        }
                    } else {
                        if let Some(c) = &p.one {
                            walk(c, weight * p.bias, out);
                        }
                        if let Some(c) = &p.two {
                            walk(c, weight * (1.0 - p.bias), out);
                        }
                    }
                }
            }
        }
        if self.is_redundant() {
            if let Some(c) = self.one.as_ref().or(self.two.as_ref()) {
                walk(c, 1.0, &mut ans);
            }
        } else {
            if let Some(c) = &self.one {
                walk(c, self.bias, &mut ans);
            }
            if let Some(c) = &self.two {
                walk(c, 1.0 - self.bias, &mut ans);
            }
        }
        ans
    }

    pub fn set_window_weights(&mut self, weights: &BTreeMap<GroupId, f64>) {
        fn tune(child: &mut SplitChild, weights: &BTreeMap<GroupId, f64>) -> f64 {
            match child {
                SplitChild::Leaf(id) => weights.get(id).copied().unwrap_or(0.0),
                SplitChild::Pair(p) => tune_pair(p, weights),
            }
        }
        fn tune_pair(pair: &mut Pair, weights: &BTreeMap<GroupId, f64>) -> f64 {
            let one = pair.one.as_mut().map(|c| tune(c, weights)).unwrap_or(0.0);
            let two = pair.two.as_mut().map(|c| tune(c, weights)).unwrap_or(0.0);
            if one > 0.0 && two > 0.0 {
                pair.bias = one / (one + two);
            }
            one + two
        }
        tune_pair(self, weights);
    }

    pub fn preserve_weights_on_removal(&mut self, removed: &HashSet<GroupId>) -> bool {
        fn tune(
            child_slot: &mut Option<SplitChild>,
            weight: f64,
            removed: &HashSet<GroupId>,
            parent_horizontal: bool,
        ) -> (f64, bool) {
            let Some(child) = child_slot else {
                return (0.0, false);
            };
            match child {
                SplitChild::Leaf(id) => {
                    if removed.contains(id) {
                        (0.0, false)
                    } else {
                        (weight, true)
                    }
                }
                SplitChild::Pair(p) => {
                    if p.horizontal != parent_horizontal {
                        let alive = p.preserve_weights_on_removal(removed);
                        if alive {
                            (weight, true)
                        } else {
                            (0.0, false)
                        }
                    } else if p.is_redundant() {
                        let mut q = p.one.take().or_else(|| p.two.take());
                        let ans = tune(&mut q, weight, removed, parent_horizontal);
                        p.one = q;
                        ans
                    } else {
                        let mut one = p.one.take();
                        let mut two = p.two.take();
                        let (ow, oa) = tune(&mut one, weight * p.bias, removed, parent_horizontal);
                        let (tw, ta) = tune(
                            &mut two,
                            weight * (1.0 - p.bias),
                            removed,
                            parent_horizontal,
                        );
                        p.one = one;
                        p.two = two;
                        if !oa && !ta {
                            (0.0, false)
                        } else {
                            let total = ow + tw;
                            if oa && ta && total > 0.0 {
                                p.bias = ow / total;
                            }
                            (if total > 0.0 { total } else { weight }, true)
                        }
                    }
                }
            }
        }
        let mut one = self.one.take();
        let mut two = self.two.take();
        let (_, oa) = tune(&mut one, self.bias, removed, self.horizontal);
        let (_, ta) = tune(&mut two, 1.0 - self.bias, removed, self.horizontal);
        self.one = one;
        self.two = two;
        oa || ta
    }

    pub fn effective_border(&self, windows: &WindowList) -> i32 {
        for wid in self.all_window_ids() {
            if let Some(g) = windows.group(wid) {
                if let Some(wid) = g.windows.first() {
                    if let Some(w) = windows.window(*wid) {
                        return w.effective_border();
                    }
                }
            }
        }
        0
    }

    pub fn minimum_width(&self, windows: &WindowList, cell_width: i32) -> i32 {
        if self.is_redundant() || !self.horizontal {
            return 1;
        }
        let bw = if windows
            .groups()
            .first()
            .and_then(|g| g.windows.first())
            .and_then(|w| windows.window(*w))
            .map(|w| w.effective_border())
            .unwrap_or(0)
            > 0
        {
            self.effective_border(windows)
        } else {
            0
        };
        let cell = cell_width.max(1);
        let one = match &self.one {
            Some(SplitChild::Pair(p)) => p.minimum_width(windows, cell_width),
            _ => cell,
        };
        let two = match &self.two {
            Some(SplitChild::Pair(p)) => p.minimum_width(windows, cell_width),
            _ => cell,
        };
        2 * bw + one + two
    }

    pub fn minimum_height(&self, windows: &WindowList, cell_height: i32) -> i32 {
        if self.is_redundant() || self.horizontal {
            return 1;
        }
        let bw = if self.effective_border(windows) > 0 {
            self.effective_border(windows)
        } else {
            0
        };
        let cell = cell_height.max(1);
        let one = match &self.one {
            Some(SplitChild::Pair(p)) => p.minimum_height(windows, cell_height),
            _ => cell,
        };
        let two = match &self.two {
            Some(SplitChild::Pair(p)) => p.minimum_height(windows, cell_height),
            _ => cell,
        };
        2 * bw + one + two
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_leaf_geometry(
        &self,
        gid: GroupId,
        left: i32,
        top: i32,
        width: i32,
        height: i32,
        windows: &mut WindowList,
        config: &LayoutConfig,
        border_mult: i32,
    ) {
        let groups = [gid];
        let xl = xlayout(
            config,
            windows,
            &groups,
            &Bias::None,
            Some(left),
            Some(width),
            0,
            border_mult,
        );
        let yl = ylayout(
            config,
            windows,
            &groups,
            &Bias::None,
            Some(top),
            Some(height),
            0,
            border_mult,
        );
        if let (Some(x), Some(y)) = (xl.first(), yl.first()) {
            let geom = super::base::window_geometry_from_layouts(
                x,
                y,
                config.cell_width,
                config.cell_height,
            );
            super::base::set_group_geometry(windows, gid, geom);
        }
    }

    pub fn layout_pair(
        &mut self,
        left: i32,
        top: i32,
        width: i32,
        height: i32,
        windows: &mut WindowList,
        config: &LayoutConfig,
    ) {
        self.left = left;
        self.top = top;
        self.width = width;
        self.height = height;
        let has_border = windows
            .groups()
            .first()
            .and_then(|g| g.windows.first())
            .and_then(|w| windows.window(*w))
            .map(|w| w.effective_border())
            .unwrap_or(0)
            > 0;
        self.border_width = if config.draw_minimal_borders && has_border {
            self.effective_border(windows)
        } else {
            0
        };
        let bw = self.border_width;
        let border_mult = if config.draw_minimal_borders { 0 } else { 1 };
        let bw2 = bw * 2;
        if self.is_redundant() {
            match self.one.clone().or_else(|| self.two.clone()) {
                Some(SplitChild::Pair(p)) => {
                    if let Some(SplitChild::Pair(p)) = &mut self.one {
                        p.layout_pair(left, top, width, height, windows, config);
                    } else if let Some(SplitChild::Pair(p)) = &mut self.two {
                        p.layout_pair(left, top, width, height, windows, config);
                    }
                    let _ = p;
                }
                Some(SplitChild::Leaf(gid)) => {
                    self.apply_leaf_geometry(
                        gid,
                        left,
                        top,
                        width,
                        height,
                        windows,
                        config,
                        border_mult,
                    );
                }
                None => {}
            }
            return;
        }
        if self.horizontal {
            let min_w1 = match &self.one {
                Some(SplitChild::Pair(p)) => p.minimum_width(windows, config.cell_width),
                _ => 1,
            };
            let min_w2 = match &self.two {
                Some(SplitChild::Pair(p)) => p.minimum_width(windows, config.cell_width),
                _ => 1,
            };
            let mut w1 = ((self.bias * width as f64) as i32 - bw).max(min_w1);
            let mut w2 = width - w1 - bw2;
            if w2 < min_w2 && width - min_w2 - bw2 >= min_w1 {
                w2 = min_w2;
                w1 = width - w2 - bw2;
            }
            self.first_extent = (left, top, left + w1, top + height);
            if let Some(SplitChild::Pair(p)) = &mut self.one {
                p.layout_pair(left, top, w1, height, windows, config);
            } else if let Some(SplitChild::Leaf(gid)) = self.one {
                self.apply_leaf_geometry(gid, left, top, w1, height, windows, config, border_mult);
            }
            let left2 = left + w1 + bw2;
            self.second_extent = (left2, top, left2 + w2, top + height);
            if let Some(SplitChild::Pair(p)) = &mut self.two {
                p.layout_pair(left2, top, w2, height, windows, config);
            } else if let Some(SplitChild::Leaf(gid)) = self.two {
                self.apply_leaf_geometry(gid, left2, top, w2, height, windows, config, border_mult);
            }
        } else {
            let min_h1 = match &self.one {
                Some(SplitChild::Pair(p)) => p.minimum_height(windows, config.cell_height),
                _ => 1,
            };
            let min_h2 = match &self.two {
                Some(SplitChild::Pair(p)) => p.minimum_height(windows, config.cell_height),
                _ => 1,
            };
            let mut h1 = ((self.bias * height as f64) as i32 - bw).max(min_h1);
            let mut h2 = height - h1 - bw2;
            if h2 < min_h2 && height - min_h2 - bw2 >= min_h1 {
                h2 = min_h2;
                h1 = height - h2 - bw2;
            }
            self.first_extent = (left, top, left + width, top + h1);
            if let Some(SplitChild::Pair(p)) = &mut self.one {
                p.layout_pair(left, top, width, h1, windows, config);
            } else if let Some(SplitChild::Leaf(gid)) = self.one {
                self.apply_leaf_geometry(gid, left, top, width, h1, windows, config, border_mult);
            }
            let top2 = top + h1 + bw2;
            self.second_extent = (left, top2, left + width, top2 + h2);
            if let Some(SplitChild::Pair(p)) = &mut self.two {
                p.layout_pair(left, top2, width, h2, windows, config);
            } else if let Some(SplitChild::Leaf(gid)) = self.two {
                self.apply_leaf_geometry(gid, left, top2, width, h2, windows, config, border_mult);
            }
        }
    }

    fn swap_windows(&mut self, a: GroupId, b: GroupId) {
        fn replace(pair: &mut Pair, a: GroupId, b: GroupId) {
            match &pair.one {
                Some(SplitChild::Leaf(id)) if *id == a => {
                    pair.one = Some(SplitChild::Leaf(b));
                }
                Some(SplitChild::Leaf(id)) if *id == b => {
                    pair.one = Some(SplitChild::Leaf(a));
                }
                _ => {}
            }
            match &pair.two {
                Some(SplitChild::Leaf(id)) if *id == a => {
                    pair.two = Some(SplitChild::Leaf(b));
                }
                Some(SplitChild::Leaf(id)) if *id == b => {
                    pair.two = Some(SplitChild::Leaf(a));
                }
                _ => {}
            }
            if let Some(SplitChild::Pair(p)) = &mut pair.one {
                replace(p, a, b);
            }
            if let Some(SplitChild::Pair(p)) = &mut pair.two {
                replace(p, a, b);
            }
        }
        replace(self, a, b);
    }

    fn edge_windows(&self, edge: Edge) -> Vec<GroupId> {
        let mut ans = Vec::new();
        if self.is_redundant() {
            if let Some(q) = self.one.as_ref().or(self.two.as_ref()) {
                match q {
                    SplitChild::Leaf(id) => ans.push(*id),
                    SplitChild::Pair(p) => ans.extend(p.edge_windows(edge)),
                }
            }
        }
        let edges = if self.horizontal {
            [Edge::Left, Edge::Right]
        } else {
            [Edge::Top, Edge::Bottom]
        };
        if edges.contains(&edge) {
            let q = if matches!(edge, Edge::Left | Edge::Top) {
                self.one.as_ref()
            } else {
                self.two.as_ref()
            };
            if let Some(q) = q {
                match q {
                    SplitChild::Leaf(id) => ans.push(*id),
                    SplitChild::Pair(p) => ans.extend(p.edge_windows(edge)),
                }
            }
        } else {
            for q in [self.one.as_ref(), self.two.as_ref()].into_iter().flatten() {
                match q {
                    SplitChild::Leaf(id) => ans.push(*id),
                    SplitChild::Pair(p) => ans.extend(p.edge_windows(edge)),
                }
            }
        }
        ans
    }

    fn neighbors_for_window(
        &self,
        window_id: GroupId,
        ans: &mut Neighbors,
        root: &Pair,
        windows: &WindowList,
    ) {
        fn geometry_for(windows: &WindowList, gid: GroupId) -> Option<WindowGeometry> {
            windows
                .group(gid)
                .and_then(|g| g.windows.last())
                .and_then(|w| windows.window(*w))
                .map(|w| w.geometry)
        }
        fn quadrant(horizontal: bool, is_first: bool) -> (Edge, Edge) {
            if horizontal {
                if is_first {
                    (Edge::Left, Edge::Right)
                } else {
                    (Edge::Right, Edge::Left)
                }
            } else if is_first {
                (Edge::Top, Edge::Bottom)
            } else {
                (Edge::Bottom, Edge::Top)
            }
        }
        fn extend(
            other: Option<&SplitChild>,
            edge: Edge,
            which: Edge,
            ans: &mut Neighbors,
            target_geom: Option<WindowGeometry>,
            windows: &WindowList,
        ) {
            if !ans.get(which).is_empty() {
                return;
            }
            let Some(other) = other else {
                return;
            };
            match other {
                SplitChild::Leaf(id) => ans.get_mut(which).push(*id),
                SplitChild::Pair(p) => {
                    for w in p.edge_windows(edge) {
                        let neighbouring = match (target_geom, geometry_for(windows, w)) {
                            (Some(a), Some(b)) => {
                                let (a1, a2, b1, b2) = if which.is_horizontal() {
                                    (a.top, a.bottom, b.top, b.bottom)
                                } else {
                                    (a.left, a.right, b.left, b.right)
                                };
                                a1 < b2 && a2 > b1
                            }
                            _ => false,
                        };
                        if neighbouring {
                            ans.get_mut(which).push(w);
                        }
                    }
                }
            }
        }
        fn visit(
            pair: &Pair,
            target: GroupId,
            ans: &mut Neighbors,
            target_geom: Option<WindowGeometry>,
            windows: &WindowList,
        ) -> bool {
            let direct_one = pair.one.as_ref().and_then(SplitChild::leaf_id) == Some(target);
            let direct_two = pair.two.as_ref().and_then(SplitChild::leaf_id) == Some(target);
            let one_has =
                direct_one || matches!(&pair.one, Some(SplitChild::Pair(p)) if p.contains(target));
            let two_has =
                direct_two || matches!(&pair.two, Some(SplitChild::Pair(p)) if p.contains(target));
            if !one_has && !two_has {
                return false;
            }
            let in_one = one_has;
            if !direct_one && !direct_two {
                if in_one {
                    if let Some(SplitChild::Pair(p)) = &pair.one {
                        if !visit(p, target, ans, target_geom, windows) {
                            return false;
                        }
                    }
                } else if let Some(SplitChild::Pair(p)) = &pair.two {
                    if !visit(p, target, ans, target_geom, windows) {
                        return false;
                    }
                }
            }
            let other = if in_one {
                pair.two.as_ref()
            } else {
                pair.one.as_ref()
            };
            let (edge, which) = quadrant(pair.horizontal, in_one);
            extend(other, edge, which, ans, target_geom, windows);
            true
        }
        let target_geom = geometry_for(windows, window_id);
        visit(root, window_id, ans, target_geom, windows);
    }
}

type MaximizedBiases = HashMap<(GroupId, bool), Vec<(Vec<bool>, f64)>>;

#[derive(Clone, Debug)]
pub struct SplitsLayoutOpts {
    pub default_axis_is_horizontal: Option<bool>,
    pub equalize_on_close: bool,
    pub proportional: bool,
}

impl Default for SplitsLayoutOpts {
    fn default() -> Self {
        Self {
            default_axis_is_horizontal: Some(true),
            equalize_on_close: false,
            proportional: false,
        }
    }
}

impl SplitsLayoutOpts {
    pub fn parse(raw: &str) -> Self {
        let mut ans = Self::default();
        for item in raw.split(';') {
            let (k, v) = item.split_once('=').unwrap_or((item, ""));
            if k.is_empty() || v.is_empty() {
                continue;
            }
            let truthy = matches!(v.to_lowercase().as_str(), "y" | "yes" | "true");
            match k {
                "split_axis" => {
                    ans.default_axis_is_horizontal = if v == "auto" {
                        None
                    } else {
                        Some(v == "horizontal")
                    }
                }
                "equalize_on_window_close" => ans.equalize_on_close = truthy,
                "proportional" => ans.proportional = truthy,
                _ => {}
            }
        }
        ans
    }

    pub fn serialized(&self) -> BTreeMap<String, Value> {
        let mut m = BTreeMap::new();
        m.insert(
            "split_axis".into(),
            Value::Str(
                match self.default_axis_is_horizontal {
                    None => "auto",
                    Some(true) => "horizontal",
                    Some(false) => "vertical",
                }
                .into(),
            ),
        );
        m.insert(
            "equalize_on_window_close".into(),
            Value::Str(if self.equalize_on_close { "y" } else { "n" }.into()),
        );
        m.insert(
            "proportional".into(),
            Value::Str(if self.proportional { "y" } else { "n" }.into()),
        );
        m
    }
}

#[derive(Clone, Debug)]
pub struct Splits {
    pub opts: SplitsLayoutOpts,
    pub root: Pair,
    config: LayoutConfig,
    maximized: MaximizedBiases,
}

impl Splits {
    pub fn new(opts: SplitsLayoutOpts, config: LayoutConfig) -> Self {
        Self {
            opts,
            root: Pair::new(true),
            config,
            maximized: HashMap::new(),
        }
    }

    fn default_horizontal(&self) -> bool {
        self.opts.default_axis_is_horizontal.unwrap_or(true)
    }

    pub fn pairs_root(&self) -> &Pair {
        &self.root
    }

    pub fn set_pairs_root(&mut self, root: Pair) {
        self.root = root;
    }

    fn window_tree_path(&self, id: GroupId) -> Option<Vec<(Vec<bool>, bool)>> {
        fn walk(
            pair: &Pair,
            id: GroupId,
            path: &mut Vec<bool>,
            out: &mut Vec<(Vec<bool>, bool)>,
        ) -> bool {
            let one_has = match &pair.one {
                Some(SplitChild::Leaf(x)) => *x == id,
                Some(SplitChild::Pair(p)) => p.contains(id),
                None => false,
            };
            let two_has = match &pair.two {
                Some(SplitChild::Leaf(x)) => *x == id,
                Some(SplitChild::Pair(p)) => p.contains(id),
                None => false,
            };
            if one_has {
                out.push((path.clone(), true));
                if let Some(SplitChild::Pair(p)) = &pair.one {
                    path.push(false);
                    let found = walk(p, id, path, out);
                    path.pop();
                    if found {
                        return true;
                    }
                }
                return true;
            }
            if two_has {
                out.push((path.clone(), false));
                if let Some(SplitChild::Pair(p)) = &pair.two {
                    path.push(true);
                    let found = walk(p, id, path, out);
                    path.pop();
                    if found {
                        return true;
                    }
                }
                return true;
            }
            false
        }
        let mut out = Vec::new();
        let mut path = Vec::new();
        if walk(&self.root, id, &mut path, &mut out) {
            Some(out)
        } else {
            None
        }
    }

    fn pair_at_path_mut<'a>(&'a mut self, path: &[bool]) -> Option<&'a mut Pair> {
        let mut current = &mut self.root;
        for step in path {
            let child = if *step {
                &mut current.two
            } else {
                &mut current.one
            };
            current = match child {
                Some(SplitChild::Pair(p)) => p,
                _ => return None,
            };
        }
        Some(current)
    }

    fn remove_ids(&mut self, ids: &[GroupId]) {
        if ids.is_empty() {
            return;
        }
        let set: HashSet<GroupId> = ids.iter().copied().collect();
        if self.opts.proportional && ids.len() > 1 {
            for id in ids {
                self.remove_ids(&[*id]);
            }
            return;
        }
        if self.opts.proportional {
            self.root.preserve_weights_on_removal(&set);
        }
        self.root.remove_windows_deep(&set);
        self.root.collapse_redundant_pairs();
        if self.root.is_redundant() {
            if let Some(SplitChild::Pair(p)) = self.root.one.clone() {
                self.root = *p;
            }
        }
    }

    fn equalize_biases(&mut self) -> bool {
        self.root.for_each_pair_mut(&mut |pair| {
            if pair.is_redundant() {
                pair.bias = 0.5;
                return;
            }
            let left = match &pair.one {
                Some(SplitChild::Pair(p)) => p.count_axis_units(pair.horizontal),
                Some(SplitChild::Leaf(_)) => 1,
                None => 0,
            };
            let right = match &pair.two {
                Some(SplitChild::Pair(p)) => p.count_axis_units(pair.horizontal),
                Some(SplitChild::Leaf(_)) => 1,
                None => 0,
            };
            let total = left + right;
            if total > 0 {
                pair.bias = left as f64 / total as f64;
            }
        });
        true
    }

    fn modify_bias_for_window(
        &mut self,
        target: GroupId,
        increment: f64,
        is_horizontal: bool,
    ) -> bool {
        fn go(
            pair: &mut Pair,
            target: GroupId,
            increment: f64,
            is_horizontal: bool,
        ) -> Option<bool> {
            let in_one = match &pair.one {
                Some(SplitChild::Leaf(x)) => *x == target,
                Some(SplitChild::Pair(p)) => p.contains(target),
                None => false,
            };
            let in_two = match &pair.two {
                Some(SplitChild::Leaf(x)) => *x == target,
                Some(SplitChild::Pair(p)) => p.contains(target),
                None => false,
            };
            if !in_one && !in_two {
                return None;
            }
            if in_one {
                if let Some(SplitChild::Pair(p)) = &mut pair.one {
                    if !p.is_redundant() && p.horizontal == is_horizontal {
                        return Some(go(p, target, increment, is_horizontal).unwrap_or(false));
                    }
                }
            } else if let Some(SplitChild::Pair(p)) = &mut pair.two {
                if !p.is_redundant() && p.horizontal == is_horizontal {
                    return Some(go(p, target, increment, is_horizontal).unwrap_or(false));
                }
            }
            if pair.horizontal == is_horizontal && !pair.is_redundant() {
                let which = if in_one { 1 } else { 2 };
                let delta = if which == 2 { -increment } else { increment };
                let new_bias = (pair.bias + delta).clamp(0.0, 1.0);
                if new_bias != pair.bias {
                    pair.bias = new_bias;
                    return Some(true);
                }
                return Some(false);
            }
            // Climb to the parent is handled by the caller recursion in kitty;
            // in this bottom-up formulation, signal "not handled" so a parent
            // can adjust its own bias.
            None
        }
        go(&mut self.root, target, increment, is_horizontal).unwrap_or(false)
    }

    fn action_maximize(&mut self, windows: &WindowList, args: &[String]) -> bool {
        let axis = args.first().map(String::as_str).unwrap_or("horizontal");
        let is_horizontal = axis == "horizontal";
        let Some(gid) = windows.active_group().map(|g| g.id) else {
            return false;
        };
        let key = (gid, is_horizontal);
        if let Some(saved) = self.maximized.remove(&key) {
            for (path, bias) in saved {
                if let Some(pair) = self.pair_at_path_mut(&path) {
                    pair.bias = bias;
                }
            }
            return true;
        }
        let stale: Vec<(GroupId, bool)> = self
            .maximized
            .keys()
            .filter(|(_, h)| *h == is_horizontal)
            .copied()
            .collect();
        for k in stale {
            if let Some(saved) = self.maximized.remove(&k) {
                for (path, bias) in saved {
                    if let Some(pair) = self.pair_at_path_mut(&path) {
                        pair.bias = bias;
                    }
                }
            }
        }
        let Some(path) = self.window_tree_path(gid) else {
            return false;
        };
        let mut saved = Vec::new();
        for (pair_path, in_one) in path {
            let pair = self.pair_at_path_mut(&pair_path);
            if let Some(pair) = pair {
                if pair.horizontal == is_horizontal && !pair.is_redundant() {
                    saved.push((pair_path, pair.bias));
                    pair.bias = if in_one { 1.0 } else { 0.0 };
                }
            }
        }
        if !saved.is_empty() {
            self.maximized.insert(key, saved);
        }
        true
    }
}

impl LayoutImpl for Splits {
    fn name(&self) -> &'static str {
        "splits"
    }

    fn config(&self) -> &LayoutConfig {
        &self.config
    }

    fn config_mut(&mut self) -> &mut LayoutConfig {
        &mut self.config
    }

    fn do_layout(&mut self, windows: &mut WindowList) {
        let groups = all_groups(windows);
        let present: HashSet<GroupId> = groups.iter().copied().collect();
        let already: HashSet<GroupId> = self.root.all_window_ids().into_iter().collect();
        let to_remove: Vec<GroupId> = already.difference(&present).copied().collect();
        if !to_remove.is_empty() {
            self.remove_ids(&to_remove);
        }
        let already: HashSet<GroupId> = self.root.all_window_ids().into_iter().collect();
        let mut to_add: Vec<GroupId> = groups
            .iter()
            .copied()
            .filter(|g| !already.contains(g))
            .collect();
        to_add.sort_by_key(|gid| groups.iter().position(|g| g == gid).unwrap_or(usize::MAX));
        for gid in to_add {
            if self.opts.proportional {
                let weights = self.root.window_weights();
                self.root.balanced_add(gid);
                if let Some(pair) = self.root.pair_for_window(gid) {
                    let sibling = if pair.one.as_ref().and_then(SplitChild::leaf_id) == Some(gid) {
                        pair.two.clone()
                    } else {
                        pair.one.clone()
                    };
                    let weight = match sibling {
                        Some(SplitChild::Leaf(id)) => weights.get(&id).copied().unwrap_or(0.0),
                        Some(SplitChild::Pair(p)) => p
                            .all_window_ids()
                            .iter()
                            .map(|id| weights.get(id).copied().unwrap_or(0.0))
                            .sum(),
                        None => 0.0,
                    };
                    if weight > 0.0 {
                        let mut w = weights.clone();
                        w.insert(gid, weight);
                        if let Some(p) = self.root.pair_for_window_mut(gid) {
                            let _ = p;
                        }
                        self.root.set_window_weights(&w);
                    }
                }
            } else {
                self.root.balanced_add(gid);
            }
        }
        if groups.len() == 1 {
            super::base::layout_single_window_group(&self.config, windows, groups[0], false);
        } else if groups.len() > 1 {
            let left = self.config.central.left;
            let top = self.config.central.top;
            let width = self.config.central.width;
            let height = self.config.central.height;
            self.root
                .layout_pair(left, top, width, height, windows, &self.config);
        }
    }

    fn neighbors_for_window(&self, window: WindowId, windows: &WindowList) -> Neighbors {
        let Some(gid) = windows.group_id_for_window(window) else {
            return Neighbors::default();
        };
        let mut ans = Neighbors::default();
        self.root
            .neighbors_for_window(gid, &mut ans, &self.root, windows);
        ans
    }

    fn add_non_overlay_window(
        &mut self,
        windows: &mut WindowList,
        window: Window,
        opts: &LayoutAddOptions,
    ) {
        let mut horizontal = self.default_horizontal();
        let mut after = true;
        match opts.location {
            WindowLocation::VSplit => horizontal = true,
            WindowLocation::HSplit => horizontal = false,
            WindowLocation::Before | WindowLocation::First => after = false,
            _ => {}
        }
        let aw = opts
            .next_to
            .or_else(|| windows.active_window_id())
            .and_then(|id| windows.group_id_for_window(id).map(|gid| (id, gid)));
        let bias = opts.bias.map(|b| b.abs().clamp(0.0, 100.0) / 100.0);
        if let Some((aw_id, agid)) = aw {
            if let Some(pair) = self.root.pair_for_window(agid) {
                if opts.location == WindowLocation::Split
                    || self.opts.default_axis_is_horizontal.is_none()
                {
                    if let Some(w) = windows.window(aw_id) {
                        let wwidth = w.geometry.right - w.geometry.left;
                        let wheight = w.geometry.bottom - w.geometry.top;
                        horizontal = wwidth >= wheight;
                    }
                }
                let id = window.id;
                windows.add_window(
                    window,
                    crate::model::AddWindowOptions {
                        next_to: Some(aw_id),
                        before: !after,
                        make_active: true,
                        ..Default::default()
                    },
                );
                let new_gid = windows.group_id_for_window(id).unwrap_or(agid);
                let _ = pair;
                if let Some(pair) = self.root.pair_for_window_mut(agid) {
                    pair.split_and_add(agid, new_gid, horizontal, after);
                }
                if let Some(bias) = bias {
                    if let Some(pair) = self.root.pair_for_window_mut(new_gid) {
                        pair.bias =
                            if pair.one.as_ref().and_then(SplitChild::leaf_id) == Some(new_gid) {
                                bias
                            } else {
                                1.0 - bias
                            };
                    }
                }
                return;
            }
        }
        let id = window.id;
        windows.add_window(
            window,
            crate::model::AddWindowOptions {
                make_active: true,
                ..Default::default()
            },
        );
        if let Some(gid) = windows.group_id_for_window(id) {
            if let Some(bias) = bias {
                self.root.balanced_add(gid);
                if let Some(pair) = self.root.pair_for_window_mut(gid) {
                    pair.bias = bias;
                }
            } else if self.opts.proportional {
                let weights = self.root.window_weights();
                self.root.balanced_add(gid);
                let sibling = self.root.pair_for_window(gid).and_then(|p| {
                    if p.one.as_ref().and_then(SplitChild::leaf_id) == Some(gid) {
                        p.two.clone()
                    } else {
                        p.one.clone()
                    }
                });
                let weight = match sibling {
                    Some(SplitChild::Leaf(id)) => weights.get(&id).copied().unwrap_or(0.0),
                    Some(SplitChild::Pair(p)) => p
                        .all_window_ids()
                        .iter()
                        .map(|id| weights.get(id).copied().unwrap_or(0.0))
                        .sum(),
                    None => 0.0,
                };
                if weight > 0.0 {
                    let mut w = weights;
                    w.insert(gid, weight);
                    self.root.set_window_weights(&w);
                }
            } else {
                self.root.balanced_add(gid);
            }
        }
    }

    fn remove_windows(&mut self, _windows: &mut WindowList, group_ids: &[GroupId]) {
        self.remove_ids(group_ids);
    }

    fn modify_size_of_window(
        &mut self,
        windows: &mut WindowList,
        window_id: WindowId,
        increment: f64,
        is_horizontal: bool,
    ) -> bool {
        let Some(gid) = windows.group_id_for_window(window_id) else {
            return false;
        };
        self.modify_bias_for_window(gid, increment, is_horizontal)
    }

    fn remove_all_biases(&mut self) -> bool {
        self.root.for_each_pair_mut(&mut |p| p.bias = 0.5);
        true
    }

    fn on_window_removed(&mut self, _windows: &mut WindowList, _removed: &[GroupId]) -> bool {
        if self.opts.equalize_on_close {
            self.equalize_biases()
        } else {
            false
        }
    }

    fn layout_action(
        &mut self,
        action_name: &str,
        args: &[String],
        windows: &mut WindowList,
    ) -> Option<bool> {
        match action_name {
            "rotate" => {
                let amt = args
                    .first()
                    .and_then(|x| x.parse::<i32>().ok())
                    .filter(|x| [90, 180, 270].contains(x))
                    .unwrap_or(90);
                let rotate = amt == 90 || amt == 270;
                let swap = amt == 180 || amt == 270;
                let gid = windows.active_group()?.id;
                let pair = self.root.pair_for_window_mut(gid)?;
                if !pair.is_redundant() {
                    if rotate {
                        pair.horizontal = !pair.horizontal;
                    }
                    if swap {
                        std::mem::swap(&mut pair.one, &mut pair.two);
                    }
                    Some(true)
                } else {
                    Some(false)
                }
            }
            "equalize" => Some(self.equalize_biases()),
            "bias" => {
                let bias = args
                    .first()
                    .and_then(|x| x.parse::<i32>().ok())
                    .unwrap_or(50)
                    .clamp(0, 100);
                let gid = windows.active_group()?.id;
                let pair = self.root.pair_for_window_mut(gid)?;
                let target = if pair.one.as_ref().and_then(SplitChild::leaf_id) == Some(gid) {
                    bias as f64 / 100.0
                } else {
                    1.0 - bias as f64 / 100.0
                };
                pair.bias = target;
                Some(true)
            }
            "maximize" => Some(self.action_maximize(windows, args)),
            _ => None,
        }
    }

    fn move_window(&mut self, windows: &mut WindowList, delta: i64) -> bool {
        let before = windows.active_group_id();
        let Some(before) = before else {
            return false;
        };
        let before_idx = windows.active_group_idx();
        if !windows.move_window_group(Some(delta), None) {
            return false;
        }
        let after = windows
            .groups()
            .get(before_idx.max(0) as usize)
            .map(|g| g.id);
        if let Some(after) = after {
            if after != before {
                self.root.swap_windows(before, after);
            }
        }
        true
    }

    fn insert_window_next_to(
        &mut self,
        windows: &mut WindowList,
        window: WindowId,
        next_to: WindowId,
        horizontal: bool,
        after: bool,
    ) {
        let Some(src_gid) = windows.group_id_for_window(window) else {
            return;
        };
        let Some(dest_gid) = windows.group_id_for_window(next_to) else {
            return;
        };
        if src_gid == dest_gid || !self.root.contains(src_gid) {
            return;
        }
        self.remove_ids(&[src_gid]);
        if let Some(pair) = self.root.pair_for_window_mut(dest_gid) {
            pair.split_and_add(dest_gid, src_gid, horizontal, after);
        } else {
            self.root.balanced_add(src_gid);
        }
    }

    fn splits_root(&self) -> Option<&Pair> {
        Some(&self.root)
    }

    fn splits_root_mut(&mut self) -> Option<&mut Pair> {
        Some(&mut self.root)
    }

    fn layout_state(&self) -> Value {
        let mut m = Value::map();
        m.insert("pairs".into(), self.root.serialize());
        if !self.maximized.is_empty() {
            let list: Vec<Value> = self
                .maximized
                .iter()
                .map(|((gid, horizontal), saved)| {
                    let mut e = Value::map();
                    e.insert("window_id".into(), Value::Int(*gid as i64));
                    e.insert("is_horizontal".into(), Value::Bool(*horizontal));
                    e.insert(
                        "saved_biases".into(),
                        Value::List(
                            saved
                                .iter()
                                .map(|(path, bias)| {
                                    let mut s = Value::map();
                                    s.insert(
                                        "path".into(),
                                        Value::List(
                                            path.iter()
                                                .map(|x| {
                                                    Value::Str(
                                                        if *x { "two" } else { "one" }.into(),
                                                    )
                                                })
                                                .collect(),
                                        ),
                                    );
                                    s.insert("bias".into(), Value::Float(*bias));
                                    Value::Map(s)
                                })
                                .collect(),
                        ),
                    );
                    Value::Map(e)
                })
                .collect();
            m.insert("maximized".into(), Value::List(list));
        }
        Value::Map(m)
    }

    fn set_layout_state(
        &mut self,
        state: &Value,
        group_id_map: &HashMap<GroupId, GroupId>,
    ) -> bool {
        let Some(pairs) = state.get("pairs") else {
            return false;
        };
        let Some(new_root) = Pair::from_value(pairs, group_id_map) else {
            return false;
        };
        let before: HashSet<GroupId> = self.root.all_window_ids().into_iter().collect();
        let new_ids: HashSet<GroupId> = new_root.all_window_ids().into_iter().collect();
        if !before.is_empty() && before != new_ids {
            return false;
        }
        self.root = new_root;
        if let Some(opts) = state.get("opts") {
            let mut data = Vec::new();
            if let Some(m) = opts.as_map() {
                for key in ["split_axis", "equalize_on_window_close", "proportional"] {
                    if let Some(v) = m.get(key) {
                        if let Some(s) = v.as_str() {
                            data.push(format!("{key}={s}"));
                        }
                    }
                }
            }
            self.opts = SplitsLayoutOpts::parse(&data.join(";"));
        }
        if let Some(entries) = state.get("maximized").and_then(Value::as_list) {
            self.maximized.clear();
            for entry in entries {
                let Some(old_gid) = entry.get_i64("window_id").map(|x| x as GroupId) else {
                    continue;
                };
                let Some(new_gid) = group_id_map.get(&old_gid).copied() else {
                    continue;
                };
                let horizontal = entry
                    .get("is_horizontal")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let mut saved = Vec::new();
                if let Some(biases) = entry.get("saved_biases").and_then(Value::as_list) {
                    for b in biases {
                        let path: Vec<bool> = b
                            .get("path")
                            .and_then(Value::as_list)
                            .map(|items| items.iter().map(|x| x.as_str() == Some("two")).collect())
                            .unwrap_or_default();
                        if let Some(bias) = b.get_f64("bias") {
                            saved.push((path, bias));
                        }
                    }
                }
                if !saved.is_empty() {
                    self.maximized.insert((new_gid, horizontal), saved);
                }
            }
        }
        true
    }

    fn layout_opts_serialized(&self) -> BTreeMap<String, Value> {
        self.opts.serialized()
    }

    // Splits uses the default drag implementation, which goes through
    // modify_size_of_window() above.
}

trait ActiveGroupId {
    fn active_group_id(&self) -> Option<GroupId>;
}

impl ActiveGroupId for WindowList {
    fn active_group_id(&self) -> Option<GroupId> {
        self.active_group().map(|g| g.id)
    }
}
