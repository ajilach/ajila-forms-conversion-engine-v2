//! The form page `.content.xml` writer: one function per [`Node`] variant,
//! rendering AEM's own structural requirements (AEM.md §3-§7) from data a
//! [`ValidForm`] already guarantees is present and correct. Nothing here
//! decides *what* a node is or *whether* it should exist -- that is the
//! Conversion Agent's job, checked by the rule engine before this ever
//! runs. This module only knows how a decided node is spelled.
//!
//! **This module carries less than it used to.** Four behaviours this
//! crate previously hardcoded moved out entirely as part of this crate's
//! redesign (see `u2s_aem::model::Node`'s own doc for the full reasoning):
//!
//! - **Repeatable expansion**: there is no `write_repeatable` any more. A
//!   repeatable is two ordinary [`Node::Component`] panels the Conversion
//!   Agent authors directly (an outer wrapper, an inner repeating panel,
//!   both buttons), and [`write_component`] writes whatever tree it is
//!   given — nothing here decides that a name ending a certain way needs
//!   two panels instead of one.
//! - **Visibility-rule script synthesis**: there is no `write_visibility_rule`
//!   any more. AEM.md never documented a grammar for the script this
//!   module used to invent (`this.visible = ...`), and the real corpus
//!   uses `fd:rules`/`fd:visible` JSON instead — the agent now authors
//!   that content directly (as ordinary `Component` children or raw
//!   [`u2s_aem::model::Passthrough`]), checked by a rule rather than a
//!   fixed Rust script shape.
//! - **Layout/wizard/DoR defaults**: nothing here assumes
//!   `panelSetType="wizard"`, `toolbarPosition="Bottom"`, or any other
//!   per-attribute default. Every property a `Component` or a `Page`
//!   carries is exactly what its own `properties` map says, no more.
//! - **The toolbar's button set and titles**: there is no `ToolbarButton`
//!   enum. `FormMetadata.toolbar` is `Vec<Node>`, written the same way any
//!   other list of `Component`s is.
//!
//! What is left is exactly what the module doc always claimed: how a
//! decided node is spelled as JCR XML, for the seven leaf field kinds that
//! stay distinct (real domain types, not authoring convention -- see
//! `Node`'s own doc) and for the one generic [`Node::Component`] shape
//! everything structural now uses.

use std::collections::HashMap;
use std::io::Cursor;

use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::writer::Writer;

use u2s_aem::model::{
    Common, ComponentName, DateFormat, DorMode, FieldCommon, FieldLayout, HeadingLevel, JcrValue,
    LabelCommon, Language, NamedDateFormat, NamedNumberFormat, Node, NumberFormat, OptionAlignment,
    Page, Presence, RawJcrNode, SortOrder, TextInput, ValidForm,
};

use crate::jcr::{multi_value, ns, option_pair, plain_bool, typed_bool};

pub type XmlResult<T> = Result<T, quick_xml::Error>;

/// Everything a node writer needs beyond the node itself: which language
/// is inlined literally (every other language lives in a dictionary --
/// see `crate::i18n`), and the `bindRef` table [`crate::xsd::generate`]
/// already computed, so this module never re-derives one.
pub struct WriteCtx<'a> {
    pub master: &'a Language,
    pub bind_refs: &'a HashMap<ComponentName, String>,
}

impl WriteCtx<'_> {
    fn bind_ref(&self, name: &ComponentName) -> Option<&str> {
        self.bind_refs.get(name).map(String::as_str)
    }
}

/// The whole form page `.content.xml`, from `<jcr:root>` down.
pub fn write_form_xml(form: &ValidForm, ctx: &WriteCtx) -> XmlResult<String> {
    let inner = form.form();
    let mut w = Writer::new_with_indent(Cursor::new(Vec::new()), b' ', 2);

    let title = display_title(form, ctx.master);

    let mut root = BytesStart::new("jcr:root");
    root.push_attribute(("xmlns:jcr", ns::JCR));
    root.push_attribute(("xmlns:sling", ns::SLING));
    root.push_attribute(("xmlns:cq", ns::CQ));
    root.push_attribute(("xmlns:nt", ns::NT));
    root.push_attribute(("xmlns:fd", ns::FD));
    root.push_attribute(("jcr:primaryType", "cq:Page"));
    w.write_event(Event::Start(root))?;

    let mut content = BytesStart::new("jcr:content");
    content.push_attribute(("jcr:primaryType", "cq:PageContent"));
    content.push_attribute(("jcr:title", title.as_str()));
    content.push_attribute(("sling:resourceType", "fd/af/components/guideContainer"));
    // The real fixture package settles this: `jcr:language`, a standard
    // JCR/CQ property (the same `mix:language` concept every dictionary
    // file's own `jcr:language` already uses), on the page's own
    // `jcr:content` node -- not an attribute this crate invented, unlike
    // an earlier draft's own `masterLanguage` on `guideContainer`.
    content.push_attribute(("jcr:language", ctx.master.as_str()));
    w.write_event(Event::Start(content))?;

    write_guide_container(&mut w, inner, ctx, &title)?;

    w.write_event(Event::End(BytesEnd::new("jcr:content")))?;
    w.write_event(Event::End(BytesEnd::new("jcr:root")))?;

    Ok(String::from_utf8(w.into_inner().into_inner()).expect("quick-xml writes valid UTF-8"))
}

/// AEM.md §5.2: falls back to `form_name` when no title was authored.
fn display_title(form: &ValidForm, master: &Language) -> String {
    form.form()
        .metadata
        .title
        .as_ref()
        .and_then(|t| t.get(master))
        .map(|t| t.as_str().to_owned())
        .unwrap_or_else(|| form.form().metadata.form_name.as_str().to_owned())
}

