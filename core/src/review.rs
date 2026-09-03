//! Post-conversion fidelity review.
//!
//! Compares the engine's parse of the **input** (the merged structured tree)
//! against the **output** (the converted [`AemNode`] tree) and reports text or
//! elements that went missing along the way. This is a deterministic checklist
//! the conversion agent runs before finishing, complementing the structural
//! `validate_aem_*` checks (which verify the AEM contract, not input fidelity).
//!
//! Text is compared in a single language — the form's master language — because
//! AEM node labels are flattened to one language at conversion time
//! (translations ride along separately in the dictionary). Comparing all
//! languages would flag every non-master string as missing.

use std::collections::BTreeSet;

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use crate::aem::{AemConfig, AemNode, AemOption, generate_aem_xml};
use crate::structured::{FieldType, StructuredNode, TableNode};

/// The result of reviewing a converted output against its input.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReviewReport {
    /// Fraction of distinct input texts that appear verbatim in the output
    /// (`1.0` when the input has no text).
    pub coverage: f32,
    /// Number of input fields (`StructuredNode::Field`).
    pub input_field_count: usize,
    /// Number of output field-like leaves (text boxes, pickers, choice groups…).
    pub output_field_count: usize,
    /// Distinct input texts (labels, options, headings, paragraphs, …) with no
    /// verbatim match anywhere in the output.
    pub missing_text: Vec<String>,
    /// Naming-convention violations found in the rendered JCR XML. Mirrors the
    /// `find_naming_violations` detector: each author-named component's leading
    /// `PREFIX_` token must be valid for its `sling:resourceType`. Empty when
    /// every named node conforms.
    pub naming_violations: Vec<NamingViolation>,
    /// Input components whose `jcr:title` is missing or is not a plain label
    /// (a parenthetical hint, leaked rich-text markup, a whole paragraph …).
    /// Every input must carry a title; positional label attachment can leave
    /// one empty or bind the wrong nearby text.
    pub label_issues: Vec<LabelIssue>,
    /// Violations of the swept feedback rules -- the invariants the deployed
    /// corpus is held to, checked on the rendered JCR XML. See
    /// [`FeedbackViolation`].
    pub feedback_violations: Vec<FeedbackViolation>,
    /// Panels still carrying a table the old way: named `TBL_` and holding
    /// nothing but static draws. AEM now has an HTML component, so a table
    /// belongs in one `HtmlDisplayer` node with a real `<table>`; these are the
    /// blocks a conversion of an already-deployed package has to convert. Empty
    /// for anything this engine converts from a PDF.
    pub legacy_tables: Vec<String>,
    /// Standard fragments the deterministic conversion of this very input
    /// derives, that the reviewed tree does not reference. A hand-authored or
    /// re-authored tree that rebuilds a section by hand silently loses the
    /// fragment the converter had matched for it; the corpus expects the
    /// fragment, so every drop is reported here. See [`DroppedFragment`].
    pub dropped_fragments: Vec<DroppedFragment>,
    /// Human-readable observations (field-count mismatch, empty tree, truncation).
    pub notes: Vec<String>,
}

/// A single naming-convention violation (one non-conforming named node).
#[derive(Debug, Clone, serde::Serialize)]
pub struct NamingViolation {
    /// JCR node tag (AEM-internal id, e.g. `panel_77897ce3…`).
    pub node: String,
    /// The offending `name` attribute.
    pub name: String,
    /// `sling:resourceType` leaf (e.g. `textbox`, `panel`, `titledraw`).
    pub rt: String,
    /// Detected role (`panel`, `repeat-panel`, `repeat-subpanel`, `button`, or the rt).
    pub role: String,
    /// The canonical prefix expected for this role/type.
    pub expected: String,
    /// `wrong-prefix` (a known prefix, wrong for the type) or `raw` (not a known prefix).
    pub bucket: String,
    /// `high` for type / plain-panel / repeat-panel checks; `low` for repeat sub-panels.
    pub confidence: String,
}

/// A single input component whose label is missing or malformed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LabelIssue {
    /// JCR node tag (AEM-internal id, e.g. `radiobutton_77897ce3…`).
    pub node: String,
    /// The component's `name` attribute.
    pub name: String,
    /// `sling:resourceType` leaf (e.g. `textbox`, `radiobutton`).
    pub rt: String,
    /// The offending `jcr:title`, unescaped and truncated for readability
    /// (empty for `missing`).
    pub title: String,
    /// `missing` | `parenthetical` | `markup` | `quoted` | `duplicate`.
    pub kind: String,
    /// `high` for a missing title or a structurally wrong one (parenthetical /
    /// markup); `low` for `quoted`, which also occurs on genuine labels in the
    /// reference forms (`Classification as "Eligible Counterparty" …`).
    pub confidence: String,
}

/// A standard fragment the converter derives from this input but the
/// reviewed tree does not reference.
///
/// The engine matches the bank's reusable fragments against the source itself
/// (`use_fragments`), so the deterministic conversion of an input is the
/// authority on which fragments that form is entitled to. When a tree is
/// authored or re-authored by hand, rebuilding such a section out of loose
/// fields drops the fragment without any other signal -- the package still
/// builds, still validates, and still deploys. This is that signal.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DroppedFragment {
    /// JCR path of the missing fragment, e.g.
    /// `/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_SignatureGeneric1`.
    pub frag_ref: String,
    /// How many times the deterministic conversion references it.
    pub expected: usize,
    /// How many times the reviewed tree references it.
    pub present: usize,
}

/// Cap on how many dropped fragments to list, so the report stays readable.
const MAX_DROPPED_FRAGMENTS: usize = 100;

/// Cap on how many missing texts to list, so the report stays readable.
const MAX_MISSING: usize = 200;

/// Cap on how many label issues to list, so the report stays readable.
const MAX_LABEL_ISSUES: usize = 400;

/// Cap on how many naming violations to list, so the report stays readable.
const MAX_NAMING: usize = 400;

/// Count every `fragRef` the tree references, keyed by JCR path.
///
/// Only fragment references carried by the nodes themselves are counted, so
/// the comparison stays symmetric: the ones a profile template injects on its
/// own (the banking-relationship preface, the form-metadata appendix) never
/// appear on either side.
fn collect_frag_refs(node: &AemNode, out: &mut std::collections::BTreeMap<String, usize>) {
    let own = match node {
        AemNode::Fragment { frag_ref, .. } => Some(frag_ref.clone()),
        AemNode::Panel { frag_ref, .. } | AemNode::Repeatable { frag_ref, .. } => frag_ref.clone(),
        _ => None,
    };
    if let Some(frag_ref) = own {
        *out.entry(frag_ref).or_insert(0) += 1;
    }
    match node {
        AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } => {
            for child in children {
                collect_frag_refs(child, out);
            }
        }
        _ => {}
    }
}

/// Fragments the deterministic conversion of `input` derives that `output`
/// does not reference, or references fewer times.
///
/// Converting the input a second time is what makes this form-independent: the
/// engine's own fragment matching is the reference, so no list of fragments
/// has to be maintained anywhere.
fn find_dropped_fragments(
    input: &[StructuredNode],
    output: &AemNode,
    config: &AemConfig,
) -> Vec<DroppedFragment> {
    if !config.use_fragments {
        return Vec::new();
    }
    let baseline = crate::aem::convert_to_aem(input, config);
    let mut expected = std::collections::BTreeMap::new();
    collect_frag_refs(&baseline, &mut expected);
    let mut present = std::collections::BTreeMap::new();
    collect_frag_refs(output, &mut present);

    expected
        .into_iter()
        .filter_map(|(frag_ref, want)| {
            let have = present.get(&frag_ref).copied().unwrap_or(0);
            (have < want).then_some(DroppedFragment {
                frag_ref,
                expected: want,
                present: have,
            })
        })
        .take(MAX_DROPPED_FRAGMENTS)
        .collect()
}

