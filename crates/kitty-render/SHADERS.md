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

Drawn with the real generated programs:

| frame data | program |
| --- | --- |
| backgrounds, decorations, cursor fills (no radius) | `tint` |
| rounded cursor rectangles | `rounded_rect` |
| Gray8 glyphs | `graphics.alpha_mask` |
| RGBA glyphs | `graphics` |
| protocol/logo images | `graphics`, `indexed_graphics` for integer textures |
| background images (opaque, representable layouts) | `bgimage` (+ `tint` for tint) |
| cursor trail | `trail` |
| fixed-cell grid (`set_cell_programs(true)`) | `cell`, `cell_bg`, `cell_fg` |
| linear offscreen layer → window | `blit` |
| scaled screenshots | `screenshot` |
| custom post-processing groups | compiled `CUSTOM_END_PROGRAM`-style pipeline |

### Cell programs and the sprite array

The CPU atlas keeps its arbitrary-rectangle shelf packing. When cell programs
are enabled, `cell.rs` adapts it to kitty's GPU representation:

* every distinct atlas slot is copied into a `cell_width` × `cell_height + 1`
  tile in a `GL_TEXTURE_2D_ARRAY`, indexed from 1 (index 0 is the empty sprite);
* a `GL_R32UI` decorations map gets one zero entry per sprite index; the
  existing decoration rectangles keep using the ordinary solid path;
* per-cell `colors`, `sprite_idx` and `is_selected` packed instance buffers feed
  the preserved attribute locations 0/1/2;
* `CellRenderData` is uploaded to binding 0 and the existing 272-entry
  `ColorTable` to binding 1, with kitty's std140 layout;
* the grid is rendered into an `RGBA16F` linear target and copied to the window
  through `blit`, which performs kitty's `linear2srgb` conversion.

Call `backend.set_cell_programs(true)` (or `render_cells_with_atlas`). Frames
that are not an origin-aligned plain grid, or that contain multi-cell glyphs,
fall back to the graphics path so shaped runs and wide characters remain
correct. `RenderStats::cell_programs` / `cell_instances` report the selection.

### Background images

`bgimage` is used for opaque backgrounds whose clipped source starts at UV
`(0, 0)` (tiling, mirrored, clamped, scaled and centred layouts). Its
`tiled`/`sizes`/`positions` inputs are derived from the background quad.
Transparent backgrounds (`opacity < 1`) and layouts that were clipped in both
axes keep the explicit UV quad path, because `bgimage.slang` has no separate
opacity input.

### Blit and screenshot

`GlBackend::blit_texture` runs `blit` for an arbitrary resident 2D texture.
`screenshot_region`/`take_screenshot` capture the current framebuffer and either
read it back 1:1 or let the `screenshot` program perform the
colour-space-correct downsample into an offscreen target. Both return top-down
RGBA8, matching `read_pixels`.

### Custom shader pipelines

`custom_shader.rs` ports kitty's pipeline definition, `.pipeline` parser and
animation state machine (start/toggle/stop events, durations, reverse, loops,
attach and per-group progress). `SlangCompiler::compile_custom_pipeline`
combines each group's shader sources, renames their `fragment_main` entry
points, injects a group dispatcher and compiles one program per pipeline.

`GlBackend::install_custom_pipeline` installs it; `render_custom_pipeline`
captures the backbuffer, runs active groups in order through ping-pong and
named (`a`, `b`, `persist`) RGBA targets, and draws the last active group's
linear output to the window with sRGB conversion. The wrapper converts the
initial sRGB backbuffer to linear and keeps intermediate targets linear.

Still deferred:

* `border` and `padding`, because chrome is emitted as ordinary solid
  instances rather than the cell UBO/sprite stream;
* the full `KittyCustomShaderData` payload and `pointer_map()` remapping (the
  pipeline uniform block is allocated and bound, but not populated);
* per-window background-image lists and image transforms beyond source-rect
  scaling.

Indexed protocol images with a live palette are currently expanded through that
palette on the CPU before upload, preserving the existing pixel tests; the
integer-texture path is used for uploads that do not carry a palette.

## Tests

`cargo test -p kitty-render --features gl --test gl` covers:

* compilation/linking of all 40 generated variants and all 16 logical programs;
* uniform, attribute and UBO reflection;
* background, decoration, glyph, image, rounded-cursor, trail, indexed-image,
  cell sprite-array, `bgimage`, `blit`/`screenshot` and custom pipeline pixel
  tests;
* the custom Slang shader compile + link path and animation timing/state tests
  in the CPU-only build.

The C-kitty screenshot corpus (`scripts/screenshot_parity.py`) continues to
cover cell/background/decoration/graphics cases through the same frame API.
