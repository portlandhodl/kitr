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
| 01_ascii | 1.193% | 1.193% | unchanged |
| 02_utf8 | 1.328% | 1.328% | unchanged |
| 03_wide | 0.814% | 0.814% | unchanged |
| 04_combining | 1.086% | 1.086% | unchanged |
| 05_sgr | 3.538% | 2.541% | -0.997 (palette fix) |
| 06_cursor | 0.953% | 0.953% | unchanged |
| 07_erase | 2.149% | 1.089% | -1.060 (no phantom tab strip) |
| 08_scroll | 2.644% | 2.644% | unchanged |
| 09_alt_screen | 0.745% | 0.745% | unchanged |
| 10_tabs | 3.704% | 0.255% | -3.449 (see the tab-bar caveat below) |

Raw tolerant percentages improved for every non-tab case (for example
`05_sgr` 11.297% -> 2.541%) because kitr no longer reserves a 19px strip when
the bar is hidden. `10_tabs` raw is 1.867% because kitr draws the bar while the
C session screenshot below does not include one.

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
`#636363`). Example: `01_ascii` is 1.503% exact / 1.193% tolerant after
alignment. The sRGB gamma LUT itself matches `kitty/srgb_gamma.h` (unit tested),
and this C build's `foreground_contrast_new` shader path is a no-op, so the
remaining difference is glyph rasterisation, not colour gamma.

### 3. UTF-8 fallback and shaping

`02_utf8` is 1.328% tolerant after alignment. Latin, Greek and Cyrillic match
well. The Arabic and Hebrew RTL lines and the emoji line are the largest
per-line contributors, consistent with fallback-font and shaping differences.
Emoji are monochrome in both screenshots with this font configuration; colour
emoji rendering is not exercised.

### 4. ANSI colour palette

The default `color0`..`color15` entries now come from kitty's option defaults
(`#cc0403` red, `#19cb00` green, `#f2201f` bright red, `#23fd00` bright green,
etc.); the 6x6x6 cube and grayscale ramp still match `init_FG_BG_table()` in
`colors.c`. A unit test cross-checks all 16 values plus representative cube and
ramp entries. `05_sgr` dropped from 3.538% to 2.541% aligned tolerant.

### 5. Cursor, selection and colours

Default foreground/background (`#dddddd`/`#000000`), cursor (`#cccccc`), cursor
text (`#111111`), selection foreground (`#000000`) and selection background
(`#fffacd`) match kitty's option defaults and are unit tested. `06_cursor`
remains 0.953% tolerant, entirely antialias-edge noise.

### 6. Pane borders and background gaps

`kitty_render::pane_border_rects()` implements kitty's minimal-border rule (only
edges shared with a neighbouring pane; each pane contributes its own strip, so a
split gets a 2x thickness gap) and full borders. The GUI emits them as a
transparent chrome overlay so per-pane scissor clears cannot erase them, using
`active_border_color`, `inactive_border_color` and `bell_border_color` with
`window_border_width` scaled to device pixels. Unit tests cover horizontal and
vertical splits, single-pane suppression and full borders. The cell-grid layout
does not yet reserve fractional border pixels or `window_margin_width`, so split
geometry is approximate and no split is exercised by this screenshot corpus
(`11_layouts` is skipped).

### 7. Tabs and layouts

`11_layouts` is skipped because kitr's session parser does not implement
`launch --location`, so split panes cannot be driven identically. Tabs are
covered by `10_tabs` subject to the C session-path caveat above; layout state
restoration is not visually tested.

### 8. Other untested surfaces

Window decorations/titlebar, graphics protocol, hyperlinks, undercurl and
coloured underlines, ligatures beyond the ASCII corpus, IME/compose input and
selection rendering are outside this screenshot corpus.

### 9. Erase, scroll and alternate screen

`07_erase` (1.089%), `08_scroll` (2.644%) and `09_alt_screen` (0.745%) show the
same picture: terminal semantics match, while glyph rasterisation and cursor
edges account for the remaining pixels.
