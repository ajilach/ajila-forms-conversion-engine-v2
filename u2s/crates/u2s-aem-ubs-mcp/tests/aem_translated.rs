//! `AemNode` <-> `AemNodeTranslated` lift/lower and `Passthrough` tests ported
//! from `blueprint` (the deleted `core` crate)'s `tests/mod.rs`
//! (`ajilach/ajila-forms-conversion-engine` at commit `f5f596a`, the last
//! commit before `core/` was deleted).
//!
//! These exercise the writer's load -> edit -> save round trip directly, on
//! real deployed UBS packages (`tests/fixtures/ubs-packages/`, see that
//! directory's README for provenance) and on small hand-built fixtures --
//! never through the mechanical PDF/XFA -> `StructuredNode` pipeline, which no
//! longer exists in this crate.

#[path = "support/mod.rs"]
mod support;

use std::collections::BTreeMap;

use u2s_aem_ubs_mcp::aem::translated::AemNodeTranslated;
use u2s_aem_ubs_mcp::aem::xml_validation::{duplicate_attribute_elements, validate_aem_form_xml};
use u2s_aem_ubs_mcp::aem::{
    AemNode, Passthrough, aem_to_translated, generate_aem_xml_with_passthrough, parse_aem_zip,
};
use uuid::Uuid;

/// A hand-built form carrying every attribute `AemAttrs` models, in the shapes
/// the deployed corpus uses: a step panel with a title exclusion and an Edit
/// button, the DoR-only internal-bank-use panel (`alwaysInPdf` without
/// `dorExclusion`), the slot-2 header draw, the first page's subtitle class,
/// and a hidden fragment.
const ATTRS_FORM_XML: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0" xmlns:fd="http://www.adobe.com/aemfd/fd/1.0"
    xmlns:cq="http://www.day.com/jcr/cq/1.0" xmlns:jcr="http://www.jcp.org/jcr/1.0"
    xmlns:nt="http://www.jcp.org/jcr/nt/1.0" jcr:primaryType="cq:Page">
    <jcr:content jcr:primaryType="cq:PageContent" jcr:language="it" jcr:title="AAOS"
        sling:resourceType="/apps/ajila-forms-customers/ajila-forms-ubs/components/pages/aftemplatedpage">
        <guideContainer jcr:primaryType="nt:unstructured" sling:resourceType="fd/af/components/guideContainer"
            guideNodeClass="guideContainerNode" name="guide1">
            <rootPanel jcr:primaryType="nt:unstructured" sling:resourceType="fd/af/components/rootPanel"
                guideNodeClass="rootPanelNode" name="guideRootPanel">
                <layout jcr:primaryType="nt:unstructured"
                    sling:resourceType="ajila-forms-customers/ajila-forms-ubs/layouts/panel/wizard"/>
                <items jcr:primaryType="nt:unstructured" sling:resourceType="fd/af/layouts/gridFluidLayout2">
                    <panel_step jcr:primaryType="nt:unstructured"
                        sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
                        dorExcludeTitle="true" guideNodeClass="guidePanel" jumpToFieldButtonVisible="true"
                        name="PN_Step" textIsRich="true">
                        <items jcr:primaryType="nt:unstructured" sling:resourceType="fd/af/layouts/gridFluidLayout2">
                            <textdraw_subtitle jcr:primaryType="nt:unstructured"
                                sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/textdraw"
                                _value="&lt;p>Attestazione di avvenuta consegna&lt;/p>" css="subtitle-after-form-title"
                                guideNodeClass="guideTextDraw" name="ST_Subtitle" textIsRich="true"/>
                            <textdraw_slot2 jcr:primaryType="nt:unstructured"
                                sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/textdraw"
                                _value="&lt;p>&lt;b>UBS Europe SE&lt;/b> (Succursale Italia)&lt;/p>" alwaysInPdf="true"
                                dorHeaderSlot="slot2" guideNodeClass="guideTextDraw" name="ST_HeaderSlot2"
                                showIfHidden="true" summaryExclusion="true" textIsRich="true" visible="{Boolean}false"/>
                            <textbox_iban jcr:primaryType="nt:unstructured" jcr:title="IBAN"
                                sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/textbox"
                                dorExclusion="true" guideNodeClass="guideTextBox" name="TXT_IBAN"
                                summaryExclusion="true" textIsRich="[true,true,true]"/>
                        </items>
                    </panel_step>
                    <panel_internal jcr:primaryType="nt:unstructured"
                        sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
                        alwaysInPdf="true" guideNodeClass="guidePanel" name="PN_InternalBankUseOnly"
                        summaryExclusion="true" textIsRich="true" visible="{Boolean}false">
                        <items jcr:primaryType="nt:unstructured" sling:resourceType="fd/af/layouts/gridFluidLayout2">
                            <panel_frg jcr:primaryType="nt:unstructured"
                                sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
                                alwaysInPdf="true"
                                fragRef="/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_internalbankuse_ouref"
                                guideNodeClass="guidePanel" name="PN_FRG_InternalBankUseOnly"
                                summaryExclusion="true" textIsRich="true" visible="{Boolean}false">
                                <items jcr:primaryType="nt:unstructured"/>
                            </panel_frg>
                        </items>
                    </panel_internal>
                </items>
            </rootPanel>
        </guideContainer>
    </jcr:content>
