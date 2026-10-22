// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of ``kitty/layout/tall.py`` (the ``tall`` and ``fat`` layouts).

use std::collections::BTreeMap;

use super::base::{
    all_groups, normalize_biases, safe_increment_bias, xlayout, ylayout, Bias, LayoutConfig,
    LayoutData, LayoutImpl, Neighbors,
};
use crate::model::{Edge, GroupId, WindowId, WindowList};
use crate::value::Value;

fn to_bool(raw: &str) -> bool {
    matches!(raw.to_lowercase().as_str(), "y" | "yes" | "true")
}

#[derive(Clone, Debug)]
pub struct TallLayoutOpts {
    pub full_size: usize,
    pub bias: i32,
    pub mirrored: bool,
}

impl Default for TallLayoutOpts {
    fn default() -> Self {
        Self {
            full_size: 1,
            bias: 50,
            mirrored: false,
        }
    }
}

impl TallLayoutOpts {
    pub fn parse(raw: &str) -> Self {
        let mut ans = Self::default();
        for item in raw.split(';') {
            let (k, v) = item.split_once('=').unwrap_or((item, ""));
            if k.is_empty() || v.is_empty() {
                continue;
            }
            match k {
                "full_size" => {
                    ans.full_size = v.parse::<usize>().unwrap_or(1).clamp(1, 100);
                }
                "bias" => ans.bias = v.parse::<i32>().unwrap_or(50),
                "mirrored" => ans.mirrored = to_bool(v),
                _ => {}
            }
        }
        ans
    }

    pub fn serialized(&self) -> BTreeMap<String, Value> {
        let mut m = BTreeMap::new();
        m.insert("full_size".into(), Value::Int(self.full_size as i64));
        m.insert("bias".into(), Value::Int(self.bias as i64));
        m.insert(
            "mirrored".into(),
            Value::Str(if self.mirrored { "y" } else { "n" }.into()),
        );
        m
    }

    pub fn build_bias_list(&self) -> Vec<f64> {
        let b = (self.bias as f64 / 100.0).clamp(0.1, 0.9);
        let mut ans = vec![b / self.full_size as f64; self.full_size];
        ans.push(1.0 - b);
        ans
    }

    pub fn as_string(&self) -> String {
        format!(
            "full_size={};bias={};mirrored={}",
            self.full_size,
            self.bias,
            if self.mirrored { "y" } else { "n" }
        )
    }
}

fn set_bias(biases: &[f64], idx: usize, target: f64) -> Vec<f64> {
    let remainder = 1.0 - target;
    let previous_remainder: f64 = biases
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != idx)
        .map(|(_, x)| *x)
        .sum();
    let mut ans = vec![1.0; biases.len()];
    for i in 0..biases.len() {
        ans[i] = if i == idx {
            target
        } else if previous_remainder == 0.0 {
            0.0
        } else {
            remainder * biases[i] / previous_remainder
        };
    }
    ans
}

/// Neighbour traversal shared by tall and grid, ported verbatim from
/// ``neighbors_for_tall_window``.
pub(crate) fn neighbors_for_tall_window(
    num_full_size_windows: usize,
    window: WindowId,
    all_windows: &WindowList,
    mirrored: bool,
    main_is_horizontal: bool,
) -> Neighbors {
    let Some(gid) = all_windows.group_id_for_window(window) else {
        return Neighbors::default();
    };
    let groups = all_groups(all_windows);
    let Some(idx) = groups.iter().position(|g| *g == gid) else {
        return Neighbors::default();
    };
    let prev = if idx == 0 {
        None
    } else {
        Some(groups[idx - 1])
    };
    let nxt = if idx + 1 == groups.len() {
        None
    } else {
        Some(groups[idx + 1])
    };
    let mut main_before = if main_is_horizontal {
        Edge::Left
    } else {
        Edge::Top
    };
    let mut main_after = if main_is_horizontal {
        Edge::Right
    } else {
        Edge::Bottom
    };
    let cross_before = if main_is_horizontal {
        Edge::Top
    } else {
        Edge::Left
    };
    let cross_after = if main_is_horizontal {
        Edge::Bottom
    } else {
        Edge::Right
    };
    if mirrored {
        std::mem::swap(&mut main_before, &mut main_after);
    }
    let mut ans = Neighbors::default();
    if let Some(p) = prev {
        ans.set(main_before, vec![p]);
    }
    if idx + 1 < num_full_size_windows {
        if let Some(n) = nxt {
            ans.set(main_after, vec![n]);
        }
    } else if idx + 1 == num_full_size_windows {
        ans.set(main_after, groups[idx + 1..].to_vec());
    } else {
        if num_full_size_windows > 0 {
            ans.set(main_before, vec![groups[num_full_size_windows - 1]]);
        }
        if idx + 1 > num_full_size_windows {
            if let Some(p) = prev {
                ans.set(cross_before, vec![p]);
            }
        }
        if let Some(n) = nxt {
            ans.set(cross_after, vec![n]);
        }
    }
    ans
}

