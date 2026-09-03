#![allow(clippy::large_enum_variant)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::collapsible_match)]
#![allow(clippy::needless_range_loop)]
#![deny(unsafe_code)]

//! Blueprint - XFA PDF document analysis library.
//!
//! This library provides a high-level API for processing XFA PDF documents.
//! It can extract form structure, iterate over all interactive states (radio buttons,
//! checkboxes), render pages to images, and produce structured or HTML output.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use blueprint::{Blueprint, HtmlConfig};
//!
//! // Load from a PDF file
//! let mut bp = Blueprint::from_pdf("form.pdf")?;
//!
//! // Get all form states (default + every radio/checkbox combination)
//! let states = bp.states()?;
//!
//! // Iterate over states
//! for state in states.iter() {
//!     // Render each state to an in-memory RGBA image
//!     let img = state.render_plain(1.5)?;
//!
//!     // Get structured representation
//!     let envelope = state.structured(bp.context());
//!
//!     // Convert to HTML
//!     let html = blueprint::to_html(&envelope.content, &HtmlConfig::default());
//! }
//!
//! // Or get everything merged into a single structured tree
//! let merged = bp.merged_structured()?;
//! ```
//!
//! # Architecture
//!
//! The processing pipeline is:
//!
//! ```text
//! PDF bytes
//!   │
//!   ▼
//! extract XFA XML bytes   ──► raw XFA XML
//!   │
//!   ▼
//! XfaNode::parse()        ──► Vec<XfaNode>          (xfa/)
//!   │
//!   ▼
//! XfaForm::new()          ──► XfaForm                (xfa/scripting)
//!   │
//!   ▼
//! Exhaustive exploration  ──► Vec<FormState>          (lib.rs)
//!   │
//!   ├──► structured()     ──► DocumentEnvelope        (structured/)
//!   │     ├──► to_html()  ──► HTML string             (html/)
//!   │     ├──► to_dot()   ──► DOT graph               (graphviz/)
//!   │     └──► serde      ──► JSON
//!   │
//!   └──► render_*()       ──► RgbaImage               (flattened/ + document/)
//! ```

pub mod aem;
pub mod context;
pub mod document;
pub mod exhaustive;
pub mod flattened;
pub mod graphviz;
pub mod html;
pub mod pdf_parser;
pub mod pipeline;
pub mod profiles;
pub mod redacto;
pub mod reference_db;
pub mod review;
#[cfg(feature = "semantic-matching")]
pub mod semantic;
pub mod structured;
pub mod target;
pub mod template;
pub mod util;
pub mod xfa;
pub mod xsd;

#[cfg(test)]
mod tests;

// ============================================================================
// Re-exports — flat access to the most commonly used types
// ============================================================================

// Context
pub use context::Context;

// Embedded profile loading
pub use profiles::{
    AemConnection, has_aem_config, has_html_config, has_redacto_config, has_xsd_config,
    list_profiles, load_aem_config, load_aem_connection, load_aem_fragments, load_aem_profile,
    load_html_custom_styles, load_profile_fonts, load_redacto_config, load_xsd_config,
    profile_targets, redacto_master_language,
};

// Flattened layer
pub use flattened::{DropdownInfo, Flattened, FlattenedNode, FlattenedNodeKind};

// Document / analysis layer
pub use document::modules::{
    AnalysisModule, GlobalContext, run_analysis_pipeline, run_analysis_pipeline_with_context,
};
pub use document::{Document, Group, GroupKind, GroupSource};

// Structured output
pub use structured::{
    ConditionalNode,
    DocumentEnvelope,
    // Element merging for editor
    ElementMergeError,
    FieldId,
    FieldNode,
    FieldType,
    GridLayout,
    GroupNode,
    HeadingLevel,
    HeadingNode,
    ImageNode,
    InlineNode,
    InlineText,
    InputValue,
    ListItem,
    ListNode,
    MergeError,
    MergeInput,
    NameValue,
    ParagraphNode,
    RecursiveMerger,
    RepeatableNode,
    Selection,
    SelectionKind,
    StructuredNode,
    TableNode,
    TranslatableString,
    TranslatedText,
    can_merge,
    can_merge_all,
    merge_nodes,
    merge_two,
};

/// JSON Schema for the structured document content (`Vec<StructuredNode>`).
///
/// Generated from the structured model via `schemars`. Intended to be embedded
/// as text in LLM prompts so the model knows the exact shape to emit — it is
/// NOT used as a structured-output constraint (the model is recursive).
pub fn structured_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(Vec<StructuredNode>))
        .expect("StructuredNode schema serializes")
}

/// JSON Schema for the multilingual AEM node tree (`AemNodeTranslated`).
///
/// Like [`structured_schema`] but for the translations-in-place AEM type the agent authors
/// directly. Embedded as text in the agent prompt so the model knows the exact
/// shape to emit (text fields are per-language maps).
pub fn aem_translated_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(AemNodeTranslated))
        .expect("AemNodeTranslated schema serializes")
}

// AEM generation
pub use aem::{
    AemAttrs, AemConfig, AemConnectionProfile, AemI18nText, AemNode, AemNodeTranslated, AemOption,
    AemOptionTranslated, AemProfile, AemScriptEngine, ConditionRule, LowerConflict,
    OptionAlignment, ParsedAemPackage, ParsedFragment, ResolvedCustomElement, aem_to_structured,
    aem_to_translated, aem_translations_from_content, collect_languages, convert_to_aem,
    detect_aem_zip, generate_aem_package, generate_aem_package_from_node,
    generate_aem_package_from_node_with_passthrough,
    generate_aem_package_from_node_with_translations, generate_aem_package_from_node_with_xml,
    generate_aem_xml, parse_aem_zip, parse_fragment_content, scan_fragments,
    translation_data_from_master_dict, validate_aem_dam_xml, validate_aem_form_xml,
    validate_xml_wellformed,
};

// Post-conversion fidelity review
pub use review::{
    FeedbackViolation, LabelIssue, NamingViolation, ReviewReport, review_output, review_redacto,
};

// GraphViz decision-flow output
pub use graphviz::{
    FieldLabelMap, GraphSelection, GraphSelectionKind, GraphState, build_field_label_map,
    generate_dot,
};

// HTML generation
pub use html::{
    FontFamilyProfile, HtmlConfig, HtmlCustomStyles, HtmlProfile, ResolvedFontFamily,
    ResolvedFontVariant, generate_form_body, generate_html,
};

// XSD generation
pub use xsd::{
    BindRefMaps, ElementMapping, RegisteredComplexType, SectionMapping, TypeChildElement,
    XsdConfig, XsdNode, XsdProfile, XsdSchema, apply_bind_refs, build_registered_types,
    build_xsd_config_from_type_sources, collect_xsd_type_sources_from_dir, compute_bind_refs,
    extract_declared_names, find_matching_types, generate_xsd_from_aem,
    generate_xsd_string_from_aem, load_xsd_config_from_dir, parse_schema, resolve_section_name,
    resolve_section_name_with_heading,
};

