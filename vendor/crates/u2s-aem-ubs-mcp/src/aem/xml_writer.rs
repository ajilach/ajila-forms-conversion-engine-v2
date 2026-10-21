//! XML serialization of an `AemNode` tree into AEM JCR content XML.
//!
//! The tree is normalized and analysed here ([`RenderIndex`]), lowered onto
//! the generic `u2s_aem` model (`super::lower`), and encoded by
//! `u2s_mapper_aem`: this crate writes no XML tags of its own for the form.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use super::{AemConfig, AemNode, ConditionRule, OptionAlignment, Passthrough};
use crate::value::InputValue;

/// No fidelity passthrough (an authored document): every node is written
/// from its own fields alone.
fn no_passthrough() -> &'static HashMap<Uuid, Passthrough> {
    use std::sync::OnceLock;
    static EMPTY: OnceLock<HashMap<Uuid, Passthrough>> = OnceLock::new();
    EMPTY.get_or_init(HashMap::new)
}

// ============================================================================
// Public API
// ============================================================================

/// The form page `.content.xml` of an `AemNode` tree (starting from `Root`):
/// the tree lowered to the generic AEM model (`super::lower`) and written by
/// the generic writer. An error names what the generic model or writer
/// refused.
pub fn generate_aem_xml(root: &AemNode, config: &AemConfig) -> Result<String, String> {
    generate_aem_xml_with_passthrough(root, config, no_passthrough())
}

/// Like [`generate_aem_xml`], but re-emits each node's captured fidelity
/// [`Passthrough`] (raw attributes + unmodeled child elements), keyed by node
/// uuid. Used when saving a working tree that was loaded from an existing
/// package, so a load→save round-trip preserves attributes/children the typed
/// model does not represent. Pass an empty map for the from-XFA engine path.
pub fn generate_aem_xml_with_passthrough(
    root: &AemNode,
    config: &AemConfig,
    pass: &HashMap<Uuid, Passthrough>,
) -> Result<String, String> {
    // Three of the swept feedback rules are about a node's position among its
    // siblings, or about a node that has to exist twice in different roles, so
    // no single node can satisfy them. They are applied here, to a copy, which is
    // what makes them hold for an agent-authored and a loaded tree as well as
    // for one built from an XFA source. See `super::normalize`.
    let mut root = root.clone();
    super::normalize::normalize(&mut root);
    let index = RenderIndex::build(&root)?;
    super::lower::form_xml(&root, config, &index, pass)
}

// ============================================================================
// Panel visibility map (AABO-style `fd:visible` SHOW_EXPRESSION)
// ============================================================================

/// Maps a conditional panel's AEM `name` to the `(trigger_field, value)` pairs
/// that should make it visible. Built by inverting the `ConditionRule`s that
/// the converter wired onto trigger fields (radio/checkbox/dropdown).
type PanelVisibilityMap = HashMap<String, Vec<(String, InputValue)>>;

/// Per-render indices derived from the whole tree before any node is written.
///
/// Both answer a question a single node cannot: which trigger values reveal this
/// panel, and which panels does this choice decide. They are built once, from the
/// root, because a node's own subtree does not contain the answer.
pub(super) struct RenderIndex {
    pub(super) visibility: PanelVisibilityMap,
    /// Every node's AEM guide path (`guide.guideRootPanel.PN_Page.PN_Address`),
    /// keyed by uuid. A rule that names the component it runs on -- the address
    /// fragment's Initialize is the one that must, see `scripts::fragment_init` -- needs
    /// the path from the root, which a node cannot know on its own.
    pub(super) guide_paths: HashMap<Uuid, String>,
    /// Trigger field name → the panels it decides, for the choices of the
    /// form configurator band only. Absent means "no reset for this field".
    pub(super) resets: HashMap<String, Vec<ResetTarget>>,
    /// Input name → the inputs that copy its value (`InputProps::copy_from`),
    /// in document order. Absent means nothing copies it.
    pub(super) copies: HashMap<String, Vec<String>>,
    /// Repeatable name → what a reader would say it repeats. Absent means
    /// nothing on screen names it.
    pub(super) add_subjects: HashMap<String, String>,
    /// Repeatable name → the signature panel that repeats in step with it, under
    /// the name AEM knows it by. Absent means the form holds no such panel.
    pub(super) signature_twins: HashMap<String, String>,
    /// The inverse, by node name: a signature twin → the data panel driving it.
    /// A twin has no buttons of its own, and takes its title from that panel.
    pub(super) twin_data_panels: HashMap<String, String>,
    /// Signature repeatable name → where its signers' names are filled from:
    /// the twin of a party whose partner generic asks a person's name.
    pub(super) name_fills: HashMap<String, super::scripts::NameFill>,
    /// The repeatables that carry the jump-to-field button, which on a page with
    /// repeatables is where it belongs — one per row, rather than one above the
    /// step heading. Only the page knows, so it is decided here.
    pub(super) jump_to_field_repeatables: HashSet<String>,
    /// The first wizard step: the first page panel directly under the root.
    /// Its title is the subtitle under the form title when the page opens with
    /// the banking relationship, and the configurator's own when it is the
    /// configurator. `None` for a form with no page.
    pub(super) first_page: Option<Uuid>,
}

impl RenderIndex {
    pub(super) fn build(root: &AemNode) -> Result<Self, String> {
        let twins = collect_signature_twins(root)?;
        Ok(Self {
            visibility: collect_panel_visibility(root),
            guide_paths: collect_guide_paths(root),
            resets: collect_configurator_resets(root),
            copies: collect_copies(root),
            add_subjects: collect_add_subjects(root),
            name_fills: collect_name_fills(root, &twins.1),
            signature_twins: twins.0,
            twin_data_panels: twins.1,
            jump_to_field_repeatables: collect_jump_to_field_repeatables(root),
            first_page: first_page(root),
        })
    }
}

/// The uuid of the first page panel directly under the root.
fn first_page(root: &AemNode) -> Option<Uuid> {
    let AemNode::Root { children, .. } = root else {
        return None;
    };
    children.iter().find_map(|child| match child {
        AemNode::Panel { uuid, is_page: true, .. } => Some(*uuid),
        _ => None,
    })
}

/// The longest a subject may be before it stops being a subject and starts being
/// prose: a repeatable's title is a noun phrase of at most six words naming one
/// row (conversion guidelines, repeatable titles).
const MAX_SUBJECT_WORDS: usize = 6;
const MAX_SUBJECT_CHARS: usize = 60;

/// The longest own title a repeatable of signatures (`affrg_SignatureGeneric1`)
/// may carry as its subject. Such a block is named after who signs, and that is
/// often an entity line, which runs longer than a party's name: the bank's
/// officers sign as "UBS Europe SE (Succursale Italia)", five words.
const MAX_SIGNATURE_SUBJECT_WORDS: usize = 8;

/// The fragment a repeatable of signatures wraps.
const SIGNATURE_GENERIC: &str = "affrg_SignatureGeneric1";

/// Words that name no entity, so a heading that is only this is no subject.
const SUBJECT_STOP_WORDS: &[&str] = &[
    "name", "nome", "nombre", "no", "nr", "number", "details", "data", "daten",
];

/// What every repeatable in the tree repeats, so its Add button can say so.
///
/// A repeatable is titleless in an engine-authored tree, so the answer is
/// whatever names the block on screen: its own title if it has one, else the
/// nearest heading above it among its siblings, else the enclosing panel's
/// title. That is the same evidence a reader uses, and the same order the
/// feedback rule reads it in (PROBLEM-repeatable-add-label).
///
/// `pub(crate)` because the package writer has to translate the same labels this
/// module writes into the form, and both must resolve the same subject.
pub(crate) fn collect_add_subjects(root: &AemNode) -> HashMap<String, String> {
    let mut map = HashMap::new();
    collect_add_subjects_rec(root, None, &mut map);
    map
}

fn collect_add_subjects_rec(
    node: &AemNode,
    inherited: Option<&str>,
    map: &mut HashMap<String, String>,
) {
    // The title in force for this node's children: this panel's own, falling back
    // to the one it inherited. Only panels contribute one — the root's title is
    // the form's name, never the name of a block inside it.
    let own_title = match node {
        AemNode::Panel { title, .. } => sane_subject(title),
        _ => None,
    };
    let in_force = own_title.as_deref().or(inherited);

    let children = match node {
        AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } => children.as_slice(),
        _ => &[],
    };

    // Headings are siblings, not parents: track the last one seen so a
    // repeatable that follows it can claim it.
    let mut heading: Option<String> = None;
    for child in children {
        match child {
            AemNode::TitleDraw { content, .. } => heading = sane_subject(content),
            AemNode::Repeatable { name, title, .. } => {
                // A title equal to the repeatable's own name is the
                // deterministic converter's sentinel for "nothing names this" --
                // `convert_repeatable` always sets it to the generated name, so
                // treating it as real text would write that hex-suffixed name as
                // the row heading (`Add RCP_1120c`). Only a title that differs
                // from the node's own name can be someone's real subject.
                let words = if wraps_signatures(child) { MAX_SIGNATURE_SUBJECT_WORDS } else { MAX_SUBJECT_WORDS };
                let own_title = (title != name).then(|| subject_of(title, words)).flatten();
                if let Some(subject) = own_title
                    .or_else(|| heading.clone())
                    .or_else(|| in_force.map(String::from))
                {
                    map.insert(name.clone(), subject);
                }
            }
            _ => {}
        }
        collect_add_subjects_rec(child, in_force, map);
    }
}

/// Whether a repeatable is a block of signatures: it wraps the generic
/// signature fragment.
fn wraps_signatures(repeatable: &AemNode) -> bool {
    node_children(repeatable)
        .iter()
        .any(|c| matches!(c, AemNode::Fragment { frag_ref, .. } if frag_ref.ends_with(SIGNATURE_GENERIC)))
}

/// `text` as a subject, or `None` when it names nothing usable.
///
/// Strips the decoration a heading carries — numbering, trailing colons, markup —
/// and rejects what is left if it reads as prose rather than as the name of a
/// thing. A wrong subject is worse than none: the button would name something the
/// panel is not about.
fn sane_subject(text: &str) -> Option<String> {
    subject_of(text, MAX_SUBJECT_WORDS)
}

/// [`sane_subject`], allowing up to `max_words` words.
fn subject_of(text: &str, max_words: usize) -> Option<String> {
    let plain = strip_markup(text);
    // A conditional wrapper's title is a machine label the converter writes
    // (`Condition: <field> = <value>`, converter.rs), not wording anyone chose.
    // A repeatable inside one used to inherit it and announce itself as
    // "Add Condition: Tipo = Individual".
    if plain.trim_start().starts_with("Condition: ") {
        return None;
    }
    // A leading token of nothing but digits and separators is section numbering
    // ("2.", "3.1)"), not part of the name. `2nd holder` keeps its number: the
    // token has letters in it, so it is a word.
    let body = match plain.split_once(char::is_whitespace) {
        Some((first, rest))
            if !first.is_empty()
                && first
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '.' | ')' | '(' | '-')) =>
        {
            rest
        }
        _ => plain.as_str(),
    };
    let trimmed: String = body
        .trim()
        .trim_end_matches(|c: char| c == ':' || c == '*' || c.is_ascii_digit())
        .trim()
        .to_string();
    if trimmed.is_empty()
        || trimmed.chars().count() > MAX_SUBJECT_CHARS
        || trimmed.split_whitespace().count() > max_words
        || SUBJECT_STOP_WORDS.contains(&trimmed.to_lowercase().as_str())
        // A sentence is not a subject.
        || trimmed.ends_with('.')
    {
        return None;
    }
    Some(trimmed)
}

/// Which repeatables carry the jump-to-field button.
///
/// The button jumps from the summary back to what a person filled in, and on a
/// page with repeatables the thing they want back is a row, not the step heading:
/// a repeatable renders one button per instance, and the step-title panel gives
/// its own up (owner directive 2026-08-24, PROBLEM-jump-to-field-button). A page
/// with nothing to fill in gets none at all.
///
/// **Not gated on the form configurator** (owner directive 2026-08-24, restated
/// after AARG_033: the whole first page there sits inside
/// `PN_FormConfigurator_a2439641` — the configurator radio *and* the
/// account-holder rows). Excluding the whole page here suppressed the button on
/// exactly the repeatables — `PN_CPGRP`, `PN_AHGRP` — a person would most want to
/// reach. Only the configurator's own title panel is exempt, which is decided
/// separately where that panel's DoR/summary exclusions are (the first page
/// named `PN_FormConfigurator...` in `Lower::step_title`), not here.
fn collect_jump_to_field_repeatables(root: &AemNode) -> HashSet<String> {
    let mut out = HashSet::new();
    let AemNode::Root { children, .. } = root else {
        return out;
    };
    for page in children {
        let AemNode::Panel {
            children: page_children,
            ..
        } = page
        else {
            continue;
        };
        if !page_children.iter().any(holds_input) {
            continue;
        }
        collect_repeatable_names(page, &mut out);
    }
    out
}

/// Every repeatable at or below `node`, nested ones included — the owner chose
/// every one over outermost-only.
pub(super) fn collect_repeatable_names(node: &AemNode, out: &mut HashSet<String>) {
    if let AemNode::Repeatable { name, .. } = node {
        out.insert(name.clone());
    }
    for child in node_children(node) {
        collect_repeatable_names(child, out);
    }
}

/// A node's children, empty for the shapes that hold none.
fn node_children(node: &AemNode) -> &[AemNode] {
    match node {
        AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } => children.as_slice(),
        _ => &[],
    }
}

/// The twin pairs: data Repeatable → its twin's repeat panel, and twin → data Repeatable.
type TwinPairs = (HashMap<String, String>, HashMap<String, String>);

