// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of ``kitty/layout/grid.py``.

use std::collections::BTreeMap;

use super::base::{
    all_groups, layout_dimension, Bias, LayoutConfig, LayoutData, LayoutImpl, Neighbors,
};
use crate::model::{Edge, GroupId, WindowId, WindowList};
use crate::value::Value;

pub fn calc_grid_size(n: usize) -> (usize, usize, usize, usize) {
    let ncols = if n <= 5 {
        if n == 1 {
            1
        } else {
            2
        }
    } else {
        let mut found = n / 2;
        for candidate in 3..=(n / 2) {
            if candidate * candidate >= n {
                found = candidate;
                break;
            }
        }
        found
    };
    let nrows = n / ncols;
    let special_rows = n - nrows * (ncols - 1);
    let special_col = if special_rows < nrows { 0 } else { ncols - 1 };
    (ncols, nrows, special_rows, special_col)
}

#[derive(Clone, Debug)]
pub struct Grid {
    biased_rows: BTreeMap<usize, f64>,
    biased_cols: BTreeMap<usize, f64>,
    config: LayoutConfig,
}

impl Grid {
    pub fn new(config: LayoutConfig) -> Self {
        Self {
            biased_rows: BTreeMap::new(),
            biased_cols: BTreeMap::new(),
            config,
        }
    }

    fn column_layout(&self, num: usize, bias: &Bias) -> Vec<LayoutData> {
        let decoration_pairs = vec![(0, 0); num];
        layout_dimension(
            self.config.central.left,
            self.config.central.width,
            self.config.cell_width,
            &decoration_pairs,
            self.config.alignment_x,
            bias,
        )
    }

    fn row_layout(&self, num: usize, bias: &Bias) -> Vec<LayoutData> {
        let decoration_pairs = vec![(0, 0); num];
        layout_dimension(
            self.config.central.top,
            self.config.central.height,
            self.config.cell_height,
            &decoration_pairs,
            self.config.alignment_y,
            bias,
        )
    }

    fn variable_layout(
        &self,
        layout_func: fn(&Self, usize, &Bias) -> Vec<LayoutData>,
        num_windows: usize,
        biased_map: &BTreeMap<usize, f64>,
    ) -> Vec<LayoutData> {
        let bias = if num_windows > 1 {
            Bias::Indexed(biased_map.clone())
        } else {
            Bias::None
        };
        layout_func(self, num_windows, &bias)
    }

    fn position_for_window_idx(
        &self,
        idx: usize,
        num_windows: usize,
        ncols: usize,
        nrows: usize,
        special_rows: usize,
        special_col: usize,
    ) -> (usize, usize) {
        let mut pos = 0;
        for col in 0..ncols {
            let rows = if col == special_col {
                special_rows
            } else {
                nrows
            };
            for row in 0..rows {
                if pos + row == idx {
                    return (row, col);
                }
            }
            pos += rows;
            let _ = num_windows;
        }
        (0, 0)
    }

    fn layout_windows(
        &self,
        num_windows: usize,
        nrows: usize,
        ncols: usize,
        special_rows: usize,
        special_col: usize,
    ) -> Vec<(usize, LayoutData, LayoutData)> {
        let xvals = self.variable_layout(Self::column_layout, ncols, &self.biased_cols);
        let yvals_normal = self.variable_layout(Self::row_layout, nrows, &self.biased_rows);
        let yvals_special = if special_rows == nrows {
            yvals_normal.clone()
        } else {
            self.variable_layout(Self::row_layout, special_rows, &self.biased_rows)
        };
        let mut ans = Vec::with_capacity(num_windows);
        let mut pos = 0;
        for col in 0..ncols {
            let rows = if col == special_col {
                special_rows
            } else {
                nrows
            };
            let yls = if col == special_col {
                &yvals_special
            } else {
                &yvals_normal
            };
            let xl = xvals.get(col).copied().unwrap_or_default();
            for (i, yl) in yls.iter().take(rows).enumerate() {
                ans.push((pos + i, xl, *yl));
            }
            pos += rows;
        }
        ans
    }
}

