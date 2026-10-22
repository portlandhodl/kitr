// SPDX-License-Identifier: GPL-3.0-or-later
use std::collections::HashMap;

use kitty_ui::layout::{calc_grid_size, layout_dimension, Pair, WindowLocation};
use kitty_ui::*;

fn config() -> LayoutConfig {
    LayoutConfig::default()
        .with_central(Region::new(0, 0, 1000, 600))
        .with_cell(10, 20)
}

fn windows(n: u64) -> WindowList {
    let mut wl = WindowList::new();
    for i in 1..=n {
        wl.add_window(Window::new(i, format!("w{i}")), AddWindowOptions::active());
    }
    wl
}

fn manager(name: &str, enabled: &[&str]) -> LayoutManager {
    let enabled: Vec<String> = if enabled.is_empty() {
        vec![name.into()]
    } else {
        enabled.iter().map(|x| x.to_string()).collect()
    };
    let mut lm = LayoutManager::new(1, 1, &enabled, config()).unwrap();
    lm.set_layout(name).unwrap();
    lm
}

fn geom(wl: &WindowList, id: u64) -> WindowGeometry {
    wl.window(id).unwrap().geometry
}

#[test]
fn layout_dimension_basic() {
    let d = layout_dimension(0, 100, 10, &[(0, 0)], 0, &Bias::None);
    assert_eq!(d[0].cells_per_window, 10);
    assert_eq!(d[0].content_pos, 0);
    assert_eq!(d[0].content_size, 100);
    let d = layout_dimension(0, 100, 10, &[(0, 0), (0, 0)], 0, &Bias::None);
    assert_eq!(d[0].cells_per_window, 5);
    assert_eq!(d[1].content_pos, 50);
    assert_eq!(d[1].content_size, 50);
}

#[test]
fn layout_dimension_never_negative() {
    for (length, cell, decs) in [
        (8, 8, vec![(5, 5)]),
        (6, 8, vec![(4, 4)]),
        (0, 8, vec![(4, 4)]),
        (4, 8, vec![(3, 3)]),
    ] {
        let d = layout_dimension(0, length, cell, &decs, 0, &Bias::None);
        assert!(d[0].cells_per_window <= 20);
        assert!(d[0].content_size >= 0);
        assert!(d[0].content_pos >= 0);
        assert!(d[0].content_pos + d[0].content_size <= length);
    }
}

#[test]
fn splits_vsplit_geometry() {
    let mut wl = WindowList::new();
    let mut lm = manager("splits", &[]);
    lm.add_window(&mut wl, Window::new(1, "a"), &LayoutAddOptions::default());
    lm.add_window(
        &mut wl,
        Window::new(2, "b"),
        &LayoutAddOptions {
            location: WindowLocation::VSplit,
            ..Default::default()
        },
    );
    let a = geom(&wl, 1);
    let b = geom(&wl, 2);
    assert_eq!((a.left, a.right, a.top, a.bottom), (0, 500, 0, 600));
    assert_eq!((b.left, b.right), (500, 1000));
}

#[test]
fn splits_hsplit_geometry() {
    let mut wl = WindowList::new();
    let mut lm = manager("splits", &[]);
    lm.add_window(&mut wl, Window::new(1, "a"), &LayoutAddOptions::default());
    lm.add_window(
        &mut wl,
        Window::new(2, "b"),
        &LayoutAddOptions {
            location: WindowLocation::HSplit,
            ..Default::default()
        },
    );
    let a = geom(&wl, 1);
    let b = geom(&wl, 2);
    assert_eq!((a.top, a.bottom), (0, 300));
    assert_eq!((b.top, b.bottom), (300, 600));
}

