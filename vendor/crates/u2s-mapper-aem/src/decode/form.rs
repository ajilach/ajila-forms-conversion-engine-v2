//! `JcrNode` -> `u2s_aem::model::AemForm` lowering: the inverse of
//! `crate::xml_writer`/`crate::xsd`.
//!
//! One function per typed leaf kind (mirroring `xml_writer`'s own one
//! function per kind), recognised by a `sling:resourceType` *suffix*
//! match (AEM.md §14's own overlay-component convention -- see
//! [`decode_node`]'s own doc) rather than exact equality, so a real
//! package's own customer-prefixed controls
//! (`ajila-forms-customers/ajila-forms-ubs/components/controls/textbox`,
//! say) are recognised the same as this crate's own
//! `fd/af/components/controls/textbox`. A resource type that matches by
//! name but not by *shape* (missing `cq:responsive`, say) falls back to
//! [`Node::Component`] rather than failing the whole decode -- see
//! [`decode_node`]. Anything that matches neither becomes `Component`
//! directly -- see `u2s_aem::model::Node`'s own doc on why that is the
//! right fallback rather than an error.
//!
//! **Scope of this pass.** Built first to close the loop on this crate's
//! own `encode` output (build a form, encode it, decode the package, get
//! the same form back), then extended to decode the real fixture package
//! (`tests/fixtures/AF_AABF.zip`) -- see `decode/mod.rs`'s own doc and the
//! design plan's own "still needs" list for what real-package fidelity
//! this pass does not yet have (DAM/DoR decode, the UBS-specific
//! `summarypanel` components, `canonical.rs`).

use std::collections::BTreeMap;

use u2s_aem::model::{
    AssistPriority, AutofillHint, ChoiceOption, ChoiceOptions, Common, ComponentName, CssClass,
    CssClasses, DateFormat, FieldCommon, FieldLayout, HeadingLevel, I18nRichText, I18nText,
    JcrName, JcrValue, Language, LabelCommon, Node, OptionAlignment, OptionValue,
    Page, Passthrough, Presence, RawJcrNode, ResourceType, RichText, SortOrder, TextInput,
    Validation,
};

use crate::i18n::Dictionary;
use crate::jcr::tree::JcrNode;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("{path}: missing required attribute {attribute:?}")]
    MissingAttribute { path: String, attribute: &'static str },
    #[error("{path}: invalid {attribute:?}: {source}")]
    InvalidAttribute {
        path: String,
        attribute: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("{path}: {message}")]
    Invalid { path: String, message: String },
}

fn invalid_attr(
    path: &str,
    attribute: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> DecodeError {
    DecodeError::InvalidAttribute {
        path: path.to_owned(),
        attribute,
        source: Box::new(source),
    }
}

/// Everything a node decoder needs beyond the node itself -- the mirror of
/// `xml_writer::WriteCtx`.
pub struct DecodeCtx<'a> {
    pub master: &'a Language,
    pub dictionaries: &'a BTreeMap<Language, Dictionary>,
}

impl DecodeCtx<'_> {
    /// The exact inverse of `xml_writer`'s own dictionary derivation: the
    /// inline value is the master language's own text; every other
    /// configured language's text is whatever that language's dictionary
    /// has keyed by the *master* value (never the other way around --
    /// `crate::i18n::collect_text` always keys by `master_value`).
    fn resolve_text(&self, inline_value: &str) -> BTreeMap<Language, String> {
        let mut out = BTreeMap::new();
        out.insert(self.master.clone(), inline_value.to_owned());
        for (language, dictionary) in self.dictionaries {
            if language == self.master {
                continue;
            }
            if let Some(translated) = dictionary.get(inline_value) {
                out.insert(language.clone(), translated.clone());
            }
        }
        out
    }

    pub(crate) fn resolve_i18n_text(&self, path: &str, inline_value: &str) -> Result<I18nText, DecodeError> {
        let mut entries = Vec::new();
        for (language, text) in self.resolve_text(inline_value) {
            let plain = u2s_aem::model::PlainText::try_from(text)
                .map_err(|e| invalid_attr(path, "(i18n text)", e))?;
            entries.push((language, plain));
        }
        Ok(I18nText::from_entries(entries))
    }

    fn resolve_i18n_rich_text(&self, path: &str, inline_value: &str) -> Result<I18nRichText, DecodeError> {
        let mut entries = Vec::new();
        for (language, text) in self.resolve_text(inline_value) {
            let rich = RichText::try_from(text).map_err(|e| invalid_attr(path, "(i18n rich text)", e))?;
            entries.push((language, rich));
        }
        Ok(I18nRichText::from_entries(entries))
    }
}

