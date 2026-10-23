# kitr vs C kitty screenshot parity

Generated: 2026-10-22 22:12:42 PDT

## Environment

- C kitty: `/home/qrsnap/kitty/kitty/launcher/kitty`
- kitten: `/home/qrsnap/kitty/kitty/launcher/kitten`
- kitr: `/home/qrsnap/kitty/rust/target/release/kitr`
- Python: `3.9.25`
- display: `:900` (Xvfb unless `--display` was used)
- font: `DejaVu Sans Mono` at `12.0pt`
- kitr startup settle: `1.5s`

## Command

```sh
scripts/screenshot_parity.py --corpus scripts/parity_corpus --kitty /home/qrsnap/kitty/kitty/launcher/kitty --kitten /home/qrsnap/kitty/kitty/launcher/kitten --kitr target/release/kitr --artifacts artifacts/screenshot-parity --markdown PARITY.md --json artifacts/screenshot-parity/report.json --gaps scripts/parity_corpus/GAPS.md --timeout 15 --settle 1.5
```

`exact` counts any non-zero channel difference. `tolerant` ignores per-channel differences up to the pixel threshold. `aligned` uses the best integer offset (shown as `(dx,dy)`); `raw` compares at `(0,0)` and therefore includes tab-bar/border chrome differences. Pass/fail uses aligned tolerant percentage.

## Results

| Case | Status | Exact aligned | Tolerant aligned | Exact raw | Tolerant raw | Offset | Artifacts |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| 00_blank | pass | 0.005% | 0.005% | 0.000% | 0.000% | (0,-2) | [00_blank](artifacts/screenshot-parity/cases/00_blank) |
| 01_ascii | pass | 1.521% | 1.349% | 1.521% | 1.349% | (0,0) | [01_ascii](artifacts/screenshot-parity/cases/01_ascii) |
| 02_utf8 | pass | 1.403% | 1.274% | 1.403% | 1.274% | (0,0) | [02_utf8](artifacts/screenshot-parity/cases/02_utf8) |
| 03_wide | pass | 1.007% | 0.869% | 1.007% | 0.869% | (0,0) | [03_wide](artifacts/screenshot-parity/cases/03_wide) |
| 04_combining | pass | 1.363% | 1.200% | 1.363% | 1.200% | (0,0) | [04_combining](artifacts/screenshot-parity/cases/04_combining) |
| 05_sgr | pass | 3.198% | 2.852% | 3.198% | 2.852% | (0,0) | [05_sgr](artifacts/screenshot-parity/cases/05_sgr) |
| 06_cursor | pass | 1.210% | 1.080% | 1.210% | 1.080% | (0,0) | [06_cursor](artifacts/screenshot-parity/cases/06_cursor) |
| 07_erase | pass | 1.410% | 1.234% | 1.410% | 1.234% | (0,0) | [07_erase](artifacts/screenshot-parity/cases/07_erase) |
| 08_scroll | pass | 3.270% | 2.910% | 3.270% | 2.910% | (0,0) | [08_scroll](artifacts/screenshot-parity/cases/08_scroll) |
| 09_alt_screen | pass | 0.936% | 0.828% | 0.936% | 0.828% | (0,0) | [09_alt_screen](artifacts/screenshot-parity/cases/09_alt_screen) |
| 13_hyperlinks | pass | 1.121% | 0.994% | 1.121% | 0.994% | (0,0) | [13_hyperlinks](artifacts/screenshot-parity/cases/13_hyperlinks) |
| 14_selection | pass | 1.343% | 1.191% | 1.343% | 1.191% | (0,0) | [14_selection](artifacts/screenshot-parity/cases/14_selection) |
| 15_mouse_cursor | pass | 0.853% | 0.751% | 0.853% | 0.751% | (0,0) | [15_mouse_cursor](artifacts/screenshot-parity/cases/15_mouse_cursor) |
| 16_graphics_image | pass | 1.022% | 0.956% | 1.022% | 0.956% | (0,0) | [16_graphics_image](artifacts/screenshot-parity/cases/16_graphics_image) |
| 17_graphics_animation | pass | 6.495% | 6.363% | 6.130% | 6.036% | (0,-3) | [17_graphics_animation](artifacts/screenshot-parity/cases/17_graphics_animation) |
| 18_text_sizing | pass | 0.793% | 0.709% | 0.793% | 0.709% | (0,0) | [18_text_sizing](artifacts/screenshot-parity/cases/18_text_sizing) |
| 23_multiline_selection | pass | 7.448% | 7.189% | 6.811% | 6.652% | (0,1) | [23_multiline_selection](artifacts/screenshot-parity/cases/23_multiline_selection) |
| 10_tabs | pass | 1.276% | 1.219% | 1.237% | 1.195% | (0,-1) | [10_tabs](artifacts/screenshot-parity/cases/10_tabs) |
| 11_layouts | pass | 0.554% | 0.495% | 0.733% | 0.692% | (-2,-24) | [11_layouts](artifacts/screenshot-parity/cases/11_layouts) |
| 12_hsplits | pass | 0.849% | 0.849% | 1.067% | 1.016% | (0,38) | [12_hsplits](artifacts/screenshot-parity/cases/12_hsplits) |
| 19_tab_bar_both_sides | pass | 2.604% | 2.522% | 2.528% | 2.450% | (0,-3) | [19_tab_bar_both_sides](artifacts/screenshot-parity/cases/19_tab_bar_both_sides) |
| 20_tab_bar_fade_both_sides | pass | 2.355% | 2.343% | 3.671% | 3.466% | (3,18) | [20_tab_bar_fade_both_sides](artifacts/screenshot-parity/cases/20_tab_bar_fade_both_sides) |
| 21_fractional_split_margins | pass | 3.001% | 3.000% | 3.576% | 3.488% | (0,48) | [21_fractional_split_margins](artifacts/screenshot-parity/cases/21_fractional_split_margins) |
| 22_full_split_borders | pass | 4.440% | 4.345% | 4.620% | 4.527% | (0,1) | [22_full_split_borders](artifacts/screenshot-parity/cases/22_full_split_borders) |

