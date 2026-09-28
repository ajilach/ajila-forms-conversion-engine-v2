//! Semantic validation: checks that need more than one node's type to see —
//! name uniqueness, a visibility trigger resolving to a real choice
//! component, translation coverage against the declared language set. Local,
//! single-node constraints (a non-empty option list, a 1..=12 column span)
//! are enforced by the types themselves at deserialization instead; this
//! module only holds what genuinely needs the whole tree.

use std::collections::{BTreeSet, HashMap};

use super::{AemForm, FieldLayout, I18nRichText, I18nText, Language, Node};

/// One semantic-validation failure, anchored to the JSON Pointer of the
/// offending value — the same shape rule scripts emit, so Output Review
/// (per PLAN.md) can render both identically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub pointer: String,
    pub message: String,
}

/// An [`AemForm`] that has passed [`AemForm::validate`]. Constructible only
/// by that method, so the encoder (phase 2) can require `&ValidForm` and
/// make encoding unvalidated output a compile error — the same pattern
/// PLAN.md's `Authorized<C>` uses for authorization.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidForm(AemForm);

impl ValidForm {
    pub fn form(&self) -> &AemForm {
        &self.0
    }

    pub fn into_form(self) -> AemForm {
        self.0
    }
}

impl AemForm {
    pub fn validate(self) -> Result<ValidForm, Vec<Violation>> {
        let mut violations = Vec::new();

        if self.pages.is_empty() {
            violations.push(Violation {
                pointer: "/pages".to_string(),
                message: "a form must have at least one page".to_string(),
            });
        }

        let master = self.metadata.master_language.clone();
        let languages = self.metadata.languages.clone();

        if !languages.contains(&master) {
            violations.push(Violation {
                pointer: "/metadata/languages".to_string(),
                message: format!("languages must contain the master language '{master}'"),
            });
        }

        if let Some(title) = &self.metadata.title {
            check_i18n_text(
                title,
                &master,
                &languages,
                "/metadata/title",
                &mut violations,
            );
        }

        // Duplicate-detection for the toolbar used to be possible because
        // `ToolbarButton` was a closed, `Eq + Hash` enum. It is now
        // `Vec<Node>` (see `FormMetadata::toolbar`'s own doc on why),
        // and a generic node has no simple discriminant to dedupe by --
        // "the same toolbar action appears twice" is exactly the kind of
        // structural check this redesign moves to a rule, not the type
        // system.

        let mut names: HashMap<String, String> = HashMap::new();
        let mut choice_nodes: HashMap<&str, (String, &Node)> = HashMap::new();

        for (page_index, page) in self.pages.iter().enumerate() {
            let page_pointer = format!("/pages/{page_index}");

            if page.children.is_empty() {
                violations.push(Violation {
                    pointer: format!("{page_pointer}/children"),
                    message: "a page must have at least one child".to_string(),
                });
            }

            check_name(
                page.name.as_str(),
                &format!("{page_pointer}/name"),
                &mut names,
                &mut violations,
            );

            // `page.title`/`page.layout` used to be typed fields checked
            // here directly. `Page` now carries `properties` -- the same
            // open, agent-authored bag `Node::Component` uses (see that
            // field's own doc) -- so whatever a page's own title/layout
            // properties should satisfy is a rule's job now, not a
            // structural check this function can still perform without
            // reaching into an untyped map by a magic key name.

            walk_nodes(
                &page.children,
                &format!("{page_pointer}/children"),
                &master,
                &languages,
                &mut names,
                &mut choice_nodes,
                &mut violations,
            );
        }

        // Visibility-trigger/value checking used to happen here, reading
        // the typed `VisibilityRule` field every `Node::Panel` carried.
        // Visibility is now ordinary agent-authored content on a
        // `Component` (raw `fd:rules`/`fd:visible` JSON, per this
        // redesign's own reasoning -- see `u2s-mapper-aem`'s module
        // doc), so this model has no typed field left to check; a rule
        // checking "this panel's visibility references a real trigger and
        // a real option value" replaces it.

        if violations.is_empty() {
            Ok(ValidForm(self))
        } else {
            Err(violations)
        }
    }
}

