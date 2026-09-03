use std::collections::HashMap;

use crate::structured::{
    ConditionalNode, FieldNode, FieldType, FootnoteNode, GridLayout, GridLayoutElement, GroupNode,
    HeadingNode, HtmlNode, InlineNode, InlineText, ListItem, ListNode, NameValue, NoticeNode,
    ParagraphNode,
    RepeatableNode, StructuredNode, TableHeader, TableNode, TableRow, TranslatableString,
    TranslatedText, TranslationMap,
};

/// Compute the LCS (longest common subsequence) table for two node slices,
/// using the given equality predicate.
pub(crate) fn lcs_table_with<F>(
    a: &[StructuredNode],
    b: &[StructuredNode],
    eq: F,
) -> Vec<Vec<usize>>
where
    F: Fn(&StructuredNode, &StructuredNode) -> bool,
{
    let m = a.len();
    let n = b.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];

    for i in 1..=m {
        for j in 1..=n {
            if eq(&a[i - 1], &b[j - 1]) {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    dp
}

/// Backtrack through the LCS table to produce aligned pairs.
/// Returns a list of (Option<idx_in_a>, Option<idx_in_b>) pairs.
/// Both Some -> matched pair. Only one Some -> unmatched node from that side.
pub(crate) fn lcs_align_with<F>(
    a: &[StructuredNode],
    b: &[StructuredNode],
    dp: &[Vec<usize>],
    eq: F,
) -> Vec<(Option<usize>, Option<usize>)>
where
    F: Fn(&StructuredNode, &StructuredNode) -> bool,
{
    let mut i = a.len();
    let mut j = b.len();

    let mut matches = Vec::new();
    while i > 0 && j > 0 {
        if eq(&a[i - 1], &b[j - 1]) {
            matches.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    matches.reverse();

    expand_alignment_matches(a.len(), b.len(), &matches)
}

/// Expand matched index pairs into a full alignment vector, inserting
/// unmatched left/right entries between and after matches.
fn expand_alignment_matches(
    a_len: usize,
    b_len: usize,
    matches: &[(usize, usize)],
) -> Vec<(Option<usize>, Option<usize>)> {
    let mut result = Vec::new();
    let mut ai = 0;
    let mut bi = 0;

    for (ma, mb) in matches {
        while ai < *ma {
            result.push((Some(ai), None));
            ai += 1;
        }
        while bi < *mb {
            result.push((None, Some(bi)));
            bi += 1;
        }
        result.push((Some(*ma), Some(*mb)));
        ai = ma + 1;
        bi = mb + 1;
    }

    while ai < a_len {
        result.push((Some(ai), None));
        ai += 1;
    }
    while bi < b_len {
        result.push((None, Some(bi)));
        bi += 1;
    }

    result
}

// ============================================================================
// Weighted LCS for semantic-aware alignment
// ============================================================================

/// Compute a weighted LCS table where each match contributes a score (0.0–1.0)
/// instead of a fixed +1.
#[cfg(feature = "semantic-matching")]
fn lcs_table_weighted<F>(a: &[StructuredNode], b: &[StructuredNode], score: F) -> Vec<Vec<f64>>
where
    F: Fn(usize, usize) -> f64,
{
    let m = a.len();
    let n = b.len();
    let mut dp = vec![vec![0.0f64; n + 1]; m + 1];

    for i in 1..=m {
        for j in 1..=n {
            let s = score(i - 1, j - 1);
            if s > 0.0 {
                dp[i][j] = (dp[i - 1][j - 1] + s).max(dp[i - 1][j]).max(dp[i][j - 1]);
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }

    dp
}

/// Backtrack through a weighted LCS table to produce aligned pairs.
#[cfg(feature = "semantic-matching")]
fn lcs_align_weighted<F>(
    a: &[StructuredNode],
    b: &[StructuredNode],
    dp: &[Vec<f64>],
    score: F,
) -> Vec<(Option<usize>, Option<usize>)>
where
    F: Fn(usize, usize) -> f64,
{
    let mut i = a.len();
    let mut j = b.len();
    let mut matches = Vec::new();

    while i > 0 && j > 0 {
        let s = score(i - 1, j - 1);
        if s > 0.0 && (dp[i][j] - (dp[i - 1][j - 1] + s)).abs() < 1e-9 {
            matches.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    matches.reverse();

    expand_alignment_matches(a.len(), b.len(), &matches)
}

/// Extract embeddable plain text from a text-bearing node.
#[cfg(feature = "semantic-matching")]
fn node_embeddable_text(node: &StructuredNode) -> Option<String> {
    match node {
        StructuredNode::Paragraph(p) => {
            let t = p.content.as_plain_text();
            if t.trim().is_empty() { None } else { Some(t) }
        }
        StructuredNode::Heading(h) => {
            let t = h.content.as_plain_text();
            if t.trim().is_empty() { None } else { Some(t) }
        }
        _ => None,
    }
}

/// Precompute a score matrix for two node slices.
///
/// For each pair `(i, j)`:
/// - 0.0 if structural types are incompatible
/// - Semantic cosine similarity if both are text-bearing (paragraph/heading)
/// - 1.0 if structurally compatible but not text-bearing
#[cfg(feature = "semantic-matching")]
fn precompute_score_matrix(
    base: &[StructuredNode],
    other: &[StructuredNode],
    semantic: &crate::semantic::SemanticMatcher,
) -> Vec<Vec<f64>> {
    let m = base.len();
    let n = other.len();

    let base_texts: Vec<Option<String>> = base.iter().map(node_embeddable_text).collect();
    let other_texts: Vec<Option<String>> = other.iter().map(node_embeddable_text).collect();

    // Collect all texts for batch embedding.
    let mut all_texts: Vec<&str> = Vec::new();
    let mut base_emb_idx: Vec<Option<usize>> = Vec::with_capacity(m);
    let mut other_emb_idx: Vec<Option<usize>> = Vec::with_capacity(n);

    for t in &base_texts {
        if let Some(text) = t {
            base_emb_idx.push(Some(all_texts.len()));
            all_texts.push(text.as_str());
        } else {
            base_emb_idx.push(None);
        }
    }
    for t in &other_texts {
        if let Some(text) = t {
            other_emb_idx.push(Some(all_texts.len()));
            all_texts.push(text.as_str());
        } else {
            other_emb_idx.push(None);
        }
    }

    let embeddings = if !all_texts.is_empty() {
        semantic.embed_batch(&all_texts).unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut scores = vec![vec![0.0f64; n]; m];
    for i in 0..m {
        for j in 0..n {
            if !node_matches_for_similarity(&base[i], &other[j]) {
                continue;
            }
            // Structurally compatible — use semantic score for text nodes.
            scores[i][j] = match (base_emb_idx[i], other_emb_idx[j]) {
                (Some(bi), Some(oi)) => crate::semantic::SemanticMatcher::cosine_similarity(
                    &embeddings[bi],
                    &embeddings[oi],
                )
                .max(0.0) as f64,
                _ => 1.0,
            };
        }
    }

    scores
}

/// Like [`align_and_tag`] but uses semantic-weighted LCS for alignment.
#[cfg(feature = "semantic-matching")]
fn align_and_tag_semantic(
    ctx: &PairwiseMergeCtx,
    base: &[StructuredNode],
    other: &[StructuredNode],
    semantic: &crate::semantic::SemanticMatcher,
) -> Vec<AlignedNode> {
    let anchors = find_som_anchors(base, other);

    align_with_anchors(
        base,
        other,
        &anchors,
        |lhs, rhs| align_segment_semantic(ctx, lhs, rhs, semantic),
        |lhs, rhs| TranslationPolicy::merge_matched(ctx, lhs, rhs),
    )
}

/// Like [`align_segment`] but uses precomputed semantic scores in a weighted LCS.
#[cfg(feature = "semantic-matching")]
fn align_segment_semantic(
    ctx: &PairwiseMergeCtx,
    base: &[StructuredNode],
    other: &[StructuredNode],
    semantic: &crate::semantic::SemanticMatcher,
) -> Vec<AlignedNode> {
    let scores = precompute_score_matrix(base, other, semantic);
    let score_fn = |i: usize, j: usize| scores[i][j];

    let dp = lcs_table_weighted(base, other, score_fn);
    let alignment = lcs_align_weighted(base, other, &dp, score_fn);

    alignment_to_nodes(base, other, alignment, |a, b| {
        TranslationPolicy::merge_matched(ctx, a, b)
    })
}

/// Convert index-based alignment pairs into tagged aligned nodes.
fn alignment_to_nodes<M>(
    base: &[StructuredNode],
    other: &[StructuredNode],
    alignment: Vec<(Option<usize>, Option<usize>)>,
    mut merge_matched: M,
) -> Vec<AlignedNode>
where
    M: FnMut(&StructuredNode, &StructuredNode) -> StructuredNode,
{
    alignment
        .into_iter()
        .map(|(ai, bi)| match (ai, bi) {
            (Some(a), Some(b)) => AlignedNode::Matched(merge_matched(&base[a], &other[b])),
            (Some(a), None) => AlignedNode::LeftOnly(base[a].clone()),
            (None, Some(b)) => AlignedNode::RightOnly(other[b].clone()),
            (None, None) => unreachable!(),
        })
        .collect()
}

/// Merge any Conditional nodes that have the same condition.
/// Content from duplicates is unioned into a single representative node.
pub(crate) fn merge_duplicate_conditionals(nodes: Vec<StructuredNode>) -> Vec<StructuredNode> {
    if nodes.len() < 2 {
        return nodes;
    }

    let has_dup = {
        let mut found = false;
        'outer: for (i, ni) in nodes.iter().enumerate() {
            if let StructuredNode::Conditional(ci) = ni {
                for nj in nodes[i + 1..].iter() {
                    if let StructuredNode::Conditional(cj) = nj {
                        if ci.condition == cj.condition {
                            found = true;
                            break 'outer;
                        }
                    }
                }
            }
        }
        found
    };

    if !has_dup {
        return nodes;
    }

    let mut skip = vec![false; nodes.len()];
    let mut extra: Vec<Vec<StructuredNode>> = nodes.iter().map(|_| Vec::new()).collect();

    for j in 1..nodes.len() {
        if let StructuredNode::Conditional(cj) = &nodes[j] {
            for i in 0..j {
                if skip[i] {
                    continue;
                }
                if let StructuredNode::Conditional(ci) = &nodes[i] {
                    if ci.condition == cj.condition {
                        skip[j] = true;
                        match cj.content.as_ref() {
                            StructuredNode::Group(g) => extra[i].extend(g.children.clone()),
                            other => extra[i].push(other.clone()),
                        }
                        break;
                    }
                }
            }
        }
    }

    nodes
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !skip[*i])
        .map(|(i, node)| {
            if extra[i].is_empty() {
                return node;
            }
            if let StructuredNode::Conditional(c) = node {
                let mut children = match *c.content {
                    StructuredNode::Group(g) => g.children,
                    other => vec![other],
                };
                children.append(&mut extra[i]);
                StructuredNode::Conditional(ConditionalNode {
                    condition: c.condition,
                    content: Box::new(StructuredNode::Group(GroupNode {
                        children,
                        column_flow: false,
                    })),
                })
            } else {
                node
            }
        })
        .collect()
}

pub(crate) fn fill_missing_translation_placeholders(
    nodes: &mut [StructuredNode],
    all_languages: &[String],
    primary_language: &str,
) {
    for node in nodes {
        fill_node(node, all_languages, primary_language);
    }
}

fn fill_node(node: &mut StructuredNode, all_languages: &[String], _primary_language: &str) {
    match node {
        // Nothing to fill: the markup map IS the per-language store, and a
        // language with no markup must stay absent rather than gain an empty
        // block that would render as a blank table.
        StructuredNode::Html(_) => {}
        StructuredNode::Heading(h) => fill_translated_text(&mut h.content, all_languages),
        StructuredNode::Paragraph(p) => fill_translated_text(&mut p.content, all_languages),
        StructuredNode::Field(f) => {
            if let Some(label) = &mut f.label {
                fill_translated_text(label, all_languages);
            }
            if let Some(ts) = &mut f.placeholder {
                fill_translatable_string(ts, all_languages, _primary_language);
            }
            fill_field_type(&mut f.input_type, all_languages, _primary_language);
        }
        StructuredNode::Table(t) => {
            if let Some(caption) = &mut t.caption {
                fill_translated_text(caption, all_languages);
            }
            if let Some(header) = &mut t.header {
                for cell in &mut header.cells {
                    fill_node(cell, all_languages, _primary_language);
                }
            }
            for row in &mut t.rows {
                for cell in &mut row.cells {
                    fill_node(cell, all_languages, _primary_language);
                }
            }
        }
        StructuredNode::Group(g) => {
            for child in &mut g.children {
                fill_node(child, all_languages, _primary_language);
            }
        }
        StructuredNode::Repeatable(r) => fill_node(&mut r.item, all_languages, _primary_language),
        StructuredNode::Conditional(c) => {
            fill_node(&mut c.content, all_languages, _primary_language)
        }
        StructuredNode::GridLayout(g) => {
            for element in &mut g.elements {
                fill_node(&mut element.node, all_languages, _primary_language);
            }
        }
        StructuredNode::List(l) => {
            for item in &mut l.items {
                fill_translated_text(&mut item.content, all_languages);
                if let Some(sub) = &mut item.sublist {
                    for sub_item in &mut sub.items {
                        fill_translated_text(&mut sub_item.content, all_languages);
                    }
                }
            }
        }
        StructuredNode::Footnote(n) => fill_translated_text(&mut n.content, all_languages),
        StructuredNode::Notice(n) => fill_translated_text(&mut n.content, all_languages),
        StructuredNode::Image(_) | StructuredNode::Empty => {}
    }
}

fn fill_field_type(input_type: &mut FieldType, all_languages: &[String], primary_language: &str) {
    match input_type {
        FieldType::Radio { options }
        | FieldType::Select { options }
        | FieldType::CheckboxGroup { options } => {
            for NameValue { name, .. } in options {
                fill_translatable_string(name, all_languages, primary_language);
            }
        }
        _ => {}
    }
}

/// Ensure a `TranslatedText` has entries for all languages.
/// Missing languages get an empty `InlineText`.
fn fill_translated_text(text: &mut TranslatedText, all_languages: &[String]) {
    for lang in all_languages {
        text.0.entry(lang.clone()).or_insert_with(InlineText::empty);
    }
}

fn fill_translatable_string(
    value: &mut TranslatableString,
    all_languages: &[String],
    primary_language: &str,
) {
    match value {
        TranslatableString::Plain(s) => {
            let mut map = TranslationMap::new();
            map.insert(primary_language.to_string(), Some(s.clone()));
            ensure_all_languages(&mut map, all_languages);
            *value = TranslatableString::Translated(map);
        }
        TranslatableString::Translated(map) => ensure_all_languages(map, all_languages),
    }
}

fn ensure_all_languages(map: &mut TranslationMap, langs: &[String]) {
    // Only treat whitespace-only entries as missing when at least one other
    // language carries real (non-whitespace) content.  If every existing value
    // is whitespace, the whitespace is structural (e.g. a space between a
    // numbering prefix and heading text) and must be preserved — including for
    // language keys that are absent from the map or already set to None.
    let has_real_content = map
        .values()
        .any(|v| v.as_ref().is_some_and(|s| !s.trim().is_empty()));

    for lang in langs {
        match map.get(lang) {
            Some(Some(value)) if value.trim().is_empty() && has_real_content => {
                map.insert(lang.clone(), None);
            }
            Some(None) if !has_real_content => {
                // Pre-existing None in a whitespace-only segment — repair it
                // by replicating the structural whitespace.
                let ws = map.values().find_map(|v| v.clone()).unwrap_or_default();
                map.insert(lang.clone(), Some(ws));
            }
            Some(_) => {}
            None => {
                if has_real_content {
                    map.insert(lang.clone(), None);
                } else {
                    // All existing values are whitespace — replicate the
                    // whitespace for the missing language key.
                    let ws = map.values().find_map(|v| v.clone()).unwrap_or_default();
                    map.insert(lang.clone(), Some(ws));
                }
            }
        }
    }
}

// ============================================================================
// Policy infrastructure for parameterised merge strategies
// ============================================================================

/// Context for a pairwise node-list merge.
pub(crate) struct PairwiseMergeCtx<'a> {
    pub base_lang: &'a str,
    pub other_lang: &'a str,
}

/// Aligned entry produced by LCS alignment, tagged with its origin.
///
/// The inner `StructuredNode` is already merged (for `Matched`) but NOT
/// localised (for `LeftOnly` / `RightOnly`).  Consolidation passes can
/// therefore inspect un-localised content before the final collection step
/// applies [`localize_structured_node`].
pub(crate) enum AlignedNode {
    /// Both sides matched — contains the already-merged node.
    Matched(StructuredNode),
    /// Present only on the left (base) side.  Contains the original node.
    LeftOnly(StructuredNode),
    /// Present only on the right (other) side.  Contains the original node.
    RightOnly(StructuredNode),
}

/// Policy trait for resolving conflicts during LCS-based node alignment.
///
/// Implementors define how nodes are matched and how matched pairs are merged.
/// Unmatched nodes are kept raw in [`AlignedNode`] and processed by the caller.
pub(crate) trait MergePolicy {
    /// Decide whether two nodes should be paired during LCS alignment.
    fn nodes_match(a: &StructuredNode, b: &StructuredNode) -> bool;

    /// Combine two paired nodes into one.
    fn merge_matched(
        ctx: &PairwiseMergeCtx,
        a: &StructuredNode,
        b: &StructuredNode,
    ) -> StructuredNode;
}

/// Align two node lists via LCS and resolve each entry through the given
/// [`MergePolicy`].  Returns tagged [`AlignedNode`] entries so callers can
/// post-process (e.g. orphan consolidation) before collecting the final list.
///
/// When nodes carry SOM path hints, matching SOM paths act as hard anchors
/// that force alignment.  The algorithm:
///  1. Collect monotonically-increasing anchor pairs from unique SOM matches.
///  2. Run LCS independently on each segment between anchors.
///  3. Anchors always produce `Matched` entries.
pub(crate) fn align_and_tag<P: MergePolicy>(
    ctx: &PairwiseMergeCtx,
    base: &[StructuredNode],
    other: &[StructuredNode],
) -> Vec<AlignedNode> {
    let anchors = find_som_anchors(base, other);

    align_with_anchors(
        base,
        other,
        &anchors,
        |lhs, rhs| align_segment::<P>(ctx, lhs, rhs),
        |lhs, rhs| P::merge_matched(ctx, lhs, rhs),
    )
}

/// Align two lists using anchor-constrained segmentation.
///
/// `anchors` must be monotonically increasing index pairs. For each segment
/// between anchors, `segment_aligner` is used to align entries; each anchor pair
/// is emitted as a `Matched` node produced by `merge_anchor`.
fn align_with_anchors<Seg, Merge>(
    base: &[StructuredNode],
    other: &[StructuredNode],
    anchors: &[(usize, usize)],
    mut segment_aligner: Seg,
    mut merge_anchor: Merge,
) -> Vec<AlignedNode>
where
    Seg: FnMut(&[StructuredNode], &[StructuredNode]) -> Vec<AlignedNode>,
    Merge: FnMut(&StructuredNode, &StructuredNode) -> StructuredNode,
{
    if anchors.is_empty() {
        return segment_aligner(base, other);
    }

    let mut result = Vec::new();
    let mut ai = 0usize;
    let mut bi = 0usize;

    for (anchor_a, anchor_b) in anchors {
        result.extend(segment_aligner(&base[ai..*anchor_a], &other[bi..*anchor_b]));
        result.push(AlignedNode::Matched(merge_anchor(
            &base[*anchor_a],
            &other[*anchor_b],
        )));
        ai = anchor_a + 1;
        bi = anchor_b + 1;
    }

    result.extend(segment_aligner(&base[ai..], &other[bi..]));

    result
}

/// Run LCS alignment on a single segment (sub-slice) of nodes.
fn align_segment<P: MergePolicy>(
    ctx: &PairwiseMergeCtx,
    base: &[StructuredNode],
    other: &[StructuredNode],
) -> Vec<AlignedNode> {
    let dp = lcs_table_with(base, other, P::nodes_match);
    let alignment = lcs_align_with(base, other, &dp, P::nodes_match);

    alignment_to_nodes(base, other, alignment, |a, b| P::merge_matched(ctx, a, b))
}

/// Find monotonically-increasing anchor pairs where both sides share the same
/// SOM path.  Only SOM paths that appear exactly once in each list qualify.
fn find_som_anchors(base: &[StructuredNode], other: &[StructuredNode]) -> Vec<(usize, usize)> {
    use std::collections::HashMap;

    // Build index: anchor_key → position (only keep keys that appear exactly once).
    // anchor_key() returns the best available language-independent identifier:
    // SOM path when available, falling back to source_name (XFA draw node name).
    let collect_unique = |nodes: &[StructuredNode]| -> HashMap<String, usize> {
        let mut counts: HashMap<String, (usize, usize)> = HashMap::new(); // key → (first_index, count)
        for (i, node) in nodes.iter().enumerate() {
            if let Some(key) = node.anchor_key() {
                counts
                    .entry(key)
                    .and_modify(|(_, c)| *c += 1)
                    .or_insert((i, 1));
            }
        }
        counts
            .into_iter()
            .filter(|(_, (_, c))| *c == 1)
            .map(|(path, (idx, _))| (path, idx))
            .collect()
    };

    let base_unique = collect_unique(base);
    let other_unique = collect_unique(other);

    // Collect matching pairs where the SOM path is unique in both lists.
    let mut pairs: Vec<(usize, usize)> = base_unique
        .iter()
        .filter_map(|(path, &a_idx)| other_unique.get(path).map(|&b_idx| (a_idx, b_idx)))
        .collect();

    // Sort by base index for LIS computation.
    pairs.sort_by_key(|(a, _)| *a);

    // Compute longest increasing subsequence on b-indices using patience sorting.
    if pairs.is_empty() {
        return Vec::new();
    }

    let b_vals: Vec<usize> = pairs.iter().map(|(_, b)| *b).collect();
    let n = b_vals.len();
    // tails[i] = smallest tail element for IS of length i+1
    let mut tails: Vec<usize> = Vec::new();
    // parent tracking for reconstruction
    let mut indices: Vec<usize> = Vec::new(); // index into tails
    let mut parent: Vec<Option<usize>> = vec![None; n];

    for i in 0..n {
        let pos = tails.partition_point(|&t| t < b_vals[i]);
        if pos == tails.len() {
            tails.push(b_vals[i]);
            indices.push(i);
        } else {
            tails[pos] = b_vals[i];
            indices[pos] = i;
        }
        if pos > 0 {
            parent[i] = Some(indices[pos - 1]);
        }
    }

    // Reconstruct the LIS
    let mut lis = Vec::with_capacity(tails.len());
    let mut idx = *indices.last().unwrap();
    loop {
        lis.push(pairs[idx]);
        if let Some(p) = parent[idx] {
            idx = p;
        } else {
            break;
        }
    }
    lis.reverse();

    lis
}

// ============================================================================
// Translation merge policy
// ============================================================================

/// Policy that merges two single-language trees into a bilingual tree.
///
/// * Matching uses relaxed type/shape comparison ([`node_matches_for_similarity`]).
/// * Matched nodes are merged recursively by combining text into `TranslatedText`.
/// * Unmatched nodes are kept raw for later localisation.
pub(crate) struct TranslationPolicy;

impl MergePolicy for TranslationPolicy {
    fn nodes_match(a: &StructuredNode, b: &StructuredNode) -> bool {
        node_matches_for_similarity(a, b)
    }

    fn merge_matched(
        ctx: &PairwiseMergeCtx,
        a: &StructuredNode,
        b: &StructuredNode,
    ) -> StructuredNode {
        merge_node(a, ctx.base_lang, b, ctx.other_lang)
    }
}

// ============================================================================
// Node matching (relaxed, for translation alignment)
// ============================================================================

/// Relaxed node matching for translation alignment and similarity pre-check.
///
/// Matches nodes based on their high-level type and shape without requiring
/// identical deep structure.  This allows translation pairs with minor layout
/// differences to be correctly aligned.
///
/// Rules:
/// - Headings: same level required
/// - Fields: same `FieldType` variant required (FieldIds may differ across languages)
/// - Tables: same header column count required
/// - GridLayouts: same column count required (element count may differ)
/// - Paragraphs, Images, Groups, Conditionals, Repeatables, Lists, Empty: match by type only
pub(crate) fn node_matches_for_similarity(a: &StructuredNode, b: &StructuredNode) -> bool {
    // If both nodes carry the same SOM path and the same top-level variant,
    // they match regardless of content/shape differences.
    // Exception: paragraphs also require matching inline structure (e.g.
    // bold vs non-bold) even with the same SOM path, because a single XFA
    // element can render as multiple paragraphs with different styling,
    // and the SOM path alone cannot distinguish them.
    if std::mem::discriminant(a) == std::mem::discriminant(b) {
        if let (Some(sa), Some(sb)) = (a.som_path(), b.som_path()) {
            if sa.as_str() == sb.as_str() {
                if let (StructuredNode::Paragraph(pa), StructuredNode::Paragraph(pb)) = (a, b) {
                    return inline_text_structure_compatible(&pa.content, &pb.content);
                }
                return true;
            }
        }
    }

    match (a, b) {
        (StructuredNode::Heading(ha), StructuredNode::Heading(hb)) => {
            // Some language variants differ in heading level assignment while
            // still representing the same logical item.
            inline_text_shape_compatible(&ha.content, &hb.content)
        }
        (StructuredNode::Paragraph(pa), StructuredNode::Paragraph(pb)) => {
            inline_text_shape_compatible(&pa.content, &pb.content)
        }
        (StructuredNode::Notice(na), StructuredNode::Notice(nb)) => {
            inline_text_shape_compatible(&na.content, &nb.content)
        }
        (StructuredNode::Image(_), StructuredNode::Image(_)) => true,
        (StructuredNode::Table(ta), StructuredNode::Table(tb)) => {
            let a_cols = ta.header.as_ref().map_or(0, |h| h.cells.len());
            let b_cols = tb.header.as_ref().map_or(0, |h| h.cells.len());
            a_cols == b_cols
        }
        (StructuredNode::Field(fa), StructuredNode::Field(fb)) => {
            if std::mem::discriminant(&fa.input_type) == std::mem::discriminant(&fb.input_type) {
                return true;
            }
            false
        }
        (StructuredNode::Repeatable(_), StructuredNode::Repeatable(_)) => true,
        (StructuredNode::Group(_), StructuredNode::Group(_)) => {
            // Prefer anchor-key equality when available. This prevents unrelated
            // section groups from being treated as interchangeable and drifting
            // alignment inside glossary-like content.
            match (a.anchor_key(), b.anchor_key()) {
                (Some(ka), Some(kb)) => ka == kb,
                _ => true,
            }
        }
        (StructuredNode::Conditional(a), StructuredNode::Conditional(b)) => {
            a.condition == b.condition
        }
        (StructuredNode::Empty, StructuredNode::Empty) => true,
        (StructuredNode::GridLayout(ga), StructuredNode::GridLayout(gb)) => {
            ga.columns == gb.columns
        }
        (StructuredNode::List(la), StructuredNode::List(lb)) => la.list_style == lb.list_style,
        // Conditional vs non-Conditional: when one language wraps content in a
        // Conditional (due to exhaustive-state differences) and the other doesn't,
        // try matching the conditional's inner content against the bare node.
        (StructuredNode::Conditional(c), other) => node_matches_for_similarity(&c.content, other),
        (other, StructuredNode::Conditional(c)) => node_matches_for_similarity(other, &c.content),
        _ => false,
    }
}

/// Check whether two inline texts have the same structural wrapping pattern
/// (e.g. both all-bold, both all-plain, etc.).
///
/// This prevents cross-matching paragraphs from the same XFA element that
/// were rendered with different styling (e.g. a non-bold introduction and a
/// bold instruction line).
fn inline_text_structure_compatible(a: &TranslatedText, b: &TranslatedText) -> bool {
    let a_bold = a.0.values().next().is_some_and(inline_text_is_bold);
    let b_bold = b.0.values().next().is_some_and(inline_text_is_bold);
    a_bold == b_bold
}

/// Returns true if all non-empty inline nodes are wrapped in Strong.
fn inline_text_is_bold(text: &InlineText) -> bool {
    let mut has_content = false;
    for node in &text.0 {
        match node {
            InlineNode::Strong(_) => {
                has_content = true;
            }
            InlineNode::Text(s) if s.trim().is_empty() => {}
            _ => return false,
        }
    }
    has_content
}

fn inline_text_shape_compatible(a: &TranslatedText, b: &TranslatedText) -> bool {
    let a_projections: Vec<String> = a.0.values().map(|t| t.as_plain_text()).collect();
    let b_projections: Vec<String> = b.0.values().map(|t| t.as_plain_text()).collect();

    // If either side is empty, use empty string as projection
    let a_projections = if a_projections.is_empty() {
        vec![String::new()]
    } else {
        a_projections
    };
    let b_projections = if b_projections.is_empty() {
        vec![String::new()]
    } else {
        b_projections
    };

    for a_proj in &a_projections {
        for b_proj in &b_projections {
            if text_shape_compatible(a_proj, b_proj) {
                return true;
            }
        }
    }
    false
}

fn text_shape_compatible(a: &str, b: &str) -> bool {
    let shape_a = text_shape(a);
    let shape_b = text_shape(b);

    if shape_a.chars == 0 && shape_b.chars == 0 {
        return true;
    }
    if shape_a.chars == 0 || shape_b.chars == 0 {
        return false;
    }

    // Similarity gate by coarse structure. Language content can differ,
    // but paragraph/heading shapes should be in the same rough range.
    let char_ratio = ratio(shape_a.chars, shape_b.chars);
    let word_ratio = ratio(shape_a.words.max(1), shape_b.words.max(1));
    let digit_delta = shape_a.digits.abs_diff(shape_b.digits);
    let punct_delta = shape_a.punct.abs_diff(shape_b.punct);

    let short_text = shape_a.chars.max(shape_b.chars) <= 32;
    let max_char_ratio = if short_text { 3.5 } else { 2.2 };

    // Compound-word languages (e.g. German) can express in one word what other
    // languages need several words for.  When one side has a single word, use a
    // more lenient word-ratio limit so valid translation pairs are not rejected.
    let min_words = shape_a.words.min(shape_b.words);
    let max_word_ratio = if min_words <= 1 { 4.0 } else { 2.5 };

    char_ratio <= max_char_ratio
        && word_ratio <= max_word_ratio
        && digit_delta <= 3
        && punct_delta <= 8
}

fn ratio(a: usize, b: usize) -> f64 {
    let max_v = a.max(b) as f64;
    let min_v = a.min(b).max(1) as f64;
    max_v / min_v
}

#[derive(Clone, Copy)]
struct TextShape {
    chars: usize,
    words: usize,
    digits: usize,
    punct: usize,
}

fn text_shape(input: &str) -> TextShape {
    let chars = input.chars().filter(|c| !c.is_whitespace()).count();
    // Count lexical tokens (not just whitespace-separated chunks) so
    // slash-delimited compounds like "A/B" are treated as two words.
    let words = input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .count();
    let digits = input.chars().filter(|c| c.is_ascii_digit()).count();
    let punct = input.chars().filter(|c| c.is_ascii_punctuation()).count();

    TextShape {
        chars,
        words,
        digits,
        punct,
    }
}

// ============================================================================
// Localisation — tag a node tree with a single source language
// ============================================================================

fn localize_translatable_string(value: &TranslatableString, lang: &str) -> TranslatableString {
    match value {
        TranslatableString::Plain(text) => {
            TranslatableString::Translated(HashMap::from([(lang.to_string(), Some(text.clone()))]))
        }
        TranslatableString::Translated(map) => TranslatableString::Translated(map.clone()),
    }
}

fn localize_field_type(field_type: &FieldType, lang: &str) -> FieldType {
    match field_type {
        FieldType::Radio { options } => FieldType::Radio {
            options: options
                .iter()
                .map(|option| NameValue {
                    name: localize_translatable_string(&option.name, lang),
                    value: option.value.clone(),
                })
                .collect(),
        },
        FieldType::Select { options } => FieldType::Select {
            options: options
                .iter()
                .map(|option| NameValue {
                    name: localize_translatable_string(&option.name, lang),
                    value: option.value.clone(),
                })
                .collect(),
        },
        FieldType::CheckboxGroup { options } => FieldType::CheckboxGroup {
            options: options
                .iter()
                .map(|option| NameValue {
                    name: localize_translatable_string(&option.name, lang),
                    value: option.value.clone(),
                })
                .collect(),
        },
        _ => field_type.clone(),
    }
}

fn localize_structured_node(node: &StructuredNode, lang: &str) -> StructuredNode {
    // With TranslatedText, content fields are already keyed by language.
    // However, if a single-language node has InlineText content that hasn't
    // been tagged yet (e.g. from a freshly parsed tree), we wrap it.
    // In practice, structured nodes already contain TranslatedText, so we
    // just need to ensure the language key is present.
    match node {
        StructuredNode::Heading(heading) => StructuredNode::Heading(HeadingNode {
            level: heading.level,
            content: localize_translated_text(&heading.content, lang),
            som_path: heading.som_path.clone(),
            source_name: heading.source_name.clone(),
        }),
        StructuredNode::Paragraph(paragraph) => StructuredNode::Paragraph(ParagraphNode {
            content: localize_translated_text(&paragraph.content, lang),
            som_path: paragraph.som_path.clone(),
            source_name: paragraph.source_name.clone(),
        }),
        StructuredNode::Image(image) => StructuredNode::Image(image.clone()),
        StructuredNode::Html(html) => StructuredNode::Html(localize_html_node(html, lang)),
        StructuredNode::Table(table) => StructuredNode::Table(TableNode {
            header: table.header.as_ref().map(|header| TableHeader {
                cells: header
                    .cells
                    .iter()
                    .map(|cell| localize_structured_node(cell, lang))
                    .collect(),
            }),
            rows: table
                .rows
                .iter()
                .map(|row| TableRow {
                    cells: row
                        .cells
                        .iter()
                        .map(|cell| localize_structured_node(cell, lang))
                        .collect(),
                })
                .collect(),
            caption: table
                .caption
                .as_ref()
                .map(|caption| localize_translated_text(caption, lang)),
        }),
        StructuredNode::Field(field) => StructuredNode::Field(FieldNode {
            name: field.name.clone(),
            som_path: field.som_path.clone(),
            label: field
                .label
                .as_ref()
                .map(|label| localize_translated_text(label, lang)),
            input_type: localize_field_type(&field.input_type, lang),
            value: field.value.clone(),
            placeholder: field
                .placeholder
                .as_ref()
                .map(|placeholder| localize_translatable_string(placeholder, lang)),
            required: field.required,
        }),
        StructuredNode::Repeatable(repeatable) => StructuredNode::Repeatable(RepeatableNode {
            item: Box::new(localize_structured_node(&repeatable.item, lang)),
            min_occurrences: repeatable.min_occurrences,
            max_occurrences: repeatable.max_occurrences,
        }),
        StructuredNode::Group(group) => StructuredNode::Group(GroupNode {
            children: group
                .children
                .iter()
                .map(|child| localize_structured_node(child, lang))
                .collect(),
            column_flow: group.column_flow,
        }),
        StructuredNode::Conditional(conditional) => StructuredNode::Conditional(ConditionalNode {
            condition: conditional.condition.clone(),
            content: Box::new(localize_structured_node(&conditional.content, lang)),
        }),
        StructuredNode::Empty => StructuredNode::Empty,
        StructuredNode::GridLayout(grid) => StructuredNode::GridLayout(GridLayout {
            columns: grid.columns,
            elements: grid
                .elements
                .iter()
                .map(|element| GridLayoutElement {
                    span: element.span,
                    node: localize_structured_node(&element.node, lang),
                })
                .collect(),
        }),
        StructuredNode::Footnote(n) => StructuredNode::Footnote(FootnoteNode {
            content: localize_translated_text(&n.content, lang),
            marker: n.marker.clone(),
            som_path: n.som_path.clone(),
            source_name: n.source_name.clone(),
        }),
        StructuredNode::Notice(n) => StructuredNode::Notice(NoticeNode {
            content: localize_translated_text(&n.content, lang),
            som_path: n.som_path.clone(),
            source_name: n.source_name.clone(),
        }),
        StructuredNode::List(list) => StructuredNode::List(ListNode {
            list_style: list.list_style,
            items: list
                .items
                .iter()
                .map(|item| ListItem {
                    content: localize_translated_text(&item.content, lang),
                    sublist: item.sublist.as_ref().map(|sub| {
                        Box::new(ListNode {
                            list_style: sub.list_style,
                            items: sub
                                .items
                                .iter()
                                .map(|si| ListItem {
                                    content: localize_translated_text(&si.content, lang),
                                    sublist: None,
                                })
                                .collect(),
                        })
                    }),
                })
                .collect(),
        }),
    }
}

/// Ensure a TranslatedText has the correct language key.
/// If the text has a single entry under a different key (e.g. "default"),
/// re-key it to the specified language.
/// [`localize_translated_text`] for an [`HtmlNode`]'s markup map: re-key a lone
/// untagged entry to `lang`, otherwise leave it alone.
fn localize_html_node(html: &HtmlNode, lang: &str) -> HtmlNode {
    if html.content.contains_key(lang) || html.content.len() != 1 {
        return html.clone();
    }
    let markup = html.content.values().next().unwrap().clone();
    HtmlNode {
        content: std::iter::once((lang.to_string(), markup)).collect(),
        source_name: html.source_name.clone(),
    }
}

fn localize_translated_text(text: &TranslatedText, lang: &str) -> TranslatedText {
    if text.0.contains_key(lang) || text.0.is_empty() {
        return text.clone();
    }
    // If there's exactly one entry with a different key, re-key it
    if text.0.len() == 1 {
        let inline = text.0.values().next().unwrap().clone();
        return TranslatedText::single(lang.to_string(), inline);
    }
    // Multiple languages already present — return as-is
    text.clone()
}

// ============================================================================
// Translation merge — node list with consolidation
// ============================================================================

fn collect_translated_text_languages(text: &TranslatedText) -> Vec<String> {
    text.0.keys().cloned().collect()
}

fn matched_paragraph_has_nonempty_language(entry: &AlignedNode, lang: &str) -> bool {
    if let AlignedNode::Matched(StructuredNode::Paragraph(para)) = entry {
        if let Some(inline_text) = para.content.get(lang) {
            let plain = inline_text.as_plain_text();
            return !plain.trim().is_empty();
        }
    }
    false
}

pub(crate) fn prepend_orphan_text_to_matched_paragraph(
    entry: &mut AlignedNode,
    text: &str,
    lang: &str,
    _base_lang: &str,
    _other_lang: &str,
) -> bool {
    if let AlignedNode::Matched(StructuredNode::Paragraph(para)) = entry {
        // Get or create the InlineText for this language
        let inline = para
            .content
            .0
            .entry(lang.to_string())
            .or_insert_with(InlineText::empty);

        let existing_plain = inline.as_plain_text();
        if existing_plain.is_empty() {
            *inline = InlineText::plain(text);
        } else {
            let leading = inline
                .0
                .first()
                .and_then(|n| n.leading_text())
                .unwrap_or(" ");
            let starts_with_space = leading
                .as_bytes()
                .first()
                .is_none_or(|b| b.is_ascii_whitespace());

            if starts_with_space || !super::needs_separator(text, &existing_plain) {
                // Prepend text node directly
                inline.0.insert(0, InlineNode::Text(text.to_string()));
            } else {
                // Add separator space
                inline.0.insert(0, InlineNode::Text(format!("{} ", text)));
            }
        }
        return true;
    }

    false
}

/// Flatten groups whose anchor keys are non-unique within the same list.
///
/// When multiple groups share the same anchor key (e.g. all derive from the
/// same XFA draw node name like `T_Left`), they provide zero anchoring value
/// and cause misalignment. Dissolving them exposes their children to direct
/// alignment by content shape/semantics.
fn flatten_non_unique_groups(nodes: &[StructuredNode]) -> Vec<StructuredNode> {
    // Count how often each group anchor key appears.
    let mut key_counts: HashMap<String, usize> = HashMap::new();
    for node in nodes {
        if let StructuredNode::Group(_) = node {
            if let Some(key) = node.anchor_key() {
                *key_counts.entry(key).or_insert(0) += 1;
            }
        }
    }

    // If no group key appears more than once, return as-is (no allocation).
    let has_duplicates = key_counts.values().any(|&c| c > 1);
    if !has_duplicates {
        return nodes.to_vec();
    }

    let mut result = Vec::with_capacity(nodes.len());
    for node in nodes {
        match node {
            StructuredNode::Group(g) => {
                let dominated = node
                    .anchor_key()
                    .map(|k| key_counts.get(&k).copied().unwrap_or(0) > 1)
                    .unwrap_or(true);
                if dominated {
                    // Dissolve: promote children to parent level.
                    result.extend(g.children.iter().cloned());
                } else {
                    result.push(node.clone());
                }
            }
            _ => result.push(node.clone()),
        }
    }
    result
}

/// Merge two node lists from different languages using LCS alignment.
///
/// Uses the [`TranslationPolicy`] for alignment and merging, then runs
/// orphan consolidation passes to absorb split-paragraph artifacts and
/// reorder-mismatched conditionals.
pub(crate) fn merge_node_lists(
    base: &[StructuredNode],
    base_lang: &str,
    other: &[StructuredNode],
    other_lang: &str,
) -> Vec<StructuredNode> {
    let base_flat = flatten_non_unique_groups(base);
    let other_flat = flatten_non_unique_groups(other);

    let ctx = PairwiseMergeCtx {
        base_lang,
        other_lang,
    };
    let entries = align_and_tag::<TranslationPolicy>(&ctx, &base_flat, &other_flat);

    finalize_aligned_entries(entries, base_lang, other_lang)
}

/// Same as [`merge_node_lists`] but uses semantic-weighted LCS alignment
/// where text-bearing nodes (paragraphs, headings) are scored by cross-lingual
/// embedding similarity instead of binary structural matching.
#[cfg(feature = "semantic-matching")]
pub(crate) fn merge_node_lists_semantic(
    base: &[StructuredNode],
    base_lang: &str,
    other: &[StructuredNode],
    other_lang: &str,
    semantic: &crate::semantic::SemanticMatcher,
) -> Vec<StructuredNode> {
    let base_flat = flatten_non_unique_groups(base);
    let other_flat = flatten_non_unique_groups(other);

    let ctx = PairwiseMergeCtx {
        base_lang,
        other_lang,
    };
    let entries = align_and_tag_semantic(&ctx, &base_flat, &other_flat, semantic);

    finalize_aligned_entries(entries, base_lang, other_lang)
}

fn finalize_aligned_entries(
    mut entries: Vec<AlignedNode>,
    base_lang: &str,
    other_lang: &str,
) -> Vec<StructuredNode> {
    consolidate_orphan_paragraphs(&mut entries, base_lang, other_lang);
    consolidate_orphan_paragraph_groups(&mut entries, base_lang, other_lang);
    consolidate_orphan_conditionals(&mut entries, base_lang, other_lang);
    consolidate_orphan_paragraph_into_field_label(&mut entries, base_lang, other_lang);
    consolidate_by_neighborhood(&mut entries, base_lang, other_lang);

    entries
        .into_iter()
        .map(|e| match e {
            AlignedNode::Matched(node) => node,
            AlignedNode::LeftOnly(node) => localize_structured_node(&node, base_lang),
            AlignedNode::RightOnly(node) => localize_structured_node(&node, other_lang),
        })
        .collect()
}

/// Post-process aligned entries to absorb orphaned (unmatched) `Paragraph`
/// nodes into an adjacent matched `Paragraph`.
///
/// When one language splits text into multiple paragraphs while another keeps
/// it as a single paragraph, LCS alignment leaves some paragraphs unmatched.
/// This step detects such orphans and prepends their text to the nearest
/// matched paragraph's `TranslatedText` map.
fn consolidate_orphan_paragraphs(
    entries: &mut Vec<AlignedNode>,
    base_lang: &str,
    other_lang: &str,
) {
    let len = entries.len();
    let mut absorbed = vec![false; len];

    let mut prepend_ops: Vec<(usize, usize, String, String)> = Vec::new();
    for i in 0..len {
        if absorbed[i] {
            continue;
        }

        // Absorb single-language orphan paragraphs from either side.
        let (orphan_lang, orphan_text) = match &entries[i] {
            AlignedNode::LeftOnly(StructuredNode::Paragraph(p)) => {
                let langs = collect_translated_text_languages(&p.content);
                if langs.len() > 1 {
                    // Already multilingual: do not flatten it into one language key.
                    continue;
                }
                let lang = langs
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| base_lang.to_string());
                (lang, p.content.as_plain_text())
            }
            AlignedNode::RightOnly(StructuredNode::Paragraph(p)) => {
                (other_lang.to_string(), p.content.as_plain_text())
            }
            _ => continue,
        };
        if orphan_text.is_empty() {
            continue;
        }

        // Search forward for the nearest Matched(Paragraph).
        let mut target = None;
        for j in (i + 1)..len {
            if absorbed[j] {
                continue;
            }
            match &entries[j] {
                AlignedNode::Matched(StructuredNode::Paragraph(_)) => {
                    let is_left_orphan = orphan_lang == base_lang;
                    if !is_left_orphan
                        || !matched_paragraph_has_nonempty_language(&entries[j], &orphan_lang)
                    {
                        target = Some(j);
                        break;
                    }
                }
                AlignedNode::LeftOnly(StructuredNode::Paragraph(_)) => continue,
                AlignedNode::RightOnly(StructuredNode::Paragraph(_)) => continue,
                _ => break,
            }
        }

        // If none found forward, search backward for nearest Matched(Paragraph).
        if target.is_none() {
            let mut j = i;
            while j > 0 {
                j -= 1;
                if absorbed[j] {
                    continue;
                }
                match &entries[j] {
                    AlignedNode::Matched(StructuredNode::Paragraph(_)) => {
                        let is_left_orphan = orphan_lang == base_lang;
                        if !is_left_orphan
                            || !matched_paragraph_has_nonempty_language(&entries[j], &orphan_lang)
                        {
                            target = Some(j);
                            break;
                        }
                    }
                    AlignedNode::LeftOnly(StructuredNode::Paragraph(_)) => continue,
                    AlignedNode::RightOnly(StructuredNode::Paragraph(_)) => continue,
                    _ => break,
                }
            }
        }

        if let Some(j) = target {
            prepend_ops.push((i, j, orphan_text, orphan_lang.to_string()));
        }
    }

    for (orphan_idx, target, text, lang) in prepend_ops.into_iter().rev() {
        if prepend_orphan_text_to_matched_paragraph(
            &mut entries[target],
            &text,
            &lang,
            base_lang,
            other_lang,
        ) {
            absorbed[orphan_idx] = true;
        }
    }

    for i in (0..len).rev() {
        if absorbed[i] {
            entries.remove(i);
        }
    }
}

/// Post-process aligned entries to merge N:M orphan paragraph groups.
///
/// When one language splits text into N paragraphs and another into M (N≠M),
/// the LCS alignment leaves all of them unmatched because no individual
/// paragraph is shape-compatible with its counterpart. This pass identifies
/// contiguous runs of orphan paragraphs, separates them by language side
/// (LeftOnly vs RightOnly), concatenates each side's text, and checks whether
/// the aggregate texts are shape-compatible. If so, the entire run is replaced
/// by a single `Matched(Paragraph)` with a `TranslatedText` map.
fn consolidate_orphan_paragraph_groups(
    entries: &mut Vec<AlignedNode>,
    base_lang: &str,
    other_lang: &str,
) {
    // Identify contiguous runs of orphan paragraphs.
    // A run ends at any Matched entry or non-Paragraph entry.
    let len = entries.len();
    if len == 0 {
        return;
    }

    // Collect runs: each run is a range [start, end) of indices that are all
    // orphan paragraphs (LeftOnly or RightOnly).
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut run_start: Option<usize> = None;
    for i in 0..len {
        let is_orphan_para = matches!(
            &entries[i],
            AlignedNode::LeftOnly(StructuredNode::Paragraph(_))
                | AlignedNode::RightOnly(StructuredNode::Paragraph(_))
        );
        match (is_orphan_para, run_start) {
            (true, None) => run_start = Some(i),
            (true, Some(_)) => {} // extend current run
            (false, Some(start)) => {
                runs.push((start, i));
                run_start = None;
            }
            (false, None) => {}
        }
    }
    if let Some(start) = run_start {
        runs.push((start, len));
    }

    // Process runs in reverse order so that removals don't invalidate indices.
    for (run_start, run_end) in runs.into_iter().rev() {
        // Separate left-only and right-only paragraph texts.
        let mut left_texts: Vec<(usize, String)> = Vec::new();
        let mut right_texts: Vec<(usize, String)> = Vec::new();
        for i in run_start..run_end {
            match &entries[i] {
                AlignedNode::LeftOnly(StructuredNode::Paragraph(p)) => {
                    left_texts.push((i, p.content.as_plain_text()));
                }
                AlignedNode::RightOnly(StructuredNode::Paragraph(p)) => {
                    right_texts.push((i, p.content.as_plain_text()));
                }
                _ => {}
            }
        }

        // Need both sides to have content, and at least one side must have
        // a different count (otherwise 1:1 consolidation should have handled it).
        if left_texts.is_empty() || right_texts.is_empty() {
            continue;
        }

        // Cap run size to avoid accidentally merging unrelated content.
        if left_texts.len() > 5 || right_texts.len() > 5 {
            continue;
        }

        // Concatenate each side's paragraph texts.
        let left_combined: String = left_texts
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let right_combined: String = right_texts
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join(" ");

        if left_combined.trim().is_empty() || right_combined.trim().is_empty() {
            continue;
        }

        // Check if the aggregate texts are shape-compatible.
        if !text_shape_compatible(&left_combined, &right_combined) {
            continue;
        }

        // Build a merged Paragraph with TranslatedText.
        let mut tt_map = HashMap::new();
        tt_map.insert(base_lang.to_string(), InlineText::plain(left_combined));
        tt_map.insert(other_lang.to_string(), InlineText::plain(right_combined));

        let merged_para = StructuredNode::Paragraph(ParagraphNode {
            content: TranslatedText::new(tt_map),
            som_path: None,
            source_name: None,
        });

        // Replace the first entry in the run with the merged node and remove
        // the rest.
        entries[run_start] = AlignedNode::Matched(merged_para);
        for i in (run_start + 1..run_end).rev() {
            entries.remove(i);
        }
    }
}

/// Post-process aligned entries to merge orphaned `Conditional` nodes that
/// have the same `FieldCondition` but ended up unmatched because the two
/// languages emitted them in a different order.
fn consolidate_orphan_conditionals(
    entries: &mut Vec<AlignedNode>,
    base_lang: &str,
    other_lang: &str,
) {
    let len = entries.len();
    let mut other_only_indices: Vec<usize> = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        if let AlignedNode::RightOnly(StructuredNode::Conditional(_)) = entry {
            other_only_indices.push(i);
        }
    }

    if other_only_indices.is_empty() {
        return;
    }

    let mut merge_ops: Vec<(usize, usize)> = Vec::new();
    let mut consumed_other: Vec<bool> = vec![false; other_only_indices.len()];

    for i in 0..len {
        if let AlignedNode::LeftOnly(StructuredNode::Conditional(base_cond)) = &entries[i] {
            for (k, &other_idx) in other_only_indices.iter().enumerate() {
                if consumed_other[k] {
                    continue;
                }
                if let AlignedNode::RightOnly(StructuredNode::Conditional(other_cond)) =
                    &entries[other_idx]
                {
                    if base_cond.condition == other_cond.condition {
                        merge_ops.push((i, other_idx));
                        consumed_other[k] = true;
                        break;
                    }
                }
            }
        }
    }

    if merge_ops.is_empty() {
        return;
    }

    let mut to_remove = vec![false; len];
    for (base_idx, other_idx) in merge_ops {
        let base_node = std::mem::replace(
            &mut entries[base_idx],
            AlignedNode::LeftOnly(StructuredNode::Empty),
        );
        let other_node = std::mem::replace(
            &mut entries[other_idx],
            AlignedNode::RightOnly(StructuredNode::Empty),
        );

        if let (AlignedNode::LeftOnly(base_sn), AlignedNode::RightOnly(other_sn)) =
            (&base_node, &other_node)
        {
            let merged = merge_node(base_sn, base_lang, other_sn, other_lang);
            entries[base_idx] = AlignedNode::Matched(merged);
        }
        to_remove[other_idx] = true;
    }

    for i in (0..len).rev() {
        if to_remove[i] {
            entries.remove(i);
        }
    }
}

/// Post-process aligned entries to absorb orphaned `Paragraph` nodes into an
/// adjacent orphaned `Field` node's label when the paragraph represents the
/// field's label in another language.
///
/// This handles the case where the label-attacher in one language correctly
/// attaches a label to a field (e.g. DE: Field with label "Firma"), but in
/// another language the same text is a standalone paragraph (e.g. EN:
/// Paragraph("Company")) because of geometry differences in the PDF.
///
/// The pattern detected is:
///   LeftOnly(Field) adjacent to RightOnly(Paragraph)  or
///   RightOnly(Field) adjacent to LeftOnly(Paragraph)
///
/// Guards against false matches:
/// - Only short paragraphs (≤ 80 chars) are absorbed — long text is unlikely a label.
/// - The field must not already have a non-empty label for the paragraph's language.
///
/// The paragraph text is absorbed as the field's label for the paragraph's
/// language, and the field becomes a Matched node with bilingual labels.
fn consolidate_orphan_paragraph_into_field_label(
    entries: &mut Vec<AlignedNode>,
    base_lang: &str,
    other_lang: &str,
) {
    /// Maximum paragraph length (in characters) to be considered a field label.
    const MAX_LABEL_LEN: usize = 80;

    let len = entries.len();

    // Collect (field_idx, para_idx) operations.
    let mut ops: Vec<(usize, usize)> = Vec::new();
    let mut para_consumed = vec![false; len];

    for i in 0..len {
        // Check if this is an orphan Field (from either side).
        let (is_left_field, is_right_field) = match &entries[i] {
            AlignedNode::LeftOnly(StructuredNode::Field(_)) => (true, false),
            AlignedNode::RightOnly(StructuredNode::Field(_)) => (false, true),
            _ => continue,
        };

        // Look at the immediate neighbor (after, then before) for an orphan
        // Paragraph from the OTHER side.
        for delta in [1isize, -1isize] {
            let j = i as isize + delta;
            if j < 0 || j as usize >= len {
                continue;
            }
            let j = j as usize;
            if para_consumed[j] {
                continue;
            }

            let is_candidate = match &entries[j] {
                AlignedNode::RightOnly(StructuredNode::Paragraph(p)) if is_left_field => {
                    let text = p.content.as_plain_text();
                    let trimmed = text.trim();
                    !trimmed.is_empty() && trimmed.len() <= MAX_LABEL_LEN
                }
                AlignedNode::LeftOnly(StructuredNode::Paragraph(p)) if is_right_field => {
                    let text = p.content.as_plain_text();
                    let trimmed = text.trim();
                    !trimmed.is_empty() && trimmed.len() <= MAX_LABEL_LEN
                }
                _ => false,
            };

            if is_candidate {
                ops.push((i, j));
                para_consumed[j] = true;
                break;
            }
        }
    }

    if ops.is_empty() {
        return;
    }

    // Apply: absorb paragraph text into field label.
    let mut to_remove = vec![false; len];
    for (field_idx, para_idx) in ops {
        let para_text = match &entries[para_idx] {
            AlignedNode::LeftOnly(StructuredNode::Paragraph(p))
            | AlignedNode::RightOnly(StructuredNode::Paragraph(p)) => {
                Some(p.content.as_plain_text())
            }
            _ => continue,
        };
        let para_lang = match &entries[para_idx] {
            AlignedNode::LeftOnly(_) => base_lang,
            _ => other_lang,
        };
        let field_lang = match &entries[field_idx] {
            AlignedNode::LeftOnly(_) => base_lang,
            _ => other_lang,
        };

        // Take the field node out, localize it, update label, and make it Matched.
        let field_node = std::mem::replace(
            &mut entries[field_idx],
            AlignedNode::LeftOnly(StructuredNode::Empty),
        );

        let raw_field = match field_node {
            AlignedNode::LeftOnly(n) | AlignedNode::RightOnly(n) => n,
            _ => continue,
        };

        // Localize the field to its own language first.
        let mut localized = localize_structured_node(&raw_field, field_lang);

        if let StructuredNode::Field(f) = &mut localized {
            // Add the paragraph text as label for the paragraph's language.
            let label = f.label.get_or_insert_with(TranslatedText::empty);
            if let Some(para_text_str) = &para_text {
                label.insert(
                    para_lang.to_string(),
                    InlineText::plain(para_text_str.as_str()),
                );
            }
        }

        entries[field_idx] = AlignedNode::Matched(localized);
        to_remove[para_idx] = true;
    }

    for i in (0..len).rev() {
        if to_remove[i] {
            entries.remove(i);
        }
    }
}

// ============================================================================
// Neighborhood-based consolidation
// ============================================================================

/// Post-process aligned entries to pair adjacent `LeftOnly` + `RightOnly` nodes
/// of the same variant that LCS failed to match (e.g. because text shapes diverged
/// too much between languages like DE compound words vs EN multi-word phrases).
///
/// For each consecutive `LeftOnly`/`RightOnly` pair of the same top-level variant
/// (both Paragraph, both Heading, etc.), compute a neighborhood score based on
/// the anchor keys of surrounding `Matched` entries. If the score exceeds the
/// threshold, merge them into a `Matched` node.
fn consolidate_by_neighborhood(entries: &mut Vec<AlignedNode>, base_lang: &str, other_lang: &str) {
    let len = entries.len();
    if len < 2 {
        return;
    }

    // Collect anchor keys from Matched entries for neighborhood lookup.
    let anchor_keys: Vec<Option<String>> = entries
        .iter()
        .map(|e| match e {
            AlignedNode::Matched(node) => node.anchor_key(),
            _ => None,
        })
        .collect();

    // Find pairs to consolidate: LeftOnly(X) + RightOnly(X) or vice versa,
    // where X is the same top-level variant.
    let mut ops: Vec<(usize, usize)> = Vec::new(); // (left_idx, right_idx) to merge
    let mut consumed = vec![false; len];

    for i in 0..len {
        if consumed[i] {
            continue;
        }

        let (left_node, is_left) = match &entries[i] {
            AlignedNode::LeftOnly(n) => (n, true),
            AlignedNode::RightOnly(n) => (n, false),
            _ => continue,
        };

        // Look at the next few entries for a complementary orphan.
        for j in (i + 1)..len.min(i + 7) {
            if consumed[j] {
                continue;
            }

            let right_node = match (&entries[j], is_left) {
                (AlignedNode::RightOnly(n), true) => n,
                (AlignedNode::LeftOnly(n), false) => n,
                // Skip over matched entries — don't stop searching at boundaries.
                // A compound-word orphan and its complement may be separated by a
                // matched anchor when the two languages have different field/heading
                // orderings (e.g. DE: [H, F] vs ES: [F, H]).
                (AlignedNode::Matched(_), _) => continue,
                _ => continue,
            };

            // Must be same top-level variant
            if std::mem::discriminant(left_node) != std::mem::discriminant(right_node) {
                continue;
            }

            // Compute neighborhood score
            let score = neighborhood_score(&anchor_keys, i, j);
            if score >= 0.5 {
                ops.push((i, j));
                consumed[i] = true;
                consumed[j] = true;
                break;
            }
        }
    }

    if ops.is_empty() {
        return;
    }

    // Apply merges
    let mut to_remove = vec![false; len];
    for (left_idx, right_idx) in ops {
        let (base_node, other_node) = {
            let a = std::mem::replace(
                &mut entries[left_idx],
                AlignedNode::Matched(StructuredNode::Empty),
            );
            let b = std::mem::replace(
                &mut entries[right_idx],
                AlignedNode::Matched(StructuredNode::Empty),
            );
            match (a, b) {
                (AlignedNode::LeftOnly(base), AlignedNode::RightOnly(other)) => (base, other),
                (AlignedNode::RightOnly(other), AlignedNode::LeftOnly(base)) => (base, other),
                _ => continue,
            }
        };

        let merged = merge_node(&base_node, base_lang, &other_node, other_lang);
        entries[left_idx] = AlignedNode::Matched(merged);
        to_remove[right_idx] = true;
    }

    for i in (0..len).rev() {
        if to_remove[i] {
            entries.remove(i);
        }
    }
}

/// Compute a neighborhood score for positions `i` and `j` in an aligned entry list.
///
/// Looks at the k nearest anchor-carrying `Matched` entries before and after
/// each position. Returns the fraction of neighboring anchors that are shared
/// between the two positions' neighborhoods.
fn neighborhood_score(anchor_keys: &[Option<String>], i: usize, j: usize) -> f64 {
    const K: usize = 3;
    let len = anchor_keys.len();

    // Collect up to K anchor keys before the smaller index
    let min_pos = i.min(j);
    let max_pos = i.max(j);
    let mut before_keys = Vec::new();
    for pos in (0..min_pos).rev() {
        if let Some(key) = &anchor_keys[pos] {
            before_keys.push(key.as_str());
            if before_keys.len() >= K {
                break;
            }
        }
    }

    // Collect up to K anchor keys after the larger index
    let mut after_keys = Vec::new();
    for pos in (max_pos + 1)..len {
        if let Some(key) = &anchor_keys[pos] {
            after_keys.push(key.as_str());
            if after_keys.len() >= K {
                break;
            }
        }
    }

    let total = before_keys.len() + after_keys.len();
    if total == 0 {
        // No surrounding anchors — accept the match if positions are adjacent.
        return if max_pos - min_pos <= 2 { 0.5 } else { 0.0 };
    }

    // Score = fraction of surrounding anchors that exist (they all match by
    // definition since they come from Matched entries on both sides).
    // The key insight: if both orphan nodes are surrounded by the same matched
    // anchors, they likely correspond to the same logical position.
    total as f64 / (2 * K) as f64
}

// ============================================================================
// Recursive node merging (translation)
// ============================================================================

/// Merge two structurally-equivalent nodes from different languages.
/// Combines text content into multilingual `TranslatedText` / `TranslatableString`.
fn merge_node(
    base: &StructuredNode,
    base_lang: &str,
    other: &StructuredNode,
    other_lang: &str,
) -> StructuredNode {
    match (base, other) {
        (StructuredNode::Heading(a), StructuredNode::Heading(b)) => {
            StructuredNode::Heading(HeadingNode {
                level: a.level,
                content: merge_translated_text(&a.content, base_lang, &b.content, other_lang),
                som_path: a.som_path.clone(),
                source_name: a.source_name.clone(),
            })
        }
        (StructuredNode::Paragraph(a), StructuredNode::Paragraph(b)) => {
            StructuredNode::Paragraph(ParagraphNode {
                content: merge_translated_text(&a.content, base_lang, &b.content, other_lang),
                som_path: a.som_path.clone(),
                source_name: a.source_name.clone(),
            })
        }
        (StructuredNode::Notice(a), StructuredNode::Notice(b)) => {
            StructuredNode::Notice(NoticeNode {
                content: merge_translated_text(&a.content, base_lang, &b.content, other_lang),
                som_path: a.som_path.clone(),
                source_name: a.source_name.clone(),
            })
        }
        (StructuredNode::Image(a), StructuredNode::Image(_b)) => StructuredNode::Image(a.clone()),
        (StructuredNode::Table(a), StructuredNode::Table(b)) => {
            StructuredNode::Table(merge_table(a, base_lang, b, other_lang))
        }
        (StructuredNode::Field(a), StructuredNode::Field(b)) => {
            StructuredNode::Field(merge_field(a, base_lang, b, other_lang))
        }
        (StructuredNode::Repeatable(a), StructuredNode::Repeatable(b)) => {
            StructuredNode::Repeatable(RepeatableNode {
                item: Box::new(merge_node(&a.item, base_lang, &b.item, other_lang)),
                min_occurrences: a.min_occurrences,
                max_occurrences: a.max_occurrences,
            })
        }
        (StructuredNode::Group(a), StructuredNode::Group(b)) => {
            let children = merge_node_lists(&a.children, base_lang, &b.children, other_lang);
            // Both languages render the same layout, so a column flow detected
            // in either one holds for the merged group. Losing it here would
            // flatten the columns of every multilingual document.
            StructuredNode::Group(GroupNode {
                children,
                column_flow: a.column_flow || b.column_flow,
            })
        }
        (StructuredNode::Conditional(a), StructuredNode::Conditional(b)) => {
            StructuredNode::Conditional(ConditionalNode {
                condition: a.condition.clone(),
                content: Box::new(merge_node(&a.content, base_lang, &b.content, other_lang)),
            })
        }
        (StructuredNode::Empty, StructuredNode::Empty) => StructuredNode::Empty,
        (StructuredNode::GridLayout(a), StructuredNode::GridLayout(b)) => {
            let elements = merge_grid_elements(&a.elements, base_lang, &b.elements, other_lang);
            StructuredNode::GridLayout(GridLayout {
                columns: a.columns,
                elements,
            })
        }
        (StructuredNode::List(a), StructuredNode::List(b)) => {
            let items = merge_list_items(&a.items, base_lang, &b.items, other_lang);
            StructuredNode::List(ListNode {
                list_style: a.list_style,
                items,
            })
        }
        // Mismatched variants can occur in recursive cases (e.g. different
        // sub-structures inside Repeatable or Conditional content).  Keep base.
        //
        // Conditional vs non-Conditional: one language may wrap content in a
        // Conditional due to exhaustive-state differences. Merge the inner
        // content and preserve the Conditional wrapper.
        (StructuredNode::Conditional(c), other) => StructuredNode::Conditional(ConditionalNode {
            condition: c.condition.clone(),
            content: Box::new(merge_node(&c.content, base_lang, other, other_lang)),
        }),
        (other, StructuredNode::Conditional(c)) => StructuredNode::Conditional(ConditionalNode {
            condition: c.condition.clone(),
            content: Box::new(merge_node(other, base_lang, &c.content, other_lang)),
        }),
        _ => base.clone(),
    }
}

// ============================================================================
// InlineText merging
// ============================================================================

/// Merge two `InlineText`s from different languages.
///
/// Merge two `TranslatedText`s by combining their language maps.
///
/// Each language's InlineText is preserved independently. If both sides
/// have the same language key (e.g. when merging a 3rd language into an
/// already-merged document), the base side wins.
fn merge_translated_text(
    base: &TranslatedText,
    _base_lang: &str,
    other: &TranslatedText,
    _other_lang: &str,
) -> TranslatedText {
    let mut map = base.0.clone();
    for (lang, text) in &other.0 {
        map.entry(lang.clone()).or_insert_with(|| text.clone());
    }
    TranslatedText::new(map)
}

// ============================================================================
// Field merging
// ============================================================================

/// Merge two `Option<T>` values, using a merge function when both are `Some`.
fn merge_option<T: Clone>(
    base: &Option<T>,
    other: &Option<T>,
    merge_fn: impl FnOnce(&T, &T) -> T,
) -> Option<T> {
    match (base, other) {
        (Some(a), Some(b)) => Some(merge_fn(a, b)),
        (Some(a), None) => Some(a.clone()),
        (None, Some(b)) => Some(b.clone()),
        (None, None) => None,
    }
}

/// Merge two `FieldNode`s from different languages.
fn merge_field(
    base: &FieldNode,
    base_lang: &str,
    other: &FieldNode,
    other_lang: &str,
) -> FieldNode {
    let label = merge_option(&base.label, &other.label, |a, b| {
        merge_translated_text(a, base_lang, b, other_lang)
    });

    let placeholder = merge_option(&base.placeholder, &other.placeholder, |a, b| {
        a.merge(base_lang, b, other_lang)
    });

    let input_type = merge_field_type(&base.input_type, base_lang, &other.input_type, other_lang);

    FieldNode {
        name: base.name.clone(),
        som_path: base.som_path.clone(),
        label,
        input_type,
        value: base.value.clone(),
        placeholder,
        required: base.required,
    }
}

/// Merge two `FieldType`s, combining option names for Radio/Select.
fn merge_field_type(
    base: &FieldType,
    base_lang: &str,
    other: &FieldType,
    other_lang: &str,
) -> FieldType {
    match (base, other) {
        (FieldType::Radio { options: opts_a }, FieldType::Radio { options: opts_b }) => {
            merge_option_field_type(opts_a, base_lang, opts_b, other_lang, |options| {
                FieldType::Radio { options }
            })
        }
        (FieldType::Select { options: opts_a }, FieldType::Select { options: opts_b }) => {
            merge_option_field_type(opts_a, base_lang, opts_b, other_lang, |options| {
                FieldType::Select { options }
            })
        }
        (
            FieldType::CheckboxGroup { options: opts_a },
            FieldType::CheckboxGroup { options: opts_b },
        ) => merge_option_field_type(opts_a, base_lang, opts_b, other_lang, |options| {
            FieldType::CheckboxGroup { options }
        }),
        _ => base.clone(),
    }
}

/// Merge option-bearing field variants (e.g. radio/select) with shared logic.
fn merge_option_field_type<F>(
    base_options: &[NameValue],
    base_lang: &str,
    other_options: &[NameValue],
    other_lang: &str,
    build: F,
) -> FieldType
where
    F: FnOnce(Vec<NameValue>) -> FieldType,
{
    let options = merge_name_values(base_options, base_lang, other_options, other_lang);
    build(options)
}

/// Merge two `GridLayout` element vectors.
fn merge_grid_elements(
    base: &[GridLayoutElement],
    base_lang: &str,
    other: &[GridLayoutElement],
    other_lang: &str,
) -> Vec<GridLayoutElement> {
    if base.len() != other.len() {
        log::warn!(
            "GridLayout element count mismatch when merging {} and {} translations: \
             {} vs {} elements; unmatched elements will be preserved from the longer side",
            base_lang,
            other_lang,
            base.len(),
            other.len()
        );
    }
    let paired = base.len().min(other.len());
    let mut elements: Vec<GridLayoutElement> = base
        .iter()
        .zip(other.iter())
        .map(|(ea, eb)| GridLayoutElement {
            span: ea.span,
            node: merge_node(&ea.node, base_lang, &eb.node, other_lang),
        })
        .collect();
    extend_unmatched_tail(&mut elements, base, other, paired);
    elements
}

/// Append cloned unmatched tail elements from both sides after a paired prefix.
fn extend_unmatched_tail<T: Clone>(out: &mut Vec<T>, base: &[T], other: &[T], paired: usize) {
    out.extend(base[paired..].iter().cloned());
    out.extend(other[paired..].iter().cloned());
}

/// Merge two `List` item vectors.
fn merge_list_items(
    base: &[ListItem],
    base_lang: &str,
    other: &[ListItem],
    other_lang: &str,
) -> Vec<ListItem> {
    if base.len() != other.len() {
        log::warn!(
            "List item count mismatch when merging {} and {} translations: \
             {} vs {} items; unmatched items will be preserved from the longer side",
            base_lang,
            other_lang,
            base.len(),
            other.len()
        );
    }
    let paired = base.len().min(other.len());
    let mut items: Vec<ListItem> = base
        .iter()
        .zip(other.iter())
        .map(|(ia, ib)| {
            let content = merge_translated_text(&ia.content, base_lang, &ib.content, other_lang);
            // For sublists, prefer base's sublist (sublists are typically language-independent)
            let sublist = ia.sublist.clone().or_else(|| ib.sublist.clone());
            ListItem { content, sublist }
        })
        .collect();
    append_unmatched_list_items(&mut items, &base[paired..], base_lang);
    append_unmatched_list_items(&mut items, &other[paired..], other_lang);
    items
}

/// Append unmatched list items as single-language translated entries.
fn append_unmatched_list_items(items: &mut Vec<ListItem>, unmatched: &[ListItem], language: &str) {
    for item in unmatched {
        let content = localize_translated_text(&item.content, language);
        items.push(ListItem {
            content,
            sublist: item.sublist.clone(),
        });
    }
}

/// Merge two `NameValue` vectors by zipping and merging names.
fn merge_name_values(
    base: &[NameValue],
    base_lang: &str,
    other: &[NameValue],
    other_lang: &str,
) -> Vec<NameValue> {
    if base.len() != other.len() {
        log::warn!(
            "Option count mismatch when merging {} and {} translations: \
             {} vs {} options; unmatched options will be preserved as \
             single-language entries",
            base_lang,
            other_lang,
            base.len(),
            other.len()
        );
    }
    let paired = base.len().min(other.len());
    let mut options: Vec<NameValue> = base
        .iter()
        .zip(other.iter())
        .map(|(a, b)| NameValue {
            name: a.name.merge(base_lang, &b.name, other_lang),
            value: a.value.clone(),
        })
        .collect();
    append_unmatched_name_values(&mut options, &base[paired..], base_lang);
    append_unmatched_name_values(&mut options, &other[paired..], other_lang);
    options
}

/// Append unmatched option entries as single-language translated entries.
fn append_unmatched_name_values(
    options: &mut Vec<NameValue>,
    unmatched: &[NameValue],
    language: &str,
) {
    for entry in unmatched {
        let name = localize_translatable_string_for_language(&entry.name, language);
        options.push(NameValue {
            name,
            value: entry.value.clone(),
        });
    }
}

/// Wrap a translatable value as a single-language translated entry when needed.
fn localize_translatable_string_for_language(
    value: &TranslatableString,
    language: &str,
) -> TranslatableString {
    match value {
        TranslatableString::Plain(s) => {
            TranslatableString::Translated(HashMap::from([(language.to_string(), Some(s.clone()))]))
        }
        TranslatableString::Translated(m) => TranslatableString::Translated(m.clone()),
    }
}

// ============================================================================
// Table merging
// ============================================================================

/// Merge two `TableNode`s from different languages.
pub(crate) fn merge_table(
    base: &TableNode,
    base_lang: &str,
    other: &TableNode,
    other_lang: &str,
) -> TableNode {
    let header = merge_option(&base.header, &other.header, |h1, h2| {
        let cells = merge_node_lists(&h1.cells, base_lang, &h2.cells, other_lang);
        TableHeader { cells }
    });

    let rows: Vec<TableRow> = {
        if base.rows.len() != other.rows.len() {
            log::warn!(
                "Table row count mismatch when merging {} and {} translations: \
                 {} vs {} rows; unmatched rows will be preserved from the longer side",
                base_lang,
                other_lang,
                base.rows.len(),
                other.rows.len()
            );
        }
        let paired = base.rows.len().min(other.rows.len());
        let mut rows: Vec<TableRow> = base
            .rows
            .iter()
            .zip(other.rows.iter())
            .map(|(r1, r2)| {
                let cells = merge_node_lists(&r1.cells, base_lang, &r2.cells, other_lang);
                TableRow { cells }
            })
            .collect();
        extend_unmatched_tail(&mut rows, &base.rows, &other.rows, paired);
        rows
    };

    let caption = merge_option(&base.caption, &other.caption, |a, b| {
        merge_translated_text(a, base_lang, b, other_lang)
    });

    TableNode {
        header,
        rows,
        caption,
    }
}