#[derive(Clone, Debug)]
pub struct Tall {
    /// True for ``tall``, false for ``fat``.
    main_is_horizontal: bool,
    pub opts: TallLayoutOpts,
    int_bias: f64,
    main_bias: Vec<f64>,
    biased_map: BTreeMap<usize, f64>,
    config: LayoutConfig,
}

impl Tall {
    pub fn new(main_is_horizontal: bool, opts: TallLayoutOpts, config: LayoutConfig) -> Self {
        let mut ans = Self {
            main_is_horizontal,
            int_bias: 50.0,
            main_bias: Vec::new(),
            biased_map: BTreeMap::new(),
            opts,
            config,
        };
        ans.remove_all_biases();
        ans
    }

    pub fn num_full_size_windows(&self) -> usize {
        self.opts.full_size
    }

    pub fn main_bias(&self) -> &[f64] {
        &self.main_bias
    }

    fn variable_layout(&self, windows: &WindowList) -> Vec<LayoutData> {
        let groups = all_groups(windows);
        let num = groups.len().saturating_sub(self.num_full_size_windows());
        let bias = if num > 1 {
            Bias::Indexed(self.biased_map.clone())
        } else {
            Bias::None
        };
        if self.main_is_horizontal {
            ylayout(
                &self.config,
                windows,
                &groups,
                &bias,
                None,
                None,
                self.num_full_size_windows(),
                1,
            )
        } else {
            xlayout(
                &self.config,
                windows,
                &groups,
                &bias,
                None,
                None,
                self.num_full_size_windows(),
                1,
            )
        }
    }

    fn main_layout(
        &self,
        windows: &WindowList,
        groups: &[GroupId],
        bias: &Bias,
        start: Option<i32>,
        size: Option<i32>,
    ) -> Vec<LayoutData> {
        if self.main_is_horizontal {
            xlayout(&self.config, windows, groups, bias, start, size, 0, 1)
        } else {
            ylayout(&self.config, windows, groups, bias, start, size, 0, 1)
        }
    }

    fn perp_layout(&self, windows: &WindowList, gid: GroupId) -> LayoutData {
        let groups = [gid];
        let val = if self.main_is_horizontal {
            ylayout(
                &self.config,
                windows,
                &groups,
                &Bias::None,
                None,
                None,
                0,
                1,
            )
        } else {
            xlayout(
                &self.config,
                windows,
                &groups,
                &Bias::None,
                None,
                None,
                0,
                1,
            )
        };
        val[0]
    }

    fn simple_layout(&self, windows: &WindowList) -> Vec<(GroupId, LayoutData, LayoutData, bool)> {
        let num = windows.num_groups();
        let is_fat = !self.main_is_horizontal;
        let mirrored = self.opts.mirrored;
        let mut groups = all_groups(windows);
        let mut main_bias = self.main_bias.clone();
        if mirrored {
            main_bias.reverse();
            groups.reverse();
        }
        main_bias.truncate(num);
        let main_bias = normalize_biases(&main_bias);
        let xlayout = self.main_layout(windows, &groups, &Bias::Seq(main_bias), None, None);
        let mut ans = Vec::new();
        for (gid, xl) in groups.iter().zip(xlayout.iter()) {
            let yl = self.perp_layout(windows, *gid);
            let (mut xl, mut yl) = (*xl, yl);
            if is_fat {
                std::mem::swap(&mut xl, &mut yl);
            }
            ans.push((*gid, xl, yl, true));
        }
        ans
    }

