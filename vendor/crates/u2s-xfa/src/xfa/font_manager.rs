//! Font resolution (XFA 3.3 §17 "The font element", chapter 28 "Font
//! Mapping in LiveCycle Forms ES3").
//!
//! A `<font>` names a typeface, a weight and a posture. The faces this
//! process can draw with are registered once at boot (`crate::fonts`) from
//! their files, each known by what its own name table says: family,
//! typographic family, full name and PostScript name. A document adds its
//! own ([`DocumentFonts`]): the faces embedded in its PDF, and the
//! `equate`/`equateRange` substitutions of its configuration packet.
//!
//! Resolution, for one `<font>`:
//!
//! 1. a face the document embeds whose name is the typeface;
//! 2. the document's `equate` for the typeface (chapter 28, substitution
//!    step 1), then the same lookup for the typeface it names;
//! 3. a registered face whose name is the typeface -- among several, the
//!    one whose weight is closest to the requested one, upright for an
//!    upright request and italic for an italic one (step 2: a name match of
//!    any weight and posture beats a different typeface);
//! 4. the fallback face, and the substitution is reported once per
//!    typeface.
//!
//! Names are compared exactly, case-insensitively; nothing is inferred
//! from a typeface's spelling. A glyph the resolved face lacks is taken
//! from another face ([`FontManager::font_for_glyph`]: the document's
//! `equateRange` for that codepoint, then the symbol fallback, then any
//! face), which is chapter 28's font fallback.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use crate::xfa::{Font, FontPosture, FontWeight, XfaNode, XfaNodeKind};
use ab_glyph::FontRef;
use thiserror::Error;
use ttf_parser::{Face, name_id};

/// DejaVu Sans, the face a glyph is taken from when the text's own font
/// lacks it (ballot boxes, arrows, symbols): a face with wide Unicode
/// coverage that is part of the project, so the fallback does not depend on
/// what the machine has installed.
pub(crate) static SYMBOL_FALLBACK: &[u8] = include_bytes!("../../assets/DejaVuSans.ttf");

/// Font-related errors
#[derive(Debug, Error)]
pub enum FontError {
    /// The requested font was not found and strict mode allows no fallback.
    #[error("font not found: {typeface:?} (weight {weight:?}, posture {posture:?})")]
    FontNotFound {
        typeface: String,
        weight: FontWeight,
        posture: FontPosture,
    },

    /// No fallback font is registered.
    #[error("no fallback font is registered: register a font directory before rendering")]
    NoFallbackAvailable,

    /// The face's bytes could not be parsed.
    #[error("font data could not be parsed: {reason}")]
    FontParseError { reason: String },

    /// Lock error when accessing font manager
    #[error("font manager lock error: {0}")]
    LockError(String),
}

/// A face as this crate knows it: its bytes, and what its name table says
/// it is called.
#[derive(Clone)]
pub struct RegisteredFace {
    pub data: &'static [u8],
    /// Every name the face answers to, lowercase and distinct: the family
    /// (name id 1), the typographic family (16), the full name (4) and the
    /// PostScript name (6).
    pub names: Vec<String>,
    /// The `OS/2` weight class (100..900).
    pub weight: u16,
    pub italic: bool,
}

impl std::fmt::Debug for RegisteredFace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredFace")
            .field("names", &self.names)
            .field("weight", &self.weight)
            .field("italic", &self.italic)
            .field("bytes", &self.data.len())
            .finish()
    }
}

