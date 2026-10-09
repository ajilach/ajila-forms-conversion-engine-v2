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

/// Where a component's `name` must be unique.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NameScope {
    /// Across the whole form: what lets a `bindRef` derivation and a rule
    /// address a node by name alone. The default.
    #[default]
    Form,
    /// Among its siblings only: for a profile whose packages repeat a name
    /// in different places (a repeatable's own Add button, say) and address
    /// nodes by path. The element names the writer writes (`jcr_name`, or
    /// the name without one) must then be unique among siblings too, as JCR
    /// requires.
    Siblings,
}

/// What [`AemForm::validate_with`] holds a form to beyond its types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ValidateOptions {
    /// Where a component's name must be unique.
    pub names: NameScope,
    /// Whether a form may have no pages and a page no children. Both are
    /// legal AEM, but an agent that authors either has usually lost content,
    /// so the default refuses them.
    pub allow_empty: bool,
}

impl AemForm {
    pub fn validate(self) -> Result<ValidForm, Vec<Violation>> {
        self.validate_with(ValidateOptions::default())
    }

    /// [`Self::validate`], held to `options`.
    pub fn validate_with(self, options: ValidateOptions) -> Result<ValidForm, Vec<Violation>> {
        let scope = options.names;
        let mut violations = Vec::new();

        if self.pages.is_empty() && !options.allow_empty {
            violations.push(Violation {
                pointer: "/pages".to_string(),
                message: "a form must have at least one page".to_string(),
            });
        }

        // Chrome the writer has no place for would be dropped without a word.
        for (field, chrome) in [("page_content", &self.metadata.page_content), ("chrome", &self.metadata.chrome)] {
            if chrome.items.is_some() {
                violations.push(Violation {
                    pointer: format!("/metadata/{field}/items"),
                    message: "this level writes no `items` of its own".to_string(),
                });
            }
        }
        if self.metadata.toolbar.is_empty() && !self.metadata.toolbar_chrome.is_empty() {
            violations.push(Violation {
                pointer: "/metadata/toolbar_chrome".to_string(),
                message: "a form without a toolbar writes no toolbar chrome".to_string(),
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
        let mut page_elements: HashMap<String, String> = HashMap::new();
        let mut choice_nodes: HashMap<&str, (String, &Node)> = HashMap::new();

        for (page_index, page) in self.pages.iter().enumerate() {
            let page_pointer = format!("/pages/{page_index}");

            if page.children.is_empty() && !options.allow_empty {
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
            if scope == NameScope::Siblings {
                let element = page.jcr_name.as_ref().map_or(page.name.as_str(), |n| n.as_str());
                check_element(element, &format!("{page_pointer}/jcr_name"), &mut page_elements, &mut violations);
            }

            // `page.title`/`page.layout` used to be typed fields checked
            // here directly. `Page` now carries `properties` -- the same
            // open, agent-authored bag `Node::Component` uses (see that
            // field's own doc) -- so whatever a page's own title/layout
            // properties should satisfy is a rule's job now, not a
            // structural check this function can still perform without
            // reaching into an untyped map by a magic key name.

            // Siblings scope: a page's children are their own namespace.
            let mut page_names = HashMap::new();
            walk_nodes(
                &page.children,
                &format!("{page_pointer}/children"),
                &Walk { master: &master, languages: &languages, scope },
                match scope {
                    NameScope::Form => &mut names,
                    NameScope::Siblings => &mut page_names,
                },
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

/// What every level of [`walk_nodes`] reads and none changes.
struct Walk<'a> {
    master: &'a Language,
    languages: &'a BTreeSet<Language>,
    scope: NameScope,
}

fn walk_nodes<'a>(
    nodes: &'a [Node],
    prefix: &str,
    walk: &Walk,
    names: &mut HashMap<String, String>,
    choice_nodes: &mut HashMap<&'a str, (String, &'a Node)>,
    violations: &mut Vec<Violation>,
) {
    let (master, languages, scope) = (walk.master, walk.languages, walk.scope);
    let mut elements = HashMap::new();
    for (index, node) in nodes.iter().enumerate() {
        let pointer = format!("{prefix}/{index}");

        check_name(
            node.name().as_str(),
            &format!("{pointer}/common/name"),
            names,
            violations,
        );
        if scope == NameScope::Siblings {
            let common = node.common();
            let element = common.jcr_name.as_ref().map_or(common.name.as_str(), |n| n.as_str());
            check_element(element, &format!("{pointer}/common/jcr_name"), &mut elements, violations);
        }
        check_leaf_passthrough(node, &pointer, violations);

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
            // Siblings scope: each list of children is its own namespace.
            let mut own = HashMap::new();
            walk_nodes(
                children,
                &format!("{pointer}/children"),
                walk,
                match scope {
                    NameScope::Form => &mut *names,
                    NameScope::Siblings => &mut own,
                },
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

/// What a leaf's passthrough asks for that its writer has no place for: an
/// `items` slot (a leaf writes no `items`) or a `cq:responsive` of its own
/// (the writer derives it from the layout). Written, either would be dropped
/// or doubled.
fn check_leaf_passthrough(node: &Node, pointer: &str, violations: &mut Vec<Violation>) {
    if node.children().is_some() {
        return;
    }
    let passthrough = &node.common().passthrough;
    if passthrough.slot.is_some() || passthrough.items.is_some() {
        violations.push(Violation {
            pointer: format!("{pointer}/common/passthrough"),
            message: "a leaf writes no `items`, so it takes no `slot` or `items`".to_string(),
        });
    }
    if node.field_layout().is_some() && passthrough.raw_children.iter().any(|c| c.tag_name == "cq:responsive") {
        violations.push(Violation {
            pointer: format!("{pointer}/common/passthrough/raw_children"),
            message: "a field's `cq:responsive` is written from its layout".to_string(),
        });
    }
}

/// An element name repeated among siblings: JCR holds one child per name.
fn check_element(
    element: &str,
    pointer: &str,
    elements: &mut HashMap<String, String>,
    violations: &mut Vec<Violation>,
) {
    if let Some(first) = elements.get(element) {
        violations.push(Violation {
            pointer: pointer.to_string(),
            message: format!("element '{element}' is already written at {first}"),
        });
    } else {
        elements.insert(element.to_string(), pointer.to_string());
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
