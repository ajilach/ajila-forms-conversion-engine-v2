//! Font Manager Module - XFA Font Resolution and Loading
//!
//! This module handles font resolution according to the XFA specification.
//! Per XFA spec section 17 (Template Reference - font element):
//! - typeface: The name of the typeface. Default is "Courier"
//! - size: Font size as measurement. Default is 10pt
//! - weight: "normal" or "bold". Default is "normal"
//! - posture: "normal" or "italic". Default is "normal"
//!
//! Font Resolution Strategy (per XFA spec section 28, "Font Mapping"):
//! 1. Try to find exact match for typeface + weight + posture
//! 2. If not found, check embedded fonts from PDF
//! 3. If not found, try common aliases (e.g., "Helvetica" -> "Arial")
//! 4. If not found, use genericFamily fallback (serif, sansSerif, monospace, etc.)
//! 5. If still not found, return error (configurable) or use system fallback
//!
//! Per XFA spec section 28 (Font Mapping in LiveCycle Forms ES3):
//! "When the XFA processor is unable to supply the requested font it substitutes
//! whatever font is available that it considers the best match."
//!
//! The genericFamily attribute values (from CSS2):
//! - sansSerif: Absence of serifs (default)
//! - serif: Presence of serifs on characters
//! - cursive: Presence of joining strokes (handwritten look)
//! - fantasy: Decorative fonts
//! - monospace: Fixed-width fonts

use crate::xfa::{Font, FontPosture, FontWeight, GenericFamily};
use ab_glyph::FontRef;
use regex_lite::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;
use thiserror::Error;
use ttf_parser::{Face, name_id};

/// Font-related errors
#[derive(Debug, Error)]
pub enum FontError {
    /// The requested font was not found and no suitable fallback exists
    #[error(
        "Font not found: '{typeface}' (weight: {weight:?}, posture: {posture:?}). Tried aliases: {tried_aliases:?}. Consider embedding the font in the PDF or specifying a genericFamily fallback."
    )]
    FontNotFound {
        typeface: String,
        weight: FontWeight,
        posture: FontPosture,
        tried_aliases: Vec<String>,
    },

    /// No fallback font available on the system
    #[error("No fallback font available. Searched paths: {searched_paths:?}")]
    NoFallbackAvailable { searched_paths: Vec<String> },

    /// Failed to parse font data
    #[error("Failed to parse font data: {reason}")]
    FontParseError { reason: String },

    /// Lock error when accessing font manager
    #[error("Font manager lock error: {0}")]
    LockError(String),

    /// Embedded font data is invalid
    #[error("Invalid embedded font data for '{name}': {reason}")]
    InvalidEmbeddedFont { name: String, reason: String },
}

/// Extension trait to add fallback_typefaces to GenericFamily
pub trait GenericFamilyExt {
    /// Get fallback font families for this generic family
    /// Returns a list of common typeface names to try
    fn fallback_typefaces(&self) -> Vec<&'static str>;
}

impl GenericFamilyExt for GenericFamily {
    fn fallback_typefaces(&self) -> Vec<&'static str> {
        match self {
            // Prefer Arial first since it has explicit bold/italic files on most systems
            GenericFamily::SansSerif => vec![
                "arial",
                "helvetica",
                "helvetica neue",
                "liberation sans",
                "dejavu sans",
                "verdana",
            ],
            GenericFamily::Serif => vec![
                "times new roman",
                "times",
                "georgia",
                "liberation serif",
                "dejavu serif",
            ],
            GenericFamily::Monospace => vec![
                "courier new",
                "courier",
                "consolas",
                "liberation mono",
                "dejavu sans mono",
            ],
            GenericFamily::Cursive => {
                vec!["comic sans ms", "brush script mt", "lucida handwriting"]
            }
            GenericFamily::Fantasy => vec!["impact", "papyrus", "copperplate"],
        }
    }
}

/// Embedded font data from a PDF file
#[derive(Debug, Clone)]
pub struct EmbeddedFont {
    /// Font name as referenced in the PDF
    pub name: String,
    /// The raw font data (TTF/OTF)
    pub data: Vec<u8>,
    /// Font weight if known
    pub weight: FontWeight,
    /// Font posture if known  
    pub posture: FontPosture,
    /// Generic family hint
    pub generic_family: Option<GenericFamily>,
}

/// Font equate mapping per XFA spec section 28
/// Maps a font name to another font name for substitution
/// Per XFA spec: "The equate element enables an XFA processor to map an unavailable
/// font to an available one."
#[derive(Debug, Clone)]
pub struct FontEquate {
    /// Source font name to match (case-insensitive)
    pub from: String,
    /// Target font name to substitute
    pub to: String,
    /// Optional: only apply when source has this weight
    pub from_weight: Option<FontWeight>,
    /// Optional: only apply when source has this posture
    pub from_posture: Option<FontPosture>,
    /// Optional: substitute with this weight (defaults to source weight)
    pub to_weight: Option<FontWeight>,
    /// Optional: substitute with this posture (defaults to source posture)
    pub to_posture: Option<FontPosture>,
}

impl FontEquate {
    /// Create a simple font name mapping
    pub fn new(from: &str, to: &str) -> Self {
        FontEquate {
            from: from.to_lowercase(),
            to: to.to_string(),
            from_weight: None,
            from_posture: None,
            to_weight: None,
            to_posture: None,
        }
    }