fn write_guide_container(
    w: &mut Writer<Cursor<Vec<u8>>>,
    form: &u2s_aem::model::AemForm,
    ctx: &WriteCtx,
    title: &str,
) -> XmlResult<()> {
    let mut container = BytesStart::new("guideContainer");
    container.push_attribute(("jcr:primaryType", "nt:unstructured"));
    container.push_attribute(("sling:resourceType", "fd/af/components/guideContainer"));
    container.push_attribute(("guideNodeClass", "guideContainerNode"));
    // Confirmed against the real fixture package
    // (`tests/fixtures/AF_AABF.zip`): "1.1", not "2.1".
    container.push_attribute(("fd:version", "1.1"));
    container.push_attribute((
        "dorType",
        match form.metadata.dor {
            DorMode::Generate => "generate",
            DorMode::None => "none",
        },
    ));
    // `chrome`'s own raw attributes are plain strings, unlike a
    // `Component`'s `properties` -- guideContainer-level configuration has
    // no translatable-value concept the way node content does, so this
    // needs none of `write_property_attributes`' `JcrValue` handling.
    for (key, value) in &form.metadata.chrome.raw_attributes {
        container.push_attribute((key.as_str(), value.as_str()));
    }
    w.write_event(Event::Start(container))?;

    write_root_panel(w, form, ctx, title)?;
    write_raw_children(w, &form.metadata.chrome.raw_children)?;

    // Confirmed against the real fixture: `guideContainer`'s own `layout`
    // child, mechanical and always this one resource type (unlike
    // `rootPanel`'s own `layout`, which varies -- see
    // `FormMetadata::root_panel_layout`'s own doc).
    let mut guide_layout = BytesStart::new("layout");
    guide_layout.push_attribute(("jcr:primaryType", "nt:unstructured"));
    guide_layout.push_attribute(("sling:resourceType", "fd/af/layouts/defaultGuideLayout"));
    w.write_event(Event::Empty(guide_layout))?;

    w.write_event(Event::End(BytesEnd::new("guideContainer")))
}

fn write_root_panel(
    w: &mut Writer<Cursor<Vec<u8>>>,
    form: &u2s_aem::model::AemForm,
    ctx: &WriteCtx,
    title: &str,
) -> XmlResult<()> {
    let mut root_panel = BytesStart::new("rootPanel");
    root_panel.push_attribute(("jcr:primaryType", "nt:unstructured"));
    // §14's own table entry, confirmed against the real fixture package
    // (`tests/fixtures/AF_AABF.zip`) over §5.4's worked example
    // (`fd/af/components/panel`) -- see `specs/AEM.md`'s own "Observed
    // Deviations" appendix.
    root_panel.push_attribute(("sling:resourceType", "fd/af/components/rootPanel"));
    root_panel.push_attribute(("guideNodeClass", "guideRootPanel"));
    root_panel.push_attribute(("jcr:title", title));
    root_panel.push_attribute(("textIsRich", "true"));
    w.write_event(Event::Start(root_panel))?;

    // `rootPanel`'s own, separate `layout` child -- present only when the
    // form carries one (a real package's own wizard layout, say). Written
    // before `items`, matching the real fixture's own child order.
    if let Some(layout_type) = &form.metadata.root_panel_layout {
        let mut layout = BytesStart::new("layout");
        layout.push_attribute(("jcr:primaryType", "nt:unstructured"));
        layout.push_attribute(("sling:resourceType", layout_type.as_str()));
        w.write_event(Event::Empty(layout))?;
    }

    // Confirmed against the real fixture: `items` itself carries
    // `gridFluidLayout2`, unconditionally -- mechanical, unlike the
    // `layout` child above, which is what actually varies.
    let mut items = BytesStart::new("items");
    items.push_attribute(("jcr:primaryType", "nt:unstructured"));
    items.push_attribute(("sling:resourceType", "fd/af/layouts/gridFluidLayout2"));
    w.write_event(Event::Start(items))?;
    for page in &form.pages {
        write_page(w, page, ctx)?;
    }
    w.write_event(Event::End(BytesEnd::new("items")))?;

    if !form.metadata.toolbar.is_empty() {
        write_toolbar(w, &form.metadata.toolbar, ctx)?;
    }

    w.write_event(Event::End(BytesEnd::new("rootPanel")))
}

/// The toolbar's own action set used to be a closed `ToolbarButton` enum
/// with hardcoded titles and resource types this module chose; it is now
/// `Vec<Node>` (see `FormMetadata::toolbar`'s own doc), written the same
/// way any other list of nodes is. The `<toolbar>` wrapper and its own
/// `items`/`layout` pair (both `toolbar/defaultToolbarLayout`, confirmed
/// against the real fixture, unconditionally -- mechanical, no variation
/// observed) are the fixed JCR structure this function still owns.
fn write_toolbar(w: &mut Writer<Cursor<Vec<u8>>>, actions: &[Node], ctx: &WriteCtx) -> XmlResult<()> {
    let mut toolbar = BytesStart::new("toolbar");
    toolbar.push_attribute(("jcr:primaryType", "nt:unstructured"));
    toolbar.push_attribute(("guideNodeClass", "guideToolbar"));
    toolbar.push_attribute(("sling:resourceType", "fd/af/layouts/toolbar/defaultToolbarLayout"));
    w.write_event(Event::Start(toolbar))?;

    let mut items = BytesStart::new("items");
    items.push_attribute(("jcr:primaryType", "nt:unstructured"));
    items.push_attribute(("sling:resourceType", "fd/af/layouts/toolbar/defaultToolbarLayout"));
    w.write_event(Event::Start(items))?;
    write_children(w, actions, ctx)?;
    w.write_event(Event::End(BytesEnd::new("items")))?;

    let mut layout = BytesStart::new("layout");
    layout.push_attribute(("jcr:primaryType", "nt:unstructured"));
    layout.push_attribute(("sling:resourceType", "fd/af/layouts/toolbar/defaultToolbarLayout"));
    w.write_event(Event::Empty(layout))?;

    w.write_event(Event::End(BytesEnd::new("toolbar")))
}

/// A page is a wizard step, not a data grouping (see `crate::xsd`'s module
/// doc): it becomes a panel directly under `rootPanel`'s `items`.
/// `properties` carries whatever configuration this step needs
/// (`jcr:title`, `panelSetType`, ...) -- the Conversion Agent's own,
/// agent-authored bag, the same shape a [`Node::Component`]'s `properties`
/// use (see [`write_property_attributes`]); this function assumes none of
/// it, per this crate's redesign.
fn write_page(w: &mut Writer<Cursor<Vec<u8>>>, page: &Page, ctx: &WriteCtx) -> XmlResult<()> {
    let tag = page.name.as_str();
    let mut el = BytesStart::new(tag);
    el.push_attribute(("jcr:primaryType", "nt:unstructured"));
    el.push_attribute(("sling:resourceType", "fd/af/components/panel"));
    el.push_attribute(("guideNodeClass", "guidePanel"));
    el.push_attribute(("name", tag));
    let property_attrs = write_property_attributes(&page.properties, ctx.master);
    for (key, value) in &property_attrs {
        el.push_attribute((key.as_str(), value.as_str()));
    }
    w.write_event(Event::Start(el))?;

    w.write_event(Event::Start(BytesStart::new("items")))?;
    write_children(w, &page.children, ctx)?;
    w.write_event(Event::End(BytesEnd::new("items")))?;

    w.write_event(Event::End(BytesEnd::new(tag)))
}

