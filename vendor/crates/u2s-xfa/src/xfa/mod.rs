pub mod acrobat_compat;
pub mod font_manager;
pub mod hyphenation;
pub mod caption;
pub mod field_meta;
pub mod instances;
pub mod locale;
pub mod pagination;
pub mod picture;
pub mod script_executor;
pub mod scripting;
pub mod shapes;
pub mod text_metrics;

use quick_xml::Reader;
use quick_xml::events::Event;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;

/// Numeric type alias for precision - using Decimal for arbitrary precision
pub type Num = Decimal;

/// Helper to create a Num from a float literal
pub fn num(value: f64) -> Num {
    Decimal::from_str(&value.to_string()).unwrap_or(Decimal::ZERO)
}

/// Border edge stroke style
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum StrokeStyle {
    #[default]
    Solid,
    Dashed,
    Dotted,
    DashDot,
    DashDotDot,
    Lowered,
    Raised,
    Etched,
    Embossed,
}

impl FromStr for StrokeStyle {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "solid" => StrokeStyle::Solid,
            "dashed" => StrokeStyle::Dashed,
            "dotted" => StrokeStyle::Dotted,
            "dashDot" => StrokeStyle::DashDot,
            "dashDotDot" => StrokeStyle::DashDotDot,
            "lowered" => StrokeStyle::Lowered,
            "raised" => StrokeStyle::Raised,
            "etched" => StrokeStyle::Etched,
            "embossed" => StrokeStyle::Embossed,
            _ => StrokeStyle::Solid,
        })
    }
}

/// Corner join style for borders
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum JoinStyle {
    #[default]
    Square,
    Round,
}

impl FromStr for JoinStyle {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "round" => JoinStyle::Round,
            _ => JoinStyle::Square,
        })
    }
}

/// Border edge properties
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Edge {
    pub thickness: Option<Num>,
    pub stroke: StrokeStyle,
    pub presence: String, // "visible", "hidden", "inactive"
    pub color: Option<(u8, u8, u8)>,
}

/// Border corner properties
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Corner {
    pub thickness: Option<Num>,
    pub join: JoinStyle,
    pub presence: String,
    /// How far each adjoining edge is trimmed back from the vertex.
    pub radius: Option<Num>,
    /// `inverted="1"`: the corner is concave, joining the edges inward.
    pub inverted: bool,
    pub stroke: StrokeStyle,
    /// The corner's own `<color>`; the adjoining edge's when absent.
    pub color: Option<(u8, u8, u8)>,
}

/// Border properties
/// Per XFA spec: border has up to 4 edges (top, right, bottom, left) and 4 corners (top-left, top-right, bottom-right, bottom-left)
/// If fewer than 4 are specified, the last one is reused
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Border {
    pub edges: Vec<Edge>,
    pub corners: Vec<Corner>,
    pub fill: Option<Fill>,
    pub presence: String,
    /// Border margins (insets from the widget edge)
    pub margin_left: Option<Num>,
    pub margin_top: Option<Num>,
    pub margin_right: Option<Num>,
    pub margin_bottom: Option<Num>,
    /// Optional render bounds override for edges (x, y, width, height).
    /// When set, edges are rendered at the parent subform's position:
    /// - Top edge at (x, y) spanning width
    /// - Bottom edge at (x, y + height) spanning width
    /// - Left/right edges use x and height
    ///
    /// This is used when a subform border needs to span multiple child cells.
    pub render_bounds: Option<(Num, Num, Num, Num)>,
}

impl Border {
    /// Get edge for a specific side (0=top, 1=right, 2=bottom, 3=left)
    pub fn get_edge(&self, index: usize) -> Option<&Edge> {
        if self.edges.is_empty() {
            None
        } else {
            // If fewer than 4 edges, reuse the last one
            Some(&self.edges[index.min(self.edges.len() - 1)])
        }
    }

    /// Get corner for a specific position (0=top-left, 1=top-right, 2=bottom-right, 3=bottom-left)
    pub fn get_corner(&self, index: usize) -> Option<&Corner> {
        if self.corners.is_empty() {
            None
        } else {
            Some(&self.corners[index.min(self.corners.len() - 1)])
        }
    }

    /// Check if the border should be rendered
    pub fn is_visible(&self) -> bool {
        self.presence != "hidden" && self.presence != "inactive" && !self.edges.is_empty()
    }

    /// Create a new Border with the specified edge indices hidden.
    ///
    /// Per XFA spec: "If fewer than four edge or corner elements are supplied
    /// the last element is reused for the remaining edges or corners."
    /// This method materialises all 4 edges (applying the reuse rule) and then
    /// sets the requested edges to `presence = "hidden"`.
    pub fn with_edges_hidden(&self, indices: &[usize]) -> Border {
        // Materialise all 4 edges using the XFA reuse rule
        let mut edges: Vec<Edge> = (0..4)
            .map(|i| self.get_edge(i).cloned().unwrap_or_default())
            .collect();

        for &idx in indices {
            if idx < 4 {
                edges[idx].presence = "hidden".to_string();
            }
        }

        Border {
            edges,
            corners: self.corners.clone(),
            fill: self.fill.clone(),
            presence: self.presence.clone(),
            margin_left: self.margin_left,
            margin_top: self.margin_top,
            margin_right: self.margin_right,
            margin_bottom: self.margin_bottom,
            render_bounds: self.render_bounds,
        }
    }
}

/// Fill properties for backgrounds
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fill {
    /// The start (or background) color; white when the fill has none.
    pub color: Option<(u8, u8, u8)>,
    pub presence: String,
    /// The fill type element (XFA 3.3 "The fill element"); solid when the
    /// fill has none.
    pub kind: FillKind,
}

/// A fill's type element: how the region is painted from the fill's own
/// color (XFA 3.3 "The fill element").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillKind {
    #[default]
    Solid,
    Linear(LinearGradient),
    /// `<radial>`: the start color at the center, `end` at the edges, or
    /// the reverse for `type="toCenter"`.
    Radial { end: (u8, u8, u8), to_center: bool },
    /// `<pattern>`: `color` strokes hatched over the background.
    Pattern { color: (u8, u8, u8), kind: PatternKind },
    /// `<stipple>`: `rate` percent of the area stippled in `color`.
    Stipple { color: (u8, u8, u8), rate: u8 },
}

/// The hatching of a `<pattern>` fill (XFA 3.3 "The pattern element").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PatternKind {
    #[default]
    CrossHatch,
    CrossDiagonal,
    DiagonalLeft,
    DiagonalRight,
    Horizontal,
    Vertical,
}

/// The direction a `<linear>` fill runs in, from its start color to its end
/// color (XFA 3.3 "The linear element").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GradientDirection {
    #[default]
    ToRight,
    ToBottom,
    ToLeft,
    ToTop,
}

/// A `<linear>` gradient fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinearGradient {
    pub direction: GradientDirection,
    /// The `<linear>`'s own `<color>`. Black when it has none: the spec
    /// leaves that default to the context, and Designer always writes one.
    pub end: (u8, u8, u8),
}

/// Generic font family for fallback per XFA spec
/// Per XFA spec section 28: "The genericFamily attribute values are defined by [CSS2]."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GenericFamily {
    /// Sans-serif fonts (no serifs) - this is the default
    #[default]
    SansSerif,
    /// Serif fonts (with serifs)
    Serif,
    /// Cursive/script fonts (handwritten look)
    Cursive,
    /// Fantasy/decorative fonts
    Fantasy,
    /// Monospace/fixed-width fonts
    Monospace,
}

impl FromStr for GenericFamily {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "serif" => GenericFamily::Serif,
            "sansserif" | "sans-serif" => GenericFamily::SansSerif,
            "cursive" => GenericFamily::Cursive,
            "fantasy" => GenericFamily::Fantasy,
            "monospace" => GenericFamily::Monospace,
            _ => GenericFamily::SansSerif, // Default per XFA spec
        })
    }
}

impl GenericFamily {
    /// Get XFA attribute value string
    pub fn as_xfa_str(&self) -> &'static str {
        match self {
            GenericFamily::SansSerif => "sansSerif",
            GenericFamily::Serif => "serif",
            GenericFamily::Cursive => "cursive",
            GenericFamily::Fantasy => "fantasy",
            GenericFamily::Monospace => "monospace",
        }
    }
}

/// Font properties
/// Per XFA spec section 17 (Template Reference - font element)
#[derive(Debug, Clone, PartialEq)]
pub struct Font {
    pub typeface: String,
    pub size: Num,
    pub weight: FontWeight,
    pub posture: FontPosture,
    pub underline: bool,
    pub line_through: bool,
    /// `underline="2"`: a double underline.
    pub underline_double: bool,
    /// `lineThrough="2"`: a double line through.
    pub line_through_double: bool,
    /// `underlinePeriod="word"`: the underline breaks at word breaks.
    pub underline_period_word: bool,
    pub color: Option<(u8, u8, u8)>,
    pub baseline_shift: Option<Num>,
    /// Letter spacing adjustment (relative measurement, can be negative)
    /// Per XFA spec: "specifies an adjustment to the spacing that would otherwise
    /// be used between successive grapheme clusters. Interword as well as
    /// interletter spacings are affected." Default is 0.
    pub letter_spacing: Option<Num>,
    /// Generic font family for fallback when typeface is not available
    /// Per XFA spec section 28 (Font Mapping): used to select appropriate fallback
    pub generic_family: Option<GenericFamily>,
    /// Kerning mode per XFA spec
    /// Per XFA spec: \"kerningMode\" attribute controls pair kerning
    /// - \"none\": No kerning applied (default)
    /// - \"pair\": Apply pair kerning from font tables
    pub kerning_mode: KerningMode,
    /// Horizontal scaling factor (percentage, default 100%)
    /// Per XFA spec: \"fontHorizontalScale\" - geometric scaling applied to glyphs
    pub font_horizontal_scale: Option<Num>,
    /// Vertical scaling factor (percentage, default 100%)
    /// Per XFA spec: \"fontVerticalScale\" - geometric scaling applied to glyphs
    pub font_vertical_scale: Option<Num>,
}

/// Kerning mode per XFA spec
/// Controls whether pair kerning from font tables is applied
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum KerningMode {
    /// No kerning applied (default per XFA spec)
    #[default]
    None,
    /// Apply pair kerning from font GPOS/kern tables
    Pair,
}

impl FromStr for KerningMode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "pair" => KerningMode::Pair,
            _ => KerningMode::None,
        })
    }
}

impl Default for Font {
    fn default() -> Self {
        // Per XFA spec section 17 "Template Reference":
        // - typeface: Default is "Courier"
        // - size: Default is 10pt
        // - weight: Default is "normal"
        // - posture: Default is "normal"
        // - letterSpacing: Default is 0
        // - kerningMode: Default is "none"
        // - fontHorizontalScale: Default is 100%
        // - fontVerticalScale: Default is 100%
        Font {
            typeface: "Courier".to_string(),
            size: num(10.0), // 10pt default
            weight: FontWeight::Normal,
            posture: FontPosture::Normal,
            underline: false,
            line_through: false,
            underline_double: false,
            line_through_double: false,
            underline_period_word: false,
            color: None,
            baseline_shift: None,
            letter_spacing: None, // 0 default (no adjustment)
            generic_family: Some(GenericFamily::Monospace), // Courier is monospace
            kerning_mode: KerningMode::None,
            font_horizontal_scale: None, // 100% default
            font_vertical_scale: None,   // 100% default
        }
    }
}

// `PartialOrd, Ord` so a `FontVariant` can key an ordered map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
/// Font weight per XFA spec (normal/bold) with extended internal weights for font matching
/// XFA only specifies "normal" or "bold", but we support finer weights internally
/// to better match fonts like "Frutiger 45 Light"
pub enum FontWeight {
    Thin,       // 100
    ExtraLight, // 200
    Light,      // 300
    #[default]
    Normal, // 400
    Medium,     // 500
    SemiBold,   // 600
    Bold,       // 700
    ExtraBold,  // 800
    Black,      // 900
}

impl FontWeight {
    /// Convert from numeric weight (100-900 scale, CSS/OpenType standard)
    pub fn from_numeric(weight: u16) -> Self {
        match weight {
            0..=149 => FontWeight::Thin,
            150..=249 => FontWeight::ExtraLight,
            250..=349 => FontWeight::Light,
            350..=449 => FontWeight::Normal,
            450..=549 => FontWeight::Medium,
            550..=649 => FontWeight::SemiBold,
            650..=749 => FontWeight::Bold,
            750..=849 => FontWeight::ExtraBold,
            _ => FontWeight::Black,
        }
    }