/// Review the converted AEM `output` against the engine's parse of the `input`
/// (the merged structured tree), comparing text in `master_language`.
pub fn review_output(
    input: &[StructuredNode],
    output: &AemNode,
    config: &AemConfig,
    master_language: &str,
) -> ReviewReport {
    let mut input_texts: Vec<String> = Vec::new();
    let mut input_fields = 0usize;
    for node in input {
        collect_input(node, master_language, &mut input_texts, &mut input_fields);
    }

    let mut output_texts: Vec<String> = Vec::new();
    let mut output_rows: Vec<String> = Vec::new();
    let mut output_fields = 0usize;
    collect_output(
        output,
        &mut output_texts,
        &mut output_rows,
        &mut output_fields,
    );

    // Naming-convention check: render the tree to JCR XML and classify every
    // named node by its `sling:resourceType`, exactly like the
    // `find_naming_violations` detector (leading `PREFIX_` only). Rendering (not
    // walking the `AemNode` tree) is what lets this see template-expanded nodes
    // and custom-element internals, just as the detector sees the shipped ZIP.
    let aem_xml = generate_aem_xml(output, config);
    let (mut naming_violations, naming_counts, mut label_issues) =
        check_naming_conventions(&aem_xml);

    let mut feedback_violations = check_feedback_rules(&aem_xml);

    let mut legacy_tables = Vec::new();
    collect_legacy_tables(output, &mut legacy_tables);

    let dropped_fragments = find_dropped_fragments(input, output, config);

    collect_duplicate_sibling_titles(output, &mut label_issues);

    let (coverage, mut missing) =
        coverage_against(&input_texts, &output_texts, &output_rows);

    let mut notes = Vec::new();
    if input.is_empty() {
        notes.push("input (merged structured tree) is empty — nothing to compare".into());
    }
    if input_fields != output_fields {
        notes.push(format!(
            "field count differs: input has {input_fields}, output has {output_fields}"
        ));
    }
    if missing.len() > MAX_MISSING {
        notes.push(format!(
            "missing_text truncated to {MAX_MISSING} of {} entries",
            missing.len()
        ));
        missing.truncate(MAX_MISSING);
    }
    let [n_ok, n_wrong, n_raw] = naming_counts;
    if !naming_violations.is_empty() {
        notes.push(format!("naming: {n_wrong} wrong-prefix, {n_raw} raw ({n_ok} ok)"));
    }
    if naming_violations.len() > MAX_NAMING {
        notes.push(format!(
            "naming_violations truncated to {MAX_NAMING} of {} entries",
            naming_violations.len()
        ));
        naming_violations.truncate(MAX_NAMING);
    }
    if !label_issues.is_empty() {
        let missing_titles = label_issues.iter().filter(|l| l.kind == "missing").count();
        notes.push(format!(
            "labels: {missing_titles} inputs without a title, {} with a malformed one",
            label_issues.len() - missing_titles
        ));
    }
    if label_issues.len() > MAX_LABEL_ISSUES {
        notes.push(format!(
            "label_issues truncated to {MAX_LABEL_ISSUES} of {} entries",
            label_issues.len()
        ));
        label_issues.truncate(MAX_LABEL_ISSUES);
    }

    if !feedback_violations.is_empty() {
        let mut rules: Vec<&str> = feedback_violations.iter().map(|v| v.rule.as_str()).collect();
        rules.sort_unstable();
        rules.dedup();
        notes.push(format!(
            "feedback: {} violation(s) of {} swept rule(s) ({})",
            feedback_violations.len(),
            rules.len(),
            rules.join(", ")
        ));
    }
    if !legacy_tables.is_empty() {
        notes.push(format!(
            "{} panel(s) still hold a table as loose draws; convert each to one HtmlDisplayer \
             node carrying a real <table> ({})",
            legacy_tables.len(),
            legacy_tables.join(", ")
        ));
    }
    if legacy_tables.len() > MAX_LEGACY_TABLES {
        notes.push(format!(
            "legacy_tables truncated to {MAX_LEGACY_TABLES} of {} entries",
            legacy_tables.len()
        ));
        legacy_tables.truncate(MAX_LEGACY_TABLES);
    }

    if !dropped_fragments.is_empty() {
        let list: Vec<&str> = dropped_fragments
            .iter()
            .map(|d| d.frag_ref.as_str())
            .collect();
        notes.push(format!(
            "{} standard fragment(s) the converter derives from this source are not referenced by \
             the tree; reference the fragment instead of rebuilding the section out of loose \
             fields ({})",
            dropped_fragments.len(),
            list.join(", ")
        ));
    }

    if feedback_violations.len() > MAX_FEEDBACK {
        notes.push(format!(
            "feedback_violations truncated to {MAX_FEEDBACK} of {} entries",
            feedback_violations.len()
        ));
        feedback_violations.truncate(MAX_FEEDBACK);
    }

    ReviewReport {
        coverage,
        input_field_count: input_fields,
        output_field_count: output_fields,
        missing_text: missing,
        naming_violations,
        label_issues,
        feedback_violations,
        legacy_tables,
        dropped_fragments,
        notes,
    }
}

/// An input's `(name, label, resourceType leaf)`, or `None` for anything that
/// is not an input.
fn input_identity(node: &AemNode) -> Option<(&str, &str, &'static str)> {
    match node {
        AemNode::TextField { name, label, .. } => Some((name, label, "textbox")),
        AemNode::NumberField { name, label, .. } => Some((name, label, "numericbox")),
        AemNode::DatePicker { name, label, .. } => Some((name, label, "datepicker")),
        AemNode::Dropdown { name, label, .. } => Some((name, label, "dropdownlist")),
        AemNode::Checkbox { name, label, .. } => Some((name, label, "checkbox")),
        AemNode::RadioButton { name, label, .. } => Some((name, label, "radiobutton")),
        _ => None,
    }
}

/// Report inputs that sit side by side under one parent carrying the very same
/// title.
///
/// Two sibling inputs reading the same thing cannot be told apart -- not on
/// screen, not in the summary, not in the DoR, and not by anyone filling the
/// form. It happens when a source group holds several fields with no captions
/// of their own and each is given the same stand-in: AABF_019 shipped seven
/// date pickers in one panel, every one of them titled "Date". The engine's own
/// fallback is an empty title, which `classify_title` already reports as
/// `missing`; this catches the other half, where something plausible was filled
/// in for all of them at once.
fn collect_duplicate_sibling_titles(node: &AemNode, out: &mut Vec<LabelIssue>) {
    let children = match node {
        AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } => children,
        _ => return,
    };

    let mut seen: std::collections::BTreeMap<&str, Vec<(&str, &'static str, &AemNode)>> =
        std::collections::BTreeMap::new();
    for child in children {
        let Some((name, label, rt)) = input_identity(child) else {
            continue;
        };
        let title = label.trim();
        if title.is_empty() {
            continue;
        }
        seen.entry(title).or_default().push((name, rt, child));
    }
    for (title, inputs) in seen {
        if inputs.len() < 2 {
            continue;
        }
        for (name, rt, child) in inputs {
            out.push(LabelIssue {
                node: child.element_name(),
                name: name.to_string(),
                rt: rt.to_string(),
                title: title.chars().take(160).collect(),
                kind: "duplicate".to_string(),
                confidence: "high".to_string(),
            });
        }
    }

    for child in children {
        collect_duplicate_sibling_titles(child, out);
    }
}

/// Collect the names of panels that still hold a table the pre-HTML-component
/// way: named `TBL_` and holding nothing but static draws.
///
/// The engine used to flatten every source table into such a panel because AEM
/// had no table component. It has one now, so this is a review finding on a
/// package that was authored (or converted) before -- not a rendered-XML rule,
/// because "every child is a draw" is a fact about the tree, and not a
/// `PROBLEM-` slug either, because there is no such rule in the feedback repo's
/// registry to port.
fn collect_legacy_tables(node: &AemNode, out: &mut Vec<String>) {
    if let AemNode::Panel { name, children, .. } = node {
        let all_draws = !children.is_empty()
            && children
                .iter()
                .all(|c| matches!(c, AemNode::TextDraw { .. } | AemNode::TitleDraw { .. }));
        if name.starts_with("TBL_") && all_draws {
            out.push(name.clone());
        }
    }
    match node {
        AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } => {
            for child in children {
                collect_legacy_tables(child, out);
            }
        }
        _ => {}
    }
}

/// Review a generated [`RedactoDump`] against the engine's parse of the `input`,
/// comparing text in `master_language`.
///
/// Reviews **the dump**, not the structured tree it was generated from: the
/// tree is an intermediate, and text can still be dropped on the way into the
/// dump (input fields, images, and the other node kinds the Redacto converter
/// warns about and skips). Reviewing the artefact that actually ships is the
/// point — deriving the review from a different source than the output is
/// precisely how an empty Redacto dump once passed a clean fidelity check.
///
/// [`ReviewReport::naming_violations`] is always empty and
/// [`ReviewReport::output_field_count`] always zero: Redacto documents carry no
/// named components and no input fields.
pub fn review_redacto(
    input: &[StructuredNode],
    dump: &crate::redacto::RedactoDump,
    master_language: &str,
) -> ReviewReport {
    let mut input_texts: Vec<String> = Vec::new();
    let mut input_fields = 0usize;
    for node in input {
        collect_input(node, master_language, &mut input_texts, &mut input_fields);
    }

    // Compare against the master-language asset bodies. Fall back to every
    // language when the master has no variants, so a monolingual dump in another
    // language still reviews.
    let master_versions: Vec<&str> = dump
        .asset_versions
        .iter()
        .filter(|v| v.language == master_language)
        .map(|v| v.content.as_str())
        .collect();
    let bodies: Vec<&str> = if master_versions.is_empty() {
        dump.asset_versions
            .iter()
            .map(|v| v.content.as_str())
            .collect()
    } else {
        master_versions
    };

    // Each asset body is one HTML fragment that may hold several source texts —
    // a list asset carries every `<li>` — so offer both the whole fragment and
    // each tag-delimited run. Without the runs, list items and table cells look
    // missing merely because stripping the tags concatenates them.
    let mut output_texts: Vec<String> = Vec::new();
    for body in bodies {
        output_texts.push(body.to_string());
        output_texts.extend(html_text_runs(body));
    }

    let (coverage, mut missing) = coverage_against(&input_texts, &output_texts, &[]);

    let mut notes = Vec::new();
    if input.is_empty() {
        notes.push("input (structured tree) is empty — nothing to compare".into());
    }
    if dump.assets.is_empty() {
        notes.push("the dump contains no text assets — it describes an empty document".into());
    }
    if input_fields > 0 {
        notes.push(format!(
            "{input_fields} input field(s) were skipped: the Redacto target supports \
             text-only documents"
        ));
    }
    for warning in &dump.warnings {
        notes.push(warning.clone());
    }
    if missing.len() > MAX_MISSING {
        notes.push(format!(
            "missing_text truncated to {MAX_MISSING} of {} entries",
            missing.len()
        ));
        missing.truncate(MAX_MISSING);
    }

    ReviewReport {
        coverage,
        input_field_count: input_fields,
        output_field_count: 0,
        missing_text: missing,
        naming_violations: Vec::new(),
        // Redacto documents are text: none of the AEM shapes these rules police
        // exist there.
        feedback_violations: Vec::new(),
        // Redacto has real tables of its own; the AEM HTML component is not
        // part of that target.
        legacy_tables: Vec::new(),
        // Redacto is a text-only target: it has no AEM inputs to label.
        label_issues: Vec::new(),
        // Fragments are an AEM concept; a Redacto document references none.
        dropped_fragments: Vec::new(),
        notes,
    }
}