/// Spells a [`Node::Component`]'s (or a [`Page`]'s) `properties` map as
/// JCR attribute values: `Single`/`Multi` verbatim (see [`JcrValue`]'s own
/// doc on the type-hint boundary), `Text`/`RichText` as the master
/// language's own value -- every other language reaches the dictionary
/// through `crate::i18n::collect_dictionaries` instead, which walks the
/// same `properties` map independently (one source of truth, not two).
/// Returns owned pairs, not pushed attributes directly, for the same
/// borrow-lifetime reason [`css_value`] does: a `BytesStart` attribute
/// value must outlive the element, and a computed `String` only lives as
/// long as its caller's own locals.
fn write_property_attributes(
    properties: &std::collections::BTreeMap<u2s_aem::model::JcrName, JcrValue>,
    master: &Language,
) -> Vec<(String, String)> {
    properties
        .iter()
        .map(|(key, value)| {
            let spelled = match value {
                JcrValue::Single(s) => s.clone(),
                JcrValue::Multi(items) => multi_value(items),
                JcrValue::Text(text) => master_text(text, master).unwrap_or_default().to_owned(),
                JcrValue::RichText(text) => text
                    .get(master)
                    .map(|t| t.as_str())
                    .unwrap_or_default()
                    .to_owned(),
            };
            (key.as_str().to_owned(), spelled)
        })
        .collect()
}

fn write_children(w: &mut Writer<Cursor<Vec<u8>>>, nodes: &[Node], ctx: &WriteCtx) -> XmlResult<()> {
    for node in nodes {
        write_node(w, node, ctx)?;
    }
    Ok(())
}

fn write_node(w: &mut Writer<Cursor<Vec<u8>>>, node: &Node, ctx: &WriteCtx) -> XmlResult<()> {
    match node {
        Node::Component {
            common,
            properties,
            children,
        } => write_component(w, common, properties, children, ctx),
        Node::TextField {
            common,
            field,
            layout,
            input,
            max_chars,
            autofill,
            validation,
        } => write_text_field(w, common, field, layout, *input, *max_chars, autofill.as_ref(), validation.as_ref(), ctx),
        Node::NumberField {
            common,
            field,
            layout,
            format,
            validation,
        } => write_number_field(w, common, field, layout, format.as_ref(), validation.as_ref(), ctx),
        Node::DatePicker {
            common,
            field,
            layout,
            default_to_current_date,
            format,
            year_range,
            validation,
        } => write_date_picker(
            w,
            common,
            field,
            layout,
            *default_to_current_date,
            format.as_ref(),
            year_range.as_ref(),
            validation.as_ref(),
            ctx,
        ),
        Node::Dropdown {
            common,
            field,
            layout,
            options,
            filtering_allowed,
            sort,
        } => write_dropdown(w, common, field, layout, options, *filtering_allowed, sort.as_ref(), ctx),
        Node::Checkbox {
            common,
            field,
            layout,
            options,
            alignment,
            hide_title,
            rich_text_options,
        } => write_checkbox(w, common, field, layout, options, *alignment, *hide_title, *rich_text_options, ctx),
        Node::RadioButton {
            common,
            field,
            layout,
            options,
            alignment,
            rich_text_options,
        } => write_radio_button(w, common, field, layout, options, *alignment, *rich_text_options, ctx),
        Node::StaticText {
            common,
            layout,
            content,
            heading_level,
        } => write_static_text(w, common, layout, content, heading_level.as_ref(), ctx),
        Node::Signature { common, field, layout } => write_signature(w, common, field, layout, ctx),
    }
}

// ------------------------------------------------------------- shared attrs

fn push_presence(el: &mut BytesStart, presence: &Presence) {
    el.push_attribute(("dorExclusion", typed_bool(presence.dor_exclusion)));
    if presence.dor_exclude_title {
        el.push_attribute(("dorExcludeTitle", "true"));
    }
    if presence.dor_exclude_description {
        el.push_attribute(("dorExcludeDescription", "true"));
    }
    el.push_attribute(("summaryExclusion", typed_bool(presence.summary_exclusion)));
}

fn push_common<'a>(el: &mut BytesStart<'a>, common: &'a Common, bind_ref: Option<&'a str>) {
    // `sling:resourceType`/`guideNodeClass` are agent-authored now (see
    // `Common`'s own doc on why this crate no longer chooses them), so
    // every node kind reads them from `common` here, in one place, rather
    // than each leaf writer hardcoding its own literal.
    if let Some(resource_type) = &common.resource_type {
        el.push_attribute(("sling:resourceType", resource_type.as_str()));
    }
    if let Some(guide_node_class) = &common.guide_node_class {
        el.push_attribute(("guideNodeClass", guide_node_class.as_str()));
    }
    el.push_attribute(("name", common.name.as_str()));
    el.push_attribute(("visible", typed_bool(common.visible)));
    el.push_attribute(("enabled", typed_bool(common.enabled)));
    // `css` is not pushed here: a `push_attribute` value must outlive
    // `el`, and the joined class list is a computed `String` the caller
    // owns only for the duration of its own function -- so every caller
    // computes it with `css_value` and pushes it itself.
    if let Some(bind_ref) = bind_ref {
        el.push_attribute(("bindRef", bind_ref));
    }
}

/// A node's `bindRef`: the carried value if [`Common::bind_ref`] has one
/// (round-tripping a decoded document, whose real `bindRef` may not match
/// this crate's own derivation -- see that field's own doc), else the
/// mechanical derivation [`crate::xsd::generate`] computed from tree
/// position, for a green-field [`u2s_aem::model::DataModel::XmlSchema`]
/// form the Conversion Agent authored directly.
fn resolve_bind_ref(common: &Common, ctx: &WriteCtx) -> Option<String> {
    common
        .bind_ref
        .clone()
        .or_else(|| ctx.bind_ref(&common.name).map(str::to_owned))
}