    fn full_layout(&self, windows: &WindowList) -> Vec<(GroupId, LayoutData, LayoutData, bool)> {
        let is_fat = !self.main_is_horizontal;
        let mirrored = self.opts.mirrored;
        let groups = all_groups(windows);
        let mut main_bias = self.main_bias.clone();
        if mirrored {
            main_bias.reverse();
        }
        let mut ans = Vec::new();

        let mut start = if is_fat {
            self.config.central.top
        } else {
            self.config.central.left
        };
        let size;
        if mirrored {
            let fsg: Vec<GroupId> = groups[..(self.num_full_size_windows() + 1).min(groups.len())]
                .iter()
                .rev()
                .copied()
                .collect();
            let xlayout =
                self.main_layout(windows, &fsg, &Bias::Seq(main_bias.clone()), None, None);
            let mut first = None;
            for (i, (gid, xl)) in fsg.iter().zip(xlayout.iter()).enumerate() {
                if i == 0 {
                    first = Some(xl.content_size + xl.space_before + xl.space_after);
                    continue;
                }
                let yl = self.perp_layout(windows, *gid);
                let (mut xl, mut yl) = (*xl, yl);
                if is_fat {
                    std::mem::swap(&mut xl, &mut yl);
                }
                ans.push((*gid, xl, yl, true));
            }
            size = first.unwrap_or(0);
            let _ = start;
        } else {
            let fsg: Vec<GroupId> = groups
                .iter()
                .take(self.num_full_size_windows() + 1)
                .copied()
                .collect();
            let xlayout =
                self.main_layout(windows, &fsg, &Bias::Seq(main_bias.clone()), None, None);
            for (i, (gid, xl)) in groups
                .iter()
                .zip(xlayout.iter())
                .take(self.num_full_size_windows())
                .enumerate()
            {
                let _ = i;
                let yl = self.perp_layout(windows, *gid);
                start = xl.content_pos + xl.content_size + xl.space_after;
                let (mut xl, mut yl) = (*xl, yl);
                if is_fat {
                    std::mem::swap(&mut xl, &mut yl);
                }
                ans.push((*gid, xl, yl, true));
            }
            let end = if is_fat {
                self.config.central.bottom()
            } else {
                self.config.central.right()
            };
            size = 1 + end - start;
        }

        let ylayout = self.variable_layout(windows);
        for (i, gid) in groups.iter().enumerate() {
            if i < self.num_full_size_windows() {
                continue;
            }
            let Some(yl) = ylayout.get(i - self.num_full_size_windows()).copied() else {
                continue;
            };
            let xl = self
                .main_layout(windows, &[*gid], &Bias::None, Some(start), Some(size))
                .into_iter()
                .next()
                .unwrap_or_default();
            let (mut xl, mut yl) = (xl, yl);
            if is_fat {
                std::mem::swap(&mut xl, &mut yl);
            }
            ans.push((*gid, xl, yl, false));
        }
        ans
    }
}