    /// Convert to numeric weight (100-900 scale)
    pub fn to_numeric(self) -> u16 {
        match self {
            FontWeight::Thin => 100,
            FontWeight::ExtraLight => 200,
            FontWeight::Light => 300,
            FontWeight::Normal => 400,
            FontWeight::Medium => 500,
            FontWeight::SemiBold => 600,
            FontWeight::Bold => 700,
            FontWeight::ExtraBold => 800,
            FontWeight::Black => 900,
        }
    }

    /// Convert to XFA-compatible weight (only normal or bold per spec)
    pub fn to_xfa(self) -> FontWeight {
        if self.to_numeric() >= 600 {
            FontWeight::Bold
        } else {
            FontWeight::Normal
        }
    }

    /// Check if this weight is considered "bold" for XFA purposes
    pub fn is_bold(self) -> bool {
        self.to_numeric() >= 600
    }
}

impl FromStr for FontWeight {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "bold" => FontWeight::Bold,
            "thin" | "hairline" => FontWeight::Thin,
            "extralight" | "ultralight" | "extra-light" | "ultra-light" => FontWeight::ExtraLight,
            "light" => FontWeight::Light,
            "medium" => FontWeight::Medium,
            "semibold" | "demibold" | "semi-bold" | "demi-bold" => FontWeight::SemiBold,
            "extrabold" | "ultrabold" | "extra-bold" | "ultra-bold" => FontWeight::ExtraBold,
            "black" | "heavy" => FontWeight::Black,
            _ => FontWeight::Normal,
        })
    }
}

// DEVIATION: `PartialOrd, Ord` added — see FontWeight above.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum FontPosture {
    #[default]
    Normal,
    Italic,
}

impl FromStr for FontPosture {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "italic" => FontPosture::Italic,
            _ => FontPosture::Normal,
        })
    }
}

/// A `<barcode>` (XFA 3.3 "The barcode element"): the options of a 1-d
/// barcode this renderer honours. The 2-d options (`dataRowCount`,
/// `dataColumnCount`, `rowColumnRatio`, `errorCorrectionLevel`, `upsMode`),
/// `dataPrep`, `charEncoding`, `moduleWidth`, `moduleHeight` and `truncate`
/// are not read; a symbology that needs them is refused as unsupported when
/// the barcode is encoded (see [`crate::barcode::encode`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Barcode {
    /// `type`.
    pub symbology: String,
    /// `wideNarrowRatio`: the width of a wide bar in narrow bars. The spec
    /// gives no default; 3.0 is the ratio the 1-d standards this renderer
    /// encodes specify, and what Designer writes.
    pub wide_narrow_ratio: f32,
    pub checksum: BarcodeChecksum,
    /// `dataLength`: the expected most characters, which with the field
    /// width sets the width of a narrow bar; the value is not validated
    /// against it.
    pub data_length: Option<usize>,
    pub text_location: BarcodeTextLocation,
    /// `startChar` and `endChar`: control characters for the symbologies
    /// that allow them; ignored for the others.
    pub start_char: Option<String>,
    pub end_char: Option<String>,
    /// `printCheckDigit`: whether a generated check digit is also in the
    /// human-readable text.
    pub print_check_digit: bool,
}

/// `checksum` of a `<barcode>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BarcodeChecksum {
    #[default]
    None,
    Auto,
    OneMod10,
    TwoMod10,
    OneMod10OneMod11,
}

impl std::str::FromStr for BarcodeChecksum {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s.trim() {
            "none" => Self::None,
            "auto" => Self::Auto,
            "1mod10" => Self::OneMod10,
            "2mod10" => Self::TwoMod10,
            "1mod10_1mod11" => Self::OneMod10OneMod11,
            other => return Err(format!("{other:?} is not a barcode checksum")),
        })
    }
}

/// `textLocation` of a `<barcode>`: where its human-readable text goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BarcodeTextLocation {
    #[default]
    Below,
    Above,
    BelowEmbedded,
    AboveEmbedded,
    None,
}

impl std::str::FromStr for BarcodeTextLocation {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s.trim() {
            "below" => Self::Below,
            "above" => Self::Above,
            "belowEmbedded" => Self::BelowEmbedded,
            "aboveEmbedded" => Self::AboveEmbedded,
            "none" => Self::None,
            other => return Err(format!("{other:?} is not a barcode text location")),
        })
    }
}

impl Barcode {
    /// The options of a `<barcode>` element. A value the spec does not
    /// list is reported and replaced by the property's default.
    pub fn of(node: &XfaNode) -> Barcode {
        let attr = |name: &str| node.attributes.get(name).map(|v| v.trim()).filter(|v| !v.is_empty());
        let symbology = attr("type").unwrap_or("unspecified").to_string();
        let wide_narrow_ratio = match attr("wideNarrowRatio").map(str::parse::<f32>) {
            Some(Ok(ratio)) if ratio >= 1.0 => ratio,
            Some(_) => {
                log::warn!("barcode {symbology}: wideNarrowRatio {:?} is not a ratio of at least 1; using 3.0", attr("wideNarrowRatio").unwrap_or(""));
                3.0
            }
            None => 3.0,
        };
        let checksum = match attr("checksum").map(str::parse) {
            Some(Ok(checksum)) => checksum,
            Some(Err(e)) => {
                log::warn!("barcode {symbology}: {e}; no checksum is inserted");
                BarcodeChecksum::None
            }
            None => BarcodeChecksum::None,
        };
        let data_length = match attr("dataLength").map(str::parse::<usize>) {
            Some(Ok(n)) => Some(n),
            Some(Err(_)) => {
                log::warn!("barcode {symbology}: dataLength {:?} is not a count; the value's own length is used", attr("dataLength").unwrap_or(""));
                None
            }
            None => None,
        };
        let text_location = match attr("textLocation").map(str::parse) {
            Some(Ok(location)) => location,
            Some(Err(e)) => {
                log::warn!("barcode {symbology}: {e}; the text is placed below");
                BarcodeTextLocation::Below
            }
            None => BarcodeTextLocation::Below,
        };
        Barcode {
            symbology,
            wide_narrow_ratio,
            checksum,
            data_length,
            text_location,
            start_char: attr("startChar").map(str::to_string),
            end_char: attr("endChar").map(str::to_string),
            print_check_digit: attr("printCheckDigit") == Some("1"),
        }
    }
}

/// A measurement as written (XFA 3.3 §2 "Measurements"): absolute, in
/// points, or relative to the current font.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measurement {
    Absolute(Num),
    /// `em`: em widths of the current font (its size).
    Em(Num),
    /// `%`: a percentage of the width of a space in the current font.
    PercentOfSpace(Num),
}

/// A field's `<checkButton>` appearance (XFA 3.3 "The checkButton element").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckButton {
    /// The button's side, in points.
    pub size: Num,
    /// Its appearance when asserted.
    pub mark: CheckMark,
    /// `shape="round"`: a circular outline, a radio button's.
    pub round: bool,
}

/// How a check button looks when asserted (XFA 3.3 "The mark property").
/// `Default` is a corner-to-corner X in a square button and a filled circle
/// in a round one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CheckMark {
    #[default]
    Default,
    Check,
    Circle,
    Cross,
    Diamond,
    Square,
    Star,
}

/// Paragraph properties for text alignment and spacing
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Para {
    pub h_align: HAlign,
    pub v_align: VAlign,
    pub line_height: Option<Num>,
    pub space_above: Option<Num>,
    pub space_below: Option<Num>,
    pub text_indent: Option<Num>,
    pub margin_left: Option<Num>,
    pub margin_right: Option<Num>,
    /// `radixOffset`: with `hAlign="radix"`, how far from the region's right
    /// edge the radix character's left edge is placed (XFA 3.3 §2
    /// "Horizontal alignment").
    pub radix_offset: Option<Num>,
    /// Auto-hyphenation settings from child `<hyphenation>` element.
    pub hyphenation: Option<hyphenation::XfaHyphenation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum HAlign {
    #[default]
    Left,
    Center,
    Right,
    Justify,
    JustifyAll,
    Radix, // Align on decimal point
}

impl std::str::FromStr for HAlign {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "center" => HAlign::Center,
            "right" => HAlign::Right,
            "justify" => HAlign::Justify,
            "justifyAll" => HAlign::JustifyAll,
            "radix" => HAlign::Radix,
            _ => HAlign::Left,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum VAlign {
    #[default]
    Top,
    Middle,
    Bottom,
}

impl std::str::FromStr for VAlign {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "middle" => VAlign::Middle,
            "bottom" => VAlign::Bottom,
            _ => VAlign::Top,
        })
    }
}

/// Node presence values per XFA spec
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Presence {
    #[default]
    Visible,
    Invisible,
    Hidden,
    Inactive,
}

impl std::str::FromStr for Presence {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "invisible" => Presence::Invisible,
            "hidden" => Presence::Hidden,
            "inactive" => Presence::Inactive,
            _ => Presence::Visible,
        })
    }
}

impl Presence {
    /// Returns the XFA string representation of this presence value
    pub fn as_str(&self) -> &'static str {
        match self {
            Presence::Visible => "visible",
            Presence::Invisible => "invisible",
            Presence::Hidden => "hidden",
            Presence::Inactive => "inactive",
        }
    }

    /// Returns true if this presence should skip rendering
    pub fn should_skip_render(&self) -> bool {
        matches!(
            self,
            Presence::Hidden | Presence::Invisible | Presence::Inactive
        )
    }

    /// Returns true if this presence should skip layout (not take up space)
    pub fn should_skip_layout(&self) -> bool {
        matches!(self, Presence::Hidden | Presence::Inactive)
    }

    /// Returns true if this presence restricts behavior compared to visible.
    /// Per XFA spec §2: invisible, hidden, and inactive all restrict phases of processing.
    pub fn is_restricted(&self) -> bool {
        !matches!(self, Presence::Visible)
    }

    /// Returns the more restrictive of two presence values.
    /// Per XFA spec §2: inner containers inherit the outer container's restricted behavior.
    /// Ordering (least to most restrictive): Visible < Invisible < Hidden < Inactive.
    pub fn most_restrictive(self, other: Presence) -> Presence {
        let rank = |p: &Presence| -> u8 {
            match p {
                Presence::Visible => 0,
                Presence::Invisible => 1,
                Presence::Hidden => 2,
                Presence::Inactive => 3,
            }
        };
        if rank(&self) >= rank(&other) {
            self
        } else {
            other
        }
    }
}

/// Main XFA node structure containing layout information and node-specific data
#[derive(Debug, Clone)]
pub struct XfaNode {
    /// Node-specific information
    pub kind: XfaNodeKind,

    /// Layout-related attributes (parsed from attributes)
    pub x: Option<Num>,
    pub y: Option<Num>,
    pub w: Option<Num>,
    pub h: Option<Num>,
    pub min_w: Option<Num>,
    pub min_h: Option<Num>,
    pub max_w: Option<Num>,
    pub max_h: Option<Num>,
    pub layout: Option<String>,
    /// Rotation in degrees (counter-clockwise, multiples of 90)
    pub rotate: i32,

    /// Margin insets (parsed from child <margin> element)
    pub margin_top: Option<Num>,
    pub margin_bottom: Option<Num>,
    pub margin_left: Option<Num>,
    pub margin_right: Option<Num>,

    /// Border, font, and paragraph properties (parsed from child elements)
    pub border: Option<Border>,
    pub font: Option<Font>,
    pub para: Option<Para>,

    /// Node name (for named nodes like subforms, fields, etc.)
    pub name: Option<String>,

    /// Presence attribute (can be modified by scripts)
    pub presence: Presence,

    /// All attributes from the XML (including layout attrs)
    pub attributes: HashMap<String, String>,

    /// Child nodes
    pub children: Vec<XfaNode>,

    /// A `<field>`'s pictures, length, lines, tooltip and null test, with
    /// its prevailing locale. Set by [`XfaNode::parse`] for every field and
    /// `None` on every other node; see [`field_meta`].
    pub field_meta: Option<Box<field_meta::FieldMeta>>,
}

/// Enum representing the specific kind/type of XFA node
#[derive(Debug, Clone, PartialEq)]
pub enum XfaNodeKind {
    /// Template node (root of form template)
    Template,

    /// Subform container
    Subform,

    /// Input field
    Field,

    /// Page set container
    PageSet,

    /// Page area definition
    PageArea,

    /// Content area within a page
    ContentArea,

    /// Draw/static text element
    Draw,

    /// Value container
    Value,

