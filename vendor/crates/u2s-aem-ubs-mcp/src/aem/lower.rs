//! The UBS form, lowered to the generic AEM model and written by the generic
//! writer (`u2s_mapper_aem::xml_writer`).
//!
//! Every UBS component is a [`Node::Component`]: its element name, its UBS
//! attributes as properties, its fixed children (`layout`, `cq:responsive`,
//! `fd:rules`, `fd:scripts`, ...) as raw children around its `items`, and its
//! rules built as typed SCRIPTMODELs (`super::scripts`). The form's own
//! scaffold (the page's paragraph systems, the container's `layout`, the
//! `FormMetadata` and summary panels around the steps, the toolbar, the print
//! settings) is the form metadata's chrome. What the UBS profile decides is
//! here, in Rust; how it is spelled in XML (escaping, multi-values, element
//! order) is the generic writer's.
//!
//! A loaded package's own attributes and children ([`Passthrough`]) are kept:
//! a raw attribute the UBS component does not write itself is carried, one it
//! writes (its [`owned`] attributes) is the component's.

use std::collections::{BTreeMap, HashMap, HashSet};

use u2s_aem::model::{
    AemForm, Common, ComponentName, CssClasses, DataModel, DorMode, FormMetadata, FormName,
    JcrName, JcrValue, Language, NameScope, Node, Page, Passthrough as Chrome, Presence,
    RawJcrNode, ValidateOptions,
};
use u2s_mapper_aem::jcr::{multi_value, option_pair};
use u2s_mapper_aem::script::{EventScript, ScriptEvent};
use uuid::Uuid;

use super::xml_writer::{
    RenderIndex, alignment_str, collect_repeatable_names, condition_value_str, holds_input,
    repeat_panel_name, repeat_row_name,
};
use super::{AemAttrs, AemConfig, AemI18nText, AemNode, AemOption, Passthrough};

/// What a lowering refuses: a node the generic model cannot hold (a name it
/// does not accept, say), or a form the generic writer will not spell.
pub type LowerError = String;

/// The timestamps every node carries; the package is a function of the
/// document, so they are fixed.
const CREATED: &str = "{Date}2025-01-01T00:00:00.000Z";
const MODIFIED_LOCAL: &str = "{Date}2025-01-01T00:00:00.000+01:00";

/// The profile variables the lowering writes: a profile without one of them
/// cannot write a UBS form, so it is refused before anything is lowered.
const REQUIRED_VARIABLES: &[&str] = &[
    "action_type", "client_lib_ref", "css_checkbox", "css_datepicker", "css_dropdownlist",
    "css_htmldisplayer", "css_numericbox", "css_radiobutton", "css_telephone", "css_textbox",
    "css_textbox_multiline", "custom_resource_type_base", "default_layout", "dor_field_styling",
    "dor_type", "email_validate_clause", "email_validate_message", "form_code", "form_type",
    "meta_template_ref", "metadata_master_language", "page_resource_type", "resource_type_base",
    "tel_validate_clause", "tel_validate_message", "template_path", "use_summary", "wizard_layout",
];

/// The profile variables an attribute is written for only when they are set.
const OPTIONAL_VARIABLES: &[&str] = &["dor_template_ref", "redirect_url", "theme_ref"];

/// The profile's variables as the attribute values they stand for. The
/// profile writes them as they appear between an attribute's quotes, so one
/// that holds `&lt;` stands for `<`; one that cannot be read that way, or a
/// required one that is missing, is an error.
fn attribute_variables(vars: &HashMap<String, String>) -> Result<HashMap<String, String>, LowerError> {
    let mut values = HashMap::new();
    for key in REQUIRED_VARIABLES.iter().chain(OPTIONAL_VARIABLES) {
        let Some(value) = vars.get(*key) else {
            if REQUIRED_VARIABLES.contains(key) {
                return Err(format!("the profile has no variable `{key}`"));
            }
            continue;
        };
        let value = quick_xml::escape::unescape(value)
            .map_err(|e| format!("the profile variable `{key}` is not an attribute value: {e}"))?;
        values.insert((*key).to_owned(), value.into_owned());
    }
    Ok(values)
}