Summary: 24 pass, 0 fail, 0 skipped, 0 error.

Artifacts for each case contain `kitty.png`, `kitr.ppm`, normalized `kitty.ppm`, `diff.ppm`, `diff.png`, logs and the per-case `report.json`.

## Reproduction notes and known visual gaps

These notes describe how to rebuild the reference and the differences that
remain after running the corpus. They are intentionally specific about what the
suite does **not** prove.

### Reference build

C kitty 0.49.2 was built from this tree without changing its C sources:

```sh
scripts/build_c_kitty.sh            # debug build, conda py312 + pinned slangc
cargo build --release -p kitty      # kitr release binary
kitty/launcher/kitty --version      # kitty 0.49.2 created by Kovid Goyal
rust/target/release/kitr --version  # kitr 0.1.1
```

The build script reported success and the normal test invocation is the one
shown under `## Command` above. It exported
`PKG_CONFIG_PATH=/tmp/opencode/py312/lib/pkgconfig` and
`LD_LIBRARY_PATH=/tmp/opencode/py312/lib`, added
`-Wno-unused-but-set-global` for clang 23, and disabled the optional Wayland
backend because `wayland-protocols` is not installed. It used slangc 2026.20
and Symbols Nerd Font Mono v3.4.0.

### Method and caveats

- Reference: C kitty 0.49.2 (debug build). Candidate: kitr 0.1.1 (release build).
- Both run under Xvfb at 1280x800x24, 80x24 cells, DejaVu Sans Mono 12pt at
  96dpi, block cursor with blinking disabled, zero window padding. The tab bar
  is hidden except in `10_tabs`.
- Comparison is 1:1 pixels; nothing is rescaled. C kitty's screenshot includes a
  1px right/bottom border (801x457 for 80x24), while kitr's is 800x456.