/// Which signature panel repeats in step with which data panel.
///
/// A party's signature block is a panel of its own, on the signature step, that
/// has to gain and lose a row whenever the party's data panel does. It has no Add
/// and no Remove of its own (one there would let the two desync), so the data
/// panel's buttons drive both, naming the twin globally because it lives on
/// another step (PROBLEM-repeating-panel §8).
///
/// The pairing is authored, never guessed from names: a signature Repeatable
/// names its party in `twin_of`, the party's Repeatable or the fragment that
/// Repeatable wraps. A signature block that repeats on its own keeps its own
/// buttons whatever it is called. A `twin_of` naming nothing that repeats is
/// refused: an `addInstance` naming a panel that is not there throws and takes
/// the rest of the click with it.
fn collect_signature_twins(root: &AemNode) -> Result<TwinPairs, String> {
    // Each repeatable by its own name and by the names of the fragments it
    // wraps (`RCP_CPGRP` wraps `PN_CPGRP`): its rows live in the inner panel
    // the writer emits, not in the node the tree names.
    let mut repeating_names: HashMap<String, String> = HashMap::new();
    collect_repeating_names(root, &mut repeating_names);

    let mut twins = HashMap::new();
    let mut data_panels = HashMap::new();
    let mut problems = Vec::new();
    collect_signature_twins_rec(root, &repeating_names, &mut twins, &mut data_panels, &mut problems);
    if problems.is_empty() {
        Ok((twins, data_panels))
    } else {
        Err(problems.join("; "))
    }
}

/// Each Repeatable's name, and each fragment it wraps, → the Repeatable.
fn collect_repeating_names(node: &AemNode, out: &mut HashMap<String, String>) {
    if let AemNode::Repeatable { name, children, .. } = node {
        out.insert(name.clone(), name.clone());
        for fragment in children.iter().filter(|c| matches!(c, AemNode::Fragment { .. })) {
            if let Some(fragment_name) = fragment.name() {
                out.insert(fragment_name.to_string(), name.clone());
            }
        }
    }
    for child in node_children(node) {
        collect_repeating_names(child, out);
    }
}

fn collect_signature_twins_rec(
    node: &AemNode,
    names: &HashMap<String, String>,
    twins: &mut HashMap<String, String>,
    data_panels: &mut HashMap<String, String>,
    problems: &mut Vec<String>,
) {
    if let AemNode::Repeatable { name, twin_of: Some(party), .. } = node {
        match names.get(party.as_str()) {
            None => problems.push(format!(
                "`{name}` repeats with `{}`, which is no Repeatable of the form nor a fragment \
                 one wraps",
                party.as_str()
            )),
            Some(data) if data == name => {
                problems.push(format!("`{name}` repeats with itself"));
            }
            Some(data) if twins.contains_key(data) => problems.push(format!(
                "`{data}` drives more than one signature twin; a party has one"
            )),
            Some(data) => {
                twins.insert(data.clone(), repeat_panel_name(name));
                data_panels.insert(name.clone(), data.clone());
            }
        }
    }
    for child in node_children(node) {
        collect_signature_twins_rec(child, names, twins, data_panels, problems);
    }
}


/// Drop any tags from a rich-text title and collapse the whitespace.
pub(super) fn strip_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether a panel is the form configurator band: the panel holding the
/// choices that decide which of the form's variants is filled in. Its name is
/// the one the configurator is always given (`PN_FormConfigurator`, with or
/// without a suffix).
pub(super) fn is_configurator_band(name: &str) -> bool {
    name.starts_with("PN_FormConfigurator")
}

/// Walk the tree and, for every choice of the form configurator band, record
/// the panels it decides (UBS-CFG-4): changing the configurator empties
/// everything it shows or hides, whatever its option labels.
fn collect_configurator_resets(root: &AemNode) -> HashMap<String, Vec<ResetTarget>> {
    let mut map = HashMap::new();
    collect_configurator_resets_rec(root, root, false, &mut map);
    map
}

fn collect_configurator_resets_rec(
    root: &AemNode,
    node: &AemNode,
    in_band: bool,
    map: &mut HashMap<String, Vec<ResetTarget>>,
) {
    let choice = match node {
        AemNode::RadioButton {
            name,
            conditions,
            ..
        }
        | AemNode::Dropdown {
            name,
            conditions,
            ..
        }
        // A configurator checkbox (the financial intermediary flag) empties
        // what it shows when it is unticked, like any other configurator.
        | AemNode::Checkbox {
            name,
            conditions,
            ..
        } => Some((name, conditions)),
        _ => None,
    };
    if let Some((name, conditions)) = choice
        && in_band
    {
        let targets = reset_targets_for(root, conditions);
        if !targets.is_empty() {
            map.insert(name.clone(), targets);
        }
    }

    let in_band = in_band || matches!(node, AemNode::Panel { name, .. } if is_configurator_band(name));
    match node {
        AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } => {
            for child in children {
                collect_configurator_resets_rec(root, child, in_band, map);
            }
        }
        _ => {}
    }
}

/// Every input other inputs copy (`InputProps::copy_from`), with the inputs
/// that copy it, in document order.
fn collect_copies(root: &AemNode) -> HashMap<String, Vec<String>> {
    fn walk(node: &AemNode, map: &mut HashMap<String, Vec<String>>) {
        if let (Some(name), Some(source)) = (
            node.name(),
            node.input().and_then(|input| input.copy_from.as_ref()),
        ) {
            map.entry(source.as_str().to_owned()).or_default().push(name.to_owned());
        }
        for child in node_children(node) {
            walk(child, map);
        }
    }
    let mut map = HashMap::new();
    walk(root, &mut map);
    map
}

/// Walk the node tree and invert every trigger field's `show` conditions into
/// a `panel_name → [(trigger_field, value), …]` map.
fn collect_panel_visibility(root: &AemNode) -> PanelVisibilityMap {
    let mut map = PanelVisibilityMap::new();
    collect_panel_visibility_rec(root, &mut map);
    map
}

fn collect_panel_visibility_rec(node: &AemNode, map: &mut PanelVisibilityMap) {
    match node {
        AemNode::RadioButton {
            name, conditions, ..
        }
        | AemNode::Checkbox {
            name, conditions, ..
        }
        | AemNode::Dropdown {
            name, conditions, ..
        } => {
            for rule in conditions {
                if rule.show {
                    map.entry(rule.target_panel_name.clone())
                        .or_default()
                        .push((name.clone(), rule.value.clone()));
                }
            }
        }
        _ => {}
    }

    match node {
        AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } => {
            for child in children {
                collect_panel_visibility_rec(child, map);
            }
        }
        _ => {}
    }
}

/// Whether a subtree holds anything a user can fill in.
///
/// This is what decides a wizard step's jump-to-field button. The button jumps
/// the reader back to a field, so a step that is pure text — legal provisions, a
/// declaration, terms to accept — has nothing to jump to and must not offer one
/// (owner directive, 2026-08-10; 177 steps across 102 forms carried one wrongly).
///
/// A fragment counts as input even though its children are not in this package:
/// the fields live in the fragment's own package, and in this corpus these are
/// signature, banking and address fragments, all fillable.
///
/// Draws, footnotes and prefaces are static text and count for
/// nothing, which is exactly the case this exists to detect.
pub(super) fn holds_input(node: &AemNode) -> bool {
    match node {
        AemNode::TextField { .. }
        | AemNode::NumberField { .. }
        | AemNode::DatePicker { .. }
        | AemNode::Dropdown { .. }
        | AemNode::Checkbox { .. }
        | AemNode::RadioButton { .. }
        | AemNode::Fragment { .. }
        // Its date is the person's to pick, whatever else it holds.
        | AemNode::Letterhead { .. } => true,

        AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } => children.iter().any(holds_input),

        AemNode::TextDraw { .. }
        | AemNode::TitleDraw { .. }
        | AemNode::HtmlDisplayer { .. }
        | AemNode::MessageBox { .. }
        | AemNode::Preface { .. }
        | AemNode::FootnotePlaceholder { .. } => false,
    }
}

// ============================================================================
// Configurator reset-on-change (feedback #107)
// ============================================================================

/// One panel a configurator choice decides, and the repeatables inside it.
///
/// The repeatables are reset first: a repeatable has to drop its added rows, not
/// just blank them, so the row count is back to its declared minimum before the
/// remaining fields are cleared.
use super::scripts::ResetTarget;

/// The panels a trigger field reveals, in document order, each with the
/// repeatables nested inside it.
///
/// The set is read from the same `ConditionRule`s that drive the panels'
/// `fd:visible` expressions, so the reset covers exactly what the choice shows
/// and hides — no more. Order follows the panels' order in the tree, which is
/// what makes the generated script stable across runs.
fn reset_targets_for(root: &AemNode, conditions: &[ConditionRule]) -> Vec<ResetTarget> {
    let mut wanted: Vec<&str> = Vec::new();
    for rule in conditions.iter().filter(|r| r.show) {
        if !wanted.contains(&rule.target_panel_name.as_str()) {
            wanted.push(&rule.target_panel_name);
        }
    }
    if wanted.is_empty() {
        return Vec::new();
    }

    let mut ordered: Vec<ResetTarget> = Vec::new();
    collect_reset_targets(root, &wanted, &mut ordered);
    // Two panels can share a name. Not `dedup_by`: they need not be adjacent
    // in the tree, and only the first occurrence should be reset (resetting
    // twice says nothing, but it does mislead anyone reading the script).
    let mut seen = std::collections::HashSet::new();
    ordered.retain(|t| seen.insert(t.panel.clone()));
    ordered
}

fn collect_reset_targets(node: &AemNode, wanted: &[&str], out: &mut Vec<ResetTarget>) {
    if let AemNode::Panel { name, children, .. } = node
        && wanted.contains(&name.as_str())
    {
        out.push(ResetTarget {
            panel: name.clone(),
            repeats: repeatable_panels(children),
        });
        // Panels the choice decides are not nested inside one another, and
        // descending would attribute a nested match to the wrong parent.
        return;
    }
    match node {
        AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } => {
            for child in children {
                collect_reset_targets(child, wanted, out);
            }
        }
        _ => {}
    }
}

/// The prefixes the naming convention accepts on a repeat-container panel; the
/// first is what a name that has none of them is given.
const REPEAT_PREFIXES: &[&str] = &["RCP", "RCBP", "RCHP", "RCHT"];

/// `name` under a repeat-container prefix: kept if it already carries one,
/// otherwise its leading type prefix is swapped for `RCP_`, or one is prepended
/// when it has no prefix at all.
///
/// The two panels a repeatable is rendered with are named by the engine, not by
/// whoever authored the tree, so their prefixes are the engine's to get right —
/// and a repeat panel must not inherit `PN_` (a plain panel) or a legacy `RP_`
/// just because the node it was derived from carries one.
fn with_repeat_prefix(name: &str) -> String {
    let (prefix, stem) = match name.split_once('_') {
        // A leading run of capitals is a type prefix; anything else is the name
        // itself (`Portfolio_ID` is not prefixed by `Portfolio`).
        Some((first, rest))
            if !first.is_empty()
                && first.len() <= 5
                && first.chars().all(|c| c.is_ascii_uppercase())
                && !rest.is_empty() =>
        {
            (Some(first), rest)
        }
        _ => (None, name),
    };
    match prefix {
        Some(p) if REPEAT_PREFIXES.contains(&p) => name.to_string(),
        _ => format!("{}_{}", REPEAT_PREFIXES[0], stem),
    }
}

/// The hidden field a signature repeatable's name fill runs on, named after
/// the repeatable (`RCP_SGN_CPGRP` → `TXT_SGN_CPGRP_NameFill`).
pub(crate) fn name_fill_anchor(name: &str) -> String {
    format!("TXT_{}_NameFill", name.strip_prefix("RCP_").unwrap_or(name))
}

/// Where each paired signature's name is filled from (UBS-SIG-4): for every
/// signature repeatable that is a party's twin (`twin_data_panels`), wrapping
/// the signature generic, the party's partner generic that asks a person's
/// name (its `PN_IndividualBasic` not hidden; the first such copy of a split
/// party). A twin of anything else (a hand-built party, a company alone) has
/// no name to take and gets none.
fn collect_name_fills(
    root: &AemNode,
    twin_data_panels: &HashMap<String, String>,
) -> HashMap<String, super::scripts::NameFill> {
    use super::fragment_init::{SIGNATURE_GENERIC, file_of};
    fn repeatables<'a>(node: &'a AemNode, out: &mut HashMap<&'a str, &'a [AemNode]>) {
        if let AemNode::Repeatable { name, children, .. } = node {
            out.entry(name.as_str()).or_insert(children.as_slice());
        }
        for child in node_children(node) {
            repeatables(child, out);
        }
    }
    let mut rows = HashMap::new();
    repeatables(root, &mut rows);
    let mut fills = HashMap::new();
    for (signature, party) in twin_data_panels {
        let (Some(sign_children), Some(party_children)) =
            (rows.get(signature.as_str()), rows.get(party.as_str()))
        else {
            continue;
        };
        let sign_fragment = sign_children.iter().find_map(|c| match c {
            AemNode::Fragment { name, frag_ref, .. } if file_of(frag_ref) == SIGNATURE_GENERIC => Some(name),
            _ => None,
        });
        let party_fragment = party_children.iter().find_map(|c| match c {
            AemNode::Fragment { name, frag_ref, init, .. }
                if super::partner::is_partner_generic(frag_ref)
                    && !init.init_hide.iter().any(|p| p.as_str() == "PN_IndividualBasic") =>
            {
                Some(name)
            }
            _ => None,
        });
        let (Some(sign_fragment), Some(party_fragment)) = (sign_fragment, party_fragment) else {
            continue;
        };
        fills.insert(
            signature.clone(),
            super::scripts::NameFill {
                data_repeat: repeat_panel_name(party),
                data_path: format!(
                    "{}.{party_fragment}.PN_IndividualBasic.PN_Name_Individual",
                    repeat_row_name(party)
                ),
                sign_repeat: repeat_panel_name(signature),
                sign_path: format!(
                    "{}.{sign_fragment}.PN_GenericSignature.TXT_Name_Generic",
                    repeat_row_name(signature)
                ),
            },
        );
    }
    fills
}

/// The instance-managed panel's name, as the repeatable lowering writes it.
pub(crate) fn repeat_panel_name(name: &str) -> String {
    format!("{}_repeat", with_repeat_prefix(name))
}

/// The row panel's name, as the repeatable lowering writes it. One row of the repeat,
/// holding the repeated fields.
pub(crate) fn repeat_row_name(name: &str) -> String {
    format!("{}_inner", with_repeat_prefix(name))
}

