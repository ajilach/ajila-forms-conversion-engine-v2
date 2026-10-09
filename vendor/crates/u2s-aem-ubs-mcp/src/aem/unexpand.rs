//! The inverse of the writer's expansions, applied to a parsed
//! package on the way into a document.
//!
//! The parser reads a package as the writer wrote it, so every node the
//! writer expands comes back expanded: a page's step title as a child panel,
//! a repeatable as three nested panels, the preface as its banking-relationship
//! fragment, each fragment with its library content inlined, and the root's
//! fixed form metadata and summary as content. Encoding such a tree expands
//! all of it a second time. These passes fold each expansion back into the node it came
//! from, recognising it by the constants the writer writes (fixed names,
//! fragment paths, the repeat panels' derived names), never by uuid: the parser
//! mints its own.
//!
//! Each pass is the counterpart of one expansion in `super::lower`.

use super::xml_writer::{repeat_panel_name, repeat_row_name};
use super::{AemAttrs, AemNode};

/// The form metadata fragment the form chrome writes into every form.
const FORM_METADATA: &str = "FormMetadata";
/// The summary the form chrome writes when the profile turns the summary on.
const SUMMARY_PANEL: &str = "summaryPanel";
/// The preface's wrapper and fragment, as the preface lowering writes them.
const PREFACE_PANEL: &str = "PN_BR";
const BANKING_RELATIONSHIP_FRAGMENT: &str = "affrg_BankingRelationship1";
/// The step title's class, and the first page's subtitle class, as the page
/// lowering writes them.
const STEP_TITLE_CSS: &str = "stepTitle";
const SUBTITLE_CSS: &str = "subtitle-after-form-title";

/// Fold the profile's expansions in `root` back into the nodes they came from.
pub fn unexpand(root: AemNode) -> AemNode {
    match root {
        AemNode::Root { title, children } => AemNode::Root {
            title,
            children: children
                .into_iter()
                .filter(|c| !matches!(c.name(), Some(FORM_METADATA | SUMMARY_PANEL)))
                .map(|c| page(fold(c)))
                .collect(),
        },
        other => fold(other),
    }
}

/// A panel directly under the root is a wizard step: undo the page lowering's
/// step title, which it writes as the page's first child.
fn page(node: AemNode) -> AemNode {
    let AemNode::Panel {
        uuid,
        name,
        title,
        mut children,
        attrs,
        visible,
        is_conditional,
        dor_num_cols,
        colspan,
        dor_colspan,
        bind_ref,
        frag_ref,
        ..
    } = node
    else {
        return node;
    };
    let step_title = children
        .first()
        .and_then(|first| step_title_of(first, &name));
    let title = match step_title {
        Some(step_title) => {
            children.remove(0);
            step_title
        }
        None => title,
    };
    AemNode::Panel {
        uuid,
        name,
        title,
        children,
        is_page: true,
        attrs,
        visible,
        is_conditional,
        dor_num_cols,
        colspan,
        dor_colspan,
        bind_ref,
        frag_ref,
    }
}

/// The page title, if `node` is the step-title panel the page lowering writes
/// for the page named `page`: `{page}Title`, holding just the title draw.
fn step_title_of(node: &AemNode, page: &str) -> Option<String> {
    let AemNode::Panel { name, children, .. } = node else {
        return None;
    };
    if *name != format!("{page}Title") {
        return None;
    }
    let content = match children.as_slice() {
        [AemNode::TitleDraw { content, attrs, .. }] if has_class(attrs, STEP_TITLE_CSS) => content,
        [AemNode::TextDraw { content, attrs, .. }] if has_class(attrs, SUBTITLE_CSS) => content,
        _ => return None,
    };
    let content = content.trim_end();
    Some(
        content
            .strip_prefix("<p>")
            .and_then(|c| c.strip_suffix("</p>"))
            .unwrap_or(content)
            .to_string(),
    )
}

fn has_class(attrs: &AemAttrs, class: &str) -> bool {
    attrs
        .css
        .as_deref()
        .is_some_and(|css| css.split_whitespace().any(|c| c == class))
}

