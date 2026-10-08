//! Shape corrections applied to a finished tree on the way out.
//!
//! Three of the swept feedback rules cannot be satisfied by a template alone:
//! they are about a node's POSITION among its siblings, or about a node that has
//! to exist twice in different roles. Doing them here rather than in the
//! converter means they hold for every tree the writer sees -- one built from an
//! XFA source, one the agent authored, and one loaded out of a deployed package
//! and edited -- which is the point: the feedback engine's output is judged by
//! the same detectors as the conversion engine's.
//!
//! - [`wrap_static_text`] -- PROBLEM-static-text-dor-excluded-step. A panel whose
//!   title is excluded from the DoR renders none of the static texts that are its
//!   DIRECT children there. Wrapping each run of them in a content panel is the
//!   owner's fix (2026-08-25, widened 2026-08-26 to every panel under a step).
//! - [`copy_infobox_into_the_dor`] -- PROBLEM-infobox-dor-copy. The Italy infobox
//!   shows on screen but must also print at the bottom of the last page, which
//!   takes a second, hidden reference to the same fragment.
//! - [`internal_bank_use_is_pdf_only`] -- PROBLEM-internal-bank-use-pdf-only. The
//!   "for internal bank use only" block is the bank's copy: never on screen,
//!   never on the summary, always in the PDF.
//!
//! Each pass is idempotent, and running the three in sequence is idempotent too:
//! they key on the state of the tree, not on what a previous run did.

use uuid::Uuid;

use super::{AemAttrs, AemNode};

/// The CSS class that marks the first page's subtitle. Such a draw is a heading,
/// not body text, so it is a run separator rather than a run member -- the same
/// exemption `static_text_wrap.py` makes.
const SUBTITLE_CSS: &str = "subtitle-after-form-title";

/// The CSS class the step-title template stamps on a step's own canonical
/// heading (`panel.xml`). It is synthesised entirely from the panel's `title`
/// field -- never a node in `children` -- so a `TitleDraw` carrying it can only
/// reach `is_run_member` from an agent-authored or loaded tree, never from this
/// engine's own conversion.
///
/// An ordinary `heading_level == 2` sub-heading is NOT this, and must join the
/// run like any other body text: reading every level-2 draw as the step title
/// is where PROBLEM-static-text-dor-excluded-step over-reached, and
/// PROBLEM-static-text-orphan-step (2026-09-02) narrows the exemption to this
/// class specifically.
const STEP_TITLE_CSS: &str = "stepTitle";

/// The Italy infobox fragment: the only infobox in any UBS fragment library.
const INFOBOX_FRAGMENT: &str = "affrg_italy_infobox";

/// The fragments that make up the internal-bank-use family, by the
/// `<library>/<fragment>` tail of their `fragRef`. Copied from
/// `internal_bank_use_pdf.py`'s `FAMILY`, which is the census of the deployed
/// corpus; a fragment missing here is simply left alone.
pub(crate) const INTERNAL_BANK_USE_FRAGMENTS: &[&str] = &[
    "afforms_germany_fragmentlib/affrg_germany_InternalBankUse",
    "afforms_germany_fragmentlib/affrg_germany_InternalBankUse_OURef_Signature",
    "afforms_italy_fragmentlib/affrg_italy_internalbankuse",
    "afforms_italy_fragmentlib/affrg_italy_internalbankuse_ouref",
    "afforms_italy_fragmentlib/affrg_italy_InternalBankUse_Text_OURef_Signature",
    "afforms_italy_fragmentlib/affrg_italy_internal_bank_use_cognome_luogo_data_visto",
    "afforms_global_fragmentlib/affrg_global_InternalBankUse_Text_OURef_Signature",
    "afforms_global_fragmentlib/affrg_global_spazio_riservato_al_soggetto_collocatore",
];

/// Run every shape correction, in the order their rules depend on: the infobox
/// copy is appended to the last page before the static-text pass walks it, so a
/// page that gains the copy is seen in its final shape.
pub fn normalize(root: &mut AemNode) {
    internal_bank_use_is_pdf_only(root);
    drop_captions_an_input_already_carries(root);
    copy_infobox_into_the_dor(root);
    wrap_static_text(root);
}

