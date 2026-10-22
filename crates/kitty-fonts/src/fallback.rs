// SPDX-License-Identifier: GPL-3.0-or-later
//! Glyph lookup through a list of faces, with lazy Fontconfig fallback.
//!
//! This is the supported subset of kitty's `FontGroup`: the primary face is
//! followed by explicit fallback faces and, optionally, faces discovered on
//! demand by Fontconfig when a codepoint is missing. Unlike kitty there is no
//! box/blank font, symbol map, sprite atlas or Nerd Font built-in.

use std::collections::HashSet;

use crate::discovery::{Fontconfig, MatchedFace};
use crate::error::{Error, Result};
use crate::face::Face;
use crate::family::{FontSpec, FontStyle};
use crate::raster::{GlyphBitmap, RasterCache};
use crate::shaping::{ShapeOptions, ShapedRun, ShapedRunCache};

/// One shaped segment, produced because the face changed or a glyph was
/// missing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FallbackRun {
    pub face_index: usize,
    /// Index into the original UTF-32 run of the first codepoint.
    pub start: usize,
    pub run: ShapedRun,
}

/// Result of shaping text that may span several faces.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShapedText {
    pub runs: Vec<FallbackRun>,
    /// `(index, codepoint)` pairs for which no face had a glyph.
    pub missing: Vec<(usize, u32)>,
}

impl ShapedText {
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }

    pub fn num_glyphs(&self) -> usize {
        self.runs.iter().map(|r| r.run.glyphs.len()).sum()
    }
}

pub struct FontStack {
    faces: Vec<Face>,
    fontconfig: Option<Fontconfig>,
    style: FontStyle,
    prefer_color: bool,
    failed_codepoints: HashSet<u32>,
}

impl FontStack {
    /// A stack over already-open faces. No automatic fallback is performed.
    pub fn new(faces: Vec<Face>) -> Self {
        Self {
            faces,
            fontconfig: None,
            style: FontStyle::Regular,
            prefer_color: false,
            failed_codepoints: HashSet::new(),
        }
    }

    /// Load the first family in `spec` and enable lazy Fontconfig fallback.
    pub fn from_spec(spec: &FontSpec, style: FontStyle, size_pts: f64, dpi: f64) -> Result<Self> {
        let fc = Fontconfig::new()?;
        let matched = match_spec(&fc, spec, style, size_pts, dpi)?
            .ok_or(Error::Fontconfig("no font matched the specification"))?;
        let face = matched.load(size_pts, dpi, dpi)?;
        Ok(Self {
            faces: vec![face],
            fontconfig: Some(fc),
            style,
            prefer_color: false,
            failed_codepoints: HashSet::new(),
        })
    }

    pub fn from_family(
        family: impl AsRef<str>,
        style: FontStyle,
        size_pts: f64,
        dpi: f64,
    ) -> Result<Self> {
        Self::from_spec(
            &FontSpec {
                system: Some(family.as_ref().to_string()),
                ..FontSpec::default()
            },
            style,
            size_pts,
            dpi,
        )
    }

    pub fn with_prefer_color(mut self, yes: bool) -> Self {
        self.prefer_color = yes;
        self
    }

    /// Disable lazy Fontconfig fallback (useful for deterministic tests).
    pub fn without_fontconfig(mut self) -> Self {
        self.fontconfig = None;
        self
    }

    pub fn faces(&self) -> &[Face] {
        &self.faces
    }

    pub fn primary_face(&self) -> &Face {
        &self.faces[0]
    }

    pub fn style(&self) -> FontStyle {
        self.style
    }

    pub fn add_face(&mut self, face: Face) -> usize {
        if let Some(idx) = self
            .faces
            .iter()
            .position(|f| f.path() == face.path() && f.index() == face.index())
        {
            return idx;
        }
        self.faces.push(face);
        self.faces.len() - 1
    }

    /// Index of the first face containing `codepoint`, loading a Fontconfig
    /// fallback if necessary.
    pub fn face_index_for_char(&mut self, codepoint: u32) -> Result<Option<usize>> {
        if let Some(idx) = self.faces.iter().position(|f| f.has_codepoint(codepoint)) {
            return Ok(Some(idx));
        }
        if self.failed_codepoints.contains(&codepoint) {
            return Ok(None);
        }
        if let Some(fc) = self.fontconfig {
            if let Some(matched) =
                fc.match_for_codepoint(codepoint, self.style, self.prefer_color)?
            {
                if let Some(idx) = self.consider_fallback(&matched, &[codepoint])? {
                    return Ok(Some(idx));
                }
            }
        }
        self.failed_codepoints.insert(codepoint);
        Ok(None)
    }

    /// Index of a face containing every codepoint in `codepoints`.
    pub fn face_index_for_chars(&mut self, codepoints: &[u32]) -> Result<Option<usize>> {
        if let Some(idx) = self
            .faces
            .iter()
            .position(|f| codepoints.iter().all(|&cp| f.has_codepoint(cp)))
        {
            return Ok(Some(idx));
        }
        if let Some(fc) = self.fontconfig {
            if let Some(matched) =
                fc.match_for_codepoints(codepoints.iter().copied(), self.style, self.prefer_color)?
            {
                return self.consider_fallback(&matched, codepoints);
            }
        }
        Ok(None)
    }