/// Pushes [`Common::passthrough`]'s raw attributes verbatim -- everything
/// this node's own JCR element carried that decode did not fold into a
/// typed field. Pushed last, after every typed attribute this function's
/// caller already wrote, so a real collision (a passthrough key naming an
/// attribute this crate's own typed fields already own) is at least a
/// reproducible last-write-wins rather than an arbitrary one; catching it
/// as a hard `EncodeError` is deferred until the decoder that could ever
/// populate a colliding key exists (`decode/form.rs`, not built in this
/// pass) -- a green-field form the Conversion Agent authors always has an
/// empty `Passthrough` and never exercises this path at all.
pub(crate) fn push_passthrough_attributes<'a>(
    el: &mut BytesStart<'a>,
    passthrough: &'a u2s_aem::model::Passthrough,
) {
    for (key, value) in &passthrough.raw_attributes {
        el.push_attribute((key.as_str(), value.as_str()));
    }
}

pub(crate) fn write_raw_children(w: &mut Writer<Cursor<Vec<u8>>>, nodes: &[RawJcrNode]) -> XmlResult<()> {
    for node in nodes {
        write_raw_node(w, node)?;
    }
    Ok(())
}

/// The exact inverse of `decode`'s own capture of an unmodelled JCR
/// element into a [`RawJcrNode`] -- see [`u2s_aem::model::Passthrough`]'s
/// own doc.
pub(crate) fn write_raw_node(w: &mut Writer<Cursor<Vec<u8>>>, node: &RawJcrNode) -> XmlResult<()> {
    let mut el = BytesStart::new(node.tag_name.as_str());
    for (key, value) in &node.attributes {
        el.push_attribute((key.as_str(), value.as_str()));
    }
    if node.children.is_empty() {
        w.write_event(Event::Empty(el))
    } else {
        w.write_event(Event::Start(el))?;
        write_raw_children(w, &node.children)?;
        w.write_event(Event::End(BytesEnd::new(node.tag_name.as_str())))
    }
}

/// `push_common` cannot own a computed `String` (attribute values must
/// outlive the element), so the CSS class list -- the one common attribute
/// that is joined from parts rather than borrowed whole -- is written by
/// the caller, which does own its own locals for the duration of the call.
fn css_value(common: &Common) -> Option<String> {
    let classes = common.css.classes();
    if classes.is_empty() {
        None
    } else {
        Some(
            classes
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        )
    }
}

fn write_responsive(w: &mut Writer<Cursor<Vec<u8>>>, layout: &FieldLayout) -> XmlResult<()> {
    w.write_event(Event::Start(BytesStart::new("cq:responsive")))?;
    let width = layout.width.value().to_string();
    let offset = layout.offset.map(|o| o.value()).unwrap_or(0).to_string();
    let mut default_el = BytesStart::new("default");
    default_el.push_attribute(("jcr:primaryType", "nt:unstructured"));
    default_el.push_attribute(("width", width.as_str()));
    default_el.push_attribute(("offset", offset.as_str()));
    w.write_event(Event::Empty(default_el))?;
    w.write_event(Event::End(BytesEnd::new("cq:responsive")))
}

fn master_text<'a>(text: &'a u2s_aem::model::I18nText, master: &Language) -> Option<&'a str> {
    text.get(master).map(|t| t.as_str())
}

// -------------------------------------------------------------- Component

/// The one generic node writer -- see the module doc for what this
/// replaced (a `Node::Panel`, a `Node::Repeatable`'s outer/inner/button
/// expansion, and a `Node::Fragment` all used to be separate functions with
/// their own hardcoded JCR shape; a `Component` is agent-authored, so this
/// function only ever spells whatever it is given).
///
/// Children are wrapped in one `<items sling:resourceType="fd/af/layouts/
/// gridFluidLayout2">` when there are any -- the one piece of fixed JCR
/// structure this function still owns, since every panel-shaped element in
/// the real corpus needs it and there is no variation for the mapper to
/// choose between (see [`write_page`], which does the same). A childless
/// `Component` (a leaf-like generic node -- a `messagebox`, a `summary`,
/// ...) gets no `items` wrapper at all, matching the real corpus.
fn write_component(
    w: &mut Writer<Cursor<Vec<u8>>>,
    common: &Common,
    properties: &std::collections::BTreeMap<u2s_aem::model::JcrName, JcrValue>,
    children: &[Node],
    ctx: &WriteCtx,
) -> XmlResult<()> {
    let tag = common
        .jcr_name
        .as_ref()
        .map(|n| n.as_str())
        .unwrap_or_else(|| common.name.as_str());
    let bind_ref = resolve_bind_ref(common, ctx);
    let css = css_value(common);
    let property_attrs = write_property_attributes(properties, ctx.master);

    let mut el = BytesStart::new(tag);
    el.push_attribute(("jcr:primaryType", "nt:unstructured"));
    push_common(&mut el, common, bind_ref.as_deref());
    if let Some(css) = css.as_deref() {
        el.push_attribute(("css", css));
    }
    for (key, value) in &property_attrs {
        el.push_attribute((key.as_str(), value.as_str()));
    }
    push_presence(&mut el, &common.presence);
    push_passthrough_attributes(&mut el, &common.passthrough);

    let has_content = !children.is_empty() || !common.passthrough.raw_children.is_empty();
    if !has_content {
        return w.write_event(Event::Empty(el));
    }
    w.write_event(Event::Start(el))?;

    if !children.is_empty() {
        w.write_event(Event::Start(BytesStart::new("items")))?;
        write_children(w, children, ctx)?;
        w.write_event(Event::End(BytesEnd::new("items")))?;
    }
    write_raw_children(w, &common.passthrough.raw_children)?;

    w.write_event(Event::End(BytesEnd::new(tag)))
}

// ---------------------------------------------------------------- TextField

