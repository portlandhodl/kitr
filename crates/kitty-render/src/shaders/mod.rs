// SPDX-License-Identifier: GPL-3.0-or-later
//! Kitty's real Slang shader pipeline.
//!
//! The GLSL in [`generated`] is produced by `scripts/generate_shaders.py` from
//! the Slang sources in `kitty/shaders`. That script ports the important parts
//! of `kitty/shaders/slang.py`: program definitions, preprocessor defines,
//! specialisations/variants, `slangc` invocation, the OpenGL fixups and the
//! uniform/attribute metadata emitted by the compiler.
//!
//! The generated sources are checked in, so building kitty-render does not
//! require `slangc`. Regenerate them with:
//!
//! ```text
//! scripts/generate_shaders.py --compile
//! ```
//!
//! The module itself has no OpenGL dependency, which makes variant selection,
//! metadata reflection and the custom-shader compiler testable without a GPU.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub mod generated;

pub use generated::BUILTIN_PROGRAMS;

/// Numeric program ids, matching the enum at the top of `kitty/shaders.c`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ProgramKind {
    Cell = 0,
    CellFg = 1,
    CellBg = 2,
    Borders = 3,
    Graphics = 4,
    GraphicsPremult = 5,
    GraphicsAlphaMask = 6,
    GraphicsIndexed = 7,
    GraphicsIndexedUnscaled = 8,
    BgImage = 9,
    Tint = 10,
    Trail = 11,
    Blit = 12,
    Screenshot = 13,
    RoundedRect = 14,
    Padding = 15,
    CustomEnd = 16,
}

impl ProgramKind {
    pub const ALL: &'static [Self] = &[
        Self::Cell,
        Self::CellFg,
        Self::CellBg,
        Self::Borders,
        Self::Graphics,
        Self::GraphicsPremult,
        Self::GraphicsAlphaMask,
        Self::GraphicsIndexed,
        Self::GraphicsIndexedUnscaled,
        Self::BgImage,
        Self::Tint,
        Self::Trail,
        Self::Blit,
        Self::Screenshot,
        Self::RoundedRect,
        Self::Padding,
        Self::CustomEnd,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Cell),
            1 => Some(Self::CellFg),
            2 => Some(Self::CellBg),
            3 => Some(Self::Borders),
            4 => Some(Self::Graphics),
            5 => Some(Self::GraphicsPremult),
            6 => Some(Self::GraphicsAlphaMask),
            7 => Some(Self::GraphicsIndexed),
            8 => Some(Self::GraphicsIndexedUnscaled),
            9 => Some(Self::BgImage),
            10 => Some(Self::Tint),
            11 => Some(Self::Trail),
            12 => Some(Self::Blit),
            13 => Some(Self::Screenshot),
            14 => Some(Self::RoundedRect),
            15 => Some(Self::Padding),
            16 => Some(Self::CustomEnd),
            _ => None,
        }
    }

    /// Name used by kitty's `shaders.c` and the generated manifest.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cell | Self::CellFg | Self::CellBg => "cell",
            Self::Borders => "border",
            Self::Graphics => "graphics",
            Self::GraphicsPremult => "graphics",
            Self::GraphicsAlphaMask => "graphics",
            Self::GraphicsIndexed => "indexed_graphics",
            Self::GraphicsIndexedUnscaled => "indexed_graphics",
            Self::BgImage => "bgimage",
            Self::Tint => "tint",
            Self::Trail => "trail",
            Self::Blit => "blit",
            Self::Screenshot => "screenshot",
            Self::RoundedRect => "rounded_rect",
            Self::Padding => "padding",
            Self::CustomEnd => "custom_end",
        }
    }

    /// The kitty `RENDER_MODE` used by the cell programs (0 = both passes,
    /// 1 = background only, 2 = foreground only).
    pub const fn render_mode(self) -> Option<&'static str> {
        match self {
            Self::Cell => Some("0"),
            Self::CellBg => Some("1"),
            Self::CellFg => Some("2"),
            _ => None,
        }
    }

    const fn variant_hint(self) -> Option<&'static str> {
        match self {
            Self::GraphicsPremult => Some("premult"),
            Self::GraphicsAlphaMask => Some("alpha_mask"),
            Self::GraphicsIndexedUnscaled => Some("unscaled"),
            _ => None,
        }
    }
}

