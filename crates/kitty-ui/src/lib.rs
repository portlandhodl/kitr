// SPDX-License-Identifier: GPL-3.0-or-later
//! Window/tab/layout and session modelling ported from kitty's Python
//! (`tabs.py`, `window.py`, `layout/*.py`, `session.py`).
//!
//! This crate is deliberately the CPU-only model: no GUI, no PTY spawning and no
//! screen/child ownership. Windows carry opaque child ids, while layouts
//! allocate cell geometries on window groups. A future GUI layer binds the
//! model to real OS windows and PTYs.
//!
//! # Modelled
//!
//! * [`Window`]/[`WindowList`]/[`WindowGroup`]: ids, titles, cwd, running and
//!   focus state, overlays, focus history, close/reflow and move semantics.
//! * [`Tab`]/[`TabManager`]: tab ordering, tab history, window moves between
//!   tabs, dirty/active callbacks and OS-window metadata.
//! * Layouts: `splits`, `stack`, `tall`, `fat`, `grid`, `vertical` and
//!   `horizontal`, with add/remove/neighbour/resize/move/focus operations,
//!   per-layout serialisable state and enabled-layout cycling.
//! * Sessions: kitty session parsing (comments, shell quoting, `$VAR`
//!   expansion, env/vars, relative cwd, layout state) plus session
//!   serialisation and [`RestorePlan`] generation.
//!
//! # Not modelled here
//!
//! Real PTYs/child processes, screen buffers, window/OS-window binding, border
//! painting, tab-bar rendering and kittens. Those belong to `kitty-os`, the
//! renderer/GUI layers and `kitty-kittens`; this crate stores only opaque ids
//! for them.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod layout;
pub mod model;
pub mod session;
pub mod tab;
pub mod value;

pub use layout::{
    create_layout, parse_layout_names, Bias, Grid, LayoutAddOptions, LayoutConfig, LayoutData,
    LayoutImpl, LayoutManager, Neighbors, Pair, Region, SplitChild, Splits, SplitsLayoutOpts,
    Stack, Tall, TallLayoutOpts, Vertical, WindowLocation, ALL_LAYOUT_NAMES,
};
pub use model::{
    AddWindowOptions, Edge, Edges, GroupId, LaunchSpec, OsWindowId, TabId, Window, WindowGeometry,
    WindowGroup, WindowId, WindowList,
};
pub use session::{
    parse_session, parse_sessions, restore_plans, serialize_session, session_arg_to_name,
    shlex_join, shlex_split, OsWindowRestore, RestorePlan, Session, SessionError, SessionOptions,
    SessionTab, TabRestore, WindowRestore, WindowSize, WindowSizes, WindowSpec,
};
pub use tab::{OsWindowMetadata, OsWindowSize, Tab, TabManager};
pub use value::Value;