impl LayoutImpl for Grid {
    fn name(&self) -> &'static str {
        "grid"
    }

    fn config(&self) -> &LayoutConfig {
        &self.config
    }

    fn config_mut(&mut self) -> &mut LayoutConfig {
        &mut self.config
    }

    fn do_layout(&mut self, windows: &mut WindowList) {
        let n = windows.num_groups();
        if n == 0 {
            return;
        }
        if n == 1 {
            super::base::layout_single_window_group(
                &self.config,
                windows,
                windows.groups()[0].id,
                false,
            );
            return;
        }
        let (ncols, nrows, special_rows, special_col) = calc_grid_size(n);
        let groups = all_groups(windows);

        fn extents(ld: &LayoutData) -> (i32, i32) {
            (
                ld.content_pos - ld.space_before,
                ld.space_before + ld.space_after + ld.content_size,
            )
        }

        fn make_layout(
            ld: &LayoutData,
            cell_length: i32,
            before_dec: i32,
            after_dec: i32,
            alignment: i32,
        ) -> LayoutData {
            let (start, size) = extents(ld);
            let content_size = size - (before_dec + after_dec);
            let number_of_cells = (content_size / cell_length).max(0) as usize;
            let cell_area = number_of_cells as i32 * cell_length;
            let extra = content_size - cell_area;
            let compensatory_before = if alignment == 0 {
                extra / 2
            } else if alignment > 0 {
                extra
            } else {
                0
            };
            let space_before = before_dec + compensatory_before;
            let space_after = size - cell_area - space_before;
            LayoutData {
                content_pos: start + space_before,
                cells_per_window: number_of_cells,
                space_before,
                space_after,
                content_size: cell_area,
                compensatory_before,
                compensatory_after: space_after - after_dec,
            }
        }

        for (window_idx, xl, yl) in self.layout_windows(n, nrows, ncols, special_rows, special_col)
        {
            let Some(gid) = groups.get(window_idx).copied() else {
                continue;
            };
            let (left_dec, top_dec, right_dec, bottom_dec) = windows
                .group(gid)
                .and_then(|g| g.windows.first())
                .and_then(|wid| windows.window(*wid))
                .map(|w| {
                    (
                        w.effective_margin(Edge::Left)
                            + w.effective_border()
                            + w.effective_padding(Edge::Left),
                        w.effective_margin(Edge::Top)
                            + w.effective_border()
                            + w.effective_padding(Edge::Top),
                        w.effective_margin(Edge::Right)
                            + w.effective_border()
                            + w.effective_padding(Edge::Right),
                        w.effective_margin(Edge::Bottom)
                            + w.effective_border()
                            + w.effective_padding(Edge::Bottom),
                    )
                })
                .unwrap_or((0, 0, 0, 0));
            let xl = make_layout(
                &xl,
                self.config.cell_width,
                left_dec,
                right_dec,
                self.config.alignment_x,
            );
            let yl = make_layout(
                &yl,
                self.config.cell_height,
                top_dec,
                bottom_dec,
                self.config.alignment_y,
            );
            let geom = super::base::window_geometry_from_layouts(
                &xl,
                &yl,
                self.config.cell_width,
                self.config.cell_height,
            );
            super::base::set_group_geometry(windows, gid, geom);
        }
    }

    fn apply_bias(
        &mut self,
        idx: usize,
        increment: f64,
        windows: &mut WindowList,
        is_horizontal: bool,
    ) -> bool {
        let n = windows.num_groups();
        let (ncols, nrows, special_rows, special_col) = calc_grid_size(n);
        let (row_num, col_num) =
            self.position_for_window_idx(idx, n, ncols, nrows, special_rows, special_col);
        if is_horizontal {
            if ncols < 2 {
                return false;
            }
            let before = self.column_layout(ncols, &Bias::Indexed(self.biased_cols.clone()));
            let before_val = self.biased_cols.get(&col_num).copied().unwrap_or(0.0);
            let mut candidate = self.biased_cols.clone();
            candidate.insert(col_num, before_val + increment);
            let after = self.column_layout(ncols, &Bias::Indexed(candidate.clone()));
            if before == after {
                false
            } else {
                self.biased_cols = candidate;
                true
            }
        } else {
            let num = nrows.max(special_rows);
            if num < 2 {
                return false;
            }
            let before = self.row_layout(num, &Bias::Indexed(self.biased_rows.clone()));
            let before_val = self.biased_rows.get(&row_num).copied().unwrap_or(0.0);
            let mut candidate = self.biased_rows.clone();
            candidate.insert(row_num, before_val + increment);
            let after = self.row_layout(num, &Bias::Indexed(candidate.clone()));
            if before == after {
                false
            } else {
                self.biased_rows = candidate;
                true
            }
        }
    }

    fn remove_all_biases(&mut self) -> bool {
        self.biased_rows.clear();
        self.biased_cols.clear();
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
        let n = windows.num_groups();
        let (ncols, nrows, special_rows, special_col) = calc_grid_size(n);
        let (row_num, col_num) =
            self.position_for_window_idx(idx, n, ncols, nrows, special_rows, special_col);
        if row_num == 0 {
            let before = self.column_layout(ncols, &Bias::Indexed(self.biased_cols.clone()));
            self.biased_cols.insert(col_num, cell_increment_bias_h);
            before == self.column_layout(ncols, &Bias::Indexed(self.biased_cols.clone()))
        } else {
            let num = nrows.max(special_rows);
            let before = self.row_layout(num, &Bias::Indexed(self.biased_rows.clone()));
            self.biased_rows.insert(row_num, cell_increment_bias_v);
            before == self.row_layout(num, &Bias::Indexed(self.biased_rows.clone()))
        }
    }

    fn neighbors_for_window(&self, window: WindowId, windows: &WindowList) -> Neighbors {
        let n = windows.num_groups();
        if n < 4 {
            return super::tall::neighbors_for_tall_window(1, window, windows, false, true);
        }
        let Some(gid) = windows.group_id_for_window(window) else {
            return Neighbors::default();
        };
        let (ncols, nrows, special_rows, special_col) = calc_grid_size(n);
        let max_rows = nrows.max(special_rows);
        let mut matrix: Vec<Vec<Option<GroupId>>> = vec![vec![None; ncols]; max_rows];
        let mut pos_map = BTreeMap::new();
        let col_counts: Vec<usize> = (0..ncols)
            .map(|col| {
                if col == special_col {
                    special_rows
                } else {
                    nrows
                }
            })
            .collect();
        let mut g_iter = windows.groups().iter();
        for (col, rows) in col_counts.iter().copied().enumerate() {
            for (row, matrix_row) in matrix.iter_mut().enumerate().take(rows) {
                if let Some(g) = g_iter.next() {
                    matrix_row[col] = Some(g.id);
                    pos_map.insert(g.id, (row, col));
                }
            }
        }
        let Some(&(row, col)) = pos_map.get(&gid) else {
            return Neighbors::default();
        };

        let neighbours = |row: usize, col: usize| -> Vec<GroupId> {
            matrix
                .get(row)
                .and_then(|r| r.get(col))
                .and_then(|x| *x)
                .into_iter()
                .collect()
        };
        let side = |row: usize, col: usize, delta: isize| -> Vec<GroupId> {
            let neighbor_col = col as isize + delta;
            if neighbor_col < 0 || neighbor_col as usize >= ncols {
                return vec![];
            }
            let neighbor_col = neighbor_col as usize;
            let neighbor_nrows = col_counts[neighbor_col];
            let nrows = col_counts[col];
            if neighbor_nrows == nrows {
                return neighbours(row, neighbor_col);
            }
            let start_row = ((neighbor_nrows * row) as f64 / nrows as f64).floor() as usize;
            let end_row = ((neighbor_nrows * (row + 1)) as f64 / nrows as f64).ceil() as usize;
            let mut xs = Vec::new();
            for neighbor_row in start_row..end_row {
                xs.extend(neighbours(neighbor_row, neighbor_col));
            }
            xs
        };

        let mut ans = Neighbors::default();
        if row > 0 {
            ans.top = neighbours(row - 1, col);
        }
        let bottom = neighbours(row + 1, col);
        if !bottom.is_empty() {
            ans.bottom = bottom;
        }
        if col > 0 {
            let left = side(row, col, -1);
            if !left.is_empty() {
                ans.left = left;
            }
        }
        if col + 1 < ncols {
            ans.right = side(row, col, 1);
        }
        ans
    }

    fn layout_state(&self) -> Value {
        let mut m = Value::map();
        m.insert(
            "biased_cols".into(),
            Value::Map(
                self.biased_cols
                    .iter()
                    .map(|(k, v)| (k.to_string(), Value::Float(*v)))
                    .collect(),
            ),
        );
        m.insert(
            "biased_rows".into(),
            Value::Map(
                self.biased_rows
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
        let Some(rows) = parse_bias_map(state, "biased_rows") else {
            return false;
        };
        let Some(cols) = parse_bias_map(state, "biased_cols") else {
            return false;
        };
        self.biased_rows = rows;
        self.biased_cols = cols;
        true
    }
}

fn parse_bias_map(state: &Value, key: &str) -> Option<BTreeMap<usize, f64>> {
    let m = state.get(key)?.as_map()?;
    let mut ans = BTreeMap::new();
    for (k, v) in m {
        ans.insert(k.parse::<usize>().ok()?, v.as_f64()?);
    }
    Some(ans)
}
