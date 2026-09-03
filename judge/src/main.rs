use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use blueprint::semantic::SemanticMatcher;
use blueprint::{
    DocumentEnvelope, StructuredNode, TranslatableString, TranslatedText,
    structured::{calculate_structural_similarity, merge_translations},
};
use clap::Parser;
use rayon::prelude::*;

/// Judge — evaluate translation quality of multi-language PDF forms.
#[derive(Parser)]
#[command(name = "judge")]
struct Args {
    /// Only process a specific form code (e.g. ABCD_019). If omitted, all forms are processed.
    #[arg(long)]
    form_code: Option<String>,

    /// Directory to discover source PDFs in.
    #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/../core/input"))]
    input_dir: PathBuf,

    /// Conversion profile whose fonts and configuration to load.
    #[arg(long, default_value = "ubs")]
    profile: String,

    /// Where to write the scores CSV.
    #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/results.csv"))]
    output: PathBuf,
}

struct FormResult {
    form_code: String,
    status: String,
    translation_rating: f64,
    missing_translation_score: f64,
    semantic_match_score: f64,
    labelled_fields_score: f64,
    total_score: f64,
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    // Fonts must be registered before any form is rendered or measured.
    blueprint::load_profile_fonts(&args.profile).map_err(|e| anyhow::anyhow!("{e}"))?;

    // Load semantic matcher once for merge + scoring
    eprintln!("Loading semantic matcher...");
    let matcher = SemanticMatcher::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    eprintln!("Semantic matcher loaded.");

    let mut forms = discover_forms(&args.input_dir)?;

    // Filter to a single form code if requested
    if let Some(ref filter) = args.form_code {
        forms.retain(|key, _| key == filter);
        if forms.is_empty() {
            anyhow::bail!("No form found matching '{filter}'");
        }
    }

    eprintln!("Processing {} form codes (parallel)", forms.len());

    let mut results: Vec<FormResult> = forms
        .par_iter()
        .map(|(form_code, variants)| {
            let result = process_form(form_code, variants, &matcher);
            match &result {
                r if r.status == "pass" => {
                    eprintln!("{form_code}: pass (score: {:.3})", r.total_score)
                }
                r => eprintln!("{form_code}: FAIL ({})", r.status),
            }
            result
        })
        .collect();

    results.sort_by(|a, b| b.total_score.partial_cmp(&a.total_score).unwrap());

    let mut wtr = csv::Writer::from_path(&args.output)?;
    wtr.write_record([
        "form_code",
        "status",
        "translation_rating",
        "missing_translation_score",
        "semantic_match_score",
        "labelled_fields_score",
        "total_score",
    ])?;
    for r in &results {
        wtr.write_record([
            &r.form_code,
            &r.status,
            &format!("{:.3}", r.translation_rating),
            &format!("{:.3}", r.missing_translation_score),
            &format!("{:.3}", r.semantic_match_score),
            &format!("{:.3}", r.labelled_fields_score),
            &format!("{:.3}", r.total_score),
        ])?;
    }

    // Write average row
    if !results.is_empty() {
        let n = results.len() as f64;
        let avg_translation = results.iter().map(|r| r.translation_rating).sum::<f64>() / n;
        let avg_missing = results
            .iter()
            .map(|r| r.missing_translation_score)
            .sum::<f64>()
            / n;
        let avg_semantic = results.iter().map(|r| r.semantic_match_score).sum::<f64>() / n;
        let avg_labelled = results.iter().map(|r| r.labelled_fields_score).sum::<f64>() / n;
        let avg_total = results.iter().map(|r| r.total_score).sum::<f64>() / n;
        wtr.write_record([
            "AVERAGE",
            "",
            &format!("{:.3}", avg_translation),
            &format!("{:.3}", avg_missing),
            &format!("{:.3}", avg_semantic),
            &format!("{:.3}", avg_labelled),
            &format!("{:.3}", avg_total),
        ])?;
    }

    wtr.flush()?;

    eprintln!("Results written to {}", args.output.display());

    Ok(())
}

/// Discover all PDFs matching {CODE}_{VERSION}_{LANG}.pdf and group by code + version.
/// Different versions (e.g. 019 vs 033) are different entity forms and must not be merged.
/// A discovered form variant: (version, language, path).
type FormVariant = (String, String, PathBuf);

