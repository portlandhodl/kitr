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
  1px right/bottom border (801x457 for 80x24), while kitr's is 800x475 because
  kitr always reserves a 19px tab-bar strip.
- Text parity was spot-checked for `02_utf8`, `05_sgr` and `08_scroll`: C kitty
  `kitten @ get-text` and kitr `--dump-screen` produced the same cells (modulo a
  trailing newline). The pixel differences below are therefore rendering and
  chrome differences, not different screen contents.

### 1. Tab bar strip (raw vs aligned metrics)

kitr reserves and draws a tab-bar strip even when `tab_bar_style` is `hidden`;
C kitty hides it for a single-tab window. This accounts for the `(0,19)` offset
in every non-tab case and is why raw tolerant percentages are much worse than
aligned ones (for example `05_sgr`: 11.297% raw vs 3.538% aligned). In
`10_tabs` both bars exist and the offset is `(0,-1)`, but the bar styling still
differs (C kitty powerline vs kitr's own tab rendering), so tab-bar appearance is
not pixel parity.

### 2. Font rasterisation and antialias gamma

Cell metrics and line positions match (10x19 cells, baseline 15, all lines align
after the offset), and the sampled text content matched exactly. Even so, exact
pixel difference on ordinary text is about 1-2% because the FreeType
rasterisation/gamma differs: kitr produces darker intermediate antialias values
(`#262626`, `#232323`) where C kitty produces lighter edges (`#5f5f5f`,
`#636363`). Example: `01_ascii` is 1.503% exact / 1.193% tolerant after
alignment. This is the noise floor for text cases, not a behavioural failure.

### 3. UTF-8 fallback and shaping

`02_utf8` is 1.328% tolerant after alignment. Latin, Greek and Cyrillic match
well. The Arabic and Hebrew RTL lines and the emoji line are the largest
per-line contributors, consistent with fallback-font and shaping differences.
Emoji are monochrome in both screenshots with this font configuration; colour
emoji rendering is not exercised.

### 4. ANSI colour palette

C kitty's default 16-colour palette is not the same as kitr's xterm-style
palette. Examples seen in `05_sgr`:

| Slot | C kitty | kitr |
| --- | --- | --- |
| color1 / red | `#cc0403` | `#cd0000` |
| color2 / green | `#19cb00` | `#00cd00` |
| bright red | `#f2201f` | `#ff0000` |
| bright green | `#23fd00` | `#00ff00` |

Truecolor SGR and the sampled 256-colour blocks are closer, but standard and
bright 16-colour output is visibly different. `05_sgr` is the worst aligned case
at 3.538% tolerant (5.010% exact). The default background and foreground do
match (`#000000` / `#dddddd`).

### 5. Cursor

Both render a block cursor and the cursor cell aligns after the offset.
`06_cursor` is 0.953% tolerant; there is no gross shape or position mismatch,
only the same antialias-edge difference as text.

### 6. Scrollback

Only the visible viewport is compared (`08_scroll`, 2.644% tolerant). Scrollback
history, scrollbars, mouse-wheel scrolling, and scrollback behaviour while on
the alternate screen are not visually tested here.

### 7. Tabs and layouts

`10_tabs` passes at 3.704% tolerant with the same 8% default threshold.
`11_layouts` is skipped because kitr's session parser does not implement
`launch --location`, so split panes cannot be driven identically. Tabs are
covered; split layouts and layout state restoration are not.

### 8. Other untested surfaces

Window decorations/titlebar, graphics protocol, hyperlinks, undercurl and
coloured underlines, ligatures beyond the ASCII corpus, IME/compose input and
selection rendering are outside this screenshot corpus.

### 9. Erase, scroll and alternate screen

`07_erase` (2.149%), `08_scroll` (2.644%) and `09_alt_screen` (0.745%) show the
same picture: terminal semantics match, while glyph rasterisation, cursor edges
and the extra kitr tab strip account for the remaining pixels.