/// The text runs of an HTML fragment: the character data between tags, one
/// entry per run, empty runs dropped.
///
/// Deliberately not a parser — the fragments here are the ones the Redacto
/// content renderer emits, and all this needs to do is keep separate source
/// texts separate.
fn html_text_runs(html: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut current = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                if !current.trim().is_empty() {
                    runs.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
            }
            '>' => in_tag = false,
            _ if !in_tag => current.push(c),
            _ => {}
        }
    }
    if !current.trim().is_empty() {
        runs.push(current);
    }
    runs
}

/// The text of each `<tr>` in an HTML fragment, one entry per row, cells joined
/// by a single space.
///
/// This is the granularity a table's text has to be compared at. The engine's
/// detector merges adjacent columns into one cell, so the same source row can
/// arrive as `"0,010% 0,00 - 0,15%"` from the parse and as two separate `<td>`s
/// from an Author who read the page and split them back apart. Neither side is
/// wrong about the CONTENT; they disagree only about where the cell boundaries
/// are, and a row is the smallest unit both agree on.
///
/// A fragment with no `<tr>` at all (a chart, an image) yields one entry with
/// everything in it.
fn html_row_texts(html: &str) -> Vec<String> {
    fn flush(text: &mut String, row: &mut Vec<String>) {
        if !text.trim().is_empty() {
            row.push(text.trim().to_string());
        }
        text.clear();
    }

    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut tag = String::new();
    let mut in_tag = false;

    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                tag.clear();
                flush(&mut text, &mut row);
            }
            '>' if in_tag => {
                in_tag = false;
                // A closing `</tr>` ends the row.
                if let Some(name) = tag.trim().strip_prefix('/')
                    && name.trim().eq_ignore_ascii_case("tr")
                    && !row.is_empty()
                {
                    rows.push(row.join(" "));
                    row.clear();
                }
            }
            _ if in_tag => tag.push(c),
            _ => text.push(c),
        }
    }
    flush(&mut text, &mut row);
    if !row.is_empty() {
        rows.push(row.join(" "));
    }
    rows
}

/// Fraction of distinct input texts that appear verbatim in the output, plus the
/// ones that do not (both sides normalized, first-seen order preserved).
///
/// Shared by [`review_output`] and [`review_redacto`]: both answer the same
/// question — which input text did not survive into the shipped artefact — and
/// differ only in how they harvest the output side.
/// `output_containers` are output texts an input text may be found INSIDE
/// rather than equal to -- table rows, and only table rows. Cell boundaries are
/// the one place the two sides legitimately disagree (see [`html_row_texts`]),
/// so the leniency is confined there: a plain label still has to match exactly,
/// or a dropped `Name` would count as covered by a surviving `Last Name`.
fn coverage_against(
    input_texts: &[String],
    output_texts: &[String],
    output_containers: &[String],
) -> (f32, Vec<String>) {
    let output_set: BTreeSet<String> = output_texts
        .iter()
        .map(|t| normalize(t))
        .filter(|t| !t.is_empty())
        .collect();
    let containers: Vec<String> = output_containers
        .iter()
        .map(|t| normalize(t))
        .filter(|t| !t.is_empty())
        .collect();

    // Distinct, normalized, non-empty input texts (preserving first-seen order).
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut distinct_input: Vec<String> = Vec::new();
    for t in input_texts {
        let n = normalize(t);
        if !n.is_empty() && seen.insert(n.clone()) {
            distinct_input.push(n);
        }
    }

    let total = distinct_input.len();
    let missing: Vec<String> = distinct_input
        .into_iter()
        .filter(|t| {
            !output_set.contains(t) && !containers.iter().any(|row| row.contains(t.as_str()))
        })
        .collect();

    let coverage = if total == 0 {
        1.0
    } else {
        (total - missing.len()) as f32 / total as f32
    };
    (coverage, missing)
}

/// Normalize text for verbatim matching: strip simple `<...>` markup (AEM option
/// labels may carry rich-text HTML), then collapse whitespace runs and trim.
fn normalize(s: &str) -> String {
    let mut stripped = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => stripped.push(c),
            _ => {}
        }
    }
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ── Input side (structured tree) ─────────────────────────────────────────────

fn collect_input(node: &StructuredNode, lang: &str, out: &mut Vec<String>, fields: &mut usize) {
    match node {
        StructuredNode::Html(h) => out.extend(html_text_runs(h.markup_in(lang))),
        StructuredNode::Heading(h) => out.push(h.content.plain_text_in(lang)),
        StructuredNode::Paragraph(p) => out.push(p.content.plain_text_in(lang)),
        StructuredNode::Footnote(f) => out.push(f.content.plain_text_in(lang)),
        StructuredNode::Field(f) => {
            *fields += 1;
            if let Some(label) = &f.label {
                out.push(label.plain_text_in(lang));
            }
            if let Some(ph) = &f.placeholder {
                out.push(ph.get_or_default(lang).to_string());
            }
            match &f.input_type {
                FieldType::Radio { options }
                | FieldType::Select { options }
                | FieldType::CheckboxGroup { options } => {
                    for opt in options {
                        out.push(opt.name.get_or_default(lang).to_string());
                    }
                }
                _ => {}
            }
        }
        StructuredNode::Group(g) => {
            for child in &g.children {
                collect_input(child, lang, out, fields);
            }
        }
        StructuredNode::Repeatable(r) => collect_input(&r.item, lang, out, fields),
        StructuredNode::Conditional(c) => collect_input(&c.content, lang, out, fields),
        StructuredNode::GridLayout(grid) => {
            for el in &grid.elements {
                collect_input(&el.node, lang, out, fields);
            }
        }
        StructuredNode::List(list) => {
            for item in &list.items {
                out.push(item.plain_text_in(lang));
                if let Some(sub) = &item.sublist {
                    for sub_item in &sub.items {
                        out.push(sub_item.plain_text_in(lang));
                    }
                }
            }
        }
        StructuredNode::Table(t) => collect_table(t, lang, out, fields),
        // Images carry no text that maps to an AEM node; Empty is a placeholder.
        StructuredNode::Image(_) | StructuredNode::Empty => {}
    }
}

fn collect_table(t: &TableNode, lang: &str, out: &mut Vec<String>, fields: &mut usize) {
    if let Some(caption) = &t.caption {
        out.push(caption.plain_text_in(lang));
    }
    if let Some(header) = &t.header {
        for cell in &header.cells {
            collect_input(cell, lang, out, fields);
        }
    }
    for row in &t.rows {
        for cell in &row.cells {
            collect_input(cell, lang, out, fields);
        }
    }
}

// ── Output side (AEM tree) ───────────────────────────────────────────────────

fn collect_output(
    node: &AemNode,
    out: &mut Vec<String>,
    rows: &mut Vec<String>,
    fields: &mut usize,
) {
    let push_options = |out: &mut Vec<String>, options: &[AemOption]| {
        for opt in options {
            out.push(opt.label.clone());
        }
    };
    match node {
        AemNode::Root { title, children } => {
            out.push(title.clone());
            for c in children {
                collect_output(c, out, rows, fields);
            }
        }
        AemNode::Panel { title, children, .. } => {
            out.push(title.clone());
            for c in children {
                collect_output(c, out, rows, fields);
            }
        }
        AemNode::Repeatable { title, children, .. } => {
            out.push(title.clone());
            for c in children {
                collect_output(c, out, rows, fields);
            }
        }
        AemNode::TextField { label, .. }
        | AemNode::NumberField { label, .. }
        | AemNode::DatePicker { label, .. } => {
            *fields += 1;
            out.push(label.clone());
        }
        AemNode::Dropdown { label, options, .. }
        | AemNode::Checkbox { label, options, .. }
        | AemNode::RadioButton { label, options, .. }
        | AemNode::Custom { label, options, .. } => {
            *fields += 1;
            out.push(label.clone());
            push_options(out, options);
        }
        AemNode::TextDraw { content, .. } | AemNode::TitleDraw { content, .. } => {
            out.push(content.clone());
        }
        // The markup is not a label, so its text has to be pulled out of the
        // HTML the same way `html_text_runs` does it for a rich-text `_value`.
        // Without this a table would read as missing text on every conversion.
        AemNode::HtmlDisplayer { content, .. } => {
            for markup in content.0.values() {
                out.extend(html_text_runs(markup));
                rows.extend(html_row_texts(markup));
            }
        }
        // Runtime-resolved or text-free placeholders.
        AemNode::Fragment { .. }
        | AemNode::Preface { .. }
        | AemNode::Appendix { .. }
        | AemNode::FootnotePlaceholder { .. } => {}
    }
}

// ── Swept feedback rules (ports the feedback repo's detectors) ───────────────
//
// `ajila-forms-conversion-feedback` fixes systemic defects across the deployed
// UBS corpus and its CI guard fails any form that re-introduces one. A form this
// engine converts joins that corpus, and a package dropped in for review IS one
// of its forms, so the same verdicts belong in the review the agent reads --
// `scripts/check_feedback_rules.py` runs the real detectors, this runs the ones
// that need nothing but the rendered XML.
//
// Deliberately not ported, because they need data this crate does not have:
// PROBLEM-fragment-title-duplicate (the titles each fragment renders, vendored
// per fragment library) and PROBLEM-metadata-languages (the source PDFs
// delivered for the form).

