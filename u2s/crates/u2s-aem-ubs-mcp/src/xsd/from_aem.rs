//! XSD generation from an AEM node tree.
//!
//! The schema and the `bindRef` of every node are produced by **one** walk, so
//! they cannot disagree: an element is emitted and its bind path recorded at the
//! same moment. That is the whole point of deriving the schema from `AemNode`.
//!
//! # Shape rules
//!
//! Two rules do most of the work and need no configuration:
//!
//! 1. **Panels are transparent.** A plain layout panel contributes no XSD level;
//!    its children bubble up to the nearest enclosing element. Only a panel that
//!    repeats or that came from a fragment produces one. This is what collapses
//!    a deeply nested AEM layout into a flat schema.
//!
//!    One addition to UBS's rule, controlled by `groupPagePanels`: a titled
//!    *page* panel also produces a level. Our forms are field-dense enough that
//!    flattening them makes many names collide into ordinal suffixes — see the
//!    field's docs for the measured cost.
//! 2. **`ref=` versus `name=`/`type=` is a lookup, not a convention.** If the
//!    resolved element name is declared as a global element in the profile's
//!    type library, `<xs:element ref="…"/>` is emitted; otherwise
//!    `<xs:element name="…" type="…"/>`.
//!
//! Everything else — which nodes to ignore, the element names that cannot be
//! derived from a title, and the occurrence values — comes from
//! `profiles/{name}/xsd/config.toml`. See [`AemElementRule`].

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::aem::{AemNode, ParsedFragment};

use super::{
    AemElementRule, AemRuleSubject, Occurs, XsdConfig, XsdNode, XsdSchema, to_xsd_element_name,
};

/// The schema derived from an AEM tree, plus the `bindRef` each node earned.
pub struct AemXsdResult {
    /// The generated schema.
    pub schema: XsdSchema,
    /// Node uuid → absolute bind path (e.g. `/UBSAF_ABFA/EmailAddressInstruction`).
    /// Only nodes that own an XSD element appear.
    pub bind_refs: HashMap<Uuid, String>,
}

/// Derive an [`XsdSchema`] from a final AEM tree and record every `bindRef`.
///
/// `fragments` is the profile's parsed fragment library; it maps a `fragRef`
/// onto the XSD type the fragment binds to (`fragmentModelRoot`).
pub fn generate_xsd_from_aem(
    root: &AemNode,
    config: &XsdConfig,
    fragments: &[ParsedFragment],
) -> AemXsdResult {
    // The profile's full library first, then whatever the caller passed, so an
    // explicitly supplied fragment wins over the indexed one.
    let mut frag_types: HashMap<&str, &str> = config
        .fragment_types
        .iter()
        .map(|(frag_ref, ty)| (frag_ref.as_str(), ty.as_str()))
        .collect();
    frag_types.extend(
        fragments
            .iter()
            .map(|f| (f.frag_ref.as_str(), f.xsd_type_name.as_str())),
    );

    let root_name = config.root_element_name();
    let root_path = format!("/{root_name}");

    let mut state = BuildState {
        includes: Vec::new(),
        seen_includes: HashSet::new(),
        bind_refs: HashMap::new(),
    };
    // Emitted first and unconditionally, ahead of anything the walk discovers.
    for path in &config.profile.always_include {
        state.note_include(path);
    }

    let children = match root {
        AemNode::Root { children, .. } => children.as_slice(),
        single => std::slice::from_ref(single),
    };

    let mut body = Vec::new();
    let mut used = HashSet::new();
    walk(
        children,
        &root_path,
        &mut body,
        &mut used,
        &mut state,
        &Ctx {
            config,
            frag_types: &frag_types,
        },
    );

    let schema = XsdSchema {
        includes: state.includes,
        root: XsdNode::Element {
            name: root_name,
            type_ref: None,
            min_occurs: None,
            max_occurs: None,
            content: Some(Box::new(XsdNode::ComplexType {
                name: None,
                sequence: body,
            })),
        },
    };

    AemXsdResult {
        schema,
        bind_refs: state.bind_refs,
    }
}