    /// Text content
    Text { content: String },

    /// Bind element
    Bind,

    /// Exclusion group container (radio button group)
    ExclGroup,

    /// Generic/unknown element
    Element {
        tag_name: String,
        text_content: Option<String>,
    },
}

/// The prototypes a template offers (see `XfaNode::resolve_use_prototypes`).
#[derive(Default)]
struct Prototypes {
    by_id: HashMap<String, XfaNode>,
    by_path: HashMap<String, XfaNode>,
}

impl Prototypes {
    fn lookup(&self, reference: &str) -> Option<&XfaNode> {
        match reference.strip_prefix('#') {
            Some(id) => self.by_id.get(id),
            None => self.by_path.get(reference),
        }
    }
}

impl XfaNodeKind {
    /// The element's tag: what a prototype reference must match.
    pub fn tag_name(&self) -> &str {
        match self {
            XfaNodeKind::Element { tag_name, .. } => tag_name.as_str(),
            XfaNodeKind::Field => "field",
            XfaNodeKind::Subform => "subform",
            XfaNodeKind::Draw => "draw",
            XfaNodeKind::ExclGroup => "exclGroup",
            XfaNodeKind::Value => "value",
            XfaNodeKind::Template => "template",
            XfaNodeKind::PageSet => "pageSet",
            XfaNodeKind::PageArea => "pageArea",
            XfaNodeKind::ContentArea => "contentArea",
            XfaNodeKind::Bind => "bind",
            XfaNodeKind::Text { .. } => "#text",
        }
    }

    /// Returns `true` if this kind represents a subform (dedicated or generic element).
    pub fn is_subform(&self) -> bool {
        matches!(self, XfaNodeKind::Subform)
            || matches!(self, XfaNodeKind::Element { tag_name, .. } if tag_name == "subform")
    }

    /// Returns `true` if this kind represents a field (dedicated or generic element).
    pub fn is_field(&self) -> bool {
        matches!(self, XfaNodeKind::Field)
            || matches!(self, XfaNodeKind::Element { tag_name, .. } if tag_name == "field")
    }

    /// Returns `true` if this kind represents an exclusion group (dedicated or generic element).
    pub fn is_exclgroup(&self) -> bool {
        matches!(self, XfaNodeKind::ExclGroup)
            || matches!(self, XfaNodeKind::Element { tag_name, .. } if tag_name == "exclGroup")
    }

    /// Returns `true` if this kind represents a draw element (dedicated or generic element).
    pub fn is_draw(&self) -> bool {
        matches!(self, XfaNodeKind::Draw)
            || matches!(self, XfaNodeKind::Element { tag_name, .. } if tag_name == "draw")
    }

    /// Whether a named node of this kind is a container that the SOM paths
    /// of its descendants pass through (XFA 3.3 §3 "Scripting Object
    /// Model"): a subform, an exclusion group, a page set or a page area --
    /// `form1.pageSet.Page1.Footer` names a footer on master page `Page1`.
    /// An unnamed node of any kind is transparent: its children are
    /// addressed as children of its parent. The one rule every walker that
    /// builds SOM paths shares.
    pub fn extends_som_path(&self) -> bool {
        self.is_subform()
            || self.is_exclgroup()
            || matches!(self, XfaNodeKind::PageSet | XfaNodeKind::PageArea)
    }

    /// Map an XML tag name to the corresponding `XfaNodeKind`.
    ///
    /// Known XFA tags are mapped to their dedicated variants; everything else
    /// becomes `Element` with the tag name preserved.
    fn from_tag(tag_name: &str) -> Self {
        match tag_name {
            "template" => XfaNodeKind::Template,
            "subform" => XfaNodeKind::Subform,
            "field" => XfaNodeKind::Field,
            "pageSet" => XfaNodeKind::PageSet,
            "pageArea" => XfaNodeKind::PageArea,
            "contentArea" => XfaNodeKind::ContentArea,
            "draw" => XfaNodeKind::Draw,
            "value" => XfaNodeKind::Value,
            "bind" => XfaNodeKind::Bind,
            "exclGroup" => XfaNodeKind::ExclGroup,
            _ => XfaNodeKind::Element {
                tag_name: tag_name.to_string(),
                text_content: None,
            },
        }
    }
}

impl XfaNode {
    /// Create a new XFA node
    pub fn new(kind: XfaNodeKind, attributes: HashMap<String, String>) -> Self {
        // Extract layout-related attributes
        let x = attributes
            .get("x")
            .and_then(|v| Self::parse_dimension(v).ok());
        let y = attributes
            .get("y")
            .and_then(|v| Self::parse_dimension(v).ok());
        let w = attributes
            .get("w")
            .and_then(|v| Self::parse_dimension(v).ok());
        let h = attributes
            .get("h")
            .and_then(|v| Self::parse_dimension(v).ok());
        let min_w = attributes
            .get("minW")
            .and_then(|v| Self::parse_dimension(v).ok());
        let min_h = attributes
            .get("minH")
            .and_then(|v| Self::parse_dimension(v).ok());
        let max_w = attributes
            .get("maxW")
            .and_then(|v| Self::parse_dimension(v).ok());
        let max_h = attributes
            .get("maxH")
            .and_then(|v| Self::parse_dimension(v).ok());
        let layout = attributes.get("layout").cloned();
        let rotate = attributes
            .get("rotate")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0);
        // XFA 3.3 §3 "Area Objects Are Always Transparent": an area "has no
        // consequences at run time" and "areas are always transparent to
        // SOM expressions even if the areas have names" (and to data
        // binding, §6). Its name is dropped here, so every path built or
        // walked in this crate sees through it: the engine, the flattener
        // and the control listing then agree on
        // `MP.LastPageOnly.STP_UP.Date` for a field inside the area `UP1`.
        let name = match &kind {
            XfaNodeKind::Element { tag_name, .. } if tag_name == "area" => None,
            _ => attributes.get("name").cloned(),
        };
        let presence = attributes
            .get("presence")
            .and_then(|s| s.parse().ok())
            .unwrap_or(Presence::Visible);