// Redacto generation
pub use redacto::{
    AssetRow, AssetType, AssetVersionRow, DocumentRow, DocumentVersionRow, FooterField,
    FooterFieldTemplate, INITIAL_VERSION, ObjectType, OwnerType, OwnershipRow, OwnershipType,
    RedactoComponent, RedactoConfig, RedactoConfiguration, RedactoCounts, RedactoDocumentMeta,
    RedactoDump, RedactoProfile, RedactoValidation, RelationRow, Status as RedactoStatus,
    asset_ref, generate_redacto_dump, generate_redacto_sql, render_block_html, sql_string,
    validate_dump,
};

// The output target a run aims at
pub use target::OutputTarget;

// XFA layer
pub use xfa::scripting::{SomPath, XfaForm};
pub use xfa::{XfaNode, XfaNodeKind};

// Image type (re-export so consumers don't need to depend on `image` directly)
pub use image::RgbaImage;

// Pipeline
pub use pipeline::{
    PipelineConfig, PipelineEvent, PipelineOutput, PipelineStep, StateMap, run_pipeline,
    run_pipeline_async,
};

use std::path::Path;
use std::sync::Arc;
use thiserror::Error;

// ============================================================================
// Render mode
// ============================================================================

/// Render mode for output images.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderMode {
    /// Plain rendering without any annotations.
    Plain,
    /// Labelled rendering with blue group overlays (runs analysis pipeline).
    Labelled,
    /// Annotated rendering with red field annotations.
    Annotated,
}

// ============================================================================
// Error type
// ============================================================================

