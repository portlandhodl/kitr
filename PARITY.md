# kitr vs C kitty screenshot parity

Generated: 2026-10-22 10:55:01 PDT

## Environment

- C kitty: `../kitty/launcher/kitty`
- kitten: `../kitty/launcher/kitten`
- kitr: `/home/qrsnap/kitty/rust/target/release/kitr`
- Python: `3.9.25`
- display: `:469` (Xvfb unless `--display` was used)
- font: `DejaVu Sans Mono` at `12.0pt`
- kitr startup settle: `1.5s`

## Command

```sh
scripts/screenshot_parity.py --corpus scripts/parity_corpus --kitty ../kitty/launcher/kitty --kitten ../kitty/launcher/kitten --kitr target/release/kitr --artifacts artifacts/screenshot-parity --markdown PARITY.md --json artifacts/screenshot-parity/report.json --gaps scripts/parity_corpus/GAPS.md
```

`exact` counts any non-zero channel difference. `tolerant` ignores per-channel differences up to the pixel threshold. `aligned` uses the best integer offset (shown as `(dx,dy)`); `raw` compares at `(0,0)` and therefore includes tab-bar/border chrome differences. Pass/fail uses aligned tolerant percentage.

## Results

| Case | Status | Exact aligned | Tolerant aligned | Exact raw | Tolerant raw | Offset | Artifacts |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| 00_blank | pass | 0.005% | 0.005% | 0.000% | 0.000% | (0,-2) | [00_blank](artifacts/screenshot-parity/cases/00_blank) |
| 01_ascii | pass | 1.521% | 1.349% | 1.521% | 1.349% | (0,0) | [01_ascii](artifacts/screenshot-parity/cases/01_ascii) |
| 02_utf8 | pass | 1.566% | 1.417% | 1.566% | 1.417% | (0,0) | [02_utf8](artifacts/screenshot-parity/cases/02_utf8) |
| 03_wide | pass | 1.003% | 0.869% | 1.003% | 0.869% | (0,0) | [03_wide](artifacts/screenshot-parity/cases/03_wide) |
| 04_combining | pass | 1.355% | 1.198% | 1.355% | 1.198% | (0,0) | [04_combining](artifacts/screenshot-parity/cases/04_combining) |
| 05_sgr | pass | 3.207% | 2.857% | 3.207% | 2.857% | (0,0) | [05_sgr](artifacts/screenshot-parity/cases/05_sgr) |
| 06_cursor | pass | 1.210% | 1.080% | 1.210% | 1.080% | (0,0) | [06_cursor](artifacts/screenshot-parity/cases/06_cursor) |
| 07_erase | pass | 1.410% | 1.234% | 1.410% | 1.234% | (0,0) | [07_erase](artifacts/screenshot-parity/cases/07_erase) |
| 08_scroll | pass | 3.270% | 2.910% | 3.270% | 2.910% | (0,0) | [08_scroll](artifacts/screenshot-parity/cases/08_scroll) |
| 09_alt_screen | pass | 0.936% | 0.828% | 0.936% | 0.828% | (0,0) | [09_alt_screen](artifacts/screenshot-parity/cases/09_alt_screen) |
| 10_tabs | pass | 0.297% | 0.271% | 1.889% | 1.828% | (0,19) | [10_tabs](artifacts/screenshot-parity/cases/10_tabs) |
| 11_layouts | pass | 0.298% | 0.239% | 0.489% | 0.454% | (0,-24) | [11_layouts](artifacts/screenshot-parity/cases/11_layouts) |
| 12_hsplits | pass | 1.208% | 1.156% | 1.309% | 1.262% | (2,-31) | [12_hsplits](artifacts/screenshot-parity/cases/12_hsplits) |

Summary: 13 pass, 0 fail, 0 skipped, 0 error.

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
rust/target/release/kitr --version  # kitr 0.1.0
```

The build script reported success and the normal test invocation is the one
shown under `## Command` above. It exported
`PKG_CONFIG_PATH=/tmp/opencode/py312/lib/pkgconfig` and
`LD_LIBRARY_PATH=/tmp/opencode/py312/lib`, added
`-Wno-unused-but-set-global` for clang 23, and disabled the optional Wayland
backend because `wayland-protocols` is not installed. It used slangc 2026.20
and Symbols Nerd Font Mono v3.4.0.

