//! Attribute bundles shared by several node kinds (AEM.md §6.1, §7.3).
//!
//! `Common` deliberately does not carry a layout: a panel's own placement
//! within its parent's grid and a leaf field's placement are both a single
//! [`FieldLayout`], but a panel *also* needs `dorNumCols` for its own
//! children's Document-of-Record layout ([`PanelLayout`]). Giving every node
//! kind exactly one layout field — `FieldLayout` on leaves, `PanelLayout` on
//! panels — means a node's placement has exactly one source of truth; an
//! earlier draft of this model put a `FieldLayout` on `Common` *and* a
//! `PanelLayout` on panels, which would have let a panel's own width
//! disagree with itself.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::enums::AssistPriority;
use super::newtypes::{ColSpan, ComponentName, CssClass, JcrName, ResourceType};
use super::text::{I18nRichText, I18nText};

/// A validated, orderless list of CSS classes (AEM.md §6.1 `css`). Each
/// class validates itself at the edge; this wrapper exists so the list has
/// its own place in the schema rather than being a raw space-separated
/// string.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct CssClasses(Vec<CssClass>);

impl CssClasses {
    pub fn classes(&self) -> &[CssClass] {
        &self.0
    }
}

impl FromIterator<CssClass> for CssClasses {
    fn from_iter<I: IntoIterator<Item = CssClass>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// Attributes every component carries (AEM.md §6.1).
///
/// **Why `resource_type`/`guide_node_class` live here, agent-authored,
/// rather than being derived by the mapper from a node's Rust variant.**
/// Earlier drafts of this model had `u2s-mapper-aem` choose a fixed
/// `sling:resourceType` per `Node` variant (`fd/af/components/panel` for
/// every panel, say). That baked a customer's own component catalogue
/// choice into Rust — the exact "hardcoded rule" the mapper's own module
/// doc always disclaimed but did not, in practice, avoid. Every node now
/// states its own resource type explicitly, the same way a real package's
/// own author chose one; the mapper only ever spells whatever value it is
/// given, correctly, as XML.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Common {
    /// Unique within the form (checked by [`crate::model::validate`]). Used
    /// as the JCR element name when [`Common::jcr_name`] is absent.
    pub name: ComponentName,
    /// `sling:resourceType` — which component this is, in AEM's own
    /// vocabulary (a foundation type, an overlay of one, or a fully
    /// customer-specific component). See this struct's own doc for why the
    /// mapper never chooses this. `None` for a JCR element that carries no
    /// resource type at all -- a real package has plenty of these
    /// (`fd:rules`, `fd:visible`, and other plain `nt:unstructured`
    /// configuration nodes are not AEM *components* and were never going
    /// to have one).
    #[serde(default)]
    pub resource_type: Option<ResourceType>,
    #[serde(default)]
    pub guide_node_class: Option<JcrName>,
    /// The real JCR element name, when it differs from [`Common::name`] --
    /// a real package's own element is not always named after its `name`
    /// attribute (`panel_00000000000040008000000000000001` carrying
    /// `name="PN_..."` is the ordinary case, not the exception). `None`
    /// means the encoder uses `name` itself, which is every green-field
    /// form this system's own Conversion Agent produces.
    #[serde(default)]
    pub jcr_name: Option<JcrName>,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub css: CssClasses,
    #[serde(default)]
    pub presence: Presence,
    /// Carried, not derived, unlike earlier drafts of this model that had
    /// the encoder always compute one from tree position. A real package's
    /// own `bindRef` does not always match that derivation (the fixture's
    /// own `formmodel="none"` form still carries stray `bindRef` values on
    /// some nodes), so a decoded value takes precedence; `None` under
    /// [`super::DataModel::XmlSchema`] still falls back to
    /// `u2s_mapper_aem::xsd::generate`'s mechanical derivation, so a
    /// green-field form the Conversion Agent authors is unaffected.
    #[serde(default)]
    pub bind_ref: Option<String>,
    /// Everything this node's own JCR element carried that no field above
    /// or on the specific [`super::Node`] variant represents. See
    /// [`Passthrough`]'s own doc for why this makes decode lossless without
    /// the model having to know what every JCR attribute means.
    #[serde(default)]
    pub passthrough: Passthrough,
}

fn default_true() -> bool {
    true
}