/// One swept-rule violation found in the rendered JCR XML.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FeedbackViolation {
    /// The registry slug, e.g. `PROBLEM-dor-exclusion-implies-summary`.
    pub rule: String,
    /// The offending component's `name`, or its JCR tag when it has none.
    pub node: String,
    /// What is wrong with this node, in one line.
    pub detail: String,
}

/// Cap on how many feedback violations to list, so the report stays readable.
const MAX_FEEDBACK: usize = 400;

/// Cap on `legacy_tables`.
const MAX_LEGACY_TABLES: usize = 50;

/// Attribute value lookup on one open tag, quote-aware.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = tag;
    while let Some(at) = rest.find(name) {
        let after = &rest[at + name.len()..];
        let before_ok = at == 0
            || rest.as_bytes()[at - 1].is_ascii_whitespace();
        if before_ok && after.starts_with("=\"") {
            let value = &after[2..];
            return value.find('"').map(|end| &value[..end]);
        }
        rest = &rest[at + name.len()..];
    }
    None
}

fn has_attr(tag: &str, name: &str, value: &str) -> bool {
    attr(tag, name) == Some(value)
}

/// The `name` a violation is reported under: the component's own, or its tag.
fn tag_label(tag_name: &str, tag: &str) -> String {
    attr(tag, "name").unwrap_or(tag_name).to_string()
}

/// Every open tag of `xml` as `(tag_name, tag_text)`, quote-aware: a rich-text
/// `_value` contains a literal `>`, so a `<tag[^>]*>` scan splits tags in the
/// middle and both over- and under-matches.
fn open_tags(xml: &str) -> Vec<(&str, &str)> {
    let bytes = xml.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' || matches!(bytes.get(i + 1), Some(b'/') | Some(b'!') | Some(b'?')) {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i + 1;
        let mut quoted = false;
        while j < bytes.len() {
            match bytes[j] {
                b'"' => quoted = !quoted,
                b'>' if !quoted => break,
                _ => {}
            }
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }
        let tag = &xml[start..=j];
        let name_end = tag[1..]
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .map(|n| n + 1)
            .unwrap_or(tag.len());
        out.push((&tag[1..name_end], tag));
        i = j + 1;
    }
    out
}

/// Check the rendered JCR XML against the swept feedback rules.
pub(crate) fn check_feedback_rules(xml: &str) -> Vec<FeedbackViolation> {
    let mut out = Vec::new();
    let mut push = |rule: &str, node: String, detail: String| {
        out.push(FeedbackViolation {
            rule: rule.into(),
            node,
            detail,
        })
    };

    let tags = open_tags(xml);
    let mut save_progress = 0usize;

    for (tag_name, tag) in &tags {
        let label = tag_label(tag_name, tag);

        // PROBLEM-panel-type-ubs: every panel is the UBS custom panel. The
        // default AEM panel has no Summary authoring section, so the
        // jump-to-field button cannot be set on it.
        if has_attr(tag, "sling:resourceType", "fd/af/components/panel") {
            push(
                "PROBLEM-panel-type-ubs",
                label.clone(),
                "uses the default AEM panel; every panel must be \
                 ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
                    .into(),
            );
        }

        // PROBLEM-fragment-library-consolidation: the germany/italy person and
        // signature fragments are being emptied into the UBS generics (UBS
        // directive 2026-08-20, "AF Fragments and Common Fields with XSD
        // List"), so a reference to one is a defect. The deliberately
        // market-specific families stay: internal bank use, footnote, infobox,
        // banking relationship and the form configurator.
        if let Some(frag) = attr(tag, "fragRef") {
            let market = frag.contains("afforms_germany_fragmentlib/")
                || frag.contains("afforms_italy_fragmentlib/");
            let allowed = frag.contains("internalbankuse")
                || frag.contains("InternalBankUse")
                || frag.contains("internal_bank_use")
                || frag.contains("footnote")
                || frag.contains("infobox")
                || frag.contains("BankingRelationship")
                || frag.contains("FormConfig");
            if market && !allowed {
                push(
                    "PROBLEM-fragment-library-consolidation",
                    label.clone(),
                    format!(
                        "references the retired market fragment {frag}; person blocks use the \
                         UBS partner generics (affrg_ContractualPartnerGeneric1 / \
                         affrg_PartnertoPartnerGeneric1 / affrg_BeneficialOwnerGeneric1 / \
                         affrg_PowerofAttorneyGeneric1) and signatures use \
                         affrg_SignatureGeneric1"
                    ),
                );
            }
        }

        // PROBLEM-dor-exclusion-implies-summary: the UBS DoR is Redacto
        // rendering the summary, so a node kept out of the DoR but left in the
        // summary still reaches the reader.
        if has_attr(tag, "dorExclusion", "true") && !has_attr(tag, "summaryExclusion", "true") {
            push(
                "PROBLEM-dor-exclusion-implies-summary",
                label.clone(),
                "is excluded from the Document of Record but not from the summary".into(),
            );
        }

        // PROBLEM-visual-editor-rules: a rule lives in the code editor as
        // JavaScript on `fd:scripts`; `fd:rules` stays empty.
        if *tag_name == "fd:rules" {
            for prop in [
                "fd:visible",
                "fd:click",
                "fd:valueCommit",
                "fd:init",
                "fd:calculate",
                "fd:validate",
            ] {
                if tag.contains(&format!("{prop}=\"")) {
                    push(
                        "PROBLEM-visual-editor-rules",
                        label.clone(),
                        format!("carries a visual-editor rule in {prop}; it belongs on fd:scripts"),
                    );
                }
            }
        }

        // PROBLEM-checkbox-rich-text-options: without it the DoR wraps a long
        // caption under the box instead of beside it.
        if attr(tag, "sling:resourceType").is_some_and(|rt| rt.ends_with("controls/checkbox"))
            && !has_attr(tag, "richTextOptions", "true")
        {
            push(
                "PROBLEM-checkbox-rich-text-options",
                label.clone(),
                "a checkbox needs richTextOptions=\"true\" or its DoR caption wraps under the box"
                    .into(),
            );
        }

        // PROBLEM-jump-to-field-button: the Edit button belongs on the
        // step-title panel, never on the title draw, where it does nothing.
        if attr(tag, "sling:resourceType").is_some_and(|rt| rt.ends_with("controls/titledraw"))
            && has_attr(tag, "jumpToFieldButtonVisible", "true")
        {
            push(
                "PROBLEM-jump-to-field-button",
                label.clone(),
                "the jump-to-field button sits on the title draw, where it has no effect; \
                 it belongs on the enclosing step-title panel"
                    .into(),
            );
        }

        // PROBLEM-internal-bank-use-pdf-only: the bank's own copy. Never on
        // screen, never on the summary, always in the PDF -- and never
        // `dorExclusion`, which would undo `alwaysInPdf`.
        if let Some(frag_ref) = attr(tag, "fragRef") {
            if crate::aem::normalize::is_internal_bank_use(frag_ref) {
                let mut missing = Vec::new();
                if !has_attr(tag, "summaryExclusion", "true") {
                    missing.push("summaryExclusion");
                }
                if !has_attr(tag, "alwaysInPdf", "true") {
                    missing.push("alwaysInPdf");
                }
                if !has_attr(tag, "visible", "{Boolean}false") {
                    missing.push("visible=false");
                }
                if !missing.is_empty() {
                    push(
                        "PROBLEM-internal-bank-use-pdf-only",
                        label.clone(),
                        format!("internal-bank-use panel lacks {}", missing.join(", ")),
                    );
                }
                if has_attr(tag, "dorExclusion", "true") {
                    push(
                        "PROBLEM-internal-bank-use-pdf-only",
                        label.clone(),
                        "dorExclusion undoes alwaysInPdf: the block would reach no one".into(),
                    );
                }
            }

            // PROBLEM-infobox-dor-copy: the on-screen infobox is kept out of the
            // DoR, and a hidden copy carries it into the PDF.
            if frag_ref.ends_with("affrg_italy_infobox") {
                let hidden = has_attr(tag, "visible", "{Boolean}false");
                if hidden && !has_attr(tag, "alwaysInPdf", "true") {
                    push(
                        "PROBLEM-infobox-dor-copy",
                        label.clone(),
                        "the DoR copy of the infobox needs alwaysInPdf, or it renders nowhere"
                            .into(),
                    );
                } else if !hidden
                    && (!has_attr(tag, "dorExclusion", "true")
                        || !has_attr(tag, "summaryExclusion", "true"))
                {
                    push(
                        "PROBLEM-infobox-dor-copy",
                        label.clone(),
                        "the on-screen infobox must be excluded from the DoR and the summary"
                            .into(),
                    );
                }
            }
        }

        if has_attr(tag, "name", "fwbSaveProgress") {
            save_progress += 1;
            if has_attr(tag, "visible", "{Boolean}false") {
                push(
                    "PROBLEM-nav-save-progress-required",
                    label.clone(),
                    "the Save Progress button is hidden".into(),
                );
            }
        }
    }

    // PROBLEM-nav-save-progress-required: the toolbar must carry it at all.
    if save_progress == 0 {
        push(
            "PROBLEM-nav-save-progress-required",
            "toolbar".into(),
            "the toolbar has no Save Progress button (`fwbSaveProgress`)".into(),
        );
    }

    out
}

