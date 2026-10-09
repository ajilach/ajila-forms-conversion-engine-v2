//! The AEM encoder: a mechanical mapper from a validated
//! [`u2s_aem::model::ValidForm`] to a FileVault content package.
//!
//! # Why this crate carries no business logic
//!
//! The reference implementation this workspace ports from
//! (`~/Documents/ajila-forms-conversion-engine`) needed real business
//! logic here -- fragment matching by XSD type, a UBS custom-element
//! catalogue, naming-convention heuristics -- because its input was a
//! generic `StructuredNode[]` tree that had to be *translated* into AEM's
//! vocabulary. u2s's Conversion Agent has no such gap: it edits
//! `output_json` directly against whatever schema is bound to a dataset,
//! so for whichever AEM format is bound (`aem` or `aem-ubs` -- this crate
//! is the mechanical encoder shared by both) it produces `AemForm`-shaped
//! JSON directly.
//! Whatever judgment call the reference made in Rust, this system makes
//! through the agent, checked afterward by a `check(output, ctx)` rule
//! script -- the same architecture already used for every other rule in
//! this project (PLAN.md's R3.1 is the worked example).
//!
//! That leaves this crate exactly one job: given a tree that is already
//! correct (`ValidForm` is constructible only via `AemForm::validate`),
//! render it. Every function here answers "how is this spelled", never
//! "should this exist" or "which one should this be" -- see this module
//! doc's own reasoning above and `PORTING.md` for the specific pieces of
//! the reference this crate deliberately does not rebuild.
//!
//! # Modules
//!
//! - [`xsd`] -- XSD generation and `bindRef` assignment, one mechanical
//!   walk (no fragment-type matching: every [`u2s_aem::model::ComponentName`]
//!   is already form-wide unique, so a `bindRef` is a pure function of
//!   tree position).
//! - [`xml_writer`] -- the form page `.content.xml`, one function per
//!   [`u2s_aem::model::Node`] variant.
//! - [`i18n`] -- Sling i18n dictionary emission from the tree's own inline
//!   `I18nText`/`I18nRichText` maps (one source of truth, so there is no
//!   merge/precedence logic to get wrong).
//! - [`dam`] -- the DAM asset `.content.xml` (AEM.md §9).
//! - [`package`] -- FileVault ZIP/META-INF assembly.
//! - [`script`] -- the rule storage of `fd:scripts`/`fd:rules`: a JCR
//!   multi-value of JSON rule objects, decoded exactly and written back
//!   byte for byte.
//! - [`fragment_library`] -- **not an encoder concern**: a mechanical text
//!   search over a fragment library directory, offered to the Conversion
//!   Agent as an MCP tool so it can choose a fragment itself. Kept in this
//!   crate, shared by every format server, because the
//!   library-has-the-logic/binary-is-thin split already used throughout
//!   this workspace puts it here rather than duplicated into each one.
//! - [`catalog`] -- **also not an encoder concern**: the stock Foundation-
//!   component table (`specs/AEM.md` §6/§14), browsable the same way, for
//!   a format server's own `component_search` tool.
//! - [`search`] -- the one substring-match predicate `fragment_library`
//!   and `catalog` both search with, so there is exactly one place that
//!   logic lives.

pub mod canonical;
pub mod catalog;
pub mod dam;
pub mod decode;
pub mod fragment_library;
pub mod i18n;
// `pub`, not `pub(crate)`: `jcr::tree`'s parser (`JcrNode`/`parse_jcr_xml`/
// `find_node_by_resource_type`) is generic JCR-XML infrastructure with no
// AEM-format opinion of its own -- the same reuse rationale `decode::zip`
// already gets from `u2s-aem-verify-core`'s `package_check` module, now
// also needed by `u2s-aem-ubs-verify-mcp` to read a UBS form's own
// authored metadata component (mandator/language entities) straight out
// of the package it is about to verify, without a second hand-rolled
// JCR-XML reader.
pub mod jcr;
pub mod package;
pub mod script;
pub(crate) mod search;
pub mod xml_writer;
pub mod xsd;

use u2s_aem::model::ValidForm;