/// The instance-managed panel of every repeatable in a subtree, in document
/// order. The repeatable lowering names it after the repeatable, and that is the node
/// `resetAllPanelInstances` has to be given.
fn repeatable_panels(children: &[AemNode]) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(node: &AemNode, out: &mut Vec<String>) {
        if let AemNode::Repeatable { name, .. } = node {
            out.push(repeat_panel_name(name));
        }
        match node {
            AemNode::Root { children, .. }
            | AemNode::Panel { children, .. }
            | AemNode::Repeatable { children, .. } => {
                for child in children {
                    walk(child, out);
                }
            }
            _ => {}
        }
    }
    for child in children {
        walk(child, &mut out);
    }
    out
}

/// Map every node to its AEM guide path.
///
/// The path is what AEM calls a component by from the form root
/// (`guide.guideRootPanel.<panel>.<…>.<component>`), and it is how a rule names
/// the component it is attached to. Segments come from the JCR nesting, so a
/// repeatable contributes three of them -- the wrapper, the instance-managed
/// panel and the row -- exactly as the repeatable lowering writes them.
fn collect_guide_paths(root: &AemNode) -> HashMap<Uuid, String> {
    fn walk(node: &AemNode, prefix: &[String], out: &mut HashMap<Uuid, String>) {
        let own = node.name().unwrap_or("");
        let mut here = prefix.to_vec();
        if !own.is_empty() {
            here.push(own.to_string());
            if let Some(uuid) = node.uuid() {
                out.insert(uuid, format!("guide.guideRootPanel.{}", here.join(".")));
            }
        }
        // A repeatable's children live two panels deeper than the repeatable
        // itself.
        if let AemNode::Repeatable { name, .. } = node {
            here.push(repeat_panel_name(name));
            here.push(repeat_row_name(name));
        }
        let children = match node {
            AemNode::Root { children, .. }
            | AemNode::Panel { children, .. }
            | AemNode::Repeatable { children, .. } => children.as_slice(),
            _ => &[],
        };
        for child in children {
            walk(child, &here, out);
        }
    }

    let mut out = HashMap::new();
    // The Root renders as `guideRootPanel` itself, so its own name is not a
    // segment: its children are the first ones.
    if let AemNode::Root { children, .. } = root {
        for child in children {
            walk(child, &[], &mut out);
        }
    }
    out
}


// ============================================================================
// Conditional visibility scripts (fd:scripts fd:visible SHOW_EXPRESSION)
// ============================================================================
//
// This matches the reference UBS form (AF_AABO): each dynamically-hidden panel
// carries a `fd:visible` SHOW_EXPRESSION on the panel itself. The expression
// returns the boolean visibility AND, as a side effect, calls the UBS DOR
// helpers so the panel is included/excluded from the Document of Record.
//
// The rule itself is `scripts::show`; what is left here is turning each
// trigger value into its string form for the `==` comparison.

/// Render a single `InputValue` as the string used in a `==` comparison.
pub(crate) fn condition_value_str(value: &InputValue) -> String {
    match value {
        InputValue::Text(s) => s.clone(),
        InputValue::Number(n) => n.to_string(),
        InputValue::Bool(b) => b.to_string(),
    }
}

// ============================================================================
// Attribute helpers
// ============================================================================

pub(super) fn alignment_str(a: OptionAlignment) -> &'static str {
    match a {
        OptionAlignment::Horizontal => "horizontal",
        OptionAlignment::Vertical => "vertical",
    }
}


// ============================================================================
// Attribute reformatting (one-per-line)
// ============================================================================

/// Reformat XML so that element attributes appear one per line, indented to
/// align with the first attribute.
///
/// Turns:
/// ```xml
///     <tag attr1="v1" attr2="v2">
/// ```
/// into:
/// ```xml
///     <tag
///         attr1="v1"
///         attr2="v2">
/// ```
///
/// Only elements with more than one attribute are reformatted.
pub(crate) fn reformat_attributes(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len() + xml.len() / 4);

    for line in xml.lines() {
        if let Some(reformatted) = try_reformat_line(line) {
            out.push_str(&reformatted);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }

    out
}

/// Try to reformat a single XML line. Returns `None` if the line should be
/// kept as-is (not an element, or has ≤1 attribute).
fn try_reformat_line(line: &str) -> Option<String> {
    let trimmed = line.trim_start();

    // Must start with '<' and not be a closing tag, comment, PI, or declaration
    if !trimmed.starts_with('<')
        || trimmed.starts_with("</")
        || trimmed.starts_with("<?")
        || trimmed.starts_with("<!")
    {
        return None;
    }

    // Find the leading indentation
    let indent = &line[..line.len() - trimmed.len()];

    // Parse the tag name and attributes.
    // Find the end of the opening tag (matching '>' or '/>').
    let (tag_content, suffix) = extract_tag_content(trimmed)?;

    // Split into tag name and attributes
    let first_space = tag_content.find(' ')?;
    let tag_name = &tag_content[1..first_space]; // skip '<'
    let attrs_str = &tag_content[first_space + 1..];

    // Parse attributes
    let attrs = parse_attributes(attrs_str);
    if attrs.len() <= 1 {
        return None;
    }

    // Build reformatted output
    let attr_indent = format!("{}{}", indent, " ".repeat(tag_name.len() + 2)); // +2 for '<' and space
    let mut result = format!("{}<{}", indent, tag_name);
    for (i, attr) in attrs.iter().enumerate() {
        if i == 0 {
            result.push(' ');
        } else {
            result.push('\n');
            result.push_str(&attr_indent);
        }
        result.push_str(attr);
    }
    result.push_str(suffix);

    Some(result)
}

/// Extract the content between '<' ... '>' or '<' ... '/>', returning
/// (content_without_close, suffix). Suffix is ">" or "/>" or "/>".
fn extract_tag_content(trimmed: &str) -> Option<(&str, &str)> {
    if let Some(stripped) = trimmed.strip_suffix("/>") {
        Some((stripped, "/>"))
    } else if let Some(stripped) = trimmed.strip_suffix('>') {
        Some((stripped, ">"))
    } else {
        None
    }
}