/// The form page `.content.xml` of `root`, `normalize`d and indexed by the
/// caller.
pub(super) fn form_xml(
    root: &AemNode,
    config: &AemConfig,
    index: &RenderIndex,
    pass: &HashMap<Uuid, Passthrough>,
) -> Result<String, LowerError> {
    let vars = attribute_variables(&config.user_vars)?;
    let lower = Lower {
        config,
        index,
        pass,
        vars: &vars,
    };
    let form = lower.form(root)?;
    let form = form
        .validate_with(ValidateOptions { names: NameScope::Siblings, allow_empty: true })
        .map_err(|violations| format!("the lowered form is not valid: {violations:?}"))?;
    let master = form.form().metadata.master_language.clone();
    let xml = u2s_mapper_aem::xml_writer::write_form_xml(
        &form,
        &u2s_mapper_aem::xml_writer::WriteCtx {
            master: &master,
            bind_refs: &HashMap::new(),
            spell_defaults: false,
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{xml}"))
}

// ---------------------------------------------------------------- elements --

/// An element's `items`: its attributes and its typed children.
type Items = (Vec<(String, String)>, Vec<Node>);

/// One UBS element, before it is a generic node: its attributes in the order
/// a reader expects them, its raw children before and after its `items`, and
/// its `items` (attributes and typed children) if it has one.
struct El {
    tag: String,
    name: String,
    attrs: Vec<(String, String)>,
    before: Vec<RawJcrNode>,
    items: Option<Items>,
    after: Vec<RawJcrNode>,
    /// A loaded package's raw attributes this element does not own.
    carried: BTreeMap<String, String>,
}

impl El {
    fn new(tag: impl Into<String>, name: impl Into<String>) -> Self {
        El {
            tag: tag.into(),
            name: name.into(),
            attrs: Vec::new(),
            before: Vec::new(),
            items: None,
            after: Vec::new(),
            carried: BTreeMap::new(),
        }
    }

    fn attr(mut self, key: &str, value: impl Into<String>) -> Self {
        self.attrs.push((key.to_owned(), value.into()));
        self
    }

    fn attr_if(self, condition: bool, key: &str, value: impl Into<String>) -> Self {
        if condition { self.attr(key, value) } else { self }
    }

    fn attr_opt(self, key: &str, value: Option<impl Into<String>>) -> Self {
        match value {
            Some(value) => self.attr(key, value),
            None => self,
        }
    }

    fn before(mut self, child: RawJcrNode) -> Self {
        self.before.push(child);
        self
    }

    fn after(mut self, child: RawJcrNode) -> Self {
        self.after.push(child);
        self
    }

    fn items(mut self, attrs: Vec<(String, String)>, children: Vec<Node>) -> Self {
        self.items = Some((attrs, children));
        self
    }

    fn chrome(&self) -> Chrome {
        let mut raw_children = self.before.clone();
        raw_children.extend(self.after.iter().cloned());
        Chrome {
            raw_attributes: self.carried.clone(),
            raw_children,
            slot: Some(self.before.len()),
            items: self.items.as_ref().map(|(attrs, _)| {
                Box::new(Chrome {
                    raw_attributes: attrs.iter().cloned().collect(),
                    ..Chrome::default()
                })
            }),
        }
    }

    fn properties(&self) -> Result<BTreeMap<JcrName, JcrValue>, LowerError> {
        self.attrs
            .iter()
            .filter(|(key, _)| key != "name" && key != "jcr:primaryType")
            .map(|(key, value)| Ok((jcr_name(key)?, JcrValue::Single(value.clone()))))
            .collect()
    }

    fn node(self) -> Result<Node, LowerError> {
        let chrome = self.chrome();
        let properties = self.properties()?;
        Ok(Node::Component {
            common: Common {
                name: component_name(&self.name)?,
                resource_type: None,
                guide_node_class: None,
                jcr_name: Some(jcr_name(&self.tag)?),
                visible: true,
                enabled: true,
                css: CssClasses::default(),
                presence: Presence::default(),
                bind_ref: None,
                passthrough: chrome,
            },
            properties,
            children: self.items.map(|(_, children)| children).unwrap_or_default(),
        })
    }

    fn page(self) -> Result<Page, LowerError> {
        let chrome = self.chrome();
        let properties = self.properties()?;
        Ok(Page {
            name: component_name(&self.name)?,
            jcr_name: Some(jcr_name(&self.tag)?),
            properties,
            passthrough: chrome,
            children: self.items.map(|(_, children)| children).unwrap_or_default(),
        })
    }
}

fn component_name(name: &str) -> Result<ComponentName, LowerError> {
    ComponentName::try_from(name.to_owned()).map_err(|e| format!("component `{name}`: {e}"))
}

fn jcr_name(name: &str) -> Result<JcrName, LowerError> {
    JcrName::try_from(name.to_owned()).map_err(|e| format!("element or attribute `{name}`: {e}"))
}

/// A raw JCR element.
fn raw(tag: &str, attrs: &[(&str, &str)], children: Vec<RawJcrNode>) -> RawJcrNode {
    RawJcrNode {
        tag_name: tag.to_owned(),
        attributes: attrs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        children,
    }
}

/// `fd:rules` or `fd:scripts` with `rules` by attribute.
fn scripts(rules: &[(ScriptEvent, Vec<EventScript>)]) -> RawJcrNode {
    let mut node = raw("fd:scripts", &[("jcr:primaryType", "nt:unstructured")], Vec::new());
    for (event, list) in rules {
        node.attributes
            .insert(event.attribute().to_owned(), super::scripts::attribute_value(list));
    }
    node
}

fn empty_rules() -> RawJcrNode {
    raw("fd:rules", &[("jcr:primaryType", "nt:unstructured")], Vec::new())
}

fn responsive(colspan: u32) -> RawJcrNode {
    let width = colspan.to_string();
    raw(
        "cq:responsive",
        &[("jcr:primaryType", "nt:unstructured")],
        vec![raw(
            "default",
            &[("jcr:primaryType", "nt:unstructured"), ("offset", "0"), ("width", &width)],
            Vec::new(),
        )],
    )
}

/// The generic JCR tree of a loaded raw child, kept as XML text in a UBS
/// [`Passthrough`].
fn raw_from_xml(xml: &str) -> Result<RawJcrNode, LowerError> {
    fn convert(node: u2s_mapper_aem::jcr::tree::JcrNode) -> RawJcrNode {
        RawJcrNode {
            tag_name: node.tag_name,
            attributes: node.attributes.into_iter().collect(),
            children: node.children.into_iter().map(convert).collect(),
        }
    }
    u2s_mapper_aem::jcr::tree::parse_jcr_xml(xml)
        .map(convert)
        .map_err(|e| format!("a carried child is not readable: {e}"))
}

// -------------------------------------------------------------- lowering --

struct Lower<'a> {
    config: &'a AemConfig,
    index: &'a RenderIndex,
    pass: &'a HashMap<Uuid, Passthrough>,
    vars: &'a HashMap<String, String>,
}

impl Lower<'_> {
    /// A required profile variable, as the attribute value it stands for
    /// ([`attribute_variables`] has checked it is there).
    fn var(&self, key: &str) -> String {
        assert!(REQUIRED_VARIABLES.contains(&key), "`{key}` is not a required profile variable");
        self.vars[key].clone()
    }

    /// An optional profile variable, when the profile sets it to something.
    fn optional_var(&self, key: &str) -> Option<String> {
        assert!(OPTIONAL_VARIABLES.contains(&key), "`{key}` is not an optional profile variable");
        self.vars.get(key).filter(|v| !v.is_empty()).cloned()
    }

    fn author(&self) -> &str {
        &self.config.author
    }

    fn controls(&self, kind: &str) -> String {
        format!("{}/controls/{kind}", self.var("custom_resource_type_base"))
    }

    fn default_layout(&self) -> String {
        self.var("default_layout")
    }

    /// The `items` attributes every UBS panel's `items` carries.
    fn grid_items(&self) -> Vec<(String, String)> {
        vec![
            ("jcr:primaryType".into(), "nt:unstructured".into()),
            ("sling:resourceType".into(), self.default_layout()),
        ]
    }

    /// A panel's `layout` child.
    fn layout(&self, dor_num_cols: Option<&str>) -> RawJcrNode {
        let layout = self.default_layout();
        let mut node = raw(
            "layout",
            &[
                ("jcr:primaryType", "nt:unstructured"),
                ("sling:resourceType", &layout),
                ("columns", "1"),
                ("dorLayoutType", "columnar"),
                ("nonNavigable", "{Boolean}true"),
                ("toolbarPosition", "Bottom"),
            ],
            Vec::new(),
        );
        if let Some(cols) = dor_num_cols {
            node.attributes.insert("dorNumCols".into(), cols.into());
        }
        node
    }

    /// The timestamps and author every component of the profile carries.
    fn stamped(&self, el: El, modified: &str) -> El {
        el.attr("jcr:created", CREATED)
            .attr("jcr:createdBy", self.author())
            .attr("jcr:lastModified", modified)
            .attr("jcr:lastModifiedBy", self.author())
            .attr("jcr:primaryType", "nt:unstructured")
    }

    /// `el` with `node`'s carried passthrough: its raw attributes the
    /// element does not own, and its raw children after the element's own.
    fn carry(&self, mut el: El, node: &AemNode, owned: &[&str]) -> Result<El, LowerError> {
        el.carried = self.carried_attributes(node, owned);
        if let Some(pt) = node.uuid().and_then(|u| self.pass.get(&u)) {
            for child in &pt.raw_children {
                el.after.push(raw_from_xml(child)?);
            }
        }
        Ok(el)
    }

    /// `node`'s loaded attributes the element does not write itself.
    fn carried_attributes(&self, node: &AemNode, owned: &[&str]) -> BTreeMap<String, String> {
        node.uuid()
            .and_then(|u| self.pass.get(&u))
            .map(|pt| {
                pt.raw_attributes
                    .iter()
                    .filter(|(k, _)| !owned.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether `node`'s carried children hold an `fd:rules` of their own, in
    /// which case the element writes no empty one.
    fn carries_rules(&self, node: &AemNode) -> bool {
        node.uuid()
            .and_then(|u| self.pass.get(&u))
            .is_some_and(|pt| pt.raw_children.iter().any(|c| c.contains("<fd:rules")))
    }

    /// The node's own CSS classes, or the profile's for its kind when it has
    /// none (an empty class list is none, as a loaded `css=""` is).
    fn css(&self, attrs: &AemAttrs, fallback: &str) -> String {
        attrs.css.clone().filter(|css| !css.is_empty()).unwrap_or_else(|| self.var(fallback))
    }

    fn children(&self, children: &[AemNode]) -> Result<Vec<Node>, LowerError> {
        children.iter().map(|c| self.node(c)).collect()
    }

    fn node(&self, node: &AemNode) -> Result<Node, LowerError> {
        match node {
            AemNode::Root { .. } => Err("a Root node inside the form".into()),
            AemNode::Panel { .. } => self.panel(node)?.node(),
            _ => self.leaf(node)?.node(),
        }
    }

    /// A field or draw: the leaves of the tree.
    fn leaf(&self, node: &AemNode) -> Result<El, LowerError> {
        let tag = node.element_name();
        let el = match node {
            AemNode::TextField {
                name, label, mandatory, visible, max_chars, colspan, dor_colspan, bind_ref, kind, attrs, ..
            } => {
                use super::TextFieldKind as K;
                let (resource, owned): (&str, &[&str]) = match kind {
                    K::Plain => ("textbox", owned::TEXTBOX),
                    K::Multiline => ("textboxMultiline", owned::TEXTBOX_MULTILINE),
                    K::Email => ("email", owned::EMAIL),
                    K::Telephone => ("telephone", owned::TELEPHONE),
                };
                let mut el = self
                    .stamped(El::new(&tag, name), CREATED)
                    .attr_if(!label.is_empty(), "jcr:title", label.as_str())
                    .attr("sling:resourceType", self.controls(resource))
                    .attr_if(attrs.always_in_pdf, "alwaysInPdf", "true");
                el = match kind {
                    K::Plain => el
                        .attr("assistPriority", "label")
                        .attr("css", self.css(attrs, "css_textbox")),
                    K::Multiline => el.attr("css", self.css(attrs, "css_textbox_multiline")),
                    K::Email => el
                        .attr("assistPriority", "label")
                        .attr("autofillFieldKeyword", "email"),
                    K::Telephone => el
                        .attr("assistPriority", "label")
                        .attr("autofillFieldKeyword", "tel")
                        .attr("css", self.css(attrs, "css_telephone"))
                        .attr("displayIsSameAsValidate", "true")
                        .attr("displayPatternType", "custom")
                        .attr("displayPictureClause", self.var("tel_validate_clause")),
                };
                let class = if *kind == K::Telephone { "guideTelephone" } else { "guideTextBox" };
                el = self
                    .field_presence(el, attrs, *dor_colspan)
                    .attr("guideNodeClass", class)
                    .attr("mandatory", mandatory.to_string());
                if *kind == K::Multiline {
                    el = el.attr("multiLine", "true");
                } else {
                    el = el.attr_opt("maxChars", max_chars.filter(|n| *n != 0).map(|n| n.to_string()));
                }
                el = el.attr("name", name.as_str()).attr_opt("bindRef", nonempty(bind_ref));
                el = tail(el, attrs);
                if *kind != K::Multiline {
                    el = el.attr("textIsRich", "[true,true,true]");
                }
                el = match kind {
                    K::Email => el
                        .attr("validatePictureClause", self.var("email_validate_clause"))
                        .attr("validatePictureClauseMessage", self.var("email_validate_message"))
                        .attr("validationPatternType", "custom"),
                    K::Telephone => el
                        .attr("validatePictureClause", self.var("tel_validate_clause"))
                        .attr("validatePictureClauseMessage", self.var("tel_validate_message"))
                        .attr("validationPatternType", "custom"),
                    _ => el,
                };
                self.carry(hidden(el, *visible).after(responsive(*colspan)), node, owned)?
            }
            AemNode::NumberField {
                name, label, mandatory, visible, colspan, dor_colspan, bind_ref, attrs, ..
            } => {
                let el = self
                    .stamped(El::new(&tag, name), CREATED)
                    .attr_if(!label.is_empty(), "jcr:title", label.as_str())
                    .attr("sling:resourceType", self.controls("numericbox"))
                    .attr_if(attrs.always_in_pdf, "alwaysInPdf", "true")
                    .attr("assistPriority", "label")
                    .attr("css", self.css(attrs, "css_numericbox"));
                let el = self
                    .field_presence(el, attrs, *dor_colspan)
                    .attr("guideNodeClass", "guideNumericBox")
                    .attr("mandatory", mandatory.to_string())
                    .attr("name", name.as_str())
                    .attr_opt("bindRef", nonempty(bind_ref));
                let el = tail(el, attrs).attr("textIsRich", "[true,true,true]");
                self.carry(hidden(el, *visible).after(responsive(*colspan)), node, owned::NUMERICBOX)?
            }
            AemNode::DatePicker {
                name, label, mandatory, visible, colspan, dor_colspan, bind_ref, attrs, ..
            } => {
                let el = self
                    .stamped(El::new(&tag, name), CREATED)
                    .attr_if(!label.is_empty(), "jcr:title", label.as_str())
                    .attr("sling:resourceType", self.controls("datepicker"))
                    .attr_if(attrs.always_in_pdf, "alwaysInPdf", "true")
                    .attr("css", self.css(attrs, "css_datepicker"));
                let el = self
                    .field_presence(el, attrs, *dor_colspan)
                    .attr("guideNodeClass", "guideDatePicker")
                    .attr("mandatory", mandatory.to_string())
                    .attr("name", name.as_str())
                    .attr_opt("bindRef", nonempty(bind_ref))
                    .attr("placeholderText", "");
                let el = tail(el, attrs)
                    .attr("textIsRich", "[true,true]")
                    .attr("validatePictureClause", "date{YYYY-MM-DD}")
                    .attr("validatePictureClauseMessage", "Please enter the date using the format YYYY-MM-DD.")
                    .attr("validationPatternType", "custom")
                    .attr("yearRangeFrom", "100")
                    .attr("yearRangeTo", "10");
                self.carry(hidden(el, *visible).after(responsive(*colspan)), node, owned::DATEPICKER)?
            }
            AemNode::Dropdown {
                name, label, options, mandatory, visible, colspan, dor_colspan, bind_ref, attrs, ..
            } => {
                let el = self
                    .stamped(El::new(&tag, name), CREATED)
                    .attr_if(!label.is_empty(), "jcr:title", label.as_str())
                    .attr("sling:resourceType", self.controls("dropdownlist"))
                    .attr_if(attrs.always_in_pdf, "alwaysInPdf", "true")
                    .attr("css", self.css(attrs, "css_dropdownlist"));
                let el = self
                    .field_presence(el, attrs, *dor_colspan)
                    .attr("guideNodeClass", "guideDropDownList")
                    .attr("mandatory", mandatory.to_string())
                    .attr("name", name.as_str())
                    .attr_opt("bindRef", nonempty(bind_ref))
                    .attr("options", options_attr(options));
                let el = tail(el, attrs).attr("textIsRich", "[true,true]");
                let el = self.reset(hidden(el, *visible).after(responsive(*colspan)), name);
                self.carry(el, node, owned::DROPDOWNLIST)?
            }
            AemNode::Checkbox {
                name, options, alignment, visible, colspan, dor_colspan, bind_ref, attrs, ..
            } => {
                let el = self
                    .stamped(El::new(&tag, name), CREATED)
                    .attr("sling:resourceType", self.controls("checkbox"))
                    .attr_if(attrs.always_in_pdf, "alwaysInPdf", "true")
                    .attr("alignment", alignment_str(*alignment))
                    .attr("assistPriority", "caption")
                    .attr("css", self.css(attrs, "css_checkbox"));
                let el = self
                    .field_presence(el, attrs, *dor_colspan)
                    .attr("guideNodeClass", "guideCheckBox")
                    .attr("hideTitle", "true")
                    .attr("name", name.as_str())
                    .attr_opt("bindRef", nonempty(bind_ref))
                    .attr("options", options_attr(options))
                    .attr("richTextOptions", "true");
                let el = tail(el, attrs).attr("textIsRich", text_is_rich(options));
                self.carry(hidden(el, *visible).after(responsive(*colspan)), node, owned::CHECKBOX)?
            }
            AemNode::RadioButton {
                name, label, options, alignment, mandatory, visible, colspan, dor_colspan, bind_ref, attrs, ..
            } => {
                let preselect = self.index.preselect.get(name).filter(|v| !v.is_empty());
                let el = self
                    .stamped(El::new(&tag, name), CREATED)
                    .attr_if(!label.is_empty(), "jcr:title", label.as_str())
                    .attr("sling:resourceType", self.controls("radiobutton"))
                    .attr_if(attrs.always_in_pdf, "alwaysInPdf", "true")
                    .attr_opt("_value", preselect.cloned())
                    .attr("alignment", alignment_str(*alignment))
                    .attr("css", self.css(attrs, "css_radiobutton"));
                let el = self
                    .field_presence(el, attrs, *dor_colspan)
                    .attr("guideNodeClass", "guideRadioButton")
                    .attr("mandatory", mandatory.to_string())
                    .attr("name", name.as_str())
                    .attr_opt("bindRef", nonempty(bind_ref))
                    .attr("options", options_attr(options))
                    .attr("richTextOptions", "true");
                let el = tail(el, attrs).attr("textIsRich", text_is_rich(options));
                let el = self.reset(hidden(el, *visible).after(responsive(*colspan)), name);
                self.carry(el, node, owned::RADIOBUTTON)?
            }
            AemNode::TextDraw { name, content, attrs, visible, colspan, dor_colspan, .. } => {
                let el = self
                    .stamped(El::new(&tag, name), CREATED)
                    .attr("sling:resourceType", self.controls("textdraw"))
                    .attr("_value", content.as_str())
                    .attr_if(attrs.always_in_pdf, "alwaysInPdf", "true")
                    .attr("css", attrs.css.clone().unwrap_or_default())
                    .attr_opt("dorColspan", nonzero(*dor_colspan))
                    .attr_if(attrs.dor_exclude, "dorExclusion", "true")
                    .attr("dorFieldStyling", self.var("dor_field_styling"))
                    .attr_opt("dorHeaderSlot", attrs.dor_header_slot.clone())
                    .attr("guideNodeClass", "guideTextDraw")
                    .attr("name", name.as_str());
                let el = hidden(tail(el, attrs).attr("textIsRich", "true"), *visible);
                let el = if self.carries_rules(node) { el } else { el.after(empty_rules()) };
                self.carry(el.after(responsive(*colspan)), node, owned::TEXTDRAW)?
            }
            AemNode::TitleDraw { name, content, heading_level, colspan, dor_colspan, attrs, visible, .. } => {
                let el = self
                    .stamped(El::new(&tag, name), CREATED)
                    .attr("sling:resourceType", self.controls("titledraw"))
                    .attr("_value", content.as_str())
                    .attr_if(attrs.always_in_pdf, "alwaysInPdf", "true")
                    .attr_opt("css", attrs.css.clone())
                    .attr_opt("dorColspan", nonzero(*dor_colspan))
                    .attr_if(attrs.dor_exclude, "dorExclusion", "true")
                    .attr("dorFieldStyling", self.var("dor_field_styling"))
                    .attr("guideNodeClass", "guideTextDraw")
                    .attr("headingLevel", heading_level.to_string())
                    .attr("name", name.as_str());
                let el = hidden(tail(el, attrs).attr("textIsRich", "true"), *visible);
                self.carry(el.after(responsive(*colspan)), node, owned::TITLEDRAW)?
            }
            AemNode::HtmlDisplayer { name, content, attrs, visible, colspan, dor_colspan, .. } => {
                let el = self
                    .stamped(El::new(&tag, name), CREATED)
                    .attr("sling:resourceType", self.controls("htmlDisplayer"))
                    .attr_if(attrs.always_in_pdf, "alwaysInPdf", "true")
                    .attr("autofillFieldKeyword", "name")
                    .attr("css", self.css(attrs, "css_htmldisplayer"))
                    .attr_opt("dorColspan", nonzero(*dor_colspan))
                    .attr_if(attrs.dor_exclude, "dorExclusion", "true")
                    .attr("dorFieldStyling", self.var("dor_field_styling"))
                    .attr("guideNodeClass", "guideTextBox")
                    .attr("initScript", "window.forms.ubs.control.htmlviewer.initialize(this)")
                    .attr("name", name.as_str());
                let el = hidden(tail(el, attrs).attr("textIsRich", "[true,true,true,true]"), *visible);
                let el = el.after(self.locale_content(content)).after(responsive(*colspan));
                self.carry(el, node, owned::HTMLDISPLAYER)?
            }
            AemNode::MessageBox { name, content, attrs, visible, colspan, .. } => {
                let el = self
                    .stamped(El::new(&tag, name), CREATED)
                    .attr("sling:resourceType", self.controls("messagebox"))
                    .attr("ariaLiveAttribute", "polite")
                    .attr("css", attrs.css.clone().unwrap_or_default())
                    .attr("dorExclusion", "true")
                    .attr("guideNodeClass", "guideTextDraw")
                    .attr("hideTitle", "{Boolean}true")
                    .attr("initScript", "com.ajila.forms.control.messagebox.initialize(this)")
                    .attr("messageboxBody", content.as_str())
                    .attr("messageboxType", "{Long}2")
                    .attr("name", name.as_str())
                    .attr("summaryExclusion", "true")
                    .attr("textIsRich", "true");
                let el = hidden(el, *visible);
                let el = if self.carries_rules(node) { el } else { el.after(empty_rules()) };
                self.carry(el.after(responsive(*colspan)), node, owned::MESSAGEBOX)?
            }
            AemNode::FootnotePlaceholder { name, colspan, .. } => {
                let el = self
                    .stamped(El::new(&tag, name), CREATED)
                    .attr("jcr:title", "Footnote Placeholder")
                    .attr("sling:resourceType", self.controls("guidefootnoteplaceholder"))
                    .attr("_value", "<p>The footnotes will appear here</p>")
                    .attr("guideNodeClass", "guideFootnotePlaceHolder")
                    .attr("name", name.as_str());
                self.carry(el.after(responsive(*colspan)), node, owned::FOOTNOTEPLACEHOLDER)?
            }
            AemNode::Repeatable { .. } => self.repeatable(node)?,
            AemNode::Fragment { .. } => self.fragment(node)?,
            AemNode::Preface { .. } => self.preface(node)?,
            AemNode::Root { .. } | AemNode::Panel { .. } => {
                return Err(format!("`{tag}` is not a leaf"));
            }
        };
        Ok(el)
    }

    /// `dorColspan`, `dorExcludeTitle`, `dorExclusion`, `dorFieldStyling` and
    /// `dorHeaderSlot`, as every input field writes them.
    fn field_presence(&self, el: El, attrs: &AemAttrs, dor_colspan: Option<u32>) -> El {
        el.attr_opt("dorColspan", nonzero(dor_colspan))
            .attr_if(attrs.dor_exclude_title, "dorExcludeTitle", "true")
            .attr_if(attrs.dor_exclude, "dorExclusion", "true")
            .attr("dorFieldStyling", self.var("dor_field_styling"))
            .attr_opt("dorHeaderSlot", attrs.dor_header_slot.clone())
    }

    /// A configurator choice's reset-on-change rule, when it is one.
    fn reset(&self, el: El, name: &str) -> El {
        match self.index.resets.get(name).filter(|t| !t.is_empty()) {
            Some(targets) => el.after(scripts(&[(
                ScriptEvent::ValueCommit,
                vec![super::scripts::configurator_reset(name, targets)],
            )])),
            None => el,
        }
    }

    /// An HTML displayer's markup, one item per locale it ships.
    fn locale_content(&self, content: &AemI18nText) -> RawJcrNode {
        let mut items = Vec::new();
        for lang in self.config.expand_languages() {
            let markup = content
                .get(&lang)
                .or_else(|| content.get(&self.config.canonical_language(&lang)))
                .unwrap_or_else(|| content.master(&self.config.master_language));
            if markup.trim().is_empty() {
                continue;
            }
            let locale = self.config.html_locale(&lang);
            items.push(raw(
                &format!("item{}", items.len()),
                &[("jcr:primaryType", "nt:unstructured"), ("html", markup), ("locale", &locale)],
                Vec::new(),
            ));
        }
        raw("localeContent", &[("jcr:primaryType", "nt:unstructured")], items)
    }
}

impl Lower<'_> {
    /// A panel: a step (`is_page`), a conditional panel, or a plain one.
    fn panel(&self, node: &AemNode) -> Result<El, LowerError> {
        let AemNode::Panel {
            uuid, name, title, children, is_page, attrs, visible, is_conditional,
            dor_num_cols, colspan, dor_colspan, bind_ref, ..
        } = node
        else {
            return Err("not a panel".into());
        };
        let tag = node.element_name();
        let dor_cols = dor_num_cols.filter(|n| *n != 0).map(|n| n.to_string());
        let mut typed = Vec::new();
        let triggers = self
            .index
            .visibility
            .get(name)
            .filter(|t| *is_conditional && !t.is_empty())
            .map(|t| {
                t.iter()
                    .map(|(field, value)| (field.clone(), condition_value_str(value)))
                    .collect::<Vec<_>>()
            });

        if *is_conditional {
            let el = self
                .stamped(El::new(&tag, name), MODIFIED_LOCAL)
                .attr_if(!title.is_empty(), "jcr:title", title.as_str())
                .attr("sling:resourceType", self.controls("panel"))
                .attr_if(attrs.always_in_pdf, "alwaysInPdf", "true")
                .attr_opt("css", attrs.css.clone())
                .attr_opt("dorColspan", nonzero(*dor_colspan))
                .attr("dorExcludeDescription", "true")
                .attr("dorExcludeTitle", "true")
                .attr_if(attrs.dor_exclude, "dorExclusion", "true")
                .attr("dorFieldStyling", self.var("dor_field_styling"))
                .attr("guideNodeClass", "guidePanel")
                .attr_if(self.var("use_summary") == "true", "hideTitle", "{Boolean}true")
                .attr("name", name.as_str());
            let el = tail(el, attrs)
                .attr("textIsRich", "true")
                .attr("validateOnStepCompletion", "{Boolean}false")
                .attr_if(!visible && triggers.is_none(), "visible", "{Boolean}false");
            let mut el = el
                .items(self.grid_items(), self.children(children)?)
                .after(self.layout(dor_cols.as_deref()));
            if let Some(triggers) = &triggers {
                let (shown, init) = super::scripts::show(name, triggers);
                el = el.after(empty_rules()).after(scripts(&[
                    (ScriptEvent::Visibility, vec![shown]),
                    (ScriptEvent::Initialize, vec![init]),
                ]));
            }
            return self.carry(el.after(responsive(*colspan)), node, owned::CONDITIONAL);
        }

        let configurator = name.starts_with("PN_FormConfigurator");
        let el = self
            .stamped(El::new(&tag, name), MODIFIED_LOCAL)
            .attr_if(!is_page && !title.is_empty(), "jcr:title", title.as_str())
            .attr("sling:resourceType", self.controls("panel"))
            .attr_if(attrs.always_in_pdf, "alwaysInPdf", "true")
            .attr_opt(
                "css",
                attrs.css.clone().or_else(|| configurator.then(|| "ubs-margin-10".to_owned())),
            )
            .attr_opt("dorColspan", nonzero(*dor_colspan))
            .attr("dorExcludeDescription", "true")
            .attr_if(attrs.dor_exclude_title || *is_page, "dorExcludeTitle", "true")
            .attr_if(attrs.dor_exclude || name == "PN_BR", "dorExclusion", "true")
            .attr("dorFieldStyling", self.var("dor_field_styling"))
            .attr_opt("dorHeaderSlot", attrs.dor_header_slot.clone())
            .attr("guideNodeClass", "guidePanel")
            .attr_if(attrs.jump_to_field, "jumpToFieldButtonVisible", "true")
            .attr("name", name.as_str())
            .attr_opt("bindRef", nonempty(bind_ref))
            .attr_if(attrs.show_if_hidden, "showIfHidden", "true")
            .attr_if(
                attrs.dor_exclude || attrs.summary_exclude || configurator || name == "PN_BR",
                "summaryExclusion",
                "true",
            )
            .attr("textIsRich", "true")
            .attr("validateOnStepCompletion", "{Boolean}false");
        let el = hidden(el, *visible);

        if *is_page && !title.is_empty() {
            typed.push(self.step_title(*uuid, name, title, children)?);
        }
        typed.extend(self.children(children)?);
        let el = el
            .items(self.grid_items(), typed)
            .after(self.layout(dor_cols.as_deref()))
            .after(responsive(*colspan));
        self.carry(el, node, owned::PANEL)
    }

    /// A step's title panel: its heading, which on the first page is the
    /// subtitle under the form title (PROBLEM-banking-subtitle), and the
    /// jump-to-field button where the step has something to fill in and no
    /// rows of its own (PROBLEM-jump-to-field-button).
    fn step_title(&self, uuid: Uuid, name: &str, title: &str, children: &[AemNode]) -> Result<Node, LowerError> {
        let first_page = children.iter().any(|c| matches!(c, AemNode::Preface { .. }));
        let configurator = first_page && name.starts_with("PN_FormConfigurator");
        let subtitle = first_page && !configurator;
        let mut repeatables = HashSet::new();
        for child in children {
            collect_repeatable_names(child, &mut repeatables);
        }
        let has_input = children.iter().any(holds_input);
        let uuid = uuid.as_simple().to_string();
        let stem = name.replace("PN_", "");
        let heading = format!("<p>{title}</p>\n");
        let draw = if subtitle {
            self.stamped(El::new(format!("textdraw_{uuid}"), format!("ST_{stem}")), CREATED)
                .attr("sling:resourceType", self.controls("textdraw"))
                .attr("_value", heading)
                .attr("css", "subtitle-after-form-title")
                .attr("dorFieldStyling", self.var("dor_field_styling"))
                .attr("guideNodeClass", "guideTextDraw")
                .attr("textIsRich", "true")
        } else {
            self.stamped(El::new(format!("titledraw_{uuid}"), format!("TTL_{stem}")), CREATED)
                .attr("sling:resourceType", self.controls("titledraw"))
                .attr("_value", heading)
                .attr("css", "stepTitle")
                .attr("dorExclusion", "true")
                .attr("dorFieldStyling", self.var("dor_field_styling"))
                .attr("guideNodeClass", "guideTextDraw")
                .attr("headingLevel", "2")
                .attr("summaryExclusion", "true")
                .attr("textIsRich", "true")
        };
        let draw_name = draw.name.clone();
        let draw = draw.attr("name", draw_name);
        let panel = self
            .stamped(El::new(format!("panel_title_{uuid}"), format!("{name}Title")), MODIFIED_LOCAL)
            .attr_if(!subtitle, "jcr:title", title)
            .attr("sling:resourceType", self.controls("panel"))
            .attr("dorExcludeDescription", "true")
            .attr_if(configurator, "dorExclusion", "true")
            .attr_if(configurator, "summaryExclusion", "true")
            .attr("dorFieldStyling", self.var("dor_field_styling"))
            .attr("guideNodeClass", "guidePanel")
            .attr("name", format!("{name}Title"))
            .attr("textIsRich", "true")
            .attr_if(
                has_input && !configurator && repeatables.is_empty(),
                "jumpToFieldButtonVisible",
                "true",
            )
            .attr("validateOnStepCompletion", "{Boolean}false")
            .before(self.layout(None))
            .items(self.grid_items(), vec![draw.node()?]);
        panel.node()
    }

    /// A repeatable: the wrapper, the repeating panel with its Remove button
    /// and row, and the Add button. A signature twin is driven by its data
    /// panel's buttons and has none of its own (PROBLEM-repeating-panel §8).
    fn repeatable(&self, node: &AemNode) -> Result<El, LowerError> {
        let AemNode::Repeatable {
            name, children, min_occur, max_occur, bind_ref, attrs, visible, ..
        } = node
        else {
            return Err("not a repeatable".into());
        };
        let tag = node.element_name();
        let panel_name = repeat_panel_name(name);
        let row_name = repeat_row_name(name);
        let data_panel = self.index.twin_data_panels.get(name);
        let is_twin = data_panel.is_some();
        // A twin borrows its data panel's subject: the same rows, announced and
        // numbered alike. A panel with no subject says a person has to name it.
        let subject = self
            .index
            .add_subjects
            .get(data_panel.unwrap_or(name))
            .or_else(|| self.index.add_subjects.get(name))
            .map(String::as_str)
            .unwrap_or("(Repeatable name)");
        let add_label = self
            .config
            .add_label(&self.config.base_language(), subject)
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| "Add".to_owned());
        let twin = self.index.signature_twins.get(name).map(String::as_str);
        let buttons = super::scripts::Repeating { panel: &panel_name, twin, subject };
        let max = if *max_occur == AemNode::UNBOUNDED_OCCUR { "-1".to_owned() } else { max_occur.to_string() };
        let jump = self.index.jump_to_field_repeatables.contains(name);
        let layout = || self.layout(Some("1"));

        let row = self
            .stamped(El::new("panel_copy_copy", &row_name), CREATED)
            .attr("sling:resourceType", self.controls("panel"))
            .attr("dorExcludeDescription", "true")
            .attr("dorExcludeTitle", "true")
            .attr("guideNodeClass", "guidePanel")
            .attr("name", row_name.as_str())
            .attr("textIsRich", "true")
            .attr("validateOnStepCompletion", "{Boolean}false")
            .before(layout())
            .items(self.grid_items(), self.children(children)?)
            .after(responsive(12));

        let mut inner_items = Vec::new();
        if !is_twin {
            let [click, shown, init] = buttons.remove();
            inner_items.push(
                self.stamped(El::new("removebutton", "BT_Remove"), CREATED)
                    .attr("sling:resourceType", self.controls("removebutton"))
                    .attr("dorExclusion", "true")
                    .attr("summaryExclusion", "true")
                    .attr("dorFieldStyling", self.var("dor_field_styling"))
                    .attr("guideNodeClass", "guideButton")
                    .attr("name", "BT_Remove")
                    .attr("textIsRich", "[true,true]")
                    .attr("type", "Button")
                    .attr("visible", "{Boolean}false")
                    .after(empty_rules())
                    .after(scripts(&[
                        (ScriptEvent::Click, vec![click]),
                        (ScriptEvent::Visibility, vec![shown]),
                        (ScriptEvent::Initialize, vec![init]),
                    ]))
                    .node()?,
            );
        }
        inner_items.push(row.node()?);
        let inner = self
            .stamped(El::new("repeatableInner", &panel_name), CREATED)
            .attr("jcr:title", subject)
            .attr("sling:resourceType", self.controls("panel"))
            .attr("accessibilityLabel", subject)
            .attr_if(!is_twin, "addButton", "BT_Add")
            .attr("ajilaPanelSubject", subject)
            .attr_opt("bindRef", nonempty(bind_ref))
            .attr("dorExcludeDescription", "true")
            .attr("dorFieldStyling", "Repeating Panel Numbering")
            .attr("guideNodeClass", "guidePanel")
            .attr("headingLevel", "3")
            .attr_if(jump, "jumpToFieldButtonVisible", "true")
            .attr("maxOccur", max)
            .attr("minOccur", min_occur.to_string())
            .attr("name", panel_name.as_str())
            .attr_if(!is_twin, "removeButton", "BT_Remove")
            .attr("summaryHeadingLevel", "4")
            .attr("textIsRich", "true")
            .attr("validateOnStepCompletion", "{Boolean}false")
            .before(layout())
            .items(self.grid_items(), inner_items);

        let mut outer_items = vec![inner.node()?];
        if !is_twin {
            let [click, shown, init] = buttons.add();
            outer_items.push(
                self.stamped(El::new("tertiarybutton", "BT_Add"), CREATED)
                    .attr("jcr:title", add_label)
                    .attr("sling:resourceType", self.controls("tertiarybutton"))
                    .attr("dorExclusion", "true")
                    .attr("summaryExclusion", "true")
                    .attr("dorFieldStyling", self.var("dor_field_styling"))
                    .attr("guideNodeClass", "guideButton")
                    .attr("name", "BT_Add")
                    .attr("textIsRich", "[true,true]")
                    .attr("type", "Button")
                    .after(empty_rules())
                    .after(scripts(&[
                        (ScriptEvent::Click, vec![click]),
                        (ScriptEvent::Visibility, vec![shown]),
                        (ScriptEvent::Initialize, vec![init]),
                    ]))
                    .node()?,
            );
        }
        let el = self
            .stamped(El::new(&tag, name), CREATED)
            .attr("sling:resourceType", self.controls("panel"))
            .attr_if(attrs.always_in_pdf, "alwaysInPdf", "true")
            .attr_opt("css", attrs.css.clone())
            .attr("dorExcludeDescription", "true")
            .attr("dorExcludeTitle", "true")
            .attr_if(attrs.dor_exclude, "dorExclusion", "true")
            .attr("guideNodeClass", "guidePanel")
            .attr("name", name.as_str());
        let el = tail(el, attrs)
            .attr("textIsRich", "true")
            .attr("validateOnStepCompletion", "{Boolean}false");
        let el = hidden(el, *visible).before(layout()).items(self.grid_items(), outer_items);
        self.carry(el, node, owned::REPEATABLE)
    }

    /// A fragment reference, with the Initialize rule its kind needs.
    fn fragment(&self, node: &AemNode) -> Result<El, LowerError> {
        let AemNode::Fragment {
            name, frag_ref, bind_ref, attrs, visible, init_hide, init_show, ..
        } = node
        else {
            return Err("not a fragment".into());
        };
        let banking = frag_ref.ends_with("affrg_BankingRelationship1");
        let el = self
            .stamped(El::new(node.element_name(), name), MODIFIED_LOCAL)
            .attr("sling:resourceType", self.controls("panel"))
            .attr_if(attrs.always_in_pdf, "alwaysInPdf", "true")
            .attr_opt("css", attrs.css.clone())
            .attr("dorExcludeDescription", "true")
            .attr("dorExcludeTitle", "true")
            .attr_if(attrs.dor_exclude || banking, "dorExclusion", "true")
            .attr("dorFieldStyling", self.var("dor_field_styling"))
            .attr("fragRef", frag_ref.as_str())
            .attr("guideNodeClass", "guidePanel")
            .attr("name", name.as_str())
            .attr_opt("bindRef", nonempty(bind_ref))
            .attr_if(attrs.show_if_hidden, "showIfHidden", "true")
            .attr_if(attrs.dor_exclude || attrs.summary_exclude || banking, "summaryExclusion", "true")
            .attr("textIsRich", "true")
            .attr("validateOnStepCompletion", "{Boolean}false");
        let el = hidden(el, *visible).before(self.layout(Some("1"))).items(self.grid_items(), Vec::new());
        let el = if self.carries_rules(node) { el } else { el.after(empty_rules()) };
        let sub_panels = super::partner::sub_panels(&self.config.fragments, frag_ref).unwrap_or(&[]);
        let calls = super::partner::init_calls(sub_panels, init_hide, init_show);
        let guide_path = node
            .uuid()
            .and_then(|u| self.index.guide_paths.get(&u))
            .map(String::as_str)
            .ok_or_else(|| format!("the fragment `{}` has no guide path", name.as_str()))?;
        let init = super::scripts::fragment_init(frag_ref, guide_path, self.is_german(), &calls);
        let el = el.after(match init {
            Some(rule) => scripts(&[(ScriptEvent::Initialize, vec![rule])]),
            None => scripts(&[]),
        });
        self.carry(el, node, owned::FRAGMENT)
    }

    /// The banking-relationship block every form opens with, and the DoR
    /// header line under it where the source has one.
    fn preface(&self, node: &AemNode) -> Result<El, LowerError> {
        let AemNode::Preface { uuid, .. } = node else {
            return Err("not a preface".into());
        };
        let uuid = uuid.as_simple().to_string();
        let mut inner = self
            .stamped(El::new(node.element_name(), "PN_BankingRelationship"), MODIFIED_LOCAL)
            .attr("sling:resourceType", self.controls("panel"))
            .attr("completionExpReq", "{Boolean}false")
            .attr("dorExclusion", "true")
            .attr("summaryExclusion", "true")
            .attr("fragRef", "/content/forms/af/afforms_ubs_fragmentlib/affrg_BankingRelationship1")
            .attr("guideNodeClass", "guidePanel")
            .attr("name", "PN_BankingRelationship")
            .attr("textIsRich", "true")
            .attr("validateOnStepCompletion", "{Boolean}false")
            .before(self.layout(Some("1")))
            .items(self.grid_items(), Vec::new());
        if !self.carries_rules(node) {
            inner = inner.after(empty_rules());
        }
        inner = inner.after(match self.is_german() {
            true => scripts(&[(ScriptEvent::Initialize, vec![super::scripts::banking_default_de()])]),
            false => scripts(&[]),
        });
        // The preface's carried children are the banking panel's own.
        if let Some(pt) = node.uuid().and_then(|u| self.pass.get(&u)) {
            for child in &pt.raw_children {
                inner = inner.after(raw_from_xml(child)?);
            }
        }
        let mut items = vec![inner.node()?];
        if let Some(text) = &self.config.header_slot_text {
            // The header text arrives as it is written between an attribute's
            // quotes (`&lt;b>`), so it is unescaped once here.
            let text = quick_xml::escape::unescape(text).map_err(|e| format!("header text: {e}"))?;
            items.push(
                self.stamped(El::new(format!("textdraw_slot2_{uuid}"), "ST_HeaderSlot2"), CREATED)
                    .attr("sling:resourceType", self.controls("textdraw"))
                    .attr("_value", format!("<p>{text}</p>\n"))
                    .attr("alwaysInPdf", "true")
                    .attr("dorFieldStyling", self.var("dor_field_styling"))
                    .attr("dorHeaderSlot", "slot2")
                    .attr("guideNodeClass", "guideTextDraw")
                    .attr("name", "ST_HeaderSlot2")
                    .attr("showIfHidden", "true")
                    .attr("summaryExclusion", "true")
                    .attr("textIsRich", "true")
                    .attr("visible", "{Boolean}false")
                    .node()?,
            );
        }
        let mut outer = self
            .stamped(El::new(format!("panel_{uuid}"), "PN_BR"), MODIFIED_LOCAL)
            .attr("sling:resourceType", self.controls("panel"))
            .attr("css", "ubs-margin-20")
            .attr("dorExclusion", "true")
            .attr("guideNodeClass", "guidePanel")
            .attr("name", "PN_BR")
            .attr("summaryExclusion", "true")
            .attr("textIsRich", "true")
            .attr("validateOnStepCompletion", "{Boolean}false")
            .before(self.layout(None))
            .items(self.grid_items(), items);
        // Only its attributes: what the preface held is the fragment it writes.
        outer.carried = self.carried_attributes(node, owned::PREFACE);
        Ok(outer)
    }

    /// Is this a German form (`formrange_entity` 019)? Its banking
    /// relationship is preset (feedback #104).
    fn is_german(&self) -> bool {
        self.config.xfa_vars.get("formrange_entity").map(String::as_str) == Some("019")
    }
}

impl Lower<'_> {
    /// The whole form: its steps as pages, and the UBS scaffold around them.
    fn form(&self, root: &AemNode) -> Result<AemForm, LowerError> {
        let AemNode::Root { title, children } = root else {
            return Err("the form is not a Root node".into());
        };
        // Every node directly under the root panel is a page to the generic
        // model; a step is a panel, and anything else keeps its own spelling.
        let pages = children
            .iter()
            .map(|child| match child {
                AemNode::Panel { .. } => self.panel(child)?.page(),
                AemNode::Root { .. } => Err("a Root node inside the form".into()),
                _ => self.leaf(child)?.page(),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let master = Language::try_from(self.config.master_language.as_str())
            .map_err(|e| format!("master language: {e}"))?;
        let use_summary = self.var("use_summary") == "true";
        let metadata = FormMetadata {
            form_name: FormName::try_from(format!("AF_{}", self.var("form_code")).as_str())
                .map_err(|e| format!("the form code does not make a form name: {e}"))?,
            title: None,
            master_language: master.clone(),
            languages: [master].into_iter().collect(),
            dor: DorMode::Generate,
            data_model: DataModel::Unbound,
            toolbar: self.toolbar(use_summary)?,
            folder_path: Vec::new(),
            root_panel_layout: None,
            chrome: self.container()?,
            dam_chrome: Chrome::default(),
            page_content: self.page_content(title),
            root_panel: self.root_panel(use_summary)?,
            toolbar_chrome: Chrome {
                raw_attributes: [
                    ("jcr:title", "Toolbar"),
                    ("sling:resourceType", "fd/af/components/toolbar"),
                    ("css", ""),
                    ("name", "toolbar"),
                ]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
                raw_children: vec![raw(
                    "layout",
                    &[
                        ("jcr:primaryType", "nt:unstructured"),
                        ("sling:resourceType", "fd/af/layouts/toolbar/defaultToolbarLayout"),
                    ],
                    Vec::new(),
                )],
                slot: Some(0),
                items: None,
            },
        };
        Ok(AemForm { metadata, pages })
    }

    /// `jcr:content`: the page's attributes, and the header and footer
    /// paragraph systems around the container.
    fn page_content(&self, title: &str) -> Chrome {
        let base = self.var("custom_resource_type_base");
        let attributes = [
            ("cq:deviceGroups", "[/etc/mobile/groups/responsive]".to_owned()),
            ("cq:lastModified", MODIFIED_LOCAL.to_owned()),
            ("cq:lastModifiedBy", self.author().to_owned()),
            ("cq:template", self.var("template_path")),
            ("jcr:title", self.config.form_code.clone()),
            ("sling:resourceType", self.var("page_resource_type")),
        ];
        let header = raw(
            "parsys1",
            &[
                ("jcr:primaryType", "nt:unstructured"),
                ("sling:resourceType", "wcm/foundation/components/responsivegrid"),
            ],
            vec![
                raw(
                    "guideheader",
                    &[
                        ("jcr:primaryType", "nt:unstructured"),
                        ("sling:resourceType", &format!("{base}/controls/guideheader")),
                    ],
                    Vec::new(),
                ),
                raw(
                    "guideformtitle",
                    &[
                        ("jcr:primaryType", "nt:unstructured"),
                        ("sling:resourceType", &format!("{base}/controls/formtitle")),
                        ("_value", &format!("<p>{title}</p>")),
                        ("css", "guideformtitle container"),
                        ("guideNodeClass", "guideTextDraw"),
                        ("name", "formTitle"),
                        ("textIsRich", "true"),
                    ],
                    Vec::new(),
                ),
            ],
        );
        let footer = raw(
            "parsys2",
            &[
                ("jcr:primaryType", "nt:unstructured"),
                ("sling:resourceType", "wcm/foundation/components/responsivegrid"),
            ],
            vec![raw(
                "guidefooter",
                &[
                    ("jcr:primaryType", "nt:unstructured"),
                    ("sling:resourceType", &format!("{base}/controls/guidefooter")),
                ],
                Vec::new(),
            )],
        );
        Chrome {
            raw_attributes: attributes.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
            raw_children: vec![header, footer],
            slot: Some(1),
            items: None,
        }
    }

    /// `guideContainer`: the form's configuration, its layout, and what follows
    /// the root panel (autosave, signer, print settings, dictionaries), and,
    /// when the package binds to its schema, the schema it binds to.
    fn container(&self) -> Result<Chrome, LowerError> {
        let mut attributes: BTreeMap<String, String> = [
            ("jcr:lastModified", MODIFIED_LOCAL.to_owned()),
            ("jcr:lastModifiedBy", self.author().to_owned()),
            ("sling:resourceType", format!("{}/guideContainer", self.var("resource_type_base"))),
            ("actionType", self.var("action_type")),
            ("autoSaveStrategyType", "fd/fp/components/actions/autosave/timebased".to_owned()),
            ("clientLibRef", self.var("client_lib_ref")),
            ("disableSwipeGesture", "{Boolean}false".to_owned()),
            ("dorType", self.var("dor_type")),
            ("enableFocusOnFirstField", "{Boolean}true".to_owned()),
            ("enableLayoutLayer", "false".to_owned()),
            ("guideCss", "guideContainer".to_owned()),
            ("name", "guide1".to_owned()),
            ("textIsRich", "true".to_owned()),
            ("thankYouMessage", "Thank you for submitting the form.".to_owned()),
            ("thankYouOption", "page".to_owned()),
            ("useExistingAF", "false".to_owned()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        for (key, var) in [("dorTemplateRef", "dor_template_ref"), ("redirect", "redirect_url"), ("themeRef", "theme_ref")] {
            if let Some(value) = self.optional_var(var) {
                attributes.insert(key.to_owned(), value);
            }
        }
        if self.config.bind_to_xsd {
            let xsd_ref = self
                .config
                .xsd_ref()
                .ok_or("a form bound to its schema needs the profile's `xsd_dir`")?;
            let root_element = self
                .config
                .xsd_config
                .as_ref()
                .ok_or("a form bound to its schema needs the profile's XSD config")?
                .root_element_name();
            attributes.insert("schemaType".to_owned(), "xmlschema".to_owned());
            attributes.insert("xsdRef".to_owned(), xsd_ref);
            attributes.insert("xsdRootElement".to_owned(), root_element);
        }
        let layout = raw(
            "layout",
            &[
                ("jcr:primaryType", "nt:unstructured"),
                ("sling:resourceType", "fd/af/layouts/defaultGuideLayout"),
                ("mobileLayout", "fd/af/layouts/mobile/step"),
                ("toolbarPosition", "Bottom"),
            ],
            Vec::new(),
        );
        let auto_save = raw(
            "autoSaveInfo",
            &[("jcr:primaryType", "nt:unstructured"), ("metadataselector", "global")],
            Vec::new(),
        );
        let signer = raw(
            "signerInfo",
            &[
                ("jcr:primaryType", "nt:unstructured"),
                ("firstSignerFormFiller", "false"),
                ("workflowType", "SEQUENTIAL"),
            ],
            vec![raw(
                "signer0",
                &[
                    ("jcr:primaryType", "nt:unstructured"),
                    ("countryCode", "undefined"),
                    ("countryCodeSource", "undefined"),
                    ("email", "undefined"),
                    ("emailSource", "undefined"),
                    ("phone", "undefined"),
                    ("phoneSource", "undefined"),
                    ("securityOption", "undefined"),
                    ("signerTitle", "Signer One"),
                ],
                Vec::new(),
            )],
        );
        let dictionary = raw(
            "dictionary",
            &[("jcr:primaryType", "nt:unstructured")],
            self.config
                .expand_languages()
                .iter()
                .map(|lang| raw(lang, &[("jcr:primaryType", "nt:unstructured")], Vec::new()))
                .collect(),
        );
        let assets = raw("assets", &[("jcr:primaryType", "nt:unstructured")], vec![dictionary]);
        Ok(Chrome {
            raw_attributes: attributes,
            raw_children: vec![layout, auto_save, signer, self.print_view(), assets],
            slot: Some(1),
            items: None,
        })
    }

    /// The print settings: the DoR template and its branding.
    fn print_view(&self) -> RawJcrNode {
        let nt = ("jcr:primaryType", "nt:unstructured");
        let textarea = "granite/ui/components/coral/foundation/form/textarea";
        let plain = |tag: &str| raw(tag, &[nt], Vec::new());
        let header = raw(
            "Header",
            &[nt],
            vec![raw(
                "items",
                &[nt],
                vec![
                    raw(
                        "AF_LOGO_IMAGE",
                        &[
                            ("jcr:lastModified", MODIFIED_LOCAL),
                            ("jcr:lastModifiedBy", self.author()),
                            nt,
                            ("valueFrom", "template"),
                        ],
                        Vec::new(),
                    ),
                    raw(
                        "FormType",
                        &[nt, ("resourceType", textarea), ("value", &self.var("form_type")), ("valueFrom", "     ")],
                        Vec::new(),
                    ),
                    raw(
                        "HeaderInfo",
                        &[nt],
                        vec![raw(
                            "items",
                            &[nt],
                            vec![
                                raw("AF_HEADER_TEXT", &[nt, ("resourceType", textarea), ("valueFrom", "template")], Vec::new()),
                                raw("AF_FORM_TITLE", &[nt, ("resourceType", textarea), ("valueFrom", "formTitle")], Vec::new()),
                            ],
                        )],
                    ),
                    raw(
                        "Adressblock",
                        &[nt],
                        vec![raw(
                            "items",
                            &[nt],
                            vec![raw(
                                "senderAddressTitle",
                                &[nt, ("resourceType", textarea), ("value", "Banking Relationship"), ("valueFrom", "     ")],
                                Vec::new(),
                            )],
                        )],
                    ),
                ],
            )],
        );
        let masterpage = raw(
            "masterpage0",
            &[nt],
            vec![raw(
                "items",
                &[nt],
                vec![
                    plain("txtBankingRelationship"),
                    raw("ShowBankingRelationship", &[nt, ("value", "1")], Vec::new()),
                    header,
                    plain("formId"),
                    plain("displayLanguage"),
                    plain("language"),
                    plain("footerVersion"),
                    plain("mandator"),
                    plain("footerFormCode"),
                    plain("footerFormVersionDate"),
                    raw("APPCode", &[nt, ("value", "AFC")], Vec::new()),
                    plain("footerFreeText"),
                ],
            )],
        );
        let print = raw(
            "print",
            &[
                ("jcr:created", MODIFIED_LOCAL),
                ("jcr:lastModified", MODIFIED_LOCAL),
                ("jcr:lastModifiedBy", self.author()),
                nt,
                ("sling:resourceType", "fd/af/authoring/components/dor/dorProperties"),
                ("accentColor", "#04a6cb"),
                ("alignment", "dorFieldVerticalAlignment"),
                ("fontFamily", "Arial"),
                ("hidePanelDescriptions", "true"),
                ("includeUnboundFields", "true"),
                ("metaTemplateRef", &self.var("meta_template_ref")),
                ("optionSeparator", ", "),
                ("optionsNumberInHorizontalAlign", "4"),
                ("showSelectedOptions", "false"),
            ],
            vec![raw("branding", &[nt], vec![raw("items", &[nt], vec![masterpage])])],
        );
        raw("view", &[nt], vec![print])
    }

    /// `rootPanel`: its attributes and layout, and its `items` holding the
    /// form metadata fragment before the steps and the summary (or the preview
    /// step) after them.
    fn root_panel(&self, use_summary: bool) -> Result<Chrome, LowerError> {
        let layout = raw(
            "layout",
            &[
                ("jcr:primaryType", "nt:unstructured"),
                ("sling:resourceType", &self.var("wizard_layout")),
                ("enableLayoutOptimization", "true"),
                ("guideNavigatorTab", "wizard-tab"),
                ("toolbarPosition", "Bottom"),
            ],
            Vec::new(),
        );
        let after = raw_of(if use_summary { self.summary_node() } else { self.preview_node() }?)?;
        Ok(Chrome {
            raw_attributes: [
                ("jcr:lastModified", MODIFIED_LOCAL.to_owned()),
                ("jcr:lastModifiedBy", self.author().to_owned()),
                ("jcr:title", "Root Panel".to_owned()),
                ("completionExpReq", "{Boolean}true".to_owned()),
                ("dorExcludeDescription", "true".to_owned()),
                ("dorExcludeTitle", "true".to_owned()),
                ("guideNodeClass", "rootPanelNode".to_owned()),
                ("name", "guideRootPanel".to_owned()),
                ("panelSetType", "Navigable".to_owned()),
                ("validateOnStepCompletion", "{Boolean}true".to_owned()),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
            raw_children: vec![layout],
            slot: Some(1),
            items: Some(Box::new(Chrome {
                raw_attributes: self.grid_items().into_iter().collect(),
                raw_children: vec![self.form_metadata(), after],
                slot: Some(1),
                items: None,
            })),
        })
    }

    /// The form-metadata fragment the UBS client library reads.
    fn form_metadata(&self) -> RawJcrNode {
        let layout = self.default_layout();
        raw(
            "fragment_formmetadata",
            &[
                ("jcr:primaryType", "nt:unstructured"),
                ("jcr:title", "FormMetadata"),
                ("sling:resourceType", &self.controls("panel")),
                ("dorExcludeDescription", "true"),
                ("dorExcludeTitle", "true"),
                ("dorExclusion", "true"),
                ("summaryExclusion", "true"),
                ("fragRef", "/content/dam/formsanddocuments/afforms_global_fragmentlib/formmetadata"),
                ("guideNodeClass", "guidePanel"),
                ("name", "FormMetadata"),
                ("textIsRich", "true"),
                ("visible", "false"),
            ],
            vec![
                self.layout(Some("1")),
                raw(
                    "items",
                    &[("jcr:primaryType", "nt:unstructured"), ("sling:resourceType", &layout)],
                    Vec::new(),
                ),
            ],
        )
    }
}

impl Lower<'_> {
    /// A messagebox of the summary or preview step.
    fn messagebox(&self, tag: &str, name: &str, attrs: &[(&str, &str)]) -> Result<Node, LowerError> {
        let mut el = El::new(tag, name)
            .attr("sling:resourceType", self.controls("messagebox"))
            .attr("guideNodeClass", "guideTextDraw");
        for (k, v) in attrs {
            el = el.attr(k, *v);
        }
        el.node()
    }

    fn summary_node(&self) -> Result<Node, LowerError> {
        let langs: Vec<String> = self.config.canonical_languages().iter().map(|l| l.to_uppercase()).collect();
        let master = self.var("metadata_master_language");
        let master = if langs.len() == 1 {
            langs[0].clone()
        } else if langs.contains(&master) {
            master
        } else {
            "EN".to_owned()
        };
        let xfa = |key: &str| self.config.xfa_vars.get(key).cloned().unwrap_or_default();
        let or = |key: &str, fallback: &str| {
            Some(xfa(key)).filter(|v| !v.is_empty()).or_else(|| self.config.xfa_vars.get(fallback).cloned()).unwrap_or_default()
        };
        let nonempty_or = |key: &str, default: &str| Some(xfa(key)).filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_owned());
        let nt = ("jcr:primaryType", "nt:unstructured");
        let cdok = raw(
            "item0",
            &[
                nt,
                ("formrange_cdokinfo", &or("formrange_cdokinfo", "Footer_Line_txtformid")),
                ("formrange_partnerlevel", &nonempty_or("formrange_partnerlevel", "false")),
                ("formrange_releasedate", &or("formrange_releasedate", "Footer_Line_txtversiondate")),
                ("formrange_version", &xfa("formrange_version")),
            ],
            Vec::new(),
        );
        let entity = raw(
            "item0",
            &[
                nt,
                ("formrange_clpmandatory", &nonempty_or("formrange_clpmandatory", "false")),
                ("formrange_entity", &xfa("formrange_entity")),
                ("formrange_language", &langs.join(",")),
            ],
            vec![raw("cdoks", &[nt], vec![cdok])],
        );
        let metadata = self
            .stamped(El::new("metadata", "metadataTextDraw"), CREATED)
            .attr("sling:resourceType", self.controls("metadata"))
            .attr("_value", "Metadata")
            .attr("dorExclusion", "true")
            .attr("summaryExclusion", "true")
            .attr("formrange_afmasterlanguage", master)
            .attr("formrange_aftype", "Single")
            .attr("formrange_code", self.var("form_code"))
            .attr("guideNodeClass", "guideTextDraw")
            .attr("visible", "false")
            .after(raw("entities", &[nt], vec![entity]));
        let summary = self
            .stamped(El::new("summary", "summaryComponent"), CREATED)
            .attr("jcr:title", "Summary")
            .attr("sling:resourceType", self.controls("summary"))
            .attr("autofillFieldKeyword", "name")
            .attr("css", "widget_ajila_forms_summary")
            .attr("dorFieldStyling", "Default")
            .attr("guideNodeClass", "guideTextBox")
            .attr("replaceEmptyValues", "true")
            .attr("showStaticText", "true")
            .attr("textIsRich", "[true,true,true]");
        let dor_options = self
            .stamped(El::new("doroptionsubs", "doroptionsubs"), CREATED)
            .attr("sling:resourceType", self.controls("dorOptionsUBS"))
            .attr("_value", "Further configurations for the document of record")
            .attr("dorExclusion", "true")
            .attr("summaryExclusion", "true")
            .attr("guideNodeClass", "guideTextDraw")
            .attr("visible", "false");
        let items = vec![
            self.messagebox("messagebox_ElsigCheck", "messagebox_ElsigCheck", &[
                ("css", "messagebox-ElsigCheck ubs-margin-10"),
                ("hideTitle", "{Boolean}true"),
                ("i18nBodyId", "ajila-forms-ubs-signature-level-error"),
                ("visible", "{Boolean}true"),
            ])?,
            summary.node()?,
            self.messagebox("messagebox_SubmissionError", "submitErrorMessage", &[
                ("messageboxBody", "<p>The form could not be sent. Please try again later.</p>"),
                ("messageboxTitle", "Submission failed"),
                ("messageboxType", "{Long}4"),
                ("visible", "{Boolean}false"),
            ])?,
            dor_options.node()?,
            metadata.node()?,
        ];
        self.stamped(El::new("summarypanel", "summaryPanel"), CREATED)
            .attr("jcr:title", "Summary of form information")
            .attr("sling:resourceType", self.controls("panel"))
            .attr("dorExclusion", "true")
            .attr("dorFieldStyling", "Default")
            .attr("summaryExclusion", "true")
            .attr("guideNodeClass", "guidePanel")
            .attr("textIsRich", "true")
            .attr("validateOnStepCompletion", "{Boolean}false")
            .attr("visible", "{Boolean}true")
            .items(
                vec![
                    ("jcr:primaryType".into(), "nt:unstructured".into()),
                    ("sling:resourceType".into(), "fd/af/layouts/gridFluidLayout2".into()),
                ],
                items,
            )
            .after(self.layout(Some("1")))
            .node()
    }

    fn preview_node(&self) -> Result<Node, LowerError> {
        let carousel = self
            .stamped(El::new("carousel", "carouselPreview"), CREATED)
            .attr("jcr:title", "Preview Carousel")
            .attr("sling:resourceType", self.controls("carousel"))
            .attr("arrows", "true")
            .attr("autofillFieldKeyword", "name")
            .attr("css", "widget_ajila_forms_carousel")
            .attr("displayPatternType", "custom")
            .attr("displayPictureClause", "\\[0-9]")
            .attr("dorExclusion", "true")
            .attr("summaryExclusion", "true")
            .attr("guideNodeClass", "guideTextBox")
            .attr("initScript", "com.ajila.forms.control.carousel.initialize(this)")
            .attr("lazyLoadingStrategy", "ondemand")
            .attr("placeholderText", "Message")
            .attr("showDots", "true")
            .attr("slidesToScroll", "1")
            .attr("slidesToShow", "1")
            .attr("textIsRich", "[true,true,true]")
            .attr("visible", "{Boolean}false");
        let error = El::new("messagebox_CarouselPreview", "previewErrorMessage")
            .attr("sling:resourceType", self.controls("messagebox-CarouselPreviewError"))
            .attr("buttonAction", "window.com.ajila.forms.control.messagebox_carouselpreview_error.initCarouselPreview()")
            .attr("css", "ubs-margin-10")
            .attr("guideNodeClass", "guideTextDraw")
            .attr("i18nBodyId", "ajila-forms-ubs-errorbox-carousel-message")
            .attr("i18nButtonLabelId", "ajila-forms-ubs-errorbox-carousel-button-label")
            .attr("i18nTitleId", "ajila-forms-ubs-errorbox-carousel-title")
            .attr("messageboxType", "{Long}4")
            .attr("showButton", "{Boolean}true")
            .attr("visible", "{Boolean}false");
        let items = vec![
            self.messagebox("messagebox_ElsigCheck", "messagebox_ElsigCheck", &[
                ("css", "messagebox-ElsigCheck ubs-margin-10"),
                ("hideTitle", "{Boolean}true"),
                ("i18nBodyId", "ajila-forms-ubs-signature-level-error"),
                ("visible", "{Boolean}true"),
            ])?,
            self.messagebox("messagebox_SubmissionInfo", "previewInformation", &[
                ("hideTitle", "{Boolean}true"),
                ("messageboxBody", "<p>By clicking on \"Preview\", you can review your document before submission.</p><p>After clicking \"Submit\", you will no longer be able to edit the document and the PDF will be created for signing.</p>"),
                ("visible", "{Boolean}true"),
            ])?,
            carousel.node()?,
            error.node()?,
            self.messagebox("messagebox_SubmissionError", "submitErrorMessage", &[
                ("messageboxBody", "<p>The form could not be sent. Please try again later.</p>"),
                ("messageboxTitle", "Submission failed"),
                ("messageboxType", "{Long}4"),
                ("visible", "{Boolean}false"),
            ])?,
        ];
        self.stamped(El::new("previewpanel", "preview"), CREATED)
            .attr("jcr:title", "Preview")
            .attr("sling:resourceType", self.controls("panel"))
            .attr("dorExclusion", "true")
            .attr("summaryExclusion", "true")
            .attr("guideNodeClass", "guidePanel")
            .attr("textIsRich", "true")
            .attr("validateOnStepCompletion", "{Boolean}false")
            .items(self.grid_items(), items)
            .after(self.layout(Some("1")))
            .node()
    }

    /// The wizard's buttons, each with its rules.
    fn toolbar(&self, use_summary: bool) -> Result<Vec<Node>, LowerError> {
        let path = |name: &str| format!("guide.guideRootPanel.toolbar.{name}");
        let click = |name: &str, contents: &[&str]| {
            contents
                .iter()
                .map(|c| EventScript::event(path(name), ScriptEvent::Click, *c))
                .collect::<Vec<_>>()
        };
        let next_visible = |name: &str, order| EventScript {
            field: path(name),
            event: ScriptEvent::Navigation,
            content: "this.visible=(!this.panel.navigationContext.hasNextItem);".into(),
            order,
            model: None,
            archetype: None,
        };
        let button = |tag: &str, name: &str, attrs: &[(&str, &str)], rules: Vec<(ScriptEvent, Vec<EventScript>)>| {
            let mut el = El::new(tag, name);
            for (k, v) in attrs {
                el = el.attr(k, *v);
            }
            el.attr("dorExclusion", "true")
                .attr("summaryExclusion", "true")
                .attr("guideNodeClass", "guideButton")
                .after(empty_rules())
                .after(scripts(&rules))
                .node()
        };
        let mut next_click = Vec::new();
        if use_summary {
            next_click.extend(click("nextitemnav", &["window.ajila.forms.ubs.control.summary.setSummaryData(guideRootPanel);"]));
        }
        next_click.extend(click("nextitemnav", &["window.com.ajila.forms.ubs.navigation.nextStep(this);"]));
        let mut buttons = vec![
            button(
                "nextitemnav",
                "nextitemnav",
                &[
                    ("fd:targetVersion", "1.1"),
                    ("jcr:title", "Next"),
                    ("sling:resourceType", "fd/af/components/actions/nextitemnav"),
                    ("type", "moveNext"),
                ],
                vec![(ScriptEvent::Click, next_click)],
            )?,
            button(
                "submit",
                "submit",
                &[
                    ("jcr:title", "Submit"),
                    ("sling:resourceType", "fd/af/components/actions/submit"),
                    ("type", "submit"),
                ],
                vec![
                    (ScriptEvent::Click, click("submit", &["window.forms.ubs.navigation.submit(submitErrorMessage);"])),
                    (ScriptEvent::Navigation, vec![next_visible("submit", u2s_mapper_aem::script::BodyOrder::FieldFirst)]),
                ],
            )?,
            button(
                "previtemnav",
                "previtemnav",
                &[
                    ("fd:targetVersion", "1.1"),
                    ("jcr:title", "Back"),
                    ("sling:resourceType", "fd/af/components/actions/previtemnav"),
                    ("type", "movePrev"),
                ],
                vec![(ScriptEvent::Click, click("previtemnav", &["window.forms.ubs.navigation.previousStep(this);"]))],
            )?,
        ];
        if !use_summary {
            buttons.push(button(
                "preview",
                "preview",
                &[
                    ("jcr:title", "Preview"),
                    ("sling:resourceType", "ajila-forms-customers/ajila-forms-ubs/components/controls/tertiarybutton"),
                    ("css", "previewGenerationButton"),
                    ("textIsRich", "[true,true]"),
                    ("type", "Button"),
                ],
                vec![
                    (
                        ScriptEvent::Click,
                        click(
                            "preview",
                            &[
                                "com.ajila.forms.control.carousel.initializeForPreview(carouselPreview, undefined, previewErrorMessage);",
                                "carouselPreview.visible = true;\n\n",
                            ],
                        ),
                    ),
                    (ScriptEvent::Navigation, vec![next_visible("preview", u2s_mapper_aem::script::BodyOrder::ContentFirst)]),
                ],
            )?);
        }
        buttons.push(button(
            "guidebutton",
            "fwbSaveProgress",
            &[
                ("jcr:title", "Save Progress"),
                ("sling:resourceType", "fd/af/components/guidebutton"),
                ("dorFieldStyling", &self.var("dor_field_styling")),
                ("textIsRich", "[true,true,true]"),
                ("type", "Button"),
            ],
            vec![(ScriptEvent::Click, click("fwbSaveProgress", &["window.forms.ubs.fwb.saveFormData();"]))],
        )?);
        Ok(buttons)
    }
}

/// A component built here, held as a raw element: for the summary and preview
/// steps, which sit beside the form's steps in `rootPanel`'s `items` rather
/// than among them. Building them as components first keeps one way of
/// spelling a UBS component.
fn raw_of(node: Node) -> Result<RawJcrNode, LowerError> {
    let Node::Component { common, properties, children } = node else {
        return Err("a fixed step's element is not a component".to_owned());
    };
    let mut attributes: BTreeMap<String, String> = BTreeMap::new();
    attributes.insert("jcr:primaryType".into(), "nt:unstructured".into());
    attributes.insert("name".into(), common.name.as_str().to_owned());
    for (key, value) in properties {
        let JcrValue::Single(value) = value else {
            return Err(format!("`{}` on `{}` is not a single value", key.as_str(), common.name.as_str()));
        };
        attributes.insert(key.as_str().to_owned(), value);
    }
    attributes.extend(common.passthrough.raw_attributes.clone());
    let slot = common.passthrough.slot.unwrap_or(0);
    let mut raw_children = common.passthrough.raw_children.clone();
    if let Some(items) = &common.passthrough.items {
        let items_node = RawJcrNode {
            tag_name: "items".into(),
            attributes: items.raw_attributes.clone(),
            children: children.into_iter().map(raw_of).collect::<Result<_, _>>()?,
        };
        raw_children.insert(slot, items_node);
    }
    let tag_name = common
        .jcr_name
        .ok_or_else(|| format!("`{}` has no element name", common.name.as_str()))?;
    Ok(RawJcrNode {
        tag_name: tag_name.as_str().to_owned(),
        attributes,
        children: raw_children,
    })
}

/// `visible="{Boolean}false"` on a node that starts hidden.
fn hidden(el: El, visible: bool) -> El {
    el.attr_if(!visible, "visible", "{Boolean}false")
}

/// `showIfHidden` and `summaryExclusion`, which a node excluded from the DoR
/// carries too.
fn tail(el: El, attrs: &AemAttrs) -> El {
    el.attr_if(attrs.show_if_hidden, "showIfHidden", "true")
        .attr_if(attrs.dor_exclude || attrs.summary_exclude, "summaryExclusion", "true")
}

/// A number the profile writes only when it is set and not zero.
fn nonzero(value: Option<u32>) -> Option<String> {
    value.filter(|n| *n != 0).map(|n| n.to_string())
}

fn nonempty(value: &Option<String>) -> Option<String> {
    value.clone().filter(|v| !v.is_empty())
}

/// A choice's `options`, `[value=label,...]`.
fn options_attr(options: &[AemOption]) -> String {
    multi_value(options.iter().map(|o| option_pair(&o.value, &o.label)))
}

/// Which of a choice's option labels are rich text.
fn text_is_rich(options: &[AemOption]) -> String {
    format!(
        "[{}]",
        options
            .iter()
            .map(|o| o.label.contains('<').to_string())
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Every attribute name some UBS component writes itself (the union of
/// [`owned`]): a loaded raw attribute of one of these names never survives as
/// passthrough.
pub(super) fn owned_attribute_names() -> impl Iterator<Item = &'static str> {
    [
        owned::TEXTBOX,
        owned::TEXTBOX_MULTILINE,
        owned::EMAIL,
        owned::TELEPHONE,
        owned::NUMERICBOX,
        owned::DATEPICKER,
        owned::DROPDOWNLIST,
        owned::CHECKBOX,
        owned::RADIOBUTTON,
        owned::TEXTDRAW,
        owned::TITLEDRAW,
        owned::HTMLDISPLAYER,
        owned::MESSAGEBOX,
        owned::REPEATABLE,
        owned::FRAGMENT,
        owned::PREFACE,
        owned::FOOTNOTEPLACEHOLDER,
        owned::PANEL,
        owned::CONDITIONAL,
    ]
    .into_iter()
    .flatten()
    .copied()
}

/// The attributes each UBS component writes itself, whatever it writes this
/// time: a loaded raw attribute of one of these names gives way to the
/// component's own value, or its absence.
mod owned {
    pub const TEXTBOX: &[&str] = &[
        "alwaysInPdf", "assistPriority", "bindRef", "css", "dorColspan",
        "dorExcludeTitle", "dorExclusion", "dorFieldStyling", "dorHeaderSlot", "guideNodeClass",
        "jcr:created", "jcr:createdBy", "jcr:lastModified", "jcr:lastModifiedBy", "jcr:primaryType",
        "jcr:title", "mandatory", "maxChars", "name", "showIfHidden",
        "sling:resourceType", "summaryExclusion", "textIsRich", "visible",
    ];
    pub const TEXTBOX_MULTILINE: &[&str] = &[
        "alwaysInPdf", "bindRef", "css", "dorColspan", "dorExcludeTitle",
        "dorExclusion", "dorFieldStyling", "dorHeaderSlot", "guideNodeClass", "jcr:created",
        "jcr:createdBy", "jcr:lastModified", "jcr:lastModifiedBy", "jcr:primaryType", "jcr:title",
        "mandatory", "multiLine", "name", "showIfHidden", "sling:resourceType",
        "summaryExclusion", "visible",
    ];
    pub const EMAIL: &[&str] = &[
        "alwaysInPdf", "assistPriority", "autofillFieldKeyword", "bindRef", "dorColspan",
        "dorExcludeTitle", "dorExclusion", "dorFieldStyling", "dorHeaderSlot", "guideNodeClass",
        "jcr:created", "jcr:createdBy", "jcr:lastModified", "jcr:lastModifiedBy", "jcr:primaryType",
        "jcr:title", "mandatory", "maxChars", "name", "showIfHidden",
        "sling:resourceType", "summaryExclusion", "textIsRich", "validatePictureClause", "validatePictureClauseMessage",
        "validationPatternType", "visible",
    ];
    pub const TELEPHONE: &[&str] = &[
        "alwaysInPdf", "assistPriority", "autofillFieldKeyword", "bindRef", "css",
        "displayIsSameAsValidate", "displayPatternType", "displayPictureClause", "dorColspan", "dorExcludeTitle",
        "dorExclusion", "dorFieldStyling", "dorHeaderSlot", "guideNodeClass", "jcr:created",
        "jcr:createdBy", "jcr:lastModified", "jcr:lastModifiedBy", "jcr:primaryType", "jcr:title",
        "mandatory", "maxChars", "name", "showIfHidden", "sling:resourceType",
        "summaryExclusion", "textIsRich", "validatePictureClause", "validatePictureClauseMessage", "validationPatternType",
        "visible",
    ];
    pub const NUMERICBOX: &[&str] = &[
        "alwaysInPdf", "assistPriority", "bindRef", "css", "dorColspan",
        "dorExcludeTitle", "dorExclusion", "dorFieldStyling", "dorHeaderSlot", "guideNodeClass",
        "jcr:created", "jcr:createdBy", "jcr:lastModified", "jcr:lastModifiedBy", "jcr:primaryType",
        "jcr:title", "mandatory", "name", "showIfHidden", "sling:resourceType",
        "summaryExclusion", "textIsRich", "visible",
    ];
    pub const DATEPICKER: &[&str] = &[
        "alwaysInPdf", "bindRef", "css", "dorColspan", "dorExcludeTitle",
        "dorExclusion", "dorFieldStyling", "dorHeaderSlot", "guideNodeClass", "jcr:created",
        "jcr:createdBy", "jcr:lastModified", "jcr:lastModifiedBy", "jcr:primaryType", "jcr:title",
        "mandatory", "name", "placeholderText", "showIfHidden", "sling:resourceType",
        "summaryExclusion", "textIsRich", "validatePictureClause", "validatePictureClauseMessage", "validationPatternType",
        "visible", "yearRangeFrom", "yearRangeTo",
    ];
    pub const DROPDOWNLIST: &[&str] = &[
        "alwaysInPdf", "bindRef", "css", "dorColspan", "dorExcludeTitle",
        "dorExclusion", "dorFieldStyling", "dorHeaderSlot", "guideNodeClass", "jcr:created",
        "jcr:createdBy", "jcr:lastModified", "jcr:lastModifiedBy", "jcr:primaryType", "jcr:title",
        "mandatory", "name", "options", "showIfHidden", "sling:resourceType",
        "summaryExclusion", "textIsRich", "visible",
    ];
    pub const CHECKBOX: &[&str] = &[
        "alignment", "alwaysInPdf", "assistPriority", "bindRef", "css",
        "dorColspan", "dorExcludeTitle", "dorExclusion", "dorFieldStyling", "dorHeaderSlot",
        "guideNodeClass", "hideTitle", "jcr:created", "jcr:createdBy", "jcr:lastModified",
        "jcr:lastModifiedBy", "jcr:primaryType", "name", "options", "richTextOptions",
        "showIfHidden", "sling:resourceType", "summaryExclusion", "textIsRich", "visible",
    ];
    pub const RADIOBUTTON: &[&str] = &[
        "_value", "alignment", "alwaysInPdf", "bindRef", "css",
        "dorColspan", "dorExcludeTitle", "dorExclusion", "dorFieldStyling", "dorHeaderSlot",
        "guideNodeClass", "jcr:created", "jcr:createdBy", "jcr:lastModified", "jcr:lastModifiedBy",
        "jcr:primaryType", "jcr:title", "mandatory", "name", "options",
        "richTextOptions", "showIfHidden", "sling:resourceType", "summaryExclusion", "textIsRich",
        "visible",
    ];
    pub const TEXTDRAW: &[&str] = &[
        "_value", "alwaysInPdf", "css", "dorColspan", "dorExclusion",
        "dorFieldStyling", "dorHeaderSlot", "guideNodeClass", "jcr:created", "jcr:createdBy",
        "jcr:lastModified", "jcr:lastModifiedBy", "jcr:primaryType", "name", "showIfHidden",
        "sling:resourceType", "summaryExclusion", "textIsRich", "visible",
    ];
    pub const TITLEDRAW: &[&str] = &[
        "_value", "alwaysInPdf", "css", "dorColspan", "dorExclusion",
        "dorFieldStyling", "guideNodeClass", "headingLevel", "jcr:created", "jcr:createdBy",
        "jcr:lastModified", "jcr:lastModifiedBy", "jcr:primaryType", "name", "showIfHidden",
        "sling:resourceType", "summaryExclusion", "textIsRich", "visible",
    ];
    pub const HTMLDISPLAYER: &[&str] = &[
        "alwaysInPdf", "autofillFieldKeyword", "css", "dorColspan", "dorExclusion",
        "dorFieldStyling", "guideNodeClass", "initScript", "jcr:created", "jcr:createdBy",
        "jcr:lastModified", "jcr:lastModifiedBy", "jcr:primaryType", "name", "showIfHidden",
        "sling:resourceType", "summaryExclusion", "textIsRich", "visible",
    ];
    pub const MESSAGEBOX: &[&str] = &[
        "ariaLiveAttribute", "css", "dorExclusion", "guideNodeClass", "hideTitle",
        "initScript", "jcr:created", "jcr:createdBy", "jcr:lastModified", "jcr:lastModifiedBy",
        "jcr:primaryType", "messageboxBody", "messageboxType", "name", "sling:resourceType",
        "summaryExclusion", "textIsRich", "visible",
    ];
    pub const REPEATABLE: &[&str] = &[
        "alwaysInPdf", "css", "dorExcludeDescription", "dorExcludeTitle", "dorExclusion",
        "guideNodeClass", "jcr:created", "jcr:createdBy", "jcr:lastModified", "jcr:lastModifiedBy",
        "jcr:primaryType", "name", "showIfHidden", "sling:resourceType", "summaryExclusion",
        "textIsRich", "validateOnStepCompletion", "visible",
    ];
    pub const FRAGMENT: &[&str] = &[
        "alwaysInPdf", "bindRef", "css", "dorExcludeDescription", "dorExcludeTitle",
        "dorExclusion", "dorFieldStyling", "fragRef", "guideNodeClass", "jcr:created",
        "jcr:createdBy", "jcr:lastModified", "jcr:lastModifiedBy", "jcr:primaryType", "name",
        "showIfHidden", "sling:resourceType", "summaryExclusion", "textIsRich", "validateOnStepCompletion",
        "visible",
    ];
    pub const PREFACE: &[&str] = &[
        "css", "dorExclusion", "guideNodeClass", "jcr:created", "jcr:createdBy",
        "jcr:lastModified", "jcr:lastModifiedBy", "jcr:primaryType", "name", "sling:resourceType",
        "summaryExclusion", "textIsRich", "validateOnStepCompletion",
    ];
    pub const FOOTNOTEPLACEHOLDER: &[&str] = &[
        "_value", "guideNodeClass", "jcr:created", "jcr:createdBy", "jcr:lastModified",
        "jcr:lastModifiedBy", "jcr:primaryType", "jcr:title", "name", "sling:resourceType",
    ];
    pub const PANEL: &[&str] = &[
        "alwaysInPdf", "bindRef", "css", "dorColspan", "dorExcludeDescription",
        "dorExcludeTitle", "dorExclusion", "dorFieldStyling", "dorHeaderSlot", "guideNodeClass",
        "jcr:created", "jcr:createdBy", "jcr:lastModified", "jcr:lastModifiedBy", "jcr:primaryType",
        "jcr:title", "jumpToFieldButtonVisible", "name", "showIfHidden", "sling:resourceType",
        "summaryExclusion", "textIsRich", "validateOnStepCompletion", "visible",
    ];
    pub const CONDITIONAL: &[&str] = &[
        "alwaysInPdf", "css", "dorColspan", "dorExcludeDescription", "dorExcludeTitle",
        "dorExclusion", "dorFieldStyling", "guideNodeClass", "hideTitle", "jcr:created",
        "jcr:createdBy", "jcr:lastModified", "jcr:lastModifiedBy", "jcr:primaryType", "jcr:title",
        "name", "showIfHidden", "sling:resourceType", "summaryExclusion", "textIsRich",
        "validateOnStepCompletion", "visible",
    ];
}