/// Convenience wrapper returning the serialised schema.
pub fn generate_xsd_string_from_aem(
    root: &AemNode,
    config: &XsdConfig,
    fragments: &[ParsedFragment],
) -> String {
    generate_xsd_from_aem(root, config, fragments)
        .schema
        .to_xml()
}

/// Write `refs` into the tree by uuid, clearing `bind_ref` on every node absent
/// from the map. Idempotent.
pub fn apply_bind_refs(root: &mut AemNode, refs: &HashMap<Uuid, String>) {
    visit_bind_ref_slots(root, &mut |uuid, slot| {
        *slot = refs.get(&uuid).cloned();
    });
}

/// Call `f` with the uuid and `bind_ref` slot of every node that has one.
fn visit_bind_ref_slots(node: &mut AemNode, f: &mut impl FnMut(Uuid, &mut Option<String>)) {
    macro_rules! slot {
        ($uuid:expr, $bind_ref:expr) => {{
            let uuid = *$uuid;
            f(uuid, $bind_ref);
        }};
    }

    match node {
        AemNode::Root { children, .. } => {
            for child in children {
                visit_bind_ref_slots(child, f);
            }
        }
        AemNode::Panel {
            uuid,
            bind_ref,
            children,
            ..
        }
        | AemNode::Repeatable {
            uuid,
            bind_ref,
            children,
            ..
        } => {
            slot!(uuid, bind_ref);
            for child in children {
                visit_bind_ref_slots(child, f);
            }
        }
        AemNode::TextField { uuid, bind_ref, .. }
        | AemNode::NumberField { uuid, bind_ref, .. }
        | AemNode::DatePicker { uuid, bind_ref, .. }
        | AemNode::Dropdown { uuid, bind_ref, .. }
        | AemNode::Checkbox { uuid, bind_ref, .. }
        | AemNode::RadioButton { uuid, bind_ref, .. }
        | AemNode::Fragment { uuid, bind_ref, .. }
        | AemNode::Custom { uuid, bind_ref, .. } => slot!(uuid, bind_ref),
        AemNode::TextDraw { .. }
        | AemNode::TitleDraw { .. }
        | AemNode::HtmlDisplayer { .. }
        | AemNode::MessageBox { .. }
        | AemNode::Preface { .. }
        | AemNode::Appendix { .. }
        | AemNode::FootnotePlaceholder { .. } => {}
    }
}

// ============================================================================
// Walk
// ============================================================================

struct Ctx<'a> {
    config: &'a XsdConfig,
    /// `fragRef` → the XSD type it binds to (`fragmentModelRoot`).
    frag_types: &'a HashMap<&'a str, &'a str>,
}

struct BuildState {
    /// Include paths in first-appearance order.
    includes: Vec<String>,
    seen_includes: HashSet<String>,
    bind_refs: HashMap<Uuid, String>,
}

impl BuildState {
    fn note_include(&mut self, path: &str) {
        if self.seen_includes.insert(path.to_string()) {
            self.includes.push(path.to_string());
        }
    }
}

/// What a node contributes to the schema.
enum Emit {
    /// Contributes nothing, not even through its children.
    Skip,
    /// Contributes no element of its own; children bubble up.
    Transparent,
    /// `<xs:element ref="…"/>` — a global element in the type library.
    Ref {
        name: String,
        occurs: Occurs,
        /// The type whose declaring file must be included, when known.
        ///
        /// Two schemas can declare the same global element name with different
        /// types — `AccountHolder` is `AccountHolderType` in
        /// `ContractualPartner.xsd` and `ContractualPartnerGenericType` in
        /// `ContractualPartnerGeneric.xsd` — so resolving the include by element
        /// name alone can pull in the wrong file, leaving a `ref=` pointing at an
        /// element of the wrong type.
        include_hint: Option<String>,
    },
    /// `<xs:element name="…" type="…"/>` — a typed leaf.
    Leaf {
        name: String,
        type_ref: String,
        occurs: Occurs,
    },
    /// `<xs:element name="…"><xs:complexType><xs:sequence>` around its children.
    Group { name: String, occurs: Occurs },
}