        XfaNode {
            kind,
            x,
            y,
            w,
            h,
            min_w,
            min_h,
            max_w,
            max_h,
            layout,
            rotate,
            margin_top: None,
            margin_bottom: None,
            margin_left: None,
            margin_right: None,
            border: None,
            font: None,
            para: None,
            name,
            presence,
            attributes,
            children: Vec::new(),
            field_meta: None,
        }
    }

    /// The value the template gives this node: its `<value>`'s content
    /// (`<text>`, `<date>`, `<integer>`, ...), the field's initial value in a
    /// merge with no data (XFA 3.3 §4). `None` when it has none or it is
    /// empty.
    pub fn template_value(&self) -> Option<String> {
        self.children
            .iter()
            .filter(|c| matches!(c.kind, XfaNodeKind::Value))
            .flat_map(|v| v.children.iter())
            .find_map(|content| match &content.kind {
                XfaNodeKind::Text { content } if !content.is_empty() => Some(content.clone()),
                XfaNodeKind::Element {
                    text_content: Some(content),
                    ..
                } if !content.is_empty() => Some(content.clone()),
                _ => None,
            })
    }

    /// Extract margins from child <margin> element after parsing children
    pub fn extract_margins_from_children(&mut self) {
        for child in &self.children {
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                && tag_name == "margin"
            {
                self.margin_top = child
                    .attributes
                    .get("topInset")
                    .and_then(|v| Self::parse_dimension(v).ok());
                self.margin_bottom = child
                    .attributes
                    .get("bottomInset")
                    .and_then(|v| Self::parse_dimension(v).ok());
                self.margin_left = child
                    .attributes
                    .get("leftInset")
                    .and_then(|v| Self::parse_dimension(v).ok());
                self.margin_right = child
                    .attributes
                    .get("rightInset")
                    .and_then(|v| Self::parse_dimension(v).ok());
                break; // Only one margin element expected
            }
        }
    }

    /// Extract border, font, and para from child elements after parsing children
    pub fn extract_styling_from_children(&mut self) {
        // Extract margins first
        self.extract_margins_from_children();

        for child in &self.children {
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind {
                match tag_name.as_str() {
                    "border" => {
                        self.border = Some(Self::parse_border(child));
                    }
                    "font" => {
                        self.font = Some(Self::parse_font(child));
                    }
                    "para" => {
                        self.para = Some(Self::parse_para(child));
                    }
                    _ => {}
                }
            }
        }
    }

    /// Parse a <border> element
    fn parse_border(node: &XfaNode) -> Border {
        let mut border = Border {
            presence: node
                .attributes
                .get("presence")
                .cloned()
                .unwrap_or_else(|| "visible".to_string()),
            ..Default::default()
        };

        for child in &node.children {
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind {
                match tag_name.as_str() {
                    "edge" => {
                        border.edges.push(Self::parse_edge(child));
                    }
                    "corner" => {
                        border.corners.push(Self::parse_corner(child));
                    }
                    "fill" => {
                        border.fill = Some(Self::parse_fill(child));
                    }
                    "margin" => {
                        border.margin_top = child
                            .attributes
                            .get("topInset")
                            .and_then(|v| Self::parse_dimension(v).ok());
                        border.margin_bottom = child
                            .attributes
                            .get("bottomInset")
                            .and_then(|v| Self::parse_dimension(v).ok());
                        border.margin_left = child
                            .attributes
                            .get("leftInset")
                            .and_then(|v| Self::parse_dimension(v).ok());
                        border.margin_right = child
                            .attributes
                            .get("rightInset")
                            .and_then(|v| Self::parse_dimension(v).ok());
                    }
                    _ => {}
                }
            }
        }

        border
    }

    /// Parse an <edge> element
    pub(crate) fn parse_edge(node: &XfaNode) -> Edge {
        Edge {
            thickness: node
                .attributes
                .get("thickness")
                .and_then(|v| Self::parse_dimension(v).ok()),
            stroke: node
                .attributes
                .get("stroke")
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
            presence: node
                .attributes
                .get("presence")
                .cloned()
                .unwrap_or_else(|| "visible".to_string()),
            color: Self::parse_color_from_children(&node.children),
        }
    }

    /// Parse a <corner> element
    pub(crate) fn parse_corner(node: &XfaNode) -> Corner {
        Corner {
            thickness: node
                .attributes
                .get("thickness")
                .and_then(|v| Self::parse_dimension(v).ok()),
            join: node
                .attributes
                .get("join")
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
            presence: node
                .attributes
                .get("presence")
                .cloned()
                .unwrap_or_else(|| "visible".to_string()),
            radius: node
                .attributes
                .get("radius")
                .and_then(|v| Self::parse_dimension(v).ok()),
            inverted: node.attributes.get("inverted").map(String::as_str) == Some("1"),
            stroke: node
                .attributes
                .get("stroke")
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
            color: Self::parse_color_from_children(&node.children),
        }
    }

    /// Parse a <fill> element
    pub(crate) fn parse_fill(node: &XfaNode) -> Fill {
        // The type element's own `<color>` is its end, stroke or stipple
        // color; black when it has none (the spec leaves that default to the
        // context, and Designer always writes one).
        let own_color = |child: &XfaNode| Self::parse_color_from_children(&child.children).unwrap_or((0, 0, 0));
        let kind = node
            .children
            .iter()
            .find_map(|child| {
                let XfaNodeKind::Element { tag_name, .. } = &child.kind else {
                    return None;
                };
                let attr = |name: &str| child.attributes.get(name).map(String::as_str);
                Some(match tag_name.as_str() {
                    "solid" => FillKind::Solid,
                    "linear" => FillKind::Linear(LinearGradient {
                        direction: match attr("type") {
                            Some("toBottom") => GradientDirection::ToBottom,
                            Some("toLeft") => GradientDirection::ToLeft,
                            Some("toTop") => GradientDirection::ToTop,
                            _ => GradientDirection::ToRight,
                        },
                        end: own_color(child),
                    }),
                    "radial" => FillKind::Radial {
                        end: own_color(child),
                        to_center: attr("type") == Some("toCenter"),
                    },
                    "pattern" => FillKind::Pattern {
                        color: own_color(child),
                        kind: match attr("type") {
                            None | Some("crossHatch") => PatternKind::CrossHatch,
                            Some("crossDiagonal") => PatternKind::CrossDiagonal,
                            Some("diagonalLeft") => PatternKind::DiagonalLeft,
                            Some("diagonalRight") => PatternKind::DiagonalRight,
                            Some("horizontal") => PatternKind::Horizontal,
                            Some("vertical") => PatternKind::Vertical,
                            Some(other) => {
                                log::warn!("unknown pattern type {other:?}; taken as crossHatch");
                                PatternKind::CrossHatch
                            }
                        },
                    },
                    "stipple" => FillKind::Stipple {
                        color: own_color(child),
                        rate: attr("rate")
                            .and_then(|r| r.trim().parse::<i64>().ok())
                            .map(|r| r.clamp(0, 100) as u8)
                            .unwrap_or(50),
                    },
                    _ => return None,
                })
            })
            .unwrap_or_default();
        Fill {
            presence: node
                .attributes
                .get("presence")
                .cloned()
                .unwrap_or_else(|| "visible".to_string()),
            color: Self::parse_color_from_children(&node.children),
            kind,
        }
    }

    /// Parse a <font> element
    /// Per XFA spec section 17: typeface defaults to Courier, size to 10pt
    /// Per XFA spec section 28: genericFamily is used for font fallback
    fn parse_font(node: &XfaNode) -> Font {
        let typeface = node
            .attributes
            .get("typeface")
            .cloned()
            .unwrap_or_else(|| "Courier".to_string());

        // `genericFamily` as written; nothing is inferred from the typeface's
        // spelling (XFA 3.3 chapter 28 leaves the family to the font's own
        // description).
        let generic_family = node
            .attributes
            .get("genericFamily")
            .and_then(|s| s.parse().ok());

        let size = node
            .attributes
            .get("size")
            .and_then(|v| Self::parse_dimension(v).ok())
            .unwrap_or_else(|| num(10.0));

        Font {
            typeface,
            size,
            weight: node
                .attributes
                .get("weight")
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
            posture: node
                .attributes
                .get("posture")
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
            underline: node
                .attributes
                .get("underline")
                .map(|s| s != "0")
                .unwrap_or(false),
            line_through: node
                .attributes
                .get("lineThrough")
                .map(|s| s != "0")
                .unwrap_or(false),
            underline_double: node.attributes.get("underline").map(String::as_str) == Some("2"),
            line_through_double: node.attributes.get("lineThrough").map(String::as_str) == Some("2"),
            underline_period_word: node.attributes.get("underlinePeriod").map(String::as_str) == Some("word"),
            color: Self::parse_color_from_children(&node.children),
            baseline_shift: node
                .attributes
                .get("baselineShift")
                .and_then(|v| Self::parse_dimension(v).ok()),
            // `letterSpacing` is a relative measurement (XFA 3.3 §2). An
            // absolute value applies; an `em` value is not applied, as
            // Acrobat does not apply it (see [`crate::xfa::acrobat_compat`],
            // item 7); `%` (of a space) needs the face's metrics, which this
            // parser does not have, and is reported and left at 0.
            letter_spacing: node.attributes.get("letterSpacing").and_then(|v| {
                match Self::parse_measurement(v) {
                    Ok(Measurement::Absolute(points)) => Some(points),
                    Ok(Measurement::Em(_)) => {
                        log::debug!("letterSpacing {v:?} is relative; not applied (acrobat_compat item 7)");
                        None
                    }
                    Ok(Measurement::PercentOfSpace(_)) => {
                        log::warn!(
                            "letterSpacing {v:?} is a percentage of a space, which is not supported; no letter spacing is applied"
                        );
                        None
                    }
                    Err(e) => {
                        log::warn!("{e}; no letter spacing is applied");
                        None
                    }
                }
            }),
            generic_family,
            // Per XFA spec: kerningMode controls pair kerning ("none" or "pair")
            kerning_mode: node
                .attributes
                .get("kerningMode")
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
            // Per XFA spec: fontHorizontalScale is a percentage (100 = normal)
            font_horizontal_scale: node
                .attributes
                .get("fontHorizontalScale")
                .and_then(|v| Self::parse_dimension(v).ok()),
            // Per XFA spec: fontVerticalScale is a percentage (100 = normal)
            font_vertical_scale: node
                .attributes
                .get("fontVerticalScale")
                .and_then(|v| Self::parse_dimension(v).ok()),
        }
    }

    /// Parse a <para> element
    fn parse_para(node: &XfaNode) -> Para {
        // Look for a <hyphenation> child element
        let hyph = node.children.iter().find_map(|child| {
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind {
                if tag_name == "hyphenation" {
                    return Some(hyphenation::XfaHyphenation::from_attributes(
                        &child.attributes,
                    ));
                }
            }
            None
        });

        Para {
            h_align: node
                .attributes
                .get("hAlign")
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
            v_align: node
                .attributes
                .get("vAlign")
                .and_then(|s| s.parse().ok())
                .unwrap_or_default(),
            line_height: node
                .attributes
                .get("lineHeight")
                .and_then(|v| Self::parse_dimension(v).ok()),
            space_above: node
                .attributes
                .get("spaceAbove")
                .and_then(|v| Self::parse_dimension(v).ok()),
            space_below: node
                .attributes
                .get("spaceBelow")
                .and_then(|v| Self::parse_dimension(v).ok()),
            text_indent: node
                .attributes
                .get("textIndent")
                .and_then(|v| Self::parse_dimension(v).ok()),
            margin_left: node
                .attributes
                .get("marginLeft")
                .and_then(|v| Self::parse_dimension(v).ok()),
            margin_right: node
                .attributes
                .get("marginRight")
                .and_then(|v| Self::parse_dimension(v).ok()),
            radix_offset: node
                .attributes
                .get("radixOffset")
                .and_then(|v| Self::parse_dimension(v).ok()),
            hyphenation: hyph,
        }
    }

    /// Parse color from child <color> element (value attribute like "255,0,0")
    fn parse_color_from_children(children: &[XfaNode]) -> Option<(u8, u8, u8)> {
        for child in children {
            let XfaNodeKind::Element { tag_name, .. } = &child.kind else {
                continue;
            };
            if tag_name == "color"
                && let Some(value) = child.attributes.get("value")
            {
                let parts: Vec<&str> = value.split(',').collect();
                if parts.len() >= 3 {
                    let r = parts[0].trim().parse::<u8>().ok()?;
                    let g = parts[1].trim().parse::<u8>().ok()?;
                    let b = parts[2].trim().parse::<u8>().ok()?;
                    return Some((r, g, b));
                }
            }
            // A <font>'s color can also be given as <fill><color .../></fill>
            // (the form Designer emits for a coloured caption/value, e.g. the
            // AAOV "Form configurator" heading). A <border>'s own <fill> is
            // its background, not text color, but the recursion here only
            // fires when a `<color>` is actually nested inside it, so this is
            // exactly the same shape either way.
            if tag_name == "fill"
                && let Some(color) = Self::parse_color_from_children(&child.children)
            {
                return Some(color);
            }
        }
        None
    }

    /// Parse dimension string to Num (handles units like pt, mm, in)
    ///
    /// A leading `=` is accepted and ignored. XFA 3.3 "Measurements" (p.36)
    /// has no such prefix, but Designer writes it on dimensions it locks
    /// (`h="=0mm"` on table rules, 185 times in the UBS corpus), and Acrobat
    /// reads the value after it. Rejecting it would leave those draws with no
    /// height and their rules undrawn.
    /// The leading `=` is a documented Acrobat deviation; see
    /// [`crate::xfa::acrobat_compat`].
    pub fn parse_dimension(s: &str) -> Result<Num, String> {
        match Self::parse_measurement(s)? {
            Measurement::Absolute(points) => Ok(points),
            relative => Err(format!(
                "{s:?} is a relative measurement ({relative:?}), not allowed here"
            )),
        }
    }

    /// An XFA measurement (XFA 3.3 §2 "Measurements"): a number with a unit
    /// of `in`, `cm`, `mm` or `pt`, or no unit, which is inches ("The
    /// default unit is assumed to be inches"); for a relative measurement
    /// also `em` and `%`. A leading `=` is accepted (see
    /// [`crate::xfa::acrobat_compat`], item 3).
    pub fn parse_measurement(s: &str) -> Result<Measurement, String> {
        let s = s.trim();
        let s = s.strip_prefix('=').map(str::trim_start).unwrap_or(s);
        let number = |value: &str| {
            Decimal::from_str(value.trim())
                .map_err(|e| format!("{s:?} is not a measurement: {e}"))
        };
        let pts_per_inch = Decimal::from(72);
        let pts_per_mm = Decimal::from_str("2.834645669291339").unwrap(); // 72 / 25.4
        let pts_per_cm = Decimal::from_str("28.34645669291339").unwrap(); // 72 / 2.54
        if let Some(v) = s.strip_suffix("pt") {
            Ok(Measurement::Absolute(number(v)?))
        } else if let Some(v) = s.strip_suffix("in") {
            Ok(Measurement::Absolute(number(v)? * pts_per_inch))
        } else if let Some(v) = s.strip_suffix("mm") {
            Ok(Measurement::Absolute(number(v)? * pts_per_mm))
        } else if let Some(v) = s.strip_suffix("cm") {
            Ok(Measurement::Absolute(number(v)? * pts_per_cm))
        } else if let Some(v) = s.strip_suffix("em") {
            Ok(Measurement::Em(number(v)?))
        } else if let Some(v) = s.strip_suffix('%') {
            Ok(Measurement::PercentOfSpace(number(v)?))
        } else {
            Ok(Measurement::Absolute(number(s)? * pts_per_inch))
        }
    }

    /// Parse XFA XML structure from a buffer
    pub fn parse(buffer: &[u8]) -> Result<Vec<XfaNode>, String> {
        let mut reader = Reader::from_reader(buffer);
        // Do NOT trim_text here: leading/trailing whitespace in HTML text
        // content (e.g. "<span> word</span>") is semantically significant
        // and must be preserved so that inter-element spaces are not lost.
        // Whitespace-only indentation nodes are handled downstream.

        let mut root_nodes = Vec::new();

        crate::util::reset_repairs();
        Self::parse_nodes(&mut reader, &mut root_nodes, None)?;
        let (repaired, example) = crate::util::take_repairs();
        if repaired > 0 {
            log::warn!(
                "{repaired} text(s) were double-encoded (UTF-8 read as Windows-1252) and were re-read as UTF-8, for example {:?}; the source should be fixed",
                example.unwrap_or_default()
            );
        }

        // Resolve `use` prototype references per XFA spec.
        // Elements with a `use` attribute reference a prototype in a `<proto>`
        // section via SOM expression. The prototype's attributes are merged
        // into the element (element's own attributes take precedence).
        Self::resolve_use_prototypes(&mut root_nodes);

        // Re-extract styling after prototype resolution so that merged
        // attributes (e.g. hyphenation settings from proto) are picked up.
        Self::refresh_styling(&mut root_nodes);

        // Each field's pictures and other value metadata, read once here
        // with its prevailing locale.
        field_meta::attach(&mut root_nodes);

        Ok(root_nodes)
    }

    /// Re-run styling extraction on all nodes. Called after prototype
    /// resolution to pick up merged attributes.
    fn refresh_styling(nodes: &mut [XfaNode]) {
        for node in nodes.iter_mut() {
            node.extract_styling_from_children();
            Self::refresh_styling(&mut node.children);
        }
    }

    /// Resolve `use`/`usehref` prototype references across the tree (XFA 3.3
    /// §6 "Prototypes"): each referencing element inherits the prototype's
    /// attributes (except `id`, `name` and `use`), its data content and its
    /// child elements, overriding what it states itself; children are
    /// matched by type and ordinal, and the prototype's child is in turn the
    /// prototype of the referencing child. A prototype may reference
    /// prototypes itself; those are resolved first. A reference that names
    /// nothing, or an element of another type, is reported and left as it
    /// is.
    fn resolve_use_prototypes(nodes: &mut [XfaNode]) {
        let mut prototypes = Prototypes::default();
        for node in nodes.iter() {
            Self::index_prototypes(node, "", &mut prototypes);
        }
        if prototypes.by_id.is_empty() && prototypes.by_path.is_empty() {
            return;
        }
        for node in nodes.iter_mut() {
            Self::resolve_uses_in(node, &prototypes, 0);
        }
    }

    /// The prototypes a template offers: any element with an `id`
    /// (reachable as `#id`), and the elements under a `<proto>` by their
    /// name-or-tag path from it (Designer's
    /// `designer__defaultHyphenation.para.hyphenation`).
    fn index_prototypes(node: &XfaNode, proto_path: &str, prototypes: &mut Prototypes) {
        if let Some(id) = node.attributes.get("id")
            && !id.is_empty()
        {
            prototypes
                .by_id
                .entry(id.clone())
                .or_insert_with(|| node.clone());
        }
        let under_proto = !proto_path.is_empty()
            || matches!(&node.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "proto");
        for child in &node.children {
            if matches!(child.kind, XfaNodeKind::Text { .. } | XfaNodeKind::Bind) {
                continue;
            }
            let child_path = if under_proto {
                let segment = child
                    .name
                    .clone()
                    .unwrap_or_else(|| child.kind.tag_name().to_string());
                let path = if proto_path.is_empty() {
                    segment
                } else {
                    format!("{proto_path}.{segment}")
                };
                prototypes
                    .by_path
                    .entry(path.clone())
                    .or_insert_with(|| child.clone());
                path
            } else {
                String::new()
            };
            Self::index_prototypes(child, &child_path, prototypes);
        }
    }

    /// Resolve the `use`/`usehref` of `node` and, recursively, of its
    /// children. `depth` bounds nested prototype references (a prototype
    /// that invokes itself, XFA 3.3 §6 Example 6.8, is reported).
    fn resolve_uses_in(node: &mut XfaNode, prototypes: &Prototypes, depth: usize) {
        if let Some(reference) = Self::prototype_reference(node) {
            if depth > 16 {
                log::warn!(
                    "prototype reference {reference:?} nests too deep; a prototype loop is likely. The reference is ignored"
                );
            } else {
                match prototypes.lookup(&reference) {
                    Some(prototype) => {
                        let mut prototype = prototype.clone();
                        // The prototype's own references first (nested prototypes).
                        Self::resolve_uses_in(&mut prototype, prototypes, depth + 1);
                        if prototype.kind.tag_name() == node.kind.tag_name() {
                            Self::inherit_prototype(node, &prototype);
                        } else {
                            log::warn!(
                                "{} references prototype {reference:?}, a {}; a prototype must be of the same type. The reference is ignored",
                                node.kind.tag_name(),
                                prototype.kind.tag_name()
                            );
                        }
                    }
                    None => log::warn!(
                        "{} references prototype {reference:?}, which is not in the template; the reference is ignored",
                        node.kind.tag_name()
                    ),
                }
            }
        }
        for child in &mut node.children {
            Self::resolve_uses_in(child, prototypes, depth);
        }
    }

    /// The prototype reference an element makes: `usehref` when non-empty
    /// (only its same-document `#id` and `#som()` forms are supported; a
    /// URI to another document is reported), else `use`.
    fn prototype_reference(node: &XfaNode) -> Option<String> {
        if let Some(href) = node
            .attributes
            .get("usehref")
            .filter(|h| !h.trim().is_empty())
        {
            let href = href.trim();
            if let Some(fragment) = href.strip_prefix('#') {
                return Some(
                    match fragment
                        .strip_prefix("som(")
                        .and_then(|s| s.strip_suffix(')'))
                    {
                        Some(expr) => expr.to_string(),
                        None => format!("#{fragment}"),
                    },
                );
            }
            log::warn!(
                "usehref {href:?} refers to another document, which is not supported; the reference is ignored"
            );
            return None;
        }
        node.attributes
            .get("use")
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty())
    }

    /// `node` inherits `prototype` (XFA 3.3 §6 "Overriding Prototype
    /// Properties"), then re-reads its layout attributes.
    fn inherit_prototype(node: &mut XfaNode, prototype: &XfaNode) {
        for (key, value) in &prototype.attributes {
            if matches!(key.as_str(), "id" | "name" | "use" | "usehref") {
                continue;
            }
            node.attributes
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        node.attributes.remove("use");
        node.attributes.remove("usehref");
        // Data content, unless the referencing element has its own.
        if let (
            XfaNodeKind::Element {
                text_content: own, ..
            },
            XfaNodeKind::Element {
                text_content: Some(text),
                ..
            },
        ) = (&mut node.kind, &prototype.kind)
            && own.as_ref().is_none_or(|t| t.trim().is_empty())
        {
            *own = Some(text.clone());
        }
        if !node
            .children
            .iter()
            .any(|c| matches!(c.kind, XfaNodeKind::Text { .. }))
        {
            node.children.extend(
                prototype
                    .children
                    .iter()
                    .filter(|c| matches!(c.kind, XfaNodeKind::Text { .. }))
                    .cloned(),
            );
        }
        // Child elements, matched by type and ordinal within type.
        for (index, proto_child) in prototype
            .children
            .iter()
            .enumerate()
            .filter(|(_, c)| !matches!(c.kind, XfaNodeKind::Text { .. }))
        {
            let tag = proto_child.kind.tag_name();
            let ordinal = prototype.children[..index]
                .iter()
                .filter(|c| c.kind.tag_name() == tag)
                .count();
            let own = node
                .children
                .iter_mut()
                .filter(|c| c.kind.tag_name() == tag)
                .nth(ordinal);
            match own {
                Some(child) => Self::inherit_prototype(child, proto_child),
                None => node.children.push(proto_child.clone()),
            }
        }
        // The layout attributes the inherited ones add.
        let rebuilt = XfaNode::new(node.kind.clone(), node.attributes.clone());
        node.x = rebuilt.x;
        node.y = rebuilt.y;
        node.w = rebuilt.w;
        node.h = rebuilt.h;
        node.min_w = rebuilt.min_w;
        node.min_h = rebuilt.min_h;
        node.max_w = rebuilt.max_w;
        node.max_h = rebuilt.max_h;
        node.layout = rebuilt.layout;
        node.rotate = rebuilt.rotate;
        node.name = rebuilt.name;
        node.presence = rebuilt.presence;
    }

    fn parse_nodes(
        reader: &mut Reader<&[u8]>,
        nodes: &mut Vec<XfaNode>,
        end_tag: Option<&str>,
    ) -> Result<(), String> {
        let mut buf = Vec::new();
        let mut text_content = String::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let tag_name = std::str::from_utf8(e.name().as_ref())
                        .map_err(|e| format!("Invalid UTF-8: {}", e))?
                        .to_string();

                    let attributes = Self::parse_attributes(&e)?;
                    let kind = XfaNodeKind::from_tag(&tag_name);

                    let mut node = Self::new(kind, attributes);

                    // Parse children (except for bind which we skip)
                    if matches!(node.kind, XfaNodeKind::Bind) {
                        Self::skip_to_end(reader, &tag_name)?;
                    } else if matches!(node.kind, XfaNodeKind::Element { .. }) {
                        let mut text = String::new();
                        Self::parse_element_content(
                            reader,
                            &mut node.children,
                            &mut text,
                            &tag_name,
                        )?;

                        // Update the kind with text content if present
                        if !text.trim().is_empty() {
                            node.kind = XfaNodeKind::Element {
                                tag_name,
                                text_content: Some(text),
                            };
                        }
                    } else {
                        Self::parse_nodes(reader, &mut node.children, Some(&tag_name))?;
                    }

                    // Extract styling (margins, border, font, para) from child elements
                    node.extract_styling_from_children();

                    nodes.push(node);
                }
                Ok(Event::End(e)) => {
                    let tag_name = std::str::from_utf8(e.name().as_ref())
                        .map_err(|e| format!("Invalid UTF-8: {}", e))?
                        .to_string();

                    if let Some(expected_end) = end_tag
                        && tag_name == expected_end
                    {
                        return Ok(());
                    }
                }
                Ok(Event::Text(e)) => {
                    let text = e
                        .unescape()
                        .map_err(|e| format!("Failed to unescape text: {}", e))?;
                    // Repair double-encoded source text (no-op on clean UTF-8).
                    let text = crate::util::fix_double_encoded_utf8(&text);
                    let text = crate::util::normalize_nfc(&text);
                    text_content.push_str(&text);
                }
                Ok(Event::Empty(e)) => {
                    // A self-closing element is an element with no content
                    // (`<keep intact="contentArea"/>`, `<line/>`), exactly as
                    // `parse_element_content` reads one.
                    let tag_name = std::str::from_utf8(e.name().as_ref())
                        .map_err(|e| format!("Invalid UTF-8: {}", e))?
                        .to_string();
                    let attributes = Self::parse_attributes(&e)?;
                    let mut node = Self::new(XfaNodeKind::from_tag(&tag_name), attributes);
                    node.extract_styling_from_children();
                    nodes.push(node);
                }
                Ok(Event::Eof) => {
                    return Ok(());
                }
                Err(e) => {
                    return Err(format!(
                        "Error parsing XML at position {}: {}",
                        reader.buffer_position(),
                        e
                    ));
                }
                _ => {}
            }
            buf.clear();
        }
    }

    fn parse_element_content(
        reader: &mut Reader<&[u8]>,
        children: &mut Vec<XfaNode>,
        text_content: &mut String,
        end_tag: &str,
    ) -> Result<(), String> {
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let tag_name = std::str::from_utf8(e.name().as_ref())
                        .map_err(|e| format!("Invalid UTF-8: {}", e))?
                        .to_string();

                    let attributes = Self::parse_attributes(&e)?;
                    let kind = XfaNodeKind::from_tag(&tag_name);

                    let mut child_node = Self::new(kind, attributes);
                    let mut child_text = String::new();

                    Self::parse_element_content(
                        reader,
                        &mut child_node.children,
                        &mut child_text,
                        &tag_name,
                    )?;

                    // Update kind with text if present (only for Element type)
                    if !child_text.trim().is_empty()
                        && let XfaNodeKind::Element { tag_name: t, .. } = &child_node.kind
                    {
                        child_node.kind = XfaNodeKind::Element {
                            tag_name: t.clone(),
                            text_content: Some(child_text),
                        };
                    }

                    // Extract styling (margins, border, font, para) from child elements
                    child_node.extract_styling_from_children();

                    children.push(child_node);
                }
                Ok(Event::End(e)) => {
                    let tag_name = std::str::from_utf8(e.name().as_ref())
                        .map_err(|e| format!("Invalid UTF-8: {}", e))?
                        .to_string();

                    if tag_name == end_tag {
                        return Ok(());
                    }
                }
                Ok(Event::Text(e)) => {
                    let text = e
                        .unescape()
                        .map_err(|e| format!("Failed to unescape text: {}", e))?;
                    // Repair double-encoded source text (no-op on clean UTF-8).
                    let text = crate::util::fix_double_encoded_utf8(&text);
                    let text = crate::util::normalize_nfc(&text);
                    text_content.push_str(&text);
                    // Also add as a Text child to preserve interleaving
                    // with sibling elements (e.g. <span>s within a <p>).
                    if !text.is_empty() {
                        children.push(Self::new(
                            XfaNodeKind::Text {
                                content: text.to_string(),
                            },
                            std::collections::HashMap::new(),
                        ));
                    }
                }
                Ok(Event::Empty(e)) => {
                    // Handle self-closing tags like <contentArea ... />
                    let tag_name = std::str::from_utf8(e.name().as_ref())
                        .map_err(|e| format!("Invalid UTF-8: {}", e))?
                        .to_string();

                    let attributes = Self::parse_attributes(&e)?;
                    let kind = XfaNodeKind::from_tag(&tag_name);

                    let child_node = Self::new(kind, attributes);
                    children.push(child_node);
                }
                Ok(Event::Eof) => return Ok(()),
                Err(e) => {
                    return Err(format!(
                        "Error parsing XML at position {}: {}",
                        reader.buffer_position(),
                        e
                    ));
                }
                _ => {}
            }
            buf.clear();
        }
    }

    fn skip_to_end(reader: &mut Reader<&[u8]>, tag_name: &str) -> Result<(), String> {
        let mut buf = Vec::new();
        let mut depth = 1;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let name = std::str::from_utf8(e.name().as_ref())
                        .map_err(|e| format!("Invalid UTF-8: {}", e))?
                        .to_string();
                    if name == tag_name {
                        depth += 1;
                    }
                }
                Ok(Event::End(e)) => {
                    let name = std::str::from_utf8(e.name().as_ref())
                        .map_err(|e| format!("Invalid UTF-8: {}", e))?
                        .to_string();
                    if name == tag_name {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(());
                        }
                    }
                }
                Ok(Event::Empty(_)) => {}
                Ok(Event::Eof) => return Ok(()),
                Err(e) => {
                    return Err(format!(
                        "Error skipping to end at position {}: {}",
                        reader.buffer_position(),
                        e
                    ));
                }
                _ => {}
            }
            buf.clear();
        }
    }

    fn parse_attributes<'a>(
        e: &'a quick_xml::events::BytesStart<'a>,
    ) -> Result<HashMap<String, String>, String> {
        let mut attributes = HashMap::new();

        for attr in e.attributes() {
            let attr = attr.map_err(|e| format!("Failed to parse attribute: {}", e))?;
            let key = std::str::from_utf8(attr.key.as_ref())
                .map_err(|e| format!("Invalid UTF-8 in attribute key: {}", e))?
                .to_string();
            let value = std::str::from_utf8(&attr.value)
                .map_err(|e| format!("Invalid UTF-8 in attribute value: {}", e))?
                .to_string();
            attributes.insert(key, value);
        }

        Ok(attributes)
    }

    /// Count all nodes recursively
    pub fn count_nodes(nodes: &[XfaNode]) -> usize {
        let mut count = nodes.len();
        for node in nodes {
            count += Self::count_nodes(&node.children);
        }
        count
    }

    /// Get a summary of the structure
    pub fn summarize_structure(nodes: &[XfaNode], indent: usize) -> String {
        let mut result = String::new();
        let indent_str = "  ".repeat(indent);

        for node in nodes.iter().take(10) {
            // Limit to first 10 at each level
            let node_type = match &node.kind {
                XfaNodeKind::Template => "Template",
                XfaNodeKind::Subform => "Subform",
                XfaNodeKind::Field => "Field",
                XfaNodeKind::PageSet => "PageSet",
                XfaNodeKind::PageArea => "PageArea",
                XfaNodeKind::ContentArea => "ContentArea",
                XfaNodeKind::Draw => "Draw",
                XfaNodeKind::Value => "Value",
                XfaNodeKind::Text { .. } => "Text",
                XfaNodeKind::Bind => "Bind",
                XfaNodeKind::ExclGroup => "ExclGroup",
                XfaNodeKind::Element { tag_name, .. } => tag_name,
            };

            let name_str = if let Some(name) = &node.name {
                format!(" '{}'", name)
            } else {
                String::new()
            };

            let text_str = if let XfaNodeKind::Element {
                text_content: Some(text),
                ..
            } = &node.kind
            {
                format!(" = '{}'", text.chars().take(30).collect::<String>())
            } else if let XfaNodeKind::Text { content } = &node.kind {
                format!(" = '{}'", content.chars().take(30).collect::<String>())
            } else {
                String::new()
            };

            result.push_str(&format!(
                "{}{}{} ({} children){}\n",
                indent_str,
                node_type,
                name_str,
                node.children.len(),
                text_str
            ));

            if indent < 3 && !node.children.is_empty() {
                result.push_str(&Self::summarize_structure(&node.children, indent + 1));
            }
        }

        result
    }

    /// Recursively find all nodes matching a predicate
    pub fn find_nodes_by_type<'a, F>(&'a self, predicate: &F, results: &mut Vec<&'a XfaNode>)
    where
        F: Fn(&XfaNode) -> bool,
    {
        if predicate(self) {
            results.push(self);
        }

        for child in &self.children {
            child.find_nodes_by_type(predicate, results);
        }
    }

    /// Get the current presence value for this node
    pub fn get_presence(&self) -> Presence {
        self.presence
    }

    /// Set the presence value for this node
    pub fn set_presence(&mut self, presence: Presence) {
        self.presence = presence;
    }

    /// Whether this node carries the `access` property at all: a field, an
    /// exclusion group or a subform (XFA 3.3 §17).
    pub fn holds_access(&self) -> bool {
        self.kind.is_field() || self.kind.is_exclgroup() || self.kind.is_subform()
    }

    /// This node's own `access` (XFA 3.3 §17), `open` when it declares
    /// none. Only `field`, `exclGroup` and `subform` carry the property; for
    /// what a field actually allows, its enclosing containers count too --
    /// see `XfaForm::effective_access`.
    pub fn get_access(&self) -> crate::flattened::FieldAccess {
        self.attributes
            .get("access")
            .and_then(|s| s.parse().ok())
            .unwrap_or_default()
    }

    /// Set this node's own `access`, as a script assigning `access` does.
    /// Written as the attribute the template would carry, so layout reads
    /// it the same way whichever set it.
    pub fn set_access(&mut self, access: crate::flattened::FieldAccess) {
        self.attributes
            .insert("access".to_string(), access.as_str().to_string());
    }

    /// The `size` of this field's check button (XFA 3.3 "The size property"
    /// of `checkButton`): the side of its box, or the diameter of its circle,
    /// 10pt when the button declares none. `None` for a field without a
    /// check button.
    pub fn check_button_size(&self) -> Option<Num> {
        self.check_button().map(|b| b.size)
    }

    /// The field's `<checkButton>` (XFA 3.3 "The checkButton element"):
    /// its `size` (10pt by default), its asserted `mark` and whether its
    /// `shape` is round. `None` for a field without one.
    pub fn check_button(&self) -> Option<CheckButton> {
        let ui = self.children.iter().find(
            |c| matches!(&c.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "ui"),
        )?;
        let button = ui.children.iter().find(
            |c| matches!(&c.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "checkButton"),
        )?;
        let attr = |name: &str| button.attributes.get(name).map(String::as_str);
        let mark = match attr("mark") {
            None | Some("default") => CheckMark::Default,
            Some("check") => CheckMark::Check,
            Some("circle") => CheckMark::Circle,
            Some("cross") => CheckMark::Cross,
            Some("diamond") => CheckMark::Diamond,
            Some("square") => CheckMark::Square,
            Some("star") => CheckMark::Star,
            Some(other) => {
                log::warn!("unknown checkButton mark {other:?} on {:?}; taken as default", self.name);
                CheckMark::Default
            }
        };
        let round = match attr("shape") {
            None | Some("square") => false,
            Some("round") => true,
            Some(other) => {
                log::warn!("unknown checkButton shape {other:?} on {:?}; taken as square", self.name);
                false
            }
        };
        Some(CheckButton {
            size: attr("size")
                .and_then(|s| Self::parse_dimension(s).ok())
                .unwrap_or_else(|| Decimal::from(10)),
            mark,
            round,
        })
    }

    /// The `match` of this node's `<bind>` (XFA 3.3 §6): `global` for a
    /// field bound to global data, `none` for one bound to nothing, and so
    /// on; `None` when the node declares no bind, which binds it by name.
    pub fn bind_match(&self) -> Option<&str> {
        self.children
            .iter()
            .find(|c| matches!(c.kind, XfaNodeKind::Bind))
            .and_then(|bind| bind.attributes.get("match"))
            .map(String::as_str)
    }

    /// Whether this node shares its value with every other node of its name
    /// through global data (XFA 3.3 §6 "Global data"). The spec allows
    /// `match="global"` on a field only; Acrobat honours it on an exclusion
    /// group too, and ACYI_001_EN's cover page depends on that: the option
    /// group a person picks from and the hidden group its scripts read are
    /// both `RB_Select_Option`, joined by nothing but this.
    /// A documented Acrobat deviation; see [`crate::xfa::acrobat_compat`].
    pub fn is_globally_bound(&self) -> bool {
        (self.kind.is_field() || self.kind.is_exclgroup()) && self.bind_match() == Some("global")
    }

    /// The `<text>` under this node's `<caption><value>`: the node a script
    /// reaches as `caption.value.#text` (XFA 3.3 §3). `None` when the
    /// caption has no plain text, a rich-text caption included.
    fn caption_text_node_mut(&mut self) -> Option<&mut XfaNode> {
        let caption = self.children.iter_mut().find(
            |c| matches!(&c.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "caption"),
        )?;
        let value = caption
            .children
            .iter_mut()
            .find(|c| matches!(c.kind, XfaNodeKind::Value))?;
        value.children.iter_mut().find(|c| {
            matches!(
                &c.kind,
                XfaNodeKind::Text { .. }
                    | XfaNodeKind::Element {
                        text_content: Some(_),
                        ..
                    }
            )
        })
    }

    /// The plain text of this node's caption, `Some` even when empty, as
    /// long as the caption holds a `<text>`; `None` otherwise. What a script
    /// reads as `caption.value.#text.value`.
    pub fn caption_text(&self) -> Option<String> {
        let caption = self.children.iter().find(
            |c| matches!(&c.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "caption"),
        )?;
        let value = caption
            .children
            .iter()
            .find(|c| matches!(c.kind, XfaNodeKind::Value))?;
        value.children.iter().find_map(|c| match &c.kind {
            XfaNodeKind::Text { content } => Some(content.clone()),
            XfaNodeKind::Element {
                text_content: Some(content),
                ..
            } => Some(content.clone()),
            _ => None,
        })
    }

    /// Relabel this node: set its caption's `<text>`, as a script assigning
    /// `caption.value.#text.value` does. False when the caption has no
    /// `<text>` to set, in which case nothing changes.
    pub fn set_caption_text(&mut self, text: &str) -> bool {
        let Some(node) = self.caption_text_node_mut() else {
            return false;
        };
        match &mut node.kind {
            XfaNodeKind::Text { content } => *content = text.to_string(),
            XfaNodeKind::Element { text_content, .. } => *text_content = Some(text.to_string()),
            _ => return false,
        }
        true
    }

    /// Replace this field's `<items>` with `items` as (display, save) pairs
    /// (XFA 3.3 §17 `items`): one `<items>` of display texts and one with
    /// `save="1"`, the shape [`Self::extract_choice_list_items`] reads back.
    /// What a script's `addItem`/`clearItems` (§6 "Scripting Methods") left
    /// in the Form DOM, written onto the node the renderer and the control
    /// listing read.
    pub fn set_choice_list_items(&mut self, items: &[(String, String)]) {
        self.children.retain(
            |c| !matches!(&c.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "items"),
        );
        let list = |values: Vec<&String>, save: bool| {
            let mut attributes = HashMap::new();
            if save {
                attributes.insert("save".to_string(), "1".to_string());
            }
            let mut node = XfaNode::new(
                XfaNodeKind::Element {
                    tag_name: "items".to_string(),
                    text_content: None,
                },
                attributes,
            );
            node.children = values
                .into_iter()
                .map(|value| {
                    XfaNode::new(
                        XfaNodeKind::Element {
                            tag_name: "text".to_string(),
                            text_content: Some(value.clone()),
                        },
                        HashMap::new(),
                    )
                })
                .collect();
            node
        };
        self.children
            .push(list(items.iter().map(|(d, _)| d).collect(), false));
        self.children
            .push(list(items.iter().map(|(_, s)| s).collect(), true));
    }

    /// Find a mutable reference to a descendant node by name
    pub fn find_node_by_name_mut(&mut self, name: &str) -> Option<&mut XfaNode> {
        if self.name.as_deref() == Some(name) {
            return Some(self);
        }
        for child in &mut self.children {
            if let Some(found) = child.find_node_by_name_mut(name) {
                return Some(found);
            }
        }
        None
    }

    /// Find a mutable reference to a descendant node by ID attribute
    pub fn find_node_by_id_mut(&mut self, id: &str) -> Option<&mut XfaNode> {
        if self.attributes.get("id").map(|s| s.as_str()) == Some(id) {
            return Some(self);
        }
        for child in &mut self.children {
            if let Some(found) = child.find_node_by_id_mut(id) {
                return Some(found);
            }
        }
        None
    }

    /// The values this check button field takes (XFA 3.3 §17, "The items
    /// property" of a field with a checkButton widget): the first of its
    /// `<items>` values is the "on" value, the second the "off" value, the
    /// third the "neutral" value, and a second or third value that is not
    /// provided is the null string. Any content element counts as a value
    /// (`<text>`, `<integer>`, `<decimal>`, ...).
    ///
    /// The spec does not say what a check button without `<items>` takes;
    /// [`CheckButtonValues::declared`] is then false and the on value is
    /// `"1"`, the value Designer writes by default.
    pub fn check_button_values(&self) -> CheckButtonValues {
        let items = self.children.iter().find(|c| {
            matches!(&c.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "items")
        });
        let Some(items) = items else {
            return CheckButtonValues {
                on: "1".to_string(),
                off: String::new(),
                neutral: String::new(),
                declared: false,
            };
        };
        let mut values = extract_items_values(items).into_iter();
        CheckButtonValues {
            on: values.next().unwrap_or_default(),
            off: values.next().unwrap_or_default(),
            neutral: values.next().unwrap_or_default(),
            declared: true,
        }
    }

    /// This field's `<items>` as (display, save) pairs (XFA 3.3 §17
    /// `items`): with two `<items>`, the one with `save="1"` supplies save
    /// values and the other display values, zipped positionally; with one,
    /// its values serve as both. Backs both the flattener's dropdown
    /// rendering ([`extract_items_values`] via `Flattened::extract_dropdown_hint`)
    /// and the scripting engine's `addItem`/`clearItems`/... methods
    /// (XFA 3.3 §6 "Scripting Methods"), so a field's choice list looks the
    /// same however a script or the renderer asks for it.
    pub fn extract_choice_list_items(&self) -> Vec<(String, String)> {
        let mut display_items: Vec<String> = Vec::new();
        let mut save_items: Vec<String> = Vec::new();

        for child in &self.children {
            let XfaNodeKind::Element { tag_name, .. } = &child.kind else {
                continue;
            };
            if tag_name != "items" {
                continue;
            }
            let is_save = child
                .attributes
                .get("save")
                .map(|s| s == "1")
                .unwrap_or(false);
            let items = extract_items_values(child);
            if is_save {
                save_items = items;
            } else if display_items.is_empty() {
                display_items = items;
            } else if save_items.is_empty() {
                save_items = items;
            }
        }

        if display_items.is_empty() && !save_items.is_empty() {
            display_items = save_items.clone();
        }
        if save_items.is_empty() {
            save_items = display_items.clone();
        }

        display_items.into_iter().zip(save_items).collect()
    }
}