#[allow(clippy::too_many_arguments)]
fn write_text_field(
    w: &mut Writer<Cursor<Vec<u8>>>,
    common: &Common,
    field: &FieldCommon,
    layout: &FieldLayout,
    input: TextInput,
    max_chars: Option<std::num::NonZeroU32>,
    autofill: Option<&u2s_aem::model::AutofillHint>,
    validation: Option<&u2s_aem::model::Validation>,
    ctx: &WriteCtx,
) -> XmlResult<()> {
    let tag = common
        .jcr_name
        .as_ref()
        .map(|n| n.as_str())
        .unwrap_or_else(|| common.name.as_str());
    let bind_ref = resolve_bind_ref(common, ctx);
    let css = css_value(common);
    let label = master_text(&field.label, ctx.master).unwrap_or(tag);
    let mandatory_message = field
        .mandatory_message
        .as_ref()
        .and_then(|m| master_text(m, ctx.master));
    let placeholder = field
        .placeholder
        .as_ref()
        .and_then(|p| master_text(p, ctx.master));
    let max_chars_value = max_chars.map(|n| n.get().to_string());

    let mut el = BytesStart::new(tag);
    el.push_attribute(("jcr:primaryType", "nt:unstructured"));
    push_common(&mut el, common, bind_ref.as_deref());
    if let Some(css) = css.as_deref() {
        el.push_attribute(("css", css));
    }
    el.push_attribute(("jcr:title", label));
    el.push_attribute(("mandatory", plain_bool(field.mandatory)));
    if let Some(message) = mandatory_message {
        el.push_attribute(("mandatoryMessage", message));
    }
    if let Some(placeholder) = placeholder {
        el.push_attribute(("placeholderText", placeholder));
    }
    el.push_attribute(("assistPriority", assist_priority_str(field.assist)));
    el.push_attribute(("multiLine", plain_bool(matches!(input, TextInput::MultiLine))));
    if let Some(max_chars) = max_chars_value.as_deref() {
        el.push_attribute(("maxChars", max_chars));
    }
    if let Some(autofill) = autofill {
        el.push_attribute(("autofillFieldKeyword", autofill_hint_str(autofill)));
    }
    if let Some(validation) = validation {
        el.push_attribute(("validatePictureClause", validation.pattern.as_str()));
    }
    push_presence(&mut el, &common.presence);
    push_passthrough_attributes(&mut el, &common.passthrough);
    w.write_event(Event::Start(el))?;
    write_responsive(w, layout)?;
    if let Some(validation) = validation
        && let Some(message) = validation.message.as_ref().and_then(|m| master_text(m, ctx.master))
    {
        write_validation_message(w, message)?;
    }
    w.write_event(Event::End(BytesEnd::new(tag)))
}

/// The message for a pattern-mismatch is written as its own `<fd:rules>`
/// script rather than a plain attribute -- AEM.md documents
/// `validatePictureClauseMessage` as a component attribute for date/number
/// fields specifically (§6.4, §6.5); for a free-text pattern, this crate
/// writes the equivalent as a validation rule instead of inventing a
/// third attribute name with no spec support either way.
fn write_validation_message(w: &mut Writer<Cursor<Vec<u8>>>, message: &str) -> XmlResult<()> {
    w.write_event(Event::Start(BytesStart::new("fd:rules")))?;
    let mut validate = BytesStart::new("fd:validate");
    validate.push_attribute(("jcr:primaryType", "nt:unstructured"));
    validate.push_attribute(("fdType", "validate"));
    validate.push_attribute(("jcr:title", message));
    w.write_event(Event::Empty(validate))?;
    w.write_event(Event::End(BytesEnd::new("fd:rules")))
}

fn assist_priority_str(assist: u2s_aem::model::AssistPriority) -> &'static str {
    match assist {
        u2s_aem::model::AssistPriority::Label => "label",
        u2s_aem::model::AssistPriority::Caption => "caption",
        u2s_aem::model::AssistPriority::Custom => "custom",
    }
}

fn autofill_hint_str(hint: &u2s_aem::model::AutofillHint) -> &'static str {
    use u2s_aem::model::AutofillHint::*;
    match hint {
        Name => "name",
        GivenName => "given-name",
        FamilyName => "family-name",
        Email => "email",
        Tel => "tel",
        StreetAddress => "street-address",
        AddressLine1 => "address-line1",
        AddressLine2 => "address-line2",
        PostalCode => "postal-code",
        Country => "country",
        Bday => "bday",
        Organization => "organization",
    }
}

// -------------------------------------------------------------- NumberField

fn write_number_field(
    w: &mut Writer<Cursor<Vec<u8>>>,
    common: &Common,
    field: &FieldCommon,
    layout: &FieldLayout,
    format: Option<&NumberFormat>,
    validation: Option<&u2s_aem::model::Validation>,
    ctx: &WriteCtx,
) -> XmlResult<()> {
    let tag = common
        .jcr_name
        .as_ref()
        .map(|n| n.as_str())
        .unwrap_or_else(|| common.name.as_str());
    let bind_ref = resolve_bind_ref(common, ctx);
    let css = css_value(common);
    let label = master_text(&field.label, ctx.master).unwrap_or(tag);
    let picture_clause = format.map(number_picture_clause);

    let mut el = BytesStart::new(tag);
    el.push_attribute(("jcr:primaryType", "nt:unstructured"));
    push_common(&mut el, common, bind_ref.as_deref());
    if let Some(css) = css.as_deref() {
        el.push_attribute(("css", css));
    }
    el.push_attribute(("jcr:title", label));
    el.push_attribute(("mandatory", plain_bool(field.mandatory)));
    el.push_attribute(("assistPriority", assist_priority_str(field.assist)));
    if let Some(clause) = picture_clause.as_deref() {
        el.push_attribute(("validatePictureClause", clause));
        el.push_attribute(("displayPictureClause", clause));
    }
    if let Some(validation) = validation {
        el.push_attribute(("validatePictureClause", validation.pattern.as_str()));
    }
    push_presence(&mut el, &common.presence);
    push_passthrough_attributes(&mut el, &common.passthrough);
    w.write_event(Event::Start(el))?;
    write_responsive(w, layout)?;
    w.write_event(Event::End(BytesEnd::new(tag)))
}

/// AEM.md §18.2's picture-clause alphabet (`9`, `Z`/`z`, `.`, `,`, `$`,
/// `%`, `S`, `s`, `(`, `)`, `C`, `R`, `c`, `r`, `-`), applied to
/// [`NamedNumberFormat`]'s four presets. There is no authoritative literal
/// for these presets either, so this is this crate's own defensible
/// translation, kept in this one function.
fn number_picture_clause(format: &NumberFormat) -> String {
    match format {
        NumberFormat::Named(named) => match named {
            NamedNumberFormat::Integer => "num{zzzzzzzzzz9}".to_owned(),
            NamedNumberFormat::Decimal => "num{zzzzzzzzzz9.99}".to_owned(),
            NamedNumberFormat::Currency => "num{$zzzzzzzzzz9.99}".to_owned(),
            NamedNumberFormat::Percent => "num{zzzzzzzzzz9%}".to_owned(),
        },
        NumberFormat::Pattern(pattern) => format!("num{{{}}}", pattern.as_str()),
    }
}

