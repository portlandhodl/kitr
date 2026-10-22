// SPDX-License-Identifier: GPL-3.0-or-later
//! Standalone benchmark for the CPU-only layout/tab model.
//!
//! Run with:
//!
//! ```text
//! cargo run --release -p kitty-ui --example bench
//! cargo run --release -p kitty-ui --example bench -- 5000 1000
//! ```
//!
//! This intentionally uses only `std::time` instead of a benchmarking crate so
//! the workspace stays dependency-free.

use std::env;
use std::hint::black_box;
use std::time::{Duration, Instant};

use kitty_ui::layout::{LayoutAddOptions, LayoutConfig, LayoutManager, Region, WindowLocation};
use kitty_ui::session::{serialize_session, SessionOptions};
use kitty_ui::*;

fn config() -> LayoutConfig {
    LayoutConfig::default()
        .with_central(Region::new(0, 0, 3840, 2160))
        .with_cell(10, 20)
}

fn time<T>(label: &str, mut f: impl FnMut() -> T) -> Duration {
    let start = Instant::now();
    black_box(f());
    let elapsed = start.elapsed();
    println!("{label:<46} {:>10.3} ms", elapsed.as_secs_f64() * 1e3);
    elapsed
}

fn main() {
    let mut args = env::args().skip(1);
    let windows: u64 = args.next().and_then(|x| x.parse().ok()).unwrap_or(1000);
    let tabs: usize = args.next().and_then(|x| x.parse().ok()).unwrap_or(500);

    println!("kitty-ui CPU model bench: {windows} windows in one splits tab, {tabs} tabs\n");

    let mut wl = WindowList::new();
    let mut layouts = LayoutManager::new(1, 1, &["splits".into()], config()).unwrap();

    let add = time("add windows (splits tree + relayout)", || {
        for i in 1..=windows {
            layouts.add_window(
                &mut wl,
                Window::new(i, format!("window {i}")).with_cwd("/tmp"),
                &LayoutAddOptions::default(),
            );
        }
    });

    let focus = time("focus cycling (4 full passes)", || {
        for _ in 0..windows * 4 {
            wl.activate_next_window_group(1);
        }
    });

    let neighbors = time("neighbour lookup for every window", || {
        let mut total = 0usize;
        for i in 1..=windows {
            let n = layouts.neighbors_for_window(i, &wl);
            total += n.left.len() + n.right.len() + n.top.len() + n.bottom.len();
        }
        total
    });

    let resize = time("resize same window 500 times", || {
        let target = wl.active_window_id().unwrap();
        let mut moved = 0;
        for i in 0..500 {
            if layouts.resize_window(&mut wl, target, if i % 2 == 0 { 0.01 } else { -0.01 }, true) {
                moved += 1;
            }
        }
        moved
    });

    let state = time("serialize layout state", || {
        layouts
            .serialize_layout_state_for_session(&wl)
            .to_json()
            .len()
    });
    time("restore layout state", || {
        let val = layouts.serialize_layout_state_for_session(&wl);
        let mut fresh = LayoutManager::new(1, 1, &["splits".into()], config()).unwrap();
        black_box(fresh.unserialize_layout_state_from_session(&val, &mut wl))
    });

    let mut tm = TabManager::new(1);
    let tab_add = time("create tabs and one window in each", || {
        for i in 0..tabs {
            tm.add_tab(
                format!("tab {i}"),
                Some("/tmp".into()),
                &["tall".into()],
                config(),
            )
            .unwrap();
            let mut w = Window::new((windows + i as u64 + 1) as u64, format!("w{i}"));
            w.launch = LaunchSpec::with_args(["sh"]);
            tm.add_window(tm.active_tab_idx(), w, &LayoutAddOptions::default());
        }
    });

    let tab_cycle = time("cycle through all tabs 10 times", || {
        for _ in 0..tabs * 10 {
            tm.next_tab(1);
        }
    });

    let session = time("serialize tab manager to session", || {
        serialize_session(&tm, "/tmp/bench.kitty-session", false)
            .join("\n")
            .len()
    });

    let raw = serialize_session(&tm, "/tmp/bench.kitty-session", false).join("\n");
    let parse = time("parse serialized session", || {
        parse_sessions(
            &raw,
            &SessionOptions {
                enabled_layouts: vec!["tall".into()],
                shell: "sh".into(),
            },
            None,
            "bench",
            "/tmp/bench.kitty-session",
        )
        .map(|x| x.len())
    });

    let total = add + focus + neighbors + resize + tab_add + tab_cycle + session + parse;
    println!(
        "\n{:<46} {:>10.3} ms",
        "total measured",
        total.as_secs_f64() * 1e3
    );
    println!(
        "session has {} tabs; layout/session state sizes included above",
        tm.tabs().len()
    );
    let _ = (state, session);

    // Keep the split-resize geometry path exercised at high window counts too.
    let mut one = WindowList::new();
    let mut split = LayoutManager::new(2, 2, &["splits".into()], config()).unwrap();
    split.add_window(&mut one, Window::new(1, "a"), &LayoutAddOptions::default());
    split.add_window(
        &mut one,
        Window::new(2, "b"),
        &LayoutAddOptions {
            location: WindowLocation::VSplit,
            ..Default::default()
        },
    );
    black_box(split.neighbors_for_window(2, &one));
}