impl RegisteredFace {
    /// Read the face's names and style from its tables.
    pub fn parse(data: &'static [u8]) -> Result<Self, FontError> {
        let face = Face::parse(data, 0).map_err(|e| FontError::FontParseError {
            reason: e.to_string(),
        })?;
        let mut names: Vec<String> = Vec::new();
        for id in [
            name_id::FAMILY,
            name_id::TYPOGRAPHIC_FAMILY,
            name_id::FULL_NAME,
            name_id::POST_SCRIPT_NAME,
        ] {
            for name in face.names().into_iter().filter(|n| n.name_id == id) {
                if let Some(s) = name.to_string() {
                    let s = s.trim().to_lowercase();
                    if !s.is_empty() && !names.contains(&s) {
                        names.push(s);
                    }
                }
            }
        }
        if names.is_empty() {
            return Err(FontError::FontParseError {
                reason: "the face has no family, full or PostScript name".to_string(),
            });
        }
        Ok(RegisteredFace {
            data,
            names,
            weight: face.weight().to_number(),
            italic: matches!(face.style(), ttf_parser::Style::Italic | ttf_parser::Style::Oblique),
        })
    }

    /// The name the face is best known by.
    pub fn name(&self) -> &str {
        &self.names[0]
    }

    /// Whether `typeface` (lowercase) is one of the face's names.
    fn answers_to(&self, typeface: &str) -> bool {
        self.names.iter().any(|n| n == typeface)
    }

    /// How far the face is from the requested style: the weight distance,
    /// plus a posture mismatch, which outweighs any weight distance.
    fn distance(&self, weight: u16, italic: bool) -> u32 {
        self.weight.abs_diff(weight) as u32 + if self.italic != italic { 1000 } else { 0 }
    }

    /// Whether the face has a glyph for `c`.
    fn has_glyph(&self, c: char) -> bool {
        FontRef::try_from_slice(self.data)
            .ok()
            .is_some_and(|f| ab_glyph::Font::glyph_id(&f, c).0 != 0)
    }

    fn font_ref(&self) -> Result<FontRef<'static>, FontError> {
        FontRef::try_from_slice(self.data).map_err(|e| FontError::FontParseError {
            reason: e.to_string(),
        })
    }
}

/// Font data embedded in a PDF (`FontFile2`, or `FontFile3` of subtype
/// `OpenType`), under the name its font descriptor gives it.
#[derive(Debug, Clone)]
pub struct EmbeddedFont {
    /// The `FontName`, without a subset prefix (`ABCDEF+`).
    pub name: String,
    /// The raw font data (TTF/OTF).
    pub data: Vec<u8>,
}

/// One side of an `equate`: a typeface, and the weight and posture it is
/// restricted to or carries. Designer writes `Typeface_weight_posture`
/// with `*` for "any"; a bare name is any weight and posture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EquateName {
    /// Lowercase.
    pub typeface: String,
    pub weight: Option<FontWeight>,
    pub posture: Option<FontPosture>,
}

impl EquateName {
    pub fn parse(spec: &str) -> Self {
        let mut parts = spec.rsplitn(3, '_');
        let (typeface, weight, posture) = match (parts.next(), parts.next(), parts.next()) {
            (Some(posture), Some(weight), Some(typeface)) => (typeface, weight, posture),
            _ => (spec, "*", "*"),
        };
        let weight = match weight {
            "bold" => Some(FontWeight::Bold),
            "normal" => Some(FontWeight::Normal),
            _ => None,
        };
        let posture = match posture {
            "italic" => Some(FontPosture::Italic),
            "normal" => Some(FontPosture::Normal),
            _ => None,
        };
        EquateName {
            typeface: typeface.trim().to_lowercase(),
            weight,
            posture,
        }
    }

    fn matches(&self, typeface: &str, weight: FontWeight, posture: FontPosture) -> bool {
        self.typeface == typeface
            && self.weight.is_none_or(|w| w == weight)
            && self.posture.is_none_or(|p| p == posture)
    }
}

/// A configuration packet's `<equate from to>`: one typeface stands in for
/// another (XFA 3.3 chapter 28, font substitution).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontEquate {
    pub from: EquateName,
    pub to: EquateName,
}

/// A configuration packet's `<equateRange from to unicodeRange>`: one
/// typeface stands in for another for the codepoints in the ranges (font
/// fallback).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontEquateRange {
    /// Lowercase.
    pub from: String,
    pub to: String,
    pub ranges: Vec<(u32, u32)>,
}

