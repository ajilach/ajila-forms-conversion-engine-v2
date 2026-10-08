use crate::xfa::font_manager::get_font_manager;
// DEVIATION: upstream imports Presence via `scripting::{Presence, SomPath}`,
// which routes through scripting/state.rs and pulls in boa_engine. Presence is
// actually defined in xfa/mod.rs, and SomPath in scripting/som.rs — neither
// needs a JS engine. Importing them directly keeps the whole scripting tree
// out of the render build.
use crate::draw_text::draw_text_mut;
use crate::xfa::Presence;
use crate::xfa::scripting::som::SomPath;
use crate::xfa::text_metrics::{TextMeasurer, xfa_px_scale};
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
    /// Document language (from XFA locale, e.g. "de", "en").
    pub language: String,
}

// ============================================================================
// Recursive Flattened Structure
// ============================================================================

/// A flattened element: either a group (with hints) or a leaf node.
/// Groups can be nested recursively and carry semantic hints.
/// Leaf nodes contain actual layout information (position, dimensions, content).
#[derive(Debug, Clone)]
pub enum FlattenedKind {
    /// A group of elements with optional hints.
    /// Groups are created when an XFA node has an <occur> element (repeatable section).
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

#[derive(Clone)]
pub struct Page {
    pub width: Num,
    pub height: Num,
    /// Y offset of the content area within the page (from XFA contentArea element).
    /// For PDF-sourced documents this is 0.
    pub content_area_y: Num,
    /// Height of the content area within the page (from XFA contentArea element).
    /// For PDF-sourced documents this equals `height`.
    pub content_area_height: Num,
    /// Pre-computed Y positions where page breaks occur in the flattened coordinate space.
    /// Since the layout is a stack of whole pages these are the multiples of
    /// [`Page::height`] strictly inside the column, kept because they are the
    /// long-standing way to ask where the pages end.
    pub page_breaks: Vec<Num>,
    /// How many pages the layout produced. The flattened column is exactly this
    /// many pages tall, whatever the last page's content ends at, so a short
    /// final page is still a whole page.
    pub page_count: u32,
}

impl Page {
    /// Create a single-page Page where the content area spans the full page.
    pub fn new(width: Num, height: Num) -> Self {
        Page {
            width,
            height,
            content_area_y: Decimal::ZERO,
            content_area_height: height,
            page_breaks: Vec::new(),
            page_count: 1,
        }
    }

    /// The height of the whole flattened column: every page, at full height.
    pub fn column_height(&self) -> Num {
        self.height * Decimal::from(self.page_count.max(1) as u64)
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
    /// Field values, keyed both by leaf name and by full SOM path.
    pub values: HashMap<SomPath, String>,
    /// Presence, keyed both by leaf name and by full SOM path.
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
    /// Fill the box, keeping the aspect ratio and cropping the overflow.
    Actual,
    /// Stretch to the box, ignoring the aspect ratio.
    None,
}

impl std::str::FromStr for ImageAspect {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "none" => ImageAspect::None,
            "actual" | "height" | "width" => ImageAspect::Actual,
            _ => ImageAspect::Fit,
        })
    }
}

// ============================================================================
// Format-Agnostic Semantic Hints
// ============================================================================

/// Field access level per XFA specification.
/// Controls user interaction capabilities with a field or container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "open" => FieldAccess::Open,
            "nonInteractive" => FieldAccess::NonInteractive,
            "protected" => FieldAccess::Protected,
            "readOnly" => FieldAccess::ReadOnly,
            _ => FieldAccess::Open, // Default per XFA spec
        })
    }
}

impl FieldAccess {
    /// Returns true if this access level allows user interaction/input.
    pub fn is_interactive(&self) -> bool {
        matches!(self, FieldAccess::Open)
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

    /// Caption/label information
    Caption {
        /// Position relative to field (left, right, top, bottom, inline)
        placement: CaptionPlacement,
        /// Caption text content
        text: Option<String>,
    },

    /// Occurrence constraints for repeatable sections
    Occurrence {
        /// Minimum occurrences
        min: u32,
        /// Maximum occurrences (None = unlimited)
        max: Option<u32>,
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

    /// The symbology of a barcode field, from `<ui><barcode type>`.
    BarcodeSymbology(String),

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
            Hint::Caption { .. } => "Caption",
            Hint::Occurrence { .. } => "Occurrence",
            Hint::LayoutBreak { .. } => "LayoutBreak",
            Hint::RichContent(_) => "RichContent",
            Hint::DataBinding { .. } => "DataBinding",
            Hint::MasterPage { .. } => "MasterPage",
            Hint::NoPrint => "NoPrint",
            Hint::SomPath(_) => "SomPath",
            Hint::BarcodeSymbology(_) => "BarcodeSymbology",
            Hint::ExclGroupSomPath(_) => "ExclGroupSomPath",
            Hint::Dropdown { .. } => "Dropdown",
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

    /// Get tight bounds around the actual rendered text content.
    ///
    /// For `Text` nodes this uses font metrics to compute the real text width
    /// and height, then applies horizontal / vertical alignment to position the
    /// text rectangle within the element box.  For `Field` nodes (and any node
    /// without text content) the full element bounds are returned unchanged.
    pub fn text_bounds(&self) -> Bounds {
        let content = match &self.kind {
            FlattenedNodeKind::Text { content, .. } if !content.is_empty() => content.as_str(),
            _ => return self.bounds(),
        };

        let font = &self.style.font;
        let para = &self.style.para;

        // --- extract paragraph properties first --------------------------------
        let h_align = para
            .as_ref()
            .map(|p| p.h_align)
            .unwrap_or(crate::xfa::HAlign::Left);
        let margin_left = para
            .as_ref()
            .and_then(|p| p.margin_left)
            .unwrap_or(Decimal::ZERO);
        let margin_right = para
            .as_ref()
            .and_then(|p| p.margin_right)
            .unwrap_or(Decimal::ZERO);
        let available_width = (self.width - margin_left - margin_right).max(Decimal::ONE);

        // --- measure text block ------------------------------------------------
        let mut measurer = TextMeasurer::new();
        let metrics = match measurer.measure_text_block(content, font, para, available_width) {
            Ok(m) => m,
            Err(_) => return self.approximate_text_bounds(content),
        };

        let text_width = metrics.total_width;
        let text_height = metrics.total_height;

        // --- horizontal placement ----------------------------------------------
        let text_x = match h_align {
            crate::xfa::HAlign::Left
            | crate::xfa::HAlign::Justify
            | crate::xfa::HAlign::JustifyAll => self.x + margin_left,
            crate::xfa::HAlign::Center => self.x + (self.width - text_width) / Decimal::TWO,
            crate::xfa::HAlign::Right => self.x + self.width - text_width - margin_right,
            crate::xfa::HAlign::Radix => self.x + self.width / Decimal::TWO,
        };

        // Use measured text width, capped at available width.
        let effective_width = text_width.min(available_width);

        // --- vertical placement ------------------------------------------------
        let v_align = para
            .as_ref()
            .map(|p| p.v_align)
            .unwrap_or(crate::xfa::VAlign::Top);
        let v_offset = metrics.first_line_offset(self.height, v_align);
        let text_y = self.y + v_offset;

        Bounds::new(text_x, text_y, effective_width, text_height)
    }

    /// Approximate text bounds when font metrics are unavailable.
    fn approximate_text_bounds(&self, content: &str) -> Bounds {
        let font_size = self
            .style
            .font
            .as_ref()
            .map(|f| f.size)
            .unwrap_or(num(10.0));
        let char_width = font_size * num(0.6);
        let text_width = char_width * Decimal::from(content.chars().count() as u32);
        let text_width = text_width.min(self.width);

        let line_height = font_size * num(1.2);
        let chars_per_line = (self.width / char_width).to_u32().unwrap_or(1).max(1) as usize;
        // Simple word-wrap estimate
        let mut num_lines: usize = 1;
        let mut current_len: usize = 0;
        for word in content.split_whitespace() {
            let wlen = word.chars().count();
            if current_len + wlen > chars_per_line && current_len > 0 {
                num_lines += 1;
                current_len = wlen;
            } else {
                current_len += wlen + if current_len > 0 { 1 } else { 0 };
            }
        }
        let text_height = (line_height * Decimal::from(num_lines as u32)).min(self.height);

        let margin_left = self
            .style
            .para
            .as_ref()
            .and_then(|p| p.margin_left)
            .unwrap_or(Decimal::ZERO);
        Bounds::new(
            self.x + margin_left,
            self.y,
            text_width.min(self.width - margin_left),
            text_height,
        )
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

    /// The barcode symbology this node carries, if it is a barcode field.
    pub fn barcode_symbology(&self) -> Option<&str> {
        self.hints.iter().find_map(|h| match h {
            Hint::BarcodeSymbology(s) => Some(s.as_str()),
            _ => None,
        })
    }

    /// Check if this node is interactive based on FieldBehavior hint
    pub fn is_interactive(&self) -> bool {
        self.field_behavior()
            .map(|(access, _, _, _)| access.is_interactive())
            .unwrap_or(true) // Default to interactive if no hint
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
///
/// Occurrence constraints for repeatable sections (from XFA <occur> element)
/// Per XFA 3.3 spec (Chapter 9, "The Occur Element"):
/// - min: minimum number of copies required (defaults to 1)
/// - max: maximum number of copies permitted (-1 = unlimited, defaults to min)
/// - initial: starting copies during empty merge (defaults to min)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OccurConstraints {
    pub min: u32,
    pub max: Option<u32>, // None = unlimited (-1 in XFA)
    pub initial: u32,
}

impl Default for OccurConstraints {
    fn default() -> Self {
        OccurConstraints {
            min: 1,
            max: Some(1),
            initial: 1,
        }
    }
}

impl OccurConstraints {
    /// Returns true if this is a repeatable section (can have more than one instance)
    pub fn is_repeatable(&self) -> bool {
        self.max.map(|m| m > 1).unwrap_or(true)
    }

    /// Returns true if at least one instance should exist initially.
    /// Per XFA spec, `initial` specifies how many instances to create when the form loads.
    /// If initial == 0, no instances exist until the user adds them.
    pub fn has_initial_instances(&self) -> bool {
        self.initial > 0
    }
}

/// Bundles all state needed during the recursive flattening process:
/// - Embed resolution data (computed_values, id_to_field) for xfa:embed references
/// - Inherited presence from parent containers (inherited_presence)
/// - Occurrence constraints from parent repeatable sections
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
    /// Occurrence constraints from parent repeatable section (if any)
    /// Used to attach Hint::Occurrence to first child node
    pub pending_occur: Option<OccurConstraints>,
    /// Hints inherited from parent nodes that should be applied to all descendants
    /// Per XFA spec, certain attributes like `relevant` are inherited by descendants
    pub inherited_hints: Vec<Hint>,
    /// Document language for hyphenation dictionary lookup
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
    /// Where to record `keep`/`breakBefore`/`breakAfter` constraints seen
    /// while laying out `layout="tb"` children, in absolute column
    /// coordinates (XFA 3.3 §7 "Break Conditions", §8 "Content Splitting").
    /// `Some` only for the single content column `from_xfa_paged_with` is
    /// about to page-break; a shared `RefCell` rather than a return value
    /// because these are found arbitrarily deep in the recursion, at every
    /// nesting level whose own layout is "tb", not just at the top.
    pub break_markers: Option<&'a RefCell<Vec<BreakMarker>>>,
}

/// One child of a `layout="tb"` container, in absolute column coordinates,
/// carrying whatever `keep`/`breakBefore`/`breakAfter` it declared. See
/// [`FlattenContext::break_markers`].
#[derive(Debug, Clone, Copy)]
pub struct BreakMarker {
    pub top: Num,
    pub bottom: Num,
    /// `keep intact="contentArea|pageArea"`: this child must not be split.
    pub intact: bool,
    /// `keep next="contentArea|pageArea"`: this child and the next sibling
    /// must land in the same content area.
    pub keep_next: bool,
    /// `keep previous="contentArea|pageArea"`: this child and the previous
    /// sibling must land in the same content area.
    pub keep_previous: bool,
    /// `breakBefore` with a `targetType` other than `auto` (the default):
    /// this child must start a new content area or page.
    pub force_before: bool,
    /// `breakAfter` with a `targetType` other than `auto`: whatever follows
    /// this child must start a new content area or page.
    pub force_after: bool,
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
            pending_occur: None,
            inherited_hints: Vec::new(),
            language: String::new(),
            table_column_widths: None,
            page_overrides: None,
            break_markers: None,
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
            pending_occur: None,
            inherited_hints: Vec::new(),
            language: String::new(),
            table_column_widths: None,
            page_overrides: None,
            break_markers: None,
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
            pending_occur: None,
            inherited_hints: Vec::new(),
            language: String::new(),
            table_column_widths: None,
            page_overrides: None,
            break_markers: None,
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
            pending_occur: self.pending_occur,
            inherited_hints: self.inherited_hints.clone(),
            language: self.language.clone(),
            table_column_widths: self.table_column_widths.clone(),
            page_overrides: self.page_overrides,
            break_markers: self.break_markers,
        }
    }

    /// Create a child context with resolved table column widths.
    pub fn with_table_column_widths(&self, widths: Option<Vec<Num>>) -> FlattenContext<'a> {
        let mut ctx = self.derive();
        ctx.table_column_widths = widths;
        ctx
    }