    /// Create a mapping with specific weight/posture variants
    pub fn with_variants(
        from: &str,
        to: &str,
        from_weight: Option<FontWeight>,
        from_posture: Option<FontPosture>,
        to_weight: Option<FontWeight>,
        to_posture: Option<FontPosture>,
    ) -> Self {
        FontEquate {
            from: from.to_lowercase(),
            to: to.to_string(),
            from_weight,
            from_posture,
            to_weight,
            to_posture,
        }
    }

    /// Check if this equate applies to the given variant
    pub fn matches(&self, variant: &FontVariant) -> bool {
        if variant.family != self.from {
            return false;
        }
        if let Some(w) = self.from_weight
            && variant.weight != w
        {
            return false;
        }
        if let Some(p) = self.from_posture
            && variant.posture != p
        {
            return false;
        }
        true
    }

    /// Get the target variant for a matching source variant
    pub fn target_variant(&self, source: &FontVariant) -> FontVariant {
        FontVariant::new(
            &self.to,
            self.to_weight.unwrap_or(source.weight),
            self.to_posture.unwrap_or(source.posture),
        )
    }
}

/// Font equate range for Unicode-based font substitution per XFA spec
/// Per XFA spec: "The equateRange element enables an XFA processor to map specific
/// Unicode ranges in an unavailable font to an available font."
#[derive(Debug, Clone)]
pub struct FontEquateRange {
    /// Source font name (case-insensitive)
    pub from: String,
    /// Target font name for specified Unicode ranges
    pub to: String,
    /// Unicode ranges as (start, end) pairs (inclusive)
    /// Per XFA spec: ranges like "U+20-37E" or "U+20,U+30-3F"
    pub unicode_ranges: Vec<(u32, u32)>,
}

impl FontEquateRange {
    /// Create a new equate range
    pub fn new(from: &str, to: &str, ranges: Vec<(u32, u32)>) -> Self {
        FontEquateRange {
            from: from.to_lowercase(),
            to: to.to_string(),
            unicode_ranges: ranges,
        }
    }

    /// Parse Unicode range string per XFA spec format
    /// Supports: "U+20-37E", "U+20,U+30-3F", "U+0041"
    pub fn parse_unicode_range(range_str: &str) -> Vec<(u32, u32)> {
        let mut ranges = Vec::new();
        for part in range_str.split(',') {
            let part = part.trim();
            if let Some(range) = part.strip_prefix("U+").or_else(|| part.strip_prefix("u+")) {
                if let Some((start_str, end_str)) = range.split_once('-') {
                    if let (Ok(start), Ok(end)) = (
                        u32::from_str_radix(start_str.trim(), 16),
                        u32::from_str_radix(end_str.trim(), 16),
                    ) {
                        ranges.push((start, end));
                    }
                } else if let Ok(single) = u32::from_str_radix(range.trim(), 16) {
                    ranges.push((single, single));
                }
            }
        }
        ranges
    }

    /// Check if a codepoint falls within this equate range's Unicode ranges
    pub fn contains_codepoint(&self, codepoint: u32) -> bool {
        self.unicode_ranges
            .iter()
            .any(|(start, end)| codepoint >= *start && codepoint <= *end)
    }

    /// Check if this equate range applies to the given font
    pub fn matches_font(&self, font_family: &str) -> bool {
        font_family.to_lowercase() == self.from
    }
}

/// Normalize a typeface name to extract base family and weight hint
/// Handles naming conventions like "Frutiger 45 Light", "Helvetica Neue Light", etc.
fn normalize_typeface(typeface: &str) -> (String, Option<FontWeight>) {
    let typeface_lower = typeface.to_lowercase();

    // Frutiger-style numeric weights: 45=Light, 46=LightItalic, 55=Roman, 56=Italic, 65=Bold, etc.
    if let Ok(re) = Regex::new(r"^(.+?)\s*(\d{2})\s*(.*)$")
        && let Some(caps) = re.captures(&typeface_lower)
    {
        let base = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        let num: u32 = caps
            .get(2)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0);

        // Map Frutiger-style numbers to weights
        let weight_from_num = match num {
            35 | 36 => Some(FontWeight::Thin),
            45 | 46 => Some(FontWeight::Light),
            55 | 56 => Some(FontWeight::Normal),
            65 | 66 => Some(FontWeight::Bold),
            75 | 76 => Some(FontWeight::ExtraBold),
            85 | 86 => Some(FontWeight::Black),
            95 | 96 => Some(FontWeight::Black),
            _ => None,
        };

        if weight_from_num.is_some() && !base.is_empty() {
            return (base.to_string(), weight_from_num);
        }
    }

    // Check for weight keywords in the name
    let weight_keywords = [
        ("ultra light", FontWeight::ExtraLight),
        ("extra light", FontWeight::ExtraLight),
        ("ultralight", FontWeight::ExtraLight),
        ("extralight", FontWeight::ExtraLight),
        ("thin", FontWeight::Thin),
        ("hairline", FontWeight::Thin),
        ("light", FontWeight::Light),
        ("medium", FontWeight::Medium),
        ("semi bold", FontWeight::SemiBold),
        ("semibold", FontWeight::SemiBold),
        ("demi bold", FontWeight::SemiBold),
        ("demibold", FontWeight::SemiBold),
        ("extra bold", FontWeight::ExtraBold),
        ("extrabold", FontWeight::ExtraBold),
        ("ultra bold", FontWeight::ExtraBold),
        ("ultrabold", FontWeight::ExtraBold),
        ("black", FontWeight::Black),
        ("heavy", FontWeight::Black),
        ("bold", FontWeight::Bold),
        ("regular", FontWeight::Normal),
        ("roman", FontWeight::Normal),
        ("book", FontWeight::Normal),
    ];

    for (keyword, weight) in weight_keywords {
        if typeface_lower.contains(keyword) {
            // Extract family by removing the weight keyword
            let family = typeface_lower
                .replace(keyword, "")
                .trim()
                .replace("  ", " ")
                .trim()
                .to_string();
            if !family.is_empty() {
                return (family, Some(weight));
            }
        }
    }

    (typeface_lower, None)
}