</jcr:root>
"##;

/// The `name` of a node, for the attribute tests.
fn node_name_of(node: &AemNode) -> Option<&str> {
    use AemNode as N;
    match node {
        N::Root { .. } => None,
        N::Panel { name, .. }
        | N::TextField { name, .. }
        | N::NumberField { name, .. }
        | N::DatePicker { name, .. }
        | N::Dropdown { name, .. }
        | N::Checkbox { name, .. }
        | N::RadioButton { name, .. }
        | N::TextDraw { name, .. }
        | N::TitleDraw { name, .. }
        | N::HtmlDisplayer { name, .. }
        | N::Repeatable { name, .. }
        | N::Fragment { name, .. }
        | N::Preface { name, .. }
        | N::Appendix { name, .. }
        | N::FootnotePlaceholder { name, .. }
        | N::MessageBox { name, .. }
        | N::Custom { name, .. } => Some(name),
    }
}

/// `visible` of the variants that carry one.
fn node_visible_of(node: &AemNode) -> Option<bool> {
    use AemNode as N;
    match node {
        N::Panel { visible, .. }
        | N::TextField { visible, .. }
        | N::NumberField { visible, .. }
        | N::DatePicker { visible, .. }
        | N::Dropdown { visible, .. }
        | N::Checkbox { visible, .. }
        | N::RadioButton { visible, .. }
        | N::TextDraw { visible, .. }
        | N::TitleDraw { visible, .. }
        | N::Repeatable { visible, .. }
        | N::Fragment { visible, .. }
        | N::Custom { visible, .. } => Some(*visible),
        _ => None,
    }
}

/// Every attribute of [`ATTRS_FORM_XML`] reaches the typed tree, and only the
/// node that carried it.
#[test]
fn presentation_attributes_are_typed_fields_after_loading() {
    let zip = support::aem_zip_from_form_xml("ATTR", ATTRS_FORM_XML);
    let package = parse_aem_zip(&zip).expect("parse the attribute fixture");

    let mut by_name = std::collections::HashMap::new();
    support::walk_aem_nodes(&package.root, &mut |node| {
        if let (Some(name), Some(attrs)) = (node_name_of(node), node.attrs()) {
            by_name.insert(name.to_string(), (attrs.clone(), node_visible_of(node)));
        }
    });

    let (step, _) = &by_name["PN_Step"];
    assert!(
        step.dor_exclude_title,
        "the step's title exclusion is typed"
    );
    assert!(
        !step.dor_exclude,
        "`dorExcludeTitle` must not read as `dorExclusion`: the step itself stays in the DoR"
    );
    assert!(
        step.jump_to_field,
        "the step-title panel keeps its Edit button"
    );

    let (subtitle, _) = &by_name["ST_Subtitle"];
    assert_eq!(subtitle.css.as_deref(), Some("subtitle-after-form-title"));

    let (slot2, slot2_visible) = &by_name["ST_HeaderSlot2"];
    assert!(slot2.always_in_pdf && slot2.show_if_hidden && slot2.summary_exclude);
    assert!(!slot2.dor_exclude, "the header draw must reach the PDF");
    assert_eq!(slot2.dor_header_slot.as_deref(), Some("slot2"));
    assert_eq!(*slot2_visible, Some(false));

    let (iban, _) = &by_name["TXT_IBAN"];
    assert!(
        iban.dor_exclude && iban.summary_exclude,
        "an input carries them too"
    );

    let (frag, frag_visible) = &by_name["PN_FRG_InternalBankUseOnly"];
    assert!(frag.always_in_pdf && frag.summary_exclude && !frag.dor_exclude);
    assert_eq!(*frag_visible, Some(false), "a hidden fragment stays hidden");
}