    /// Create a child context with occurrence constraints from a repeatable section
    /// The occur hint will be attached to the first content node created
    pub fn with_occur_constraints(&self, occur: OccurConstraints) -> FlattenContext<'a> {
        let mut ctx = self.derive();
        // Only propagate if this is actually a repeatable section
        if occur.is_repeatable() {
            ctx.pending_occur = Some(occur);
        }
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
    pub fn with_path_segment(&self, name: &str) -> FlattenContext<'a> {
        let mut ctx = self.derive();
        ctx.current_path = if self.current_path.is_empty() {
            name.to_string()
        } else {
            format!("{}.{}", self.current_path, name)
        };
        ctx
    }

    /// Get the full SOM path for a named node at the current level
    pub fn get_full_path(&self, name: &str) -> String {
        if self.current_path.is_empty() {
            name.to_string()
        } else {
            format!("{}.{}", self.current_path, name)
        }
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
            if let Some(presence) = overrides
                .presence
                .get(&full)
                .or_else(|| overrides.presence.get(name))
            {
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

/// One `pageArea` found by [`Flattened::find_all_page_areas`], together with
/// its `contentArea` and the nearest enclosing `pageSet` (`None` for a
/// pageArea that sits directly under the root, outside any pageSet).
///
/// The pageSet is what lets a content subform's own
/// `<breakBefore targetType="pageArea" target="...">` (XFA 3.3 §7) scope a
/// "run" of pages to one pageSet's areas, rather than to every pageArea in
/// the document -- the fix for a document with more than one pageSet (e.g.
/// AAOV: `MP1Set` for its first two pages, `MP2Set` for its third).
#[derive(Clone, Copy)]
struct PageAreaRef<'a> {
    page_area: &'a XfaNode,
    content_area: &'a XfaNode,
    page_set: Option<&'a XfaNode>,
}

#[allow(dead_code)]
impl Flattened {
    /// Create a new Flattened instance with the given page and children.
    pub fn new(page: Page, children: Vec<FlattenedKind>) -> Self {
        Flattened {
            page,
            children,
            language: String::new(),
        }
    }

    /// Create a Flattened from a flat list of nodes (wraps each in FlattenedKind::Node)
    pub fn from_nodes(page: Page, nodes: Vec<FlattenedNode>) -> Self {
        let children = nodes.into_iter().map(FlattenedKind::Node).collect();
        Flattened {
            page,
            children,
            language: String::new(),
        }
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
        let height = self.page.height;
        if height <= Decimal::ZERO {
            return (1, Decimal::ZERO);
        }
        let count = self.page.page_count.max(1);
        let page_index = (y / height)
            .floor()
            .to_i64()
            .unwrap_or(0)
            .clamp(0, count as i64 - 1) as u32;
        (page_index + 1, height * Decimal::from(page_index))
    }

    /// A node's page and page-local bounds, or `None` if the layout placed it
    /// outside every page — negative Y, or at/past the bottom of the last
    /// page. Unlike [`Self::page_of_y`], this never clamps: a caller asking
    /// where to crop a region needs to know when a node has no real page,
    /// not a plausible-looking wrong one.
    pub fn locate(&self, node: &FlattenedNode) -> Option<(u32, Bounds)> {
        if node.y < Decimal::ZERO {
            return None;
        }
        let height = self.page.height;
        if height <= Decimal::ZERO {
            // Degenerate height: one page covering the whole column, same as
            // `page_of_y` and `bands()`.
            return Some((1, Bounds::new(node.x, node.y, node.width, node.height)));
        }
        let count = self.page.page_count.max(1) as i64;
        let page_index = (node.y / height).floor().to_i64()?;
        if page_index >= count {
            return None;
        }
        let page_top = height * Decimal::from(page_index);
        Some((
            page_index as u32 + 1,
            Bounds::new(node.x, node.y - page_top, node.width, node.height),
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
        script_presence_changes: &[(String, Option<String>, Presence)],
    ) {
        // Step 1: Find the "form" element and collect presence values
        let mut form_presence: HashMap<String, Presence> = HashMap::new();
        for node in xfa_nodes.iter() {
            Self::find_form_element_and_collect_presence(node, &mut form_presence);
        }

        if form_presence.is_empty() {
            return;
        }

        // Build a set of leaf names that scripts already set presence for.
        // These take priority over stale Form DOM values.
        let script_touched: HashSet<&str> = script_presence_changes
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect();

        // Remove any form DOM paths whose leaf name was touched by scripts
        form_presence.retain(|path, _| {
            let leaf = path.rsplit('.').next().unwrap_or(path);
            !script_touched.contains(leaf)
        });

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
        let id_to_field = Self::build_id_to_field_map(xfa_nodes);
        let language = super::xfa::extract_language_from_nodes(xfa_nodes);
        Self::from_xfa_with_computed_values(xfa_nodes, computed_values, &id_to_field, &language)
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
        let id_to_field = Self::build_id_to_field_map(xfa_nodes);
        let language = super::xfa::extract_language_from_nodes(xfa_nodes);
        Self::from_xfa_paged_with(xfa_nodes, computed_values, &id_to_field, &language, master)
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

    /// Recursively collect ID attributes and map them to full SOM paths.
    ///
    /// Path-building mirrors the script executor's rules:
    /// - Only Subform and ExclGroup nodes contribute to the parent path prefix
    /// - Fields and Draws get a full path but don't extend the parent prefix
    fn collect_ids_recursive(
        nodes: &[XfaNode],
        id_map: &mut HashMap<String, String>,
        parent_path: Option<&str>,
    ) {
        for node in nodes {
            let name = node.name.as_deref().unwrap_or("");

            let is_subform = matches!(node.kind, XfaNodeKind::Subform)
                || matches!(&node.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "subform");
            let is_exclgroup = matches!(node.kind, XfaNodeKind::ExclGroup);

            // Build the full SOM path for this node
            let full_path = if !name.is_empty() {
                match parent_path {
                    Some(p) => format!("{}.{}", p, name),
                    None => name.to_string(),
                }
            } else {
                parent_path.unwrap_or("").to_string()
            };

            // Store ID -> full SOM path
            if let Some(id) = node.attributes.get("id") {
                if !full_path.is_empty() {
                    id_map.insert(id.clone(), full_path.clone());
                }
            }

            // Only subforms and exclGroups extend the parent path
            let next_parent = if !name.is_empty() && (is_subform || is_exclgroup) {
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

    /// Create a flattened representation with pre-computed field values
    ///
    /// Parameters:
    /// - `xfa_nodes`: The parsed XFA template nodes (presence already set by scripts)
    /// - `computed_values`: Map of field name -> computed value from scripts
    /// - `id_to_field`: Map of element ID -> field name for resolving xfa:embed references
    fn from_xfa_with_computed_values(
        xfa_nodes: &[XfaNode],
        computed_values: &HashMap<SomPath, String>,
        id_to_field: &HashMap<String, String>,
        language: &str,
    ) -> Result<Self, String> {
        Self::from_xfa_paged_with(
            xfa_nodes,
            computed_values,
            id_to_field,
            language,
            &mut |_, _, _| PageOverrides::default(),
        )
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
        master: &mut dyn FnMut(&str, usize, usize) -> PageOverrides,
    ) -> Result<Self, String> {
        // Default to A4 size (210mm x 297mm in points)
        let mut page = Page::new(
            XfaNode::parse_dimension("210mm").unwrap_or_else(|_| num(595.27)),
            XfaNode::parse_dimension("297mm").unwrap_or_else(|_| num(841.89)),
        );

        // Find page dimensions and contentArea offset from pageArea
        let mut content_offset_x = Decimal::ZERO;
        let mut content_offset_y = Decimal::ZERO;
        let mut content_width = page.width;
        let mut content_height = page.height;

        // Find ALL pageAreas - XFA forms can have multiple (first page, last page, annex, etc.)
        // Each pageArea may have its own background content that needs to be rendered.
        let all_page_areas = Self::find_all_page_areas(xfa_nodes);

        // Use the first pageArea for page dimensions and primary contentArea offset
        if let Some(first) = all_page_areas.first() {
            let (first_page_area, first_content_area) = (first.page_area, first.content_area);
            // Get pageArea dimensions (defines the page size). A pageArea that
            // states neither takes them from its <medium>, which is where the
            // UBS masters (and most Designer output) put the paper size.
            let medium = Self::medium_size(first_page_area);
            if let Some(w) = first_page_area.w.or(medium.map(|(w, _)| w)) {
                page.width = w;
            }
            if let Some(h) = first_page_area.h.or(medium.map(|(_, h)| h)) {
                page.height = h;
            }

            // Get contentArea offset and dimensions (defines the usable area for form content)
            content_offset_x = first_content_area.x.unwrap_or(Decimal::ZERO);
            content_offset_y = first_content_area.y.unwrap_or(Decimal::ZERO);
            content_width = first_content_area.w.unwrap_or(page.width);
            content_height = first_content_area.h.unwrap_or(page.height);

            // Store content area info in Page for downstream use (e.g. column layout detection)
            page.content_area_y = content_offset_y;
            page.content_area_height = content_height;
        }

        // ============================================================
        // STEP 1: Render FORM CONTENT (from Form DOM) INTO contentArea
        // ============================================================
        // Per XFA spec: "The Form DOM is the place where the data from the XFA Data DOM
        // is bound to logical structure from the Template DOM."
        //
        // The root content subform (Form DOM root) is rendered into the contentArea.
        // Its position is offset by the contentArea's x,y coordinates.
        //
        // This runs *before* the master pages, unlike the XFA spec's drawing
        // order, because the number of pages is a result of it and the master
        // pages have to be repeated once per page.
        let mut content_children: Vec<FlattenedKind> = Vec::new();

        // Where each page's content starts, in this content column's
        // coordinates. Always at least one entry: every document has a page 1.
        let mut page_starts: Vec<Num> = vec![content_offset_y];

        let root_position = Position::new(
            content_offset_x,
            content_offset_y,
            content_width,
            content_height,
        );

        // Find and flatten ALL content subforms (the Form DOM)
        // These are siblings to pageSet, NOT inside pageArea
        // Some forms (e.g., AAGS) have multiple content subforms (Page, Page_66439)
        // that are all siblings under the root container.
        let content_subforms = Self::find_all_content_subforms_with_paths(xfa_nodes);

        // Which pageSet's pageAreas each content subform's own pages are
        // chosen from (XFA 3.3 §7: a subform's own `breakBefore
        // targetType="pageArea"` names it), grouped into "runs" of subforms
        // that share one -- see `group_content_subforms_into_runs`. A
        // document with no explicit targets (the common case) is one run
        // spanning every pageArea, exactly as before this distinction
        // existed.
        let (run_ids, run_pools): (Vec<usize>, Vec<Vec<PageAreaRef<'_>>>) =
            if content_subforms.is_empty() {
                (Vec::new(), vec![all_page_areas.clone()])
            } else {
                Self::group_content_subforms_into_runs(&content_subforms, &all_page_areas)
            };
        // Page 0 always exists (`page_starts`'s seed entry) and belongs to
        // the first content subform's run; every further entry is pushed in
        // lockstep with `page_starts` below, so the two stay the same length.
        let mut run_id_of_page: Vec<usize> =
            vec![if content_subforms.is_empty() { 0 } else { run_ids[0] }];

        if !content_subforms.is_empty() {
            // Track current y-offset for stacking multiple content subforms vertically.
            // Each content subform represents a separate page in the XFA form.
            // Per XFA spec, the root container's layout (typically "tb") determines
            // how sibling content subforms are arranged.
            let mut current_content_y = content_offset_y;

            for (index, (content_subform, content_path)) in content_subforms.iter().enumerate() {
                // Each content subform after the first starts a new page: that is
                // what "a separate page in the XFA form" means for these forms.
                if index > 0 {
                    page_starts.push(current_content_y);
                    run_id_of_page.push(run_ids[index]);
                }

                // Create flatten context with the content subform's path prefix
                // This ensures computed_values lookups use the full SOM path
                let mut ctx = FlattenContext::new_with_path(
                    computed_values,
                    id_to_field,
                    content_path.clone(),
                );
                ctx.language = language.to_string();

                // Collects `keep`/`breakBefore`/`breakAfter` constraints found
                // while laying out this column, consulted below when deciding
                // where it actually breaks into pages (XFA 3.3 §7-§8). Only
                // meaningful for the single-subform case the break loop below
                // handles; harmless to always collect it.
                let break_cell: RefCell<Vec<BreakMarker>> = RefCell::new(Vec::new());
                ctx.break_markers = Some(&break_cell);

                // Get the layout from the content subform (often "tb" for top-to-bottom)
                let layout = content_subform
                    .layout
                    .as_ref()
                    .and_then(|l| l.parse().ok())
                    .unwrap_or(Layout::Position);

                // Position this content subform at the current y-offset
                let subform_position = Position::new(
                    content_offset_x,
                    current_content_y,
                    content_width,
                    content_height,
                );

                let consumed_height = Self::flatten_nodes(
                    &content_subform.children,
                    subform_position,
                    layout,
                    &mut content_children,
                    &ctx,
                )?;

                let markers = break_cell.into_inner();
                let forced_points = Self::forced_break_points(&markers);

                // Where content overflows the content area -- or where a
                // `breakBefore`/`breakAfter` forces a page start regardless
                // -- compute page breaks, then snap each one to the nearest
                // positioned-subform boundary, unsplittable leaf,
                // `keep`-constrained range, or forced break point. In XFA,
                // positioned-layout subforms that straddle a page boundary
                // are pushed entirely to the next page. Applies to every
                // content subform, not only a document's sole one: each
                // lays out (and, if it overflows, breaks) in its own column
                // starting at `current_content_y`.
                if content_height > Decimal::ZERO
                    && (consumed_height > content_height || !forced_points.is_empty())
                {
                    let content_end = current_content_y + consumed_height;
                    let boundaries = Self::subform_boundaries(
                        &content_children,
                        content_offset_x,
                        content_width,
                    );
                    let spans = Self::leaf_spans(&content_children);
                    let keep_ranges = Self::keep_ranges(&markers, &spans);

                    // The last page of *this subform's run* can have a shorter
                    // contentArea of its own (a `pagePosition="last"` master
                    // that reserves room for a legal footnote). Breaks are
                    // laid with the ordinary height and the tail is checked
                    // against the last one afterwards.
                    let last_content_height = Self::last_page_content_height(&run_pools[run_ids[index]]);

                    // The next forced break strictly after `after`, if it
                    // comes before the geometric ceiling `before` -- a forced
                    // break is honored as soon as it is reached, not only
                    // when content would otherwise overflow.
                    let next_forced = |after: Num, before: Num| {
                        forced_points
                            .iter()
                            .copied()
                            .find(|&f| f > after && f < before)
                    };

                    let mut page_start = current_content_y;
                    // One break per page; the content cannot be more pages long
                    // than it is content-areas tall, and this bounds the loop
                    // against a snap that fails to advance.
                    let max_pages = (consumed_height / content_height)
                        .ceil()
                        .to_usize()
                        .unwrap_or(1)
                        + 2
                        + forced_points.len();
                    for _ in 0..max_pages {
                        let geometric = page_start + content_height;
                        let forced = next_forced(page_start, geometric);
                        let theoretical = forced.unwrap_or(geometric);
                        if theoretical >= content_end {
                            break;
                        }
                        let chosen = match forced {
                            // A forced break is an explicit instruction, not a
                            // geometric estimate: honor it exactly.
                            Some(f) => f,
                            None => Self::snap_break(
                                theoretical,
                                page_start,
                                content_end,
                                content_height,
                                &boundaries,
                                &spans,
                                &keep_ranges,
                            ),
                        };
                        page_starts.push(chosen);
                        run_id_of_page.push(run_ids[index]);
                        page_start = chosen;
                    }

                    // Does the tail still overflow the (possibly shorter) last
                    // contentArea? Then it is one page more.
                    if let Some(last_h) = last_content_height
                        && last_h > Decimal::ZERO
                    {
                        for _ in 0..max_pages {
                            let geometric = page_start + last_h;
                            let forced = next_forced(page_start, geometric);
                            let theoretical = forced.unwrap_or(geometric);
                            if theoretical >= content_end {
                                break;
                            }
                            let chosen = match forced {
                                Some(f) => f,
                                None => Self::snap_break(
                                    theoretical,
                                    page_start,
                                    content_end,
                                    content_height,
                                    &boundaries,
                                    &spans,
                                    &keep_ranges,
                                ),
                            };
                            page_starts.push(chosen);
                            run_id_of_page.push(run_ids[index]);
                            page_start = chosen;
                        }
                    }
                }

                // Advance y-offset for the next content subform
                // Use the subform's explicit height if set, otherwise use consumed height
                let subform_height = content_subform.h.unwrap_or(consumed_height);
                current_content_y += subform_height;
            }
        } else {
            // Fallback: flatten all nodes (old behavior for simple forms without proper structure)
            let mut ctx = FlattenContext::new(computed_values, id_to_field);
            ctx.language = language.to_string();
            Self::flatten_nodes(
                xfa_nodes,
                root_position,
                Layout::Position,
                &mut content_children,
                &ctx,
            )?;
        };

        debug_assert_eq!(
            run_id_of_page.len(),
            page_starts.len(),
            "run_id_of_page must track page_starts 1:1 -- every page_starts.push needs a \
             matching run_id_of_page.push of the current subform's run id"
        );

        // For each page, this run's local index and length -- what
        // `choose_page_area` needs below to pick a pagePosition
        // "first"/"last" area *within this run*, not across the whole
        // document (XFA 3.3 §7/§8: a pageSet's `pagePosition` is only
        // meaningful among the pages that pageSet's areas actually produced).
        let run_length_of_run: Vec<usize> = {
            let mut lengths = vec![0usize; run_pools.len()];
            for &r in &run_id_of_page {
                lengths[r] += 1;
            }
            lengths
        };
        let run_local_index_of_page: Vec<usize> = {
            let mut seen_so_far = vec![0usize; run_pools.len()];
            run_id_of_page
                .iter()
                .map(|&r| {
                    let idx = seen_so_far[r];
                    seen_so_far[r] += 1;
                    idx
                })
                .collect()
        };

        Self::apply_computed_values(&mut content_children, computed_values);

        // ============================================================
        // STEP 2: Stack the content column into whole pages
        // ============================================================
        Self::stack_pages(
            &mut content_children,
            &page_starts,
            page.height,
            content_offset_y,
        );
        page.page_count = page_starts.len() as u32;
        page.page_breaks = (1..page_starts.len())
            .map(|k| page.height * Decimal::from(k as u64))
            .collect();

        // ============================================================
        // STEP 3: Render PAGE BACKGROUND (from Template DOM's pageAreas)
        // ============================================================
        // Per XFA spec (section 7, "Page Background"):
        // "A pageArea may contain content such as subforms. Such content, which is placed
        // directly in a pageArea element, represents page background."
        //
        // Page background elements are positioned relative to the page origin (0,0),
        // NOT the contentArea. They use positioned layout (absolute coordinates).
        //
        // One pageArea is chosen per page (`pagePosition`), and its content is
        // emitted once for every page that uses it -- a header is on every page.
        // Master content goes in front of the body in document order so that it
        // is drawn first and the body sits on top of it, as before.
        let mut flattened_children: Vec<FlattenedKind> = Vec::new();
        let page_count = page_starts.len();
        if !all_page_areas.is_empty() {
            let page_position =
                Position::new(Decimal::ZERO, Decimal::ZERO, page.width, page.height);

            for k in 0..page_count {
                let run_id = run_id_of_page[k];
                let page_area_ref = Self::choose_page_area(
                    &run_pools[run_id],
                    run_local_index_of_page[k],
                    run_length_of_run[run_id],
                );
                let page_area = page_area_ref.page_area;

                // Collect content areas for region classification (header/footer detection)
                let content_areas = Self::collect_content_areas(page_area, page.width, page.height);

                // What this page's own scripts made of it: "Pagina 2/3", a
                // first-page-only block hidden from page 2 on.
                let overrides = master(page_area.name.as_deref().unwrap_or(""), k, page_count);
                let values = overrides.merged_with(computed_values);

                let mut page_ctx = FlattenContext::new(&values, id_to_field);
                page_ctx.language = language.to_string();
                page_ctx.page_overrides = Some(&overrides);

                let mut per_page: Vec<FlattenedKind> = Vec::new();
                for child in &page_area.children {
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

                // Classification happened in page-1 coordinates; move the page
                // into its slot only afterwards.
                let dy = page.height * Decimal::from(k as u64);
                if !dy.is_zero() {
                    Self::shift_y(&mut per_page, dy);
                }
                flattened_children.append(&mut per_page);
            }
        }

        flattened_children.extend(content_children);

        Ok(Flattened {
            page,
            children: flattened_children,
            language: language.to_string(),
        })
    }

    /// Apply computed values from scripts to nodes that have none of their own.
    fn apply_computed_values(
        children: &mut [FlattenedKind],
        computed_values: &HashMap<SomPath, String>,
    ) {
        for child in children {
            match child {
                FlattenedKind::Node(node) => match &mut node.kind {
                    FlattenedNodeKind::Field { name, value, .. } => {
                        if value.is_empty()
                            && let Some(computed) = computed_values.get(name.as_str())
                        {
                            *value = computed.clone();
                        }
                    }
                    FlattenedNodeKind::Text {
                        content,
                        source_name,
                        ..
                    } => {
                        if let Some(name) = source_name
                            && content.is_empty()
                            && let Some(computed) = computed_values.get(name.as_str())
                        {
                            *content = computed.clone();
                        }
                    }
                    // An image's bytes come from the template, not from a script.
                    FlattenedNodeKind::Image(_) => {}
                },
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

    /// The `pagePosition` of a pageArea; absent means "any page".
    fn page_position_of(page_area: &XfaNode) -> &str {
        page_area
            .attributes
            .get("pagePosition")
            .map(|s| s.as_str())
            .unwrap_or("any")
    }

    /// Which master page page `page_index` (of `page_count`, both counting
    /// only the pages in this run -- see `from_xfa_paged_with`'s "run"
    /// grouping) uses.
    ///
    /// Per XFA, `pagePosition` selects between masters: `first` for the opening
    /// page, `last` for the closing one, `only` for a document that is a single
    /// page, and `any`/`rest` for everything else. A single-page document is the
    /// interesting case -- it is both the first and the last page, and the
    /// closing master (with its legal footnote) is the one that must win.
    fn choose_page_area<'a>(
        all: &[PageAreaRef<'a>],
        page_index: usize,
        page_count: usize,
    ) -> PageAreaRef<'a> {
        let with = |want: &str| {
            all.iter()
                .find(|r| Self::page_position_of(r.page_area) == want)
                .copied()
        };
        let rest = || {
            all.iter()
                .find(|r| matches!(Self::page_position_of(r.page_area), "any" | "rest"))
                .copied()
        };

        let chosen = if page_count <= 1 {
            with("only")
                .or_else(|| with("last"))
                .or_else(|| with("first"))
                .or_else(rest)
        } else if page_index + 1 == page_count {
            with("last").or_else(rest)
        } else if page_index == 0 {
            with("first").or_else(rest)
        } else {
            rest()
        };

        chosen.unwrap_or(all[0])
    }

    /// The content height of the master page used for the last page, when it
    /// differs from the ordinary one (a `pagePosition="last"` master usually
    /// reserves room for a footnote).
    fn last_page_content_height(all: &[PageAreaRef<'_>]) -> Option<Num> {
        all.iter()
            .find(|r| Self::page_position_of(r.page_area) == "last")
            .and_then(|r| r.content_area.h)
    }

    /// Y positions in the flattened output that look like the start of a
    /// positioned subform: a Y with a wide element on both sides of the column
    /// midpoint. In XFA a positioned subform that straddles a page boundary is
    /// pushed whole to the next page, and these are the candidate boundaries.
    fn subform_boundaries(
        children: &[FlattenedKind],
        content_offset_x: Num,
        content_width: Num,
    ) -> Vec<Num> {
        let midpoint_x = content_offset_x + content_width / Decimal::TWO;
        let wide_threshold = content_width * num(0.3);

        fn collect_wide_columns(
            nodes: &[FlattenedKind],
            midpoint_x: Num,
            wide_threshold: Num,
            info: &mut std::collections::HashMap<rust_decimal::Decimal, (bool, bool)>,
        ) {
            for kind in nodes {
                match kind {
                    FlattenedKind::Node(node) => {
                        if node.width >= wide_threshold {
                            let entry = info.entry(node.y).or_insert((false, false));
                            let center = node.x + node.width / Decimal::TWO;
                            if center < midpoint_x {
                                entry.0 = true;
                            } else {
                                entry.1 = true;
                            }
                        }
                    }
                    FlattenedKind::Group { children, .. } => {
                        collect_wide_columns(children, midpoint_x, wide_threshold, info);
                    }
                }
            }
        }

        let mut y_info: std::collections::HashMap<rust_decimal::Decimal, (bool, bool)> =
            std::collections::HashMap::new();
        collect_wide_columns(children, midpoint_x, wide_threshold, &mut y_info);

        let mut boundaries: Vec<Num> = y_info
            .iter()
            .filter(|(_, (has_left, has_right))| *has_left && *has_right)
            .map(|(&y, _)| y)
            .collect();
        boundaries.sort();
        boundaries
    }

    /// Every leaf's vertical extent, ascending by top edge. Used to keep a page
    /// break from cutting through the middle of an element.
    fn leaf_spans(children: &[FlattenedKind]) -> Vec<(Num, Num)> {
        fn walk(nodes: &[FlattenedKind], out: &mut Vec<(Num, Num)>) {
            for kind in nodes {
                match kind {
                    FlattenedKind::Node(node) => out.push((node.y, node.y + node.height)),
                    FlattenedKind::Group { children, .. } => walk(children, out),
                }
            }
        }
        let mut out = Vec::new();
        walk(children, &mut out);
        out.sort_by_key(|a| a.0);
        out
    }

    /// Where a page actually breaks, given where it would break if content were
    /// infinitely divisible.
    ///
    /// Three corrections: first to the start of a positioned subform that
    /// straddles the break, then off any single element the break would cut
    /// in half -- both bounded to a tenth of the content height so that a
    /// coincidence far up the page cannot drag a break with it -- and finally
    /// out of any `keep`-constrained range the break would cut through
    /// (XFA 3.3 §8 "Content Splitting"), looped to a fixed point and
    /// otherwise unbounded, because that one is not a heuristic but an
    /// explicit author constraint (or, for a positioned subform, an implicit
    /// one -- see the marker push in `flatten_nodes`): the template says this
    /// span must not be split, so it never is, however far the snap has to
    /// reach, even if doing so walks it into another such range. The result
    /// is always strictly between `page_start` and the theoretical break, so
    /// the page that follows is never *taller* than the content area --
    /// which is the bug that made page 2 of the UBS forms 737pt tall against
    /// a 723pt contentArea.
    fn snap_break(
        theoretical: Num,
        page_start: Num,
        content_end: Num,
        content_height: Num,
        boundaries: &[Num],
        spans: &[(Num, Num)],
        keep_ranges: &[(Num, Num)],
    ) -> Num {
        let max_snap_distance = content_height / num(10.0);
        let mut chosen = theoretical;

        // Find the last subform boundary before the theoretical break whose
        // content extends past it (the subform overflows).
        let snapped = boundaries
            .iter()
            .rev()
            .filter(|&&b| {
                b < theoretical && b > page_start && (theoretical - b) <= max_snap_distance
            })
            .find(|&&b| {
                // Check the content from this boundary extends past the
                // theoretical break: the next boundary (or content end) is past it.
                let next_boundary = boundaries
                    .iter()
                    .find(|&&nb| nb > b)
                    .copied()
                    .unwrap_or(content_end);
                next_boundary > theoretical
            })
            .copied();
        if let Some(b) = snapped {
            chosen = b;
        }

        // A leaf the break still runs through belongs entirely to the page it
        // starts on; move the break up to its top edge.
        for _ in 0..3 {
            let straddler = spans
                .iter()
                .filter(|(top, bottom)| *top > page_start && *top < chosen && *bottom > chosen)
                .map(|(top, _)| *top)
                .min();
            match straddler {
                Some(top) if (chosen - top) <= max_snap_distance => chosen = top,
                _ => break,
            }
        }

        // A `keep`-constrained range the break still cuts through is pushed
        // to its own start, whole, whatever the distance. Looped to a fixed
        // point: snapping out of one range can land inside another (e.g. a
        // draw with `keep next="contentArea"` immediately followed by a
        // positioned subform that is itself implicitly keep-intact --
        // snapping past the draw's range can still land inside the
        // subform's, and vice versa). `chosen` strictly decreases each
        // iteration, and no range can trigger against its own top twice, so
        // this always terminates within `keep_ranges.len()` iterations.
        for _ in 0..keep_ranges.len().max(1) {
            let hit = keep_ranges
                .iter()
                .find(|&&(top, bottom)| top > page_start && top < chosen && bottom > chosen)
                .copied();
            match hit {
                Some((top, _)) => chosen = top,
                None => break,
            }
        }

        if chosen <= page_start {
            theoretical
        } else {
            chosen
        }
    }

    /// The union of `keep`-constrained Y ranges from a set of break markers:
    /// each marker's own span (`intact`, or as the anchor of a `next`/
    /// `previous` request), extended to cover whichever leaf abuts it in the
    /// direction requested. The neighbour a `keep_next`/`keep_previous`
    /// marker must stay with is not necessarily another `keep`-carrying node
    /// -- in the UBS forms it is a plain trailing `<draw>` -- so this reaches
    /// into `spans` (every leaf in the column, not just marked ones) rather
    /// than only pairing up markers with each other.
    fn keep_ranges(markers: &[BreakMarker], spans: &[(Num, Num)]) -> Vec<(Num, Num)> {
        let mut ranges = Vec::new();
        for m in markers {
            if !(m.intact || m.keep_next || m.keep_previous) {
                continue;
            }
            let mut top = m.top;
            let mut bottom = m.bottom;
            if m.keep_next {
                // The next leaf in the column: the one starting no earlier
                // than this marker's own bottom edge, closest to it.
                if let Some(&(_, next_bottom)) = spans
                    .iter()
                    .filter(|&&(s_top, _)| s_top >= m.bottom)
                    .min_by_key(|&&(s_top, _)| s_top)
                {
                    bottom = bottom.max(next_bottom);
                }
            }
            if m.keep_previous {
                // The previous leaf: the one ending no later than this
                // marker's own top edge, closest to it.
                if let Some(&(prev_top, _)) = spans
                    .iter()
                    .filter(|&&(_, s_bottom)| s_bottom <= m.top)
                    .max_by_key(|&&(_, s_bottom)| s_bottom)
                {
                    top = top.min(prev_top);
                }
            }
            ranges.push((top, bottom));
        }
        ranges
    }

    /// Absolute Y positions where a page must start, from `breakBefore`
    /// (`marker.top`) and `breakAfter` (`marker.bottom`) markers (XFA 3.3 §7
    /// "Break Conditions").
    fn forced_break_points(markers: &[BreakMarker]) -> Vec<Num> {
        let mut points = Vec::new();
        for m in markers {
            if m.force_before {
                points.push(m.top);
            }
            if m.force_after {
                points.push(m.bottom);
            }
        }
        points.sort();
        points.dedup();
        points
    }

    /// Move a laid-out content column into per-page slots.
    ///
    /// `starts` is where each page's content begins in the column's own
    /// coordinates; afterwards page `k`'s content begins at
    /// `k * page_height + content_top`, so the column becomes a stack of whole
    /// pages with the page margins present rather than a continuous ribbon of
    /// content areas.
    fn stack_pages(
        children: &mut [FlattenedKind],
        starts: &[Num],
        page_height: Num,
        content_top: Num,
    ) {
        for child in children {
            match child {
                FlattenedKind::Node(node) => {
                    // Which page this leaf starts on. A leaf above the first
                    // start (there should be none) stays with page 1.
                    let k = starts.partition_point(|s| *s <= node.y).saturating_sub(1);
                    Self::shift_node_y(
                        node,
                        page_height * Decimal::from(k as u64) + content_top - starts[k],
                    );
                }
                FlattenedKind::Group { children, .. } => {
                    Self::stack_pages(children, starts, page_height, content_top);
                }
            }
        }
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

    /// Find the content subform (the Form DOM root)
    ///
    /// Per XFA spec:
    /// - Template DOM contains: pageSet (page structure) + root subform (form content)
    /// - Form DOM is a duplicate of the subtree under the root subform (NOT including pageSet)
    /// - The root content subform is typically a sibling to pageSet, not inside it
    /// - xfa:datasets contains data and should NOT be used for layout
    ///
    /// Structure example:
    ///   template
    ///     subform 'UBSForms' (root container)
    ///       pageSet 'MPs'        <-- page structure (stays in Template DOM)
    ///       subform 'Page'       <-- root content subform (becomes Form DOM)
    /// Returns (subform, full_path) where full_path is the SOM path to the subform (e.g., "UBSForms.Page")
    fn find_root_subform_with_path(nodes: &[XfaNode]) -> Option<(&XfaNode, String)> {
        /// Helper to check if a node is a pageSet or similar page structure element
        fn is_page_structure(node: &XfaNode) -> bool {
            matches!(
                node.kind,
                XfaNodeKind::PageSet | XfaNodeKind::PageArea | XfaNodeKind::ContentArea
            ) || matches!(&node.kind, XfaNodeKind::Element { tag_name, .. } 
                if tag_name == "pageSet" || tag_name == "pageArea" || tag_name == "contentArea")
        }

        /// Helper to check if a node is a non-content element (variables, proto, desc, event, etc.)
        fn is_non_content_element(node: &XfaNode) -> bool {
            if let XfaNodeKind::Element { tag_name, .. } = &node.kind {
                matches!(
                    tag_name.as_str(),
                    "variables" | "proto" | "desc" | "event" | "extras" | "occur"
                )
            } else {
                false
            }
        }

        /// Helper to check if a node is a data-only element that should be skipped for layout
        fn is_data_element(node: &XfaNode) -> bool {
            if let XfaNodeKind::Element { tag_name, .. } = &node.kind {
                // Skip xfa:datasets, xfa:data, form (Form DOM), and similar data containers
                // The "form" element is the Form DOM root - it's a duplicate of Template content
                tag_name.starts_with("xfa:") || 
                tag_name.starts_with("dd:") ||  // data description
                tag_name == "datasets" || 
                tag_name == "data" ||
                tag_name == "form" // Form DOM - duplicates Template content
            } else {
                false
            }
        }

        /// Find content subform inside a container subform (sibling to pageSet)
        /// Returns (subform, name) where name is the subform's name
        fn find_content_subform_in_container(
            container: &XfaNode,
        ) -> Option<(&XfaNode, Option<&str>)> {
            // Look for a subform that is NOT a pageSet and NOT a non-content element
            // This is the actual content subform that goes into the Form DOM
            for child in &container.children {
                if is_page_structure(child) || is_non_content_element(child) {
                    continue;
                }

                // Found a content subform
                if matches!(child.kind, XfaNodeKind::Subform) {
                    return Some((child, child.name.as_deref()));
                }
                if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                    && tag_name == "subform"
                {
                    return Some((child, child.name.as_deref()));
                }
            }
            None
        }

        fn search_recursive(nodes: &[XfaNode]) -> Option<(&XfaNode, String)> {
            for node in nodes {
                // Skip data elements - we only want Template DOM content
                if is_data_element(node) {
                    continue;
                }

                // Look in template
                if matches!(node.kind, XfaNodeKind::Template) {
                    // Template's direct child subform is the root container
                    for child in &node.children {
                        if matches!(child.kind, XfaNodeKind::Subform) {
                            // This is the root container subform (e.g., 'UBSForms')
                            let container_name = child.name.as_deref().unwrap_or("");

                            // Look for the content subform inside it (sibling to pageSet)
                            if let Some((content_subform, content_name)) =
                                find_content_subform_in_container(child)
                            {
                                // Build full path: container.content (e.g., "UBSForms.Page")
                                let path = if let Some(name) = content_name {
                                    if container_name.is_empty() {
                                        name.to_string()
                                    } else {
                                        format!("{}.{}", container_name, name)
                                    }
                                } else {
                                    container_name.to_string()
                                };
                                return Some((content_subform, path));
                            }
                            // If no content subform found, the container itself might be the content
                            // (for simpler forms without separate pageSet)
                            let has_page_set = child.children.iter().any(is_page_structure);
                            if !has_page_set {
                                return Some((child, container_name.to_string()));
                            }
                        }
                    }
                }

                // Check Element nodes for template
                if let XfaNodeKind::Element { tag_name, .. } = &node.kind
                    && tag_name == "template"
                {
                    for child in &node.children {
                        let is_subform = matches!(child.kind, XfaNodeKind::Subform)
                            || matches!(&child.kind, XfaNodeKind::Element { tag_name: ct, .. } if ct == "subform");

                        if is_subform {
                            // This is the root container subform
                            let container_name = child.name.as_deref().unwrap_or("");

                            if let Some((content_subform, content_name)) =
                                find_content_subform_in_container(child)
                            {
                                let path = if let Some(name) = content_name {
                                    if container_name.is_empty() {
                                        name.to_string()
                                    } else {
                                        format!("{}.{}", container_name, name)
                                    }
                                } else {
                                    container_name.to_string()
                                };
                                return Some((content_subform, path));
                            }
                            // Fallback: use the container if no pageSet
                            let has_page_set = child.children.iter().any(is_page_structure);
                            if !has_page_set {
                                return Some((child, container_name.to_string()));
                            }
                        }
                    }
                }

                // Only recurse into Template or container nodes, skip data elements
                if !is_data_element(node)
                    && let Some(result) = search_recursive(&node.children)
                {
                    return Some(result);
                }
            }
            None
        }
        search_recursive(nodes)
    }

    /// Find ALL content subforms (siblings to pageSet) with their full SOM paths.
    ///
    /// Per XFA spec, the root container subform can have multiple content subforms
    /// as siblings to the pageSet. For example:
    /// ```text
    ///   template
    ///     subform 'UBSForms' (root container)
    ///       pageSet 'MPs'        <-- page structure
    ///       subform 'Page'       <-- first content subform
    ///       subform 'Page_66439' <-- second content subform
    /// ```
    ///
    /// Returns a Vec of (subform, full_path) pairs for all content subforms found.
    /// Falls back to `find_root_subform_with_path` to return a single-element Vec
    /// if the multi-subform search doesn't find a container with pageSet.
    fn find_all_content_subforms_with_paths(nodes: &[XfaNode]) -> Vec<(&XfaNode, String)> {
        /// Helper to check if a node is a pageSet or similar page structure element
        fn is_page_structure(node: &XfaNode) -> bool {
            matches!(
                node.kind,
                XfaNodeKind::PageSet | XfaNodeKind::PageArea | XfaNodeKind::ContentArea
            ) || matches!(&node.kind, XfaNodeKind::Element { tag_name, .. }
                if tag_name == "pageSet" || tag_name == "pageArea" || tag_name == "contentArea")
        }

        /// Helper to check if a node is a non-content element (variables, proto, desc, event, etc.)
        fn is_non_content_element(node: &XfaNode) -> bool {
            if let XfaNodeKind::Element { tag_name, .. } = &node.kind {
                matches!(
                    tag_name.as_str(),
                    "variables"
                        | "proto"
                        | "desc"
                        | "event"
                        | "extras"
                        | "occur"
                        | "breakBefore"
                        | "breakAfter"
                        | "break"
                        | "overflow"
                        | "instanceManager"
                )
            } else {
                false
            }
        }

        /// Helper to check if a node is a data-only element
        fn is_data_element(node: &XfaNode) -> bool {
            if let XfaNodeKind::Element { tag_name, .. } = &node.kind {
                tag_name.starts_with("xfa:")
                    || tag_name.starts_with("dd:")
                    || tag_name == "datasets"
                    || tag_name == "data"
                    || tag_name == "form"
            } else {
                false
            }
        }

        /// Check if node is a content subform (subform that is not page structure)
        fn is_content_subform(node: &XfaNode) -> bool {
            if is_page_structure(node) || is_non_content_element(node) {
                return false;
            }
            matches!(node.kind, XfaNodeKind::Subform)
                || matches!(&node.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "subform")
        }

        /// Collect all content subforms from a container
        fn collect_content_subforms_in_container<'a>(
            container: &'a XfaNode,
            container_name: &str,
        ) -> Vec<(&'a XfaNode, String)> {
            // Collect fingerprints of visible content subforms so we can detect
            // hidden duplicates (e.g. XFA overflow page copies with identical structure).
            let visible_fingerprints: Vec<Vec<Option<&str>>> = container
                .children
                .iter()
                .filter(|c| is_content_subform(c) && !c.presence.should_skip_layout())
                .map(|c| c.children.iter().map(|gc| gc.name.as_deref()).collect())
                .collect();

            let mut result = Vec::new();
            for child in &container.children {
                if is_content_subform(child) {
                    // Skip hidden/inactive content subforms that are structural
                    // duplicates of a visible sibling (same child names in same order).
                    // These are XFA overflow page copies that should not be rendered.
                    // Hidden subforms with unique structure are dynamic pages
                    // toggled by scripts and must be kept.
                    if child.presence.should_skip_layout() {
                        let fingerprint: Vec<Option<&str>> =
                            child.children.iter().map(|gc| gc.name.as_deref()).collect();
                        if visible_fingerprints.contains(&fingerprint) {
                            continue;
                        }
                    }
                    let path = if let Some(name) = child.name.as_deref() {
                        if container_name.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}.{}", container_name, name)
                        }
                    } else {
                        container_name.to_string()
                    };
                    result.push((child, path));
                }
            }
            result
        }

        fn search_recursive(nodes: &[XfaNode]) -> Vec<(&XfaNode, String)> {
            for node in nodes {
                if is_data_element(node) {
                    continue;
                }

                // Look in template
                let is_template = matches!(node.kind, XfaNodeKind::Template)
                    || matches!(&node.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "template");

                if is_template {
                    for child in &node.children {
                        let is_subform = matches!(child.kind, XfaNodeKind::Subform)
                            || matches!(&child.kind, XfaNodeKind::Element { tag_name: ct, .. } if ct == "subform");

                        if is_subform {
                            let container_name = child.name.as_deref().unwrap_or("");
                            let has_page_set = child.children.iter().any(is_page_structure);

                            // Collect ALL content subforms from the container
                            // (siblings to pageSet, skipping non-content elements)
                            let subforms =
                                collect_content_subforms_in_container(child, container_name);

                            if !subforms.is_empty() {
                                return subforms;
                            }

                            // If no content subforms found and no pageSet,
                            // the container itself is the content
                            if !has_page_set {
                                return vec![(child, container_name.to_string())];
                            }
                        }
                    }
                }

                // Recurse into container nodes
                if !is_data_element(node) {
                    let result = search_recursive(&node.children);
                    if !result.is_empty() {
                        return result;
                    }
                }
            }
            Vec::new()
        }

        search_recursive(nodes)
    }

    /// Flatten a single node (used for pageArea children)
    fn flatten_single_node(
        node: &XfaNode,
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
        let height = node.h.unwrap_or_else(|| num(20.0));

        let pos = Position::new(parent_position.x + x, parent_position.y + y, width, height);

        match &node.kind {
            XfaNodeKind::Draw => {
                // An image draw carries pixels rather than text; the whole text
                // pipeline below has nothing to do with it.
                if let Some(image_node) = Self::flatten_image_draw(node, pos, ctx)? {
                    flattened_children.push(FlattenedKind::Node(image_node));
                    return Ok(());
                }

                // Extract text content, or use empty string if none (scripts may fill it later)
                // Use context to resolve xfa:embed references
                let text_content = ctx.extract_text(&node.children).unwrap_or_default();

                // Extract font info from XFA <font> element first
                let xfa_font_size = Self::extract_font_size(node);
                let xfa_font_name = Self::extract_font_name(node);

                // Check if HTML content has CSS font overrides
                // Per XFA spec, CSS styles in exData content can override the <font> element
                let (css_font_family, css_font_size, css_is_bold) =
                    Self::extract_font_from_html_content(&node.children);

                // Use CSS font if available, otherwise fall back to XFA font
                let font_size = css_font_size.unwrap_or(xfa_font_size);
                let font_name = css_font_family.clone().unwrap_or(xfa_font_name);

                // Extract style
                let mut style = Self::extract_style(node);

                // Compute default_bold/italic from original XFA font BEFORE CSS overrides.
                // CSS font-weight from individual paragraphs should not change the default
                // for rich text parsing - each paragraph handles its own font-weight CSS.
                let (default_bold, default_italic) = style.get_xfa_bold_italic();

                // Only apply CSS overrides if at least one CSS property was found
                // This ensures consistency: either all CSS properties are considered, or none
                if (css_font_size.is_some() || css_font_family.is_some() || css_is_bold.is_some())
                    && let Some(ref mut font) = style.font
                {
                    // Apply CSS overrides if present
                    if let Some(size) = css_font_size {
                        font.size = size;
                    }
                    if let Some(ref family) = css_font_family {
                        font.typeface = family.clone();
                    }
                    if let Some(is_bold) = css_is_bold {
                        font.weight = if is_bold {
                            crate::xfa::FontWeight::Bold
                        } else {
                            crate::xfa::FontWeight::Normal
                        };
                    }
                }

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
                    let som_path = ctx.get_full_path(name);
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
                let is_checked =
                    Self::compute_field_checked_state(&node.children, &ctx.parent_exclgroup_value);
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
                let som_path = ctx.get_full_path(&field_name);
                field_node.add_hint(Hint::SomPath(SomPath::new(som_path)));
                // Add ExclGroupSomPath hint if inside an exclGroup
                if let Some(ref exclgroup_path) = ctx.parent_exclgroup_som_path {
                    field_node.add_hint(Hint::ExclGroupSomPath(exclgroup_path.clone()));
                }
                // Add FieldBehavior hint with access level
                field_node.add_hint(Hint::FieldBehavior {
                    access,
                    multiline: false,
                    max_length: None,
                    comb_cells: None,
                });
                // Add WidgetType hint if extracted
                if let Some(kind) = Self::extract_widget_kind(node) {
                    if kind == WidgetKind::Barcode
                        && let Some(sym) = Self::extract_barcode_symbology(node)
                    {
                        field_node.add_hint(Hint::BarcodeSymbology(sym));
                    }
                    field_node.add_hint(Hint::WidgetType(kind));
                }
                // Add NoPrint hint if relevant="-print" or inherited from parent
                if Self::is_no_print(node) || ctx.has_inherited_hint(&Hint::NoPrint) {
                    field_node.add_hint(Hint::NoPrint);
                }
                flattened_children.push(FlattenedKind::Node(field_node));
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
                    Self::flatten_nodes(
                        std::slice::from_ref(node),
                        parent_position,
                        Layout::Position,
                        flattened_children,
                        ctx,
                    )?;
                    return Ok(());
                }

                // Check if this subform has an <occur> element (repeatable section)
                if let Some(occur) = Self::extract_occur_constraints(node) {
                    if occur.is_repeatable() && occur.has_initial_instances() {
                        // Create a group for repeatable sections that have initial instances
                        let mut group_children = Vec::new();
                        let subform_ctx = ctx.with_occur_constraints(occur);
                        for child in &node.children {
                            Self::flatten_single_node(
                                child,
                                pos,
                                Layout::Position,
                                &mut group_children,
                                &subform_ctx,
                            )?;
                        }
                        let hints = vec![Hint::Occurrence {
                            min: occur.min,
                            max: occur.max,
                        }];
                        flattened_children.push(FlattenedKind::Group {
                            children: group_children,
                            hints,
                        });
                        return Ok(());
                    } else if occur.is_repeatable() && !occur.has_initial_instances() {
                        // Repeatable but initial=0: skip entirely (no instances exist yet)
                        return Ok(());
                    }
                }
                // No occur or not repeatable - just recurse without creating a group
                for child in &node.children {
                    Self::flatten_single_node(
                        child,
                        pos,
                        Layout::Position,
                        flattened_children,
                        ctx,
                    )?;
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn find_page_and_content_area(nodes: &[XfaNode]) -> Option<(&XfaNode, &XfaNode)> {
        fn search_recursive(nodes: &[XfaNode]) -> Option<(&XfaNode, &XfaNode)> {
            for node in nodes {
                // Check for PageArea node type
                if matches!(node.kind, XfaNodeKind::PageArea) {
                    // Found pageArea, now look for contentArea within it
                    for child in &node.children {
                        if matches!(child.kind, XfaNodeKind::ContentArea) {
                            return Some((node, child));
                        }
                        // Also check Element nodes that might be contentArea
                        if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                            && tag_name == "contentArea"
                        {
                            return Some((node, child));
                        }
                    }
                    // If no contentArea found, return pageArea twice (use page dimensions)
                    return Some((node, node));
                }

                // Check for pageArea as Element
                if let XfaNodeKind::Element { tag_name, .. } = &node.kind
                    && tag_name == "pageArea"
                {
                    // Found pageArea as Element, look for contentArea
                    for child in &node.children {
                        if matches!(child.kind, XfaNodeKind::ContentArea) {
                            return Some((node, child));
                        }
                        if let XfaNodeKind::Element {
                            tag_name: ca_tag, ..
                        } = &child.kind
                            && ca_tag == "contentArea"
                        {
                            return Some((node, child));
                        }
                    }
                    return Some((node, node));
                }

                // Recurse into all container-like nodes to find pageArea
                let should_recurse = matches!(
                    node.kind,
                    XfaNodeKind::Template
                        | XfaNodeKind::PageSet
                        | XfaNodeKind::Subform
                        | XfaNodeKind::ExclGroup
                ) || matches!(&node.kind, XfaNodeKind::Element { .. });

                if should_recurse && let Some(result) = search_recursive(&node.children) {
                    return Some(result);
                }
            }
            None
        }
        search_recursive(nodes)
    }

    /// Find ALL pageAreas and their contentAreas in the XFA tree.
    /// This is needed because XFA forms can have multiple pageAreas (e.g., first page, last page,
    /// annex pages), each with their own page background content that must be rendered.
    ///
    /// Returns one [`PageAreaRef`] per pageArea found, each carrying the
    /// nearest enclosing `pageSet` (needed to scope a content subform whose
    /// own `breakBefore targetType="pageArea"` names one; see
    /// `content_subform_page_area_target` and the "run" grouping in
    /// `from_xfa_paged_with`).
    fn find_all_page_areas(nodes: &[XfaNode]) -> Vec<PageAreaRef<'_>> {
        fn find_content_area_in_page(page_area: &XfaNode) -> &XfaNode {
            for child in &page_area.children {
                if matches!(child.kind, XfaNodeKind::ContentArea) {
                    return child;
                }
                if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                    && tag_name == "contentArea"
                {
                    return child;
                }
            }
            // If no contentArea found, return pageArea itself (use page dimensions)
            page_area
        }

        fn collect_recursive<'a>(
            nodes: &'a [XfaNode],
            enclosing_page_set: Option<&'a XfaNode>,
            result: &mut Vec<PageAreaRef<'a>>,
        ) {
            for node in nodes {
                // Check for PageArea node type
                if matches!(node.kind, XfaNodeKind::PageArea) {
                    let content_area = find_content_area_in_page(node);
                    result.push(PageAreaRef {
                        page_area: node,
                        content_area,
                        page_set: enclosing_page_set,
                    });
                    // Don't recurse into pageArea children - they are content, not structure
                    continue;
                }

                // Check for pageArea as Element
                if let XfaNodeKind::Element { tag_name, .. } = &node.kind
                    && tag_name == "pageArea"
                {
                    let content_area = find_content_area_in_page(node);
                    result.push(PageAreaRef {
                        page_area: node,
                        content_area,
                        page_set: enclosing_page_set,
                    });
                    continue;
                }

                let is_page_set = matches!(node.kind, XfaNodeKind::PageSet)
                    || matches!(&node.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "pageSet");

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
                    collect_recursive(&node.children, next_page_set, result);
                }
            }
        }

        let mut result = Vec::new();
        collect_recursive(nodes, None, &mut result);
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
    fn extract_field_access(node: &XfaNode) -> FieldAccess {
        node.attributes
            .get("access")
            .and_then(|s| s.parse().ok())
            .unwrap_or(FieldAccess::Open)
    }

    /// Find the first leaf `FlattenedNode` in a mutable slice of `FlattenedKind`,
    /// recursing into Groups.
    fn first_leaf_node_mut(kinds: &mut [FlattenedKind]) -> Option<&mut FlattenedNode> {
        for kind in kinds.iter_mut() {
            match kind {
                FlattenedKind::Node(node) => return Some(node),
                FlattenedKind::Group { children, .. } => {
                    if let Some(node) = Self::first_leaf_node_mut(children) {
                        return Some(node);
                    }
                }
            }
        }
        None
    }

    /// Find the last leaf `FlattenedNode` in a mutable slice of `FlattenedKind`,
    /// recursing into Groups.
    fn last_leaf_node_mut(kinds: &mut [FlattenedKind]) -> Option<&mut FlattenedNode> {
        for kind in kinds.iter_mut().rev() {
            match kind {
                FlattenedKind::Node(node) => return Some(node),
                FlattenedKind::Group { children, .. } => {
                    if let Some(node) = Self::last_leaf_node_mut(children) {
                        return Some(node);
                    }
                }
            }
        }
        None
    }

    /// Propagate a subform's explicitly-specified visible border edges to the first and last
    /// leaf child nodes produced during that subform's flattening.
    ///
    /// Per XFA spec, a `<border>` element on a `subform` draws a box around the subform's
    /// content area. For heading detection and visual fidelity we materialise this by
    /// applying the subform's top border edge to the first enclosed leaf node and the
    /// bottom edge to the last, but only when the node's own corresponding edge is not
    /// already visible and only for edges explicitly specified in the XFA source.
    ///
    /// The `parent_x`, `parent_y`, `parent_width`, `parent_height` parameters specify
    /// the subform's outer bounds; these are stored in the border's `render_bounds` field
    /// so that edges are rendered at the subform's position rather than the individual
    /// child node's position.
    fn propagate_subform_border_to_children(
        border: &crate::xfa::Border,
        children_range: &mut [FlattenedKind],
        parent_x: Num,
        parent_y: Num,
        parent_width: Num,
        parent_height: Num,
    ) {
        // Per XFA spec: if fewer than 4 edge elements are supplied, the last is reused.
        // We intentionally consume edges via get_edge() so reused visible edges are
        // propagated consistently with runtime border rendering.
        //
        // We apply horizontal edges (top/bottom) to BOTH first and last leaf nodes
        // so that heading detection and semantic queries can determine if ANY node
        // in the row has a border. The render_bounds field ensures that edges render
        // at the parent's position; since all nodes share the same render bounds,
        // overlapping lines appear as a single visual line.
        if let Some(top) = border.get_edge(0) {
            if top.presence == "visible" && top.thickness.is_some() {
                if let Some(node) = Self::first_leaf_node_mut(children_range) {
                    Self::apply_edge_to_node_if_not_visible(
                        node,
                        top,
                        0,
                        parent_x,
                        parent_y,
                        parent_width,
                        parent_height,
                    );
                }
                if let Some(node) = Self::last_leaf_node_mut(children_range) {
                    Self::apply_edge_to_node_if_not_visible(
                        node,
                        top,
                        0,
                        parent_x,
                        parent_y,
                        parent_width,
                        parent_height,
                    );
                }
            }
        }

        if let Some(bottom) = border.get_edge(2) {
            if bottom.presence == "visible" && bottom.thickness.is_some() {
                if let Some(node) = Self::last_leaf_node_mut(children_range) {
                    Self::apply_edge_to_node_if_not_visible(
                        node,
                        bottom,
                        2,
                        parent_x,
                        parent_y,
                        parent_width,
                        parent_height,
                    );
                }
                if let Some(node) = Self::first_leaf_node_mut(children_range) {
                    Self::apply_edge_to_node_if_not_visible(
                        node,
                        bottom,
                        2,
                        parent_x,
                        parent_y,
                        parent_width,
                        parent_height,
                    );
                }
            }
        }

        // A subform's <border><fill> is its background colour (e.g. the AAOV
        // "Form configurator" section's pink panel, whose border has every
        // edge hidden but a visible fill). Painted the same way as the edges
        // above: on the first enclosed leaf, at the subform's own bounds, and
        // only when that leaf does not already carry a fill of its own.
        if let Some(fill) = &border.fill
            && fill.presence != "hidden"
            && fill.presence != "inactive"
            && fill.color.is_some()
            && let Some(node) = Self::first_leaf_node_mut(children_range)
        {
            Self::apply_fill_to_node_if_absent(
                node,
                fill,
                parent_x,
                parent_y,
                parent_width,
                parent_height,
            );
        }
    }

    /// Apply `fill` as `node`'s border background when it does not already
    /// have one, creating a minimal (edge-less) border to carry it if `node`
    /// has none. The parent bounds are stored as `render_bounds` so the fill
    /// paints at the subform's position, matching how a propagated edge does.
    fn apply_fill_to_node_if_absent(
        node: &mut FlattenedNode,
        fill: &crate::xfa::Fill,
        parent_x: Num,
        parent_y: Num,
        parent_width: Num,
        parent_height: Num,
    ) {
        match &mut node.style.border {
            Some(border) => {
                if border.fill.is_none() {
                    border.fill = Some(fill.clone());
                    // Always anchor to the subform's bounds here, even if this
                    // leaf's own border already carries `render_bounds` (e.g.
                    // from `anchor_own_border`, pointing at the leaf's own
                    // outer box): a fill propagated from an ancestor subform
                    // must paint the subform's full row, not just this leaf.
                    border.render_bounds =
                        Some((parent_x, parent_y, parent_width, parent_height));
                }
            }
            None => {
                node.style.border = Some(crate::xfa::Border {
                    fill: Some(fill.clone()),
                    presence: "visible".to_string(),
                    render_bounds: Some((parent_x, parent_y, parent_width, parent_height)),
                    ..Default::default()
                });
            }
        }
    }

    /// Apply `edge` to `edge_index` in `node`'s border when that edge is not already visible.
    /// If the node has no border, create a minimal border with just this edge materialised.
    /// The parent bounds are stored as render bounds so edges render at the parent's position.
    fn apply_edge_to_node_if_not_visible(
        node: &mut FlattenedNode,
        edge: &crate::xfa::Edge,
        edge_index: usize,
        parent_x: Num,
        parent_y: Num,
        parent_width: Num,
        parent_height: Num,
    ) {
        match &mut node.style.border {
            Some(border) => {
                let already_visible = border
                    .get_edge(edge_index)
                    .map(|e| e.presence == "visible" && e.thickness.is_some())
                    .unwrap_or(false);

                if !already_visible {
                    if border.edges.len() < 4 {
                        let materialised: Vec<_> = (0..4)
                            .map(|i| border.get_edge(i).cloned().unwrap_or_default())
                            .collect();
                        border.edges = materialised;
                    }
                    border.edges[edge_index] = edge.clone();
                    if border.presence == "hidden" || border.presence == "inactive" {
                        border.presence = "visible".to_string();
                    }
                    // Store render bounds so edges render at parent's position
                    border.render_bounds = Some((parent_x, parent_y, parent_width, parent_height));
                }
            }
            None => {
                let hidden = crate::xfa::Edge {
                    presence: "hidden".to_string(),
                    ..Default::default()
                };
                let mut edges = vec![hidden; 4];
                edges[edge_index] = edge.clone();
                node.style.border = Some(crate::xfa::Border {
                    edges,
                    presence: "visible".to_string(),
                    render_bounds: Some((parent_x, parent_y, parent_width, parent_height)),
                    ..Default::default()
                });
            }
        }
    }

    /// Extract widget kind from a field's <ui> child element.
    /// Per XFA spec, the <ui> element contains the widget type (textEdit, checkButton, etc.)
    /// and the shape attribute distinguishes radio buttons (round) from checkboxes (square/default).
    fn extract_widget_kind(node: &XfaNode) -> Option<WidgetKind> {
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
    fn extract_barcode_symbology(node: &XfaNode) -> Option<String> {
        for child in &node.children {
            let XfaNodeKind::Element { tag_name, .. } = &child.kind else {
                continue;
            };
            if tag_name != "ui" {
                continue;
            }
            for ui_child in &child.children {
                let XfaNodeKind::Element {
                    tag_name: ui_tag, ..
                } = &ui_child.kind
                else {
                    continue;
                };
                if ui_tag == "barcode" {
                    return Some(
                        ui_child
                            .attributes
                            .get("type")
                            .cloned()
                            .unwrap_or_else(|| "unspecified".to_string()),
                    );
                }
            }
        }
        None
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

    /// Check if a node has the relevant="-print" attribute.
    /// Per XFA spec, relevant="-print" means the element should not appear in print output.
    /// It's used for screen-only interactive elements like add/remove buttons.
    fn is_no_print(node: &XfaNode) -> bool {
        node.attributes
            .get("relevant")
            .map(|s| s == "-print")
            .unwrap_or(false)
    }

    /// Extract occurrence constraints from a node's <occur> child element.
    /// Per XFA 3.3 spec (Chapter 9, "The Occur Element"):
    /// - min: minimum occurrences (default 1)
    /// - max: maximum occurrences, -1 = unlimited (default = min)
    /// - initial: starting occurrences during empty merge (default = min)
    fn extract_occur_constraints(node: &XfaNode) -> Option<OccurConstraints> {
        for child in &node.children {
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                && tag_name == "occur"
            {
                // Parse attributes with defaults per XFA spec
                let min = child
                    .attributes
                    .get("min")
                    .and_then(|s| s.parse::<i32>().ok())
                    .map(|v| v.max(0) as u32)
                    .unwrap_or(1);

                let max = child
                    .attributes
                    .get("max")
                    .and_then(|s| s.parse::<i32>().ok())
                    .map(|v| if v == -1 { None } else { Some(v.max(0) as u32) })
                    .unwrap_or(Some(min)); // Default max = min

                let initial = child
                    .attributes
                    .get("initial")
                    .and_then(|s| s.parse::<i32>().ok())
                    .map(|v| v.max(0) as u32)
                    .unwrap_or(min); // Default initial = min

                return Some(OccurConstraints { min, max, initial });
            }
        }
        None
    }

    /// Extract font size from node, with default fallback
    fn extract_font_size(node: &XfaNode) -> Num {
        node.font
            .as_ref()
            .map(|f| f.size)
            .unwrap_or_else(|| num(10.0))
    }

    /// Extract font name from node, with default fallback
    fn extract_font_name(node: &XfaNode) -> String {
        node.font
            .as_ref()
            .map(|f| f.typeface.clone())
            .unwrap_or_else(|| "Helvetica".to_string())
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
                    underline: false,
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
        let mut row_growable_slack = Decimal::ZERO;
        let start_y = parent_position.y;

        // For positioned layout, track the maximum extent (bottom-most point of all children)
        // This is needed when this container has no explicit height - we compute it from children
        let mut max_extent_y = Decimal::ZERO;

        for node in nodes {
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

            // Create child context - if this node's presence is hidden/inactive,
            // children inherit that presence
            // Also extend the SOM path if this node has a name
            // Also propagate NoPrint hint if this node has relevant="-print"
            let child_ctx = {
                let base_ctx = if presence.should_skip_layout() {
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
                // Extend the SOM path if this node has a name
                if let Some(name) = &node.name {
                    base_ctx.with_path_segment(name)
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
                            &mut row_growable_slack,
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

                    // Check if this subform has an <occur> element (repeatable section)
                    // If so, create a group to contain its children
                    let children_height = if let Some(occur) = Self::extract_occur_constraints(node)
                    {
                        if occur.is_repeatable() && occur.has_initial_instances() {
                            // Create a group for repeatable sections that have initial instances
                            let hints = vec![Hint::Occurrence {
                                min: occur.min,
                                max: occur.max,
                            }];
                            let subform_ctx = layout_ctx.with_occur_constraints(occur);

                            // Check if this node has multiple dynamic instance subforms
                            // (created by clone_child_instances with _inst0, _inst1 naming).
                            // If so, flatten each into its own occurrence group so they become
                            // separate sections in the Document pipeline.
                            let has_dynamic_instances = node.children.iter().any(|c| {
                                matches!(c.kind, XfaNodeKind::Subform)
                                    && c.name.as_ref().is_some_and(|n| n.contains("_inst"))
                            });

                            if has_dynamic_instances {
                                // Multiple subform instances: each gets its own occurrence group
                                let mut instance_y = content_pos.y;
                                for child in &node.children {
                                    if matches!(child.kind, XfaNodeKind::Subform) {
                                        let instance_pos = Position::new(
                                            content_pos.x,
                                            instance_y,
                                            content_pos.width,
                                            content_pos.height,
                                        );
                                        let mut instance_children = Vec::new();
                                        let h = Self::flatten_nodes(
                                            std::slice::from_ref(child),
                                            instance_pos,
                                            layout,
                                            &mut instance_children,
                                            &subform_ctx,
                                        )?;
                                        flattened_children.push(FlattenedKind::Group {
                                            children: instance_children,
                                            hints: hints.clone(),
                                        });
                                        instance_y += h;
                                    } else {
                                        Self::flatten_nodes(
                                            std::slice::from_ref(child),
                                            content_pos,
                                            layout,
                                            flattened_children,
                                            &subform_ctx,
                                        )?;
                                    }
                                }
                                instance_y - content_pos.y
                            } else {
                                // Single or no subform children: flatten as before
                                let mut group_children = Vec::new();
                                let h = Self::flatten_nodes(
                                    &node.children,
                                    content_pos,
                                    layout,
                                    &mut group_children,
                                    &subform_ctx,
                                )?;
                                flattened_children.push(FlattenedKind::Group {
                                    children: group_children,
                                    hints,
                                });
                                h
                            }
                        } else if occur.is_repeatable() && !occur.has_initial_instances() {
                            // Repeatable but initial=0: skip entirely (no instances exist yet)
                            Decimal::ZERO
                        } else {
                            // Not repeatable, just recurse normally
                            Self::flatten_nodes(
                                &node.children,
                                content_pos,
                                layout,
                                flattened_children,
                                &layout_ctx,
                            )?
                        }
                    } else {
                        // No occur element, just recurse normally
                        Self::flatten_nodes(
                            &node.children,
                            content_pos,
                            layout,
                            flattened_children,
                            &layout_ctx,
                        )?
                    };

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

                    // Per XFA spec: a subform's <border> draws a visual box around the subform.
                    // Propagate visible border edges (and any background fill) to the
                    // first/last enclosed child nodes so heading detection and
                    // rendering reflect the subform border.
                    // Pass the subform's outer_pos -- at its finalised height, when
                    // it grew -- so edges/fill render at the row's true position/size.
                    if let Some(ref border) = subform_border {
                        if border.is_visible() || border.fill.is_some() {
                            let range = &mut flattened_children[subform_children_start..];
                            Self::propagate_subform_border_to_children(
                                border,
                                range,
                                outer_pos.x,
                                outer_pos.y,
                                outer_pos.width,
                                finalised_height.unwrap_or(outer_pos.height),
                            );
                        }
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

                    // Record this subform's keep/break constraints, if any,
                    // now that `current_y` reflects its true bottom edge --
                    // for a growable subform (the common case: `keep`/`break`
                    // sit on a plain "tb" wrapper with no explicit `h`) that is
                    // only true after the fixup just above, not from
                    // `compute_position_for_node_with_children`'s own
                    // still-zero placeholder height for an unsized container.
                    Self::record_break_marker(
                        node,
                        Some(layout),
                        parent_layout,
                        outer_pos.y,
                        current_y,
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
                        // Skip .max(min_h) — minH on container subforms in lr-tb
                        // is a fixed-layout alignment property that shouldn't
                        // inflate the row height in a flowable web layout.
                        let effective_height = actual_height.max(consumed_height);
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
                            &mut row_growable_slack,
                        )?;

                    // For positioned layout, track the max extent (relative to parent_position)
                    if parent_layout == Layout::Position {
                        let node_bottom = (outer_pos.y - parent_position.y) + outer_pos.height;
                        max_extent_y = max_extent_y.max(node_bottom);
                    }

                    // Record this field's keep/break constraints, if any. In
                    // tb layout `compute_position_for_node_with_children` has
                    // already advanced `current_y` past this leaf's true
                    // bottom edge (unlike a growable container, a leaf's
                    // height is known up front), so no fixup is needed first.
                    Self::record_break_marker(node, None, parent_layout, outer_pos.y, current_y, ctx);

                    // Only add to output if not hidden
                    // For fields, use outer_pos for rendering (field box includes margins)
                    if !skip_render {
                        let field_name = node.name.clone().unwrap_or_else(|| "unnamed".to_string());
                        let field_value = Self::extract_field_value(&node.children);
                        let mut style = Self::extract_style(node);
                        Self::anchor_own_border(&mut style, outer_pos);
                        let is_checked = Self::compute_field_checked_state(
                            &node.children,
                            &child_ctx.parent_exclgroup_value,
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
                        // Add FieldBehavior hint with access level
                        field_node.add_hint(Hint::FieldBehavior {
                            access,
                            multiline: false,
                            max_length: None,
                            comb_cells: None,
                        });
                        // Add WidgetType hint if extracted
                        if let Some(kind) = widget_kind {
                            // Add Dropdown hint with options if this is a dropdown
                            if kind == WidgetKind::Dropdown
                                && let Some(dropdown_hint) = Self::extract_dropdown_hint(node)
                            {
                                field_node.add_hint(dropdown_hint);
                            }
                            if kind == WidgetKind::Barcode
                                && let Some(sym) = Self::extract_barcode_symbology(node)
                            {
                                field_node.add_hint(Hint::BarcodeSymbology(sym));
                            }
                            field_node.add_hint(Hint::WidgetType(kind));
                        }
                        // Add NoPrint hint if relevant="-print" or inherited from parent
                        if Self::is_no_print(node) || child_ctx.has_inherited_hint(&Hint::NoPrint) {
                            field_node.add_hint(Hint::NoPrint);
                        }
                        flattened_children.push(FlattenedKind::Node(field_node));
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
                            &mut row_growable_slack,
                        )?;

                    // For positioned layout, track the max extent (relative to parent_position)
                    if parent_layout == Layout::Position {
                        let node_bottom = (outer_pos.y - parent_position.y) + outer_pos.height;
                        max_extent_y = max_extent_y.max(node_bottom);
                    }

                    // Record this draw's keep/break constraints, if any --
                    // regardless of `skip_render`, matching the subform case:
                    // an invisible node still occupies layout space and can
                    // still carry a constraint on it. This is what lets a
                    // section-heading draw's `<keep next="contentArea"/>`
                    // (e.g. AAJB_033_IT's `Text_B2`) drag it onto the same
                    // page as the unsplittable question block that follows.
                    Self::record_break_marker(node, None, parent_layout, outer_pos.y, current_y, ctx);

                    // Only add to output if not hidden
                    if !skip_render {
                        // An image draw carries pixels rather than text.
                        if let Some(image_node) =
                            Self::flatten_image_draw(node, content_pos, &child_ctx)?
                        {
                            flattened_children.push(FlattenedKind::Node(image_node));
                            continue;
                        }

                        // Extract text content from draw node, or use empty (scripts may fill it)
                        // Use context to resolve xfa:embed references
                        let text_content =
                            child_ctx.extract_text(&node.children).unwrap_or_default();
                        let mut font_size = Self::extract_font_size(node);
                        let mut font_name = Self::extract_font_name(node);
                        let mut style = Self::extract_style(node);
                        Self::anchor_own_border(&mut style, outer_pos);

                        // Compute default_bold/italic from original XFA font BEFORE CSS overrides.
                        // CSS font-weight from individual paragraphs should not change the default
                        // for rich text parsing - each paragraph handles its own font-weight CSS.
                        let (default_bold, default_italic) = style.get_xfa_bold_italic();

                        // For HTML content (exData with contentType="text/html"), extract CSS font properties
                        // which may override the XFA <font> element values
                        let (css_font_family, css_font_size, css_is_bold) =
                            Self::extract_font_from_html_content(&node.children);
                        if let Some(ref name) = css_font_family {
                            font_name = name.clone();
                        }
                        if let Some(size) = css_font_size {
                            font_size = size;
                        }

                        // Only apply CSS overrides if at least one CSS property was found
                        if (css_font_size.is_some()
                            || css_font_family.is_some()
                            || css_is_bold.is_some())
                            && let Some(ref mut font) = style.font
                        {
                            // Apply CSS overrides if present
                            if let Some(size) = css_font_size {
                                font.size = size;
                            }
                            if let Some(ref family) = css_font_family {
                                font.typeface = family.clone();
                            }
                            if let Some(is_bold) = css_is_bold {
                                font.weight = if is_bold {
                                    crate::xfa::FontWeight::Bold
                                } else {
                                    crate::xfa::FontWeight::Normal
                                };
                            }
                        }

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
                            &mut row_growable_slack,
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
                    let children_height = Self::flatten_nodes(
                        &node.children,
                        content_pos,
                        layout,
                        flattened_children,
                        &exclgroup_ctx,
                    )?;

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

                    // Record this exclGroup's keep/break constraints, if
                    // any, now that `current_y` reflects its true bottom
                    // edge for a growable one (same reasoning as a subform,
                    // which this is treated like for layout purposes).
                    Self::record_break_marker(
                        node,
                        Some(layout),
                        parent_layout,
                        outer_pos.y,
                        current_y,
                        ctx,
                    );
                }
                XfaNodeKind::Element { tag_name, .. } => {
                    // Handle generic elements that might be containers
                    match tag_name.as_str() {
                        "subform" => {
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
                                    &mut row_growable_slack,
                                )?;

                            // For positioned layout, track the max extent (relative to parent_position)
                            if parent_layout == Layout::Position {
                                let node_bottom =
                                    (outer_pos.y - parent_position.y) + outer_pos.height;
                                max_extent_y = max_extent_y.max(node_bottom);
                            }

                            // Check if this subform has an <occur> element (repeatable section)
                            // If so, create a group to contain its children
                            let children_height = if let Some(occur) =
                                Self::extract_occur_constraints(node)
                            {
                                if occur.is_repeatable() && occur.has_initial_instances() {
                                    // Create a group for repeatable sections that have initial instances
                                    let mut group_children = Vec::new();
                                    let subform_ctx = child_ctx.with_occur_constraints(occur);
                                    let height = Self::flatten_nodes(
                                        &node.children,
                                        content_pos,
                                        layout,
                                        &mut group_children,
                                        &subform_ctx,
                                    )?;
                                    let hints = vec![Hint::Occurrence {
                                        min: occur.min,
                                        max: occur.max,
                                    }];
                                    flattened_children.push(FlattenedKind::Group {
                                        children: group_children,
                                        hints,
                                    });
                                    height
                                } else if occur.is_repeatable() && !occur.has_initial_instances() {
                                    // Repeatable but initial=0: skip entirely (no instances exist yet)
                                    Decimal::ZERO
                                } else {
                                    // Not repeatable, just recurse normally
                                    Self::flatten_nodes(
                                        &node.children,
                                        content_pos,
                                        layout,
                                        flattened_children,
                                        &child_ctx,
                                    )?
                                }
                            } else {
                                // No occur element, just recurse normally
                                Self::flatten_nodes(
                                    &node.children,
                                    content_pos,
                                    layout,
                                    flattened_children,
                                    &child_ctx,
                                )?
                            };

                            // For tb layout, update current_y based on actual content height
                            if parent_layout == Layout::TopToBottom && node.h.is_none() {
                                let actual_height = children_height
                                    + node.margin_top.unwrap_or(Decimal::ZERO)
                                    + node.margin_bottom.unwrap_or(Decimal::ZERO);
                                let min_h = node.min_h.unwrap_or(Decimal::ZERO);
                                let effective_height =
                                    actual_height.max(min_h).max(consumed_height);

                                if effective_height > consumed_height {
                                    current_y = outer_pos.y + effective_height;
                                }
                            }

                            // For lr-tb layout, update max_height_in_row based on actual
                            // content height (growable subform, nominal extent from children).
                            if matches!(
                                parent_layout,
                                Layout::LeftToRightTopToBottom | Layout::LeftToRight
                            ) && node.h.is_none()
                            {
                                let actual_height = children_height
                                    + node.margin_top.unwrap_or(Decimal::ZERO)
                                    + node.margin_bottom.unwrap_or(Decimal::ZERO);
                                // Skip .max(min_h) — see comment above for Subform.
                                let effective_height = actual_height.max(consumed_height);
                                max_height_in_row = max_height_in_row.max(effective_height);
                            }

                            // For positioned layout, update max_extent_y after recursion.
                            // Per XFA spec, a container without explicit h grows to fit
                            // its children's nominal extents.
                            if parent_layout == Layout::Position && node.h.is_none() {
                                let actual_height = children_height
                                    + node.margin_top.unwrap_or(Decimal::ZERO)
                                    + node.margin_bottom.unwrap_or(Decimal::ZERO);
                                let effective_height = actual_height.max(consumed_height);
                                let node_bottom =
                                    (outer_pos.y - parent_position.y) + effective_height;
                                max_extent_y = max_extent_y.max(node_bottom);
                            }
                        }
                        "field" => {
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
                                    &mut row_growable_slack,
                                )?;

                            // For positioned layout, track the max extent (relative to parent_position)
                            if parent_layout == Layout::Position {
                                let node_bottom =
                                    (outer_pos.y - parent_position.y) + outer_pos.height;
                                max_extent_y = max_extent_y.max(node_bottom);
                            }

                            // Only add to output if not hidden
                            if !skip_render {
                                let field_name =
                                    node.name.clone().unwrap_or_else(|| "unnamed".to_string());
                                let field_value = Self::extract_field_value(&node.children);
                                let mut style = Self::extract_style(node);
                                Self::anchor_own_border(&mut style, outer_pos);
                                let is_checked = Self::compute_field_checked_state(
                                    &node.children,
                                    &child_ctx.parent_exclgroup_value,
                                );
                                let access = Self::extract_field_access(node);
                                let widget_kind = Self::extract_widget_kind(node);

                                let mut field_node = FlattenedNode::new_field_with_checked(
                                    field_name.clone(),
                                    field_value.clone(),
                                    field_value,
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
                                if let Some(ref exclgroup_path) =
                                    child_ctx.parent_exclgroup_som_path
                                {
                                    field_node
                                        .add_hint(Hint::ExclGroupSomPath(exclgroup_path.clone()));
                                }
                                // Add FieldBehavior hint with access level
                                field_node.add_hint(Hint::FieldBehavior {
                                    access,
                                    multiline: false,
                                    max_length: None,
                                    comb_cells: None,
                                });
                                // Add WidgetType hint if we could determine it
                                if let Some(widget_kind) = widget_kind {
                                    // Add Dropdown hint with options if this is a dropdown
                                    if widget_kind == WidgetKind::Dropdown
                                        && let Some(dropdown_hint) =
                                            Self::extract_dropdown_hint(node)
                                    {
                                        field_node.add_hint(dropdown_hint);
                                    }
                                    if widget_kind == WidgetKind::Barcode
                                        && let Some(sym) = Self::extract_barcode_symbology(node)
                                    {
                                        field_node.add_hint(Hint::BarcodeSymbology(sym));
                                    }
                                    field_node.add_hint(Hint::WidgetType(widget_kind));
                                }
                                // Add NoPrint hint if relevant="-print" or inherited from parent
                                if Self::is_no_print(node)
                                    || child_ctx.has_inherited_hint(&Hint::NoPrint)
                                {
                                    field_node.add_hint(Hint::NoPrint);
                                }
                                flattened_children.push(FlattenedKind::Node(field_node));
                            }
                        }
                        "draw" => {
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
                                    &mut row_growable_slack,
                                )?;

                            // For positioned layout, track the max extent (relative to parent_position)
                            if parent_layout == Layout::Position {
                                let node_bottom =
                                    (outer_pos.y - parent_position.y) + outer_pos.height;
                                max_extent_y = max_extent_y.max(node_bottom);
                            }

                            // Only add to output if not hidden
                            if !skip_render {
                                // An image draw carries pixels rather than text.
                                if let Some(image_node) =
                                    Self::flatten_image_draw(node, content_pos, &child_ctx)?
                                {
                                    flattened_children.push(FlattenedKind::Node(image_node));
                                    continue;
                                }

                                // Draw nodes render text or images - use empty string if no content (scripts may fill it)
                                // Use context to resolve xfa:embed references
                                let text_content =
                                    child_ctx.extract_text(&node.children).unwrap_or_default();
                                let mut font_size = Self::extract_font_size(node);
                                let mut font_name = Self::extract_font_name(node);
                                let mut style = Self::extract_style(node);
                                Self::anchor_own_border(&mut style, outer_pos);

                                // Compute default_bold/italic from original XFA font BEFORE CSS overrides.
                                // CSS font-weight from individual paragraphs should not change the default
                                // for rich text parsing - each paragraph handles its own font-weight CSS.
                                let (default_bold, default_italic) = style.get_xfa_bold_italic();

                                // For HTML content (exData with contentType="text/html"), extract CSS font properties
                                // which may override the XFA <font> element values
                                let (css_font_family, css_font_size, css_is_bold) =
                                    Self::extract_font_from_html_content(&node.children);
                                if let Some(ref name) = css_font_family {
                                    font_name = name.clone();
                                }
                                if let Some(size) = css_font_size {
                                    font_size = size;
                                }

                                // Only apply CSS overrides if at least one CSS property was found
                                if (css_font_size.is_some()
                                    || css_font_family.is_some()
                                    || css_is_bold.is_some())
                                    && let Some(ref mut font) = style.font
                                {
                                    // Apply CSS overrides if present
                                    if let Some(size) = css_font_size {
                                        font.size = size;
                                    }
                                    if let Some(ref family) = css_font_family {
                                        font.typeface = family.clone();
                                    }
                                    if let Some(is_bold) = css_is_bold {
                                        font.weight = if is_bold {
                                            crate::xfa::FontWeight::Bold
                                        } else {
                                            crate::xfa::FontWeight::Normal
                                        };
                                    }
                                }

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
                                let mut draw_kinds = Self::split_draw_into_paragraph_nodes(
                                    draw_node,
                                    &child_ctx.language,
                                );
                                // Add NoPrint hint if relevant="-print" or inherited from parent
                                if Self::is_no_print(node)
                                    || child_ctx.has_inherited_hint(&Hint::NoPrint)
                                {
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
                        "template" | "pageSet" | "pageArea" | "contentArea" => {
                            // NOTE: These should NOT normally be encountered when processing Form DOM content,
                            // since find_root_subform returns the content subform (sibling to pageSet).
                            // This fallback handles edge cases or malformed documents.
                            // Per XFA spec: pageSet/pageArea define page structure and are NOT part of Form DOM.
                            let child_layout = if tag_name == "pageArea" {
                                Layout::Position
                            } else {
                                parent_layout
                            };

                            Self::flatten_nodes(
                                &node.children,
                                parent_position,
                                child_layout,
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
                XfaNodeKind::Template | XfaNodeKind::ContentArea | XfaNodeKind::PageSet => {
                    // NOTE: These should NOT normally be encountered when processing Form DOM content.
                    // This handles fallback cases. Pass through with same parent position and layout.
                    Self::flatten_nodes(
                        &node.children,
                        parent_position,
                        parent_layout,
                        flattened_children,
                        &child_ctx,
                    )?;
                }
                XfaNodeKind::PageArea => {
                    // NOTE: PageArea should NOT normally be encountered when processing Form DOM content.
                    // Page background (pageArea children) are handled separately in from_xfa().
                    // This fallback handles edge cases - pass through with positioned layout.
                    Self::flatten_nodes(
                        &node.children,
                        parent_position,
                        Layout::Position,
                        flattened_children,
                        &child_ctx,
                    )?;
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

    /// A node's `<keep>` child, if it asks for anything (XFA 3.3 §8 "Content
    /// Splitting"). `none`/absent on each sub-property is the default and
    /// means "no constraint", so a `<keep>` with every sub-property `none`
    /// is the same as no `<keep>` at all.
    fn keep_of(node: &XfaNode) -> Option<(bool, bool, bool)> {
        for child in &node.children {
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                && tag_name == "keep"
            {
                let forces = |attr: &str| {
                    matches!(
                        child.attributes.get(attr).map(String::as_str),
                        Some("contentArea") | Some("pageArea")
                    )
                };
                let (intact, next, previous) =
                    (forces("intact"), forces("next"), forces("previous"));
                return if intact || next || previous {
                    Some((intact, next, previous))
                } else {
                    None
                };
            }
        }
        None
    }

    /// A node's `<breakBefore>`/`<breakAfter>` children, if either forces a
    /// move to a new content area or page (XFA 3.3 §7 "Break Conditions").
    /// The `targetType` default is `auto` ("layout continues using the
    /// current layout container"), so a bare `<breakBefore/>` with no
    /// `targetType` -- common in Designer output, left over from reordering
    /// -- forces nothing; only `contentArea`, `pageArea`, `pageEven` and
    /// `pageOdd` do. The deprecated combined `<break>` element uses
    /// `before=`/`after=` attributes with the same value set.
    fn forced_break(node: &XfaNode) -> (bool, bool) {
        let forces = |v: Option<&String>| {
            matches!(
                v.map(String::as_str),
                Some("contentArea") | Some("pageArea") | Some("pageEven") | Some("pageOdd")
            )
        };
        let (mut before, mut after) = (false, false);
        for child in &node.children {
            let XfaNodeKind::Element { tag_name, .. } = &child.kind else {
                continue;
            };
            match tag_name.as_str() {
                "breakBefore" => before |= forces(child.attributes.get("targetType")),
                "breakAfter" => after |= forces(child.attributes.get("targetType")),
                "break" => {
                    before |= forces(child.attributes.get("before"));
                    after |= forces(child.attributes.get("after"));
                }
                _ => {}
            }
        }
        (before, after)
    }

    /// Record `node`'s `keep`/`breakBefore`/`breakAfter` constraints as a
    /// [`BreakMarker`], if it has any and it is a direct child of the
    /// `layout="tb"` container currently being page-broken (XFA 3.3 §7-§8).
    ///
    /// `own_layout` is `node`'s own layout attribute for a container
    /// (`subform`/`exclGroup`); `None` for a leaf (`draw`/`field`), which has
    /// no layout of its own. `top`/`bottom` are the node's final extent in
    /// absolute column coordinates -- for a growable container this must be
    /// its true bottom edge (`current_y` after the caller's own height
    /// fixup), not `compute_position_for_node_with_children`'s still-zero
    /// placeholder height for an unsized container.
    ///
    /// Every kind of `layout="tb"` child needs this, not only subforms: a
    /// UBS section heading is a plain `<draw>` with `<keep next="contentArea"/>`
    /// directly followed by the positioned question block it introduces
    /// (`Text_B2` -> `STP_Economica` in AAJB_033_IT), and a `<keep>` the
    /// layout never records cannot be honored by `snap_break`.
    fn record_break_marker(
        node: &XfaNode,
        own_layout: Option<Layout>,
        parent_layout: Layout,
        top: Num,
        bottom: Num,
        ctx: &FlattenContext,
    ) {
        if parent_layout != Layout::TopToBottom {
            return;
        }
        let Some(cell) = ctx.break_markers else {
            return;
        };
        let keep = Self::keep_of(node);
        let (force_before, force_after) = Self::forced_break(node);
        // A subform/exclGroup whose own layout is "position" (XFA's default
        // when no `layout` attribute is given) is never split across pages
        // by any viewer we have observed: per XFA 3.3 ch. 8 "Content
        // Splitting", "the individual object has a single position relative
        // to the other positioned contents of its immediate container" -- an
        // ancestor using flowing layout may still split *around* it, but its
        // own children are never torn apart from each other. Treat it the
        // same as an explicit `keep intact="contentArea"`. Not applicable to
        // a leaf, which has no `own_layout` and nothing of its own to split.
        let positioned_intact = own_layout == Some(Layout::Position);
        if keep.is_some() || force_before || force_after || positioned_intact {
            let (intact, keep_next, keep_previous) = keep.unwrap_or_default();
            cell.borrow_mut().push(BreakMarker {
                top,
                bottom,
                intact: intact || positioned_intact,
                keep_next,
                keep_previous,
                force_before,
                force_after,
            });
        }
    }

    /// A content subform's own page-area target: the `target` of a direct
    /// `<breakBefore targetType="pageArea" target="...">` child (or the
    /// deprecated `<break before="pageArea" beforeTarget="...">`), if it has
    /// one, as the last SOM segment (`"MP2Set.MP1"` -> `"MP1"`).
    ///
    /// Per XFA 3.3 §7 "Break Conditions", this is how a content subform
    /// states which pageArea -- and therefore which pageSet -- its own pages
    /// start on (e.g. AAOV's `Page2` states `target="MP2Set.MP1"`, moving it
    /// onto a different pageSet than `Page`, which precedes it). `None` means
    /// the subform does not care, and its pages are chosen from every
    /// pageArea in the document, as before this had a name.
    fn content_subform_page_area_target(node: &XfaNode) -> Option<&str> {
        for child in &node.children {
            let XfaNodeKind::Element { tag_name, .. } = &child.kind else {
                continue;
            };
            let target = match tag_name.as_str() {
                "breakBefore"
                    if child.attributes.get("targetType").map(String::as_str)
                        == Some("pageArea") =>
                {
                    child.attributes.get("target")
                }
                "break" if child.attributes.get("before").map(String::as_str) == Some("pageArea") => {
                    child.attributes.get("beforeTarget")
                }
                _ => None,
            };
            if let Some(t) = target {
                return t.rsplit('.').next();
            }
        }
        None
    }

    /// Group content subforms into "runs": maximal consecutive sequences that
    /// resolve (via [`Self::content_subform_page_area_target`]) to the same
    /// pageSet -- or, for subforms with no target, to the same fallback of
    /// "every pageArea in the document" (today's behaviour, preserved for a
    /// document with one pageSet or with untargeted content subforms).
    ///
    /// Returns, in subform order, the run id each subform belongs to, and
    /// each run's own pool of pageAreas to choose a master page from.
    fn group_content_subforms_into_runs<'a>(
        content_subforms: &[(&XfaNode, String)],
        all_page_areas: &[PageAreaRef<'a>],
    ) -> (Vec<usize>, Vec<Vec<PageAreaRef<'a>>>) {
        let targets: Vec<Option<&XfaNode>> = content_subforms
            .iter()
            .map(|(subform, _)| {
                Self::content_subform_page_area_target(subform).and_then(|name| {
                    all_page_areas
                        .iter()
                        .find(|r| r.page_area.name.as_deref() == Some(name))
                        .and_then(|r| r.page_set)
                        .or_else(|| {
                            all_page_areas
                                .iter()
                                .find(|r| r.page_set.is_some_and(|ps| ps.name.as_deref() == Some(name)))
                                .and_then(|r| r.page_set)
                        })
                })
            })
            .collect();

        let mut run_ids = Vec::with_capacity(targets.len());
        let mut run_pools: Vec<Vec<PageAreaRef<'a>>> = Vec::new();
        let mut prev_key: Option<Option<*const XfaNode>> = None;
        for target in &targets {
            let key = target.map(|ps| ps as *const XfaNode);
            if prev_key != Some(key) {
                let pool: Vec<PageAreaRef<'a>> = match target {
                    Some(ps) => all_page_areas
                        .iter()
                        .filter(|r| r.page_set.is_some_and(|p| std::ptr::eq(p, *ps)))
                        .copied()
                        .collect(),
                    None => all_page_areas.to_vec(),
                };
                run_pools.push(pool);
                prev_key = Some(key);
            }
            run_ids.push(run_pools.len() - 1);
        }
        (run_ids, run_pools)
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
        row_growable_slack: &mut Num,
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
                XfaNodeKind::Element { tag_name, .. } if tag_name == "draw" => {
                    // Same logic for generic draw elements
                    let natural_width = Self::growable_draw_width(node, ctx);
                    let min_w = node.min_w.unwrap_or(Decimal::ZERO);
                    let max_w = node.max_w;

                    let width = natural_width.max(min_w);
                    if let Some(max) = max_w {
                        width.min(max)
                    } else {
                        width
                    }
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
                XfaNodeKind::Field => {
                    // For fields, calculate based on font size + margins
                    // Per XFA spec: natural height of text widget is height of text block
                    let font_size = node
                        .font
                        .as_ref()
                        .map(|f| f.size)
                        .unwrap_or_else(|| num(10.0));
                    // Line gap of 20% plus some padding
                    let content_height = font_size * num(1.4); // Font size + 20% line gap + padding
                    let total_height = content_height + margin_top + margin_bottom;
                    total_height.max(min_height)
                }
                XfaNodeKind::Element { tag_name, .. } => {
                    match tag_name.as_str() {
                        "draw" => Self::natural_draw_height(
                            node,
                            (width - margin_left - margin_right).max(Decimal::ZERO),
                            min_height,
                            margin_top,
                            margin_bottom,
                            ctx,
                        ),
                        "field" => {
                            let font_size = node
                                .font
                                .as_ref()
                                .map(|f| f.size)
                                .unwrap_or_else(|| num(10.0));
                            let content_height = font_size * num(1.4);
                            let total_height = content_height + margin_top + margin_bottom;
                            total_height.max(min_height)
                        }
                        _ => {
                            // Containers: children determine height. Don't apply
                            // minH here — the parent-layout-specific branches
                            // handle it (tb/position apply .max(min_height) but
                            // lr-tb intentionally does not, since minH on container
                            // subforms is a fixed-layout alignment property that
                            // shouldn't inflate row height in flowable web layout).
                            Decimal::ZERO
                        }
                    }
                }
                _ => {
                    // Containers: children determine height. Don't apply
                    // minH here — see comment above for Element containers.
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
                let mut should_wrap = *current_x + width
                    > parent_position.x + parent_position.width
                    && *current_x > parent_position.x;

                // Suppress false wrapping caused by heuristic width overestimation of
                // preceding growable elements. The `row_growable_slack` accumulates how
                // much wider those elements were estimated vs their actual font-measured
                // width. If the overflow is within that slack AND the current element
                // has an explicit width, the row would fit with accurate measurements.
                // Only explicitly-sized elements qualify because growable elements that
                // overflow should wrap normally (their overflow is content-driven).
                if should_wrap && node.w.is_some() {
                    let overflow =
                        (*current_x + width) - (parent_position.x + parent_position.width);
                    if overflow <= *row_growable_slack {
                        should_wrap = false;
                    }
                }

                if should_wrap {
                    // Wrap to next line
                    *current_x = parent_position.x;
                    *current_y += *max_height_in_row;
                    *max_height_in_row = Decimal::ZERO;
                    *row_growable_slack = Decimal::ZERO;
                }

                // Track heuristic slack for growable Draw elements (w not specified).
                // The heuristic `calculate_natural_text_width` uses 60%-of-em which
                // overestimates for most proportional fonts. Font-based measurement
                // gives the accurate width; the difference is "slack" that can absorb
                // overflow of later explicitly-sized siblings.
                if node.w.is_none() {
                    let is_draw = matches!(&node.kind, XfaNodeKind::Draw)
                        || matches!(
                            &node.kind,
                            XfaNodeKind::Element { tag_name, .. } if tag_name == "draw"
                        );
                    if is_draw {
                        let text = ctx.extract_text(&node.children).unwrap_or_default();
                        if !text.is_empty() {
                            let font_based =
                                Self::calculate_natural_text_width_font_based(&text, &node.font);
                            // Only count slack if font measurement is plausible (≥60% of heuristic).
                            // This guards against incorrect font resolution producing
                            // unrealistically small widths that inflate slack.
                            if width > font_based && font_based >= width * num(0.6) {
                                *row_growable_slack += width - font_based;
                            }
                        }
                    }
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
    /// all non-hidden children. For children without explicit w/h, estimates are
    /// used (min_w/min_h, font-based sizing, or 0).
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

            // Estimate child height: use explicit h, else min_h, else font-based guess
            let ch = child.h.unwrap_or_else(|| {
                child.min_h.unwrap_or_else(|| {
                    // Font-based estimate for leaf nodes
                    match &child.kind {
                        XfaNodeKind::Field => {
                            let fs = child
                                .font
                                .as_ref()
                                .map(|f| f.size)
                                .unwrap_or_else(|| num(10.0));
                            fs * num(1.4)
                                + child.margin_top.unwrap_or(Decimal::ZERO)
                                + child.margin_bottom.unwrap_or(Decimal::ZERO)
                        }
                        XfaNodeKind::Draw | XfaNodeKind::Element { .. } => {
                            let fs = child
                                .font
                                .as_ref()
                                .map(|f| f.size)
                                .unwrap_or_else(|| num(10.0));
                            fs * num(1.4)
                                + child.margin_top.unwrap_or(Decimal::ZERO)
                                + child.margin_bottom.unwrap_or(Decimal::ZERO)
                        }
                        _ => Decimal::ZERO,
                    }
                })
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
    fn growable_draw_width(node: &XfaNode, ctx: &FlattenContext) -> Num {
        let text = ctx.extract_text(&node.children).unwrap_or_default();
        Self::calculate_natural_text_width(&text, &node.font)
            + node.margin_left.unwrap_or(Decimal::ZERO)
            + node.margin_right.unwrap_or(Decimal::ZERO)
    }

    fn calculate_natural_text_width(text: &str, font: &Option<Font>) -> Num {
        // Get font size from style or use default
        let font_size = font.as_ref().map(|f| f.size).unwrap_or_else(|| num(10.0));

        let font_size_f32 = font_size.to_f32().unwrap_or(10.0);

        // Approximate character width as 60% of font size (rough estimate)
        // This is a simplified calculation; for accurate width we'd need actual font metrics
        let char_width = font_size_f32 * 0.6;

        // Calculate width of the text
        let text_width = text.chars().count() as f32 * char_width;

        // Add some padding for margins
        let padded_width = text_width + font_size_f32 * 0.5;

        Decimal::from_f32(padded_width).unwrap_or_else(|| num(100.0))
    }

    /// Calculate the natural width using font-based measurement for accurate results.
    /// Returns the actual text width based on real font metrics (used for determining
    /// how much the heuristic overestimates in lr-tb slack tracking).
    fn calculate_natural_text_width_font_based(text: &str, font: &Option<Font>) -> Num {
        let font_size = font.as_ref().map(|f| f.size).unwrap_or_else(|| num(10.0));

        let xfa_font = font.clone().unwrap_or_default();
        let mut measurer = TextMeasurer::new();
        let large_width = num(10000.0);
        if let Ok(block_metrics) =
            measurer.measure_text_block(text, &Some(xfa_font), &None, large_width)
        {
            if block_metrics.total_width > Decimal::ZERO {
                // Match the heuristic's padding style for consistent comparison
                return block_metrics.total_width + font_size * num(0.5);
            }
        }

        // If font-based measurement fails, return the heuristic (no slack)
        Self::calculate_natural_text_width(text, font)
    }

    /// Estimate the number of text lines for word-wrapped content.
    /// This is a helper used for height calculation.
    fn estimate_line_count(text: &str, chars_per_line: usize) -> usize {
        let words: Vec<&str> = text.split_whitespace().collect();
        let mut num_lines: usize = 1;
        let mut current_line_chars: usize = 0;

        for word in words {
            let word_chars = word.chars().count();
            if current_line_chars == 0 {
                current_line_chars = word_chars;
            } else if current_line_chars + 1 + word_chars <= chars_per_line {
                current_line_chars += 1 + word_chars;
            } else {
                num_lines += 1;
                current_line_chars = word_chars;
            }
        }

        if text.is_empty() { 1 } else { num_lines }
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
            Self::calculate_rich_text_draw_height(
                &node.children,
                &node.font,
                &node.para,
                content_width,
                ctx.computed_values,
                ctx.id_to_field,
                &ctx.language,
            )
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
                num(12.0)
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
        let font_size = font.as_ref().map(|f| f.size).unwrap_or_else(|| num(10.0));
        let font_size_f32 = font_size.to_f32().unwrap_or(10.0);
        let xfa_font = font.clone().unwrap_or_default();
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
            Err(_) => {
                // Fallback: crude character-width estimate (used when font is unavailable)
                let char_width = font_size_f32 * 0.45;
                let max_width_f32 = max_width.to_f32().unwrap_or(1000.0);
                let chars_per_line = (max_width_f32 / char_width).max(1.0) as usize;
                let num_lines = Self::estimate_line_count(text, chars_per_line) + extra_lines;

                let line_height = para
                    .as_ref()
                    .and_then(|p| p.line_height)
                    .unwrap_or(font_size * num(1.2));
                let margin_top = para
                    .as_ref()
                    .and_then(|p| p.space_above)
                    .unwrap_or(Decimal::ZERO);
                let margin_bottom = para
                    .as_ref()
                    .and_then(|p| p.space_below)
                    .unwrap_or(Decimal::ZERO);

                if num_lines <= 1 {
                    margin_top + font_size + margin_bottom
                } else {
                    let line_gap = line_height - font_size;
                    let lines_height = num(num_lines as f64) * line_height - line_gap;
                    margin_top + lines_height + margin_bottom
                }
            }
        }
    }

    /// Calculate the natural height for a text/draw element (convenience wrapper).
    fn calculate_natural_text_height(
        text: &str,
        font: &Option<Font>,
        para: &Option<Para>,
        max_width: Num,
    ) -> Num {
        Self::calculate_natural_text_height_with_paragraphs(text, font, para, max_width, 0)
    }

    /// Calculate the natural content height for a draw element that has rich text
    /// (HTML exData with multiple `<p>` paragraphs at potentially different font sizes).
    ///
    /// Delegates to `rich_paragraph_heights`, the same per-paragraph measurement
    /// `split_draw_into_paragraph_nodes` uses to place the paragraphs, so a
    /// draw's natural height can never disagree with where its own content
    /// actually lands. That function charges every line a full derived line
    /// spacing (text height + line gap); per XFA AXTE spec (Appendix C, "Last
    /// line in block: Any line gap value does not apply to the last line in a
    /// text block"), one line gap is removed here, sized to the last visible
    /// paragraph's own font.
    fn calculate_rich_text_draw_height(
        children: &[XfaNode],
        node_font: &Option<Font>,
        node_para: &Option<Para>,
        max_width: Num,
        computed_values: &HashMap<SomPath, String>,
        id_to_field: &HashMap<String, String>,
        language: &str,
    ) -> Option<Num> {
        // Extract the default h_align and then parse rich text
        let default_h_align = node_para
            .as_ref()
            .map(|p| p.h_align)
            .unwrap_or(HAlign::Left);

        let default_bold = node_font
            .as_ref()
            .map(|f| f.weight == crate::xfa::FontWeight::Bold)
            .unwrap_or(false);
        let default_italic = node_font
            .as_ref()
            .map(|f| f.posture == crate::xfa::FontPosture::Italic)
            .unwrap_or(false);
        let rich_text = Self::extract_rich_text_from_node(
            children,
            default_h_align,
            Some(computed_values),
            Some(id_to_field),
            default_bold,
            default_italic,
        )?;

        if rich_text.paragraphs.len() <= 1 {
            return None; // Single paragraph — fall back to the standard heuristic
        }

        let base_font_size = node_font
            .as_ref()
            .map(|f| f.size)
            .unwrap_or_else(|| num(10.0));
        let base_font_name = node_font
            .as_ref()
            .map(|f| f.typeface.clone())
            .unwrap_or_default();

        let xfa_font = node_font.clone().unwrap_or_else(|| Font {
            typeface: base_font_name,
            size: base_font_size,
            ..Font::default()
        });

        let paragraph_heights =
            Self::rich_paragraph_heights(&rich_text, &xfa_font, node_para, max_width, language);
        let total_height: Num = paragraph_heights.iter().copied().sum();

        let last_font_size = rich_text
            .paragraphs
            .iter()
            .rev()
            .find(|p| !p.is_empty && p.runs.iter().any(|r| !r.text.trim().is_empty()))
            .and_then(|p| p.font_size)
            .map(|s| num(s as f64))
            .unwrap_or(base_font_size);
        // AXTE line gap is always 20% of font size (see the AXTE_LINE_GAP_RATIO
        // constant used throughout this file's height calculations).
        let last_line_gap = last_font_size * num(0.2);

        Some((total_height - last_line_gap).max(Decimal::ZERO))
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
            XfaNodeKind::Element { tag_name, .. } if tag_name == "draw" => {
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
                    if w > Decimal::ZERO { w } else { num(20.0) }
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

    /// A `<draw>` whose value is an embedded image becomes one image node.
    ///
    /// Returns `Ok(None)` for an ordinary text draw, so a caller can fall
    /// through to the text path.
    fn flatten_image_draw(
        node: &XfaNode,
        pos: Position,
        ctx: &FlattenContext,
    ) -> Result<Option<FlattenedNode>, String> {
        let Some(image) = Self::extract_value_image(node)? else {
            return Ok(None);
        };

        let mut image_node = FlattenedNode::builder()
            .bounds(pos.x, pos.y, pos.width, pos.height)
            .style(Self::extract_style(node))
            .rotate(node.rotate)
            .image(image)
            .build();

        if Self::is_no_print(node) || ctx.has_inherited_hint(&Hint::NoPrint) {
            image_node.add_hint(Hint::NoPrint);
        }
        if let Some(name) = &node.name {
            image_node.add_hint(Hint::SomPath(SomPath::new(ctx.get_full_path(name))));
        }
        Ok(Some(image_node))
    }

    /// Extract the key value from a field's `<items>` element
    ///
    /// Per XFA spec (section 4 "Exclusion Groups"):
    /// Each field within an exclusion group is associated with a key value from its <items> element.
    /// When a field is activated, the exclGroup's rawValue is set to that field's key.
    /// A field is "on" when exclGroup.rawValue == field.items[0].text
    fn extract_field_item_key(children: &[XfaNode]) -> Option<String> {
        for child in children {
            // Look for <items> element
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                && tag_name == "items"
            {
                // Get the first <text> child's content
                for item_child in &child.children {
                    if let XfaNodeKind::Element {
                        tag_name: t2,
                        text_content,
                        ..
                    } = &item_child.kind
                        && t2 == "text"
                        && let Some(text) = text_content
                    {
                        return Some(text.clone());
                    }
                    if let XfaNodeKind::Text { content } = &item_child.kind {
                        return Some(content.clone());
                    }
                }
            }
        }
        None
    }

    fn extract_text_content(children: &[XfaNode]) -> Option<String> {
        // Use empty context for backward compatibility
        Self::extract_text_content_with_embed(children, &HashMap::new(), &HashMap::new())
    }

    /// Compute the is_checked state for a field that may be in an exclGroup.
    /// Per XFA spec: A field in an exclGroup is "on" when exclGroup.rawValue == field.items[0].text
    fn compute_field_checked_state(
        children: &[XfaNode],
        parent_exclgroup_value: &Option<String>,
    ) -> Option<bool> {
        parent_exclgroup_value.as_ref().and_then(|exclgroup_value| {
            Self::extract_field_item_key(children).map(|key| exclgroup_value == &key)
        })
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

    /// Resolve an xfa:embed reference to the actual field value
    ///
    /// The embed reference can be:
    /// - A URI fragment like "#uuid:field_id" (xfa:embedType="uri")
    /// - A SOM expression like "FIELD_NAME" (xfa:embedType="som")
    ///
    /// Values in computed_values are now stored BOTH by field ID (primary) and by name (fallback).
    /// This handles the case where multiple subforms have same-named children with different IDs.
    fn resolve_embed_reference(
        embed_ref: &str,
        computed_values: &HashMap<SomPath, String>,
        id_to_field: &HashMap<String, String>,
    ) -> Option<String> {
        // Handle URI reference (starts with #)
        if let Some(id) = embed_ref.strip_prefix('#') {
            // Remove the # prefix

            // FIRST: Try to look up the value directly by ID (preferred - handles multiple same-named fields)
            if let Some(value) = computed_values.get(id) {
                return Some(value.clone());
            }

            // SECOND: Look up the field name from the ID, then look up by name (fallback)
            if let Some(field_name) = id_to_field.get(id) {
                return computed_values.get(field_name.as_str()).cloned();
            }

            return None;
        }

        // Handle SOM expression (no # prefix) - direct field name reference
        computed_values.get(embed_ref).cloned()
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

        let page_h_px = (self.page.height * scale_dec)
            .to_f32()
            .unwrap_or(0.0)
            .round() as u32;
        let page_count = self.page.page_count.max(1);
        if page_h_px == 0 || page_count <= 1 {
            return vec![full];
        }

        // Build vertical bands of one page each, clipped to the buffer: a
        // buffer shorter than the layout claims still yields whole pages up to
        // where it ends rather than an out-of-range crop.
        let mut bands: Vec<(u32, u32)> = Vec::with_capacity(page_count as usize);
        for k in 0..page_count {
            let top = k * page_h_px;
            if top >= full_h {
                break;
            }
            bands.push((top, ((k + 1) * page_h_px).min(full_h)));
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
        // Scale dimensions for better resolution (e.g., scale=2.0 for 2x)
        let scale_dec = num(scale as f64);

        // Width is fixed to page width
        let img_width = (self.page.width * scale_dec).to_f32().unwrap_or(0.0) as u32;

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

        // Colors (RGBA - last value is alpha: 255=opaque, 0=transparent)
        let black = Rgba([0u8, 0u8, 0u8, 255u8]);
        let dark_gray = Rgba([80u8, 80u8, 80u8, 255u8]);

        // Resolve hyphenation dictionary for the document language so that
        // text wrapping during rendering matches the height calculation in
        // split_draw_into_paragraph_nodes (which already uses the dict).
        let hyph_dict = super::xfa::hyphenation::dict_for_language(&self.language);

        // ============================================
        // Draw actual content (as in PDF) - no debug overlay
        // ============================================
        for node in self.iter_nodes() {
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

            // Skip nodes outside the visible area or with invalid dimensions
            if x < 0 || y < 0 || w <= 0 || h <= 0 {
                continue;
            }

            // Draw fill background if present. A fill propagated from a
            // parent subform's border (see
            // `propagate_subform_border_to_children`) carries `render_bounds`
            // so it paints at the subform's own position, matching its
            // propagated edges, rather than at just this one leaf's box.
            if let Some(border) = &node.style.border
                && let Some(fill) = &border.fill
                && fill.presence != "hidden"
                && fill.presence != "inactive"
                && let Some((r, g, b)) = fill.color
            {
                let (fx, fy, fw, fh) =
                    Self::resolve_border_render_bounds(border, x, y, w, h, scale_dec);
                Self::fill_rect(&mut img, fx, fy, fw, fh, Rgba([r, g, b, 255u8]));
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
                            Self::draw_barcode(
                                &mut img,
                                node.barcode_symbology().unwrap_or("unspecified"),
                                value,
                                x,
                                y,
                                w,
                                h,
                                black,
                            );
                        }
                    } else if is_checkbox_or_radio {
                        // A checkButton is square, unless shape="round" made it a
                        // radio -- which is a circle in every viewer, and reads as
                        // a different control when drawn as a box.
                        let box_size = (w.min(h) as f32 * 0.8).max(8.0) as i32;
                        let box_x = x + (w - box_size) / 2;
                        let box_y = y + (h - box_size) / 2;
                        let line_color = Rgba([80u8, 80u8, 80u8, 255u8]);

                        if is_radio {
                            let radius = box_size / 2;
                            let cx = box_x + radius;
                            let cy = box_y + radius;
                            // Ring: a filled disc with the interior punched back
                            // out, so the stroke scales with the widget.
                            let stroke = ((scale).round() as i32).max(1);
                            Self::fill_circle(&mut img, cx, cy, radius, line_color);
                            Self::fill_circle(
                                &mut img,
                                cx,
                                cy,
                                (radius - stroke).max(0),
                                Rgba([255u8, 255u8, 255u8, 255u8]),
                            );
                            if let Some(true) = is_checked {
                                let dot = ((radius - stroke * 2).max(1) as f32 * 0.8) as i32;
                                Self::fill_circle(&mut img, cx, cy, dot.max(1), line_color);
                            }
                        } else {
                            Self::draw_rect_outline(
                                &mut img, box_x, box_y, box_size, box_size, line_color,
                            );

                            if let Some(true) = is_checked {
                                let inset = (box_size as f32 * 0.2) as i32;
                                Flattened::fill_rect(
                                    &mut img,
                                    box_x + inset,
                                    box_y + inset,
                                    box_size - 2 * inset,
                                    box_size - 2 * inset,
                                    line_color,
                                );
                            }
                        }
                    } else {
                        // Text/choice fields: draw an underline instead of a blue
                        // fill, as Acrobat highlights a fillable field. Acrobat
                        // does not highlight a field the user cannot type into
                        // (access="protected"/"readOnly"/"nonInteractive"), so
                        // neither do we -- only "open" fields get the synthetic
                        // line; everything else shows only its own template
                        // border, if any.
                        let access = node
                            .field_behavior()
                            .map(|(access, ..)| access)
                            .unwrap_or(FieldAccess::Open);
                        if access.is_interactive() {
                            let underline_color = Rgba([0u8, 0u8, 0u8, 180u8]);
                            let line_y = y + h - 1;
                            Flattened::fill_rect(&mut img, x, line_y, w, 1, underline_color);
                        }

                        // Only draw field VALUE (not name) in black if present
                        if !value.is_empty() {
                            // Get font style from node, or use XFA defaults
                            let xfa_font = node.style.font.clone().unwrap_or_default();
                            let font_size = xfa_font.size.to_f32().unwrap_or(10.0);
                            let scaled_font_size = (font_size * scale).max(8.0);

                            // Get the appropriate font for this style (with fallback)
                            let render_font = {
                                let mut mgr = font_manager
                                    .lock()
                                    .map_err(|e| format!("Lock error: {}", e))?;
                                mgr.get_font(&xfa_font)
                                    .unwrap_or_else(|_| fallback_font.clone())
                            };
                            let text_scale = xfa_px_scale(&render_font, scaled_font_size);

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

                            draw_text_mut(
                                &mut img,
                                text_color,
                                text_x,
                                text_y,
                                text_scale,
                                &render_font,
                                value,
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
                    let scaled_font_size = (effective_font_size * scale).max(8.0);

                    // Get the appropriate font for this style (with fallback)
                    // Also get bold and italic variants for rich text rendering
                    let (render_font, normal_font, bold_font, italic_font, bold_italic_font) = {
                        let mut mgr = font_manager
                            .lock()
                            .map_err(|e| format!("Lock error: {}", e))?;

                        // Get font as specified in XFA (may be bold/italic)
                        let base = mgr
                            .get_font(&xfa_font)
                            .unwrap_or_else(|_| fallback_font.clone());

                        // Get normal weight variant (for rich text base)
                        let mut normal_xfa_font = xfa_font.clone();
                        normal_xfa_font.weight = crate::xfa::FontWeight::Normal;
                        normal_xfa_font.posture = crate::xfa::FontPosture::Normal;
                        let normal = mgr.get_font(&normal_xfa_font).ok();

                        // Get bold variant
                        let mut bold_xfa_font = xfa_font.clone();
                        bold_xfa_font.weight = crate::xfa::FontWeight::Bold;
                        bold_xfa_font.posture = crate::xfa::FontPosture::Normal;
                        let bold = mgr.get_font(&bold_xfa_font).ok();

                        // Get italic variant
                        let mut italic_xfa_font = xfa_font.clone();
                        italic_xfa_font.weight = crate::xfa::FontWeight::Normal;
                        italic_xfa_font.posture = crate::xfa::FontPosture::Italic;
                        let italic = mgr.get_font(&italic_xfa_font).ok();

                        // Get bold italic variant
                        let mut bold_italic_xfa_font = xfa_font.clone();
                        bold_italic_xfa_font.weight = crate::xfa::FontWeight::Bold;
                        bold_italic_xfa_font.posture = crate::xfa::FontPosture::Italic;
                        let bold_italic = mgr.get_font(&bold_italic_xfa_font).ok();

                        (base, normal, bold, italic, bold_italic)
                    };
                    let text_scale = xfa_px_scale(&render_font, scaled_font_size);

                    // Get text color from style or use dark gray
                    let text_color = node
                        .style
                        .font
                        .as_ref()
                        .and_then(|f| f.color)
                        .map(|(r, g, b)| Rgba([r, g, b, 255u8]))
                        .unwrap_or(dark_gray);

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
                        );
                    } else if !content.is_empty() {
                        // Check if content is a checkbox/ballot box symbol
                        // These characters are commonly missing from fallback fonts
                        let checkbox_char = content.trim().chars().next();
                        if content.trim().len() <= 4
                            && matches!(
                                checkbox_char,
                                Some('☐' | '☑' | '☒' | '□' | '■' | '▢' | '▪')
                            )
                        {
                            // Draw checkbox symbol as a rectangle
                            let box_size = (scaled_font_size * 0.75) as i32;
                            let box_x = content_x + 1;
                            let box_y =
                                content_y + ((scaled_font_size - box_size as f32) * 0.5) as i32;
                            let line_color = Rgba([100u8, 100u8, 100u8, 255u8]);

                            // Draw outline rectangle
                            Self::draw_rect_outline(
                                &mut img, box_x, box_y, box_size, box_size, line_color,
                            );

                            // Draw check mark if checked
                            if matches!(checkbox_char, Some('☑' | '☒' | '■' | '▪')) {
                                let inset = (box_size as f32 * 0.2) as i32;
                                Flattened::fill_rect(
                                    &mut img,
                                    box_x + inset,
                                    box_y + inset,
                                    box_size - 2 * inset,
                                    box_size - 2 * inset,
                                    line_color,
                                );
                            }
                        } else {
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
                                    draw_text_mut(
                                        &mut img,
                                        text_color,
                                        line_x,
                                        line_y,
                                        text_scale,
                                        &render_font,
                                        line,
                                    );
                                }
                            }
                        }
                    }
                }

                FlattenedNodeKind::Image(embedded) => {
                    Self::draw_embedded_image(&mut img, embedded, x, y, w, h);
                }
            }
        }

        Ok(img)
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

        let (draw_w, draw_h) = match embedded.aspect {
            // Stretch to the box, aspect ratio be damned.
            ImageAspect::None => (box_w, box_h),
            // Keep the aspect ratio and fit inside the box.
            ImageAspect::Fit | ImageAspect::Actual => {
                let scale = (box_w as f32 / src_w as f32).min(box_h as f32 / src_h as f32);
                (
                    ((src_w as f32 * scale).round() as u32).max(1),
                    ((src_h as f32 * scale).round() as u32).max(1),
                )
            }
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
    fn draw_barcode(
        img: &mut RgbaImage,
        symbology: &str,
        value: &str,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        color: Rgba<u8>,
    ) {
        let bars = match crate::barcode::encode(symbology, value) {
            Ok(bars) => bars,
            Err(e) => {
                log::warn!("barcode not drawn: {e}");
                Self::draw_placeholder(img, x, y, w, h);
                return;
            }
        };

        let vertical = h > w;
        let along = if vertical { h } else { w };
        let across = if vertical { w } else { h };
        if along <= 0 || across <= 0 {
            return;
        }

        // A quiet zone of ten narrow modules at each end is what the
        // symbology requires to be scannable.
        const QUIET_MODULES: i32 = 10;
        let narrow_units: i32 = bars.iter().map(|b| if b.wide { 3 } else { 1 }).sum();
        let total_units = narrow_units + 2 * QUIET_MODULES;
        let module = (along as f32 / total_units as f32).max(0.0);
        if module <= 0.0 {
            return;
        }

        let mut offset = QUIET_MODULES as f32 * module;
        for bar in &bars {
            let len = if bar.wide { 3.0 } else { 1.0 } * module;
            if bar.black {
                let start = offset.round() as i32;
                let thickness = (len.round() as i32).max(1);
                if vertical {
                    Self::fill_rect(img, x, y + start, across, thickness, color);
                } else {
                    Self::fill_rect(img, x + start, y, thickness, across, color);
                }
            }
            offset += len;
        }
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

    /// Resolve the box a border (or its fill) actually paints in: its own
    /// leaf bounds, or -- when a parent subform propagated its border onto
    /// this leaf (`render_bounds` set, e.g. by
    /// [`Self::propagate_subform_border_to_children`]) -- the parent
    /// subform's bounds, scaled to pixels. Shared so a propagated fill lines
    /// up with its propagated edges.
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

                // Use render bounds for all edges when available
                let (edge_x, edge_y, edge_w, edge_h) = (render_x, render_y, render_w, render_h);

                // Draw based on stroke style
                match edge.stroke {
                    StrokeStyle::Solid => {
                        Self::draw_edge_solid(
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
                        );
                    }
                    StrokeStyle::Dashed => {
                        Self::draw_edge_dashed(
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
                            6,
                        );
                    }
                    StrokeStyle::Dotted => {
                        Self::draw_edge_dashed(
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
                            2,
                        );
                    }
                    StrokeStyle::Lowered
                    | StrokeStyle::Raised
                    | StrokeStyle::Etched
                    | StrokeStyle::Embossed => {
                        // 3D effects - draw with two colors for highlight/shadow
                        let (light, dark) =
                            if matches!(edge.stroke, StrokeStyle::Raised | StrokeStyle::Embossed) {
                                (
                                    Rgba([255u8, 255u8, 255u8, 255u8]),
                                    Rgba([128u8, 128u8, 128u8, 255u8]),
                                )
                            } else {
                                (
                                    Rgba([128u8, 128u8, 128u8, 255u8]),
                                    Rgba([255u8, 255u8, 255u8, 255u8]),
                                )
                            };
                        // Top and left get one color, bottom and right get the other
                        let edge_color = if edge_idx == 0 || edge_idx == 3 {
                            light
                        } else {
                            dark
                        };
                        Self::draw_edge_solid(
                            img,
                            edge_x,
                            edge_y,
                            edge_w,
                            edge_h,
                            edge_idx,
                            thickness_px,
                            edge_color,
                            img_width,
                            img_height,
                        );
                    }
                    _ => {
                        Self::draw_edge_solid(
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
                        );
                    }
                }
            }
        }
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

    /// Draw a dashed edge
    fn draw_edge_dashed(
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
        dash_len: i32,
    ) {
        Self::draw_edge_impl(
            img,
            x,
            y,
            w,
            h,
            edge_idx,
            thickness,
            color,
            img_width,
            img_height,
            Some(dash_len),
        );
    }

    /// Internal helper for drawing edges (solid or dashed).
    /// If `dash_len` is Some, draws a dashed pattern; otherwise draws solid.
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
        dash_len: Option<i32>,
    ) {
        // Helper to check if we should draw at this position (for dashing)
        let should_draw = |pos: i32| -> bool {
            match dash_len {
                Some(len) => (pos / len) % 2 == 0,
                None => true,
            }
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
        let x = (node_x * scale).to_f32().unwrap_or(0.0);
        let y = (node_y * scale).to_f32().unwrap_or(0.0);
        let w = (node_width * scale).to_f32().unwrap_or(0.0);
        let h = (node_height * scale).to_f32().unwrap_or(0.0);

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
    fn calculate_text_x(
        box_x: i32,
        box_w: i32,
        text: &str,
        font_size: f32,
        para: &Option<Para>,
        font: &FontRef<'_>,
    ) -> i32 {
        let h_align = para.as_ref().map(|p| p.h_align).unwrap_or(HAlign::Left);

        // Measure actual text width using font metrics
        let scale = xfa_px_scale(font, font_size);
        let scaled_font = font.as_scaled(scale);
        let mut text_width: f32 = 0.0;
        for ch in text.chars() {
            let glyph_id = font.glyph_id(ch);
            if glyph_id.0 != 0 {
                text_width += scaled_font.h_advance(glyph_id);
            } else {
                // Fallback for missing glyphs
                text_width += font_size * 0.6;
            }
        }
        let text_width = text_width as i32;

        let margin_left = para
            .as_ref()
            .and_then(|p| p.margin_left)
            .map(|m| m.to_f32().unwrap_or(0.0) as i32)
            .unwrap_or(0);
        let margin_right = para
            .as_ref()
            .and_then(|p| p.margin_right)
            .map(|m| m.to_f32().unwrap_or(0.0) as i32)
            .unwrap_or(0);

        match h_align {
            HAlign::Left | HAlign::Justify | HAlign::JustifyAll => box_x + margin_left + 2,
            HAlign::Center => box_x + (box_w - text_width) / 2,
            HAlign::Right => box_x + box_w - text_width - margin_right - 2,
            HAlign::Radix => box_x + box_w / 2, // Simplified: center for radix
        }
    }

    /// Calculate text Y position based on vertical alignment using AXTE rules
    /// Per AXTE spec:
    /// - Baseline position: B = MT + TH - D
    /// - Text is drawn from baseline, so we need to position at baseline - ascent
    /// - Line gap is 20% of font size
    ///
    /// The `render_scale` parameter converts points to pixels (e.g., 2.0 for 2x resolution).
    /// This is needed because `para` values (lineHeight, spaceAbove) are in points,
    /// while `font_size` is already in scaled pixels.
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
        // Scale paragraph values from points to pixels
        let space_above = para
            .as_ref()
            .and_then(|p| p.space_above)
            .map(|s| s.to_f32().unwrap_or(0.0) * render_scale)
            .unwrap_or(0.0);
        let line_height_override = para
            .as_ref()
            .and_then(|p| p.line_height)
            .map(|lh| lh.to_f32().unwrap_or(0.0) * render_scale);

        // Get font metrics (for glyph scaling, not render scaling)
        let scale = xfa_px_scale(font, font_size);
        let scaled_font = font.as_scaled(scale);
        let ascent = scaled_font.ascent();
        let descent = scaled_font.descent().abs();

        // Per AXTE: ensure ascent + descent >= font_size
        let mut effective_ascent = ascent;
        if ascent + descent < font_size {
            effective_ascent = font_size - descent;
        }

        // Per AXTE: line gap is 20% of font size
        let line_gap = font_size * 0.2;

        // Text height: TH = A + D
        let text_height = effective_ascent + descent;

        // Derived line spacing: DS = TH + LG (unless overridden)
        let line_spacing = line_height_override.unwrap_or(text_height + line_gap);

        // Full height calculation for all lines
        let total_text_height = if total_lines == 1 {
            // Single line: no line gap at end
            text_height
        } else {
            // Multiple lines: full line spacing for all but last line
            let _is_last_line = line_index == total_lines - 1;
            let full_height_per_line = line_spacing;
            let last_line_height = text_height; // No line gap on last line

            (total_lines - 1) as f32 * full_height_per_line + last_line_height
        };

        // Calculate first line offset based on vertical alignment
        // Per AXTE: if total height > block height, treat as top-aligned
        let first_line_offset = if total_text_height > box_h as f32 {
            0.0
        } else {
            match v_align {
                VAlign::Top => 0.0,
                VAlign::Middle => (box_h as f32 - total_text_height) / 2.0,
                VAlign::Bottom => box_h as f32 - total_text_height,
            }
        };

        // Position for this specific line
        // Baseline position from top of text block: B = TH - D (for first line)
        // Y for drawing = block_y + first_line_offset + space_above + baseline - ascent
        // Since draw_text_mut positions at top-left of text, we use the top of the line
        let line_y =
            box_y as f32 + first_line_offset + space_above + (line_index as f32 * line_spacing);

        line_y as i32
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

        let scale = xfa_px_scale(font, font_size);
        let scaled_font = font.as_scaled(scale);

        // Get space width (also affected by letter spacing per XFA spec)
        let space_glyph = font.glyph_id(' ');
        let base_space_width = if space_glyph.0 != 0 {
            scaled_font.h_advance(space_glyph)
        } else {
            font_size * 0.3
        };
        // Per XFA spec: letterSpacing affects spacing between grapheme clusters, including spaces
        let space_width = base_space_width + letter_spacing;

        let mut lines = Vec::new();
        let mut current_line = String::new();
        let mut current_width: f32 = 0.0;

        for word in text.split_whitespace() {
            // Measure word width (with letter spacing between characters)
            let word_width = Self::measure_text_width(word, font_size, font, letter_spacing);

            if current_line.is_empty() {
                // First word on line
                current_line = word.to_string();
                current_width = word_width;
            } else if current_width + space_width + word_width <= max_width {
                // Word fits on current line
                current_line.push(' ');
                current_line.push_str(word);
                current_width += space_width + word_width;
            } else {
                // Word doesn't fit, start new line
                lines.push(current_line);
                current_line = word.to_string();
                current_width = word_width;
            }
        }

        if !current_line.is_empty() {
            lines.push(current_line);
        }

        if lines.is_empty() {
            lines.push(String::new());
        }

        lines
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

    /// Calculate the total text block height using AXTE rules
    /// Per AXTE: FH = MT + DS + MB, with LG removed on last line
    pub fn calculate_text_block_height(
        text: &str,
        font_size: f32,
        max_width: f32,
        para: &Option<Para>,
        font: &FontRef<'_>,
    ) -> f32 {
        let lines = Self::wrap_text_with_font(text, max_width, font_size, font);
        let num_lines = lines.len();

        if num_lines == 0 {
            return 0.0;
        }

        // Get font metrics
        let scale = xfa_px_scale(font, font_size);
        let scaled_font = font.as_scaled(scale);
        let ascent = scaled_font.ascent();
        let descent = scaled_font.descent().abs();

        // Per AXTE: ensure ascent + descent >= font_size
        let mut effective_ascent = ascent;
        if ascent + descent < font_size {
            effective_ascent = font_size - descent;
        }

        // Per AXTE: line gap is 20% of font size
        let line_gap = font_size * 0.2;

        // Text height: TH = A + D
        let text_height = effective_ascent + descent;

        // Line spacing override from para element
        let line_spacing = para
            .as_ref()
            .and_then(|p| p.line_height)
            .map(|lh| lh.to_f32().unwrap_or(0.0))
            .unwrap_or(text_height + line_gap);

        // Paragraph margins
        let margin_top = para
            .as_ref()
            .and_then(|p| p.space_above)
            .map(|s| s.to_f32().unwrap_or(0.0))
            .unwrap_or(0.0);
        let margin_bottom = para
            .as_ref()
            .and_then(|p| p.space_below)
            .map(|s| s.to_f32().unwrap_or(0.0))
            .unwrap_or(0.0);

        // Calculate total height
        // Per AXTE: FH = MT + DS + MB for each line, but LG removed on last line
        if num_lines == 1 {
            // Single line: MT + TH + MB (no line gap)
            margin_top + text_height + margin_bottom
        } else {
            // Multiple lines:
            // - First line: MT + DS
            // - Middle lines: DS each
            // - Last line: TH + MB (no line gap)
            let first_line = margin_top + line_spacing;
            let middle_lines = (num_lines - 2) as f32 * line_spacing;
            let last_line = text_height + margin_bottom;

            first_line + middle_lines + last_line
        }
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
    pub fn parse_rich_text_from_html(
        children: &[XfaNode],
        default_h_align: HAlign,
        computed_values: Option<&std::collections::HashMap<SomPath, String>>,
        id_to_field: Option<&std::collections::HashMap<String, String>>,
        default_bold: bool,
        default_italic: bool,
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
                                underline: false,
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
                                para.text_indent = Self::parse_css_dimension(style, "text-indent");
                                para.font_size = Self::parse_css_dimension(style, "font-size");
                                para.line_height = Self::parse_css_dimension(style, "line-height");
                                // Parse margin-top as space_above
                                if let Some(mt) = Self::parse_css_dimension(style, "margin-top") {
                                    para.space_above = Some(mt);
                                }
                                // Parse CSS margin-left/margin-right for paragraph width reduction
                                // Per XFA spec Chapter 27: margin-left reduces available width for text wrapping
                                para.margin_left = Self::parse_css_dimension(style, "margin-left");
                                para.margin_right =
                                    Self::parse_css_dimension(style, "margin-right");
                                // Parse CSS letter-spacing for paragraph-level override
                                let para_font = para.font_size.unwrap_or(8.0);
                                para.letter_spacing = Self::parse_css_dimension_with_em(
                                    style,
                                    "letter-spacing",
                                    Some(para_font),
                                );
                                // Only override h_align if CSS specifies it
                                let css_align = Self::parse_css_alignment_optional(style);
                                if let Some(align) = css_align {
                                    para.h_align = align;
                                }

                                // Check for font-weight in paragraph style
                                // font-weight:bold sets bold, font-weight:normal resets it
                                let has_bold = style.contains("font-weight:bold")
                                    || style.contains("font-weight: bold");
                                let has_normal = style.contains("font-weight:normal")
                                    || style.contains("font-weight: normal");

                                if has_bold {
                                    Some(true)
                                } else if has_normal {
                                    Some(false)
                                } else {
                                    None
                                }
                            } else {
                                None
                            };

                            // Compute effective bold: CSS overrides inherited, otherwise inherit
                            let effective_bold = para_bold.unwrap_or(bold);

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
                                        underline: false,
                                        superscript: false,
                                        letter_spacing: run_letter_spacing,
                                    });
                                }
                                continue; // Don't recurse into embed spans
                            }

                            // Parse CSS styles from span for font-weight/font-style and xfa-spacerun
                            let (new_preserve, span_bold, span_italic) =
                                if let Some(style) = child.attributes.get("style") {
                                    let preserve = style.contains("xfa-spacerun:yes")
                                        || style.contains("xfa-spacerun: yes");

                                    // Parse font-weight: bold sets, normal resets
                                    let effective_bold = if style.contains("font-weight:bold")
                                        || style.contains("font-weight: bold")
                                    {
                                        true
                                    } else if style.contains("font-weight:normal")
                                        || style.contains("font-weight: normal")
                                    {
                                        false
                                    } else {
                                        bold // inherit
                                    };

                                    // Parse font-style: italic sets, normal resets
                                    let effective_italic = if style.contains("font-style:italic")
                                        || style.contains("font-style: italic")
                                    {
                                        true
                                    } else if style.contains("font-style:normal")
                                        || style.contains("font-style: normal")
                                    {
                                        false
                                    } else {
                                        italic // inherit
                                    };

                                    (preserve, effective_bold, effective_italic)
                                } else {
                                    (preserve_spaces, bold, italic)
                                };

                            // Detect superscript from positive vertical-align + small font-size on span
                            let span_superscript = child
                                .attributes
                                .get("style")
                                .map(|style| {
                                    Self::parse_css_dimension(style, "vertical-align")
                                        .is_some_and(|va| va > 0.0)
                                })
                                .unwrap_or(false);

                            // Track run count before recursion to mark new runs as superscript
                            let runs_before: usize = paragraphs.iter().map(|p| p.runs.len()).sum();

                            // Parse CSS letter-spacing from span style
                            let span_letter_spacing =
                                if let Some(style) = child.attributes.get("style") {
                                    let span_font =
                                        paragraphs.last().and_then(|p| p.font_size).unwrap_or(8.0);
                                    Self::parse_css_dimension_with_em(
                                        style,
                                        "letter-spacing",
                                        Some(span_font),
                                    )
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
                    underline: false,
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
    fn parse_css_dimension(style: &str, property: &str) -> Option<f32> {
        Self::parse_css_dimension_with_em(style, property, None)
    }

    /// Parse a CSS dimension value, with optional `em` unit support.
    /// When `font_size_pt` is provided, `em` values are resolved relative to it.
    fn parse_css_dimension_with_em(
        style: &str,
        property: &str,
        font_size_pt: Option<f32>,
    ) -> Option<f32> {
        let search = format!("{}:", property);
        if let Some(pos) = style.find(&search) {
            let rest = &style[pos + search.len()..];
            let value_str = rest.split(';').next()?.trim();

            // Parse the dimension with unit
            if let Some(val) = value_str.strip_suffix("pt") {
                val.trim().parse().ok()
            } else if let Some(val) = value_str.strip_suffix("em") {
                let em_val = val.trim().parse::<f32>().ok()?;
                Some(em_val * font_size_pt.unwrap_or(10.0))
            } else if let Some(val) = value_str.strip_suffix("in") {
                val.trim().parse::<f32>().ok().map(|v| v * 72.0)
            } else if let Some(val) = value_str.strip_suffix("mm") {
                val.trim().parse::<f32>().ok().map(|v| v * 2.834_645_7)
            } else if let Some(val) = value_str.strip_suffix("px") {
                // Approximate px to pt (1px ≈ 0.75pt at 96dpi)
                val.trim().parse::<f32>().ok().map(|v| v * 0.75)
            } else {
                // Try parsing as bare number (assume pt)
                value_str.parse().ok()
            }
        } else {
            None
        }
    }

    /// Parse font-family from CSS style string.
    /// Handles quoted and unquoted font names like:
    /// - font-family:'Frutiger 45 Light'
    /// - font-family: "Arial"
    /// - font-family: Helvetica
    fn parse_css_font_family(style: &str) -> Option<String> {
        let search = "font-family:";
        if let Some(pos) = style.find(search) {
            let rest = &style[pos + search.len()..];
            let value_str = rest.split(';').next()?.trim();

            // Handle quoted font names
            if value_str.starts_with('\'') || value_str.starts_with('"') {
                let quote = value_str.chars().next()?;
                let end_quote = value_str[1..].find(quote)?;
                Some(value_str[1..end_quote + 1].to_string())
            } else {
                // Unquoted font name - take until comma or end
                Some(value_str.split(',').next()?.trim().to_string())
            }
        } else {
            None
        }
    }

    /// Parse font-weight from CSS style string.
    /// Returns true if bold, false if normal/not specified.
    fn parse_css_font_weight(style: &str) -> Option<bool> {
        let search = "font-weight:";
        if let Some(pos) = style.find(search) {
            let rest = &style[pos + search.len()..];
            let value_str = rest.split(';').next()?.trim().to_lowercase();

            // Check for bold values
            if value_str == "bold" || value_str == "700" || value_str == "800" || value_str == "900"
            {
                Some(true)
            } else if value_str == "normal"
                || value_str == "400"
                || value_str == "300"
                || value_str == "200"
                || value_str == "100"
            {
                Some(false)
            } else {
                // Unknown value, return None to not override
                None
            }
        } else {
            None
        }
    }

    /// Extract font information (font-family, font-size, font-weight-is-bold) from HTML exData content.
    /// Returns (font_family, font_size_pt, is_bold) if found in the first <p> element's style.
    /// Per XFA spec, the HTML content inside exData can have CSS styles that should
    /// override the <font> element's properties.
    fn extract_font_from_html_content(
        children: &[XfaNode],
    ) -> (Option<String>, Option<Num>, Option<bool>) {
        // Navigate to value -> exData -> body -> p and extract style
        for child in children {
            if matches!(child.kind, XfaNodeKind::Value) {
                for value_child in &child.children {
                    if let XfaNodeKind::Element { tag_name, .. } = &value_child.kind
                        && tag_name == "exData"
                    {
                        // Find body element
                        for ex_child in &value_child.children {
                            if let XfaNodeKind::Element {
                                tag_name: body_tag, ..
                            } = &ex_child.kind
                                && body_tag == "body"
                            {
                                // Find first <p> element with style
                                for body_child in &ex_child.children {
                                    if let XfaNodeKind::Element {
                                        tag_name: p_tag, ..
                                    } = &body_child.kind
                                        && p_tag == "p"
                                        && let Some(style) = body_child.attributes.get("style")
                                    {
                                        let font_family = Self::parse_css_font_family(style);
                                        let font_size =
                                            Self::parse_css_dimension(style, "font-size")
                                                .map(|s| num(s as f64));
                                        let is_bold = Self::parse_css_font_weight(style);
                                        return (font_family, font_size, is_bold);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // Also check Element with tag_name "value"
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                && tag_name == "value"
            {
                for value_child in &child.children {
                    if let XfaNodeKind::Element {
                        tag_name: inner_tag,
                        ..
                    } = &value_child.kind
                        && inner_tag == "exData"
                    {
                        for ex_child in &value_child.children {
                            if let XfaNodeKind::Element {
                                tag_name: body_tag, ..
                            } = &ex_child.kind
                                && body_tag == "body"
                            {
                                for body_child in &ex_child.children {
                                    if let XfaNodeKind::Element {
                                        tag_name: p_tag, ..
                                    } = &body_child.kind
                                        && p_tag == "p"
                                        && let Some(style) = body_child.attributes.get("style")
                                    {
                                        let font_family = Self::parse_css_font_family(style);
                                        let font_size =
                                            Self::parse_css_dimension(style, "font-size")
                                                .map(|s| num(s as f64));
                                        let is_bold = Self::parse_css_font_weight(style);
                                        return (font_family, font_size, is_bold);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        (None, None, None)
    }

    /// Parse CSS text-align property from style string
    fn parse_css_alignment(style: &str) -> HAlign {
        if style.contains("text-align:justify") || style.contains("text-align: justify") {
            HAlign::Justify
        } else if style.contains("text-align:center") || style.contains("text-align: center") {
            HAlign::Center
        } else if style.contains("text-align:right") || style.contains("text-align: right") {
            HAlign::Right
        } else {
            HAlign::Left
        }
    }

    /// Parse CSS text-align property from style string, returning None if not specified
    fn parse_css_alignment_optional(style: &str) -> Option<HAlign> {
        if style.contains("text-align:justify") || style.contains("text-align: justify") {
            Some(HAlign::Justify)
        } else if style.contains("text-align:center") || style.contains("text-align: center") {
            Some(HAlign::Center)
        } else if style.contains("text-align:right") || style.contains("text-align: right") {
            Some(HAlign::Right)
        } else if style.contains("text-align:left") || style.contains("text-align: left") {
            Some(HAlign::Left)
        } else {
            None // CSS doesn't specify - use default
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
        let px_scale = xfa_px_scale(font, font_size);
        let scaled_font = font.as_scaled(px_scale);

        // Get base space width from font metrics.
        // Paragraph/run letter-spacing is applied below per paragraph.
        let space_glyph = font.glyph_id(' ');
        let base_space_width = if space_glyph.0 != 0 {
            scaled_font.h_advance(space_glyph)
        } else {
            font_size * 0.3
        };

        for para in &rich_text.paragraphs {
            // Paragraph CSS letter-spacing overrides inherited/font-level spacing,
            // including explicit 0in and negative values (e.g. -0.002em).
            let para_letter_spacing = para
                .letter_spacing
                .map(|ls| ls * scale)
                .unwrap_or(letter_spacing);
            // Interword spacing uses base space advance plus paragraph/run
            // letter spacing (consistent with existing AXTE-compatible behavior
            // in this pipeline).
            let space_width = base_space_width + para_letter_spacing;

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

            // Word-wrap the tokens using effective width (reduced by margins, using resolved value).
            //
            // Per XFA 3.3 spec the line breaking is per [UAX-14] (see §Layout
            // "Break Individual Lines"), and our `space_width` already follows
            // the spec rule that inter-cluster letter-spacing applies on both
            // sides of an interword space. However, when paragraph-level
            // letter-spacing is set explicitly on rich-text `<p>` (Adobe
            // template inspector / AEM dor templates frequently use values
            // like `letter-spacing:-0.002em`), the cumulative inter-cluster
            // adjustment within the line can still differ from Adobe AXTE by
            // less than a typographic point because of font-metric rounding
            // (h_advance vs Adobe's measured cluster advance). To remain
            // visually aligned with the Adobe-produced layout in those cases,
            // we allow a tiny overshoot tolerance scaled to the font size, and
            // ONLY when the paragraph requested explicit negative letter-
            // spacing. This keeps standard (letter-spacing = 0) paragraphs at
            // a strict UAX-14 break boundary.
            let wrap_tolerance = if para_letter_spacing < 0.0 {
                (font_size * 0.35).max(1.5)
            } else {
                0.0
            };
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
                wrap_tolerance,
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
        wrap_tolerance: f32,
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

            if current_width + token_space + token.width <= effective_max + wrap_tolerance
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
                        },
                        LayoutToken {
                            text: second_part.to_string(),
                            width: second_width,
                            preserve_spaces: token.preserve_spaces,
                            bold: token.bold,
                            italic: token.italic,
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
        let px_scale = xfa_px_scale(font, font_size);
        let scaled_font = font.as_scaled(px_scale);

        let mut width: f32 = 0.0;
        let char_count = text.chars().count();
        for (i, ch) in text.chars().enumerate() {
            let glyph_id = font.glyph_id(ch);
            if glyph_id.0 != 0 {
                width += scaled_font.h_advance(glyph_id);
            } else {
                // Fallback for missing glyphs
                width += font_size * 0.6;
            }
            // Add letter spacing between characters (not after the last one)
            if i < char_count - 1 {
                width += letter_spacing;
            }
        }
        width
    }

    // ========================================================================
    // Multi-Paragraph Draw Node Splitting
    // ========================================================================

    /// Split a Draw FlattenedNode that contains multi-paragraph RichText into
    /// separate FlattenedKind entries — one per paragraph.
    ///
    /// Per XFA spec (section on "Line Height" and "Paragraph Spacing"):
    /// - Each paragraph's height is computed from text wrapping within the draw width,
    ///   using AXTE line metrics (ascent, descent, line gap = 20% of font size).
    ///
    /// Determine the font weight override from a rich text paragraph's runs.
    /// Returns `Some(Bold)` if all content runs are bold, `Some(Normal)` if all
    /// are normal, or the dominant weight (by character count) for mixed paragraphs.
    /// Returns `None` only for empty paragraphs.
    fn rich_paragraph_weight_override(para: &RichParagraph) -> Option<FontWeight> {
        if para.runs.is_empty() {
            return None;
        }
        let has_bold = para
            .runs
            .iter()
            .any(|r| !r.text.trim().is_empty() && r.bold);
        let has_normal = para
            .runs
            .iter()
            .any(|r| !r.text.trim().is_empty() && !r.bold);
        if has_bold && !has_normal {
            Some(FontWeight::Bold)
        } else if has_normal && !has_bold {
            Some(FontWeight::Normal)
        } else if has_bold && has_normal {
            // Mixed: use the dominant weight by character count.
            // A paragraph that starts with a short bold phrase but continues
            // with normal text should not be marked as bold overall.
            let bold_chars: usize = para
                .runs
                .iter()
                .filter(|r| r.bold && !r.text.trim().is_empty())
                .map(|r| r.text.len())
                .sum();
            let normal_chars: usize = para
                .runs
                .iter()
                .filter(|r| !r.bold && !r.text.trim().is_empty())
                .map(|r| r.text.len())
                .sum();

            if bold_chars >= normal_chars {
                Some(FontWeight::Bold)
            } else {
                Some(FontWeight::Normal)
            }
        } else {
            None
        }
    }

    /// - spaceAbove/spaceBelow on RichParagraph add inter-paragraph spacing.
    /// - Empty paragraphs (is_empty=true) are preserved as spacing nodes.
    ///
    /// For single-paragraph or no-RichText nodes, returns the original node unchanged.
    /// Per-paragraph text-block heights for a rich-text draw, using the same
    /// per-paragraph font/line-height resolution and the same `layout_rich_text`
    /// tokenizer that draws the paragraphs. Shared by `split_draw_into_paragraph_nodes`
    /// (which places each paragraph at these heights) and `calculate_rich_text_draw_height`
    /// (which sizes the draw's natural height before it is split), so natural-height
    /// measurement can never drift from what actually gets drawn.
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
            hyphenation: default_para.as_ref().and_then(|p| p.hyphenation.clone()),
        }
    }

    fn rich_paragraph_heights(
        rich_text: &RichText,
        xfa_font: &Font,
        default_para: &Option<Para>,
        max_width: Num,
        language: &str,
    ) -> Vec<Num> {
        let mut measurer = TextMeasurer::new();
        let hyph_dict = super::xfa::hyphenation::dict_for_language(language);
        let base_font_size = xfa_font.size;

        let mut paragraph_heights: Vec<Num> = Vec::with_capacity(rich_text.paragraphs.len());

        for para in rich_text.paragraphs.iter() {
            // Use per-paragraph font size override if available
            let para_font_size = para
                .font_size
                .map(|s| num(s as f64))
                .unwrap_or(base_font_size);
            let mut para_xfa_font = xfa_font.clone();
            para_xfa_font.size = para_font_size;

            // Override font weight/posture based on HTML CSS properties in the
            // paragraph's runs. The XFA <font> element may specify weight="bold",
            // but the rich text HTML CSS can override this
            // (e.g. <p style="font-weight:normal">). Since layout_rich_text uses
            // a single font for all tokens, we pick the dominant style.
            //
            // When a paragraph has BOTH bold and normal runs, we use the weight
            // that covers the majority of the text (by character count). This
            // prevents a short bold prefix from forcing the bold font for width
            // measurement, which would inflate line widths and cause incorrect
            // wrapping (e.g. AACX STP_Definitions_1 «Servizi» paragraph).
            if !para.runs.is_empty() {
                let has_bold = para
                    .runs
                    .iter()
                    .any(|r| !r.text.trim().is_empty() && r.bold);
                let has_normal = para
                    .runs
                    .iter()
                    .any(|r| !r.text.trim().is_empty() && !r.bold);
                if has_bold && !has_normal {
                    // All content runs are bold: CSS overrides XFA normal to bold
                    para_xfa_font.weight = FontWeight::Bold;
                } else if has_normal && !has_bold {
                    // All content runs are non-bold: CSS overrides XFA bold to normal
                    para_xfa_font.weight = FontWeight::Normal;
                } else if has_normal && has_bold {
                    // Mixed: use the dominant weight by character count
                    let bold_chars: usize = para
                        .runs
                        .iter()
                        .filter(|r| r.bold && !r.text.trim().is_empty())
                        .map(|r| r.text.len())
                        .sum();
                    let normal_chars: usize = para
                        .runs
                        .iter()
                        .filter(|r| !r.bold && !r.text.trim().is_empty())
                        .map(|r| r.text.len())
                        .sum();
                    if normal_chars > bold_chars {
                        para_xfa_font.weight = FontWeight::Normal;
                    }
                }
                let has_italic = para
                    .runs
                    .iter()
                    .any(|r| !r.text.trim().is_empty() && r.italic);
                let has_upright = para
                    .runs
                    .iter()
                    .any(|r| !r.text.trim().is_empty() && !r.italic);
                if has_upright && !has_italic {
                    para_xfa_font.posture = FontPosture::Normal;
                }
            }

            // Resolve the effective line height for this paragraph.
            // Per XFA spec §17: lineHeight is the vertical distance between successive
            // baselines. When splitting multi-paragraph draws, each line should occupy
            // exactly lineHeight of vertical space for consistent alignment between
            // overlapping draw elements (e.g. T_Left / T_LeftIndent in AAAI).
            let effective_line_height = para
                .line_height
                .map(|lh| num(lh as f64))
                .or_else(|| default_para.as_ref().and_then(|p| p.line_height))
                .unwrap_or_else(|| {
                    if let Ok(metrics) = measurer.get_metrics_for_style(&para_xfa_font) {
                        metrics.derived_line_spacing()
                    } else {
                        para_font_size * num(1.2)
                    }
                });

            if para.is_empty {
                // Per XFA spec §2.5+: a leading `<p><br/></p>` at the very start
                // of a rich text body (first paragraph, no text runs — only a <br/>
                // marker) produces zero height.  The <br/> terminates an empty first
                // line that has no content, so it does not advance the vertical
                // position.  This is critical for alignment between overlapping draw
                // elements (e.g. T_Indent / T_The_Authorized in BAGE) where the
                // first br-only paragraph is a structural placeholder, not a visible
                // blank line.
                if paragraph_heights.is_empty() && para.runs.is_empty() && para.has_br {
                    paragraph_heights.push(Decimal::ZERO);
                    continue;
                }
                // Empty paragraph: height is one line
                let space_above = para
                    .space_above
                    .map(|s| num(s as f64))
                    .unwrap_or(Decimal::ZERO);
                let space_below = para
                    .space_below
                    .map(|s| num(s as f64))
                    .unwrap_or(Decimal::ZERO);
                paragraph_heights.push(effective_line_height + space_above + space_below);
                continue;
            }

            // Collect plain text for this paragraph
            let plain_text: String = para
                .runs
                .iter()
                .map(|r| r.text.as_str())
                .collect::<Vec<_>>()
                .join("");

            if plain_text.trim().is_empty() {
                // Paragraph with only whitespace runs — treat like empty,
                // using the same line_height as empty paragraphs for consistency.
                paragraph_heights.push(effective_line_height);
                continue;
            }

            // Build a Para for this specific paragraph (merge RichParagraph props with defaults)
            let para_props = Some(Self::para_for_rich_paragraph(para, default_para));

            // Use layout_rich_text for line counting instead of measure_text_block.
            // This ensures the same wrapping logic (tokenization, text width
            // measurement) is used for both height calculation and rendering,
            // preventing misalignment between overlapping draw elements.
            let space_above = para_props
                .as_ref()
                .and_then(|p| p.space_above)
                .unwrap_or(Decimal::ZERO);
            let space_below = para_props
                .as_ref()
                .and_then(|p| p.space_below)
                .unwrap_or(Decimal::ZERO);

            // Per XFA spec Chapter 27: margin-left/margin-right on paragraphs reduce
            // the available width for text wrapping. This is critical for proper
            // alignment between overlapping draw elements (e.g. T_Left / T_LeftIndent
            // in AAIS forms where T_Left paragraphs have margin-left:25.512pt).
            let para_margin_left = para.margin_left.unwrap_or(0.0);
            let para_margin_right = para.margin_right.unwrap_or(0.0);
            let effective_width_f32 =
                (max_width.to_f32().unwrap_or(500.0) - para_margin_left - para_margin_right)
                    .max(0.0);
            let effective_width = num(effective_width_f32 as f64);

            // Get the font for layout_rich_text
            let font_for_layout = measurer.get_font_for_style(&para_xfa_font).ok().cloned();

            if let Some(layout_font) = &font_for_layout {
                // Per XFA spec: text-indent shifts the first line's starting x position
                // but does NOT reduce the available line width for wrapping purposes.
                // Using text-indent in line-count measurement would cause extra line
                // breaks compared to Adobe's XFA renderer, misaligning overlapping
                // draw elements (e.g. T_Left / T_LeftIndent in AAAI).
                let mut height_para = para.clone();
                height_para.text_indent = None;
                // Clear margins since effective_width_f32 already accounts for them.
                // This prevents double-reduction in layout_rich_text.
                height_para.margin_left = None;
                height_para.margin_right = None;

                let single_rt = RichText {
                    paragraphs: vec![height_para.clone()],
                };
                // `para_xfa_font`'s weight/posture were already resolved to
                // this paragraph's dominant style above, deliberately as one
                // face for the whole paragraph rather than per run (see that
                // comment); `only` carries that single choice through.
                let run_fonts = RunFonts::only(layout_font);
                let rendered_lines = Self::layout_rich_text(
                    &single_rt,
                    effective_width_f32,
                    para_font_size.to_f32().unwrap_or(10.0),
                    &run_fonts,
                    1.0, // scale=1.0 for measurement in pt units
                    if let Some(ls) = para.letter_spacing {
                        ls
                    } else {
                        para_xfa_font
                            .letter_spacing
                            .and_then(|ls| ls.to_f32())
                            .unwrap_or(0.0)
                    },
                    default_para.as_ref().and_then(|p| p.hyphenation.as_ref()),
                    hyph_dict,
                );
                let num_lines = num(rendered_lines.len().max(1) as f64);
                let height = num_lines * effective_line_height + space_above + space_below;
                paragraph_heights.push(height);
            } else {
                // Fallback to measure_text_block if font is unavailable.
                // Also clear text_indent here to stay consistent with the primary path.
                let para_props_no_indent = para_props.as_ref().map(|p| Para {
                    text_indent: None,
                    ..p.clone()
                });
                match measurer.measure_text_block(
                    &plain_text,
                    &Some(para_xfa_font.clone()),
                    &para_props_no_indent,
                    effective_width,
                ) {
                    Ok(block_metrics) => {
                        let num_lines = num(block_metrics.lines.len() as f64);
                        let height = num_lines * effective_line_height + space_above + space_below;
                        paragraph_heights.push(height);
                    }
                    Err(_) => {
                        let estimated_chars_per_line =
                            effective_width / (para_font_size * num(0.5));
                        let estimated_lines = if estimated_chars_per_line > Decimal::ZERO {
                            let text_len = num(plain_text.len() as f64);
                            (text_len / estimated_chars_per_line).ceil()
                        } else {
                            Decimal::ONE
                        };
                        paragraph_heights.push(
                            estimated_lines * effective_line_height + space_above + space_below,
                        );
                    }
                }
            }
        }

        paragraph_heights
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
                let weight_override = single_para
                    .as_ref()
                    .and_then(Self::rich_paragraph_weight_override);
                if let Some(weight) = weight_override {
                    if let Some(ref mut font) = node.style.font {
                        font.weight = weight;
                    }
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

            // Build per-paragraph style with font size override if needed
            let mut para_style = node.style.clone();
            if para.font_size.is_some() {
                if let Some(ref mut font) = para_style.font {
                    font.size = para_font_size;
                }
            }

            // Override font weight based on CSS in rich text runs.
            // The XFA <font> element may specify weight="bold", but the HTML CSS
            // in individual paragraphs can override this (e.g. font-weight:normal).
            // Conversely, the XFA font may say Normal but all CSS runs are bold.
            // We override in both directions when all content runs agree.
            if let Some(weight) = Self::rich_paragraph_weight_override(para) {
                if let Some(ref mut font) = para_style.font {
                    font.weight = weight;
                }
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
    ) {
        let px_scale = xfa_px_scale(base_font, font_size);
        let scaled_font = base_font.as_scaled(px_scale);

        // Get space width and font metrics (space width affected by letter spacing)
        let space_glyph = base_font.glyph_id(' ');
        let base_space_width = if space_glyph.0 != 0 {
            scaled_font.h_advance(space_glyph)
        } else {
            font_size * 0.3
        };
        let space_width = base_space_width + letter_spacing;

        // Get vertical alignment and spacing from para
        let v_align = para.as_ref().map(|p| p.v_align).unwrap_or(VAlign::Top);
        let space_above = para
            .as_ref()
            .and_then(|p| p.space_above)
            .map(|s| s.to_f32().unwrap_or(0.0) * scale)
            .unwrap_or(0.0);
        let line_height_override = para
            .as_ref()
            .and_then(|p| p.line_height)
            .map(|lh| lh.to_f32().unwrap_or(0.0) * scale);

        // Font metrics
        let ascent = scaled_font.ascent();
        let descent = scaled_font.descent().abs();
        let line_gap = font_size * 0.2;
        let text_height = ascent + descent;
        let line_spacing = line_height_override.unwrap_or(text_height + line_gap);

        // Calculate total text block height
        let total_lines = lines.len();
        let total_height = if total_lines == 0 {
            0.0
        } else if total_lines == 1 {
            text_height
        } else {
            (total_lines - 1) as f32 * line_spacing + text_height
        };

        // Calculate starting Y based on vertical alignment
        let start_y = match v_align {
            VAlign::Top => box_y as f32 + space_above,
            VAlign::Middle => box_y as f32 + (box_h as f32 - total_height) / 2.0,
            VAlign::Bottom => box_y as f32 + box_h as f32 - total_height,
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
            let line_y = start_y + (line_idx as f32 * line_spacing);

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
        let px_scale = xfa_px_scale(font, font_size);

        // If no letter spacing, use the fast path
        if letter_spacing.abs() < 0.001 {
            draw_text_mut(img, color, x, y, px_scale, font, text);
            return;
        }

        // Render each character with letter spacing
        let scaled_font = font.as_scaled(px_scale);
        let mut current_x = x as f32;
        let char_count = text.chars().count();

        for (i, ch) in text.chars().enumerate() {
            let ch_str = ch.to_string();
            draw_text_mut(img, color, current_x as i32, y, px_scale, font, &ch_str);

            // Advance position by glyph width + letter spacing
            let glyph_id = font.glyph_id(ch);
            let advance = if glyph_id.0 != 0 {
                scaled_font.h_advance(glyph_id)
            } else {
                font_size * 0.6
            };
            current_x += advance;

            // Add letter spacing between characters (not after the last one)
            if i < char_count - 1 {
                current_x += letter_spacing;
            }
        }
    }

    // ========================================================================
    // Helper: Extract rich text from exData for draw elements
    // ========================================================================

    /// Extract rich text from exData HTML content.
    /// Returns RichText structure if HTML content is found, None otherwise.
    pub fn extract_rich_text_from_exdata(
        children: &[XfaNode],
        default_h_align: HAlign,
    ) -> Option<RichText> {
        for child in children {
            if let XfaNodeKind::Element { tag_name, .. } = &child.kind
                && tag_name == "body"
            {
                return Some(Self::parse_rich_text_from_html(
                    std::slice::from_ref(child),
                    default_h_align,
                    None,
                    None,
                    false,
                    false,
                ));
            }
            // Recurse into children
            if let Some(rich_text) =
                Self::extract_rich_text_from_exdata(&child.children, default_h_align)
            {
                return Some(rich_text);
            }
        }
        None
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

    /// Get rich text from a node's value if present
    pub fn get_rich_text_from_value(
        children: &[XfaNode],
        default_h_align: HAlign,
    ) -> Option<RichText> {
        for child in children {
            if matches!(child.kind, XfaNodeKind::Value) {
                for value_child in &child.children {
                    if let XfaNodeKind::Element { tag_name, .. } = &value_child.kind
                        && tag_name == "exData"
                    {
                        return Self::extract_rich_text_from_exdata(
                            &value_child.children,
                            default_h_align,
                        );
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            Flattened::parse_rich_text_from_html(&[body], HAlign::Left, None, None, false, false);

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
            Flattened::parse_rich_text_from_html(&[body], HAlign::Left, None, None, false, false);

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
            Flattened::parse_rich_text_from_html(&[body], HAlign::Left, None, None, false, false);

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
            },
            RichRun {
                text: " ".to_string(), // Space-only run
                bold: false,
                italic: false,
                underline: false,
                superscript: false,
                preserve_spaces: false,
                letter_spacing: None,
            },
            RichRun {
                text: "bold ".to_string(),
                bold: true,
                italic: false,
                underline: false,
                superscript: false,
                preserve_spaces: false,
                letter_spacing: None,
            },
            RichRun {
                text: "suffix".to_string(),
                bold: false,
                italic: false,
                underline: false,
                superscript: false,
                preserve_spaces: false,
                letter_spacing: None,
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
        let mut page = Page::new(num(width), num(height));
        page.page_count = page_count;
        page.page_breaks = (1..page_count)
            .map(|k| num(height) * Decimal::from(k))
            .collect();
        Flattened::new(page, vec![])
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