// ── Naming conventions (ports `find_naming_violations.py`) ───────────────────
//
// The UBS AEM naming convention: every author-named component's `name` attribute
// begins with `PREFIX_`, where PREFIX is fixed per component type (see
// `specs/AEM Naming Conventions.md`). This is the exact classification the
// `ajila-forms-conversion-feedback` detector applies to the shipped ZIP —
// reproduced here so the conversion agent gets the same verdicts up front.
//
// Scoping (kept identical to the detector; do not silently change):
//   - Only the leading `PREFIX_` token is enforced; the `<shortUuid>` is
//     optional and the `<CamelCaseName>` is never inspected or invented.
//   - System / fixed-name resourceTypes are exempt; fragment-referenced nodes
//     (`fragRef`, `affrg` in the name, an `AF_` prefix) are exempt; the `preview`
//     construct and `*Title` step-title wrappers are exempt.
//   - Three buckets: `ok`, `wrong-prefix` (a known prefix, wrong for the type),
//     `raw` (not a known prefix at all).

/// Canonical prefix(es) for a resourceType leaf (first is canonical). `None`
/// means "not a mapped field type".
fn type_prefixes(rt: &str) -> Option<&'static [&'static str]> {
    Some(match rt {
        "textbox" | "guidetextbox" => &["TXT"],
        "checkbox" => &["CB"],
        "radiobutton" => &["RB"],
        "dropdownlist" => &["DD"],
        "datepicker" | "guidedatepicker" => &["DATE"],
        "numericbox" => &["NB"],
        "telephone" => &["TEL"],
        "email" => &["EML"],
        "textboxMultiline" => &["TXTM"],
        "titledraw" => &["TTL"],
        "signature" => &["SIGN"],
        "textdraw" => &["ST", "ITXT", "ETXT"],
        "image" => &["IMG"],
        "chart" => &["CRT"],
        "separator" => &["SPT"],
        "barcode" => &["BARCODE"],
        "qrcode" => &["QRCODE"],
        "table" => &["TBL"],
        // One component renders all three: a table, a chart, an image. First
        // is canonical, the same multi-prefix shape `textdraw` uses.
        "htmlDisplayer" => &["TBL", "CRT", "IMG"],
        _ => return None,
    })
}

/// The authoritative closed set of allowed prefixes — used to tell a
/// `wrong-prefix` (a known prefix used on the wrong type) from a `raw` name.
const KNOWN_PREFIXES: &[&str] = &[
    "RB", "DATE", "TXT", "CB", "DD", "PN", "RCHT", "RCP", "RCBP", "RCHP", "NB", "BT", "ST", "TEL",
    "TTL", "TXTM", "EML", "IMG", "CRT", "SPT", "ITXT", "ETXT", "TBL", "BARCODE", "QRCODE", "SIGN",
];

/// System / fixed-name resourceType leaves — never author-named, skip entirely.
const EXEMPT_RT: &[&str] = &[
    "guideContainer", "rootPanel", "toolbar", "nextitemnav", "previtemnav", "submit", "summary",
    "dorOptionsUBS", "metadata", "letterhead", "carousel", "messagebox",
    "messagebox-CarouselPreviewError", "errorboxcarouselpreview", "previewbutton", "signaturebox",
    "guidefootnoteplaceholder", "guideheader", "guidefooter", "formtitle", "aftemplatedpage",
    "responsivegrid", "guidefieldset", "defaultGuideLayout", "wizard", "gridFluidLayout2",
    "defaultToolbarLayout", "dorProperties",
];

/// Button resourceType leaves (also anything ending in `button`).
const BUTTON_RT: &[&str] = &["removebutton", "tertiarybutton", "secondarybutton", "primarybutton"];

/// System panels with fixed engine names — exempt.
const EXEMPT_PANEL_NAMES: &[&str] = &[
    "summaryPanel",
    "previewPanel",
    "PN_Preview",
    "guideRootPanel",
    // The global FormMetadata fragment step carries the fragment's own fixed
    // name, so the `PN_` convention does not apply to it.
    "FormMetadata",
];

/// Input (data-entry) resourceType leaves. Every one of these must present a
/// label to the user, so a missing `jcr:title` is a defect — unlike a draw or
/// a panel, which legitimately carries none.
const INPUT_RT: &[&str] = &[
    "textbox", "guidetextbox", "textboxMultiline", "numericbox", "datepicker", "guidedatepicker",
    "dropdownlist", "radiobutton", "checkbox", "telephone", "email",
];

/// Classify one input's `jcr:title`. `None` = the title is a plain, usable
/// label.
///
/// Both failure modes this catches come from positional label attachment
/// (`document::modules::label_attacher`): it binds the nearest free text block
/// to a field, so a field can end up with no label at all once its neighbours
/// are taken, or with a fragment that merely sits close by — a parenthetical
/// hint, a rich-text paragraph — instead of its actual question.
fn classify_title(
    rt_leaf: &str,
    title: &str,
    option_labels_present: bool,
) -> Option<(&'static str, &'static str)> {
    if !INPUT_RT.contains(&rt_leaf) {
        return None;
    }
    let t = title.trim();
    if t.is_empty() {
        // A single-option checkbox carries its label in the option by design
        // (`converter.rs`, `FieldType::Bool`), so it renders a label even with
        // an empty title.
        if rt_leaf == "checkbox" && option_labels_present {
            return None;
        }
        return Some(("missing", "high"));
    }
    if t.starts_with('(') && t.ends_with(')') {
        return Some(("parenthetical", "high"));
    }
    if t.contains('<') && t.contains('>') {
        return Some(("markup", "high"));
    }
    if t.contains('"') {
        return Some(("quoted", "low"));
    }
    None
}

/// A classified verdict for one node.
struct Verdict {
    bucket: &'static str, // "ok" | "wrong-prefix" | "raw"
    expected: String,
    role: String,
    confidence: &'static str,
}

/// Leading prefix token — everything before the first `_` (or the whole name).
fn name_prefix(nm: &str) -> &str {
    nm.split_once('_').map(|(a, _)| a).unwrap_or(nm)
}

/// Classify one element. `None` = skip (exempt / unmapped / no name). Mirrors
/// `classify()` in the detector.
fn classify(
    tag: &str,
    rt_leaf: &str,
    name: &str,
    has_frag_ref: bool,
    self_repeatable: bool,
    ancestor_in_repeat: bool,
) -> Option<Verdict> {
    if name.is_empty() || rt_leaf.is_empty() {
        return None;
    }
    if EXEMPT_RT.contains(&rt_leaf) || rt_leaf.to_ascii_lowercase().contains("layout") {
        return None;
    }
    // fragment-referenced / engine-injected — exempt.
    if has_frag_ref || name.contains("affrg") || name.starts_with("AF_") {
        return None;
    }
    // the UBS "Preview" construct is referenced by name on nearly every form.
    if name.eq_ignore_ascii_case("preview") {
        return None;
    }

    let (role, valid, confidence): (String, Vec<&'static str>, &'static str) = if rt_leaf == "panel"
    {
        if EXEMPT_PANEL_NAMES.contains(&name)
            || tag.starts_with("summarypanel")
            || tag.starts_with("previewpanel")
        {
            return None;
        }
        // title-wrapper panels track their parent's name — exempt (parent drives the fix).
        if tag.starts_with("panel_title") || name.ends_with("Title") {
            return None;
        }
        if self_repeatable {
            // a repeatable panel → repeat-container panel; accept the RC* family.
            ("repeat-panel".into(), vec!["RCP", "RCBP", "RCHP", "RCHT"], "high")
        } else if ancestor_in_repeat {
            // a sub-panel within a repeat container — low confidence, human review.
            ("repeat-subpanel".into(), vec!["RCP", "RCHP", "RCBP", "RCHT", "PN"], "low")
        } else {
            ("panel".into(), vec!["PN"], "high")
        }
    } else if let Some(tp) = type_prefixes(rt_leaf) {
        // must precede the button catch — radiobutton ends in "button".
        (rt_leaf.to_string(), tp.to_vec(), "high")
    } else if BUTTON_RT.contains(&rt_leaf) || rt_leaf.ends_with("button") {
        ("button".into(), vec!["BT"], "high")
    } else {
        return None; // unknown / unmapped type — don't guess.
    };

    let expected = valid[0].to_string();
    let pfx = name_prefix(name);
    let bucket = if valid.contains(&pfx) {
        "ok"
    } else if KNOWN_PREFIXES.contains(&pfx) {
        "wrong-prefix"
    } else {
        "raw"
    };
    Some(Verdict { bucket, expected, role, confidence })
}