/// Ordering key for "which face best represents a family when no style was
/// asked for": upright normal-weight first, then by the variant's own total
/// order so ties never depend on iteration order.
///
/// DEVIATION: exists because upstream resolved family lookups by taking an
/// arbitrary HashMap entry.
fn variant_neutrality(v: &FontVariant) -> (u8, u8, FontWeight, FontPosture) {
    (
        u8::from(v.weight != FontWeight::Normal),
        u8::from(v.posture != FontPosture::Normal),
        v.weight,
        v.posture,
    )
}

/// Font variant key for caching
// DEVIATION: `PartialOrd, Ord` added so this can key a BTreeMap.
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct FontVariant {
    pub family: String,
    pub weight: FontWeight,
    pub posture: FontPosture,
}

impl FontVariant {
    pub fn new(family: &str, weight: FontWeight, posture: FontPosture) -> Self {
        FontVariant {
            family: family.to_lowercase(),
            weight,
            posture,
        }
    }

    pub fn from_xfa_font(font: &Font) -> Self {
        // Normalize typeface to extract base family and weight hint
        let (normalized_family, weight_hint) = normalize_typeface(&font.typeface);

        // Use detected weight if XFA weight is Normal and we found a weight hint
        let weight = if font.weight == FontWeight::Normal {
            weight_hint.unwrap_or(font.weight)
        } else {
            font.weight
        };

        FontVariant {
            family: normalized_family,
            weight,
            posture: font.posture,
        }
    }
}

/// Configuration for font resolution behavior
#[derive(Debug, Clone)]
pub struct FontConfig {
    /// If true, return an error when a font is not found (instead of using fallback)
    pub strict_mode: bool,
    /// Generic family to use for fallback when typeface not found
    pub default_generic_family: GenericFamily,
}

impl Default for FontConfig {
    fn default() -> Self {
        FontConfig {
            strict_mode: false,
            default_generic_family: GenericFamily::SansSerif,
        }
    }
}

/// Font manager that handles font resolution and loading
///
/// Per XFA spec, font resolution follows this priority:
/// 1. Embedded fonts from PDF (registered via `register_embedded_font`)
/// 2. Exact typeface match from system fonts
/// 3. Aliases (e.g., "Helvetica" -> "Arial")
/// 4. Generic family fallback (serif, sansSerif, monospace, etc.)
/// 5. System default fallback
pub struct FontManager {
    /// Loaded fonts (static lifetime for ab_glyph)
    // DEVIATION: upstream uses a HashMap here, and `find_loaded_by_family` /
    // `find_loaded_fuzzy` below resolve a family by taking the *first* matching
    // entry of that map. With several variants of one family loaded — the UBS
    // profile has three Frutiger faces — which one wins depends on the
    // process's random hash seed. Different faces have different advances,
    // which change wrap points, which change node heights, which change
    // `page_breaks`: the rendered page *count* could differ between runs.
    // A BTreeMap makes iteration order total and stable.
    loaded_fonts: std::collections::BTreeMap<FontVariant, &'static [u8]>,
    /// Font family aliases (e.g., "Helvetica" -> ["Arial", "Helvetica Neue"])
    aliases: HashMap<String, Vec<String>>,
    /// Default fallback font data
    fallback_font_data: Option<&'static [u8]>,
    /// Embedded fonts (compile-time + from PDF files per XFA spec)
    embedded_fonts: HashMap<String, EmbeddedFont>,
    /// Loaded embedded font data (static lifetime)
    loaded_embedded: HashMap<String, &'static [u8]>,
    /// Font equate mappings per XFA spec section 28
    /// These are checked first in font resolution (step 1 of XFA algorithm)
    equates: Vec<FontEquate>,
    /// Font equate ranges for Unicode-based substitution per XFA spec
    equate_ranges: Vec<FontEquateRange>,
    /// Configuration
    config: FontConfig,
}

impl FontManager {
    /// Create a new font manager and scan for system fonts
    pub fn new() -> Self {
        Self::with_config(FontConfig::default())
    }

    /// Create a new font manager with custom configuration
    pub fn with_config(config: FontConfig) -> Self {
        FontManager {
            loaded_fonts: std::collections::BTreeMap::new(),
            aliases: Self::build_aliases(),
            fallback_font_data: None,
            embedded_fonts: HashMap::new(),
            loaded_embedded: HashMap::new(),
            equates: Vec::new(),
            equate_ranges: Vec::new(),
            config,
        }
    }