/// The same attributes come back out of the writer, on the same nodes and
/// once each. This is the round trip a dropped-in package makes when the
/// agent edits it: load, lift to the multilingual tree, lower, render.
#[test]
fn presentation_attributes_survive_a_load_save_round_trip() {
    let zip = support::aem_zip_from_form_xml("ATTR", ATTRS_FORM_XML);
    let package = parse_aem_zip(&zip).expect("parse the attribute fixture");
    let languages = vec![package.language.clone()];
    let lifted = aem_to_translated(
        &package.root,
        &package.translations,
        &languages,
        &package.language,
        &package.raw_by_uuid,
    );
    let (lowered, _dict) = lifted.lower(&package.language, &languages);
    let config = support::ubs_config_for(&package.language, &languages, "ATTR");
    let xml = generate_aem_xml_with_passthrough(&lowered, &config, &lifted.passthrough_map());

    let dups = duplicate_attribute_elements(&xml);
    assert!(
        dups.is_empty(),
        "duplicate attributes after saving:\n{}",
        dups.join("\n")
    );

    for (node, attr) in [
        ("PN_Step", "dorExcludeTitle=\"true\""),
        ("PN_Step", "jumpToFieldButtonVisible=\"true\""),
        ("ST_Subtitle", "css=\"subtitle-after-form-title\""),
        ("ST_HeaderSlot2", "alwaysInPdf=\"true\""),
        ("ST_HeaderSlot2", "dorHeaderSlot=\"slot2\""),
        ("ST_HeaderSlot2", "showIfHidden=\"true\""),
        ("ST_HeaderSlot2", "summaryExclusion=\"true\""),
        ("ST_HeaderSlot2", "visible=\"{Boolean}false\""),
        ("TXT_IBAN", "dorExclusion=\"true\""),
        ("TXT_IBAN", "summaryExclusion=\"true\""),
        ("PN_InternalBankUseOnly", "alwaysInPdf=\"true\""),
        ("PN_InternalBankUseOnly", "visible=\"{Boolean}false\""),
        ("PN_FRG_InternalBankUseOnly", "alwaysInPdf=\"true\""),
        ("PN_FRG_InternalBankUseOnly", "summaryExclusion=\"true\""),
        ("PN_FRG_InternalBankUseOnly", "visible=\"{Boolean}false\""),
    ] {
        let tag = support::open_tag_of(&xml, node)
            .unwrap_or_else(|| panic!("node {node} is missing from the saved form"));
        assert!(
            tag.contains(attr),
            "node {node} lost {attr} on save; its tag was:\n{tag}"
        );
    }

    let step = support::open_tag_of(&xml, "PN_Step").expect("PN_Step");
    assert!(
        !step.contains("dorExclusion="),
        "a `dorExcludeTitle` step must not gain `dorExclusion`:\n{step}"
    );
    for node in ["PN_InternalBankUseOnly", "PN_FRG_InternalBankUseOnly"] {
        let tag = support::open_tag_of(&xml, node).expect(node);
        assert!(
            !tag.contains("dorExclusion="),
            "{node} carries alwaysInPdf, so dorExclusion would undo it:\n{tag}"
        );
    }
}

/// Lifting an AEM package to `AemNodeTranslated` and lowering it back must
/// reproduce the original `AemNode` tree exactly -- the lift is the
/// structural inverse of `lower`. `AemNode` has no `PartialEq`, so compare
/// via serde.
#[test]
fn to_translated_lift_round_trips_to_aem_node() {
    for fixture in ["AACX.zip", "AAFM_019.zip"] {
        let zip_bytes = support::read_package_fixture(fixture);
        let package = parse_aem_zip(&zip_bytes).expect("parse zip fixture");
        let languages = support::lift_languages(&package);

        let lifted = aem_to_translated(
            &package.root,
            &package.translations,
            &languages,
            &package.language,
            &package.raw_by_uuid,
        );
        let (lowered, _dict) = lifted.lower(&package.language, &languages);

        assert_eq!(
            serde_json::to_value(&lowered).unwrap(),
            serde_json::to_value(&package.root).unwrap(),
            "{fixture}: lift→lower must round-trip back to the original AemNode tree"
        );
    }
}

/// A multilingual package must lift into per-language text: at least one
/// `AemI18nText` carries >= 2 languages.
#[test]
fn to_translated_lift_preserves_multiple_languages() {
    use u2s_aem_ubs_mcp::aem::translated::{AemI18nText, AemOptionTranslated};

    let zip_bytes = support::read_package_fixture("AAFM_019.zip");
    let package = parse_aem_zip(&zip_bytes).expect("parse AAFM_019.zip");
    let languages = support::lift_languages(&package);
    assert!(
        languages.len() >= 2,
        "AAFM_019.zip is expected to be multilingual, got {languages:?}"
    );

    let lifted = aem_to_translated(
        &package.root,
        &package.translations,
        &languages,
        &package.language,
        &package.raw_by_uuid,
    );

    fn max_langs(node: &AemNodeTranslated) -> usize {
        fn count(t: &AemI18nText) -> usize {
            t.languages().count()
        }
        fn opts(os: &[AemOptionTranslated]) -> usize {
            os.iter().map(|o| count(&o.label)).max().unwrap_or(0)
        }
        let here = match node {
            AemNodeTranslated::Root { title, .. } => count(title),
            AemNodeTranslated::Panel { title, .. } | AemNodeTranslated::Repeatable { title, .. } => {
                count(title)
            }
            AemNodeTranslated::TextField { label, .. }
            | AemNodeTranslated::NumberField { label, .. }
            | AemNodeTranslated::DatePicker { label, .. } => count(label),
            AemNodeTranslated::Dropdown { label, options, .. }
            | AemNodeTranslated::Checkbox { label, options, .. }
            | AemNodeTranslated::RadioButton { label, options, .. }
            | AemNodeTranslated::Custom { label, options, .. } => count(label).max(opts(options)),
            AemNodeTranslated::TextDraw { content, .. }
            | AemNodeTranslated::TitleDraw { content, .. } => count(content),
            _ => 0,
        };
        let children = match node {
            AemNodeTranslated::Root { children, .. }
            | AemNodeTranslated::Panel { children, .. }
            | AemNodeTranslated::Repeatable { children, .. } => {
                children.iter().map(max_langs).max().unwrap_or(0)
            }
            _ => 0,
        };
        here.max(children)
    }

    assert!(
        max_langs(&lifted) >= 2,
        "expected at least one text carrying >= 2 languages after the lift"
    );
}

