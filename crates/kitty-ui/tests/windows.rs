// SPDX-License-Identifier: GPL-3.0-or-later
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use kitty_ui::layout::LayoutConfig;
use kitty_ui::*;

fn config() -> LayoutConfig {
    LayoutConfig::default()
        .with_central(Region::new(0, 0, 1000, 600))
        .with_cell(10, 20)
}

fn opts() -> Vec<String> {
    vec!["tall".into()]
}

fn list_with(n: u64) -> WindowList {
    let mut wl = WindowList::new();
    for i in 1..=n {
        wl.add_window(Window::new(i, format!("w{i}")), AddWindowOptions::active());
    }
    wl.set_active_group_idx(0, true);
    wl
}

#[test]
fn add_windows_creates_groups_and_order() {
    let wl = list_with(5);
    assert_eq!(wl.len(), 5);
    assert_eq!(wl.num_groups(), 5);
    assert_eq!(wl.active_group_idx(), 0);
    let ids: Vec<_> = wl.groups().iter().map(|g| g.windows[0]).collect();
    assert_eq!(ids, vec![1, 2, 3, 4, 5]);
}

#[test]
fn next_previous_focus_cycles() {
    let mut wl = list_with(3);
    wl.activate_next_window_group(1);
    assert_eq!(wl.active_window_id(), Some(2));
    wl.activate_next_window_group(1);
    assert_eq!(wl.active_window_id(), Some(3));
    wl.activate_next_window_group(1);
    assert_eq!(wl.active_window_id(), Some(1));
    wl.activate_next_window_group(-1);
    assert_eq!(wl.active_window_id(), Some(3));
}

#[test]
fn focus_history_previous_group() {
    let mut wl = list_with(4);
    wl.set_active_group_idx(1, true);
    wl.set_active_group_idx(3, true);
    assert_eq!(wl.active_window_id(), Some(4));
    wl.make_previous_group_active(1, true);
    assert_eq!(wl.active_window_id(), Some(2));
    // Focusing a group pushes the previous one back onto the history, so the
    // next "previous" returns to where we came from (kitty semantics).
    wl.make_previous_group_active(1, true);
    assert_eq!(wl.active_window_id(), Some(4));
}

#[test]
fn active_history_is_most_recent_first_and_deduped() {
    let mut wl = list_with(3);
    wl.set_active_group_idx(1, true);
    wl.set_active_group_idx(2, true);
    wl.set_active_group_idx(1, true);
    let h = wl.active_window_history();
    assert_eq!(h[0], 3);
    assert!(h.contains(&2));
    assert!(h.contains(&1));
    let mut seen = std::collections::HashSet::new();
    for id in h {
        assert!(seen.insert(id));
    }
}

#[test]
fn move_window_group_swaps() {
    let mut wl = list_with(5);
    assert_eq!(wl.active_group_idx(), 0);
    assert!(wl.move_window_group(Some(3), None));
    assert_eq!(wl.active_group_idx(), 3);
    let ids: Vec<_> = wl.groups().iter().map(|g| g.windows[0]).collect();
    assert_eq!(ids, vec![4, 2, 3, 1, 5]);
    wl.set_active_group_idx(0, true);
    assert!(wl.move_window_group(Some(3), None));
    let ids: Vec<_> = wl.groups().iter().map(|g| g.windows[0]).collect();
    assert_eq!(ids, vec![1, 2, 3, 4, 5]);
}

#[test]
fn close_active_group_returns_to_previous_window() {
    let mut wl = list_with(6);
    wl.set_active_group_idx(5, true);
    let prev = wl.active_window_id();
    wl.set_active_group_idx(3, true);
    wl.remove_window(4);
    assert_eq!(wl.active_window_id(), prev);
    let ids: Vec<_> = wl.groups().iter().map(|g| g.windows[0]).collect();
    assert_eq!(ids, vec![1, 2, 3, 5, 6]);
}

#[test]
fn close_non_active_group_keeps_focus() {
    let mut wl = list_with(4);
    wl.set_active_group_idx(3, true);
    let active = wl.active_window_id();
    wl.remove_window(1);
    assert_eq!(wl.active_window_id(), active);
    assert_eq!(wl.active_group_idx(), 2);
    let ids: Vec<_> = wl.groups().iter().map(|g| g.windows[0]).collect();
    assert_eq!(ids, vec![2, 3, 4]);
}

#[test]
fn overlay_joins_existing_group() {
    let mut wl = list_with(5);
    let overlay = Window::new(6, "overlay");
    wl.add_window(
        overlay,
        AddWindowOptions {
            group_of: Some(2),
            make_active: true,
            ..Default::default()
        },
    );
    assert_eq!(wl.num_groups(), 5);
    let g = wl.group_for_window(2).unwrap();
    assert_eq!(g.windows, vec![2, 6]);
    assert_eq!(wl.active_window_id(), Some(6));
    wl.remove_window(6);
    assert_eq!(wl.active_window_id(), Some(2));
    assert_eq!(wl.num_groups(), 5);
}

