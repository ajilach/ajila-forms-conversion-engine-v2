//! Restoring a previously recorded editing session from the edit-history store.
//!
//! A document's history is spread over sibling session ids, and the rows under
//! them hold more than one shape because several producers write them:
//!
//! * `<session>` — a [`DocumentEnvelope`] (pipeline runs and structured-editor
//!   edits) or a bare `Vec<StructuredNode>` (older agent runs, plus the `"[]"`
//!   seed every agent run still writes).
//! * `<session>#aem` — an [`AemNodeTranslated`] (the multilingual tree the
//!   conversion agent authors, snapshotted on every mutating tool call) or an
//!   [`AemNode`] (single-language snapshots the AEM editor wrote before the
//!   history was unified on the translated shape).
//!
//! An agent run never fills its structured tree — it authors the AEM tree
//! directly — so its `<session>` rows hold nothing but the empty seed and the
//! `#aem` history is the only record of what the run produced. Restoring
//! therefore prefers a non-empty structured snapshot when one exists and
//! otherwise reconstructs the document from the AEM tree.

use std::collections::{BTreeMap, HashMap};

use blueprint::aem::TranslationData;
use blueprint::{
    AemNode, AemNodeTranslated, Context, DocumentEnvelope, StructuredNode, aem_to_structured,
    aem_to_translated,
};

use crate::conversion::collect_translated_languages;

/// A session's recorded state, lifted back into the shapes the editors take.
pub struct RestoredSession {
    /// The structured document, for the structured editor and the derived
    /// outputs. Reconstructed from [`Self::aem_translated`] when the session
    /// recorded no structured content of its own (every agent run).
    pub envelope: DocumentEnvelope,
    /// The AEM tree as it was actually authored, when the session has one.
    ///
    /// Authoritative for the AEM editor: re-deriving the tree from
    /// `envelope.content` would discard the agent's own work (and for an agent
    /// run whose structured tree is empty, would yield an empty tree).
    pub aem_translated: Option<AemNodeTranslated>,
    /// The page headers the agent set, by language (the `#headers` sibling).
    pub headers: BTreeMap<String, String>,
}

/// Restore `session_id` from the edit-history store, reading the latest
/// snapshot of both the structured session and its `#aem` sibling.
///
/// `profile` is the conversion profile the session was created with (see
/// [`crate::db::session_profile`]); it supplies the master language when the
/// recorded tree does not pin one down.
pub fn restore(session_id: &str, profile: Option<&str>) -> Option<RestoredSession> {
    let structured = latest_snapshot(session_id);
    let aem = latest_snapshot(&format!("{session_id}#aem"));
    let mut restored = restore_from_snapshots(structured.as_ref(), aem.as_ref(), profile)?;
    restored.headers = latest_snapshot(&format!("{session_id}#headers"))
        .and_then(|s| serde_json::from_str(&s.json).ok())
        .unwrap_or_default();
    Some(restored)
}

/// The latest row of one session, with the timestamp that orders it against the
/// sibling session's latest row.
pub struct Snapshot {
    pub json: String,
    /// RFC3339 timestamp the row was recorded at. Every row is written by
    /// [`crate::db`] as UTC, so plain string ordering is chronological.
    pub recorded_at: Option<String>,
}

/// The pure core of [`restore`]: lift the two latest snapshots into a
/// [`RestoredSession`], accepting every shape either session has ever held.
///
/// Returns `None` only when neither snapshot carries a document — the caller
/// should report that rather than silently doing nothing.
pub fn restore_from_snapshots(
    structured: Option<&Snapshot>,
    aem: Option<&Snapshot>,
    profile: Option<&str>,
) -> Option<RestoredSession> {
    let mut tree = aem.and_then(|s| parse_aem_snapshot(&s.json, profile));
    let recorded = structured.and_then(|s| parse_structured_snapshot(&s.json));

    // A structure edit regenerates the AEM package from the structured content,
    // so an AEM tree older than the latest structure edit no longer describes
    // the document — drop it and let the editor re-derive, as it did before the
    // tree was persisted at all.
    if let (Some(structured), Some(aem)) = (structured, aem)
        && let (Some(edited), Some(authored)) = (&structured.recorded_at, &aem.recorded_at)
        && edited > authored
    {
        tree = None;
    }

    // A non-empty structured snapshot is what the user last edited; keep it
    // verbatim rather than reconstructing over the top of it.
    if let Some(envelope) = recorded.filter(|e| !e.content.is_empty()) {
        return Some(RestoredSession {
            envelope,
            aem_translated: tree,
            headers: BTreeMap::new(),
        });
    }

    // Otherwise the AEM tree is the only record of the run.
    let tree = tree?;
    let content = structured_from_aem_tree(&tree, profile);
    if content.is_empty() {
        return None;
    }
    let languages = tree_languages(&tree, profile);
    Some(RestoredSession {
        envelope: DocumentEnvelope {
            context: Context::with_language(languages.join(",")),
            content,
            state_count: 1,
        },
        aem_translated: Some(tree),
        headers: BTreeMap::new(),
    })
}