- Session paths are resolved to absolute paths before launching either terminal.
  Earlier runs passed relative paths and C kitty looked them up under
  `~/.config/kitty`, so its `10_tabs` reference was really the invalid-session
  fallback window.
- Text parity was spot-checked for `02_utf8`, `05_sgr` and `08_scroll`: C kitty
  `kitten @ get-text` and kitr `--dump-screen` produced the same cells (modulo a
  trailing newline). The pixel differences below are therefore rendering and
  chrome differences, not different screen contents.

### Before/after per case (aligned tolerant)

| Case | Before | After | Change |
| --- | ---: | ---: | --- |
| 00_blank | 0.000% | 0.005% | +0.005 (alignment picks `(0,-2)` instead of `(0,0)`; raw is 0.000%) |
| 01_ascii | 1.193% | 1.349% | +0.156 (rebuilt reference/font environment) |
| 02_utf8 | 1.328% | 1.274% | -0.054 (colour/RTL shaping path) |
| 03_wide | 0.814% | 0.869% | +0.055 (rebuilt reference/font environment) |
| 04_combining | 1.086% | 1.200% | +0.114 (rebuilt reference/font environment) |
| 05_sgr | 3.538% | 2.852% | -0.686 (palette fix; still the largest text case) |
| 06_cursor | 0.953% | 1.080% | +0.127 (rebuilt reference/font environment) |
| 07_erase | 2.149% | 1.234% | -0.915 (no phantom tab strip) |
| 08_scroll | 2.644% | 2.910% | +0.266 (rebuilt reference/font environment) |
| 09_alt_screen | 0.745% | 0.828% | +0.083 (rebuilt reference/font environment) |
| 10_tabs | 3.704% | 1.219% | -2.485 (aligned below the tab bar; see the tab-bar caveat below) |
| 11_layouts | skipped | 0.495% | new vertical split case (`layout splits`, `--location=vsplit`) |
| 12_hsplits | skipped | 0.849% | new horizontal split case (`layout splits`, `--location=hsplit`) |
| 13_hyperlinks | skipped | 0.994% | new hyperlink case |
| 14_selection | skipped | 1.191% | new selection case |
| 15_mouse_cursor | skipped | 0.751% | new painted cursor case |
| 16_graphics_image | skipped | 0.956% | new graphics-protocol image case |
| 17_graphics_animation | skipped | 6.363% | new animation case |
| 18_text_sizing | skipped | 0.709% | new text-sizing case |
| 19_tab_bar_both_sides | skipped | 2.522% | new bottom tab bar case |
| 20_tab_bar_fade_both_sides | skipped | 2.343% | new bottom fade tab bar case |
| 21_fractional_split_margins | skipped | 3.000% | new fractional margin case |
| 22_full_split_borders | skipped | 4.345% | new full-border case |
| 23_multiline_selection | skipped | 7.189% | new multi-line selection case |

Raw tolerant percentages improved for every non-split text case because kitr no
longer reserves a 19px strip when the bar is hidden. `10_tabs` raw is 1.195%
because kitr draws the bar while the C session screenshot below does not include
one.

### 1. Tab bar visibility and geometry

- `tab_bar_style hidden` and the single-tab autohide rule
  (`tab_bar_min_tabs`, default 2) now reserve zero pixels. The OS window height
  follows kitty's convention: `initial_window_height` is the total height, and
  a visible bar takes one row from the central grid instead of growing the
  window. Adding or closing tabs updates the central row count without resizing
  the OS window.
- `hidden`, `powerline`, `separator`, `slant` and `fade` are implemented in the
  GUI tab strip, with kitty's default tab colours, fonts, powerline glyphs and
  separator text. `custom` currently falls back to the fade renderer. There is
  no kitty tab-bar style named `fat`; `fat` is a window layout.