// ── A caption rendered twice ────────────────────────────────────────────────

/// Drop a static text that says exactly what the input beside it already says.
///
/// A tree the agent authored, or one loaded back out of a deployed package,
/// arrives here already shaped -- and
/// authoring a caption twice over, once as a draw and once as the field's own
/// title, is exactly what a model does when the source shows it the caption
/// twice. AABF_019 shipped `ST_Amount` reading "Amount:" next to a textbox
/// titled "Amount:", three times over.
///
/// Only an immediate sibling counts, and only an exact match after stripping
/// markup, so a draw that introduces a group of fields is left alone.
/// Idempotent: a second run finds nothing left to drop.
pub fn drop_captions_an_input_already_carries(root: &mut AemNode) {
    if let Some(children) = children_mut(root) {
        for child in children.iter_mut() {
            drop_captions_an_input_already_carries(child);
        }
    }
    let Some(children) = children_mut(root) else {
        return;
    };
    let mut i = 0;
    while i + 1 < children.len() {
        let dropped = match (&children[i], &children[i + 1]) {
            (AemNode::TextDraw { content, .. }, input) => {
                input_title(input).is_some_and(|t| captions_match(content, t))
            }
            _ => false,
        } || match (&children[i], &children[i + 1]) {
            (input, AemNode::TextDraw { content, .. }) => {
                input_title(input).is_some_and(|t| captions_match(content, t))
            }
            _ => false,
        };
        if !dropped {
            i += 1;
            continue;
        }
        let draw_at = if matches!(children[i], AemNode::TextDraw { .. }) {
            i
        } else {
            i + 1
        };
        children.remove(draw_at);
    }
}

/// The title an input renders, or `None` for anything that is not an input.
fn input_title(node: &AemNode) -> Option<&str> {
    match node {
        AemNode::TextField { label, .. }
        | AemNode::NumberField { label, .. }
        | AemNode::DatePicker { label, .. }
        | AemNode::Dropdown { label, .. }
        | AemNode::Checkbox { label, .. }
        | AemNode::RadioButton { label, .. } => Some(label),
        _ => None,
    }
}

/// Do a draw and a title read the same, once markup and whitespace are gone?
fn captions_match(draw: &str, title: &str) -> bool {
    let plain = |s: &str| {
        let mut out = String::new();
        let mut in_tag = false;
        for c in s.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => out.push(c),
                _ => {}
            }
        }
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    let (draw, title) = (plain(draw), plain(title));
    !draw.is_empty() && draw == title
}

// ── PROBLEM-static-text-dor-excluded-step ───────────────────────────────────

/// Wrap every run of adjacent static texts that sits directly under a panel
/// whose title is DoR-excluded -- which, since every wizard step carries
/// `dorExcludeTitle` itself, includes any static text left as a direct child
/// of a step: PROBLEM-static-text-orphan-step's "orphan" is this same shape,
/// named for what breaks in Redacto rather than in the DoR pipeline this
/// docstring first measured against.
///
/// The mechanism inside the DoR pipeline is not established -- what is
/// established is that the same texts render when they sit one panel deeper
/// (AAOV `PN_Dichiarazione`, whose whole declaration was missing from the DoR,
/// against `PN_Info_Last`, which renders). A run is broken by anything that is
/// not a static text, and by the two draws that are headings rather than body:
/// the step's own canonical title (`css="stepTitle"`) and the first page's
/// subtitle. An ordinary heading is not either of those, whatever level it
/// draws at, and joins the run like any other body text.
pub fn wrap_static_text(root: &mut AemNode) {
    walk_panels(root, &mut |parent, children, excludes_title| {
        if !excludes_title {
            return;
        }
        let mut out: Vec<AemNode> = Vec::with_capacity(children.len());
        let mut run: Vec<AemNode> = Vec::new();
        for node in std::mem::take(children) {
            if is_run_member(&node) {
                run.push(node);
            } else {
                flush_run(parent, &mut run, &mut out);
                out.push(node);
            }
        }
        flush_run(parent, &mut run, &mut out);
        *children = out;
    });
}