fn walk_nodes<'a>(
    nodes: &'a [Node],
    prefix: &str,
    master: &Language,
    languages: &BTreeSet<Language>,
    names: &mut HashMap<String, String>,
    choice_nodes: &mut HashMap<&'a str, (String, &'a Node)>,
    violations: &mut Vec<Violation>,
) {
    for (index, node) in nodes.iter().enumerate() {
        let pointer = format!("{prefix}/{index}");

        check_name(
            node.name().as_str(),
            &format!("{pointer}/common/name"),
            names,
            violations,
        );

        for (field, text) in node.i18n_texts() {
            check_i18n_text(
                text,
                master,
                languages,
                &format!("{pointer}/{field}"),
                violations,
            );
        }
        for (field, text) in node.i18n_rich_texts() {
            check_i18n_rich_text(
                text,
                master,
                languages,
                &format!("{pointer}/{field}"),
                violations,
            );
        }

        if let Some(options) = node.options() {
            choice_nodes.insert(node.name().as_str(), (pointer.clone(), node));
            for (option_index, option) in options.options().iter().enumerate() {
                check_i18n_text(
                    &option.label,
                    master,
                    languages,
                    &format!("{pointer}/options/{option_index}/label"),
                    violations,
                );
            }
        }

        // Visibility, a `Component`'s own layout width/offset, and a
        // repeatable's min/max-occur ordering used to be checked here
        // against typed `Node::Panel`/`Node::Repeatable` fields. All three
        // are now ordinary agent-authored `properties` on a generic
        // `Component` (see `Node`'s own doc), so there is no longer a
        // typed field here to check without reaching into an untyped map
        // by a magic key name -- that check moves to a rule instead.
        if let Some(layout) = node.field_layout() {
            check_layout(layout, &format!("{pointer}/layout"), violations);
        }

        if let Some(children) = node.children() {
            walk_nodes(
                children,
                &format!("{pointer}/children"),
                master,
                languages,
                names,
                choice_nodes,
                violations,
            );
        }
    }
}

fn check_name(
    name: &str,
    pointer: &str,
    names: &mut HashMap<String, String>,
    violations: &mut Vec<Violation>,
) {
    if let Some(first) = names.get(name) {
        violations.push(Violation {
            pointer: pointer.to_string(),
            message: format!("name '{name}' is already used at {first}"),
        });
    } else {
        names.insert(name.to_string(), pointer.to_string());
    }
}

fn check_layout(layout: &FieldLayout, pointer: &str, violations: &mut Vec<Violation>) {
    let offset = layout.offset.map(|o| o.value()).unwrap_or(0);
    if layout.width.value() + offset > 12 {
        violations.push(Violation {
            pointer: pointer.to_string(),
            message: format!(
                "width ({}) + offset ({offset}) exceeds the 12-column grid",
                layout.width.value()
            ),
        });
    }
}

fn check_i18n_text(
    text: &I18nText,
    master: &Language,
    languages: &BTreeSet<Language>,
    pointer: &str,
    violations: &mut Vec<Violation>,
) {
    check_i18n_keys(text.languages(), master, languages, pointer, violations);
}

fn check_i18n_rich_text(
    text: &I18nRichText,
    master: &Language,
    languages: &BTreeSet<Language>,
    pointer: &str,
    violations: &mut Vec<Violation>,
) {
    check_i18n_keys(text.languages(), master, languages, pointer, violations);
}

fn check_i18n_keys<'a>(
    keys: impl Iterator<Item = &'a Language>,
    master: &Language,
    languages: &BTreeSet<Language>,
    pointer: &str,
    violations: &mut Vec<Violation>,
) {
    let mut saw_master = false;
    let mut any = false;
    for lang in keys {
        any = true;
        if lang == master {
            saw_master = true;
        }
        if !languages.contains(lang) {
            violations.push(Violation {
                pointer: pointer.to_string(),
                message: format!("language '{lang}' is not declared in metadata.languages"),
            });
        }
    }
    if !any {
        violations.push(Violation {
            pointer: pointer.to_string(),
            message: "must not be empty".to_string(),
        });
    } else if !saw_master {
        violations.push(Violation {
            pointer: pointer.to_string(),
            message: format!("missing translation for the master language '{master}'"),
        });
    }
}