impl FontEquateRange {
    /// `unicodeRange`: a comma-separated list of `U+XXXX` codepoints and
    /// `U+XXXX-YYYY` ranges, whitespace allowed around the commas. Entries
    /// that do not parse are reported and skipped.
    pub fn parse_unicode_range(spec: &str) -> Vec<(u32, u32)> {
        spec.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter_map(|entry| {
                let hex = |s: &str| u32::from_str_radix(s.trim().trim_start_matches("U+").trim_start_matches("u+"), 16).ok();
                let parsed = match entry.split_once('-') {
                    Some((a, b)) => hex(a).zip(hex(b)),
                    None => hex(entry).map(|c| (c, c)),
                };
                if parsed.is_none() {
                    log::warn!("unicodeRange entry {entry:?} is not a codepoint or range; ignored");
                }
                parsed
            })
            .collect()
    }

    fn contains(&self, c: char) -> bool {
        let cp = c as u32;
        self.ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&cp))
    }
}

/// What one document brings to font resolution: the faces embedded in its
/// PDF and the substitutions of its configuration packet.
#[derive(Debug, Default)]
pub struct DocumentFonts {
    pub equates: Vec<FontEquate>,
    pub equate_ranges: Vec<FontEquateRange>,
    embedded: Vec<RegisteredFace>,
}

impl DocumentFonts {
    /// The `equate` and `equateRange` elements of the document's
    /// configuration packet (`fontInfo/map`), wherever it is in `nodes`.
    pub fn from_nodes(nodes: &[XfaNode]) -> Self {
        let mut fonts = DocumentFonts::default();
        fn walk(nodes: &[XfaNode], fonts: &mut DocumentFonts) {
            for node in nodes {
                if let XfaNodeKind::Element { tag_name, .. } = &node.kind {
                    let attr = |name: &str| node.attributes.get(name).map(String::as_str);
                    match tag_name.as_str() {
                        "equate" => {
                            if let (Some(from), Some(to)) = (attr("from"), attr("to")) {
                                fonts.equates.push(FontEquate {
                                    from: EquateName::parse(from),
                                    to: EquateName::parse(to),
                                });
                            }
                        }
                        "equateRange" => {
                            if let (Some(from), Some(to), Some(range)) =
                                (attr("from"), attr("to"), attr("unicodeRange"))
                            {
                                fonts.equate_ranges.push(FontEquateRange {
                                    from: from.trim().to_lowercase(),
                                    to: to.to_string(),
                                    ranges: FontEquateRange::parse_unicode_range(range),
                                });
                            }
                        }
                        _ => {}
                    }
                }
                walk(&node.children, fonts);
            }
        }
        walk(nodes, &mut fonts);
        fonts
    }

    /// With the faces embedded in the document's PDF. A face that will not
    /// parse is reported and left out. The bytes are leaked: a face lives
    /// as long as the process, like a registered one.
    pub fn with_embedded(mut self, fonts: Vec<EmbeddedFont>) -> Self {
        for font in fonts {
            let data: &'static [u8] = Box::leak(font.data.into_boxed_slice());
            match RegisteredFace::parse(data) {
                Ok(mut face) => {
                    let name = font.name.trim().to_lowercase();
                    if !name.is_empty() && !face.names.contains(&name) {
                        face.names.insert(0, name);
                    }
                    self.embedded.push(face);
                }
                Err(e) => log::warn!("embedded font {:?} is not usable: {e}", font.name),
            }
        }
        self
    }

    pub fn embedded(&self) -> &[RegisteredFace] {
        &self.embedded
    }

    /// Make these the fonts of the document being laid out or drawn on this
    /// thread, until the guard is dropped. Interior mutability, deliberately:
    /// the engine resolves fonts from the many places that measure or draw
    /// text, and the document those belong to is what the thread is
    /// working on; the guard restores what was set before it.
    pub fn enter(self: &Arc<Self>) -> DocumentFontsGuard {
        let previous = CURRENT.with(|c| c.replace(Some(Arc::clone(self))));
        DocumentFontsGuard { previous }
    }

