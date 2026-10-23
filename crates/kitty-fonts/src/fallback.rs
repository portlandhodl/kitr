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
    symbol_map: Vec<SymbolMapEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SymbolMapEntry {
    start: u32,
    end: u32,
    family: String,
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
            symbol_map: Vec::new(),
        }
    }

    /// Load the first family in `spec` and enable lazy Fontconfig fallback.
    pub fn from_spec(spec: &FontSpec, style: FontStyle, size_pts: f64, dpi: f64) -> Result<Self> {
        let fc = Fontconfig::new()?;
        let matched = match_spec(&fc, spec, style, size_pts, dpi)?
            .ok_or(Error::Fontconfig("no font matched the specification"))?;
        let explicit_bold_or_italic = !spec.is_auto();
        if let Some(requested) = spec.primary_family() {
            if (style == FontStyle::Regular || explicit_bold_or_italic)
                && !family_matches(requested, &matched)
                && !face_name_matches(requested, &matched)
            {
                eprintln!(
                    "kitr: configured font family {requested:?} did not match; using {:?}",
                    matched.family.as_deref().unwrap_or("<unknown>")
                );
            }
        }
        let face = matched.load(size_pts, dpi, dpi)?;
        Ok(Self {
            faces: vec![face],
            fontconfig: Some(fc),
            style,
            prefer_color: false,
            failed_codepoints: HashSet::new(),
            symbol_map: Vec::new(),
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

    /// Install kitty's `symbol_map`: codepoint ranges that must be served by a
    /// named family instead of the normal Fontconfig fallback. Later entries
    /// override earlier ones, matching kitty's map merge order.
    pub fn set_symbol_map<I>(&mut self, entries: I)
    where
        I: IntoIterator<Item = (u32, u32, String)>,
    {
        self.symbol_map = entries
            .into_iter()
            .filter(|(start, end, family)| start <= end && !family.is_empty())
            .map(|(start, end, family)| SymbolMapEntry { start, end, family })
            .collect();
    }

    fn symbol_family_for(&self, codepoints: &[u32]) -> Option<String> {
        let first = *codepoints.first()?;
        self.symbol_map
            .iter()
            .rfind(|entry| entry.start <= first && first <= entry.end)
            .map(|entry| entry.family.clone())
    }

    fn face_for_symbol(&mut self, family: &str, codepoints: &[u32]) -> Result<Option<usize>> {
        let Some(fc) = self.fontconfig else {
            return Ok(None);
        };
        let Some(matched) = fc.match_family(family, self.style)? else {
            return Ok(None);
        };
        self.consider_fallback(&matched, codepoints)
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
        if let Some(family) = self.symbol_family_for(&[codepoint]) {
            if let Some(idx) = self.face_for_symbol(&family, &[codepoint])? {
                return Ok(Some(idx));
            }
        }
        if wants_color(&[codepoint]) {
            if let Some(idx) = self
                .faces
                .iter()
                .position(|f| f.has_color() && f.has_codepoint(codepoint))
            {
                return Ok(Some(idx));
            }
            if let Some(fc) = self.fontconfig {
                if let Some(matched) = fc.match_for_codepoint(codepoint, self.style, true)? {
                    if matched.color {
                        if let Some(idx) = self.consider_fallback(&matched, &[codepoint])? {
                            return Ok(Some(idx));
                        }
                    }
                }
            }
        }
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
        if let Some(family) = self.symbol_family_for(codepoints) {
            if let Some(idx) = self.face_for_symbol(&family, codepoints)? {
                return Ok(Some(idx));
            }
        }
        let covers = |f: &Face| codepoints.iter().all(|&cp| f.has_codepoint(cp));
        if wants_color(codepoints) {
            if let Some(idx) = self.faces.iter().position(|f| f.has_color() && covers(f)) {
                return Ok(Some(idx));
            }
            if let Some(fc) = self.fontconfig {
                if let Some(matched) =
                    fc.match_for_codepoints(codepoints.iter().copied(), self.style, true)?
                {
                    if matched.color {
                        if let Some(idx) = self.consider_fallback(&matched, codepoints)? {
                            return Ok(Some(idx));
                        }
                    }
                }
            }
        }
        if let Some(idx) = self.faces.iter().position(covers) {
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

fn family_key(family: &str) -> String {
    family
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn family_matches(requested: &str, matched: &MatchedFace) -> bool {
    let requested = family_key(requested);
    let generic = matches!(
        requested.as_str(),
        "monospace"
            | "sansserif"
            | "serif"
            | "cursive"
            | "fantasy"
            | "systemui"
            | "emoji"
            | "symbols"
    );
    if generic {
        return true;
    }
    matched
        .family
        .as_deref()
        .map(|family| {
            let family = family_key(family);
            family == requested || family.contains(&requested) || requested.contains(&family)
        })
        .unwrap_or(false)
}

/// kitty's `has_emoji_presentation()`: an emoji drawn two cells wide
/// (by default, or via VS16) and not followed by VS15 is drawn from a colour
/// font when one covers it.
fn wants_color(codepoints: &[u32]) -> bool {
    let Some(&first) = codepoints.first() else {
        return false;
    };
    if codepoints.get(1) == Some(&0xFE0E) {
        return false;
    }
    let props = kitty_unicode::char_props_for(first);
    props.is_emoji()
        && (kitty_unicode::wcwidth_std(props) == 2 || codepoints.get(1) == Some(&0xFE0F))
}

/// Match `name` as an exact full name or PostScript name.
fn match_face_name(fc: &Fontconfig, name: &str) -> Result<Option<MatchedFace>> {
    let query = crate::discovery::FontQuery::new().full_name(name);
    if let Some(m) = fc.match_font(&query)? {
        if face_name_matches(name, &m) {
            return Ok(Some(m));
        }
    }
    if let Some(m) = fc.match_postscript_name(name)? {
        if face_name_matches(name, &m) {
            return Ok(Some(m));
        }
    }
    Ok(None)
}

fn face_name_matches(requested: &str, matched: &MatchedFace) -> bool {
    let requested = family_key(requested);
    [&matched.full_name, &matched.postscript_name]
        .into_iter()
        .flatten()
        .any(|name| family_key(name) == requested)
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
        // Like kitty's `find_best_match()`, a plain name may also be a face's
        // full name ("JetBrainsMono NF Bold") or PostScript name.
        if let Some(m) = match_face_name(fc, family)? {
            return Ok(Some(m));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squashed_and_spaced_family_names_match() {
        let matched = MatchedFace {
            family: Some("JetBrainsMono Nerd Font".into()),
            ..MatchedFace::default()
        };
        assert!(family_matches("JetBrainsMonoNerdFont", &matched));
        assert!(family_matches("jetbrainsmononerdfont", &matched));
        assert!(family_matches("JetBrainsMono Nerd Font", &matched));
        assert!(!family_matches("Some Other Font", &matched));
    }

    #[test]
    fn emoji_presentation_wants_color() {
        assert!(wants_color(&[0x1F600]));
        assert!(!wants_color(&[0x1F600, 0xFE0E]));
        assert!(wants_color(&[0x2764, 0xFE0F]));
        assert!(!wants_color(&[0x2764]));
        assert!(!wants_color(&['a' as u32]));
    }

    #[test]
    fn full_and_postscript_names_match() {
        let matched = MatchedFace {
            family: Some("JetBrainsMono Nerd Font".into()),
            full_name: Some("JetBrainsMono NF Bold".into()),
            postscript_name: Some("JetBrainsMonoNF-Bold".into()),
            ..MatchedFace::default()
        };
        assert!(face_name_matches("JetBrainsMono NF Bold", &matched));
        assert!(face_name_matches("jetbrainsmononf-bold", &matched));
        assert!(!face_name_matches("JetBrainsMono NF Italic", &matched));
    }

    #[test]
    fn symbol_map_uses_the_last_matching_range() {
        let mut stack = FontStack::new(Vec::new());
        stack.set_symbol_map([
            (0xE000, 0xE0FF, "First".to_string()),
            (0xE0A0, 0xE0A3, "Second".to_string()),
        ]);
        assert_eq!(
            stack.symbol_family_for(&[0xE0A1]).as_deref(),
            Some("Second")
        );
        assert_eq!(stack.symbol_family_for(&[0xE050]).as_deref(), Some("First"));
        assert_eq!(stack.symbol_family_for(&[0x41]), None);
    }
}