/// Lift an AEM tree back into structured content.
///
/// Lowers the tree to the single-language [`AemNode`] plus its translation
/// dictionary — the pair the package writer consumes — and runs the regular
/// AEM→structured conversion over it. Lossy relative to the tree itself (the
/// tree stays authoritative for the AEM editor), but it yields an editable
/// document where there would otherwise be none.
pub fn structured_from_aem_tree(
    tree: &AemNodeTranslated,
    profile: Option<&str>,
) -> Vec<StructuredNode> {
    let languages = tree_languages(tree, profile);
    let master = master_language(profile, &languages);
    let (root, dict) = tree.lower(&master, &languages);
    let translations = blueprint::translation_data_from_master_dict(dict);
    aem_to_structured(
        &root,
        // No script engine ran, so every node keeps its own `visible` flag and
        // no panel gains a visibility condition.
        &HashMap::new(),
        &translations,
        &languages,
        &master,
        &HashMap::new(),
    )
}

/// The latest snapshot recorded under `session_id`, if any.
fn latest_snapshot(session_id: &str) -> Option<Snapshot> {
    let seq = crate::db::latest_seq(session_id)?;
    let json = crate::db::snapshot_at(session_id, seq)?;
    let recorded_at = crate::db::list_edits(session_id)
        .into_iter()
        .find(|edit| edit.seq == seq)
        .map(|edit| edit.created_at);
    Some(Snapshot { json, recorded_at })
}

/// Parse a `<session>` row: a [`DocumentEnvelope`], or the bare node list older
/// agent runs recorded.
fn parse_structured_snapshot(json: &str) -> Option<DocumentEnvelope> {
    if let Ok(envelope) = serde_json::from_str::<DocumentEnvelope>(json) {
        return Some(envelope);
    }
    let content = serde_json::from_str::<Vec<StructuredNode>>(json).ok()?;
    let languages = blueprint::collect_languages(&content);
    Some(DocumentEnvelope {
        context: Context::with_language(languages.into_iter().collect::<Vec<String>>().join(",")),
        content,
        state_count: 1,
    })
}

/// Parse an `#aem` row into the multilingual tree, accepting the single-language
/// [`AemNode`] rows the AEM editor wrote before the history was unified.
///
/// The two shapes are unambiguous: a text field is a `{lang: text}` map in the
/// translated tree and a plain string in `AemNode`, so neither parses as the
/// other.
fn parse_aem_snapshot(json: &str, profile: Option<&str>) -> Option<AemNodeTranslated> {
    if let Ok(tree) = serde_json::from_str::<AemNodeTranslated>(json) {
        return Some(tree);
    }
    let node = serde_json::from_str::<AemNode>(json).ok()?;
    let master = master_language(profile, &[]);
    Some(aem_to_translated(
        &node,
        &TranslationData::default(),
        std::slice::from_ref(&master),
        &master,
        &HashMap::new(),
    ))
}

/// Every language present in the tree, with the master language guaranteed to
/// be among them (an all-empty tree pins down no language at all).
///
/// Public because a tree must be lowered with its own languages listed, or
/// lowering drops every locale the list omits.
pub fn tree_languages(tree: &AemNodeTranslated, profile: Option<&str>) -> Vec<String> {
    let mut languages: Vec<String> = collect_translated_languages(tree).into_iter().collect();
    let master = master_language(profile, &languages);
    if !languages.contains(&master) {
        languages.insert(0, master);
    }
    languages
}

