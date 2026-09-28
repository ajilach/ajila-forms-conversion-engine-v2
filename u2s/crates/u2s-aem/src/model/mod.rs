//! The generic AEM Adaptive Forms output model: one root structure
//! (`AemForm`), its enums and newtypes, and semantic validation. This is the
//! *entire* intermediate JSON the Conversion Agent edits — JCR content-XML
//! emission is an encoder concern, not modelled here.

pub mod common;
pub mod enums;
pub mod format;
pub mod newtypes;
pub mod text;
pub mod validate;

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::num::NonZeroU32;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

pub use common::{
    Common, CssClasses, FieldCommon, FieldLayout, JcrValue, LabelCommon, PanelLayout, Passthrough,
    Presence, RawJcrNode,
};
pub use enums::{
    AssistPriority, AutofillHint, DataModel, DorMode, HeadingLevel, NamedDateFormat,
    NamedNumberFormat, OptionAlignment, SortOrder, TextInput,
};
pub use format::{DateFormat, NumberFormat, Validation, YearRange};
pub use newtypes::{
    ColSpan, ComponentName, CssClass, DatePattern, FormName, FragmentRef, JcrName, NumberPattern,
    OptionValue, ResourceType, TextPattern, XmlName,
};
pub use text::{I18nRichText, I18nText, Language, PlainText, RichText};
pub use validate::{ValidForm, Violation};

/// The AEM output document. `schema_for!(AemForm)` (see [`crate::schema`])
/// IS the format schema published in the format server's manifest — there
/// is no hand-maintained schema file for it to drift from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AemForm {
    pub metadata: FormMetadata,
    /// Wizard steps, in order. Everything the root carries beyond them —
    /// the summary and preview steps, the toolbar's rendering — is
    /// encoder-emitted and has no representation here.
    pub pages: Vec<Page>,
}