/// A node's attributes, taken one at a time as each field recognises its
/// own -- whatever is left when a decoder is done becomes
/// [`Passthrough::raw_attributes`]. Built once per node so "did we already
/// take this key" is never duplicated logic between a decoder and
/// `Common`'s own extraction.
struct AttrPool(BTreeMap<String, String>);

impl AttrPool {
    fn new(node: &JcrNode) -> Self {
        Self(node.attributes.iter().cloned().collect())
    }

    fn take(&mut self, key: &str) -> Option<String> {
        self.0.remove(key)
    }

    fn into_remaining(self) -> BTreeMap<String, String> {
        self.0
    }
}

/// The exact inverse of `crate::xml_writer::write_raw_node`: captures a
/// JCR element this pass does not model, verbatim, so it can be replayed
/// unchanged.
pub(crate) fn to_raw_node(node: &JcrNode) -> RawJcrNode {
    RawJcrNode {
        tag_name: node.tag_name.clone(),
        attributes: node.attributes.iter().cloned().collect(),
        children: node.children.iter().map(to_raw_node).collect(),
    }
}

/// `Common`'s own attributes only -- name, resource type, guide node
/// class, the JCR element name when it differs from `name`, visible/
/// enabled, css, bind ref, and the four presence flags. Child handling
/// (the `items` wrapper, everything else) is each caller's own job, since
/// only a [`Node::Component`] has a typed `children: Vec<Node>` to put an
/// `items` child's own children into -- see this module's own doc.
fn decode_common_attrs(path: &str, node: &JcrNode) -> Result<Common, DecodeError> {
    let mut pool = AttrPool::new(node);

    // Always the same fixed value on every element this crate's own
    // writer produces (`jcr:primaryType="nt:unstructured"`, mechanical
    // JCR boilerplate, never a per-node choice -- see
    // `crate::xml_writer`'s own module doc on the syntax/judgment line).
    // Discarded, not carried: `write_component`/every leaf writer
    // re-adds it unconditionally regardless of what passthrough says, so
    // carrying it would only ever produce a value encode ignores anyway.
    pool.take("jcr:primaryType");

    let name_raw = pool
        .take("name")
        .ok_or_else(|| DecodeError::MissingAttribute { path: path.to_owned(), attribute: "name" })?;
    let name = ComponentName::try_from(name_raw).map_err(|e| invalid_attr(path, "name", e))?;

    let jcr_name = if node.tag_name != name.as_str() {
        Some(JcrName::try_from(node.tag_name.clone()).map_err(|e| invalid_attr(path, "(element name)", e))?)
    } else {
        None
    };

    let resource_type = pool
        .take("sling:resourceType")
        .map(ResourceType::try_from)
        .transpose()
        .map_err(|e| invalid_attr(path, "sling:resourceType", e))?;
    let guide_node_class = pool
        .take("guideNodeClass")
        .map(JcrName::try_from)
        .transpose()
        .map_err(|e| invalid_attr(path, "guideNodeClass", e))?;

    let visible = pool.take("visible").map(|v| !v.contains("false")).unwrap_or(true);
    let enabled = pool.take("enabled").map(|v| v.contains("true")).unwrap_or(true);

    let css = match pool.take("css") {
        Some(value) => value
            .split_whitespace()
            .map(CssClass::try_from)
            .collect::<Result<CssClasses, _>>()
            .map_err(|e| invalid_attr(path, "css", e))?,
        None => CssClasses::default(),
    };

    let bind_ref = pool.take("bindRef");

    let dor_exclusion = pool.take("dorExclusion").map(|v| v.contains("true")).unwrap_or(false);
    let dor_exclude_title = pool.take("dorExcludeTitle").map(|v| v == "true").unwrap_or(false);
    let dor_exclude_description =
        pool.take("dorExcludeDescription").map(|v| v == "true").unwrap_or(false);
    let summary_exclusion = pool.take("summaryExclusion").map(|v| v.contains("true")).unwrap_or(false);

    Ok(Common {
        name,
        resource_type,
        guide_node_class,
        jcr_name,
        visible,
        enabled,
        css,
        presence: Presence {
            dor_exclusion,
            dor_exclude_title,
            dor_exclude_description,
            summary_exclusion,
        },
        bind_ref,
        passthrough: Passthrough {
            raw_attributes: pool.into_remaining(),
            raw_children: Vec::new(),
        },
    })
}