/// A static text that may join a run: body text, not a heading.
fn is_run_member(node: &AemNode) -> bool {
    match node {
        AemNode::TextDraw { attrs, .. } => !has_class(attrs, SUBTITLE_CSS),
        AemNode::TitleDraw { attrs, .. } => {
            !has_class(attrs, STEP_TITLE_CSS) && !has_class(attrs, SUBTITLE_CSS)
        }
        // The HTML component is static content too, but it is a BLOCK: a table,
        // a chart, an image. It stays out of the run, which is exactly how the
        // `TBL_` panel it replaces behaved, so no existing form's wrapping
        // shifts. Whether the DoR defect `wrap_static_text` exists for also
        // afflicts this component is a question for a deployed form.
        AemNode::HtmlDisplayer { .. } => false,
        _ => false,
    }
}

fn has_class(attrs: &AemAttrs, class: &str) -> bool {
    attrs
        .css
        .as_deref()
        .is_some_and(|css| css.split_whitespace().any(|c| c == class))
}

/// Move a finished run into a content panel of its own. A run in which every
/// draw is already DoR-excluded needs no wrapper: nothing in it was going to
/// render there anyway.
fn flush_run(parent: &str, run: &mut Vec<AemNode>, out: &mut Vec<AemNode>) {
    if run.is_empty() {
        return;
    }
    let drawn = run
        .iter()
        .any(|n| !n.attrs().is_some_and(|a| a.dor_exclude));
    if !drawn {
        out.append(run);
        return;
    }
    let colspan = run.iter().filter_map(colspan_of).max().unwrap_or(12);
    // Derived from the panel AND the run's first draw: two panels can each hold a
    // run starting with a draw of the same name, and a shared uuid would be two
    // different nodes claiming one JCR identity.
    let first = run.first().and_then(AemNode::name).unwrap_or("");
    let seed = format!("{parent}/{first}");
    let name = format!(
        "PN_StaticText_{}",
        &Uuid::new_v5(&Uuid::NAMESPACE_OID, seed.as_bytes())
            .as_simple()
            .to_string()[..8]
    );
    let uuid = Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes());
    out.push(AemNode::Panel {
        uuid,
        name,
        title: String::new(),
        children: std::mem::take(run),
        is_page: false,
        attrs: AemAttrs::default(),
        visible: true,
        is_conditional: false,
        dor_num_cols: None,
        colspan,
        dor_colspan: None,
        bind_ref: None,
        frag_ref: None,
    });
}

// ── PROBLEM-infobox-dor-copy ────────────────────────────────────────────────

/// Keep the on-screen infobox out of the DoR and put a hidden copy of it at the
/// end of the last page, which is where the finished document prints it.
///
/// The copy is not merely `visible: false`: the summary builder drops a hidden
/// node before Redacto sees it, so without `always_in_pdf` the copy renders
/// nowhere -- the mistake the first rollout of this rule shipped 28 times.
pub fn copy_infobox_into_the_dor(root: &mut AemNode) {
    // Nothing is changed until it is clear the copy can be placed: excluding the
    // on-screen infobox from the DoR without putting a copy in the document is
    // strictly worse than leaving both alone -- the text would then reach nobody.
    if last_page(root).is_none() {
        return;
    }

    let mut on_screen: Option<String> = None;
    let mut copy_exists = false;
    visit(root, &mut |node| {
        if let AemNode::Fragment {
            frag_ref,
            attrs,
            visible,
            ..
        } = node
        {
            if !frag_ref.ends_with(INFOBOX_FRAGMENT) {
                return;
            }
            if *visible {
                attrs.dor_exclude = true;
                attrs.summary_exclude = true;
                on_screen = Some(frag_ref.clone());
            } else {
                copy_exists = true;
            }
        }
    });

    let Some(frag_ref) = on_screen else { return };
    if copy_exists {
        return;
    }
    let name = "PN_ItalyInfoboxDoR".to_string();
    let uuid = Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes());
    let copy = AemNode::Fragment {
        uuid,
        name,
        title: String::new(),
        frag_ref,
        attrs: AemAttrs::pdf_only(),
        visible: false,
        bind_ref: None,
        init_hide: Vec::new(),
    };
    // `last_page` was checked above, so this is the page the copy belongs on.
    if let Some(AemNode::Panel { children, .. }) = last_page(root) {
        children.push(copy);
    }
}