impl AemForm {
    /// Structural parse: agents edit the JSON as a `Value` through
    /// `u2s-jsondoc`; the mapper deserializes exactly once, at encode.
    pub fn from_json(value: &serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FormMetadata {
    /// JCR-safe node name for the form's page and DAM asset.
    pub form_name: FormName,
    /// Display title (AEM.md §5.2 `jcr:title`). `None` means the encoder
    /// falls back to `form_name`.
    #[serde(default)]
    pub title: Option<I18nText>,
    pub master_language: Language,
    /// Every delivered language; must contain `master_language`
    /// (checked in [`AemForm::validate`]).
    pub languages: BTreeSet<Language>,
    pub dor: DorMode,
    /// AEM.md §19.1 `formmodel`. `Unbound` means no field carries a
    /// `bindRef`; `XmlSchema` means the encoder generates an XSD and
    /// assigns every `bindRef` in that same walk, which is why `bindRef`
    /// never appears anywhere in this model.
    pub data_model: DataModel,
    /// AEM.md §6.11 toolbar composition, in order — ordinary generic nodes
    /// now, not a closed `ToolbarButton` enum. Earlier drafts of this model
    /// hardcoded the toolbar's own button set and titles in the mapper,
    /// which could not represent the real corpus's own `guidebutton` at
    /// all; the Conversion Agent now authors each toolbar action directly
    /// (guided by rules), the same way it authors everything else
    /// structural.
    #[serde(default)]
    pub toolbar: Vec<Node>,
    /// Intermediate JCR folder segments between `/content/forms/af/` and
    /// this form's own name, e.g. `["afforms_germany_all", "af_aa"]` for a
    /// real package rooted at
    /// `/content/forms/af/afforms_germany_all/af_aa/AF_AABF`. Empty
    /// reproduces this crate's own flat `/content/forms/af/<form_name>`
    /// path exactly -- every green-field form this system's own Conversion
    /// Agent produces.
    #[serde(default)]
    pub folder_path: Vec<XmlName>,
    /// `rootPanel`'s own separate `layout` child (distinct from its
    /// `items` child, which always carries
    /// `fd/af/layouts/gridFluidLayout2` -- mechanical, so the mapper still
    /// owns it unconditionally). A real package's own wizard forms use a
    /// customer-specific layout type here
    /// (`.../layouts/panel/wizard`, say); `None` means no such child at
    /// all, this crate's own encoder's behaviour prior to this field's
    /// addition.
    #[serde(default)]
    pub root_panel_layout: Option<ResourceType>,
    /// `guideContainer`'s own remainder: every attribute AEM.md documents
    /// there beyond `fd:version`/`dorType` (which stay typed fields, since
    /// this model already needed them structurally) -- `actionType`,
    /// `autoSaveStrategyType`, `clientLibRef`, `dorTemplateRef`, `redirect`,
    /// `thankYouOption`, `thankYouMessage`, `themeRef`, `useExistingAF`,
    /// `guideCss`, ... -- plus every child `guideContainer` carries beyond
    /// `rootPanel` (`autoSaveInfo`, `signerInfo`, `view`, ...). Real,
    /// per-deployment configuration, not mechanical boilerplate, so it
    /// gets the same lossless treatment a node's own [`Common::passthrough`]
    /// does rather than a dedicated typed field per attribute this model
    /// would otherwise need to keep inventing.
    #[serde(default)]
    pub chrome: Passthrough,
    /// The DAM asset's own `<metadata>` node's remainder: every attribute
    /// beyond what the encoder already derives mechanically from this form
    /// itself (`allowedRenderFormat`, `dorType`, `formmodel`,
    /// `hasCustomThumbnail`, `title`) -- `author`, `availableStylings`,
    /// `availableInMobileApp`, `dorTemplateRef`, `menuOptions`,
    /// `redactoSummary`, `themeRef`, ... -- plus its sibling `<dictionary>`
    /// child, which the real corpus does not always keep in sync with the
    /// guideContainer's own dictionary directory (see `dam::decode`'s own
    /// doc). Real, per-deployment DAM authoring metadata, not mechanical
    /// boilerplate, so it gets the same lossless treatment
    /// [`FormMetadata::chrome`] does rather than a dedicated typed field
    /// per attribute this model would otherwise need to keep inventing.
    #[serde(default)]
    pub dam_chrome: Passthrough,
}

/// A navigable wizard step. Pages may only appear here, as direct children
/// of the root, so "a page nested inside a panel" has no encoding — it is
/// not a validation rule, because the type has no field for it.
///
/// `properties` carries whatever this step's own panel needs beyond its
/// name and content -- title, presence flags, layout, `panelSetType` —
/// the same open, agent-authored bag [`Node::Component`] uses. Earlier
/// drafts of this model gave `Page` its own typed `title`/`presence`/
/// `layout` fields and had the mapper assume `panelSetType="wizard"` on
/// every one; that assumption is exactly the kind of hardcoded default
/// this redesign moves to the agent (guided by rules), so those fields
/// collapsed into the same generic shape a `Component`'s properties use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub name: ComponentName,
    #[serde(default)]
    pub properties: BTreeMap<JcrName, JcrValue>,
    /// Non-empty — checked in [`AemForm::validate`].
    pub children: Vec<Node>,
}

/// One node of the form's content tree.
///
/// **Why only these eight variants.** The seven leaf field kinds
/// (`TextField` through `Signature`) are real domain types: what a
/// Conversion Agent's output means by "this holds a date" versus "this
/// holds free text" is content the schema should teach it directly, not
/// authoring convention. Everything structural — a panel, a repeating
/// group's outer/inner pair, a fragment reference, a customer-specific
/// component (`messagebox`, `summary`, a toolbar action, ...) — is a
/// `Component`: one generic shape the agent builds explicitly, checked by
/// rules rather than by a Rust type that used to bake in a specific
/// authoring pattern (a repeatable's outer+inner+Add/Remove-button
/// expansion, a visibility rule's synthesized script, a closed toolbar
/// button enum). See `u2s-mapper-aem`'s module doc for the full
/// reasoning and which four behaviours moved out of the mapper because of
/// it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Node {
    /// The generic structural node — see this enum's own doc.
    Component {
        common: Common,
        #[serde(default)]
        properties: BTreeMap<JcrName, JcrValue>,
        #[serde(default)]
        children: Vec<Node>,
    },
    TextField {
        common: Common,
        field: FieldCommon,
        layout: FieldLayout,
        /// AEM.md §6.3 `multiLine`.
        input: TextInput,
        #[serde(default)]
        max_chars: Option<NonZeroU32>,
        /// AEM.md §19.3 `autofillFieldKeyword`.
        #[serde(default)]
        autofill: Option<AutofillHint>,
        #[serde(default)]
        validation: Option<Validation>,
    },
    NumberField {
        common: Common,
        field: FieldCommon,
        layout: FieldLayout,
        /// AEM.md §6.4 validate/display picture clauses.
        #[serde(default)]
        format: Option<NumberFormat>,
        #[serde(default)]
        validation: Option<Validation>,
    },
    DatePicker {
        common: Common,
        field: FieldCommon,
        layout: FieldLayout,
        /// AEM.md §6.5 `defaultToCurrentDate`.
        #[serde(default)]
        default_to_current_date: bool,
        format: Option<DateFormat>,
        /// `yearRangeFrom` / `yearRangeTo`.
        #[serde(default)]
        year_range: Option<YearRange>,
        #[serde(default)]
        validation: Option<Validation>,
    },
    Dropdown {
        common: Common,
        field: FieldCommon,
        layout: FieldLayout,
        options: ChoiceOptions,
        /// AEM.md §6.6 `filteringAllowed`.
        #[serde(default)]
        filtering_allowed: bool,
        sort: Option<SortOrder>,
    },
    Checkbox {
        common: Common,
        /// [`LabelCommon`], not [`FieldCommon`]: AEM has no `mandatory`
        /// concept for a checkbox group (AEM.md §6.7), so that gap is a
        /// missing field rather than an unused flag.
        field: LabelCommon,
        layout: FieldLayout,
        options: ChoiceOptions,
        alignment: OptionAlignment,
        /// AEM.md §6.7 `hideTitle`.
        #[serde(default)]
        hide_title: bool,
        #[serde(default)]
        rich_text_options: bool,
    },
    RadioButton {
        common: Common,
        field: FieldCommon,
        layout: FieldLayout,
        options: ChoiceOptions,
        alignment: OptionAlignment,
        #[serde(default)]
        rich_text_options: bool,
    },
    StaticText {
        common: Common,
        layout: FieldLayout,
        /// AEM.md §6.9 `_value`, rich text; headings below the page level
        /// live here as markup.
        content: I18nRichText,
        heading_level: Option<HeadingLevel>,
    },
    /// AEM.md §6.10 `guideScribble`: a signature/drawing input.
    Signature {
        common: Common,
        field: FieldCommon,
        layout: FieldLayout,
    },
}