- `10_tabs` caveat: in this C build the `--session` startup path never marks the
  tab bar visible, even though both tabs exist and `tab_bar_style=powerline` is
  set; the C reference is 801x457 with no bar. kitr does show the bar for the
  same two-tab session. The aligned score therefore compares the content
  *below* kitr's bar and does not validate tab-bar pixels; the raw score includes
  the bar. Manually driving C kitty (launch normally, then `kitten @ action
  new_tab`) does show the bar, so this is a difference in the session startup
  path, not in kitty's visibility rules. Tab-bar pixel styling remains a known
  gap for this corpus.

### 2. Font rasterisation and antialias gamma

Cell metrics and line positions match (10x19 cells, baseline 15, all lines align
after the offset), and the sampled text content matched exactly. Even so, exact
pixel difference on ordinary text is about 1-2% because the FreeType
rasterisation differs: kitr produces darker intermediate antialias values
(`#262626`, `#232323`) where C kitty produces lighter edges (`#5f5f5f`,
`#636363`). Example: `01_ascii` is 1.521% exact / 1.349% tolerant after
alignment. The sRGB gamma LUT itself matches `kitty/srgb_gamma.h` (unit tested),
and this C build's `foreground_contrast_new` shader path is a no-op, so the
remaining difference is glyph rasterisation, not colour gamma.

### 3. UTF-8 fallback and shaping

`02_utf8` is 1.274% tolerant after alignment. Latin, Greek and Cyrillic match
well. The Arabic and Hebrew RTL lines and the emoji line are the largest
per-line contributors, consistent with fallback-font and shaping differences.
Emoji are monochrome in both screenshots with this font configuration; colour
emoji rendering is not exercised.

### 4. ANSI colour palette

The default `color0`..`color15` entries come from kitty's option defaults
(`#cc0403` red, `#19cb00` green, `#f2201f` bright red, `#23fd00` bright green,
etc.); the 6x6x6 cube and grayscale ramp still match `init_FG_BG_table()` in
`colors.c`. A unit test cross-checks all 16 values plus representative cube and
ramp entries. `05_sgr` is 2.852% aligned tolerant.

### 4a. Indexed-colour images and animation storage

Indexed protocol images (`f=8`/`f=16`) now stay as integer index textures; the
GL indexed-graphics shader looks each pixel up in the std140-padded live
palette, so changing terminal colours does not re-upload the image and the
change is visible on the next draw. Animation frames remain delta-compressed and
can be spilled to a private disk cache under the renderer's temp directory;
deleted, edited, evicted and replaced frame files are unlinked immediately, and
the cache subdirectory is removed on drop/clear. `16_graphics_image` and
`17_graphics_animation` exercise the graphics and animation paths in the
corpus, and both still have CPU/GPU unit tests, including a live-palette redraw
and a per-frame disk-cleanup test. Placements without a `p` id now keep distinct graph
instances (the protocol allows several per image), and cascade deletion follows
the first-placement rule for `Q=0`. The screen sink still does not advance the
cursor after a put without `C=1`, since that needs cell pixel metrics, so that
side effect is not modelled by the screenshot corpus.

### 4b. Shader-path completion

The deferred renderer paths are now selected and covered by headless GL tests:
the `cell`/`cell_bg`/`cell_fg` programs consume a fixed-cell
`GL_TEXTURE_2D_ARRAY` sprite atlas adapted from the packed CPU pages (via
`GlBackend::set_cell_programs`/`render_cells_with_atlas`), `bgimage` draws
representable opaque background layouts, `blit`/`screenshot` are exposed as
offscreen/readback operations, and animated multi-group custom shader pipelines
run through ping-pong and named textures with kitty's start/toggle/stop timing.
These paths do not change the default frame adapter used by the screenshot
corpus, so the percentages above are unchanged. Remaining limits are documented
in `crates/kitty-render/SHADERS.md` (multi-cell glyphs fall back to the graphics
path, `border`/`padding` still use solid chrome instances, and the full
`KittyCustomShaderData`/pointer-map payload is not populated).

### 5. Cursor, selection and colours