/// Read the attributes we care about + whether this element is itself a repeat
/// container, classify it, and record a violation if non-`ok`. Returns whether
/// the element is repeatable (so descendants count as "in a repeat container").
fn scan_element(
    e: &BytesStart,
    ancestor_in_repeat: bool,
    counts: &mut [usize; 3],
    out: &mut Vec<NamingViolation>,
    labels: &mut Vec<LabelIssue>,
) -> bool {
    let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    let mut name = String::new();
    let mut rt_full = String::new();
    let mut title = String::new();
    let mut options = String::new();
    let mut has_frag_ref = false;
    let mut has_occur = false;
    for attr in e.attributes().flatten() {
        match attr.key.as_ref() {
            b"name" => name = attr.unescape_value().unwrap_or_default().into_owned(),
            b"sling:resourceType" => {
                rt_full = attr.unescape_value().unwrap_or_default().into_owned()
            }
            b"jcr:title" => title = attr.unescape_value().unwrap_or_default().into_owned(),
            b"options" => options = attr.unescape_value().unwrap_or_default().into_owned(),
            b"fragRef" => has_frag_ref = true,
            b"minOccur" | b"maxOccur" => has_occur = true,
            _ => {}
        }
    }
    let rt_leaf = rt_full.rsplit('/').next().unwrap_or("");
    let self_repeatable = has_occur || tag.starts_with("repeatable");

    // `options` is `[value=Label,…]`; a label is present when any entry carries
    // text after its `=`.
    let option_labels_present = options
        .trim_matches(['[', ']'])
        .split(',')
        .any(|o| o.split_once('=').is_some_and(|(_, l)| !l.trim().is_empty()));
    if let Some((kind, confidence)) = classify_title(rt_leaf, &title, option_labels_present) {
        labels.push(LabelIssue {
            node: tag.clone(),
            name: name.clone(),
            rt: rt_leaf.to_string(),
            title: title.chars().take(160).collect(),
            kind: kind.to_string(),
            confidence: confidence.to_string(),
        });
    }

    if let Some(v) = classify(&tag, rt_leaf, &name, has_frag_ref, self_repeatable, ancestor_in_repeat)
    {
        match v.bucket {
            "ok" => counts[0] += 1,
            "wrong-prefix" => counts[1] += 1,
            "raw" => counts[2] += 1,
            _ => {}
        }
        if v.bucket != "ok" {
            out.push(NamingViolation {
                node: tag,
                name,
                rt: rt_leaf.to_string(),
                role: v.role,
                expected: v.expected,
                bucket: v.bucket.to_string(),
                confidence: v.confidence.to_string(),
            });
        }
    }
    self_repeatable
}