/// The form's master language: the profile's AEM config decides, as long as it
/// is a language the tree actually carries; otherwise the tree's first language
/// wins, and `en` is the last resort.
fn master_language(profile: Option<&str>, languages: &[String]) -> String {
    let provisional = Context::with_language(languages.join(","));
    profile
        .filter(|p| blueprint::has_aem_config(p))
        .and_then(|p| blueprint::load_aem_config(p, &provisional).ok())
        .map(|cfg| cfg.master_language)
        .filter(|m| languages.is_empty() || languages.contains(m))
        .or_else(|| languages.first().cloned())
        .unwrap_or_else(|| "en".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use blueprint::{AemAttrs, AemI18nText, StructuredNode};
    use uuid::Uuid;

    /// A bilingual tree with one panel holding one text field.
    fn bilingual_tree() -> AemNodeTranslated {
        AemNodeTranslated::Root {
            title: text(&[("en", "Form"), ("de", "Formular")]),
            children: vec![AemNodeTranslated::Panel {
                uuid: Uuid::from_u128(1),
                passthrough: Default::default(),
                name: "p1".into(),
                title: text(&[("en", "Details"), ("de", "Angaben")]),
                children: vec![AemNodeTranslated::TextField {
                    uuid: Uuid::from_u128(2),
                    passthrough: Default::default(),
                    name: "f1".into(),
                    label: text(&[("en", "Last name"), ("de", "Nachname")]),
                    mandatory: false,
                    visible: true,
                    max_chars: None,
                    colspan: 12,
                    dor_colspan: None,
                    bind_ref: None,
                    kind: Default::default(),
                    attrs: AemAttrs::default(),
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
            }],
        }
    }

    fn text(entries: &[(&str, &str)]) -> AemI18nText {
        AemI18nText(
            entries
                .iter()
                .map(|(l, t)| (l.to_string(), t.to_string()))
                .collect(),
        )
    }

    /// Collect every field label found anywhere in the content, in `lang`.
    fn field_labels(content: &[StructuredNode], lang: &str) -> Vec<String> {
        fn walk(nodes: &[StructuredNode], lang: &str, out: &mut Vec<String>) {
            for node in nodes {
                match node {
                    StructuredNode::Field(f) => {
                        if let Some(label) = &f.label {
                            out.push(label.plain_text_in(lang));
                        }
                    }
                    StructuredNode::Group(g) => walk(&g.children, lang, out),
                    StructuredNode::Conditional(c) => {
                        walk(std::slice::from_ref(c.content.as_ref()), lang, out)
                    }
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(content, lang, &mut out);
        out
    }

    /// A snapshot row with no timestamp (ordering is irrelevant to the case).
    fn snap(json: &str) -> Snapshot {
        Snapshot {
            json: json.to_string(),
            recorded_at: None,
        }
    }

    /// A snapshot row recorded at `at`.
    fn snap_at(json: &str, at: &str) -> Snapshot {
        Snapshot {
            json: json.to_string(),
            recorded_at: Some(at.to_string()),
        }
    }

    /// The bug: an agent run records only the `"[]"` seed in its structured
    /// session and everything it authored in `#aem`, so loading the session
    /// found nothing and silently did nothing.
    #[test]
    fn agent_session_restores_from_its_aem_history() {
        let tree = bilingual_tree();
        let aem = serde_json::to_string(&tree).unwrap();

        let restored = restore_from_snapshots(Some(&snap("[]")), Some(&snap(&aem)), None)
            .expect("a session with an AEM history must restore");

        assert!(
            !restored.envelope.content.is_empty(),
            "the structured document must be reconstructed from the AEM tree"
        );
        assert_eq!(
            field_labels(&restored.envelope.content, "en"),
            vec!["Last name".to_string()],
            "the authored field must survive the reconstruction"
        );
        assert_eq!(
            restored.aem_translated.as_ref(),
            Some(&tree),
            "the AEM tree must be handed back exactly as it was recorded"
        );
    }

    /// The tree is the authoritative artifact, so it comes back even when the
    /// structured session does hold a document.
    #[test]
    fn aem_tree_is_restored_alongside_a_structured_snapshot() {
        let tree = bilingual_tree();
        let aem = serde_json::to_string(&tree).unwrap();
        let envelope = DocumentEnvelope {
            context: Context::with_language("en"),
            content: vec![StructuredNode::Paragraph(blueprint::ParagraphNode {
                content: blueprint::TranslatedText::plain("Edited by hand"),
                som_path: None,
                source_name: None,
            })],
            state_count: 1,
        };
        let base = serde_json::to_string(&envelope).unwrap();

        let restored =
            restore_from_snapshots(Some(&snap(&base)), Some(&snap(&aem)), None).expect("restores");

        assert_eq!(
            restored.envelope.content.len(),
            1,
            "an edited structured document must be kept verbatim, not reconstructed"
        );
        assert_eq!(restored.aem_translated.as_ref(), Some(&tree));
    }

    /// Per-language text must survive the lower→convert round trip, otherwise a
    /// restored session would silently lose every translation.
    #[test]
    fn reconstruction_keeps_translations() {
        let aem = serde_json::to_string(&bilingual_tree()).unwrap();

        let restored =
            restore_from_snapshots(Some(&snap("[]")), Some(&snap(&aem)), None).expect("restores");

        let langs = blueprint::collect_languages(&restored.envelope.content);
        assert!(
            langs.contains("de") && langs.contains("en"),
            "both languages must survive, got {langs:?}"
        );
        assert_eq!(
            field_labels(&restored.envelope.content, "de"),
            vec!["Nachname".to_string()],
            "the non-master label must come back in its own language"
        );
        assert!(
            restored.envelope.context.language().contains("de"),
            "the synthesized context must name the languages, got '{}'",
            restored.envelope.context.language()
        );
    }

    /// Single-language rows written by the AEM editor before the history was
    /// unified on the translated shape must still load.
    #[test]
    fn legacy_single_language_aem_snapshot_restores() {
        let (node, _) = bilingual_tree().lower("en", &["en".to_string(), "de".to_string()]);
        let aem = serde_json::to_string(&node).unwrap();

        let restored = restore_from_snapshots(Some(&snap("[]")), Some(&snap(&aem)), None)
            .expect("legacy row restores");

        assert_eq!(
            field_labels(&restored.envelope.content, "en"),
            vec!["Last name".to_string()]
        );
        assert!(
            restored.aem_translated.is_some(),
            "a lifted legacy tree must still reach the AEM editor"
        );
    }

    /// Older agent runs recorded the bare node list rather than an envelope.
    #[test]
    fn bare_node_list_snapshot_restores_as_an_envelope() {
        let content = vec![StructuredNode::Paragraph(blueprint::ParagraphNode {
            content: blueprint::TranslatedText::plain("Recorded as an array"),
            som_path: None,
            source_name: None,
        })];
        let base = serde_json::to_string(&content).unwrap();

        let restored = restore_from_snapshots(Some(&snap(&base)), None, None)
            .expect("a bare node list must still load");

        assert_eq!(restored.envelope.content.len(), 1);
        assert!(restored.aem_translated.is_none());
    }

    /// A structure edit regenerates the AEM package, so a tree recorded before
    /// that edit is stale and must not be handed to the AEM editor.
    #[test]
    fn aem_tree_older_than_the_last_structure_edit_is_dropped() {
        let aem = serde_json::to_string(&bilingual_tree()).unwrap();
        let envelope = DocumentEnvelope {
            context: Context::with_language("en"),
            content: vec![StructuredNode::Paragraph(blueprint::ParagraphNode {
                content: blueprint::TranslatedText::plain("Edited after the AEM tree"),
                som_path: None,
                source_name: None,
            })],
            state_count: 1,
        };
        let base = serde_json::to_string(&envelope).unwrap();

        let restored = restore_from_snapshots(
            Some(&snap_at(&base, "2026-08-05T12:00:00+00:00")),
            Some(&snap_at(&aem, "2026-08-05T11:00:00+00:00")),
            None,
        )
        .expect("restores");

        assert!(
            restored.aem_translated.is_none(),
            "a tree older than the last structure edit no longer describes the document"
        );
        assert_eq!(restored.envelope.content.len(), 1);
    }

    /// The reverse order: AEM edits made after the last structure edit stand.
    #[test]
    fn aem_tree_newer_than_the_last_structure_edit_is_kept() {
        let aem = serde_json::to_string(&bilingual_tree()).unwrap();
        let envelope = DocumentEnvelope {
            context: Context::with_language("en"),
            content: vec![StructuredNode::Paragraph(blueprint::ParagraphNode {
                content: blueprint::TranslatedText::plain("Edited before the AEM tree"),
                som_path: None,
                source_name: None,
            })],
            state_count: 1,
        };
        let base = serde_json::to_string(&envelope).unwrap();

        let restored = restore_from_snapshots(
            Some(&snap_at(&base, "2026-08-05T11:00:00+00:00")),
            Some(&snap_at(&aem, "2026-08-05T12:00:00+00:00")),
            None,
        )
        .expect("restores");

        assert!(
            restored.aem_translated.is_some(),
            "AEM edits made after the last structure edit must survive a reload"
        );
    }

    /// Nothing recoverable must be reported as such, not silently ignored.
    #[test]
    fn empty_session_does_not_restore() {
        assert!(restore_from_snapshots(Some(&snap("[]")), None, None).is_none());
        assert!(restore_from_snapshots(None, None, None).is_none());
        assert!(
            restore_from_snapshots(Some(&snap("[]")), Some(&snap("not json")), None).is_none(),
            "an unparseable AEM row must not be mistaken for a document"
        );
    }
}