/// Walk `nodes`, appending their elements to the `out` sequence.
///
/// `used` holds the element names already taken in that sequence. It is owned by
/// the sequence, not by the call: a transparent panel appends into its parent's
/// sequence and so must share the parent's scope, while a group starts a fresh
/// one.
fn walk(
    nodes: &[AemNode],
    parent_path: &str,
    out: &mut Vec<XsdNode>,
    used: &mut HashSet<String>,
    st: &mut BuildState,
    ctx: &Ctx,
) {
    for (index, node) in nodes.iter().enumerate() {
        // The next sibling that could carry data; presentational nodes are
        // skipped so a stray text draw does not hide the fragment behind it.
        let next = nodes[index + 1..].iter().find(|n| !is_presentational(n));

        match classify(node, next, ctx) {
            Emit::Skip => {}
            Emit::Transparent => {
                if let Some(children) = child_nodes(node) {
                    walk(children, parent_path, out, used, st, ctx);
                }
            }
            Emit::Ref {
                name,
                occurs,
                include_hint,
            } => {
                let name = unique_name(name, used);
                bind(node, parent_path, &name, st);
                note_include_for(include_hint.as_deref().unwrap_or(&name), st, ctx);
                out.push(XsdNode::Ref {
                    ref_name: name,
                    min_occurs: occurs.min,
                    max_occurs: occurs.max,
                });
            }
            Emit::Leaf {
                name,
                type_ref,
                occurs,
            } => {
                let name = unique_name(name, used);
                bind(node, parent_path, &name, st);
                note_include_for(&type_ref, st, ctx);
                out.push(XsdNode::Element {
                    name,
                    type_ref: Some(type_ref),
                    min_occurs: occurs.min,
                    max_occurs: occurs.max,
                    content: None,
                });
            }
            // A repeatable wrapping exactly one element *is* that element,
            // repeated: the repeatable contributes cardinality, the child
            // contributes identity. UBS authors this as a single panel carrying
            // both `fragRef` and `maxOccur`, which is why their schema has
            //
            //     <xs:element name="AuthRepSignature" type="SignatureType" maxOccurs="50"/>
            //
            // where an uncollapsed walk would emit a nameless group around it.
            // Restricted to a child that resolves to exactly one element, so the
            // walk below cannot bind anything at the wrong path.
            Emit::Group { name, occurs }
                if matches!(node, AemNode::Repeatable { .. })
                    && single_element_child(node, ctx).is_some() =>
            {
                let child = single_element_child(node, ctx).expect("guarded above");
                let mut inner = Vec::new();
                walk(
                    std::slice::from_ref(child),
                    parent_path,
                    &mut inner,
                    used,
                    st,
                    ctx,
                );
                debug_assert_eq!(inner.len(), 1, "a Ref/Leaf child emits one element");
                let Some(mut element) = inner.pop() else {
                    continue;
                };
                set_occurs(&mut element, occurs);

                // The repeating panel owns the bind path — that is the node AEM
                // instantiates per row — so move it off the child.
                if let (Some(child_uuid), Some(uuid)) = (child.uuid(), node.uuid())
                    && let Some(path) = st.bind_refs.remove(&child_uuid)
                {
                    st.bind_refs.insert(uuid, path);
                }
                let _ = name;
                out.push(element);
            }

            Emit::Group { name, occurs } => {
                let name = unique_name(name, used);
                let path = format!("{parent_path}/{name}");
                // A grouping element is bound only when it repeats — a
                // non-repeating group is a pure schema convenience with no AEM
                // node to attach data to.
                if occurs.repeats()
                    && let Some(uuid) = node.uuid()
                {
                    st.bind_refs.insert(uuid, path.clone());
                }
                let mut sequence = Vec::new();
                let mut inner_used = HashSet::new();
                if let Some(children) = child_nodes(node) {
                    walk(children, &path, &mut sequence, &mut inner_used, st, ctx);
                }

                // A group whose children all resolved to nothing carries no
                // data. Emitting it would put an empty `xs:sequence` in the
                // schema and, if it repeats, bind a node to a path with nothing
                // under it. Drop it and release its name and binding.
                if sequence.is_empty() {
                    if let Some(uuid) = node.uuid() {
                        st.bind_refs.remove(&uuid);
                    }
                    used.remove(&name);
                    continue;
                }

                out.push(XsdNode::Element {
                    name,
                    type_ref: None,
                    min_occurs: occurs.min,
                    max_occurs: occurs.max,
                    content: Some(Box::new(XsdNode::ComplexType {
                        name: None,
                        sequence,
                    })),
                });
            }
        }
    }
}