/// Splits a node's children into "the `items` wrapper's own children" (what
/// a [`Node::Component`]'s typed `children: Vec<Node>` decodes from -- the
/// exact inverse of `write_component`'s own auto-generated wrapper) and
/// "everything else", verbatim-captured as [`RawJcrNode`]s for
/// [`Passthrough::raw_children`]. `layout`/`cq:responsive` are excluded
/// from the second group entirely for a `Component` (see
/// `JcrNode::passthrough_children`): a `Component` never had a typed field
/// that would have produced either, so a real package's own use of them
/// there is exactly the kind of remainder passthrough exists to carry —
/// but through `passthrough_children`'s own filter, which additionally
/// excludes `items` (already handled separately here).
fn split_component_children(node: &JcrNode) -> (&[JcrNode], Vec<RawJcrNode>) {
    let items_children: &[JcrNode] = node.child("items").map(|items| items.children.as_slice()).unwrap_or(&[]);
    let raw_children = node
        .passthrough_children()
        .filter(|child| child.tag_name != "items")
        .map(to_raw_node)
        .collect();
    (items_children, raw_children)
}

/// Decodes a node as a generic [`Node::Component`] -- the fallback for
/// anything the resource-type recognisers in [`decode_node`] did not
/// match. Every attribute becomes a `properties` entry (promoted to
/// [`JcrValue::Text`] when its value is also a dictionary key in at least
/// one configured language, so a `Component`'s own translatable content
/// round-trips the same way a typed leaf kind's fields do -- see
/// `u2s_aem::model::JcrValue`'s own doc), since `Component` has no typed
/// field of its own to claim any of them first.
fn decode_component(path: &str, node: &JcrNode, ctx: &DecodeCtx) -> Result<Node, DecodeError> {
    let mut common = decode_common_attrs(path, node)?;
    let raw_properties = std::mem::take(&mut common.passthrough.raw_attributes);

    let mut properties = BTreeMap::new();
    for (key, value) in raw_properties {
        let jcr_key = JcrName::try_from(key.clone()).map_err(|e| invalid_attr(path, "(property name)", e))?;
        let resolved = ctx.resolve_text(&value);
        let jcr_value = if resolved.len() > 1 {
            // Try `PlainText` first (the common case), then `RichText` --
            // a `Component`'s own translatable property is just as likely
            // to carry markup as a typed leaf kind's rich-text field (a
            // `titledraw`/`textdraw`'s `_value` that fell back to
            // `Component` for an unrelated reason, say). Falling straight
            // to `Single` on the first parse failure, as this used to,
            // silently dropped every markup-bearing property's
            // translations -- verified against the real fixture, where
            // this was the dominant cause of lost dictionary entries.
            match as_text_value(&resolved) {
                Some(text_value) => text_value,
                None => JcrValue::Single(value),
            }
        } else {
            JcrValue::Single(value)
        };
        properties.entry(jcr_key).or_insert(jcr_value);
    }

    let (items_children, raw_children) = split_component_children(node);
    common.passthrough.raw_children = raw_children;

    let mut children = Vec::new();
    for (index, child) in items_children.iter().enumerate() {
        children.push(decode_node(&format!("{path}/children/{index}"), child, ctx)?);
    }

    Ok(Node::Component { common, properties, children })
}