/// The encoded package's bytes, ready to hand to a blob store -- the same
/// shape every other MCP server in this workspace already returns a large
/// payload as.
pub struct EncodedPackage {
    pub bytes: Vec<u8>,
    pub media_type: &'static str,
}

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("could not write XML: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("could not assemble the FileVault package: {0}")]
    Package(#[from] crate::package::PackageError),
}

/// The one entry point: a validated form in, a FileVault content package
/// out. Everything this function calls is mechanical -- see the module
/// doc for what that means and why.
pub fn encode(form: &ValidForm) -> Result<EncodedPackage, EncodeError> {
    let master = form.form().metadata.master_language.clone();
    let xsd = xsd::generate(form);
    let ctx = xml_writer::WriteCtx {
        master: &master,
        bind_refs: &xsd.bind_refs,
        spell_defaults: true,
    };

    let form_xml = xml_writer::write_form_xml(form, &ctx)?;
    let dam_xml = dam::write_dam_xml(form, &master)?;
    let dictionaries = i18n::collect_dictionaries(form);

    let bytes = package::assemble(package::PackageInput {
        form,
        form_xml: &form_xml,
        dam_xml: &dam_xml,
        xsd_xml: xsd.schema_xml.as_deref(),
        dictionaries: &dictionaries,
    })?;

    Ok(EncodedPackage {
        bytes,
        media_type: "application/zip",
    })
}

/// Test-only builders for a valid, minimal [`u2s_aem::model::AemForm`] and
/// its nodes, shared by every module's unit tests in this crate so a
/// fixture shape is defined exactly once.
#[cfg(test)]
pub mod test_support {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::atomic::{AtomicU32, Ordering};

    use u2s_aem::model::{
        AemForm, AssistPriority, ChoiceOption, Common, ComponentName, CssClasses, DataModel,
        DorMode, FieldCommon, FieldLayout, FormMetadata, FormName, I18nRichText, I18nText,
        JcrName, JcrValue, Language, LabelCommon, Node, OptionValue, Page, PanelLayout,
        Passthrough, PlainText, Presence, ResourceType, RichText, TextInput, ValidForm, XmlName,
    };

    pub fn lang(code: &str) -> Language {
        Language::try_from(code).expect("valid test language code")
    }

    pub fn plain(text: &str) -> PlainText {
        PlainText::try_from(text.to_owned()).expect("valid test plain text")
    }

    pub fn i18n_en(text: &str) -> I18nText {
        I18nText::single(lang("en"), plain(text))
    }

    pub fn component_name(name: &str) -> ComponentName {
        ComponentName::try_from(name).expect("valid test component name")
    }

    pub fn option_value(value: &str) -> OptionValue {
        OptionValue::try_from(value).expect("valid test option value")
    }

    pub fn colspan(n: u8) -> u2s_aem::model::ColSpan {
        u2s_aem::model::ColSpan::try_from(n).expect("valid test column span")
    }

    pub fn field_layout() -> FieldLayout {
        FieldLayout {
            width: colspan(12),
            offset: None,
        }
    }

    pub fn panel_layout() -> PanelLayout {
        PanelLayout {
            field: field_layout(),
            dor_columns: None,
        }
    }

    pub fn resource_type(value: &str) -> ResourceType {
        ResourceType::try_from(value).expect("valid test resource type")
    }

    pub fn jcr_name(value: &str) -> JcrName {
        JcrName::try_from(value).expect("valid test jcr name")
    }

    /// A bare [`Common`] with no resource type -- for a generic
    /// [`Node::Component`] whose test cares about something other than how
    /// it is spelled in JCR terms. Most callers want [`common_typed`]
    /// instead.
    pub fn common(name: &str) -> Common {
        Common {
            name: component_name(name),
            resource_type: None,
            guide_node_class: None,
            jcr_name: None,
            visible: true,
            enabled: true,
            css: CssClasses::default(),
            presence: Presence::default(),
            bind_ref: None,
            passthrough: Passthrough::default(),
        }
    }

    /// [`common`] plus an explicit resource type and guide node class --
    /// what every leaf-kind builder below uses, with the same values this
    /// crate's own encoder used to hardcode per `Node` variant, so a test
    /// asserting on that spelling continues to describe real, agent-set
    /// data rather than a Rust default.
    pub fn common_typed(name: &str, resource_type_value: &str, guide_node_class_value: &str) -> Common {
        Common {
            resource_type: Some(resource_type(resource_type_value)),
            guide_node_class: Some(jcr_name(guide_node_class_value)),
            ..common(name)
        }
    }

    pub fn field_common(name: &str) -> FieldCommon {
        FieldCommon {
            label: i18n_en(name),
            mandatory: false,
            mandatory_message: None,
            placeholder: None,
            assist: AssistPriority::default(),
        }
    }

    pub fn label_common(name: &str) -> LabelCommon {
        LabelCommon {
            label: i18n_en(name),
            placeholder: None,
            assist: AssistPriority::default(),
        }
    }

    pub fn xml_schema(root: &str) -> DataModel {
        DataModel::XmlSchema {
            root_element: XmlName::try_from(root).expect("valid test root element name"),
        }
    }

    pub fn a_text_field(name: &str) -> Node {
        Node::TextField {
            common: common_typed(name, "fd/af/components/controls/textbox", "guideTextBox"),
            field: field_common(name),
            layout: field_layout(),
            input: TextInput::SingleLine,
            max_chars: None,
            autofill: None,
            validation: None,
        }
    }

    /// A bare, childless [`Node::Component`] -- the generic node every
    /// structural test now builds directly, per this crate's redesign (see
    /// `u2s_aem::model::Node`'s own doc).
    pub fn a_component(name: &str, resource_type_value: &str, children: Vec<Node>) -> Node {
        Node::Component {
            common: common_typed(name, resource_type_value, "guidePanel"),
            properties: BTreeMap::new(),
            children,
        }
    }

    /// A `Component` with the same `fd/af/components/panel` resource type
    /// this crate's own encoder used to hardcode for every
    /// (now-removed) `Node::Panel` -- kept under this name so existing test
    /// call sites read the same way.
    pub fn a_panel(name: &str, children: Vec<Node>) -> Node {
        a_component(name, "fd/af/components/panel", children)
    }

    /// A panel carrying a `jcr:title` property, the same shape a page's own
    /// `properties` uses (see [`a_page_named`]).
    pub fn a_panel_with_title(name: &str, title_en: &str, children: Vec<Node>) -> Node {
        let mut node = a_panel(name, children);
        if let Node::Component { properties, .. } = &mut node {
            properties.insert(
                jcr_name_key("jcr:title"),
                JcrValue::Text(i18n_en(title_en)),
            );
        }
        node
    }

    pub fn a_static_text(name: &str) -> Node {
        Node::StaticText {
            common: common_typed(name, "fd/af/components/controls/textdraw", "guideTextDraw"),
            layout: field_layout(),
            content: I18nRichText::single(
                lang("en"),
                RichText::try_from(format!("<p>{name}</p>")).expect("valid test rich text"),
            ),
            heading_level: None,
        }
    }

    /// A `Component` carrying a `fragRef` property -- the same shape a real
    /// fragment reference takes now that `Node::Fragment` is gone (see
    /// `u2s_aem::model::Node`'s own doc: a fragment is just a panel with a
    /// `fragRef`).
    pub fn a_fragment(name: &str) -> Node {
        let mut node = a_panel(name, Vec::new());
        if let Node::Component { properties, .. } = &mut node {
            properties.insert(
                jcr_name_key("fragRef"),
                JcrValue::Single(format!("/content/dam/formsanddocuments/lib/{name}")),
            );
        }
        node
    }

    fn jcr_name_key(value: &str) -> JcrName {
        JcrName::try_from(value).expect("valid test property key")
    }

    pub fn a_dropdown(name: &str, options: &[(&str, &str)]) -> Node {
        let options = options
            .iter()
            .map(|(value, label)| ChoiceOption {
                value: option_value(value),
                label: i18n_en(label),
            })
            .collect::<Vec<_>>();
        Node::Dropdown {
            common: common_typed(name, "fd/af/components/controls/dropdownlist", "guideDropDownList"),
            field: field_common(name),
            layout: field_layout(),
            options: options.try_into().expect("at least one test option"),
            filtering_allowed: false,
            sort: None,
        }
    }

    /// A page with the given name -- use this over [`a_page_with`] whenever
    /// a test builds more than one page in the same form, since names must
    /// be form-wide unique. `properties` used to be typed `title`/
    /// `presence`/`layout` fields; a page's own configuration is now the
    /// same open, agent-authored bag a `Component`'s is (see `Page`'s own
    /// doc), so this builder defaults it empty -- a test that needs a
    /// specific page property sets it directly on the returned value.
    pub fn a_page_named(name: &str, children: Vec<Node>) -> Page {
        Page {
            name: component_name(name),
            jcr_name: None,
            properties: BTreeMap::new(),
            passthrough: Default::default(),
            children,
        }
    }

    /// A single-use page with an auto-generated unique name, for the
    /// common case of a test that only ever builds one page.
    pub fn a_page_with(children: Vec<Node>) -> Page {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        a_page_named(&format!("Page{n}"), children)
    }

    pub fn build_form(data_model: DataModel, pages: Vec<Page>) -> ValidForm {
        build_form_with_toolbar(data_model, pages, Vec::new())
    }

    pub fn build_form_with_toolbar(
        data_model: DataModel,
        pages: Vec<Page>,
        toolbar: Vec<Node>,
    ) -> ValidForm {
        let metadata = FormMetadata {
            form_name: FormName::try_from("TestForm").expect("valid test form name"),
            title: None,
            master_language: lang("en"),
            languages: BTreeSet::from([lang("en")]),
            dor: DorMode::None,
            data_model,
            toolbar,
            folder_path: Vec::new(),
            root_panel_layout: None,
            chrome: Default::default(),
            dam_chrome: Default::default(),
            page_content: Default::default(),
            root_panel: Default::default(),
            toolbar_chrome: Default::default(),
        };
        AemForm { metadata, pages }
            .validate()
            .expect("test form must validate")
    }
}
