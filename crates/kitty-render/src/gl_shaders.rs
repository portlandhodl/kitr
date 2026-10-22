// SPDX-License-Identifier: GPL-3.0-or-later
//! Embedded shaders for the OpenGL backend.
//!
//! **These are a functional rendering subset, not a line-for-line port of
//! `kitty/shaders/*.slang`.** `slangc` is not available in this build, and the
//! kitty shaders depend on the full GPU cell/vertex layout, sprite atlas
//! indirection, graphics-protocol layers and a large uniform block. The
//! programs below cover the subset the CPU core produces:
//!
//! * [`GLYPH_VERTEX_330`] / [`GLYPH_FRAGMENT_330`] — instanced glyph quads
//!   sampling a Gray8 (`GL_R8`) or RGBA8 (`GL_RGBA8`) atlas page, multiplied
//!   by a per-instance foreground colour and alpha;
//! * [`SOLID_VERTEX_330`] / [`SOLID_FRAGMENT_330`] — instanced solid quads for
//!   backgrounds, underline/strikethrough rectangles and cursor rectangles,
//!   with an optional rounded-rectangle SDF (anti-aliased with `fwidth`);
//! * [`IMAGE_VERTEX_330`] / [`IMAGE_FRAGMENT_330`] — instanced textured quads
//!   for graphics-protocol images, background images and window logos, with
//!   background tinting, opacity and tiled UVs;
//! * an optional per-fragment gamma mode: `0` writes colours unchanged, `1`
//!   converts sRGB input to linear light, `2` encodes linear input as sRGB.
//!
//! All colour output is **premultiplied**, because the backend uses
//! `glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA)`.
//!
//! # GLSL dialects
//!
//! The canonical sources are GLSL 330 core. [`ShaderDialect::Glsl300Es`]
//! produces the OpenGL ES 3.00 variant by replacing the version directive and
//! adding precision qualifiers; the rest of the source is shared.
//!
//! # Not implemented here
//!
//! Animated/trails-specific shaders, selection/hyperlink overlays, animations,
//! custom user shaders, cell shaders, linear-to-sRGB framebuffer resolves and
//! the sprite atlas' multiple-format lookup table. See the crate level
//! documentation.

use crate::egl::GlApi;

/// Canonical vertex shader for instanced glyph quads (GLSL 330 core).
pub const GLYPH_VERTEX_330: &str = r#"#version 330 core
layout(location = 0) in vec2 a_unit;
layout(location = 1) in vec4 a_rect;
layout(location = 2) in vec4 a_uv;
layout(location = 3) in vec4 a_color;
layout(location = 4) in vec4 a_params;
uniform vec2 u_viewport;
out vec2 v_uv;
out vec4 v_color;
void main() {
    vec2 pos = a_rect.xy + a_unit * a_rect.zw;
    vec2 ndc = vec2(pos.x / u_viewport.x * 2.0 - 1.0,
                    1.0 - pos.y / u_viewport.y * 2.0);
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_uv = mix(a_uv.xy, a_uv.zw, a_unit);
    v_color = a_color;
}
"#;

/// Canonical fragment shader for instanced glyph quads (GLSL 330 core).
///
/// `u_is_color == 0` treats the atlas as a Gray8 coverage mask; otherwise the
/// atlas page is RGBA8 colour glyph data.
pub const GLYPH_FRAGMENT_330: &str = r#"#version 330 core
in vec2 v_uv;
in vec4 v_color;
uniform sampler2D u_atlas;
uniform int u_is_color;
uniform int u_gamma;
out vec4 fragColor;

vec3 srgb_to_linear(vec3 c) {
    vec3 lo = c / 12.92;
    vec3 hi = pow((c + 0.055) / 1.055, vec3(2.4));
    return mix(hi, lo, lessThanEqual(c, vec3(0.04045)));
}

vec3 linear_to_srgb(vec3 c) {
    vec3 lo = c * 12.92;
    vec3 hi = 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055;
    return mix(hi, lo, lessThanEqual(c, vec3(0.0031308)));
}

void main() {
    vec4 texel = texture(u_atlas, v_uv);
    vec3 rgb = v_color.rgb;
    float alpha = v_color.a;
    if (u_is_color != 0) {
        rgb *= texel.rgb;
        alpha *= texel.a;
    } else {
        alpha *= texel.r;
    }
    if (u_gamma == 1) {
        rgb = srgb_to_linear(rgb);
    } else if (u_gamma == 2) {
        rgb = linear_to_srgb(rgb);
    }
    fragColor = vec4(rgb * alpha, alpha);
}
"#;