/// Builds a [`JcrValue::Text`] or [`JcrValue::RichText`] from a resolved
/// per-language map, whichever the values actually parse as -- `None` if
/// neither does, so the caller can fall back to an untranslated `Single`
/// rather than fail the whole decode over one property.
fn as_text_value(resolved: &BTreeMap<Language, String>) -> Option<JcrValue> {
    let plain: Option<Vec<_>> = resolved
        .iter()
        .map(|(lang, text)| {
            u2s_aem::model::PlainText::try_from(text.clone())
                .ok()
                .map(|p| (lang.clone(), p))
        })
        .collect();
    if let Some(entries) = plain {
        return Some(JcrValue::Text(I18nText::from_entries(entries)));
    }
    let rich: Option<Vec<_>> = resolved
        .iter()
        .map(|(lang, text)| RichText::try_from(text.clone()).ok().map(|r| (lang.clone(), r)))
        .collect();
    rich.map(|entries| JcrValue::RichText(I18nRichText::from_entries(entries)))
}

/// Recognises a node by its `sling:resourceType` (checked without
/// consuming it -- the matched decoder still takes it as part of its own
/// `Common` extraction) and dispatches to the matching typed decoder, or
/// [`decode_component`] when nothing matches. See this module's own doc
/// on the scope of what is recognised today.
pub fn decode_node(path: &str, node: &JcrNode, ctx: &DecodeCtx) -> Result<Node, DecodeError> {
    // Recognition is by resource-type *suffix*, not exact equality: AEM.md
    // §14's own "Custom / Overlay Components" documents that a project may
    // overlay a foundation component under its own prefix
    // (`<project>/components/controls/textbox` is semantically
    // `fd/af/components/controls/textbox`) -- confirmed against the real
    // fixture, whose own leaf controls all use the
    // `ajila-forms-customers/ajila-forms-ubs/...` prefix. A suffix match
    // is classification, not authorship: getting it wrong only ever costs
    // falling back to `Component` (see below), never a wrong write, so
    // there is no harm in matching generously here.
    let recognised = match node.resource_type() {
        Some(rt) if rt.ends_with("controls/textbox") => Some(decode_text_field(path, node, ctx)),
        Some(rt) if rt.ends_with("controls/numericbox") => Some(decode_number_field(path, node, ctx)),
        Some(rt) if rt.ends_with("controls/datepicker") => Some(decode_date_picker(path, node, ctx)),
        Some(rt) if rt.ends_with("controls/dropdownlist") => Some(decode_dropdown(path, node, ctx)),
        Some(rt) if rt.ends_with("controls/checkbox") => Some(decode_checkbox(path, node, ctx)),
        Some(rt) if rt.ends_with("controls/radiobutton") => Some(decode_radio_button(path, node, ctx)),
        Some(rt) if rt.ends_with("controls/scribble") => Some(decode_signature(path, node, ctx)),
        Some(rt) if rt.ends_with("controls/textdraw") || rt.ends_with("controls/titledraw") => {
            Some(decode_static_text(path, node, ctx))
        }
        _ => None,
    };
    // A resource type matching a typed kind's *name* does not guarantee
    // the node also matches its *shape* (a real package's own component
    // can lack `cq:responsive`, say, if a customer overlay adds fields
    // this pass has no typed slot for). Falling back to `Component` on
    // that failure, rather than propagating a hard error, is what makes
    // decode lossless over a real package instead of merely over this
    // crate's own encoder output: an unrecognised *shape* is exactly what
    // `Component` exists for, the same as an unrecognised resource type.
    match recognised {
        Some(Ok(decoded)) => Ok(decoded),
        Some(Err(_)) | None => decode_component(path, node, ctx),
    }
}