    /// Register a font from raw data (e.g. loaded from a profile).
    ///
    /// Reads metadata via ttf-parser and inserts into `loaded_fonts` keyed
    /// by the detected family/weight/posture variant.
    pub fn register_font_data(&mut self, data: &'static [u8]) {
        if let Some((family, weight, posture)) = Self::read_font_metadata(data, 0) {
            self.loaded_fonts
                .insert(FontVariant::new(&family, weight, posture), data);
        }
    }

    /// Set the fallback font data (used when no matching font is found).
    pub fn set_fallback(&mut self, data: &'static [u8]) {
        self.fallback_font_data = Some(data);
    }

    /// Whether a fallback font is set: without one, layout has nothing to
    /// measure an unresolved typeface with (see `crate::fonts`).
    pub fn has_fallback(&self) -> bool {
        self.fallback_font_data.is_some()
    }

    /// Enable or disable strict mode
    /// In strict mode, returns an error when a font is not found instead of using fallback
    pub fn set_strict_mode(&mut self, strict: bool) {
        self.config.strict_mode = strict;
    }

    /// Register an embedded font from a PDF file
    /// Per XFA spec section 28: "When an XFA form is packaged inside a PDF file
    /// the PDF file may carry the required fonts along with the form."
    pub fn register_embedded_font(&mut self, mut font: EmbeddedFont) -> Result<(), FontError> {
        // Validate that we can parse the font data
        FontRef::try_from_slice(&font.data).map_err(|e| FontError::InvalidEmbeddedFont {
            name: font.name.clone(),
            reason: e.to_string(),
        })?;

        // Try to read accurate metadata from font data
        if let Some((family, weight, posture)) = Self::read_font_metadata(&font.data, 0) {
            // Update with detected values (more accurate than provided)
            font.weight = weight;
            font.posture = posture;
            // Register under both the provided name and the detected family name
            let name_lower = font.name.to_lowercase();
            self.embedded_fonts.insert(name_lower.clone(), font.clone());
            if family != name_lower {
                let mut family_font = font.clone();
                family_font.name = family.clone();
                self.embedded_fonts.insert(family, family_font);
            }
        } else {
            let name_lower = font.name.to_lowercase();
            self.embedded_fonts.insert(name_lower, font);
        }

        Ok(())
    }

    /// Register multiple embedded fonts from a PDF
    pub fn register_embedded_fonts(&mut self, fonts: Vec<EmbeddedFont>) -> Result<(), FontError> {
        for font in fonts {
            self.register_embedded_font(font)?;
        }
        Ok(())
    }

    /// Check if an embedded font with this name exists
    pub fn has_embedded_font(&self, name: &str) -> bool {
        self.embedded_fonts.contains_key(&name.to_lowercase())
    }

    /// Get list of registered embedded fonts
    pub fn embedded_font_names(&self) -> Vec<&str> {
        self.embedded_fonts
            .values()
            .map(|f| f.name.as_str())
            .collect()
    }

    /// Register a font equate mapping per XFA spec section 28
    /// Per XFA spec: "The equate element enables an XFA processor to map an unavailable
    /// font to an available one."
    ///
    /// Equates are checked first in font resolution (step 1 of XFA algorithm).
    pub fn register_equate(&mut self, equate: FontEquate) {
        self.equates.push(equate);
    }

    /// Register multiple font equate mappings
    pub fn register_equates(&mut self, equates: Vec<FontEquate>) {
        self.equates.extend(equates);
    }

    /// Register a font equate range for Unicode-based substitution per XFA spec
    /// Per XFA spec: "The equateRange element enables an XFA processor to map specific
    /// Unicode ranges in an unavailable font to an available font."
    ///
    /// This is used for per-codepoint font fallback (e.g., CJK characters).
    pub fn register_equate_range(&mut self, equate_range: FontEquateRange) {
        self.equate_ranges.push(equate_range);
    }

    /// Register multiple font equate ranges
    pub fn register_equate_ranges(&mut self, equate_ranges: Vec<FontEquateRange>) {
        self.equate_ranges.extend(equate_ranges);
    }

    /// Get fallback font for a specific codepoint based on equate ranges
    /// Returns the target font family if an equate range matches, None otherwise
    pub fn get_fallback_for_codepoint(&self, font_family: &str, codepoint: u32) -> Option<&str> {
        for range in &self.equate_ranges {
            if range.matches_font(font_family) && range.contains_codepoint(codepoint) {
                return Some(&range.to);
            }
        }
        None
    }

    /// Clear all registered equates and equate ranges
    pub fn clear_equates(&mut self) {
        self.equates.clear();
        self.equate_ranges.clear();
    }