impl fmt::Display for ProgramKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Metadata as emitted by `slang.py` after fixing up a program.
#[derive(Clone, Copy, Debug)]
pub struct GeneratedMetadata {
    /// Slang name -> actual GLSL uniform name (e.g. `edges` -> `edges_0`).
    pub loose_uniforms: &'static [(&'static str, &'static str)],
    /// Slang UBO name -> (member -> actual GLSL member name).
    pub uniform_structs: &'static [(&'static str, &'static [(&'static str, &'static str)])],
    /// Slang attribute name -> explicit location.
    pub input_locations: &'static [(&'static str, &'static str)],
    /// Slang UBO name -> actual GLSL block name.
    pub uniform_struct_names: &'static [(&'static str, &'static str)],
}

/// One generated program variant.
#[derive(Clone, Copy, Debug)]
pub struct GeneratedProgram {
    pub program: &'static str,
    pub variant: &'static str,
    pub defines: &'static [(&'static str, &'static str)],
    pub vertex: &'static str,
    pub fragment: &'static str,
    pub metadata: GeneratedMetadata,
}

impl GeneratedProgram {
    /// Look up a uniform by its Slang name, falling back to the name itself.
    pub fn uniform_name(&self, name: &str) -> Option<&'static str> {
        self.metadata
            .loose_uniforms
            .iter()
            .find(|(slang, _)| *slang == name)
            .map(|(_, glsl)| *glsl)
            .or_else(|| {
                self.metadata
                    .uniform_structs
                    .iter()
                    .flat_map(|(_, members)| members.iter())
                    .find(|(slang, _)| *slang == name)
                    .map(|(_, glsl)| *glsl)
            })
    }

    /// All program names referenced by the metadata, mapped to their GLSL
    /// names. This is the reflection table used by the backend.
    pub fn uniform_reflection(&self) -> BTreeMap<String, String> {
        let mut ans = BTreeMap::new();
        for (slang, glsl) in self.metadata.loose_uniforms {
            ans.insert((*slang).to_owned(), (*glsl).to_owned());
        }
        for (struct_name, members) in self.metadata.uniform_structs {
            for (slang, glsl) in *members {
                ans.insert(format!("{struct_name}.{slang}"), (*glsl).to_owned());
            }
        }
        ans
    }

    /// Attribute locations recorded by the Slang fixup.
    pub fn attribute_reflection(&self) -> BTreeMap<String, u32> {
        self.metadata
            .input_locations
            .iter()
            .filter_map(|(name, location)| location.parse().ok().map(|v| ((*name).to_owned(), v)))
            .collect()
    }

    /// UBO names in Slang and GLSL form.
    pub fn uniform_blocks(&self) -> BTreeMap<String, String> {
        self.metadata
            .uniform_struct_names
            .iter()
            .map(|(slang, glsl)| ((*slang).to_owned(), (*glsl).to_owned()))
            .collect()
    }

    pub fn has_define(&self, key: &str, value: &str) -> bool {
        self.defines.iter().any(|(k, v)| *k == key && *v == value)
    }
}

/// All generated variants belonging to `kind`.
pub fn variants(kind: ProgramKind) -> impl Iterator<Item = &'static GeneratedProgram> {
    BUILTIN_PROGRAMS.iter().filter(move |program| {
        program.program == kind.name()
            && kind
                .variant_hint()
                .is_none_or(|hint| program.variant == hint)
            && (kind == ProgramKind::Cell
                || kind
                    .render_mode()
                    .is_none_or(|mode| program.has_define("RENDER_MODE", mode)))
    })
}

/// The variant used by default for `kind`.
pub fn default_variant(kind: ProgramKind) -> Option<&'static GeneratedProgram> {
    variants(kind).max_by_key(|program| {
        // Prefer the unnamed/default specialisation for graphics and the
        // kitty defaults for the three cell passes.
        let variant = u8::from(program.variant == "default");
        (variant, program.variant == "default")
    })
}