/// The single child of `node` that resolves to exactly one XSD element.
///
/// Only a `Ref` or `Leaf` child qualifies: a transparent panel may expand to any
/// number of elements, and collapsing then would be wrong.
fn single_element_child<'a>(node: &'a AemNode, ctx: &Ctx) -> Option<&'a AemNode> {
    let children = child_nodes(node)?;
    let [only] = children else { return None };
    matches!(
        classify(only, None, ctx),
        Emit::Ref { .. } | Emit::Leaf { .. }
    )
    .then_some(only)
}

/// Overwrite an element's occurrence attributes.
fn set_occurs(node: &mut XsdNode, occurs: Occurs) {
    match node {
        XsdNode::Element {
            min_occurs,
            max_occurs,
            ..
        }
        | XsdNode::Ref {
            min_occurs,
            max_occurs,
            ..
        } => {
            *min_occurs = occurs.min;
            *max_occurs = occurs.max;
        }
        XsdNode::ComplexType { .. } => {}
    }
}

/// Reserve `name` within a sequence, suffixing with 2, 3, … if already taken.
fn unique_name(name: String, used: &mut HashSet<String>) -> String {
    if used.insert(name.clone()) {
        return name;
    }
    for n in 2u32.. {
        let candidate = format!("{name}{n}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("an unused suffix always exists")
}

fn bind(node: &AemNode, parent_path: &str, name: &str, st: &mut BuildState) {
    if let Some(uuid) = node.uuid() {
        st.bind_refs.insert(uuid, format!("{parent_path}/{name}"));
    }
}

/// Record the `xs:include` that declares `name`, if the type library has one.
fn note_include_for(name: &str, st: &mut BuildState, ctx: &Ctx) {
    if name.starts_with("xs:") {
        return;
    }
    if let Some(path) = ctx.config.type_to_file.get(name) {
        st.note_include(path);
    }
}

/// Whether a node exists only for presentation and so cannot be a lookahead
/// target.
fn is_presentational(node: &AemNode) -> bool {
    matches!(
        node,
        AemNode::TextDraw { .. }
            | AemNode::TitleDraw { .. }
            | AemNode::HtmlDisplayer { .. }
            | AemNode::MessageBox { .. }
            | AemNode::Preface { .. }
            | AemNode::Appendix { .. }
            | AemNode::FootnotePlaceholder { .. }
    )
}

fn child_nodes(node: &AemNode) -> Option<&[AemNode]> {
    match node {
        AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } => Some(children),
        _ => None,
    }
}

// ============================================================================
// Classification
// ============================================================================

/// Node kinds an [`AemElementRule`] can match on.
fn node_kind(node: &AemNode) -> &'static str {
    // A panel or repeatable whose fragment content was inlined is still a
    // fragment as far as the schema is concerned — it contributes one element
    // of the fragment's type, not a group around its inlined children.
    if node_frag_ref(node).is_some() {
        return "fragment";
    }
    match node {
        AemNode::Root { .. } => "root",
        AemNode::Panel { .. } => "panel",
        AemNode::Repeatable { .. } => "repeatable",
        AemNode::Fragment { .. } => "fragment",
        AemNode::TextField { .. } => "textbox",
        AemNode::NumberField { .. } => "numericbox",
        AemNode::DatePicker { .. } => "datepicker",
        AemNode::Dropdown { .. } => "dropdownlist",
        AemNode::Checkbox { .. } => "checkbox",
        AemNode::RadioButton { .. } => "radiobutton",
        AemNode::Custom { .. } => "custom",
        AemNode::TextDraw { .. } => "textdraw",
        AemNode::TitleDraw { .. } => "titledraw",
        AemNode::MessageBox { .. } => "messagebox",
        AemNode::HtmlDisplayer { .. } => "htmldisplayer",
        AemNode::Preface { .. } => "preface",
        AemNode::Appendix { .. } => "appendix",
        AemNode::FootnotePlaceholder { .. } => "footnoteplaceholder",
    }
}