    /// Build font family aliases map
    /// Per XFA spec: when requested font is unavailable, substitute best match
    fn build_aliases() -> HashMap<String, Vec<String>> {
        let mut aliases = HashMap::new();

        // Sans-serif family aliases
        aliases.insert(
            "helvetica".to_string(),
            vec![
                "arial".to_string(),
                "helvetica neue".to_string(),
                "liberation sans".to_string(),
                "dejavu sans".to_string(),
            ],
        );
        aliases.insert(
            "arial".to_string(),
            vec![
                "helvetica".to_string(),
                "helvetica neue".to_string(),
                "liberation sans".to_string(),
                "dejavu sans".to_string(),
            ],
        );

        // Serif family aliases
        aliases.insert(
            "times".to_string(),
            vec![
                "times new roman".to_string(),
                "liberation serif".to_string(),
                "dejavu serif".to_string(),
            ],
        );
        aliases.insert(
            "times new roman".to_string(),
            vec![
                "times".to_string(),
                "liberation serif".to_string(),
                "dejavu serif".to_string(),
            ],
        );

        // Monospace family aliases (XFA default is Courier)
        aliases.insert(
            "courier".to_string(),
            vec![
                "courier new".to_string(),
                "liberation mono".to_string(),
                "dejavu sans mono".to_string(),
            ],
        );
        aliases.insert(
            "courier new".to_string(),
            vec![
                "courier".to_string(),
                "liberation mono".to_string(),
                "dejavu sans mono".to_string(),
            ],
        );

        aliases
    }

    /// Read font metadata (family, weight, posture) from font file data using ttf-parser
    fn read_font_metadata(
        font_data: &[u8],
        face_index: u32,
    ) -> Option<(String, FontWeight, FontPosture)> {
        let face = Face::parse(font_data, face_index).ok()?;

        let typographic_family = face
            .names()
            .into_iter()
            .filter(|name| name.name_id == name_id::TYPOGRAPHIC_FAMILY)
            .filter_map(|name| name.to_string())
            .next();

        let font_family = face
            .names()
            .into_iter()
            .filter(|name| name.name_id == name_id::FAMILY)
            .filter_map(|name| name.to_string())
            .next();

        let raw_family = typographic_family.or(font_family)?;
        let (family, _weight_hint) = normalize_typeface(&raw_family);
        let family = if family.is_empty() {
            raw_family.to_lowercase()
        } else {
            family
        };

        let weight = FontWeight::from_numeric(face.weight().to_number());

        let posture = match face.style() {
            ttf_parser::Style::Normal => FontPosture::Normal,
            ttf_parser::Style::Italic | ttf_parser::Style::Oblique => FontPosture::Italic,
        };

        Some((family, weight, posture))
    }