/// Parse rendered JCR XML once and return the naming violations with their
/// `[ok, wrong-prefix, raw]` counts, plus every input whose label is missing or
/// malformed.
fn check_naming_conventions(
    xml: &str,
) -> (Vec<NamingViolation>, [usize; 3], Vec<LabelIssue>) {
    let mut reader = Reader::from_str(xml);
    let mut violations = Vec::new();
    let mut labels = Vec::new();
    let mut counts = [0usize; 3];
    // Stack of "is this open element a repeat container" flags; the running
    // count of `true`s is the number of repeat ancestors.
    let mut stack: Vec<bool> = Vec::new();
    let mut repeat_ancestors: usize = 0;
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let rep = scan_element(
                    e,
                    repeat_ancestors > 0,
                    &mut counts,
                    &mut violations,
                    &mut labels,
                );
                stack.push(rep);
                if rep {
                    repeat_ancestors += 1;
                }
            }
            Ok(Event::Empty(ref e)) => {
                scan_element(
                    e,
                    repeat_ancestors > 0,
                    &mut counts,
                    &mut violations,
                    &mut labels,
                );
            }
            Ok(Event::End(_)) => {
                if let Some(rep) = stack.pop() {
                    if rep {
                        repeat_ancestors = repeat_ancestors.saturating_sub(1);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break, // our own render is well-formed; bail on the unexpected.
            _ => {}
        }
    }
    (violations, counts, labels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aem::{AemAttrs, AemConfig};
    use crate::structured::{
        FieldNode, FieldType, HeadingLevel, HeadingNode, TableRow, TranslatedText,
    };

    fn text_field(name: &str, label: &str) -> StructuredNode {
        StructuredNode::Field(FieldNode {
            name: name.into(),
            som_path: None,
            label: Some(TranslatedText::plain_with_lang("en", label)),
            input_type: FieldType::Text {
                regex: None,
                max_length: None,
                min_length: None,
            },
            value: None,
            placeholder: None,
            required: false,
        })
    }

    fn sample_input() -> Vec<StructuredNode> {
        vec![
            StructuredNode::Heading(HeadingNode {
                level: HeadingLevel::H1,
                content: TranslatedText::plain_with_lang("en", "Personal data"),
                som_path: None,
                source_name: None,
            }),
            text_field("first_name", "First name"),
            text_field("last_name", "Last name"),
        ]
    }

    fn cell_paragraph(text: &str) -> StructuredNode {
        StructuredNode::Paragraph(crate::structured::ParagraphNode {
            content: TranslatedText::plain_with_lang("en", text),
            som_path: None,
            source_name: None,
        })
    }

    fn draw(name: &str, text: &str) -> AemNode {
        AemNode::TextDraw {
            uuid: uuid::Uuid::new_v4(),
            name: name.into(),
            content: format!("<p>{text}</p>"),
            attrs: AemAttrs::default(),
            visible: true,
            colspan: 12,
            dor_colspan: None,
        }
    }

    fn panel(name: &str, children: Vec<AemNode>) -> AemNode {
        AemNode::Panel {
            uuid: uuid::Uuid::new_v4(),
            name: name.into(),
            title: String::new(),
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
        }
    }

    /// The engine's table detector merges adjacent columns into one cell, so a
    /// source row arrives as `"0,010% 0,00 - 0,15%"`. An Author that reads the
    /// page and splits that back into the two real columns is MORE faithful than
    /// the parse -- and used to be punished for it: neither half matched the
    /// merged input string, so the text read as missing and coverage collapsed.
    /// A real agent run on AACW_019 scored 0.51 that way and spent two review
    /// rounds arguing with the number.
    #[test]
    fn a_split_table_cell_still_counts_as_covered() {
        let input = vec![StructuredNode::Table(TableNode {
            header: None,
            caption: None,
            rows: vec![TableRow {
                cells: vec![
                    cell_paragraph("Börsengebühren (3)"),
                    // One merged cell, the way the detector reports it.
                    cell_paragraph("0,010% 0,00 - 0,15%"),
                ],
            }],
        })];
        // What a careful Author authors instead: the two real columns.
        let markup = "<table><tbody><tr><td>Börsengebühren (3)</td>\
                      <td>0,010%</td><td>0,00 - 0,15%</td></tr></tbody></table>";
        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![AemNode::HtmlDisplayer {
                uuid: uuid::Uuid::new_v4(),
                name: "TBL_Fees".into(),
                content: crate::aem::AemI18nText::single("en", markup),
                attrs: AemAttrs::default(),
                visible: true,
                colspan: 12,
                dor_colspan: None,
            }],
        };

        let cfg = AemConfig::test_default("TEST");
        let report = review_output(&input, &root, &cfg, "en");

        assert!(
            report.missing_text.is_empty(),
            "splitting a merged cell is not a loss: {:?}",
            report.missing_text
        );
        assert_eq!(report.coverage, 1.0, "and coverage must say so");
    }

    /// The leniency must not hide a real drop: text that is nowhere in the
    /// output is still reported, and a short label must not match merely because
    /// it is a substring of an unrelated one.
    #[test]
    fn genuinely_absent_table_text_is_still_reported() {
        let input = vec![StructuredNode::Table(TableNode {
            header: None,
            caption: None,
            rows: vec![TableRow {
                cells: vec![
                    cell_paragraph("Börsengebühren (3)"),
                    cell_paragraph("Fremde Spesen"),
                ],
            }],
        })];
        let markup = "<table><tbody><tr><td>Börsengebühren (3)</td></tr></tbody></table>";
        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![AemNode::HtmlDisplayer {
                uuid: uuid::Uuid::new_v4(),
                name: "TBL_Fees".into(),
                content: crate::aem::AemI18nText::single("en", markup),
                attrs: AemAttrs::default(),
                visible: true,
                colspan: 12,
                dor_colspan: None,
            }],
        };

        let cfg = AemConfig::test_default("TEST");
        let report = review_output(&input, &root, &cfg, "en");

        assert_eq!(
            report.missing_text,
            vec!["Fremde Spesen".to_string()],
            "a dropped cell is still a loss"
        );
    }

    /// A `TBL_` panel of loose draws is how a table had to be written while AEM
    /// had no table component. It has one now, so the review names each such
    /// panel for the Reviewer to convert.
    #[test]
    fn a_legacy_table_panel_is_reported() {
        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![panel(
                "PN_Step",
                vec![panel(
                    "TBL_Plans",
                    vec![draw("ST_A", "Plan"), draw("ST_B", "Share")],
                )],
            )],
        };
        let cfg = AemConfig::test_default("TEST");
        let report = review_output(&[], &root, &cfg, "en");

        assert_eq!(report.legacy_tables, vec!["TBL_Plans".to_string()]);
        assert!(
            report.notes.iter().any(|n| n.contains("HtmlDisplayer")),
            "and the note says what to convert it to: {:?}",
            report.notes
        );
    }

    /// The HTML component is the fixed shape, so it must not be reported -- and
    /// neither must a `TBL_` panel that still holds an input field, since that
    /// one legitimately keeps the panel.
    #[test]
    fn the_html_component_and_an_interactive_table_are_not_reported() {
        let root = AemNode::Root {
            title: "TEST".into(),
            children: vec![
                AemNode::HtmlDisplayer {
                    uuid: uuid::Uuid::new_v4(),
                    name: "TBL_Plans".into(),
                    content: crate::aem::AemI18nText::single(
                        "en",
                        "<table><tr><td>Plan</td></tr></table>",
                    ),
                    attrs: AemAttrs::default(),
                    visible: true,
                    colspan: 12,
                    dor_colspan: None,
                },
                panel(
                    "TBL_Interactive",
                    vec![
                        draw("ST_A", "Plan"),
                        crate::convert_to_aem(
                            &[text_field("share", "Share")],
                            &AemConfig::test_default("TEST"),
                        ),
                    ],
                ),
            ],
        };
        let cfg = AemConfig::test_default("TEST");
        let report = review_output(&[], &root, &cfg, "en");

        assert!(
            report.legacy_tables.is_empty(),
            "neither shape is a legacy table: {:?}",
            report.legacy_tables
        );
    }

    /// The component renders a table, a chart or an image, so all three
    /// prefixes are legal on it -- `TBL_` first, as the canonical one.
    #[test]
    fn the_html_component_accepts_the_table_chart_and_image_prefixes() {
        let allowed = type_prefixes("htmlDisplayer").expect("the component is a mapped type");
        assert_eq!(allowed, ["TBL", "CRT", "IMG"]);
    }

    #[test]
    fn roundtrip_has_no_misses() {
        let input = sample_input();
        let cfg = AemConfig::test_default("TEST");
        let aem = crate::convert_to_aem(&input, &cfg);
        let report = review_output(&input, &aem, &cfg, "en");
        assert!(
            report.missing_text.is_empty(),
            "expected no misses, got {:?}",
            report.missing_text
        );
        assert_eq!(report.input_field_count, 2);
        assert_eq!(report.output_field_count, 2);
        assert!((report.coverage - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn dropped_field_is_reported() {
        let input = sample_input();
        // Output is missing "Last name".
        let reduced: Vec<StructuredNode> = input.iter().take(2).cloned().collect();
        let cfg = AemConfig::test_default("TEST");
        let aem = crate::convert_to_aem(&reduced, &cfg);
        let report = review_output(&input, &aem, &cfg, "en");
        assert!(
            report.missing_text.iter().any(|t| t == "Last name"),
            "expected 'Last name' missing, got {:?}",
            report.missing_text
        );
        assert!(report.coverage < 1.0);
    }

    // ── Naming-convention checks (ported detector) ────────────────────────────

    #[test]
    fn name_prefix_is_leading_token() {
        assert_eq!(name_prefix("TXT_FirstName_ab12cd34"), "TXT");
        assert_eq!(name_prefix("PN_affrg_Address1"), "PN"); // splits at FIRST underscore
        assert_eq!(name_prefix("bareName"), "bareName");
    }

    /// Only the leading `PREFIX_` matters — a missing shortUuid or an arbitrary
    /// CamelCase part is still `ok`.
    #[test]
    fn leading_prefix_only_is_ok() {
        let v = classify("textbox_x", "textbox", "TXT_FirstName_ab12cd34", false, false, false).unwrap();
        assert_eq!(v.bucket, "ok");
        let v = classify("textbox_x", "textbox", "TXT_FirstName", false, false, false).unwrap();
        assert_eq!(v.bucket, "ok"); // no shortUuid — still fine
        let v = classify("datepicker_x", "datepicker", "DATE", false, false, false).unwrap();
        assert_eq!(v.bucket, "ok"); // prefix only
    }

    #[test]
    fn wrong_prefix_vs_raw_buckets() {
        // NUM_ is not a known prefix at all → raw.
        let v = classify("numericbox_x", "numericbox", "NUM_Age_ab12", false, false, false).unwrap();
        assert_eq!((v.bucket, v.expected.as_str()), ("raw", "NB"));
        // PN_ is a known prefix but wrong for a repeat panel (expects RCP) → wrong-prefix.
        let v = classify("panel_x", "panel", "PN_Rows", false, true, false).unwrap();
        assert_eq!((v.bucket, v.expected.as_str(), v.role.as_str()), ("wrong-prefix", "RCP", "repeat-panel"));
        // legacy RP_ / lowercase tag word → raw.
        assert_eq!(classify("panel_x", "panel", "RP_x", false, false, false).unwrap().bucket, "raw");
        assert_eq!(classify("panel_x", "panel", "panel_11881", false, false, false).unwrap().bucket, "raw");
    }

    #[test]
    fn exemptions_are_skipped() {
        // fragment-referenced
        assert!(classify("panel_x", "panel", "PN_x", true, false, false).is_none());
        // affrg in name / AF_ prefix
        assert!(classify("panel_x", "panel", "affrg_Address", false, false, false).is_none());
        assert!(classify("textbox_x", "textbox", "AF_bound", false, false, false).is_none());
        // system resourceType + layouts
        assert!(classify("toolbar_x", "toolbar", "whatever", false, false, false).is_none());
        assert!(classify("layout", "gridFluidLayout2", "x", false, false, false).is_none());
        // preview construct + Title wrappers + system panels
        assert!(classify("tertiarybutton_x", "tertiarybutton", "preview", false, false, false).is_none());
        assert!(classify("panel_title_x", "panel", "PN_FooTitle", false, false, false).is_none());
        assert!(classify("panel_x", "panel", "guideRootPanel", false, false, false).is_none());
        // unmapped resourceType → skip (don't guess)
        assert!(classify("weird_x", "somethingElse", "XX_y", false, false, false).is_none());
    }

    #[test]
    fn panel_roles_and_repeat_context() {
        // plain panel expects PN
        assert_eq!(classify("panel_x", "panel", "PN_Foo", false, false, false).unwrap().bucket, "ok");
        // repeat panel (own minOccur/maxOccur) → RC* family, high confidence
        let v = classify("panel_x", "panel", "RCP_Foo", false, true, false).unwrap();
        assert_eq!((v.role.as_str(), v.confidence), ("repeat-panel", "high"));
        // sub-panel inside a repeat container → low confidence, PN also accepted
        let v = classify("panel_x", "panel", "PN_Inner", false, false, true).unwrap();
        assert_eq!((v.bucket, v.role.as_str(), v.confidence), ("ok", "repeat-subpanel", "low"));
    }

    #[test]
    fn buttons_expect_bt() {
        assert_eq!(classify("removebutton_x", "removebutton", "BT_Remove", false, false, false).unwrap().bucket, "ok");
        // radiobutton must NOT be caught by the button rule (it's a mapped field type)
        let v = classify("radiobutton_x", "radiobutton", "RB_Choice", false, false, false).unwrap();
        assert_eq!((v.bucket, v.role.as_str()), ("ok", "radiobutton"));
    }

    #[test]
    fn xml_walk_reports_violations_with_counts() {
        // Two named nodes: a conforming textbox and a raw-named panel.
        let xml = r#"<jcr:root xmlns:jcr="j" xmlns:sling="s">
            <panel_1 sling:resourceType="ubs/controls/panel" name="myPanel"/>
            <textbox_1 sling:resourceType="ubs/controls/textbox" name="TXT_First_ab12cd34"/>
        </jcr:root>"#;
        let (viol, counts, _) = check_naming_conventions(xml);
        assert_eq!(counts, [1, 0, 1], "one ok (textbox), one raw (panel)");
        assert_eq!(viol.len(), 1);
        assert_eq!(viol[0].name, "myPanel");
        assert_eq!((viol[0].bucket.as_str(), viol[0].expected.as_str()), ("raw", "PN"));
        assert_eq!(viol[0].role, "panel");
    }

    #[test]
    fn xml_walk_tracks_repeat_ancestry() {
        // A sub-panel is only "repeat-subpanel" when nested under a repeatable.
        let xml = r#"<r xmlns:sling="s">
            <repeatable_1 sling:resourceType="ubs/controls/panel" name="RCP_Rows" maxOccur="4">
                <panel_2 sling:resourceType="ubs/controls/panel" name="PN_Inner"/>
            </repeatable_1>
        </r>"#;
        let (viol, counts, _) = check_naming_conventions(xml);
        // RCP_Rows (repeat-panel, ok) + PN_Inner (repeat-subpanel, ok) → no violations.
        assert_eq!(counts, [2, 0, 0], "both conform, got {viol:?}");
        assert!(viol.is_empty());
    }

    /// A germany/italy person or signature fragment is retired (UBS general
    /// fragments, 2026-08-20); the deliberately market-specific families and
    /// the UBS generics themselves are not.
    #[test]
    fn retired_market_fragments_are_reported() {
        let xml = r#"<r xmlns:sling="s" xmlns:jcr="j">
            <panel_1 sling:resourceType="ubs/controls/panel" name="PN_AHRP"
                fragRef="/content/dam/formsanddocuments/afforms_germany_fragmentlib/affrg_IndividualBasic1"/>
            <panel_2 sling:resourceType="ubs/controls/panel" name="PN_LRP_Sign"
                fragRef="/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_LegalRepresentativeSignature1"/>
            <panel_3 sling:resourceType="ubs/controls/panel" name="PN_FRG_InternalBankUseOnly"
                fragRef="/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_internalbankuse_ouref"/>
            <panel_4 sling:resourceType="ubs/controls/panel" name="PN_ITFootnote"
                fragRef="/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_footnote"/>
            <panel_5 sling:resourceType="ubs/controls/panel" name="PN_BankingRelationship"
                fragRef="/content/forms/af/afforms_ubs_fragmentlib/affrg_BankingRelationship1"/>
            <panel_6 sling:resourceType="ubs/controls/panel" name="PN_CPGRP"
                fragRef="/content/dam/formsanddocuments/afforms_ubs_fragmentlib/affrg_ContractualPartnerGeneric1"/>
        </r>"#;
        let hits: Vec<_> = check_feedback_rules(xml)
            .into_iter()
            .filter(|v| v.rule == "PROBLEM-fragment-library-consolidation")
            .collect();
        let names: Vec<&str> = hits.iter().map(|v| v.node.as_str()).collect();
        assert_eq!(names, vec!["PN_AHRP", "PN_LRP_Sign"], "got {hits:?}");
        assert!(hits[0].detail.contains("affrg_IndividualBasic1"), "{hits:?}");
        assert!(hits[0].detail.contains("affrg_SignatureGeneric1"), "the detail names the fix");
    }

    /// AALJ shipped a radio button with no `jcr:title` at all: the label
    /// attacher found no free text block for it, and nothing downstream
    /// asserted that an input ends up labelled.
    #[test]
    fn untitled_input_is_reported() {
        let xml = r#"<r xmlns:sling="s" xmlns:jcr="j">
            <radiobutton_1 sling:resourceType="ubs/controls/radiobutton" name="RB_UsedFor"
                options="[RB_1=business purposes,RB_2=private purposes]"/>
            <textbox_1 sling:resourceType="ubs/controls/textbox" name="TXT_CommValue"/>
            <textbox_2 sling:resourceType="ubs/controls/textbox" name="TXT_Ok" jcr:title="Last name"/>
        </r>"#;
        let (_, _, labels) = check_naming_conventions(xml);
        let names: Vec<&str> = labels.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["RB_UsedFor", "TXT_CommValue"], "got {labels:?}");
        assert!(labels.iter().all(|l| l.kind == "missing" && l.confidence == "high"));
    }

    /// A single-option checkbox carries its label in the option by design
    /// (`converter.rs`, `FieldType::Bool`), so an empty title is not a defect
    /// there — but a choice group whose options carry no labels still is.
    #[test]
    fn single_checkbox_label_may_live_in_its_option() {
        let xml = r#"<r xmlns:sling="s" xmlns:jcr="j">
            <checkbox_1 sling:resourceType="ubs/controls/checkbox" name="CB_RecordCalls"
                options="[1=Recording of telephone calls]"/>
            <checkbox_2 sling:resourceType="ubs/controls/checkbox" name="CB_Bare" options="[1=]"/>
        </r>"#;
        let (_, _, labels) = check_naming_conventions(xml);
        assert_eq!(labels.len(), 1, "only the label-less group is a defect: {labels:?}");
        assert_eq!(labels[0].name, "CB_Bare");
    }

    /// AAEJ shipped a radio button whose title was the parenthetical hint next
    /// to it — `(Mandatory entry only if NMS IDD hit is "True")` — instead of
    /// the question, which stayed behind in a separate static text.
    #[test]
    fn malformed_titles_are_classified() {
        let xml = r#"<r xmlns:sling="s" xmlns:jcr="j">
            <radiobutton_1 sling:resourceType="ubs/controls/radiobutton" name="RB_A1Q3"
                jcr:title="(Mandatory entry only if NMS IDD hit is &quot;True&quot;)" options="[1=Yes,2=No]"/>
            <textbox_1 sling:resourceType="ubs/controls/textbox" name="TXT_Rich"
                jcr:title="&lt;p&gt;Company&lt;/p&gt;"/>
            <textbox_2 sling:resourceType="ubs/controls/textbox" name="TXT_Quoted"
                jcr:title="Classification as &quot;Eligible Counterparty&quot; can be approved."/>
        </r>"#;
        let (_, _, labels) = check_naming_conventions(xml);
        let got: Vec<(&str, &str, &str)> = labels
            .iter()
            .map(|l| (l.name.as_str(), l.kind.as_str(), l.confidence.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("RB_A1Q3", "parenthetical", "high"),
                ("TXT_Rich", "markup", "high"),
                // quotes also occur in genuine reference-form labels.
                ("TXT_Quoted", "quoted", "low"),
            ]
        );
        assert!(
            labels[0].title.contains('"'),
            "the reported title is unescaped: {:?}",
            labels[0].title
        );
    }

    /// Draws and panels legitimately carry no title — only inputs are checked.
    #[test]
    fn non_input_components_are_not_label_checked() {
        let xml = r#"<r xmlns:sling="s" xmlns:jcr="j">
            <panel_1 sling:resourceType="ubs/controls/panel" name="PN_Section"/>
            <textdraw_1 sling:resourceType="ubs/controls/textdraw" name="ST_Question"/>
            <titledraw_1 sling:resourceType="ubs/controls/titledraw" name="TTL_Section"/>
        </r>"#;
        let (_, _, labels) = check_naming_conventions(xml);
        assert!(labels.is_empty(), "got {labels:?}");
    }

    #[test]
    fn review_output_runs_naming_on_rendered_xml() {
        // A convert → review round-trip renders and scans without error.
        let input = sample_input();
        let cfg = AemConfig::test_default("TEST");
        let aem = crate::convert_to_aem(&input, &cfg);
        let report = review_output(&input, &aem, &cfg, "en");
        // naming_violations is populated from the rendered XML (may be empty).
        let _ = &report.naming_violations;
    }
}


#[cfg(test)]
mod feedback_rule_tests {
    use super::check_feedback_rules;

    /// A form with the swept defects in it, one per node. Hand-built: the engine
    /// cannot emit these shapes any more, and a package loaded into the engine is
    /// repaired by the templates on the way out -- which is why this checks the
    /// checker directly, on the XML a reviewer would be looking at.
    const DEFECTIVE: &str = r##"<jcr:root>
  <panel_default sling:resourceType="fd/af/components/panel" dorExclusion="true"
      guideNodeClass="guidePanel" name="PN_Default"/>
  <checkbox_plain sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/checkbox"
      guideNodeClass="guideCheckBox" name="CB_Options" options="[1=One]"/>
  <titledraw_jtf sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/titledraw"
      _value="&lt;p>Heading&lt;/p>" headingLevel="2" jumpToFieldButtonVisible="true" name="TTL_Step"/>
  <fd:rules fd:visible="[{&quot;nodeName&quot;:&quot;ROOT&quot;}]" jcr:primaryType="nt:unstructured"/>
  <panel_internal sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
      fragRef="/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_internalbankuse_ouref"
      guideNodeClass="guidePanel" name="PN_FRG_InternalBankUseOnly"/>
  <panel_infobox_copy sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
      fragRef="/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_infobox"
      guideNodeClass="guidePanel" name="PN_ItalyInfoboxDoR" visible="{Boolean}false"/>
</jcr:root>"##;

    /// A form that satisfies every rule the checker knows.
    const CLEAN: &str = r##"<jcr:root>
  <panel_ubs sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
      dorExclusion="true" summaryExclusion="true" guideNodeClass="guidePanel" name="PN_Ok"/>
  <checkbox_ok sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/checkbox"
      guideNodeClass="guideCheckBox" name="CB_Ok" options="[1=One]" richTextOptions="true"/>
  <fd:rules jcr:primaryType="nt:unstructured"/>
  <panel_internal sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
      alwaysInPdf="true"
      fragRef="/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_internalbankuse_ouref"
      guideNodeClass="guidePanel" name="PN_FRG_InternalBankUseOnly" summaryExclusion="true"
      visible="{Boolean}false"/>
  <panel_infobox sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
      dorExclusion="true"
      fragRef="/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_infobox"
      guideNodeClass="guidePanel" name="PN_ItalyInfobox" summaryExclusion="true"/>
  <panel_infobox_copy sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
      alwaysInPdf="true"
      fragRef="/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_infobox"
      guideNodeClass="guidePanel" name="PN_ItalyInfoboxDoR" summaryExclusion="true"
      visible="{Boolean}false"/>
  <guidebutton sling:resourceType="fd/af/components/guidebutton" dorExclusion="true"
      summaryExclusion="true" guideNodeClass="guideButton" name="fwbSaveProgress"/>
</jcr:root>"##;

    fn rules_of(xml: &str) -> Vec<String> {
        let mut rules: Vec<String> = check_feedback_rules(xml)
            .into_iter()
            .map(|v| v.rule)
            .collect();
        rules.sort();
        rules.dedup();
        rules
    }

    #[test]
    fn every_swept_defect_is_reported() {
        let rules = rules_of(DEFECTIVE);
        for expected in [
            "PROBLEM-checkbox-rich-text-options",
            "PROBLEM-dor-exclusion-implies-summary",
            "PROBLEM-infobox-dor-copy",
            "PROBLEM-internal-bank-use-pdf-only",
            "PROBLEM-jump-to-field-button",
            "PROBLEM-nav-save-progress-required",
            "PROBLEM-panel-type-ubs",
            "PROBLEM-visual-editor-rules",
        ] {
            assert!(
                rules.iter().any(|r| r == expected),
                "{expected} was not reported; got {rules:?}"
            );
        }
    }

    #[test]
    fn a_conforming_form_reports_nothing() {
        assert_eq!(rules_of(CLEAN), Vec::<String>::new());
    }

    /// The defect is named on the node that carries it, so the reviewer can find
    /// it -- by the component's `name`, which is how these forms address a node.
    #[test]
    fn a_violation_names_the_node_it_is_on() {
        let violations = check_feedback_rules(DEFECTIVE);
        let internal = violations
            .iter()
            .find(|v| v.rule == "PROBLEM-internal-bank-use-pdf-only")
            .expect("the internal-bank-use violation");
        assert_eq!(internal.node, "PN_FRG_InternalBankUseOnly");
        assert!(
            internal.detail.contains("summaryExclusion")
                && internal.detail.contains("alwaysInPdf")
                && internal.detail.contains("visible"),
            "the detail must say what is missing: {}",
            internal.detail
        );
    }

    /// A rich-text `_value` carries a literal `>`, which a naive tag scan reads
    /// as the end of the tag -- and then misses the attributes behind it.
    #[test]
    fn a_rich_text_value_does_not_hide_the_rest_of_the_tag() {
        let xml = r##"<jcr:root>
  <textdraw sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/textdraw"
      _value="&lt;p>text with a &gt; and a &lt;b>bold&lt;/b> run&lt;/p>" name="ST_Rich"
      dorExclusion="true"/>
  <guidebutton name="fwbSaveProgress"/>
</jcr:root>"##;
        let rules = rules_of(xml);
        assert_eq!(
            rules,
            vec!["PROBLEM-dor-exclusion-implies-summary".to_string()],
            "the attribute after the rich text must still be seen"
        );
    }
}