// -------------------------------------------------------------- DatePicker

#[allow(clippy::too_many_arguments)]
fn write_date_picker(
    w: &mut Writer<Cursor<Vec<u8>>>,
    common: &Common,
    field: &FieldCommon,
    layout: &FieldLayout,
    default_to_current_date: bool,
    format: Option<&DateFormat>,
    year_range: Option<&u2s_aem::model::YearRange>,
    validation: Option<&u2s_aem::model::Validation>,
    ctx: &WriteCtx,
) -> XmlResult<()> {
    let tag = common
        .jcr_name
        .as_ref()
        .map(|n| n.as_str())
        .unwrap_or_else(|| common.name.as_str());
    let bind_ref = resolve_bind_ref(common, ctx);
    let css = css_value(common);
    let label = master_text(&field.label, ctx.master).unwrap_or(tag);
    let picture_clause = format.map(date_picture_clause);

    let mut el = BytesStart::new(tag);
    el.push_attribute(("jcr:primaryType", "nt:unstructured"));
    push_common(&mut el, common, bind_ref.as_deref());
    if let Some(css) = css.as_deref() {
        el.push_attribute(("css", css));
    }
    el.push_attribute(("jcr:title", label));
    el.push_attribute(("mandatory", plain_bool(field.mandatory)));
    el.push_attribute(("assistPriority", assist_priority_str(field.assist)));
    el.push_attribute(("defaultToCurrentDate", plain_bool(default_to_current_date)));
    if let Some(clause) = picture_clause.as_deref() {
        el.push_attribute(("validatePictureClause", clause));
        el.push_attribute(("displayPictureClause", clause));
    }
    if let Some(validation) = validation {
        el.push_attribute(("validatePictureClause", validation.pattern.as_str()));
    }
    let before = year_range.map(|r| r.before_today.to_string());
    let after = year_range.map(|r| r.after_today.to_string());
    if let Some(before) = before.as_deref() {
        el.push_attribute(("yearRangeFrom", before));
    }
    if let Some(after) = after.as_deref() {
        el.push_attribute(("yearRangeTo", after));
    }
    push_presence(&mut el, &common.presence);
    push_passthrough_attributes(&mut el, &common.passthrough);
    w.write_event(Event::Start(el))?;
    write_responsive(w, layout)?;
    w.write_event(Event::End(BytesEnd::new(tag)))
}

/// See [`number_picture_clause`]'s note: no authoritative literal exists
/// for the named presets, so these are this crate's own translation
/// within AEM.md §18.1's documented alphabet (`D`, `M`, `Y`, `E`, and
/// separators).
fn date_picture_clause(format: &DateFormat) -> String {
    match format {
        DateFormat::Named(named) => match named {
            NamedDateFormat::Short => "date{MM/DD/YY}".to_owned(),
            NamedDateFormat::Medium => "date{MMM DD, YYYY}".to_owned(),
            NamedDateFormat::Long => "date{MMMM DD, YYYY}".to_owned(),
            NamedDateFormat::Full => "date{EEEE, MMMM DD, YYYY}".to_owned(),
        },
        DateFormat::Pattern(pattern) => format!("date{{{}}}", pattern.as_str()),
    }
}

// ---------------------------------------------------------------- Dropdown

#[allow(clippy::too_many_arguments)]
fn write_dropdown(
    w: &mut Writer<Cursor<Vec<u8>>>,
    common: &Common,
    field: &FieldCommon,
    layout: &FieldLayout,
    options: &u2s_aem::model::ChoiceOptions,
    filtering_allowed: bool,
    sort: Option<&SortOrder>,
    ctx: &WriteCtx,
) -> XmlResult<()> {
    let tag = common
        .jcr_name
        .as_ref()
        .map(|n| n.as_str())
        .unwrap_or_else(|| common.name.as_str());
    let bind_ref = resolve_bind_ref(common, ctx);
    let css = css_value(common);
    let label = master_text(&field.label, ctx.master).unwrap_or(tag);
    let options_value = options_attr(options, ctx.master);

    let mut el = BytesStart::new(tag);
    el.push_attribute(("jcr:primaryType", "nt:unstructured"));
    push_common(&mut el, common, bind_ref.as_deref());
    if let Some(css) = css.as_deref() {
        el.push_attribute(("css", css));
    }
    el.push_attribute(("jcr:title", label));
    el.push_attribute(("mandatory", plain_bool(field.mandatory)));
    el.push_attribute(("assistPriority", assist_priority_str(field.assist)));
    el.push_attribute(("options", options_value.as_str()));
    el.push_attribute(("filteringAllowed", plain_bool(filtering_allowed)));
    if let Some(sort) = sort {
        el.push_attribute((
            "sort",
            match sort {
                SortOrder::Ascending => "ascending",
                SortOrder::Descending => "descending",
            },
        ));
    }
    push_presence(&mut el, &common.presence);
    push_passthrough_attributes(&mut el, &common.passthrough);
    w.write_event(Event::Start(el))?;
    write_responsive(w, layout)?;
    w.write_event(Event::End(BytesEnd::new(tag)))
}

fn options_attr(options: &u2s_aem::model::ChoiceOptions, master: &Language) -> String {
    multi_value(options.options().iter().map(|option| {
        let label = master_text(&option.label, master).unwrap_or(option.value.as_str());
        option_pair(option.value.as_str(), label)
    }))
}

// ---------------------------------------------------------------- Checkbox

