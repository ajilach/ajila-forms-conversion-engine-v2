//! The feedback guard's problems, checked on the package a build wrote.
//!
//! The check rules (`rules/aem/`) see the authored document; the templates, the
//! writer and the normalize passes are trusted to turn it into the shapes the
//! deployed corpus is held to. This is where that trust is checked: every build
//! scans the form's rendered `.content.xml` for the problems a template or
//! writer regression would bring back. A finding here is an engine defect, not
//! something the Author can fix by editing the document, so the build reports it
//! rather than refusing.
//!
//! Ported from the retired engine's `check_feedback_rules` (`core/src/review.rs`
//! before 87e8a42), which mirrored the feedback repo's detectors on the rendered
//! XML.

use serde::Serialize;

/// One guard problem found in the rendered package.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackageFinding {
    /// The guard's problem slug, e.g. `PROBLEM-dor-exclusion-implies-summary`.
    pub problem: &'static str,
    /// The offending component's `name`, or its JCR tag when it has none.
    pub node: String,
    /// What is wrong with this node, in one line.
    pub detail: String,
}

/// The findings on a built package's form XML. A package without a form XML
/// is an error, since `validate_package_bytes` already refuses to build one.
pub fn check_package(package: &[u8]) -> Result<Vec<PackageFinding>, String> {
    let files = references_mcp::unzip_package(package).map_err(|e| format!("could not read the package: {e}"))?;
    let (_, xml) = crate::conversion::form_content_xml(&files)
        .ok_or("the package has no form .content.xml (cq:Page)")?;
    Ok(check_form_xml(xml))
}