Default foreground/background (`#dddddd`/`#000000`), cursor (`#cccccc`), cursor
text (`#111111`), selection foreground (`#000000`) and selection background
(`#fffacd`) match kitty's option defaults and are unit tested. `06_cursor` is
1.080% tolerant, entirely antialias-edge noise.

### 6. Pane borders and background gaps

`kitty_render::pane_border_rects()` implements kitty's minimal-border rule (only
edges shared with a neighbouring pane; each pane contributes its own strip, so a
split gets a 2x thickness gap) and full borders. The GUI emits them as a
transparent chrome overlay so per-pane scissor clears cannot erase them, using
`active_border_color`, `inactive_border_color` and `bell_border_color` with
`window_border_width` scaled to device pixels. Unit tests cover horizontal and
vertical splits, single-pane suppression and full borders. The cell-grid layout
does not reserve fractional border pixels or `window_margin_width`, so split
geometry remains approximate. `11_layouts`, `12_hsplits`,
`21_fractional_split_margins` and `22_full_split_borders` exercise both split
directions, margins and full borders; their aligned-tolerant results are in the
table above.

### 7. Tabs and layouts

`launch --location` is parsed, validated and restored before layout state is
applied, so sessions can reproduce `vsplit`, `hsplit`, `before`, `after`,
`first`, `last`, `neighbor` and `split`. `11_layouts` (vertical) and
`12_hsplits` (horizontal) cover the two primary split directions; tabs remain
covered by `10_tabs` subject to the C session-path caveat above. Serialised
layout state is round-tripped in unit tests, but restoration of every layout
parameter (biases, arbitrary nesting) is not visually tested.

### 8. Other untested surfaces

Window decorations/titlebar, undercurl and coloured underlines, ligatures beyond
the ASCII corpus, IME/compose input and live pointer/cursor animation are outside
this screenshot corpus. `13_hyperlinks`, `14_selection`,
`15_mouse_cursor`, `16_graphics_image`, `17_graphics_animation` and
`23_multiline_selection` now cover hyperlinks, selection, cursor painting,
images, animation and multi-line selection. Wayland cursor-surface painting,
`zwp_tablet_v2` pressure/tilt forwarding and `wlr-layer-shell` panel/QAT
surfaces are unit tested or compile-checked in `kitty-window` and the GUI, but
still need a real compositor/tablet for end-to-end validation. Core `wl_touch`
has no pressure axis, so Wayland finger-touch pressure remains the
protocol-implied 1.0/0.0; X11 and tablet-tool contacts use the device value
through the configurable pressure curve.

### 9. Erase, scroll and alternate screen

`07_erase` (1.234%), `08_scroll` (2.910%) and `09_alt_screen` (0.828%) show the
same picture: terminal semantics match, while glyph rasterisation and cursor
edges account for the remaining pixels.

### 10. Remaining non-macOS parity diffs

- Glyph rasterisation/antialias gamma remains the dominant pixel difference.
- The C session startup path's tab-bar visibility differs from kitr, so tab-bar
  pixels are not validated by `10_tabs`.
- Split geometry does not reserve fractional border/margin pixels; the corpus
  validates text/placement but not exact border widths.
- Wayland `wl_touch` has no pressure axis; only X11 touch and
  `zwp_tablet_v2` tools can forward real pressure. Tablet pressure is shaped by
  `PressureCurve`; tilt/rotation/slider/wheel are forwarded, but no compositor
  or physical tablet is part of this corpus.
- The Flatpak manifest uses generated `packaging/flatpak/cargo-sources.json`;
  CI runs `flatpak-builder`, `dpkg-buildpackage` and `rpmbuild`, and runs this
  full corpus against a distro C kitty. Package jobs still produce unsigned
  artifacts only (no repository signing).
- Layer-shell panels/QAT are created by the GUI, but the single-instance
  toggle/`--toggle-visibility` control socket and hide-on-focus-loss policy are
  not exercised end to end without a panel-aware compositor session.