/// Fold one subtree, outermost expansion first: a recognised node is replaced
/// whole, so nothing inside it is folded twice.
fn fold(node: AemNode) -> AemNode {
    match node {
        AemNode::Panel {
            uuid,
            name,
            children,
            ..
        } if name == PREFACE_PANEL && children.iter().any(is_banking_relationship) => {
            AemNode::Preface { uuid, name }
        }
        // An inlined fragment: the writer references the fragment and never
        // writes its content, so the content goes and the reference stays.
        AemNode::Panel {
            uuid,
            name,
            title,
            attrs,
            visible,
            bind_ref,
            frag_ref: Some(frag_ref),
            ..
        } => AemNode::Fragment {
            uuid,
            name,
            title,
            frag_ref,
            attrs,
            visible,
            bind_ref,
            // Read back from the package's Initialize rule after decoding.
            init_hide: Vec::new(),
            init_show: Vec::new(),
        },
        AemNode::Panel { .. } => match repeatable(&node) {
            Some(repeatable) => fold_children(repeatable),
            None => fold_children(node),
        },
        other => fold_children(other),
    }
}

/// The preface's banking-relationship fragment, inlined or not.
fn is_banking_relationship(node: &AemNode) -> bool {
    let frag_ref = match node {
        AemNode::Panel {
            frag_ref: Some(frag_ref),
            ..
        }
        | AemNode::Fragment { frag_ref, .. } => frag_ref,
        _ => return false,
    };
    frag_ref.ends_with(BANKING_RELATIONSHIP_FRAGMENT)
}

fn fold_children(node: AemNode) -> AemNode {
    let fold_all = |children: Vec<AemNode>| children.into_iter().map(fold).collect();
    match node {
        AemNode::Panel {
            uuid,
            name,
            title,
            children,
            is_page,
            attrs,
            visible,
            is_conditional,
            dor_num_cols,
            colspan,
            dor_colspan,
            bind_ref,
            frag_ref,
        } => AemNode::Panel {
            uuid,
            name,
            title,
            children: fold_all(children),
            is_page,
            attrs,
            visible,
            is_conditional,
            dor_num_cols,
            colspan,
            dor_colspan,
            bind_ref,
            frag_ref,
        },
        AemNode::Repeatable {
            uuid,
            name,
            title,
            children,
            min_occur,
            max_occur,
            attrs,
            visible,
            bind_ref,
            frag_ref,
        } => AemNode::Repeatable {
            uuid,
            name,
            title,
            children: fold_all(children),
            min_occur,
            max_occur,
            attrs,
            visible,
            bind_ref,
            frag_ref,
        },
        other => other,
    }
}

/// The repeatable lowering writes a repeatable `X` as a panel `X`, holding the
/// instance-managed panel `RCP_X_repeat` (which carries the occurrences, the
/// subject and the `bindRef`), holding the row `RCP_X_inner` with the content.
/// Fold that back into one repeatable, if `panel` is one.
fn repeatable(panel: &AemNode) -> Option<AemNode> {
    let AemNode::Panel {
        uuid,
        name,
        children,
        attrs,
        visible,
        ..
    } = panel
    else {
        return None;
    };
    let [
        AemNode::Repeatable {
            name: repeat,
            title,
            children: rows,
            min_occur,
            max_occur,
            bind_ref,
            ..
        },
    ] = children.as_slice()
    else {
        return None;
    };
    let [
        AemNode::Panel {
            name: row,
            children: content,
            ..
        },
    ] = rows.as_slice()
    else {
        return None;
    };
    if *repeat != repeat_panel_name(name) || *row != repeat_row_name(name) {
        return None;
    }
    Some(AemNode::Repeatable {
        uuid: *uuid,
        name: name.clone(),
        title: title.clone(),
        children: content.clone(),
        min_occur: *min_occur,
        max_occur: *max_occur,
        attrs: attrs.clone(),
        visible: *visible,
        bind_ref: bind_ref.clone(),
        frag_ref: None,
    })
}