/// Canonical vertex shader for instanced solid/rounded quads (GLSL 330 core).
pub const SOLID_VERTEX_330: &str = r#"#version 330 core
layout(location = 0) in vec2 a_unit;
layout(location = 1) in vec4 a_rect;
layout(location = 2) in vec4 a_uv;
layout(location = 3) in vec4 a_color;
layout(location = 4) in vec4 a_params;
uniform vec2 u_viewport;
out vec4 v_color;
out vec2 v_local;
out vec2 v_size;
flat out float v_radius;
void main() {
    vec2 pos = a_rect.xy + a_unit * a_rect.zw;
    vec2 ndc = vec2(pos.x / u_viewport.x * 2.0 - 1.0,
                    1.0 - pos.y / u_viewport.y * 2.0);
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_color = a_color;
    v_local = a_unit * a_rect.zw;
    v_size = a_rect.zw;
    v_radius = a_params.x;
}
"#;

/// Canonical fragment shader for solid and rounded-rectangle quads.
///
/// A positive `a_params.x` selects the rounded-rectangle SDF; zero gives an
/// axis-aligned solid rectangle. The SDF uses the local pixel coordinate, so
/// it is resolution independent and antialiases with one-pixel derivatives.
pub const SOLID_FRAGMENT_330: &str = r#"#version 330 core
in vec4 v_color;
in vec2 v_local;
in vec2 v_size;
flat in float v_radius;
uniform int u_gamma;
out vec4 fragColor;

vec3 srgb_to_linear(vec3 c) {
    vec3 lo = c / 12.92;
    vec3 hi = pow((c + 0.055) / 1.055, vec3(2.4));
    return mix(hi, lo, lessThanEqual(c, vec3(0.04045)));
}

vec3 linear_to_srgb(vec3 c) {
    vec3 lo = c * 12.92;
    vec3 hi = 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055;
    return mix(hi, lo, lessThanEqual(c, vec3(0.0031308)));
}

float rounded_box(vec2 p, vec2 half_size, float radius) {
    vec2 q = abs(p - half_size) - (half_size - vec2(radius));
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

void main() {
    float coverage = 1.0;
    if (v_radius > 0.0 && v_size.x > 0.0 && v_size.y > 0.0) {
        float radius = min(v_radius, min(v_size.x, v_size.y) * 0.5);
        float d = rounded_box(v_local, v_size * 0.5, radius);
        float width = max(fwidth(d), 0.0001);
        coverage = clamp(0.5 - d / width, 0.0, 1.0);
    }
    vec3 rgb = v_color.rgb;
    if (u_gamma == 1) {
        rgb = srgb_to_linear(rgb);
    } else if (u_gamma == 2) {
        rgb = linear_to_srgb(rgb);
    }
    float alpha = v_color.a * coverage;
    fragColor = vec4(rgb * alpha, alpha);
}
"#;

/// Canonical vertex shader for instanced image quads (GLSL 330 core).
///
/// The same per-instance layout as the solid/glyph programs is used. `a_uv`
/// carries the source rectangle in normalised texture coordinates and may
/// exceed `[0, 1]` for tiled background images; the sampler's wrap mode then
/// decides how it repeats.
pub const IMAGE_VERTEX_330: &str = r#"#version 330 core
layout(location = 0) in vec2 a_unit;
layout(location = 1) in vec4 a_rect;
layout(location = 2) in vec4 a_uv;
layout(location = 3) in vec4 a_color;
layout(location = 4) in vec4 a_params;
uniform vec2 u_viewport;
out vec2 v_uv;
out vec4 v_color;
out vec2 v_params;
void main() {
    vec2 pos = a_rect.xy + a_unit * a_rect.zw;
    vec2 ndc = vec2(pos.x / u_viewport.x * 2.0 - 1.0,
                    1.0 - pos.y / u_viewport.y * 2.0);
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_uv = mix(a_uv.xy, a_uv.zw, a_unit);
    v_color = a_color;
    v_params = a_params.xy;
}
"#;

/// Canonical fragment shader for instanced image quads (GLSL 330 core).
///
/// `v_params.x` mixes the sampled RGB towards `v_color.rgb` (the background
/// tint) and `v_params.y` is an extra alpha multiplier (opacity). Texture data
/// is non-premultiplied RGBA8; the result is premultiplied for the backend's
/// `GL_ONE, GL_ONE_MINUS_SRC_ALPHA` blend.
pub const IMAGE_FRAGMENT_330: &str = r#"#version 330 core
in vec2 v_uv;
in vec4 v_color;
in vec2 v_params;
uniform sampler2D u_image;
uniform int u_gamma;
out vec4 fragColor;

vec3 srgb_to_linear(vec3 c) {
    vec3 lo = c / 12.92;
    vec3 hi = pow((c + 0.055) / 1.055, vec3(2.4));
    return mix(hi, lo, lessThanEqual(c, vec3(0.04045)));
}

vec3 linear_to_srgb(vec3 c) {
    vec3 lo = c * 12.92;
    vec3 hi = 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055;
    return mix(hi, lo, lessThanEqual(c, vec3(0.0031308)));
}

void main() {
    vec4 texel = texture(u_image, v_uv);
    vec3 rgb = mix(texel.rgb, v_color.rgb, clamp(v_params.x, 0.0, 1.0));
    float alpha = texel.a * clamp(v_params.y, 0.0, 1.0);
    if (u_gamma == 1) {
        rgb = srgb_to_linear(rgb);
    } else if (u_gamma == 2) {
        rgb = linear_to_srgb(rgb);
    }
    fragColor = vec4(rgb * alpha, alpha);
}
"#;

/// A GLSL dialect the backend can target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaderDialect {
    /// Desktop OpenGL 3.3 core.
    Glsl330Core,
    /// OpenGL ES 3.00.
    Glsl300Es,
}