impl Node {
    pub fn common(&self) -> &Common {
        match self {
            Node::Component { common, .. }
            | Node::TextField { common, .. }
            | Node::NumberField { common, .. }
            | Node::DatePicker { common, .. }
            | Node::Dropdown { common, .. }
            | Node::Checkbox { common, .. }
            | Node::RadioButton { common, .. }
            | Node::StaticText { common, .. }
            | Node::Signature { common, .. } => common,
        }
    }

    pub fn name(&self) -> &ComponentName {
        &self.common().name
    }

    pub fn common_mut(&mut self) -> &mut Common {
        match self {
            Node::Component { common, .. }
            | Node::TextField { common, .. }
            | Node::NumberField { common, .. }
            | Node::DatePicker { common, .. }
            | Node::Dropdown { common, .. }
            | Node::Checkbox { common, .. }
            | Node::RadioButton { common, .. }
            | Node::StaticText { common, .. }
            | Node::Signature { common, .. } => common,
        }
    }

    pub fn children(&self) -> Option<&[Node]> {
        match self {
            Node::Component { children, .. } => Some(children),
            _ => None,
        }
    }

    pub fn options(&self) -> Option<&ChoiceOptions> {
        match self {
            Node::Dropdown { options, .. }
            | Node::Checkbox { options, .. }
            | Node::RadioButton { options, .. } => Some(options),
            _ => None,
        }
    }

    /// The single-value `FieldLayout` for leaf nodes. A `Component`'s own
    /// layout, if it has one, lives in its `properties` bag instead (see
    /// that variant's own doc on why layout is no longer a typed field for
    /// structural nodes).
    pub fn field_layout(&self) -> Option<&FieldLayout> {
        match self {
            Node::TextField { layout, .. }
            | Node::NumberField { layout, .. }
            | Node::DatePicker { layout, .. }
            | Node::Dropdown { layout, .. }
            | Node::Checkbox { layout, .. }
            | Node::RadioButton { layout, .. }
            | Node::StaticText { layout, .. }
            | Node::Signature { layout, .. } => Some(layout),
            Node::Component { .. } => None,
        }
    }

