# Shader pipeline parity

`kitty-render` uses kitty's real Slang shader sources. The hand-written GLSL
subset that used to live in `src/gl_shaders.rs` has been removed.

## Generation

`scripts/generate_shaders.py` is a standalone port of the relevant parts of
`kitty/shaders/slang.py`:

* imports/modules are compiled to `.slang-module` in dependency order;
* cell defines (`MARK_MASK`, bit shifts, colour-type constants, …) are passed
  exactly as `SlangFile.defines` does;
* specialisations are written as `export static const … = value;` modules and
  linked into the entry-point compile, matching `create_specialisations()`;
* each entry point is compiled with
  `slangc -target glsl -profile glsl_150 -entry … -stage …`;
* `fixup_opengl_code()` is ported: explicit fragment-output locations, bindings
  and push constants are removed, std430 push constants become loose uniforms,
  std140 blocks stay UBOs, `GammaLUT` becomes a loose uniform and Vulkan
  builtins (`gl_VertexIndex`, `gl_BaseVertex`, …) are replaced.

Generated files are checked in under `src/shaders/generated/` together with a
Rust manifest. Rebuild them with:

```sh
SLANGC=/path/to/slangc scripts/generate_shaders.py --compile
```

A normal `cargo build` only uses `include_str!` and never invokes `slangc`.

## Program catalogue

The numeric ids match the enum at the top of `kitty/shaders.c`:

| id | program | generated variant(s) |
| ---: | --- | --- |
| 0 | `CELL_PROGRAM` | 27 cell variants |
| 1 | `CELL_FG_PROGRAM` | `RENDER_MODE=2` cell variants |
| 2 | `CELL_BG_PROGRAM` | `RENDER_MODE=1` cell variants |
| 3 | `BORDERS_PROGRAM` | `border` |
| 4 | `GRAPHICS_PROGRAM` | `graphics` default |
| 5 | `GRAPHICS_PREMULT_PROGRAM` | `graphics.premult` |
| 6 | `GRAPHICS_ALPHA_MASK_PROGRAM` | `graphics.alpha_mask` |
| 7 | `GRAPHICS_INDEXED_PROGRAM` | `indexed_graphics` default |
| 8 | `GRAPHICS_INDEXED_UNSCALED_PROGRAM` | `indexed_graphics.unscaled` |
| 9 | `BGIMAGE_PROGRAM` | `bgimage` |
| 10 | `TINT_PROGRAM` | `tint` |
| 11 | `TRAIL_PROGRAM` | `trail` |
| 12 | `BLIT_PROGRAM` | `blit` |
| 13 | `SCREENSHOT_PROGRAM` | `screenshot` |
| 14 | `ROUNDED_RECT_PROGRAM` | `rounded_rect` |
| 15 | `PADDING_PROGRAM` | `padding` |
| 16 | `CUSTOM_END_PROGRAM` | compiled from user Slang |

Cell variants are identified with kitty's five-character MD5 variant names and
carry their `FG_OVERRIDE_ALGO`, `RENDER_MODE`, `TEXT_NEW_GAMMA` and
`TEXT_CONTRAST_IDENTITY` defines. Attribute locations (`colors=0`,
`sprite_idx=1`, `is_selected=2`) and UBO names (`CellRenderData`, `ColorTable`)
are preserved in the metadata.

## Reflection

`GeneratedProgram::uniform_reflection()` maps Slang names to the actual GLSL
names (`edges` → `edges_0`, `sprites` → `sprites_0`,
`gamma_lut` → `gamma_lut_0[0]`). `attribute_reflection()` and
`uniform_blocks()` expose the input locations and std140 block names.
`GlBackend::program_uniform()` uses the same metadata for `glGetUniformLocation`
lookups.

## Backend wiring

Currently drawn with the real generated programs:

| frame data | program |
| --- | --- |
| backgrounds, decorations, cursor fills (no radius) | `tint` |
| rounded cursor rectangles | `rounded_rect` |
| Gray8 glyphs | `graphics.alpha_mask` |
| RGBA glyphs | `graphics` |
| protocol/background/logo images | `graphics` (+ `tint` for background tint) |
| cursor trail | `trail` |
| indexed image uploads without a palette | `indexed_graphics` |

Compiled, reflected and available, but not selected by the current `Frame`
adapter:

* the full `cell`/`cell_fg`/`cell_bg` vertex + UBO path, because the CPU atlas
  packs arbitrary glyph rectangles into 2D pages instead of kitty's fixed-cell
  `GL_TEXTURE_2D_ARRAY` sprite grid with its decorations map;
* `border` and `padding`, because window borders and padding are emitted as
  ordinary solid instances instead of the cell UBO/sprite stream;
* `bgimage`, because background images use the explicit UV quad path (which
  also supports the source rectangle and tint semantics of `ImageInstance`);
* `blit`/`screenshot`, which need offscreen targets the current public frame
  API does not expose;
* kitty's multi-group animated custom-shader pipeline. `SlangCompiler` and
  `GlBackend::install_custom_shader*` compile and link a user shader; applying
  it as a post-process group is not wired.

Indexed protocol images with a live palette are currently expanded through that
palette on the CPU before upload, preserving the existing pixel tests; the
integer-texture path is used for uploads that do not carry a palette.

## Tests

`cargo test -p kitty-render --features gl --test gl` covers:

* compilation/linking of all 40 generated variants and all 16 logical programs;
* uniform, attribute and UBO reflection;
* background, decoration, glyph, image, rounded-cursor, trail and indexed-image
  pixel tests;
* the custom Slang shader compile + link path.

The C-kitty screenshot corpus (`scripts/screenshot_parity.py`) continues to
cover cell/background/decoration/graphics cases through the same frame API.