fn decode_responsive(path: &str, node: &JcrNode) -> Result<FieldLayout, DecodeError> {
    let responsive = node.child("cq:responsive").ok_or_else(|| DecodeError::Invalid {
        path: path.to_owned(),
        message: "a leaf field must carry cq:responsive".to_owned(),
    })?;
    let default = responsive.child("default").ok_or_else(|| DecodeError::Invalid {
        path: path.to_owned(),
        message: "cq:responsive must carry a default breakpoint".to_owned(),
    })?;
    let width = default
        .attr("width")
        .ok_or_else(|| DecodeError::MissingAttribute { path: path.to_owned(), attribute: "width" })?
        .parse::<u8>()
        .map_err(|e| invalid_attr(path, "width", e))?;
    let width = u2s_aem::model::ColSpan::try_from(width).map_err(|e| invalid_attr(path, "width", e))?;
    let offset = default
        .attr("offset")
        .and_then(|s| s.parse::<u8>().ok())
        .filter(|&o| o > 0)
        .map(u2s_aem::model::ColSpan::try_from)
        .transpose()
        .map_err(|e| invalid_attr(path, "offset", e))?;
    Ok(FieldLayout { width, offset })
}

fn decode_field_common(
    path: &str,
    pool: &mut AttrPool,
    ctx: &DecodeCtx,
    label_attr: &str,
    mandatory: bool,
) -> Result<FieldCommon, DecodeError> {
    let label_raw = pool
        .take(label_attr)
        .ok_or_else(|| DecodeError::MissingAttribute { path: path.to_owned(), attribute: "jcr:title" })?;
    let label = ctx.resolve_i18n_text(path, &label_raw)?;
    let mandatory_message = pool
        .take("mandatoryMessage")
        .map(|v| ctx.resolve_i18n_text(path, &v))
        .transpose()?;
    let placeholder = pool
        .take("placeholderText")
        .map(|v| ctx.resolve_i18n_text(path, &v))
        .transpose()?;
    let assist = decode_assist_priority(pool.take("assistPriority").as_deref());
    let _ = mandatory;
    Ok(FieldCommon { label, mandatory, mandatory_message, placeholder, assist })
}

fn decode_assist_priority(value: Option<&str>) -> AssistPriority {
    match value {
        Some("caption") => AssistPriority::Caption,
        Some("custom") => AssistPriority::Custom,
        _ => AssistPriority::Label,
    }
}

fn finish_leaf(node: &JcrNode, mut common: Common, pool: AttrPool) -> Result<Common, DecodeError> {
    common.passthrough.raw_attributes = pool.into_remaining();
    // A leaf field has no typed `children: Vec<Node>`; any non-layout
    // child a real package's own leaf carries (rare) is carried verbatim.
    common.passthrough.raw_children = node
        .passthrough_children()
        .filter(|c| c.tag_name != "cq:responsive")
        .map(to_raw_node)
        .collect();
    Ok(common)
}

fn decode_text_field(path: &str, node: &JcrNode, ctx: &DecodeCtx) -> Result<Node, DecodeError> {
    let common = decode_common_attrs(path, node)?;
    let mut pool = AttrPool(common.passthrough.raw_attributes.clone());
    let mandatory = pool.take("mandatory").map(|v| v == "true").unwrap_or(false);
    let field = decode_field_common(path, &mut pool, ctx, "jcr:title", mandatory)?;
    let input = match pool.take("multiLine").as_deref() {
        Some("true") => TextInput::MultiLine,
        _ => TextInput::SingleLine,
    };
    let max_chars = pool
        .take("maxChars")
        .map(|v| v.parse::<u32>())
        .transpose()
        .map_err(|e| invalid_attr(path, "maxChars", e))?
        .and_then(std::num::NonZeroU32::new);
    let autofill = pool.take("autofillFieldKeyword").map(|v| decode_autofill_hint(path, &v)).transpose()?;
    let validation = pool
        .take("validatePictureClause")
        .map(u2s_aem::model::TextPattern::try_from)
        .transpose()
        .map_err(|e| invalid_attr(path, "validatePictureClause", e))?
        .map(|pattern| Validation { pattern, message: None });
    let layout = decode_responsive(path, node)?;
    let common = finish_leaf(node, common, pool)?;
    Ok(Node::TextField { common, field, layout, input, max_chars, autofill, validation })
}