    /// Every plain-text field on this node, paired with the pointer segment
    /// it lives at, so [`AemForm::validate`] can check language coverage
    /// without a duplicate match per caller. Choice options are handled
    /// separately, since they need per-option pointers. A `Component`'s own
    /// translatable properties (`JcrValue::Text`) are included here too, so
    /// they get exactly the same coverage check and the same dictionary
    /// derivation (`u2s-mapper-aem::i18n`) a typed leaf kind's fields
    /// already get — one mechanism, not two.
    pub fn i18n_texts(&self) -> Vec<(String, &I18nText)> {
        let mut out = Vec::new();
        match self {
            Node::Component { properties, .. } => {
                for (key, value) in properties {
                    if let JcrValue::Text(text) = value {
                        // `/value`, not just `/properties/{key}`: the JSON
                        // document at that shorter path is the whole
                        // `JcrValue` wrapper (`{"kind":"text","value":
                        // {...}}`), not the `I18nText` map itself -- the
                        // violation pointer should name the actual
                        // offending value.
                        out.push((format!("properties/{key}/value"), text));
                    }
                }
            }
            Node::TextField { field, .. }
            | Node::NumberField { field, .. }
            | Node::DatePicker { field, .. }
            | Node::Dropdown { field, .. }
            | Node::RadioButton { field, .. }
            | Node::Signature { field, .. } => {
                out.push(("field/label".to_owned(), &field.label));
                if let Some(message) = &field.mandatory_message {
                    out.push(("field/mandatory_message".to_owned(), message));
                }
                if let Some(placeholder) = &field.placeholder {
                    out.push(("field/placeholder".to_owned(), placeholder));
                }
            }
            Node::Checkbox { field, .. } => {
                out.push(("field/label".to_owned(), &field.label));
                if let Some(placeholder) = &field.placeholder {
                    out.push(("field/placeholder".to_owned(), placeholder));
                }
            }
            Node::StaticText { .. } => {}
        }
        out
    }

    pub fn i18n_rich_texts(&self) -> Vec<(String, &I18nRichText)> {
        match self {
            Node::Component { properties, .. } => properties
                .iter()
                .filter_map(|(key, value)| match value {
                    JcrValue::RichText(text) => Some((format!("properties/{key}/value"), text)),
                    _ => None,
                })
                .collect(),
            Node::StaticText { content, .. } => vec![("content".to_owned(), content)],
            _ => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChoiceOption {
    pub value: OptionValue,
    pub label: I18nText,
}

/// A non-empty list of [`ChoiceOption`]s with unique values. Both
/// constraints are enforced at deserialization, not by
/// [`AemForm::validate`], because they depend on nothing outside this list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChoiceOptions(Vec<ChoiceOption>);

#[derive(Debug, Clone, thiserror::Error)]
pub enum ChoiceOptionsError {
    #[error("a choice component must declare at least one option")]
    Empty,
    #[error("duplicate option value {0:?}")]
    DuplicateValue(String),
}

impl ChoiceOptions {
    pub fn options(&self) -> &[ChoiceOption] {
        &self.0
    }
}

impl TryFrom<Vec<ChoiceOption>> for ChoiceOptions {
    type Error = ChoiceOptionsError;
    fn try_from(options: Vec<ChoiceOption>) -> Result<Self, Self::Error> {
        if options.is_empty() {
            return Err(ChoiceOptionsError::Empty);
        }
        let mut seen = HashSet::new();
        for option in &options {
            if !seen.insert(option.value.as_str()) {
                return Err(ChoiceOptionsError::DuplicateValue(option.value.to_string()));
            }
        }
        Ok(Self(options))
    }
}

impl<'de> Deserialize<'de> for ChoiceOptions {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Vec::<ChoiceOption>::deserialize(deserializer)?;
        ChoiceOptions::try_from(raw).map_err(D::Error::custom)
    }
}

impl JsonSchema for ChoiceOptions {
    fn schema_name() -> Cow<'static, str> {
        "ChoiceOptions".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let item_schema = generator.subschema_for::<ChoiceOption>();
        json_schema!({
            "type": "array",
            "items": item_schema,
            "minItems": 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(value: &str) -> ChoiceOption {
        ChoiceOption {
            value: OptionValue::try_from(value).unwrap(),
            label: I18nText::single(
                Language::try_from("en").unwrap(),
                PlainText::try_from(value.to_string()).unwrap(),
            ),
        }
    }

    #[test]
    fn choice_options_rejects_empty() {
        assert!(matches!(
            ChoiceOptions::try_from(vec![]),
            Err(ChoiceOptionsError::Empty)
        ));
    }

    #[test]
    fn choice_options_rejects_duplicate_values() {
        let result = ChoiceOptions::try_from(vec![option("a"), option("a")]);
        assert!(matches!(result, Err(ChoiceOptionsError::DuplicateValue(_))));
    }

    #[test]
    fn choice_options_accepts_unique_non_empty() {
        assert!(ChoiceOptions::try_from(vec![option("a"), option("b")]).is_ok());
    }
}