impl ShaderDialect {
    /// Pick a dialect for a client API.
    pub const fn for_api(api: GlApi) -> Self {
        match api {
            GlApi::OpenGl => Self::Glsl330Core,
            GlApi::OpenGlEs => Self::Glsl300Es,
        }
    }

    /// Whether this is the GLES variant.
    pub const fn is_gles(self) -> bool {
        matches!(self, Self::Glsl300Es)
    }

    /// Adapt a canonical 330 core source to this dialect.
    pub fn adapt(self, source: &str) -> String {
        match self {
            Self::Glsl330Core => source.to_owned(),
            Self::Glsl300Es => {
                let body = source.strip_prefix("#version 330 core\n").unwrap_or(source);
                format!("#version 300 es\nprecision highp float;\nprecision highp int;\n{body}")
            }
        }
    }
}

/// A vertex/fragment source pair ready for compilation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramSources {
    pub vertex: String,
    pub fragment: String,
}

/// Glyph program sources for `api`.
pub fn glyph_sources(api: GlApi) -> ProgramSources {
    let dialect = ShaderDialect::for_api(api);
    ProgramSources {
        vertex: dialect.adapt(GLYPH_VERTEX_330),
        fragment: dialect.adapt(GLYPH_FRAGMENT_330),
    }
}

/// Solid/rounded-rect program sources for `api`.
pub fn solid_sources(api: GlApi) -> ProgramSources {
    let dialect = ShaderDialect::for_api(api);
    ProgramSources {
        vertex: dialect.adapt(SOLID_VERTEX_330),
        fragment: dialect.adapt(SOLID_FRAGMENT_330),
    }
}

/// Image program sources for `api`.
pub fn image_sources(api: GlApi) -> ProgramSources {
    let dialect = ShaderDialect::for_api(api);
    ProgramSources {
        vertex: dialect.adapt(IMAGE_VERTEX_330),
        fragment: dialect.adapt(IMAGE_FRAGMENT_330),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gles_sources_have_gles_header_and_precision() {
        let sources = glyph_sources(GlApi::OpenGlEs);
        assert!(sources.vertex.starts_with("#version 300 es\n"));
        assert!(sources.vertex.contains("precision highp float;"));
        assert!(!sources.vertex.contains("#version 330 core"));
        assert!(sources.fragment.contains("texture(u_atlas"));
    }

    #[test]
    fn desktop_sources_are_unchanged() {
        let sources = solid_sources(GlApi::OpenGl);
        assert_eq!(sources.vertex, SOLID_VERTEX_330);
        assert_eq!(sources.fragment, SOLID_FRAGMENT_330);
    }
}