    fn consider_fallback(
        &mut self,
        matched: &MatchedFace,
        codepoints: &[u32],
    ) -> Result<Option<usize>> {
        if matched.path.is_empty() {
            return Ok(None);
        }
        if let Some(idx) = self.faces.iter().position(|f| {
            f.path() == std::path::Path::new(&matched.path) && f.index() == matched.index
        }) {
            if codepoints
                .iter()
                .all(|&cp| self.faces[idx].has_codepoint(cp))
            {
                return Ok(Some(idx));
            }
            return Ok(None);
        }
        let face = matched.load(
            self.faces[0].point_size(),
            self.faces[0].dpi().0,
            self.faces[0].dpi().1,
        )?;
        if !codepoints.iter().all(|&cp| face.has_codepoint(cp)) {
            return Ok(None);
        }
        Ok(Some(self.add_face(face)))
    }

    /// Rasterize a codepoint through the stack, loading a fallback face if
    /// necessary. Returns the face index and bitmap, or `None` if no face
    /// covers the codepoint.
    pub fn rasterize_codepoint(&mut self, codepoint: u32) -> Result<Option<(usize, GlyphBitmap)>> {
        let Some(face_index) = self.face_index_for_char(codepoint)? else {
            return Ok(None);
        };
        let bitmap = self.faces[face_index].rasterize_codepoint(codepoint)?;
        Ok(Some((face_index, bitmap)))
    }

    /// Cached variant of [`FontStack::rasterize_codepoint`].
    pub fn rasterize_codepoint_cached(
        &mut self,
        cache: &mut RasterCache,
        codepoint: u32,
    ) -> Result<Option<(usize, GlyphBitmap)>> {
        let Some(face_index) = self.face_index_for_char(codepoint)? else {
            return Ok(None);
        };
        let bitmap = cache.rasterize_codepoint(&self.faces[face_index], codepoint)?;
        Ok(Some((face_index, bitmap)))
    }

    /// Shape text, splitting at face boundaries. Ligatures cannot span two
    /// faces; this is the main gap compared with kitty's cell-oriented run
    /// grouping.
    pub fn shape(&mut self, text: &[u32], options: &ShapeOptions) -> Result<ShapedText> {
        self.shape_impl(text, options, None)
    }

    /// Like [`FontStack::shape`], using a shaped-run cache for each segment.
    pub fn shape_cached(
        &mut self,
        cache: &mut ShapedRunCache,
        text: &[u32],
        options: &ShapeOptions,
    ) -> Result<ShapedText> {
        self.shape_impl(text, options, Some(cache))
    }

    fn shape_impl(
        &mut self,
        text: &[u32],
        options: &ShapeOptions,
        mut cache: Option<&mut ShapedRunCache>,
    ) -> Result<ShapedText> {
        let mut ans = ShapedText::default();
        if text.is_empty() {
            return Ok(ans);
        }
        let mut segmented: Vec<(usize, usize, usize)> = Vec::new();
        let mut current: Option<(usize, usize)> = None;
        for (i, &cp) in text.iter().enumerate() {
            match self.face_index_for_char(cp)? {
                Some(face_idx) => match current {
                    Some((idx, _start)) if idx == face_idx => {}
                    Some((idx, start)) => {
                        segmented.push((idx, start, i));
                        current = Some((face_idx, i));
                    }
                    None => current = Some((face_idx, i)),
                },
                None => {
                    if let Some((idx, start)) = current.take() {
                        segmented.push((idx, start, i));
                    }
                    ans.missing.push((i, cp));
                }
            }
        }
        if let Some((idx, start)) = current {
            segmented.push((idx, start, text.len()));
        }

        for (face_idx, start, end) in segmented {
            let run = match cache.as_deref_mut() {
                Some(cache) => cache.shape(&self.faces[face_idx], &text[start..end], options)?,
                None => self.faces[face_idx].shape(&text[start..end], options)?,
            };
            ans.runs.push(FallbackRun {
                face_index: face_idx,
                start,
                run,
            });
        }
        Ok(ans)
    }
}

fn family_matches(requested: &str, matched: &MatchedFace) -> bool {
    let requested = requested.trim().to_lowercase();
    let generic = matches!(
        requested.as_str(),
        "monospace" | "sans-serif" | "serif" | "cursive" | "fantasy" | "system-ui" | "emoji"
    );
    if generic {
        return true;
    }
    matched
        .family
        .as_deref()
        .map(|family| {
            let family = family.to_lowercase();
            family == requested || family.contains(&requested) || requested.contains(&family)
        })
        .unwrap_or(false)
}

fn match_spec(
    fc: &Fontconfig,
    spec: &FontSpec,
    style: FontStyle,
    _size_pts: f64,
    _dpi: f64,
) -> Result<Option<MatchedFace>> {
    if let Some(name) = &spec.postscript_name {
        return fc.match_postscript_name(name);
    }
    if let Some(name) = &spec.full_name {
        let query = crate::discovery::FontQuery::new()
            .full_name(name.clone())
            .style(style);
        return fc.match_font(&query);
    }
    let mut best = None;
    for family in spec.families() {
        if let Some(m) = fc.match_family(family, style)? {
            if family_matches(family, &m) {
                return Ok(Some(m));
            }
            if best.is_none() {
                best = Some(m);
            }
        }
    }
    if best.is_some() {
        // Fontconfig substituted a different family. Prefer the configured
        // monospace default over an arbitrary text face.
        if let Some(m) = fc.default_monospace(style)? {
            return Ok(Some(m));
        }
    }
    Ok(best)
}