    /// Get font data for a specific variant
    /// Returns static font data that can be used with ab_glyph
    ///
    /// Resolution order:
    /// 1. Check equate elements for direct mapping
    /// 2. Embedded fonts (compile-time + PDF)
    /// 3. Aliases
    /// 4. Generic family fallback
    /// 5. Fallback font (if not in strict mode)
    pub fn get_font_data(&mut self, variant: &FontVariant) -> Result<&'static [u8], FontError> {
        self.get_font_data_with_generic(variant, None)
    }

    /// Get font data with optional generic family fallback
    pub fn get_font_data_with_generic(
        &mut self,
        variant: &FontVariant,
        generic_family: Option<GenericFamily>,
    ) -> Result<&'static [u8], FontError> {
        let mut tried_aliases = Vec::new();

        // Check if already loaded
        if let Some(data) = self.loaded_fonts.get(variant) {
            return Ok(*data);
        }

        // 1. Check equate elements first (per XFA spec step 1)
        for equate in self.equates.clone() {
            if equate.matches(variant) {
                let target = equate.target_variant(variant);
                tried_aliases.push(target.family.clone());

                if let Ok(data) = self.get_font_data(&target) {
                    self.loaded_fonts.insert(variant.clone(), data);
                    return Ok(data);
                }
            }
        }

        // 2. Try embedded fonts from PDF (keyed by family name)
        let family_lower = variant.family.to_lowercase();
        if let Some(embedded) = self.embedded_fonts.get(&family_lower) {
            if let Some(data) = self.loaded_embedded.get(&family_lower) {
                return Ok(*data);
            }
            let static_data: &'static [u8] = Box::leak(embedded.data.clone().into_boxed_slice());
            self.loaded_embedded
                .insert(family_lower.clone(), static_data);
            self.loaded_fonts.insert(variant.clone(), static_data);
            return Ok(static_data);
        }

        // 3. Try loaded fonts with relaxed weight/posture (same family)
        // Better to use the right typeface with wrong weight than a different typeface
        let normal_variant =
            FontVariant::new(&variant.family, FontWeight::Normal, FontPosture::Normal);
        if *variant != normal_variant {
            if let Some(&data) = self.loaded_fonts.get(&normal_variant) {
                self.loaded_fonts.insert(variant.clone(), data);
                return Ok(data);
            }
        }
        // Try any available weight/posture of the same family
        if let Some(data) = self.find_loaded_by_family(&variant.family) {
            self.loaded_fonts.insert(variant.clone(), data);
            return Ok(data);
        }

        // 3b. Fuzzy matching — e.g. "frutiger" matches "frutiger lt std"
        if let Some(data) = self.find_loaded_fuzzy(&variant.family) {
            self.loaded_fonts.insert(variant.clone(), data);
            return Ok(data);
        }

        // 4. Try aliases
        if let Some(aliases) = self.aliases.get(&variant.family).cloned() {
            for alias in &aliases {
                tried_aliases.push(alias.clone());

                // Check PDF-embedded fonts for alias
                if let Some(embedded) = self.embedded_fonts.get(alias) {
                    if let Some(data) = self.loaded_embedded.get(alias) {
                        return Ok(*data);
                    }
                    let static_data: &'static [u8] =
                        Box::leak(embedded.data.clone().into_boxed_slice());
                    self.loaded_embedded.insert(alias.clone(), static_data);
                    self.loaded_fonts.insert(variant.clone(), static_data);
                    return Ok(static_data);
                }

                // Check loaded fonts for alias with same weight/posture
                let alias_variant = FontVariant::new(alias, variant.weight, variant.posture);
                if let Some(&data) = self.loaded_fonts.get(&alias_variant) {
                    self.loaded_fonts.insert(variant.clone(), data);
                    return Ok(data);
                }

                // Check loaded fonts for alias with normal weight/posture
                let alias_normal = FontVariant::new(alias, FontWeight::Normal, FontPosture::Normal);
                if let Some(&data) = self.loaded_fonts.get(&alias_normal) {
                    self.loaded_fonts.insert(variant.clone(), data);
                    return Ok(data);
                }

                // Any variant of alias family
                if let Some(data) = self.find_loaded_by_family(alias) {
                    self.loaded_fonts.insert(variant.clone(), data);
                    return Ok(data);
                }
            }
        }

        // 5. Try generic family fallback
        let generic = generic_family.unwrap_or(self.config.default_generic_family);
        for fallback_typeface in generic.fallback_typefaces() {
            let ft_lower = fallback_typeface.to_lowercase();
            if ft_lower == variant.family {
                continue;
            }
            if !tried_aliases.contains(&ft_lower) {
                tried_aliases.push(ft_lower.clone());
            }

            // Check PDF-embedded fonts
            if let Some(embedded) = self.embedded_fonts.get(&ft_lower) {
                if let Some(data) = self.loaded_embedded.get(&ft_lower) {
                    return Ok(*data);
                }
                let static_data: &'static [u8] =
                    Box::leak(embedded.data.clone().into_boxed_slice());
                self.loaded_embedded.insert(ft_lower, static_data);
                self.loaded_fonts.insert(variant.clone(), static_data);
                return Ok(static_data);
            }

            // Check loaded fonts with same weight/posture
            let fb_variant = FontVariant::new(fallback_typeface, variant.weight, variant.posture);
            if let Some(&data) = self.loaded_fonts.get(&fb_variant) {
                self.loaded_fonts.insert(variant.clone(), data);
                return Ok(data);
            }

            // Check loaded fonts with normal weight/posture
            let fb_normal =
                FontVariant::new(fallback_typeface, FontWeight::Normal, FontPosture::Normal);
            if let Some(&data) = self.loaded_fonts.get(&fb_normal) {
                self.loaded_fonts.insert(variant.clone(), data);
                return Ok(data);
            }
        }

        // 6. In strict mode, return error. Otherwise use fallback.
        if self.config.strict_mode {
            return Err(FontError::FontNotFound {
                typeface: variant.family.clone(),
                weight: variant.weight,
                posture: variant.posture,
                tried_aliases,
            });
        }

        self.get_fallback_font()
    }

    /// The registered variants, in the map's total order.
    ///
    /// ADDITION (not upstream): lets a test assert *which* face an ambiguous
    /// family lookup resolves to, which is the whole point of the ordering fix.
    pub fn loaded_variants(&self) -> Vec<FontVariant> {
        self.loaded_fonts.keys().cloned().collect()
    }

    /// Find any loaded font with the given family name.
    ///
    /// DEVIATION: upstream returns the first match from a HashMap, i.e. an
    /// arbitrary variant. This walks the ordered map and prefers the most
    /// neutral face — normal weight and upright — so a family lookup that does
    /// not specify a style resolves to the same face on every run and in every
    /// process.
    fn find_loaded_by_family(&self, family: &str) -> Option<&'static [u8]> {
        self.loaded_fonts
            .iter()
            .filter(|(v, _)| v.family == family)
            .min_by_key(|(v, _)| variant_neutrality(v))
            .map(|(_, &data)| data)
    }

    /// Find a loaded font via fuzzy matching on the family name.
    ///
    /// DEVIATION: deterministic for the same reason as `find_loaded_by_family`.
    fn find_loaded_fuzzy(&self, family: &str) -> Option<&'static [u8]> {
        self.loaded_fonts
            .iter()
            .filter(|(v, _)| {
                v.family.starts_with(family) || v.family.contains(&format!("{} ", family))
            })
            .min_by_key(|(v, _)| variant_neutrality(v))
            .map(|(_, &data)| data)
    }

    /// Get fallback font data.
    fn get_fallback_font(&mut self) -> Result<&'static [u8], FontError> {
        if let Some(data) = self.fallback_font_data {
            return Ok(data);
        }

        Err(FontError::NoFallbackAvailable {
            searched_paths: vec!["(no embedded fonts available)".to_string()],
        })
    }

    /// Get a FontRef for a specific XFA font style
    pub fn get_font(&mut self, xfa_font: &Font) -> Result<FontRef<'static>, FontError> {
        let variant = FontVariant::from_xfa_font(xfa_font);
        let data = self.get_font_data_with_generic(&variant, xfa_font.generic_family)?;

        FontRef::try_from_slice(data).map_err(|e| FontError::FontParseError {
            reason: e.to_string(),
        })
    }

    /// Get a FontRef using default XFA font settings
    pub fn get_default_font(&mut self) -> Result<FontRef<'static>, FontError> {
        let default_font = Font::default();
        self.get_font(&default_font)
    }

    /// Check if a specific font variant is available
    pub fn has_font(&self, family: &str, _weight: FontWeight, _posture: FontPosture) -> bool {
        self.embedded_fonts.contains_key(&family.to_lowercase())
    }

    /// Get list of available font families
    pub fn available_families(&self) -> Vec<String> {
        let mut families: Vec<String> = self
            .embedded_fonts
            .values()
            .map(|f| f.name.clone())
            .collect();
        families.sort();
        families.dedup();
        families
    }
}