/// Errors that can occur during blueprint processing.
#[derive(Debug, Error)]
pub enum Error {
    /// The PDF could not be parsed.
    #[error("PDF parse error: {0}")]
    PdfParse(String),
    /// The PDF does not contain XFA data.
    #[error("PDF does not contain XFA data")]
    NoXfaData,
    /// The raw XFA XML could not be parsed into an XFA node tree.
    #[error("XFA parse error: {0}")]
    XfaParse(String),
    /// XFA form creation / scripting failed.
    #[error("Form creation error: {0}")]
    FormCreation(String),
    /// The exhaustive state exploration failed.
    #[error("State exploration error: {0}")]
    StateExploration(String),
    /// Rendering to an image buffer failed.
    #[error("Render error: {0}")]
    Render(String),
    /// Structured conversion failed.
    #[error("Conversion error: {0}")]
    Conversion(String),
    /// AEM configuration could not be constructed (missing XFA variables).
    #[error("AEM config error: {0}")]
    AemConfig(String),
    /// A profile template string could not be rendered.
    #[error("Template error: {0}")]
    Template(String),
    /// Profile loading failed (fonts, config, etc.).
    #[error("Profile error: {0}")]
    Profile(String),
    /// Generic I/O error (e.g. file not found).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ============================================================================
// PDF / XFA extraction helpers
// ============================================================================

/// Extract raw XFA XML bytes from a PDF file on disk.
///
/// Returns `Ok(Some(bytes))` if the PDF contains XFA data, `Ok(None)` if it
/// does not, or an error if the PDF cannot be read / parsed.
pub fn extract_xfa_from_pdf(path: impl AsRef<Path>) -> Result<Option<Vec<u8>>, Error> {
    let data = std::fs::read(path.as_ref()).map_err(Error::Io)?;
    extract_xfa_from_pdf_bytes(&data)
}

/// Extract raw XFA XML bytes from in-memory PDF bytes.
///
/// Returns `Ok(Some(bytes))` if the PDF contains XFA data, `Ok(None)` otherwise.
pub fn extract_xfa_from_pdf_bytes(pdf_bytes: &[u8]) -> Result<Option<Vec<u8>>, Error> {
    let doc = lopdf::Document::load_mem(pdf_bytes).map_err(|e| Error::PdfParse(e.to_string()))?;

    // Navigate: Catalog -> AcroForm -> XFA
    let catalog = doc.catalog().map_err(|e| Error::PdfParse(e.to_string()))?;

    let acroform_ref = match catalog.get(b"AcroForm") {
        Ok(obj) => obj,
        Err(_) => return Ok(None),
    };

    let acroform_dict = match acroform_ref {
        lopdf::Object::Dictionary(d) => d.clone(),
        lopdf::Object::Reference(r) => match doc.get_object(*r) {
            Ok(lopdf::Object::Dictionary(d)) => d.clone(),
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };

    let xfa_obj = match acroform_dict.get(b"XFA") {
        Ok(obj) => obj.clone(),
        Err(_) => return Ok(None),
    };

    match xfa_obj {
        lopdf::Object::Stream(mut stream) => {
            stream.decompress();
            return Ok(Some(stream.content.clone()));
        }
        lopdf::Object::Reference(r) => {
            if let Ok(lopdf::Object::Stream(stream)) = doc.get_object(r).cloned().as_mut() {
                stream.decompress();
                return Ok(Some(stream.content.clone()));
            }
        }
        lopdf::Object::Array(arr) => {
            let mut xfa_data = Vec::new();

            // XFA array: ["preamble", stream_ref, "config", stream_ref, ...]
            for i in (1..arr.len()).step_by(2) {
                let stream_obj = match &arr[i] {
                    lopdf::Object::Reference(r) => doc.get_object(*r).ok().cloned(),
                    lopdf::Object::Stream(_) => Some(arr[i].clone()),
                    _ => None,
                };
                if let Some(lopdf::Object::Stream(mut stream)) = stream_obj {
                    stream.decompress();
                    xfa_data.extend_from_slice(&stream.content);
                }
            }

            if !xfa_data.is_empty() {
                return Ok(Some(xfa_data));
            }
        }
        _ => {}
    }

    Ok(None)
}

// ============================================================================
// FormState — a single snapshot of the form
// ============================================================================

/// A single form state produced by exhaustive exploration.
///
/// Each `FormState` represents the document with a specific combination of
/// radio buttons and checkboxes selected. It holds the flattened layout and
/// a shared reference to the `GlobalContext` (computed from *all* states) so
/// that analysis modules produce consistent results across states.
pub struct FormState {
    /// The flattened layout for this state.
    pub flattened: Flattened,
    /// Which controls were toggled to reach this state.
    pub selections: Vec<Selection>,
    /// Human-readable label (e.g. `"default"`, `"RB_1_CB_2"`).
    pub label: String,
    /// Shared global context computed from all sibling states.
    global_ctx: Arc<GlobalContext>,
}

impl FormState {
    /// Convert this state to a [`DocumentEnvelope`] containing the structured
    /// representation of the form.
    ///
    /// This runs the full analysis pipeline (text grouping, field detection,
    /// heading detection, label attachment, …) and then converts to the
    /// structured node tree.
    pub fn structured(&self, context: Context) -> DocumentEnvelope {
        let mut doc = Document::from_flattened(&self.flattened);
        run_analysis_pipeline_with_context(&mut doc, &self.global_ctx);
        structured::convert_with_context(&doc, context)
    }

    /// Render the page as a plain image (content only, no debug overlays).
    pub fn render_plain(&self, scale: f32) -> Result<RgbaImage, Error> {
        self.flattened
            .render_to_image_buffer_plain(scale)
            .map_err(Error::Render)
    }

    /// Render the page with red field-level debug annotations.
    pub fn render_annotated(&self, scale: f32) -> Result<RgbaImage, Error> {
        self.flattened
            .render_to_image_buffer(scale)
            .map_err(Error::Render)
    }

    /// Render the page with blue group-level analysis overlays.
    ///
    /// This runs the analysis pipeline first, then composites group bounding
    /// boxes and labels on top of the rendered content.
    pub fn render_labelled(&self, scale: f32) -> Result<RgbaImage, Error> {
        let mut doc = Document::from_flattened(&self.flattened);
        run_analysis_pipeline_with_context(&mut doc, &self.global_ctx);
        doc.render_to_image_buffer(scale).map_err(Error::Render)
    }

    /// Render the plain image, split into one image per page.
    ///
    /// The renderer stacks every page into one tall buffer; this returns each
    /// page as its own image so downstream consumers (the vision API, the app
    /// preview) never handle a single oversized image. Single-page forms return
    /// a one-element vector.
    pub fn render_plain_pages(&self, scale: f32) -> Result<Vec<RgbaImage>, Error> {
        let img = self.render_plain(scale)?;
        Ok(self.flattened.slice_into_pages(img, scale))
    }

    /// Render the annotated (red debug overlay) image, split into one image per page.
    pub fn render_annotated_pages(&self, scale: f32) -> Result<Vec<RgbaImage>, Error> {
        let img = self.render_annotated(scale)?;
        Ok(self.flattened.slice_into_pages(img, scale))
    }

    /// Render the labelled (blue group overlay) image, split into one image per page.
    pub fn render_labelled_pages(&self, scale: f32) -> Result<Vec<RgbaImage>, Error> {
        let img = self.render_labelled(scale)?;
        Ok(self.flattened.slice_into_pages(img, scale))
    }
}

// ============================================================================
// Blueprint — the main façade
// ============================================================================

/// High-level entry point for processing PDF documents.
///
/// `Blueprint` supports two modes:
/// - **XFA mode**: The PDF contains XFA data; exhaustive exploration toggles
///   radio buttons and checkboxes to discover all visual states.
/// - **AcroForm mode**: A regular (non-XFA) PDF; form fields and page text
///   are extracted directly into the flattened representation.
///
/// The constructor auto-detects which mode to use.
pub struct Blueprint {
    /// Inner representation differs between XFA and AcroForm PDFs.
    inner: BlueprintInner,
    /// Auto-detected (or caller-supplied) document language.
    language: String,
    /// All `<variables><text>` values from the XFA template (empty for AcroForm).
    variables: std::collections::HashMap<String, String>,
}

/// Distinguishes between XFA and AcroForm PDF processing pipelines.
enum BlueprintInner {
    /// Traditional XFA pipeline: parse XFA XML, explore states via scripting.
    Xfa {
        /// The live XFA form.
        form: XfaForm,
    },
    /// Non-XFA AcroForm pipeline: direct extraction to Flattened.
    AcroForm {
        /// Pre-parsed flattened pages (one per PDF page).
        pages: Vec<Flattened>,
    },
    /// AEM Adaptive Form input pipeline: parsed from ZIP package.
    Aem {
        /// The parsed AEM package (node tree, scripts, translations).
        package: ParsedAemPackage,
    },
}

impl Blueprint {
    // ────────────────────────────────────────────────────────────────────────
    // Construction
    // ────────────────────────────────────────────────────────────────────────

    /// Create a `Blueprint` from a PDF file on disk.
    ///
    /// Auto-detects whether the PDF contains XFA data. If it does, the XFA
    /// pipeline is used; otherwise the AcroForm/content-stream parser
    /// extracts text and fields directly.
    pub fn from_pdf(path: impl AsRef<Path>) -> Result<Self, Error> {
        let pdf_bytes = std::fs::read(path.as_ref()).map_err(Error::Io)?;
        Self::from_pdf_bytes(&pdf_bytes)
    }

    /// Create a `Blueprint` from in-memory PDF bytes.
    ///
    /// Auto-detects XFA vs AcroForm.
    pub fn from_pdf_bytes(pdf_bytes: &[u8]) -> Result<Self, Error> {
        // Try XFA extraction first
        match extract_xfa_from_pdf_bytes(pdf_bytes)? {
            Some(xfa_bytes) => {
                // XFA pipeline - parse once and reuse nodes
                let nodes = XfaNode::parse(&xfa_bytes).map_err(Error::XfaParse)?;
                let (language, variables) = xfa::extract_context_from_nodes(&nodes);
                Self::from_xfa_nodes(nodes, &language, variables)
            }
            None => {
                // AcroForm / regular PDF pipeline
                Self::from_acroform_bytes(pdf_bytes)
            }
        }
    }

    /// Create a `Blueprint` from an AEM ZIP content package.
    ///
    /// Parses the ZIP, extracts the Adaptive Form structure, runs scripts
    /// for visibility/value determination, and builds the AEM node tree.
    pub fn from_aem_zip(zip_bytes: &[u8]) -> Result<Self, Error> {
        let package = aem::parse_aem_zip(zip_bytes)
            .map_err(|e| Error::PdfParse(format!("AEM ZIP parse error: {e}")))?;

        let language = package.language.clone();

        Ok(Blueprint {
            inner: BlueprintInner::Aem { package },
            language,
            variables: std::collections::HashMap::new(),
        })
    }

    /// Create a `Blueprint` from a non-XFA PDF's raw bytes.
    ///
    /// Parses page content streams and AcroForm fields directly into
    /// the flattened representation.
    fn from_acroform_bytes(pdf_bytes: &[u8]) -> Result<Self, Error> {
        let pages = pdf_parser::parse_pdf(pdf_bytes).map_err(Error::PdfParse)?;

        // Default to "en" — a more sophisticated implementation could analyze
        // character frequencies or look at PDF metadata.
        let language = "en".to_string();

        Ok(Blueprint {
            inner: BlueprintInner::AcroForm { pages },
            language,
            variables: std::collections::HashMap::new(),
        })
    }

    /// Create a `Blueprint` directly from raw XFA XML bytes and a language tag.
    ///
    /// Use this when you have already extracted the XFA XML yourself or when
    /// working with non-PDF sources of XFA data.
    pub fn from_xfa_bytes(xfa_bytes: Vec<u8>, language: &str) -> Result<Self, Error> {
        Self::from_xfa_bytes_with_variables(xfa_bytes, language, std::collections::HashMap::new())
    }

    /// Create a `Blueprint` from raw XFA XML bytes, a language tag, and
    /// pre-extracted variables.
    fn from_xfa_bytes_with_variables(
        xfa_bytes: Vec<u8>,
        language: &str,
        variables: std::collections::HashMap<String, String>,
    ) -> Result<Self, Error> {
        let nodes = XfaNode::parse(&xfa_bytes).map_err(Error::XfaParse)?;
        Self::from_xfa_nodes(nodes, language, variables)
    }

    /// Create a `Blueprint` from pre-parsed XFA nodes.
    fn from_xfa_nodes(
        nodes: Vec<XfaNode>,
        language: &str,
        variables: std::collections::HashMap<String, String>,
    ) -> Result<Self, Error> {
        let form = XfaForm::new(nodes).map_err(Error::FormCreation)?;
        Ok(Blueprint {
            inner: BlueprintInner::Xfa { form },
            language: language.to_string(),
            variables,
        })
    }

    /// Returns `true` if this Blueprint was created from an XFA PDF.
    pub fn is_xfa(&self) -> bool {
        matches!(&self.inner, BlueprintInner::Xfa { .. })
    }

    /// Returns `true` if this Blueprint was created from a regular (AcroForm) PDF.
    pub fn is_acroform(&self) -> bool {
        matches!(&self.inner, BlueprintInner::AcroForm { .. })
    }

    /// Returns `true` if this Blueprint was created from an AEM ZIP package.
    pub fn is_aem(&self) -> bool {
        matches!(&self.inner, BlueprintInner::Aem { .. })
    }

    // ────────────────────────────────────────────────────────────────────────
    // Accessors
    // ────────────────────────────────────────────────────────────────────────

    /// The auto-detected (or explicitly provided) document language.
    pub fn language(&self) -> &str {
        &self.language
    }

    /// Build a [`Context`] seeded with the document language and XFA variables.
    pub fn context(&self) -> Context {
        Context::new(self.language.clone(), self.variables.clone())
    }

    /// Access the underlying [`XfaForm`] (only available for XFA PDFs).
    ///
    /// Returns `None` for AcroForm PDFs.
    pub fn form(&self) -> Option<&XfaForm> {
        match &self.inner {
            BlueprintInner::Xfa { form, .. } => Some(form),
            _ => None,
        }
    }

    /// Mutable access to the underlying [`XfaForm`] (only available for XFA PDFs).
    ///
    /// Returns `None` for AcroForm PDFs.
    pub fn form_mut(&mut self) -> Option<&mut XfaForm> {
        match &mut self.inner {
            BlueprintInner::Xfa { form, .. } => Some(form),
            _ => None,
        }
    }

    // ────────────────────────────────────────────────────────────────────────
    // Exhaustive state exploration
    // ────────────────────────────────────────────────────────────────────────

    /// Discover all reachable form states by toggling every visible radio
    /// button and checkbox, then return them as [`FormState`] values.
    ///
    /// This uses the two-pass architecture:
    /// 1. **Collection pass** — recursively explore states, collecting
    ///    `Flattened` snapshots.
    /// 2. **Analysis pass** — compute `GlobalContext` from all snapshots so
    ///    that analysis modules (e.g. heading detection) produce consistent
    ///    results.
    ///
    /// The returned `FormState` values borrow from the `GlobalContext` that is
    /// stored inside the returned [`FormStates`] wrapper.
    pub fn states(&mut self) -> Result<FormStates, Error> {
        match &mut self.inner {
            BlueprintInner::Xfa { form } => {
                let collected = exhaustive::collect_states(form)?;
                Ok(FormStates::new(collected))
            }
            BlueprintInner::AcroForm { pages } => {
                // For AcroForm PDFs there is no scripting. After multi-page
                // merging in parse_pdf(), `pages` always contains exactly one
                // merged Flattened, so we produce a single "default" state.
                let collected: Vec<exhaustive::CollectedState> = pages
                    .iter()
                    .enumerate()
                    .map(|(i, flat)| {
                        let label = if pages.len() == 1 {
                            "default".to_string()
                        } else {
                            format!("page_{}", i + 1)
                        };
                        exhaustive::CollectedState::new_simple(flat.clone(), Vec::new(), label)
                    })
                    .collect();
                Ok(FormStates::new(collected))
            }
            BlueprintInner::Aem { .. } => {
                // AEM doesn't use FormStates. Use aem_structured() instead.
                Err(Error::StateExploration(
                    "AEM input does not support FormStates. Use aem_structured() instead.".into(),
                ))
            }
        }
    }

    /// Get structured output for an AEM input.
    ///
    /// Runs the script engine to determine field visibility and values,
    /// then converts the AEM node tree to `StructuredNode`s.
    ///
    /// Returns `None` if this Blueprint is not an AEM input.
    pub fn aem_structured(&self) -> Option<DocumentEnvelope> {
        let package = match &self.inner {
            BlueprintInner::Aem { package } => package,
            _ => return None,
        };

        // Collect all field names and their initial visibility
        let field_names = collect_aem_field_names(&package.root);
        let initial_values = std::collections::HashMap::new();

        // Run script engine
        let mut engine = aem::AemScriptEngine::new(&field_names, &initial_values);

        // Collect all scripts and execute them
        let all_scripts: Vec<aem::AemScript> = package
            .scripts
            .values()
            .flat_map(|v| v.iter().cloned())
            .collect();
        engine.execute_all(&all_scripts);

        // Read back visibility state
        let visibility = engine.read_visibility();

        // Convert to structured nodes
        let languages: Vec<String> = if package.translations.entries.is_empty() {
            vec![package.language.clone()]
        } else {
            let mut langs: Vec<String> = package
                .translations
                .entries
                .values()
                .flat_map(|m| m.keys().cloned())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            if !langs.contains(&package.language) {
                langs.push(package.language.clone());
            }
            langs
        };

        let content = aem::aem_to_structured(
            &package.root,
            &visibility,
            &package.translations,
            &languages,
            &package.language,
            &package.visibility_conditions,
        );

        let context = self.context();
        Some(DocumentEnvelope {
            context,
            content,
            state_count: 1,
        })
    }

    /// Lift a parsed AEM package into the multilingual [`AemNodeTranslated`]
    /// working tree. Returns `None` for non-AEM inputs.
    ///
    /// This is the inverse of `AemNodeTranslated::lower`: it lets an uploaded
    /// content-package ZIP act as an editable template (pre-loaded as the
    /// conversion agent's working tree) instead of authoring from scratch.
    pub fn aem_translated(&self) -> Option<AemNodeTranslated> {
        let package = match &self.inner {
            BlueprintInner::Aem { package } => package,
            _ => return None,
        };

        // Same language union as `aem_structured`: all languages present across
        // the Sling dictionaries, plus the master language.
        let languages: Vec<String> = if package.translations.entries.is_empty() {
            vec![package.language.clone()]
        } else {
            let mut langs: Vec<String> = package
                .translations
                .entries
                .values()
                .flat_map(|m| m.keys().cloned())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            if !langs.contains(&package.language) {
                langs.push(package.language.clone());
            }
            langs
        };

        Some(aem::aem_to_translated(
            &package.root,
            &package.translations,
            &languages,
            &package.language,
            &package.raw_by_uuid,
        ))
    }

    /// Run full exhaustive exploration *and* merge all state trees into a
    /// single [`DocumentEnvelope`].
    ///
    /// Equivalent to calling [`states()`](Self::states), converting each state
    /// to structured output, and then running `RecursiveMerger`.
    pub fn merged_structured(&mut self) -> Result<DocumentEnvelope, Error> {
        // For AEM input, use the direct AEM structured path
        if self.is_aem() {
            return self
                .aem_structured()
                .ok_or_else(|| Error::Conversion("AEM structured conversion failed".into()));
        }

        let form_states = self.states()?;
        let state_count = form_states.len();
        let mut context = self.context();
        let (merged, header) = merge_form_states(&form_states, context.clone());
        context.header = header;

        Ok(DocumentEnvelope {
            context,
            content: merged,
            state_count,
        })
    }
}

// ============================================================================
// AEM field name collection
// ============================================================================

/// Recursively collect all field/panel names from an AEM node tree.
///
/// Returns `(name, visible)` pairs for use with the script engine.
fn collect_aem_field_names(node: &AemNode) -> Vec<(String, bool)> {
    let mut names = Vec::new();
    collect_aem_field_names_recursive(node, &mut names);
    names
}

fn collect_aem_field_names_recursive(node: &AemNode, names: &mut Vec<(String, bool)>) {
    match node {
        AemNode::Root { children, .. } => {
            for child in children {
                collect_aem_field_names_recursive(child, names);
            }
        }
        AemNode::Panel {
            name,
            children,
            visible,
            ..
        } => {
            names.push((name.clone(), *visible));
            for child in children {
                collect_aem_field_names_recursive(child, names);
            }
        }
        AemNode::TextField { name, visible, .. } => {
            names.push((name.clone(), *visible));
        }
        AemNode::NumberField { name, visible, .. } => {
            names.push((name.clone(), *visible));
        }
        AemNode::DatePicker { name, visible, .. } => {
            names.push((name.clone(), *visible));
        }
        AemNode::Dropdown { name, visible, .. } => {
            names.push((name.clone(), *visible));
        }
        AemNode::Checkbox { name, visible, .. } => {
            names.push((name.clone(), *visible));
        }
        AemNode::RadioButton { name, visible, .. } => {
            names.push((name.clone(), *visible));
        }
        AemNode::TextDraw { name, .. } => {
            names.push((name.clone(), true));
        }
        AemNode::MessageBox { name, .. } => {
            names.push((name.clone(), true));
        }
        AemNode::TitleDraw { name, .. } => {
            names.push((name.clone(), true));
        }
        AemNode::HtmlDisplayer { name, .. } => {
            names.push((name.clone(), true));
        }
        AemNode::Repeatable { name, children, .. } => {
            names.push((name.clone(), true));
            for child in children {
                collect_aem_field_names_recursive(child, names);
            }
        }
        AemNode::Fragment { name, .. } => {
            names.push((name.clone(), true));
        }
        AemNode::Preface { .. }
        | AemNode::Appendix { .. }
        | AemNode::FootnotePlaceholder { .. }
        | AemNode::Custom { .. } => {}
    }
}

// ============================================================================
// Language detection for AcroForm PDFs
// ============================================================================
// FormStates — owns the GlobalContext and yields FormState references
// ============================================================================

/// Owns the collected states and the `GlobalContext` computed from them.
///
/// Individual [`FormState`] values share the context via `Arc`.
pub struct FormStates {
    /// Raw collected data (flattened + selections + label).
    collected: Vec<exhaustive::CollectedState>,
    /// Shared global context built once from all flattened states.
    global_ctx: Arc<GlobalContext>,
}

impl FormStates {
    fn new(collected: Vec<exhaustive::CollectedState>) -> Self {
        // Build owned GlobalContext from cloned flattened states.
        let all_flattened: Vec<Flattened> = collected.iter().map(|s| s.flattened.clone()).collect();
        let global_ctx = Arc::new(GlobalContext::new(all_flattened));
        FormStates {
            collected,
            global_ctx,
        }
    }

    /// Number of unique form states.
    pub fn len(&self) -> usize {
        self.collected.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.collected.is_empty()
    }

    /// Iterate over all form states.
    ///
    /// Each [`FormState`] shares the `GlobalContext` via `Arc`.
    pub fn iter(&self) -> FormStatesIter<'_> {
        FormStatesIter {
            owner: self,
            index: 0,
        }
    }
}

/// Iterator over [`FormState`] values.
pub struct FormStatesIter<'a> {
    owner: &'a FormStates,
    index: usize,
}

