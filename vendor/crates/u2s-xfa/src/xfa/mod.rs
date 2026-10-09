pub mod font_manager;
pub mod hyphenation;
pub mod instances;
pub mod script_executor;
pub mod scripting;
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
    pub radius: Option<Num>, // for inverted corners
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
    pub color: Option<(u8, u8, u8)>,
    pub presence: String,
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

// DEVIATION: `PartialOrd, Ord` added so FontVariant can key a BTreeMap — see
// font_manager::find_loaded_by_family for why the map must be ordered.
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

impl XfaNodeKind {
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
        let name = attributes.get("name").cloned();
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
        }
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
    fn parse_edge(node: &XfaNode) -> Edge {
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
    fn parse_corner(node: &XfaNode) -> Corner {
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
        }
    }

    /// Parse a <fill> element
    fn parse_fill(node: &XfaNode) -> Fill {
        Fill {
            presence: node
                .attributes
                .get("presence")
                .cloned()
                .unwrap_or_else(|| "visible".to_string()),
            color: Self::parse_color_from_children(&node.children),
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

        // Determine generic family: from attribute, or infer from typeface
        let generic_family = node
            .attributes
            .get("genericFamily")
            .and_then(|s| s.parse().ok())
            .or_else(|| {
                // Infer generic family from common typeface names
                let tf_lower = typeface.to_lowercase();
                if tf_lower.contains("courier")
                    || tf_lower.contains("mono")
                    || tf_lower.contains("consolas")
                {
                    Some(GenericFamily::Monospace)
                } else if tf_lower.contains("times")
                    || tf_lower.contains("georgia")
                    || tf_lower.contains("serif")
                {
                    Some(GenericFamily::Serif)
                } else if tf_lower.contains("helvetica")
                    || tf_lower.contains("arial")
                    || tf_lower.contains("verdana")
                {
                    Some(GenericFamily::SansSerif)
                } else if tf_lower.contains("comic")
                    || tf_lower.contains("script")
                    || tf_lower.contains("cursive")
                {
                    Some(GenericFamily::Cursive)
                } else {
                    None // Will use default
                }
            });

        Font {
            typeface,
            size: node
                .attributes
                .get("size")
                .and_then(|v| Self::parse_dimension(v).ok())
                .unwrap_or_else(|| num(10.0)),
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
            color: Self::parse_color_from_children(&node.children),
            baseline_shift: node
                .attributes
                .get("baselineShift")
                .and_then(|v| Self::parse_dimension(v).ok()),
            // Per XFA spec: letterSpacing is a relative measurement (e.g., "0.5pt", "-0.1em")
            // that adjusts spacing between grapheme clusters. Default is 0.
            letter_spacing: node
                .attributes
                .get("letterSpacing")
                .and_then(|v| Self::parse_dimension(v).ok()),
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
    pub fn parse_dimension(s: &str) -> Result<Num, String> {
        let s = s.trim();

        // Conversion constants with full precision
        let pts_per_inch = Decimal::from_str("72").unwrap();
        let pts_per_mm = Decimal::from_str("2.834645669291339").unwrap(); // 72 / 25.4 with high precision
        let pts_per_cm = Decimal::from_str("28.34645669291339").unwrap(); // 72 / 2.54 with high precision

        if let Some(val) = s.strip_suffix("pt") {
            Decimal::from_str(val.trim()).map_err(|e| format!("Failed to parse dimension: {}", e))
        } else if let Some(val) = s.strip_suffix("in") {
            Decimal::from_str(val.trim())
                .map(|v| v * pts_per_inch)
                .map_err(|e| format!("Failed to parse dimension: {}", e))
        } else if let Some(val) = s.strip_suffix("mm") {
            Decimal::from_str(val.trim())
                .map(|v| v * pts_per_mm)
                .map_err(|e| format!("Failed to parse dimension: {}", e))
        } else if let Some(val) = s.strip_suffix("cm") {
            Decimal::from_str(val.trim())
                .map(|v| v * pts_per_cm)
                .map_err(|e| format!("Failed to parse dimension: {}", e))
        } else {
            // No unit, assume points or just a number
            Decimal::from_str(s).map_err(|e| format!("Failed to parse dimension: {}", e))
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

        Self::parse_nodes(&mut reader, &mut root_nodes, None)?;

        // Resolve `use` prototype references per XFA spec.
        // Elements with a `use` attribute reference a prototype in a `<proto>`
        // section via SOM expression. The prototype's attributes are merged
        // into the element (element's own attributes take precedence).
        Self::resolve_use_prototypes(&mut root_nodes);

        // Re-extract styling after prototype resolution so that merged
        // attributes (e.g. hyphenation settings from proto) are picked up.
        Self::refresh_styling(&mut root_nodes);

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

    /// Resolve `use` attribute prototype references across the tree.
    ///
    /// Per XFA 3.3: "At template load time, invokes another object in the same
    /// document as a prototype for this object." The `use` value is a SOM
    /// expression (e.g. `"designer__defaultHyphenation.para.hyphenation"`).
    ///
    /// Implementation:
    /// 1. Collect all prototype definitions from `<proto>` sections.
    /// 2. Walk the tree and resolve `use` references by merging prototype
    ///    attributes into the referencing element.
    fn resolve_use_prototypes(nodes: &mut [XfaNode]) {
        // Step 1: Collect prototypes indexed by SOM path
        let mut proto_map: HashMap<String, HashMap<String, String>> = HashMap::new();
        for node in nodes.iter() {
            Self::collect_proto_definitions(node, &mut proto_map);
        }

        if proto_map.is_empty() {
            return;
        }

        // Step 2: Walk tree and resolve use references
        for node in nodes.iter_mut() {
            Self::resolve_use_in_subtree(node, &proto_map);
        }
    }

    /// Recursively collect attribute maps from `<proto>` children.
    ///
    /// Builds a map from SOM path (e.g. `"designer__defaultHyphenation.para.hyphenation"`)
    /// to the attributes of the prototype element at that path.
    fn collect_proto_definitions(
        node: &XfaNode,
        proto_map: &mut HashMap<String, HashMap<String, String>>,
    ) {
        if let XfaNodeKind::Element { tag_name, .. } = &node.kind {
            if tag_name == "proto" {
                // Index all descendants of this proto section
                for child in &node.children {
                    Self::index_proto_element(child, "", proto_map);
                }
                return;
            }
        }
        for child in &node.children {
            Self::collect_proto_definitions(child, proto_map);
        }
    }

    /// Index a proto element and its descendants by SOM path.
    fn index_proto_element(
        node: &XfaNode,
        parent_path: &str,
        proto_map: &mut HashMap<String, HashMap<String, String>>,
    ) {
        let name = node.name.as_deref().unwrap_or("");
        let tag = match &node.kind {
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
            XfaNodeKind::Bind => return,
            XfaNodeKind::Text { .. } => return,
        };

        // Build SOM path segment: use name if present, else tag
        let segment = if !name.is_empty() { name } else { tag };
        let path = if parent_path.is_empty() {
            segment.to_string()
        } else {
            format!("{}.{}", parent_path, segment)
        };

        // Store this element's attributes under its path
        proto_map.insert(path.clone(), node.attributes.clone());

        // Recurse into children
        for child in &node.children {
            Self::index_proto_element(child, &path, proto_map);
        }
    }

    /// Walk the subtree and resolve `use` references by merging prototype attributes.
    fn resolve_use_in_subtree(
        node: &mut XfaNode,
        proto_map: &HashMap<String, HashMap<String, String>>,
    ) {
        // Check children for use attributes — any element type can have prototypes
        for child in &mut node.children {
            if !matches!(&child.kind, XfaNodeKind::Text { .. }) {
                if let Some(use_ref) = child.attributes.get("use").cloned() {
                    // Look up the prototype
                    if let Some(proto_attrs) = proto_map.get(&use_ref) {
                        // Merge: prototype attributes are defaults; element's
                        // own attributes take precedence
                        for (key, value) in proto_attrs {
                            if key != "use" {
                                child.attributes.entry(key.clone()).or_insert(value.clone());
                            }
                        }
                    }
                }
            }
            Self::resolve_use_in_subtree(child, proto_map);
        }
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
                Ok(Event::Empty(_e)) => {
                    // Self-closing tags - we can handle them if needed
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

    /// Extract the item values from `<items><text>...</text></items>` children.
    ///
    /// Per XFA 3.3 §17 pp.758-759, the items list can have up to three values:
    ///   - First value: the "on" value (item key)
    ///   - Second value: the "off" value (defaults to empty string if absent)
    ///   - Third value: the "neutral" value (defaults to empty string if absent)
    ///
    /// Used for exclGroup parent→child propagation per §4 pp.195-197.
    pub fn extract_item_values(&self) -> (Option<String>, Option<String>) {
        for child in &self.children {
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                && tag_name == "items"
            {
                let mut values = Vec::new();
                for item_child in &child.children {
                    if let XfaNodeKind::Element {
                        tag_name: t2,
                        text_content,
                        ..
                    } = &item_child.kind
                        && (t2 == "text" || t2 == "integer" || t2 == "float")
                    {
                        values.push(text_content.clone().unwrap_or_default());
                    }
                    if let XfaNodeKind::Text { content } = &item_child.kind {
                        values.push(content.clone());
                    }
                }
                let on_value = values.first().cloned();
                let off_value = values.get(1).cloned();
                return (on_value, off_value);
            }
        }
        (None, None)
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
        let next_path = if !name.is_empty() && (node.kind.is_subform() || node.kind.is_exclgroup())
        {
            crate::xfa::scripting::som::child_som_path(owner_path, &name, index)
        } else {
            owner_path.to_string()
        };
        collect_variable_script_owners_recursive(&node.children, &next_path, scripts);
    }
}

/// Extract the document language from the root subform's `locale` attribute,
/// cross-checked against actual text content using `whatlang`.
///
/// Per XFA 3.3 §17: the `locale` attribute on a `<subform>` specifies the
/// prevailing locale using `language[_country]` format (e.g. `"de_DE"`, `"en_US"`).
/// If no locale is found, falls back to `"en"` (ambient default per spec is `en_US`).
///
/// After extracting the locale-based language, we verify it against the actual
/// text content. If `whatlang` detects a different language with high confidence,
/// the detected language is used instead. This fixes cases where the locale
/// attribute is incorrect (e.g. an English document with `locale="de_DE"`).
pub fn extract_language_from_nodes(nodes: &[XfaNode]) -> String {
    let locale_lang = if let Some(root) = find_root_subform(nodes) {
        if let Some(locale) = root.attributes.get("locale") {
            let lang = locale.split('_').next().unwrap_or("en");
            lang.to_lowercase()
        } else {
            "en".to_string()
        }
    } else {
        return "en".to_string();
    };

    // Cross-check locale with actual text content
    let text = collect_visible_text(nodes);
    if text.len() >= 50 {
        if let Some(info) = whatlang::detect(&text) {
            let detected = whatlang_to_iso(info.lang());
            if detected != locale_lang && info.confidence() > 0.8 {
                log::info!(
                    "Language override: locale says '{}' but text detection says '{}' (confidence: {:.0}%)",
                    locale_lang,
                    detected,
                    info.confidence() * 100.0
                );
                return detected;
            }
        }
    }

    locale_lang
}

/// Collect visible text content from XFA nodes for language detection.
///
/// Walks the tree collecting text from `Draw` and `Field` caption nodes,
/// skipping metadata subtrees (`variables`, `script`, `proto`, `pageSet`).
/// Stops after collecting ~500 characters (enough for reliable detection).
fn collect_visible_text(nodes: &[XfaNode]) -> String {
    let mut buf = String::with_capacity(512);
    collect_visible_text_recursive(nodes, &mut buf, false);
    buf
}

fn collect_visible_text_recursive(nodes: &[XfaNode], buf: &mut String, inside_draw: bool) {
    const MAX_LEN: usize = 5000;

    for node in nodes {
        if buf.len() >= MAX_LEN {
            return;
        }

        // Skip metadata and non-content subtrees
        match &node.kind {
            XfaNodeKind::Element { tag_name, .. } => {
                if matches!(
                    tag_name.as_str(),
                    "variables"
                        | "script"
                        | "proto"
                        | "pageSet"
                        | "event"
                        | "calculate"
                        | "items"
                        | "validate"
                        | "bind"
                ) {
                    continue;
                }
                // Enter caption subtrees (field labels contain prose)
                if tag_name == "caption" {
                    collect_visible_text_recursive(&node.children, buf, true);
                    continue;
                }
            }
            XfaNodeKind::PageSet => continue,
            _ => {}
        }

        // Only collect text if we're inside a Draw node or caption
        let in_content = inside_draw || matches!(node.kind, XfaNodeKind::Draw);

        if in_content {
            match &node.kind {
                XfaNodeKind::Text { content } => {
                    let trimmed = content.trim();
                    if !trimmed.is_empty() && trimmed.len() > 3 {
                        buf.push_str(trimmed);
                        buf.push(' ');
                    }
                }
                XfaNodeKind::Element {
                    text_content: Some(text),
                    ..
                } => {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() && trimmed.len() > 3 {
                        buf.push_str(trimmed);
                        buf.push(' ');
                    }
                }
                _ => {}
            }
        }

        // Recurse into children, marking that we're inside a Draw subtree
        collect_visible_text_recursive(&node.children, buf, in_content);
    }
}

/// Map `whatlang::Lang` to ISO 639-1 two-letter code.
fn whatlang_to_iso(lang: whatlang::Lang) -> String {
    use whatlang::Lang;
    match lang {
        Lang::Deu => "de".to_string(),
        Lang::Eng => "en".to_string(),
        Lang::Fra => "fr".to_string(),
        Lang::Ita => "it".to_string(),
        Lang::Spa => "es".to_string(),
        Lang::Por => "pt".to_string(),
        Lang::Nld => "nl".to_string(),
        Lang::Pol => "pl".to_string(),
        Lang::Tur => "tr".to_string(),
        Lang::Rus => "ru".to_string(),
        Lang::Jpn => "ja".to_string(),
        Lang::Cmn => "zh".to_string(),
        Lang::Kor => "ko".to_string(),
        Lang::Ara => "ar".to_string(),
        other => format!("{:?}", other).to_lowercase(),
    }
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