fn discover_forms(input_dir: &Path) -> Result<BTreeMap<String, Vec<FormVariant>>> {
    let mut forms: BTreeMap<String, Vec<FormVariant>> = BTreeMap::new();

    for entry in std::fs::read_dir(input_dir).context("Failed to read input directory")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("pdf") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();

        // Parse {CODE}_{VERSION}_{LANG}
        let parts: Vec<&str> = stem.split('_').collect();
        if parts.len() != 3 {
            log::debug!("Skipping non-standard filename: {stem}");
            continue;
        }
        let (code, version, lang) = (parts[0], parts[1], parts[2]);

        // Only 4-letter form codes
        if code.len() != 4 || !code.chars().all(|c| c.is_ascii_uppercase()) {
            log::debug!("Skipping non-4-letter code: {code}");
            continue;
        }

        let key = format!("{code}_{version}");
        forms
            .entry(key)
            .or_default()
            .push((version.to_string(), lang.to_lowercase(), path));
    }

    Ok(forms)
}

/// Process a single form code: run pipeline, compute all ratings.
fn process_form(
    form_code: &str,
    variants: &[FormVariant],
    matcher: &SemanticMatcher,
) -> FormResult {
    let fail = |msg: &str| FormResult {
        form_code: form_code.to_string(),
        status: format!("fail: {msg}"),
        translation_rating: 0.0,
        missing_translation_score: 0.0,
        semantic_match_score: 0.0,
        labelled_fields_score: 0.0,
        total_score: 0.0,
    };

    // Step 1: Run exhaustive-to-envelope for each language variant
    let mut envelopes: Vec<DocumentEnvelope> = Vec::new();
    for (_version, lang, path) in variants {
        match blueprint::run_exhaustive_to_envelope(path, lang) {
            Ok(env) => envelopes.push(env),
            Err(e) => return fail(&format!("envelope({lang}): {e}")),
        }
    }

    if envelopes.is_empty() {
        return fail("no envelopes");
    }

    // Step 2: Labelled fields score — average across all envelopes
    let labelled_fields_score = {
        let scores: Vec<f64> = envelopes
            .iter()
            .map(|env| compute_labelled_fields_score(&env.content))
            .collect();
        scores.iter().sum::<f64>() / scores.len() as f64
    };

    // Step 3: Translation rating and merge
    let (translation_rating, missing_translation_score, semantic_match_score) = if envelopes.len()
        == 1
    {
        (1.0, 1.0, 1.0)
    } else {
        // Compute pairwise structural similarity
        let mut similarities = Vec::new();
        for i in 0..envelopes.len() {
            for j in (i + 1)..envelopes.len() {
                let sim =
                    calculate_structural_similarity(&envelopes[i].content, &envelopes[j].content);
                similarities.push(sim);
            }
        }
        let translation_rating = if similarities.is_empty() {
            1.0
        } else {
            similarities.iter().sum::<f64>() / similarities.len() as f64
        };

        let semantic_ref = Some(matcher as &blueprint::structured::SemanticCtx);

        let merged = match merge_translations(envelopes, semantic_ref) {
            Ok(m) => m,
            Err(e) => return fail(&format!("merge: {e}")),
        };

        // Compute missing translation score
        let missing_translation_score = compute_missing_translation_score(&merged.content);

        // Compute semantic match score (always, regardless of feature)
        let semantic_match_score = compute_semantic_match_score(&merged.content, matcher);

        (
            translation_rating,
            missing_translation_score,
            semantic_match_score,
        )
    };

    let total_score = translation_rating
        * missing_translation_score
        * semantic_match_score
        * labelled_fields_score;

    FormResult {
        form_code: form_code.to_string(),
        status: "pass".to_string(),
        translation_rating,
        missing_translation_score,
        semantic_match_score,
        labelled_fields_score,
        total_score,
    }
}

// =============================================================================
// Labelled fields score: labelled_fields / total_fields
// =============================================================================

fn compute_labelled_fields_score(nodes: &[StructuredNode]) -> f64 {
    let (total, labelled) = count_fields(nodes);
    if total == 0 {
        return 1.0;
    }
    labelled as f64 / total as f64
}