/// What a check button field stores in each of its states; see
/// [`XfaNode::check_button_values`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckButtonValues {
    pub on: String,
    pub off: String,
    pub neutral: String,
    /// Whether the field declares them in `<items>`; when it does not, the
    /// values are a documented default rather than the form's own.
    pub declared: bool,
}

/// Extract values from an `<items>` element's children.
/// Handles `<text>`, `<integer>`, `<decimal>`, `<float>`, `<boolean>`,
/// `<date>`, `<dateTime>`, `<time>` children.
pub fn extract_items_values(items_node: &XfaNode) -> Vec<String> {
    let mut values = Vec::new();
    for child in &items_node.children {
        match &child.kind {
            XfaNodeKind::Element {
                tag_name,
                text_content,
            } => {
                if matches!(
                    tag_name.as_str(),
                    "text" | "integer" | "decimal" | "float" | "boolean" | "date" | "dateTime"
                        | "time"
                ) {
                    values.push(text_content.clone().unwrap_or_default());
                }
            }
            XfaNodeKind::Text { content } => {
                values.push(content.clone());
            }
            _ => {}
        }
    }
    values
}

// ============================================================================
// Public helper functions for extracting context from the XFA node tree
// ============================================================================

/// Find the root content subform (the first named `<subform>` that is not a `<pageSet>`).
pub fn find_root_subform(xfa_nodes: &[XfaNode]) -> Option<&XfaNode> {
    for node in xfa_nodes {
        let is_subform = node.kind.is_subform();
        let is_page_set =
            matches!(&node.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "pageSet");

        if is_page_set {
            continue;
        }

        if is_subform && node.name.is_some() {
            return Some(node);
        }

        if let Some(found) = find_root_subform(&node.children) {
            return Some(found);
        }
    }
    None
}