/// Parse a string of XML attributes like `attr1="val1" attr2="val2"` into
/// a vec of individual attribute strings.
fn parse_attributes(s: &str) -> Vec<&str> {
    let mut attrs = Vec::new();
    let mut i = 0;
    let bytes = s.as_bytes();
    let len = bytes.len();

    while i < len {
        // Skip whitespace
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }

        // Start of attribute name
        let start = i;

        // Find '='
        while i < len && bytes[i] != b'=' {
            i += 1;
        }
        if i >= len {
            break;
        }
        i += 1; // skip '='

        // Expect opening quote
        if i >= len {
            break;
        }
        let quote = bytes[i];
        if quote != b'"' && quote != b'\'' {
            break;
        }
        i += 1; // skip opening quote

        // Find closing quote
        while i < len && bytes[i] != quote {
            i += 1;
        }
        if i >= len {
            break;
        }
        i += 1; // skip closing quote

        attrs.push(&s[start..i]);
    }

    attrs
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aem::{AemAttrs, FragmentInit};

    /// The element of `xml` whose `name` is `name`.
    fn element(xml: &str, name: &str) -> u2s_mapper_aem::jcr::tree::JcrNode {
        fn find(node: &u2s_mapper_aem::jcr::tree::JcrNode, name: &str) -> Option<u2s_mapper_aem::jcr::tree::JcrNode> {
            if node.attr("name") == Some(name) {
                return Some(node.clone());
            }
            node.children.iter().find_map(|c| find(c, name))
        }
        let tree = u2s_mapper_aem::jcr::tree::parse_jcr_xml(xml).expect("well-formed form XML");
        find(&tree, name).unwrap_or_else(|| panic!("no element named {name} in:\n{xml}"))
    }

    /// The form XML of a form holding just `node`, written as it would be
    /// inside any form.
    fn render_one(node: &AemNode, config: &AemConfig) -> String {
        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![node.clone()],
        };
        generate_aem_xml(&root, config).expect("the form is written")
    }
    use crate::aem::{
        AemConfig, AemI18nText, AemNode, AemOption, ConditionRule, OptionAlignment, TextFieldKind,
    };
    use uuid::Uuid;

    /// A uuid of its own for each name: JCR holds one child per element name,
    /// and the writer names a node's element after its uuid.
    fn uuid_of(name: &str) -> Uuid {
        Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes())
    }

    /// A minimal config with deterministic ids.
    fn test_config() -> AemConfig {
        let mut config = AemConfig::test_default("TEST");
        config.deterministic_uuids = true;
        config
    }

    fn html_node(content: &[(&str, &str)]) -> AemNode {
        AemNode::HtmlDisplayer {
            visible: true,
            uuid: fixed_uuid(),
            name: "TBL_Plans".into(),
            content: AemI18nText(
                content
                    .iter()
                    .map(|(l, h)| ((*l).to_string(), (*h).to_string()))
                    .collect(),
            ),
            attrs: AemAttrs::default(),
            colspan: 12,
            dor_colspan: None,
        }
    }

    /// The HTML component gets one `localeContent` item per locale the form
    /// ships, numbered positionally, with the markup XML-escaped into the
    /// attribute the way every other rich-text value is.
    #[test]
    fn the_html_component_emits_one_locale_item_per_locale() {
        let mut config = test_config();
        config.languages = vec!["en".into(), "it".into()];
        let root = AemNode::Root {
            title: "Test Form".into(),
            children: vec![html_node(&[
                ("en", "<table><tr><td>Plan 1</td></tr></table>"),
                ("it", "<table><tr><td>Piano 1</td></tr></table>"),
            ])],
        };

        let xml = generate_aem_xml(&root, &config).expect("the form is written");

        assert!(
            xml.contains("<item0") && xml.contains("<item1"),
            "items are positional:\n{xml}"
        );
        assert!(
            !xml.contains("<item2"),
            "two languages, two items -- no third:\n{xml}"
        );
        assert!(
            xml.contains("locale=\"en\"") && xml.contains("locale=\"it\""),
            "each locale is named:\n{xml}"
        );
        assert!(
            xml.contains("&lt;table>") && !xml.contains("<table>"),
            "the markup is escaped INTO the attribute, not emitted as elements:\n{xml}"
        );
        assert!(
            xml.contains("Piano 1"),
            "the Italian markup is the Italian item's, not a copy of the English:\n{xml}"
        );
    }

    /// A synonym locale gets its own item, carrying its canonical language's
    /// markup: the component renders NOTHING for a locale it has no item for,
    /// so a reader on `de-ch` must not fall off the end. Same reason
    /// `generate_dictionary_xml` writes a synonym dictionary file.
    #[test]
    fn a_synonym_locale_gets_its_own_html_item() {
        let mut config = test_config();
        config.languages = vec!["de".into()];
        let root = AemNode::Root {
            title: "Test Form".into(),
            children: vec![html_node(&[(
                "de",
                "<table><tr><td>Plan 1</td></tr></table>",
            )])],
        };

        let xml = generate_aem_xml(&root, &config).expect("the form is written");

        assert!(
            xml.contains("locale=\"de\"") && xml.contains("locale=\"de-ch\""),
            "`de` brings `de-ch` with it:\n{xml}"
        );
        assert_eq!(
            xml.matches("Plan 1").count(),
            2,
            "both items carry the same markup:\n{xml}"
        );
    }

    /// Markup authored under a synonym reaches its canonical locale too: a
    /// document writes Spanish as `es`, and the component's `sp` item must carry
    /// that, not fall back to the master language (BBTB_019 shipped English
    /// tables to every `sp` reader).
    #[test]
    fn the_canonical_locale_takes_markup_authored_under_its_synonym() {
        let mut config = test_config();
        config.languages = vec!["en".into(), "es".into()];
        config.master_language = "en".into();
        let root = AemNode::Root {
            title: "Test Form".into(),
            children: vec![html_node(&[
                ("en", "<table><tr><td>Plan EN</td></tr></table>"),
                ("es", "<table><tr><td>Plan ES</td></tr></table>"),
            ])],
        };

        let xml = generate_aem_xml(&root, &config).expect("the form is written");

        let item = |locale: &str| {
            xml.split("<item")
                .find(|item| item.contains(&format!("locale=\"{locale}\"")))
                .unwrap_or_else(|| panic!("no `{locale}` item:\n{xml}"))
                .to_string()
        };
        assert!(item("sp").contains("Plan ES"), "{}", item("sp"));
        assert!(item("es").contains("Plan ES"), "{}", item("es"));
        assert!(item("en").contains("Plan EN"), "{}", item("en"));
    }

    /// A locale the profile renames is written under the profile's spelling.
    /// Which spelling the deployed component matches on is its business, not
    /// this engine's, so it has to be configurable.
    #[test]
    fn the_profile_decides_the_html_locale_spelling() {
        let mut config = test_config();
        config.languages = vec!["en".into()];
        config.language_synonyms.clear();
        config.html_locales.insert("en".into(), "en-us".into());
        let root = AemNode::Root {
            title: "Test Form".into(),
            children: vec![html_node(&[(
                "en",
                "<table><tr><td>Plan 1</td></tr></table>",
            )])],
        };

        let xml = generate_aem_xml(&root, &config).expect("the form is written");

        assert!(
            xml.contains("locale=\"en-us\""),
            "the profile's spelling wins:\n{xml}"
        );
        assert!(
            !xml.contains("locale=\"en\""),
            "and the short code is not also written:\n{xml}"
        );
    }

    /// A language the markup does not cover falls back to the master language's
    /// markup rather than to nothing: the component renders NOTHING for a locale
    /// it has no item for, and an untranslated table still carries its numbers.
    /// The missing translation is surfaced elsewhere -- the working tree's
    /// outline flags a node whose text exists in only one language.
    #[test]
    fn a_language_without_markup_falls_back_to_the_master() {
        let mut config = test_config();
        config.languages = vec!["en".into(), "it".into()];
        config.language_synonyms.clear();
        let root = AemNode::Root {
            title: "Test Form".into(),
            children: vec![html_node(&[(
                "en",
                "<table><tr><td>Plan 1</td></tr></table>",
            )])],
        };

        let xml = generate_aem_xml(&root, &config).expect("the form is written");

        assert!(
            xml.contains("locale=\"en\"") && xml.contains("locale=\"it\""),
            "the Italian reader still gets an item:\n{xml}"
        );
        assert_eq!(
            xml.matches("Plan 1").count(),
            2,
            "both carry the English markup:\n{xml}"
        );
    }

    /// A locale with no markup is skipped rather than emitted empty: an empty
    /// item renders a blank block, which reads as a broken table rather than as
    /// an absent translation.
    #[test]
    fn a_locale_with_no_html_markup_is_skipped() {
        let mut config = test_config();
        config.languages = vec!["en".into()];
        config.language_synonyms.clear();
        let root = AemNode::Root {
            title: "Test Form".into(),
            children: vec![html_node(&[("en", "   ")])],
        };

        let xml = generate_aem_xml(&root, &config).expect("the form is written");

        let displayer = element(&xml, "TBL_Plans");
        let items: Vec<&str> = displayer
            .children
            .iter()
            .filter(|c| c.tag_name == "localeContent")
            .flat_map(|c| c.children.iter().map(|i| i.tag_name.as_str()))
            .collect();
        assert!(items.is_empty(), "no markup, no item: {items:?}");
    }

    fn fixed_uuid() -> Uuid {
        Uuid::new_v5(&Uuid::from_bytes([0; 16]), b"test")
    }

    #[test]
    fn xml_output_renders_textdraw() {
        let root = AemNode::Root {
            title: "Test Form".into(),
            children: vec![AemNode::TextDraw {
                visible: true,
                uuid: fixed_uuid(),
                name: "ST_1".into(),
                content: "<p>Hello &amp; world</p>".into(),
                attrs: AemAttrs::default(),
                colspan: 12,
                dor_colspan: None,
            }],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        assert!(xml.contains("guideTextDraw"));
        assert!(xml.contains("ST_1"));
    }

    #[test]
    fn text_field_has_responsive_width() {
        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![AemNode::TextField {
                attrs: AemAttrs::default(),
                uuid: fixed_uuid(),
                name: "TF_test".into(),
                label: "Test Label".into(),
                mandatory: false,
                visible: true,
                max_chars: Some(100),
                colspan: 6,
                dor_colspan: None,
                bind_ref: None,
                kind: TextFieldKind::Plain,
                input: Default::default(),
            }],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        assert!(xml.contains("cq:responsive"));
        assert!(xml.contains("width=\"6\""));
        assert!(xml.contains("maxChars=\"100\""));
    }

    #[test]
    fn checkbox_options_serialized() {
        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![AemNode::Checkbox {
                attrs: AemAttrs::default(),
                uuid: fixed_uuid(),
                name: "CB_test".into(),
                label: String::new(),
                options: vec![
                    AemOption {
                        label: "Yes".into(),
                        value: "1".into(),
                    },
                    AemOption {
                        label: "No".into(),
                        value: "0".into(),
                    },
                ],
                alignment: OptionAlignment::Horizontal,
                visible: true,
                colspan: 12,
                dor_colspan: None,
                conditions: vec![],
                bind_ref: None,
                mandatory: false,
                input: Default::default(),
                hide_title: false,
            }],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        assert!(xml.contains("options=\"[1=Yes,0=No]\""));
        assert!(xml.contains("alignment=\"horizontal\""));
    }

    #[test]
    fn radio_options_with_commas_are_escaped() {
        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![AemNode::RadioButton {
                attrs: AemAttrs::default(),
                uuid: fixed_uuid(),
                name: "RB_comma".into(),
                label: "Choose".into(),
                options: vec![
                    AemOption {
                        label: "Yes, definitely".into(),
                        value: "1".into(),
                    },
                    AemOption {
                        label: "No, thanks".into(),
                        value: "2".into(),
                    },
                ],
                alignment: OptionAlignment::Vertical,
                mandatory: false,
                visible: true,
                colspan: 12,
                dor_colspan: None,
                conditions: vec![],
                bind_ref: None,
                hide_title: false,
                input: Default::default(),
            }],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        // Commas inside labels must be escaped as \, so the list stays parseable
        assert!(
            xml.contains(r#"options="[1=Yes\, definitely,2=No\, thanks]""#),
            "Commas in option labels must be backslash-escaped. Got:\n{}",
            xml
        );
    }

    #[test]
    fn dropdown_options_with_commas_are_escaped() {
        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![AemNode::Dropdown {
                attrs: AemAttrs::default(),
                uuid: fixed_uuid(),
                name: "DD_comma".into(),
                label: "Pick".into(),
                options: vec![
                    AemOption {
                        label: "Option A, first".into(),
                        value: "a".into(),
                    },
                    AemOption {
                        label: "Option B".into(),
                        value: "b".into(),
                    },
                ],
                mandatory: false,
                visible: true,
                colspan: 12,
                dor_colspan: None,
                conditions: vec![],
                bind_ref: None,
                input: Default::default(),
            }],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        assert!(
            xml.contains(r#"options="[a=Option A\, first,b=Option B]""#),
            "Commas in dropdown labels must be backslash-escaped. Got:\n{}",
            xml
        );
    }

    #[test]
    fn checkbox_options_with_commas_are_escaped() {
        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![AemNode::Checkbox {
                attrs: AemAttrs::default(),
                uuid: fixed_uuid(),
                name: "CB_comma".into(),
                label: String::new(),
                options: vec![
                    AemOption {
                        label: "I agree, fully".into(),
                        value: "1".into(),
                    },
                    AemOption {
                        label: "No".into(),
                        value: "0".into(),
                    },
                ],
                alignment: OptionAlignment::Horizontal,
                visible: true,
                colspan: 12,
                dor_colspan: None,
                conditions: vec![],
                bind_ref: None,
                mandatory: false,
                input: Default::default(),
                hide_title: false,
            }],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        assert!(
            xml.contains(r#"options="[1=I agree\, fully,0=No]""#),
            "Commas in checkbox labels must be backslash-escaped. Got:\n{}",
            xml
        );
    }

    #[test]
    fn repeatable_has_min_max_occur() {
        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![AemNode::Repeatable {
                attrs: AemAttrs::default(),
                visible: true,
                uuid: fixed_uuid(),
                name: "RCP_1".into(),
                title: "Repeat Section".into(),
                children: vec![],
                min_occur: 1,
                max_occur: 10,
                bind_ref: None,
                frag_ref: None,
                twin_of: None,
            }],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        assert!(xml.contains("minOccur=\"1\""), "missing minOccur");
        assert!(xml.contains("maxOccur=\"10\""), "missing maxOccur");
        assert!(xml.contains("name=\"RCP_1\""));
    }

    #[test]
    fn dropdown_has_options() {
        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![AemNode::Dropdown {
                attrs: AemAttrs::default(),
                uuid: fixed_uuid(),
                name: "DD_test".into(),
                label: "Pick one".into(),
                options: vec![
                    AemOption {
                        label: "A".into(),
                        value: "a".into(),
                    },
                    AemOption {
                        label: "B".into(),
                        value: "b".into(),
                    },
                ],
                mandatory: true,
                visible: true,
                colspan: 12,
                dor_colspan: None,
                conditions: vec![],
                bind_ref: None,
                input: Default::default(),
            }],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        assert!(xml.contains("guideDropDownList"));
        assert!(xml.contains("options=\"[a=A,b=B]\""));
    }

    #[test]
    fn hidden_panel_emits_visible_false() {
        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![AemNode::Panel {
                uuid: fixed_uuid(),
                name: "COND_Panel".into(),
                title: "Hidden Panel".into(),
                children: vec![],
                is_page: false,
                attrs: AemAttrs {
                    dor_exclude: true,
                    ..Default::default()
                },
                visible: false,
                is_conditional: true,
                dor_num_cols: None,
                colspan: 12,
                dor_colspan: None,
                bind_ref: None,
                frag_ref: None,
            }],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        assert!(
            xml.contains("visible=\"{Boolean}false\""),
            "Hidden panel should have visible={{Boolean}}false. Got:\n{}",
            xml
        );
        assert!(
            xml.contains("dorExclusion=\"true\""),
            "Hidden panel should have dorExclusion=true. Got:\n{}",
            xml
        );
    }

    /// Build a conditional panel node with the given AEM name (the target of a
    /// trigger field's condition rule).
    fn conditional_panel(name: &str) -> AemNode {
        AemNode::Panel {
            uuid: fixed_uuid(),
            name: name.into(),
            title: String::new(),
            children: vec![],
            is_page: false,
            attrs: AemAttrs {
                dor_exclude: true,
                ..Default::default()
            },
            visible: false,
            is_conditional: true,
            dor_num_cols: None,
            colspan: 12,
            dor_colspan: None,
            bind_ref: None,
            frag_ref: None,
        }
    }

    #[test]
    fn radio_button_with_conditions_emits_panel_visible_script() {
        use crate::value::InputValue;

        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![
                AemNode::RadioButton {
                    attrs: AemAttrs::default(),
                    uuid: fixed_uuid(),
                    name: "RB_TriggerField".into(),
                    label: "Choose".into(),
                    options: vec![
                        AemOption {
                            label: "Yes".into(),
                            value: "yes".into(),
                        },
                        AemOption {
                            label: "No".into(),
                            value: "no".into(),
                        },
                    ],
                    alignment: OptionAlignment::Vertical,
                    mandatory: false,
                    visible: true,
                    colspan: 12,
                    dor_colspan: None,
                    conditions: vec![ConditionRule {
                        target_panel_name: "COND_TargetPanel".into(),
                        value: InputValue::Text("yes".into()),
                        show: true,
                    }],
                    bind_ref: None,
                    hide_title: false,
                    input: Default::default(),
                },
                conditional_panel("COND_TargetPanel"),
            ],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        // AABO mechanism: the SHOW_EXPRESSION lives on the target panel, not as
        // a valueCommit on the trigger field.
        assert!(
            !xml.contains("fd:valueCommit"),
            "Trigger field should NOT emit fd:valueCommit anymore. Got:\n{}",
            xml
        );
        assert!(
            xml.contains("fd:visible") && xml.contains("SHOW_EXPRESSION"),
            "Conditional panel should emit a fd:visible SHOW_EXPRESSION. Got:\n{}",
            xml
        );
        assert!(
            xml.contains("showAFShowDor") && xml.contains("hideAFHideDor"),
            "Script should call the UBS DOR helpers. Got:\n{}",
            xml
        );
        assert!(
            xml.contains("RB_TriggerField.value =="),
            "Expression should reference the trigger field by name. Got:\n{}",
            xml
        );
    }

    #[test]
    fn radio_without_conditions_has_no_scripts() {
        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![AemNode::RadioButton {
                attrs: AemAttrs::default(),
                uuid: fixed_uuid(),
                name: "RB_Simple".into(),
                label: "Choose".into(),
                options: vec![AemOption {
                    label: "A".into(),
                    value: "a".into(),
                }],
                alignment: OptionAlignment::Vertical,
                mandatory: false,
                visible: true,
                colspan: 12,
                dor_colspan: None,
                conditions: vec![],
                bind_ref: None,
                hide_title: false,
                input: Default::default(),
            }],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        let radio = element(&xml, "RB_Simple");
        assert!(
            radio.children.iter().all(|c| c.tag_name != "fd:scripts"),
            "Radio without conditions should NOT emit fd:scripts. Got:\n{}",
            xml
        );
    }

    #[test]
    fn dropdown_with_conditions_emits_scripts() {
        use crate::value::InputValue;

        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![
                AemNode::Dropdown {
                    attrs: AemAttrs::default(),
                    uuid: fixed_uuid(),
                    name: "DD_Trigger".into(),
                    label: "Select".into(),
                    options: vec![
                        AemOption {
                            label: "Option A".into(),
                            value: "a".into(),
                        },
                        AemOption {
                            label: "Option B".into(),
                            value: "b".into(),
                        },
                    ],
                    mandatory: false,
                    visible: true,
                    colspan: 12,
                    dor_colspan: None,
                    conditions: vec![ConditionRule {
                        target_panel_name: "COND_PanelA".into(),
                        value: InputValue::Text("a".into()),
                        show: true,
                    }],
                    bind_ref: None,
                    input: Default::default(),
                },
                conditional_panel("COND_PanelA"),
            ],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        assert!(
            xml.contains("fd:visible") && xml.contains("SHOW_EXPRESSION"),
            "Conditional panel for a dropdown trigger should emit fd:visible. Got:\n{}",
            xml
        );
        assert!(
            xml.contains("DD_Trigger.value =="),
            "Expression should reference DD_Trigger. Got:\n{}",
            xml
        );
    }

    #[test]
    fn checkbox_with_conditions_emits_scripts() {
        use crate::value::InputValue;

        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![
                AemNode::Checkbox {
                    attrs: AemAttrs::default(),
                    uuid: fixed_uuid(),
                    name: "CB_Trigger".into(),
                    label: String::new(),
                    options: vec![AemOption {
                        label: "Accept".into(),
                        value: "true".into(),
                    }],
                    alignment: OptionAlignment::Horizontal,
                    visible: true,
                    colspan: 12,
                    dor_colspan: None,
                    conditions: vec![ConditionRule {
                        target_panel_name: "COND_AcceptPanel".into(),
                        value: InputValue::Bool(true),
                        show: true,
                    }],
                    bind_ref: None,
                    mandatory: false,
                    input: Default::default(),
                    hide_title: false,
                },
                conditional_panel("COND_AcceptPanel"),
            ],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        assert!(
            xml.contains("fd:visible") && xml.contains("SHOW_EXPRESSION"),
            "Conditional panel for a checkbox trigger should emit fd:visible. Got:\n{}",
            xml
        );
        assert!(
            xml.contains("CB_Trigger.value == \\\\&quot;true\\\\&quot;"),
            "Expression should compare CB_Trigger to the bool value. Got:\n{}",
            xml
        );
    }

    #[test]
    fn visible_script_ors_multiple_trigger_values() {
        use crate::value::InputValue;

        // A trigger that shows the same panel for two different values
        // (AABO: `RB_FormularAdressat.value == "3" || ... == "4"`).
        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![
                AemNode::RadioButton {
                    attrs: AemAttrs::default(),
                    uuid: fixed_uuid(),
                    name: "RB_Adressat".into(),
                    label: "Choose".into(),
                    options: vec![
                        AemOption {
                            label: "Three".into(),
                            value: "3".into(),
                        },
                        AemOption {
                            label: "Four".into(),
                            value: "4".into(),
                        },
                    ],
                    alignment: OptionAlignment::Vertical,
                    mandatory: false,
                    visible: true,
                    colspan: 12,
                    dor_colspan: None,
                    conditions: vec![
                        ConditionRule {
                            target_panel_name: "COND_Entity".into(),
                            value: InputValue::Text("3".into()),
                            show: true,
                        },
                        ConditionRule {
                            target_panel_name: "COND_Entity".into(),
                            value: InputValue::Text("4".into()),
                            show: true,
                        },
                    ],
                    bind_ref: None,
                    hide_title: false,
                    input: Default::default(),
                },
                conditional_panel("COND_Entity"),
            ],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        // The rendered expression OR-s both comparisons (AABO escaping `\\&quot;`).
        assert!(
            xml.contains(
                "RB_Adressat.value == \\\\&quot;3\\\\&quot; || RB_Adressat.value == \\\\&quot;4\\\\&quot;"
            ),
            "Expression should OR both trigger values. Got:\n{}",
            xml
        );
        assert!(
            xml.contains("window.forms.ubs.showAFShowDor(this);")
                && xml.contains("window.forms.ubs.hideAFHideDor(this);"),
            "Script should call both DOR helpers. Got:\n{}",
            xml
        );
    }

    #[test]
    fn visible_json_has_show_expression_structure() {
        use crate::value::InputValue;

        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![
                AemNode::Dropdown {
                    attrs: AemAttrs::default(),
                    uuid: fixed_uuid(),
                    name: "DD_Field".into(),
                    label: "Select".into(),
                    options: vec![AemOption {
                        label: "Val".into(),
                        value: "val".into(),
                    }],
                    mandatory: false,
                    visible: true,
                    colspan: 12,
                    dor_colspan: None,
                    conditions: vec![ConditionRule {
                        target_panel_name: "PN_Cond".into(),
                        value: InputValue::Text("val".into()),
                        show: true,
                    }],
                    bind_ref: None,
                    input: Default::default(),
                },
                conditional_panel("PN_Cond"),
            ],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        // The SHOW_EXPRESSION SCRIPTMODEL envelope, as `scripts::show` builds it.
        assert!(
            xml.contains("&quot;field&quot;:&quot;PN_Cond&quot;"),
            "field should be the target panel name. Got:\n{}",
            xml
        );
        assert!(
            xml.contains("&quot;event&quot;:&quot;Visibility&quot;"),
            "event should be Visibility. Got:\n{}",
            xml
        );
        assert!(
            xml.contains("&quot;nodeName&quot;:&quot;SHOW_EXPRESSION&quot;"),
            "model nodeName should be SHOW_EXPRESSION. Got:\n{}",
            xml
        );
        assert!(
            xml.contains("SCRIPTMODEL"),
            "envelope should contain SCRIPTMODEL. Got:\n{}",
            xml
        );
    }

    #[test]
    fn dor_colspan_emitted_on_fields_in_grid_panel() {
        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![AemNode::Panel {
                uuid: uuid_of("GridPanel"),
                name: "GridPanel".into(),
                title: "Grid Panel".into(),
                is_page: false,
                attrs: AemAttrs::default(),
                visible: true,
                is_conditional: false,
                dor_num_cols: Some(3),
                colspan: 12,
                dor_colspan: None,
                bind_ref: None,
                frag_ref: None,
                children: vec![
                    AemNode::TextField {
                        attrs: AemAttrs::default(),
                        uuid: uuid_of("Street"),
                        name: "Street".into(),
                        label: "Street".into(),
                        mandatory: false,
                        visible: true,
                        max_chars: None,
                        colspan: 8,
                        dor_colspan: Some(2),
                        bind_ref: None,
                        kind: TextFieldKind::Plain,
                        input: Default::default(),
                    },
                    AemNode::TextField {
                        attrs: AemAttrs::default(),
                        uuid: uuid_of("No"),
                        name: "No".into(),
                        label: "No".into(),
                        mandatory: false,
                        visible: true,
                        max_chars: None,
                        colspan: 4,
                        dor_colspan: Some(1),
                        bind_ref: None,
                        kind: TextFieldKind::Plain,
                        input: Default::default(),
                    },
                ],
            }],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        assert!(
            xml.contains("dorNumCols=\"3\""),
            "Panel should have dorNumCols=3. Got:\n{}",
            xml
        );
        assert!(
            xml.contains("dorColspan=\"2\""),
            "Street field should have dorColspan=2. Got:\n{}",
            xml
        );
        assert!(
            xml.contains("dorColspan=\"1\""),
            "No field should have dorColspan=1. Got:\n{}",
            xml
        );
    }

    #[test]
    fn dor_colspan_not_emitted_when_none() {
        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![AemNode::TextField {
                attrs: AemAttrs::default(),
                uuid: fixed_uuid(),
                name: "PlainField".into(),
                label: "Plain".into(),
                mandatory: false,
                visible: true,
                max_chars: None,
                colspan: 12,
                dor_colspan: None,
                bind_ref: None,
                kind: TextFieldKind::Plain,
                input: Default::default(),
            }],
        };
        let xml = generate_aem_xml(&root, &test_config()).expect("the form is written");
        assert!(
            !xml.contains("dorColspan"),
            "Field without dor_colspan should not emit dorColspan. Got:\n{}",
            xml
        );
    }


    /// The preface's fragment panel points at affrg_BankingRelationship1
    /// whatever the entity.
    #[test]
    fn preface_renders_entity_based_banking_relationship_fragment() {
        let expected_frag_ref =
            "/content/forms/af/afforms_ubs_fragmentlib/affrg_BankingRelationship1";

        for entity in &["033", "019", "001"] {
            let mut config = test_config();
            config
                .xfa_vars
                .insert("formrange_entity".into(), entity.to_string());
            config.user_vars.insert(
                "default_layout".into(),
                "fd/af/layouts/gridFluidLayout2".into(),
            );
            config
                .user_vars
                .insert("dor_field_styling".into(), "Default".into());
            config.user_vars.insert(
                "custom_resource_type_base".into(),
                "ajila-forms-customers/ajila-forms-ubs/components".into(),
            );

            let node = AemNode::Preface {
                uuid: fixed_uuid(),
                name: "PN_Preface_abcdef01".into(),
            };

            let xml = render_one(&node, &config);

            assert!(
                xml.contains(&format!("fragRef=\"{}\"", expected_frag_ref)),
                "entity {}: expected fragRef='{}' in:\n{}",
                entity,
                expected_frag_ref,
                xml
            );
            assert!(
                xml.contains("name=\"PN_BankingRelationship\""),
                "entity {}: expected name='PN_BankingRelationship' in:\n{}",
                entity,
                xml
            );
        }
    }

    /// The banking-relationship preface fragment must be wrapped in a `PN_BR`
    /// panel that is excluded from both the Document of Record and the Summary.
    /// This mirrors the AAJC reference form; the fragment panel inside carries
    /// its own `dorExclusion` as well (see
    /// [`preface_fragment_panel_is_dor_excluded`]).
    #[test]
    fn preface_wraps_banking_relationship_in_excluded_panel() {
        let mut config = test_config();
        config.user_vars.insert(
            "default_layout".into(),
            "fd/af/layouts/gridFluidLayout2".into(),
        );
        config.user_vars.insert(
            "custom_resource_type_base".into(),
            "ajila-forms-customers/ajila-forms-ubs/components".into(),
        );

        let node = AemNode::Preface {
            uuid: fixed_uuid(),
            name: "PN_Preface_abcdef01".into(),
        };
        let xml = render_one(&node, &config);

        // The wrapper panel exists and carries both exclusions.
        assert!(
            xml.contains("name=\"PN_BR\""),
            "expected a wrapping PN_BR panel. Got:\n{}",
            xml
        );
        assert!(
            xml.contains("dorExclusion=\"true\""),
            "PN_BR wrapper must be DOR-excluded. Got:\n{}",
            xml
        );
        assert!(
            xml.contains("summaryExclusion=\"true\""),
            "PN_BR wrapper must be summary-excluded. Got:\n{}",
            xml
        );
        // The inner fragment panel is nested inside the wrapper.
        let pn_br = xml.find("name=\"PN_BR\"").unwrap();
        let pn_banking = xml.find("name=\"PN_BankingRelationship\"").unwrap();
        assert!(
            pn_br < pn_banking,
            "PN_BankingRelationship must be nested inside PN_BR. Got:\n{}",
            xml
        );
    }

    /// The panel that *bears* the banking fragment must itself carry
    /// `dorExclusion="true"`, not only the `PN_BR` wrapper around it.
    ///
    /// The wrapper alone is what the engine used to emit, and it is not what the
    /// deployed corpus has: every form there carries the exclusion on the
    /// fragment panel too, and the feedback guard reads that node and no other
    /// (PROBLEM-banking-relationship-fragment, `find_panel_noncanonical.py
    /// --require-attr dorExclusion=true`). The assertion is deliberately scoped
    /// to the one tag: an exclusion anywhere else in the XML does not satisfy
    /// the rule, which is exactly how the gap went unnoticed.
    #[test]
    fn preface_fragment_panel_is_dor_excluded() {
        let mut config = test_config();
        config.user_vars.insert(
            "default_layout".into(),
            "fd/af/layouts/gridFluidLayout2".into(),
        );
        config.user_vars.insert(
            "custom_resource_type_base".into(),
            "ajila-forms-customers/ajila-forms-ubs/components".into(),
        );

        let node = AemNode::Preface {
            uuid: fixed_uuid(),
            name: "PN_Preface_abcdef01".into(),
        };
        let xml = render_one(&node, &config);

        let frag_at = xml
            .find("affrg_BankingRelationship1")
            .expect("the preface must emit the banking fragment");
        let tag_start = xml[..frag_at].rfind('<').expect("fragRef inside a tag");
        let tag_end = tag_start
            + xml[tag_start..]
                .find('>')
                .expect("the fragment panel tag must close");
        let tag = &xml[tag_start..tag_end];
        assert!(
            tag.contains("dorExclusion=\"true\""),
            "the fragment panel tag must carry dorExclusion. Got:\n{}",
            tag
        );
    }

    /// The `PN_BR` wrapper must carry `css="ubs-margin-20"` so the banking
    /// block gets the standard vertical spacing (feedback registry
    /// PROBLEM-banking-relationship-margin, UBS directive 2026-07-27).
    #[test]
    fn preface_wrapper_carries_ubs_margin_class() {
        let mut config = test_config();
        config.user_vars.insert(
            "default_layout".into(),
            "fd/af/layouts/gridFluidLayout2".into(),
        );
        config.user_vars.insert(
            "custom_resource_type_base".into(),
            "ajila-forms-customers/ajila-forms-ubs/components".into(),
        );

        let node = AemNode::Preface {
            uuid: fixed_uuid(),
            name: "PN_Preface_abcdef01".into(),
        };
        let xml = render_one(&node, &config);

        assert_eq!(
            element(&xml, "PN_BR").attr("css"),
            Some("ubs-margin-20"),
            "the margin class must sit on the PN_BR wrapper. Got:\n{xml}"
        );
    }

    /// Date pickers must not pre-fill today's date: a pre-filled current date
    /// silently becomes wrong data on any form not submitted the same day
    /// (feedback registry PROBLEM-datepicker-current-date-default). The
    /// unchecked state is the attribute being ABSENT, not `false`.
    #[test]
    fn datepicker_does_not_default_to_current_date() {
        let mut config = test_config();
        config.user_vars.insert(
            "custom_resource_type_base".into(),
            "ajila-forms-customers/ajila-forms-ubs/components".into(),
        );
        config
            .user_vars
            .insert("css_datepicker".into(), "widget_datepicker".into());
        config
            .user_vars
            .insert("dor_field_styling".into(), "Default".into());

        let node = AemNode::DatePicker {
            attrs: AemAttrs::default(),
            uuid: fixed_uuid(),
            name: "DATE_1".into(),
            label: "Date".into(),
            mandatory: false,
            visible: true,
            colspan: 12,
            dor_colspan: None,
            bind_ref: None,
            input: Default::default(),
        };
        let xml = render_one(&node, &config);

        assert!(
            xml.contains("guideNodeClass=\"guideDatePicker\""),
            "expected the date picker component. Got:\n{}",
            xml
        );
        assert!(
            !xml.contains("defaultToCurrentDate"),
            "date pickers must not carry defaultToCurrentDate. Got:\n{}",
            xml
        );
    }

    /// For a `PN_FormConfigurator` page panel, the full `dorExclusion` must sit
    /// on the generated `…Title` sub-panel, NOT on the parent panel (the parent
    /// only carries `dorExcludeTitle`/`dorExcludeDescription`). This mirrors the
    /// reference packages; an earlier writer wrongly excluded the whole
    /// configurator subtree from the DOR.
    #[test]
    fn form_configurator_excludes_title_subpanel_not_parent_from_dor() {
        let mut config = test_config();
        config.user_vars.insert(
            "custom_resource_type_base".into(),
            "ajila-forms-customers/ajila-forms-ubs/components".into(),
        );
        config.user_vars.insert(
            "default_layout".into(),
            "fd/af/layouts/gridFluidLayout2".into(),
        );
        config
            .user_vars
            .insert("dor_field_styling".into(), "default".into());

        let node = AemNode::Panel {
            uuid: fixed_uuid(),
            name: "PN_FormConfigurator_abcdef01".into(),
            title: "Form configurator".into(),
            // The configurator is the first page, which the banking preface
            // marks.
            children: vec![AemNode::Preface {
                uuid: uuid::Uuid::from_u128(2),
                name: "PN_Preface".into(),
            }],
            is_page: true,
            attrs: AemAttrs::default(),
            visible: true,
            is_conditional: false,
            dor_num_cols: None,
            colspan: 12,
            dor_colspan: None,
            bind_ref: None,
            frag_ref: None,
        };
        let xml = render_one(&node, &config);

        // The FormConfigurator panel itself is NOT excluded from the DoR; it
        // keeps only its title excluded.
        let parent = element(&xml, "PN_FormConfigurator_abcdef01");
        assert_eq!(parent.attr("dorExclusion"), None, "the configurator must reach the DoR");
        assert_eq!(parent.attr("dorExcludeTitle"), Some("true"));
        // The generated title sub-panel carries the full exclusion.
        let title = element(&xml, "PN_FormConfigurator_abcdef01Title");
        assert_eq!(title.attr("dorExclusion"), Some("true"));
    }

    /// The two panels the engine names for a repeatable carry a repeat-container
    /// prefix whatever the repeatable itself is called.
    ///
    /// They used to inherit the authored name verbatim, so a repeatable named
    /// `RP_Individual` (a legacy prefix, and what a run does pick) produced
    /// `RP_Individual_repeat` and `RP_Individual_inner` — three violations of
    /// PROBLEM-naming-conventions where the author made one. The authored name is
    /// left alone: other scripts address the repeatable by it, and it is the
    /// author's to correct.
    #[test]
    fn engine_named_repeat_panels_carry_a_repeat_prefix() {
        // Prefix swapped, stem kept.
        assert_eq!(with_repeat_prefix("RP_Individual"), "RCP_Individual");
        assert_eq!(with_repeat_prefix("PN_AHRP"), "RCP_AHRP");
        // Already a repeat-container prefix: untouched, never doubled.
        assert_eq!(with_repeat_prefix("RCP_Clients"), "RCP_Clients");
        assert_eq!(with_repeat_prefix("RCHT_Rows"), "RCHT_Rows");
        // No prefix at all: given one.
        assert_eq!(with_repeat_prefix("Clients"), "RCP_Clients");
        // A capitalised word is not a prefix, so the whole name is the stem.
        assert_eq!(with_repeat_prefix("Portfolio_ID"), "RCP_Portfolio_ID");

        let mut config = test_config();
        config.user_vars.insert(
            "default_layout".into(),
            "fd/af/layouts/gridFluidLayout2".into(),
        );
        config.user_vars.insert(
            "custom_resource_type_base".into(),
            "ubs/af/components".into(),
        );
        config
            .user_vars
            .insert("dor_field_styling".into(), "some_styling".into());

        let node = AemNode::Repeatable {
            attrs: AemAttrs::default(),
            visible: true,
            uuid: fixed_uuid(),
            name: "RP_Individual".into(),
            title: String::new(),
            children: vec![],
            min_occur: 1,
            max_occur: 5,
            bind_ref: None,
            frag_ref: None,
            twin_of: None,
        };
        let xml = render_one(&node, &config);

        for name in ["RCP_Individual_repeat", "RCP_Individual_inner"] {
            assert!(
                xml.contains(&format!("name=\"{name}\"")),
                "expected name={name} in:\n{}",
                xml
            );
        }
        // The scripts must address the panel by the name it was actually given.
        assert!(
            xml.contains("RCP_Individual_repeat.instanceManager"),
            "the scripts must follow the panel's name:\n{}",
            xml
        );
        assert!(
            !xml.contains("RP_Individual_repeat"),
            "no panel may keep the legacy prefix:\n{}",
            xml
        );
    }

    /// An Add button has to name what it adds. A repeatable carries no title of
    /// its own in an engine-authored tree, so the subject is the heading above it,
    /// or failing that the enclosing panel's title.
    ///
    /// A bare "Add" tells the reader nothing about which of a form's several
    /// repeat blocks they are adding a row to, and the feedback guard reports it
    /// (PROBLEM-repeatable-add-label).
    #[test]
    fn the_add_button_names_what_it_adds() {
        let label_for = |children: Vec<AemNode>, panel_title: &str| {
            let mut config = test_config();
            config.user_vars.insert(
                "default_layout".into(),
                "fd/af/layouts/gridFluidLayout2".into(),
            );
            config.user_vars.insert(
                "custom_resource_type_base".into(),
                "ubs/af/components".into(),
            );
            config
                .user_vars
                .insert("dor_field_styling".into(), "some_styling".into());
            config
                .add_label_patterns
                .insert("en".into(), "Add {subject}".into());

            let root = AemNode::Root {
                title: "Form".into(),
                children: vec![AemNode::Panel {
                    uuid: fixed_uuid(),
                    name: "PN_Outer".into(),
                    title: panel_title.into(),
                    children,
                    is_page: false,
                    attrs: AemAttrs::default(),
                    visible: true,
                    is_conditional: false,
                    dor_num_cols: None,
                    colspan: 12,
                    dor_colspan: None,
                    bind_ref: None,
                    frag_ref: None,
                }],
            };
            let xml = generate_aem_xml(&root, &config).expect("the form is written");
            xml.split("jcr:title=\"")
                .find(|part| part.starts_with("Add"))
                .and_then(|part| part.split('"').next())
                .unwrap_or_default()
                .to_string()
        };

        let repeatable = |title: &str| AemNode::Repeatable {
            attrs: AemAttrs::default(),
            visible: true,
            uuid: fixed_uuid(),
            name: "RCP_1".into(),
            title: title.into(),
            children: vec![],
            min_occur: 1,
            max_occur: 5,
            bind_ref: None,
            frag_ref: None,
            twin_of: None,
        };
        let heading = |content: &str| AemNode::TitleDraw {
            attrs: AemAttrs::default(),
            visible: true,
            uuid: fixed_uuid(),
            name: "TTL_1".into(),
            content: content.into(),
            heading_level: 2,
            colspan: 12,
            dor_colspan: None,
        };

        // Its own title wins.
        assert_eq!(
            label_for(vec![repeatable("Beneficial owner")], "Client details"),
            "Add Beneficial owner"
        );
        // Titleless: the heading above it, decoration stripped.
        assert_eq!(
            label_for(
                vec![heading("2. Authorized representative:"), repeatable("")],
                "Client details"
            ),
            "Add Authorized representative"
        );
        // Nothing above it: the panel it sits in.
        assert_eq!(
            label_for(vec![repeatable("")], "Client details"),
            "Add Client details"
        );
        // Prose names nothing, so the button falls back to the SAME placeholder
        // the panel's own jcr:title/ajilaPanelSubject already carries -- a bare,
        // unlocalised "Add" is its own defect (PROBLEM-repeatable-add-label
        // reports it), and PROBLEM-repeatable-add-label's own detector reads
        // ajilaPanelSubject back out as the panel's stated subject regardless,
        // so leaving the button off the placeholder just means the two disagree.
        assert_eq!(
            label_for(
                vec![
                    heading("The client confirms that the details given above are correct."),
                    repeatable("")
                ],
                ""
            ),
            "Add (Repeatable name)"
        );
    }

    /// Every panel inside a repeatable must keep the repeatable's own name as its
    /// stem, so the `RCP_` prefix the naming convention requires survives.
    ///
    /// The innermost row panel used to be named from the repeatable's *title*.
    /// Engine-authored repeatables carry no title, so the name came out as the
    /// bare suffix `_inner` — no prefix at all — and a title that did exist
    /// dragged its spaces into a component name (`Portfolio ID_inner` in the
    /// corpus). Both are violations of PROBLEM-naming-conventions, and the title
    /// was never the right source: a name is not display text.
    #[test]
    fn repeatable_inner_panels_keep_the_repeatable_prefix() {
        let mut config = test_config();
        config.user_vars.insert(
            "default_layout".into(),
            "fd/af/layouts/gridFluidLayout2".into(),
        );
        config.user_vars.insert(
            "custom_resource_type_base".into(),
            "ubs/af/components".into(),
        );
        config
            .user_vars
            .insert("dor_field_styling".into(), "some_styling".into());

        // Titleless, as an engine-authored repeatable is.
        let node = AemNode::Repeatable {
            attrs: AemAttrs::default(),
            visible: true,
            uuid: fixed_uuid(),
            name: "RCP_Clients".into(),
            title: String::new(),
            children: vec![],
            min_occur: 1,
            max_occur: 5,
            bind_ref: None,
            frag_ref: None,
            twin_of: None,
        };
        let xml = render_one(&node, &config);

        assert!(
            xml.contains("name=\"RCP_Clients_inner\""),
            "the row panel must be named after the repeatable. Got:\n{}",
            xml
        );
        assert!(
            !xml.contains("name=\"_inner\""),
            "a titleless repeatable must not produce a prefixless name. Got:\n{}",
            xml
        );
        // Every name the repeatable emits for itself stays under one prefix.
        for name in ["RCP_Clients", "RCP_Clients_repeat", "RCP_Clients_inner"] {
            assert!(
                xml.contains(&format!("name=\"{name}\"")),
                "expected name={name} in:\n{}",
                xml
            );
        }
    }

    /// A tree written through the UBS repeatable lowering.
    fn render_tree(children: Vec<AemNode>) -> String {
        let mut config = test_config();
        config.user_vars.insert(
            "default_layout".into(),
            "fd/af/layouts/gridFluidLayout2".into(),
        );
        config
            .user_vars
            .insert("resource_type_base".into(), "fd/af/components".into());
        config.user_vars.insert(
            "custom_resource_type_base".into(),
            "ubs/af/components".into(),
        );
        config
            .user_vars
            .insert("dor_field_styling".into(), "some_styling".into());

        let root = AemNode::Root {
            title: "Form".into(),
            children,
        };
        generate_aem_xml(&root, &config).expect("the form is written")
    }

    /// One repeatable, rendered the way an engine-authored tree holds it: inside
    /// a panel, because that is the tree the subject is resolved from.
    fn render_repeatable(name: &str, title: &str, min_occur: u32, max_occur: u32) -> String {
        render_tree(vec![AemNode::Panel {
            uuid: fixed_uuid(),
            name: "PN_Outer".into(),
            title: String::new(),
            children: vec![AemNode::Repeatable {
                attrs: AemAttrs::default(),
                visible: true,
                uuid: fixed_uuid(),
                name: name.into(),
                title: title.into(),
                children: vec![],
                min_occur,
                max_occur,
                bind_ref: None,
                frag_ref: None,
                twin_of: None,
            }],
            is_page: false,
            attrs: AemAttrs::default(),
            visible: true,
            is_conditional: false,
            dor_num_cols: None,
            colspan: 12,
            dor_colspan: None,
            bind_ref: None,
            frag_ref: None,
        }])
    }

    /// Nothing in a click body may put the Add button back on screen.
    ///
    /// Its visibility is an expression over the instance count, so it returns by
    /// itself. The imperative version the engine used to emit ran *after* the
    /// instance holding it was destroyed and named the button by a name that
    /// repeats across a form — the line the archetype singles out as the one
    /// that fails in the corpus (PROBLEM-repeating-panel §5).
    #[test]
    fn no_click_body_re_shows_the_add_button() {
        let xml = render_repeatable("RCP_Test", "Client", 1, 5);

        assert!(
            !xml.contains("BT_Add.visible = true"),
            "the Add button's return must stay reactive. Got:\n{}",
            xml
        );
        // The instance manager is reached only through the library helpers.
        assert!(
            !xml.contains("instanceManager.addInstance()")
                && !xml.contains("instanceManager.removeInstance("),
            "the buttons must call window.forms.ubs, never instanceManager. Got:\n{}",
            xml
        );
    }

    /// The properties the UBS client library reads off a repeating panel.
    ///
    /// `dorFieldStyling="Repeating Panel Numbering"` is what makes it number the
    /// rows, and `headingLevel` is what makes AEM render the title server-side —
    /// without it the first row shows no heading until it has been re-rendered.
    /// `dorExcludeTitle` has to be absent, because that title *is* the row
    /// heading the numbering is built on (PROBLEM-repeating-panel §3).
    #[test]
    fn the_repeating_panel_carries_the_archetype_properties() {
        let xml = render_repeatable("RCP_Test", "Client", 1, 5);
        let panel = xml
            .split("<repeatableInner")
            .nth(1)
            .and_then(|rest| rest.split('>').next())
            .expect("the writer must emit the repeating panel");

        for attr in [
            "jcr:title=\"Client\"",
            "accessibilityLabel=\"Client\"",
            "ajilaPanelSubject=\"Client\"",
            "addButton=\"BT_Add\"",
            "removeButton=\"BT_Remove\"",
            "dorFieldStyling=\"Repeating Panel Numbering\"",
            "headingLevel=\"3\"",
            "summaryHeadingLevel=\"4\"",
        ] {
            assert!(
                panel.contains(attr),
                "expected {attr} on the repeating panel. Got:\n{}",
                panel
            );
        }
        assert!(
            !panel.contains("dorExcludeTitle"),
            "the row heading must not be excluded from the DoR. Got:\n{}",
            panel
        );
    }

    /// The deterministic converter's own sentinel for "nothing titles this
    /// repeatable" is a `title` equal to the node's own generated `name`
    /// (`convert_repeatable` always sets `title: name`) -- never a heading's or
    /// a panel's, so treating it as real text wrote the auto-generated name
    /// itself onto screen (`jcr:title="RCP_e42e142a"`, `Add RCP_e42e142a`, found
    /// on a real conversion, AAOS). It must fall through to the heading above
    /// the repeatable, or the enclosing panel's title, exactly as a blank title
    /// does.
    #[test]
    fn a_repeatable_titled_with_its_own_name_falls_through_like_a_blank_one() {
        let sentinel = |name: &str, children: Vec<AemNode>, panel_title: &str| AemNode::Panel {
            uuid: fixed_uuid(),
            name: "PN_Outer".into(),
            title: panel_title.into(),
            children: {
                let mut all = children;
                all.push(AemNode::Repeatable {
                    attrs: AemAttrs::default(),
                    visible: true,
                    uuid: fixed_uuid(),
                    name: name.into(),
                    title: name.into(), // the converter's sentinel
                    children: vec![],
                    min_occur: 1,
                    max_occur: 5,
                    bind_ref: None,
                    frag_ref: None,
                    twin_of: None,
                });
                all
            },
            is_page: false,
            attrs: AemAttrs::default(),
            visible: true,
            is_conditional: false,
            dor_num_cols: None,
            colspan: 12,
            dor_colspan: None,
            bind_ref: None,
            frag_ref: None,
        };

        // Nothing above it but the enclosing panel: falls through to that.
        let xml = render_tree(vec![sentinel("RCP_e42e142a", vec![], "Client details")]);
        assert!(
            xml.contains("jcr:title=\"Client details\""),
            "expected the panel's own title, not the sentinel name. Got:\n{}",
            xml
        );
        for leaked in [
            "jcr:title=\"RCP_e42e142a\"",
            "accessibilityLabel=\"RCP_e42e142a\"",
            "ajilaPanelSubject=\"RCP_e42e142a\"",
            "jcr:title=\"Add RCP_e42e142a\"",
        ] {
            assert!(
                !xml.contains(leaked),
                "the generated name must never reach a title or the Add label. Got:\n{}",
                xml
            );
        }

        // Nothing above it and no panel title either: the placeholder, not the
        // sentinel name.
        let xml = render_tree(vec![sentinel("RCP_9f8e7d6c", vec![], "")]);
        assert!(
            xml.contains("jcr:title=\"(Repeatable name)\""),
            "expected the placeholder, not the sentinel name. Got:\n{}",
            xml
        );
    }

    /// A panel nothing names still gets a title, so a person can see there is one
    /// to write. Square brackets are not an option: a vault value opening with
    /// `[` comes back out of the import as a multi-value, and the form editor
    /// then refuses to open the form (PROBLEM-repeating-panel §4).
    #[test]
    fn a_subjectless_repeating_panel_carries_a_placeholder_title() {
        let xml = render_repeatable("RCP_Test", "", 1, 5);

        assert!(
            xml.contains("jcr:title=\"(Repeatable name)\""),
            "expected the placeholder title. Got:\n{}",
            xml
        );
        assert!(
            !xml.contains("jcr:title=\"[Repeatable name]\""),
            "a bracketed title is read back as a multi-value. Got:\n{}",
            xml
        );
    }

    /// A comma in the subject must not end the rule value.
    ///
    /// The label reaches the browser through four layers of escaping, and the
    /// comma is the one that silently splits one rule document into two.
    #[test]
    fn a_comma_in_the_subject_stays_inside_the_rule_value() {
        let xml = render_repeatable("RCP_Test", "Owner, natural person", 1, 5);

        assert!(
            xml.contains(r#"\\&quot;Owner\, natural person\\&quot;"#),
            "the comma must be escaped inside the rule value. Got:\n{}",
            xml
        );
    }

    /// On a page with repeatables, the jump-to-field button belongs to the rows.
    ///
    /// A repeatable renders one button per instance, which is what a person
    /// coming back from the summary wants; the step-title panel then gives its
    /// own up rather than rendering a second one above the heading (owner
    /// directive 2026-08-24, PROBLEM-jump-to-field-button).
    #[test]
    fn a_page_with_repeatables_gives_them_the_jump_to_field_button() {
        let mut config = test_config();
        config.user_vars.insert(
            "default_layout".into(),
            "fd/af/layouts/gridFluidLayout2".into(),
        );
        config.user_vars.insert(
            "custom_resource_type_base".into(),
            "ubs/af/components".into(),
        );
        config
            .user_vars
            .insert("dor_field_styling".into(), "some_styling".into());

        let field = || AemNode::TextField {
            attrs: AemAttrs::default(),
            uuid: fixed_uuid(),
            name: "TXT_Name".into(),
            label: "Name".into(),
            mandatory: false,
            visible: true,
            max_chars: None,
            colspan: 12,
            dor_colspan: None,
            bind_ref: None,
            kind: TextFieldKind::Plain,
            input: Default::default(),
        };
        let page = |name: &str, children: Vec<AemNode>| AemNode::Panel {
            uuid: fixed_uuid(),
            name: name.into(),
            title: "A step".into(),
            children,
            is_page: true,
            attrs: AemAttrs::default(),
            visible: true,
            is_conditional: false,
            dor_num_cols: None,
            colspan: 12,
            dor_colspan: None,
            bind_ref: None,
            frag_ref: None,
        };
        let repeatable = AemNode::Repeatable {
            attrs: AemAttrs::default(),
            visible: true,
            uuid: fixed_uuid(),
            name: "RCP_Clients".into(),
            title: "Client".into(),
            children: vec![field()],
            min_occur: 1,
            max_occur: 5,
            bind_ref: None,
            frag_ref: None,
            twin_of: None,
        };

        // The page holds a repeatable: the row carries the button, the title
        // panel does not.
        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![page("PN_Parties", vec![repeatable.clone()])],
        };
        let xml = generate_aem_xml(&root, &config).expect("the form is written");
        let row = xml
            .split("<repeatableInner")
            .nth(1)
            .and_then(|rest| rest.split('>').next())
            .expect("the writer must emit the repeating panel");
        assert!(
            row.contains("jumpToFieldButtonVisible=\"true\""),
            "the row must carry the button. Got:\n{}",
            row
        );
        assert_eq!(
            xml.matches("jumpToFieldButtonVisible=\"true\"").count(),
            1,
            "only the row may carry it, not the title panel too. Got:\n{}",
            xml
        );

        // No repeatable on the page: the title panel keeps it, as before.
        let plain = AemNode::Root {
            title: "Form".into(),
            children: vec![page("PN_Details", vec![field()])],
        };
        let xml = generate_aem_xml(&plain, &config).expect("the form is written");
        assert!(
            xml.contains("name=\"PN_DetailsTitle\"")
                && xml.contains("jumpToFieldButtonVisible=\"true\""),
            "a page without repeatables keeps the button on its title panel. Got:\n{}",
            xml
        );

        // Nothing to fill in on the page: no button anywhere, the repeatable's
        // rows included.
        let text_only = AemNode::Root {
            title: "Form".into(),
            children: vec![page(
                "PN_Terms",
                vec![AemNode::Repeatable {
                    attrs: AemAttrs::default(),
                    visible: true,
                    uuid: fixed_uuid(),
                    name: "RCP_Terms".into(),
                    title: "Term".into(),
                    children: vec![AemNode::TextDraw {
                        uuid: fixed_uuid(),
                        name: "ST_Term".into(),
                        content: "Legal provisions".into(),
                        attrs: AemAttrs::default(),
                        visible: true,
                        colspan: 12,
                        dor_colspan: None,
                    }],
                    min_occur: 1,
                    max_occur: 5,
                    bind_ref: None,
                    frag_ref: None,
                    twin_of: None,
                }],
            )],
        };
        let xml = generate_aem_xml(&text_only, &config).expect("the form is written");
        assert!(
            !xml.contains("jumpToFieldButtonVisible"),
            "a step with nothing to fill in offers no button. Got:\n{}",
            xml
        );
    }

    /// A repeatable sharing the form-configurator's own page still gets the
    /// jump-to-field button.
    ///
    /// AARG_033's first page sits entirely inside `PN_FormConfigurator_a2439641`
    /// — the configurator radio *and* the account-holder rows together. Gating
    /// repeatable collection on `is_configurator` at the page level suppressed
    /// the button on exactly the repeatables — `PN_CPGRP`, `PN_AHGRP` — a person
    /// would most want to reach (owner directive 2026-08-24, restated after that
    /// report). Only the configurator's own title panel is exempt.
    #[test]
    fn a_repeatable_on_the_configurator_page_still_gets_the_jump_to_field_button() {
        let mut config = test_config();
        config.user_vars.insert(
            "default_layout".into(),
            "fd/af/layouts/gridFluidLayout2".into(),
        );
        config.user_vars.insert(
            "custom_resource_type_base".into(),
            "ubs/af/components".into(),
        );
        config
            .user_vars
            .insert("dor_field_styling".into(), "some_styling".into());

        let repeatable = AemNode::Repeatable {
            attrs: AemAttrs::default(),
            visible: true,
            uuid: fixed_uuid(),
            name: "PN_CPGRP".into(),
            title: "Client".into(),
            children: vec![AemNode::TextField {
                attrs: AemAttrs::default(),
                uuid: fixed_uuid(),
                name: "TXT_Name".into(),
                label: "Name".into(),
                mandatory: false,
                visible: true,
                max_chars: None,
                colspan: 12,
                dor_colspan: None,
                bind_ref: None,
                kind: TextFieldKind::Plain,
                input: Default::default(),
            }],
            min_occur: 1,
            max_occur: 5,
            bind_ref: None,
            frag_ref: None,
            twin_of: None,
        };
        let root = AemNode::Root {
            title: "Form".into(),
            children: vec![AemNode::Panel {
                uuid: fixed_uuid(),
                name: "PN_FormConfigurator_a2439641".into(),
                title: "Form configurator".into(),
                children: vec![
                    AemNode::Preface {
                        uuid: fixed_uuid(),
                        name: "banking_relationship".into(),
                    },
                    repeatable,
                ],
                is_page: true,
                attrs: AemAttrs::default(),
                visible: true,
                is_conditional: false,
                dor_num_cols: None,
                colspan: 12,
                dor_colspan: None,
                bind_ref: None,
                frag_ref: None,
            }],
        };
        let xml = generate_aem_xml(&root, &config).expect("the form is written");
        let row = xml
            .split("<repeatableInner")
            .nth(1)
            .and_then(|rest| rest.split('>').next())
            .expect("the writer must emit the repeating panel");
        assert!(
            row.contains("jumpToFieldButtonVisible=\"true\""),
            "a repeatable on the configurator's own page must still get the button. Got:\n{}",
            row
        );
    }

    /// A party's signature block repeats in step with the party, driven by the
    /// party's own buttons: it has no Add and no Remove of its own, because one
    /// there would let the two desync (PROBLEM-repeating-panel §8).
    ///
    /// The pairing is the twin's `twin_of`, here naming the party's fragment.
    /// The twin is named globally, since it lives on another step, and it is
    /// the panel that repeats that has to be named: the rows are instances of
    /// the inner panel, not of the node the tree names.
    #[test]
    fn a_party_drives_its_signature_twin() {
        let signature_step = |twin: AemNode| AemNode::Panel {
            uuid: fixed_uuid(),
            name: "PN_Signatures".into(),
            title: "Signatures".into(),
            children: vec![twin],
            is_page: true,
            attrs: AemAttrs::default(),
            visible: true,
            is_conditional: false,
            dor_num_cols: None,
            colspan: 12,
            dor_colspan: None,
            bind_ref: None,
            frag_ref: None,
        };
        let fragment = |name: &str, frag_ref: &str| AemNode::Fragment {
            uuid: fixed_uuid(),
            name: name.into(),
            title: String::new(),
            frag_ref: frag_ref.into(),
            bind_ref: None,
            init: FragmentInit::default(),
            attrs: AemAttrs::default(),
            visible: true,
        };
        let party = |name: &str, child: AemNode, twin_of: Option<&str>| AemNode::Repeatable {
            attrs: AemAttrs::default(),
            visible: true,
            uuid: fixed_uuid(),
            name: name.into(),
            title: "Contractual partner".into(),
            children: vec![child],
            min_occur: 1,
            max_occur: 5,
            bind_ref: None,
            frag_ref: None,
            twin_of: twin_of.map(|t| t.to_owned().try_into().unwrap()),
        };

        // The party is named after the fragment it repeats, and its twin is a
        // repeatable of its own on the signature step.
        let xml = render_tree(vec![
            party(
                "RCP_1",
                fragment("PN_CPGRP", "affrg_ContractualPartnerGeneric1"),
                None,
            ),
            signature_step(party(
                "PN_SGN_CPGRP",
                fragment("PN_GenericSignature", "affrg_SignatureGeneric1"),
                Some("PN_CPGRP"),
            )),
        ]);
        assert!(
            xml.contains("window.forms.ubs.addInstance(RCP_SGN_CPGRP_repeat);"),
            "the Add button must add a row to the twin. Got:\n{}",
            xml
        );
        assert!(
            xml.contains("window.forms.ubs.removeInstance(RCP_SGN_CPGRP_repeat);"),
            "the Remove button must drop the twin's row too. Got:\n{}",
            xml
        );
        // Relabelled with both buttons empty: it has none of its own.
        assert!(
            xml.contains(concat!(
                "setRepeatPanelAccessibilityLabelsForButtons(RCP_SGN_CPGRP_repeat",
                r#"\, \\&quot;Contractual partner\\&quot;\, \\&quot;\\&quot;\, \\&quot;\\&quot;);"#
            )),
            "the twin must be relabelled with no buttons of its own. Got:\n{}",
            xml
        );

        // No twin in the form: no call naming one. `addInstance` on a panel that
        // is not there throws and takes the rest of the click with it.
        let alone = render_tree(vec![party(
            "RCP_1",
            fragment("PN_CPGRP", "affrg_ContractualPartnerGeneric1"),
            None,
        )]);
        assert!(
            !alone.contains("PN_SGN_CPGRP") && !alone.contains("RCP_SGN_CPGRP"),
            "a form without the twin must not name it. Got:\n{}",
            alone
        );
    }

    /// A twin that wraps the signature fragment (`RCP_SGN_CPGRP` around
    /// `PN_SGN_CPGRP`) names its party by the party's fragment (`PN_CPGRP`),
    /// which leads to the party's repeatable; the Add drives the twin's
    /// repeating panel, never its fragment.
    #[test]
    fn a_signature_fragment_names_the_repeatable_that_wraps_it() {
        let fragment = |name: &str, frag_ref: &str| AemNode::Fragment {
            uuid: uuid_of(name),
            name: name.into(),
            title: String::new(),
            frag_ref: frag_ref.into(),
            bind_ref: None,
            init: FragmentInit::default(),
            attrs: AemAttrs::default(),
            visible: true,
        };
        let repeatable = |name: &str, child: AemNode, twin_of: Option<&str>| AemNode::Repeatable {
            attrs: AemAttrs::default(),
            visible: true,
            uuid: uuid_of(name),
            name: name.into(),
            title: "Account holder".into(),
            children: vec![child],
            min_occur: 1,
            max_occur: 4,
            bind_ref: None,
            frag_ref: None,
            twin_of: twin_of.map(|t| t.to_owned().try_into().unwrap()),
        };
        let xml = render_tree(vec![
            repeatable(
                "RCP_CPGRP",
                fragment("PN_CPGRP", "affrg_ContractualPartnerGeneric1"),
                None,
            ),
            repeatable(
                "RCP_SGN_CPGRP",
                fragment("PN_SGN_CPGRP", "affrg_SignatureGeneric1"),
                Some("PN_CPGRP"),
            ),
        ]);
        assert!(
            xml.contains("window.forms.ubs.addInstance(RCP_SGN_CPGRP_repeat);")
                && !xml.contains("addInstance(PN_SGN_CPGRP)"),
            "the Add button must add a row to the repeating panel. Got:\n{xml}"
        );
        assert_eq!(
            xml.matches("name=\"BT_Add\"").count(),
            1,
            "the twin must have no buttons of its own. Got:\n{xml}"
        );
    }

    /// A twin of a hand-built party block names the party's repeatable, and
    /// gives up its own Add and Remove, which is what would let the two panels
    /// desync. Whatever either is called: `RCP_LR` with `RCP_Sign_LR`, and two
    /// names that share nothing.
    #[test]
    fn a_twin_names_its_party_and_gives_up_its_buttons() {
        let party = |name: &str, title: &str, twin_of: Option<&str>| AemNode::Repeatable {
            attrs: AemAttrs::default(),
            visible: true,
            uuid: uuid_of(name),
            name: name.into(),
            title: title.into(),
            children: vec![],
            min_occur: 1,
            max_occur: 4,
            bind_ref: None,
            frag_ref: None,
            twin_of: twin_of.map(|t| t.to_owned().try_into().unwrap()),
        };

        for (data, twin, twin_repeat) in [
            ("RCP_LR", "RCP_Sign_LR", "RCP_Sign_LR_repeat"),
            ("RCP_Holders", "RCP_Signatures", "RCP_Signatures_repeat"),
        ] {
            let xml = render_tree(vec![
                party(data, "Legal representative", None),
                party(twin, "Signature", Some(data)),
            ]);

            // The data panel drives both.
            assert!(
                xml.contains(&format!("window.forms.ubs.addInstance({twin_repeat});"))
                    && xml.contains(&format!("window.forms.ubs.removeInstance({twin_repeat});")),
                "{data} must drive {twin}. Got:\n{}",
                xml
            );

            // The twin has no buttons, and does not claim any.
            let twin_panel = xml
                .split("<repeatableInner")
                .find(|chunk| chunk.contains(&format!("name=\"{twin_repeat}\"")))
                .and_then(|chunk| chunk.split("</repeatableInner>").next())
                .expect("the twin's repeating panel");
            assert!(
                !twin_panel.contains("name=\"BT_Remove\"")
                    && !twin_panel.contains("addButton=")
                    && !twin_panel.contains("removeButton="),
                "the twin must have no buttons of its own. Got:\n{}",
                twin_panel
            );
            // Its rows are the data panel's rows, so both announce them alike.
            assert!(
                twin_panel.contains("accessibilityLabel=\"Legal representative\""),
                "the twin takes the data panel's subject. Got:\n{}",
                twin_panel
            );
            // Exactly one Add button in the form: the data panel's.
            assert_eq!(
                xml.matches("name=\"BT_Add\"").count(),
                1,
                "only the data panel has an Add button. Got:\n{}",
                xml
            );
        }
    }

    /// A signature block is a twin only when it says so: one named like a
    /// party's twin (`RCP_SGN_Client` beside `RCP_Client`) but without
    /// `twin_of` repeats on its own, with its own Add and Remove.
    #[test]
    fn a_signature_named_like_a_twin_repeats_on_its_own() {
        let repeatable = |name: &str| AemNode::Repeatable {
            attrs: AemAttrs::default(),
            visible: true,
            uuid: uuid_of(name),
            name: name.into(),
            title: "Client".into(),
            children: vec![],
            min_occur: 1,
            max_occur: 5,
            bind_ref: None,
            frag_ref: None,
            twin_of: None,
        };
        let xml = render_tree(vec![repeatable("RCP_Client"), repeatable("RCP_SGN_Client")]);
        assert!(
            !xml.contains("window.forms.ubs.addInstance(RCP_SGN_Client_repeat);"),
            "the client's Add must not drive the signature. Got:\n{xml}"
        );
        assert_eq!(xml.matches("name=\"BT_Add\"").count(), 2, "each keeps its Add. Got:\n{xml}");
    }

    /// A `twin_of` that names no repeatable is refused: the party's Add would
    /// name a panel that is not there.
    #[test]
    fn a_twin_of_nothing_that_repeats_is_refused() {
        let twin = AemNode::Repeatable {
            attrs: AemAttrs::default(),
            visible: true,
            uuid: uuid_of("RCP_SGN_Client"),
            name: "RCP_SGN_Client".into(),
            title: "Client".into(),
            children: vec![],
            min_occur: 1,
            max_occur: 5,
            bind_ref: None,
            frag_ref: None,
            twin_of: Some("RCP_Gone".to_owned().try_into().unwrap()),
        };
        let root = AemNode::Root { title: "Form".into(), children: vec![twin] };
        let error = generate_aem_xml(&root, &test_config()).expect_err("an unknown party is refused");
        assert!(error.contains("`RCP_SGN_Client` repeats with `RCP_Gone`"), "{error}");
    }

    /// All six rule documents, byte for byte.
    ///
    /// Every one of them is a JCR multi-value property holding a single JSON
    /// document: the `[` unescaped, the commas inside the document escaped `\,`,
    /// and a newline written `\\n` so it survives as a newline in the JSON
    /// string. Written as one value carrying a JSON array instead — which is
    /// well-formed XML and re-parses as JSON — AEM reads one opaque string and
    /// the form editor refuses to open the form.
    ///
    /// The newline matters twice over: each body opens with the ownership comment
    /// (PROBLEM-repeating-panel §6), so a body that lost its newlines would be
    /// commented out in its entirety.
    #[test]
    fn the_button_rules_are_the_archetypes_own() {
        let xml = render_repeatable("RCP_Test", "Client", 1, 5);

        let comment = "// [repeating-panel] Generated automatically. Do not edit: will be \
                       overwritten. Create your own different script.";
        let expected: [(&str, String); 6] = [
            // BT_Add: add a row, then relabel the panel's rows and its buttons.
            // Focus moves through the labelling helper, which ends by calling
            // `setFocus`; with no field to name it lands on the button itself.
            (
                "BT_Add fd:click",
                format!(
                    r#"fd:click="[{{&quot;script&quot;:{{&quot;content&quot;:&quot;{comment}\\nwindow.forms.ubs.addInstance(this.parent.RCP_Test_repeat);\\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabels(this.parent.RCP_Test_repeat\, \\&quot;Client\\&quot;\, this);\\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabelsForButtons(this.parent.RCP_Test_repeat\, \\&quot;Client\\&quot;\, this\, this.parent.RCP_Test_repeat.instanceManager.instances[this.parent.RCP_Test_repeat.instanceManager.instances.length - 1].BT_Remove);&quot;\,&quot;event&quot;:&quot;Click&quot;\,&quot;field&quot;:&quot;BT_Add&quot;}}\,&quot;nodeName&quot;:&quot;SCRIPTMODEL&quot;\,&quot;version&quot;:1\,&quot;enabled&quot;:true\,&quot;_archetype&quot;:&quot;repeating-panel&quot;}}]""#
                ),
            ),
            // A single expression, no declaration in front of it: AEM takes the
            // value of a visibility body, and a `var` would make it `undefined`.
            (
                "BT_Add fd:visible",
                format!(
                    r#"fd:visible="[{{&quot;script&quot;:{{&quot;field&quot;:&quot;BT_Add&quot;\,&quot;event&quot;:&quot;Visibility&quot;\,&quot;model&quot;:{{&quot;nodeName&quot;:&quot;EVENT_SCRIPTS&quot;}}\,&quot;content&quot;:&quot;{comment}\\nthis.parent.RCP_Test_repeat.instanceManager.instances.length &lt; this.parent.RCP_Test_repeat.instanceManager.maxOccur;&quot;}}\,&quot;nodeName&quot;:&quot;SCRIPTMODEL&quot;\,&quot;version&quot;:1\,&quot;enabled&quot;:true\,&quot;_archetype&quot;:&quot;repeating-panel&quot;}}]""#
                ),
            ),
            // The same expression on Initialize, assigned: a visibility rule only
            // fires when a dependency changes, so on a freshly loaded form it
            // never runs and the button keeps whatever the node was saved with.
            (
                "BT_Add fd:init",
                format!(
                    r#"fd:init="[{{&quot;script&quot;:{{&quot;content&quot;:&quot;{comment}\\nthis.visible = (this.parent.RCP_Test_repeat.instanceManager.instances.length &lt; this.parent.RCP_Test_repeat.instanceManager.maxOccur);&quot;\,&quot;event&quot;:&quot;Initialize&quot;\,&quot;field&quot;:&quot;BT_Add&quot;}}\,&quot;nodeName&quot;:&quot;SCRIPTMODEL&quot;\,&quot;version&quot;:1\,&quot;enabled&quot;:true\,&quot;_archetype&quot;:&quot;repeating-panel&quot;}}]""#
                ),
            ),
            // The panel and the Add button are read into variables first: the row
            // this button lives in is gone by the next line.
            (
                "BT_Remove fd:click",
                format!(
                    r#"fd:click="[{{&quot;script&quot;:{{&quot;content&quot;:&quot;{comment}\\nvar repeatingPanel = this.parent;\\nvar addButton = this.parent.parent.BT_Add;\\nwindow.forms.ubs.removeInstance(repeatingPanel);\\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabels(repeatingPanel\, \\&quot;Client\\&quot;\, addButton);\\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabelsForButtons(repeatingPanel\, \\&quot;Client\\&quot;\, addButton\, this);&quot;\,&quot;event&quot;:&quot;Click&quot;\,&quot;field&quot;:&quot;BT_Remove&quot;}}\,&quot;nodeName&quot;:&quot;SCRIPTMODEL&quot;\,&quot;version&quot;:1\,&quot;enabled&quot;:true\,&quot;_archetype&quot;:&quot;repeating-panel&quot;}}]""#
                ),
            ),
            // `> minOccur`, not `> 1`: a panel that starts with two instances
            // would otherwise leave a dead Remove on screen at its minimum.
            (
                "BT_Remove fd:visible",
                format!(
                    r#"fd:visible="[{{&quot;script&quot;:{{&quot;field&quot;:&quot;BT_Remove&quot;\,&quot;event&quot;:&quot;Visibility&quot;\,&quot;model&quot;:{{&quot;nodeName&quot;:&quot;EVENT_SCRIPTS&quot;}}\,&quot;content&quot;:&quot;{comment}\\nthis.parent.instanceIndex === this.parent.instanceManager.instances.length - 1 &amp;&amp; this.parent.instanceManager.instances.length > this.parent.instanceManager.minOccur;&quot;}}\,&quot;nodeName&quot;:&quot;SCRIPTMODEL&quot;\,&quot;version&quot;:1\,&quot;enabled&quot;:true\,&quot;_archetype&quot;:&quot;repeating-panel&quot;}}]""#
                ),
            ),
            (
                "BT_Remove fd:init",
                format!(
                    r#"fd:init="[{{&quot;script&quot;:{{&quot;content&quot;:&quot;{comment}\\nthis.visible = (this.parent.instanceIndex === this.parent.instanceManager.instances.length - 1 &amp;&amp; this.parent.instanceManager.instances.length > this.parent.instanceManager.minOccur);&quot;\,&quot;event&quot;:&quot;Initialize&quot;\,&quot;field&quot;:&quot;BT_Remove&quot;}}\,&quot;nodeName&quot;:&quot;SCRIPTMODEL&quot;\,&quot;version&quot;:1\,&quot;enabled&quot;:true\,&quot;_archetype&quot;:&quot;repeating-panel&quot;}}]""#
                ),
            ),
        ];

        for (rule, text) in &expected {
            assert!(
                xml.contains(text.as_str()),
                "{rule} mismatch.\nExpected to find:\n{}\n\nIn:\n{}",
                text,
                xml
            );
        }
    }

    /// A page titled "Firme" holding one repeatable titled `title`, around the
    /// signature fragment when `signatures`, else around a text field.
    fn signature_page(title: &str, signatures: bool) -> AemNode {
        let content = if signatures {
            AemNode::Fragment {
                uuid: uuid_of("PN_Officer"),
                name: "PN_Officer".into(),
                title: String::new(),
                frag_ref: "/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_SignatureGeneric1".into(),
                attrs: AemAttrs::default(),
                visible: true,
                bind_ref: None,
                init: FragmentInit::default(),
            }
        } else {
            AemNode::TextField {
                uuid: uuid_of("TXT_Name"),
                name: "TXT_Name".into(),
                label: "Nome".into(),
                mandatory: false,
                visible: true,
                max_chars: None,
                attrs: AemAttrs::default(),
                colspan: 12,
                dor_colspan: None,
                bind_ref: None,
                kind: Default::default(),
                input: Default::default(),
            }
        };
        AemNode::Root {
            title: "Form".into(),
            children: vec![AemNode::Panel {
                uuid: uuid_of("PN_Firme"),
                name: "PN_Firme".into(),
                title: "Firme".into(),
                children: vec![AemNode::Repeatable {
                    uuid: uuid_of("RCP_Officers"),
                    name: "RCP_Officers".into(),
                    title: title.into(),
                    children: vec![content],
                    min_occur: 1,
                    max_occur: 2,
                    attrs: AemAttrs::default(),
                    visible: true,
                    bind_ref: None,
                    frag_ref: None,
                    twin_of: None,
                }],
                is_page: true,
                attrs: AemAttrs::default(),
                visible: true,
                is_conditional: false,
                dor_num_cols: None,
                colspan: 12,
                dor_colspan: None,
                bind_ref: None,
                frag_ref: None,
            }],
        }
    }

    fn subject(title: &str, signatures: bool) -> Option<String> {
        collect_add_subjects(&signature_page(title, signatures)).get("RCP_Officers").cloned()
    }

    /// A block of signatures is named after who signs, an entity line of up to
    /// eight words; any other repeatable keeps the six-word limit and falls back
    /// to the page's title. A sentence and a machine label stay no subject.
    #[test]
    fn a_signature_block_takes_an_entity_line_as_its_subject() {
        let entity = "UBS Europe SE (Succursale Italia)";
        assert_eq!(subject(entity, true).as_deref(), Some(entity));
        assert_eq!(subject(entity, false).as_deref(), Some(entity));
        assert_eq!(subject("Uno due tre quattro cinque sei sette", false).as_deref(), Some("Firme"));
        assert_eq!(subject("Uno due tre quattro cinque sei sette otto", true).as_deref(), Some("Uno due tre quattro cinque sei sette otto"));
        assert_eq!(subject("Uno due tre quattro cinque sei sette otto nove", true).as_deref(), Some("Firme"));
        assert_eq!(subject("Firmano i funzionari della banca.", true).as_deref(), Some("Firme"));
        assert_eq!(subject("Condition: RB_X = 1", true).as_deref(), Some("Firme"));
    }
}