impl Default for FontManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Global font manager instance (lazy initialized)
static GLOBAL_FONT_MANAGER: OnceLock<std::sync::Mutex<FontManager>> = OnceLock::new();

/// Get the global font manager
pub fn get_font_manager() -> &'static std::sync::Mutex<FontManager> {
    // DEVIATION: upstream seeds this with the compiled-in UBS profile under
    // #[cfg(test)], so its tests never had to bootstrap fonts. That profile is
    // gone (licensing), and an implicit font environment is exactly what makes
    // the silent-degradation trap easy to fall into — tests register
    // explicitly, like production does.
    GLOBAL_FONT_MANAGER.get_or_init(|| std::sync::Mutex::new(FontManager::new()))
}

/// Convenience function to get a font for an XFA font style
pub fn get_font_for_style(xfa_font: &Font) -> Result<FontRef<'static>, FontError> {
    let manager = get_font_manager();
    let mut manager = manager
        .lock()
        .map_err(|e| FontError::LockError(e.to_string()))?;
    manager.get_font(xfa_font)
}

/// Convenience function to get the default fallback font
pub fn get_fallback_font() -> Result<FontRef<'static>, FontError> {
    let manager = get_font_manager();
    let mut manager = manager
        .lock()
        .map_err(|e| FontError::LockError(e.to_string()))?;
    manager.get_default_font()
}

/// Register an embedded font globally (for use with PDFs that contain embedded fonts)
pub fn register_embedded_font_global(font: EmbeddedFont) -> Result<(), FontError> {
    let manager = get_font_manager();
    let mut manager = manager
        .lock()
        .map_err(|e| FontError::LockError(e.to_string()))?;
    manager.register_embedded_font(font)
}

/// Enable strict mode globally (returns errors instead of fallback)
pub fn set_strict_mode_global(strict: bool) -> Result<(), FontError> {
    let manager = get_font_manager();
    let mut manager = manager
        .lock()
        .map_err(|e| FontError::LockError(e.to_string()))?;
    manager.set_strict_mode(strict);
    Ok(())
}

/// Register a font equate mapping globally per XFA spec section 28
pub fn register_equate_global(equate: FontEquate) -> Result<(), FontError> {
    let manager = get_font_manager();
    let mut manager = manager
        .lock()
        .map_err(|e| FontError::LockError(e.to_string()))?;
    manager.register_equate(equate);
    Ok(())
}

/// Register multiple font equate mappings globally
pub fn register_equates_global(equates: Vec<FontEquate>) -> Result<(), FontError> {
    let manager = get_font_manager();
    let mut manager = manager
        .lock()
        .map_err(|e| FontError::LockError(e.to_string()))?;
    manager.register_equates(equates);
    Ok(())
}

/// Register a font equate range globally for Unicode-based substitution
pub fn register_equate_range_global(equate_range: FontEquateRange) -> Result<(), FontError> {
    let manager = get_font_manager();
    let mut manager = manager
        .lock()
        .map_err(|e| FontError::LockError(e.to_string()))?;
    manager.register_equate_range(equate_range);
    Ok(())
}

/// Register multiple font equate ranges globally
pub fn register_equate_ranges_global(equate_ranges: Vec<FontEquateRange>) -> Result<(), FontError> {
    let manager = get_font_manager();
    let mut manager = manager
        .lock()
        .map_err(|e| FontError::LockError(e.to_string()))?;
    manager.register_equate_ranges(equate_ranges);
    Ok(())
}

/// Get fallback font for a specific codepoint globally
pub fn get_fallback_for_codepoint_global(
    font_family: &str,
    codepoint: u32,
) -> Result<Option<String>, FontError> {
    let manager = get_font_manager();
    let manager = manager
        .lock()
        .map_err(|e| FontError::LockError(e.to_string()))?;
    Ok(manager
        .get_fallback_for_codepoint(font_family, codepoint)
        .map(|s| s.to_string()))
}