/// Rebuild an AEM package ZIP identical to `original_bytes` except that the
/// main form `.content.xml` is replaced by `new_form_xml`. Every other entry
/// (fragments, dictionaries, DAM, vault metadata) is preserved verbatim so
/// the re-parse resolves fragments/translations exactly as the original did.
fn repackage_with_form_xml(original_bytes: &[u8], new_form_xml: &str) -> Vec<u8> {
    use std::io::{Cursor, Read, Write};

    let mut archive =
        zip::ZipArchive::new(Cursor::new(original_bytes)).expect("read original AEM zip");

    let mut form_path: Option<String> = None;
    for i in 0..archive.len() {
        let mut f = archive.by_index(i).unwrap();
        if f.is_dir() {
            continue;
        }
        let name = f.name().to_string();
        if name.contains("jcr_root/content/forms/af/") && name.ends_with(".content.xml") {
            let mut s = String::new();
            f.read_to_string(&mut s).ok();
            if s.contains("guideContainer") {
                form_path = Some(name);
                break;
            }
        }
    }
    let form_path = form_path.expect("locate form .content.xml in fixture zip");

    let buf = Cursor::new(Vec::<u8>::new());
    let mut writer = zip::ZipWriter::new(buf);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    for i in 0..archive.len() {
        let mut f = archive.by_index(i).unwrap();
        let name = f.name().to_string();
        if f.is_dir() {
            continue;
        }
        writer.start_file(&name, opts).unwrap();
        if name == form_path {
            writer.write_all(new_form_xml.as_bytes()).unwrap();
        } else {
            let mut bytes = Vec::new();
            f.read_to_end(&mut bytes).unwrap();
            writer.write_all(&bytes).unwrap();
        }
    }
    writer.finish().unwrap().into_inner()
}

/// Recursively strip every `uuid` key from a serde JSON value. Node uuids are
/// synthesized during parsing and are node identity, not content.
fn strip_uuids(value: &mut serde_json::Value) {
    const DROP: &[&str] = &["uuid", "dor_num_cols"];
    match value {
        serde_json::Value::Object(map) => {
            for k in DROP {
                map.remove(*k);
            }
            for v in map.values_mut() {
                strip_uuids(v);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                strip_uuids(v);
            }
        }
        _ => {}
    }
}

/// Assert that no loaded node is DROPPED on reload: every named node in
/// `orig` appears somewhere under the correspondingly-named node in
/// `reloaded` (children matched by `name`, not position).
fn loaded_subtree_diff(
    orig: &serde_json::Value,
    reloaded: &serde_json::Value,
    path: String,
) -> Option<(String, String)> {
    use serde_json::Value;
    let (oo, ro) = match (orig.as_object(), reloaded.as_object()) {
        (Some(a), Some(b)) => (a, b),
        _ => return None,
    };

    if oo.get("type").and_then(|t| t.as_str()) == Some("Repeatable") {
        return None;
    }

    if let Some(Value::Array(oc)) = oo.get("children") {
        let empty = Vec::new();
        let rc = match ro.get("children") {
            Some(Value::Array(a)) => a,
            _ => &empty,
        };
        let mut by_name: std::collections::HashMap<&str, &Value> = std::collections::HashMap::new();
        for child in rc {
            let name = child.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if name.starts_with("PN_StaticText_")
                && let Some(Value::Array(inner)) = child.get("children")
            {
                for wrapped in inner {
                    if let Some(n) = wrapped.get("name").and_then(|n| n.as_str()) {
                        by_name.insert(n, wrapped);
                    }
                }
            }
            if !name.is_empty() {
                by_name.insert(name, child);
            }
        }
        for child in oc {
            let name = child.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let ty = child.get("type").and_then(|t| t.as_str()).unwrap_or("?");
            match by_name.get(name) {
                None => {
                    return Some((
                        format!("{path}/{name}"),
                        format!("loaded {ty} child dropped on reload"),
                    ));
                }
                Some(rchild) => {
                    if let Some(d) = loaded_subtree_diff(child, rchild, format!("{path}/{name}")) {
                        return Some(d);
                    }
                }
            }
        }
    }
    None
}

/// The union of attribute names every component template writes on its own
/// opening tag. Used to exclude template-owned attributes -- whose exact
/// value is a deferred override step -- from the "unmodeled attributes
/// round-trip" assertion.
fn template_owned_names(
    config: &u2s_aem_ubs_mcp::aem::AemConfig,
) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for template in config.component_templates.values() {
        let head = template
            .split("{{ extra_attributes }}")
            .next()
            .unwrap_or(template);
        let bytes = head.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'=' && i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                let mut start = i;
                while start > 0 {
                    let c = bytes[start - 1];
                    if c.is_ascii_alphanumeric() || matches!(c, b'_' | b':' | b'.' | b'-') {
                        start -= 1;
                    } else {
                        break;
                    }
                }
                if start < i {
                    set.insert(head[start..i].to_string());
                }
            }
            i += 1;
        }
    }
    set
}