// ── PROBLEM-internal-bank-use-pdf-only ──────────────────────────────────────

/// The internal-bank-use block reaches the reader through the PDF alone: never
/// on screen, never on the summary step. `dor_exclude` must stay off -- it would
/// undo `always_in_pdf` on the Redacto path.
pub fn internal_bank_use_is_pdf_only(root: &mut AemNode) {
    visit(root, &mut |node| {
        if let AemNode::Fragment {
            frag_ref,
            attrs,
            visible,
            ..
        } = node
        {
            if !is_internal_bank_use(frag_ref) {
                return;
            }
            attrs.summary_exclude = true;
            attrs.always_in_pdf = true;
            attrs.dor_exclude = false;
            *visible = false;
        }
    });
}

/// Whether a `fragRef` names one of the internal-bank-use fragments.
///
/// Public because a host's check of the rendered package asks the same
/// question of the XML, and two readings of "is this the family" would drift.
pub fn is_internal_bank_use(frag_ref: &str) -> bool {
    let mut parts = frag_ref.rsplit('/');
    let (Some(fragment), Some(library)) = (parts.next(), parts.next()) else {
        return false;
    };
    let tail = format!("{library}/{fragment}");
    INTERNAL_BANK_USE_FRAGMENTS.contains(&tail.as_str())
}

// ── walking ─────────────────────────────────────────────────────────────────

/// Every node in the tree, depth first, node before children.
fn visit(node: &mut AemNode, f: &mut impl FnMut(&mut AemNode)) {
    f(node);
    if let Some(children) = children_mut(node) {
        for child in children {
            visit(child, f);
        }
    }
}

/// Call `f(name, children, excludes_title)` for every container, deepest FIRST
/// (the callback runs after the recursion), where `excludes_title` says whether
/// the container's own title is kept out of the DoR -- the condition the
/// static-text rule keys on -- and `name` identifies it, so a node the callback
/// creates can be named after the place it was created in. A container that is
/// itself DoR-excluded is not walked: nothing below it reaches the DoR.
fn walk_panels(node: &mut AemNode, f: &mut impl FnMut(&str, &mut Vec<AemNode>, bool)) {
    if node.attrs().is_some_and(|a| a.dor_exclude) {
        return;
    }
    // A page and a conditional panel carry `dorExcludeTitle` from their template,
    // and so does a repeatable's row panel; an ordinary panel says so itself.
    let excludes_title = match node {
        AemNode::Panel {
            is_page,
            is_conditional,
            attrs,
            ..
        } => *is_page || *is_conditional || attrs.dor_exclude_title,
        AemNode::Repeatable { .. } => true,
        _ => false,
    };
    let name = node.name().unwrap_or("").to_string();
    if let Some(children) = children_mut(node) {
        for child in children.iter_mut() {
            walk_panels(child, f);
        }
    }
    if let Some(children) = children_mut(node) {
        f(&name, children, excludes_title);
    }
}

fn children_mut(node: &mut AemNode) -> Option<&mut Vec<AemNode>> {
    match node {
        AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } => Some(children),
        _ => None,
    }
}

/// The last content page: the last direct child of the Root that is a page.
fn last_page(root: &mut AemNode) -> Option<&mut AemNode> {
    let AemNode::Root { children, .. } = root else {
        return None;
    };
    children
        .iter_mut()
        .rfind(|c| matches!(c, AemNode::Panel { is_page: true, .. }))
}