#[allow(clippy::too_many_arguments)]
fn write_checkbox(
    w: &mut Writer<Cursor<Vec<u8>>>,
    common: &Common,
    field: &LabelCommon,
    layout: &FieldLayout,
    options: &u2s_aem::model::ChoiceOptions,
    alignment: OptionAlignment,
    hide_title: bool,
    rich_text_options: bool,
    ctx: &WriteCtx,
) -> XmlResult<()> {
    let tag = common
        .jcr_name
        .as_ref()
        .map(|n| n.as_str())
        .unwrap_or_else(|| common.name.as_str());
    let bind_ref = resolve_bind_ref(common, ctx);
    let css = css_value(common);
    let label = master_text(&field.label, ctx.master).unwrap_or(tag);
    let options_value = options_attr(options, ctx.master);

    let mut el = BytesStart::new(tag);
    el.push_attribute(("jcr:primaryType", "nt:unstructured"));
    push_common(&mut el, common, bind_ref.as_deref());
    if let Some(css) = css.as_deref() {
        el.push_attribute(("css", css));
    }
    el.push_attribute(("jcr:title", label));
    el.push_attribute(("assistPriority", assist_priority_str(field.assist)));
    el.push_attribute(("options", options_value.as_str()));
    el.push_attribute((
        "alignment",
        match alignment {
            OptionAlignment::Horizontal => "horizontal",
            OptionAlignment::Vertical => "vertical",
        },
    ));
    el.push_attribute(("hideTitle", plain_bool(hide_title)));
    el.push_attribute(("richTextOptions", plain_bool(rich_text_options)));
    push_presence(&mut el, &common.presence);
    push_passthrough_attributes(&mut el, &common.passthrough);
    w.write_event(Event::Start(el))?;
    write_responsive(w, layout)?;
    w.write_event(Event::End(BytesEnd::new(tag)))
}

// ------------------------------------------------------------- RadioButton

#[allow(clippy::too_many_arguments)]
fn write_radio_button(
    w: &mut Writer<Cursor<Vec<u8>>>,
    common: &Common,
    field: &FieldCommon,
    layout: &FieldLayout,
    options: &u2s_aem::model::ChoiceOptions,
    alignment: OptionAlignment,
    rich_text_options: bool,
    ctx: &WriteCtx,
) -> XmlResult<()> {
    let tag = common
        .jcr_name
        .as_ref()
        .map(|n| n.as_str())
        .unwrap_or_else(|| common.name.as_str());
    let bind_ref = resolve_bind_ref(common, ctx);
    let css = css_value(common);
    let label = master_text(&field.label, ctx.master).unwrap_or(tag);
    let options_value = options_attr(options, ctx.master);

    let mut el = BytesStart::new(tag);
    el.push_attribute(("jcr:primaryType", "nt:unstructured"));
    push_common(&mut el, common, bind_ref.as_deref());
    if let Some(css) = css.as_deref() {
        el.push_attribute(("css", css));
    }
    el.push_attribute(("jcr:title", label));
    el.push_attribute(("mandatory", plain_bool(field.mandatory)));
    el.push_attribute(("assistPriority", assist_priority_str(field.assist)));
    el.push_attribute(("options", options_value.as_str()));
    el.push_attribute((
        "alignment",
        match alignment {
            OptionAlignment::Horizontal => "horizontal",
            OptionAlignment::Vertical => "vertical",
        },
    ));
    el.push_attribute(("richTextOptions", plain_bool(rich_text_options)));
    push_presence(&mut el, &common.presence);
    push_passthrough_attributes(&mut el, &common.passthrough);
    w.write_event(Event::Start(el))?;
    write_responsive(w, layout)?;
    w.write_event(Event::End(BytesEnd::new(tag)))
}

// -------------------------------------------------------------- StaticText

fn write_static_text(
    w: &mut Writer<Cursor<Vec<u8>>>,
    common: &Common,
    layout: &FieldLayout,
    content: &u2s_aem::model::I18nRichText,
    heading_level: Option<&HeadingLevel>,
    ctx: &WriteCtx,
) -> XmlResult<()> {
    let tag = common
        .jcr_name
        .as_ref()
        .map(|n| n.as_str())
        .unwrap_or_else(|| common.name.as_str());
    let css = css_value(common);
    // No `bindRef`: static text carries no value (`crate::xsd`'s module
    // doc).
    let value = content
        .get(ctx.master)
        .map(|t| t.as_str())
        .unwrap_or_default();

    let mut el = BytesStart::new(tag);
    el.push_attribute(("jcr:primaryType", "nt:unstructured"));
    push_common(&mut el, common, None);
    if let Some(css) = css.as_deref() {
        el.push_attribute(("css", css));
    }
    el.push_attribute(("textIsRich", "true"));
    // Quick-xml escapes attribute values on write, so `<h2>...</h2>`
    // becomes `&lt;h2&gt;...&lt;/h2&gt;` exactly once here -- never
    // pre-escaped by this function, which would double-escape it.
    el.push_attribute(("_value", value));
    if let Some(level) = heading_level {
        el.push_attribute(("headingLevel", heading_level_str(level)));
    }
    push_presence(&mut el, &common.presence);
    push_passthrough_attributes(&mut el, &common.passthrough);
    w.write_event(Event::Start(el))?;
    write_responsive(w, layout)?;
    w.write_event(Event::End(BytesEnd::new(tag)))
}

// AEM's own `headingLevel` attribute is a plain heading-tag number
// ("2" for an `<h2>`), not an "H"-prefixed spelling -- confirmed against
// the real `AF_AABF.zip` fixture (`headingLevel="2"`/"3"/"4") and the
// reference engine's own parser (`core/src/aem/parser.rs::convert_titledraw`,
// `node.attr("headingLevel").and_then(|v| v.parse().ok())`).
fn heading_level_str(level: &HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "1",
        HeadingLevel::H2 => "2",
        HeadingLevel::H3 => "3",
        HeadingLevel::H4 => "4",
        HeadingLevel::H5 => "5",
        HeadingLevel::H6 => "6",
    }
}

// --------------------------------------------------------------- Signature