impl<'a> Iterator for FormStatesIter<'a> {
    type Item = FormState;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.owner.collected.len() {
            return None;
        }

        let state = &self.owner.collected[self.index];
        self.index += 1;

        Some(FormState {
            flattened: state.flattened.clone(),
            selections: state.selections.clone(),
            label: state.label.clone(),
            global_ctx: Arc::clone(&self.owner.global_ctx),
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.owner.collected.len() - self.index;
        (remaining, Some(remaining))
    }
}

impl<'a> ExactSizeIterator for FormStatesIter<'a> {
    fn len(&self) -> usize {
        self.owner.collected.len() - self.index
    }
}

// ============================================================================
// Shared merge helper
// ============================================================================

/// Collect structured output from every form state and merge into a single tree.
///
/// This is the shared implementation behind [`Blueprint::merged_structured()`]
/// and [`run_exhaustive_to_merged()`].
/// Merge the per-state structured outputs into one tree, and surface the
/// master-page header text recovered during conversion (document furniture that
/// is dropped from the content itself; the same across states, so the first
/// non-empty value is used).
fn merge_form_states(
    form_states: &FormStates,
    context: Context,
) -> (Vec<StructuredNode>, Option<String>) {
    #[cfg(not(target_arch = "wasm32"))]
    let per_state: Vec<(Vec<Selection>, Vec<StructuredNode>, Option<String>)> = {
        use rayon::prelude::*;
        form_states
            .collected
            .par_iter()
            .map(|collected_state| {
                let state = FormState {
                    flattened: collected_state.flattened.clone(),
                    selections: collected_state.selections.clone(),
                    label: collected_state.label.clone(),
                    global_ctx: Arc::clone(&form_states.global_ctx),
                };
                let envelope = state.structured(context.clone());
                (state.selections, envelope.content, envelope.context.header)
            })
            .collect()
    };

    #[cfg(target_arch = "wasm32")]
    let per_state: Vec<(Vec<Selection>, Vec<StructuredNode>, Option<String>)> = form_states
        .iter()
        .map(|state| {
            let envelope = state.structured(context.clone());
            (state.selections, envelope.content, envelope.context.header)
        })
        .collect();

    let header = per_state.iter().find_map(|(_, _, h)| h.clone());
    let structured_outputs = per_state
        .into_iter()
        .map(|(selections, content, _)| (selections, content))
        .collect();
    (merge_structured_outputs(structured_outputs), header)
}

/// Merge per-state structured outputs into a single tree.
pub(crate) fn merge_structured_outputs(
    structured_outputs: Vec<(Vec<Selection>, Vec<StructuredNode>)>,
) -> Vec<StructuredNode> {
    if structured_outputs.is_empty() {
        return Vec::new();
    }

    let merge_inputs: Vec<MergeInput> = structured_outputs
        .into_iter()
        .map(|(selections, nodes)| MergeInput::new(selections, nodes))
        .collect();

    let merger = RecursiveMerger::new(merge_inputs);
    let mut merged = merger.merge();
    normalize_split_checkbox_groups_recursive(&mut merged);
    merged
}

fn normalize_split_checkbox_groups_recursive(nodes: &mut Vec<StructuredNode>) {
    fn option_value_from_field(field: &FieldNode) -> InputValue {
        field
            .som_path
            .as_ref()
            .map(|p| {
                InputValue::Text(
                    p.as_str()
                        .rsplit('.')
                        .next()
                        .unwrap_or(p.as_str())
                        .to_string(),
                )
            })
            .unwrap_or_else(|| InputValue::Text(field.name.to_string()))
    }

    for node in nodes.iter_mut() {
        match node {
            StructuredNode::Group(g) => normalize_split_checkbox_groups_recursive(&mut g.children),
            StructuredNode::Conditional(c) => {
                if let StructuredNode::Group(g) = c.content.as_mut() {
                    normalize_split_checkbox_groups_recursive(&mut g.children);
                }
            }
            StructuredNode::Repeatable(r) => {
                if let StructuredNode::Group(g) = r.item.as_mut() {
                    normalize_split_checkbox_groups_recursive(&mut g.children);
                }
            }
            StructuredNode::GridLayout(g) => {
                for e in &mut g.elements {
                    if let StructuredNode::Group(gr) = &mut e.node {
                        normalize_split_checkbox_groups_recursive(&mut gr.children);
                    }
                }
            }
            _ => {}
        }
    }

    let mut i = 0;
    while i + 2 < nodes.len() {
        let group_and_options = match &nodes[i] {
            StructuredNode::Field(f) => match &f.input_type {
                FieldType::CheckboxGroup { options } if options.len() >= 2 => {
                    Some((f.name.clone(), options[1].value.clone()))
                }
                _ => None,
            },
            _ => None,
        };

        let Some((group_name, second_value)) = group_and_options else {
            i += 1;
            continue;
        };

        let cond = match &nodes[i + 1] {
            StructuredNode::Conditional(c) => Some(c.clone()),
            _ => None,
        };
        let trailing_bool = match &nodes[i + 2] {
            StructuredNode::Field(f) if matches!(f.input_type, FieldType::Bool) => Some(f.clone()),
            _ => None,
        };

        if let (Some(mut cond), Some(trailing_bool)) = (cond, trailing_bool) {
            if let StructuredNode::Field(f) = &mut nodes[i]
                && let FieldType::CheckboxGroup { options } = &mut f.input_type
            {
                if let Some(label) = trailing_bool.label.as_ref().map(|l| l.as_plain_text())
                    && !label.trim().is_empty()
                    && !options.iter().any(|o| o.name.contains(&label))
                {
                    options.push(NameValue {
                        name: TranslatableString::Plain(label),
                        value: option_value_from_field(&trailing_bool),
                    });
                }
            }

            cond.condition.field_name = group_name;
            cond.condition.value = second_value;
            nodes[i + 1] = StructuredNode::Conditional(cond);
            nodes.remove(i + 2);
            continue;
        }

        i += 1;
    }

    let mut j = 0;
    while j + 3 < nodes.len() {
        let (f1, f2, cond, f3) = match (&nodes[j], &nodes[j + 1], &nodes[j + 2], &nodes[j + 3]) {
            (
                StructuredNode::Field(f1),
                StructuredNode::Field(f2),
                StructuredNode::Conditional(cond),
                StructuredNode::Field(f3),
            ) if matches!(f1.input_type, FieldType::Bool)
                && matches!(f2.input_type, FieldType::Bool)
                && matches!(f3.input_type, FieldType::Bool) =>
            {
                (f1.clone(), f2.clone(), cond.clone(), f3.clone())
            }
            _ => {
                j += 1;
                continue;
            }
        };

        if cond.condition.field_name != f2.name || cond.condition.value != InputValue::Bool(true) {
            j += 1;
            continue;
        }

        let Some(name1) = f1.label.as_ref().map(|l| l.as_plain_text()) else {
            j += 1;
            continue;
        };
        let Some(name2) = f2.label.as_ref().map(|l| l.as_plain_text()) else {
            j += 1;
            continue;
        };
        let Some(name3) = f3.label.as_ref().map(|l| l.as_plain_text()) else {
            j += 1;
            continue;
        };

        let group_label = if j > 0 {
            if let StructuredNode::Paragraph(p) = &nodes[j - 1] {
                Some(TranslatedText::single(
                    "default",
                    InlineText::plain(p.content.as_plain_text()),
                ))
            } else {
                None
            }
        } else {
            None
        };

        let options = vec![
            NameValue {
                name: TranslatableString::Plain(name1),
                value: option_value_from_field(&f1),
            },
            NameValue {
                name: TranslatableString::Plain(name2),
                value: option_value_from_field(&f2),
            },
            NameValue {
                name: TranslatableString::Plain(name3),
                value: option_value_from_field(&f3),
            },
        ];

        let mut new_cond = cond;
        new_cond.condition.field_name = f1.name.clone();
        new_cond.condition.value = options[1].value.clone();

        nodes[j] = StructuredNode::Field(FieldNode {
            name: f1.name.clone(),
            som_path: f1.som_path.clone(),
            label: group_label,
            input_type: FieldType::CheckboxGroup { options },
            value: None,
            placeholder: None,
            required: false,
        });
        nodes[j + 1] = StructuredNode::Conditional(new_cond);
        nodes.remove(j + 3);
        nodes.remove(j + 2);
        if j > 0 && matches!(nodes[j - 1], StructuredNode::Paragraph(_)) {
            nodes.remove(j - 1);
            j = j.saturating_sub(1);
        }
    }
}

// ============================================================================
// Convenience free functions
// ============================================================================

/// Merge multiple [`DocumentEnvelope`]s from different languages into one
/// multilingual envelope.
pub fn merge_translations(
    envelopes: Vec<DocumentEnvelope>,
    semantic: Option<&structured::SemanticCtx>,
) -> Result<DocumentEnvelope, structured::MergeError> {
    structured::merge_translations(envelopes, semantic)
}

/// Generate a complete HTML document from structured nodes.
pub fn to_html(content: &[StructuredNode], config: &HtmlConfig) -> String {
    generate_html(content, config)
}

/// Convert structured nodes to an AEM node tree and serialize to XML.
pub fn to_aem(content: &[StructuredNode], config: &AemConfig) -> String {
    ensure_aem_bind_config(config);
    let config = resolve_aem_languages(content, config);
    let root = convert_to_aem(content, &config);
    generate_aem_xml(&root, &config)
}

/// Generate the XSD schema for the form `content` produces.
///
/// The schema is derived from the AEM tree, not from `content` directly, so a
/// standalone XSD is byte-identical to the one bundled in the package and its
/// element paths are exactly the `bindRef`s that form carries.
pub fn to_xsd(
    content: &[StructuredNode],
    aem_config: &AemConfig,
    xsd_config: &XsdConfig,
) -> String {
    let aem_config = resolve_aem_languages(content, aem_config);
    let root = convert_to_aem(content, &aem_config);
    crate::xsd::generate_xsd_string_from_aem(&root, xsd_config, &aem_config.fragments)
}

/// Generate a PostgreSQL dump for the Redacto platform from structured nodes.
///
/// Intended for documents without input fields; any field encountered is
/// skipped and reported in [`RedactoDump::warnings`].
pub fn to_redacto_sql(content: &[StructuredNode], config: &RedactoConfig) -> String {
    let config = resolve_redacto_languages(content, config);
    generate_redacto_sql(content, &config)
}

/// Build the Redacto dump for `content` under `profile`, resolving the profile's
/// configuration against `contexts` and the languages against the content.
///
/// The one place that performs the whole `load_redacto_config` ->
/// `resolve_redacto_languages` -> generate sequence, so callers cannot get the
/// order wrong or skip the language resolution. Returns the resolved config
/// alongside the dump because validation and reporting need both.
///
/// Pass one context per language variant of the source. The document's identity
/// is resolved against the variant written in the profile's master language:
/// each variant carries its own master-page header and its own `Footer_Line_*`
/// XFA variables, so taking whichever one happened to be uploaded first gave an
/// English-master document a Spanish title when the SP variant came first. The
/// page header and footer are then rendered per variant.
///
/// Fails when `contexts` is empty, when the profile has no Redacto section, or
/// when its identity templates cannot be rendered — typically a document lacking
/// the XFA variables the profile derives `document_id` from.
pub fn to_redacto_dump_for_profile(
    profile: &str,
    contexts: &[Context],
    content: &[StructuredNode],
) -> Result<(RedactoDump, RedactoConfig), String> {
    if !has_redacto_config(profile) {
        return Err(format!("Profile '{profile}' has no redacto/config.toml."));
    }
    let master = redacto_master_language(profile)
        .ok_or_else(|| format!("Profile '{profile}' has no readable redacto/config.toml."))?;
    let master_ctx = contexts
        .iter()
        .find(|c| c.language() == master)
        .or_else(|| contexts.first())
        .ok_or("The Redacto dump needs at least one document context.")?;
    let config = load_redacto_config(profile, master_ctx, contexts)?;
    let config = resolve_redacto_languages(content, &config);
    let dump = generate_redacto_dump(content, &config);
    Ok((dump, config))
}

/// Convert structured nodes to a complete AEM FileVault content package (ZIP).
///
/// Returns the raw ZIP bytes ready to be written to disk.
pub fn to_aem_package(content: &[StructuredNode], config: &AemConfig) -> Vec<u8> {
    ensure_aem_bind_config(config);
    let config = resolve_aem_languages(content, config);
    let root = convert_to_aem(content, &config);
    generate_aem_package(&root, &config, content)
}

/// Generate a complete AEM FileVault content package (ZIP) directly from an
/// edited [`AemNode`] tree.
///
/// Unlike [`to_aem_package`], this takes the AEM node tree as the source of
/// truth (e.g. after editing in the AEM editor) rather than re-deriving it from
/// structured nodes. No form-content translation dictionary or XSD is emitted.
pub fn to_aem_package_from_node(root: &AemNode, config: &AemConfig) -> Vec<u8> {
    generate_aem_package_from_node(root, config)
}

/// Like [`to_aem_package_from_node`] but emits an explicit per-language
/// translation dictionary (e.g. labels edited in the AEM editor).
pub fn to_aem_package_from_node_with_translations(
    root: &AemNode,
    config: &AemConfig,
    translations: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
) -> Vec<u8> {
    generate_aem_package_from_node_with_translations(root, config, translations)
}

/// Like [`to_aem_package_from_node_with_translations`] but uses a pre-generated
/// `.content.xml` string verbatim instead of rendering it from the node tree.
///
/// Used when the form XML has been hand-edited (expert mode). The XSD,
/// translations and DAM metadata are still derived from `root`/`translations`.
pub fn to_aem_package_from_node_with_xml(
    root: &AemNode,
    config: &AemConfig,
    translations: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    form_xml: String,
) -> Vec<u8> {
    generate_aem_package_from_node_with_xml(root, config, translations, form_xml)
}

/// Like [`to_aem_package_from_node_with_translations`] but re-emits each node's
/// captured fidelity [`aem::Passthrough`] (raw attributes + unmodeled child
/// elements), keyed by node uuid — for saving a working tree loaded from an
/// existing package losslessly. Get the map from
/// [`aem::AemNodeTranslated::passthrough_map`]; pass an empty map for from-XFA
/// trees (output identical to the plain variant).
pub fn to_aem_package_from_node_with_passthrough(
    root: &AemNode,
    config: &AemConfig,
    translations: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    passthrough: &std::collections::HashMap<uuid::Uuid, aem::Passthrough>,
) -> Vec<u8> {
    generate_aem_package_from_node_with_passthrough(root, config, translations, passthrough)
}

fn ensure_aem_bind_config(config: &AemConfig) {
    assert!(
        !config.bind_to_xsd || config.xsd_config.is_some(),
        "bind_to_xsd=true requires xsd_config to be set"
    );
    // use_fragments does not require xsd_config (it gracefully degrades)
}

/// Detect available languages from content, applying them to a clone of the
/// provided config. The master language is taken from the config as-is
/// (set via the TOML profile or the constructor).
pub fn resolve_aem_languages(content: &[StructuredNode], config: &AemConfig) -> AemConfig {
    let detected_langs = collect_languages(content);
    let mut config = config.clone();
    if !detected_langs.is_empty() {
        config.languages = detected_langs.into_iter().collect();
    }
    config
}

/// Fill [`RedactoConfig::languages`] from the languages present in `content`,
/// keeping the configured languages when the content carries none.
pub fn resolve_redacto_languages(
    content: &[StructuredNode],
    config: &RedactoConfig,
) -> RedactoConfig {
    let detected_langs = collect_languages(content);
    let mut config = config.clone();
    if !detected_langs.is_empty() {
        config.languages = detected_langs.into_iter().collect();
    }
    config
}

/// Run exhaustive exploration on a PDF file and return the merged structured tree.
///
/// This helper reads the PDF from disk, explores all states, and merges them
/// into a single structured representation. It does not perform any file I/O
/// beyond reading the input PDF.
///
/// During tests, results are cached so that repeated calls with the same file
/// are essentially free. The expensive `from_pdf` + `states()` work is shared
/// across all language variants of the same PDF.
pub fn run_exhaustive_to_merged(pdf_path: impl AsRef<Path>) -> Result<Vec<StructuredNode>, Error> {
    run_exhaustive_to_merged_inner(pdf_path.as_ref())
}

fn run_exhaustive_to_merged_inner(pdf_path: &Path) -> Result<Vec<StructuredNode>, Error> {
    #[cfg(test)]
    {
        let envelope = test_cache::cached_envelope(pdf_path, None);
        Ok(envelope.content)
    }

    #[cfg(not(test))]
    {
        let mut bp = Blueprint::from_pdf(pdf_path)?;
        let form_states = bp.states()?;
        let context = bp.context();
        Ok(merge_form_states(&form_states, context).0)
    }
}

/// Run exhaustive exploration on a PDF file and return a `DocumentEnvelope`.
///
/// The caller controls the language value stored in the envelope context.
///
/// During tests, results are cached so that repeated calls with the same
/// arguments are essentially free. The expensive `from_pdf` + `states()` work
/// is shared across all language variants of the same PDF.
pub fn run_exhaustive_to_envelope(
    pdf_path: impl AsRef<Path>,
    language: &str,
) -> Result<DocumentEnvelope, Error> {
    run_exhaustive_to_envelope_inner(pdf_path.as_ref(), language)
}

fn run_exhaustive_to_envelope_inner(
    pdf_path: &Path,
    language: &str,
) -> Result<DocumentEnvelope, Error> {
    #[cfg(test)]
    {
        Ok(test_cache::cached_envelope(pdf_path, Some(language)))
    }

    #[cfg(not(test))]
    {
        let mut bp = Blueprint::from_pdf(pdf_path)?;
        let form_states = bp.states()?;
        let state_count = form_states.len();
        let mut context = bp.context();
        context.set_language(language.to_string());
        let (content, header) = merge_form_states(&form_states, context.clone());
        context.header = header;
        Ok(DocumentEnvelope {
            context,
            content,
            state_count,
        })
    }
}

// ============================================================================
// Test-only two-tier cache
// ============================================================================
//
// Tier 1 – **states cache** (keyed by canonical path):
//   Runs `Blueprint::from_pdf` + `bp.states()` once per PDF.  Shared across
//   all language variants so the expensive XFA scripting / state exploration
//   never repeats.
//
// Tier 2 – **envelope cache** (keyed by canonical path + language):
//   Runs `merge_form_states` (analysis pipeline + recursive merge) once per
//   `(PDF, language)` pair.
//
// Both tiers use `Arc<OnceLock<T>>` entries so that concurrent test threads
// requesting the same key block on the first computation instead of all
// computing in parallel.
// ============================================================================

#[cfg(test)]
mod test_cache {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};