fn colspan_of(node: &AemNode) -> Option<u32> {
    match node {
        AemNode::TextDraw { colspan, .. } | AemNode::TitleDraw { colspan, .. } => Some(*colspan),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw(name: &str) -> AemNode {
        AemNode::TextDraw {
            uuid: Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes()),
            name: name.into(),
            content: format!("<p>{name}</p>"),
            attrs: AemAttrs::default(),
            visible: true,
            colspan: 12,
            dor_colspan: None,
        }
    }

    fn title(name: &str, level: u8) -> AemNode {
        AemNode::TitleDraw {
            uuid: Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes()),
            name: name.into(),
            content: format!("<p>{name}</p>"),
            heading_level: level,
            attrs: AemAttrs::default(),
            visible: true,
            colspan: 12,
            dor_colspan: None,
        }
    }

    fn field(name: &str) -> AemNode {
        AemNode::TextField {
            uuid: Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes()),
            name: name.into(),
            label: name.into(),
            mandatory: false,
            visible: true,
            attrs: AemAttrs::default(),
            max_chars: None,
            colspan: 12,
            dor_colspan: None,
            bind_ref: None,
            kind: Default::default(),
        }
    }

    fn fragment(name: &str, frag_ref: &str, visible: bool) -> AemNode {
        AemNode::Fragment {
            uuid: Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes()),
            name: name.into(),
            title: String::new(),
            frag_ref: frag_ref.into(),
            attrs: AemAttrs::default(),
            visible,
            bind_ref: None,
            init_hide: Vec::new(),
        }
    }

    fn page(name: &str, children: Vec<AemNode>) -> AemNode {
        AemNode::Panel {
            uuid: Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes()),
            name: name.into(),
            title: name.into(),
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
        }
    }

    fn root(children: Vec<AemNode>) -> AemNode {
        AemNode::Root {
            title: "Form".into(),
            children,
        }
    }

    /// The names of a container's children, for compact assertions.
    fn child_names(node: &AemNode) -> Vec<String> {
        match node {
            AemNode::Root { children, .. }
            | AemNode::Panel { children, .. }
            | AemNode::Repeatable { children, .. } => children
                .iter()
                .map(|c| c.name().unwrap_or("?").to_string())
                .collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn adjacent_static_texts_under_a_page_are_wrapped_together() {
        let mut tree = root(vec![page(
            "PN_Declaration",
            vec![
                draw("ST_One"),
                draw("ST_Two"),
                field("TXT_Name"),
                draw("ST_Three"),
            ],
        )]);
        wrap_static_text(&mut tree);

        let AemNode::Root { children, .. } = &tree else {
            unreachable!()
        };
        let names = child_names(&children[0]);
        assert_eq!(names.len(), 3, "two runs and the field: {names:?}");
        assert!(names[0].starts_with("PN_StaticText_"), "{names:?}");
        assert_eq!(names[1], "TXT_Name");
        assert!(names[2].starts_with("PN_StaticText_"), "{names:?}");
        assert_ne!(names[0], names[2], "each run gets a name of its own");

        let AemNode::Panel {
            children: page_children,
            ..
        } = &children[0]
        else {
            unreachable!()
        };
        assert_eq!(child_names(&page_children[0]), ["ST_One", "ST_Two"]);
        assert_eq!(child_names(&page_children[2]), ["ST_Three"]);
    }

    /// A titledraw carrying a class breaks a run rather than joining it, but
    /// only the two classes that mean something: the step's own canonical title
    /// (`css="stepTitle"`) and the first page's subtitle. Both are headings, not
    /// body text, and are left where they are.
    #[test]
    fn the_two_marked_headings_separate_runs_and_are_never_wrapped() {
        let with_css = |node: AemNode, css: &str| match node {
            AemNode::TextDraw {
                uuid,
                name,
                content,
                visible,
                colspan,
                dor_colspan,
                ..
            } => AemNode::TextDraw {
                uuid,
                name,
                content,
                attrs: AemAttrs {
                    css: Some(css.into()),
                    ..AemAttrs::default()
                },
                visible,
                colspan,
                dor_colspan,
            },
            AemNode::TitleDraw {
                uuid,
                name,
                content,
                heading_level,
                visible,
                colspan,
                dor_colspan,
                ..
            } => AemNode::TitleDraw {
                uuid,
                name,
                content,
                heading_level,
                attrs: AemAttrs {
                    css: Some(css.into()),
                    ..AemAttrs::default()
                },
                visible,
                colspan,
                dor_colspan,
            },
            other => other,
        };
        let subtitle = with_css(draw("ST_Subtitle"), SUBTITLE_CSS);
        let step_title = with_css(title("TTL_Step", 2), STEP_TITLE_CSS);
        let mut tree = root(vec![page(
            "PN_First",
            vec![subtitle, draw("ST_Body"), step_title, draw("ST_After")],
        )]);
        wrap_static_text(&mut tree);

        let AemNode::Root { children, .. } = &tree else {
            unreachable!()
        };
        let names = child_names(&children[0]);
        assert_eq!(names[0], "ST_Subtitle", "the subtitle stays put: {names:?}");
        assert!(names[1].starts_with("PN_StaticText_"), "{names:?}");
        assert_eq!(names[2], "TTL_Step", "the step title stays put: {names:?}");
        assert!(names[3].starts_with("PN_StaticText_"), "{names:?}");
    }

    /// An h2 with no `stepTitle` class is an ordinary sub-heading, not the
    /// step's own title -- and this engine never puts the real one in
    /// `children` at all; the template synthesises it from the panel's own
    /// `title` field. So any level-2 draw reaching this code is agent-authored
    /// content, and joins the run like any other body text
    /// (PROBLEM-static-text-orphan-step, 2026-09-02): reading every h2 as a step
    /// title is exactly what PROBLEM-static-text-dor-excluded-step over-read.
    #[test]
    fn an_unmarked_h2_is_ordinary_content_and_joins_the_run() {
        let mut tree = root(vec![page(
            "PN_First",
            vec![draw("ST_Before"), title("TTL_Sub", 2), draw("ST_After")],
        )]);
        wrap_static_text(&mut tree);

        let AemNode::Root { children, .. } = &tree else {
            unreachable!()
        };
        let names = child_names(&children[0]);
        assert_eq!(names.len(), 1, "one run, the heading included: {names:?}");
        assert!(names[0].starts_with("PN_StaticText_"), "{names:?}");
        let AemNode::Panel { children: run, .. } = &children[0] else {
            unreachable!()
        };
        assert_eq!(
            child_names(&run[0]),
            ["ST_Before", "TTL_Sub", "ST_After"],
            "the unmarked heading must sit inside the run, not break it"
        );
    }

    /// A run of draws that are all excluded from the DoR anyway needs no wrapper,
    /// and neither does a panel that keeps its own title in the DoR.
    #[test]
    fn a_run_is_left_alone_when_wrapping_would_change_nothing() {
        let excluded = match draw("ST_Hidden") {
            AemNode::TextDraw {
                uuid,
                name,
                content,
                visible,
                colspan,
                dor_colspan,
                ..
            } => AemNode::TextDraw {
                uuid,
                name,
                content,
                attrs: AemAttrs::dor_excluded(),
                visible,
                colspan,
                dor_colspan,
            },
            _ => unreachable!(),
        };
        let mut tree = root(vec![page("PN_Page", vec![excluded])]);
        wrap_static_text(&mut tree);
        let AemNode::Root { children, .. } = &tree else {
            unreachable!()
        };
        assert_eq!(child_names(&children[0]), ["ST_Hidden"]);

        // A plain content panel below a page keeps its title in the DoR, so its
        // direct static texts render and are left as they are.
        let inner = AemNode::Panel {
            uuid: Uuid::nil(),
            name: "PN_Inner".into(),
            title: "Inner".into(),
            children: vec![draw("ST_Body")],
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
        let mut tree = root(vec![page("PN_Page", vec![inner])]);
        wrap_static_text(&mut tree);
        let AemNode::Root { children, .. } = &tree else {
            unreachable!()
        };
        let AemNode::Panel {
            children: page_children,
            ..
        } = &children[0]
        else {
            unreachable!()
        };
        assert_eq!(child_names(&page_children[0]), ["ST_Body"]);
    }

    /// Two panels can each hold a run that starts with a draw of the same name.
    /// The wrappers must still be two different nodes: a shared uuid is two nodes
    /// claiming one JCR identity.
    #[test]
    fn two_runs_with_the_same_first_draw_get_different_wrappers() {
        let mut tree = root(vec![
            page("PN_One", vec![draw("ST_Intro"), draw("ST_Body")]),
            page("PN_Two", vec![draw("ST_Intro"), draw("ST_Body")]),
        ]);
        wrap_static_text(&mut tree);

        let AemNode::Root { children, .. } = &tree else {
            unreachable!()
        };
        let wrappers: Vec<_> = children
            .iter()
            .map(|p| match p {
                AemNode::Panel { children, .. } => match &children[0] {
                    AemNode::Panel { name, uuid, .. } => (name.clone(), *uuid),
                    other => panic!("expected a wrapper, got {other:?}"),
                },
                other => panic!("expected a page, got {other:?}"),
            })
            .collect();
        assert_ne!(wrappers[0].0, wrappers[1].0, "the names must differ");
        assert_ne!(wrappers[0].1, wrappers[1].1, "the uuids must differ");
    }

    /// A form whose pages are not direct children of the Root has nowhere to put
    /// the copy. Excluding the on-screen infobox anyway would leave the text
    /// reaching nobody, so neither half is applied.
    #[test]
    fn the_infobox_is_left_alone_when_the_copy_has_nowhere_to_go() {
        let path = "/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_infobox";
        let mut tree = root(vec![fragment("PN_ItalyInfobox", path, true)]);
        let before = serde_json::to_value(&tree).unwrap();
        copy_infobox_into_the_dor(&mut tree);
        assert_eq!(
            before,
            serde_json::to_value(&tree).unwrap(),
            "the on-screen infobox must not be excluded without a copy to carry it"
        );
    }

    #[test]
    fn wrapping_static_text_is_idempotent() {
        let mut once = root(vec![page("PN_Page", vec![draw("ST_One"), draw("ST_Two")])]);
        wrap_static_text(&mut once);
        let mut twice = once.clone();
        wrap_static_text(&mut twice);
        assert_eq!(
            serde_json::to_value(&once).unwrap(),
            serde_json::to_value(&twice).unwrap(),
            "a second pass must change nothing"
        );
    }

    #[test]
    fn the_infobox_is_excluded_on_screen_and_copied_into_the_dor() {
        let path = "/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_infobox";
        let mut tree = root(vec![
            page("PN_First", vec![fragment("PN_ItalyInfobox", path, true)]),
            page("PN_Last", vec![draw("ST_Closing")]),
        ]);
        copy_infobox_into_the_dor(&mut tree);

        let AemNode::Root { children, .. } = &tree else {
            unreachable!()
        };
        let AemNode::Panel {
            children: first, ..
        } = &children[0]
        else {
            unreachable!()
        };
        let AemNode::Fragment { attrs, .. } = &first[0] else {
            unreachable!()
        };
        assert!(
            attrs.dor_exclude && attrs.summary_exclude,
            "the on-screen infobox must not print where it sits"
        );

        let AemNode::Panel { children: last, .. } = &children[1] else {
            unreachable!()
        };
        let AemNode::Fragment {
            attrs,
            visible,
            name,
            ..
        } = last.last().unwrap()
        else {
            panic!(
                "the copy is the last child of the last page, got {:?}",
                child_names(&children[1])
            )
        };
        assert_eq!(name, "PN_ItalyInfoboxDoR");
        assert!(!visible, "the copy never shows on screen");
        assert!(
            attrs.always_in_pdf && attrs.summary_exclude && !attrs.dor_exclude,
            "a hidden copy without alwaysInPdf renders nowhere -- the mistake the \
             first rollout of this rule shipped"
        );
    }

    #[test]
    fn the_infobox_copy_is_made_once() {
        let path = "/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_infobox";
        let mut tree = root(vec![
            page("PN_First", vec![fragment("PN_ItalyInfobox", path, true)]),
            page("PN_Last", vec![draw("ST_Closing")]),
        ]);
        copy_infobox_into_the_dor(&mut tree);
        let after_one = serde_json::to_value(&tree).unwrap();
        copy_infobox_into_the_dor(&mut tree);
        assert_eq!(after_one, serde_json::to_value(&tree).unwrap());
    }

    /// A form with no infobox is not given one.
    #[test]
    fn a_form_without_an_infobox_gains_nothing() {
        let mut tree = root(vec![page("PN_Only", vec![draw("ST_Body")])]);
        let before = serde_json::to_value(&tree).unwrap();
        copy_infobox_into_the_dor(&mut tree);
        assert_eq!(before, serde_json::to_value(&tree).unwrap());
    }

    #[test]
    fn the_internal_bank_use_block_reaches_the_pdf_alone() {
        let path = "/content/dam/formsanddocuments/afforms_italy_fragmentlib/\
                    affrg_italy_internalbankuse_ouref";
        let mut tree = root(vec![page(
            "PN_Signature",
            vec![fragment("PN_FRG_InternalBankUseOnly", path, true)],
        )]);
        internal_bank_use_is_pdf_only(&mut tree);

        let AemNode::Root { children, .. } = &tree else {
            unreachable!()
        };
        let AemNode::Panel {
            children: page_children,
            ..
        } = &children[0]
        else {
            unreachable!()
        };
        let AemNode::Fragment { attrs, visible, .. } = &page_children[0] else {
            unreachable!()
        };
        assert!(!visible, "never on screen");
        assert!(attrs.summary_exclude, "never on the summary step");
        assert!(attrs.always_in_pdf, "always in the PDF");
        assert!(
            !attrs.dor_exclude,
            "dorExclusion would undo alwaysInPdf on the Redacto path"
        );
    }

    /// A draw that repeats the input's own title is dropped, whichever side of
    /// it it sits on.
    #[test]
    fn a_caption_the_input_already_carries_is_dropped() {
        for children in [
            vec![draw("Amount:"), field("Amount:")],
            vec![field("Amount:"), draw("Amount:")],
        ] {
            let mut tree = root(vec![page("PN_Order", children)]);
            drop_captions_an_input_already_carries(&mut tree);
            let json = serde_json::to_string(&tree).unwrap();
            assert!(
                !json.contains("TextDraw"),
                "the duplicated draw must be gone: {json}"
            );
            assert!(
                json.contains("\"label\":\"Amount:\""),
                "the input keeps its own title: {json}"
            );
        }
    }

    /// A draw that says something else is left alone, and so is one that
    /// introduces a group rather than captioning the field next to it.
    #[test]
    fn an_unrelated_draw_is_not_dropped() {
        let mut tree = root(vec![page(
            "PN_Order",
            vec![draw("All costs to be borne by the:"), field("Amount:")],
        )]);
        let before = serde_json::to_value(&tree).unwrap();
        drop_captions_an_input_already_carries(&mut tree);
        assert_eq!(before, serde_json::to_value(&tree).unwrap());
    }

    /// Running it twice changes nothing the first run did not already do.
    #[test]
    fn dropping_captions_is_idempotent() {
        let mut tree = root(vec![page(
            "PN_Order",
            vec![draw("Amount:"), field("Amount:"), draw("Amount:")],
        )]);
        drop_captions_an_input_already_carries(&mut tree);
        let once = serde_json::to_value(&tree).unwrap();
        drop_captions_an_input_already_carries(&mut tree);
        assert_eq!(once, serde_json::to_value(&tree).unwrap());
    }

    /// Any other fragment is left exactly as it is.
    #[test]
    fn an_unrelated_fragment_is_not_touched() {
        let mut tree = root(vec![page(
            "PN_Address",
            vec![fragment(
                "PN_Address",
                "/content/dam/formsanddocuments/afforms_italy_fragmentlib/\
                 affrg_italy_AddressBlock_CountryDD",
                true,
            )],
        )]);
        let before = serde_json::to_value(&tree).unwrap();
        internal_bank_use_is_pdf_only(&mut tree);
        assert_eq!(before, serde_json::to_value(&tree).unwrap());
    }
}