fn decode_autofill_hint(path: &str, value: &str) -> Result<AutofillHint, DecodeError> {
    Ok(match value {
        "name" => AutofillHint::Name,
        "given-name" => AutofillHint::GivenName,
        "family-name" => AutofillHint::FamilyName,
        "email" => AutofillHint::Email,
        "tel" => AutofillHint::Tel,
        "street-address" => AutofillHint::StreetAddress,
        "address-line1" => AutofillHint::AddressLine1,
        "address-line2" => AutofillHint::AddressLine2,
        "postal-code" => AutofillHint::PostalCode,
        "country" => AutofillHint::Country,
        "bday" => AutofillHint::Bday,
        "organization" => AutofillHint::Organization,
        other => {
            return Err(DecodeError::Invalid {
                path: path.to_owned(),
                message: format!("unrecognised autofillFieldKeyword {other:?}"),
            });
        }
    })
}

fn decode_number_field(path: &str, node: &JcrNode, ctx: &DecodeCtx) -> Result<Node, DecodeError> {
    let common = decode_common_attrs(path, node)?;
    let mut pool = AttrPool(common.passthrough.raw_attributes.clone());
    let mandatory = pool.take("mandatory").map(|v| v == "true").unwrap_or(false);
    let field = decode_field_common(path, &mut pool, ctx, "jcr:title", mandatory)?;
    // `displayPictureClause` is always identical to `validatePictureClause`
    // when the writer derives one from a `NumberFormat` -- consumed here
    // so it never becomes stray passthrough, but not independently
    // decoded back into a `NumberFormat`: this pass carries the raw
    // `validatePictureClause` string on `Validation` instead (see below),
    // matching what a hand-authored `validation.pattern` already produces
    // on the encode side.
    pool.take("displayPictureClause");
    let validation = pool
        .take("validatePictureClause")
        .map(u2s_aem::model::TextPattern::try_from)
        .transpose()
        .map_err(|e| invalid_attr(path, "validatePictureClause", e))?
        .map(|pattern| Validation { pattern, message: None });
    let layout = decode_responsive(path, node)?;
    let common = finish_leaf(node, common, pool)?;
    Ok(Node::NumberField { common, field, layout, format: None, validation })
}

fn decode_date_picker(path: &str, node: &JcrNode, ctx: &DecodeCtx) -> Result<Node, DecodeError> {
    let common = decode_common_attrs(path, node)?;
    let mut pool = AttrPool(common.passthrough.raw_attributes.clone());
    let mandatory = pool.take("mandatory").map(|v| v == "true").unwrap_or(false);
    let field = decode_field_common(path, &mut pool, ctx, "jcr:title", mandatory)?;
    let default_to_current_date = pool.take("defaultToCurrentDate").map(|v| v == "true").unwrap_or(false);
    pool.take("displayPictureClause");
    let validation = pool
        .take("validatePictureClause")
        .map(u2s_aem::model::TextPattern::try_from)
        .transpose()
        .map_err(|e| invalid_attr(path, "validatePictureClause", e))?
        .map(|pattern| Validation { pattern, message: None });
    let before_today = pool.take("yearRangeFrom").map(|v| v.parse::<u16>()).transpose()
        .map_err(|e| invalid_attr(path, "yearRangeFrom", e))?;
    let after_today = pool.take("yearRangeTo").map(|v| v.parse::<u16>()).transpose()
        .map_err(|e| invalid_attr(path, "yearRangeTo", e))?;
    let year_range = match (before_today, after_today) {
        (Some(before_today), Some(after_today)) => Some(u2s_aem::model::YearRange { before_today, after_today }),
        _ => None,
    };
    let layout = decode_responsive(path, node)?;
    let common = finish_leaf(node, common, pool)?;
    Ok(Node::DatePicker {
        common,
        field,
        layout,
        default_to_current_date,
        format: None::<DateFormat>,
        year_range,
        validation,
    })
}