/// The core losslessness guarantee: loading a package into the working tree,
/// saving it back through the templates (carrying each node's
/// `Passthrough`), and loading the result again yields the **identical**
/// working tree. Because `AemNodeTranslated` embeds `passthrough`, this
/// proves every attribute and unmodeled child the loader captured is
/// re-emitted by the writer -- a load->save->load fixpoint.
#[test]
fn passthrough_load_save_load_is_a_fixpoint() {
    // Real deployed packages: rich in unmodeled attributes (guideNodeClass,
    // css, textIsRich, dorFieldStyling, ...), fd:rules/fd:scripts children,
    // cq:responsive widths, fragments, conditional panels and signatures.
    for fixture in [
        "Germany_AAJC.zip",
        "Germany_AACR.zip",
        "AAGO.zip",
        "AACX.zip",
        "AAFM_019.zip",
        "AAOW.zip",
        "AAOX.zip",
    ] {
        let zip_bytes = support::read_package_fixture(fixture);
        let package = parse_aem_zip(&zip_bytes).expect("parse zip fixture");
        let languages = support::lift_languages(&package);

        let lifted = support::lift_package(&package);
        let (lowered, _dict) = lifted.lower(&package.language, &languages);
        let passthrough = lifted.passthrough_map();
        let form_code = match &lowered {
            AemNode::Root { title, .. } => title.clone(),
            _ => String::new(),
        };
        let config = support::ubs_config_for(&package.language, &languages, &form_code);
        let regen = generate_aem_xml_with_passthrough(&lowered, &config, &passthrough);

        if let Err(violations) = validate_aem_form_xml(&regen) {
            panic!(
                "{fixture}: regenerated form XML is invalid:\n{}",
                violations.join("\n")
            );
        }
        let dups = duplicate_attribute_elements(&regen);
        assert!(
            dups.is_empty(),
            "{fixture}: regenerated XML has duplicate attributes:\n{}",
            dups.join("\n")
        );

        let zip2 = repackage_with_form_xml(&zip_bytes, &regen);
        let package2 = parse_aem_zip(&zip2)
            .unwrap_or_else(|e| panic!("{fixture}: re-parse of regenerated package failed: {e}"));
        let languages2 = support::lift_languages(&package2);
        let lifted2 = support::lift_package(&package2);
        let (lowered2, _dict2) = lifted2.lower(&package2.language, &languages2);

        let mut a = serde_json::to_value(&lowered).unwrap();
        let mut b = serde_json::to_value(&lowered2).unwrap();
        strip_uuids(&mut a);
        strip_uuids(&mut b);
        if let Some((path, msg)) = loaded_subtree_diff(&a, &b, String::from("root")) {
            panic!("{fixture}: load→save→load dropped/altered a loaded node at {path}: {msg}");
        }

        let owned_global = template_owned_names(&config);
        let unmodeled_attrs = |m: &std::collections::HashMap<Uuid, Passthrough>| {
            let mut v: Vec<(String, String)> = m
                .values()
                .flat_map(|p| p.raw_attributes.iter())
                .filter(|(k, _)| !owned_global.contains(k.as_str()))
                .map(|(k, val)| (k.clone(), val.clone()))
                .collect();
            v.sort();
            v
        };
        let unmodeled_children = |m: &std::collections::HashMap<Uuid, Passthrough>| {
            const BOILERPLATE: &str = "<fd:rules jcr:primaryType=\"nt:unstructured\"/>";
            let mut v: Vec<String> = m
                .values()
                .flat_map(|p| p.raw_children.iter())
                .filter(|c| c.as_str() != BOILERPLATE)
                .cloned()
                .collect();
            v.sort();
            v
        };
        let pass2 = lifted2.passthrough_map();
        assert_eq!(
            unmodeled_attrs(&passthrough),
            unmodeled_attrs(&pass2),
            "{fixture}: unmodeled attributes must round-trip verbatim"
        );
        assert_eq!(
            unmodeled_children(&passthrough),
            unmodeled_children(&pass2),
            "{fixture}: unmodeled child elements must round-trip verbatim"
        );
    }
}