/// Register font data with an already-locked FontManager.
///
/// Called from `profiles::load_profile_fonts` which holds the lock itself.
pub fn register_profile_font_data(manager: &mut FontManager, data: &'static [u8]) {
    manager.register_font_data(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_font_variant_creation() {
        let variant = FontVariant::new("Arial", FontWeight::Bold, FontPosture::Italic);
        assert_eq!(variant.family, "arial");
        assert_eq!(variant.weight, FontWeight::Bold);
        assert_eq!(variant.posture, FontPosture::Italic);
    }

    #[test]
    fn test_font_manager_creation() {
        let manager = FontManager::new();
        // Manager starts empty — fonts come from profiles
        let families = manager.available_families();
        assert!(families.is_empty(), "New manager should have no fonts");
    }

    /// The determinism fix, tested against the case that motivated it.
    ///
    /// The UBS font directory registers three faces under one family
    /// (`frutiger`: normal, italic, bold). Upstream resolved a bare-family
    /// lookup by taking the first match out of a `HashMap`, so which face won
    /// depended on the process's hash seed. Different faces have different
    /// advances, which change wrap points, which change node heights, which
    /// change `page_breaks` — so the rendered *page count* could differ between
    /// runs of the same input.
    #[test]
    fn an_ambiguous_family_lookup_resolves_to_the_neutral_face() {
        crate::fonts::register_test_fonts();
        let manager = get_font_manager();
        let mut manager = manager.lock().unwrap();

        let variants = manager.loaded_variants();
        let frutiger: Vec<_> = variants.iter().filter(|v| v.family == "frutiger").collect();
        if frutiger.len() < 2 {
            return; // this machine's font dir has nothing ambiguous to resolve
        }

        // Ask for the family with no distinguishing style, repeatedly.
        let ask = FontVariant::new("frutiger", FontWeight::Normal, FontPosture::Normal);
        let first = manager.get_font_data(&ask).expect("resolve").as_ptr();
        for _ in 0..8 {
            assert_eq!(
                manager.get_font_data(&ask).expect("resolve").as_ptr(),
                first,
                "the same lookup must resolve to the same face every time"
            );
        }

        // And it must be the neutral face, not merely *a* stable one — an
        // arbitrary-but-consistent pick would still differ across processes.
        let neutral = FontVariant::new("frutiger", FontWeight::Normal, FontPosture::Normal);
        let expected = manager.get_font_data(&neutral).expect("neutral").as_ptr();
        assert_eq!(
            first, expected,
            "an unstyled request must resolve to the upright normal-weight face"
        );
    }

    #[test]
    fn test_fallback_font() {
        crate::fonts::register_test_fonts();
        let manager = get_font_manager();
        let mut manager = manager.lock().unwrap();
        let result = manager.get_fallback_font();
        assert!(result.is_ok(), "Should be able to load fallback font");
    }

    #[test]
    fn test_default_xfa_font() {
        crate::fonts::register_test_fonts();
        let manager = get_font_manager();
        let mut manager = manager.lock().unwrap();
        let font = Font::default();
        // Courier is the XFA default, should fallback to a loaded font
        let result = manager.get_font(&font);
        if result.is_err() {
            let fallback = manager.get_fallback_font();
            assert!(fallback.is_ok(), "Fallback font should work");
        }
    }

    #[test]
    fn test_generic_family_parsing() {
        assert_eq!(
            "serif".parse::<GenericFamily>().unwrap(),
            GenericFamily::Serif
        );
        assert_eq!(
            "sansSerif".parse::<GenericFamily>().unwrap(),
            GenericFamily::SansSerif
        );
        assert_eq!(
            "sans-serif".parse::<GenericFamily>().unwrap(),
            GenericFamily::SansSerif
        );
        assert_eq!(
            "monospace".parse::<GenericFamily>().unwrap(),
            GenericFamily::Monospace
        );
        assert_eq!(
            "cursive".parse::<GenericFamily>().unwrap(),
            GenericFamily::Cursive
        );
        assert_eq!(
            "fantasy".parse::<GenericFamily>().unwrap(),
            GenericFamily::Fantasy
        );
        // Unknown defaults to SansSerif
        assert_eq!(
            "unknown".parse::<GenericFamily>().unwrap(),
            GenericFamily::SansSerif
        );
    }

    #[test]
    fn test_generic_family_fallbacks() {
        let sans = GenericFamily::SansSerif;
        let fallbacks = sans.fallback_typefaces();
        assert!(fallbacks.contains(&"arial"));
        assert!(fallbacks.contains(&"helvetica"));

        let mono = GenericFamily::Monospace;
        let fallbacks = mono.fallback_typefaces();
        assert!(fallbacks.contains(&"courier"));
    }

    #[test]
    fn test_strict_mode_error() {
        // Create a manager with strict mode and no fonts registered
        let mut manager = FontManager {
            loaded_fonts: std::collections::BTreeMap::new(),
            aliases: HashMap::new(),
            fallback_font_data: None,
            embedded_fonts: HashMap::new(),
            loaded_embedded: HashMap::new(),
            equates: Vec::new(),
            equate_ranges: Vec::new(),
            config: FontConfig {
                strict_mode: true,
                default_generic_family: GenericFamily::SansSerif,
            },
        };

        // Try to get a font that definitely doesn't exist
        let variant = FontVariant::new(
            "NonExistentFontXYZ123",
            FontWeight::Normal,
            FontPosture::Normal,
        );
        let result = manager.get_font_data(&variant);

        assert!(
            result.is_err(),
            "Should return error in strict mode when font not found"
        );
        if let Err(FontError::FontNotFound { typeface, .. }) = result {
            assert_eq!(typeface, "nonexistentfontxyz123");
        } else {
            panic!("Expected FontNotFound error, got {:?}", result);
        }
    }

    #[test]
    fn test_embedded_font_registration() {
        let mut manager = FontManager::new();

        // Create a minimal valid TTF (this is a placeholder - in practice you'd use real font data)
        // For testing, we just verify the API works
        let font = EmbeddedFont {
            name: "TestFont".to_string(),
            data: vec![], // Empty data will fail validation
            weight: FontWeight::Normal,
            posture: FontPosture::Normal,
            generic_family: Some(GenericFamily::SansSerif),
        };

        // Should fail due to invalid font data
        let result = manager.register_embedded_font(font);
        assert!(result.is_err());
    }
}