/// The node's user-visible `jcr:title` / label.
fn node_title(node: &AemNode) -> &str {
    match node {
        AemNode::Root { title, .. }
        | AemNode::Panel { title, .. }
        | AemNode::Repeatable { title, .. }
        | AemNode::Fragment { title, .. } => title,
        AemNode::TextField { label, .. }
        | AemNode::NumberField { label, .. }
        | AemNode::DatePicker { label, .. }
        | AemNode::Dropdown { label, .. }
        | AemNode::Checkbox { label, .. }
        | AemNode::RadioButton { label, .. }
        | AemNode::Custom { label, .. } => label,
        _ => "",
    }
}

/// Whether the node is visible. Invisible *fields* carry no data worth binding;
/// invisible *panels* routinely wrap conditional content that does.
fn node_visible(node: &AemNode) -> bool {
    match node {
        AemNode::Panel { visible, .. }
        | AemNode::TextField { visible, .. }
        | AemNode::NumberField { visible, .. }
        | AemNode::DatePicker { visible, .. }
        | AemNode::Dropdown { visible, .. }
        | AemNode::Checkbox { visible, .. }
        | AemNode::RadioButton { visible, .. }
        | AemNode::Custom { visible, .. } => *visible,
        _ => true,
    }
}

/// Option labels, for rules that match on an option set.
fn node_options(node: &AemNode) -> Option<Vec<&str>> {
    match node {
        AemNode::Dropdown { options, .. }
        | AemNode::Checkbox { options, .. }
        | AemNode::RadioButton { options, .. }
        | AemNode::Custom { options, .. } => {
            Some(options.iter().map(|o| o.label.as_str()).collect())
        }
        _ => None,
    }
}

/// The `fragRef` behind this node, whether it is an opaque `Fragment` or a
/// `Panel`/`Repeatable` whose fragment content was inlined.
fn node_frag_ref(node: &AemNode) -> Option<&str> {
    match node {
        AemNode::Fragment { frag_ref, .. } => Some(frag_ref),
        AemNode::Panel {
            frag_ref: Some(fr), ..
        }
        | AemNode::Repeatable {
            frag_ref: Some(fr), ..
        } => Some(fr),
        _ => None,
    }
}

/// The lone option's label, when a field has exactly one.
///
/// A single-option checkbox *is* its statement — "Erbschaft", "Real estate
/// (Sale/income)" — so when the field carries no label of its own, the option
/// names it. With several options they are answers, not the question, and say
/// nothing about what the field holds.
fn sole_option_label(node: &AemNode) -> Option<&str> {
    match node_options(node)?.as_slice() {
        [only] if !only.trim().is_empty() => Some(only),
        _ => None,
    }
}

/// Whether the node repeats, and hence needs `maxOccurs`.
fn node_repeats(node: &AemNode) -> bool {
    matches!(node, AemNode::Repeatable { .. })
}