fn count_fields(nodes: &[StructuredNode]) -> (usize, usize) {
    let mut total = 0;
    let mut labelled = 0;

    for node in nodes {
        match node {
            StructuredNode::Field(f) => {
                total += 1;
                if f.label.as_ref().is_some_and(|l| !l.is_empty()) {
                    labelled += 1;
                }
            }
            StructuredNode::Group(g) => {
                let (t, l) = count_fields(&g.children);
                total += t;
                labelled += l;
            }
            StructuredNode::Conditional(cond) => {
                let (t, l) = count_fields(std::slice::from_ref(&cond.content));
                total += t;
                labelled += l;
            }
            StructuredNode::Repeatable(r) => {
                let (t, l) = count_fields(std::slice::from_ref(&r.item));
                total += t;
                labelled += l;
            }
            StructuredNode::Table(table) => {
                if let Some(header) = &table.header {
                    let (t, l) = count_fields(&header.cells);
                    total += t;
                    labelled += l;
                }
                for row in &table.rows {
                    let (t, l) = count_fields(&row.cells);
                    total += t;
                    labelled += l;
                }
            }
            StructuredNode::GridLayout(grid) => {
                for elem in &grid.elements {
                    let (t, l) = count_fields(std::slice::from_ref(&elem.node));
                    total += t;
                    labelled += l;
                }
            }
            _ => {}
        }
    }

    (total, labelled)
}

// =============================================================================
// Missing translation score: 1.0 - (missing / total)
// =============================================================================

fn compute_missing_translation_score(nodes: &[StructuredNode]) -> f64 {
    let (total_slots, missing_slots) = count_translation_slots(nodes);
    if total_slots == 0 {
        return 1.0;
    }
    1.0 - (missing_slots as f64 / total_slots as f64)
}