/// Parses `options="[value=label,...]"`, splitting each entry on the
/// *first* `=` -- the exact inverse of `crate::jcr::option_pair`, whose own
/// doc explains why a label may contain `=` and a value may not.
fn decode_options(path: &str, options_value: &str, ctx: &DecodeCtx) -> Result<ChoiceOptions, DecodeError> {
    let entries = crate::jcr::value::parse_options(&{
        let mut node = JcrNode::leaf("options-holder", []);
        node.attributes.push(("options".to_owned(), options_value.to_owned()));
        node
    });
    let mut options = Vec::new();
    for (value, label) in entries {
        let value = OptionValue::try_from(value).map_err(|e| invalid_attr(path, "options", e))?;
        let label = ctx.resolve_i18n_text(path, &label)?;
        options.push(ChoiceOption { value, label });
    }
    ChoiceOptions::try_from(options).map_err(|e| DecodeError::Invalid {
        path: path.to_owned(),
        message: e.to_string(),
    })
}

fn decode_dropdown(path: &str, node: &JcrNode, ctx: &DecodeCtx) -> Result<Node, DecodeError> {
    let common = decode_common_attrs(path, node)?;
    let mut pool = AttrPool(common.passthrough.raw_attributes.clone());
    let mandatory = pool.take("mandatory").map(|v| v == "true").unwrap_or(false);
    let field = decode_field_common(path, &mut pool, ctx, "jcr:title", mandatory)?;
    let options_value = pool
        .take("options")
        .ok_or_else(|| DecodeError::MissingAttribute { path: path.to_owned(), attribute: "options" })?;
    let options = decode_options(path, &options_value, ctx)?;
    let filtering_allowed = pool.take("filteringAllowed").map(|v| v == "true").unwrap_or(false);
    let sort = match pool.take("sort").as_deref() {
        Some("ascending") => Some(SortOrder::Ascending),
        Some("descending") => Some(SortOrder::Descending),
        _ => None,
    };
    let layout = decode_responsive(path, node)?;
    let common = finish_leaf(node, common, pool)?;
    Ok(Node::Dropdown { common, field, layout, options, filtering_allowed, sort })
}

fn decode_checkbox(path: &str, node: &JcrNode, ctx: &DecodeCtx) -> Result<Node, DecodeError> {
    let common = decode_common_attrs(path, node)?;
    let mut pool = AttrPool(common.passthrough.raw_attributes.clone());
    let label_raw = pool
        .take("jcr:title")
        .ok_or_else(|| DecodeError::MissingAttribute { path: path.to_owned(), attribute: "jcr:title" })?;
    let label = ctx.resolve_i18n_text(path, &label_raw)?;
    let placeholder = None; // the writer never emits one for Checkbox
    let assist = decode_assist_priority(pool.take("assistPriority").as_deref());
    let field = LabelCommon { label, placeholder, assist };
    let options_value = pool
        .take("options")
        .ok_or_else(|| DecodeError::MissingAttribute { path: path.to_owned(), attribute: "options" })?;
    let options = decode_options(path, &options_value, ctx)?;
    let alignment = decode_alignment(pool.take("alignment").as_deref());
    let hide_title = pool.take("hideTitle").map(|v| v == "true").unwrap_or(false);
    let rich_text_options = pool.take("richTextOptions").map(|v| v == "true").unwrap_or(false);
    let layout = decode_responsive(path, node)?;
    let common = finish_leaf(node, common, pool)?;
    Ok(Node::Checkbox { common, field, layout, options, alignment, hide_title, rich_text_options })
}

fn decode_alignment(value: Option<&str>) -> OptionAlignment {
    match value {
        Some("vertical") => OptionAlignment::Vertical,
        _ => OptionAlignment::Horizontal,
    }
}

fn decode_radio_button(path: &str, node: &JcrNode, ctx: &DecodeCtx) -> Result<Node, DecodeError> {
    let common = decode_common_attrs(path, node)?;
    let mut pool = AttrPool(common.passthrough.raw_attributes.clone());
    let mandatory = pool.take("mandatory").map(|v| v == "true").unwrap_or(false);
    let field = decode_field_common(path, &mut pool, ctx, "jcr:title", mandatory)?;
    let options_value = pool
        .take("options")
        .ok_or_else(|| DecodeError::MissingAttribute { path: path.to_owned(), attribute: "options" })?;
    let options = decode_options(path, &options_value, ctx)?;
    let alignment = decode_alignment(pool.take("alignment").as_deref());
    let rich_text_options = pool.take("richTextOptions").map(|v| v == "true").unwrap_or(false);
    let layout = decode_responsive(path, node)?;
    let common = finish_leaf(node, common, pool)?;
    Ok(Node::RadioButton { common, field, layout, options, alignment, rich_text_options })
}