    /// The fonts of the document this thread is working on, if any.
    pub fn current() -> Option<Arc<DocumentFonts>> {
        CURRENT.with(|c| c.borrow().clone())
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<DocumentFonts>>> = const { RefCell::new(None) };
}

/// Restores the previously entered document fonts when dropped.
pub struct DocumentFontsGuard {
    previous: Option<Arc<DocumentFonts>>,
}

impl Drop for DocumentFontsGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT.with(|c| *c.borrow_mut() = previous);
    }
}

/// Where a font draws its underline and strikethrough, in pixels at one
/// size: from the face's own tables (`post` and `OS/2`), as XFA 3.3's
/// "Underline and Strikethrough" leaves them to the font.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineGeometry {
    /// Distance from the baseline down to the top of the underline.
    pub underline_offset: f32,
    pub underline_thickness: f32,
    /// Distance from the baseline up to the strikethrough, when the face
    /// states one.
    pub strike_offset: Option<f32>,
    pub strike_thickness: Option<f32>,
}

/// A font request, as the measurer caches faces by: the typeface
/// (lowercase, as written), weight and posture.
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct FontVariant {
    pub family: String,
    pub weight: FontWeight,
    pub posture: FontPosture,
}

impl FontVariant {
    pub fn new(family: &str, weight: FontWeight, posture: FontPosture) -> Self {
        FontVariant {
            family: family.trim().to_lowercase(),
            weight,
            posture,
        }
    }

    pub fn from_xfa_font(font: &Font) -> Self {
        Self::new(&font.typeface, font.weight, font.posture)
    }
}

/// The faces this process can draw with, and the resolution of a `<font>`
/// to one of them.
pub struct FontManager {
    faces: Vec<RegisteredFace>,
    fallback: Option<RegisteredFace>,
    /// Refuse a font that would need the fallback.
    strict: bool,
    /// Typefaces already reported as substituted.
    reported: HashSet<String>,
}

impl FontManager {
    pub fn new() -> Self {
        FontManager {
            faces: Vec::new(),
            fallback: None,
            strict: false,
            reported: HashSet::new(),
        }
    }

    /// Register a face from its bytes. A face that will not parse is
    /// reported and not registered.
    pub fn register_font_data(&mut self, data: &'static [u8]) {
        match RegisteredFace::parse(data) {
            Ok(face) => {
                if !self.faces.iter().any(|f| std::ptr::eq(f.data, face.data)) {
                    self.faces.push(face);
                }
            }
            Err(e) => log::warn!("a font file was not registered: {e}"),
        }
    }

    /// The face every typeface no face answers to is drawn in.
    pub fn set_fallback(&mut self, data: &'static [u8]) {
        match RegisteredFace::parse(data) {
            Ok(face) => self.fallback = Some(face),
            Err(e) => log::warn!("the fallback font was not set: {e}"),
        }
    }

    pub fn has_fallback(&self) -> bool {
        self.fallback.is_some()
    }

    /// In strict mode a typeface no face answers to is an error rather
    /// than a reported substitution.
    pub fn set_strict_mode(&mut self, strict: bool) {
        self.strict = strict;
    }

    /// The registered faces, in registration order.
    pub fn faces(&self) -> &[RegisteredFace] {
        &self.faces
    }

    /// Every name the registered faces answer to, sorted.
    pub fn available_families(&self) -> Vec<String> {
        let mut names: Vec<String> = self.faces.iter().flat_map(|f| f.names.iter().cloned()).collect();
        names.sort();
        names.dedup();
        names
    }