/// Recursively collect all `<text name="...">value</text>` items from
/// `<variables>` elements in the XFA node tree.
///
/// Returns a map of variable name → value for all text variables found.
pub fn collect_text_variables(nodes: &[XfaNode]) -> HashMap<String, String> {
    let mut text_vars: Vec<(String, String)> = Vec::new();
    collect_text_variables_recursive(nodes, &mut text_vars);
    text_vars.into_iter().collect()
}

fn collect_text_variables_recursive(nodes: &[XfaNode], text_vars: &mut Vec<(String, String)>) {
    for node in nodes {
        if let XfaNodeKind::Element { tag_name, .. } = &node.kind
            && tag_name == "variables"
        {
            for child in &node.children {
                if let XfaNodeKind::Element {
                    tag_name: child_tag,
                    text_content,
                    ..
                } = &child.kind
                    && child_tag == "text"
                    && let Some(name) = child.name.as_ref().or_else(|| child.attributes.get("name"))
                {
                    let value = text_content.clone().unwrap_or_default();
                    text_vars.push((name.clone(), value));
                }
            }
        }

        collect_text_variables_recursive(&node.children, text_vars);
    }
}

/// Recursively collect all `<script name="...">content</script>` items from
/// `<variables>` elements in the XFA node tree.
///
/// Returns a list of `(name, content)` pairs for all script variables found.
pub fn collect_variable_scripts(nodes: &[XfaNode]) -> Vec<(String, String)> {
    let mut scripts = Vec::new();
    collect_variable_scripts_recursive(nodes, &mut scripts);
    scripts
}