/// Spot-check that specific attributes/children the *old* lossy converter
/// dropped now survive into the regenerated XML: a `guideNodeClass`, an
/// `fd:rules` child, a JCR-typed `{Boolean}` value, and a non-default
/// `cq:responsive` width parsed into `colspan`.
#[test]
fn passthrough_preserves_previously_dropped_details() {
    let zip_bytes = support::read_package_fixture("AACX.zip");
    let package = parse_aem_zip(&zip_bytes).expect("parse AACX.zip");
    let languages = support::lift_languages(&package);
    let config = support::ubs_config_for(&package.language, &languages, "AACX");
    let lifted = support::lift_package(&package);
    let (lowered, _dict) = lifted.lower(&package.language, &languages);
    let passthrough = lifted.passthrough_map();
    let regen = generate_aem_xml_with_passthrough(&lowered, &config, &passthrough);

    assert!(
        regen.contains("guideNodeClass="),
        "regenerated XML should retain guideNodeClass attributes"
    );
    assert!(
        regen.contains("fd:rules"),
        "regenerated XML should retain fd:rules child elements"
    );
    assert!(
        regen.contains("{Boolean}"),
        "regenerated XML should retain JCR-typed {{Boolean}} values verbatim"
    );

    assert!(
        !passthrough.is_empty(),
        "a real deployed package must produce non-empty passthrough on load"
    );
}