    /// Cached result of `Blueprint::from_pdf` + `bp.states()`.
    struct CachedStates {
        states: FormStates,
        context: Context,
    }

    // -- tier 1: states --

    type StatesEntry = Arc<OnceLock<Arc<CachedStates>>>;

    fn states_cache() -> &'static Mutex<HashMap<std::path::PathBuf, StatesEntry>> {
        static CACHE: OnceLock<Mutex<HashMap<std::path::PathBuf, StatesEntry>>> = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn cached_states(pdf_path: &Path) -> Arc<CachedStates> {
        let key = pdf_path
            .canonicalize()
            .unwrap_or_else(|_| pdf_path.to_path_buf());
        let entry = {
            let mut guard = states_cache().lock().unwrap();
            Arc::clone(
                guard
                    .entry(key)
                    .or_insert_with(|| Arc::new(OnceLock::new())),
            )
        };
        let was_empty = entry.get().is_none();
        if was_empty {
            println!("[cache] states: computing {}", pdf_path.display());
        } else {
            println!("[cache] states: reusing {}", pdf_path.display());
        }
        let data = entry.get_or_init(|| {
            let mut bp = Blueprint::from_pdf(pdf_path)
                .unwrap_or_else(|e| panic!("Failed to load {}: {e}", pdf_path.display()));
            let context = bp.context();
            let states = bp.states().unwrap_or_else(|e| {
                panic!("Failed to explore states for {}: {e}", pdf_path.display())
            });
            Arc::new(CachedStates { states, context })
        });
        Arc::clone(data)
    }