fn write_signature(
    w: &mut Writer<Cursor<Vec<u8>>>,
    common: &Common,
    field: &FieldCommon,
    layout: &FieldLayout,
    ctx: &WriteCtx,
) -> XmlResult<()> {
    let tag = common
        .jcr_name
        .as_ref()
        .map(|n| n.as_str())
        .unwrap_or_else(|| common.name.as_str());
    let bind_ref = resolve_bind_ref(common, ctx);
    let label = master_text(&field.label, ctx.master).unwrap_or(tag);

    let mut el = BytesStart::new(tag);
    el.push_attribute(("jcr:primaryType", "nt:unstructured"));
    push_common(&mut el, common, bind_ref.as_deref());
    el.push_attribute(("jcr:title", label));
    el.push_attribute(("mandatory", plain_bool(field.mandatory)));
    push_presence(&mut el, &common.presence);
    push_passthrough_attributes(&mut el, &common.passthrough);
    w.write_event(Event::Start(el))?;
    write_responsive(w, layout)?;
    w.write_event(Event::End(BytesEnd::new(tag)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    fn ctx_for<'a>(master: &'a Language, bind_refs: &'a HashMap<ComponentName, String>) -> WriteCtx<'a> {
        WriteCtx { master, bind_refs }
    }

    #[test]
    fn a_text_field_carries_its_bind_ref_and_label() {
        let form = build_form(xml_schema("Root"), vec![a_page_named("Page1", vec![a_text_field("Name")])]);
        let xsd = crate::xsd::generate(&form);
        let master = lang("en");
        let ctx = ctx_for(&master, &xsd.bind_refs);
        let xml = write_form_xml(&form, &ctx).expect("writes");
        assert!(xml.contains(r#"guideNodeClass="guideTextBox""#));
        assert!(xml.contains(r#"bindRef="/Root/Name""#));
        assert!(xml.contains(r#"jcr:title="Name""#));
    }

    /// `a_repeatable_expands_into_outer_inner_and_both_buttons` used to
    /// exercise `write_repeatable`'s own hardcoded outer/inner/button
    /// expansion. There is no such function any more (see this module's
    /// own doc): a repeatable is two ordinary `Component` panels the
    /// Conversion Agent authors directly. This test now proves the writer
    /// faithfully reproduces exactly that agent-authored shape, rather
    /// than proving the writer invents it.
    #[test]
    fn an_agent_authored_repeatable_shape_writes_faithfully() {
        let inner = a_component(
            "OwnersInstance",
            "fd/af/components/panel",
            vec![
                a_text_field("OwnerName"),
                a_component("RemoveOwner", "fd/af/components/controls/removebutton", Vec::new()),
            ],
        );
        let outer = a_component(
            "Owners",
            "fd/af/components/panel",
            vec![
                inner,
                a_component("AddOwner", "fd/af/components/controls/tertiarybutton", Vec::new()),
            ],
        );
        let form = build_form(xml_schema("Root"), vec![a_page_named("Page1", vec![outer])]);
        let xsd = crate::xsd::generate(&form);
        let master = lang("en");
        let ctx = ctx_for(&master, &xsd.bind_refs);
        let xml = write_form_xml(&form, &ctx).expect("writes");
        assert!(xml.contains("<Owners "));
        assert!(xml.contains("<OwnersInstance "));
        assert!(xml.contains(r#"sling:resourceType="fd/af/components/controls/removebutton""#));
        assert!(xml.contains(r#"sling:resourceType="fd/af/components/controls/tertiarybutton""#));
    }

    #[test]
    fn static_text_escapes_markup_exactly_once() {
        let form = build_form(
            u2s_aem::model::DataModel::Unbound,
            vec![a_page_named("Page1", vec![a_static_text("Intro")])],
        );
        let xsd = crate::xsd::generate(&form);
        let master = lang("en");
        let ctx = ctx_for(&master, &xsd.bind_refs);
        let xml = write_form_xml(&form, &ctx).expect("writes");
        assert!(
            xml.contains("&lt;p&gt;Intro&lt;/p&gt;"),
            "escaped exactly once: {xml}"
        );
        assert!(
            !xml.contains("&amp;lt;"),
            "must not be escaped twice: {xml}"
        );
    }

    /// `a_visibility_rule_becomes_an_fd_initialize_script` used to exercise
    /// `write_visibility_rule`'s own synthesized script -- a shape AEM.md
    /// never documented and the real corpus never used (see this module's
    /// own doc). Visibility is now ordinary agent-authored `fd:rules`/
    /// `fd:visible` content; this test proves that content, authored as a
    /// `Component` child the same way any other structural content is,
    /// writes through faithfully.
    #[test]
    fn agent_authored_visibility_content_writes_faithfully() {
        let trigger = a_dropdown("Trigger", &[("a", "A"), ("b", "B")]);
        let mut visible_rule = a_component("VisibleRule", "placeholder", Vec::new());
        if let Node::Component { common, properties, .. } = &mut visible_rule {
            common.jcr_name = Some(jcr_name("fd:visible"));
            common.resource_type = None;
            properties.insert(jcr_name("trigger"), JcrValue::Single("Trigger".to_owned()));
            properties.insert(jcr_name("value"), JcrValue::Single("a".to_owned()));
        }
        let mut rules = a_component("VisibilityRules", "placeholder", vec![visible_rule]);
        if let Node::Component { common, .. } = &mut rules {
            common.jcr_name = Some(jcr_name("fd:rules"));
            common.resource_type = None;
        }
        let panel = a_component(
            "Conditional",
            "fd/af/components/panel",
            vec![rules, a_text_field("Inner")],
        );
        let page = a_page_named("Page1", vec![trigger, panel]);
        let form = build_form(u2s_aem::model::DataModel::Unbound, vec![page]);
        let xsd = crate::xsd::generate(&form);
        let master = lang("en");
        let ctx = ctx_for(&master, &xsd.bind_refs);
        let xml = write_form_xml(&form, &ctx).expect("writes");
        assert!(xml.contains("<fd:rules"));
        assert!(xml.contains("<fd:visible"));
        assert!(xml.contains(r#"trigger="Trigger""#));
        assert!(xml.contains(r#"value="a""#));
    }

    #[test]
    fn the_toolbar_renders_every_action_in_order() {
        let form = build_form_with_toolbar(
            xml_schema("Root"),
            vec![a_page_named("Page1", vec![a_text_field("Name")])],
            vec![
                a_component("previtemnav", "fd/af/components/actions/previtemnav", Vec::new()),
                a_component("nextitemnav", "fd/af/components/actions/nextitemnav", Vec::new()),
                a_component("submit", "fd/af/components/actions/submit", Vec::new()),
            ],
        );
        let xsd = crate::xsd::generate(&form);
        let master = lang("en");
        let ctx = ctx_for(&master, &xsd.bind_refs);
        let xml = write_form_xml(&form, &ctx).expect("writes");
        let prev = xml.find("previtemnav").expect("previous action present");
        let next = xml.find("nextitemnav").expect("next action present");
        let submit = xml.find("submit").expect("submit action present");
        assert!(prev < next && next < submit, "actions must render in declared order");
    }
}