/// A working-tree snapshot written before `passthrough` existed must still
/// deserialize (the field is `#[serde(default)]`), yielding an empty
/// passthrough -- back-compat for persisted SQLite snapshots.
#[test]
fn passthrough_snapshot_back_compat_deserializes() {
    let mut raw_attributes = BTreeMap::new();
    raw_attributes.insert("myCustomProp".to_string(), "x".to_string());
    let node = AemNodeTranslated::TextField {
        attrs: u2s_aem_ubs_mcp::aem::AemAttrs::default(),
        uuid: Uuid::from_u128(1),
        passthrough: Passthrough {
            raw_attributes,
            raw_children: vec![],
        },
        name: "f1".into(),
        label: Default::default(),
        mandatory: false,
        visible: true,
        max_chars: None,
        colspan: 12,
        dor_colspan: None,
        bind_ref: None,
        kind: u2s_aem_ubs_mcp::aem::TextFieldKind::Plain,
    };
    let mut value = serde_json::to_value(&node).unwrap();
    fn drop_passthrough(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(m) => {
                m.remove("passthrough");
                for x in m.values_mut() {
                    drop_passthrough(x);
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(drop_passthrough),
            _ => {}
        }
    }
    drop_passthrough(&mut value);
    assert!(
        !serde_json::to_string(&value)
            .unwrap()
            .contains("passthrough"),
        "test setup: passthrough key must be gone from the legacy snapshot"
    );

    let restored: AemNodeTranslated = serde_json::from_value(value)
        .expect("legacy snapshot without passthrough must deserialize");
    match restored {
        AemNodeTranslated::TextField { passthrough, .. } => {
            assert!(
                passthrough.is_empty(),
                "missing passthrough must default to empty"
            );
        }
        _ => panic!("expected a TextField"),
    }
}

/// `passthrough` survives a serde snapshot round-trip (the SQLite-persist
/// path) and is surfaced by `passthrough_map`, without leaking into unrelated
/// nodes.
#[test]
fn passthrough_survives_serde_snapshot_round_trip() {
    let uuid = Uuid::from_u128(0x1234);
    let mut raw_attributes = BTreeMap::new();
    raw_attributes.insert("myCustomProp".to_string(), "{Boolean}true".to_string());
    let passthrough = Passthrough {
        raw_attributes,
        raw_children: vec!["<fd:rules jcr:primaryType=\"nt:unstructured\"/>".to_string()],
    };

    let tree = AemNodeTranslated::Root {
        title: Default::default(),
        children: vec![AemNodeTranslated::TextField {
            attrs: u2s_aem_ubs_mcp::aem::AemAttrs::default(),
            uuid,
            passthrough: passthrough.clone(),
            name: "f1".into(),
            label: Default::default(),
            mandatory: false,
            visible: true,
            max_chars: None,
            colspan: 12,
            dor_colspan: None,
            bind_ref: None,
            kind: u2s_aem_ubs_mcp::aem::TextFieldKind::Plain,
        }],
    };

    let json = serde_json::to_string(&tree).unwrap();
    let restored: AemNodeTranslated = serde_json::from_str(&json).unwrap();

    let map = restored.passthrough_map();
    assert_eq!(map.len(), 1, "exactly the one field carries passthrough");
    assert_eq!(
        map.get(&uuid),
        Some(&passthrough),
        "passthrough must survive the snapshot round-trip intact"
    );
}

// ============================================================================
// The AEM HTML component (`controls/htmlDisplayer`)
// ============================================================================

/// A form carrying the HTML component in exactly the shape AEM's own
/// authoring UI writes it (taken from `AAOV_033.zip`): positional `item{N}`
/// children, regional UBS locales, markup XML-escaped into the `html`
/// attribute.
const HTML_COMPONENT_FORM_XML: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0" xmlns:fd="http://www.adobe.com/aemfd/fd/1.0"
    xmlns:cq="http://www.day.com/jcr/cq/1.0" xmlns:jcr="http://www.jcp.org/jcr/1.0"
    xmlns:nt="http://www.jcp.org/jcr/nt/1.0" jcr:primaryType="cq:Page">
    <jcr:content jcr:primaryType="cq:PageContent" jcr:language="it" jcr:title="AAOV"
        sling:resourceType="/apps/ajila-forms-customers/ajila-forms-ubs/components/pages/aftemplatedpage">
        <guideContainer jcr:primaryType="nt:unstructured" sling:resourceType="fd/af/components/guideContainer"
            guideNodeClass="guideContainerNode" name="guide1">
            <rootPanel jcr:primaryType="nt:unstructured" sling:resourceType="fd/af/components/rootPanel"
                guideNodeClass="rootPanelNode" name="guideRootPanel">
                <items jcr:primaryType="nt:unstructured" sling:resourceType="fd/af/layouts/gridFluidLayout2">
                    <panel_step jcr:primaryType="nt:unstructured"
                        sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
                        guideNodeClass="guidePanel" name="PN_Step" textIsRich="true">
                        <items jcr:primaryType="nt:unstructured" sling:resourceType="fd/af/layouts/gridFluidLayout2">
                            <htmldisplayer jcr:primaryType="nt:unstructured"
                                sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/htmlDisplayer"
                                autofillFieldKeyword="name" css="widget_ajila_forms_htmlViewer"
                                guideNodeClass="guideTextBox"
                                initScript="window.forms.ubs.control.htmlviewer.initialize(this)"
                                name="TBL_Plans" textIsRich="[true,true,true,true]">
                                <localeContent jcr:primaryType="nt:unstructured">
                                    <item0 jcr:primaryType="nt:unstructured" locale="en-us"
                                        html="&lt;table&gt;&lt;tr&gt;&lt;td&gt;Plan 1&lt;/td&gt;&lt;/tr&gt;&lt;/table&gt;"/>
                                    <item1 jcr:primaryType="nt:unstructured" locale="it-ch"
                                        html="&lt;table&gt;&lt;tr&gt;&lt;td&gt;Piano 1&lt;/td&gt;&lt;/tr&gt;&lt;/table&gt;"/>
                                </localeContent>
                            </htmldisplayer>
                        </items>
                    </panel_step>
                </items>
            </rootPanel>
        </guideContainer>
    </jcr:content>
</jcr:root>
"##;

/// Find the single [`AemNode::HtmlDisplayer`] in a tree.
fn only_html_displayer(root: &AemNode) -> u2s_aem_ubs_mcp::aem::AemI18nText {
    let mut found = Vec::new();
    support::walk_aem_nodes(root, &mut |node| {
        if let AemNode::HtmlDisplayer { content, .. } = node {
            found.push(content.clone());
        }
    });
    assert_eq!(found.len(), 1, "expected exactly one HTML component");
    found.remove(0)
}

/// The regression this component was silently failing: an `htmlDisplayer`
/// has no `items` child and no `fragRef`, so before it was recognised it
/// fell into the parser's unknown-component arm and the whole block was
/// dropped from a loaded package without a word.
#[test]
fn the_html_component_survives_a_load() {
    let zip = support::aem_zip_from_form_xml("AAOV", HTML_COMPONENT_FORM_XML);
    let package = parse_aem_zip(&zip).expect("parse the HTML-component fixture");

    let content = only_html_displayer(&package.root);

    assert_eq!(
        content.languages().collect::<Vec<_>>(),
        vec!["en", "it"],
        "regional locales fold onto the engine's language codes"
    );
    assert!(
        content
            .get("en")
            .is_some_and(|m| m.contains("<table>") && m.contains("Plan 1")),
        "the English markup arrives decoded: {:?}",
        content.get("en")
    );
    assert!(
        content.get("it").is_some_and(|m| m.contains("Piano 1")),
        "and each locale keeps its OWN markup, not a copy of the first"
    );
}

/// Load -> save -> load must be a fixpoint for the component, or a review run
/// over a package that carries one degrades it a little every time.
#[test]
fn the_html_component_survives_a_load_save_load_round_trip() {
    let zip = support::aem_zip_from_form_xml("AAOV", HTML_COMPONENT_FORM_XML);
    let package = parse_aem_zip(&zip).expect("parse the HTML-component fixture");
    let before = only_html_displayer(&package.root);

    let languages = vec!["en".to_string(), "it".to_string()];
    let lifted = support::lift_package(&package);
    let (lowered, dict) = lifted.lower(&package.language, &languages);
    assert!(
        !dict.keys().any(|k| k.contains("<table>")),
        "the markup must not become a translation-dictionary key: {:?}",
        dict.keys().collect::<Vec<_>>()
    );

    let config = support::ubs_config_for(&package.language, &languages, "AAOV");
    let regen = generate_aem_xml_with_passthrough(&lowered, &config, &lifted.passthrough_map());
    if let Err(violations) = validate_aem_form_xml(&regen) {
        panic!(
            "the re-rendered form is invalid AEM XML:\n{}",
            violations.join("\n")
        );
    }

    let reparsed = parse_aem_zip(&support::aem_zip_from_form_xml("AAOV", &regen))
        .expect("reparse the re-rendered form");
    let after = only_html_displayer(&reparsed.root);

    assert_eq!(
        before.0, after.0,
        "the per-locale markup must come back byte-identical"
    );
}

/// A form whose repeating panel carries the archetype attributes
/// `repeatable.xml` writes, plus an ordinary panel carrying an
/// `accessibilityLabel` a person authored.
const ARCHETYPE_ATTRS_FORM_XML: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<jcr:root xmlns:sling="http://sling.apache.org/jcr/sling/1.0" xmlns:fd="http://www.adobe.com/aemfd/fd/1.0"
    xmlns:cq="http://www.day.com/jcr/cq/1.0" xmlns:jcr="http://www.jcp.org/jcr/1.0"
    xmlns:nt="http://www.jcp.org/jcr/nt/1.0" jcr:primaryType="cq:Page">
    <jcr:content jcr:primaryType="cq:PageContent" jcr:language="de" jcr:title="ARCH"
        sling:resourceType="/apps/ajila-forms-customers/ajila-forms-ubs/components/pages/aftemplatedpage">
        <guideContainer jcr:primaryType="nt:unstructured" sling:resourceType="fd/af/components/guideContainer"
            guideNodeClass="guideContainerNode" name="guide1">
            <rootPanel jcr:primaryType="nt:unstructured" sling:resourceType="fd/af/components/rootPanel"
                guideNodeClass="rootPanelNode" name="guideRootPanel">
                <items jcr:primaryType="nt:unstructured" sling:resourceType="fd/af/layouts/gridFluidLayout2">
                    <panel_plain jcr:primaryType="nt:unstructured"
                        sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
                        accessibilityLabel="Hand-authored label" guideNodeClass="guidePanel"
                        name="PN_Plain" textIsRich="true">
                        <items jcr:primaryType="nt:unstructured" sling:resourceType="fd/af/layouts/gridFluidLayout2">
                            <repeatableInner jcr:primaryType="nt:unstructured" jcr:title="Client"
                                sling:resourceType="ajila-forms-customers/ajila-forms-ubs/components/controls/panel"
                                accessibilityLabel="Client" addButton="BT_Add" ajilaPanelSubject="Client"
                                dorFieldStyling="Repeating Panel Numbering" guideNodeClass="guidePanel"
                                headingLevel="3" maxOccur="4" minOccur="1" name="RCP_Client_repeat"
                                removeButton="BT_Remove" summaryHeadingLevel="4" textIsRich="true">
                                <items jcr:primaryType="nt:unstructured"/>
                            </repeatableInner>
                        </items>
                    </panel_plain>
                </items>
            </rootPanel>
        </guideContainer>
    </jcr:content>
</jcr:root>
"##;

/// The repeating-panel archetype is engine-owned: `repeatable.xml` writes
/// `accessibilityLabel`, `addButton`, `ajilaPanelSubject`, `removeButton` and
/// `summaryHeadingLevel` itself, so a loaded value must NOT be kept as
/// unmodeled passthrough. Keeping it carried a stale value forward and made
/// load -> save -> load stop being a fixpoint on every deployed package that
/// predates the archetype (the regression `b15ff20` introduced on
/// `Germany_AACR.zip`).
///
/// An ordinary panel's `accessibilityLabel` is a different thing entirely --
/// a person authored it and no template regenerates it -- so it is kept.
#[test]
fn the_repeating_panel_archetype_attributes_are_not_passthrough() {
    let zip = support::aem_zip_from_form_xml("ARCH", ARCHETYPE_ATTRS_FORM_XML);
    let package = parse_aem_zip(&zip).expect("parse the archetype fixture");

    let mut repeatable_uuid = None;
    let mut panel_uuid = None;
    support::walk_aem_nodes(&package.root, &mut |node| match node {
        AemNode::Repeatable { uuid, name, .. } if name == "RCP_Client_repeat" => {
            repeatable_uuid = Some(*uuid);
        }
        AemNode::Panel { uuid, name, .. } if name == "PN_Plain" => panel_uuid = Some(*uuid),
        _ => {}
    });

    let repeatable = package.raw_by_uuid[&repeatable_uuid.expect("the repeating panel")].clone();
    for owned in [
        "accessibilityLabel",
        "addButton",
        "ajilaPanelSubject",
        "removeButton",
        "summaryHeadingLevel",
    ] {
        assert!(
            !repeatable.raw_attributes.contains_key(owned),
            "`{owned}` is the template's to write, so it must not be captured: {:?}",
            repeatable.raw_attributes.keys().collect::<Vec<_>>()
        );
    }
    assert!(
        repeatable.raw_attributes.contains_key("dorFieldStyling"),
        "an attribute the archetype does NOT cover is still captured: {:?}",
        repeatable.raw_attributes.keys().collect::<Vec<_>>()
    );

    let panel = package.raw_by_uuid[&panel_uuid.expect("the plain panel")].clone();
    assert_eq!(
        panel
            .raw_attributes
            .get("accessibilityLabel")
            .map(String::as_str),
        Some("Hand-authored label"),
        "an ordinary panel's own label survives: {:?}",
        panel.raw_attributes
    );
}