    // -- tier 2: envelope --

    type EnvelopeEntry = Arc<OnceLock<DocumentEnvelope>>;

    fn envelope_cache() -> &'static Mutex<HashMap<(std::path::PathBuf, String), EnvelopeEntry>> {
        static CACHE: OnceLock<Mutex<HashMap<(std::path::PathBuf, String), EnvelopeEntry>>> =
            OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Get a `DocumentEnvelope` from the two-tier cache.
    ///
    /// When `language` is `None` the auto-detected language from the PDF is
    /// used (matching what `run_exhaustive_to_merged` does).
    pub(super) fn cached_envelope(pdf_path: &Path, language: Option<&str>) -> DocumentEnvelope {
        let form_data = cached_states(pdf_path);
        let lang = language
            .map(|l| l.to_string())
            .unwrap_or_else(|| form_data.context.language().to_string());

        let key = (
            pdf_path
                .canonicalize()
                .unwrap_or_else(|_| pdf_path.to_path_buf()),
            lang.clone(),
        );
        let entry = {
            let mut guard = envelope_cache().lock().unwrap();
            Arc::clone(
                guard
                    .entry(key)
                    .or_insert_with(|| Arc::new(OnceLock::new())),
            )
        };
        let was_empty = entry.get().is_none();
        if was_empty {
            println!(
                "[cache] envelope: computing {} ({})",
                pdf_path.display(),
                lang
            );
        } else {
            println!(
                "[cache] envelope: reusing {} ({})",
                pdf_path.display(),
                lang
            );
        }
        let value = entry.get_or_init(|| {
            let state_count = form_data.states.len();
            let mut context = form_data.context.clone();
            context.set_language(lang.clone());
            let (content, header) = merge_form_states(&form_data.states, context.clone());
            context.header = header;
            DocumentEnvelope {
                context,
                content,
                state_count,
            }
        });
        value.clone()
    }
}
