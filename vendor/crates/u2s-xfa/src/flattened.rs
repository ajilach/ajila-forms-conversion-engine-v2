use crate::xfa::font_manager::{DocumentFonts, FontManager, LineGeometry, get_font_manager};
// DEVIATION: upstream imports Presence via `scripting::{Presence, SomPath}`,
// which routes through scripting/state.rs and pulls in boa_engine. Presence is
// actually defined in xfa/mod.rs, and SomPath in scripting/som.rs — neither
// needs a JS engine. Importing them directly keeps the whole scripting tree
// out of the render build.
use crate::draw_text::draw_text_mut;
use crate::xfa::Presence;
use crate::xfa::scripting::som::{SomPath, child_som_path, sibling_indices};
use crate::xfa::text_metrics::{FontMetrics, LineMetrics, RunFontSet, TextMeasurer, space_advance, text_advance, wrap_words, xfa_px_scale};
use crate::xfa::KerningMode;
use crate::xfa::pagination::{
    self, BreakRequest, BreakTarget, Extent, PageRequest, PageSets, Paginator, Splitting, Target,
};
use crate::xfa::{
    Border, Font, FontPosture, FontWeight, HAlign, Num, Para, StrokeStyle, VAlign, XfaNode,
    XfaNodeKind, num,
};
use ab_glyph::{Font as AbGlyphFont, FontRef, PxScale, ScaleFont};
use base64::Engine as _;
use image::{ImageBuffer, Rgba, RgbaImage};
use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

#[derive(Clone)]
pub struct Flattened {
    pub page: Page,
    pub children: Vec<FlattenedKind>,
    /// The document's locale as the root subform states it (`de_CH`), or
    /// `en_US` (XFA 3.3 "Determining the Prevailing Locale").
    pub language: String,
    /// SOM paths of every pageArea of the template, in document order.
    pub page_areas: Vec<String>,
    /// The document's own fonts (embedded faces, `equate`s): entered while
    /// the layout is measured and drawn.
    pub fonts: Arc<DocumentFonts>,
}

// ============================================================================
// Recursive Flattened Structure
// ============================================================================

/// A flattened element: either a group (with hints) or a leaf node.
/// Groups can be nested recursively and carry semantic hints.
/// Leaf nodes contain actual layout information (position, dimensions, content).
#[derive(Debug, Clone)]
pub enum FlattenedKind {
    /// A group of elements with optional hints. Nothing in the XFA
    /// flattener emits one: repeated instances are ordinary sibling nodes,
    /// each carrying its own indexed `Hint::SomPath`.
    Group {
        /// Child elements (can be nested groups or leaf nodes)
        children: Vec<FlattenedKind>,
        /// Semantic hints for this group (format-agnostic)
        hints: Vec<Hint>,
    },
    /// A leaf node with position and rendering information
    Node(FlattenedNode),
}

impl FlattenedKind {
    /// Create a new group with children and hints
    pub fn group(children: Vec<FlattenedKind>, hints: Vec<Hint>) -> Self {
        FlattenedKind::Group { children, hints }
    }

    /// Create a leaf node
    pub fn node(node: FlattenedNode) -> Self {
        FlattenedKind::Node(node)
    }

    /// Get hints for this element (both groups and nodes can have hints)
    pub fn hints(&self) -> &[Hint] {
        match self {
            FlattenedKind::Group { hints, .. } => hints,
            FlattenedKind::Node(node) => &node.hints,
        }
    }

    /// Get mutable hints for this element (both groups and nodes can have hints)
    pub fn hints_mut(&mut self) -> &mut Vec<Hint> {
        match self {
            FlattenedKind::Group { hints, .. } => hints,
            FlattenedKind::Node(node) => &mut node.hints,
        }
    }

    /// Add a hint to this element (works for both groups and nodes)
    pub fn add_hint(&mut self, hint: Hint) {
        match self {
            FlattenedKind::Group { hints, .. } => {
                let discriminant = hint.discriminant();
                hints.retain(|h| h.discriminant() != discriminant);
                hints.push(hint);
            }
            FlattenedKind::Node(node) => {
                node.add_hint(hint);
            }
        }
    }

    /// Get a hint by discriminant
    pub fn get_hint(&self, discriminant: &str) -> Option<&Hint> {
        self.hints()
            .iter()
            .find(|h| h.discriminant() == discriminant)
    }

    /// Returns true if this is a group
    pub fn is_group(&self) -> bool {
        matches!(self, FlattenedKind::Group { .. })
    }

    /// Returns true if this is a leaf node
    pub fn is_node(&self) -> bool {
        matches!(self, FlattenedKind::Node(_))
    }

    /// Get the underlying node if this is a leaf node
    pub fn as_node(&self) -> Option<&FlattenedNode> {
        match self {
            FlattenedKind::Node(node) => Some(node),
            FlattenedKind::Group { .. } => None,
        }
    }

    /// Get the underlying node mutably if this is a leaf node
    pub fn as_node_mut(&mut self) -> Option<&mut FlattenedNode> {
        match self {
            FlattenedKind::Node(node) => Some(node),
            FlattenedKind::Group { .. } => None,
        }
    }

    /// Get children if this is a group
    pub fn children(&self) -> Option<&[FlattenedKind]> {
        match self {
            FlattenedKind::Group { children, .. } => Some(children),
            FlattenedKind::Node(_) => None,
        }
    }

    /// Iterate over all leaf nodes recursively
    pub fn iter_nodes(&self) -> FlattenedNodeIter<'_> {
        FlattenedNodeIter::new(std::slice::from_ref(self))
    }

    /// Count all leaf nodes recursively
    pub fn node_count(&self) -> usize {
        match self {
            FlattenedKind::Node(_) => 1,
            FlattenedKind::Group { children, .. } => children.iter().map(|c| c.node_count()).sum(),
        }
    }
}

/// Iterator over all leaf nodes in a FlattenedKind tree
pub struct FlattenedNodeIter<'a> {
    stack: Vec<std::slice::Iter<'a, FlattenedKind>>,
}

impl<'a> FlattenedNodeIter<'a> {
    fn new(children: &'a [FlattenedKind]) -> Self {
        FlattenedNodeIter {
            stack: vec![children.iter()],
        }
    }
}

impl<'a> Iterator for FlattenedNodeIter<'a> {
    type Item = &'a FlattenedNode;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(iter) = self.stack.last_mut() {
            match iter.next() {
                Some(FlattenedKind::Node(node)) => return Some(node),
                Some(FlattenedKind::Group { children, .. }) => {
                    self.stack.push(children.iter());
                }
                None => {
                    self.stack.pop();
                }
            }
        }
        None
    }
}

/// Viewer chrome a rendering may add on top of what the template draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderOptions {
    /// Underline every open (`access="open"`) text or choice field along its
    /// bottom edge, the way Acrobat highlights a fillable field. Not part of
    /// the template: off by default.
    pub field_underline: bool,
}

/// One page of the layout: where it sits in the column, its size, and the
/// `pageArea` it is an instance of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSlot {
    /// Column Y of the page's top edge.
    pub top: Num,
    pub width: Num,
    pub height: Num,
    /// SOM path of the pageArea (`form1.MPs.MP`); empty for a document
    /// without one.
    pub page_area: String,
    /// The pageArea's name, which page-dependent master scripts are keyed by.
    pub page_area_name: String,
    /// Its content areas, in document order, in page coordinates.
    pub content_areas: Vec<Bounds>,
    /// A page holding no foreground content: a filler for a break to an odd
    /// or even page, a blank designated last page, an occurrence minimum.
    pub blank: bool,
}

/// The pages of a layout: a stack of whole pages, each at its own size.
/// The flattened column is exactly as tall as the pages together; page `k`
/// owns `[pages[k].top, pages[k].top + pages[k].height)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// The first page's width, for callers that need one size for the
    /// document; every page's own size is in `pages`.
    pub width: Num,
    /// The first page's height.
    pub height: Num,
    /// Column Y positions where a page starts, for pages 2 on.
    pub page_breaks: Vec<Num>,
    pub page_count: u32,
    pub pages: Vec<PageSlot>,
}

impl Page {
    /// A single page of `width` x `height` whose content area is the whole
    /// page.
    pub fn new(width: Num, height: Num) -> Self {
        Self::uniform(width, height, 1)
    }

    /// `count` pages of one size, each a whole-page content area.
    pub fn uniform(width: Num, height: Num, count: u32) -> Self {
        let pages = (0..count.max(1))
            .map(|k| PageSlot {
                top: height * Decimal::from(k),
                width,
                height,
                page_area: String::new(),
                page_area_name: String::new(),
                content_areas: vec![Bounds::new(Decimal::ZERO, Decimal::ZERO, width, height)],
                blank: false,
            })
            .collect();
        Self::stacked(pages)
    }

    /// Pages from their slots, which must be stacked without gaps.
    pub fn stacked(pages: Vec<PageSlot>) -> Self {
        assert!(!pages.is_empty(), "a layout has at least one page");
        let mut expected = Decimal::ZERO;
        for p in &pages {
            assert_eq!(p.top, expected, "pages are stacked without gaps");
            expected += p.height;
        }
        Page {
            width: pages[0].width,
            height: pages[0].height,
            page_breaks: pages.iter().skip(1).map(|p| p.top).collect(),
            page_count: pages.len() as u32,
            pages,
        }
    }

    /// The height of the whole flattened column: every page, at full height.
    pub fn column_height(&self) -> Num {
        self.pages
            .last()
            .map(|p| p.top + p.height)
            .unwrap_or(Decimal::ZERO)
    }

    /// The widest page.
    pub fn column_width(&self) -> Num {
        self.pages.iter().map(|p| p.width).max().unwrap_or(Decimal::ZERO)
    }

    /// The 0-based index of the page a column Y lands on: above the first
    /// page is the first, past the last is the last.
    pub fn index_of_y(&self, y: Num) -> usize {
        self.pages.partition_point(|p| p.top <= y).saturating_sub(1)
    }
}

/// What one page's own scripts made of its master page.
///
/// Master-page content is laid out once per page, and scripts on it can depend
/// on which page that is (`xfa.layout.page()`, `pageArea.index`). These are the
/// values and presences that came out of evaluating them for one page; they win
/// over the document-wide ones for that page only.
#[derive(Debug, Clone, Default)]
pub struct PageOverrides {
    /// Field values, keyed by full SOM path.
    pub values: HashMap<SomPath, String>,
    /// Presence, keyed by full SOM path.
    pub presence: HashMap<String, Presence>,
}

impl PageOverrides {
    /// This page's values laid over the document-wide ones.
    pub fn merged_with(&self, base: &HashMap<SomPath, String>) -> HashMap<SomPath, String> {
        if self.values.is_empty() {
            return base.clone();
        }
        let mut merged = base.clone();
        for (k, v) in &self.values {
            merged.insert(k.clone(), v.clone());
        }
        merged
    }
}

/// Rendering style information
#[derive(Debug, Clone, Default)]
pub struct RenderStyle {
    /// Border properties
    pub border: Option<Border>,
    /// Font properties
    pub font: Option<Font>,
    /// Paragraph properties  
    pub para: Option<Para>,
}

impl RenderStyle {
    /// Returns (default_bold, default_italic) from the XFA font element.
    /// This should be called BEFORE CSS overrides are applied, so rich text
    /// parsing gets the original XFA element's font weight/posture as defaults.
    /// Per XFA spec Chapter 27: paragraph-level CSS styling is independent.
    pub fn get_xfa_bold_italic(&self) -> (bool, bool) {
        let bold = self
            .font
            .as_ref()
            .map(|f| f.weight.is_bold())
            .unwrap_or(false);
        let italic = self
            .font
            .as_ref()
            .map(|f| f.posture == crate::xfa::FontPosture::Italic)
            .unwrap_or(false);
        (bold, italic)
    }
}

/// Main flattened node structure containing position and rendering information
#[derive(Debug, Clone)]
pub struct FlattenedNode {
    /// Node-specific information
    pub kind: FlattenedNodeKind,

    /// Position and dimensions
    pub x: Num,
    pub y: Num,
    pub width: Num,
    pub height: Num,

    /// Rotation in degrees (counter-clockwise, multiples of 90)
    pub rotate: i32,

    /// Rendering style
    pub style: RenderStyle,

    /// Semantic hints for this node (node-specific hints like RichContent, Validation, etc.)
    pub hints: Vec<Hint>,

    /// When true the renderer must not re-wrap the text content.
    /// Set for AcroForm text nodes whose width was computed from the PDF
    /// content stream glyph widths — re-measuring with the resolved render
    /// font may produce different (wider) widths and cause unwanted wrapping.
    pub no_wrap: bool,
    /// The locale that prevails at this node (XFA 3.3 "Determining the
    /// Prevailing Locale": the nearest `locale` attribute on it or an
    /// enclosing container), as the template wrote it. `None` when the
    /// node came from a context without one, which means the document's.
    pub locale: Option<String>,
}

/// Enum representing the specific kind of flattened node
#[derive(Debug, Clone)]
pub enum FlattenedNodeKind {
    /// Text/draw element
    Text {
        content: String,
        font_size: Num,
        font_name: String,
        /// Name of the source XFA node (for Draw elements with scripts)
        source_name: Option<String>,
    },

    /// Input field
    Field {
        name: String,
        value: String,
        label: String,
        /// For checkButton fields: whether the button is currently "on" (checked/selected)
        /// Per XFA spec:
        /// - Standalone checkbox: on when rawValue == "on" (or the "on" item value)
        /// - Radio in exclGroup: on when exclGroup.rawValue == this field's item key
        is_checked: Option<bool>,
    },

    /// An embedded raster image: a `<draw>` whose value is an `<image>`, such
    /// as a logo on the master page.
    Image(EmbeddedImage),

    /// A `<draw>` whose value is a `<line>`, `<rectangle>` or `<arc>`: a
    /// rule, a box, an ellipse, drawn in the node's box.
    Shape(crate::xfa::shapes::Shape),
}

/// An image embedded in the template, decoded from base64 once at flatten time
/// so nothing downstream has to hold an undecodable string.
#[derive(Debug, Clone)]
pub struct EmbeddedImage {
    /// The encoded image file (PNG, JPEG, TIFF, ...), as the template carried it.
    pub data: Arc<Vec<u8>>,
    /// The declared MIME type, for diagnostics when the bytes will not decode.
    pub content_type: String,
    /// How the image fills its box.
    pub aspect: ImageAspect,
    /// Name of the source XFA node, for diagnostics.
    pub source_name: Option<String>,
}

/// XFA `<image aspect>`: how an image is fitted to the box it is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageAspect {
    /// Scale to fit inside the box, keeping the aspect ratio (the XFA default).
    #[default]
    Fit,
    /// The image's own size: its pixels at 72 per inch, the box playing no
    /// part. (An image's own resolution metadata is not read.)
    Actual,
    /// Stretch to the box, ignoring the aspect ratio.
    None,
    /// Scale so the width is the box's; the height follows.
    Width,
    /// Scale so the height is the box's; the width follows.
    Height,
}

impl std::str::FromStr for ImageAspect {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "fit" => ImageAspect::Fit,
            "none" => ImageAspect::None,
            "actual" => ImageAspect::Actual,
            "width" => ImageAspect::Width,
            "height" => ImageAspect::Height,
            other => return Err(format!("unknown image aspect {other:?}")),
        })
    }
}

// ============================================================================
// Format-Agnostic Semantic Hints
// ============================================================================

/// Field access level per XFA specification (XFA 3.3 §17, the `access`
/// property of `field`, `exclGroup` and, since XFA 2.8, `subform`).
/// Controls user interaction capabilities with a field or container.
///
/// Serialized under the spec's own keywords (`open`, `nonInteractive`,
/// `protected`, `readOnly`), so what a tool reports is exactly what the
/// template or a script wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldAccess {
    /// Allow update without restriction. User may modify content and navigate to it.
    #[default]
    Open,
    /// Content can be loaded but not updated interactively. Behaves like printed content.
    /// Calculations and scripts can still modify the content.
    NonInteractive,
    /// User cannot make direct changes. Does not participate in tabbing.
    /// Will not generate any events. May allow text selection for copying.
    Protected,
    /// User cannot make direct changes but can tab to it, view/scroll content,
    /// and select content for copying. Generates a subset of events.
    ReadOnly,
}

impl std::str::FromStr for FieldAccess {
    type Err = std::convert::Infallible;

    /// Lenient, for template attributes: anything unrecognised is the spec
    /// default, `open`. A value a *script* assigns goes through
    /// [`FieldAccess::parse_strict`] instead.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(FieldAccess::parse_strict(s).unwrap_or(FieldAccess::Open))
    }
}

impl FieldAccess {
    /// One of the four XFA keywords, or `None`. Used where an unknown value
    /// must not quietly become `open`: a script writing `this.access =
    /// "locked"` has not unlocked its field.
    pub fn parse_strict(s: &str) -> Option<Self> {
        match s {
            "open" => Some(FieldAccess::Open),
            "nonInteractive" => Some(FieldAccess::NonInteractive),
            "protected" => Some(FieldAccess::Protected),
            "readOnly" => Some(FieldAccess::ReadOnly),
            _ => None,
        }
    }

    /// The XFA keyword for this level.
    pub fn as_str(self) -> &'static str {
        match self {
            FieldAccess::Open => "open",
            FieldAccess::NonInteractive => "nonInteractive",
            FieldAccess::Protected => "protected",
            FieldAccess::ReadOnly => "readOnly",
        }
    }

    /// Returns true if this access level allows user interaction/input.
    pub fn is_interactive(&self) -> bool {
        matches!(self, FieldAccess::Open)
    }

    /// How restrictive this level is. XFA 3.3 §17 orders them, from highest
    /// to lowest precedence: nonInteractive, protected, readOnly, open.
    fn restrictiveness(self) -> u8 {
        match self {
            FieldAccess::Open => 0,
            FieldAccess::ReadOnly => 1,
            FieldAccess::Protected => 2,
            FieldAccess::NonInteractive => 3,
        }
    }

    /// The more restrictive of the two: how an object's own access combines
    /// with the one it inherits from an enclosing subform or exclusion
    /// group, since an object may only tighten what it inherits (XFA 3.3
    /// §2 "Access Restrictions", §17 `access`).
    pub fn most_restrictive(self, other: FieldAccess) -> FieldAccess {
        if other.restrictiveness() > self.restrictiveness() {
            other
        } else {
            self
        }
    }
}

/// Dropdown information extracted from a [`Hint::Dropdown`].
#[derive(Debug, Clone, PartialEq)]
pub struct DropdownInfo<'a> {
    /// Pairs of (display text, save value)
    pub options: &'a [(String, String)],
    /// Whether the user can type a custom value
    pub text_entry: bool,
    /// Whether multiple selections are allowed
    pub multi_select: bool,
}

/// Semantic hints that can be attached to any flattened node.
/// These are format-agnostic concepts applicable to XFA, PDF AcroForms, HTML forms, etc.
/// Multiple hints can be attached to a single node; they are deduplicated by discriminant.
#[derive(Debug, Clone, PartialEq)]
pub enum Hint {
    /// A field's pictures, length and the rest of its value metadata (see
    /// [`crate::xfa::field_meta`]): how its value is shown is computed from
    /// this when it is drawn or read, so it always matches the value.
    ValueFormat(Arc<crate::xfa::field_meta::FieldMeta>),
    /// Accessibility information
    Accessibility {
        /// Semantic role (e.g., "TH" for table header, "TR" for table row)
        role: Option<String>,
        /// Tooltip text for user assistance
        tool_tip: Option<String>,
        /// Text to be spoken by screen readers
        speak_text: Option<String>,
    },

    /// Validation constraints
    Validation {
        /// Whether the field is required (nullTest)
        required: bool,
        /// Format pattern for validation (picture clause)
        format_pattern: Option<String>,
        /// Custom error message
        error_message: Option<String>,
    },

    /// Field behavior properties
    FieldBehavior {
        /// Field access level (open, nonInteractive, protected, readOnly)
        access: FieldAccess,
        /// Whether multiline input is allowed
        multiline: bool,
        /// Maximum character count
        max_length: Option<u32>,
        /// Number of comb cells (for comb-style input)
        comb_cells: Option<u32>,
    },

    /// Widget type information
    WidgetType(WidgetKind),

    /// A check box or radio button's `<checkButton>`: its size, mark and
    /// shape, which is how it is drawn.
    CheckButton(crate::xfa::CheckButton),

    /// Caption/label information
    Caption {
        /// Position relative to field (left, right, top, bottom, inline)
        placement: CaptionPlacement,
        /// Caption text content
        text: Option<String>,
        /// The space the caption takes along its side, in points: the
        /// value is drawn in the rest of the field's box.
        reserve: Num,
    },

    /// Layout break hints
    LayoutBreak {
        /// Break before this element
        before: bool,
        /// Break after this element
        after: bool,
        /// Keep this element together (don't split across breaks)
        keep_together: bool,
    },

    /// Rich text content (HTML, formatted text)
    RichContent(RichText),

    /// Data binding reference
    DataBinding {
        /// Generic data reference path
        ref_path: Option<String>,
    },

    /// Master page content indicator (page background elements)
    /// Elements on the master page are outside the contentArea and typically
    /// contain headers, footers, or background decorations.
    MasterPage {
        /// Which region of the master page this element is in
        region: MasterPageRegion,
    },

    /// Non-printable content indicator
    /// Elements with relevant="-print" attribute should not appear in print output.
    /// This is used for screen-only interactive elements.
    NoPrint,

    /// SOM (Scripting Object Model) path for this field
    /// The full hierarchical path in the XFA form tree (e.g., "Form.Page.Subform.Field")
    /// Used to uniquely identify fields and for radio button group naming.
    SomPath(SomPath),

    /// The `<ui><barcode>` of a barcode field.
    Barcode(crate::xfa::Barcode),

    /// SOM path of the parent exclGroup (for radio buttons)
    /// When a field is inside an exclGroup, this stores the exclGroup's full SOM path.
    /// Used by the radio button grouper to name the group correctly.
    ExclGroupSomPath(SomPath),

    /// Dropdown options extracted from XFA <items> elements.
    /// Each pair is (display_value, save_value) per XFA spec.
    /// When only one <items> element exists, display and save values are the same.
    Dropdown {
        /// Pairs of (display text, save value)
        options: Vec<(String, String)>,
        /// Whether the user can type a custom value (choiceList textEntry="1")
        text_entry: bool,
        /// Whether multiple selections are allowed (choiceList open="multiSelect")
        multi_select: bool,
    },
}

/// Region classification for master page (page background) content.
/// Based on element position relative to contentArea bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MasterPageRegion {
    /// Element is primarily above the contentArea (header region)
    /// Classified when >50% of element area is above contentArea.y
    Header,
    /// Element is primarily below the contentArea (footer region)
    /// Classified when >50% of element area is below contentArea.y + contentArea.h
    Footer,
    /// Element is alongside or overlapping the contentArea (background/sidebar)
    Background,
}

impl Hint {
    /// Returns a discriminant identifier for deduplication.
    /// Two hints with the same discriminant are considered duplicates.
    pub fn discriminant(&self) -> &'static str {
        match self {
            Hint::Accessibility { .. } => "Accessibility",
            Hint::Validation { .. } => "Validation",
            Hint::FieldBehavior { .. } => "FieldBehavior",
            Hint::WidgetType(_) => "WidgetType",
            Hint::CheckButton(_) => "CheckButton",
            Hint::Caption { .. } => "Caption",
            Hint::LayoutBreak { .. } => "LayoutBreak",
            Hint::RichContent(_) => "RichContent",
            Hint::DataBinding { .. } => "DataBinding",
            Hint::MasterPage { .. } => "MasterPage",
            Hint::NoPrint => "NoPrint",
            Hint::SomPath(_) => "SomPath",
            Hint::Barcode(_) => "Barcode",
            Hint::ExclGroupSomPath(_) => "ExclGroupSomPath",
            Hint::Dropdown { .. } => "Dropdown",
            Hint::ValueFormat(_) => "ValueFormat",
        }
    }
}

/// Widget type for field elements
#[derive(Debug, Clone, PartialEq)]
pub enum WidgetKind {
    /// Single-line text input
    Text,
    /// Multi-line text area
    TextArea,
    /// Checkbox (square, can be standalone)
    Checkbox,
    /// Radio button (round, part of exclusive group)
    Radio,
    /// Dropdown/select list
    Dropdown,
    /// Date picker
    Date,
    /// Time picker
    Time,
    /// DateTime picker
    DateTime,
    /// Numeric input
    Numeric,
    /// Password input (masked)
    Password,
    /// Digital signature field
    Signature,
    /// Barcode display
    Barcode,
    /// Push button
    Button,
    /// Image selection/display
    Image,
}

impl WidgetKind {
    /// Whether a person fills this widget by typing or picking a value: the
    /// edit widgets of XFA 3.3 "The ui element" (`textEdit`, `numericEdit`,
    /// `passwordEdit`, `dateTimeEdit`, `choiceList`). A button, check
    /// button, barcode, image or signature takes no typed value.
    pub fn takes_typed_value(&self) -> bool {
        matches!(
            self,
            WidgetKind::Text
                | WidgetKind::TextArea
                | WidgetKind::Dropdown
                | WidgetKind::Date
                | WidgetKind::Time
                | WidgetKind::DateTime
                | WidgetKind::Numeric
                | WidgetKind::Password
        )
    }
}

/// Caption placement relative to its field
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum CaptionPlacement {
    #[default]
    Left,
    Right,
    Top,
    Bottom,
    Inline,
}

// ============================================================================
// XFA-Compliant Rich Text Model
// ============================================================================

/// A rich text document consisting of multiple paragraphs.
/// Per XFA spec, rich text in exData contentType="text/html" is structured as
/// XHTML paragraphs with inline styling.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RichText {
    /// Paragraphs in the document
    pub paragraphs: Vec<RichParagraph>,
}

/// A single paragraph with optional styling and text runs.
/// Per XFA spec (Chapter 27): paragraphs can have text-indent, margins, alignment.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RichParagraph {
    /// Text runs within the paragraph
    pub runs: Vec<RichRun>,
    /// First-line text indent (from CSS text-indent style)
    pub text_indent: Option<f32>,
    /// Horizontal alignment
    pub h_align: HAlign,
    /// Space above paragraph
    pub space_above: Option<f32>,
    /// Space below paragraph
    pub space_below: Option<f32>,
    /// Whether this is an empty paragraph (just whitespace/separator)
    pub is_empty: bool,
    /// Per-paragraph font size override (from CSS font-size on `<p>` style)
    pub font_size: Option<f32>,
    /// Per-paragraph typeface (from CSS font-family on `<p>` style).
    pub font_family: Option<String>,
    /// Per-paragraph line height override (from CSS line-height on `<p>` style)
    pub line_height: Option<f32>,
    /// Per-paragraph left margin (from CSS margin-left on `<p>` style)
    /// Per XFA spec: reduces available width for text wrapping
    pub margin_left: Option<f32>,
    /// Per-paragraph right margin (from CSS margin-right on `<p>` style)
    pub margin_right: Option<f32>,
    /// Whether this empty paragraph was created by a `<br/>` element.
    /// Distinguishes `<p><br/></p>` (structural placeholder in rich text)
    /// from empty U+2029 segments in plain text (vertical spacers).
    pub has_br: bool,
    /// Per-paragraph CSS letter-spacing (in pt). From `<p style="letter-spacing:...">`
    pub letter_spacing: Option<f32>,
}

/// A run of text with uniform styling.
/// Per XFA spec: spans can have xfa-spacerun:yes to preserve consecutive spaces.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RichRun {
    /// The text content
    pub text: String,
    /// Whether consecutive spaces should be preserved (xfa-spacerun:yes)
    pub preserve_spaces: bool,
    /// Font weight (bold)
    pub bold: bool,
    /// Font style (italic)
    pub italic: bool,
    /// Underline
    pub underline: bool,
    /// Superscript (detected from positive vertical-align + small font-size)
    pub superscript: bool,
    /// Per-run CSS letter-spacing override (in pt). From `<span style="letter-spacing:...">`
    pub letter_spacing: Option<f32>,
    /// Strikethrough
    pub line_through: bool,
}

impl RichRun {
    /// The run's underline and strikethrough.
    pub fn decoration(&self) -> Decoration {
        Decoration {
            underline: self.underline,
            line_through: self.line_through,
            ..Decoration::default()
        }
    }
}

/// A run's lines: underline and strikethrough (XFA 3.3 "Underline and
/// Strikethrough"; the font element's `underline`/`lineThrough`, rich
/// text's CSS `text-decoration`). Inherited by nested text like bold and
/// italic.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Decoration {
    pub underline: bool,
    pub line_through: bool,
    /// A double underline (`underline="2"`, CSS `double`).
    pub double_underline: bool,
    /// A double line through (`lineThrough="2"`).
    pub double_line_through: bool,
    /// The underline breaks at word breaks (`underlinePeriod="word"`).
    pub word_period: bool,
}

impl Decoration {
    /// The rectangles (x, y, width, height in pixels) of this decoration's
    /// lines under or through a run of text `width` wide whose line box
    /// starts at `top`, for a font with `ascent` and `descent` (positive) at
    /// `font_px` pixels. The face's own line geometry places them when it
    /// is known; otherwise the underline is a third of the descent below the
    /// baseline, the strikethrough a third of the ascent above it, each a
    /// fourteenth of the font size thick and at least a pixel. A double line
    /// is a second one a thickness further out.
    pub fn line_rects(
        self,
        x: f32,
        top: f32,
        width: f32,
        ascent: f32,
        descent: f32,
        font_px: f32,
        geometry: Option<LineGeometry>,
    ) -> Vec<(i32, i32, i32, i32)> {
        let default_thickness = (font_px / 14.0).round().max(1.0);
        let baseline = top + ascent;
        let mut rects = Vec::new();
        let mut line_at = |y: f32, thickness: f32, double: bool| {
            let t = thickness.round().max(1.0) as i32;
            rects.push((x.round() as i32, y.round() as i32, width.round().max(1.0) as i32, t));
            if double {
                rects.push((x.round() as i32, y.round() as i32 + 2 * t, width.round().max(1.0) as i32, t));
            }
        };
        if self.underline {
            let (offset, thickness) = geometry
                .map(|g| (g.underline_offset, g.underline_thickness))
                .unwrap_or((descent / 3.0, default_thickness));
            line_at(baseline + offset, thickness, self.double_underline);
        }
        if self.line_through {
            let (offset, thickness) = geometry
                .and_then(|g| g.strike_offset.zip(g.strike_thickness))
                .unwrap_or((ascent / 3.0, default_thickness));
            line_at(baseline - offset, thickness, self.double_line_through);
        }
        rects
    }

    /// The decoration a `<font>` sets: `underline`/`lineThrough` 1 for a
    /// single line, 2 for a double one, and `underlinePeriod`.
    pub fn of_font(font: Option<&crate::xfa::Font>) -> Self {
        font.map(|f| Decoration {
            underline: f.underline,
            line_through: f.line_through,
            double_underline: f.underline_double,
            double_line_through: f.line_through_double,
            word_period: f.underline_period_word,
        })
        .unwrap_or_default()
    }

    /// This decoration as an element's CSS `style` changes it. Only a
    /// `text-decoration` declaration does: `none` clears both lines, and
    /// `underline`/`line-through` (alone or together) set exactly those,
    /// with the rich text reference's `double` and `word` keywords.
    pub fn with_css(self, style: &str) -> Self {
        let Some(value) = style.split(';').find_map(|decl| {
            let (name, value) = decl.split_once(':')?;
            (name.trim() == "text-decoration").then(|| value.trim().to_ascii_lowercase())
        }) else {
            return self;
        };
        if value == "none" {
            return Decoration::default();
        }
        Decoration {
            underline: value.contains("underline"),
            line_through: value.contains("line-through"),
            double_underline: value.contains("underline") && value.contains("double"),
            double_line_through: value.contains("line-through") && value.contains("double"),
            word_period: value.contains("word"),
        }
    }
}

/// A positioned word/token ready for rendering.
/// Used for glyph-by-glyph rendering with proper justify support.
#[derive(Debug, Clone)]
pub struct RenderedWord {
    /// The text of this word
    pub text: String,
    /// X position in pixels
    pub x: f32,
    /// Whether spaces should be preserved (from xfa-spacerun)
    pub preserve_spaces: bool,
    /// Whether this word should be rendered bold
    pub bold: bool,
    /// Whether this word should be rendered italic
    pub italic: bool,
    /// Underline and strikethrough to draw with this word.
    pub decoration: Decoration,
}

/// A line of text ready for rendering, with positioning info.
#[derive(Debug, Clone)]
pub struct RenderedLine {
    /// Words/tokens in this line
    pub words: Vec<RenderedWord>,
    /// Y position of the line baseline
    pub y: f32,
    /// Whether this is the first line of a paragraph (for text-indent)
    pub is_first_line: bool,
    /// Whether this is the last line of a paragraph (for justify - don't stretch)
    pub is_last_line: bool,
    /// The paragraph's text indent (only applied if is_first_line)
    pub text_indent: f32,
    /// Horizontal alignment for this line
    pub h_align: HAlign,
    /// Total width of all content on this line
    pub content_width: f32,
    /// Per-paragraph left margin (from CSS margin-left), already scaled.
    /// `Some(value)` means CSS explicitly set margin-left (even if 0),
    /// `None` means no CSS override (use XFA `<para marginLeft>` default).
    pub margin_left: Option<f32>,
}

/// One paragraph of a rich-text block as laid out for measurement (see
/// `Flattened::rich_paragraph_lines`).
struct MeasuredParagraph {
    /// The paragraph's effective font: the block's with the paragraph's own
    /// CSS family and size applied.
    font: Font,
    /// The paragraph's effective properties over the draw's `<para>`.
    props: Para,
    /// Its lines as `layout_rich_text` wraps them; `None` when no face could
    /// be loaded for `font`. An empty paragraph has no lines and is one line
    /// tall.
    lines: Option<Vec<RenderedLine>>,
}

/// A token for text layout - a word or preserved space run.
/// Used internally during text layout.
#[derive(Debug, Clone)]
pub struct LayoutToken {
    pub text: String,
    pub width: f32,
    pub preserve_spaces: bool,
    /// Whether this token should be rendered bold
    pub bold: bool,
    /// Whether this token should be rendered italic
    pub italic: bool,
    /// Underline and strikethrough. A token merged from two runs keeps the
    /// first one's, as it keeps its bold and italic.
    pub decoration: Decoration,
}

/// The font variants a rich-text run can be measured and rendered with.
///
/// A `<span style="font-weight:bold">` (or an XFA `<font weight="bold">`)
/// run is a genuinely different face -- typically wider -- not the base font
/// drawn with a synthetic bold stroke. Wrapping has to measure each run with
/// the same face `render_text_glyph_by_glyph` will later draw it with, or a
/// line that measured as fitting can render wider than the column once its
/// bold words are drawn in their real (wider) glyphs.
///
/// A variant the font manager could not resolve falls back to `regular`,
/// mirroring the selection `render_text_glyph_by_glyph` already does.
#[derive(Clone, Copy)]
pub struct RunFonts<'a> {
    pub regular: &'a FontRef<'a>,
    pub bold: Option<&'a FontRef<'a>>,
    pub italic: Option<&'a FontRef<'a>>,
    pub bold_italic: Option<&'a FontRef<'a>>,
}

impl<'a> RunFonts<'a> {
    /// A single font for every run, regardless of its own bold/italic --
    /// e.g. when the caller has already resolved one dominant variant for
    /// the whole paragraph (see `split_draw_into_paragraph_nodes`'s height
    /// measurement, which deliberately does this to avoid a short bold
    /// prefix skewing the width of an otherwise-normal paragraph).
    pub fn only(font: &'a FontRef<'a>) -> Self {
        RunFonts {
            regular: font,
            bold: None,
            italic: None,
            bold_italic: None,
        }
    }

    /// The face to measure/render a run with, given its own `bold`/`italic`.
    fn for_run(&self, bold: bool, italic: bool) -> &FontRef<'a> {
        match (bold, italic) {
            (true, true) => self.bold_italic.unwrap_or_else(|| self.for_run(true, false)),
            (true, false) => self.bold.unwrap_or(self.regular),
            (false, true) => self.italic.unwrap_or(self.regular),
            (false, false) => self.regular,
        }
    }
}

/// Builder for creating FlattenedNode instances with sensible defaults.
/// Consolidates the many constructor variants into a fluent API.
#[derive(Debug, Clone)]
pub struct FlattenedNodeBuilder {
    kind: Option<FlattenedNodeKind>,
    x: Num,
    y: Num,
    width: Num,
    height: Num,
    rotate: i32,
    style: RenderStyle,
    hints: Vec<Hint>,
    no_wrap: bool,
    locale: Option<String>,
}

impl Default for FlattenedNodeBuilder {
    fn default() -> Self {
        Self {
            kind: None,
            x: Decimal::ZERO,
            y: Decimal::ZERO,
            width: Decimal::ZERO,
            height: Decimal::ZERO,
            rotate: 0,
            style: RenderStyle::default(),
            hints: Vec::new(),
            no_wrap: false,
            locale: None,
        }
    }
}

impl FlattenedNodeBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set position and dimensions
    pub fn bounds(mut self, x: Num, y: Num, width: Num, height: Num) -> Self {
        self.x = x;
        self.y = y;
        self.width = width;
        self.height = height;
        self
    }

    /// Set rotation in degrees
    pub fn rotate(mut self, rotate: i32) -> Self {
        self.rotate = rotate;
        self
    }

    /// Set render style
    pub fn style(mut self, style: RenderStyle) -> Self {
        self.style = style;
        self
    }

    /// Configure as a text node
    pub fn text(mut self, content: String, font_size: Num, font_name: String) -> Self {
        self.kind = Some(FlattenedNodeKind::Text {
            content,
            font_size,
            font_name,
            source_name: None,
        });
        self
    }

    /// Configure as a text node with source name (for Draw elements with scripts)
    pub fn text_named(
        mut self,
        content: String,
        font_size: Num,
        font_name: String,
        source_name: Option<String>,
    ) -> Self {
        self.kind = Some(FlattenedNodeKind::Text {
            content,
            font_size,
            font_name,
            source_name,
        });
        self
    }

    /// Configure as a text node with rich text content
    pub fn text_rich(
        mut self,
        content: String,
        font_size: Num,
        font_name: String,
        source_name: Option<String>,
        rich_text: Option<RichText>,
    ) -> Self {
        self.kind = Some(FlattenedNodeKind::Text {
            content,
            font_size,
            font_name,
            source_name,
        });
        if let Some(rt) = rich_text {
            self.hints.push(Hint::RichContent(rt));
        }
        self
    }

    /// Configure as an embedded image node
    pub fn image(mut self, image: EmbeddedImage) -> Self {
        self.kind = Some(FlattenedNodeKind::Image(image));
        self
    }

    /// Configure as a shape node
    pub fn shape(mut self, shape: crate::xfa::shapes::Shape) -> Self {
        self.kind = Some(FlattenedNodeKind::Shape(shape));
        self
    }

    /// Configure as a field node
    pub fn field(mut self, name: String, value: String, label: String) -> Self {
        self.kind = Some(FlattenedNodeKind::Field {
            name,
            value,
            label,
            is_checked: None,
        });
        self
    }

    /// Configure as a field node with checked state (for radio buttons)
    pub fn field_checked(
        mut self,
        name: String,
        value: String,
        label: String,
        is_checked: Option<bool>,
    ) -> Self {
        self.kind = Some(FlattenedNodeKind::Field {
            name,
            value,
            label,
            is_checked,
        });
        self
    }

    /// Add a hint to this node
    pub fn hint(mut self, hint: Hint) -> Self {
        // Deduplicate by discriminant
        let discriminant = hint.discriminant();
        self.hints.retain(|h| h.discriminant() != discriminant);
        self.hints.push(hint);
        self
    }

    /// Add multiple hints to this node
    pub fn hints(mut self, hints: Vec<Hint>) -> Self {
        for hint in hints {
            self = self.hint(hint);
        }
        self
    }

    /// Mark this node as no-wrap (width already measured from PDF content stream).
    pub fn no_wrap(mut self, no_wrap: bool) -> Self {
        self.no_wrap = no_wrap;
        self
    }

    /// Build the FlattenedNode. Panics if kind was not set.
    pub fn build(self) -> FlattenedNode {
        FlattenedNode {
            kind: self
                .kind
                .expect("FlattenedNodeBuilder: kind must be set before building"),
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            rotate: self.rotate,
            style: self.style,
            hints: self.hints,
            no_wrap: self.no_wrap,
            locale: self.locale,
        }
    }
}

impl FlattenedNode {
    /// Create a builder for constructing a FlattenedNode
    pub fn builder() -> FlattenedNodeBuilder {
        FlattenedNodeBuilder::new()
    }

    // ========================================================================
    // Simple factory methods (for backward compatibility and convenience)
    // ========================================================================

    /// Create a new text node (simple version without styling)
    pub fn new_text(
        content: String,
        font_size: Num,
        font_name: String,
        x: Num,
        y: Num,
        width: Num,
        height: Num,
    ) -> Self {
        Self::builder()
            .bounds(x, y, width, height)
            .text(content, font_size, font_name)
            .build()
    }

    /// Create a new field node (simple version without styling)
    pub fn new_field(
        name: String,
        value: String,
        label: String,
        x: Num,
        y: Num,
        width: Num,
        height: Num,
    ) -> Self {
        Self::builder()
            .bounds(x, y, width, height)
            .field(name, value, label)
            .build()
    }

    // ========================================================================
    // Full factory methods (with all options)
    // ========================================================================

    /// Create a new text node with rich text content (for HTML exData)
    pub fn new_text_with_rich_text(
        content: String,
        font_size: Num,
        font_name: String,
        x: Num,
        y: Num,
        width: Num,
        height: Num,
        style: RenderStyle,
        rotate: i32,
        source_name: Option<String>,
        rich_text: Option<RichText>,
    ) -> Self {
        Self::builder()
            .bounds(x, y, width, height)
            .style(style)
            .rotate(rotate)
            .text_rich(content, font_size, font_name, source_name, rich_text)
            .build()
    }

    /// Create a new field node with all options
    pub fn new_field_with_checked(
        name: String,
        value: String,
        label: String,
        x: Num,
        y: Num,
        width: Num,
        height: Num,
        style: RenderStyle,
        rotate: i32,
        is_checked: Option<bool>,
    ) -> Self {
        Self::builder()
            .bounds(x, y, width, height)
            .style(style)
            .rotate(rotate)
            .field_checked(name, value, label, is_checked)
            .build()
    }

    /// Get the bounds of this node.
    pub fn bounds(&self) -> Bounds {
        Bounds::new(self.x, self.y, self.width, self.height)
    }



    // ========================================================================
    // Hint accessor methods
    // ========================================================================

    /// Get a hint by type (returns the first matching hint)
    pub fn get_hint<F, T>(&self, f: F) -> Option<T>
    where
        F: Fn(&Hint) -> Option<T>,
    {
        self.hints.iter().find_map(f)
    }

    /// Check if this node has a specific hint type
    pub fn has_hint(&self, discriminant: &str) -> bool {
        self.hints.iter().any(|h| h.discriminant() == discriminant)
    }

    /// Get rich text content if present
    pub fn rich_text(&self) -> Option<&RichText> {
        self.hints.iter().find_map(|h| match h {
            Hint::RichContent(rt) => Some(rt),
            _ => None,
        })
    }

    /// Return the leading plain-text content of this node (if any).
    ///
    /// Prefers rich-text content when available; falls back to the plain
    /// `Text` content string.
    pub fn leading_text(&self) -> Option<&str> {
        if let Some(rt) = self.rich_text() {
            for para in &rt.paragraphs {
                for run in &para.runs {
                    if !run.text.is_empty() {
                        return Some(run.text.as_str());
                    }
                }
            }
        }
        if let FlattenedNodeKind::Text { content, .. } = &self.kind {
            if !content.is_empty() {
                return Some(content.as_str());
            }
        }
        None
    }

    /// Get field behavior hint if present
    pub fn field_behavior(&self) -> Option<(FieldAccess, bool, Option<u32>, Option<u32>)> {
        self.hints.iter().find_map(|h| match h {
            Hint::FieldBehavior {
                access,
                multiline,
                max_length,
                comb_cells,
            } => Some((*access, *multiline, *max_length, *comb_cells)),
            _ => None,
        })
    }

    /// Get validation hint if present
    pub fn validation(&self) -> Option<(bool, Option<&String>, Option<&String>)> {
        self.hints.iter().find_map(|h| match h {
            Hint::Validation {
                required,
                format_pattern,
                error_message,
            } => Some((*required, format_pattern.as_ref(), error_message.as_ref())),
            _ => None,
        })
    }

    /// Get accessibility hint if present
    pub fn accessibility(&self) -> Option<(Option<&String>, Option<&String>, Option<&String>)> {
        self.hints.iter().find_map(|h| match h {
            Hint::Accessibility {
                role,
                tool_tip,
                speak_text,
            } => Some((role.as_ref(), tool_tip.as_ref(), speak_text.as_ref())),
            _ => None,
        })
    }

    /// Get widget type if present
    pub fn widget_type(&self) -> Option<&WidgetKind> {
        self.hints.iter().find_map(|h| match h {
            Hint::WidgetType(wk) => Some(wk),
            _ => None,
        })
    }

    /// The field's `<checkButton>`, for a check box or radio button.
    pub fn check_button(&self) -> Option<&crate::xfa::CheckButton> {
        self.hints.iter().find_map(|h| match h {
            Hint::CheckButton(b) => Some(b),
            _ => None,
        })
    }

    /// The `<barcode>` this node carries, if it is a barcode field.
    pub fn barcode(&self) -> Option<&crate::xfa::Barcode> {
        self.hints.iter().find_map(|h| match h {
            Hint::Barcode(b) => Some(b),
            _ => None,
        })
    }

    /// Check if this node is interactive based on FieldBehavior hint
    pub fn is_interactive(&self) -> bool {
        self.field_behavior()
            .map(|(access, _, _, _)| access.is_interactive())
            .unwrap_or(true) // Default to interactive if no hint
    }

    /// The field's value metadata, when its template gave any.
    pub fn value_format(&self) -> Option<&crate::xfa::field_meta::FieldMeta> {
        self.hints.iter().find_map(|h| match h {
            Hint::ValueFormat(meta) => Some(meta.as_ref()),
            _ => None,
        })
    }

    /// A field's value as the form shows it: through its format picture
    /// (XFA 3.3 "The format element"), or as it is when it has none or the
    /// value does not fit it. `None` for a node that is not a field.
    pub fn shown_value(&self) -> Option<String> {
        let FlattenedNodeKind::Field { value, .. } = &self.kind else {
            return None;
        };
        Some(
            self.value_format()
                .and_then(|meta| meta.display(value))
                .unwrap_or_else(|| value.clone()),
        )
    }

    /// The part of the box (`x`, `y`, `w`, `h`, device pixels at `scale`
    /// per point) that a field's value is drawn in: all of it, less what
    /// its caption reserves.
    pub fn value_box(&self, x: i32, y: i32, w: i32, h: i32, scale: f32) -> (i32, i32, i32, i32) {
        let Some((placement, reserve)) = self.hints.iter().find_map(|hint| match hint {
            Hint::Caption {
                placement, reserve, ..
            } => Some((*placement, *reserve)),
            _ => None,
        }) else {
            return (x, y, w, h);
        };
        let (_, (vx, vy, vw, vh)) = crate::xfa::caption::split_caption_box(
            (x as f32, y as f32, w as f32, h as f32),
            placement,
            reserve.to_f32().unwrap_or(0.0) * scale,
        );
        (vx.round() as i32, vy.round() as i32, vw.round() as i32, vh.round() as i32)
    }

    /// Get the SOM path hint if present
    pub fn som_path(&self) -> Option<&SomPath> {
        self.hints.iter().find_map(|h| match h {
            Hint::SomPath(p) => Some(p),
            _ => None,
        })
    }

    /// Get the exclusion group SOM path hint if present
    pub fn excl_group_som_path(&self) -> Option<&SomPath> {
        self.hints.iter().find_map(|h| match h {
            Hint::ExclGroupSomPath(p) => Some(p),
            _ => None,
        })
    }

    /// Get dropdown options if present
    pub fn dropdown(&self) -> Option<DropdownInfo<'_>> {
        self.hints.iter().find_map(|h| match h {
            Hint::Dropdown {
                options,
                text_entry,
                multi_select,
            } => Some(DropdownInfo {
                options: options.as_slice(),
                text_entry: *text_entry,
                multi_select: *multi_select,
            }),
            _ => None,
        })
    }

    // ========================================================================
    // Font property helpers
    // ========================================================================

    /// Returns `true` if the node's font weight is Bold.
    pub fn is_bold(&self) -> bool {
        self.style
            .font
            .as_ref()
            .map(|f| f.weight == FontWeight::Bold)
            .unwrap_or(false)
    }

    /// Returns `true` if the node's font posture is Italic.
    pub fn is_italic(&self) -> bool {
        self.style
            .font
            .as_ref()
            .map(|f| f.posture == FontPosture::Italic)
            .unwrap_or(false)
    }

    /// Returns `true` if the node's font has underline set.
    pub fn is_underline(&self) -> bool {
        self.style
            .font
            .as_ref()
            .map(|f| f.underline)
            .unwrap_or(false)
    }

    /// Returns the font size as `f32`, or `None` if no font is set.
    pub fn font_size_f32(&self) -> Option<f32> {
        self.style.font.as_ref().and_then(|f| f.size.to_f32())
    }

    /// Add a hint to this node, deduplicating by discriminant
    pub fn add_hint(&mut self, hint: Hint) {
        let discriminant = hint.discriminant();
        self.hints.retain(|h| h.discriminant() != discriminant);
        self.hints.push(hint);
    }

    /// Add multiple hints to this node
    pub fn add_hints(&mut self, hints: Vec<Hint>) {
        for hint in hints {
            self.add_hint(hint);
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Position {
    pub x: Num,
    pub y: Num,
    pub width: Num,
    pub height: Num,
}

#[derive(Debug, Clone, Copy)]
struct TableCellSlot {
    x: Num,
    width: Num,
}

impl Position {
    pub fn new(x: Num, y: Num, width: Num, height: Num) -> Self {
        Position {
            x,
            y,
            width,
            height,
        }
    }

    pub fn zero() -> Self {
        Position {
            x: Decimal::ZERO,
            y: Decimal::ZERO,
            width: Decimal::ZERO,
            height: Decimal::ZERO,
        }
    }
}

/// Bounding box with geometry helper methods.
///
/// Provides convenient methods for spatial relationship calculations
/// commonly used in document analysis modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub x: Num,
    pub y: Num,
    pub width: Num,
    pub height: Num,
}

impl Bounds {
    /// Create a new Bounds from position and dimensions.
    pub fn new(x: Num, y: Num, width: Num, height: Num) -> Self {
        Bounds {
            x,
            y,
            width,
            height,
        }
    }

    /// Create bounds from a tuple (x, y, width, height).
    pub fn from_tuple(tuple: (Num, Num, Num, Num)) -> Self {
        Bounds {
            x: tuple.0,
            y: tuple.1,
            width: tuple.2,
            height: tuple.3,
        }
    }

    /// Convert to tuple (x, y, width, height).
    pub fn to_tuple(self) -> (Num, Num, Num, Num) {
        (self.x, self.y, self.width, self.height)
    }

    // ========================================================================
    // Edge accessors
    // ========================================================================

    /// Right edge (x + width).
    #[inline]
    pub fn right(&self) -> Num {
        self.x + self.width
    }

    /// Bottom edge (y + height).
    #[inline]
    pub fn bottom(&self) -> Num {
        self.y + self.height
    }

    /// Left edge (alias for x).
    #[inline]
    pub fn left(&self) -> Num {
        self.x
    }

    /// Top edge (alias for y).
    #[inline]
    pub fn top(&self) -> Num {
        self.y
    }

    // ========================================================================
    // Center calculations
    // ========================================================================

    /// Horizontal center (x + width / 2).
    #[inline]
    pub fn center_x(&self) -> Num {
        self.x + self.width / Decimal::TWO
    }

    /// Vertical center (y + height / 2).
    #[inline]
    pub fn center_y(&self) -> Num {
        self.y + self.height / Decimal::TWO
    }

    // ========================================================================
    // Distance calculations
    // ========================================================================

    /// Horizontal gap from this bounds' right edge to another bounds' left edge.
    /// Returns None if other is not to the right (overlapping or reversed).
    pub fn horizontal_gap_to(&self, other: &Bounds) -> Option<Num> {
        if other.x >= self.right() {
            Some(other.x - self.right())
        } else {
            None
        }
    }

    /// Vertical gap from this bounds' bottom edge to another bounds' top edge.
    /// Returns None if other is not below (overlapping or reversed).
    pub fn vertical_gap_to(&self, other: &Bounds) -> Option<Num> {
        if other.y >= self.bottom() {
            Some(other.y - self.bottom())
        } else {
            None
        }
    }

    /// Absolute vertical distance between center points.
    pub fn vertical_center_distance(&self, other: &Bounds) -> Num {
        (self.center_y() - other.center_y()).abs()
    }

    /// Absolute horizontal distance between center points.
    pub fn horizontal_center_distance(&self, other: &Bounds) -> Num {
        (self.center_x() - other.center_x()).abs()
    }

    // ========================================================================
    // Alignment checks
    // ========================================================================

    /// Check if horizontally aligned (centers within tolerance).
    pub fn is_horizontally_aligned(&self, other: &Bounds, tolerance: Num) -> bool {
        self.vertical_center_distance(other) <= tolerance
    }

    /// Check if vertically aligned (centers within tolerance).
    pub fn is_vertically_aligned(&self, other: &Bounds, tolerance: Num) -> bool {
        self.horizontal_center_distance(other) <= tolerance
    }

    /// Check if on the same line (vertical centers within tolerance based on max height).
    pub fn is_on_same_line(&self, other: &Bounds, tolerance: Num) -> bool {
        let max_half_height = self.height.max(other.height) / Decimal::TWO;
        self.vertical_center_distance(other) <= max_half_height + tolerance
    }

    // ========================================================================
    // Overlap checks
    // ========================================================================

    /// Check if this bounds overlaps horizontally with another (within tolerance).
    pub fn overlaps_horizontally(&self, other: &Bounds, tolerance: Num) -> bool {
        !(self.right() < other.x - tolerance || self.x > other.right() + tolerance)
    }

    /// Check if this bounds overlaps vertically with another (within tolerance).
    pub fn overlaps_vertically(&self, other: &Bounds, tolerance: Num) -> bool {
        !(self.bottom() < other.y - tolerance || self.y > other.bottom() + tolerance)
    }

    /// Check if this bounds overlaps with another in both dimensions.
    pub fn overlaps(&self, other: &Bounds) -> bool {
        self.overlaps_horizontally(other, Decimal::ZERO)
            && self.overlaps_vertically(other, Decimal::ZERO)
    }

    // ========================================================================
    // Relative position checks
    // ========================================================================

    /// Check if other is above this bounds (other's bottom <= this top).
    pub fn is_above(&self, other: &Bounds) -> bool {
        other.bottom() <= self.y
    }

    /// Check if other is below this bounds (other's top >= this bottom).
    pub fn is_below(&self, other: &Bounds) -> bool {
        other.y >= self.bottom()
    }

    /// Check if other is to the left of this bounds (other's right <= this left).
    pub fn is_left_of(&self, other: &Bounds) -> bool {
        other.right() <= self.x
    }

    /// Check if other is to the right of this bounds (other's left >= this right).
    pub fn is_right_of(&self, other: &Bounds) -> bool {
        other.x >= self.right()
    }

    // ========================================================================
    // Spatial proximity checks (combined gap + alignment)
    // ========================================================================

    /// Check if `self` is above `other` within a vertical threshold,
    /// aligned horizontally (overlapping within tolerance).
    /// Returns the vertical gap if the check passes.
    pub fn is_above_within(&self, other: &Bounds, max_gap: Num, tolerance: Num) -> Option<Num> {
        let gap = match self.vertical_gap_to(other) {
            Some(g) => g,
            None => {
                // Allow small overlaps: if self's center is above other's center
                // and the overlap is within tolerance, treat as gap=0
                let overlap = self.bottom() - other.y;
                if overlap > Decimal::ZERO
                    && overlap <= tolerance
                    && self.center_y() < other.center_y()
                {
                    Decimal::ZERO
                } else {
                    return None;
                }
            }
        };
        if gap > max_gap {
            return None;
        }
        if !self.overlaps_horizontally(other, tolerance) {
            return None;
        }
        Some(gap)
    }

    /// Check if `self` is below `other` within a vertical threshold,
    /// aligned horizontally (overlapping within tolerance).
    /// Returns the vertical gap if the check passes.
    pub fn is_below_within(&self, other: &Bounds, max_gap: Num, tolerance: Num) -> Option<Num> {
        let gap = match other.vertical_gap_to(self) {
            Some(g) => g,
            None => {
                // Allow small overlaps: if self's center is below other's center
                // and the overlap is within tolerance, treat as gap=0
                let overlap = other.bottom() - self.y;
                if overlap > Decimal::ZERO
                    && overlap <= tolerance
                    && self.center_y() > other.center_y()
                {
                    Decimal::ZERO
                } else {
                    return None;
                }
            }
        };
        if gap > max_gap {
            return None;
        }
        if !self.overlaps_horizontally(other, tolerance) {
            return None;
        }
        Some(gap)
    }

    /// Check if `self` is to the left of `other` within a horizontal threshold,
    /// on the same line (within tolerance).
    /// Returns the horizontal gap if the check passes.
    pub fn is_left_within(&self, other: &Bounds, max_gap: Num, tolerance: Num) -> Option<Num> {
        let gap = match self.horizontal_gap_to(other) {
            Some(g) => g,
            None => {
                // Allow small overlaps: if self's center is left of other's center
                // and the overlap is within tolerance, treat as gap=0
                let overlap = self.right() - other.x;
                if overlap > Decimal::ZERO
                    && overlap <= tolerance
                    && self.center_x() < other.center_x()
                {
                    Decimal::ZERO
                } else {
                    return None;
                }
            }
        };
        if gap > max_gap {
            return None;
        }
        if !self.is_on_same_line(other, tolerance) {
            return None;
        }
        Some(gap)
    }

    /// Check if `self` is to the right of `other` within a horizontal threshold,
    /// on the same line (within tolerance).
    /// Returns the horizontal gap if the check passes.
    pub fn is_right_within(&self, other: &Bounds, max_gap: Num, tolerance: Num) -> Option<Num> {
        let gap = match other.horizontal_gap_to(self) {
            Some(g) => g,
            None => {
                // Allow small overlaps: if self's center is right of other's center
                // and the overlap is within tolerance, treat as gap=0
                let overlap = other.right() - self.x;
                if overlap > Decimal::ZERO
                    && overlap <= tolerance
                    && self.center_x() > other.center_x()
                {
                    Decimal::ZERO
                } else {
                    return None;
                }
            }
        };
        if gap > max_gap {
            return None;
        }
        if !self.is_on_same_line(other, tolerance) {
            return None;
        }
        Some(gap)
    }

    // ========================================================================
    // Bounding box operations
    // ========================================================================

    /// Compute the fraction (0.0–1.0) of `other`'s area that is contained within `self`.
    ///
    /// Returns 0.0 when the two bounds do not overlap at all, and 1.0 when
    /// `other` is fully inside `self`.
    pub fn contains_percentage(&self, other: &Bounds) -> f64 {
        let other_area = other.width * other.height;
        if other_area <= Decimal::ZERO {
            return 0.0;
        }

        // Intersection rectangle
        let ix = self.x.max(other.x);
        let iy = self.y.max(other.y);
        let ix2 = self.right().min(other.right());
        let iy2 = self.bottom().min(other.bottom());

        if ix >= ix2 || iy >= iy2 {
            return 0.0;
        }

        let intersection_area = (ix2 - ix) * (iy2 - iy);
        (intersection_area / other_area).to_f64().unwrap_or(0.0)
    }

    /// Compute union of this bounds with another.
    pub fn union(&self, other: &Bounds) -> Bounds {
        let min_x = self.x.min(other.x);
        let min_y = self.y.min(other.y);
        let max_x = self.right().max(other.right());
        let max_y = self.bottom().max(other.bottom());
        Bounds::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }

    // ========================================================================
    // Shape helpers
    // ========================================================================

    /// Returns `true` if the bounding box is approximately square.
    ///
    /// Computes the ratio `max(w, h) / min(w, h)` and checks that it is
    /// within `tolerance` of 1.0.  Returns `false` for zero-sized boxes.
    pub fn is_square(&self, tolerance: Num) -> bool {
        if self.width.is_zero() || self.height.is_zero() {
            return false;
        }
        let ratio = if self.width > self.height {
            self.width / self.height
        } else {
            self.height / self.width
        };
        (ratio - Num::ONE).abs() <= tolerance
    }

    /// Returns `true` if both width and height are at most `max`.
    pub fn fits_within_size(&self, max: Num) -> bool {
        self.width <= max && self.height <= max
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Layout {
    TopToBottom,            // tb
    LeftToRightTopToBottom, // lr-tb
    RightToLeftTopToBottom, // rl-tb
    LeftToRight,            // lr (alias for lr-tb in many cases)
    Row,                    // row
    RightToLeftRow,         // rl-row
    Position,               // position (absolute positioning)
    Table,                  // table
}

/// Context for flattening XFA nodes into absolute positions.
/// Bundles all state needed during the recursive flattening process:
/// - Embed resolution data (computed_values, id_to_field) for xfa:embed references
/// - Inherited presence from parent containers (inherited_presence)
///
/// Per XFA 3.3 spec (page 221, "Rich Text That Contains External Objects"):
/// External references via xfa:embed are resolved during the layout process.
///
/// Per XFA 3.3 spec (section 2, "Explicitly Concealing Containers"):
/// Children inherit presence from their parent container - if a parent is hidden,
/// all its children are also hidden regardless of their individual presence values.
///
/// Per XFA 3.3 spec (Chapter 9, "The Occur Element"):
/// Subforms can have occurrence constraints that define repeatability.
#[derive(Clone)]
pub struct FlattenContext<'a> {
    /// Map of field SOM path -> computed value from scripts
    pub computed_values: &'a HashMap<SomPath, String>,
    /// Map of element ID -> field name for resolving embed URI references
    pub id_to_field: &'a HashMap<String, String>,
    /// Inherited presence from parent - if Hidden or Inactive, children are also hidden
    pub inherited_presence: Option<Presence>,
    /// For fields inside an exclGroup: the parent exclGroup's current value
    /// Used to determine if a radio button should be rendered as "checked"
    pub parent_exclgroup_value: Option<String>,
    /// For fields inside an exclGroup: the parent exclGroup's SOM path
    /// Used for radio button group naming in the structured output
    pub parent_exclgroup_som_path: Option<SomPath>,
    /// Current SOM path - tracks the path as we descend into the tree
    /// Used for path-based lookups in computed_values
    pub current_path: String,
    /// Hints inherited from parent nodes that should be applied to all descendants
    /// Per XFA spec, certain attributes like `relevant` are inherited by descendants
    pub inherited_hints: Vec<Hint>,
    /// The locale prevailing at the node being flattened (XFA 3.3
    /// "Determining the Prevailing Locale"): the nearest `locale` attribute
    /// on it or an enclosing container, else the document's. It selects
    /// the hyphenation dictionary.
    pub language: String,
    /// Resolved table column widths for descendants within a table layout.
    ///
    /// Propagated through intermediate containers so row subforms do not need to
    /// be direct table children.
    pub table_column_widths: Option<Vec<Num>>,
    /// When flattening one page's master content: what that page's own scripts
    /// made of it. Consulted for presence ahead of the node's own attribute, so
    /// a block that is visible on page 1 and hidden afterwards can be both.
    pub page_overrides: Option<&'a PageOverrides>,
    /// Where to record the [`Extent`] of every laid-out object, for the
    /// page breaking that follows (XFA 3.3 §7 "Break Conditions", §8
    /// "Content Splitting"). `Some` only for the content column
    /// `from_xfa_paged_with` is about to break into pages. A shared
    /// `RefCell` rather than a return value because extents are found at
    /// every depth of the recursion while `flatten_nodes` returns a height;
    /// it is written only through [`ExtentRecorder`]'s three methods.
    pub extents: Option<&'a RefCell<ExtentRecorder>>,
}

/// Records the [`Extent`] of every laid-out object while the flattener
/// recurses: a frame per open container, closed with the container's own
/// extent once its children are known.
#[derive(Debug, Default)]
pub struct ExtentRecorder {
    frames: Vec<Vec<Extent>>,
}

impl ExtentRecorder {
    pub fn new() -> Self {
        ExtentRecorder {
            frames: vec![Vec::new()],
        }
    }

    /// A container starts: its children are recorded until `end`.
    pub fn begin(&mut self) {
        self.frames.push(Vec::new());
    }

    /// The container that `begin` opened ends, as `extent`, with the
    /// extents recorded since as its children.
    pub fn end(&mut self, mut extent: Extent) {
        extent.children = self.frames.pop().expect("`begin` before `end`");
        self.frames
            .last_mut()
            .expect("the root frame is never popped")
            .push(extent);
    }

    pub fn leaf(&mut self, extent: Extent) {
        self.frames
            .last_mut()
            .expect("the root frame is never popped")
            .push(extent);
    }

    /// The extents of the column's top-level objects, in document order.
    pub fn into_extents(mut self) -> Vec<Extent> {
        assert_eq!(self.frames.len(), 1, "every `begin` has its `end`");
        self.frames.pop().unwrap_or_default()
    }
}

impl<'a> FlattenContext<'a> {
    /// Create a new flatten context with the given embed resolution data
    pub fn new(
        computed_values: &'a HashMap<SomPath, String>,
        id_to_field: &'a HashMap<String, String>,
    ) -> Self {
        FlattenContext {
            computed_values,
            id_to_field,
            inherited_presence: None,
            parent_exclgroup_value: None,
            parent_exclgroup_som_path: None,
            current_path: String::new(),
            inherited_hints: Vec::new(),
            language: String::new(),
            table_column_widths: None,
            page_overrides: None,
            extents: None,
        }
    }

    /// Create a new flatten context with an initial SOM path
    /// Used when flattening starts from a subform that isn't the root
    pub fn new_with_path(
        computed_values: &'a HashMap<SomPath, String>,
        id_to_field: &'a HashMap<String, String>,
        initial_path: String,
    ) -> Self {
        FlattenContext {
            computed_values,
            id_to_field,
            inherited_presence: None,
            parent_exclgroup_value: None,
            parent_exclgroup_som_path: None,
            current_path: initial_path,
            inherited_hints: Vec::new(),
            language: String::new(),
            table_column_widths: None,
            page_overrides: None,
            extents: None,
        }
    }

    /// Create an empty context (no embed resolution)
    pub fn empty() -> FlattenContext<'static> {
        static EMPTY_SOM: std::sync::LazyLock<HashMap<SomPath, String>> =
            std::sync::LazyLock::new(HashMap::new);
        static EMPTY_STR: std::sync::LazyLock<HashMap<String, String>> =
            std::sync::LazyLock::new(HashMap::new);
        FlattenContext {
            computed_values: &EMPTY_SOM,
            id_to_field: &EMPTY_STR,
            inherited_presence: None,
            parent_exclgroup_value: None,
            parent_exclgroup_som_path: None,
            current_path: String::new(),
            inherited_hints: Vec::new(),
            language: String::new(),
            table_column_widths: None,
            page_overrides: None,
            extents: None,
        }
    }

    /// Internal helper to clone context with modifications
    fn derive(&self) -> Self {
        FlattenContext {
            computed_values: self.computed_values,
            id_to_field: self.id_to_field,
            inherited_presence: self.inherited_presence,
            parent_exclgroup_value: self.parent_exclgroup_value.clone(),
            parent_exclgroup_som_path: self.parent_exclgroup_som_path.clone(),
            current_path: self.current_path.clone(),
            inherited_hints: self.inherited_hints.clone(),
            language: self.language.clone(),
            table_column_widths: self.table_column_widths.clone(),
            page_overrides: self.page_overrides,
            extents: self.extents,
        }
    }

    /// The context for the children of a container with its own `locale`
    /// attribute.
    pub fn with_locale(mut self, locale: &str) -> FlattenContext<'a> {
        self.language = locale.to_string();
        self
    }

    /// Create a child context with resolved table column widths.
    pub fn with_table_column_widths(&self, widths: Option<Vec<Num>>) -> FlattenContext<'a> {
        let mut ctx = self.derive();
        ctx.table_column_widths = widths;
        ctx
    }

    /// Create a child context with inherited presence
    /// Used when recursing into subforms that may have presence set
    pub fn with_inherited_presence(&self, presence: Presence) -> FlattenContext<'a> {
        let mut ctx = self.derive();
        ctx.inherited_presence = Some(presence);
        ctx
    }

    /// Create a child context for fields inside an exclGroup
    /// Used when recursing into exclGroup children
    pub fn with_exclgroup_value(&self, value: String) -> FlattenContext<'a> {
        let mut ctx = self.derive();
        ctx.parent_exclgroup_value = Some(value);
        ctx
    }

    /// Create a child context with the parent exclGroup's SOM path
    /// Used when recursing into exclGroup children to track the group's path
    pub fn with_exclgroup_som_path(&self, som_path: SomPath) -> FlattenContext<'a> {
        let mut ctx = self.derive();
        ctx.parent_exclgroup_som_path = Some(som_path);
        ctx
    }

    /// Create a child context with an additional inherited hint
    /// Used when recursing into nodes that have inheritable properties like relevant="-print"
    pub fn with_inherited_hint(&self, hint: Hint) -> FlattenContext<'a> {
        let mut ctx = self.derive();
        if !ctx.inherited_hints.contains(&hint) {
            ctx.inherited_hints.push(hint);
        }
        ctx
    }

    /// Check if a specific hint is inherited from an ancestor
    pub fn has_inherited_hint(&self, hint: &Hint) -> bool {
        self.inherited_hints.contains(hint)
    }

    /// Create a child context with extended path for a named node
    /// Used when recursing into named containers to track the full SOM path
    pub fn with_path_segment(&self, name: &str, index: usize) -> FlattenContext<'a> {
        let mut ctx = self.derive();
        ctx.current_path = self.get_full_path(name, index);
        ctx
    }

    /// The full SOM path of the named node at `index` among its same-named
    /// siblings at the current level.
    pub fn get_full_path(&self, name: &str, index: usize) -> String {
        child_som_path(&self.current_path, name, index)
    }

    /// Get the effective presence for a node, considering:
    /// 1. Inherited presence from parent (takes precedence if hidden/inactive)
    /// 2. Presence stored directly on the XfaNode (set by scripts or from attributes)
    pub fn get_effective_presence(&self, node: &XfaNode) -> Presence {
        // If parent is hidden/inactive, children inherit that
        if let Some(inherited) = self.inherited_presence
            && inherited.should_skip_layout()
        {
            return inherited;
        }
        let own = self.own_presence(node);
        // The contents of an `invisible` container inherit its restriction
        // (XFA 3.3 "The presence property": they take their space but are
        // not drawn), unless their own presence takes them out of layout
        // altogether. ACYI_001_EN keeps its hidden option group `invisible`;
        // without this its radio buttons were drawn as a stray box.
        if self.inherited_presence == Some(Presence::Invisible) && !own.should_skip_layout() {
            return Presence::Invisible;
        }
        own
    }

    /// The node's own presence, before anything inherited: what this page's
    /// master-page scripts set, else what the node says.
    fn own_presence(&self, node: &XfaNode) -> Presence {
        // On a master page, this page's own scripts have the last word: the
        // same node is visible on page 1 and hidden on page 2.
        if let Some(overrides) = self.page_overrides
            && let Some(name) = node.name.as_deref()
        {
            let full = if self.current_path.is_empty() {
                name.to_string()
            } else {
                format!("{}.{}", self.current_path, name)
            };
            if let Some(presence) = overrides.presence.get(&full) {
                return *presence;
            }
        }

        // Read presence directly from the XFA node (scripts modify this directly)
        node.get_presence()
    }

    /// Extract text content from node children, resolving any xfa:embed references
    pub fn extract_text(&self, children: &[XfaNode]) -> Option<String> {
        Flattened::extract_text_content_with_embed(children, self.computed_values, self.id_to_field)
    }
}

impl std::str::FromStr for Layout {
    type Err = std::convert::Infallible;

    /// Parse layout attribute string to Layout enum
    /// Per XFA spec: if subform has no layout attribute, it defaults to "position"
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "tb" => Layout::TopToBottom,
            "lr-tb" => Layout::LeftToRightTopToBottom,
            "rl-tb" => Layout::RightToLeftTopToBottom,
            "lr" => Layout::LeftToRight,
            "row" => Layout::Row,
            "rl-row" => Layout::RightToLeftRow,
            "position" => Layout::Position,
            "table" => Layout::Table,
            // Per XFA spec section 8 (page 280): "If a subform element does not have
            // a layout attribute it defaults to positioned layout."
            _ => Layout::Position,
        })
    }
}

impl Layout {
    /// Returns true if this layout mode is a flowing layout (ignores x/y coordinates)
    /// Per XFA spec: "In flowing layout the contained object's x and y properties,
    /// as well as its anchor point, are ignored."
    pub fn is_flowing(&self) -> bool {
        !matches!(self, Layout::Position)
    }
}

// ============================================================================
// Content Area Bounds for Master Page Region Classification
// ============================================================================

/// Content area bounds for master page region classification.
/// Used to determine whether page background elements are in header, footer, or background regions.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ContentAreaBounds {
    x: Num,
    y: Num,
    w: Num,
    h: Num,
}

impl ContentAreaBounds {
    /// Create from an XfaNode (expected to be a ContentArea)
    fn from_node(node: &XfaNode, page_width: Num, page_height: Num) -> Self {
        ContentAreaBounds {
            x: node.x.unwrap_or(Decimal::ZERO),
            y: node.y.unwrap_or(Decimal::ZERO),
            w: node.w.unwrap_or(page_width),
            h: node.h.unwrap_or(page_height),
        }
    }

    /// Calculate the top edge (y coordinate)
    fn top(&self) -> Num {
        self.y
    }

    /// Calculate the bottom edge (y + height)
    fn bottom(&self) -> Num {
        self.y + self.h
    }
}

/// One `pageArea` found by [`Flattened::find_all_page_areas`], with the
/// nearest enclosing `pageSet` (`None` for a pageArea outside any pageSet,
/// which the template grammar does not allow but a fragment may have).
#[derive(Clone)]
struct PageAreaRef<'a> {
    page_area: &'a XfaNode,
    page_set: Option<&'a XfaNode>,
    /// The pageArea's SOM path (`form1.MPs.MP`), under which its content
    /// is addressed by scripts and looked up in the values.
    path: String,
}

/// One `contentArea` of a page, in page coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ContentAreaGeometry {
    name: Option<String>,
    bounds: Bounds,
}

/// The physical layout of one `pageArea`: its page size and content areas,
/// parallel to [`PageSets::areas`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct PageGeometry {
    width: Num,
    height: Num,
    /// SOM path of the pageArea; empty for the default page of a document
    /// without any.
    path: String,
    name: String,
    content_areas: Vec<ContentAreaGeometry>,
}

/// How a content area came to be opened, which decides whether a
/// `startNew` break at its very start is already satisfied by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opening {
    /// The document's first content area.
    Start,
    /// The previous one overflowed.
    Overflow,
    /// A forced break.
    Break,
}

/// One content area the column is poured into: which page and which of its
/// content areas, and the span of column Y it holds.
#[derive(Debug, Clone)]
struct Region {
    /// Index into the paginator's pages.
    page: usize,
    /// Index into that page's content areas.
    area: usize,
    column_top: Num,
    /// Set when the region is closed.
    column_bottom: Option<Num>,
    /// The content area's bounds at the time of opening (a designated last
    /// page may replace the page afterwards; the final bounds are read from
    /// the finished pages).
    bounds: Bounds,
    opened_by: Opening,
}

impl Region {
    fn open(page: usize, area: usize, bounds: Bounds, opened_by: Opening) -> Self {
        Region {
            page,
            area,
            column_top: bounds.y,
            column_bottom: None,
            bounds,
            opened_by,
        }
    }
}

/// Where one region's content goes on its page: nodes whose top is at or
/// below `column_top` (and above the next placement's) move by `dx`/`dy`.
#[derive(Debug, Clone, Copy)]
struct RegionPlacement {
    column_top: Num,
    dx: Num,
    dy: Num,
}

/// A node's `<keep>` (XFA 3.3 §8 "Content Splitting", "Adhesion").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Keep {
    /// `intact`: `Some(true)` for `contentArea`/`pageArea`, `Some(false)`
    /// for an explicit `none`, `None` when absent (the default depends on
    /// the object).
    intact: Option<bool>,
    next: bool,
    previous: bool,
}

#[allow(dead_code)]
impl Flattened {
    /// Create a new Flattened instance with the given page and children.
    pub fn new(page: Page, children: Vec<FlattenedKind>) -> Self {
        Flattened {
            page,
            children,
            language: String::new(),
            page_areas: Vec::new(),
            fonts: Arc::new(DocumentFonts::default()),
        }
    }

    /// Create a Flattened from a flat list of nodes (wraps each in FlattenedKind::Node)
    pub fn from_nodes(page: Page, nodes: Vec<FlattenedNode>) -> Self {
        let children = nodes.into_iter().map(FlattenedKind::Node).collect();
        Flattened {
            page,
            children,
            language: String::new(),
            page_areas: Vec::new(),
            fonts: Arc::new(DocumentFonts::default()),
        }
    }

    /// The SOM paths of the template's pageAreas no page of this layout is
    /// an instance of: their content is on no page.
    pub fn unused_page_areas(&self) -> Vec<&str> {
        self.page_areas
            .iter()
            .map(String::as_str)
            .filter(|p| !self.page.pages.iter().any(|slot| slot.page_area == *p))
            .collect()
    }

    /// The language part of the document's locale (`de` for `de_CH`).
    pub fn language_code(&self) -> String {
        super::xfa::language_of_locale(Some(&self.language))
    }

    /// Iterate over all leaf nodes recursively
    pub fn iter_nodes(&self) -> FlattenedNodeIter<'_> {
        FlattenedNodeIter::new(&self.children)
    }

    /// Count all leaf nodes
    pub fn node_count(&self) -> usize {
        self.children.iter().map(|c| c.node_count()).sum()
    }

    /// Collect all leaf nodes into a flat Vec (for compatibility with old code)
    pub fn collect_nodes(&self) -> Vec<&FlattenedNode> {
        self.iter_nodes().collect()
    }

    /// Which page (1-based) a column-space Y lands on, and that page's top
    /// edge — mirrors `u2s-render-xfa`'s `bands()`, which this crate cannot
    /// depend on (the dependency runs the other way), so the two are kept in
    /// step by a unit test on both sides rather than shared code.
    ///
    /// A degenerate page height puts everything on page 1, matching `bands()`.
    /// A Y past the last page clamps to it, rather than reporting a page that
    /// does not exist — `locate` is what refuses an out-of-range node instead.
    pub fn page_of_y(&self, y: Num) -> (u32, Num) {
        if self.page.column_height() <= Decimal::ZERO {
            return (1, Decimal::ZERO);
        }
        let k = self.page.index_of_y(y);
        (k as u32 + 1, self.page.pages[k].top)
    }

    /// A node's page and page-local bounds, or `None` if the layout placed it
    /// outside every page -- negative Y, or at/past the bottom of the last
    /// page. Unlike [`Self::page_of_y`], this never clamps: a caller asking
    /// where to crop a region needs to know when a node has no real page,
    /// not a plausible-looking wrong one.
    pub fn locate(&self, node: &FlattenedNode) -> Option<(u32, Bounds)> {
        if node.y < Decimal::ZERO {
            return None;
        }
        let column = self.page.column_height();
        if column <= Decimal::ZERO {
            // Degenerate height: one page covering the whole column, same as
            // `page_of_y` and `bands()`.
            return Some((1, Bounds::new(node.x, node.y, node.width, node.height)));
        }
        if node.y >= column {
            return None;
        }
        let k = self.page.index_of_y(node.y);
        let top = self.page.pages[k].top;
        Some((
            k as u32 + 1,
            Bounds::new(node.x, node.y - top, node.width, node.height),
        ))
    }

    /// Merge `<items>` from the Form DOM packet into Template DOM fields.
    ///
    /// Per XFA 3.3 spec, the Form DOM is the result of merging data with the
    /// template. When a PDF is saved interactively, the Form DOM is serialized
    /// as the `<form>` packet inside `<xdp:xdp>`. This packet preserves runtime
    /// state, including `<items>` that were populated by scripts (e.g. `addItem()`).
    ///
    /// This method finds Form DOM fields that have non-empty `<items>` and copies
    /// them into the corresponding Template DOM fields (matched by SOM-like path).
    /// Only template fields whose `<items>` are empty (0 children) are updated.
    pub fn merge_form_items_into_template(xfa_nodes: &mut [XfaNode]) {
        // Step 1: Find the "form" element and collect items from its fields
        let mut form_items: HashMap<String, Vec<XfaNode>> = HashMap::new();
        for node in xfa_nodes.iter() {
            Self::find_form_element_and_collect_items(node, &mut form_items);
        }

        if form_items.is_empty() {
            return;
        }

        // Step 2: Walk the template and inject items into fields with empty items
        for node in xfa_nodes.iter_mut() {
            Self::inject_items_into_template(node, &form_items, &mut String::new());
        }
    }

    /// Merge presence values from the Form DOM packet into Template DOM nodes.
    ///
    /// Per XFA 3.3, the Form DOM is the result of merging data with the template.
    /// When a PDF is saved interactively, the Form DOM preserves runtime state
    /// including presence changes made by scripts (e.g., hiding a section based
    /// on a dropdown selection). This method copies those presence values into
    /// the corresponding Template DOM nodes so they are respected during layout.
    ///
    /// Only template nodes whose presence differs from the Form DOM are updated.
    /// Matching is done by SOM-like path (e.g. "Page.Section.Company").
    ///
    /// Paths that were already set by script execution (via `script_presence_changes`)
    /// are skipped, since scripts produce the authoritative runtime state and the
    /// Form DOM may contain stale saved state.
    pub fn merge_form_presence_into_template(
        xfa_nodes: &mut [XfaNode],
        script_presence_changes: &[crate::xfa::script_executor::PresenceChange],
    ) {
        // Step 1: Find the "form" element and collect presence values
        let mut form_presence: HashMap<String, Presence> = HashMap::new();
        for node in xfa_nodes.iter() {
            Self::find_form_element_and_collect_presence(node, &mut form_presence);
        }

        if form_presence.is_empty() {
            return;
        }

        // A presence the scripts set wins over the saved one: the saved
        // state is where the scripts started, not where they ended.
        let script_touched: HashSet<&str> = script_presence_changes
            .iter()
            .map(|change| change.path.as_str())
            .collect();
        form_presence.retain(|path, _| !script_touched.contains(path.as_str()));

        // Step 2: Walk the template and apply presence from form DOM
        for node in xfa_nodes.iter_mut() {
            Self::apply_presence_to_template(node, &form_presence, &mut String::new());
        }
    }

    /// Recursively find the `<form>` element and collect presence values.
    fn find_form_element_and_collect_presence(
        node: &XfaNode,
        presence_map: &mut HashMap<String, Presence>,
    ) {
        if let XfaNodeKind::Element { tag_name, .. } = &node.kind
            && tag_name == "form"
        {
            for child in &node.children {
                Self::collect_form_node_presence(child, &mut String::new(), presence_map);
            }
            return;
        }
        for child in &node.children {
            Self::find_form_element_and_collect_presence(child, presence_map);
        }
    }

    /// Walk inside the form packet collecting presence values for subforms and fields.
    fn collect_form_node_presence(
        node: &XfaNode,
        path: &mut String,
        presence_map: &mut HashMap<String, Presence>,
    ) {
        let segment = match &node.kind {
            XfaNodeKind::Subform => node.name.as_deref(),
            XfaNodeKind::Field => node.name.as_deref(),
            XfaNodeKind::ExclGroup => node.name.as_deref(),
            _ => None,
        };

        let prev_len = path.len();
        if let Some(name) = segment {
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(name);
        }

        // Only record non-visible presence (hidden, invisible, inactive)
        // which differs from the default.
        let presence = node.get_presence();
        if segment.is_some() && presence != Presence::Visible {
            presence_map.insert(path.clone(), presence);
        }

        for child in &node.children {
            Self::collect_form_node_presence(child, path, presence_map);
        }

        path.truncate(prev_len);
    }

    /// Walk the template and apply presence values from the form DOM.
    fn apply_presence_to_template(
        node: &mut XfaNode,
        form_presence: &HashMap<String, Presence>,
        path: &mut String,
    ) {
        let segment = match &node.kind {
            XfaNodeKind::Subform => node.name.as_deref(),
            XfaNodeKind::Field => node.name.as_deref(),
            XfaNodeKind::ExclGroup => node.name.as_deref(),
            _ => None,
        };

        let prev_len = path.len();
        if let Some(name) = segment {
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(name);
        }

        // Apply presence from form DOM if it differs
        if segment.is_some() {
            if let Some(&form_pres) = form_presence.get(path.as_str()) {
                if node.get_presence() != form_pres {
                    node.set_presence(form_pres);
                }
            }
        }

        for child in &mut node.children {
            Self::apply_presence_to_template(child, form_presence, path);
        }

        path.truncate(prev_len);
    }

    /// Merge `access` attributes from the Form DOM packet into the Template DOM.
    ///
    /// Some forms leave a field's `access` unset in the template and only set it
    /// (e.g. to `nonInteractive`) at runtime — either via an `initialize` script
    /// (`this.access = "nonInteractive"`) or directly in the `<form>` packet. The
    /// `<form>` packet captures that runtime state, so copying its `access` values
    /// onto the matching template fields lets `extract_field_access` see them and
    /// correctly treat such fields as non-interactive captions rather than inputs.
    pub fn merge_form_access_into_template(xfa_nodes: &mut [XfaNode]) {
        // Step 1: Find the "form" element and collect access values.
        let mut form_access: HashMap<String, String> = HashMap::new();
        for node in xfa_nodes.iter() {
            Self::find_form_element_and_collect_access(node, &mut form_access);
        }

        if form_access.is_empty() {
            return;
        }

        // Step 2: Walk the template and apply access from form DOM.
        for node in xfa_nodes.iter_mut() {
            Self::apply_access_to_template(node, &form_access, &mut String::new());
        }
    }

    /// Recursively find the `<form>` element and collect `access` attribute values.
    fn find_form_element_and_collect_access(
        node: &XfaNode,
        access_map: &mut HashMap<String, String>,
    ) {
        if let XfaNodeKind::Element { tag_name, .. } = &node.kind
            && tag_name == "form"
        {
            for child in &node.children {
                Self::collect_form_node_access(child, &mut String::new(), access_map);
            }
            return;
        }
        for child in &node.children {
            Self::find_form_element_and_collect_access(child, access_map);
        }
    }

    /// Walk inside the form packet collecting `access` values for fields.
    fn collect_form_node_access(
        node: &XfaNode,
        path: &mut String,
        access_map: &mut HashMap<String, String>,
    ) {
        let segment = match &node.kind {
            XfaNodeKind::Subform | XfaNodeKind::Field | XfaNodeKind::ExclGroup => {
                node.name.as_deref()
            }
            _ => None,
        };

        let prev_len = path.len();
        if let Some(name) = segment {
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(name);
        }

        // Only record `nonInteractive` ("renders to paper") — the static-caption
        // case. `readOnly`/`protected` appear pervasively in form packets as
        // transient runtime states and must NOT disable a field's interactivity,
        // which the template-level access already governs.
        if segment.is_some() {
            if let Some(access) = node.attributes.get("access") {
                if access == "nonInteractive" {
                    access_map.insert(path.clone(), access.clone());
                }
            }
        }

        for child in &node.children {
            Self::collect_form_node_access(child, path, access_map);
        }

        path.truncate(prev_len);
    }

    /// Walk the template and apply `access` values from the form DOM.
    fn apply_access_to_template(
        node: &mut XfaNode,
        form_access: &HashMap<String, String>,
        path: &mut String,
    ) {
        let segment = match &node.kind {
            XfaNodeKind::Subform | XfaNodeKind::Field | XfaNodeKind::ExclGroup => {
                node.name.as_deref()
            }
            _ => None,
        };

        let prev_len = path.len();
        if let Some(name) = segment {
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(name);
        }

        if segment.is_some() {
            if let Some(access) = form_access.get(path.as_str()) {
                if node.attributes.get("access") != Some(access) {
                    node.attributes.insert("access".to_string(), access.clone());
                }
            }
        }

        for child in &mut node.children {
            Self::apply_access_to_template(child, form_access, path);
        }

        path.truncate(prev_len);
    }

    /// Recursively find the `<form>` element and collect items from all its fields.
    /// Builds a map from SOM-like path (e.g. "UBSForms.Page.ClientType.CL_ClientType")
    /// to the list of `<items>` XfaNode children.
    fn find_form_element_and_collect_items(
        node: &XfaNode,
        items_map: &mut HashMap<String, Vec<XfaNode>>,
    ) {
        if let XfaNodeKind::Element { tag_name, .. } = &node.kind
            && tag_name == "form"
        {
            // Found the form packet — walk its children to collect items
            for child in &node.children {
                Self::collect_form_field_items(child, &mut String::new(), items_map);
            }
            return;
        }
        // Keep searching for the form element
        for child in &node.children {
            Self::find_form_element_and_collect_items(child, items_map);
        }
    }

    /// Walk inside the form packet collecting field items.
    /// Tracks the SOM path using subform/field names.
    fn collect_form_field_items(
        node: &XfaNode,
        path: &mut String,
        items_map: &mut HashMap<String, Vec<XfaNode>>,
    ) {
        let segment = match &node.kind {
            XfaNodeKind::Subform => node.name.as_deref(),
            XfaNodeKind::Field => node.name.as_deref(),
            _ => None,
        };

        let prev_len = path.len();
        if let Some(name) = segment {
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(name);
        }

        if matches!(node.kind, XfaNodeKind::Field) {
            // Collect all <items> children that have content
            let items_nodes: Vec<XfaNode> = node
                .children
                .iter()
                .filter(|c| {
                    matches!(&c.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "items")
                        && !c.children.is_empty()
                })
                .cloned()
                .collect();

            if !items_nodes.is_empty() {
                items_map.insert(path.clone(), items_nodes);
            }
        }

        // Recurse into children (subforms contain more fields)
        for child in &node.children {
            Self::collect_form_field_items(child, path, items_map);
        }

        path.truncate(prev_len);
    }

    /// Walk the template and inject form items into fields with empty items.
    fn inject_items_into_template(
        node: &mut XfaNode,
        form_items: &HashMap<String, Vec<XfaNode>>,
        path: &mut String,
    ) {
        // Only process template subtree
        let _is_template = matches!(node.kind, XfaNodeKind::Template);
        let segment = match &node.kind {
            XfaNodeKind::Subform => node.name.as_deref(),
            XfaNodeKind::Field => node.name.as_deref(),
            _ => None,
        };

        let prev_len = path.len();
        if let Some(name) = segment {
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(name);
        }

        if matches!(node.kind, XfaNodeKind::Field)
            && let Some(form_items_nodes) = form_items.get(path.as_str())
        {
            // Check if this field's existing <items> are all empty
            let all_items_empty = node.children.iter().all(|c| {
                if let XfaNodeKind::Element { tag_name, .. } = &c.kind
                    && tag_name == "items"
                {
                    return c.children.is_empty();
                }
                true // non-items children don't count
            });

            if all_items_empty {
                // Remove existing empty <items> elements
                node.children.retain(|c| {
                    if let XfaNodeKind::Element { tag_name, .. } = &c.kind {
                        tag_name != "items"
                    } else {
                        true
                    }
                });

                // Insert items from form DOM
                for items_node in form_items_nodes {
                    node.children.push(items_node.clone());
                }
            }
        }

        // Recurse
        for child in &mut node.children {
            Self::inject_items_into_template(child, form_items, path);
        }

        path.truncate(prev_len);
    }

    /// Create a flattened representation from XFA nodes with computed absolute positions.
    ///
    /// This implements the XFA Layout process per the spec (section 3, "Template DOM, Form DOM, and Layout DOM"):
    ///
    /// 1. **Template DOM** supplies:
    ///    - Page structure: pageSet → pageArea → contentArea
    ///    - Page background: direct children of pageArea (excluding contentArea/medium)
    ///
    /// 2. **Form DOM** (derived from Template DOM):
    ///    - A duplicate of the subtree under the root subform (NOT including pageSet)
    ///    - Contains the actual form content (fields, draws, subforms with data)
    ///
    /// 3. **Layout DOM** (what we're building here):
    ///    - Page structure from Template DOM
    ///    - Page background from Template DOM (rendered per page instance)
    ///    - Form content placed INTO the contentArea
    ///
    /// Structure example:
    /// ```text
    /// template
    ///   subform 'UBSForms' (root container)
    ///     pageSet 'MPs'           <-- stays in Template DOM
    ///       pageArea 'Page1'
    ///         draw (header)       <-- page background, rendered at page origin
    ///         contentArea 'Body'  <-- defines where form content goes
    ///     subform 'Page'          <-- Form DOM root, content goes INTO contentArea
    /// ```
    ///
    /// Create a flattened representation from XFA nodes.
    ///
    /// This is a **pure transformation** - it takes immutable XFA nodes and
    /// pre-computed values, and produces a static layout representation.
    /// No scripts are executed, no side effects occur.
    ///
    /// # Arguments
    /// * `xfa_nodes` - The XFA node tree (immutable, presence already set)
    /// * `computed_values` - Pre-computed field values from script execution
    ///
    /// # Architecture
    /// Script execution should happen separately via `ScriptExecutor` before
    /// calling this method. This keeps Flattened as a pure intermediate
    /// representation suitable for rendering.
    pub fn from_xfa(
        xfa_nodes: &[XfaNode],
        computed_values: &HashMap<SomPath, String>,
    ) -> Result<Self, String> {
        Self::from_xfa_fonts(
            xfa_nodes,
            computed_values,
            Arc::new(DocumentFonts::from_nodes(xfa_nodes)),
        )
    }

    /// As [`Flattened::from_xfa`], with the document's fonts (its embedded
    /// faces and `equate`s) rather than only what its packets declare.
    pub fn from_xfa_fonts(
        xfa_nodes: &[XfaNode],
        computed_values: &HashMap<SomPath, String>,
        fonts: Arc<DocumentFonts>,
    ) -> Result<Self, String> {
        let id_to_field = Self::build_id_to_field_map(xfa_nodes);
        let language = super::xfa::document_locale(xfa_nodes);
        Self::from_xfa_paged_with(
            xfa_nodes,
            computed_values,
            &id_to_field,
            &language,
            fonts,
            &mut |_, _, _| PageOverrides::default(),
        )
    }

    /// As [`Flattened::from_xfa`], but evaluating page-dependent master-page
    /// scripts once per page.
    ///
    /// `master` is called with the chosen pageArea's name, the 0-based page
    /// index and the page count, and returns what that page's own scripts made
    /// of its master page. It is called only after the body has been laid out,
    /// because the page count is a result of that.
    pub fn from_xfa_paged(
        xfa_nodes: &[XfaNode],
        computed_values: &HashMap<SomPath, String>,
        master: &mut dyn FnMut(&str, usize, usize) -> PageOverrides,
    ) -> Result<Self, String> {
        Self::from_xfa_paged_fonts(
            xfa_nodes,
            computed_values,
            Arc::new(DocumentFonts::from_nodes(xfa_nodes)),
            master,
        )
    }

    /// As [`Flattened::from_xfa_paged`], with the document's fonts.
    pub fn from_xfa_paged_fonts(
        xfa_nodes: &[XfaNode],
        computed_values: &HashMap<SomPath, String>,
        fonts: Arc<DocumentFonts>,
        master: &mut dyn FnMut(&str, usize, usize) -> PageOverrides,
    ) -> Result<Self, String> {
        let id_to_field = Self::build_id_to_field_map(xfa_nodes);
        let language = super::xfa::document_locale(xfa_nodes);
        Self::from_xfa_paged_with(xfa_nodes, computed_values, &id_to_field, &language, fonts, master)
    }

    /// Alias for `from_xfa` - provided for backwards compatibility.
    ///
    /// Use `from_xfa` directly for new code.
    #[inline]
    pub fn reflatten(
        xfa_nodes: &[XfaNode],
        computed_values: &HashMap<SomPath, String>,
    ) -> Result<Self, String> {
        Self::from_xfa(xfa_nodes, computed_values)
    }

    /// Create a flattened representation without computed values.
    ///
    /// Convenience method for simple cases where no script execution is needed.
    pub fn from_xfa_simple(xfa_nodes: &[XfaNode]) -> Result<Self, String> {
        Self::from_xfa(xfa_nodes, &HashMap::new())
    }

    /// Build a map from element ID to full SOM path (for resolving xfa:embed references)
    ///
    /// Per XFA spec, `xfa:embed` with `embedType="uri"` uses unique element IDs
    /// (e.g. `#floatingField006334`) to reference specific field instances.
    /// We map these IDs to full SOM paths so that `resolve_embed_reference` can
    /// look up the correct per-field computed value even when multiple fields
    /// share the same short name.
    fn build_id_to_field_map(xfa_nodes: &[XfaNode]) -> HashMap<String, String> {
        let mut id_map = HashMap::new();
        Self::collect_ids_recursive(xfa_nodes, &mut id_map, None);
        id_map
    }

    /// Recursively collect ID attributes and map them to full SOM paths,
    /// built by the same rule as everywhere else (`XfaNodeKind::extends_som_path`).
    fn collect_ids_recursive(
        nodes: &[XfaNode],
        id_map: &mut HashMap<String, String>,
        parent_path: Option<&str>,
    ) {
        for (node, index) in nodes.iter().zip(sibling_indices(nodes)) {
            let name = node.name.as_deref().unwrap_or("");

            // Build the full SOM path for this node. Instances of a repeated
            // section carry their own ids (`instances::localize_ids`), so an
            // id maps to its own instance's path.
            let full_path = if !name.is_empty() {
                child_som_path(parent_path.unwrap_or(""), name, index)
            } else {
                parent_path.unwrap_or("").to_string()
            };

            // Store ID -> full SOM path
            if let Some(id) = node.attributes.get("id") {
                if !full_path.is_empty() {
                    id_map.insert(id.clone(), full_path.clone());
                }
            }

            let next_parent = if !name.is_empty() && node.kind.extends_som_path() {
                Some(full_path.as_str())
            } else {
                parent_path
            };

            Self::collect_ids_recursive(&node.children, id_map, next_parent);
        }
    }

    /// Collect all content areas from a pageArea node
    fn collect_content_areas(
        page_area: &XfaNode,
        page_width: Num,
        page_height: Num,
    ) -> Vec<ContentAreaBounds> {
        let mut content_areas = Vec::new();

        for child in &page_area.children {
            if matches!(child.kind, XfaNodeKind::ContentArea) {
                content_areas.push(ContentAreaBounds::from_node(child, page_width, page_height));
            } else if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                && tag_name == "contentArea"
            {
                content_areas.push(ContentAreaBounds::from_node(child, page_width, page_height));
            }
        }

        content_areas
    }

    /// Classify an element's master page region based on its position relative to content areas.
    ///
    /// Uses >50% overlap rule:
    /// - Header: >50% of element area is above all content areas
    /// - Footer: >50% of element area is below all content areas  
    /// - Background: otherwise (alongside or overlapping content areas)
    fn classify_master_page_region(
        elem_y: Num,
        elem_h: Num,
        content_areas: &[ContentAreaBounds],
    ) -> MasterPageRegion {
        if content_areas.is_empty() {
            // No content areas defined - treat as background
            return MasterPageRegion::Background;
        }

        let elem_center_y = elem_y + elem_h / num(2.0);

        // Find the topmost and bottommost content area boundaries
        let content_top = content_areas
            .iter()
            .map(|ca| ca.top())
            .min()
            .unwrap_or(Decimal::ZERO);
        let content_bottom = content_areas
            .iter()
            .map(|ca| ca.bottom())
            .max()
            .unwrap_or(Decimal::ZERO);

        // >50% above all content areas = header
        // This means the center of the element is above the top of content areas
        if elem_center_y < content_top {
            return MasterPageRegion::Header;
        }

        // >50% below all content areas = footer
        // This means the center of the element is below the bottom of content areas
        if elem_center_y > content_bottom {
            return MasterPageRegion::Footer;
        }

        // Otherwise, it's alongside or overlapping content areas
        MasterPageRegion::Background
    }

    /// Recursively add MasterPage hint to a flattened element and all its children,
    /// classifying each node based on its own rendered position.
    fn add_master_page_hint_by_position(
        item: &mut FlattenedKind,
        content_areas: &[ContentAreaBounds],
    ) {
        match item {
            FlattenedKind::Node(node) => {
                // Classify based on the node's actual rendered y-position
                let region = Self::classify_master_page_region(node.y, node.height, content_areas);
                node.add_hint(Hint::MasterPage { region });
            }
            FlattenedKind::Group {
                children, hints, ..
            } => {
                // Process all children first
                for child in children.iter_mut() {
                    Self::add_master_page_hint_by_position(child, content_areas);
                }
                // For the group, use the region of the first child or Background if empty
                let group_region = children
                    .first()
                    .and_then(|c| {
                        c.hints().iter().find_map(|h| {
                            if let Hint::MasterPage { region } = h {
                                Some(*region)
                            } else {
                                None
                            }
                        })
                    })
                    .unwrap_or(MasterPageRegion::Background);
                hints.push(Hint::MasterPage {
                    region: group_region,
                });
            }
        }
    }

    /// Lay the document out as a stack of whole pages.
    ///
    /// The result is a column exactly `page_count * page.height` tall: page `k`
    /// owns `[k*H, (k+1)*H)`, the body content of page `k` starts at
    /// `k*H + content_area_y`, and the master page (the `pageArea` chosen for
    /// that page) is drawn at `k*H` on *every* page. Consumers slice pages out
    /// of it by that arithmetic alone -- see `Page::column_height`,
    /// `slice_into_pages` and `u2s-render-xfa`'s `bands`.
    ///
    /// `master` is called once per page with the chosen pageArea's name, the
    /// 0-based page index and the page count, and returns the values and
    /// presences that page-dependent master-page scripts produced for that page
    /// (`xfa.layout.page()`, `pageArea.index`). It is called only after the
    /// content pass, because the page count is not known before it.
    fn from_xfa_paged_with(
        xfa_nodes: &[XfaNode],
        computed_values: &HashMap<SomPath, String>,
        id_to_field: &HashMap<String, String>,
        language: &str,
        fonts: Arc<DocumentFonts>,
        master: &mut dyn FnMut(&str, usize, usize) -> PageOverrides,
    ) -> Result<Self, String> {
        // Text is measured in the document's fonts for the whole layout.
        let _document_fonts = fonts.enter();

        // ============================================================
        // The pages the template offers (XFA 3.3 §7 "Page Selection")
        // ============================================================
        let page_area_refs = Self::find_all_page_areas(xfa_nodes);
        let (page_sets, geometries) = Self::page_set_specs(xfa_nodes, &page_area_refs);

        // The Form DOM root: its children flow in its own layout, and its
        // pageSets are page structure, not content.
        let root = Self::find_root_subform(xfa_nodes);
        let recorder = RefCell::new(ExtentRecorder::new());
        let root_ctx = root.as_ref().map(|(_, root_path)| {
            let mut ctx =
                FlattenContext::new_with_path(computed_values, id_to_field, root_path.clone());
            ctx.language = language.to_string();
            ctx.extents = Some(&recorder);
            ctx
        });

        // Where the layout starts (XFA 3.3 §8 "Determining the start
        // point"): the root subform's `breakBefore` target, else its first
        // subform child's -- the first one in the layout, a hidden one
        // (ACYI_001_EN's cover, once the form proper is shown) takes no
        // part in it -- else the first page set in document order.
        let entry = root
            .as_ref()
            .zip(root_ctx.as_ref())
            .and_then(|((r, _), ctx)| {
                Self::break_requests(r).0.and_then(|b| b.target).or_else(|| {
                    r.children
                        .iter()
                        .find(|c| {
                            matches!(c.kind, XfaNodeKind::Subform)
                                && !ctx.get_effective_presence(c).should_skip_layout()
                        })
                        .and_then(|c| Self::break_requests(c).0.and_then(|b| b.target))
                })
            })
            .and_then(|t| {
                let resolved = page_sets.resolve_target(&t);
                if resolved.is_none() {
                    log::warn!(
                        "the start page target {t:?} names no pageArea or pageSet; the first page set is used"
                    );
                }
                resolved
            });

        let mut paginator =
            Paginator::new(&page_sets, entry).map_err(|e| Self::describe_pagination_error(&e))?;
        let mut regions: Vec<Region> = Vec::new();
        let first_request = entry.map(PageRequest::Target).unwrap_or(PageRequest::Next);
        let first_page = Self::open_page(&mut paginator, first_request, &geometries)?;
        let first_bounds = geometries[paginator.pages()[first_page].area].content_areas[0].bounds;
        regions.push(Region::open(first_page, 0, first_bounds, Opening::Start));
        let origin = first_bounds;

        // ============================================================
        // STEP 1: lay the Form DOM out as one column
        // ============================================================
        // The column's coordinates are the first page's: content starts at
        // the first content area's own x and y. Every later content area
        // is reached by moving its span of the column into place.
        //
        // This runs *before* the master pages, unlike the XFA spec's drawing
        // order, because the number of pages is a result of it and the master
        // pages have to be repeated once per page.
        let mut content_children: Vec<FlattenedKind> = Vec::new();
        let column_position = Position::new(origin.x, origin.y, origin.width, origin.height);
        let consumed_height = match root.as_ref().zip(root_ctx.as_ref()) {
            Some(((root_subform, _), ctx)) => {
                let layout = root_subform
                    .layout
                    .as_ref()
                    .and_then(|l| l.parse().ok())
                    .unwrap_or(Layout::Position);
                Self::flatten_nodes(
                    &root_subform.children,
                    column_position,
                    layout,
                    &mut content_children,
                    ctx,
                )?
            }
            None => {
                // No root subform: every node is content (a template
                // fragment), positioned.
                let mut ctx = FlattenContext::new(computed_values, id_to_field);
                ctx.language = language.to_string();
                ctx.extents = Some(&recorder);
                Self::flatten_nodes(
                    xfa_nodes,
                    column_position,
                    Layout::Position,
                    &mut content_children,
                    &ctx,
                )?
            }
        };
        let extents = recorder.into_inner().into_extents();
        let content_end = origin.y + consumed_height;
        Self::apply_computed_values(&mut content_children, computed_values);

        // ============================================================
        // STEP 2: break the column into content areas and pages
        // ============================================================
        // Each content area takes as much of the column as fits, or up to
        // the next forced break (XFA 3.3 §7 "Break Conditions"), and where
        // the content does not divide there the split consensus of §8
        // "Content Splitting" moves the break up to where it does.
        let breaks = pagination::forced_breaks(&extents);
        let mut next_break = 0;
        loop {
            let region = regions.last().expect("a content area is open");
            let capacity_end = region.column_top + region.bounds.height;
            debug_assert!(
                breaks.get(next_break).is_none_or(|b| b.at >= region.column_top),
                "a forced break is never passed by: every break at or before a content area's \
                 capacity is taken before a geometric break is chosen"
            );
            let forced = breaks.get(next_break).filter(|b| b.at <= capacity_end);
            match forced {
                Some(b) if b.at == region.column_top => {
                    next_break += 1;
                    if Self::break_satisfied(&b.request, &paginator, &page_sets, &geometries, region)
                    {
                        continue;
                    }
                    // Nothing is on this content area yet, but the break
                    // asks for another one (`startNew`, or a page this one
                    // is not).
                    let (at, request) = (b.at, b.request.clone());
                    Self::close_region(&mut regions, at);
                    Self::open_after_break(
                        &request,
                        &mut regions,
                        &mut paginator,
                        &page_sets,
                        &geometries,
                        at,
                    )?;
                }
                Some(b) => {
                    let (at, request) = (b.at, b.request.clone());
                    next_break += 1;
                    Self::close_region(&mut regions, at);
                    Self::open_after_break(
                        &request,
                        &mut regions,
                        &mut paginator,
                        &page_sets,
                        &geometries,
                        at,
                    )?;
                }
                None => {
                    if capacity_end >= content_end {
                        let at = content_end.max(region.column_top);
                        Self::close_region(&mut regions, at);
                        break;
                    }
                    // A split that leaves the content area with less than a
                    // hundredth of a point of column holds nothing: a
                    // rounding residue, not content.
                    let empty = |at: Num| at - region.column_top < num(0.01);
                    let at = match pagination::split_point(&extents, region.column_top, capacity_end) {
                        Some(at) if !empty(at) => at,
                        _ => {
                            // The object fits nowhere, not even split: it goes
                            // on this page whole, past the content area's
                            // boundary, and the next content area starts after
                            // it.
                            let at = pagination::split_point_below(&extents, capacity_end);
                            log::warn!(
                                "nothing of the content from {} on fits a content area {} tall, even \
                                 split; the content area's boundary is overridden to {} (XFA 3.3 \
                                 §8 \"Overriding ContentArea Boundaries\")",
                                region.column_top,
                                region.bounds.height,
                                at
                            );
                            at
                        }
                    };
                    Self::close_region(&mut regions, at);
                    Self::open_overflow(
                        &mut regions,
                        &mut paginator,
                        &geometries,
                        at,
                        None,
                        Opening::Overflow,
                    )?;
                }
            }
        }
        Self::finish_page_set(&mut paginator, &regions, &geometries);
        let planned = paginator.into_pages();
        for r in &regions {
            log::debug!(
                "content area: page {} area {} column {}..{:?} ({:?})",
                r.page + 1,
                r.area,
                r.column_top,
                r.column_bottom,
                r.opened_by
            );
        }

        // ============================================================
        // STEP 3: stack the content areas into whole pages
        // ============================================================
        let mut slots: Vec<PageSlot> = Vec::with_capacity(planned.len());
        let mut top = Decimal::ZERO;
        for p in &planned {
            let g = &geometries[p.area];
            slots.push(PageSlot {
                top,
                width: g.width,
                height: g.height,
                page_area: g.path.clone(),
                page_area_name: g.name.clone(),
                content_areas: g.content_areas.iter().map(|c| c.bounds).collect(),
                blank: p.blank,
            });
            top += g.height;
        }
        let page = Page::stacked(slots);
        let placements: Vec<RegionPlacement> = regions
            .iter()
            .map(|r| {
                let slot = &page.pages[r.page];
                let bounds = slot.content_areas[r.area];
                RegionPlacement {
                    column_top: r.column_top,
                    dx: bounds.x - origin.x,
                    dy: slot.top + bounds.y - r.column_top,
                }
            })
            .collect();
        if regions
            .iter()
            .any(|r| page.pages[r.page].content_areas[r.area].width != origin.width)
        {
            log::warn!(
                "content areas of different widths share one flow; the content is laid out at \
                 the first one's width ({})",
                origin.width
            );
        }
        Self::stack_regions(&mut content_children, &placements);

        // ============================================================
        // STEP 4: the page background of every page (Template DOM pageAreas)
        // ============================================================
        // Per XFA spec (section 7, "Page Background"):
        // "A pageArea may contain content such as subforms. Such content, which is placed
        // directly in a pageArea element, represents page background."
        //
        // Page background elements are positioned relative to the page origin (0,0),
        // NOT the contentArea. They use positioned layout (absolute coordinates).
        //
        // Each page's own pageArea is emitted once for it -- a header is on
        // every page. Master content goes in front of the body in document
        // order so that it is drawn first and the body sits on top of it.
        let mut flattened_children: Vec<FlattenedKind> = Vec::new();
        let page_count = page.pages.len();
        for (k, planned_page) in planned.iter().enumerate() {
            let Some(page_area_ref) = page_area_refs.get(planned_page.area) else {
                // The default page of a document without a pageArea.
                continue;
            };
            let slot = &page.pages[k];
            let page_area = page_area_ref.page_area;
            let page_position = Position::new(Decimal::ZERO, Decimal::ZERO, slot.width, slot.height);

            // Collect content areas for region classification (header/footer detection)
            let content_areas = Self::collect_content_areas(page_area, slot.width, slot.height);

            // What this page's own scripts made of it: "Pagina 2/3", a
            // first-page-only block hidden from page 2 on.
            let overrides = master(&slot.page_area_name, k, page_count);
            let values = overrides.merged_with(computed_values);

            let mut page_ctx =
                FlattenContext::new_with_path(&values, id_to_field, page_area_ref.path.clone());
            page_ctx.language = language.to_string();
            page_ctx.page_overrides = Some(&overrides);

            let mut per_page: Vec<FlattenedKind> = Vec::new();
            for (child, child_index) in page_area
                .children
                .iter()
                .zip(sibling_indices(&page_area.children))
            {
                // Skip contentArea and medium - these define page structure, not content
                if matches!(child.kind, XfaNodeKind::ContentArea) {
                    continue;
                }
                if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                    && (tag_name == "contentArea" || tag_name == "medium")
                {
                    continue;
                }

                // Render page background element with positioned layout relative to page origin
                let start_idx = per_page.len();
                Self::flatten_single_node(
                    child,
                    child_index,
                    page_position,
                    Layout::Position,
                    &mut per_page,
                    &page_ctx,
                )?;

                // Add MasterPage hint to all newly created nodes/groups
                // Classify each node based on its actual rendered position, not the parent container's position
                for item in &mut per_page[start_idx..] {
                    Self::add_master_page_hint_by_position(item, &content_areas);
                }
            }

            Self::apply_computed_values(&mut per_page, &values);

            // Classification happened in page coordinates; move the page
            // into its slot only afterwards.
            if !slot.top.is_zero() {
                Self::shift_y(&mut per_page, slot.top);
            }
            flattened_children.append(&mut per_page);
        }

        flattened_children.extend(content_children);

        Ok(Flattened {
            page,
            children: flattened_children,
            language: language.to_string(),
            page_areas: geometries
                .iter()
                .map(|g| g.path.clone())
                .filter(|p| !p.is_empty())
                .collect(),
            fonts: Arc::clone(&fonts),
        })
    }

    /// The pagination's view of every `pageSet` and `pageArea` of the
    /// template, and the physical layout of each pageArea (parallel to
    /// `page_area_refs` and to the returned [`PageSets::areas`]). A
    /// template without a pageArea gets one default A4 page whose content
    /// area is the whole page, repeating as needed.
    fn page_set_specs(
        xfa_nodes: &[XfaNode],
        page_area_refs: &[PageAreaRef<'_>],
    ) -> (PageSets, Vec<PageGeometry>) {
        use pagination::{AreaSpec, BlankOrNotBlank, OddOrEven, PagePosition, Relation, SetSpec};

        if page_area_refs.is_empty() {
            let width = XfaNode::parse_dimension("210mm").unwrap_or_else(|_| num(595.27));
            let height = XfaNode::parse_dimension("297mm").unwrap_or_else(|_| num(841.89));
            let specs = PageSets {
                areas: vec![AreaSpec {
                    name: None,
                    position: PagePosition::Any,
                    odd_or_even: OddOrEven::Any,
                    blank: BlankOrNotBlank::Any,
                    min_occur: 0,
                    max_occur: None,
                    content_areas: 1,
                }],
                sets: vec![SetSpec {
                    name: None,
                    relation: Relation::OrderedOccurrence,
                    areas: vec![0],
                    children: Vec::new(),
                }],
            };
            let geometry = PageGeometry {
                width,
                height,
                path: String::new(),
                name: String::new(),
                content_areas: vec![ContentAreaGeometry {
                    name: None,
                    bounds: Bounds::new(Decimal::ZERO, Decimal::ZERO, width, height),
                }],
            };
            return (specs, vec![geometry]);
        }

        /// A pageArea qualifier as written, or its default when the value
        /// is not one the specification names (reported).
        fn qualifier<T>(node: &XfaNode, name: &str, parse: fn(Option<&str>) -> Result<T, String>) -> T {
            parse(node.attributes.get(name).map(String::as_str)).unwrap_or_else(|e| {
                log::warn!("{e} on pageArea {:?}; taken as the default", node.name);
                parse(None).expect("the default parses")
            })
        }

        let mut areas = Vec::with_capacity(page_area_refs.len());
        let mut geometries = Vec::with_capacity(page_area_refs.len());
        for r in page_area_refs {
            let pa = r.page_area;
            let (min_occur, max_occur) = Self::page_occurrence(pa);
            let medium = Self::medium_size(pa);
            let width = pa
                .w
                .or(medium.map(|(w, _)| w))
                .unwrap_or_else(|| XfaNode::parse_dimension("210mm").unwrap_or_else(|_| num(595.27)));
            let height = pa
                .h
                .or(medium.map(|(_, h)| h))
                .unwrap_or_else(|| XfaNode::parse_dimension("297mm").unwrap_or_else(|_| num(841.89)));
            let content_areas: Vec<ContentAreaGeometry> = pa
                .children
                .iter()
                .filter(|c| {
                    matches!(c.kind, XfaNodeKind::ContentArea)
                        || matches!(&c.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "contentArea")
                })
                .map(|c| ContentAreaGeometry {
                    name: c.name.clone(),
                    bounds: Bounds::new(
                        c.x.unwrap_or(Decimal::ZERO),
                        c.y.unwrap_or(Decimal::ZERO),
                        c.w.unwrap_or(width),
                        c.h.unwrap_or(height),
                    ),
                })
                .collect();
            areas.push(AreaSpec {
                name: pa.name.clone(),
                position: qualifier(pa, "pagePosition", PagePosition::parse),
                odd_or_even: qualifier(pa, "oddOrEven", OddOrEven::parse),
                blank: qualifier(pa, "blankOrNotBlank", BlankOrNotBlank::parse),
                min_occur,
                max_occur,
                content_areas: content_areas.len(),
            });
            geometries.push(PageGeometry {
                width,
                height,
                path: r.path.clone(),
                name: pa.name.clone().unwrap_or_default(),
                content_areas,
            });
        }

        // The pageSets, in document order, each with its direct pageAreas
        // and child pageSets.
        fn collect_sets(
            nodes: &[XfaNode],
            page_area_refs: &[PageAreaRef<'_>],
            sets: &mut Vec<SetSpec>,
        ) -> Vec<usize> {
            let mut found = Vec::new();
            for node in nodes {
                let is_set = matches!(node.kind, XfaNodeKind::PageSet)
                    || matches!(&node.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "pageSet");
                if is_set {
                    let relation = Relation::parse(node.attributes.get("relation").map(String::as_str))
                        .unwrap_or_else(|e| {
                            log::warn!("{e} on pageSet {:?}; taken as orderedOccurrence", node.name);
                            Relation::OrderedOccurrence
                        });
                    let index = sets.len();
                    sets.push(SetSpec {
                        name: node.name.clone(),
                        relation,
                        areas: page_area_refs
                            .iter()
                            .enumerate()
                            .filter(|(_, r)| r.page_set.is_some_and(|ps| std::ptr::eq(ps, node)))
                            .map(|(i, _)| i)
                            .collect(),
                        children: Vec::new(),
                    });
                    let children = collect_sets(&node.children, page_area_refs, sets);
                    sets[index].children = children;
                    found.push(index);
                } else if !matches!(node.kind, XfaNodeKind::PageArea) {
                    found.extend(collect_sets(&node.children, page_area_refs, sets));
                }
            }
            found
        }
        let mut sets = Vec::new();
        collect_sets(xfa_nodes, page_area_refs, &mut sets);
        let orphans: Vec<usize> = page_area_refs
            .iter()
            .enumerate()
            .filter(|(_, r)| r.page_set.is_none())
            .map(|(i, _)| i)
            .collect();
        if !orphans.is_empty() {
            log::warn!(
                "{} pageArea(s) outside any pageSet; they are taken as one ordered page set after \
                 the others",
                orphans.len()
            );
            sets.push(SetSpec {
                name: None,
                relation: Relation::OrderedOccurrence,
                areas: orphans,
                children: Vec::new(),
            });
        }
        (PageSets { areas, sets }, geometries)
    }

    /// A pageArea's `<occur>` limits for ordered pagination: `min` (0 when
    /// absent: only an explicit minimum adds blank pages) and `max` (`None`
    /// for `-1` or absent: a page replicates without limit, XFA 3.3 §8
    /// "Occurrence Limits in Ordered Pagination").
    fn page_occurrence(page_area: &XfaNode) -> (u32, Option<u32>) {
        let occur = page_area.children.iter().find(
            |c| matches!(&c.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "occur"),
        );
        let Some(occur) = occur else {
            return (0, None);
        };
        let number = |name: &str| -> Option<i64> {
            let v = occur.attributes.get(name)?;
            match v.trim().parse::<i64>() {
                Ok(n) => Some(n),
                Err(_) => {
                    log::warn!("occur {name}={v:?} on pageArea {:?} is not a number; ignored", page_area.name);
                    None
                }
            }
        };
        let min = number("min").filter(|n| *n >= 0).map(|n| n as u32).unwrap_or(0);
        let max = number("max").filter(|n| *n >= 0).map(|n| n as u32);
        (min, max)
    }

    fn describe_pagination_error(error: &pagination::PaginationError) -> String {
        use pagination::PaginationError;
        match error {
            PaginationError::NoQualifiedArea { set } => format!(
                "no pageArea of pageSet {set:?} matches the layout state (XFA 3.3 §8: a fatal \
                 error); its first pageArea is used"
            ),
            PaginationError::Exhausted { set } => format!(
                "every pageArea of pageSet {set:?} has reached its occur max; its last pageArea is \
                 used once more"
            ),
            PaginationError::NoPages => "the template has no pageArea".to_string(),
        }
    }

    /// The next page the paginator chooses for `request`, skipping any pure
    /// boilerplate page (one without a contentArea) it emits on the way
    /// (XFA 3.3 §8 "Determining the start point").
    fn open_page(
        paginator: &mut Paginator<'_>,
        request: PageRequest,
        geometries: &[PageGeometry],
    ) -> Result<usize, String> {
        let mut request = request;
        for _ in 0..8 {
            let (page, error) = paginator.next_page(request);
            if let Some(e) = error {
                log::warn!("{}", Self::describe_pagination_error(&e));
            }
            if !geometries[paginator.pages()[page].area].content_areas.is_empty() {
                return Ok(page);
            }
            request = PageRequest::Next;
        }
        Err("more than eight pure boilerplate pages in a row: the page set never reaches a \
             content area"
            .to_string())
    }

    fn close_region(regions: &mut [Region], at: Num) {
        let region = regions.last_mut().expect("a content area is open");
        region.column_bottom = Some(at);
    }

    /// Open the content area that follows the last one: the next on the
    /// same page (the next with `name`, when one is asked for), else the
    /// first of the next page.
    fn open_overflow(
        regions: &mut Vec<Region>,
        paginator: &mut Paginator<'_>,
        geometries: &[PageGeometry],
        at: Num,
        name: Option<&str>,
        opened_by: Opening,
    ) -> Result<(), String> {
        let last = regions.last().expect("a content area is open");
        let page = last.page;
        let areas = &geometries[paginator.pages()[page].area].content_areas;
        let next_on_page = (last.area + 1..areas.len())
            .find(|&i| name.is_none_or(|n| areas[i].name.as_deref() == Some(n)));
        let (page, index) = match next_on_page {
            Some(i) => (page, i),
            None => {
                let page = Self::open_page(paginator, PageRequest::Next, geometries)?;
                let areas = &geometries[paginator.pages()[page].area].content_areas;
                let index = name
                    .and_then(|n| areas.iter().position(|a| a.name.as_deref() == Some(n)))
                    .unwrap_or(0);
                (page, index)
            }
        };
        let mut bounds = geometries[paginator.pages()[page].area].content_areas[index].bounds;
        bounds.y = at;
        let mut region = Region::open(page, index, bounds, opened_by);
        region.bounds = geometries[paginator.pages()[page].area].content_areas[index].bounds;
        regions.push(region);
        Ok(())
    }

    /// Open the content area a forced break asks for, at column `at`.
    fn open_after_break(
        request: &BreakRequest,
        regions: &mut Vec<Region>,
        paginator: &mut Paginator<'_>,
        page_sets: &PageSets,
        geometries: &[PageGeometry],
        at: Num,
    ) -> Result<(), String> {
        let page_request = match request.target_type {
            BreakTarget::ContentArea => {
                return Self::open_overflow(
                    regions,
                    paginator,
                    geometries,
                    at,
                    request.target.as_deref(),
                    Opening::Break,
                );
            }
            BreakTarget::PageArea => {
                let target = request.target.as_deref().and_then(|t| {
                    let resolved = page_sets.resolve_target(t);
                    if resolved.is_none() {
                        log::warn!(
                            "break target {t:?} names no pageArea or pageSet; the next page of the \
                             current page set is used"
                        );
                    }
                    resolved
                });
                target.map(PageRequest::Target).unwrap_or(PageRequest::Next)
            }
            BreakTarget::PageEven => PageRequest::Parity(pagination::Parity::Even),
            BreakTarget::PageOdd => PageRequest::Parity(pagination::Parity::Odd),
        };
        if let Some(set) = paginator.set_change(page_request) {
            // Leaving a page set ends its contiguous sequence of pages.
            Self::finish_page_set(paginator, regions, geometries);
            paginator.enter(set);
        }
        let page = Self::open_page(paginator, page_request, geometries)?;
        let mut region = Region::open(
            page,
            0,
            geometries[paginator.pages()[page].area].content_areas[0].bounds,
            Opening::Break,
        );
        region.column_top = at;
        regions.push(region);
        Ok(())
    }

    /// Whether a forced break at the very start of the open content area is
    /// already satisfied by it: the content area is on the page (or in the
    /// page set) the break names, and the break does not insist on a new
    /// one (`startNew`) -- unless this content area was itself opened
    /// because the previous one overflowed at exactly this point, which is
    /// the new one the break asks for.
    fn break_satisfied(
        request: &BreakRequest,
        paginator: &Paginator<'_>,
        page_sets: &PageSets,
        geometries: &[PageGeometry],
        region: &Region,
    ) -> bool {
        let current_area = paginator.pages()[region.page].area;
        let target_ok = match request.target_type {
            BreakTarget::PageArea => match request
                .target
                .as_deref()
                .and_then(|t| page_sets.resolve_target(t))
            {
                None => true,
                Some(Target::Area(a)) => a == current_area,
                Some(Target::Set(s)) => {
                    page_sets.effective_set(s) == Some(page_sets.set_of_area(current_area))
                }
            },
            BreakTarget::ContentArea => request.target.as_deref().is_none_or(|n| {
                geometries[current_area].content_areas[region.area].name.as_deref() == Some(n)
            }),
            BreakTarget::PageEven => (region.page + 1).is_multiple_of(2),
            BreakTarget::PageOdd => !(region.page + 1).is_multiple_of(2),
        };
        target_ok && (!request.start_new || region.opened_by == Opening::Overflow)
    }

    /// Termination processing for the page set the layout is in (XFA 3.3 §8
    /// "Termination processing"): the last page is redone on the designated
    /// last (or only) page when everything on it fits that page's content
    /// areas, and a blank instance of that page follows it otherwise.
    fn finish_page_set(
        paginator: &mut Paginator<'_>,
        regions: &[Region],
        geometries: &[PageGeometry],
    ) {
        let Some(last_page) = paginator.pages().len().checked_sub(1) else {
            return;
        };
        let spans: Vec<(usize, Num)> = regions
            .iter()
            .filter(|r| r.page == last_page)
            .map(|r| (r.area, r.column_bottom.unwrap_or(r.column_top) - r.column_top))
            .collect();
        let outcome = paginator.finish(|candidate| {
            let areas = &geometries[candidate].content_areas;
            spans
                .iter()
                .all(|(i, h)| areas.get(*i).is_some_and(|a| a.bounds.height >= *h))
        });
        log::debug!("page set termination: {outcome:?}");
    }

    /// Move a laid-out content column into its pages: every node goes with
    /// the content area its top edge falls in.
    fn stack_regions(children: &mut [FlattenedKind], placements: &[RegionPlacement]) {
        for child in children {
            match child {
                FlattenedKind::Node(node) => {
                    // A node above the first placement (there is none) stays
                    // with the first content area.
                    let k = placements
                        .partition_point(|p| p.column_top <= node.y)
                        .saturating_sub(1);
                    let Some(placement) = placements.get(k) else {
                        continue;
                    };
                    Self::shift_node_y(node, placement.dy);
                    if !placement.dx.is_zero() {
                        node.x += placement.dx;
                        if let Some(border) = node.style.border.as_mut()
                            && let Some((rx, _, _, _)) = border.render_bounds.as_mut().map(|b| &mut *b)
                        {
                            *rx += placement.dx;
                        }
                    }
                }
                FlattenedKind::Group { children, .. } => {
                    Self::stack_regions(children, placements);
                }
            }
        }
    }

    /// Apply the values the scripts hold to the laid-out nodes.
    ///
    /// A non-empty value under the node's own full path is the form's
    /// current value for it -- the engine seeds every field from its
    /// template value, so one that differs was written by a script or a
    /// person -- and replaces whatever the template says, the way
    /// ACYI_001_EN's initialize scripts replace a label's `*** [Last name]
    /// ***` placeholder with its text in the form's language. An empty one
    /// is not taken over a template text: a draw is carried in the values
    /// with no value of its own, and its text would vanish. A node is only
    /// ever looked up under its own full path: a bare name is shared by
    /// every node of that name, one instance of a repeated section and
    /// another, or two unrelated fields in different subforms.
    fn apply_computed_values(
        children: &mut [FlattenedKind],
        computed_values: &HashMap<SomPath, String>,
    ) {
        for child in children {
            match child {
                FlattenedKind::Node(node) => {
                    let path = node.som_path().cloned();
                    let computed = path
                        .as_ref()
                        .and_then(|p| computed_values.get(p.as_str()))
                        .filter(|v| !v.is_empty());
                    match &mut node.kind {
                        FlattenedNodeKind::Field { value, .. } => {
                            if let Some(computed) = computed {
                                *value = computed.clone();
                            }
                        }
                        FlattenedNodeKind::Text { content, .. } => {
                            if let Some(computed) = computed {
                                *content = computed.clone();
                            }
                        }
                        // An image's bytes and a shape's geometry come from
                        // the template, not from a script.
                        FlattenedNodeKind::Image(_) | FlattenedNodeKind::Shape(_) => {}
                    }
                }
                FlattenedKind::Group { children, .. } => {
                    Self::apply_computed_values(children, computed_values);
                }
            }
        }
    }

    /// Page dimensions from a pageArea's `<medium>`, for a pageArea that does
    /// not state its own `w`/`h`. `short` and `long` are the paper's two edges;
    /// `orientation` decides which of them is the width.
    fn medium_size(page_area: &XfaNode) -> Option<(Num, Num)> {
        for child in &page_area.children {
            let is_medium = matches!(
                &child.kind,
                XfaNodeKind::Element { tag_name, .. } if tag_name == "medium"
            );
            if !is_medium {
                continue;
            }
            let short = child
                .attributes
                .get("short")
                .and_then(|v| XfaNode::parse_dimension(v).ok())?;
            let long = child
                .attributes
                .get("long")
                .and_then(|v| XfaNode::parse_dimension(v).ok())?;
            let landscape = child
                .attributes
                .get("orientation")
                .map(|o| o == "landscape")
                .unwrap_or(false);
            return Some(if landscape {
                (long, short)
            } else {
                (short, long)
            });
        }
        None
    }

    /// Move every node in a subtree down by `dy`.
    fn shift_y(children: &mut [FlattenedKind], dy: Num) {
        for child in children {
            match child {
                FlattenedKind::Node(node) => Self::shift_node_y(node, dy),
                FlattenedKind::Group { children, .. } => Self::shift_y(children, dy),
            }
        }
    }

    /// Move one node down by `dy`.
    ///
    /// A node's position is not only its `y`: a row subform's border carries
    /// absolute `render_bounds` so its edges can span the cells, and those are
    /// in the same coordinate space. Moving one without the other leaves the
    /// rules of a table drawn across whichever page happens to sit at the old
    /// coordinates.
    fn shift_node_y(node: &mut FlattenedNode, dy: Num) {
        if dy.is_zero() {
            return;
        }
        node.y += dy;
        if let Some(border) = node.style.border.as_mut()
            && let Some((_, ry, _, _)) = border.render_bounds.as_mut().map(|b| &mut *b)
        {
            *ry += dy;
        }
    }

    /// The template's root subform and its SOM path (XFA 3.3 §7: the
    /// Template DOM is a root subform holding the page sets and the
    /// content; the Form DOM is the content under that root). The first
    /// `subform` child of the `template`, or, in a fragment without one,
    /// the first subform anywhere.
    fn find_root_subform(nodes: &[XfaNode]) -> Option<(&XfaNode, String)> {
        fn is_subform(node: &XfaNode) -> bool {
            matches!(node.kind, XfaNodeKind::Subform)
                || matches!(&node.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "subform")
        }
        fn is_template(node: &XfaNode) -> bool {
            matches!(node.kind, XfaNodeKind::Template)
                || matches!(&node.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "template")
        }
        fn is_data(node: &XfaNode) -> bool {
            matches!(&node.kind, XfaNodeKind::Element { tag_name, .. }
                if tag_name.starts_with("xfa:") || tag_name.starts_with("dd:")
                    || tag_name == "datasets" || tag_name == "data" || tag_name == "form")
        }
        fn under_template(nodes: &[XfaNode]) -> Option<&XfaNode> {
            for node in nodes {
                if is_data(node) {
                    continue;
                }
                if is_template(node)
                    && let Some(root) = node.children.iter().find(|c| is_subform(c))
                {
                    return Some(root);
                }
                if let Some(root) = under_template(&node.children) {
                    return Some(root);
                }
            }
            None
        }
        fn first_subform(nodes: &[XfaNode]) -> Option<&XfaNode> {
            for node in nodes {
                if is_data(node) {
                    continue;
                }
                if is_subform(node) {
                    return Some(node);
                }
                if let Some(found) = first_subform(&node.children) {
                    return Some(found);
                }
            }
            None
        }
        let root = under_template(nodes).or_else(|| first_subform(nodes))?;
        Some((root, root.name.clone().unwrap_or_default()))
    }

    /// Flatten a single node (used for pageArea children)
    fn flatten_single_node(
        node: &XfaNode,
        index: usize,
        parent_position: Position,
        parent_layout: Layout,
        flattened_children: &mut Vec<FlattenedKind>,
        ctx: &FlattenContext,
    ) -> Result<(), String> {
        let first_new = flattened_children.len();
        Self::flatten_single_node_untagged(
            node,
            index,
            parent_position,
            parent_layout,
            flattened_children,
            ctx,
        )?;
        // Every node this call produced is under the prevailing locale of
        // `ctx`, unless a nested call already stamped a closer one.
        for kind in &mut flattened_children[first_new..] {
            Self::stamp_locale(kind, &ctx.language);
        }
        Ok(())
    }

    /// Give every node under `kind` that has no locale yet the `locale`.
    fn stamp_locale(kind: &mut FlattenedKind, locale: &str) {
        match kind {
            FlattenedKind::Node(node) => {
                if node.locale.is_none() {
                    node.locale = Some(locale.to_string());
                }
            }
            FlattenedKind::Group { children, .. } => {
                for child in children {
                    Self::stamp_locale(child, locale);
                }
            }
        }
    }

    fn flatten_single_node_untagged(
        node: &XfaNode,
        index: usize,
        parent_position: Position,
        _parent_layout: Layout,
        flattened_children: &mut Vec<FlattenedKind>,
        ctx: &FlattenContext,
    ) -> Result<(), String> {
        // Check presence - per XFA spec, hidden/inactive nodes should not be rendered
        // This is critical for fields whose values are set by scripts but should remain hidden
        let presence = ctx.get_effective_presence(node);
        if presence.should_skip_render() {
            // Hidden/Inactive: skip entirely - don't render, don't consume layout space.
            // Invisible is also skipped here: this path is positioned layout, where
            // the space an invisible node would keep changes nothing for its
            // siblings, and emitting it would draw master-page content that the
            // form's own scripts concealed.
            return Ok(());
        }

        // For positioned layout, use node's x,y directly
        let x = node.x.unwrap_or(Decimal::ZERO);
        let y = node.y.unwrap_or(Decimal::ZERO);

        // Per XFA spec: if w is not specified, the element is horizontally growable.
        // Use minW as the width, or calculate natural width for Draw elements.
        let width = node.w.unwrap_or_else(|| {
            // For Draw elements without explicit width, use minW or natural text width
            if let XfaNodeKind::Draw = &node.kind {
                let natural_width = Self::growable_draw_width(node, ctx);
                let min_w = node.min_w.unwrap_or(Decimal::ZERO);
                natural_width.max(min_w)
            } else {
                // For other elements, use minW or fall back to parent width
                node.min_w.unwrap_or(parent_position.width)
            }
        });
        // A node without `h` is growable (XFA 3.3 §8): as tall as its
        // content, at least its `minH`; a container's own box does not
        // size its positioned children.
        let height = node.h.unwrap_or_else(|| {
            let min_h = node.min_h.unwrap_or(Decimal::ZERO);
            match &node.kind {
                XfaNodeKind::Draw => Self::natural_draw_height(
                    node,
                    (width - node.margin_left.unwrap_or(Decimal::ZERO)
                        - node.margin_right.unwrap_or(Decimal::ZERO))
                        .max(Decimal::ZERO),
                    min_h,
                    node.margin_top.unwrap_or(Decimal::ZERO),
                    node.margin_bottom.unwrap_or(Decimal::ZERO),
                    ctx,
                ),
                XfaNodeKind::Field => Self::field_natural_height(node).max(min_h),
                _ => min_h,
            }
        });

        let pos = Position::new(parent_position.x + x, parent_position.y + y, width, height);

        match &node.kind {
            XfaNodeKind::Draw => {
                // An image draw carries pixels rather than text; the whole text
                // pipeline below has nothing to do with it.
                let som_path = node.name.as_deref().map(|n| ctx.get_full_path(n, index));
                if let Some(image_node) = Self::flatten_value_draw(node, som_path, pos, ctx)? {
                    flattened_children.push(FlattenedKind::Node(image_node));
                    return Ok(());
                }

                // Extract text content, or use empty string if none (scripts may fill it later)
                // Use context to resolve xfa:embed references
                let text_content = ctx.extract_text(&node.children).unwrap_or_default();

                // Extract font info from XFA <font> element first
                let xfa_font_size = Self::extract_font_size(node);
                let xfa_font_name = Self::extract_font_name(node);

                let font_size = xfa_font_size;
                let font_name = xfa_font_name;
                let style = Self::extract_style(node);
                let (default_bold, default_italic) = style.get_xfa_bold_italic();

                // Get default h_align from XFA para element
                let default_h_align = node
                    .para
                    .as_ref()
                    .map(|p| p.h_align)
                    .unwrap_or(HAlign::Left);

                // Extract rich text if this is HTML content (exData with contentType="text/html")
                let rich_text = Self::extract_rich_text_from_node(
                    &node.children,
                    default_h_align,
                    Some(ctx.computed_values),
                    Some(ctx.id_to_field),
                    default_bold,
                    default_italic,
                    Decoration::of_font(node.font.as_ref()),
                    font_size,
                );

                let draw_node = FlattenedNode::new_text_with_rich_text(
                    text_content,
                    font_size,
                    font_name,
                    pos.x,
                    pos.y,
                    pos.width,
                    pos.height,
                    style,
                    node.rotate,
                    node.name.clone(),
                    rich_text,
                );
                // Split multi-paragraph draw nodes into one FlattenedNode per paragraph
                let mut draw_kinds =
                    Self::split_draw_into_paragraph_nodes(draw_node, &ctx.language);
                // Add NoPrint hint if relevant="-print" or inherited from parent
                if Self::is_no_print(node) || ctx.has_inherited_hint(&Hint::NoPrint) {
                    for kind in &mut draw_kinds {
                        kind.add_hint(Hint::NoPrint);
                    }
                }
                // Add SomPath hint to draw nodes (same pattern as fields)
                if let Some(name) = &node.name {
                    let som_path = ctx.get_full_path(name, index);
                    for kind in &mut draw_kinds {
                        kind.add_hint(Hint::SomPath(SomPath::new(som_path.clone())));
                    }
                }
                flattened_children.extend(draw_kinds);
            }
            XfaNodeKind::Field => {
                let field_name = node.name.clone().unwrap_or_else(|| "unnamed".to_string());
                let field_value = Self::extract_field_value(&node.children);
                let style = Self::extract_style(node);
                let som_path = ctx.get_full_path(&field_name, index);
                let is_checked = Self::checked_state(
                    node,
                    &som_path,
                    &field_value,
                    &ctx.parent_exclgroup_value,
                    ctx,
                );
                let access = Self::extract_field_access(node);

                let mut field_node = FlattenedNode::new_field_with_checked(
                    field_name.clone(),
                    field_value,
                    field_name.clone(),
                    pos.x,
                    pos.y,
                    pos.width,
                    pos.height,
                    style,
                    node.rotate,
                    is_checked,
                );
                // Add SomPath hint with full XFA path
                // In flatten_single_node, the ctx was NOT extended with the field's name,
                // so we need to append it here
                field_node.add_hint(Hint::SomPath(SomPath::new(som_path)));
                // Add ExclGroupSomPath hint if inside an exclGroup
                if let Some(ref exclgroup_path) = ctx.parent_exclgroup_som_path {
                    field_node.add_hint(Hint::ExclGroupSomPath(exclgroup_path.clone()));
                }
                // Access, length and lines, and how the value is shown.
                Self::add_field_value_hints(&mut field_node, node, access);
                // Its caption, beside, above or below the value.
                let caption_node = Self::caption_node(node, &mut field_node);
                // Add WidgetType hint if extracted
                if let Some(kind) = Self::extract_widget_kind(node) {
                    if kind == WidgetKind::Barcode
                        && let Some(barcode) = Self::extract_barcode(node)
                    {
                        field_node.add_hint(Hint::Barcode(barcode));
                    }
                    field_node.add_hint(Hint::WidgetType(kind));
                }
                // Add NoPrint hint if relevant="-print" or inherited from parent
                if Self::is_no_print(node) || ctx.has_inherited_hint(&Hint::NoPrint) {
                    field_node.add_hint(Hint::NoPrint);
                }
                flattened_children.push(FlattenedKind::Node(field_node));
                if let Some(caption) = caption_node {
                    flattened_children.push(FlattenedKind::Node(caption));
                }
            }
            XfaNodeKind::Subform | XfaNodeKind::ExclGroup | XfaNodeKind::Element { .. } => {
                // A subform (or exclGroup) that declares a flow layout ("tb",
                // "lr-tb", "table", ...) lays its children out with that flow,
                // not by their own x/y -- the same rule the form body already
                // follows. Master-page content below used to always treat
                // children as positioned (each at its own x/y relative to
                // this node), which piles lr-tb/tb children on top of each
                // other whenever none of them has an explicit x/y -- e.g. a
                // page header built as an lr-tb row of draws. Delegate those
                // to the growable-container and flow-layout code the body
                // uses (`flatten_nodes` / `compute_position_for_node_with_children`)
                // instead of duplicating it here; that code positions this
                // node itself from `parent_position` exactly as this
                // function's own `pos` above does, then flows its children
                // by its `layout` attribute.
                let layout = node
                    .layout
                    .as_ref()
                    .and_then(|l| l.parse().ok())
                    .unwrap_or(Layout::Position);
                if layout != Layout::Position {
                    Self::flatten_nodes_indexed(
                        std::slice::from_ref(node),
                        &[index],
                        parent_position,
                        Layout::Position,
                        flattened_children,
                        ctx,
                    )?;
                    return Ok(());
                }

                // Repeated instances are ordinary siblings in the Form DOM
                // (see `xfa::instances`); a subform is flattened the same way
                // whether or not it repeats. Its children are addressed
                // under its own SOM path, as in the body (`flatten_nodes_indexed`),
                // and inherit `relevant="-print"`.
                let child_ctx = {
                    let base = if Self::is_no_print(node) || ctx.has_inherited_hint(&Hint::NoPrint) {
                        ctx.with_inherited_hint(Hint::NoPrint)
                    } else {
                        ctx.derive()
                    };
                    let base = match node.attributes.get("locale") {
                        Some(locale) => base.with_locale(locale),
                        None => base,
                    };
                    match &node.name {
                        Some(name) => base.with_path_segment(name, index),
                        None => base,
                    }
                };
                for (child, child_index) in
                    node.children.iter().zip(sibling_indices(&node.children))
                {
                    Self::flatten_single_node(
                        child,
                        child_index,
                        pos,
                        Layout::Position,
                        flattened_children,
                        &child_ctx,
                    )?;
                }
            }
            _ => {}
        }

        Ok(())
    }


    /// Find ALL pageAreas and their contentAreas in the XFA tree.
    /// This is needed because XFA forms can have multiple pageAreas (e.g., first page, last page,
    /// annex pages), each with their own page background content that must be rendered.
    ///
    /// Returns one [`PageAreaRef`] per pageArea found, in document order,
    /// each carrying the nearest enclosing `pageSet`.
    fn find_all_page_areas(nodes: &[XfaNode]) -> Vec<PageAreaRef<'_>> {
        fn collect_recursive<'a>(
            nodes: &'a [XfaNode],
            enclosing_page_set: Option<&'a XfaNode>,
            parent_path: &str,
            result: &mut Vec<PageAreaRef<'a>>,
        ) {
            for (node, index) in nodes.iter().zip(sibling_indices(nodes)) {
                let path = match node.name.as_deref() {
                    Some(name) if node.kind.extends_som_path() => {
                        child_som_path(parent_path, name, index)
                    }
                    _ => parent_path.to_string(),
                };
                if matches!(node.kind, XfaNodeKind::PageArea) {
                    result.push(PageAreaRef {
                        page_area: node,
                        page_set: enclosing_page_set,
                        path,
                    });
                    // Don't recurse into pageArea children - they are content, not structure
                    continue;
                }

                let is_page_set = matches!(node.kind, XfaNodeKind::PageSet);

                // Recurse into container-like nodes to find more pageAreas
                let should_recurse = matches!(
                    node.kind,
                    XfaNodeKind::Template
                        | XfaNodeKind::PageSet
                        | XfaNodeKind::Subform
                        | XfaNodeKind::ExclGroup
                ) || matches!(&node.kind, XfaNodeKind::Element { .. });

                if should_recurse {
                    let next_page_set = if is_page_set {
                        Some(node)
                    } else {
                        enclosing_page_set
                    };
                    collect_recursive(&node.children, next_page_set, &path, result);
                }
            }
        }

        let mut result = Vec::new();
        collect_recursive(nodes, None, "", &mut result);
        result
    }

    /// Extract style information from an XFA node
    fn extract_style(node: &XfaNode) -> RenderStyle {
        RenderStyle {
            border: node.border.clone(),
            font: node.font.clone(),
            para: node.para.clone(),
        }
    }

    /// Anchor a leaf's own border to its outer (margin-inclusive) box.
    ///
    /// A leaf (draw/field) is positioned by its *content* box -- its own
    /// nominal box inset by its own `<margin>` -- so that text starts after
    /// the margin (see `compute_position_for_node_with_children`'s
    /// `content_pos`). But per XFA 3.3 ch. 8 "Layout for Growable Objects":
    /// "borders have no effect upon the nominal extent" of an object -- a
    /// `<border>` draws around the object's own (outer) box, not its
    /// margin-inset content box. Without this, a draw's own top edge (e.g.
    /// the rule above a section heading) paints `topInset` too low, flush
    /// against the first line of text instead of above the margin.
    ///
    /// Reuses the same `render_bounds` field
    /// `propagate_subform_border_to_children` uses to paint a *parent*
    /// subform's border at the subform's own bounds instead of a child
    /// leaf's; here the "parent" bounds are simply this leaf's own outer box.
    fn anchor_own_border(style: &mut RenderStyle, outer: Position) {
        if let Some(border) = &mut style.border {
            border.render_bounds = Some((outer.x, outer.y, outer.width, outer.height));
        }
    }

    /// Extract field access level from an XFA node's attributes.
    /// Defaults to Open if no access attribute is specified.
    /// The text node of `node`'s caption, when it has one to draw, with the
    /// `Caption` hint on `field_node` that keeps its value out of the
    /// caption's region (XFA 3.3 "The caption element"). The caption takes
    /// its `reserve`, or what its text needs when it gives none; a button's
    /// caption is its face, so it covers the whole button.
    fn caption_node(node: &XfaNode, field_node: &mut FlattenedNode) -> Option<FlattenedNode> {
        let caption = crate::xfa::caption::Caption::of(node)?;
        let is_button = matches!(Self::extract_widget_kind(node), Some(WidgetKind::Button));
        let font = caption.font.clone().or_else(|| node.font.clone());
        let bounds = (
            field_node.x.to_f32().unwrap_or(0.0),
            field_node.y.to_f32().unwrap_or(0.0),
            field_node.width.to_f32().unwrap_or(0.0),
            field_node.height.to_f32().unwrap_or(0.0),
        );
        let caption_box = if is_button {
            bounds
        } else {
            let reserve = caption.reserve.unwrap_or_else(|| match caption.placement {
                CaptionPlacement::Top | CaptionPlacement::Bottom => Self::text_line_height(&font),
                _ => Self::measured_text_width(&caption.text, &font),
            });
            field_node.add_hint(Hint::Caption {
                placement: caption.placement,
                text: (!caption.text.is_empty()).then(|| caption.text.clone()),
                reserve,
            });
            crate::xfa::caption::split_caption_box(bounds, caption.placement, reserve.to_f32().unwrap_or(0.0)).0
        };
        if !caption.drawn || caption.text.is_empty() {
            return None;
        }
        let (font_size, font_name) = {
            let f = font.clone().unwrap_or_default();
            (f.size, f.typeface)
        };
        let mut text_node = FlattenedNode::builder()
            .bounds(num(caption_box.0 as f64), num(caption_box.1 as f64), num(caption_box.2 as f64), num(caption_box.3 as f64))
            .style(RenderStyle {
                border: None,
                font,
                para: caption.para.clone(),
            })
            .rotate(node.rotate)
            .text(caption.text, font_size, font_name)
            .build();
        for hint in field_node.hints.iter().filter(|h| matches!(h, Hint::NoPrint)) {
            text_node.add_hint(hint.clone());
        }
        Some(text_node)
    }

    /// A field node's `FieldBehavior` (its access, and from its template
    /// whether it takes several lines and how many characters) and, when
    /// the template describes its value, the `ValueFormat` that shows it.
    fn add_field_value_hints(field_node: &mut FlattenedNode, node: &XfaNode, access: FieldAccess) {
        let meta = node.field_meta.as_deref();
        field_node.add_hint(Hint::FieldBehavior {
            access,
            multiline: meta.is_some_and(|m| m.multiline),
            max_length: meta.and_then(|m| m.max_chars),
            comb_cells: None,
        });
        if let Some(meta) = meta {
            field_node.add_hint(Hint::ValueFormat(Arc::new(meta.clone())));
        }
        if let Some(button) = node.check_button() {
            field_node.add_hint(Hint::CheckButton(button));
        }
    }

    fn extract_field_access(node: &XfaNode) -> FieldAccess {
        node.attributes
            .get("access")
            .and_then(|s| s.parse().ok())
            .unwrap_or(FieldAccess::Open)
    }

    /// A container's own `<border>` as a node of its own, drawn under its
    /// children (XFA 3.3 "The border element": the box model element of a
    /// subform or exclusion group, like a draw's). `None` when the border is
    /// hidden, or has neither a visible edge nor a fill. `(x, y, w, h)` is
    /// the container's outer box at its final height; the border's own
    /// margins inset it.
    fn container_border_node(
        border: &crate::xfa::Border,
        (x, y, w, h): (Num, Num, Num, Num),
    ) -> Option<FlattenedKind> {
        use crate::xfa::shapes::{Hand, RectangleShape, Shape, default_edge};
        if border.presence == "hidden" || border.presence == "inactive" {
            return None;
        }
        // A border declaring no edge has the default edge, as a rectangle
        // does.
        let edges = if border.edges.is_empty() {
            vec![default_edge()]
        } else {
            border.edges.clone()
        };
        let drawn = border.fill.as_ref().is_some_and(|f| f.presence == "visible")
            || edges.iter().any(|e| e.presence == "visible");
        if !drawn {
            return None;
        }
        let inset = |m: Option<Num>| m.unwrap_or(Decimal::ZERO);
        let (ml, mt, mr, mb) = (
            inset(border.margin_left),
            inset(border.margin_top),
            inset(border.margin_right),
            inset(border.margin_bottom),
        );
        let node = FlattenedNode::builder()
            .bounds(x + ml, y + mt, (w - ml - mr).max(Decimal::ZERO), (h - mt - mb).max(Decimal::ZERO))
            .shape(Shape::Rectangle(RectangleShape {
                hand: Hand::default(),
                edges,
                corners: border.corners.clone(),
                fill: border.fill.clone(),
            }))
            .build();
        Some(FlattenedKind::Node(node))
    }

    /// Extract widget kind from a field's <ui> child element.
    /// Per XFA spec, the <ui> element contains the widget type (textEdit, checkButton, etc.)
    /// and the shape attribute distinguishes radio buttons (round) from checkboxes (square/default).
    pub(crate) fn extract_widget_kind(node: &XfaNode) -> Option<WidgetKind> {
        for child in &node.children {
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                && tag_name == "ui"
            {
                // Look for widget type element inside <ui>
                for ui_child in &child.children {
                    if let XfaNodeKind::Element {
                        tag_name: ui_tag, ..
                    } = &ui_child.kind
                    {
                        match ui_tag.as_str() {
                            "textEdit" => {
                                // Check if multiLine attribute is set
                                let multiline = ui_child
                                    .attributes
                                    .get("multiLine")
                                    .map(|s| s == "1")
                                    .unwrap_or(false);
                                return Some(if multiline {
                                    WidgetKind::TextArea
                                } else {
                                    WidgetKind::Text
                                });
                            }
                            "checkButton" => {
                                // Check shape attribute: "round" = radio, default/square = checkbox
                                let shape = ui_child.attributes.get("shape");
                                return Some(if shape.map(|s| s.as_str()) == Some("round") {
                                    WidgetKind::Radio
                                } else {
                                    WidgetKind::Checkbox
                                });
                            }
                            "choiceList" => {
                                return Some(WidgetKind::Dropdown);
                            }
                            "dateTimeEdit" => {
                                // Check picker attribute to determine date/time/datetime
                                let picker = ui_child.attributes.get("picker");
                                return Some(match picker.map(|s| s.as_str()) {
                                    Some("date") => WidgetKind::Date,
                                    Some("time") => WidgetKind::Time,
                                    Some("dateTime") => WidgetKind::DateTime,
                                    _ => WidgetKind::Date, // default to date
                                });
                            }
                            "numericEdit" => return Some(WidgetKind::Numeric),
                            "passwordEdit" => return Some(WidgetKind::Password),
                            "button" => return Some(WidgetKind::Button),
                            "barcode" => return Some(WidgetKind::Barcode),
                            "imageEdit" => return Some(WidgetKind::Image),
                            "signature" => return Some(WidgetKind::Signature),
                            _ => {}
                        }
                    }
                }
                break;
            }
        }
        None
    }

    /// The `type` of a field's `<ui><barcode>`, which says how its value is
    /// turned into bars.
    /// The field's `<ui><barcode>`, with its options.
    fn extract_barcode(node: &XfaNode) -> Option<crate::xfa::Barcode> {
        let ui = node.children.iter().find(
            |c| matches!(&c.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "ui"),
        )?;
        let barcode = ui.children.iter().find(
            |c| matches!(&c.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "barcode"),
        )?;
        Some(crate::xfa::Barcode::of(barcode))
    }

    /// Extract dropdown options from a field node's <items> children and <choiceList> attributes.
    /// Per XFA spec, a field can have one or two <items> elements:
    /// - One <items>: display value = save value
    /// - Two <items>: the one with save="1" contains save values, the other display values
    ///
    /// The <choiceList> element inside <ui> provides textEntry and open attributes. The
    /// (display, save) pairing itself is [`XfaNode::extract_choice_list_items`], shared
    /// with the scripting engine's `addItem`/`clearItems`/... methods so a field's choice
    /// list looks the same however it is asked for.
    fn extract_dropdown_hint(node: &XfaNode) -> Option<Hint> {
        let options = node.extract_choice_list_items();
        if options.is_empty() {
            return None;
        }

        let mut text_entry = false;
        let mut multi_select = false;
        for child in &node.children {
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                && tag_name == "ui"
            {
                // Look for <choiceList> inside <ui> to get textEntry and open attributes
                for ui_child in &child.children {
                    if let XfaNodeKind::Element {
                        tag_name: ui_tag, ..
                    } = &ui_child.kind
                        && ui_tag == "choiceList"
                    {
                        text_entry = ui_child
                            .attributes
                            .get("textEntry")
                            .map(|s| s == "1")
                            .unwrap_or(false);
                        multi_select = ui_child
                            .attributes
                            .get("open")
                            .map(|s| s == "multiSelect")
                            .unwrap_or(false);
                    }
                }
            }
        }

        Some(Hint::Dropdown {
            options,
            text_entry,
            multi_select,
        })
    }

    /// Whether `relevant` leaves the node out of the print view (XFA 3.3
    /// "The relevant property": a space-separated list of `[+|-]viewname`
    /// tokens). `-print` excludes it; a list that only includes views and
    /// does not name print excludes it too. Used for screen-only
    /// interactive elements like add/remove buttons.
    fn is_no_print(node: &XfaNode) -> bool {
        let Some(relevant) = node.attributes.get("relevant") else {
            return false;
        };
        let tokens: Vec<&str> = relevant.split_whitespace().collect();
        if tokens.is_empty() {
            return false;
        }
        if tokens.contains(&"-print") {
            return true;
        }
        tokens.iter().all(|t| !t.starts_with('-'))
            && !tokens.iter().any(|t| t.trim_start_matches('+') == "print")
    }

    /// Extract font size from node, with default fallback
    fn extract_font_size(node: &XfaNode) -> Num {
        node.font
            .as_ref()
            .map(|f| f.size)
            .unwrap_or_else(|| Font::default().size)
    }

    /// Extract font name from node, with the XFA default (XFA 3.3 "The
    /// font element": Courier) when it declares none
    fn extract_font_name(node: &XfaNode) -> String {
        node.font
            .as_ref()
            .map(|f| f.typeface.clone())
            .unwrap_or_else(|| Font::default().typeface)
    }

    /// Count the number of paragraphs in HTML content.
    /// Used for height calculation to account for paragraph breaks.
    fn count_html_paragraphs(children: &[XfaNode]) -> usize {
        let mut count = 0;
        Self::count_paragraphs_recursive(children, &mut count);
        count.max(1) // At least 1 paragraph
    }

    fn count_paragraphs_recursive(children: &[XfaNode], count: &mut usize) {
        for child in children {
            match &child.kind {
                XfaNodeKind::Element { tag_name, .. } => {
                    let tag_lower = tag_name.to_lowercase();
                    if tag_lower == "p" {
                        *count += 1;
                    }
                    // Recurse into children
                    Self::count_paragraphs_recursive(&child.children, count);
                }
                XfaNodeKind::Value => {
                    Self::count_paragraphs_recursive(&child.children, count);
                }
                _ => {
                    Self::count_paragraphs_recursive(&child.children, count);
                }
            }
        }
    }

    /// Check if a node contains HTML exData content
    fn has_html_exdata(children: &[XfaNode]) -> bool {
        for child in children {
            if matches!(child.kind, XfaNodeKind::Value) {
                for value_child in &child.children {
                    if let XfaNodeKind::Element { tag_name, .. } = &value_child.kind
                        && tag_name == "exData"
                    {
                        for ex_child in &value_child.children {
                            if let XfaNodeKind::Element {
                                tag_name: inner_tag,
                                ..
                            } = &ex_child.kind
                                && inner_tag == "body"
                            {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }

    /// Extract rich text from a node's value children.
    /// Handles both:
    /// - HTML exData content (with paragraph styling, text-indent, xfa-spacerun)
    /// - Plain <text> elements containing U+2029 paragraph separators
    fn extract_rich_text_from_node(
        children: &[XfaNode],
        default_h_align: HAlign,
        computed_values: Option<&HashMap<SomPath, String>>,
        id_to_field: Option<&HashMap<String, String>>,
        default_bold: bool,
        default_italic: bool,
        default_decoration: Decoration,
        // The draw's own font size, the base of `em` lengths in its CSS.
        default_font_size: Num,
    ) -> Option<RichText> {
        for child in children {
            // Check for XfaNodeKind::Value
            if matches!(child.kind, XfaNodeKind::Value) {
                for value_child in &child.children {
                    if let XfaNodeKind::Element {
                        tag_name,
                        text_content,
                    } = &value_child.kind
                    {
                        // Check for <text> element with U+2029 paragraph separators
                        if tag_name == "text"
                            && let Some(text) = text_content
                            && text.contains('\u{2029}')
                        {
                            // Create rich text from plain text with paragraph separators
                            return Some(Self::create_rich_text_from_plain_with_separators(
                                text,
                                default_h_align,
                                default_decoration,
                            ));
                        }

                        if tag_name == "exData" {
                            // Check if it has HTML body content
                            for ex_child in &value_child.children {
                                if let XfaNodeKind::Element {
                                    tag_name: inner_tag,
                                    ..
                                } = &ex_child.kind
                                    && inner_tag == "body"
                                {
                                    // Found HTML body - parse it into RichText
                                    return Some(Self::parse_rich_text_from_html(
                                        &value_child.children,
                                        default_h_align,
                                        computed_values,
                                        id_to_field,
                                        default_bold,
                                        default_italic,
                                        default_decoration,
                                        default_font_size.to_f32().unwrap_or(10.0),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            // Also check for Element with tag_name "value"
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                && tag_name == "value"
            {
                for value_child in &child.children {
                    if let XfaNodeKind::Element {
                        tag_name: inner_tag,
                        text_content,
                    } = &value_child.kind
                    {
                        // Check for <text> element with U+2029 paragraph separators
                        if inner_tag == "text"
                            && let Some(text) = text_content
                            && text.contains('\u{2029}')
                        {
                            return Some(Self::create_rich_text_from_plain_with_separators(
                                text,
                                default_h_align,
                                default_decoration,
                            ));
                        }

                        if inner_tag == "exData" {
                            for ex_child in &value_child.children {
                                if let XfaNodeKind::Element {
                                    tag_name: body_tag, ..
                                } = &ex_child.kind
                                    && body_tag == "body"
                                {
                                    return Some(Self::parse_rich_text_from_html(
                                        &value_child.children,
                                        default_h_align,
                                        computed_values,
                                        id_to_field,
                                        default_bold,
                                        default_italic,
                                        default_decoration,
                                        default_font_size.to_f32().unwrap_or(10.0),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Create rich text structure from plain text containing U+2029 paragraph separators.
    /// Each segment separated by U+2029 becomes a separate paragraph.
    fn create_rich_text_from_plain_with_separators(
        text: &str,
        default_h_align: HAlign,
        decoration: Decoration,
    ) -> RichText {
        let segments: Vec<&str> = text.split('\u{2029}').collect();
        let mut paragraphs = Vec::new();

        for segment in segments {
            // Normalize whitespace in each segment
            let normalized = Self::normalize_whitespace(segment);

            let mut para = RichParagraph {
                h_align: default_h_align,
                ..RichParagraph::default()
            };

            if !normalized.is_empty() {
                para.runs.push(RichRun {
                    text: normalized,
                    preserve_spaces: false,
                    bold: false,
                    italic: false,
                    underline: decoration.underline,
                    line_through: decoration.line_through,
                    superscript: false,
                    letter_spacing: None,
                });
            } else {
                para.is_empty = true;
            }

            paragraphs.push(para);
        }

        // If no paragraphs were created, add an empty one
        if paragraphs.is_empty() {
            paragraphs.push(RichParagraph {
                h_align: default_h_align,
                is_empty: true,
                ..RichParagraph::default()
            });
        }

        RichText { paragraphs }
    }

    fn flatten_nodes(
        nodes: &[XfaNode],
        parent_position: Position,
        parent_layout: Layout,
        flattened_children: &mut Vec<FlattenedKind>,
        ctx: &FlattenContext,
    ) -> Result<Num, String> {
        Self::flatten_nodes_indexed(
            nodes,
            &sibling_indices(nodes),
            parent_position,
            parent_layout,
            flattened_children,
            ctx,
        )
    }

    /// As [`flatten_nodes`](Self::flatten_nodes), with each node's index
    /// among its same-named siblings given by the caller -- needed when
    /// `nodes` is a one-element slice cut from a longer sibling list, whose
    /// own indices would all restart at 0.
    fn flatten_nodes_indexed(
        nodes: &[XfaNode],
        indices: &[usize],
        parent_position: Position,
        parent_layout: Layout,
        flattened_children: &mut Vec<FlattenedKind>,
        ctx: &FlattenContext,
    ) -> Result<Num, String> {
        assert_eq!(nodes.len(), indices.len(), "one sibling index per node");
        // Returns the total height consumed by these nodes
        // Initialize flow position based on layout direction
        // For right-to-left layouts, start from the right edge
        let mut current_x = match parent_layout {
            Layout::RightToLeftTopToBottom | Layout::RightToLeftRow => {
                parent_position.x + parent_position.width
            }
            _ => parent_position.x,
        };
        let mut current_y = parent_position.y;
        let mut max_height_in_row = Decimal::ZERO;
        let start_y = parent_position.y;

        // For positioned layout, track the maximum extent (bottom-most point of all children)
        // This is needed when this container has no explicit height - we compute it from children
        let mut max_extent_y = Decimal::ZERO;

        for (node, &index) in nodes.iter().zip(indices) {
            // Check presence attribute using the context
            // This considers: inherited presence > script-set presence > static attribute
            // Per XFA spec (section 2, "Explicitly Concealing Containers"):
            // - Visible - element is rendered and participates in layout (normal behavior)
            // - Invisible - element takes up space but is NOT rendered (participates in layout)
            // - Hidden - element does NOT take up space and is NOT rendered (no layout)
            // - Inactive - element does NOT take up space and is NOT rendered (no layout, no automation)
            let presence = ctx.get_effective_presence(node);
            let skip_render = presence.should_skip_render();

            if presence.should_skip_layout() {
                // Hidden/Inactive: skip entirely - don't render, don't consume layout space
                continue;
            }

            // Create child context - if this node's presence is hidden,
            // inactive or invisible, children inherit that presence
            // Also extend the SOM path if this node has a name
            // Also propagate NoPrint hint if this node has relevant="-print"
            let child_ctx = {
                let base_ctx = if presence.should_skip_render() {
                    ctx.with_inherited_presence(presence)
                } else {
                    // Pass through existing inherited presence or none
                    if let Some(inherited) = ctx.inherited_presence {
                        ctx.with_inherited_presence(inherited)
                    } else {
                        // No inheritance needed, use visible as default
                        ctx.with_inherited_presence(Presence::Visible)
                    }
                };
                // Propagate NoPrint hint if this node has relevant="-print"
                // Per XFA spec, relevant is inherited by children
                let base_ctx = if Self::is_no_print(node) || ctx.has_inherited_hint(&Hint::NoPrint)
                {
                    base_ctx.with_inherited_hint(Hint::NoPrint)
                } else {
                    base_ctx
                };
                let base_ctx = match node.attributes.get("locale") {
                    Some(locale) => base_ctx.with_locale(locale),
                    None => base_ctx,
                };
                // Extend the SOM path if this node has a name
                if let Some(name) = &node.name {
                    base_ctx.with_path_segment(name, index)
                } else {
                    base_ctx
                }
            };

            match &node.kind {
                XfaNodeKind::Subform => {
                    let (outer_pos, content_pos, layout, consumed_height) =
                        Self::compute_position_for_node_with_children(
                            node,
                            parent_position,
                            parent_layout,
                            &mut current_x,
                            &mut current_y,
                            &mut max_height_in_row,
                            flattened_children,
                            &child_ctx,
                        )?;

                    let layout_ctx = if layout == Layout::Table {
                        let widths = Self::resolve_table_column_widths(node, &child_ctx);
                        child_ctx.with_table_column_widths(Some(widths))
                    } else {
                        child_ctx.clone()
                    };

                    // For positioned layout, track the max extent (relative to parent_position)
                    if parent_layout == Layout::Position {
                        let node_bottom = (outer_pos.y - parent_position.y) + outer_pos.height;
                        max_extent_y = max_extent_y.max(node_bottom);
                    }

                    // Snapshot the current output length so we can locate the children
                    // added by this subform during border propagation below.
                    let subform_border = node.border.clone();
                    let subform_children_start = flattened_children.len();

                    Self::begin_extent(ctx);
                    let children_height = Self::flatten_nodes(
                        &node.children,
                        content_pos,
                        layout,
                        flattened_children,
                        &layout_ctx,
                    )?;

                    // A growable subform (no explicit `h`) is given height 0
                    // by `compute_position_for_node_with_children` up front --
                    // "containers grow to fit their children", but the
                    // children have not been flattened yet at that point.
                    // `outer_pos.height` therefore still reads 0 here,
                    // regardless of `parent_layout`. The true height, now
                    // that `children_height` is known, is this -- used both
                    // to draw the subform's own border/fill at its real
                    // bounds (below) and, for tb layout, to advance the flow
                    // position past it.
                    let finalised_height = node.h.is_none().then(|| {
                        let actual_height = children_height
                            + node.margin_top.unwrap_or(Decimal::ZERO)
                            + node.margin_bottom.unwrap_or(Decimal::ZERO);
                        let min_h = node.min_h.unwrap_or(Decimal::ZERO);
                        actual_height.max(min_h).max(consumed_height)
                    });

                    // The subform's own `<border>`, at its final (grown)
                    // height, drawn under its children.
                    if let Some(border_node) = subform_border.as_ref().and_then(|border| {
                        Self::container_border_node(
                            border,
                            (
                                outer_pos.x,
                                outer_pos.y,
                                outer_pos.width,
                                finalised_height.unwrap_or(outer_pos.height),
                            ),
                        )
                    }) {
                        flattened_children.insert(subform_children_start, border_node);
                    }

                    // For tb layout, update current_y based on actual content height if no explicit height
                    if parent_layout == Layout::TopToBottom
                        && let Some(effective_height) = finalised_height
                    {
                        // Adjust current_y if children consumed more height than the default
                        if effective_height > consumed_height {
                            current_y = outer_pos.y + effective_height;
                        }
                    }

                    // The subform's extent, at its true height now that its
                    // children are laid out.
                    Self::end_container_extent(
                        node,
                        parent_layout,
                        outer_pos.y,
                        outer_pos.y + finalised_height.unwrap_or(outer_pos.height),
                        ctx,
                    );

                    // Table layout also stacks child rows/non-row objects vertically.
                    // If a child has no explicit height, grow by measured children height.
                    if parent_layout == Layout::Table && node.h.is_none() {
                        let actual_height = children_height
                            + node.margin_top.unwrap_or(Decimal::ZERO)
                            + node.margin_bottom.unwrap_or(Decimal::ZERO);
                        let min_h = node.min_h.unwrap_or(Decimal::ZERO);
                        let effective_height = actual_height.max(min_h).max(consumed_height);
                        if effective_height > consumed_height {
                            current_y = outer_pos.y + effective_height;
                        }
                    }

                    // For lr-tb layout, update max_height_in_row based on actual
                    // content height. Per XFA spec, a subform without explicit `h`
                    // is growable and its nominal extent is determined inside-out
                    // from its children. Without this, the row height stays at 0
                    // and the next wrapped row overlaps this one.
                    if matches!(
                        parent_layout,
                        Layout::LeftToRightTopToBottom | Layout::LeftToRight
                    ) && node.h.is_none()
                    {
                        let actual_height = children_height
                            + node.margin_top.unwrap_or(Decimal::ZERO)
                            + node.margin_bottom.unwrap_or(Decimal::ZERO);
                        // `minH` holds in every layout (XFA 3.3 §8).
                        let min_h = node.min_h.unwrap_or(Decimal::ZERO);
                        let effective_height = actual_height.max(min_h).max(consumed_height);
                        max_height_in_row = max_height_in_row.max(effective_height);
                    }

                    // For positioned layout, update max_extent_y after recursion.
                    // Per XFA spec, a container without explicit h grows to fit
                    // its children's nominal extents. The pre-recursion update
                    // used outer_pos.height which is 0 for growable containers.
                    if parent_layout == Layout::Position && node.h.is_none() {
                        let actual_height = children_height
                            + node.margin_top.unwrap_or(Decimal::ZERO)
                            + node.margin_bottom.unwrap_or(Decimal::ZERO);
                        let effective_height = actual_height.max(consumed_height);
                        let node_bottom = (outer_pos.y - parent_position.y) + effective_height;
                        max_extent_y = max_extent_y.max(node_bottom);
                    }
                }
                XfaNodeKind::Field => {
                    let (outer_pos, content_pos, _layout, _) =
                        Self::compute_position_for_node_with_children(
                            node,
                            parent_position,
                            parent_layout,
                            &mut current_x,
                            &mut current_y,
                            &mut max_height_in_row,
                            flattened_children,
                            &child_ctx,
                        )?;

                    // For positioned layout, track the max extent (relative to parent_position)
                    if parent_layout == Layout::Position {
                        let node_bottom = (outer_pos.y - parent_position.y) + outer_pos.height;
                        max_extent_y = max_extent_y.max(node_bottom);
                    }

                    // The field's extent: a leaf's height is known up front.
                    Self::leaf_extent(
                        node,
                        outer_pos.y,
                        outer_pos.y + outer_pos.height,
                        Self::field_splitting(node, outer_pos.height),
                        ctx,
                    );

                    // Only add to output if not hidden
                    // For fields, use outer_pos for rendering (field box includes margins)
                    if !skip_render {
                        let field_name = node.name.clone().unwrap_or_else(|| "unnamed".to_string());
                        let field_value = Self::extract_field_value(&node.children);
                        let mut style = Self::extract_style(node);
                        Self::anchor_own_border(&mut style, outer_pos);
                        let is_checked = Self::checked_state(
                            node,
                            child_ctx.current_path.as_str(),
                            &field_value,
                            &child_ctx.parent_exclgroup_value,
                            ctx,
                        );
                        let access = Self::extract_field_access(node);
                        let widget_kind = Self::extract_widget_kind(node);

                        let mut field_node = FlattenedNode::new_field_with_checked(
                            field_name.clone(),
                            field_value,
                            field_name.clone(),
                            content_pos.x,
                            content_pos.y,
                            content_pos.width,
                            content_pos.height,
                            style,
                            node.rotate,
                            is_checked,
                        );
                        // Add SomPath hint with full XFA path
                        // The context path already includes this field's name (via with_path_segment)
                        let som_path = child_ctx.current_path.clone();
                        field_node.add_hint(Hint::SomPath(SomPath::new(som_path)));
                        // Add ExclGroupSomPath hint if inside an exclGroup
                        if let Some(ref exclgroup_path) = child_ctx.parent_exclgroup_som_path {
                            field_node.add_hint(Hint::ExclGroupSomPath(exclgroup_path.clone()));
                        }
                        // Access, length and lines, and how the value is shown.
                        Self::add_field_value_hints(&mut field_node, node, access);
                        // Its caption, beside, above or below the value.
                        let caption_node = Self::caption_node(node, &mut field_node);
                        // Add WidgetType hint if extracted
                        if let Some(kind) = widget_kind {
                            // Add Dropdown hint with options if this is a dropdown
                            if kind == WidgetKind::Dropdown
                                && let Some(dropdown_hint) = Self::extract_dropdown_hint(node)
                            {
                                field_node.add_hint(dropdown_hint);
                            }
                            if kind == WidgetKind::Barcode
                                && let Some(barcode) = Self::extract_barcode(node)
                            {
                                field_node.add_hint(Hint::Barcode(barcode));
                            }
                            field_node.add_hint(Hint::WidgetType(kind));
                        }
                        // Add NoPrint hint if relevant="-print" or inherited from parent
                        if Self::is_no_print(node) || child_ctx.has_inherited_hint(&Hint::NoPrint) {
                            field_node.add_hint(Hint::NoPrint);
                        }
                        flattened_children.push(FlattenedKind::Node(field_node));
                        if let Some(caption) = caption_node {
                            flattened_children.push(FlattenedKind::Node(caption));
                        }
                    }

                    // Don't recurse into field children for positioning
                }
                XfaNodeKind::Draw => {
                    let (outer_pos, content_pos, _layout, _) =
                        Self::compute_position_for_node_with_children(
                            node,
                            parent_position,
                            parent_layout,
                            &mut current_x,
                            &mut current_y,
                            &mut max_height_in_row,
                            flattened_children,
                            ctx,
                        )?;

                    // For positioned layout, track the max extent (relative to parent_position)
                    if parent_layout == Layout::Position {
                        let node_bottom = (outer_pos.y - parent_position.y) + outer_pos.height;
                        max_extent_y = max_extent_y.max(node_bottom);
                    }

                    // The draw's extent -- regardless of `skip_render`: an
                    // invisible node still occupies layout space and can
                    // still carry a `keep` (a section heading's `keep next`
                    // drags it onto the page of the block it introduces).
                    Self::leaf_extent(
                        node,
                        outer_pos.y,
                        outer_pos.y + outer_pos.height,
                        Self::draw_splitting(node),
                        ctx,
                    );

                    // Only add to output if not hidden
                    if !skip_render {
                        // An image draw carries pixels rather than text.
                        if let Some(image_node) = Self::flatten_value_draw(
                            node,
                            node.name.as_ref().map(|_| child_ctx.current_path.clone()),
                            content_pos,
                            &child_ctx,
                        )? {
                            flattened_children.push(FlattenedKind::Node(image_node));
                            continue;
                        }

                        // Extract text content from draw node, or use empty (scripts may fill it)
                        // Use context to resolve xfa:embed references
                        let text_content =
                            child_ctx.extract_text(&node.children).unwrap_or_default();
                        let font_size = Self::extract_font_size(node);
                        let font_name = Self::extract_font_name(node);
                        let mut style = Self::extract_style(node);
                        Self::anchor_own_border(&mut style, outer_pos);

                        // Compute default_bold/italic from original XFA font BEFORE CSS overrides.
                        // CSS font-weight from individual paragraphs should not change the default
                        // for rich text parsing - each paragraph handles its own font-weight CSS.
                        let (default_bold, default_italic) = style.get_xfa_bold_italic();

                        // Get default h_align from XFA para element
                        let default_h_align = node
                            .para
                            .as_ref()
                            .map(|p| p.h_align)
                            .unwrap_or(HAlign::Left);

                        // Extract rich text if this is HTML content (exData with contentType="text/html")
                        // This preserves paragraph structure, text-indent, and xfa-spacerun spacing
                        let rich_text = Self::extract_rich_text_from_node(
                            &node.children,
                            default_h_align,
                            Some(ctx.computed_values),
                            Some(ctx.id_to_field),
                            default_bold,
                            default_italic,
                            Decoration::of_font(node.font.as_ref()),
                            font_size,
                        );

                        let draw_node = FlattenedNode::new_text_with_rich_text(
                            text_content,
                            font_size,
                            font_name,
                            content_pos.x,
                            content_pos.y,
                            content_pos.width,
                            content_pos.height,
                            style,
                            node.rotate,
                            node.name.clone(),
                            rich_text,
                        );
                        // Split multi-paragraph draw nodes into one FlattenedNode per paragraph
                        let mut draw_kinds =
                            Self::split_draw_into_paragraph_nodes(draw_node, &child_ctx.language);
                        // Add NoPrint hint if relevant="-print" or inherited from parent
                        if Self::is_no_print(node) || child_ctx.has_inherited_hint(&Hint::NoPrint) {
                            for kind in &mut draw_kinds {
                                kind.add_hint(Hint::NoPrint);
                            }
                        }
                        // Add SomPath hint to draw nodes (same pattern as fields)
                        if !child_ctx.current_path.is_empty() {
                            let som = SomPath::new(child_ctx.current_path.clone());
                            for kind in &mut draw_kinds {
                                kind.add_hint(Hint::SomPath(som.clone()));
                            }
                        }
                        flattened_children.extend(draw_kinds);
                    }

                    // Don't recurse into draw children for positioning
                }
                XfaNodeKind::ExclGroup => {
                    // Per XFA spec (section 17 "The exclGroup element"):
                    // exclGroup is a container element with x, y, w, h, layout, and other positioning attributes.
                    // It should be treated like a subform for layout purposes - compute its position
                    // and use that as the parent position for its children (the radio button fields).
                    let (outer_pos, content_pos, layout, consumed_height) =
                        Self::compute_position_for_node_with_children(
                            node,
                            parent_position,
                            parent_layout,
                            &mut current_x,
                            &mut current_y,
                            &mut max_height_in_row,
                            flattened_children,
                            &child_ctx,
                        )?;

                    // For positioned layout, track the max extent (relative to parent_position)
                    if parent_layout == Layout::Position {
                        let node_bottom = (outer_pos.y - parent_position.y) + outer_pos.height;
                        max_extent_y = max_extent_y.max(node_bottom);
                    }

                    // Per XFA spec (section 4 "Exclusion Groups"):
                    // The exclGroup has a rawValue that determines which child field is "on".
                    // Get the exclGroup's current value to pass to children.
                    // IMPORTANT: Use full SOM path for lookup to handle duplicate names correctly
                    let exclgroup_value = if node.name.is_some() {
                        // child_ctx already has this exclGroup's name in its path
                        // (added by with_path_segment above), so use it directly
                        let full_path = &child_ctx.current_path;
                        // Look up computed_values by FULL PATH (primary lookup)
                        ctx.computed_values
                            .get(full_path.as_str())
                            .cloned()
                            // Then check the node's value child
                            .or_else(|| Self::extract_field_value(&node.children).into())
                            // Then check rawValue attribute
                            .or_else(|| node.attributes.get("rawValue").cloned())
                    } else {
                        None
                    };

                    // Create a child context with the exclGroup value for radio button checked state
                    // Also set the exclGroup's SOM path so children can reference it
                    let exclgroup_som_path = child_ctx.current_path.clone();
                    let exclgroup_ctx = {
                        let ctx_with_path =
                            child_ctx.with_exclgroup_som_path(SomPath::new(exclgroup_som_path));
                        if let Some(value) = exclgroup_value.filter(|v| !v.is_empty()) {
                            ctx_with_path.with_exclgroup_value(value)
                        } else {
                            ctx_with_path
                        }
                    };

                    // Recurse into exclGroup children with the computed content position
                    // The exclGroup's layout applies to its children (the fields)
                    let group_children_start = flattened_children.len();
                    Self::begin_extent(ctx);
                    let children_height = Self::flatten_nodes(
                        &node.children,
                        content_pos,
                        layout,
                        flattened_children,
                        &exclgroup_ctx,
                    )?;
                    // A growable group is as tall as its children, at least
                    // its `minH`; a sized one as tall as it says.
                    let grown_height = node.h.is_none().then(|| {
                        (children_height
                            + node.margin_top.unwrap_or(Decimal::ZERO)
                            + node.margin_bottom.unwrap_or(Decimal::ZERO))
                        .max(node.min_h.unwrap_or(Decimal::ZERO))
                        .max(consumed_height)
                    });

                    // For tb layout, update current_y based on actual content height if no explicit height
                    if parent_layout == Layout::TopToBottom && node.h.is_none() {
                        let actual_height = children_height
                            + node.margin_top.unwrap_or(Decimal::ZERO)
                            + node.margin_bottom.unwrap_or(Decimal::ZERO);
                        let min_h = node.min_h.unwrap_or(Decimal::ZERO);
                        let effective_height = actual_height.max(min_h).max(consumed_height);

                        if effective_height > consumed_height {
                            current_y = outer_pos.y + effective_height;
                        }
                    }

                    // For positioned layout, update max_extent_y after recursion.
                    // Per XFA spec, a container without explicit h grows to fit
                    // its children's nominal extents.
                    if parent_layout == Layout::Position && node.h.is_none() {
                        let actual_height = children_height
                            + node.margin_top.unwrap_or(Decimal::ZERO)
                            + node.margin_bottom.unwrap_or(Decimal::ZERO);
                        let effective_height = actual_height.max(consumed_height);
                        let node_bottom = (outer_pos.y - parent_position.y) + effective_height;
                        max_extent_y = max_extent_y.max(node_bottom);
                    }

                    Self::end_container_extent(
                        node,
                        parent_layout,
                        outer_pos.y,
                        outer_pos.y + grown_height.unwrap_or(outer_pos.height),
                        ctx,
                    );
                    // The group's own `<border>`, drawn under its members.
                    if let Some(border_node) = node.border.as_ref().and_then(|border| {
                        Self::container_border_node(
                            border,
                            (
                                outer_pos.x,
                                outer_pos.y,
                                outer_pos.width,
                                grown_height.unwrap_or(outer_pos.height),
                            ),
                        )
                    }) {
                        flattened_children.insert(group_children_start, border_node);
                    }
                }
                XfaNodeKind::Element { tag_name, .. } => {
                    // Handle generic elements that might be containers
                    match tag_name.as_str() {
                        "pageSet" | "pageArea" | "contentArea" => {
                            // Page structure (XFA 3.3 §7), not Form DOM
                            // content; laid out per page by
                            // `from_xfa_paged_with`.
                        }
                        "proto" | "variables" => {
                            // Not form content. A prototype "plays no part in
                            // the form except when it is referenced by a use
                            // or usehref attribute on some other element", and
                            // even a fully specified field in one "will never
                            // be directly visible or accessible to a form
                            // filling user" (XFA 3.3 §6 "Prototypes"); the
                            // references were merged into their users by
                            // `XfaNode::resolve_use_prototypes`. Document
                            // variables hold boilerplate a script inserts
                            // elsewhere ("The variables element"), drawn only
                            // where it is inserted.
                        }
                        "template" => {
                            Self::flatten_nodes(
                                &node.children,
                                parent_position,
                                parent_layout,
                                flattened_children,
                                &child_ctx,
                            )?;
                        }
                        "area" => {
                            // Per XFA spec: area elements group related objects.
                            // "Area objects grow to the minimum size required to hold the
                            //  nominal extents of all the layout objects they contain.
                            //  Area objects do not have margins or borders of their own.
                            //  [...] when an object within the area object uses positioned
                            //  layout the X and Y positions are specified relative to the
                            //  area object."
                            //
                            // Areas participate in their parent's flow layout (lr-tb, tb, etc.)
                            // but internally use positioned layout for their children.

                            // Pre-scan children to compute the area's natural dimensions.
                            // Per spec, the area grows to the minimum bounding box that
                            // holds all children's nominal extents.
                            let (area_width, area_height) =
                                Self::compute_area_dimensions(node, ctx);

                            // Area's own x/y offset (defaults to 0)
                            let area_x = node.x.unwrap_or(Decimal::ZERO);
                            let area_y = node.y.unwrap_or(Decimal::ZERO);

                            // Position the area within the parent's flow, similar to how
                            // subforms are positioned via compute_position_for_node_with_children.
                            // Areas have no margins or borders per spec.
                            let outer_pos = match parent_layout {
                                Layout::Position => Position::new(
                                    parent_position.x + area_x,
                                    parent_position.y + area_y,
                                    area_width,
                                    area_height,
                                ),
                                Layout::TopToBottom => {
                                    let pos = Position::new(
                                        parent_position.x,
                                        current_y,
                                        area_width,
                                        area_height,
                                    );
                                    current_y += area_height;
                                    pos
                                }
                                Layout::LeftToRightTopToBottom | Layout::LeftToRight => {
                                    // Check if we need to wrap to next line
                                    if current_x + area_width
                                        > parent_position.x + parent_position.width
                                        && current_x > parent_position.x
                                    {
                                        current_x = parent_position.x;
                                        current_y += max_height_in_row;
                                        max_height_in_row = Decimal::ZERO;
                                    }
                                    let pos = Position::new(
                                        current_x,
                                        current_y,
                                        area_width,
                                        area_height,
                                    );
                                    current_x += area_width;
                                    max_height_in_row = max_height_in_row.max(area_height);
                                    pos
                                }
                                Layout::RightToLeftTopToBottom => {
                                    let right_edge = parent_position.x + parent_position.width;
                                    if current_x - area_width < parent_position.x
                                        && current_x < right_edge
                                    {
                                        current_x = right_edge;
                                        current_y += max_height_in_row;
                                        max_height_in_row = Decimal::ZERO;
                                    }
                                    let pos_x = current_x - area_width;
                                    let pos =
                                        Position::new(pos_x, current_y, area_width, area_height);
                                    current_x = pos_x;
                                    max_height_in_row = max_height_in_row.max(area_height);
                                    pos
                                }
                                _ => {
                                    // Row, Table, etc. — treat like positioned
                                    Position::new(
                                        parent_position.x + area_x,
                                        parent_position.y + area_y,
                                        area_width,
                                        area_height,
                                    )
                                }
                            };

                            // For positioned layout, track the max extent
                            if parent_layout == Layout::Position {
                                let node_bottom =
                                    (outer_pos.y - parent_position.y) + outer_pos.height;
                                max_extent_y = max_extent_y.max(node_bottom);
                            }

                            // Recurse into children with the area's position as origin
                            // and Layout::Position (area's default internal layout per spec)
                            Self::begin_extent(ctx);
                            let children_height = Self::flatten_nodes(
                                &node.children,
                                outer_pos,
                                Layout::Position,
                                flattened_children,
                                &child_ctx,
                            )?;

                            // Post-recursion: if children grew beyond pre-computed height,
                            // update parent flow state (same pattern as subform growable containers)
                            let effective_height = children_height.max(area_height);
                            // An area has no margins of its own and splits
                            // where its children allow (XFA 3.3 §8: "Split
                            // consensus is employed when splitting subforms,
                            // areas, and exclusion groups").
                            if let Some(recorder) = ctx.extents {
                                recorder.borrow_mut().end(Extent::new(
                                    outer_pos.y,
                                    outer_pos.y + effective_height,
                                    Splitting::Inside {
                                        margin_top: Decimal::ZERO,
                                        margin_bottom: Decimal::ZERO,
                                    },
                                ));
                            }
                            if effective_height > area_height {
                                match parent_layout {
                                    Layout::TopToBottom => {
                                        current_y = outer_pos.y + effective_height;
                                    }
                                    Layout::LeftToRightTopToBottom | Layout::LeftToRight => {
                                        max_height_in_row = max_height_in_row.max(effective_height);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        // Skip data-only elements - these are Form DOM data, not layout
                        _ if tag_name.starts_with("xfa:")
                            || tag_name.starts_with("dd:")
                            || tag_name == "datasets"
                            || tag_name == "data"
                            || tag_name == "form" =>
                        {
                            // Skip xfa:datasets, xfa:data, form (Form DOM), etc. - they contain duplicate data
                        }
                        _ => {
                            // Other elements, recurse with current position
                            Self::flatten_nodes(
                                &node.children,
                                parent_position,
                                parent_layout,
                                flattened_children,
                                &child_ctx,
                            )?;
                        }
                    }
                }
                XfaNodeKind::Template | XfaNodeKind::ContentArea => {
                    // Not Form DOM content; pass through with the same
                    // position and layout.
                    Self::flatten_nodes(
                        &node.children,
                        parent_position,
                        parent_layout,
                        flattened_children,
                        &child_ctx,
                    )?;
                }
                XfaNodeKind::PageSet | XfaNodeKind::PageArea => {
                    // Page structure (XFA 3.3 §7): the root subform's page
                    // sets are not part of the Form DOM, and page background
                    // is laid out once per page by `from_xfa_paged_with`.
                }
                _ => {}
            }
        }

        // Return the total height consumed
        // For positioned layout, use max_extent_y (the bottom-most point of all children)
        // For flowing layouts, use current_y - start_y
        match parent_layout {
            Layout::Position => Ok(max_extent_y),
            _ => Ok(current_y - start_y + max_height_in_row),
        }
    }

    /// A node's `<keep>` (XFA 3.3 §8 "Content Splitting", "Adhesion").
    fn keep_of(node: &XfaNode) -> Keep {
        let mut keep = Keep::default();
        for child in &node.children {
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                && tag_name == "keep"
            {
                let attr = |name: &str| child.attributes.get(name).map(String::as_str);
                keep.intact = match attr("intact") {
                    Some("contentArea" | "pageArea") => Some(true),
                    Some("none") => Some(false),
                    None => None,
                    Some(other) => {
                        log::warn!(
                            "unknown keep intact={other:?} on {:?}; taken as none",
                            node.name
                        );
                        Some(false)
                    }
                };
                keep.next = matches!(attr("next"), Some("contentArea" | "pageArea"));
                keep.previous = matches!(attr("previous"), Some("contentArea" | "pageArea"));
            }
        }
        keep
    }

    /// A node's `breakBefore` and `breakAfter` requests (XFA 3.3 §7 "Break
    /// Conditions"), including the deprecated combined `<break>`. The
    /// `targetType` default is `auto` ("layout continues using the current
    /// layout container"), so a bare `<breakBefore/>` with no `targetType`
    /// -- common in Designer output, left over from reordering -- asks for
    /// nothing. A break with a script is taken: the layout cannot run it,
    /// and says so.
    fn break_requests(node: &XfaNode) -> (Option<BreakRequest>, Option<BreakRequest>) {
        let parse = |element: &XfaNode,
                     target_type: Option<&String>,
                     target: Option<&String>,
                     start_new: Option<&String>|
         -> Option<BreakRequest> {
            let target_type = match target_type.map(String::as_str) {
                None | Some("auto") => return None,
                Some("contentArea") => BreakTarget::ContentArea,
                Some("pageArea") => BreakTarget::PageArea,
                Some("pageEven") => BreakTarget::PageEven,
                Some("pageOdd") => BreakTarget::PageOdd,
                Some(other) => {
                    log::warn!(
                        "unknown break targetType {other:?} on {:?}; the break is ignored",
                        node.name
                    );
                    return None;
                }
            };
            if element.children.iter().any(|c| {
                matches!(&c.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "script")
                    && c.template_value().is_some_and(|v| !v.trim().is_empty())
            }) {
                log::warn!(
                    "a break condition script on {:?} cannot be evaluated by the layout; the \
                     break is taken",
                    node.name
                );
            }
            Some(BreakRequest {
                target_type,
                target: target.filter(|t| !t.is_empty()).cloned(),
                start_new: start_new.map(String::as_str) == Some("1"),
            })
        };
        let (mut before, mut after) = (None, None);
        for child in &node.children {
            let XfaNodeKind::Element { tag_name, .. } = &child.kind else {
                continue;
            };
            let a = &child.attributes;
            match tag_name.as_str() {
                "breakBefore" => {
                    before = before.or_else(|| {
                        parse(child, a.get("targetType"), a.get("target"), a.get("startNew"))
                    })
                }
                "breakAfter" => {
                    after = after.or_else(|| {
                        parse(child, a.get("targetType"), a.get("target"), a.get("startNew"))
                    })
                }
                "break" => {
                    before = before.or_else(|| {
                        parse(child, a.get("before"), a.get("beforeTarget"), a.get("startNew"))
                    });
                    after = after.or_else(|| parse(child, a.get("after"), a.get("afterTarget"), None));
                }
                _ => {}
            }
        }
        (before, after)
    }

    /// A container starts: its children's extents are recorded under it.
    fn begin_extent(ctx: &FlattenContext) {
        if let Some(recorder) = ctx.extents {
            recorder.borrow_mut().begin();
        }
    }

    /// A container ends, spanning `top`..`bottom` of the column. Whether it
    /// may be split (XFA 3.3 §8 "Split Restrictions"): never with `keep
    /// intact="contentArea"`; by default never as a table row, when placed
    /// by a positioned parent, or when positioned itself (Acrobat's
    /// reading, see `acrobat_compat` item 6); otherwise wherever its
    /// children allow, outside its own margins.
    fn end_container_extent(
        node: &XfaNode,
        parent_layout: Layout,
        top: Num,
        bottom: Num,
        ctx: &FlattenContext,
    ) {
        let Some(recorder) = ctx.extents else {
            return;
        };
        let keep = Self::keep_of(node);
        let own_layout = node
            .layout
            .as_ref()
            .and_then(|l| l.parse().ok())
            .unwrap_or(Layout::Position);
        let positioned = own_layout == Layout::Position
            || matches!(parent_layout, Layout::Position | Layout::Table);
        let splitting = match keep.intact {
            Some(true) => Splitting::Never,
            Some(false) => Self::inside_margins(node),
            None if positioned => Splitting::Never,
            None => Self::inside_margins(node),
        };
        let mut extent = Extent::new(top, bottom, splitting);
        Self::set_extent_requests(&mut extent, node, keep);
        recorder.borrow_mut().end(extent);
    }

    fn inside_margins(node: &XfaNode) -> Splitting {
        Splitting::Inside {
            margin_top: node.margin_top.unwrap_or(Decimal::ZERO),
            margin_bottom: node.margin_bottom.unwrap_or(Decimal::ZERO),
        }
    }

    /// A leaf (draw or field) spanning `top`..`bottom` of the column.
    /// `splitting` is what its content allows; `keep intact` overrides it.
    fn leaf_extent(node: &XfaNode, top: Num, bottom: Num, splitting: Splitting, ctx: &FlattenContext) {
        let Some(recorder) = ctx.extents else {
            return;
        };
        let keep = Self::keep_of(node);
        let splitting = if keep.intact == Some(true) {
            Splitting::Never
        } else {
            splitting
        };
        let mut extent = Extent::new(top, bottom, splitting);
        Self::set_extent_requests(&mut extent, node, keep);
        recorder.borrow_mut().leaf(extent);
    }

    fn set_extent_requests(extent: &mut Extent, node: &XfaNode, keep: Keep) {
        extent.keep_next = keep.next;
        extent.keep_previous = keep.previous;
        let (before, after) = Self::break_requests(node);
        extent.break_before = before;
        extent.break_after = after;
    }

    /// Where a draw's text may be split: a draw's `intact` defaults to
    /// `contentArea` (XFA 3.3 §8 "Split Restrictions"), so only an explicit
    /// `none` makes it splittable, and then only between its lines -- which
    /// the renderer cannot yet draw in two parts, so it is reported and the
    /// draw kept whole.
    fn draw_splitting(node: &XfaNode) -> Splitting {
        if Self::keep_of(node).intact == Some(false) {
            log::warn!(
                "draw {:?} allows splitting between its lines (keep intact=\"none\"), which is not \
                 supported; it is kept whole",
                node.name
            );
        }
        Splitting::Never
    }

    /// Where a field may be split: a widget never (XFA 3.3 §8 "Split
    /// Restrictions"); a text field between its lines by default, which the
    /// renderer cannot yet draw in two parts, so a multi-line text field is
    /// reported and kept whole.
    fn field_splitting(node: &XfaNode, height: Num) -> Splitting {
        let is_text = matches!(
            Self::extract_widget_kind(node),
            None | Some(WidgetKind::Text | WidgetKind::TextArea)
        );
        if is_text && height > Self::text_line_height(&node.font) * num(1.5) {
            log::debug!(
                "text field {:?} may split between its lines, which is not supported; it is kept \
                 whole",
                node.name
            );
        }
        Splitting::Never
    }

    /// Compute position for a node, considering its children for height calculation
    /// Returns (outer_position, content_position, layout, height_consumed)
    fn compute_position_for_node_with_children(
        node: &XfaNode,
        parent_position: Position,
        parent_layout: Layout,
        current_x: &mut Num,
        current_y: &mut Num,
        max_height_in_row: &mut Num,
        _flattened_children: &mut Vec<FlattenedKind>,
        ctx: &FlattenContext,
    ) -> Result<(Position, Position, Layout, Num), String> {
        // Check if explicit coordinates are provided
        let has_explicit_x = node.x.is_some();
        let has_explicit_y = node.y.is_some();

        // Get dimensions from node's parsed layout attributes
        let x = node.x.unwrap_or(Decimal::ZERO);
        let y = node.y.unwrap_or(Decimal::ZERO);

        // Per XFA spec: if w is not specified, the element is horizontally growable.
        // - For Draw elements: use natural text width (constrained by minW/maxW)
        // - For other elements: use minW if available, otherwise parent width
        let mut width = node.w.unwrap_or_else(|| {
            match &node.kind {
                XfaNodeKind::Draw => {
                    // Calculate natural width from text content
                    let natural_width = Self::growable_draw_width(node, ctx);
                    let min_w = node.min_w.unwrap_or(Decimal::ZERO);
                    let max_w = node.max_w;

                    // Constrain by minW and maxW
                    let width = natural_width.max(min_w);
                    if let Some(max) = max_w {
                        width.min(max)
                    } else {
                        width
                    }
                }
                // A check box or radio button has a natural width of its own
                // (XFA 3.3 §2 "Natural size of widgets"): the button's `size`
                // plus the field's left and right margins. Without it a field
                // whose `minW` only covers its left margin (ACYI_001_EN's
                // confirm box: `minW="5mm"`, `leftInset="5mm"`) is left with
                // no content width and no box to draw.
                _ if node.check_button_size().is_some() => {
                    let size = node.check_button_size().unwrap_or_default();
                    let natural_width = size
                        + node.margin_left.unwrap_or(Decimal::ZERO)
                        + node.margin_right.unwrap_or(Decimal::ZERO);
                    natural_width.max(node.min_w.unwrap_or(Decimal::ZERO))
                }
                _ => {
                    // For containers without explicit width, try natural width from children
                    // before falling back to parent width. This is important for flowing
                    // layouts (e.g. lr-tb): using full parent width can trigger an
                    // unintended wrap to the next line.
                    let min_w = node.min_w.unwrap_or(Decimal::ZERO);
                    let natural_width = if matches!(
                        parent_layout,
                        Layout::LeftToRightTopToBottom
                            | Layout::LeftToRight
                            | Layout::RightToLeftTopToBottom
                            | Layout::Row
                            | Layout::RightToLeftRow
                    ) {
                        match &node.kind {
                            XfaNodeKind::Subform | XfaNodeKind::ExclGroup => {
                                let (w, _) = Self::compute_area_dimensions(node, ctx);
                                w
                            }
                            XfaNodeKind::Element { tag_name, .. }
                                if tag_name == "subform"
                                    || tag_name == "subformSet"
                                    || tag_name == "area"
                                    || tag_name == "exclGroup" =>
                            {
                                let (w, _) = Self::compute_area_dimensions(node, ctx);
                                w
                            }
                            _ => Decimal::ZERO,
                        }
                    } else {
                        Decimal::ZERO
                    };

                    if natural_width > Decimal::ZERO {
                        natural_width.max(min_w)
                    } else {
                        node.min_w.unwrap_or(parent_position.width)
                    }
                }
            }
        });

        let table_slot = if matches!(parent_layout, Layout::Row | Layout::RightToLeftRow) {
            ctx.table_column_widths.as_ref().and_then(|widths| {
                Self::compute_table_cell_slot(
                    widths,
                    *current_x,
                    parent_position,
                    parent_layout,
                    Self::extract_col_span(node),
                )
            })
        } else {
            None
        };
        if let Some(slot) = table_slot {
            width = slot.width;
        }

        // Get margins (these define spacing between the element's edges and its content)
        // NOTE: Must be extracted before height calculation since natural height includes margins
        let margin_top = node.margin_top.unwrap_or(Decimal::ZERO);
        let margin_bottom = node.margin_bottom.unwrap_or(Decimal::ZERO);
        let margin_left = node.margin_left.unwrap_or(Decimal::ZERO);
        let margin_right = node.margin_right.unwrap_or(Decimal::ZERO);

        // Height: use explicit h, or calculate natural height for leaf nodes
        let explicit_height = node.h;
        let min_height = node.min_h.unwrap_or(Decimal::ZERO);

        // For containers without explicit height, calculate natural height
        // NOTE: For draw/field elements, natural height is content + margins
        let height = explicit_height.unwrap_or_else(|| {
            // For leaf nodes (field/draw), calculate natural height based on content
            // The natural height must include space for margins + content
            match &node.kind {
                XfaNodeKind::Draw => Self::natural_draw_height(
                    node,
                    (width - margin_left - margin_right).max(Decimal::ZERO),
                    min_height,
                    margin_top,
                    margin_bottom,
                    ctx,
                ),
                XfaNodeKind::Field => Self::field_natural_height(node).max(min_height),
                _ => {
                    // Containers: children determine height, and the
                    // parent-layout branches apply `minH` once that is known.
                    Decimal::ZERO
                }
            }
        });

        // Get layout from node's layout attribute
        // Per XFA spec: if subform has no layout attribute, it defaults to "position"
        let layout = node
            .layout
            .as_ref()
            .and_then(|l| l.parse().ok())
            .unwrap_or(Layout::Position);

        // Get anchor type for positioning (default is topLeft)
        let anchor_type = node
            .attributes
            .get("anchorType")
            .map(|s| s.as_str())
            .unwrap_or("topLeft");

        // Compute absolute position based on parent layout strategy
        let outer_pos = match parent_layout {
            Layout::Position => {
                // Position layout: children specify their own positions relative to parent
                let (adj_x, adj_y) = Self::apply_anchor_type(x, y, width, height, anchor_type);
                Position::new(
                    parent_position.x + adj_x,
                    parent_position.y + adj_y,
                    width,
                    height.max(min_height),
                )
            }
            Layout::TopToBottom => {
                // TopToBottom (tb): Stack vertically, left-aligned
                // Per XFA spec (section 8): "In this type of layout the contained object's
                // x and y properties, as well as its anchor point, are ignored."
                //
                // Elements are placed at top-left of container, then immediately below
                // the nominal extent of the previous object, aligned with left edge.

                let abs_x = parent_position.x;
                let abs_y = *current_y;

                let effective_height = height.max(min_height);
                let pos = Position::new(abs_x, abs_y, width, effective_height);

                // Advance flow position in tb layout
                *current_y = abs_y + effective_height;

                pos
            }
            Layout::LeftToRightTopToBottom | Layout::LeftToRight => {
                // Left-to-right top-to-bottom tiled layout (lr-tb)
                // Per XFA spec (section 8): "In this type of layout the contained object's
                // x and y properties, as well as its anchor point, are ignored."
                //
                // Elements flow horizontally from left to right. When an element doesn't fit
                // in the remaining width, it wraps to the next line.

                // Check if we need to wrap to next line.
                let should_wrap = *current_x + width
                    > parent_position.x + parent_position.width
                    && *current_x > parent_position.x;

                if should_wrap {
                    // Wrap to next line
                    *current_x = parent_position.x;
                    *current_y += *max_height_in_row;
                    *max_height_in_row = Decimal::ZERO;
                }

                // Position at current flow position (x and y properties are ignored)
                let pos = Position::new(*current_x, *current_y, width, height);

                // Advance flow position
                *current_x += width;
                *max_height_in_row = (*max_height_in_row).max(height);

                pos
            }
            Layout::RightToLeftTopToBottom => {
                // Right-to-left top-to-bottom tiled layout (rl-tb)
                // Per XFA spec (section 8): "In this type of layout the contained object's
                // x and y properties, as well as its anchor point, are ignored."
                //
                // Like lr-tb but objects flow from right to left instead of left to right.

                // Calculate position from right edge
                let right_edge = parent_position.x + parent_position.width;

                // Check if we need to wrap to next line
                if *current_x - width < parent_position.x && *current_x < right_edge {
                    // Wrap to next line
                    *current_x = right_edge;
                    *current_y += *max_height_in_row;
                    *max_height_in_row = Decimal::ZERO;
                }

                // Position from right, moving left
                let pos_x = *current_x - width;
                let pos = Position::new(pos_x, *current_y, width, height);
                *current_x = pos_x;
                *max_height_in_row = (*max_height_in_row).max(height);
                pos
            }
            Layout::Row => {
                // Row layout: similar to lr-tb but typically within a table row
                // In XFA tables, cells are aligned by table columns; slotting takes precedence
                // over explicit x/y on cell objects.
                if let Some(slot) = table_slot {
                    let pos = Position::new(slot.x, *current_y, slot.width, height);
                    *current_x = slot.x + slot.width;
                    *max_height_in_row = (*max_height_in_row).max(height);
                    pos
                } else if has_explicit_x || has_explicit_y {
                    Position::new(parent_position.x + x, parent_position.y + y, width, height)
                } else {
                    let pos = Position::new(*current_x, *current_y, width, height);
                    *current_x += width;
                    *max_height_in_row = (*max_height_in_row).max(height);
                    pos
                }
            }
            Layout::RightToLeftRow => {
                // Right-to-left row layout
                // In XFA tables, cells are aligned by table columns; slotting takes precedence.
                if let Some(slot) = table_slot {
                    let pos = Position::new(slot.x, *current_y, slot.width, height);
                    *current_x = slot.x;
                    *max_height_in_row = (*max_height_in_row).max(height);
                    pos
                } else if has_explicit_x || has_explicit_y {
                    Position::new(parent_position.x + x, parent_position.y + y, width, height)
                } else {
                    let pos_x = *current_x - width;
                    let pos =
                        Position::new(pos_x.max(parent_position.x), *current_y, width, height);
                    *current_x = pos_x;
                    *max_height_in_row = (*max_height_in_row).max(height);
                    pos
                }
            }
            Layout::Table => {
                // Table layout stacks children vertically; row alignment is applied inside row subforms.
                // Children occupy the full table width unless they use explicit positioned coordinates.
                let effective_width = if has_explicit_x || has_explicit_y || node.w.is_some() {
                    width
                } else {
                    parent_position.width
                };
                let pos = Position::new(parent_position.x, *current_y, effective_width, height);
                *current_y += height;
                pos
            }
        };

        // Calculate content position (inset by margins)
        // This is where children will be placed
        let content_pos = Position::new(
            outer_pos.x + margin_left,
            outer_pos.y + margin_top,
            (outer_pos.width - margin_left - margin_right).max(Decimal::ZERO),
            (outer_pos.height - margin_top - margin_bottom).max(Decimal::ZERO),
        );

        // Return the height consumed by this node
        let consumed_height = outer_pos.height;
        Ok((outer_pos, content_pos, layout, consumed_height))
    }

    /// Compute the natural dimensions of an `<area>` element by scanning its children.
    /// Per XFA spec: "Area objects grow to the minimum size required to hold the nominal
    /// extents of all the layout objects they contain."
    ///
    /// Area elements use positioned layout internally, so the bounding box is
    /// determined by the maximum (child.x + child.w, child.y + child.h) across
    /// all non-hidden children; a child without `w`/`h` counts at its
    /// natural size.
    fn compute_area_dimensions(node: &XfaNode, ctx: &FlattenContext) -> (Num, Num) {
        let mut max_right = Decimal::ZERO;
        let mut max_bottom = Decimal::ZERO;

        for child in &node.children {
            // Skip hidden/inactive children
            let presence = ctx.get_effective_presence(child);
            if presence.should_skip_layout() {
                continue;
            }

            let cx = child.x.unwrap_or(Decimal::ZERO);
            let cy = child.y.unwrap_or(Decimal::ZERO);

            // Estimate child width
            let cw = child
                .w
                .unwrap_or_else(|| child.min_w.unwrap_or(Decimal::ZERO));

            // A child without a height is growable: as tall as its content
            // (XFA 3.3 §8), at least its `minH`.
            let ch = child.h.unwrap_or_else(|| {
                let min_h = child.min_h.unwrap_or(Decimal::ZERO);
                match &child.kind {
                    XfaNodeKind::Field => Self::field_natural_height(child).max(min_h),
                    XfaNodeKind::Draw => Self::natural_draw_height(
                        child,
                        (cw - child.margin_left.unwrap_or(Decimal::ZERO)
                            - child.margin_right.unwrap_or(Decimal::ZERO))
                            .max(Decimal::ZERO),
                        min_h,
                        child.margin_top.unwrap_or(Decimal::ZERO),
                        child.margin_bottom.unwrap_or(Decimal::ZERO),
                        ctx,
                    ),
                    _ => min_h,
                }
            });

            max_right = max_right.max(cx + cw);
            max_bottom = max_bottom.max(cy + ch);
        }

        (max_right, max_bottom)
    }

    /// Apply anchor type adjustment to coordinates
    /// Per XFA spec: anchor point determines which point of the object's nominal extent
    /// is placed at the (x, y) coordinate
    fn apply_anchor_type(x: Num, y: Num, width: Num, height: Num, anchor_type: &str) -> (Num, Num) {
        let two = num(2.0);
        match anchor_type {
            "topLeft" => (x, y),
            "topCenter" => (x - width / two, y),
            "topRight" => (x - width, y),
            "middleLeft" => (x, y - height / two),
            "middleCenter" => (x - width / two, y - height / two),
            "middleRight" => (x - width, y - height / two),
            "bottomLeft" => (x, y - height),
            "bottomCenter" => (x - width / two, y - height),
            "bottomRight" => (x - width, y - height),
            _ => (x, y), // Default to topLeft
        }
    }

    /// Calculate the natural width for a text/draw element.
    /// Per XFA spec: when w is not specified, the element is horizontally growable
    /// and its width is determined by the content (natural width).
    /// The width is constrained by minW (minimum) and maxW (maximum) if specified.
    /// Width of a growable `<draw>` (one with no explicit `w`): the natural
    /// width of its text plus the node's own left and right margin insets.
    ///
    /// The insets have to be included because
    /// [`Self::compute_position_for_node_with_children`] shrinks the content box
    /// by them. Sizing the box to the bare text therefore leaves a content box
    /// *narrower* than the text, and zero-wide whenever the insets exceed it --
    /// which is how the 5mm/3mm-inset "USD" radio label disappeared entirely.
    /// One line of text in `font` (the XFA default font when `None`): its
    /// ascent plus descent (AXTE; no line gap, which does not apply to a
    /// block's last line). Nothing to measure with is reported and counts
    /// as the font size.
    fn text_line_height(font: &Option<Font>) -> Num {
        let font = font.clone().unwrap_or_default();
        match TextMeasurer::new().get_metrics_for_style(&font) {
            Ok(metrics) => metrics.text_height(),
            Err(e) => {
                log::warn!("text cannot be measured ({e}); a line is taken as its font size");
                font.size
            }
        }
    }

    /// The natural height of a growable field (XFA 3.3 §2 "Natural size of
    /// widgets", §8 "Layout for Growable Objects"): its widget -- a check
    /// button's `size`, otherwise one line of its font -- with its caption
    /// above or below it (the caption's `reserve`, or one line of the
    /// caption's font) or beside it (the taller of the two), plus the
    /// field's margins.
    fn field_natural_height(node: &XfaNode) -> Num {
        let widget = node
            .check_button_size()
            .unwrap_or_else(|| Self::text_line_height(&node.font));
        let content = match crate::xfa::caption::Caption::of(node) {
            Some(caption) => {
                let caption_font = caption.font.clone().or_else(|| node.font.clone());
                let caption_line = Self::text_line_height(&caption_font);
                match caption.placement {
                    CaptionPlacement::Top | CaptionPlacement::Bottom => {
                        widget + caption.reserve.unwrap_or(caption_line)
                    }
                    _ => widget.max(caption_line),
                }
            }
            None => widget,
        };
        content + node.margin_top.unwrap_or(Decimal::ZERO) + node.margin_bottom.unwrap_or(Decimal::ZERO)
    }

    /// The natural width of a growable `draw` (XFA 3.3 §8 "Layout for
    /// Growable Objects"): its text measured in its font on one line, plus
    /// its paragraph's and its own margins. The caller clamps to `minW` and
    /// `maxW`.
    fn growable_draw_width(node: &XfaNode, ctx: &FlattenContext) -> Num {
        // Rich text is measured run by run: a bold run is wider than the
        // same words in the draw's own face, and a width taken from the
        // plain text alone would wrap it onto a second line when drawn.
        let text_width = match Self::draw_rich_text(node, ctx) {
            Some((rich_text, font)) => {
                Self::rich_text_natural_width(&rich_text, &font, &node.para, &ctx.language)
            }
            None => {
                let text = ctx.extract_text(&node.children).unwrap_or_default();
                Self::measured_text_width(&text, &node.font)
            }
        };
        text_width
            + node.para.as_ref().and_then(|p| p.margin_left).unwrap_or_default()
            + node.para.as_ref().and_then(|p| p.margin_right).unwrap_or_default()
            + node.margin_left.unwrap_or(Decimal::ZERO)
            + node.margin_right.unwrap_or(Decimal::ZERO)
    }

    /// `text` on one line in `font` (the XFA default font when `None`),
    /// measured with the font's letter spacing, kerning and horizontal
    /// scale. Nothing to measure with is reported and counts as no width.
    fn measured_text_width(text: &str, font: &Option<Font>) -> Num {
        let font = font.clone().unwrap_or_default();
        match TextMeasurer::new().measure_text_width_styled(text, &font) {
            Ok(width) => width,
            Err(e) => {
                log::warn!("text cannot be measured ({e}); {text:?} is given no width");
                Decimal::ZERO
            }
        }
    }

    /// Compute final height from line count, font size, and paragraph settings.
    /// Per AXTE: FH = MT + (num_lines * line_height) + MB, with line gap removed on last line.
    /// Natural height of a growable `draw` element (no explicit `h`): rich text
    /// (HTML exData with multiple `<p>`) is measured by `calculate_rich_text_draw_height`,
    /// everything else falls back to `calculate_natural_text_height_with_paragraphs`.
    ///
    /// Both are measured at `content_width` — the width left after the draw's
    /// own margin insets — because that is the width `content_pos` uses when
    /// the text is actually wrapped and drawn (see the `content_pos` computed
    /// a few lines below this function's only two call sites). Measuring at
    /// the outer width instead wraps fewer lines than are actually drawn and
    /// under-sizes the element.
    fn natural_draw_height(
        node: &XfaNode,
        content_width: Num,
        min_height: Num,
        margin_top: Num,
        margin_bottom: Num,
        ctx: &FlattenContext,
    ) -> Num {
        let natural_content_height = if Self::has_html_exdata(&node.children) {
            Self::calculate_rich_text_draw_height(node, content_width, ctx)
        } else {
            None
        }
        .unwrap_or_else(|| {
            if let Some(text) = ctx.extract_text(&node.children) {
                let paragraph_count = if Self::has_html_exdata(&node.children) {
                    Self::count_html_paragraphs(&node.children)
                } else {
                    0
                };
                Self::calculate_natural_text_height_with_paragraphs(
                    &text,
                    &node.font,
                    &node.para,
                    content_width,
                    paragraph_count,
                )
            } else {
                // No text: no content height. The margins and `minH` still
                // apply.
                Decimal::ZERO
            }
        });
        (natural_content_height + margin_top + margin_bottom).max(min_height)
    }

    /// Calculate the natural height for a text/draw element based on AXTE rules.
    /// This is used when no explicit height is specified for a draw element.
    /// Pass `paragraph_count > 0` to account for HTML paragraphs that add line breaks.
    ///
    /// `measure_text_block`'s `total_height` already is the AXTE full height
    /// (per-line ascent+descent+line gap, `spaceAbove`/`spaceBelow` on the
    /// first/last line, line gap removed on the block's last line) — this
    /// returns it directly rather than re-deriving a cruder height from just
    /// the line count, so a draw's natural height matches what is actually
    /// drawn. `paragraph_count`/inline breaks are lines the flattened `text`
    /// string lost (HTML `<p>` boundaries and `\n`/U+2029, both collapsed to
    /// spaces by the wrapper), so each adds one more derived line spacing.
    fn calculate_natural_text_height_with_paragraphs(
        text: &str,
        font: &Option<Font>,
        para: &Option<Para>,
        max_width: Num,
        paragraph_count: usize,
    ) -> Num {
        let xfa_font = font.clone().unwrap_or_default();
        let font_size = xfa_font.size;
        let mut measurer = TextMeasurer::new();

        let extra_lines = paragraph_count.saturating_sub(1)
            + text.matches('\n').count()
            + text.matches('\u{2029}').count();

        match measurer.measure_text_block(text, &Some(xfa_font.clone()), para, max_width) {
            Ok(block_metrics) if extra_lines == 0 => block_metrics.total_height,
            Ok(block_metrics) => {
                let extra_line_height = para
                    .as_ref()
                    .and_then(|p| p.line_height)
                    .unwrap_or_else(|| {
                        measurer
                            .get_metrics_for_style(&xfa_font)
                            .map(|m| m.derived_line_spacing())
                            .unwrap_or(font_size * num(1.2))
                    });
                block_metrics.total_height + num(extra_lines as f64) * extra_line_height
            }
            Err(e) => {
                // Nothing to measure with: one line of the font size, said so.
                log::warn!("text cannot be measured ({e}); its height is taken as one line");
                let margin_top = para.as_ref().and_then(|p| p.space_above).unwrap_or(Decimal::ZERO);
                let margin_bottom = para.as_ref().and_then(|p| p.space_below).unwrap_or(Decimal::ZERO);
                margin_top + font_size + margin_bottom
            }
        }
    }


    /// Calculate the natural content height for a draw element that has rich text
    /// (HTML exData with multiple `<p>` paragraphs at potentially different font sizes).
    ///
    /// Delegates to `rich_paragraph_heights`, the same per-paragraph measurement
    /// `split_draw_into_paragraph_nodes` uses to place the paragraphs, so a
    /// draw's natural height can never disagree with where its own content
    /// actually lands. `None` when the draw has no rich text.
    fn calculate_rich_text_draw_height(
        node: &XfaNode,
        max_width: Num,
        ctx: &FlattenContext,
    ) -> Option<Num> {
        let (rich_text, xfa_font) = Self::draw_rich_text(node, ctx)?;
        let paragraph_heights = Self::rich_paragraph_heights(
            &rich_text,
            &xfa_font,
            &node.para,
            max_width,
            &ctx.language,
        );
        Some(paragraph_heights.iter().copied().sum())
    }

    /// A draw's rich text (HTML `exData`) with the font its runs inherit:
    /// the draw's own `<font>`, or the default font when it has none.
    /// `None` when the draw holds no rich text.
    fn draw_rich_text(node: &XfaNode, ctx: &FlattenContext) -> Option<(RichText, Font)> {
        let default_h_align = node
            .para
            .as_ref()
            .map(|p| p.h_align)
            .unwrap_or(HAlign::Left);
        let xfa_font = node.font.clone().unwrap_or_default();
        let rich_text = Self::extract_rich_text_from_node(
            &node.children,
            default_h_align,
            Some(ctx.computed_values),
            Some(ctx.id_to_field),
            xfa_font.weight == crate::xfa::FontWeight::Bold,
            xfa_font.posture == crate::xfa::FontPosture::Italic,
            Decoration::of_font(node.font.as_ref()),
            xfa_font.size,
        )?;
        Some((rich_text, xfa_font))
    }

    /// The width of a rich-text block laid out on unbounded lines (XFA 3.3
    /// §8 "Layout for Growable Objects": a draw with no `w` is as wide as
    /// its text): the widest paragraph line, measured in the faces its runs
    /// are drawn in, with the line's indent and the paragraph's CSS margins.
    /// A block no face can be loaded for has no width.
    fn rich_text_natural_width(
        rich_text: &RichText,
        xfa_font: &Font,
        default_para: &Option<Para>,
        language: &str,
    ) -> Num {
        // Wide enough that no paragraph ever wraps.
        const UNBOUNDED: Num = Decimal::from_parts(1_000_000_000, 0, 0, false, 0);
        Self::rich_paragraph_lines(rich_text, xfa_font, default_para, UNBOUNDED, language)
            .iter()
            .flat_map(|paragraph| paragraph.lines.iter().flatten())
            .map(|line| {
                let indent = if line.is_first_line { line.text_indent } else { 0.0 };
                let margin_left = line.margin_left.unwrap_or(0.0);
                num((line.content_width + indent + margin_left) as f64)
            })
            .max()
            .unwrap_or(Decimal::ZERO)
    }

    fn extract_col_span(node: &XfaNode) -> i32 {
        node.attributes
            .get("colSpan")
            .or_else(|| node.attributes.get("colspan"))
            .and_then(|s| s.trim().parse::<i32>().ok())
            .unwrap_or(1)
    }

    fn compute_table_cell_slot(
        widths: &[Num],
        current_x: Num,
        parent_position: Position,
        parent_layout: Layout,
        raw_col_span: i32,
    ) -> Option<TableCellSlot> {
        if widths.is_empty() {
            return None;
        }

        let mut col_index = 0usize;
        match parent_layout {
            Layout::Row => {
                let mut cursor = parent_position.x;
                while col_index < widths.len() && cursor < current_x {
                    cursor += widths[col_index];
                    col_index += 1;
                }
            }
            Layout::RightToLeftRow => {
                let mut cursor = parent_position.x + parent_position.width;
                while col_index < widths.len() && cursor > current_x {
                    cursor -= widths[col_index];
                    col_index += 1;
                }
            }
            _ => return None,
        }

        if col_index >= widths.len() {
            return None;
        }

        let span = if raw_col_span == -1 {
            widths.len().saturating_sub(col_index).max(1)
        } else if raw_col_span <= 0 {
            1
        } else {
            raw_col_span as usize
        };
        let end = (col_index + span).min(widths.len());
        let slot_width = widths[col_index..end]
            .iter()
            .copied()
            .fold(Decimal::ZERO, |acc, w| acc + w);

        if slot_width <= Decimal::ZERO {
            return None;
        }

        let slot_x = match parent_layout {
            Layout::Row => {
                parent_position.x
                    + widths[..col_index]
                        .iter()
                        .copied()
                        .fold(Decimal::ZERO, |acc, w| acc + w)
            }
            Layout::RightToLeftRow => {
                parent_position.x + parent_position.width
                    - widths[..end]
                        .iter()
                        .copied()
                        .fold(Decimal::ZERO, |acc, w| acc + w)
            }
            _ => unreachable!(),
        };

        Some(TableCellSlot {
            x: slot_x,
            width: slot_width,
        })
    }

    fn parse_column_width_tokens(column_widths: &str) -> Vec<Option<Num>> {
        column_widths
            .split_whitespace()
            .map(|token| {
                if token == "-1" {
                    None
                } else {
                    XfaNode::parse_dimension(token).ok()
                }
            })
            .collect()
    }

    fn collect_row_subforms<'n>(nodes: &'n [XfaNode], out: &mut Vec<&'n XfaNode>) {
        for node in nodes {
            if !node.kind.is_subform() {
                continue;
            }

            let layout = node
                .layout
                .as_ref()
                .and_then(|l| l.parse().ok())
                .unwrap_or(Layout::Position);

            if layout == Layout::Row || layout == Layout::RightToLeftRow {
                out.push(node);
                continue;
            }

            // Nested tables define their own width context.
            if layout == Layout::Table {
                continue;
            }

            Self::collect_row_subforms(&node.children, out);
        }
    }

    fn estimate_natural_cell_width(node: &XfaNode, ctx: &FlattenContext) -> Num {
        if let Some(w) = node.w {
            return w;
        }
        match &node.kind {
            XfaNodeKind::Draw => {
                Self::growable_draw_width(node, ctx).max(node.min_w.unwrap_or(Decimal::ZERO))
            }
            _ => node.min_w.unwrap_or(Decimal::ZERO),
        }
    }

    fn resolve_table_column_widths(node: &XfaNode, ctx: &FlattenContext) -> Vec<Num> {
        let parsed_widths_seed = node
            .attributes
            .get("columnWidths")
            .map(|s| Self::parse_column_width_tokens(s))
            .unwrap_or_default();

        let mut rows = Vec::new();
        Self::collect_row_subforms(&node.children, &mut rows);

        let mut col_count = parsed_widths_seed.len();
        for row in &rows {
            let mut col = 0usize;
            for cell in &row.children {
                let raw_span = Self::extract_col_span(cell);
                let span = if raw_span == -1 {
                    parsed_widths_seed.len().saturating_sub(col).max(1)
                } else if raw_span > 0 {
                    raw_span as usize
                } else {
                    1
                };
                col += span;
                if raw_span == -1 {
                    break;
                }
            }
            col_count = col_count.max(col);
        }

        let mut parsed_widths = parsed_widths_seed;
        col_count = col_count.max(parsed_widths.len());
        if col_count == 0 {
            return Vec::new();
        }

        if parsed_widths.len() < col_count {
            parsed_widths.resize(col_count, None);
        }

        let mut widest_auto_cols = vec![Decimal::ZERO; col_count];
        for row in &rows {
            let mut col = 0usize;
            for cell in &row.children {
                if col >= col_count {
                    break;
                }
                let raw_span = Self::extract_col_span(cell);
                let span = if raw_span == -1 {
                    col_count.saturating_sub(col).max(1)
                } else if raw_span <= 0 {
                    1
                } else {
                    raw_span as usize
                };
                let end = (col + span).min(col_count);
                let natural_width = Self::estimate_natural_cell_width(cell, ctx);
                let per_col = natural_width / Decimal::from((end - col) as i64);
                for w in &mut widest_auto_cols[col..end] {
                    *w = (*w).max(per_col);
                }
                col = end;
                if raw_span == -1 {
                    break;
                }
            }
        }

        parsed_widths
            .into_iter()
            .enumerate()
            .map(|(idx, width)| {
                width.unwrap_or_else(|| {
                    let w = widest_auto_cols[idx];
                    // An auto column is as wide as its widest cell's content
                    // (XFA 3.3 §17 `columnWidths`, -1): nothing to measure, no width.
                    w
                })
            })
            .collect()
    }

    fn extract_field_value(children: &[XfaNode]) -> String {
        for child in children {
            if matches!(child.kind, XfaNodeKind::Value) {
                // Look for text content in value node's children
                for value_child in &child.children {
                    if let XfaNodeKind::Text { content } = &value_child.kind {
                        return content.clone();
                    }
                    if let XfaNodeKind::Element { text_content, .. } = &value_child.kind
                        && let Some(text) = text_content
                    {
                        return text.clone();
                    }
                }
            }
        }
        String::new()
    }

    /// The `<image>` inside a node's `<value>`, decoded from base64 here at the
    /// edge so that nothing downstream has to carry a string that might not be
    /// an image.
    ///
    /// Returns `Ok(None)` when the node's value is not an image at all, and an
    /// error only when it says it is one and the payload will not decode.
    fn extract_value_image(node: &XfaNode) -> Result<Option<EmbeddedImage>, String> {
        for child in &node.children {
            if !matches!(child.kind, XfaNodeKind::Value) {
                continue;
            }
            for value_child in &child.children {
                let XfaNodeKind::Element {
                    tag_name,
                    text_content,
                } = &value_child.kind
                else {
                    continue;
                };
                if tag_name != "image" {
                    continue;
                }

                let content_type = value_child
                    .attributes
                    .get("contentType")
                    .cloned()
                    .unwrap_or_else(|| "application/octet-stream".to_string());
                let aspect = value_child
                    .attributes
                    .get("aspect")
                    .map(|a| a.parse().unwrap_or_default())
                    .unwrap_or_default();

                // An href-only image points outside the document; there is
                // nothing embedded to draw.
                let Some(payload) = text_content.as_deref().or_else(|| {
                    value_child.children.iter().find_map(|c| match &c.kind {
                        XfaNodeKind::Text { content } => Some(content.as_str()),
                        _ => None,
                    })
                }) else {
                    return Ok(None);
                };

                let encoding = value_child
                    .attributes
                    .get("transferEncoding")
                    .map(|s| s.as_str())
                    .unwrap_or("base64");
                if encoding != "base64" {
                    return Err(format!(
                        "image {}: transferEncoding {encoding:?} is not supported (only base64)",
                        node.name.as_deref().unwrap_or("<unnamed>")
                    ));
                }

                let stripped: String = payload
                    .chars()
                    .filter(|c| !c.is_ascii_whitespace())
                    .collect();
                if stripped.is_empty() {
                    return Ok(None);
                }
                let data = base64::engine::general_purpose::STANDARD
                    .decode(&stripped)
                    .map_err(|e| {
                        format!(
                            "image {}: payload is not valid base64: {e}",
                            node.name.as_deref().unwrap_or("<unnamed>")
                        )
                    })?;

                return Ok(Some(EmbeddedImage {
                    data: Arc::new(data),
                    content_type,
                    aspect,
                    source_name: node.name.clone(),
                }));
            }
        }
        Ok(None)
    }

    /// The shape a draw's `<value>` holds, if it holds a `<line>`,
    /// `<rectangle>` or `<arc>`.
    fn extract_value_shape(node: &XfaNode) -> Result<Option<crate::xfa::shapes::Shape>, String> {
        for child in &node.children {
            if !matches!(child.kind, XfaNodeKind::Value) {
                continue;
            }
            for value_child in &child.children {
                if let Some(shape) = crate::xfa::shapes::Shape::parse(value_child).map_err(|e| {
                    format!("draw {}: {e}", node.name.as_deref().unwrap_or("<unnamed>"))
                })? {
                    return Ok(Some(shape));
                }
            }
        }
        Ok(None)
    }

    /// A `<draw>` whose value is an embedded image or a shape becomes one
    /// image or shape node.
    ///
    /// Returns `Ok(None)` for an ordinary text draw, so a caller can fall
    /// through to the text path.
    fn flatten_value_draw(
        node: &XfaNode,
        som_path: Option<String>,
        pos: Position,
        ctx: &FlattenContext,
    ) -> Result<Option<FlattenedNode>, String> {
        let builder = FlattenedNode::builder()
            .bounds(pos.x, pos.y, pos.width, pos.height)
            .style(Self::extract_style(node))
            .rotate(node.rotate);
        let builder = if let Some(image) = Self::extract_value_image(node)? {
            builder.image(image)
        } else if let Some(shape) = Self::extract_value_shape(node)? {
            builder.shape(shape)
        } else {
            return Ok(None);
        };
        let mut image_node = builder.build();

        if Self::is_no_print(node) || ctx.has_inherited_hint(&Hint::NoPrint) {
            image_node.add_hint(Hint::NoPrint);
        }
        if let Some(path) = som_path {
            image_node.add_hint(Hint::SomPath(SomPath::new(path)));
        }
        Ok(Some(image_node))
    }

    fn extract_text_content(children: &[XfaNode]) -> Option<String> {
        // Use empty context for backward compatibility
        Self::extract_text_content_with_embed(children, &HashMap::new(), &HashMap::new())
    }

    /// Whether a check button field is on, `None` for a field that is not
    /// a check button. A field is on when its value is its on value (XFA
    /// 3.3 §17, see [`XfaNode::check_button_values`]); an exclusion group
    /// member's value is its group's (§17 "The exclGroup element": each
    /// member is on if and only if the group's value is its on value). A
    /// standalone box's value is the form's current one under the field's
    /// full path, when the scripts hold a non-empty one, else the
    /// template's.
    fn checked_state(
        node: &XfaNode,
        path: &str,
        template_value: &str,
        parent_exclgroup_value: &Option<String>,
        ctx: &FlattenContext,
    ) -> Option<bool> {
        node.check_button_size()?;
        let on = node.check_button_values().on;
        if let Some(group_value) = parent_exclgroup_value {
            return Some(*group_value == on);
        }
        let value = ctx
            .computed_values
            .get(path)
            .filter(|v| !v.is_empty())
            .map(String::as_str)
            .unwrap_or(template_value);
        Some(value == on)
    }

    /// Extract text content with xfa:embed resolution support
    ///
    /// Parameters:
    /// - `children`: The node's children to extract text from
    /// - `computed_values`: Map of field name -> computed value
    /// - `id_to_field`: Map of element ID -> field name for resolving embed URIs
    fn extract_text_content_with_embed(
        children: &[XfaNode],
        computed_values: &HashMap<SomPath, String>,
        id_to_field: &HashMap<String, String>,
    ) -> Option<String> {
        for child in children {
            // Check for XfaNodeKind::Value
            if matches!(child.kind, XfaNodeKind::Value)
                && let Some(text) = Self::extract_value_text_with_embed(
                    &child.children,
                    computed_values,
                    id_to_field,
                )
            {
                return Some(text);
            }
            // Also check for Element with tag_name "value" (when parsed via parse_element_content)
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                && tag_name == "value"
                && let Some(text) = Self::extract_value_text_with_embed(
                    &child.children,
                    computed_values,
                    id_to_field,
                )
            {
                return Some(text);
            }
            if let XfaNodeKind::Text { content } = &child.kind {
                return Some(content.clone());
            }
        }
        None
    }

    /// Extract text from value node's children (handles both text and exData with HTML)
    #[inline]
    fn extract_value_text(children: &[XfaNode]) -> Option<String> {
        Self::extract_value_text_with_embed(children, &HashMap::new(), &HashMap::new())
    }

    /// Extract text from value node's children with xfa:embed resolution
    fn extract_value_text_with_embed(
        children: &[XfaNode],
        computed_values: &HashMap<SomPath, String>,
        id_to_field: &HashMap<String, String>,
    ) -> Option<String> {
        for value_child in children {
            if let XfaNodeKind::Text { content } = &value_child.kind {
                return Some(content.clone());
            }
            if let XfaNodeKind::Element {
                tag_name,
                text_content,
            } = &value_child.kind
            {
                if tag_name == "text"
                    && let Some(text) = text_content
                {
                    return Some(text.clone());
                }
                // Handle exData with HTML content - extract plain text from it
                if tag_name == "exData" {
                    // Try to extract text from HTML body with embed resolution
                    if let Some(plain_text) = Self::extract_text_from_exdata_with_embed(
                        &value_child.children,
                        computed_values,
                        id_to_field,
                    ) {
                        return Some(plain_text);
                    }
                    // Fallback to text_content if available
                    if let Some(text) = text_content {
                        return Some(text.clone());
                    }
                }
            }
        }
        None
    }

    /// Extract plain text from exData HTML content
    #[inline]
    fn extract_text_from_exdata(children: &[XfaNode]) -> Option<String> {
        Self::extract_text_from_exdata_with_embed(children, &HashMap::new(), &HashMap::new())
    }

    /// Extract plain text from exData HTML content with xfa:embed resolution
    fn extract_text_from_exdata_with_embed(
        children: &[XfaNode],
        computed_values: &HashMap<SomPath, String>,
        id_to_field: &HashMap<String, String>,
    ) -> Option<String> {
        let mut text_parts = Vec::new();
        Self::collect_text_recursive_with_embed(
            children,
            &mut text_parts,
            computed_values,
            id_to_field,
        );
        if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join(""))
        }
    }

    /// Recursively collect text content from nested elements
    #[inline]
    fn collect_text_recursive(children: &[XfaNode], text_parts: &mut Vec<String>) {
        Self::collect_text_recursive_with_embed(
            children,
            text_parts,
            &HashMap::new(),
            &HashMap::new(),
        );
    }

    /// Recursively collect text content from nested elements with xfa:embed resolution
    fn collect_text_recursive_with_embed(
        children: &[XfaNode],
        text_parts: &mut Vec<String>,
        computed_values: &HashMap<SomPath, String>,
        id_to_field: &HashMap<String, String>,
    ) {
        for child in children {
            match &child.kind {
                XfaNodeKind::Text { content } => {
                    let trimmed = content.trim();
                    if !trimmed.is_empty() {
                        // Ensure word-boundary separation between consecutive
                        // non-space text segments.  With trim_text(true), the
                        // XML reader strips whitespace that originally
                        // separated adjacent text and element nodes; this
                        // restores a single space so words don't get fused.
                        if !text_parts.is_empty() {
                            if let Some(last) = text_parts.last() {
                                if !last.is_empty() && !last.ends_with(' ') && !last.ends_with('\n')
                                {
                                    text_parts.push(" ".to_string());
                                }
                            }
                        }
                        text_parts.push(trimmed.to_string());
                    } else if content.contains(' ') && !text_parts.is_empty() {
                        // Preserve a single space between inline elements
                        // (e.g., between xfa:embed spans in draw node HTML).
                        // Pure indentation (only newlines/tabs) is still dropped.
                        text_parts.push(" ".to_string());
                    }
                }
                XfaNodeKind::Element {
                    tag_name,
                    text_content,
                } => {
                    // Check for xfa:embed attribute (span elements with embedded references)
                    if let Some(embed_ref) = child.attributes.get("xfa:embed") {
                        // Resolve the embedded reference
                        if let Some(resolved_text) =
                            Self::resolve_embed_reference(embed_ref, computed_values, id_to_field)
                        {
                            text_parts.push(resolved_text);
                            continue; // Don't recurse into embed spans - they're empty
                        }
                    }

                    // When children include interleaved Text nodes (created
                    // by the parser to preserve DOM order), skip the
                    // aggregated text_content and rely on the children
                    // instead.  Otherwise fall back to text_content.
                    let has_text_children = child
                        .children
                        .iter()
                        .any(|c| matches!(&c.kind, XfaNodeKind::Text { .. }));

                    if !has_text_children {
                        // Add text content if present
                        if let Some(text) = text_content {
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                text_parts.push(trimmed.to_string());
                            } else if text.contains(' ') && !text_parts.is_empty() {
                                // Preserve a single space between inline elements
                                text_parts.push(" ".to_string());
                            }
                        }
                    }
                    // Add space/newline for paragraph breaks
                    if (tag_name == "p" || tag_name == "br") && !text_parts.is_empty() {
                        text_parts.push(" ".to_string());
                    }
                    // Recurse into children
                    Self::collect_text_recursive_with_embed(
                        &child.children,
                        text_parts,
                        computed_values,
                        id_to_field,
                    );
                }
                _ => {
                    // Recurse into other node types
                    Self::collect_text_recursive_with_embed(
                        &child.children,
                        text_parts,
                        computed_values,
                        id_to_field,
                    );
                }
            }
        }
    }

    /// The value an `xfa:embed` reference stands for (XFA 3.3 "Rich Text
    /// That Contains External Objects"): a `#id` fragment names a node by
    /// its `id`, which `id_to_field` maps to the node's SOM path; anything
    /// else is taken as the SOM path itself. The value is read under that
    /// path only.
    fn resolve_embed_reference(
        embed_ref: &str,
        computed_values: &HashMap<SomPath, String>,
        id_to_field: &HashMap<String, String>,
    ) -> Option<String> {
        let path = match embed_ref.strip_prefix('#') {
            Some(id) => id_to_field.get(id)?.as_str(),
            None => embed_ref,
        };
        computed_values.get(path).cloned()
    }

    /// Render the flattened layout to an image file
    /// Pass 1: Draw actual content (text in black, field boxes)
    /// Pass 2: Overlay debug info in transparent red (names, outlines)
    ///
    /// Per XFA spec, font rendering respects:
    /// - typeface: Font family name (default: Courier)
    /// - size: Font size in points (default: 10pt)
    /// - weight: normal or bold (default: normal)
    /// - posture: normal or italic (default: normal)
    pub fn render_to_image<P: AsRef<Path>>(
        &self,
        output_path: P,
        scale: f32,
    ) -> Result<(), String> {
        let img = self.render_to_image_buffer(scale)?;
        img.save(output_path.as_ref())
            .map_err(|e| format!("Failed to save image: {}", e))?;
        Ok(())
    }

    /// Render the flattened layout to an image buffer (for compositing)
    ///
    /// Returns the rendered image without saving to disk. This is useful for
    /// compositing additional overlays (e.g., group annotations in Document).
    /// This includes both the actual content and red debug annotations.
    pub fn render_to_image_buffer(&self, scale: f32) -> Result<RgbaImage, String> {
        // Start with the plain rendering (PASS 1)
        let mut img = self.render_to_image_buffer_plain(scale)?;

        // Get the scale and dimensions for PASS 2
        let scale_dec = num(scale as f64);

        // Get a default fallback font for debug text
        let fallback_font = Self::load_fallback_font()?;

        // Colors for debug overlay
        let debug_red = Rgba([255u8, 0u8, 0u8, 180u8]); // More visible red for field names
        let debug_red_outline = Rgba([255u8, 0u8, 0u8, 20u8]);

        // ============================================
        // PASS 2: Draw debug overlay in red
        // ============================================
        for node in self.iter_nodes() {
            // Handle rotation: for 90/270 degrees, we swap width/height and adjust position
            let (x, y, w, h) = Self::apply_rotation_to_bounds(
                node.x,
                node.y,
                node.width,
                node.height,
                node.rotate,
                scale_dec,
            );

            if x < 0 || y < 0 || w <= 0 || h <= 0 {
                continue;
            }

            // Draw debug outline with transparency (blend with existing pixels)
            Self::draw_transparent_rect(&mut img, x, y, w, h, debug_red_outline);

            // Draw debug name label
            let debug_name = match &node.kind {
                FlattenedNodeKind::Field { name, .. } => name.clone(),
                FlattenedNodeKind::Text { .. } => "Text".to_string(),
                FlattenedNodeKind::Image(_) => "Image".to_string(),
                FlattenedNodeKind::Shape(_) => "Shape".to_string(),
            };

            let debug_font_size = (8.0 * scale).max(6.0);
            let debug_scale = PxScale::from(debug_font_size);

            // Draw name in transparent red at top-left of box (using fallback font)
            draw_text_mut(
                &mut img,
                debug_red,
                x + 1,
                y + 1,
                debug_scale,
                &fallback_font,
                &debug_name,
            );
        }

        Ok(img)
    }

    /// Slice a full (all-pages-stacked) render into one image per page.
    ///
    /// The renderers stack every page into a single tall buffer in a cumulative
    /// Y coordinate space; the layout makes that buffer exactly
    /// [`Page::page_count`] pages tall, so a page is the band
    /// `[k * height, (k+1) * height)` and this is the crop. Page images are far
    /// shorter than the combined buffer, which keeps them under the vision
    /// API's per-image size limit.
    pub fn slice_into_pages(&self, full: RgbaImage, scale: f32) -> Vec<RgbaImage> {
        let scale_dec = num(scale as f64);
        let full_w = full.width();
        let full_h = full.height();
        let px = |v: Num| (v * scale_dec).to_f32().unwrap_or(0.0).round() as u32;

        if self.page.pages.len() <= 1 {
            return vec![full];
        }

        // Build vertical bands of one page each, clipped to the buffer: a
        // buffer shorter than the layout claims still yields whole pages up to
        // where it ends rather than an out-of-range crop.
        let mut bands: Vec<(u32, u32)> = Vec::with_capacity(self.page.pages.len());
        for slot in &self.page.pages {
            let top = px(slot.top);
            if top >= full_h {
                break;
            }
            let bottom = px(slot.top + slot.height).min(full_h);
            if bottom > top {
                bands.push((top, bottom));
            }
        }

        if bands.len() <= 1 {
            return vec![full];
        }

        bands
            .into_iter()
            .map(|(y0, y1)| image::imageops::crop_imm(&full, 0, y0, full_w, y1 - y0).to_image())
            .collect()
    }

    /// Render the flattened layout to an image buffer without debug annotations (plain mode)
    ///
    /// Returns the rendered image without red debug overlays, only showing the actual content.
    pub fn render_to_image_buffer_plain(&self, scale: f32) -> Result<RgbaImage, String> {
        self.render_to_image_buffer_with(scale, RenderOptions::default())
    }

    /// As [`Self::render_to_image_buffer_plain`], with viewer chrome chosen
    /// by `options`.
    pub fn render_to_image_buffer_with(
        &self,
        scale: f32,
        options: RenderOptions,
    ) -> Result<RgbaImage, String> {
        // Text is drawn in the document's fonts, as it was measured.
        let _document_fonts = self.fonts.enter();
        // Scale dimensions for better resolution (e.g., scale=2.0 for 2x)
        let scale_dec = num(scale as f64);

        // Width is the widest page's
        let img_width = (self.page.column_width() * scale_dec).to_f32().unwrap_or(0.0) as u32;

        // Height is the whole page stack, not the content extent: the last
        // page is a whole page even when its content stops early, and anything
        // hanging past the final page edge is off the page, exactly as it would
        // be in a viewer.
        let img_height = (self.page.column_height() * scale_dec)
            .to_f32()
            .unwrap_or(0.0)
            .round() as u32;

        // Create a white background image (RGBA for transparency support)
        let mut img: RgbaImage =
            ImageBuffer::from_pixel(img_width, img_height, Rgba([255u8, 255u8, 255u8, 255u8]));

        // Get the font manager for font resolution
        let font_manager = get_font_manager();

        // Get a default fallback font
        let fallback_font = Self::load_fallback_font()?;

        // Text without a fill of its own "is drawn with a solid black fill"
        // (XFA 3.3 "The fill element").
        let black = Rgba([0u8, 0u8, 0u8, 255u8]);

        // The hyphenation dictionary of the document's locale; a node under
        // a container with its own `locale` uses that one instead, as the
        // height calculation in split_draw_into_paragraph_nodes did.
        let document_dict = super::xfa::hyphenation::dict_for_language(&self.language);

        // ============================================
        // Draw actual content (as in PDF) - no debug overlay
        // ============================================
        for node in self.iter_nodes() {
            let hyph_dict = match &node.locale {
                Some(locale) if *locale != self.language => {
                    super::xfa::hyphenation::dict_for_language(locale)
                }
                _ => document_dict,
            };
            // Handle rotation: for 90/270 degrees, we swap width/height and adjust position
            // Per XFA spec: rotation is counter-clockwise about anchor point
            let (x, y, w, h) = Self::apply_rotation_to_bounds(
                node.x,
                node.y,
                node.width,
                node.height,
                node.rotate,
                scale_dec,
            );

            // Skip nodes outside the visible area or with invalid dimensions.
            // A shape may have one zero dimension: a zero-height `<line>` is
            // a horizontal rule.
            let is_shape = matches!(node.kind, FlattenedNodeKind::Shape(_));
            if x < 0 || y < 0 || w < 0 || h < 0 || (w == 0 && h == 0) {
                continue;
            }
            if !is_shape && (w == 0 || h == 0) {
                continue;
            }

            // Draw fill background if present, in the box the border paints
            // in (`render_bounds`: the leaf's outer box, see
            // `anchor_own_border`).
            if let Some(border) = &node.style.border
                && let Some(fill) = &border.fill
                && fill.presence != "hidden"
                && fill.presence != "inactive"
                && fill.color.is_some()
            {
                let (fx, fy, fw, fh) =
                    Self::resolve_border_render_bounds(border, x, y, w, h, scale_dec);
                let fill_box = crate::xfa::shapes::PixelBox {
                    x: fx as f32,
                    y: fy as f32,
                    w: fw as f32,
                    h: fh as f32,
                };
                let radii = crate::xfa::shapes::rounded_corner_radii(border, fill_box, scale);
                Self::fill_area(&mut img, fill, fx, fy, fw, fh, radii);
            }

            // Draw border if present and visible
            if let Some(border) = &node.style.border
                && border.is_visible()
            {
                Self::draw_border(&mut img, x, y, w, h, border, scale);
            }

            match &node.kind {
                FlattenedNodeKind::Field {
                    value, is_checked, ..
                } => {
                    // The value is drawn beside its caption, not under it.
                    let (x, y, w, h) = node.value_box(x, y, w, h, scale);
                    // Determine widget type from hints
                    let widget = node.widget_type();
                    let is_radio = matches!(widget, Some(WidgetKind::Radio));
                    let is_checkbox_or_radio =
                        matches!(widget, Some(WidgetKind::Checkbox | WidgetKind::Radio));

                    if matches!(widget, Some(WidgetKind::Barcode)) {
                        // A barcode's value is bars, not glyphs. An empty one
                        // is a barcode whose script did not run; there is
                        // nothing to encode, so nothing is drawn.
                        if !value.is_empty() {
                            match node.barcode() {
                                Some(spec) => {
                                    // The human-readable text is in the field's font.
                                    let xfa_font = node.style.font.clone().unwrap_or_default();
                                    let legend_font =
                                        Self::render_font_for(font_manager, &fallback_font, &xfa_font)?;
                                    let legend_px = xfa_font.size.to_f32().unwrap_or(10.0) * scale;
                                    Self::draw_barcode(
                                        &mut img,
                                        spec,
                                        value,
                                        (x, y, w, h),
                                        node.rotate,
                                        black,
                                        &legend_font,
                                        legend_px,
                                    );
                                }
                                None => {
                                    log::warn!("barcode field without a <barcode> element; a placeholder is drawn");
                                    Self::draw_placeholder(&mut img, x, y, w, h);
                                }
                            }
                        }
                    } else if is_checkbox_or_radio {
                        Self::draw_check_button(
                            &mut img,
                            (x, y, w, h),
                            node.check_button(),
                            is_radio,
                            is_checked == &Some(true),
                            scale,
                            black,
                        );
                    } else {
                        // Text/choice fields: draw an underline instead of a blue
                        // fill, as Acrobat highlights a fillable field. Acrobat
                        // does not highlight a field the user cannot type into,
                        // whether by its access ("protected"/"readOnly"/
                        // "nonInteractive") or by its widget (a button, image
                        // or signature takes no typed value), so neither do we
                        // -- only "open" edit widgets get the synthetic line;
                        // everything else shows only its own template border,
                        // if any. A field without a `<ui>` has the default UI,
                        // a text edit (XFA 3.3 "The defaultUi element").
                        let access = node
                            .field_behavior()
                            .map(|(access, ..)| access)
                            .unwrap_or(FieldAccess::Open);
                        let takes_typed_value = widget.is_none_or(WidgetKind::takes_typed_value);
                        if options.field_underline && access.is_interactive() && takes_typed_value {
                            let underline_color = Rgba([0u8, 0u8, 0u8, 180u8]);
                            let line_y = y + h - 1;
                            Flattened::fill_rect(&mut img, x, line_y, w, 1, underline_color);
                        }

                        // Only draw field VALUE (not name) in black if present,
                        // as its format picture shows it.
                        let shown = node.shown_value().unwrap_or_default();
                        let value = &shown;
                        if !value.is_empty() {
                            // Get font style from node, or use XFA defaults
                            let xfa_font = node.style.font.clone().unwrap_or_default();
                            let font_size = xfa_font.size.to_f32().unwrap_or(10.0);
                            let scaled_font_size = font_size * scale;

                            let render_font =
                                Self::render_font_for(font_manager, &fallback_font, &xfa_font)?;

                            // Get text color from style or use black
                            let text_color = node
                                .style
                                .font
                                .as_ref()
                                .and_then(|f| f.color)
                                .map(|(r, g, b)| Rgba([r, g, b, 255u8]))
                                .unwrap_or(black);

                            // Calculate content area inside border margins
                            let (content_x, content_y, content_w, content_h) = if let Some(border) =
                                &node.style.border
                            {
                                let ml = (border
                                    .margin_left
                                    .unwrap_or(Decimal::ZERO)
                                    .to_f32()
                                    .unwrap_or(0.0)
                                    * scale) as i32;
                                let mt = (border
                                    .margin_top
                                    .unwrap_or(Decimal::ZERO)
                                    .to_f32()
                                    .unwrap_or(0.0)
                                    * scale) as i32;
                                let mr = (border
                                    .margin_right
                                    .unwrap_or(Decimal::ZERO)
                                    .to_f32()
                                    .unwrap_or(0.0)
                                    * scale) as i32;
                                let mb = (border
                                    .margin_bottom
                                    .unwrap_or(Decimal::ZERO)
                                    .to_f32()
                                    .unwrap_or(0.0)
                                    * scale) as i32;
                                (x + ml, y + mt, (w - ml - mr).max(0), (h - mt - mb).max(0))
                            } else {
                                (x, y, w, h)
                            };

                            // Apply text alignment from para using font metrics (within content area)
                            let text_x = Self::calculate_text_x(
                                content_x,
                                content_w,
                                value,
                                scaled_font_size,
                                &node.style.para,
                                &render_font,
                            scale,
                            );
                            let text_y = Self::calculate_text_y(
                                content_y,
                                content_h,
                                scaled_font_size,
                                &node.style.para,
                                &render_font,
                                0,
                                1,
                                scale,
                            );

                            Self::draw_glyphs(
                                &mut img,
                                value,
                                text_x,
                                text_y,
                                scaled_font_size,
                                &render_font,
                                text_color,
                                0.0,
                                Some(&xfa_font.typeface),
                            );
                            Self::draw_decoration(
                                &mut img,
                                Decoration::of_font(node.style.font.as_ref()),
                                value,
                                text_x,
                                text_y,
                                scaled_font_size,
                                &render_font,
                                0.0,
                                text_color,
                                Self::line_geometry_of(&xfa_font, scaled_font_size),
                            );
                        }
                    }
                }
                FlattenedNodeKind::Text {
                    content,
                    font_size,
                    source_name: _,
                    ..
                } => {
                    // Draw text content (draw elements/labels)
                    // Get rich text from hints if present
                    let rich_text = node.rich_text();

                    // Get font style from node, or use XFA defaults
                    let xfa_font = node.style.font.clone().unwrap_or_default();
                    // Use style font size if available, otherwise use the passed value
                    let effective_font_size = if node.style.font.is_some() {
                        xfa_font.size.to_f32().unwrap_or(10.0)
                    } else {
                        font_size.to_f32().unwrap_or(10.0)
                    };
                    let scaled_font_size = effective_font_size * scale;

                    // Get the appropriate font for this style (with fallback)
                    // Also get bold and italic variants for rich text rendering
                    // The faces, resolved the same way the layout measured them
                    // (`TextMeasurer::run_fonts`): the font as styled for plain
                    // text, and the normal/bold/italic variants rich-text runs
                    // are set in.
                    let (render_font, normal_font, bold_font, italic_font, bold_italic_font) = {
                        let mut measurer = TextMeasurer::new();
                        let render_font = measurer
                            .get_font_for_style(&xfa_font)
                            .cloned()
                            .unwrap_or_else(|_| fallback_font.clone());
                        let RunFontSet {
                            regular,
                            bold,
                            italic,
                            bold_italic,
                        } = measurer.run_fonts(&xfa_font);
                        (render_font, regular, bold, italic, bold_italic)
                    };

                    // Get text color from style or use black
                    let text_color = node
                        .style
                        .font
                        .as_ref()
                        .and_then(|f| f.color)
                        .map(|(r, g, b)| Rgba([r, g, b, 255u8]))
                        .unwrap_or(black);

                    // Calculate content area inside border margins
                    let (content_x, content_y, content_w, content_h) = {
                        // Get border margins if present
                        let (ml, mt, mr, mb) = if let Some(border) = &node.style.border {
                            (
                                (border
                                    .margin_left
                                    .unwrap_or(Decimal::ZERO)
                                    .to_f32()
                                    .unwrap_or(0.0)
                                    * scale) as i32,
                                (border
                                    .margin_top
                                    .unwrap_or(Decimal::ZERO)
                                    .to_f32()
                                    .unwrap_or(0.0)
                                    * scale) as i32,
                                (border
                                    .margin_right
                                    .unwrap_or(Decimal::ZERO)
                                    .to_f32()
                                    .unwrap_or(0.0)
                                    * scale) as i32,
                                (border
                                    .margin_bottom
                                    .unwrap_or(Decimal::ZERO)
                                    .to_f32()
                                    .unwrap_or(0.0)
                                    * scale) as i32,
                            )
                        } else {
                            (0, 0, 0, 0)
                        };

                        (
                            x + ml,
                            y + mt,
                            (w - ml - mr).max(0),
                            (h - mt - mb).max(scaled_font_size as i32),
                        )
                    };

                    // Check if we have rich text (HTML content with paragraph structure)
                    let has_rich_content = rich_text.is_some_and(|rt| {
                        rt.paragraphs
                            .iter()
                            .any(|p| !p.is_empty && p.runs.iter().any(|r| !r.text.is_empty()))
                    });

                    // Get letter spacing from XFA font (scaled to pixels)
                    let letter_spacing = xfa_font
                        .letter_spacing
                        .map(|ls| ls.to_f32().unwrap_or(0.0) * scale)
                        .unwrap_or(0.0);

                    if has_rich_content {
                        let rt = rich_text.unwrap();
                        // For rich text, use normal weight font as base
                        let base_font = normal_font.as_ref().unwrap_or(&render_font);
                        // Each run is measured with its own bold/italic variant
                        // (falling back to `base_font`), the same selection
                        // `render_text_glyph_by_glyph` below makes per word --
                        // otherwise a line that measured as fitting with the
                        // normal-weight glyphs can render wider than the
                        // column once its bold words draw in their real,
                        // wider glyphs.
                        let run_fonts = RunFonts {
                            regular: base_font,
                            bold: bold_font.as_ref(),
                            italic: italic_font.as_ref(),
                            bold_italic: bold_italic_font.as_ref(),
                        };

                        // Use XFA-compliant rich text rendering with glyph-by-glyph positioning
                        let rendered_lines = Self::layout_rich_text(
                            rt,
                            content_w as f32,
                            scaled_font_size,
                            &run_fonts,
                            scale,
                            letter_spacing,
                            node.style
                                .para
                                .as_ref()
                                .and_then(|p| p.hyphenation.as_ref()),
                            hyph_dict,
                        );

                        Self::render_text_glyph_by_glyph(
                            &mut img,
                            &rendered_lines,
                            content_x,
                            content_y,
                            content_w,
                            content_h,
                            scaled_font_size,
                            base_font,
                            bold_font.as_ref(),
                            italic_font.as_ref(),
                            bold_italic_font.as_ref(),
                            text_color,
                            &node.style.para,
                            scale,
                            letter_spacing,
                            Self::line_geometry_of(&xfa_font, scaled_font_size),
                        );
                    } else if !content.is_empty() {
                        // Fallback to simple text rendering for plain text content
                        // Use styled version to account for letter spacing
                        // Skip wrapping when the node's width was computed from PDF
                        // content stream glyph widths (NoWrap hint) since re-measuring
                        // with the resolved font may yield different widths.
                        let no_wrap = node.no_wrap;
                        let lines = if no_wrap {
                            vec![content.to_string()]
                        } else {
                            Self::wrap_text_with_font_styled(
                                content,
                                content_w as f32,
                                scaled_font_size,
                                &render_font,
                                letter_spacing,
                            )
                        };
                        let total_lines = lines.len();

                        for (i, line) in lines.iter().enumerate() {
                            // Calculate x position based on alignment (within content area)
                            let line_x = Self::calculate_text_x(
                                content_x,
                                content_w,
                                line,
                                scaled_font_size,
                                &node.style.para,
                                &render_font,
                            scale,
                            );

                            // Calculate y position using AXTE-compliant method (within content area)
                            let line_y = Self::calculate_text_y(
                                content_y,
                                content_h,
                                scaled_font_size,
                                &node.style.para,
                                &render_font,
                                i,
                                total_lines,
                                scale,
                            );

                            if line_y >= 0
                                && line_y < img_height as i32 - scaled_font_size as i32
                            {
                                Self::draw_glyphs(
                                    &mut img,
                                    line,
                                    line_x,
                                    line_y,
                                    scaled_font_size,
                                    &render_font,
                                    text_color,
                                    letter_spacing,
                                    Some(&xfa_font.typeface),
                                );
                                Self::draw_decoration(
                                    &mut img,
                                    Decoration::of_font(node.style.font.as_ref()),
                                    line,
                                    line_x,
                                    line_y,
                                    scaled_font_size,
                                    &render_font,
                                    letter_spacing,
                                    text_color,
                                    Self::line_geometry_of(&xfa_font, scaled_font_size),
                                );
                            }
                        }
                    
                    }
                }

                FlattenedNodeKind::Image(embedded) => {
                    Self::draw_embedded_image(&mut img, embedded, x, y, w, h, scale);
                }

                FlattenedNodeKind::Shape(shape) => {
                    Self::draw_shape(&mut img, shape, x, y, w, h, scale);
                }
            }
        }

        Ok(img)
    }

    /// Draw `decoration`'s lines under or through one line of plain text
    /// drawn at (`x`, `top`), the way the rich-text path draws them per word.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn draw_decoration(
        img: &mut RgbaImage,
        decoration: Decoration,
        text: &str,
        x: i32,
        top: i32,
        font_px: f32,
        font: &FontRef<'_>,
        letter_spacing: f32,
        color: Rgba<u8>,
        geometry: Option<LineGeometry>,
    ) {
        if decoration == Decoration::default() || text.trim().is_empty() {
            return;
        }
        let scaled = font.as_scaled(xfa_px_scale(font, font_px));
        let measure = |t: &str| Self::measure_text_width(t, font_px, font, letter_spacing);
        // Each span the lines run under: the whole run, or each word of it
        // (`underlinePeriod="word"`: "the rendered line is interrupted at
        // word breaks").
        let spans: Vec<(f32, f32)> = if decoration.word_period {
            text.split_whitespace()
                .filter_map(|word| {
                    let start = text.find(word)?;
                    Some((measure(&text[..start]), measure(word)))
                })
                .collect()
        } else {
            vec![(0.0, measure(text))]
        };
        for (offset, width) in spans {
            for (rx, ry, rw, rh) in decoration.line_rects(
                x as f32 + offset,
                top as f32,
                width,
                scaled.ascent(),
                scaled.descent().abs(),
                font_px,
                geometry,
            ) {
                Self::fill_rect(img, rx, ry, rw, rh, color);
            }
        }
    }

    /// Where `font`'s face draws its lines at `font_px`, for
    /// [`Self::draw_decoration`]; `None` when the face does not say.
    fn line_geometry_of(font: &Font, font_px: f32) -> Option<LineGeometry> {
        get_font_manager()
            .lock()
            .ok()
            .and_then(|mut m| m.line_geometry(font, font_px))
    }

    /// Draw `text` at (`x`, `y`) in `font` at `font_px`, letter-spaced, taking
    /// every glyph the font lacks from a face that has it (XFA 3.3 §28: a
    /// codepoint's font is substituted when the requested one cannot show
    /// it; the symbol fallback is DejaVu Sans). `family` is the text's own
    /// typeface, for the `equateRange` lookup. A character no face has is
    /// reported once and skipped, its space kept.
    #[allow(clippy::too_many_arguments)]
    fn draw_glyphs(
        img: &mut RgbaImage,
        text: &str,
        x: i32,
        y: i32,
        font_px: f32,
        font: &FontRef<'_>,
        color: Rgba<u8>,
        letter_spacing: f32,
        family: Option<&str>,
    ) {
        use ab_glyph::Font as _;
        let missing = |c: char| !c.is_whitespace() && font.glyph_id(c).0 == 0;
        let chars: Vec<char> = text.chars().collect();
        let mut cursor = x as f32;
        let mut i = 0;
        while i < chars.len() {
            let run_missing = missing(chars[i]);
            let mut j = i + 1;
            while j < chars.len() && missing(chars[j]) == run_missing {
                j += 1;
            }
            let run: String = chars[i..j].iter().collect();
            let fallback = if run_missing {
                Self::fallback_font_for(chars[i], family)
            } else {
                None
            };
            let run_font: &FontRef<'_> = fallback.as_ref().unwrap_or(font);
            let drawn = !run_missing || fallback.is_some();
            let advance = |t: &str| text_advance(run_font, font_px, t, 0.0, 1.0, KerningMode::None);
            if letter_spacing.abs() < 0.001 {
                if drawn {
                    draw_text_mut(img, color, cursor.round() as i32, y, xfa_px_scale(run_font, font_px), run_font, &run);
                }
                cursor += advance(&run);
            } else {
                for (k, ch) in run.chars().enumerate() {
                    let ch = ch.to_string();
                    if drawn {
                        draw_text_mut(img, color, cursor.round() as i32, y, xfa_px_scale(run_font, font_px), run_font, &ch);
                    }
                    cursor += advance(&ch);
                    if i + k + 1 < chars.len() {
                        cursor += letter_spacing;
                    }
                }
            }
            i = j;
        }
    }

    /// A face with a glyph for `c`, reported once per character when there
    /// is none.
    fn fallback_font_for(c: char, family: Option<&str>) -> Option<FontRef<'static>> {
        static REPORTED: std::sync::Mutex<Option<HashSet<char>>> = std::sync::Mutex::new(None);
        let found = get_font_manager()
            .lock()
            .ok()
            .and_then(|mut m| m.font_for_glyph(c, family));
        if found.is_none()
            && let Ok(mut reported) = REPORTED.lock()
            && reported.get_or_insert_with(HashSet::new).insert(c)
        {
            log::warn!("no registered font has a glyph for {c:?} (U+{:04X}); it is not drawn", c as u32);
        }
        found
    }

    /// Draw a `<line>`, `<rectangle>` or `<arc>` in its node's box (device
    /// pixels). The geometry is [`crate::xfa::shapes`]'; a rectangle's edges
    /// are drawn by the same code as a border's.
    fn draw_shape(
        img: &mut RgbaImage,
        shape: &crate::xfa::shapes::Shape,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        scale: f32,
    ) {
        use crate::xfa::shapes::{self, PixelBox, Shape};
        let edge_color = |edge: &crate::xfa::Edge| {
            edge.color
                .map(|(r, g, b)| Rgba([r, g, b, 255u8]))
                .unwrap_or(Rgba([0u8, 0u8, 0u8, 255u8]))
        };

        let bx = PixelBox {
            x: x as f32,
            y: y as f32,
            w: w as f32,
            h: h as f32,
        };
        match shape {
            Shape::Line(line) => {
                if !shapes::is_drawn(&line.edge.presence) {
                    return;
                }
                let t = shapes::thickness_px(line.edge.thickness, scale);
                let Some([a, b]) = shapes::line_centre(line.slope, line.hand, bx, t) else {
                    return;
                };
                let color = edge_color(&line.edge);
                for r in shapes::stroke_segment(a, b, t) {
                    Self::fill_rect(img, r.x, r.y, r.w, r.h, color);
                }
            }
            Shape::Rectangle(rect) => {
                let border = Border {
                    edges: rect.edges.clone(),
                    corners: rect.corners.clone(),
                    presence: "visible".to_string(),
                    ..Border::default()
                };
                if let Some(fill) = rect.fill.as_ref().filter(|f| shapes::is_drawn(&f.presence)) {
                    let radii = shapes::rounded_corner_radii(&border, bx, scale);
                    Self::fill_area(img, fill, x, y, w, h, radii);
                }
                let t = shapes::thickness_px(rect.edges[0].thickness, scale);
                let sb = shapes::rectangle_stroke_box(rect.hand, bx, t);
                Self::draw_border(
                    img,
                    sb.x.round() as i32,
                    sb.y.round() as i32,
                    sb.w.round() as i32,
                    sb.h.round() as i32,
                    &border,
                    scale,
                );
            }
            Shape::Arc(arc) => {
                if arc.sweep_angle.abs() >= 360.0
                    && let Some(fill) = arc.fill.as_ref().filter(|f| shapes::is_drawn(&f.presence))
                {
                    for py in y..=y + h {
                        for px in x..=x + w {
                            let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
                            if shapes::inside_ellipse(bx, arc.circular, cx, cy)
                                && px >= 0
                                && py >= 0
                                && (px as u32) < img.width()
                                && (py as u32) < img.height()
                            {
                                let (r, g, b) = shapes::fill_color_at(fill, bx, cx, cy);
                                img.put_pixel(px as u32, py as u32, Rgba([r, g, b, 255u8]));
                            }
                        }
                    }
                }
                if !shapes::is_drawn(&arc.edge.presence) {
                    return;
                }
                let t = shapes::thickness_px(arc.edge.thickness, scale);
                let path = shapes::arc_stroke_box(arc.hand, bx, t);
                let points = shapes::arc_points(path, arc.circular, arc.start_angle, arc.sweep_angle);
                let color = edge_color(&arc.edge);
                for pair in points.windows(2) {
                    for r in shapes::stroke_segment(pair[0], pair[1], t) {
                        Self::fill_rect(img, r.x, r.y, r.w, r.h, color);
                    }
                }
            }
        }
    }

    /// Draw an embedded image into its box.
    ///
    /// A payload that will not decode is reported and replaced by a
    /// placeholder: one broken logo must not cost the reader the rest of the
    /// page, but it must not pass for a page that simply has no logo either.
    fn draw_embedded_image(
        img: &mut RgbaImage,
        embedded: &EmbeddedImage,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        scale: f32,
    ) {
        let decoded = match image::load_from_memory(&embedded.data) {
            Ok(d) => d,
            Err(e) => {
                log::warn!(
                    "image {} ({}, {} bytes) could not be decoded: {e}",
                    embedded.source_name.as_deref().unwrap_or("<unnamed>"),
                    embedded.content_type,
                    embedded.data.len()
                );
                Self::draw_placeholder(img, x, y, w, h);
                return;
            }
        };

        let (src_w, src_h) = (decoded.width().max(1), decoded.height().max(1));
        let (box_w, box_h) = (w.max(1) as u32, h.max(1) as u32);

        let scaled = |factor: f32| {
            (
                ((src_w as f32 * factor).round() as u32).max(1),
                ((src_h as f32 * factor).round() as u32).max(1),
            )
        };
        let (draw_w, draw_h) = match embedded.aspect {
            // Stretch to the box, aspect ratio be damned.
            ImageAspect::None => (box_w, box_h),
            // Keep the aspect ratio and fit inside the box.
            ImageAspect::Fit => scaled((box_w as f32 / src_w as f32).min(box_h as f32 / src_h as f32)),
            // One pixel per point at 72 dpi, times the render scale.
            ImageAspect::Actual => scaled(scale),
            ImageAspect::Width => scaled(box_w as f32 / src_w as f32),
            ImageAspect::Height => scaled(box_h as f32 / src_h as f32),
        };

        let resized = image::imageops::resize(
            &decoded.to_rgba8(),
            draw_w,
            draw_h,
            image::imageops::FilterType::Lanczos3,
        );
        // `overlay` clips at the destination's edges, so a page-edge image is
        // cropped rather than a panic.
        image::imageops::overlay(img, &resized, x as i64, y as i64);
    }

    /// Draw a barcode's bars along the long axis of its box.
    ///
    /// The bars run across the short axis, so this works for an upright
    /// barcode and for the `rotate="90"` one in the UBS footer alike: the
    /// rotation has already been applied to the bounds, and a barcode is just
    /// stripes across whichever box comes out.
    /// The face `font` resolves to through the font manager, else the
    /// renderer's fallback face (the manager reports the substitution).
    fn render_font_for(
        font_manager: &std::sync::Mutex<FontManager>,
        fallback: &FontRef<'static>,
        font: &Font,
    ) -> Result<FontRef<'static>, String> {
        let mut mgr = font_manager
            .lock()
            .map_err(|e| format!("Lock error: {e}"))?;
        Ok(mgr.get_font(font).unwrap_or_else(|_| fallback.clone()))
    }

    /// Draw the barcode `spec` for `value` in the box `(x, y, w, h)`, the
    /// field's box after its `rotate`: the bars run along the field's own
    /// width and its human-readable text sits where `textLocation` puts it
    /// (XFA 3.3 "The barcode element": the field's `w` and `h` are the
    /// barcode's, the field's font the text's), and the whole is turned by
    /// the rotation. A value the symbology cannot encode, or a symbology or
    /// checksum this renderer does not have, is reported and leaves a
    /// placeholder.
    #[allow(clippy::too_many_arguments)]
    fn draw_barcode(
        img: &mut RgbaImage,
        spec: &crate::xfa::Barcode,
        value: &str,
        (x, y, w, h): (i32, i32, i32, i32),
        rotate: i32,
        color: Rgba<u8>,
        legend_font: &FontRef<'_>,
        legend_px: f32,
    ) {
        use crate::xfa::BarcodeTextLocation as Location;

        let encoded = match crate::barcode::encode(spec, value) {
            Ok(encoded) => encoded,
            Err(e) => {
                log::warn!("barcode not drawn: {e}");
                Self::draw_placeholder(img, x, y, w, h);
                return;
            }
        };

        // The barcode in the field's own orientation.
        let turned = rotate.rem_euclid(180) == 90;
        let (along, across) = if turned { (h, w) } else { (w, h) };
        if along <= 0 || across <= 0 {
            return;
        }
        let mut strip = RgbaImage::from_pixel(along as u32, across as u32, Rgba([0, 0, 0, 0]));

        // Embedded text has no region of its own in this symbology; the spec
        // lets the processor read it as plain above or below.
        let legend = (spec.text_location != Location::None)
            .then_some(encoded.legend.as_str())
            .filter(|l| !l.is_empty());
        let above = matches!(
            spec.text_location,
            Location::Above | Location::AboveEmbedded
        );
        let legend_h = legend
            .map_or(0, |_| (legend_px * 1.2).ceil() as i32)
            .clamp(0, across - 1);
        let bars_h = across - legend_h;
        let bars_top = if above { legend_h } else { 0 };

        // A quiet zone of ten narrow modules at each end is what the
        // symbology requires to be scannable.
        const QUIET_MODULES: f32 = 10.0;
        let ratio = spec.wide_narrow_ratio;
        let modules = crate::barcode::width_in_modules(spec, &encoded) + 2.0 * QUIET_MODULES;
        let module = along as f32 / modules;
        let mut offset = QUIET_MODULES * module;
        for bar in &encoded.bars {
            let len = if bar.wide { ratio } else { 1.0 } * module;
            if bar.black {
                let start = offset.round() as i32;
                let thickness = (len.round() as i32).max(1);
                Self::fill_rect(&mut strip, start, bars_top, thickness, bars_h, color);
            }
            offset += len;
        }
        if let Some(text) = legend {
            let width = text_advance(legend_font, legend_px, text, 0.0, 1.0, KerningMode::None);
            let lx = ((along as f32 - width) / 2.0).max(0.0).round() as i32;
            let ly = if above { 0 } else { bars_h };
            Self::draw_glyphs(&mut strip, text, lx, ly, legend_px, legend_font, color, 0.0, None);
        }

        // XFA rotation is counter-clockwise.
        let turned = match rotate.rem_euclid(360) {
            90 => image::imageops::rotate270(&strip),
            180 => image::imageops::rotate180(&strip),
            270 => image::imageops::rotate90(&strip),
            _ => strip,
        };
        image::imageops::overlay(img, &turned, x as i64, y as i64);
    }

    /// A visibly marked box, for content that is present in the form but that
    /// this renderer could not draw. Deliberately conspicuous: a silent gap
    /// would read as a form that has nothing there.
    fn draw_placeholder(img: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32) {
        if w <= 0 || h <= 0 {
            return;
        }
        let color = Rgba([150u8, 150u8, 150u8, 255u8]);
        Self::draw_rect_outline(img, x, y, w, h, color);

        // Two diagonals, drawn as a run of single pixels so that no shape
        // primitive beyond fill_rect is needed.
        let steps = w.max(h);
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let px = x + (t * w as f32) as i32;
            let py = y + (t * h as f32) as i32;
            Self::fill_rect(img, px, py, 1, 1, color);
            Self::fill_rect(img, px, y + h - 1 - (py - y), 1, 1, color);
        }
    }

    /// Draw an outlined rectangle (1px stroke).
    fn draw_rect_outline(img: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32, color: Rgba<u8>) {
        let iw = img.width() as i32;
        let ih = img.height() as i32;
        for dx in 0..w {
            let px = x + dx;
            if px >= 0 && px < iw {
                if y >= 0 && y < ih {
                    img.put_pixel(px as u32, y as u32, color);
                }
                let by = y + h - 1;
                if by >= 0 && by < ih {
                    img.put_pixel(px as u32, by as u32, color);
                }
            }
        }
        for dy in 0..h {
            let py = y + dy;
            if py >= 0 && py < ih {
                if x >= 0 && x < iw {
                    img.put_pixel(x as u32, py as u32, color);
                }
                let rx = x + w - 1;
                if rx >= 0 && rx < iw {
                    img.put_pixel(rx as u32, py as u32, color);
                }
            }
        }
    }

    /// Resolve the box a border (or its fill) actually paints in: the node's
    /// bounds, or its `render_bounds` when set (a leaf's outer box, see
    /// [`Self::anchor_own_border`]), scaled to pixels. Shared so a fill
    /// lines up with its edges.
    fn resolve_border_render_bounds(
        border: &Border,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        scale_dec: Num,
    ) -> (i32, i32, i32, i32) {
        match border.render_bounds {
            Some((rx, ry, rw, rh)) => (
                (rx * scale_dec).to_f32().unwrap_or(0.0) as i32,
                (ry * scale_dec).to_f32().unwrap_or(0.0) as i32,
                (rw * scale_dec).to_f32().unwrap_or(0.0) as i32,
                (rh * scale_dec).to_f32().unwrap_or(0.0) as i32,
            ),
            None => (x, y, w, h),
        }
    }

    /// Draw border with proper edge styling
    fn draw_border(
        img: &mut RgbaImage,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        border: &Border,
        scale: f32,
    ) {
        let img_width = img.width() as i32;
        let img_height = img.height() as i32;

        // Check if we have override render bounds from a parent subform.
        // When set, edges are rendered at the parent's position (for full-width row borders).
        let scale_dec = num(scale as f64);
        let (render_x, render_y, render_w, render_h) =
            Self::resolve_border_render_bounds(border, x, y, w, h, scale_dec);

        // A rounded corner trims the edges it joins by its radius and draws
        // the quarter circle between them (XFA 3.3 "The corner element").
        let border_box = crate::xfa::shapes::PixelBox {
            x: render_x as f32,
            y: render_y as f32,
            w: render_w as f32,
            h: render_h as f32,
        };
        let radii = crate::xfa::shapes::corner_trims(border, border_box, scale);
        // Each edge's (start, end) trim: the corners at its two ends, top and
        // bottom running left to right, left and right top to bottom.
        let trims = [
            (radii[0], radii[1]),
            (radii[1], radii[2]),
            (radii[3], radii[2]),
            (radii[0], radii[3]),
        ];

        // Get edges (0=top, 1=right, 2=bottom, 3=left)
        // Per XFA spec: if fewer than 4 edges, reuse the last one
        for edge_idx in 0..4 {
            if let Some(edge) = border.get_edge(edge_idx) {
                // Skip hidden edges
                if edge.presence == "hidden" || edge.presence == "inactive" {
                    continue;
                }

                // Get thickness in pixels (scaled)
                let thickness = edge
                    .thickness
                    .map(|t| t.to_f32().unwrap_or(1.0))
                    .unwrap_or(1.0)
                    * scale;
                let thickness_px = (thickness as i32).max(1);

                // Get color (default black)
                let color = edge
                    .color
                    .map(|(r, g, b)| Rgba([r, g, b, 255u8]))
                    .unwrap_or(Rgba([0u8, 0u8, 0u8, 255u8]));

                // Use render bounds for all edges when available, trimmed at
                // rounded corners along the edge's own direction.
                let (start, end) = (
                    trims[edge_idx].0.round() as i32,
                    trims[edge_idx].1.round() as i32,
                );
                let (edge_x, edge_y, edge_w, edge_h) = if edge_idx % 2 == 0 {
                    (render_x + start, render_y, render_w - start - end, render_h)
                } else {
                    (render_x, render_y + start, render_w, render_h - start - end)
                };

                // Draw based on stroke style (XFA 3.3 "The edge element").
                // Dashes and dots are application-defined: a dash is 4pt, a
                // gap 2pt, a dot as long as the stroke is thick (dots are
                // square). A 3D stroke takes its light and dark shades from
                // the edge's own color: lowered and raised shade the whole
                // edge by side; etched and embossed split it into an outer
                // and an inner half of opposite shades.
                let pattern = Self::dash_pattern(edge.stroke, thickness_px as f32, scale);
                let three_d = matches!(
                    edge.stroke,
                    StrokeStyle::Lowered | StrokeStyle::Raised | StrokeStyle::Etched | StrokeStyle::Embossed
                );
                if !three_d {
                    Self::draw_edge_impl(
                        img,
                        edge_x,
                        edge_y,
                        edge_w,
                        edge_h,
                        edge_idx,
                        thickness_px,
                        color,
                        img_width,
                        img_height,
                        pattern.as_deref(),
                    );
                    continue;
                }
                let (light, dark) = (Self::shade(color, 0.6), Self::shade(color, -0.4));
                let top_or_left = edge_idx == 0 || edge_idx == 3;
                match edge.stroke {
                    StrokeStyle::Lowered | StrokeStyle::Raised => {
                        let raised = edge.stroke == StrokeStyle::Raised;
                        let shade = if top_or_left == raised { light } else { dark };
                        Self::draw_edge_impl(
                            img, edge_x, edge_y, edge_w, edge_h, edge_idx, thickness_px, shade,
                            img_width, img_height, None,
                        );
                    }
                    _ => {
                        let embossed = edge.stroke == StrokeStyle::Embossed;
                        let (outer, inner) = if embossed { (light, dark) } else { (dark, light) };
                        let half = (thickness_px / 2).max(1);
                        Self::draw_edge_impl(
                            img, edge_x, edge_y, edge_w, edge_h, edge_idx, half, outer, img_width,
                            img_height, None,
                        );
                        Self::draw_edge_impl(
                            img,
                            edge_x + half,
                            edge_y + half,
                            edge_w - 2 * half,
                            edge_h - 2 * half,
                            edge_idx,
                            half,
                            inner,
                            img_width,
                            img_height,
                            None,
                        );
                    }
                }
            }
        }

        // The corners, drawn inside the box like the edges, between the
        // trimmed ends of the edges they join (XFA 3.3 "The corner element").
        for (corner_idx, radius) in radii.iter().enumerate() {
            let Some(corner) = border.get_corner(corner_idx) else {
                continue;
            };
            if *radius <= 0.0 || !crate::xfa::shapes::is_drawn(&corner.presence) {
                continue;
            }
            if corner.stroke != StrokeStyle::Solid {
                log::debug!("a corner's stroke {:?} is drawn solid", corner.stroke);
            }
            let thickness = crate::xfa::shapes::thickness_px(corner.thickness, scale);
            let inset = crate::xfa::shapes::PixelBox {
                x: border_box.x + thickness / 2.0,
                y: border_box.y + thickness / 2.0,
                w: border_box.w - thickness,
                h: border_box.h - thickness,
            };
            let color = corner
                .color
                .or_else(|| border.get_edge(corner_idx).and_then(|e| e.color))
                .map(|(r, g, b)| Rgba([r, g, b, 255u8]))
                .unwrap_or(Rgba([0u8, 0u8, 0u8, 255u8]));
            let points = crate::xfa::shapes::corner_outline(
                inset,
                corner_idx,
                (radius - thickness / 2.0).max(0.5),
                corner.join == crate::xfa::JoinStyle::Round,
                corner.inverted,
            );
            for pair in points.windows(2) {
                for r in crate::xfa::shapes::stroke_segment(pair[0], pair[1], thickness) {
                    Self::fill_rect(img, r.x, r.y, r.w, r.h, color);
                }
            }
        }
    }

    /// The on/off lengths in pixels of a stroke's dash pattern, or `None`
    /// for a continuous one. A dot is as long as the stroke is thick.
    fn dash_pattern(stroke: StrokeStyle, thickness_px: f32, scale: f32) -> Option<Vec<f32>> {
        let (dash, gap, dot) = (4.0 * scale, 2.0 * scale, thickness_px.max(1.0));
        match stroke {
            StrokeStyle::Dashed => Some(vec![dash, gap]),
            StrokeStyle::Dotted => Some(vec![dot, dot]),
            StrokeStyle::DashDot => Some(vec![dash, gap, dot, gap]),
            StrokeStyle::DashDotDot => Some(vec![dash, gap, dot, gap, dot, gap]),
            _ => None,
        }
    }

    /// `color` shaded toward white (`amount` > 0) or black (`amount` < 0).
    fn shade(color: Rgba<u8>, amount: f32) -> Rgba<u8> {
        let target = if amount > 0.0 { 255.0 } else { 0.0 };
        let t = amount.abs().clamp(0.0, 1.0);
        let mix = |c: u8| (c as f32 + (target - c as f32) * t).round() as u8;
        Rgba([mix(color[0]), mix(color[1]), mix(color[2]), color[3]])
    }

    /// Draw a solid edge
    fn draw_edge_solid(
        img: &mut RgbaImage,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        edge_idx: usize,
        thickness: i32,
        color: Rgba<u8>,
        img_width: i32,
        img_height: i32,
    ) {
        Self::draw_edge_impl(
            img, x, y, w, h, edge_idx, thickness, color, img_width, img_height, None,
        );
    }

    /// Internal helper for drawing edges (solid or dashed).
    /// `pattern` is the dash pattern's on/off lengths in pixels, repeated
    /// along the edge; `None` draws it solid.
    #[allow(clippy::too_many_arguments)]
    fn draw_edge_impl(
        img: &mut RgbaImage,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        edge_idx: usize,
        thickness: i32,
        color: Rgba<u8>,
        img_width: i32,
        img_height: i32,
        pattern: Option<&[f32]>,
    ) {
        // Whether the pattern is "on" at `pos` pixels along the edge.
        let total: f32 = pattern.map(|p| p.iter().sum()).unwrap_or(0.0);
        let should_draw = |pos: i32| -> bool {
            let Some(pattern) = pattern.filter(|_| total > 0.0) else {
                return true;
            };
            let mut along = (pos as f32 + 0.5).rem_euclid(total);
            for (i, len) in pattern.iter().enumerate() {
                if along < *len {
                    return i % 2 == 0;
                }
                along -= len;
            }
            true
        };

        // Helper to safely put pixel with bounds check
        let put_pixel = |img: &mut RgbaImage, px: i32, py: i32| {
            if px >= 0 && px < img_width && py >= 0 && py < img_height {
                img.put_pixel(px as u32, py as u32, color);
            }
        };

        match edge_idx {
            0 => {
                // Top edge
                for t in 0..thickness {
                    for dx in 0..w {
                        if should_draw(dx) {
                            put_pixel(img, x + dx, y + t);
                        }
                    }
                }
            }
            1 => {
                // Right edge
                for t in 0..thickness {
                    for dy in 0..h {
                        if should_draw(dy) {
                            put_pixel(img, x + w - 1 - t, y + dy);
                        }
                    }
                }
            }
            2 => {
                // Bottom edge
                for t in 0..thickness {
                    for dx in 0..w {
                        if should_draw(dx) {
                            put_pixel(img, x + dx, y + h - 1 - t);
                        }
                    }
                }
            }
            3 => {
                // Left edge
                for t in 0..thickness {
                    for dy in 0..h {
                        if should_draw(dy) {
                            put_pixel(img, x + t, y + dy);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Apply rotation to bounds, returning (x, y, w, h) in screen coordinates
    /// Per XFA spec: rotation is counter-clockwise about the anchor point (default topLeft)
    /// Angles are multiples of 90 degrees
    fn apply_rotation_to_bounds(
        node_x: Num,
        node_y: Num,
        node_width: Num,
        node_height: Num,
        rotate: i32,
        scale: Num,
    ) -> (i32, i32, i32, i32) {
        // The pixel box covers the node's box: its origin rounded down, its
        // far edges rounded up. Truncating the width lost up to a pixel,
        // and a draw sized exactly to its text (a growable draw) then
        // wrapped its last word at render.
        let exact_x = (node_x * scale).to_f32().unwrap_or(0.0);
        let exact_y = (node_y * scale).to_f32().unwrap_or(0.0);
        let exact_w = (node_width * scale).to_f32().unwrap_or(0.0);
        let exact_h = (node_height * scale).to_f32().unwrap_or(0.0);
        let x = exact_x.floor();
        let y = exact_y.floor();
        let w = ((exact_x + exact_w).ceil() - x).max(0.0);
        let h = ((exact_y + exact_h).ceil() - y).max(0.0);

        // Normalize rotation to 0, 90, 180, 270
        let rot = rotate.rem_euclid(360);

        match rot {
            0 => (x as i32, y as i32, w as i32, h as i32),
            90 => {
                // 90 degrees counter-clockwise: content rotates, anchor stays at top-left
                // The bounding box on screen has swapped dimensions
                // Original top-left (x,y) becomes screen position, but box extends differently
                // For 90° CCW rotation around top-left: the rendered box goes UP and RIGHT from anchor
                // Screen coords: new_x = x, new_y = y - w (box extends upward by original width)
                // But we'll just render it as a vertical rectangle starting at the same position
                // Actually: the XFA spec says rotation about anchor point
                // For topLeft anchor at (x,y), 90° CCW means:
                //   - Original bottom-left becomes top-left
                //   - The rectangle that was horizontal becomes vertical
                // Screen position adjustment: the rotated box effectively starts at (x, y-w) with size (h, w)
                // But since we can't rotate text in imageproc easily, we'll just draw a vertical box
                (x as i32, (y - w) as i32, h as i32, w as i32)
            }
            180 => {
                // 180 degrees: box is mirrored around anchor
                // Screen position: (x-w, y-h) with same dimensions
                ((x - w) as i32, (y - h) as i32, w as i32, h as i32)
            }
            270 => {
                // 270 degrees CCW (same as 90 CW):
                // Screen position: (x-h, y) with swapped dimensions
                ((x - h) as i32, y as i32, h as i32, w as i32)
            }
            _ => (x as i32, y as i32, w as i32, h as i32),
        }
    }

    /// Fill a rectangle with a solid color
    /// Paint `fill` over the box (`x`, `y`, `w`, `h`), in device pixels: its
    /// solid color or `<linear>` gradient, clipped to the box's rounded
    /// corners (`radii`, see [`crate::xfa::shapes::rounded_corner_radii`]).
    fn fill_area(
        img: &mut RgbaImage,
        fill: &crate::xfa::Fill,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        radii: [f32; 4],
    ) {
        use crate::xfa::shapes::{PixelBox, fill_color_at, inside_rounded_box};
        let bx = PixelBox {
            x: x as f32,
            y: y as f32,
            w: w as f32,
            h: h as f32,
        };
        let plain = fill.kind == crate::xfa::FillKind::Solid && radii.iter().all(|r| *r <= 0.0);
        if plain {
            let (r, g, b) = fill_color_at(fill, bx, 0.0, 0.0);
            Self::fill_rect(img, x, y, w, h, Rgba([r, g, b, 255u8]));
            return;
        }
        let (img_w, img_h) = (img.width() as i32, img.height() as i32);
        for py in y.max(0)..(y + h).min(img_h) {
            for px in x.max(0)..(x + w).min(img_w) {
                let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
                if inside_rounded_box(bx, radii, cx, cy) {
                    let (r, g, b) = fill_color_at(fill, bx, cx - 0.5, cy - 0.5);
                    img.put_pixel(px as u32, py as u32, Rgba([r, g, b, 255u8]));
                }
            }
        }
    }

    pub fn fill_rect(img: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32, color: Rgba<u8>) {
        let img_width = img.width() as i32;
        let img_height = img.height() as i32;

        for dy in 0..h {
            for dx in 0..w {
                let px = x + dx;
                let py = y + dy;
                if px >= 0 && px < img_width && py >= 0 && py < img_height {
                    img.put_pixel(px as u32, py as u32, color);
                }
            }
        }
    }

    /// Draw a check box or radio button in the value box `(x, y, w, h)`
    /// (pixels): its outline `button.size` wide, centered, square or round
    /// (XFA 3.3 "The checkButton element"), and when `checked` its mark
    /// ("The mark property": by default an X in a square, a disc in a
    /// circle). `None` for a button the template did not declare: a square
    /// as large as the box allows.
    #[allow(clippy::too_many_arguments)]
    fn draw_check_button(
        img: &mut RgbaImage,
        (x, y, w, h): (i32, i32, i32, i32),
        button: Option<&crate::xfa::CheckButton>,
        is_radio: bool,
        checked: bool,
        scale: f32,
        color: Rgba<u8>,
    ) {
        use crate::xfa::CheckMark;
        use crate::xfa::shapes::{DEFAULT_EDGE_THICKNESS_PT, polygon_fill, stroke_segment};

        let size = button
            .map(|b| (b.size.to_f32().unwrap_or(0.0) * scale).round() as i32)
            .unwrap_or(w.min(h))
            .max(1);
        let round = button.map(|b| b.round).unwrap_or(is_radio);
        let left = x + (w - size) / 2;
        let top = y + (h - size) / 2;
        let stroke = (DEFAULT_EDGE_THICKNESS_PT as f32 * scale).round().max(1.0) as i32;
        let (cx, cy) = (left + size / 2, top + size / 2);
        let white = Rgba([255u8, 255u8, 255u8, 255u8]);

        if round {
            let radius = size / 2;
            Self::fill_circle(img, cx, cy, radius, color);
            Self::fill_circle(img, cx, cy, (radius - stroke).max(0), white);
        } else {
            for t in 0..stroke {
                Self::draw_rect_outline(img, left + t, top + t, size - 2 * t, size - 2 * t, color);
            }
        }
        if !checked {
            return;
        }

        let mark = button.map(|b| b.mark).unwrap_or_default();
        let f = size as f32;
        let at = |px: f32, py: f32| (left as f32 + px * f, top as f32 + py * f);
        let thickness = (f / 8.0).max(stroke as f32);
        let mut rects = Vec::new();
        match mark {
            CheckMark::Default if round => {
                Self::fill_circle(img, cx, cy, ((f * 0.3) as i32).max(1), color);
            }
            CheckMark::Circle => {
                Self::fill_circle(img, cx, cy, ((f * 0.3) as i32).max(1), color);
            }
            CheckMark::Default | CheckMark::Cross => {
                rects.extend(stroke_segment(at(0.2, 0.2), at(0.8, 0.8), thickness));
                rects.extend(stroke_segment(at(0.8, 0.2), at(0.2, 0.8), thickness));
            }
            CheckMark::Check => {
                rects.extend(stroke_segment(at(0.2, 0.55), at(0.42, 0.78), thickness));
                rects.extend(stroke_segment(at(0.42, 0.78), at(0.82, 0.25), thickness));
            }
            CheckMark::Square => {
                rects.extend(polygon_fill(&[at(0.2, 0.2), at(0.8, 0.2), at(0.8, 0.8), at(0.2, 0.8)]));
            }
            CheckMark::Diamond => {
                rects.extend(polygon_fill(&[at(0.5, 0.15), at(0.85, 0.5), at(0.5, 0.85), at(0.15, 0.5)]));
            }
            CheckMark::Star => {
                let points: Vec<(f32, f32)> = (0..10)
                    .map(|i| {
                        let angle = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
                        let r = if i % 2 == 0 { 0.42 } else { 0.17 };
                        at(0.5 + r * angle.cos(), 0.5 + r * angle.sin())
                    })
                    .collect();
                rects.extend(polygon_fill(&points));
            }
        }
        for r in rects {
            Self::fill_rect(img, r.x, r.y, r.w, r.h, color);
        }
    }

    /// Draw a filled circle (used for radio button checked indicator)
    pub fn fill_circle(
        img: &mut RgbaImage,
        center_x: i32,
        center_y: i32,
        radius: i32,
        color: Rgba<u8>,
    ) {
        let img_width = img.width() as i32;
        let img_height = img.height() as i32;

        // Use the midpoint circle algorithm (filled version)
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                // Check if (dx, dy) is inside the circle
                if dx * dx + dy * dy <= radius * radius {
                    let px = center_x + dx;
                    let py = center_y + dy;
                    if px >= 0 && px < img_width && py >= 0 && py < img_height {
                        img.put_pixel(px as u32, py as u32, color);
                    }
                }
            }
        }
    }

    /// Calculate text X position based on horizontal alignment
    /// Uses actual font metrics for accurate text width measurement
    /// The x of a line of `text` in a box `box_w` wide at `box_x` (pixels),
    /// by the paragraph's `hAlign` (XFA 3.3 §2 "Horizontal alignment"): the
    /// paragraph's margins (points, scaled by `scale`) inset the region;
    /// `radix` puts the first radix character's left edge `radixOffset`
    /// from the region's right edge, or the line's right edge there when
    /// it has none.
    fn calculate_text_x(
        box_x: i32,
        box_w: i32,
        text: &str,
        font_size: f32,
        para: &Option<Para>,
        font: &FontRef<'_>,
        scale: f32,
    ) -> i32 {
        let h_align = para.as_ref().map(|p| p.h_align).unwrap_or(HAlign::Left);
        let advance = |t: &str| text_advance(font, font_size, t, 0.0, 1.0, KerningMode::None);
        let text_width = advance(text);
        let points = |v: Option<Num>| v.and_then(|m| m.to_f32()).unwrap_or(0.0) * scale;
        let margin_left = points(para.as_ref().and_then(|p| p.margin_left));
        let margin_right = points(para.as_ref().and_then(|p| p.margin_right));
        let (left, right) = (box_x as f32 + margin_left, (box_x + box_w) as f32 - margin_right);

        let x = match h_align {
            HAlign::Left | HAlign::Justify | HAlign::JustifyAll => left,
            HAlign::Center => left + (right - left - text_width) / 2.0,
            HAlign::Right => right - text_width,
            HAlign::Radix => {
                let anchor = right - points(para.as_ref().and_then(|p| p.radix_offset));
                let before_radix = text
                    .find(['.', ','])
                    .map(|i| advance(&text[..i]))
                    .unwrap_or(text_width);
                anchor - before_radix
            }
        };
        x.round() as i32
    }

    /// Where each of `line_count` lines of text in `font` at `font_size`
    /// (pixels) goes in a block styled by `para` (whose point values are
    /// scaled by `scale`): the top of each line's glyph box, relative to
    /// the block's top, and the block's full height -- from the same AXTE
    /// line metrics the layout measured with (`LineMetrics`: the line
    /// spacing override capped on the first line, the paragraph's space
    /// above and below, no line gap on the last line), so what is drawn is
    /// as tall as what was laid out.
    fn line_tops(
        font: &FontRef<'_>,
        font_size: f32,
        para: &Option<Para>,
        scale: f32,
        line_count: usize,
    ) -> (Vec<f32>, f32) {
        let metrics = FontMetrics::from_font(font, num(font_size as f64));
        let real_ascent = num(font.as_scaled(xfa_px_scale(font, font_size)).ascent() as f64);
        let scaled = |value: Option<Num>| {
            value.and_then(|v| v.to_f32()).map(|v| num((v * scale) as f64))
        };
        let spacing_override = scaled(para.as_ref().and_then(|p| p.line_height));
        let space_above = scaled(para.as_ref().and_then(|p| p.space_above)).unwrap_or_default();
        let space_below = scaled(para.as_ref().and_then(|p| p.space_below)).unwrap_or_default();
        let mut tops = Vec::with_capacity(line_count);
        let mut y = Decimal::ZERO;
        for i in 0..line_count {
            let mut line = LineMetrics::new(i == 0, i + 1 == line_count);
            line.accumulate(&metrics);
            line.spacing_override = spacing_override;
            if i == 0 {
                line.margin_top = space_above;
            }
            if i + 1 == line_count {
                line.margin_bottom = space_below;
            }
            tops.push((y + line.baseline_from_top() - real_ascent).to_f32().unwrap_or(0.0));
            y += line.full_height();
        }
        (tops, y.to_f32().unwrap_or(0.0))
    }

    /// The y of line `line_index` of `total_lines` lines of plain text in a
    /// box `box_h` tall at `box_y`: the block of lines placed by the
    /// paragraph's vertical alignment (top when it does not fit), each line
    /// where [`Self::line_tops`] puts it.
    #[allow(clippy::too_many_arguments)]
    fn calculate_text_y(
        box_y: i32,
        box_h: i32,
        font_size: f32,
        para: &Option<Para>,
        font: &FontRef<'_>,
        line_index: usize,
        total_lines: usize,
        render_scale: f32,
    ) -> i32 {
        let v_align = para.as_ref().map(|p| p.v_align).unwrap_or(VAlign::Top);
        let (tops, total_height) = Self::line_tops(font, font_size, para, render_scale, total_lines);
        let first_line_offset = if total_height > box_h as f32 {
            0.0
        } else {
            match v_align {
                VAlign::Top => 0.0,
                VAlign::Middle => (box_h as f32 - total_height) / 2.0,
                VAlign::Bottom => box_h as f32 - total_height,
            }
        };
        (box_y as f32 + first_line_offset + tops.get(line_index).copied().unwrap_or(0.0)) as i32
    }

    /// Text wrapping using actual font metrics for accurate width measurement
    /// Per XFA spec: letterSpacing affects spacing between grapheme clusters
    fn wrap_text_with_font_styled(
        text: &str,
        max_width: f32,
        font_size: f32,
        font: &FontRef<'_>,
        letter_spacing: f32,
    ) -> Vec<String> {
        if max_width <= 0.0 {
            return vec![text.to_string()];
        }
        wrap_words(font, font_size, text, max_width, 0.0, letter_spacing, 1.0, KerningMode::None)
    }

    /// Text wrapping using actual font metrics (backward compatible - no letter spacing)
    fn wrap_text_with_font(
        text: &str,
        max_width: f32,
        font_size: f32,
        font: &FontRef<'_>,
    ) -> Vec<String> {
        Self::wrap_text_with_font_styled(text, max_width, font_size, font, 0.0)
    }

    /// Public test helper wrapping [`Self::wrap_text_with_font`].
    #[cfg(test)]
    pub fn wrap_text_with_font_test(
        text: &str,
        max_width: f32,
        font_size: f32,
        font: &FontRef<'_>,
    ) -> Vec<String> {
        Self::wrap_text_with_font(text, max_width, font_size, font)
    }


    /// Draw a transparent hollow rectangle by blending with existing pixels
    pub fn draw_transparent_rect(
        img: &mut RgbaImage,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        color: Rgba<u8>,
    ) {
        let img_width = img.width() as i32;
        let img_height = img.height() as i32;

        // Helper to blend a pixel with transparency
        let blend_pixel = |img: &mut RgbaImage, px: i32, py: i32, color: Rgba<u8>| {
            if px >= 0 && px < img_width && py >= 0 && py < img_height {
                let existing = img.get_pixel(px as u32, py as u32);
                let alpha = color[3] as f32 / 255.0;
                let inv_alpha = 1.0 - alpha;

                let blended = Rgba([
                    (color[0] as f32 * alpha + existing[0] as f32 * inv_alpha) as u8,
                    (color[1] as f32 * alpha + existing[1] as f32 * inv_alpha) as u8,
                    (color[2] as f32 * alpha + existing[2] as f32 * inv_alpha) as u8,
                    255u8, // Keep full opacity for the result
                ]);
                img.put_pixel(px as u32, py as u32, blended);
            }
        };

        // Draw top and bottom edges
        for dx in 0..w {
            blend_pixel(img, x + dx, y, color); // Top edge
            blend_pixel(img, x + dx, y + h - 1, color); // Bottom edge
        }

        // Draw left and right edges
        for dy in 0..h {
            blend_pixel(img, x, y + dy, color); // Left edge
            blend_pixel(img, x + w - 1, y + dy, color); // Right edge
        }
    }

    /// Load a fallback font for rendering when specific fonts aren't available
    /// This uses the font_manager's fallback mechanism
    pub fn load_fallback_font() -> Result<FontRef<'static>, String> {
        let manager = get_font_manager();
        let mut manager = manager.lock().map_err(|e| format!("Lock error: {}", e))?;
        manager.get_default_font().map_err(|e| e.to_string())
    }

    // ========================================================================
    // XFA-Compliant Rich Text Parsing
    // ========================================================================

    /// Parse HTML content from exData into a RichText structure.
    /// This handles:
    /// - Paragraph elements (<p>) with inline styles (text-indent, font-weight, etc.)
    /// - Span elements with xfa-spacerun:yes for preserved spaces
    /// - Non-breaking spaces (U+00A0, &#160;)
    /// - Paragraph separators (U+2029)
    ///
    /// The `default_h_align` is used when CSS doesn't specify text-align (inherited from XFA para element)
    #[allow(clippy::too_many_arguments)]
    pub fn parse_rich_text_from_html(
        children: &[XfaNode],
        default_h_align: HAlign,
        computed_values: Option<&std::collections::HashMap<SomPath, String>>,
        id_to_field: Option<&std::collections::HashMap<String, String>>,
        default_bold: bool,
        default_italic: bool,
        default_decoration: Decoration,
        // The font size in effect outside the HTML, the base of `em` lengths.
        default_font_size: f32,
    ) -> RichText {
        let mut paragraphs = Vec::new();
        Self::parse_html_nodes_to_rich_text(
            children,
            &mut paragraphs,
            false,
            default_bold,
            default_italic,
            default_h_align,
            computed_values,
            id_to_field,
            None,
            default_decoration,
            default_font_size,
        );

        // If no paragraphs were created but we have content, create a single paragraph
        if paragraphs.is_empty() {
            paragraphs.push(RichParagraph {
                h_align: default_h_align,
                ..RichParagraph::default()
            });
        }

        RichText { paragraphs }
    }

    /// Recursively parse HTML nodes into rich text paragraphs
    fn parse_html_nodes_to_rich_text(
        children: &[XfaNode],
        paragraphs: &mut Vec<RichParagraph>,
        preserve_spaces: bool,
        bold: bool,
        italic: bool,
        default_h_align: HAlign,
        computed_values: Option<&std::collections::HashMap<SomPath, String>>,
        id_to_field: Option<&std::collections::HashMap<String, String>>,
        run_letter_spacing: Option<f32>,
        decoration: Decoration,
        // The font size in effect here, in points: the base of `em`.
        font_size: f32,
    ) {
        for child in children {
            match &child.kind {
                XfaNodeKind::Text { content } => {
                    // Handle text content - check for paragraph separators (U+2029)
                    // which should create new paragraphs
                    let segments: Vec<&str> = content.split('\u{2029}').collect();

                    for (seg_idx, segment) in segments.iter().enumerate() {
                        // If not the first segment, create a new paragraph for each U+2029
                        if seg_idx > 0 {
                            paragraphs.push(RichParagraph {
                                h_align: default_h_align,
                                ..RichParagraph::default()
                            });
                        }

                        let text = if preserve_spaces {
                            // xfa-spacerun: preserve all whitespace, convert NBSP to space
                            segment.replace('\u{00A0}', " ")
                        } else {
                            // Normal mode: collapse whitespace but preserve structure
                            Self::normalize_whitespace(segment)
                        };

                        if !text.is_empty() || preserve_spaces {
                            // If the original segment led with whitespace and
                            // normalize_whitespace stripped it, HTML collapsing
                            // rules still require a single space between
                            // adjacent inline runs.  Append it to the last run.
                            if !preserve_spaces
                                && segment.starts_with(|c: char| {
                                    c.is_ascii_whitespace() || c == '\u{00A0}'
                                })
                                && !text.starts_with(' ')
                            {
                                if let Some(para) = paragraphs.last_mut() {
                                    if let Some(last_run) = para.runs.last_mut() {
                                        if !last_run.text.ends_with(' ') {
                                            last_run.text.push(' ');
                                        }
                                    }
                                }
                            }

                            // Ensure we have a paragraph
                            if paragraphs.is_empty() {
                                paragraphs.push(RichParagraph {
                                    h_align: default_h_align,
                                    ..RichParagraph::default()
                                });
                            }

                            let para = paragraphs.last_mut().unwrap();
                            para.runs.push(RichRun {
                                text,
                                preserve_spaces,
                                bold,
                                italic,
                                underline: decoration.underline,
                                        line_through: decoration.line_through,
                                superscript: false,
                                letter_spacing: run_letter_spacing,
                            });
                        } else if !preserve_spaces
                            && segment
                                .contains(|c: char| c.is_ascii_whitespace() || c == '\u{00A0}')
                        {
                            // Whitespace-only text node between inline elements
                            // (e.g. "<span>text1</span>   <span>text2</span>").
                            // normalize_whitespace drops it entirely, but HTML
                            // rules require the whitespace to collapse to a
                            // single space between the adjacent runs.  Append
                            // that space to the last run when we are already
                            // in the middle of paragraph content.
                            if let Some(para) = paragraphs.last_mut() {
                                if let Some(last_run) = para.runs.last_mut() {
                                    if !last_run.text.ends_with(' ') {
                                        last_run.text.push(' ');
                                    }
                                }
                            }
                        }
                    }
                }
                XfaNodeKind::Element {
                    tag_name,
                    text_content,
                } => {
                    let tag_lower = tag_name.to_lowercase();
                    // An element's own `text-decoration` applies to it and
                    // everything inside it.
                    let decoration = child
                        .attributes
                        .get("style")
                        .map_or(decoration, |style| decoration.with_css(style));

                    match tag_lower.as_str() {
                        "body" => {
                            // Body element - recurse into children
                            Self::parse_html_nodes_to_rich_text(
                                &child.children,
                                paragraphs,
                                preserve_spaces,
                                bold,
                                italic,
                                default_h_align,
                                computed_values,
                                id_to_field,
                                run_letter_spacing,
                                decoration,
                                font_size,
                            );
                        }
                        "p" => {
                            // Paragraph element - create new paragraph
                            let mut para = RichParagraph {
                                h_align: default_h_align, // Use XFA default if CSS doesn't override
                                ..RichParagraph::default()
                            };

                            // Parse paragraph styles from style attribute
                            let para_bold = if let Some(style) = child.attributes.get("style") {
                                // `em` in the paragraph's own declarations is relative to
                                // the font size in effect outside it; once the paragraph
                                // sets its own size, that is the base for everything in it.
                                para.font_size =
                                    Self::parse_css_dimension_with_em(style, "font-size", font_size);
                                para.font_family = Self::parse_css_font_family(style);
                                let para_font = para.font_size.unwrap_or(font_size);
                                para.text_indent =
                                    Self::parse_css_dimension_with_em(style, "text-indent", para_font);
                                para.line_height =
                                    Self::parse_css_dimension_with_em(style, "line-height", para_font);
                                if let Some(mt) =
                                    Self::parse_css_dimension_with_em(style, "margin-top", para_font)
                                {
                                    para.space_above = Some(mt);
                                }
                                // CSS margin-left/margin-right reduce the width the
                                // paragraph's lines wrap in (XFA 3.3 "Rich Text Reference").
                                para.margin_left =
                                    Self::parse_css_dimension_with_em(style, "margin-left", para_font);
                                para.margin_right =
                                    Self::parse_css_dimension_with_em(style, "margin-right", para_font);
                                para.letter_spacing =
                                    Self::parse_css_dimension_with_em(style, "letter-spacing", para_font);
                                // Only override h_align if CSS specifies it
                                let css_align = Self::parse_css_alignment_optional(style);
                                if let Some(align) = css_align {
                                    para.h_align = align;
                                }

                                Self::parse_css_font_weight(style)
                            } else {
                                None
                            };

                            // Compute effective bold: CSS overrides inherited, otherwise inherit
                            let effective_bold = para_bold.unwrap_or(bold);
                            let para_font_size = para.font_size.unwrap_or(font_size);

                            // Add paragraph to list
                            paragraphs.push(para);

                            // Handle direct text_content of the <p> element.
                            // When the parser created interleaved Text children
                            // (to preserve DOM order), skip the aggregated
                            // text_content and let the recursion handle it.
                            let has_text_children = child
                                .children
                                .iter()
                                .any(|c| matches!(&c.kind, XfaNodeKind::Text { .. }));

                            if !has_text_children {
                                if let Some(text) = text_content {
                                    Self::add_text_with_paragraph_splits(
                                        text,
                                        paragraphs,
                                        preserve_spaces,
                                        effective_bold,
                                        italic,
                                        default_h_align,
                                        None,
                                        decoration,
                                    );
                                }
                            }

                            // Then parse children with inherited styles.
                            // Paragraph CSS letter-spacing is applied via
                            // the global parameter in layout_rich_text (through
                            // para.letter_spacing). Only explicit <span>
                            // letter-spacing should set per-run overrides.
                            Self::parse_html_nodes_to_rich_text(
                                &child.children,
                                paragraphs,
                                preserve_spaces,
                                effective_bold,
                                italic,
                                default_h_align,
                                computed_values,
                                id_to_field,
                                None,
                                decoration,
                                para_font_size,
                            );

                            // Check if paragraph ended up empty (only whitespace spans)
                            if let Some(last_para) = paragraphs.last_mut()
                                && (last_para.runs.is_empty()
                                    || last_para.runs.iter().all(|r| r.text.trim().is_empty()))
                            {
                                last_para.is_empty = true;
                            }
                        }
                        "span" => {
                            // Check for xfa:embed attribute (embedded references)
                            if let Some(embed_ref) = child.attributes.get("xfa:embed")
                                && let (Some(cv), Some(itf)) = (computed_values, id_to_field)
                                && let Some(resolved_text) =
                                    Self::resolve_embed_reference(embed_ref, cv, itf)
                            {
                                if !resolved_text.trim().is_empty() {
                                    if paragraphs.is_empty() {
                                        paragraphs.push(RichParagraph {
                                            h_align: default_h_align,
                                            ..RichParagraph::default()
                                        });
                                    }
                                    paragraphs.last_mut().unwrap().runs.push(RichRun {
                                        text: resolved_text,
                                        preserve_spaces: false,
                                        bold,
                                        italic,
                                        underline: decoration.underline,
                                        line_through: decoration.line_through,
                                        superscript: false,
                                        letter_spacing: run_letter_spacing,
                                    });
                                }
                                continue; // Don't recurse into embed spans
                            }

                            // Parse CSS styles from span for font-weight/font-style and xfa-spacerun
                            let (new_preserve, span_bold, span_italic) =
                                if let Some(style) = child.attributes.get("style") {
                                    let preserve = Self::css_declaration(style, "xfa-spacerun")
                                        .is_some_and(|v| v.eq_ignore_ascii_case("yes"));
                                    let effective_bold = Self::parse_css_font_weight(style).unwrap_or(bold);
                                    let effective_italic =
                                        Self::parse_css_font_italic(style).unwrap_or(italic);
                                    (preserve, effective_bold, effective_italic)
                                } else {
                                    (preserve_spaces, bold, italic)
                                };

                            // Detect superscript from positive vertical-align + small font-size on span
                            let span_superscript = child
                                .attributes
                                .get("style")
                                .map(|style| {
                                    Self::parse_css_dimension_with_em(style, "vertical-align", font_size)
                                        .is_some_and(|va| va > 0.0)
                                })
                                .unwrap_or(false);

                            // Track run count before recursion to mark new runs as superscript
                            let runs_before: usize = paragraphs.iter().map(|p| p.runs.len()).sum();

                            // Parse CSS letter-spacing from span style
                            let span_letter_spacing =
                                if let Some(style) = child.attributes.get("style") {
                                    Self::parse_css_dimension_with_em(style, "letter-spacing", font_size)
                                        .or(run_letter_spacing)
                                } else {
                                    run_letter_spacing
                                };

                            // Handle text_content if present
                            // Handle text_content with U+2029 support
                            // Skip when Text children are present (they preserve DOM order)
                            let has_text_children_span = child
                                .children
                                .iter()
                                .any(|c| matches!(&c.kind, XfaNodeKind::Text { .. }));

                            if !has_text_children_span {
                                if let Some(text) = text_content {
                                    Self::add_text_with_paragraph_splits(
                                        text,
                                        paragraphs,
                                        new_preserve,
                                        span_bold,
                                        span_italic,
                                        default_h_align,
                                        span_letter_spacing,
                                        decoration,
                                    );
                                }
                            }

                            // Recurse into span children with effective styles
                            Self::parse_html_nodes_to_rich_text(
                                &child.children,
                                paragraphs,
                                new_preserve,
                                span_bold,
                                span_italic,
                                default_h_align,
                                computed_values,
                                id_to_field,
                                span_letter_spacing,
                                decoration,
                                font_size,
                            );

                            // Mark newly added runs as superscript if vertical-align was positive
                            if span_superscript {
                                let runs_after: usize =
                                    paragraphs.iter().map(|p| p.runs.len()).sum();
                                if runs_after > runs_before {
                                    let mut to_mark = runs_after - runs_before;
                                    for para in paragraphs.iter_mut().rev() {
                                        for run in para.runs.iter_mut().rev() {
                                            if to_mark == 0 {
                                                break;
                                            }
                                            run.superscript = true;
                                            to_mark -= 1;
                                        }
                                        if to_mark == 0 {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        "b" | "strong" => {
                            // Bold text - handle U+2029 paragraph separators
                            if let Some(text) = text_content {
                                Self::add_text_with_paragraph_splits(
                                    text,
                                    paragraphs,
                                    preserve_spaces,
                                    true,
                                    italic,
                                    default_h_align,
                                    run_letter_spacing,
                                    decoration,
                                );
                            }
                            Self::parse_html_nodes_to_rich_text(
                                &child.children,
                                paragraphs,
                                preserve_spaces,
                                true,
                                italic,
                                default_h_align,
                                computed_values,
                                id_to_field,
                                run_letter_spacing,
                                decoration,
                                font_size,
                            );
                        }
                        "i" | "em" => {
                            // Italic text - handle U+2029 paragraph separators
                            if let Some(text) = text_content {
                                Self::add_text_with_paragraph_splits(
                                    text,
                                    paragraphs,
                                    preserve_spaces,
                                    bold,
                                    true,
                                    default_h_align,
                                    run_letter_spacing,
                                    decoration,
                                );
                            }
                            Self::parse_html_nodes_to_rich_text(
                                &child.children,
                                paragraphs,
                                preserve_spaces,
                                bold,
                                true,
                                default_h_align,
                                computed_values,
                                id_to_field,
                                run_letter_spacing,
                                decoration,
                                font_size,
                            );
                        }
                        "br" => {
                            // Line break handling per XFA/HTML semantics.
                            // When <br/> appears inside a <p> that has no content yet
                            // (e.g. <p><br/></p>), mark the existing paragraph as empty
                            // rather than creating a second one. This is critical for
                            // alignment between overlapping draw elements (T_Left/T_LeftIndent)
                            // where each <p> should produce exactly ONE paragraph slot.
                            if let Some(last_para) = paragraphs.last_mut() {
                                if last_para.runs.is_empty()
                                    || last_para.runs.iter().all(|r| r.text.trim().is_empty())
                                {
                                    // Current paragraph is empty, just mark it
                                    last_para.is_empty = true;
                                    last_para.has_br = true;
                                } else {
                                    // Current paragraph has content, start a new empty one
                                    paragraphs.push(RichParagraph {
                                        h_align: default_h_align,
                                        is_empty: true,
                                        has_br: true,
                                        ..Default::default()
                                    });
                                }
                            } else {
                                // No paragraphs yet - create one
                                paragraphs.push(RichParagraph {
                                    h_align: default_h_align,
                                    is_empty: true,
                                    has_br: true,
                                    ..Default::default()
                                });
                            }
                        }
                        _ => {
                            // Unknown element (e.g. <a href="...">text</a>,
                            // which has no dedicated case): handle U+2029
                            // paragraph separators in its aggregated text,
                            // then recurse into its children. When the parser
                            // created interleaved Text children (to preserve
                            // DOM order) those children already cover
                            // `text_content` verbatim, so counting both would
                            // double the element's text -- exactly the guard
                            // the "p" case above uses.
                            let has_text_children = child
                                .children
                                .iter()
                                .any(|c| matches!(&c.kind, XfaNodeKind::Text { .. }));

                            if !has_text_children
                                && let Some(text) = text_content
                            {
                                Self::add_text_with_paragraph_splits(
                                    text,
                                    paragraphs,
                                    preserve_spaces,
                                    bold,
                                    italic,
                                    default_h_align,
                                    run_letter_spacing,
                                    decoration,
                                );
                            }
                            Self::parse_html_nodes_to_rich_text(
                                &child.children,
                                paragraphs,
                                preserve_spaces,
                                bold,
                                italic,
                                default_h_align,
                                computed_values,
                                id_to_field,
                                run_letter_spacing,
                                decoration,
                                font_size,
                            );
                        }
                    }
                }
                _ => {
                    // Other node types - recurse into children
                    Self::parse_html_nodes_to_rich_text(
                        &child.children,
                        paragraphs,
                        preserve_spaces,
                        bold,
                        italic,
                        default_h_align,
                        computed_values,
                        id_to_field,
                        run_letter_spacing,
                        decoration,
                        font_size,
                    );
                }
            }
        }
    }

    /// Helper to add text content to paragraphs, handling U+2029 paragraph separators.
    /// This splits text on U+2029 and creates new paragraphs as needed.
    fn add_text_with_paragraph_splits(
        text: &str,
        paragraphs: &mut Vec<RichParagraph>,
        preserve_spaces: bool,
        bold: bool,
        italic: bool,
        default_h_align: HAlign,
        run_letter_spacing: Option<f32>,
        decoration: Decoration,
    ) {
        // Split on U+2029 paragraph separator
        let segments: Vec<&str> = text.split('\u{2029}').collect();

        for (seg_idx, segment) in segments.iter().enumerate() {
            // If not the first segment, create a new paragraph for each U+2029
            if seg_idx > 0 {
                paragraphs.push(RichParagraph {
                    h_align: default_h_align,
                    ..RichParagraph::default()
                });
            }

            let processed = if preserve_spaces {
                // xfa-spacerun: preserve all whitespace, convert NBSP to space
                segment.replace('\u{00A0}', " ")
            } else {
                // Normal mode: collapse whitespace but preserve structure
                Self::normalize_whitespace(segment)
            };

            if !processed.is_empty() || preserve_spaces {
                // Ensure we have a paragraph
                if paragraphs.is_empty() {
                    paragraphs.push(RichParagraph {
                        h_align: default_h_align,
                        ..RichParagraph::default()
                    });
                }

                paragraphs.last_mut().unwrap().runs.push(RichRun {
                    text: processed,
                    preserve_spaces,
                    bold,
                    italic,
                    underline: decoration.underline,
                    line_through: decoration.line_through,
                    superscript: false,
                    letter_spacing: run_letter_spacing,
                });
            }
        }
    }

    /// Normalize whitespace in text content per XFA/HTML rules.
    /// Collapses consecutive whitespace to single space, handles special chars.
    /// Note: U+2029 is NOT handled here - it should be split before calling this.
    fn normalize_whitespace(text: &str) -> String {
        let mut result = String::new();
        let mut last_was_space = true; // Start true to trim leading

        for ch in text.chars() {
            match ch {
                // Paragraph separator - treat as paragraph break marker
                '\u{2029}' => {
                    if !last_was_space {
                        result.push(' ');
                        last_was_space = true;
                    }
                }
                // Non-breaking space - convert to regular space but keep
                '\u{00A0}' => {
                    result.push(' ');
                    last_was_space = true;
                }
                // Regular whitespace
                ' ' | '\t' | '\n' | '\r' => {
                    if !last_was_space {
                        result.push(' ');
                        last_was_space = true;
                    }
                }
                // Regular character
                _ => {
                    result.push(ch);
                    last_was_space = false;
                }
            }
        }

        // Trim trailing space
        if result.ends_with(' ') {
            result.pop();
        }

        result
    }

    /// Parse a CSS dimension value from a style string.
    /// Looks for "property: Xpt" or "property: Xin" etc.
    /// The value of the CSS declaration `property` in a `style` attribute
    /// (`name:value; name:value`), trimmed; `None` when the style has no
    /// such declaration. Names match whole and case-insensitively, so
    /// `font-size` is not found inside `xfa-font-size` (XFA 3.3 "Rich Text
    /// Reference", CSS declarations).
    fn css_declaration<'a>(style: &'a str, property: &str) -> Option<&'a str> {
        style.split(';').find_map(|declaration| {
            let (name, value) = declaration.split_once(':')?;
            name.trim().eq_ignore_ascii_case(property).then(|| value.trim())
        })
    }

    /// A CSS length declaration in points. `em` is relative to `em_base`,
    /// the font size in effect where the declaration applies (XFA 3.3 "Rich
    /// Text Reference": the length units of CSS 2, with `em` the current
    /// font size); a unitless number is read as points, which is how
    /// Designer writes `0`. An unknown unit is reported and ignored.
    fn parse_css_dimension_with_em(style: &str, property: &str, em_base: f32) -> Option<f32> {
        let value = Self::css_declaration(style, property)?;
        let (number, unit) = value
            .find(|c: char| c.is_ascii_alphabetic() || c == '%')
            .map_or((value, ""), |i| value.split_at(i));
        let number: f32 = number.trim().parse().ok()?;
        match unit.trim().to_ascii_lowercase().as_str() {
            "pt" | "" => Some(number),
            "em" => Some(number * em_base),
            "in" => Some(number * 72.0),
            "mm" => Some(number * 72.0 / 25.4),
            "cm" => Some(number * 72.0 / 2.54),
            "px" => Some(number * 0.75),
            other => {
                log::warn!("CSS {property}: {value:?} has the unit {other:?}, which is not a length unit here; ignored");
                None
            }
        }
    }

    /// Parse font-family from CSS style string.
    /// Handles quoted and unquoted font names like:
    /// - font-family:'Frutiger 45 Light'
    /// - font-family: "Arial"
    /// - font-family: Helvetica
    fn parse_css_font_family(style: &str) -> Option<String> {
        let value = Self::css_declaration(style, "font-family")?;
        let first = value.split(',').next()?.trim();
        let name = first.trim_matches(|c| c == '\'' || c == '"').trim();
        (!name.is_empty()).then(|| name.to_string())
    }

    /// Parse font-weight from CSS style string.
    /// Returns true if bold, false if normal/not specified.
    fn parse_css_font_weight(style: &str) -> Option<bool> {
        let value = Self::css_declaration(style, "font-weight")?.to_ascii_lowercase();
        match value.as_str() {
            "bold" | "bolder" => Some(true),
            "normal" | "lighter" => Some(false),
            numeric => match numeric.parse::<u32>() {
                Ok(weight) => Some(weight >= 600),
                Err(_) => {
                    log::warn!("CSS font-weight {value:?} is not a weight; ignored");
                    None
                }
            },
        }
    }

    /// `font-style`: `Some(true)` for italic or oblique, `Some(false)` for
    /// normal, `None` when the style does not say.
    fn parse_css_font_italic(style: &str) -> Option<bool> {
        let value = Self::css_declaration(style, "font-style")?.to_ascii_lowercase();
        match value.as_str() {
            "italic" | "oblique" => Some(true),
            "normal" => Some(false),
            _ => {
                log::warn!("CSS font-style {value:?} is not a style; ignored");
                None
            }
        }
    }

    /// Parse CSS text-align property from style string
    fn parse_css_alignment(style: &str) -> HAlign {
        Self::parse_css_alignment_optional(style).unwrap_or(HAlign::Left)
    }

    /// `text-align`, `None` when the style does not set it.
    fn parse_css_alignment_optional(style: &str) -> Option<HAlign> {
        let value = Self::css_declaration(style, "text-align")?.to_ascii_lowercase();
        match value.as_str() {
            "left" => Some(HAlign::Left),
            "center" => Some(HAlign::Center),
            "right" => Some(HAlign::Right),
            "justify" => Some(HAlign::Justify),
            _ => {
                log::warn!("CSS text-align {value:?} is not an alignment; ignored");
                None
            }
        }
    }

    // ========================================================================
    // XFA-Compliant Text Layout and Wrapping
    // ========================================================================

    /// Layout rich text into rendered lines with proper word wrapping.
    /// This handles:
    ///
    /// - Per-paragraph text-indent (first line only)
    /// - Preserved spaces (xfa-spacerun)
    /// - Proper word breaking (don't break on NBSP)
    /// - Justify preparation (marking first/last lines)
    ///
    /// Per XFA spec: letterSpacing affects interword and interletter spacings
    ///
    /// `fonts` supplies the bold/italic variants runs are measured with (see
    /// [`RunFonts`]); inter-word spacing and hyphenation still use its
    /// `regular` face, matching how this pipeline has always measured those.
    pub fn layout_rich_text(
        rich_text: &RichText,
        max_width: f32,
        font_size: f32,
        fonts: &RunFonts<'_>,
        scale: f32,
        letter_spacing: f32,
        hyph_settings: Option<&super::xfa::hyphenation::XfaHyphenation>,
        hyph_dict: Option<&hyphenation::Standard>,
    ) -> Vec<RenderedLine> {
        let font = fonts.regular;
        let mut lines = Vec::new();

        for para in &rich_text.paragraphs {
            // Paragraph CSS letter-spacing overrides inherited/font-level spacing,
            // including explicit 0in and negative values (e.g. -0.002em).
            let para_letter_spacing = para
                .letter_spacing
                .map(|ls| ls * scale)
                .unwrap_or(letter_spacing);
            // The space between two words, letter spacing included, as
            // `text_advance` measures it.
            let space_width = space_advance(font, font_size, para_letter_spacing, 1.0);

            // Handle empty paragraphs as blank lines
            if para.is_empty {
                lines.push(RenderedLine {
                    words: vec![],
                    y: 0.0, // Will be calculated later
                    is_first_line: true,
                    is_last_line: true,
                    text_indent: 0.0,
                    h_align: para.h_align,
                    content_width: 0.0,
                    margin_left: para.margin_left.map(|m| m * scale),
                });
                continue;
            }

            // Per XFA spec Chapter 27: margin-left/margin-right on paragraphs reduce
            // the available width for text wrapping
            let para_margin_left = para.margin_left.map(|m| m * scale);
            let para_margin_right = para.margin_right.unwrap_or(0.0) * scale;
            let para_margin_left_val = para_margin_left.unwrap_or(0.0);
            let para_effective_width =
                (max_width - para_margin_left_val - para_margin_right).max(0.0);

            // Calculate effective indent (in pixels after scaling)
            let para_indent = para.text_indent.unwrap_or(0.0) * scale;

            // Collect all text from runs into tokens for wrapping
            let tokens =
                Self::tokenize_paragraph_runs(&para.runs, font_size, fonts, para_letter_spacing);

            if tokens.is_empty() {
                // Empty paragraph - add blank line
                lines.push(RenderedLine {
                    words: vec![],
                    y: 0.0,
                    is_first_line: true,
                    is_last_line: true,
                    text_indent: para_indent,
                    h_align: para.h_align,
                    content_width: 0.0,
                    margin_left: para_margin_left,
                });
                continue;
            }

            // Lines break at UAX-14 opportunities (spaces here) exactly at the
            // available width (XFA 3.3 "Break Individual Lines").
            let para_lines = Self::wrap_tokens_to_lines(
                &tokens,
                para_effective_width,
                para_indent,
                space_width,
                hyph_settings,
                hyph_dict,
                Some(font),
                font_size,
                letter_spacing,
            );
            let num_para_lines = para_lines.len();

            for (i, line_tokens) in para_lines.into_iter().enumerate() {
                let is_first = i == 0;
                let is_last = i == num_para_lines - 1;

                // Calculate content width
                let mut content_width: f32 = 0.0;
                for (j, token) in line_tokens.iter().enumerate() {
                    content_width += token.width;
                    if j < line_tokens.len() - 1 {
                        content_width += space_width;
                    }
                }

                // Convert tokens to rendered words (positioning happens during render)
                let words: Vec<RenderedWord> = line_tokens
                    .into_iter()
                    .map(|t| RenderedWord {
                        text: t.text,
                        x: 0.0, // Will be calculated during render
                        preserve_spaces: t.preserve_spaces,
                        bold: t.bold,
                        italic: t.italic,
                        decoration: t.decoration,
                    })
                    .collect();

                lines.push(RenderedLine {
                    words,
                    y: 0.0, // Will be calculated later
                    is_first_line: is_first,
                    is_last_line: is_last,
                    text_indent: if is_first { para_indent } else { 0.0 },
                    h_align: para.h_align,
                    content_width,
                    margin_left: para_margin_left,
                });
            }
        }

        lines
    }

    /// Tokenize paragraph runs into layout tokens
    /// Per XFA spec: letterSpacing affects interword and interletter spacings
    ///
    /// Each run is measured with the font variant matching its own
    /// `bold`/`italic` (via `fonts`), not a single font for the whole
    /// paragraph: a `<span style="font-weight:bold">` run inside an
    /// otherwise-normal paragraph is a different, typically wider, face, and
    /// wrapping has to know that to match what `render_text_glyph_by_glyph`
    /// (which already selects per-word) draws.
    pub(crate) fn tokenize_paragraph_runs(
        runs: &[RichRun],
        font_size: f32,
        fonts: &RunFonts<'_>,
        letter_spacing: f32,
    ) -> Vec<LayoutToken> {
        let mut tokens = Vec::new();

        // Track if we've seen a word boundary (space) since the last token was added.
        // This prevents merging across space-only runs.
        let mut seen_word_boundary = true; // Start true so first word is not merged

        for run in runs {
            let effective_ls = run.letter_spacing.unwrap_or(letter_spacing);
            let run_font = fonts.for_run(run.bold, run.italic);
            if run.preserve_spaces {
                // Preserved space run - keep as single token
                let width = Self::measure_text_width(&run.text, font_size, run_font, effective_ls);
                if !run.text.is_empty() {
                    tokens.push(LayoutToken {
                        text: run.text.clone(),
                        width,
                        preserve_spaces: true,
                        bold: run.bold,
                        italic: run.italic,
                        decoration: run.decoration(),
                    });
                    // A preserved-space run that is only whitespace IS a word boundary.
                    // This prevents the next run from merging its first word into this space.
                    seen_word_boundary = run.text.trim().is_empty();
                }
            } else {
                // Normal text - split into words
                let mut current_word = String::new();
                // Track if this run's first character was NOT a space
                // If so, we should merge with previous token (e.g., "currencies<span>5</span>")
                // BUT only if we haven't seen a word boundary since the last token
                let mut run_starts_without_space =
                    !run.text.starts_with(' ') && !seen_word_boundary;

                for ch in run.text.chars() {
                    if ch == ' ' {
                        if !current_word.is_empty() {
                            // Check if we should merge with previous token
                            if run_starts_without_space && !tokens.is_empty() {
                                // Merge with previous token. It keeps its own
                                // bold/italic (unchanged by this merge), so
                                // re-measure with the font matching *that*,
                                // not necessarily this run's.
                                let prev_token = tokens.last_mut().unwrap();
                                prev_token.text.push_str(&current_word);
                                let prev_font =
                                    fonts.for_run(prev_token.bold, prev_token.italic);
                                prev_token.width = Self::measure_text_width(
                                    &prev_token.text,
                                    font_size,
                                    prev_font,
                                    effective_ls,
                                );
                            } else {
                                let width = Self::measure_text_width(
                                    &current_word,
                                    font_size,
                                    run_font,
                                    effective_ls,
                                );
                                tokens.push(LayoutToken {
                                    text: current_word.clone(),
                                    width,
                                    preserve_spaces: false,
                                    bold: run.bold,
                                    italic: run.italic,
                                    decoration: run.decoration(),
                                });
                            }
                            current_word.clear();
                        }
                        // After first word in this run, we're no longer at the start
                        run_starts_without_space = false;
                        // We've seen a word boundary (space)
                        seen_word_boundary = true;
                    } else {
                        current_word.push(ch);
                    }
                }

                // Don't forget the last word
                if !current_word.is_empty() {
                    // Check if we should merge with previous token
                    if run_starts_without_space && !tokens.is_empty() {
                        // Merge with previous token; see the comment above.
                        let prev_token = tokens.last_mut().unwrap();
                        prev_token.text.push_str(&current_word);
                        let prev_font = fonts.for_run(prev_token.bold, prev_token.italic);
                        prev_token.width = Self::measure_text_width(
                            &prev_token.text,
                            font_size,
                            prev_font,
                            effective_ls,
                        );
                    } else {
                        let width = Self::measure_text_width(
                            &current_word,
                            font_size,
                            run_font,
                            effective_ls,
                        );
                        tokens.push(LayoutToken {
                            text: current_word,
                            width,
                            preserve_spaces: false,
                            bold: run.bold,
                            italic: run.italic,
                            decoration: run.decoration(),
                        });
                    }
                    // We added a word, so clear the word boundary flag
                    seen_word_boundary = false;
                }
            }
        }

        tokens
    }

    /// Wrap tokens into lines respecting max width and indentation.
    ///
    /// When `hyph_dict` is provided, words that don't fit on the current line
    /// are split at valid hyphenation break points per XFA spec. The hyphen
    /// mark ("-") is appended to the first fragment and its width is accounted
    /// for in the line measurement.
    fn wrap_tokens_to_lines(
        tokens: &[LayoutToken],
        max_width: f32,
        first_line_indent: f32,
        space_width: f32,
        hyph_settings: Option<&super::xfa::hyphenation::XfaHyphenation>,
        hyph_dict: Option<&hyphenation::Standard>,
        font: Option<&ab_glyph::FontRef<'_>>,
        font_size: f32,
        letter_spacing: f32,
    ) -> Vec<Vec<LayoutToken>> {
        if tokens.is_empty() {
            return vec![vec![]];
        }

        // Pre-measure hyphen width if hyphenation is available
        let hyphen_width = if let Some(f) = font {
            Self::measure_text_width("-", font_size, f, letter_spacing)
        } else {
            0.0
        };

        let mut lines: Vec<Vec<LayoutToken>> = Vec::new();
        let mut current_line: Vec<LayoutToken> = Vec::new();
        let mut current_width: f32 = 0.0;
        let mut is_first_line = true;

        let mut i = 0;
        while i < tokens.len() {
            let token = &tokens[i];
            let effective_max = if is_first_line {
                max_width - first_line_indent
            } else {
                max_width
            };

            let token_space = if current_line.is_empty() {
                0.0
            } else {
                space_width
            };

            if current_width + token_space + token.width <= effective_max
                || current_line.is_empty()
            {
                // Token fits on current line
                if !current_line.is_empty() {
                    current_width += space_width;
                }
                current_width += token.width;
                current_line.push(token.clone());
                i += 1;
            } else {
                // Token doesn't fit — try hyphenation before falling back to
                // whole-word wrapping.
                let remaining_space = effective_max - current_width - token_space;

                if let (Some(settings), Some(dict), Some(f)) = (hyph_settings, hyph_dict, font) {
                    if let Some((first_frag, second_frag)) = Self::try_hyphenate_token(
                        token,
                        remaining_space,
                        settings,
                        dict,
                        f,
                        font_size,
                        letter_spacing,
                        hyphen_width,
                    ) {
                        // Hyphenation succeeded: place first fragment on current line
                        current_line.push(first_frag);
                        lines.push(current_line);

                        // Second fragment starts a new line unconditionally
                        // (matching non-hyphenation path where oversized tokens
                        // are placed at line start without further splitting).
                        current_line = Vec::new();
                        is_first_line = false;

                        // Place second_frag on the new line as its first token.
                        current_width = second_frag.width;
                        current_line.push(second_frag);
                        i += 1;
                        continue;
                    }
                }

                // No hyphenation or no valid break: original behavior
                lines.push(current_line);
                current_line = vec![token.clone()];
                current_width = token.width;
                is_first_line = false;
                i += 1;
            }
        }

        // Don't forget the last line
        if !current_line.is_empty() {
            lines.push(current_line);
        }

        if lines.is_empty() {
            lines.push(vec![]);
        }

        lines
    }

    /// Try to split a token at a hyphenation boundary that fits the given space.
    ///
    /// Returns `Some((first_fragment, second_fragment))` if a valid break was
    /// found, where the first fragment includes the trailing hyphen and its
    /// width accounts for the hyphen character.
    fn try_hyphenate_token(
        token: &LayoutToken,
        available_width: f32,
        settings: &super::xfa::hyphenation::XfaHyphenation,
        dict: &hyphenation::Standard,
        font: &ab_glyph::FontRef<'_>,
        font_size: f32,
        letter_spacing: f32,
        hyphen_width: f32,
    ) -> Option<(LayoutToken, LayoutToken)> {
        // Get break points (byte indices) — filtered by XFA rules
        let mut break_points = settings.break_points(&token.text, dict);

        if break_points.is_empty() {
            // Try emergency hyphenation if no regular break points exist
            break_points = settings.emergency_break_points(&token.text, dict);
            if break_points.is_empty() {
                return None;
            }
        }

        // Sort break points for reverse iteration (largest first = maximize
        // grapheme clusters before break per XFA spec).
        break_points.sort_unstable();

        // Helper closure to try splitting at a set of break points
        let try_split = |break_points: &[usize]| -> Option<(LayoutToken, LayoutToken)> {
            for &byte_idx in break_points.iter().rev() {
                let first_part = &token.text[..byte_idx];
                let first_width =
                    Self::measure_text_width(first_part, font_size, font, letter_spacing)
                        + hyphen_width;

                if first_width <= available_width || available_width <= 0.0 {
                    let second_part = &token.text[byte_idx..];
                    let second_width =
                        Self::measure_text_width(second_part, font_size, font, letter_spacing);

                    return Some((
                        LayoutToken {
                            text: format!("{}-", first_part),
                            width: first_width,
                            preserve_spaces: token.preserve_spaces,
                            bold: token.bold,
                            italic: token.italic,
                            decoration: token.decoration,
                        },
                        LayoutToken {
                            text: second_part.to_string(),
                            width: second_width,
                            preserve_spaces: token.preserve_spaces,
                            bold: token.bold,
                            italic: token.italic,
                            decoration: token.decoration,
                        },
                    ));
                }
            }
            None
        };

        if let Some(result) = try_split(&break_points) {
            return Some(result);
        }

        // Per XFA spec: if no regular break point fits, try emergency hyphenation
        // which relaxes remain/push character count constraints.
        let emergency = settings.emergency_break_points(&token.text, dict);
        if !emergency.is_empty() {
            let mut emergency_sorted = emergency;
            emergency_sorted.sort_unstable();
            if let Some(result) = try_split(&emergency_sorted) {
                return Some(result);
            }
        }

        None
    }

    /// Measure text width using font metrics
    /// Per XFA 3.3 spec (§17 font.letterSpacing, §27 "Letter Spacing"):
    /// letterSpacing is "an adjustment to the spacing that would otherwise be
    /// used between successive grapheme clusters". This function adds
    /// `letter_spacing` between each adjacent pair of characters within the
    /// supplied text.
    ///
    /// Note: this function intentionally does NOT apply pair kerning. Per the
    /// XFA spec the default value of `font/@kerningMode` is "none"
    /// (see §17 font.kerningMode), and the UBS templates this engine targets
    /// explicitly set kerningMode="none". Kerning support, if/when needed,
    /// must be plumbed in alongside the font's kerningMode and gated on
    /// `kerningMode == "pair"` to match Adobe AXTE behavior.
    fn measure_text_width(
        text: &str,
        font_size: f32,
        font: &FontRef<'_>,
        letter_spacing: f32,
    ) -> f32 {
        text_advance(font, font_size, text, letter_spacing, 1.0, KerningMode::None)
    }

    // ========================================================================
    // Multi-Paragraph Draw Node Splitting
    // ========================================================================

    /// `font` with the paragraph's own CSS `font-family` and `font-size`
    /// applied: a style applies to its own element, so these are the
    /// paragraph's and not the draw's (XFA 3.3 "Rich Text Reference").
    fn apply_paragraph_font(font: &mut Font, para: &RichParagraph) {
        if let Some(family) = &para.font_family {
            font.typeface = family.clone();
        }
        if let Some(size) = para.font_size {
            font.size = num(size as f64);
        }
    }

    /// Merge one `RichParagraph`'s CSS overrides (from its `<p style="...">`)
    /// onto the draw's own `<para>` element. The paragraph's own `line_height`
    /// / `space_above` / `space_below` / `text_indent` / `h_align` win when
    /// present (XFA 3.3 Chapter 27, "Line Spacing" / "Space Before Paragraph":
    /// CSS `line-height`/`margin-top` on a `<p>` are that paragraph's own
    /// lineHeight/spaceAbove, not the draw's); everything else -- vertical
    /// alignment, margins, hyphenation -- comes from the draw's `<para>`.
    /// Shared by `rich_paragraph_heights` (paragraph height) and
    /// `split_draw_into_paragraph_nodes` (the style each split-off paragraph
    /// node renders with), so both agree on what a paragraph's own line
    /// height/spacing is.
    fn para_for_rich_paragraph(para: &RichParagraph, default_para: &Option<Para>) -> Para {
        Para {
            h_align: para.h_align,
            v_align: default_para
                .as_ref()
                .map(|p| p.v_align)
                .unwrap_or(VAlign::Top),
            line_height: para
                .line_height
                .map(|lh| num(lh as f64))
                .or_else(|| default_para.as_ref().and_then(|p| p.line_height)),
            space_above: para
                .space_above
                .map(|s| num(s as f64))
                .or_else(|| default_para.as_ref().and_then(|p| p.space_above)),
            space_below: para
                .space_below
                .map(|s| num(s as f64))
                .or_else(|| default_para.as_ref().and_then(|p| p.space_below)),
            text_indent: para
                .text_indent
                .map(|s| num(s as f64))
                .or_else(|| default_para.as_ref().and_then(|p| p.text_indent)),
            margin_left: default_para.as_ref().and_then(|p| p.margin_left),
            margin_right: default_para.as_ref().and_then(|p| p.margin_right),
            radix_offset: default_para.as_ref().and_then(|p| p.radix_offset),
            hyphenation: default_para.as_ref().and_then(|p| p.hyphenation.clone()),
        }
    }

    /// Per-paragraph heights of a rich-text draw `max_width` wide, from the
    /// very lines `layout_rich_text` draws, measured in the same faces
    /// (each run in its own weight and posture) with the same indent and
    /// margins. Each line's height follows AXTE (XFA 3.3 appendix "Text
    /// Layout"): ascent plus descent of the faces on it, the line gap or
    /// the paragraph's `line-height` override (capped on the block's first
    /// line), `spaceAbove`/`spaceBelow` on a paragraph's first and last
    /// line, and no line gap on the block's last line. An empty paragraph
    /// is one line. Shared by `split_draw_into_paragraph_nodes` (which
    /// places each paragraph at these heights) and
    /// `calculate_rich_text_draw_height` (which sizes the draw before it
    /// is split), so the measured height is the drawn one.
    fn rich_paragraph_heights(
        rich_text: &RichText,
        xfa_font: &Font,
        default_para: &Option<Para>,
        max_width: Num,
        language: &str,
    ) -> Vec<Num> {
        let mut measurer = TextMeasurer::new();
        let paragraphs =
            Self::rich_paragraph_lines(rich_text, xfa_font, default_para, max_width, language);
        let count = paragraphs.len();
        let mut heights: Vec<Num> = Vec::with_capacity(count);

        for (index, paragraph) in paragraphs.iter().enumerate() {
            let props = &paragraph.props;
            let Some(lines) = &paragraph.lines else {
                heights.push(
                    paragraph.font.size
                        + props.space_above.unwrap_or_default()
                        + props.space_below.unwrap_or_default(),
                );
                continue;
            };
            let first_in_block = index == 0;
            let last_in_block = index + 1 == count;
            let metrics_of = |measurer: &mut TextMeasurer, weight: FontWeight, posture: FontPosture| {
                let mut f = paragraph.font.clone();
                f.weight = weight;
                f.posture = posture;
                measurer.get_metrics_for_style(&f).ok()
            };

            // The lines, exactly as drawn: an empty paragraph is one line.
            let line_runs: Vec<(bool, bool)> = if lines.is_empty() {
                vec![(false, false)]
            } else {
                lines
                    .iter()
                    .map(|line| {
                        (
                            line.words.iter().any(|w| w.bold),
                            line.words.iter().any(|w| w.italic),
                        )
                    })
                    .collect()
            };

            let line_count = line_runs.len();
            let mut height = Decimal::ZERO;
            for (i, (has_bold, has_italic)) in line_runs.into_iter().enumerate() {
                let mut line = LineMetrics::new(first_in_block && i == 0, last_in_block && i + 1 == line_count);
                if let Some(m) = metrics_of(&mut measurer, FontWeight::Normal, FontPosture::Normal) {
                    line.accumulate(&m);
                }
                if has_bold && let Some(m) = metrics_of(&mut measurer, FontWeight::Bold, FontPosture::Normal) {
                    line.accumulate(&m);
                }
                if has_italic && let Some(m) = metrics_of(&mut measurer, FontWeight::Normal, FontPosture::Italic) {
                    line.accumulate(&m);
                }
                line.spacing_override = props.line_height;
                if i == 0 {
                    line.margin_top = props.space_above.unwrap_or_default();
                }
                if i + 1 == line_count {
                    line.margin_bottom = props.space_below.unwrap_or_default();
                }
                height += line.full_height();
            }
            heights.push(height);
        }

        heights
    }

    /// Every paragraph of a rich-text block laid out `max_width` wide for
    /// measurement, in document order: the lines `layout_rich_text` wraps
    /// it into, in the faces its runs are drawn in, with the paragraph's own
    /// CSS font and properties applied over the draw's.
    fn rich_paragraph_lines(
        rich_text: &RichText,
        xfa_font: &Font,
        default_para: &Option<Para>,
        max_width: Num,
        language: &str,
    ) -> Vec<MeasuredParagraph> {
        let mut measurer = TextMeasurer::new();
        let hyph_dict = super::xfa::hyphenation::dict_for_language(language);
        rich_text
            .paragraphs
            .iter()
            .map(|para| {
                let mut font = xfa_font.clone();
                Self::apply_paragraph_font(&mut font, para);
                let props = Self::para_for_rich_paragraph(para, default_para);
                let fonts = measurer.run_fonts(&font);
                let Some(regular) = fonts.regular.clone() else {
                    log::warn!(
                        "no font for {:?}: paragraph taken as one line of its font size",
                        font.typeface
                    );
                    return MeasuredParagraph { font, props, lines: None };
                };
                let lines = if para.is_empty || para.runs.iter().all(|r| r.text.trim().is_empty()) {
                    Vec::new()
                } else {
                    // `max_width` is the draw's content width; the paragraph's own
                    // CSS margins are taken off inside `layout_rich_text`.
                    let run_fonts = RunFonts {
                        regular: &regular,
                        bold: fonts.bold.as_ref(),
                        italic: fonts.italic.as_ref(),
                        bold_italic: fonts.bold_italic.as_ref(),
                    };
                    let single = RichText { paragraphs: vec![para.clone()] };
                    let letter_spacing = para
                        .letter_spacing
                        .or_else(|| font.letter_spacing.and_then(|ls| ls.to_f32()))
                        .unwrap_or(0.0);
                    Self::layout_rich_text(
                        &single,
                        max_width.to_f32().unwrap_or(0.0),
                        font.size.to_f32().unwrap_or(10.0),
                        &run_fonts,
                        1.0,
                        letter_spacing,
                        default_para.as_ref().and_then(|p| p.hyphenation.as_ref()),
                        hyph_dict,
                    )
                };
                MeasuredParagraph { font, props, lines: Some(lines) }
            })
            .collect()
    }

    pub fn split_draw_into_paragraph_nodes(
        node: FlattenedNode,
        language: &str,
    ) -> Vec<FlattenedKind> {
        // Extract rich text; if absent or single paragraph, return unchanged
        let rich_text = match node.rich_text() {
            Some(rt) if rt.paragraphs.len() > 1 => rt.clone(),
            _ => {
                // Even for single-paragraph (or absent) rich text, override the
                // node's font weight when the CSS in the rich text runs disagrees
                // with the XFA <font> element.  This mirrors the multi-paragraph
                // logic below and fixes cases where the XFA font says Normal but
                // all rich text runs are bold (or vice versa).
                let mut node = node;
                let single_para = node.rich_text().and_then(|rt| rt.paragraphs.first()).cloned();
                // The paragraph's own CSS font-family and font-size are the
                // font it is drawn and measured in; a run's weight and
                // posture are its own (`RichRun`), so the draw's weight
                // stays as the template set it.
                if let Some(para) = &single_para
                    && let Some(ref mut font) = node.style.font
                {
                    Self::apply_paragraph_font(font, para);
                }
                // Also carry the paragraph's own CSS line-height/margin-top
                // (etc.) onto the node's rendered `<para>`, the same way the
                // multi-paragraph branch below does -- otherwise a
                // single-paragraph draw with `<p style="line-height:...">`
                // renders with the draw's own (possibly very different)
                // line height instead of its paragraph's.
                if let Some(para) = &single_para {
                    let merged = Self::para_for_rich_paragraph(para, &node.style.para);
                    node.style.para = Some(merged);
                }
                return vec![FlattenedKind::Node(node)];
            }
        };

        // Get font info from the node for text measurement
        let (base_font_size, base_font_name) = match &node.kind {
            FlattenedNodeKind::Text {
                font_size,
                font_name,
                ..
            } => (*font_size, font_name.clone()),
            _ => return vec![FlattenedKind::Node(node)],
        };

        let source_name = match &node.kind {
            FlattenedNodeKind::Text { source_name, .. } => source_name.clone(),
            _ => None,
        };

        // Build an XFA Font for text measurement
        let mut xfa_font = node.style.font.clone().unwrap_or_else(|| Font {
            typeface: base_font_name.clone(),
            size: base_font_size,
            ..Font::default()
        });
        xfa_font.typeface = base_font_name.clone();
        xfa_font.size = base_font_size;

        // Get the default para from the node's style for line_height override
        let default_para = node.style.para.clone();

        let paragraph_heights = Self::rich_paragraph_heights(
            &rich_text,
            &xfa_font,
            &default_para,
            node.width,
            language,
        );

        // Determine first/last non-empty paragraph indices.
        // Border edges should be anchored to visible content paragraphs, not
        // to leading/trailing empty paragraphs introduced by rich-text markup.
        let is_non_empty_para =
            |p: &RichParagraph| !p.is_empty && p.runs.iter().any(|r| !r.text.trim().is_empty());
        let first_non_empty_idx = rich_text
            .paragraphs
            .iter()
            .position(is_non_empty_para)
            .unwrap_or(0);
        let last_non_empty_idx = rich_text
            .paragraphs
            .iter()
            .rposition(is_non_empty_para)
            .unwrap_or_else(|| rich_text.paragraphs.len().saturating_sub(1));

        // Now create one FlattenedNode per paragraph, distributing y-positions
        let mut result = Vec::with_capacity(rich_text.paragraphs.len());
        let mut current_y = node.y;

        for (i, para) in rich_text.paragraphs.iter().enumerate() {
            let para_height = paragraph_heights[i];

            // Use per-paragraph font size override if available
            let para_font_size = para
                .font_size
                .map(|s| num(s as f64))
                .unwrap_or(base_font_size);

            // Build the plain text for this paragraph
            let para_text: String = para
                .runs
                .iter()
                .map(|r| r.text.as_str())
                .collect::<Vec<_>>()
                .join("");

            // Create a single-paragraph RichText for this node
            let single_rich_text = RichText {
                paragraphs: vec![para.clone()],
            };

            // The paragraph's own CSS font-family and font-size.
            let mut para_style = node.style.clone();
            if let Some(ref mut font) = para_style.font {
                Self::apply_paragraph_font(font, para);
            }

            // Carry this paragraph's own CSS line-height/margin-top/etc. onto
            // the node it is split into -- the same merge `rich_paragraph_heights`
            // used to size it, so the rendered spacing matches the height the
            // paragraph was given. Without this, every split-off paragraph
            // rendered with the *draw's* own `<para>` (e.g. `Text_FormTitle`'s
            // 22.5pt title line height for an 8pt intro paragraph that has its
            // own `<p style="line-height:9pt;margin-top:13.3pt">`).
            para_style.para = Some(Self::para_for_rich_paragraph(para, &default_para));

            // Per XFA spec, the border is a rectangle around the *entire* draw
            // element. When we split into separate paragraph nodes, each node
            // must only keep the border edges that correspond to its position:
            //   - First paragraph:  keep top, hide bottom
            //   - Middle paragraphs: hide top and bottom
            //   - Last paragraph:   hide top, keep bottom
            // Left and right edges are preserved on all paragraphs.
            if let Some(ref border) = para_style.border {
                let adjusted = if first_non_empty_idx == last_non_empty_idx {
                    // Single non-empty paragraph: keep full border only there.
                    if i == first_non_empty_idx {
                        border.clone()
                    } else {
                        border.with_edges_hidden(&[0, 2])
                    }
                } else if i < first_non_empty_idx || i > last_non_empty_idx {
                    // Leading/trailing empty paragraphs: hide top/bottom edges.
                    border.with_edges_hidden(&[0, 2])
                } else if i == first_non_empty_idx {
                    // First visible paragraph: keep top edge, hide bottom.
                    border.with_edges_hidden(&[2])
                } else if i == last_non_empty_idx {
                    // Last visible paragraph: hide top edge, keep bottom.
                    border.with_edges_hidden(&[0])
                } else {
                    // Middle visible paragraphs: hide top and bottom.
                    border.with_edges_hidden(&[0, 2])
                };
                para_style.border = Some(adjusted);
            }

            let para_node = FlattenedNode::new_text_with_rich_text(
                para_text,
                para_font_size,
                base_font_name.clone(),
                node.x,
                current_y,
                node.width,
                para_height,
                para_style,
                node.rotate,
                source_name.clone(),
                Some(single_rich_text),
            );

            // Copy over non-RichContent hints from the original node
            let mut para_kind = FlattenedKind::Node(para_node);
            for hint in &node.hints {
                if hint.discriminant() != "RichContent" {
                    para_kind.add_hint(hint.clone());
                }
            }

            result.push(para_kind);
            current_y += para_height;
        }

        result
    }

    // ========================================================================
    // XFA-Compliant Glyph-by-Glyph Text Rendering
    // ========================================================================

    /// Render text with proper glyph-by-glyph positioning.
    /// This handles:
    ///
    /// - Justify alignment (distributes extra space between words)
    /// - Text-indent on first line of paragraphs
    /// - Preserved spaces (xfa-spacerun)
    /// - Bold/italic variants for styled text
    ///
    /// Per XFA spec: letterSpacing affects spacing between grapheme clusters
    pub fn render_text_glyph_by_glyph(
        img: &mut RgbaImage,
        lines: &[RenderedLine],
        box_x: i32,
        box_y: i32,
        box_w: i32,
        box_h: i32,
        font_size: f32,
        base_font: &FontRef<'_>,
        bold_font: Option<&FontRef<'_>>,
        italic_font: Option<&FontRef<'_>>,
        bold_italic_font: Option<&FontRef<'_>>,
        color: Rgba<u8>,
        para: &Option<Para>,
        scale: f32,
        letter_spacing: f32,
        geometry: Option<LineGeometry>,
    ) {
        let scaled_font = base_font.as_scaled(xfa_px_scale(base_font, font_size));
        // The space between two words, letter spacing included, as
        // `text_advance` measures it.
        let space_width = space_advance(base_font, font_size, letter_spacing, 1.0);

        // The lines' tops and the block's height, from the AXTE metrics
        // the layout measured with; a block taller than its box sits at the
        // top, as `calculate_text_y` places plain text.
        let v_align = para.as_ref().map(|p| p.v_align).unwrap_or(VAlign::Top);
        let ascent = scaled_font.ascent();
        let descent = scaled_font.descent().abs();
        let (tops, total_height) = Self::line_tops(base_font, font_size, para, scale, lines.len());
        let start_y = box_y as f32
            + if total_height > box_h as f32 {
                0.0
            } else {
                match v_align {
                    VAlign::Top => 0.0,
                    VAlign::Middle => (box_h as f32 - total_height) / 2.0,
                    VAlign::Bottom => box_h as f32 - total_height,
                }
            };

        // Get paragraph margins (node-level defaults, may be overridden per-line)
        let default_margin_left = para
            .as_ref()
            .and_then(|p| p.margin_left)
            .map(|m| m.to_f32().unwrap_or(0.0) * scale)
            .unwrap_or(0.0);
        let default_margin_right = para
            .as_ref()
            .and_then(|p| p.margin_right)
            .map(|m| m.to_f32().unwrap_or(0.0) * scale)
            .unwrap_or(0.0);

        // Render each line
        for (line_idx, line) in lines.iter().enumerate() {
            let line_y = start_y + tops[line_idx];

            if line_y < 0.0 || line_y > img.height() as f32 {
                continue;
            }

            if line.words.is_empty() {
                continue;
            }

            // Per XFA spec §11: para.marginLeft supplies the default for CSS margin-left.
            // Use per-line CSS margin if explicitly set (even if 0), otherwise XFA default.
            let line_margin_left = line.margin_left.unwrap_or(default_margin_left);

            // Calculate effective width for this line (reduced by margins)
            let effective_width = box_w as f32 - line_margin_left - default_margin_right;

            // Calculate available width (considering text-indent for first line)
            let available_width = effective_width - line.text_indent;

            // Determine alignment and spacing
            let (start_x, extra_space) = match line.h_align {
                HAlign::Left => (box_x as f32 + line_margin_left + line.text_indent, 0.0),
                HAlign::Center => {
                    let offset = (available_width - line.content_width) / 2.0;
                    (
                        box_x as f32 + line_margin_left + line.text_indent + offset,
                        0.0,
                    )
                }
                HAlign::Right => {
                    let offset = available_width - line.content_width;
                    (
                        box_x as f32 + line_margin_left + line.text_indent + offset,
                        0.0,
                    )
                }
                HAlign::Justify | HAlign::JustifyAll => {
                    // Only justify if not the last line (unless JustifyAll)
                    if line.is_last_line && line.h_align != HAlign::JustifyAll {
                        // Last line of paragraph - left align
                        (box_x as f32 + line_margin_left + line.text_indent, 0.0)
                    } else if line.words.len() > 1 {
                        // Distribute extra space between words
                        let extra = available_width - line.content_width;
                        let gaps = (line.words.len() - 1) as f32;
                        (
                            box_x as f32 + line_margin_left + line.text_indent,
                            extra / gaps,
                        )
                    } else {
                        (box_x as f32 + line_margin_left + line.text_indent, 0.0)
                    }
                }
                HAlign::Radix => {
                    // Simplified: treat as center
                    let offset = (available_width - line.content_width) / 2.0;
                    (
                        box_x as f32 + line_margin_left + line.text_indent + offset,
                        0.0,
                    )
                }
            };

            // Render words with proper spacing
            let mut x = start_x;
            for (word_idx, word) in line.words.iter().enumerate() {
                // Select the appropriate font variant based on word style
                let word_font: &FontRef<'_> = if word.bold && word.italic {
                    bold_italic_font.unwrap_or(bold_font.unwrap_or(base_font))
                } else if word.bold {
                    bold_font.unwrap_or(base_font)
                } else if word.italic {
                    italic_font.unwrap_or(base_font)
                } else {
                    base_font
                };

                // Render each glyph with letter spacing
                Self::render_glyphs(
                    img,
                    &word.text,
                    x as i32,
                    line_y as i32,
                    font_size,
                    word_font,
                    color,
                    letter_spacing,
                );

                // Advance position (word width already includes letter spacing)
                let word_width =
                    Self::measure_text_width(&word.text, font_size, word_font, letter_spacing);

                // A decorated word's line runs on under the space after it
                // when the next word is decorated the same way, so an
                // underlined phrase is one line rather than one per word.
                if word.decoration != Decoration::default() {
                    let continues = line
                        .words
                        .get(word_idx + 1)
                        .is_some_and(|next| next.decoration == word.decoration);
                    let span = word_width + if continues { space_width + extra_space } else { 0.0 };
                    for (rx, ry, rw, rh) in
                        word.decoration
                            .line_rects(x, line_y, span, ascent, descent, font_size, geometry)
                    {
                        Self::fill_rect(img, rx, ry, rw, rh, color);
                    }
                }
                x += word_width;

                // Add space between words (space_width already includes letter_spacing)
                if word_idx < line.words.len() - 1 {
                    x += space_width + extra_space;
                }
            }
        }
    }

    /// Render individual glyphs for a text string with letter spacing
    /// Per XFA spec: letterSpacing affects spacing between grapheme clusters
    fn render_glyphs(
        img: &mut RgbaImage,
        text: &str,
        x: i32,
        y: i32,
        font_size: f32,
        font: &FontRef<'_>,
        color: Rgba<u8>,
        letter_spacing: f32,
    ) {
        Self::draw_glyphs(img, text, x, y, font_size, font, color, letter_spacing, None);
    }

    /// Check if the value node contains rich text (exData with HTML)
    pub fn has_rich_text_content(children: &[XfaNode]) -> bool {
        for child in children {
            if matches!(child.kind, XfaNodeKind::Value) {
                for value_child in &child.children {
                    if let XfaNodeKind::Element { tag_name, .. } = &value_child.kind
                        && tag_name == "exData"
                    {
                        // Check if it has HTML body content
                        for ex_child in &value_child.children {
                            if let XfaNodeKind::Element {
                                tag_name: inner_tag,
                                ..
                            } = &ex_child.kind
                                && inner_tag == "body"
                            {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }

}

#[cfg(test)]
mod relevant_tests {
    use super::*;

    fn draw(relevant: &str) -> XfaNode {
        let xml = format!(r#"<xdp><draw name="D" relevant="{relevant}"/></xdp>"#);
        XfaNode::parse(xml.as_bytes()).expect("xml")[0].children[0].clone()
    }

    /// XFA 3.3 "The relevant property": `-print` excludes the object from
    /// print; a list naming only other views leaves print out as well.
    #[test]
    fn relevant_is_a_token_list() {
        assert!(Flattened::is_no_print(&draw("-print")));
        assert!(Flattened::is_no_print(&draw("+screen -print")));
        assert!(Flattened::is_no_print(&draw("screen")));
        assert!(!Flattened::is_no_print(&draw("+print")));
        assert!(!Flattened::is_no_print(&draw("print screen")));
        assert!(!Flattened::is_no_print(&draw("-screen")));
        assert!(!Flattened::is_no_print(&draw("")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_text_decoration_sets_clears_and_leaves_lines() {
        let none = Decoration::default();
        let under = Decoration { underline: true, ..Decoration::default() };
        assert_eq!(none.with_css("text-decoration:underline"), under);
        assert_eq!(
            none.with_css("margin-left:2pt; text-decoration: underline line-through"),
            Decoration { underline: true, line_through: true, ..Decoration::default() }
        );
        assert_eq!(under.with_css("text-decoration:none"), none, "none clears");
        assert_eq!(under.with_css("letter-spacing:0in"), under, "no declaration inherits");
        assert_eq!(
            none.with_css("text-decoration:line-through"),
            Decoration { line_through: true, ..Decoration::default() }
        );
        assert_eq!(
            none.with_css("text-decoration: underline double word"),
            Decoration { underline: true, double_underline: true, word_period: true, ..Decoration::default() }
        );
    }

    #[test]
    fn decoration_lines_sit_below_and_across_the_baseline() {
        let both = Decoration { underline: true, line_through: true, ..Decoration::default() };
        // A 28px font with ascent 21 and descent 6, its line box at y=100.
        let rects = both.line_rects(10.0, 100.0, 50.0, 21.0, 6.0, 28.0, None);
        assert_eq!(rects, vec![(10, 123, 50, 2), (10, 114, 50, 2)]);
        assert!(Decoration::default().line_rects(0.0, 0.0, 9.0, 9.0, 3.0, 12.0, None).is_empty());
        // The face's own geometry places and sizes the lines; a double
        // underline adds a second one a thickness further down.
        let geometry = LineGeometry {
            underline_offset: 3.0,
            underline_thickness: 1.0,
            strike_offset: Some(8.0),
            strike_thickness: Some(1.0),
        };
        let double = Decoration { underline: true, double_underline: true, ..Decoration::default() };
        assert_eq!(double.line_rects(10.0, 100.0, 50.0, 21.0, 6.0, 28.0, Some(geometry)), vec![(10, 124, 50, 1), (10, 126, 50, 1)]);
        let strike = Decoration { line_through: true, ..Decoration::default() };
        assert_eq!(strike.line_rects(10.0, 100.0, 50.0, 21.0, 6.0, 28.0, Some(geometry)), vec![(10, 113, 50, 1)]);
    }
    use crate::xfa::font_manager::get_font_manager;
    use crate::xfa::{HAlign, Para, VAlign, XfaNode, XfaNodeKind, num};

    /// Test that line height from para element is properly scaled.
    /// This tests the fix for the "Vereinbarung" title line spacing issue.
    ///
    /// Per XFA spec: lineHeight attribute on para element specifies the line spacing
    /// in points. When rendering, this must be scaled to pixels using the render scale.
    #[test]
    fn test_line_height_scaling() {
        // Needs real font metrics. Upstream got fonts implicitly from a
        // #[cfg(test)] hook that loaded the compiled-in UBS profile; that
        // profile is gone, so the dependency is now explicit.
        crate::fonts::register_test_fonts();
        // Get a font for testing
        let font_manager = get_font_manager();
        let mut mgr = font_manager.lock().unwrap();
        let default_font = crate::xfa::Font::default();
        let font = mgr.get_font(&default_font).unwrap();

        // Create para with lineHeight of 22.5pt (like the T_FormTitle element)
        let para = Some(Para {
            h_align: crate::xfa::HAlign::Left,
            v_align: VAlign::Top,
            line_height: Some(num(22.5)), // 22.5pt line height
            space_above: Some(num(5.0)),  // 5pt space above
            space_below: None,
            text_indent: None,
            margin_left: None,
            margin_right: None,
            ..Default::default()
        });

        // Test with scale=1.0 (1x resolution)
        let font_size_scaled = 18.0; // 18pt font, already scaled
        let render_scale = 1.0;

        // Calculate Y positions for two lines
        let y_line_0 =
            Flattened::calculate_text_y(0, 100, font_size_scaled, &para, &font, 0, 2, render_scale);
        let y_line_1 =
            Flattened::calculate_text_y(0, 100, font_size_scaled, &para, &font, 1, 2, render_scale);

        // Line spacing should be approximately lineHeight (22.5pt) at scale 1.0
        let line_spacing = (y_line_1 - y_line_0) as f32;
        assert!(
            (line_spacing - 22.5).abs() < 1.0,
            "Line spacing at scale 1.0 should be ~22.5, got {}",
            line_spacing
        );

        // Test with scale=2.0 (2x resolution, like Retina)
        let font_size_scaled_2x = 36.0; // 18pt * 2 = 36px
        let render_scale_2x = 2.0;

        let y_line_0_2x = Flattened::calculate_text_y(
            0,
            200,
            font_size_scaled_2x,
            &para,
            &font,
            0,
            2,
            render_scale_2x,
        );
        let y_line_1_2x = Flattened::calculate_text_y(
            0,
            200,
            font_size_scaled_2x,
            &para,
            &font,
            1,
            2,
            render_scale_2x,
        );

        // Line spacing should be approximately lineHeight * scale (22.5 * 2 = 45px) at scale 2.0
        let line_spacing_2x = (y_line_1_2x - y_line_0_2x) as f32;
        assert!(
            (line_spacing_2x - 45.0).abs() < 2.0,
            "Line spacing at scale 2.0 should be ~45, got {}",
            line_spacing_2x
        );
    }

    /// Test that space_above from para element is properly scaled.
    #[test]
    fn test_space_above_scaling() {
        // Needs real font metrics. Upstream got fonts implicitly from a
        // #[cfg(test)] hook that loaded the compiled-in UBS profile; that
        // profile is gone, so the dependency is now explicit.
        crate::fonts::register_test_fonts();
        let font_manager = get_font_manager();
        let mut mgr = font_manager.lock().unwrap();
        let default_font = crate::xfa::Font::default();
        let font = mgr.get_font(&default_font).unwrap();

        // Create para with spaceAbove of 10pt
        let para_with_space = Some(Para {
            h_align: crate::xfa::HAlign::Left,
            v_align: VAlign::Top,
            line_height: None,
            space_above: Some(num(10.0)), // 10pt space above
            space_below: None,
            text_indent: None,
            margin_left: None,
            margin_right: None,
            ..Default::default()
        });

        let para_without_space = Some(Para {
            h_align: crate::xfa::HAlign::Left,
            v_align: VAlign::Top,
            line_height: None,
            space_above: None,
            space_below: None,
            text_indent: None,
            margin_left: None,
            margin_right: None,
            ..Default::default()
        });

        let font_size = 12.0;
        let render_scale = 2.0;

        // Y with space_above at 2x scale
        let y_with_space = Flattened::calculate_text_y(
            0,
            100,
            font_size * render_scale,
            &para_with_space,
            &font,
            0,
            1,
            render_scale,
        );
        let y_without_space = Flattened::calculate_text_y(
            0,
            100,
            font_size * render_scale,
            &para_without_space,
            &font,
            0,
            1,
            render_scale,
        );

        // Difference should be space_above * scale = 10 * 2 = 20 pixels
        let space_diff = (y_with_space - y_without_space) as f32;
        assert!(
            (space_diff - 20.0).abs() < 1.0,
            "Space above at scale 2.0 should add ~20px, got {}",
            space_diff
        );
    }

    #[test]
    fn test_parse_rich_text_spacerun_preserves_non_space_characters() {
        let mut span_attrs = std::collections::HashMap::new();
        span_attrs.insert("style".to_string(), "xfa-spacerun:yes".to_string());
        let span = XfaNode {
            children: vec![],
            ..XfaNode::new(
                XfaNodeKind::Element {
                    tag_name: "span".to_string(),
                    text_content: Some("AB  CD".to_string()),
                },
                span_attrs,
            )
        };

        let body = XfaNode {
            children: vec![span],
            ..XfaNode::new(
                XfaNodeKind::Element {
                    tag_name: "body".to_string(),
                    text_content: None,
                },
                std::collections::HashMap::new(),
            )
        };

        let rich =
            Flattened::parse_rich_text_from_html(&[body], HAlign::Left, None, None, false, false, Decoration::default(), 10.0);

        let text: String = rich
            .paragraphs
            .iter()
            .flat_map(|p| p.runs.iter())
            .map(|r| r.text.clone())
            .collect();
        assert_eq!(text, "AB  CD");
    }

    #[test]
    fn test_parse_rich_text_bold_not_inherited_between_paragraphs() {
        // Test that bold styling on one <p> does NOT leak to the next <p>
        // This reproduces the AAIR bug where "Es ist der Bank..." was incorrectly bold

        // Create first paragraph with font-weight:bold
        let mut p1_attrs = std::collections::HashMap::new();
        p1_attrs.insert(
            "style".to_string(),
            "font-weight:bold;letter-spacing:0in".to_string(),
        );
        let p1 = XfaNode {
            children: vec![XfaNode::new(
                XfaNodeKind::Text {
                    content: "Bold heading".to_string(),
                },
                std::collections::HashMap::new(),
            )],
            ..XfaNode::new(
                XfaNodeKind::Element {
                    tag_name: "p".to_string(),
                    text_content: None,
                },
                p1_attrs,
            )
        };

        // Create second paragraph WITHOUT font-weight (should NOT be bold)
        let mut p2_attrs = std::collections::HashMap::new();
        p2_attrs.insert("style".to_string(), "letter-spacing:0in".to_string());
        let p2 = XfaNode {
            children: vec![XfaNode::new(
                XfaNodeKind::Text {
                    content: "Normal body text".to_string(),
                },
                std::collections::HashMap::new(),
            )],
            ..XfaNode::new(
                XfaNodeKind::Element {
                    tag_name: "p".to_string(),
                    text_content: None,
                },
                p2_attrs,
            )
        };

        // Create body containing both paragraphs
        let body = XfaNode {
            children: vec![p1, p2],
            ..XfaNode::new(
                XfaNodeKind::Element {
                    tag_name: "body".to_string(),
                    text_content: None,
                },
                std::collections::HashMap::new(),
            )
        };

        let rich = Flattened::parse_rich_text_from_html(
            &[body],
            HAlign::Left,
            None,
            None,
            false, // default_bold = false
            false,
            Decoration::default(),
            10.0,
        );

        assert_eq!(rich.paragraphs.len(), 2, "Should have 2 paragraphs");

        // First paragraph should be bold
        let p1_runs = &rich.paragraphs[0].runs;
        assert!(!p1_runs.is_empty(), "First paragraph should have runs");
        assert!(
            p1_runs[0].bold,
            "First paragraph text should be bold (has font-weight:bold)"
        );

        // Second paragraph should NOT be bold
        let p2_runs = &rich.paragraphs[1].runs;
        assert!(!p2_runs.is_empty(), "Second paragraph should have runs");
        assert!(
            !p2_runs[0].bold,
            "Second paragraph text should NOT be bold (no font-weight in style)"
        );
    }

    #[test]
    fn test_parse_rich_text_span_font_weight_bold() {
        // Test that <span style="font-weight:bold"> correctly sets bold on its text

        let mut span_attrs = std::collections::HashMap::new();
        span_attrs.insert("style".to_string(), "font-weight:bold".to_string());
        let bold_span = XfaNode {
            children: vec![XfaNode::new(
                XfaNodeKind::Text {
                    content: "bold_word".to_string(),
                },
                std::collections::HashMap::new(),
            )],
            ..XfaNode::new(
                XfaNodeKind::Element {
                    tag_name: "span".to_string(),
                    text_content: None,
                },
                span_attrs,
            )
        };

        let p = XfaNode {
            children: vec![
                XfaNode::new(
                    XfaNodeKind::Text {
                        content: "prefix ".to_string(),
                    },
                    std::collections::HashMap::new(),
                ),
                bold_span,
                XfaNode::new(
                    XfaNodeKind::Text {
                        content: " suffix".to_string(),
                    },
                    std::collections::HashMap::new(),
                ),
            ],
            ..XfaNode::new(
                XfaNodeKind::Element {
                    tag_name: "p".to_string(),
                    text_content: None,
                },
                std::collections::HashMap::new(),
            )
        };

        let body = XfaNode {
            children: vec![p],
            ..XfaNode::new(
                XfaNodeKind::Element {
                    tag_name: "body".to_string(),
                    text_content: None,
                },
                std::collections::HashMap::new(),
            )
        };

        let rich =
            Flattened::parse_rich_text_from_html(&[body], HAlign::Left, None, None, false, false, Decoration::default(), 10.0);

        assert_eq!(rich.paragraphs.len(), 1);
        let runs = &rich.paragraphs[0].runs;

        let bold_run = runs.iter().find(|r| r.text.contains("bold_word"));
        assert!(bold_run.is_some(), "Should find bold_word run");
        assert!(bold_run.unwrap().bold, "bold_word should be bold");

        let prefix_run = runs.iter().find(|r| r.text.contains("prefix"));
        if let Some(run) = prefix_run {
            assert!(!run.bold, "prefix should not be bold");
        }
    }

    /// A tag `parse_html_nodes_to_rich_text` has no dedicated case for (e.g.
    /// `<a href="...">dl-pb-italy-bm</a>`) falls into the generic "unknown
    /// element" arm. The real XML parser (`XfaNode::parse_element_content`)
    /// sets *both* that element's aggregated `text_content` and a `Text`
    /// child with the same string, to preserve DOM order among sibling
    /// elements -- exactly like the `<p>` case just above, which already
    /// guards against reading both. Before this fix the fallback arm read
    /// both unconditionally, duplicating the element's text.
    #[test]
    fn test_unknown_inline_element_does_not_duplicate_its_text() {
        let link = XfaNode {
            children: vec![XfaNode::new(
                XfaNodeKind::Text {
                    content: "dl-pb-italy-bm".to_string(),
                },
                std::collections::HashMap::new(),
            )],
            ..XfaNode::new(
                XfaNodeKind::Element {
                    tag_name: "a".to_string(),
                    text_content: Some("dl-pb-italy-bm".to_string()),
                },
                std::collections::HashMap::new(),
            )
        };

        let p = XfaNode {
            children: vec![
                XfaNode::new(
                    XfaNodeKind::Text {
                        content: "DL: ".to_string(),
                    },
                    std::collections::HashMap::new(),
                ),
                link,
                XfaNode::new(
                    XfaNodeKind::Text {
                        content: ".".to_string(),
                    },
                    std::collections::HashMap::new(),
                ),
            ],
            ..XfaNode::new(
                XfaNodeKind::Element {
                    tag_name: "p".to_string(),
                    text_content: None,
                },
                std::collections::HashMap::new(),
            )
        };

        let body = XfaNode {
            children: vec![p],
            ..XfaNode::new(
                XfaNodeKind::Element {
                    tag_name: "body".to_string(),
                    text_content: None,
                },
                std::collections::HashMap::new(),
            )
        };

        let rich =
            Flattened::parse_rich_text_from_html(&[body], HAlign::Left, None, None, false, false, Decoration::default(), 10.0);

        assert_eq!(rich.paragraphs.len(), 1);
        let joined: String = rich.paragraphs[0]
            .runs
            .iter()
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(
            joined.matches("dl-pb-italy-bm").count(),
            1,
            "the <a> element's text should appear exactly once, got {joined:?}"
        );
        assert!(
            joined.starts_with("DL:") && joined.ends_with('.'),
            "the text around the link should be unaffected, got {joined:?}"
        );
    }

    /// Test that tokenization correctly separates bold words across space-only runs.
    /// This regression test ensures that when a space-only run separates a normal word
    /// from a bold word, the bold word becomes its own token with the bold flag preserved.
    #[test]
    fn test_tokenize_paragraph_runs_preserves_bold_across_space_run() {
        // Needs real font metrics. Upstream got fonts implicitly from a
        // #[cfg(test)] hook that loaded the compiled-in UBS profile; that
        // profile is gone, so the dependency is now explicit.
        crate::fonts::register_test_fonts();
        // Simulate: "normal " + " " + "bold " where middle run is space-only
        let runs = vec![
            RichRun {
                text: "normal".to_string(),
                bold: false,
                italic: false,
                underline: false,
                superscript: false,
                preserve_spaces: false,
                letter_spacing: None,
                line_through: false,
            },
            RichRun {
                text: " ".to_string(), // Space-only run
                bold: false,
                italic: false,
                underline: false,
                superscript: false,
                preserve_spaces: false,
                letter_spacing: None,
                line_through: false,
            },
            RichRun {
                text: "bold ".to_string(),
                bold: true,
                italic: false,
                underline: false,
                superscript: false,
                preserve_spaces: false,
                letter_spacing: None,
                line_through: false,
            },
            RichRun {
                text: "suffix".to_string(),
                bold: false,
                italic: false,
                underline: false,
                superscript: false,
                preserve_spaces: false,
                letter_spacing: None,
                line_through: false,
            },
        ];

        // Get a font from the font manager (same approach as other tests)
        let font_manager = get_font_manager();
        let mut mgr = font_manager.lock().unwrap();
        let default_xfa_font = crate::xfa::Font::default();
        let font = mgr.get_font(&default_xfa_font).unwrap();

        let tokens =
            Flattened::tokenize_paragraph_runs(&runs, 12.0, &RunFonts::only(&font), 0.0);

        // Should have 3 tokens: "normal", "bold", "suffix"
        assert_eq!(
            tokens.len(),
            3,
            "Expected 3 tokens: {:?}",
            tokens.iter().map(|t| &t.text).collect::<Vec<_>>()
        );

        // First token: "normal", not bold
        assert_eq!(tokens[0].text, "normal");
        assert!(!tokens[0].bold, "normal should not be bold");

        // Second token: "bold", SHOULD be bold
        assert_eq!(tokens[1].text, "bold");
        assert!(tokens[1].bold, "bold should be bold");

        // Third token: "suffix", not bold
        assert_eq!(tokens[2].text, "suffix");
        assert!(!tokens[2].bold, "suffix should not be bold");
    }

    /// A run marked bold must be measured with the bold face, not the
    /// paragraph's base font drawn bold in the renderer's mind only: if
    /// wrapping measures every run with `regular` regardless of `run.bold`
    /// (as `layout_rich_text` used to), a line that measures as fitting can
    /// render wider than the column once its bold words draw in their real,
    /// wider glyphs -- the AAOV "Informazioni sulla banca" heading overflow.
    #[test]
    fn tokenize_paragraph_runs_measures_a_bold_run_with_the_bold_face() {
        crate::fonts::register_test_fonts();
        let font_manager = get_font_manager();
        let mut mgr = font_manager.lock().unwrap();
        // "Frutiger 45 Light" (not the default `Font`'s "Courier", which
        // does not match any Frutiger face by family and so resolves both
        // weights to the very same fallback file) -- the family AAOV's own
        // template asks for.
        let regular_style = crate::xfa::Font {
            typeface: "Frutiger 45 Light".to_string(),
            ..crate::xfa::Font::default()
        };
        let bold_style = crate::xfa::Font {
            weight: crate::xfa::FontWeight::Bold,
            ..regular_style.clone()
        };
        let regular = mgr.get_font(&regular_style).expect("regular font");
        let bold = mgr.get_font(&bold_style).expect("bold font");
        drop(mgr);

        let text = "Informazioni sulla banca";
        let run = |bold: bool| {
            vec![RichRun {
                text: text.to_string(),
                bold,
                italic: false,
                underline: false,
                superscript: false,
                preserve_spaces: false,
                letter_spacing: None,
                line_through: false,
            }]
        };
        let fonts = RunFonts {
            regular: &regular,
            bold: Some(&bold),
            italic: None,
            bold_italic: None,
        };

        let regular_width: f32 = Flattened::tokenize_paragraph_runs(&run(false), 12.0, &fonts, 0.0)
            .iter()
            .map(|t| t.width)
            .sum();
        let bold_width: f32 = Flattened::tokenize_paragraph_runs(&run(true), 12.0, &fonts, 0.0)
            .iter()
            .map(|t| t.width)
            .sum();

        assert!(
            bold_width > regular_width,
            "a run marked bold must measure wider (the bold face's own glyphs), \
             got regular={regular_width} bold={bold_width}"
        );
    }

    // ── slice_into_pages ─────────────────────────────────────────────────────

    /// Build a minimal `Flattened` of `page_count` pages of the given size.
    fn flattened_with_pages(width: f64, height: f64, page_count: u32) -> Flattened {
        Flattened::new(Page::uniform(num(width), num(height), page_count), vec![])
    }

    fn blank(width: u32, height: u32) -> RgbaImage {
        ImageBuffer::from_pixel(width, height, Rgba([255u8, 255u8, 255u8, 255u8]))
    }

    #[test]
    fn slice_splits_the_column_into_whole_pages() {
        // Two 50-tall pages stacked into a 100-tall buffer.
        let f = flattened_with_pages(100.0, 50.0, 2);
        let pages = f.slice_into_pages(blank(100, 100), 1.0);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].height(), 50);
        assert_eq!(pages[1].height(), 50);
        assert_eq!(pages.iter().map(|p| p.height()).sum::<u32>(), 100);
        assert!(pages.iter().all(|p| p.width() == 100));
    }

    #[test]
    fn slice_single_page_returns_unchanged() {
        let f = flattened_with_pages(100.0, 50.0, 1);
        let pages = f.slice_into_pages(blank(100, 50), 1.0);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].height(), 50);
    }

    /// A buffer shorter than the layout claims still yields whole pages up to
    /// where it ends, rather than an out-of-range crop.
    #[test]
    fn slice_clips_to_a_short_buffer() {
        let f = flattened_with_pages(100.0, 50.0, 3);
        let pages = f.slice_into_pages(blank(100, 120), 1.0);
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0].height(), 50);
        assert_eq!(pages[1].height(), 50);
        assert_eq!(pages[2].height(), 20);
    }

    #[test]
    fn slice_honors_scale() {
        // At scale 2.0 a 50pt page is 100 pixel rows of a 200-tall buffer.
        let f = flattened_with_pages(100.0, 50.0, 2);
        let pages = f.slice_into_pages(blank(200, 200), 2.0);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].height(), 100);
        assert_eq!(pages[1].height(), 100);
    }

    // ── split_draw_into_paragraph_nodes: per-paragraph CSS line-height ────────

    fn plain_run(text: &str) -> RichRun {
        RichRun {
            text: text.to_string(),
            bold: false,
            italic: false,
            underline: false,
            superscript: false,
            preserve_spaces: false,
            letter_spacing: None,
            line_through: false,
        }
    }

    /// A draw's own `<para lineHeight="22.5pt">` (like AAJB's `Text_FormTitle`,
    /// an 18pt title) must not leak onto a paragraph that carries its own CSS
    /// `<p style="line-height:9pt;margin-top:13.3pt">` (that title's 8pt intro
    /// sentence) -- reported as "wrong spacing between lines" on AAJB page 1.
    #[test]
    fn split_draw_into_paragraph_nodes_carries_each_paragraphs_own_css_line_height() {
        crate::fonts::register_test_fonts();
        let title_para = RichParagraph {
            runs: vec![plain_run("Title line")],
            h_align: HAlign::Left,
            ..RichParagraph::default()
        };
        let intro_para = RichParagraph {
            runs: vec![plain_run("Intro paragraph text.")],
            h_align: HAlign::Left,
            line_height: Some(9.0),
            space_above: Some(13.3),
            font_size: Some(8.0),
            ..RichParagraph::default()
        };
        let rich_text = RichText {
            paragraphs: vec![title_para, intro_para],
        };

        let style = RenderStyle {
            font: Some(crate::xfa::Font {
                typeface: "Frutiger 45 Light".to_string(),
                size: num(18.0),
                ..crate::xfa::Font::default()
            }),
            para: Some(Para {
                h_align: HAlign::Left,
                v_align: VAlign::Top,
                line_height: Some(num(22.5)),
                ..Para::default()
            }),
            ..RenderStyle::default()
        };

        let node = FlattenedNode::new_text_with_rich_text(
            "Title line Intro paragraph text.".to_string(),
            num(18.0),
            "Frutiger 45 Light".to_string(),
            num(0.0),
            num(0.0),
            num(500.0),
            num(50.0),
            style,
            0,
            None,
            Some(rich_text),
        );

        let split = Flattened::split_draw_into_paragraph_nodes(node, "en");
        assert_eq!(split.len(), 2, "two paragraphs should split into two nodes");

        let para_of = |kind: &FlattenedKind| match kind {
            FlattenedKind::Node(n) => n.style.para.clone(),
            _ => None,
        };

        let title_line_height = para_of(&split[0]).and_then(|p| p.line_height);
        assert_eq!(
            title_line_height,
            Some(num(22.5)),
            "the title paragraph (no CSS override) should keep the draw's own \
             22.5pt line height, got {title_line_height:?}"
        );

        let intro_para_out = para_of(&split[1]);
        let intro_line_height = intro_para_out
            .as_ref()
            .and_then(|p| p.line_height)
            .and_then(|n| n.to_f32());
        let intro_space_above = intro_para_out
            .as_ref()
            .and_then(|p| p.space_above)
            .and_then(|n| n.to_f32());
        assert!(
            intro_line_height.is_some_and(|v| (v - 9.0).abs() < 0.01),
            "the intro paragraph's own CSS line-height:9pt must override the \
             draw's 22.5pt, got {intro_line_height:?}"
        );
        assert!(
            intro_space_above.is_some_and(|v| (v - 13.3).abs() < 0.01),
            "the intro paragraph's own CSS margin-top:13.3pt must be its \
             space_above, got {intro_space_above:?}"
        );
    }

    /// The single-paragraph early-return path must carry the same per-paragraph
    /// CSS override -- a draw with exactly one `<p style="line-height:...">`
    /// never reaches the multi-paragraph loop above.
    #[test]
    fn split_draw_into_paragraph_nodes_carries_css_line_height_for_a_single_paragraph_draw() {
        crate::fonts::register_test_fonts();
        let para = RichParagraph {
            runs: vec![plain_run("Solo paragraph")],
            h_align: HAlign::Left,
            line_height: Some(9.0),
            space_above: Some(13.3),
            ..RichParagraph::default()
        };
        let rich_text = RichText {
            paragraphs: vec![para],
        };

        let style = RenderStyle {
            font: Some(crate::xfa::Font {
                typeface: "Frutiger 45 Light".to_string(),
                size: num(18.0),
                ..crate::xfa::Font::default()
            }),
            para: Some(Para {
                h_align: HAlign::Left,
                v_align: VAlign::Top,
                line_height: Some(num(22.5)),
                ..Para::default()
            }),
            ..RenderStyle::default()
        };

        let node = FlattenedNode::new_text_with_rich_text(
            "Solo paragraph".to_string(),
            num(18.0),
            "Frutiger 45 Light".to_string(),
            num(0.0),
            num(0.0),
            num(500.0),
            num(50.0),
            style,
            0,
            None,
            Some(rich_text),
        );

        let split = Flattened::split_draw_into_paragraph_nodes(node, "en");
        assert_eq!(split.len(), 1);
        let para_out = match &split[0] {
            FlattenedKind::Node(n) => n.style.para.clone(),
            _ => None,
        };
        let line_height = para_out.as_ref().and_then(|p| p.line_height);
        assert_eq!(
            line_height,
            Some(num(9.0)),
            "single-paragraph draw must also pick up its own CSS line-height, \
             got {line_height:?}"
        );
    }
}