#[test]
fn move_window_to_top_of_group() {
    let mut wl = list_with(3);
    wl.add_window(
        Window::new(10, "overlay"),
        AddWindowOptions {
            group_of: Some(1),
            make_active: false,
            ..Default::default()
        },
    );
    let gid = wl.group_id_for_window(1).unwrap();
    let g = wl.group(gid).unwrap();
    assert_eq!(g.windows, vec![1, 10]);
    // make window 1 active within its group
    assert!(wl.move_window_to_top_of_group(1));
    assert_eq!(wl.group(gid).unwrap().windows, vec![10, 1]);
    assert_eq!(wl.active_window_id(), Some(1));
}

#[test]
fn active_and_dirty_callbacks_fire() {
    let mut wl = WindowList::new();
    let changes = Rc::new(RefCell::new(Vec::new()));
    let c = changes.clone();
    wl.add_active_callback(move |old, new| c.borrow_mut().push((old, new)));
    let dirty = Rc::new(RefCell::new(0));
    let d = dirty.clone();
    wl.add_dirty_callback(move || *d.borrow_mut() += 1);
    wl.add_window(Window::new(1, "one"), AddWindowOptions::active());
    wl.add_window(Window::new(2, "two"), AddWindowOptions::active());
    assert_eq!(changes.borrow().last().copied(), Some((Some(1), Some(2))));
    wl.mark_dirty();
    wl.mark_dirty();
    assert_eq!(*dirty.borrow(), 2);
}

fn manager_with_tabs(n: usize) -> TabManager {
    let mut tm = TabManager::new(1);
    for i in 0..n {
        tm.add_tab(format!("t{i}"), None, &opts(), config())
            .unwrap();
    }
    tm
}

#[test]
fn tabs_can_be_added_and_cycled() {
    let mut tm = manager_with_tabs(3);
    assert_eq!(tm.tabs().len(), 3);
    assert_eq!(tm.active_tab_idx(), 2);
    assert!(tm.next_tab(1));
    assert_eq!(tm.active_tab_idx(), 0);
    assert!(tm.previous_tab());
    assert_eq!(tm.active_tab_idx(), 2);
}

#[test]
fn closing_active_tab_prefers_previous() {
    let mut tm = manager_with_tabs(3);
    tm.set_active_tab_idx(0);
    tm.set_active_tab_idx(2);
    let removed = tm.close_tab(2).unwrap();
    assert_eq!(removed.name, "t2");
    assert_eq!(tm.tabs().len(), 2);
    assert_eq!(tm.active_tab_idx(), 0);
}

#[test]
fn move_window_between_tabs_preserves_id_and_metadata() {
    let mut tm = manager_with_tabs(2);
    let mut win = Window::new(42, "moved").with_cwd(PathBuf::from("/tmp"));
    win.override_title = Some("hello".into());
    tm.add_window(0, win, &LayoutAddOptions::default());
    let target = tm.tabs()[1].id;
    assert!(tm.move_window_to_tab(0, 42, target));
    // The now-empty source tab is closed automatically.
    assert_eq!(tm.tabs().len(), 1);
    assert_eq!(tm.tabs()[0].windows().len(), 1);
    let w = tm.tabs()[0].windows().window(42).unwrap();
    assert_eq!(w.effective_title(), "hello");
    assert_eq!(w.cwd.as_deref(), Some(std::path::Path::new("/tmp")));
}

#[test]
fn tab_title_falls_back_to_window() {
    let mut tm = manager_with_tabs(1);
    tm.tabs_mut()[0].set_title("");
    tm.add_window(
        0,
        Window::new(1, "window title"),
        &LayoutAddOptions::default(),
    );
    assert_eq!(tm.tabs()[0].effective_title(), "window title");
    tm.tabs_mut()[0].set_title("tab title");
    assert_eq!(tm.tabs()[0].effective_title(), "tab title");
}

#[test]
fn tab_manager_dirty_and_active_callbacks() {
    let mut tm = manager_with_tabs(1);
    let dirty = Rc::new(RefCell::new(0));
    let d = dirty.clone();
    tm.add_dirty_callback(move || *d.borrow_mut() += 1);
    let active = Rc::new(RefCell::new(Vec::new()));
    let a = active.clone();
    tm.add_active_callback(move |o, n| a.borrow_mut().push((o, n)));
    tm.add_window(0, Window::new(1, "w"), &LayoutAddOptions::default());
    tm.add_tab("two", None, &opts(), config()).unwrap();
    assert!(*dirty.borrow() >= 2);
    assert!(!active.borrow().is_empty());
}