### Method and caveats

- Reference: C kitty 0.49.2 (debug build). Candidate: kitr 0.1.0 (release build).
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
| 02_utf8 | 1.328% | 1.417% | +0.089 (rebuilt reference/font environment) |
| 03_wide | 0.814% | 0.869% | +0.055 (rebuilt reference/font environment) |
| 04_combining | 1.086% | 1.198% | +0.112 (rebuilt reference/font environment) |
| 05_sgr | 3.538% | 2.857% | -0.681 (palette fix; still the largest text case) |
| 06_cursor | 0.953% | 1.080% | +0.127 (rebuilt reference/font environment) |
| 07_erase | 2.149% | 1.234% | -0.915 (no phantom tab strip) |
| 08_scroll | 2.644% | 2.910% | +0.266 (rebuilt reference/font environment) |
| 09_alt_screen | 0.745% | 0.828% | +0.083 (rebuilt reference/font environment) |
| 10_tabs | 3.704% | 0.271% | -3.433 (aligned below the tab bar; see the tab-bar caveat below) |
| 11_layouts | skipped | 0.239% | new vertical split case (`layout splits`, `--location=vsplit`) |
| 12_hsplits | skipped | 1.156% | new horizontal split case (`layout splits`, `--location=hsplit`) |

Raw tolerant percentages improved for every non-split text case because kitr no
longer reserves a 19px strip when the bar is hidden. `10_tabs` raw is 1.828%
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

`02_utf8` is 1.417% tolerant after alignment. Latin, Greek and Cyrillic match
well. The Arabic and Hebrew RTL lines and the emoji line are the largest
per-line contributors, consistent with fallback-font and shaping differences.
Emoji are monochrome in both screenshots with this font configuration; colour
emoji rendering is not exercised.

### 4. ANSI colour palette

The default `color0`..`color15` entries come from kitty's option defaults
(`#cc0403` red, `#19cb00` green, `#f2201f` bright red, `#23fd00` bright green,
etc.); the 6x6x6 cube and grayscale ramp still match `init_FG_BG_table()` in
`colors.c`. A unit test cross-checks all 16 values plus representative cube and
ramp entries. `05_sgr` is 2.857% aligned tolerant.

### 4a. Indexed-colour images and animation storage

Indexed protocol images (`f=8`/`f=16`) now stay as integer index textures; the
GL indexed-graphics shader looks each pixel up in the std140-padded live
palette, so changing terminal colours does not re-upload the image. Animation
frames remain delta-compressed and can be spilled to a private disk cache under
the renderer's temp directory; the cache subdirectory is removed on drop/clear.
Neither path is exercised by the screenshot corpus (no graphics protocol case or
animated image is included), but both have CPU/GPU unit tests.

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
geometry remains approximate. `11_layouts` and `12_hsplits` now exercise a
vertical and a horizontal split and pass at 0.239% and 1.156% aligned tolerant
respectively; the `12_hsplits` offset `(2,-31)` reflects the extra C kitty
chrome/border rows rather than different pane text.

### 7. Tabs and layouts

`launch --location` is parsed, validated and restored before layout state is
applied, so sessions can reproduce `vsplit`, `hsplit`, `before`, `after`,
`first`, `last`, `neighbor` and `split`. `11_layouts` (vertical) and
`12_hsplits` (horizontal) cover the two primary split directions; tabs remain
covered by `10_tabs` subject to the C session-path caveat above. Serialised
layout state is round-tripped in unit tests, but restoration of every layout
parameter (biases, arbitrary nesting) is not visually tested.

### 8. Other untested surfaces

Window decorations/titlebar, graphics protocol images, hyperlinks, undercurl and
coloured underlines, ligatures beyond the ASCII corpus, IME/compose input and
selection rendering are outside this screenshot corpus. Wayland cursor-surface
painting and X11 touch pressure mapping are unit tested in `kitty-window` but
need a real compositor/tablet to exercise end to end.

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
- Wayland `wl_touch` has no pressure axis; only X11 can forward real pressure.
  `zwp_tablet_v2` pressure curves/tilt remain unimplemented.
- The Flatpak manifest is a skeleton and needs generated Cargo sources before a
  reproducible Flatpak build; deb/RPM specs are not built in CI here.