/// What `node` contributes to the schema.
///
/// `next` is the following non-presentational sibling, needed only by rules with
/// a `nextFragment` lookahead.
fn classify(node: &AemNode, next: Option<&AemNode>, ctx: &Ctx) -> Emit {
    let profile = &ctx.config.profile;
    let subject = AemRuleSubject {
        kind: node_kind(node),
        name: node.name().unwrap_or_default(),
        title: node_title(node),
        frag_ref: node_frag_ref(node),
        options: node_options(node),
        visible: node_visible(node),
        next_frag_ref: next.and_then(node_frag_ref),
    };
    let rule = profile.match_aem_rule(&subject);

    if rule.is_some_and(|r| r.ignore) {
        return Emit::Skip;
    }

    // Structural nodes never carry data.
    match node {
        AemNode::Root { .. }
        | AemNode::TextDraw { .. }
        | AemNode::TitleDraw { .. }
        | AemNode::HtmlDisplayer { .. }
        // A notice is prose shown on screen; it holds no data.
        | AemNode::MessageBox { .. }
        | AemNode::Preface { .. }
        | AemNode::Appendix { .. }
        | AemNode::FootnotePlaceholder { .. } => return Emit::Skip,

        // A Custom node stands in for a whole hand-written profile template —
        // `apply_custom_elements` replaces a panel's entire contents with one of
        // them, keeping only the panel title as its label. The fields it renders
        // live in the template, not in the model, so there is nothing here to
        // describe. Treating it as a data leaf would emit one `xs:string` named
        // after the section and silently drop every field the section holds.
        //
        // A rule naming an `element` opts a specific custom template back in.
        AemNode::Custom { .. } if rule.and_then(|r| r.element.as_ref()).is_none() => {
            return Emit::Skip;
        }
        _ => {}
    }

    // Resolved once: `element_for` may pick a different element based on the
    // following sibling.
    let rule_element = rule.and_then(|r| r.element_for(&subject));

    let occurs = |default: Occurs| -> Occurs {
        rule.and_then(|r| r.occurs.as_ref())
            .map(|spec| spec.to_occurs(profile.max_occurs_value))
            .unwrap_or(default)
    };

    // A fragment is a leaf in the schema: its internals live in its own type.
    if let Some(frag_ref) = node_frag_ref(node) {
        let default = if node_repeats(node) {
            Occurs::optional_repeating(profile.max_occurs_value)
        } else {
            Occurs::optional()
        };
        return fragment_emit(frag_ref, rule, rule_element, occurs(default), ctx);
    }

    match node {
        // A repeating panel becomes a grouping element with maxOccurs.
        AemNode::Repeatable { .. } => {
            let name = rule_element
                .clone()
                .unwrap_or_else(|| to_xsd_element_name(node_title(node)));
            Emit::Group {
                name,
                occurs: occurs(Occurs::optional_repeating(profile.max_occurs_value)),
            }
        }

        // A layout panel adds no level: its children bubble up.
        //
        // The exception is a titled *page* panel — a section of the form. Those
        // do add a level, because without one two sections that repeat the same
        // field or fragment (a "Client" and an "Authorized representative" block
        // each holding an IndividualBasic fragment) would collide into duplicate
        // sibling elements, which is invalid XSD and would bind two nodes to one
        // path.
        //
        // A parsed form's wizard steps are not marked as pages, so a schema
        // derived from an existing package stays flat — matching how UBS's own
        // schemas are shaped. A config rule naming an element wins over both.
        AemNode::Panel { is_page, title, .. } => match rule.and_then(|r| r.element.clone()) {
            Some(name) => Emit::Group {
                name,
                occurs: occurs(Occurs::optional()),
            },
            None if profile.group_page_panels && *is_page && !title.trim().is_empty() => {
                // `[sections]` patterns name a section across languages —
                // "Unterschrift der Firma" and "signature of company" both
                // resolve to CompanySignature. The panel title *is* the heading,
                // so pass it as one: a heading match takes priority over a
                // body-text match.
                let name = super::resolve_section_name_with_heading(title, Some(title), profile)
                    .unwrap_or_else(|| to_xsd_element_name(title));
                Emit::Group {
                    name,
                    occurs: occurs(Occurs::optional()),
                }
            }
            None => Emit::Transparent,
        },

        // Everything else is a data leaf.
        _ => {
            // `[elements]` normalises a label across languages and supplies the
            // type: `Straße`/`Via` → `Street`, `Data` → `Date`/`xs:date`. Whole
            // labels only — see `resolve_element_whole_label` for why substring
            // matching is unusable here.
            let synonym = super::resolve_element_whole_label(node_title(node), profile);

            // Every step is a fallback for the one before. Without the last two,
            // a label-less checkbox or radio — common in these forms, where the
            // text sits in the options or in a neighbouring text block — would
            // resolve to nothing and drop out of the schema entirely, taking its
            // data with it.
            let usable = |name: String| (!name.is_empty() && name != "Unknown").then_some(name);
            let Some(name) = rule_element
                .clone()
                .or_else(|| synonym.as_ref().map(|res| res.name.clone()))
                .or_else(|| usable(to_xsd_element_name(node_title(node))))
                .or_else(|| sole_option_label(node).map(to_xsd_element_name))
                .and_then(usable)
                .or_else(|| {
                    super::element_name_from_component_name(node.name().unwrap_or_default())
                })
            else {
                return Emit::Skip;
            };

            let occurs = occurs(Occurs::optional());

            // A global element is referenced, never re-declared.
            if ctx.config.is_global_element(&name) && rule.is_none_or(|r| r.type_ref.is_none()) {
                return Emit::Ref {
                    name,
                    occurs,
                    include_hint: None,
                };
            }

            let type_ref = rule
                .and_then(|r| r.type_ref.clone())
                .or_else(|| synonym.map(|res| res.type_ref))
                .unwrap_or_else(|| profile.default_type_for(node_kind(node)));

            Emit::Leaf {
                name,
                type_ref,
                occurs,
            }
        }
    }
}