#[test]
fn splits_neighbors_match_kitty_geometry_rules() {
    let mut wl = windows(4);
    let mut layout = create_layout("splits", config()).unwrap();
    let value =
        Value::from_json(r#"{"one":1,"two":{"horizontal":false,"one":2,"two":{"one":3,"two":4}}}"#)
            .unwrap();
    let map: HashMap<GroupId, GroupId> = (1..=4).map(|i| (i, i)).collect();
    *layout.splits_root_mut().unwrap() = Pair::from_value(&value, &map).unwrap();
    layout.do_layout(&mut wl);
    let g = [
        WindowGeometry {
            left: 0,
            top: 0,
            right: 10,
            bottom: 20,
            ..Default::default()
        },
        WindowGeometry {
            left: 11,
            top: 0,
            right: 20,
            bottom: 10,
            ..Default::default()
        },
        WindowGeometry {
            left: 11,
            top: 11,
            right: 15,
            bottom: 20,
            ..Default::default()
        },
        WindowGeometry {
            left: 16,
            top: 11,
            right: 20,
            bottom: 20,
            ..Default::default()
        },
    ];
    for (i, geometry) in g.iter().enumerate() {
        wl.window_mut(i as u64 + 1).unwrap().set_geometry(*geometry);
    }
    let n0 = layout.neighbors_for_window(1, &wl);
    assert_eq!(n0.right, vec![2, 3]);
    let n1 = layout.neighbors_for_window(2, &wl);
    assert_eq!(n1.left, vec![1]);
    assert_eq!(n1.bottom, vec![3, 4]);
    let n2 = layout.neighbors_for_window(3, &wl);
    assert_eq!(n2.left, vec![1]);
    assert_eq!(n2.right, vec![4]);
    assert_eq!(n2.top, vec![2]);
    let n3 = layout.neighbors_for_window(4, &wl);
    assert_eq!(n3.left, vec![3]);
    assert_eq!(n3.top, vec![2]);
}

#[test]
fn stack_neighbors_and_visibility() {
    let mut wl = WindowList::new();
    let mut lm = manager("stack", &[]);
    for i in 1..=3 {
        lm.add_window(
            &mut wl,
            Window::new(i, format!("w{i}")),
            &LayoutAddOptions::default(),
        );
    }
    let n = lm.neighbors_for_window(2, &wl);
    assert_eq!(n.top, vec![1]);
    assert_eq!(n.left, vec![1]);
    assert_eq!(n.right, vec![3]);
    assert_eq!(n.bottom, vec![3]);
    assert!(wl.window(3).unwrap().is_visible_in_layout);
    assert!(!wl.window(1).unwrap().is_visible_in_layout);
    assert!(!wl.window(2).unwrap().is_visible_in_layout);
}

#[test]
fn vertical_neighbors_wrap() {
    let mut wl = WindowList::new();
    let mut lm = manager("vertical", &[]);
    for i in 1..=3 {
        lm.add_window(
            &mut wl,
            Window::new(i, format!("w{i}")),
            &LayoutAddOptions::default(),
        );
    }
    let n = lm.neighbors_for_window(2, &wl);
    assert_eq!(n.top, vec![1]);
    assert_eq!(n.bottom, vec![3]);
    let n = lm.neighbors_for_window(1, &wl);
    assert_eq!(n.top, vec![3]);
    assert_eq!(n.bottom, vec![2]);
}

#[test]
fn horizontal_neighbors_wrap() {
    let mut wl = WindowList::new();
    let mut lm = manager("horizontal", &[]);
    for i in 1..=3 {
        lm.add_window(
            &mut wl,
            Window::new(i, format!("w{i}")),
            &LayoutAddOptions::default(),
        );
    }
    let n = lm.neighbors_for_window(2, &wl);
    assert_eq!(n.left, vec![1]);
    assert_eq!(n.right, vec![3]);
}

#[test]
fn tall_neighbors_full_size_and_stack_column() {
    let mut wl = WindowList::new();
    let mut lm = manager("tall", &[]);
    for i in 1..=3 {
        lm.add_window(
            &mut wl,
            Window::new(i, format!("w{i}")),
            &LayoutAddOptions::default(),
        );
    }
    let n = lm.neighbors_for_window(1, &wl);
    assert_eq!(n.right, vec![2, 3]);
    let n = lm.neighbors_for_window(2, &wl);
    assert_eq!(n.left, vec![1]);
    assert_eq!(n.bottom, vec![3]);
    let n = lm.neighbors_for_window(3, &wl);
    assert_eq!(n.left, vec![1]);
    assert_eq!(n.top, vec![2]);
}

#[test]
fn tall_geometry_full_size_and_short_windows() {
    let mut wl = WindowList::new();
    let mut lm = manager("tall", &[]);
    for i in 1..=3 {
        lm.add_window(
            &mut wl,
            Window::new(i, format!("w{i}")),
            &LayoutAddOptions::default(),
        );
    }
    let a = geom(&wl, 1);
    let b = geom(&wl, 2);
    let c = geom(&wl, 3);
    assert_eq!((a.left, a.right, a.top, a.bottom), (0, 500, 0, 600));
    assert_eq!((b.left, b.right, b.top, b.bottom), (500, 1000, 0, 300));
    assert_eq!((c.left, c.right, c.top, c.bottom), (500, 1000, 300, 600));
}

#[test]
fn fat_layout_swaps_axes() {
    let mut wl = WindowList::new();
    let mut lm = manager("fat", &[]);
    for i in 1..=3 {
        lm.add_window(
            &mut wl,
            Window::new(i, format!("w{i}")),
            &LayoutAddOptions::default(),
        );
    }
    let a = geom(&wl, 1);
    assert_eq!((a.left, a.right, a.top, a.bottom), (0, 1000, 0, 300));
    let b = geom(&wl, 2);
    assert_eq!((b.left, b.right, b.top, b.bottom), (0, 500, 300, 600));
}

#[test]
fn grid_sizes_match_kitty() {
    let expected = [
        (1, 1, 1, 0),
        (2, 1, 1, 1),
        (2, 1, 2, 1),
        (2, 2, 2, 1),
        (2, 2, 3, 1),
        (3, 2, 2, 2),
        (3, 2, 3, 2),
        (3, 2, 4, 2),
        (3, 3, 3, 2),
        (4, 2, 4, 3),
    ];
    for (i, exp) in expected.iter().enumerate() {
        assert_eq!(calc_grid_size(i + 1), *exp, "n={}", i + 1);
    }
}

#[test]
fn grid_geometry_and_neighbors() {
    let mut wl = WindowList::new();
    let mut lm = manager("grid", &[]);
    for i in 1..=4 {
        lm.add_window(
            &mut wl,
            Window::new(i, format!("w{i}")),
            &LayoutAddOptions::default(),
        );
    }
    let a = geom(&wl, 1);
    assert_eq!((a.left, a.right, a.top, a.bottom), (0, 500, 0, 300));
    let d = geom(&wl, 4);
    assert_eq!((d.left, d.right, d.top, d.bottom), (500, 1000, 300, 600));
    let n1 = lm.neighbors_for_window(1, &wl);
    assert_eq!(n1.right, vec![3]);
    assert_eq!(n1.bottom, vec![2]);
}

#[test]
fn splits_equalize_rebalances_tree() {
    let mut wl = WindowList::new();
    let mut lm = manager("splits", &[]);
    lm.add_window(&mut wl, Window::new(1, "w1"), &LayoutAddOptions::default());
    lm.add_window(
        &mut wl,
        Window::new(2, "w2"),
        &LayoutAddOptions {
            location: WindowLocation::VSplit,
            ..Default::default()
        },
    );
    lm.add_window(
        &mut wl,
        Window::new(3, "w3"),
        &LayoutAddOptions {
            location: WindowLocation::VSplit,
            ..Default::default()
        },
    );
    lm.current_mut().splits_root_mut().unwrap().bias = 0.8;
    let ans = lm.layout_action("equalize", &[], &mut wl);
    assert_eq!(ans, Some(true));
    let root = lm.current().splits_root().unwrap();
    assert!((root.bias - 1.0 / 3.0).abs() < 1e-6);
}

#[test]
fn splits_maximize_toggles() {
    let mut wl = WindowList::new();
    let mut layout = create_layout("splits", config()).unwrap();
    for i in 1..=3 {
        wl.add_window(Window::new(i, format!("w{i}")), AddWindowOptions::active());
    }
    layout.do_layout(&mut wl);
    // root H(1, V(2,3)) from balanced_add
    wl.set_active_group_idx(2, true);
    let before = layout.splits_root().unwrap().bias;
    assert_eq!(
        layout.layout_action("maximize", &["horizontal".into()], &mut wl),
        Some(true)
    );
    assert_eq!(layout.splits_root().unwrap().bias, 0.0);
    assert_eq!(
        layout.layout_action("maximize", &["horizontal".into()], &mut wl),
        Some(true)
    );
    assert_eq!(layout.splits_root().unwrap().bias, before);
}

#[test]
fn splits_resize_changes_bias_and_geometry() {
    let mut wl = WindowList::new();
    let mut lm = manager("splits", &[]);
    lm.add_window(&mut wl, Window::new(1, "a"), &LayoutAddOptions::default());
    lm.add_window(
        &mut wl,
        Window::new(2, "b"),
        &LayoutAddOptions {
            location: WindowLocation::VSplit,
            ..Default::default()
        },
    );
    assert!((lm.current().splits_root().unwrap().bias - 0.5).abs() < 1e-9);
    assert!(lm.resize_window(&mut wl, 1, 0.1, true));
    assert!((lm.current().splits_root().unwrap().bias - 0.6).abs() < 1e-9);
    assert_eq!(geom(&wl, 1).right, 600);
}

#[test]
fn vertical_resize_changes_geometry() {
    let mut wl = WindowList::new();
    let mut lm = manager("vertical", &[]);
    for i in 1..=2 {
        lm.add_window(
            &mut wl,
            Window::new(i, format!("w{i}")),
            &LayoutAddOptions::default(),
        );
    }
    let before = geom(&wl, 1).bottom;
    assert!(lm.resize_window(&mut wl, 1, 0.1, false));
    assert!(geom(&wl, 1).bottom > before);
}

#[test]
fn layout_opts_roundtrip() {
    let opts = SplitsLayoutOpts {
        default_axis_is_horizontal: None,
        equalize_on_close: true,
        proportional: true,
    };
    let s = opts.serialized();
    let raw: Vec<String> = ["split_axis", "equalize_on_window_close", "proportional"]
        .iter()
        .filter_map(|k| {
            s.get(*k)
                .and_then(Value::as_str)
                .map(|v| format!("{k}={v}"))
        })
        .collect();
    let back = SplitsLayoutOpts::parse(&raw.join(";"));
    assert_eq!(back.default_axis_is_horizontal, None);
    assert!(back.equalize_on_close);
    assert!(back.proportional);
}

#[test]
fn split_tree_serializes_and_restores() {
    let mut wl = windows(4);
    let mut layout = create_layout("splits", config()).unwrap();
    let value =
        Value::from_json(r#"{"one":1,"two":{"horizontal":false,"one":2,"two":{"one":3,"two":4}}}"#)
            .unwrap();
    let map: HashMap<GroupId, GroupId> = (1..=4).map(|i| (i, i)).collect();
    *layout.splits_root_mut().unwrap() = Pair::from_value(&value, &map).unwrap();
    layout.do_layout(&mut wl);
    let tree = layout.splits_root().unwrap().serialize();
    let state = layout.serialize(&wl);
    let mut fresh = create_layout("splits", config()).unwrap();
    assert!(fresh.unserialize(&state, &mut wl, true));
    assert_eq!(fresh.splits_root().unwrap().serialize(), tree);
}

#[test]
fn layout_unserialize_rejects_malformed_state() {
    let mut wl = windows(3);
    let layout = create_layout("tall", config()).unwrap();
    let state = layout.serialize(&wl);
    let mut bad = state.clone();
    if let Value::Map(ref mut m) = bad {
        m.remove("all_windows");
    }
    let mut fresh = create_layout("tall", config()).unwrap();
    assert!(!fresh.unserialize(&bad, &mut wl, true));
    let mut wrong_class = state.clone();
    if let Value::Map(ref mut m) = wrong_class {
        m.insert("class".into(), Value::Str("Nope".into()));
    }
    assert!(!fresh.unserialize(&wrong_class, &mut wl, true));
    let mut splits = create_layout("splits", config()).unwrap();
    assert!(!splits.unserialize(&state, &mut wl, true));
}

#[test]
fn layout_manager_cycles_enabled_layouts() {
    let mut lm = manager("tall", &["tall", "grid", "stack"]);
    assert_eq!(lm.current_name(), "tall");
    assert_eq!(lm.next_layout(1), "grid");
    assert_eq!(lm.next_layout(1), "stack");
    assert_eq!(lm.next_layout(1), "tall");
    assert_eq!(lm.next_layout(-1), "stack");
}

#[test]
fn set_enabled_layouts_switches_when_current_missing() {
    let mut lm = manager("tall", &["tall", "grid"]);
    lm.set_enabled_layouts(&["vertical".into(), "splits".into()])
        .unwrap();
    assert_eq!(
        lm.enabled_layouts(),
        &["vertical".to_string(), "splits".into()]
    );
    assert_eq!(lm.current_name(), "vertical");
}

#[test]
fn splits_remove_window_collapses_tree() {
    let mut wl = WindowList::new();
    let mut lm = manager("splits", &[]);
    for i in 1..=3 {
        lm.add_window(
            &mut wl,
            Window::new(i, format!("w{i}")),
            &LayoutAddOptions::default(),
        );
    }
    assert_eq!(
        lm.current().splits_root().unwrap().all_window_ids().len(),
        3
    );
    assert!(lm.remove_window(&mut wl, 2));
    assert_eq!(
        lm.current().splits_root().unwrap().all_window_ids().len(),
        2
    );
    assert_eq!(wl.num_groups(), 2);
    // Remaining windows still get a full layout.
    assert!(geom(&wl, 1).right - geom(&wl, 1).left > 0);
}

#[test]
fn splits_move_window_swaps_tree_leaves() {
    let mut wl = WindowList::new();
    let mut lm = manager("splits", &[]);
    for i in 1..=3 {
        lm.add_window(
            &mut wl,
            Window::new(i, format!("w{i}")),
            &LayoutAddOptions::default(),
        );
    }
    wl.set_active_group_idx(0, true);
    let before = lm.current().splits_root().unwrap().all_window_ids();
    assert!(lm.move_window(&mut wl, 2));
    let after = lm.current().splits_root().unwrap().all_window_ids();
    assert_ne!(before, after);
    assert_eq!(
        lm.current().splits_root().unwrap().all_window_ids().len(),
        3
    );
}

#[test]
fn window_list_layout_state_reorders_groups() {
    let mut wl = windows(3);
    wl.set_active_group_idx(2, true);
    let state = wl.serialize_layout_state();
    let mut fresh = WindowList::new();
    for serialized in [3u64, 2, 1] {
        let mut w = Window::new(10 + serialized, format!("s{serialized}"));
        w.serialized_id = serialized;
        fresh.add_window(w, AddWindowOptions::active());
    }
    fresh.set_active_group_idx(0, true);
    let map = fresh.window_id_map_for_layout();
    assert!(fresh.unserialize_layout_state(&state, &map, true).is_some());
    let ids: Vec<_> = fresh.groups().iter().map(|g| g.windows[0]).collect();
    assert_eq!(ids, vec![11, 12, 13]);
    assert_eq!(fresh.windows().get(&13).unwrap().serialized_id, 3);
}

#[test]
fn proportional_weights_set_root_and_nested_biases() {
    let mut root = Pair::new(true);
    let mut inner = Pair::new(false);
    inner.one = Some(SplitChild::Leaf(2));
    inner.two = Some(SplitChild::Leaf(3));
    root.one = Some(SplitChild::Leaf(1));
    root.two = Some(SplitChild::Pair(Box::new(inner)));
    let weights: std::collections::BTreeMap<GroupId, f64> =
        [(1u64, 0.5), (2, 0.1), (3, 0.4)].into_iter().collect();
    root.set_window_weights(&weights);
    assert!((root.bias - 0.5).abs() < 1e-12);
    let inner = root.pair_for_window(2).unwrap();
    assert!((inner.bias - 0.2).abs() < 1e-12);
}

#[test]
fn vertical_resize_that_does_not_change_layout_leaves_state_untouched() {
    let mut wl = WindowList::new();
    let mut lm = manager("vertical", &[]);
    for i in 1..=2 {
        lm.add_window(
            &mut wl,
            Window::new(i, format!("w{i}")),
            &LayoutAddOptions::default(),
        );
    }
    let before = lm.current().layout_state().to_json();
    assert!(!lm.resize_window(&mut wl, 1, 0.0, false));
    assert_eq!(lm.current().layout_state().to_json(), before);
}

#[test]
fn closing_overlay_keeps_split_group() {
    let mut wl = WindowList::new();
    let mut lm = manager("splits", &[]);
    lm.add_window(&mut wl, Window::new(1, "a"), &LayoutAddOptions::default());
    lm.add_window(
        &mut wl,
        Window::new(2, "b"),
        &LayoutAddOptions {
            location: WindowLocation::VSplit,
            ..Default::default()
        },
    );
    lm.add_window(
        &mut wl,
        Window::new(3, "overlay"),
        &LayoutAddOptions {
            overlay_for: Some(1),
            ..Default::default()
        },
    );
    assert_eq!(wl.num_groups(), 2);
    assert_eq!(
        lm.current().splits_root().unwrap().all_window_ids().len(),
        2
    );
    assert!(lm.remove_window(&mut wl, 3));
    assert_eq!(wl.num_groups(), 2);
    assert_eq!(
        lm.current().splits_root().unwrap().all_window_ids().len(),
        2
    );
    assert_eq!(wl.active_window_id(), Some(1));
}

#[test]
fn tall_state_roundtrips_biases() {
    let mut wl = windows(3);
    let layout = create_layout("tall", config()).unwrap();
    let mut state = layout.serialize(&wl);
    if let Value::Map(ref mut m) = state {
        m.insert(
            "main_bias".into(),
            Value::List(vec![Value::Float(0.7), Value::Float(0.3)]),
        );
    }
    let mut fresh = create_layout("tall", config()).unwrap();
    assert!(fresh.unserialize(&state, &mut wl, true));
    let back = fresh.layout_state();
    let main = back.get("main_bias").unwrap().as_list().unwrap();
    assert!((main[0].as_f64().unwrap() - 0.7).abs() < 1e-12);
}
