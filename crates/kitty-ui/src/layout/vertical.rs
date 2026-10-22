// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of ``kitty/layout/vertical.py`` (the ``vertical`` and ``horizontal``
//! layouts).

use std::collections::BTreeMap;

use super::base::{
    all_groups, xlayout, ylayout, Bias, LayoutConfig, LayoutData, LayoutImpl, Neighbors,
};
use crate::model::{WindowId, WindowList};
use crate::value::Value;

#[derive(Clone, Debug)]
pub struct Vertical {
    horizontal_main: bool,
    biased_map: BTreeMap<usize, f64>,
    config: LayoutConfig,
}

impl Vertical {
    pub fn new(horizontal_main: bool, config: LayoutConfig) -> Self {
        Self {
            horizontal_main,
            biased_map: BTreeMap::new(),
            config,
        }
    }

    fn variable_layout(&self, windows: &WindowList) -> Vec<LayoutData> {
        let groups = all_groups(windows);
        let bias = if groups.len() > 1 {
            Bias::Indexed(self.biased_map.clone())
        } else {
            Bias::None
        };
        if self.horizontal_main {
            xlayout(&self.config, windows, &groups, &bias, None, None, 0, 1)
        } else {
            ylayout(&self.config, windows, &groups, &bias, None, None, 0, 1)
        }
    }

    fn fixed_layout(&self, windows: &WindowList, gid: crate::model::GroupId) -> Vec<LayoutData> {
        let groups = [gid];
        let border_mult = if self.config.draw_minimal_borders {
            0
        } else {
            1
        };
        if self.horizontal_main {
            ylayout(
                &self.config,
                windows,
                &groups,
                &Bias::None,
                None,
                None,
                0,
                border_mult,
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
                border_mult,
            )
        }
    }

    pub fn biased_map(&self) -> &BTreeMap<usize, f64> {
        &self.biased_map
    }
}

impl LayoutImpl for Vertical {
    fn name(&self) -> &'static str {
        if self.horizontal_main {
            "horizontal"
        } else {
            "vertical"
        }
    }

    fn config(&self) -> &LayoutConfig {
        &self.config
    }

    fn config_mut(&mut self) -> &mut LayoutConfig {
        &mut self.config
    }

    fn do_layout(&mut self, windows: &mut WindowList) {
        let groups = all_groups(windows);
        if groups.len() == 1 {
            super::base::layout_single_window_group(&self.config, windows, groups[0], false);
            return;
        }
        let variable = self.variable_layout(windows);
        for (gid, yl) in groups.iter().zip(variable.iter()) {
            let mut xl = self.fixed_layout(windows, *gid)[0];
            let mut yl = *yl;
            if self.horizontal_main {
                std::mem::swap(&mut xl, &mut yl);
            }
            let geom = super::base::window_geometry_from_layouts(
                &xl,
                &yl,
                self.config.cell_width,
                self.config.cell_height,
            );
            super::base::set_group_geometry(windows, *gid, geom);
        }
    }

    fn neighbors_for_window(&self, window: WindowId, windows: &WindowList) -> Neighbors {
        let Some(gid) = windows.group_id_for_window(window) else {
            return Neighbors::default();
        };
        let groups = all_groups(windows);
        let Some(idx) = groups.iter().position(|g| *g == gid) else {
            return Neighbors::default();
        };
        let lg = groups.len();
        let (after, before) = if lg > 1 {
            (
                vec![groups[(idx + lg - 1) % lg]],
                vec![groups[(idx + 1) % lg]],
            )
        } else {
            (vec![], vec![])
        };
        let (a_key, b_key) = if self.horizontal_main {
            (crate::model::Edge::Left, crate::model::Edge::Right)
        } else {
            (crate::model::Edge::Top, crate::model::Edge::Bottom)
        };
        let mut ans = Neighbors::default();
        if !after.is_empty() {
            ans.set(a_key, after);
        }
        if !before.is_empty() {
            ans.set(b_key, before);
        }
        ans
    }

    fn apply_bias(
        &mut self,
        idx: usize,
        increment: f64,
        windows: &mut WindowList,
        is_horizontal: bool,
    ) -> bool {
        if self.horizontal_main != is_horizontal || windows.num_groups() < 2 {
            return false;
        }
        let before_layout = self.variable_layout(windows);
        let mut candidate = self.biased_map.clone();
        let before = candidate.get(&idx).copied().unwrap_or(0.0);
        candidate.insert(idx, before + increment);
        let old = std::mem::replace(&mut self.biased_map, candidate);
        if before_layout == self.variable_layout(windows) {
            self.biased_map = old;
            false
        } else {
            true
        }
    }

    fn remove_all_biases(&mut self) -> bool {
        self.biased_map.clear();
        true
    }

    fn bias_slot(
        &mut self,
        windows: &mut WindowList,
        idx: usize,
        _fractional_bias: f64,
        cell_increment_bias_h: f64,
        cell_increment_bias_v: f64,
    ) -> bool {
        let before = self.variable_layout(windows);
        self.biased_map.insert(
            idx,
            if self.horizontal_main {
                cell_increment_bias_h
            } else {
                cell_increment_bias_v
            },
        );
        before == self.variable_layout(windows)
    }

    fn layout_state(&self) -> Value {
        let mut m = Value::map();
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
        _group_id_map: &std::collections::HashMap<crate::model::GroupId, crate::model::GroupId>,
    ) -> bool {
        let Some(m) = state.get("biased_map") else {
            return false;
        };
        let Some(m) = m.as_map() else {
            return false;
        };
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
        true
    }
}