/// Attribute value lookup on one open tag, quote-aware.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = tag;
    while let Some(at) = rest.find(name) {
        let after = &rest[at + name.len()..];
        let before_ok = at == 0 || rest.as_bytes()[at - 1].is_ascii_whitespace();
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

/// The visual-editor properties an `fd:rules` node must not carry: a rule lives
/// in the code editor, on `fd:scripts`.
const VISUAL_EDITOR_PROPERTIES: &[&str] = &[
    "fd:visible",
    "fd:enabled",
    "fd:click",
    "fd:valueCommit",
    "fd:init",
    "fd:calc",
    "fd:calculate",
    "fd:validate",
    "fd:navigationChange",
];

/// Check the form's rendered JCR XML against the guard's problems.
pub fn check_form_xml(xml: &str) -> Vec<PackageFinding> {
    let mut out = Vec::new();
    let mut push = |problem: &'static str, node: String, detail: String| {
        out.push(PackageFinding { problem, node, detail })
    };
    let mut save_progress = 0usize;

    for (tag_name, tag) in open_tags(xml) {
        let label = attr(tag, "name").unwrap_or(tag_name).to_string();
        let resource_type = attr(tag, "sling:resourceType");

        // Every panel is the UBS custom panel: the default AEM panel has no
        // Summary authoring section, so the jump-to-field button cannot be set.
        if resource_type == Some("fd/af/components/panel") {
            push(
                "PROBLEM-panel-type-ubs",
                label.clone(),
                "uses the default AEM panel; every panel must be \
                 ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
                    .into(),
            );
        }

        // The germany/italy person and signature fragments were emptied into
        // the UBS generics; the deliberately market-specific families stay.
        if let Some(frag) = attr(tag, "fragRef") {
            let market =
                frag.contains("afforms_germany_fragmentlib/") || frag.contains("afforms_italy_fragmentlib/");
            let kept = ["internalbankuse", "InternalBankUse", "internal_bank_use", "footnote", "infobox", "BankingRelationship", "FormConfig"]
                .iter()
                .any(|family| frag.contains(family));
            if market && !kept {
                push(
                    "PROBLEM-fragment-library-consolidation",
                    label.clone(),
                    format!(
                        "references the retired market fragment {frag}; person blocks use the UBS \
                         partner generics and signatures affrg_SignatureGeneric1"
                    ),
                );
            }
        }

        // The UBS DoR is Redacto rendering the summary, so a node kept out of
        // the DoR but left in the summary still reaches the reader.
        if has_attr(tag, "dorExclusion", "true") && !has_attr(tag, "summaryExclusion", "true") {
            push(
                "PROBLEM-dor-exclusion-implies-summary",
                label.clone(),
                "is excluded from the Document of Record but not from the summary".into(),
            );
        }

        if tag_name == "fd:rules" {
            for property in VISUAL_EDITOR_PROPERTIES {
                if tag.contains(&format!(" {property}=\"")) {
                    push(
                        "PROBLEM-visual-editor-rules",
                        label.clone(),
                        format!("carries a visual-editor rule in {property}; it belongs on fd:scripts"),
                    );
                }
            }
        }

        // Without it the DoR wraps a long caption under the box.
        if resource_type.is_some_and(|rt| rt.ends_with("controls/checkbox"))
            && !has_attr(tag, "richTextOptions", "true")
        {
            push(
                "PROBLEM-checkbox-rich-text-options",
                label.clone(),
                "a checkbox needs richTextOptions=\"true\" or its DoR caption wraps under the box".into(),
            );
        }

        // The Edit button belongs on the step-title panel, never on the draw.
        if resource_type.is_some_and(|rt| rt.ends_with("controls/titledraw"))
            && has_attr(tag, "jumpToFieldButtonVisible", "true")
        {
            push(
                "PROBLEM-jump-to-field-button",
                label.clone(),
                "the jump-to-field button sits on the title draw, where it has no effect; it \
                 belongs on the enclosing step-title panel"
                    .into(),
            );
        }

        if let Some(frag_ref) = attr(tag, "fragRef") {
            // The bank's own copy: never on screen, never in the summary, always
            // in the PDF, and never `dorExclusion`, which would undo `alwaysInPdf`.
            if u2s_aem_ubs_mcp::aem::normalize::is_internal_bank_use(frag_ref) {
                let missing: Vec<&str> = [
                    ("summaryExclusion", has_attr(tag, "summaryExclusion", "true")),
                    ("alwaysInPdf", has_attr(tag, "alwaysInPdf", "true")),
                    ("visible=false", has_attr(tag, "visible", "{Boolean}false")),
                ]
                .into_iter()
                .filter(|(_, present)| !present)
                .map(|(name, _)| name)
                .collect();
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

            // The on-screen infobox stays out of the DoR, and a hidden copy
            // carries it into the PDF.
            if frag_ref.ends_with("affrg_italy_infobox") {
                let hidden = has_attr(tag, "visible", "{Boolean}false");
                if hidden && !has_attr(tag, "alwaysInPdf", "true") {
                    push(
                        "PROBLEM-infobox-dor-copy",
                        label.clone(),
                        "the DoR copy of the infobox needs alwaysInPdf, or it renders nowhere".into(),
                    );
                } else if !hidden
                    && (!has_attr(tag, "dorExclusion", "true") || !has_attr(tag, "summaryExclusion", "true"))
                {
                    push(
                        "PROBLEM-infobox-dor-copy",
                        label.clone(),
                        "the on-screen infobox must be excluded from the DoR and the summary".into(),
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

    if save_progress == 0 {
        push(
            "PROBLEM-nav-save-progress-required",
            "toolbar".into(),
            "the toolbar has no Save Progress button (`fwbSaveProgress`)".into(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A form with the guard's defects in it, one per node. Hand-built: the
    /// writer does not emit these shapes, which is the point of checking it.
    const DEFECTIVE: &str = r##"<jcr:root>
  <panel_default sling:resourceType="fd/af/components/panel" dorExclusion="true"
      guideNodeClass="guidePanel" name="PN_Default"/>
  <checkbox_plain sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/checkbox"
      guideNodeClass="guideCheckBox" name="CB_Options" options="[1=One]"/>
  <titledraw_jtf sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/titledraw"
      _value="&lt;p>Heading&lt;/p>" headingLevel="2" jumpToFieldButtonVisible="true" name="TTL_Step"/>
  <fd:rules fd:visible="[{&quot;nodeName&quot;:&quot;ROOT&quot;}]" jcr:primaryType="nt:unstructured"/>
  <panel_person sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
      fragRef="/content/dam/formsanddocuments/afforms_germany_fragmentlib/affrg_germany_Person"
      guideNodeClass="guidePanel" name="PN_Person"/>
  <panel_internal sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
      fragRef="/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_internalbankuse_ouref"
      guideNodeClass="guidePanel" name="PN_FRG_InternalBankUseOnly"/>
  <panel_infobox_copy sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
      fragRef="/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_infobox"
      guideNodeClass="guidePanel" name="PN_ItalyInfoboxDoR" visible="{Boolean}false"/>
</jcr:root>"##;

    /// A form that satisfies every check.
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

    fn problems_of(xml: &str) -> Vec<&'static str> {
        let mut problems: Vec<&'static str> = check_form_xml(xml).into_iter().map(|f| f.problem).collect();
        problems.sort();
        problems.dedup();
        problems
    }

    #[test]
    fn every_defect_is_reported() {
        assert_eq!(
            problems_of(DEFECTIVE),
            [
                "PROBLEM-checkbox-rich-text-options",
                "PROBLEM-dor-exclusion-implies-summary",
                "PROBLEM-fragment-library-consolidation",
                "PROBLEM-infobox-dor-copy",
                "PROBLEM-internal-bank-use-pdf-only",
                "PROBLEM-jump-to-field-button",
                "PROBLEM-nav-save-progress-required",
                "PROBLEM-panel-type-ubs",
                "PROBLEM-visual-editor-rules",
            ]
        );
    }

    #[test]
    fn a_conforming_form_reports_nothing() {
        assert!(check_form_xml(CLEAN).is_empty(), "{:?}", check_form_xml(CLEAN));
    }

    /// The defect is named on the node that carries it, by the component's
    /// `name`, which is how these forms address a node.
    #[test]
    fn a_finding_names_the_node_it_is_on() {
        let findings = check_form_xml(DEFECTIVE);
        let internal = findings
            .iter()
            .find(|f| f.problem == "PROBLEM-internal-bank-use-pdf-only")
            .expect("the internal-bank-use finding");
        assert_eq!(internal.node, "PN_FRG_InternalBankUseOnly");
        for missing in ["summaryExclusion", "alwaysInPdf", "visible"] {
            assert!(internal.detail.contains(missing), "{}", internal.detail);
        }
    }

    /// A rich-text `_value` carries a literal `>`, which a naive tag scan reads
    /// as the end of the tag, missing the attributes behind it.
    #[test]
    fn a_rich_text_value_does_not_hide_the_rest_of_the_tag() {
        let xml = r##"<jcr:root>
  <textdraw sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/textdraw"
      _value="&lt;p>text with a &gt; and a &lt;b>bold&lt;/b> run&lt;/p>" name="ST_Rich"
      dorExclusion="true"/>
  <guidebutton name="fwbSaveProgress"/>
</jcr:root>"##;
        assert_eq!(problems_of(xml), ["PROBLEM-dor-exclusion-implies-summary"]);
    }

    /// What the writer builds today is clean: every golden document, encoded,
    /// gives a package with no finding. A template or writer change that
    /// brings a guard problem back fails here first.
    #[test]
    fn the_golden_documents_build_clean_packages() {
        let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../u2s/crates/u2s-aem-ubs-mcp/tests/fixtures/golden");
        for form in ["AAOS_033_IT", "AAEV_019_EN", "AABF_019"] {
            let json: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(golden.join(form).join("document.json")).unwrap(),
            )
            .unwrap();
            let doc = u2s_aem_ubs_mcp::UbsAemDocument::from_json(&json).unwrap();
            let build = u2s_aem_ubs_mcp::encode(&doc).unwrap();
            let findings = check_package(&build.package).unwrap();
            assert!(findings.is_empty(), "{form}: {findings:?}");
        }
    }
}