/// A node's raw remainder: every JCR attribute and child element decode did
/// not fold into a typed field (on [`Common`] or on the specific
/// [`super::Node`] variant), carried verbatim so a round trip through
/// `u2s-mapper-aem`'s decoder and encoder loses nothing. Never
/// interpreted by this crate or by `AemForm::validate` -- what a passthrough
/// attribute *means* is profile/customer knowledge this model deliberately
/// does not have (see `u2s-mapper-aem`'s own module doc).
///
/// `raw_attributes` is a plain string map (order does not matter for JCR
/// attributes, and a map reads naturally as a JSON object); `raw_children`
/// is an ordered list (sibling order is semantic — a wizard step's position
/// among its siblings, an option's position in its list — so it cannot be
/// a map).
///
/// An element the encoder writes holds, besides these, one child the
/// encoder generates itself: a component's or a page's `items` (its typed
/// children), `jcr:content`'s `guideContainer`, `guideContainer`'s
/// `rootPanel`, `rootPanel`'s `items`. `slot` says where among
/// `raw_children` that child goes, and `items` describes a generated
/// `items` element's own attributes and raw children, so a real package's
/// sibling order and `items` attributes survive a round trip. A raw
/// attribute overrides an attribute the encoder would write itself under
/// the same name.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Passthrough {
    pub raw_attributes: BTreeMap<String, String>,
    pub raw_children: Vec<RawJcrNode>,
    /// How many of `raw_children` come before the generated child; the
    /// rest come after it. `None` puts the generated child first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<usize>,
    /// The generated `items` element's own remainder: its attributes, and
    /// raw children around the typed ones (by its own `slot`). `None`
    /// writes a plain `items`, and only when there are typed children.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<Passthrough>>,
}

impl Passthrough {
    pub fn is_empty(&self) -> bool {
        self.raw_attributes.is_empty()
            && self.raw_children.is_empty()
            && self.slot.is_none()
            && self.items.is_none()
    }
}

/// One element of a [`Passthrough`]'s `raw_children` — a JCR element this
/// model has no typed representation for, kept as a small recursive tree
/// (not a verbatim XML string) so the Conversion Agent's own JSON tools
/// (`u2s-jsondoc`'s `outline`/`get`/`patch`) can address into it the same
/// way they address into any other part of the document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RawJcrNode {
    pub tag_name: String,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
    #[serde(default)]
    pub children: Vec<RawJcrNode>,
}

/// A generic node's own property value (`Node::Component::properties`).
///
/// `Text`/`RichText` exist so a `Component`'s own translatable content
/// round-trips the same way a typed leaf kind's `I18nText`/`I18nRichText`
/// field already does: written as the master language's own value inline,
/// with every other language becoming a Sling i18n dictionary entry keyed
/// by that master value (AEM.md §10) -- the same mechanism
/// `u2s-mapper-aem::i18n` already used for the seven typed leaf kinds,
/// now shared rather than duplicated. `Single`/`Multi` are for properties
/// with no translation at all (`dorLayoutType`, `messageboxType`, a plain
/// `_value`, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case", deny_unknown_fields)]
pub enum JcrValue {
    /// Written verbatim as the attribute value -- including any
    /// `{Boolean}`/`{Date}` type-hint prefix AEM's own convention needs for
    /// that particular property name. Which properties need one is a
    /// per-attribute-name platform convention, not inferable from the
    /// value alone, so it is the agent's responsibility (informed by rules
    /// and real examples), not a table this crate hardcodes.
    Single(String),
    /// A JCR multi-value property. The mapper joins these as `"[a,b,c]"`
    /// with `\,`/`\\` escaping -- mechanical, error-prone boilerplate the
    /// agent should not have to hand-escape, unlike the type-hint choice
    /// above.
    Multi(Vec<String>),
    Text(I18nText),
    RichText(I18nRichText),
}

/// Attributes every input field carries beyond [`Common`]. Checkbox has no
/// AEM concept of `mandatory` (AEM.md §6.7); it uses [`LabelCommon`]
/// instead, so that gap is a missing field rather than an unused flag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FieldCommon {
    pub label: I18nText,
    #[serde(default)]
    pub mandatory: bool,
    #[serde(default)]
    pub mandatory_message: Option<I18nText>,
    #[serde(default)]
    pub placeholder: Option<I18nText>,
    #[serde(default)]
    pub assist: AssistPriority,
}

/// [`FieldCommon`] without `mandatory`, for components AEM has no
/// mandatory concept for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabelCommon {
    pub label: I18nText,
    #[serde(default)]
    pub placeholder: Option<I18nText>,
    #[serde(default)]
    pub assist: AssistPriority,
}

/// AEM.md §6.1 Document-of-Record and summary exclusion. AEM treats these
/// four flags as independent, so they stay independent here.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields, default)]
pub struct Presence {
    pub dor_exclusion: bool,
    pub dor_exclude_title: bool,
    pub dor_exclude_description: bool,
    pub summary_exclusion: bool,
}

/// AEM.md §7.3 `cq:responsive`, in AEM's 12-column grid. `offset` is
/// `Option<ColSpan>` rather than a bare `ColSpan` because "no offset" (0
/// columns) is the common case and `ColSpan` itself only spans 1..=12.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FieldLayout {
    pub width: ColSpan,
    #[serde(default)]
    pub offset: Option<ColSpan>,
}

/// AEM.md §7.2 panel layout: the panel's own grid placement, plus how many
/// columns its Document-of-Record rendering uses (`dorNumCols`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PanelLayout {
    pub field: FieldLayout,
    #[serde(default)]
    pub dor_columns: Option<ColSpan>,
}
