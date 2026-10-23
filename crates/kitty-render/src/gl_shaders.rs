// SPDX-License-Identifier: GPL-3.0-or-later
//! Compatibility shims over the real kitty shader pipeline.
//!
//! Historically this module contained a hand-written GLSL subset. It is now a
//! thin view over [`crate::shaders`]: every source comes from the Slang files
//! in `kitty/shaders`, compiled and fixed up by `scripts/generate_shaders.py`.
//!
//! The old recipe functions are kept because they are a convenient way for
//! callers to obtain the program kitty uses for a particular draw:
//!
//! * glyphs: `graphics.slang` with `is_alpha_mask=true` for Gray8 atlas pages
//!   and the default graphics program for RGBA glyphs;
//! * solids: `tint.slang` (and `rounded_rect.slang` for rounded cursors);
//! * images: `graphics.slang` (non-premultiplied source textures).

use crate::egl::GlApi;
pub use crate::shaders::{ProgramSources, ShaderDialect};

fn sources(kind: crate::shaders::ProgramKind, api: GlApi) -> ProgramSources {
    let program = crate::shaders::default_variant(kind)
        .unwrap_or_else(|| panic!("missing generated {kind:?} shader"));
    ProgramSources::for_api(program, ShaderDialect::for_api(api))
}

/// Real kitty glyph program sources (alpha-mask graphics pass).
pub fn glyph_sources(api: GlApi) -> ProgramSources {
    sources(crate::shaders::ProgramKind::GraphicsAlphaMask, api)
}

/// Real kitty solid-colour program sources (tint pass).
pub fn solid_sources(api: GlApi) -> ProgramSources {
    sources(crate::shaders::ProgramKind::Tint, api)
}

/// Real kitty image program sources (graphics pass).
pub fn image_sources(api: GlApi) -> ProgramSources {
    sources(crate::shaders::ProgramKind::Graphics, api)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shims_return_generated_kitty_programs() {
        let glyph = glyph_sources(GlApi::OpenGl);
        assert!(glyph.vertex.starts_with("#version 330 core"));
        assert!(glyph.fragment.contains("amask_fg"));
        let solid = solid_sources(GlApi::OpenGl);
        assert!(solid.fragment.contains("tint_color"));
        let image = image_sources(GlApi::OpenGl);
        assert!(image.fragment.contains("texture("));
    }

    #[test]
    fn gles_sources_have_gles_header_and_precision() {
        let sources = glyph_sources(GlApi::OpenGlEs);
        assert!(sources.vertex.starts_with("#version 300 es\n"));
        assert!(sources.vertex.contains("precision highp float;"));
    }
}