    /// Among `faces`, the one answering to `typeface` closest to the
    /// requested style.
    fn best<'a>(
        faces: impl Iterator<Item = &'a RegisteredFace>,
        typeface: &str,
        weight: u16,
        italic: bool,
    ) -> Option<&'a RegisteredFace> {
        faces
            .filter(|f| f.answers_to(typeface))
            .min_by_key(|f| f.distance(weight, italic))
    }

    /// The face for `font`, by the resolution the module doc describes.
    pub fn resolve(&mut self, font: &Font) -> Result<&'static [u8], FontError> {
        let document = DocumentFonts::current();
        let typeface = font.typeface.trim().to_lowercase();
        let italic = font.posture == FontPosture::Italic;
        let weight = font.weight.to_numeric();

        // 1. The document's own faces.
        if let Some(doc) = &document
            && let Some(face) = Self::best(doc.embedded.iter(), &typeface, weight, italic)
        {
            return Ok(face.data);
        }

        // 2. The document's substitution for this typeface.
        if let Some(doc) = &document
            && let Some(equate) = doc
                .equates
                .iter()
                .find(|e| e.from.matches(&typeface, font.weight, font.posture))
        {
            let weight = equate.to.weight.unwrap_or(font.weight).to_numeric();
            let italic = equate.to.posture.map(|p| p == FontPosture::Italic).unwrap_or(italic);
            let target = &equate.to.typeface;
            let face = Self::best(doc.embedded.iter(), target, weight, italic)
                .or_else(|| Self::best(self.faces.iter(), target, weight, italic));
            if let Some(face) = face {
                return Ok(face.data);
            }
        }

        // 3. A registered face of that name.
        if let Some(face) = Self::best(self.faces.iter(), &typeface, weight, italic) {
            return Ok(face.data);
        }

        // 4. The fallback, reported.
        if self.strict {
            return Err(FontError::FontNotFound {
                typeface: font.typeface.clone(),
                weight: font.weight,
                posture: font.posture,
            });
        }
        let fallback = self.fallback.as_ref().ok_or(FontError::NoFallbackAvailable)?;
        if self.reported.insert(typeface.clone()) {
            log::warn!(
                "typeface {:?} is not available; {:?} is drawn in its place",
                font.typeface,
                fallback.name()
            );
        }
        Ok(fallback.data)
    }

    /// Get a FontRef for a specific XFA font style
    pub fn get_font(&mut self, xfa_font: &Font) -> Result<FontRef<'static>, FontError> {
        let data = self.resolve(xfa_font)?;
        FontRef::try_from_slice(data).map_err(|e| FontError::FontParseError {
            reason: e.to_string(),
        })
    }

    /// The face for the XFA default font (Courier 10pt), or the fallback.
    pub fn get_default_font(&mut self) -> Result<FontRef<'static>, FontError> {
        self.get_font(&Font::default())
    }

    /// A face that has a glyph for `c`, when the text's own font has none
    /// (XFA 3.3 chapter 28, font fallback): the face the document's
    /// `equateRange` maps `family`'s codepoint to, else the symbol fallback
    /// (DejaVu Sans), else any face with the glyph. `None` when no face has
    /// it.
    pub fn font_for_glyph(&mut self, c: char, family: Option<&str>) -> Option<FontRef<'static>> {
        let document = DocumentFonts::current();
        if let (Some(doc), Some(family)) = (&document, family) {
            let family = family.trim().to_lowercase();
            for range in doc.equate_ranges.iter().filter(|r| r.from == family && r.contains(c)) {
                let target = range.to.trim().to_lowercase();
                let face = doc
                    .embedded
                    .iter()
                    .chain(self.faces.iter())
                    .find(|f| f.answers_to(&target) && f.has_glyph(c));
                if let Some(face) = face {
                    return face.font_ref().ok();
                }
            }
        }
        if let Ok(symbol) = FontRef::try_from_slice(SYMBOL_FALLBACK)
            && ab_glyph::Font::glyph_id(&symbol, c).0 != 0
        {
            return Some(symbol);
        }
        document
            .iter()
            .flat_map(|d| d.embedded.iter())
            .chain(self.faces.iter())
            .chain(self.fallback.iter())
            .find(|f| f.has_glyph(c))
            .and_then(|f| f.font_ref().ok())
    }

    /// Where `font`'s face draws its underline and strikethrough at
    /// `font_px` pixels, from the face's own tables. `None` for a face
    /// without an underline position.
    pub fn line_geometry(&mut self, font: &Font, font_px: f32) -> Option<LineGeometry> {
        let data = self.resolve(font).ok()?;
        let face = Face::parse(data, 0).ok()?;
        let per_unit = font_px / face.units_per_em() as f32;
        let underline = face.underline_metrics()?;
        let strike = face.strikeout_metrics();
        Some(LineGeometry {
            underline_offset: -(underline.position as f32) * per_unit,
            underline_thickness: (underline.thickness as f32 * per_unit).max(1.0),
            strike_offset: strike.map(|s| s.position as f32 * per_unit),
            strike_thickness: strike.map(|s| (s.thickness as f32 * per_unit).max(1.0)),
        })
    }
}