/// The `cell` variant matching a full set of Slang specialisation defines.
pub fn cell_variant(defines: &[(&str, &str)]) -> Option<&'static GeneratedProgram> {
    BUILTIN_PROGRAMS.iter().find(|program| {
        program.program == "cell"
            && program.defines.len() == defines.len()
            && defines
                .iter()
                .all(|(key, value)| program.has_define(key, value))
    })
}

/// Find a named variant (the name used in the generated file / manifest).
pub fn named_variant(kind: ProgramKind, variant: &str) -> Option<&'static GeneratedProgram> {
    variants(kind).find(|program| program.variant == variant)
}

/// A GLSL dialect the backend can target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaderDialect {
    Glsl330Core,
    Glsl300Es,
}

impl ShaderDialect {
    #[cfg(feature = "gl")]
    pub const fn for_api(api: crate::egl::GlApi) -> Self {
        match api {
            crate::egl::GlApi::OpenGl => Self::Glsl330Core,
            crate::egl::GlApi::OpenGlEs => Self::Glsl300Es,
        }
    }

    pub const fn is_gles(self) -> bool {
        matches!(self, Self::Glsl300Es)
    }

    /// Adapt a generated OpenGL GLSL 140 source to this dialect.
    pub fn adapt(self, source: &str) -> String {
        let mut body: String = source
            .lines()
            .filter(|line| {
                !line.starts_with("#extension ")
                    && !line.contains("GL_ARB_shader_draw_parameters")
                    && *line != "layout(row_major) buffer;"
                    && *line != "layout(push_constant)"
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !body.ends_with('\n') {
            body.push('\n');
        }
        match self {
            Self::Glsl330Core => {
                if let Some(rest) = body.strip_prefix("#version 140\n") {
                    format!("#version 330 core\n{rest}")
                } else if body.starts_with("#version ") {
                    let rest = body.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
                    format!("#version 330 core\n{rest}")
                } else {
                    format!("#version 330 core\n{body}")
                }
            }
            Self::Glsl300Es => {
                let rest = body.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
                format!(
                    "#version 300 es\nprecision highp float;\nprecision highp int;\nprecision highp sampler2D;\nprecision highp sampler2DArray;\nprecision highp usampler2D;\nprecision highp usampler2DArray;\n{rest}"
                )
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

impl ProgramSources {
    pub fn for_api(program: &GeneratedProgram, dialect: ShaderDialect) -> Self {
        Self {
            vertex: dialect.adapt(program.vertex),
            fragment: dialect.adapt(program.fragment),
        }
    }
}

/// Errors from the offline/custom Slang compiler.
#[derive(Debug)]
pub enum ShaderError {
    Io(String),
    SlangNotFound(PathBuf),
    Compile {
        program: String,
        log: String,
    },
    MissingVariant {
        program: ProgramKind,
        variant: &'static str,
    },
}

impl fmt::Display for ShaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "shader I/O error: {error}"),
            Self::SlangNotFound(path) => {
                write!(
                    f,
                    "slangc not found at {}; run scripts/build_c_kitty.sh or set SLANGC",
                    path.display()
                )
            }
            Self::Compile { program, log } => {
                write!(f, "failed to compile shader {program}: {log}")
            }
            Self::MissingVariant { program, variant } => {
                write!(f, "no generated variant {variant} for {program}")
            }
        }
    }
}

impl std::error::Error for ShaderError {}

/// Compiles user Slang shaders with the same `slangc` invocation as kitty.
#[derive(Clone, Debug)]
pub struct SlangCompiler {
    pub slangc: PathBuf,
    pub shader_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl SlangCompiler {
    /// Locate `slangc` and kitty's shader directory from the environment.
    ///
    /// `SLANGC` and `KITTY_SHADERS` override the defaults. If they are not set
    /// the compiler looks next to `CARGO_MANIFEST_DIR` (useful for tests).
    pub fn from_env() -> Result<Self, ShaderError> {
        let slangc = std::env::var_os("SLANGC")
            .map(PathBuf::from)
            .or_else(|| {
                if Path::new("/tmp/opencode/tools/bin/slangc").exists() {
                    Some(PathBuf::from("/tmp/opencode/tools/bin/slangc"))
                } else {
                    std::env::var_os("PATH").and_then(|path| {
                        std::env::split_paths(&path)
                            .map(|dir| dir.join("slangc"))
                            .find(|candidate| candidate.is_file())
                    })
                }
            })
            .ok_or_else(|| ShaderError::SlangNotFound(PathBuf::from("slangc")))?;
        let shader_dir = std::env::var_os("KITTY_SHADERS")
            .map(PathBuf::from)
            .or_else(|| {
                let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
                let candidate = crate_dir.join("../../../kitty/shaders");
                candidate.exists().then_some(candidate)
            })
            .ok_or_else(|| ShaderError::Io("set KITTY_SHADERS to kitty/shaders".into()))?;
        let cache_dir =
            std::env::temp_dir().join(format!("kitty-render-shaders-{}", std::process::id()));
        Ok(Self {
            slangc,
            shader_dir,
            cache_dir,
        })
    }

    /// Compile one user shader.
    ///
    /// The source must provide `vertex_main` and `fragment_main` entry points,
    /// exactly like the built-in `.slang` programs. Defines are passed as
    /// `-DNAME=VALUE`, which also makes this useful for testing kitty's
    /// specialisation mechanism.
    pub fn compile_custom_shader(
        &self,
        name: &str,
        source: &str,
        defines: &[(&str, &str)],
    ) -> Result<CustomShader, ShaderError> {
        if !self.slangc.is_file() {
            return Err(ShaderError::SlangNotFound(self.slangc.clone()));
        }
        std::fs::create_dir_all(&self.cache_dir)
            .map_err(|error| ShaderError::Io(error.to_string()))?;
        let source_path = self.cache_dir.join(format!("{name}.slang"));
        std::fs::write(&source_path, source).map_err(|error| ShaderError::Io(error.to_string()))?;
        let module_path = self.cache_dir.join(format!("{name}.slang-module"));
        let mut cmd = Command::new(&self.slangc);
        cmd.args(["-warnings-as-errors", "all"]);
        for (key, value) in defines {
            cmd.arg("-D");
            cmd.arg(format!("{key}={value}"));
        }
        cmd.args([
            "-I",
            self.cache_dir.to_string_lossy().as_ref(),
            "-I",
            self.shader_dir.to_string_lossy().as_ref(),
            "-target",
            "none",
            "-o",
            module_path.to_string_lossy().as_ref(),
            "--",
            source_path.to_string_lossy().as_ref(),
        ]);
        run_slang(&mut cmd, name)?;

        let mut stages = Vec::new();
        for (stage, entry) in [("vertex", "vertex_main"), ("fragment", "fragment_main")] {
            let output = self.cache_dir.join(format!(
                "{name}.{}.glsl",
                if stage == "vertex" { "vert" } else { "frag" }
            ));
            let mut cmd = Command::new(&self.slangc);
            cmd.args(["-warnings-as-errors", "all"]);
            for (key, value) in defines {
                cmd.arg("-D");
                cmd.arg(format!("{key}={value}"));
            }
            cmd.args([
                "-I",
                self.cache_dir.to_string_lossy().as_ref(),
                "-I",
                self.shader_dir.to_string_lossy().as_ref(),
                module_path.to_string_lossy().as_ref(),
                "-line-directive-mode",
                "none",
                "-target",
                "glsl",
                "-profile",
                "glsl_150",
                "-entry",
                entry,
                "-stage",
                stage,
                "-o",
                output.to_string_lossy().as_ref(),
            ]);
            run_slang(&mut cmd, name)?;
            let source = std::fs::read_to_string(&output)
                .map_err(|error| ShaderError::Io(error.to_string()))?;
            stages.push(source);
        }
        Ok(CustomShader {
            name: name.to_owned(),
            vertex: fixup_custom_glsl(&stages[0], false),
            fragment: fixup_custom_glsl(&stages[1], true),
            defines: defines
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
        })
    }
}

fn run_slang(cmd: &mut Command, name: &str) -> Result<(), ShaderError> {
    let output = cmd
        .output()
        .map_err(|error| ShaderError::Io(error.to_string()))?;
    if output.status.success() {
        return Ok(());
    }
    let mut log = String::from_utf8_lossy(&output.stderr).into_owned();
    log.push_str(&String::from_utf8_lossy(&output.stdout));
    Err(ShaderError::Compile {
        program: name.to_owned(),
        log,
    })
}

/// A compiled user shader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomShader {
    pub name: String,
    pub vertex: String,
    pub fragment: String,
    pub defines: BTreeMap<String, String>,
}

impl CustomShader {
    pub fn sources(&self, dialect: ShaderDialect) -> ProgramSources {
        ProgramSources {
            vertex: dialect.adapt(&self.vertex),
            fragment: dialect.adapt(&self.fragment),
        }
    }
}

/// Convert freshly compiled Slang GLSL. This is a Rust port of the
/// `fixup_opengl_code()` stage in `kitty/shaders/slang.py`: explicit locations
/// on fragment outputs and push constants are removed, std430 push constants
/// become loose uniforms and Vulkan-only builtins are replaced.
fn fixup_custom_glsl(source: &str, is_fragment: bool) -> String {
    let src_lines: Vec<&str> = source.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut in_block = false;
    let mut push_block = false;
    let mut push_block_name = String::new();
    let mut push_instance_name = String::new();
    for (index, raw) in src_lines.iter().enumerate() {
        let line = *raw;
        let next = src_lines.get(index + 1).copied().unwrap_or("");
        if in_block {
            if line.starts_with('}') {
                in_block = false;
                if push_block {
                    let instance = line.trim_start_matches('}').trim_end_matches(';').trim();
                    if !instance.is_empty() {
                        push_instance_name = instance.to_owned();
                    }
                    out.push(format!("// {}", line));
                } else {
                    out.push(line.to_owned());
                }
            } else if push_block {
                let trimmed = line.trim();
                if !trimmed.is_empty() && trimmed != "{" {
                    out.push(format!("uniform {trimmed}"));
                }
            } else {
                out.push(line.to_owned());
            }
        } else if line.starts_with("#version ") {
            out.push("#version 140".to_owned());
            if !is_fragment {
                out.push("#extension GL_ARB_explicit_attrib_location : require".to_owned());
            }
        } else if line.starts_with("#extension ")
            || line.starts_with("layout(push_constant)")
            || line.starts_with("layout(row_major) buffer")
            || line.starts_with("layout(binding =")
            || (line.starts_with("layout(location =") && (is_fragment || next.starts_with("out ")))
        {
            out.push(format!("// {line}"));
        } else if let Some(rest) = line.strip_prefix("flat layout(location =") {
            let _ = rest;
            out.push("flat".to_owned());
        } else if line.starts_with("layout(")
            && line.split_whitespace().any(|word| word == "uniform")
        {
            push_block = line.starts_with("layout(std430");
            if push_block {
                push_block_name = line.split_whitespace().last().unwrap_or("").to_owned();
                out.push(format!("// {line}"));
            } else {
                out.push(line.to_owned());
            }
            in_block = true;
        } else {
            out.push(line.to_owned());
        }
    }
    let mut ans = out.join("\n");
    ans.push('\n');
    if !push_block_name.is_empty() {
        ans = ans.replace(&format!("{push_block_name}."), "");
    }
    if !push_instance_name.is_empty() {
        ans = ans.replace(&format!("{push_instance_name}."), "");
    }
    for (from, to) in [
        ("gl_VertexIndex", "gl_VertexID"),
        ("gl_BaseVertex", "0"),
        ("gl_InstanceIndex", "gl_InstanceID"),
        ("gl_BaseInstance", "0"),
    ] {
        ans = ans.replace(from, to);
    }
    ans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_numeric_programs_are_present() {
        for (index, kind) in ProgramKind::ALL.iter().enumerate() {
            assert_eq!(kind.index(), index);
            assert_eq!(ProgramKind::from_index(index), Some(*kind));
        }
        assert_eq!(ProgramKind::from_index(17), None);
    }

    #[test]
    fn every_program_has_a_generated_variant() {
        for kind in ProgramKind::ALL {
            if *kind == ProgramKind::CustomEnd {
                continue;
            }
            let variant = default_variant(*kind)
                .unwrap_or_else(|| panic!("no generated variant for {kind:?}"));
            assert!(!variant.vertex.is_empty());
            assert!(!variant.fragment.is_empty());
            assert!(variant.vertex.contains("void main"));
        }
    }

    #[test]
    fn cell_variants_and_defines_match_kitty() {
        let all: Vec<_> = variants(ProgramKind::Cell).collect();
        assert_eq!(all.len(), 27);
        let default = default_variant(ProgramKind::Cell).unwrap();
        assert_eq!(default.defines.len(), 4);
        assert!(default.has_define("FG_OVERRIDE_ALGO", "0"));
        assert!(default.has_define("RENDER_MODE", "0"));
        assert!(default.has_define("TEXT_NEW_GAMMA", "true"));
        assert!(default.has_define("TEXT_CONTRAST_IDENTITY", "true"));
        let fg: Vec<_> = variants(ProgramKind::CellFg).collect();
        let bg: Vec<_> = variants(ProgramKind::CellBg).collect();
        assert_eq!(fg.len(), 9);
        assert_eq!(bg.len(), 9);
        assert!(fg.iter().all(|p| p.has_define("RENDER_MODE", "2")));
        assert!(bg.iter().all(|p| p.has_define("RENDER_MODE", "1")));
    }

    #[test]
    fn graphics_specialisations_are_present() {
        assert_eq!(variants(ProgramKind::Graphics).count(), 3);
        assert!(named_variant(ProgramKind::Graphics, "alpha_mask")
            .unwrap()
            .has_define("is_alpha_mask", "true"));
        assert!(named_variant(ProgramKind::Graphics, "premult")
            .unwrap()
            .has_define("texture_is_not_premultiplied", "false"));
        assert_eq!(variants(ProgramKind::GraphicsIndexed).count(), 2);
        assert!(named_variant(ProgramKind::GraphicsIndexed, "unscaled")
            .unwrap()
            .has_define("is_scaled", "false"));
    }

    #[test]
    fn metadata_reflects_uniforms_attributes_and_blocks() {
        let cell = default_variant(ProgramKind::Cell).unwrap();
        let uniforms = cell.uniform_reflection();
        assert_eq!(
            uniforms.get("sprites").map(String::as_str),
            Some("sprites_0")
        );
        assert_eq!(
            uniforms.get("gamma_lut").map(String::as_str),
            Some("gamma_lut_0[0]")
        );
        assert!(uniforms.contains_key("CellRenderData.columns"));
        let attributes = cell.attribute_reflection();
        assert_eq!(attributes.get("colors"), Some(&0));
        assert_eq!(attributes.get("sprite_idx"), Some(&1));
        assert_eq!(attributes.get("is_selected"), Some(&2));
        let blocks = cell.uniform_blocks();
        assert_eq!(
            blocks.get("CellRenderData").map(String::as_str),
            Some("block_CellRenderData_0")
        );
        assert_eq!(
            blocks.get("ColorTable").map(String::as_str),
            Some("block_ColorTable_0")
        );

        let graphics = default_variant(ProgramKind::GraphicsAlphaMask).unwrap();
        assert!(graphics.uniform_reflection().contains_key("image"));
        assert!(graphics.uniform_reflection().contains_key("amask_fg"));
    }

    #[test]
    fn dialect_adaptation() {
        let cell = default_variant(ProgramKind::Cell).unwrap();
        let desktop = ProgramSources::for_api(cell, ShaderDialect::Glsl330Core);
        assert!(desktop.vertex.starts_with("#version 330 core\n"));
        assert!(!desktop.vertex.contains("GL_ARB_shader_draw_parameters"));
        let es = ProgramSources::for_api(cell, ShaderDialect::Glsl300Es);
        assert!(es.vertex.starts_with("#version 300 es\n"));
        assert!(es.vertex.contains("precision highp float;"));
        assert!(!es.vertex.contains("#version 140"));
    }
}