/// Count total translation slots and missing translation slots in the tree.
fn count_translation_slots(nodes: &[StructuredNode]) -> (usize, usize) {
    let mut total = 0;
    let mut missing = 0;

    for node in nodes {
        match node {
            StructuredNode::Heading(h) => {
                count_inline_text_slots(&h.content, &mut total, &mut missing)
            }
            StructuredNode::Notice(n) => {
                count_inline_text_slots(&n.content, &mut total, &mut missing)
            }
            StructuredNode::Paragraph(p) => {
                count_inline_text_slots(&p.content, &mut total, &mut missing)
            }
            StructuredNode::Field(f) => {
                if let Some(label) = &f.label {
                    count_inline_text_slots(label, &mut total, &mut missing);
                }
                if let Some(placeholder) = &f.placeholder {
                    count_translatable_string_slots(placeholder, &mut total, &mut missing);
                }
                // Check option names for Radio/Select
                match &f.input_type {
                    blueprint::FieldType::Radio { options }
                    | blueprint::FieldType::Select { options } => {
                        for opt in options {
                            count_translatable_string_slots(&opt.name, &mut total, &mut missing);
                        }
                    }
                    _ => {}
                }
            }
            StructuredNode::Table(table) => {
                if let Some(caption) = &table.caption {
                    count_inline_text_slots(caption, &mut total, &mut missing);
                }
                if let Some(header) = &table.header {
                    let (t, m) = count_translation_slots(&header.cells);
                    total += t;
                    missing += m;
                }
                for row in &table.rows {
                    let (t, m) = count_translation_slots(&row.cells);
                    total += t;
                    missing += m;
                }
            }
            StructuredNode::Group(g) => {
                let (t, m) = count_translation_slots(&g.children);
                total += t;
                missing += m;
            }
            StructuredNode::Conditional(cond) => {
                let (t, m) = count_translation_slots(std::slice::from_ref(&cond.content));
                total += t;
                missing += m;
            }
            StructuredNode::Repeatable(r) => {
                let (t, m) = count_translation_slots(std::slice::from_ref(&r.item));
                total += t;
                missing += m;
            }
            StructuredNode::GridLayout(grid) => {
                for elem in &grid.elements {
                    let (t, m) = count_translation_slots(std::slice::from_ref(&elem.node));
                    total += t;
                    missing += m;
                }
            }
            StructuredNode::List(list) => {
                for item in &list.items {
                    count_inline_text_slots(&item.content, &mut total, &mut missing);
                    if let Some(sub) = &item.sublist {
                        for sub_item in &sub.items {
                            count_inline_text_slots(&sub_item.content, &mut total, &mut missing);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    (total, missing)
}

fn count_inline_text_slots(text: &TranslatedText, total: &mut usize, missing: &mut usize) {
    for (_lang, inline) in text.iter() {
        *total += 1;
        if inline.is_empty() {
            *missing += 1;
        }
    }
}

fn count_translatable_string_slots(
    ts: &TranslatableString,
    total: &mut usize,
    missing: &mut usize,
) {
    if let TranslatableString::Translated(map) = ts {
        for value in map.values() {
            *total += 1;
            if value.is_none() {
                *missing += 1;
            }
        }
    }
}

// =============================================================================
// Semantic match score: average pairwise cosine similarity of matched translations
// =============================================================================

/// Compute how well matched translations correspond semantically.
///
/// Walks the merged tree, collects every `TranslatedText` node that has at
/// least two non-missing language variants, embeds them with the multilingual
/// model, computes pairwise cosine similarity, and returns the average.
fn compute_semantic_match_score(nodes: &[StructuredNode], matcher: &SemanticMatcher) -> f64 {
    // Collect all text pairs to evaluate
    let mut text_groups: Vec<Vec<String>> = Vec::new();
    collect_translated_text_groups(nodes, &mut text_groups);

    if text_groups.is_empty() {
        return 1.0;
    }

    // Flatten all texts for batch embedding
    let mut all_texts: Vec<&str> = Vec::new();
    let mut group_ranges: Vec<(usize, usize)> = Vec::new();
    for group in &text_groups {
        let start = all_texts.len();
        for text in group {
            all_texts.push(text.as_str());
        }
        group_ranges.push((start, all_texts.len()));
    }

    // Batch embed all texts at once
    let embeddings = match matcher.embed_batch(&all_texts) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("Semantic embedding failed, skipping score: {e}");
            return 1.0;
        }
    };

    // Compute pairwise similarity within each group
    let mut total_sim = 0.0;
    let mut total_pairs = 0usize;

    for (start, end) in &group_ranges {
        let count = end - start;
        if count < 2 {
            continue;
        }
        for i in *start..*end {
            for j in (i + 1)..*end {
                let sim = SemanticMatcher::cosine_similarity(&embeddings[i], &embeddings[j]);
                total_sim += sim as f64;
                total_pairs += 1;
            }
        }
    }

    if total_pairs == 0 {
        return 1.0;
    }

    (total_sim / total_pairs as f64).clamp(0.0, 1.0)
}

/// Collect groups of non-missing translation texts from TranslatedText nodes.
/// Each group contains the text values for each language in one TranslatedText node.
fn collect_translated_text_groups(nodes: &[StructuredNode], out: &mut Vec<Vec<String>>) {
    for node in nodes {
        match node {
            StructuredNode::Heading(h) => {
                collect_from_inline_text(&h.content, out);
            }
            StructuredNode::Paragraph(p) => {
                collect_from_inline_text(&p.content, out);
            }
            StructuredNode::Notice(n) => {
                collect_from_inline_text(&n.content, out);
            }
            StructuredNode::Field(f) => {
                if let Some(label) = &f.label {
                    collect_from_inline_text(label, out);
                }
                if let Some(placeholder) = &f.placeholder {
                    collect_from_translatable_string(placeholder, out);
                }
                match &f.input_type {
                    blueprint::FieldType::Radio { options }
                    | blueprint::FieldType::Select { options } => {
                        for opt in options {
                            collect_from_translatable_string(&opt.name, out);
                        }
                    }
                    _ => {}
                }
            }
            StructuredNode::Table(table) => {
                if let Some(caption) = &table.caption {
                    collect_from_inline_text(caption, out);
                }
                if let Some(header) = &table.header {
                    collect_translated_text_groups(&header.cells, out);
                }
                for row in &table.rows {
                    collect_translated_text_groups(&row.cells, out);
                }
            }
            StructuredNode::Group(g) => {
                collect_translated_text_groups(&g.children, out);
            }
            StructuredNode::Conditional(cond) => {
                collect_translated_text_groups(std::slice::from_ref(&cond.content), out);
            }
            StructuredNode::Repeatable(r) => {
                collect_translated_text_groups(std::slice::from_ref(&r.item), out);
            }
            StructuredNode::GridLayout(grid) => {
                for elem in &grid.elements {
                    collect_translated_text_groups(std::slice::from_ref(&elem.node), out);
                }
            }
            StructuredNode::List(list) => {
                for item in &list.items {
                    collect_from_inline_text(&item.content, out);
                    if let Some(sub) = &item.sublist {
                        for sub_item in &sub.items {
                            collect_from_inline_text(&sub_item.content, out);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn collect_from_inline_text(text: &TranslatedText, out: &mut Vec<Vec<String>>) {
    let texts: Vec<String> = text
        .iter()
        .map(|(_, inline)| inline.as_plain_text())
        .filter(|s| !s.trim().is_empty())
        .collect();
    if texts.len() >= 2 {
        out.push(texts);
    }
}

fn collect_from_translatable_string(ts: &TranslatableString, out: &mut Vec<Vec<String>>) {
    if let TranslatableString::Translated(map) = ts {
        let texts: Vec<String> = map
            .values()
            .filter_map(|v| v.as_ref())
            .filter(|v| !v.trim().is_empty())
            .cloned()
            .collect();
        if texts.len() >= 2 {
            out.push(texts);
        }
    }
}