impl Default for FontManager {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL_FONT_MANAGER: OnceLock<std::sync::Mutex<FontManager>> = OnceLock::new();

/// The process's font manager: the registered faces are shared by every
/// document.
pub fn get_font_manager() -> &'static std::sync::Mutex<FontManager> {
    GLOBAL_FONT_MANAGER.get_or_init(|| std::sync::Mutex::new(FontManager::new()))
}

/// Register a face's bytes with `manager` (see `crate::fonts`).
pub fn register_profile_font_data(manager: &mut FontManager, data: &'static [u8]) {
    manager.register_font_data(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font(typeface: &str, weight: FontWeight, posture: FontPosture) -> Font {
        Font {
            typeface: typeface.to_string(),
            weight,
            posture,
            ..Font::default()
        }
    }

    /// The vendored Frutiger files answer to "Frutiger 45 Light" (family)
    /// and "Frutiger" (typographic family); the bold and italic requests go
    /// to the bold and italic faces, everything else to the light one.
    #[test]
    fn a_typeface_resolves_by_its_name_table_and_closest_style() {
        crate::fonts::register_test_fonts();
        let mut manager = get_font_manager().lock().unwrap_or_else(|e| e.into_inner());
        let weight_of = |m: &mut FontManager, f: &Font| {
            RegisteredFace::parse(m.resolve(f).unwrap()).unwrap()
        };
        let light = weight_of(&mut manager, &font("Frutiger 45 Light", FontWeight::Normal, FontPosture::Normal));
        assert!(light.weight <= 400 && !light.italic && light.names.contains(&"frutiger45light".to_string()), "{light:?}");
        // The PostScript name is a name too: the corpus writes it as a typeface.
        let by_ps = weight_of(&mut manager, &font("Frutiger45LightBold", FontWeight::Normal, FontPosture::Normal));
        assert!(by_ps.weight >= 600, "{by_ps:?}");
        let bold = weight_of(&mut manager, &font("frutiger 45 light", FontWeight::Bold, FontPosture::Normal));
        assert!(bold.weight >= 600 && !bold.italic, "{bold:?}");
        let italic = weight_of(&mut manager, &font("Frutiger", FontWeight::Normal, FontPosture::Italic));
        assert!(italic.italic, "{italic:?}");
    }

    #[test]
    fn an_unknown_typeface_is_the_fallback_and_strict_mode_refuses_it() {
        crate::fonts::register_test_fonts();
        let mut manager = get_font_manager().lock().unwrap_or_else(|e| e.into_inner());
        let unknown = font("No Such Face", FontWeight::Normal, FontPosture::Normal);
        let data = manager.resolve(&unknown).expect("the fallback stands in");
        assert!(std::ptr::eq(data, manager.fallback.as_ref().unwrap().data));
        manager.set_strict_mode(true);
        let refused = manager.resolve(&unknown);
        manager.set_strict_mode(false);
        assert!(matches!(refused, Err(FontError::FontNotFound { .. })));
    }

    #[test]
    fn equate_names_carry_designer_weight_and_posture_wildcards() {
        let any = EquateName::parse("MyriadPro_*_*");
        assert_eq!(any, EquateName { typeface: "myriadpro".into(), weight: None, posture: None });
        let italic = EquateName::parse("Myriad Pro_*_italic");
        assert_eq!(italic.typeface, "myriad pro");
        assert_eq!(italic.posture, Some(FontPosture::Italic));
        let bold_from = EquateName::parse("DTLDocumentaSansT_bold_*");
        assert_eq!(bold_from.weight, Some(FontWeight::Bold));
        assert!(bold_from.matches("dtldocumentasanst", FontWeight::Bold, FontPosture::Italic));
        assert!(!bold_from.matches("dtldocumentasanst", FontWeight::Normal, FontPosture::Normal));
        let bare = EquateName::parse("HelvOut Blk");
        assert_eq!(bare, EquateName { typeface: "helvout blk".into(), weight: None, posture: None });
    }

    #[test]
    fn unicode_ranges_parse_points_and_ranges() {
        assert_eq!(
            FontEquateRange::parse_unicode_range("U+20-37E, U+0400-43F ,U+2610"),
            vec![(0x20, 0x37e), (0x400, 0x43f), (0x2610, 0x2610)]
        );
        assert_eq!(FontEquateRange::parse_unicode_range("U+zz"), vec![]);
    }

    /// A document's `equate` sends a typeface to another name, which then
    /// resolves like any request; its `equateRange` sends a codepoint to
    /// another face.
    #[test]
    fn a_documents_equates_apply_while_entered() {
        crate::fonts::register_test_fonts();
        let xml = r#"<xdp><config><present><common><fontInfo><map>
<equate from="Helvetica_*_*" to="Frutiger 45 Light_bold_*" force="0"/>
<equateRange from="Frutiger 45 Light" to="DejaVu Sans" unicodeRange="U+2610-2612"/>
</map></fontInfo></common></present></config></xdp>"#;
        let nodes = XfaNode::parse(xml.as_bytes()).expect("xml");
        let fonts = Arc::new(DocumentFonts::from_nodes(&nodes));
        assert_eq!(fonts.equates.len(), 1);
        assert_eq!(fonts.equate_ranges.len(), 1);
        let helvetica = font("Helvetica", FontWeight::Normal, FontPosture::Normal);
        let mut manager = get_font_manager().lock().unwrap_or_else(|e| e.into_inner());
        {
            let _guard = fonts.enter();
            let face = RegisteredFace::parse(manager.resolve(&helvetica).unwrap()).unwrap();
            assert!(face.weight >= 600, "the equate asks for the bold face, got {face:?}");
            // The range names DejaVu, which is not registered here: the
            // symbol fallback (DejaVu itself) has the ballot box anyway.
            assert!(manager.font_for_glyph('☐', Some("Frutiger 45 Light")).is_some());
        }
        assert!(DocumentFonts::current().is_none(), "the guard restores the previous fonts");
    }

    #[test]
    fn an_embedded_face_wins_over_a_registered_one() {
        crate::fonts::register_test_fonts();
        let fonts = Arc::new(DocumentFonts::default().with_embedded(vec![EmbeddedFont {
            name: "ABCDEF+Frutiger 45 Light".trim_start_matches("ABCDEF+").to_string(),
            data: SYMBOL_FALLBACK.to_vec(),
        }]));
        let _guard = fonts.enter();
        let mut manager = get_font_manager().lock().unwrap_or_else(|e| e.into_inner());
        let data = manager.resolve(&font("Frutiger 45 Light", FontWeight::Normal, FontPosture::Normal)).unwrap();
        assert_eq!(data.len(), SYMBOL_FALLBACK.len(), "the document's own face is used");
    }

    #[test]
    fn line_geometry_comes_from_the_face() {
        crate::fonts::register_test_fonts();
        let mut manager = get_font_manager().lock().unwrap_or_else(|e| e.into_inner());
        let g = manager
            .line_geometry(&font("Frutiger 45 Light", FontWeight::Normal, FontPosture::Normal), 20.0)
            .expect("the face has underline metrics");
        assert!(g.underline_offset > 0.0 && g.underline_thickness >= 1.0, "{g:?}");
    }
}