impl LayoutImpl for Tall {
    fn name(&self) -> &'static str {
        if self.main_is_horizontal {
            "tall"
        } else {
            "fat"
        }
    }

    fn config(&self) -> &LayoutConfig {
        &self.config
    }

    fn config_mut(&mut self) -> &mut LayoutConfig {
        &mut self.config
    }

    fn do_layout(&mut self, windows: &mut WindowList) {
        let num = windows.num_groups();
        if num == 0 {
            return;
        }
        if num == 1 {
            super::base::layout_single_window_group(
                &self.config,
                windows,
                windows.groups()[0].id,
                false,
            );
            return;
        }
        let layouts = if num <= self.num_full_size_windows() + 1 {
            self.simple_layout(windows)
        } else {
            self.full_layout(windows)
        };
        for (gid, xl, yl, _is_full) in layouts {
            let geom = super::base::window_geometry_from_layouts(
                &xl,
                &yl,
                self.config.cell_width,
                self.config.cell_height,
            );
            super::base::set_group_geometry(windows, gid, geom);
        }
    }

    fn neighbors_for_window(&self, window: WindowId, windows: &WindowList) -> Neighbors {
        neighbors_for_tall_window(
            self.num_full_size_windows(),
            window,
            windows,
            self.opts.mirrored,
            self.main_is_horizontal,
        )
    }

    fn remove_all_biases(&mut self) -> bool {
        self.main_bias = self.opts.build_bias_list();
        self.biased_map.clear();
        true
    }

    fn apply_bias(
        &mut self,
        idx: usize,
        increment: f64,
        windows: &mut WindowList,
        is_horizontal: bool,
    ) -> bool {
        if self.main_is_horizontal == is_horizontal {
            let before = self.main_bias.clone();
            let ncols = self.num_full_size_windows() + 1;
            let biased_col = if idx < self.num_full_size_windows() {
                idx
            } else {
                ncols - 1
            };
            self.main_bias = (0..ncols)
                .map(|i| {
                    safe_increment_bias(
                        *self.main_bias.get(i).unwrap_or(&0.5),
                        if i == biased_col {
                            increment
                        } else {
                            -increment
                        },
                    )
                })
                .collect();
            self.main_bias = normalize_biases(&self.main_bias);
            self.main_bias != before
        } else {
            let num_windows = windows.num_groups();
            let num_short = num_windows.saturating_sub(self.num_full_size_windows());
            if idx < self.num_full_size_windows() || num_short < 2 {
                return false;
            }
            let widx = idx - self.num_full_size_windows();
            let before_layout = self.variable_layout(windows);
            let before = self.biased_map.get(&widx).copied().unwrap_or(0.0);
            let mut candidate = self.biased_map.clone();
            candidate.insert(widx, before + increment);
            let old = std::mem::replace(&mut self.biased_map, candidate.clone());
            if before_layout == self.variable_layout(windows) {
                self.biased_map = old;
                false
            } else {
                true
            }
        }
    }

    fn bias_slot(
        &mut self,
        windows: &mut WindowList,
        idx: usize,
        fractional_bias: f64,
        cell_increment_bias_h: f64,
        cell_increment_bias_v: f64,
    ) -> bool {
        if idx < self.main_bias.len() {
            let before = self.main_bias.clone();
            self.main_bias = set_bias(&self.main_bias, idx, fractional_bias);
            return self.main_bias != before;
        }
        let before = self.variable_layout(windows);
        self.biased_map.insert(
            idx - self.num_full_size_windows(),
            if self.main_is_horizontal {
                cell_increment_bias_v
            } else {
                cell_increment_bias_h
            },
        );
        before == self.variable_layout(windows)
    }

    fn layout_action(
        &mut self,
        action_name: &str,
        args: &[String],
        _windows: &mut WindowList,
    ) -> Option<bool> {
        match action_name {
            "increase_num_full_size_windows" => {
                self.opts.full_size += 1;
                self.main_bias = self.opts.build_bias_list();
                Some(true)
            }
            "decrease_num_full_size_windows" => {
                if self.opts.full_size > 1 {
                    self.opts.full_size -= 1;
                    self.main_bias = self.opts.build_bias_list();
                    Some(true)
                } else {
                    Some(false)
                }
            }
            "mirror" => {
                let action = args.first().map(String::as_str).unwrap_or("toggle");
                if action == "toggle" {
                    self.opts.mirrored = !self.opts.mirrored;
                    Some(true)
                } else {
                    let new_val = to_bool(action);
                    if new_val != self.opts.mirrored {
                        self.opts.mirrored = new_val;
                        Some(true)
                    } else {
                        Some(false)
                    }
                }
            }
            "bias" => {
                if args.is_empty() {
                    return Some(false);
                }
                let biases: Vec<&str> = args[0].split_whitespace().collect();
                let biases = if biases.len() == 1 {
                    vec![biases[0], "50"]
                } else {
                    biases
                };
                let i = biases
                    .iter()
                    .position(|x| x.parse::<i32>().ok() == Some(self.opts.bias))
                    .map(|x| x + 1)
                    .unwrap_or(0);
                match biases[i % biases.len()].parse::<i32>() {
                    Ok(v) => {
                        self.opts.bias = v;
                        self.int_bias = v as f64;
                        self.remove_all_biases();
                        Some(true)
                    }
                    Err(_) => Some(false),
                }
            }
            _ => None,
        }
    }

    fn layout_state(&self) -> Value {
        let mut m = Value::map();
        m.insert(
            "main_bias".into(),
            Value::List(self.main_bias.iter().map(|x| Value::Float(*x)).collect()),
        );
        m.insert(
            "biased_map".into(),
            Value::Map(
                self.biased_map
                    .iter()
                    .map(|(k, v)| (k.to_string(), Value::Float(*v)))
                    .collect(),
            ),
        );
        Value::Map(m)
    }

    fn set_layout_state(
        &mut self,
        state: &Value,
        _group_id_map: &std::collections::HashMap<GroupId, GroupId>,
    ) -> bool {
        if let Some(list) = state.get("main_bias").and_then(Value::as_list) {
            let mut vals = Vec::new();
            for v in list {
                match v.as_f64() {
                    Some(f) => vals.push(f),
                    None => return false,
                }
            }
            self.main_bias = vals;
        }
        if let Some(m) = state.get("biased_map").and_then(Value::as_map) {
            let mut ans = BTreeMap::new();
            for (k, v) in m {
                let Ok(idx) = k.parse::<usize>() else {
                    return false;
                };
                let Some(val) = v.as_f64() else {
                    return false;
                };
                ans.insert(idx, val);
            }
            self.biased_map = ans;
        }
        if let Some(opts) = state.get("opts") {
            let mut data = Vec::new();
            if let Some(m) = opts.as_map() {
                for key in ["full_size", "bias", "mirrored"] {
                    if let Some(v) = m.get(key) {
                        let val = match v {
                            Value::Int(i) => i.to_string(),
                            Value::Str(s) => s.clone(),
                            Value::Bool(b) => (if *b { "y" } else { "n" }).to_string(),
                            _ => return false,
                        };
                        data.push(format!("{key}={val}"));
                    }
                }
            }
            self.opts = TallLayoutOpts::parse(&data.join(";"));
        }
        true
    }

    fn layout_opts_serialized(&self) -> BTreeMap<String, Value> {
        self.opts.serialized()
    }
}