/// Resolve a fragment node to its XSD element.
///
/// The element name comes from the config rule when there is one — the same
/// `fragRef` can appear twice in a form under different titles — and otherwise
/// from the global element declared for the fragment's `fragmentModelRoot` type.
fn fragment_emit(
    frag_ref: &str,
    rule: Option<&AemElementRule>,
    rule_element: Option<String>,
    occurs: Occurs,
    ctx: &Ctx,
) -> Emit {
    let type_name = ctx.frag_types.get(frag_ref).copied();

    let name = rule_element
        .or_else(|| type_name.and_then(|t| ctx.config.type_to_element_name.get(t).cloned()));

    let Some(name) = name else {
        // Neither config nor the type library knows this fragment. Emitting a
        // guessed element would corrupt the schema, so leave it out.
        log::warn!("No XSD element for fragment {frag_ref}; omitted from the schema");
        return Emit::Skip;
    };

    // An explicit `type` in the rule forces the name=/type= form, which is how
    // two panels sharing one fragRef get distinct element names.
    if let Some(type_ref) = rule.and_then(|r| r.type_ref.clone()) {
        return Emit::Leaf {
            name,
            type_ref,
            occurs,
        };
    }

    if ctx.config.is_global_element(&name) {
        return Emit::Ref {
            name,
            occurs,
            include_hint: type_name.map(str::to_string),
        };
    }

    match type_name {
        Some(t) => Emit::Leaf {
            name,
            type_ref: t.to_string(),
            occurs,
        },
        None => {
            log::warn!(
                "Fragment {frag_ref} resolves to element {name}, but no fragment in the \
                 library declares its type; omitted from the schema"
            );
            Emit::Skip
        }
    }
}