fn collect_variable_scripts_recursive(nodes: &[XfaNode], scripts: &mut Vec<(String, String)>) {
    for node in nodes {
        if let XfaNodeKind::Element { tag_name, .. } = &node.kind
            && tag_name == "variables"
        {
            for child in &node.children {
                if let XfaNodeKind::Element {
                    tag_name: child_tag,
                    text_content,
                    ..
                } = &child.kind
                    && child_tag == "script"
                    && let Some(name) = child.name.as_ref().or_else(|| child.attributes.get("name"))
                {
                    // Handle <script> - may have content directly in text_content
                    if let Some(content) = text_content
                        && !content.is_empty()
                    {
                        scripts.push((name.clone(), content.clone()));
                    }
                    // Also check for content in child nodes (both Element and Text types)
                    for script_child in &child.children {
                        match &script_child.kind {
                            XfaNodeKind::Text { content } if !content.is_empty() => {
                                scripts.push((name.clone(), content.clone()));
                            }
                            XfaNodeKind::Element {
                                text_content: Some(content),
                                ..
                            } if !content.is_empty() => {
                                scripts.push((name.clone(), content.clone()));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        collect_variable_scripts_recursive(&node.children, scripts);
    }
}

/// Like [`collect_variable_scripts`], but also reports the SOM path of the
/// subform that owns each script object.
///
/// Per XFA 3.3 §10 "Instantiation of Named Script Objects": "the contents of
/// the object are compiled into a script object and that object is then
/// registered with the subform" — so a script written as `foo.bar()` from
/// inside that subform (or `xfa.form.form1.foo.bar()` from anywhere) must
/// resolve, not just the bare global most callers register it as.
///
/// `owner_path` is the SOM path already assigned to `nodes`' own container
/// (typically the root subform's name); it is extended exactly the way
/// `ScriptExecutor::build_and_register_xfa_som_hierarchy` extends SOM paths:
/// only `subform`/`exclGroup` elements add a segment, `pageSet`/`pageArea`
/// do not. Call this on the same `nodes`/`owner_path` pair that hierarchy is
/// built from, so the returned paths line up with the registered objects.
pub fn collect_variable_script_owners(
    owner_path: &str,
    nodes: &[XfaNode],
) -> Vec<(String, String, String)> {
    let mut scripts = Vec::new();
    collect_variable_script_owners_recursive(nodes, owner_path, &mut scripts);
    scripts
}

fn collect_variable_script_owners_recursive(
    nodes: &[XfaNode],
    owner_path: &str,
    scripts: &mut Vec<(String, String, String)>,
) {
    for (node, index) in nodes
        .iter()
        .zip(crate::xfa::scripting::som::sibling_indices(nodes))
    {
        if let XfaNodeKind::Element { tag_name, .. } = &node.kind
            && tag_name == "variables"
        {
            for child in &node.children {
                if let XfaNodeKind::Element {
                    tag_name: child_tag,
                    text_content,
                    ..
                } = &child.kind
                    && child_tag == "script"
                    && let Some(name) = child.name.as_ref().or_else(|| child.attributes.get("name"))
                {
                    if let Some(content) = text_content
                        && !content.is_empty()
                    {
                        scripts.push((name.clone(), content.clone(), owner_path.to_string()));
                    }
                    for script_child in &child.children {
                        match &script_child.kind {
                            XfaNodeKind::Text { content } if !content.is_empty() => {
                                scripts.push((
                                    name.clone(),
                                    content.clone(),
                                    owner_path.to_string(),
                                ));
                            }
                            XfaNodeKind::Element {
                                text_content: Some(content),
                                ..
                            } if !content.is_empty() => {
                                scripts.push((
                                    name.clone(),
                                    content.clone(),
                                    owner_path.to_string(),
                                ));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        let name = node.name.clone().unwrap_or_default();
        let next_path = if !name.is_empty() && node.kind.extends_som_path() {
            crate::xfa::scripting::som::child_som_path(owner_path, &name, index)
        } else {
            owner_path.to_string()
        };
        collect_variable_script_owners_recursive(&node.children, &next_path, scripts);
    }
}

/// The document's locale: the root subform's `locale` as written (XFA 3.3
/// §17: `language[_country]`), `en_US` when it names none (the ambient
/// locale). Nothing is inferred from the text: a `locale` the template
/// states is what its author meant.
pub fn document_locale(nodes: &[XfaNode]) -> String {
    find_root_subform(nodes)
        .and_then(|root| root.attributes.get("locale"))
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "en_US".to_string())
}

/// The language part of the document's locale (`de` for `de_CH`).
pub fn extract_language_from_nodes(nodes: &[XfaNode]) -> String {
    language_of_locale(Some(&document_locale(nodes)))
}

/// The language part of a locale tag (`de_CH` is `de`), `en` for none.
pub fn language_of_locale(locale: Option<&str>) -> String {
    locale
        .and_then(|l| l.split(['_', '-']).next())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_lowercase())
        .unwrap_or_else(|| "en".to_string())
}

/// Extract both the document language and all `<variables><text>` values from
/// the XFA node tree.
///
/// Returns `(language, variables)` where language comes from the root subform's
/// `locale` attribute (spec-compliant) and variables contains all text variable
/// name-value pairs.
pub fn extract_context_from_nodes(nodes: &[XfaNode]) -> (String, HashMap<String, String>) {
    let language = extract_language_from_nodes(nodes);
    let variables = collect_text_variables(nodes);
    (language, variables)
}

#[cfg(test)]
mod context_extraction_tests {
    use super::*;

    /// Helper to create a minimal XfaNode with only the required fields set.
    fn bare_node(kind: XfaNodeKind) -> XfaNode {
        XfaNode {
            kind,
            x: None,
            y: None,
            w: None,
            h: None,
            min_w: None,
            min_h: None,
            max_w: None,
            max_h: None,
            layout: None,
            rotate: 0,
            margin_top: None,
            margin_bottom: None,
            margin_left: None,
            margin_right: None,
            border: None,
            font: None,
            para: None,
            name: None,
            presence: Presence::Visible,
            attributes: HashMap::new(),
            children: vec![],
            field_meta: None,
        }
    }

    /// Build a minimal XFA node tree with a root subform carrying the given locale
    /// and optional `<variables><text>` entries.
    fn make_tree(locale: Option<&str>, text_vars: &[(&str, &str)]) -> Vec<XfaNode> {
        let variables_children: Vec<XfaNode> = text_vars
            .iter()
            .map(|(name, value)| {
                let mut node = bare_node(XfaNodeKind::Element {
                    tag_name: "text".to_string(),
                    text_content: Some(value.to_string()),
                });
                node.name = Some(name.to_string());
                node
            })
            .collect();

        let mut subform_children = vec![];
        if !variables_children.is_empty() {
            let mut vars_node = bare_node(XfaNodeKind::Element {
                tag_name: "variables".to_string(),
                text_content: None,
            });
            vars_node.children = variables_children;
            subform_children.push(vars_node);
        }

        let mut root = bare_node(XfaNodeKind::Subform);
        root.name = Some("RootForm".to_string());
        root.children = subform_children;
        if let Some(loc) = locale {
            root.attributes
                .insert("locale".to_string(), loc.to_string());
        }

        vec![root]
    }

    #[test]
    fn test_extract_language_from_locale_de_de() {
        let nodes = make_tree(Some("de_DE"), &[]);
        assert_eq!(extract_language_from_nodes(&nodes), "de");
    }

    #[test]
    fn test_extract_language_from_locale_it_it() {
        let nodes = make_tree(Some("it_IT"), &[]);
        assert_eq!(extract_language_from_nodes(&nodes), "it");
    }

    #[test]
    fn test_extract_language_from_locale_language_only() {
        let nodes = make_tree(Some("fr"), &[]);
        assert_eq!(extract_language_from_nodes(&nodes), "fr");
    }

    #[test]
    fn test_extract_language_fallback_no_locale() {
        let nodes = make_tree(None, &[]);
        assert_eq!(extract_language_from_nodes(&nodes), "en");
    }

    #[test]
    fn test_extract_language_fallback_empty_tree() {
        let nodes: Vec<XfaNode> = vec![];
        assert_eq!(extract_language_from_nodes(&nodes), "en");
    }

    #[test]
    fn test_collect_text_variables() {
        let nodes = make_tree(
            Some("de_DE"),
            &[
                ("formrange_language", "DE"),
                ("formrange_code", "AAEI"),
                ("Footer_Line_txtformid", "66284"),
            ],
        );
        let vars = collect_text_variables(&nodes);
        assert_eq!(vars.len(), 3);
        assert_eq!(vars.get("formrange_language").unwrap(), "DE");
        assert_eq!(vars.get("formrange_code").unwrap(), "AAEI");
        assert_eq!(vars.get("Footer_Line_txtformid").unwrap(), "66284");
    }

    #[test]
    fn test_extract_context_from_nodes() {
        let nodes = make_tree(
            Some("fr_FR"),
            &[("formrange_language", "FR"), ("formrange_entity", "019")],
        );
        let (lang, vars) = extract_context_from_nodes(&nodes);
        assert_eq!(lang, "fr");
        assert_eq!(vars.len(), 2);
        assert_eq!(vars.get("formrange_language").unwrap(), "FR");
        assert_eq!(vars.get("formrange_entity").unwrap(), "019");
    }

    #[test]
    fn test_find_root_subform_skips_page_set() {
        let page_set = bare_node(XfaNodeKind::Element {
            tag_name: "pageSet".to_string(),
            text_content: None,
        });

        let mut root = bare_node(XfaNodeKind::Subform);
        root.name = Some("RootForm".to_string());
        root.attributes
            .insert("locale".to_string(), "en_US".to_string());

        let nodes = vec![page_set, root];
        let found = find_root_subform(&nodes).unwrap();
        assert_eq!(found.name.as_deref(), Some("RootForm"));
    }
}

#[cfg(test)]
mod parse_edge_case_tests {
    use super::*;

    #[test]
    fn a_designer_locked_dimension_reads_the_value_after_the_equals_sign() {
        assert_eq!(XfaNode::parse_dimension("=0mm"), Ok(Num::ZERO));
        assert_eq!(XfaNode::parse_dimension("= 2pt"), Ok(num(2.0)));
        assert!(XfaNode::parse_dimension("=").is_err());
    }

    /// XFA 3.3 §2 "Measurements": "The default unit is assumed to be
    /// inches"; `em` and `%` are relative and only allowed where the
    /// property says so.
    #[test]
    fn a_unitless_measurement_is_inches_and_relative_units_are_typed() {
        assert_eq!(XfaNode::parse_dimension("2"), Ok(num(144.0)));
        assert_eq!(XfaNode::parse_dimension("0.5in"), Ok(num(36.0)));
        assert_eq!(XfaNode::parse_measurement("-0.01em"), Ok(Measurement::Em(num(-0.01))));
        assert_eq!(XfaNode::parse_measurement("50%"), Ok(Measurement::PercentOfSpace(num(50.0))));
        assert!(XfaNode::parse_dimension("1em").is_err());
        assert!(XfaNode::parse_dimension("abc").is_err());
    }

    /// An absolute `letterSpacing` applies; an `em` one does not (Acrobat
    /// deviation 7, see `acrobat_compat`).
    #[test]
    fn absolute_letter_spacing_applies_and_em_letter_spacing_does_not() {
        let parse = |spacing: &str| {
            let xml = format!(
                r#"<template><subform name="S"><draw name="D"><font typeface="Courier" size="10pt" letterSpacing="{spacing}"/><value><text>x</text></value></draw></subform></template>"#
            );
            let nodes = XfaNode::parse(xml.as_bytes()).expect("parse");
            nodes[0].children[0].children[0].font.as_ref().expect("font").letter_spacing
        };
        assert_eq!(parse("0.5pt"), Some(num(0.5)));
        assert_eq!(parse("-0.05em"), None);
        assert_eq!(parse("3%"), None);
    }

    #[test]
    fn self_closing_children_survive_under_a_bare_template_root() {
        let nodes = XfaNode::parse(
            br#"<template><subform name="S"><keep intact="contentArea"/><draw name="D"><value><line/></value></draw></subform></template>"#,
        )
        .expect("parse");
        let subform = &nodes[0].children[0];
        let tags: Vec<String> = subform
            .children
            .iter()
            .map(|c| match &c.kind {
                XfaNodeKind::Element { tag_name, .. } => tag_name.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(tags, ["keep", "Draw"]);
        let value = &subform.children[1].children[0];
        assert!(matches!(
            &value.children[0].kind,
            XfaNodeKind::Element { tag_name, .. } if tag_name == "line"
        ));
    }
}

#[cfg(test)]
mod prototype_tests {
    use super::*;

    fn tag(node: &XfaNode) -> &str {
        node.kind.tag_name()
    }

    /// XFA 3.3 §6: the referencing element inherits the prototype's
    /// attributes and children, its own winning; children match by type
    /// and ordinal, and a prototype's child is the prototype of the
    /// referencing child.
    #[test]
    fn a_use_reference_inherits_attributes_and_children_it_lacks() {
        let nodes = XfaNode::parse(
            br##"<template><subform name="S"><proto><field id="base" name="Base" w="50mm" h="8mm"><ui><textEdit/></ui><font typeface="Courier" size="12pt"/><border><edge thickness="2pt"/></border></field></proto><field name="F" use="#base" h="10mm"><font size="9pt"/></field><draw name="D" use="#base"/><field name="G" use="#missing"/></subform></template>"##,
        )
        .expect("parse");
        let subform = &nodes[0].children[0];
        let f = &subform.children[1];
        assert_eq!(f.name.as_deref(), Some("F"), "name is never inherited");
        assert_eq!(f.w, XfaNode::parse_dimension("50mm").ok(), "w comes from the prototype");
        assert_eq!(f.h, XfaNode::parse_dimension("10mm").ok(), "the element's own h wins");
        assert!(!f.attributes.contains_key("use"));
        assert!(!f.attributes.contains_key("id"), "id is never inherited");
        let font = f.font.as_ref().expect("font merged by ordinal");
        assert_eq!(font.typeface, "Courier", "the prototype's typeface fills the gap");
        assert_eq!(font.size, num(9.0), "the element's own size wins");
        let child_tags: Vec<&str> = f.children.iter().map(tag).collect();
        assert!(child_tags.contains(&"ui") && child_tags.contains(&"border"));
        assert_eq!(f.children.iter().filter(|c| tag(c) == "font").count(), 1);
        let d = &subform.children[2];
        assert!(d.w.is_none(), "a draw cannot use a field prototype");
        let g = &subform.children[3];
        assert!(g.w.is_none(), "an unresolved reference changes nothing");
    }

    /// A prototype may reference a prototype (XFA 3.3 §6 "Nested
    /// Prototypes"); the chain is resolved before it is inherited.
    #[test]
    fn nested_prototypes_resolve_through_the_chain_and_by_proto_path() {
        let nodes = XfaNode::parse(
            br##"<template><subform name="S"><proto><font id="f1" typeface="Courier" size="7pt"/><para id="p1" hAlign="right"/><draw name="styled"><font use="#f1" size="8pt"/><para use="#p1"/></draw></proto><draw name="A" use="styled"/><draw name="B" use="#f1"/></subform></template>"##,
        )
        .expect("parse");
        let subform = &nodes[0].children[0];
        let a = &subform.children[1];
        let font = a.font.as_ref().expect("font via nested prototype");
        assert_eq!(font.typeface, "Courier");
        assert_eq!(font.size, num(8.0));
        assert_eq!(a.para.as_ref().map(|p| p.h_align), Some(HAlign::Right));
        let b = &subform.children[2];
        assert!(b.font.is_none(), "a draw is not a font; the reference is ignored");
    }
}

#[cfg(test)]
mod font_and_encoding_tests {
    use super::*;

    fn find_by_name<'a>(nodes: &'a [XfaNode], name: &str) -> Option<&'a XfaNode> {
        for node in nodes {
            if node.name.as_deref() == Some(name) {
                return Some(node);
            }
            if let Some(found) = find_by_name(&node.children, name) {
                return Some(found);
            }
        }
        None
    }

    fn find_text_content(node: &XfaNode) -> Option<String> {
        for child in &node.children {
            if let XfaNodeKind::Element {
                tag_name,
                text_content,
            } = &child.kind
                && tag_name == "text"
                && text_content.is_some()
            {
                return text_content.clone();
            }
            if let Some(found) = find_text_content(child) {
                return Some(found);
            }
        }
        None
    }

    /// The AAOV "Form configurator" heading colours its caption via
    /// `<font><fill><color .../></fill></font>`, not the plainer
    /// `<font><color .../></font>` most edges/corners use.
    #[test]
    fn parse_font_reads_color_nested_inside_fill() {
        let xml = br#"<?xml version="1.0"?>
<template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
<draw name="T_FormConfigurator">
<font weight="bold" size="8pt" typeface="Frutiger 45 Light">
  <fill><color value="255,0,0"/></fill>
</font>
</draw>
</template>"#;
        let nodes = XfaNode::parse(xml).expect("parse");
        let draw = find_by_name(&nodes, "T_FormConfigurator").expect("draw node");
        let font = draw.font.as_ref().expect("font parsed");
        assert_eq!(
            font.color,
            Some((255, 0, 0)),
            "a <font><fill><color> must set the font's color like a direct <font><color> does"
        );
    }

    /// A direct `<font><color>` (the common case, e.g. an `<edge>`'s colour)
    /// keeps working -- the `<fill>` lookup is a fallback, not a replacement.
    #[test]
    fn parse_font_still_reads_a_direct_color() {
        let xml = br#"<?xml version="1.0"?>
<template xmlns="http://www.xfa.org/schema/xfa-template/3.3/">
<draw name="T"><font><color value="10,20,30"/></font></draw>
</template>"#;
        let nodes = XfaNode::parse(xml).expect("parse");
        let draw = find_by_name(&nodes, "T").expect("draw node");
        assert_eq!(draw.font.as_ref().unwrap().color, Some((10, 20, 30)));
    }

    /// XML text can arrive as a combining-character decomposition even when
    /// the embedded font only has the precomposed glyph (AAOV: "capacità"
    /// encoded as "a" + U+0300 COMBINING GRAVE ACCENT, rendering as a
    /// missing-glyph box). The parser composes it to NFC once, so every
    /// downstream consumer sees the same "à" (U+00E0) regardless.
    #[test]
    fn text_is_normalized_to_nfc() {
        let xml = "<?xml version=\"1.0\"?>\n<template xmlns=\"http://www.xfa.org/schema/xfa-template/3.3/\">\n<draw name=\"T\"><value><text>capacita\u{0300}</text></value></draw>\n</template>";
        let nodes = XfaNode::parse(xml.as_bytes()).expect("parse");
        let draw = find_by_name(&nodes, "T").expect("draw node");
        let text = find_text_content(draw).expect("text content");
        assert_eq!(
            text, "capacit\u{e0}",
            "the combining grave accent must compose into precomposed à (U+00E0)"
        );
    }
}

#[cfg(test)]
mod check_button_value_tests {
    use super::*;

    fn field(items: &str) -> XfaNode {
        let xml = format!(
            r#"<template><subform name="f"><field name="b"><ui><checkButton/></ui>{items}</field></subform></template>"#
        );
        let nodes = XfaNode::parse(xml.as_bytes()).expect("parse");
        fn find(nodes: &[XfaNode]) -> Option<XfaNode> {
            nodes.iter().find_map(|n| {
                (n.name.as_deref() == Some("b")).then(|| n.clone()).or_else(|| find(&n.children))
            })
        }
        find(&nodes).expect("field b")
    }

    #[test]
    fn the_items_give_on_off_and_neutral_in_order() {
        let values = field("<items><text>Y</text><text>N</text><text>?</text></items>").check_button_values();
        assert_eq!((values.on.as_str(), values.off.as_str(), values.neutral.as_str()), ("Y", "N", "?"));
        assert!(values.declared);
    }

    #[test]
    fn a_missing_off_or_neutral_value_is_the_null_string() {
        let values = field("<items><integer>1</integer></items>").check_button_values();
        assert_eq!((values.on.as_str(), values.off.as_str(), values.neutral.as_str()), ("1", "", ""));
    }

    #[test]
    fn any_content_element_is_a_value() {
        let values = field("<items><decimal>2.5</decimal><boolean>0</boolean></items>").check_button_values();
        assert_eq!((values.on.as_str(), values.off.as_str()), ("2.5", "0"));
    }

    #[test]
    fn without_items_the_on_value_is_the_documented_default_not_the_name() {
        let values = field("").check_button_values();
        assert_eq!(values.on, "1");
        assert!(!values.declared);
    }
}
