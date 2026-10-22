// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of ``kitty/layout/stack.py``.

use super::base::{layout_single_window_group, LayoutConfig, LayoutImpl, Neighbors};
use crate::model::{WindowId, WindowList};

#[derive(Clone, Debug)]
pub struct Stack {
    config: LayoutConfig,
}

impl Stack {
    pub fn new(config: LayoutConfig) -> Self {
        Self { config }
    }
}

impl LayoutImpl for Stack {
    fn name(&self) -> &'static str {
        "stack"
    }

    fn config(&self) -> &LayoutConfig {
        &self.config
    }

    fn config_mut(&mut self) -> &mut LayoutConfig {
        &mut self.config
    }

    fn only_active_window_visible(&self) -> bool {
        true
    }

    fn do_layout(&mut self, windows: &mut WindowList) {
        let groups: Vec<_> = windows.groups().iter().map(|g| g.id).collect();
        for gid in groups {
            layout_single_window_group(&self.config, windows, gid, false);
        }
    }

    fn neighbors_for_window(&self, window: WindowId, windows: &WindowList) -> Neighbors {
        let Some(gid) = windows.group_id_for_window(window) else {
            return Neighbors::default();
        };
        let groups: Vec<_> = windows.groups().iter().map(|g| g.id).collect();
        let Some(idx) = groups.iter().position(|g| *g == gid) else {
            return Neighbors::default();
        };
        let before = if idx == 0 {
            vec![]
        } else {
            vec![groups[idx - 1]]
        };
        let after = if idx + 1 == groups.len() {
            vec![]
        } else {
            vec![groups[idx + 1]]
        };
        Neighbors {
            top: before.clone(),
            left: before,
            right: after.clone(),
            bottom: after,
        }
    }
}