fn decode_static_text(path: &str, node: &JcrNode, ctx: &DecodeCtx) -> Result<Node, DecodeError> {
    let common = decode_common_attrs(path, node)?;
    let mut pool = AttrPool(common.passthrough.raw_attributes.clone());
    pool.take("textIsRich");
    let value = pool.take("_value").unwrap_or_default();
    let content = ctx.resolve_i18n_rich_text(path, &value)?;
    let heading_level = pool.take("headingLevel").map(|v| decode_heading_level(path, &v)).transpose()?;
    let layout = decode_responsive(path, node)?;
    let common = finish_leaf(node, common, pool)?;
    Ok(Node::StaticText { common, layout, content, heading_level })
}

/// AEM's own `headingLevel` attribute is a plain heading-tag number
/// ("2" for an `<h2>`) -- confirmed against the real `AF_AABF.zip` fixture
/// (`headingLevel="2"`/"3"/"4") and the reference engine's own parser. The
/// "H1".."H6" spelling is also accepted for robustness (this crate's own
/// writer used it before this was verified against a real package; a
/// decoder should stay lenient about a spelling it once wrote itself).
fn decode_heading_level(path: &str, value: &str) -> Result<HeadingLevel, DecodeError> {
    Ok(match value {
        "1" | "H1" => HeadingLevel::H1,
        "2" | "H2" => HeadingLevel::H2,
        "3" | "H3" => HeadingLevel::H3,
        "4" | "H4" => HeadingLevel::H4,
        "5" | "H5" => HeadingLevel::H5,
        "6" | "H6" => HeadingLevel::H6,
        other => {
            return Err(DecodeError::Invalid {
                path: path.to_owned(),
                message: format!("unrecognised headingLevel {other:?}"),
            });
        }
    })
}

fn decode_signature(path: &str, node: &JcrNode, ctx: &DecodeCtx) -> Result<Node, DecodeError> {
    let common = decode_common_attrs(path, node)?;
    let mut pool = AttrPool(common.passthrough.raw_attributes.clone());
    let mandatory = pool.take("mandatory").map(|v| v == "true").unwrap_or(false);
    let field = decode_field_common(path, &mut pool, ctx, "jcr:title", mandatory)?;
    let layout = decode_responsive(path, node)?;
    let common = finish_leaf(node, common, pool)?;
    Ok(Node::Signature { common, field, layout })
}

/// A page is a wizard step, decoded from `rootPanel/items`'s own children
/// (see `crate::xml_writer::write_page`'s own doc). `properties` is
/// whatever attributes the encoder wrote for this step (`jcr:title`,
/// `panelSetType`, ...), decoded the same generic way a `Component`'s own
/// properties are.
pub fn decode_page(path: &str, node: &JcrNode, ctx: &DecodeCtx) -> Result<Page, DecodeError> {
    let common = decode_common_attrs(path, node)?;
    let raw_properties = common.passthrough.raw_attributes;
    let mut properties = BTreeMap::new();
    for (key, value) in raw_properties {
        let jcr_key = JcrName::try_from(key).map_err(|e| invalid_attr(path, "(property name)", e))?;
        let resolved = ctx.resolve_text(&value);
        let jcr_value = if resolved.len() > 1 {
            as_text_value(&resolved).unwrap_or(JcrValue::Single(value))
        } else {
            JcrValue::Single(value)
        };
        properties.insert(jcr_key, jcr_value);
    }

    let items = node.child("items").ok_or_else(|| DecodeError::Invalid {
        path: path.to_owned(),
        message: "a page must carry an items wrapper".to_owned(),
    })?;
    let mut children = Vec::new();
    for (index, child) in items.children.iter().enumerate() {
        children.push(decode_node(&format!("{path}/children/{index}"), child, ctx)?);
    }

    Ok(Page { name: common.name, properties, children })
}
